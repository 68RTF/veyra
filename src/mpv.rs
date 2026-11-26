use glib::{ControlFlow, IOCondition};
use gtk::gio::{self, prelude::*};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const IO_BYTES: usize = 32 * 1024;
const IO_CALLS: usize = 16;
// Only small playback properties are subscribed; leave headroom for other mpv events.
const MAX_FRAME: usize = 1024 * 1024;
const MAX_OUTGOING: usize = 1024 * 1024;
const MAX_REQUESTS: usize = 4096;
const FD_ERRORS: IOCondition = IOCondition::HUP
    .union(IOCondition::ERR)
    .union(IOCondition::NVAL);

pub(crate) struct MpvProcess {
    child: gio::Subprocess,
    path: PathBuf,
    cancel_connect: gio::Cancellable,
}

impl MpvProcess {
    pub(crate) fn spawn(app: Option<&gio::Application>) -> Option<Self> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("own-player-{}-{nonce}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&directory).ok()?;
        let path = directory.join("mpv.sock");
        let mut ipc_arg = std::ffi::OsString::from("--input-ipc-server=");
        ipc_arg.push(&path);
        let args = [
            OsStr::new("mpv"),
            OsStr::new("--no-config"),
            OsStr::new("--idle=yes"),
            OsStr::new("--no-video"),
            OsStr::new("--no-terminal"),
            OsStr::new("--audio-display=no"),
            OsStr::new("--force-window=no"),
            ipc_arg.as_os_str(),
            #[cfg(test)]
            OsStr::new("--ao=null"),
        ];
        let launcher = gio::SubprocessLauncher::new(
            gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE,
        );
        // Spawn on the main thread: Linux ties the parent-death signal to the spawning thread.
        let parent_pid = std::process::id() as libc::pid_t;
        launcher.set_child_setup(move || unsafe {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1
                || libc::getppid() != parent_pid
            {
                libc::_exit(127);
            }
        });
        let Ok(child) = launcher.spawn(&args) else {
            let _ = fs::remove_dir(directory);
            return None;
        };

        let cancel_connect = gio::Cancellable::new();
        let cancel_on_exit = cancel_connect.clone();
        let cleanup_path = path.clone();
        // Keep the main context running after the last window closes until GIO has
        // reaped mpv and we have removed the socket directory.
        let hold = app.map(|app| app.hold());
        child.wait_async(gio::Cancellable::NONE, move |_| {
            cancel_on_exit.cancel();
            let _ = fs::remove_file(cleanup_path);
            let _ = fs::remove_dir(directory);
            drop(hold);
        });
        Some(Self {
            child,
            path,
            cancel_connect,
        })
    }

    pub(crate) fn connect_task(&self) -> gio::JoinHandle<Option<UnixStream>> {
        let path = self.path.clone();
        let cancel = self.cancel_connect.clone();
        gio::spawn_blocking(move || Self::connect(&path, &cancel))
    }

    fn connect(path: &Path, cancel: &gio::Cancellable) -> Option<UnixStream> {
        for _ in 0..30 {
            if cancel.is_cancelled() {
                break;
            }
            if let Ok(socket) = UnixStream::connect(path) {
                socket.set_nonblocking(true).ok()?;
                return Some(socket);
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        None
    }
}

impl Drop for MpvProcess {
    fn drop(&mut self) {
        self.cancel_connect.cancel();
        // GIO owns the PID and serializes signaling with its child watch.
        self.child.force_exit();
    }
}

type Dispatch = dyn Fn(IOCondition, bool) -> ControlFlow;

pub(crate) struct Mpv {
    socket: UnixStream,
    event_source: Option<glib::SourceId>,
    write_source: Option<glib::SourceId>,
    dispatch: Option<Rc<Dispatch>>,
    transport_error: Option<io::Error>,
    buffer: Vec<u8>,
    scanned: usize,
    req: u64,
    outgoing: VecDeque<u8>,
    pending: VecDeque<(u64, Reply)>,
    load_returns_entry: bool,
    selection: Selection,
    started: Option<Started>,
    pos: f64,
    sampled_at: Instant,
    pub(crate) duration: f64,
    pub(crate) generation: u64,
    pub(crate) volume: Option<f64>,
    pub(crate) muted: Option<bool>,
    volume_request: Option<u64>,
    mute_request: Option<u64>,
    pub(crate) paused: bool,
    pub(crate) loaded: bool,
    pub(crate) paused_lyric_position: Option<f64>,
    pub(crate) ended: bool,
    pub(crate) failed: bool,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Copy)]
enum Reply {
    Load,
    EntryId(u64),
    Volume,
    Mute,
    Pause(u64),
    Seek(u64),
    ReadPause(u64),
    ReadPosition(u64),
    Command(&'static str),
}

#[derive(Clone, Copy, PartialEq)]
enum Selection {
    Idle,
    Request(u64),
    Entry(u64),
}

#[derive(Clone, Copy)]
struct Started {
    entry: u64,
    loaded: bool,
    end: Option<End>,
}

#[derive(Clone, Copy)]
enum End {
    Eof,
    Error,
    Redirect(u64),
    Stopped,
}

#[derive(Debug, PartialEq)]
enum ReadState {
    Drained,
    More,
}

pub(crate) enum ReadReady {
    Open,
    Closing,
    Disconnected(io::Error),
}

impl Mpv {
    pub(crate) fn new(socket: UnixStream) -> Self {
        Self {
            socket,
            event_source: None,
            write_source: None,
            dispatch: None,
            transport_error: None,
            buffer: Vec::new(),
            scanned: 0,
            req: 0,
            outgoing: VecDeque::new(),
            pending: VecDeque::new(),
            load_returns_entry: false,
            selection: Selection::Idle,
            started: None,
            pos: 0.0,
            sampled_at: Instant::now(),
            duration: 0.0,
            generation: 0,
            volume: None,
            muted: None,
            volume_request: None,
            mute_request: None,
            paused: true,
            loaded: false,
            paused_lyric_position: None,
            ended: false,
            failed: false,
            error: None,
        }
    }

    pub(crate) fn watch(&mut self, dispatch: impl Fn(IOCondition, bool) -> ControlFlow + 'static) {
        let dispatch: Rc<Dispatch> = Rc::new(dispatch);
        let read = dispatch.clone();
        self.event_source = Some(glib::unix_fd_add_local(
            self.socket.as_raw_fd(),
            IOCondition::IN | FD_ERRORS,
            move |_, condition| read(condition, false),
        ));
        self.dispatch = Some(dispatch);
        self.arm_output();
    }

    pub(crate) fn observe(&mut self) {
        for (id, property) in (1..).zip(["time-pos", "duration", "pause", "volume", "mute"]) {
            let _ = self.command(json!(["observe_property", id, property]));
        }
    }

    pub(crate) fn load(&mut self, path: &str) -> io::Result<()> {
        let request = self.queue(json!(["loadfile", path, "replace"]), Reply::Load)?;
        // Older mpv versions return no ID from loadfile. Queue the lookup before
        // any other load or stop on this connection can change playlist entry 0.
        if !self.load_returns_entry {
            self.queue(
                json!(["get_property", "playlist/0/id"]),
                Reply::EntryId(request),
            )?;
        }
        self.generation = self.generation.wrapping_add(1);
        self.reset_playback();
        self.selection = Selection::Request(request);
        self.paused = false;
        Ok(())
    }

    pub(crate) fn has_track(&self) -> bool {
        self.selection != Selection::Idle
    }

    pub(crate) fn command(&mut self, command: Value) -> io::Result<()> {
        let reply = match command[0].as_str() {
            Some("seek") => Reply::Seek(self.generation),
            Some("cycle") => Reply::Pause(self.generation),
            Some("set_property") if command[1] == "pause" => Reply::Pause(self.generation),
            _ => Reply::Command(match (command[0].as_str(), command[1].as_str()) {
                (Some("observe_property"), _) => "подписаться на состояние",
                (Some("stop"), _) => "остановить воспроизведение",
                (Some("set_property"), Some("volume")) => "изменить громкость",
                (Some("set_property"), Some("mute")) => "переключить звук",
                (Some("set_property"), Some("speed")) => "изменить скорость",
                (Some("set_property"), Some("loop-file")) => "установить повтор трека",
                (Some("set_property"), _) => "изменить настройку",
                _ => "выполнить команду",
            }),
        };
        let stop = command[0] == "stop";
        let reset_lyric_position = matches!(reply, Reply::Seek(_)) || command[0] == "cycle";
        self.queue(command, reply)?;
        if reset_lyric_position {
            self.paused_lyric_position = None;
        }
        if stop {
            self.generation = self.generation.wrapping_add(1);
            self.reset_playback();
        }
        Ok(())
    }

    fn reset_playback(&mut self) {
        self.selection = Selection::Idle;
        self.loaded = false;
        self.paused = true;
        self.paused_lyric_position = None;
        self.set_position(0.0);
        self.duration = 0.0;
        self.ended = false;
        self.failed = false;
    }

    fn queue(&mut self, command: Value, reply: Reply) -> io::Result<u64> {
        if let Some(error) = &self.transport_error {
            return Err(error.kind().into());
        }
        let req = self.req + 1;
        let mut frame = serde_json::to_vec(&json!({"command": command, "request_id": req}))
            .expect("serializing a JSON value");
        frame.push(b'\n');
        if self.pending.len() == MAX_REQUESTS || frame.len() > MAX_OUTGOING - self.outgoing.len() {
            let message = "переполнена очередь команд mpv";
            self.fail_transport(io::Error::other(message));
            return Err(io::Error::other(message));
        }
        self.req = req;
        self.pending.push_back((req, reply));
        self.outgoing.extend(frame);
        self.arm_output();
        Ok(req)
    }

    pub(crate) fn set_volume(&mut self, volume: f64) -> io::Result<()> {
        self.command(json!(["set_property", "volume", volume]))?;
        // Read back the actual property after all earlier writes on this socket.
        self.volume_request = Some(self.queue(json!(["get_property", "volume"]), Reply::Volume)?);
        Ok(())
    }

    pub(crate) fn set_muted(&mut self, muted: bool) -> io::Result<()> {
        self.command(json!(["set_property", "mute", muted]))?;
        self.mute_request = Some(self.queue(json!(["get_property", "mute"]), Reply::Mute)?);
        Ok(())
    }

    fn arm_output(&mut self) {
        if self.write_source.is_some() {
            return;
        }
        let Some(dispatch) = self.dispatch.clone() else {
            return;
        };
        self.write_source = if self.transport_error.is_some() {
            Some(glib::idle_add_local_full(
                glib::Priority::DEFAULT,
                move || dispatch(IOCondition::empty(), false),
            ))
        } else if !self.outgoing.is_empty() {
            Some(glib::unix_fd_add_local(
                self.socket.as_raw_fd(),
                IOCondition::OUT,
                move |_, condition| dispatch(condition, true),
            ))
        } else {
            None
        };
    }

    fn fail_transport(&mut self, error: io::Error) {
        if self.transport_error.is_none() {
            self.transport_error = Some(error);
            if let Some(source) = self.write_source.take() {
                source.remove();
            }
            self.arm_output();
        }
    }

    pub(crate) fn write_ready(&mut self, condition: IOCondition) -> ControlFlow {
        // IN owns hangup handling: drain readable bytes before dropping the socket.
        if !condition.intersects(FD_ERRORS) {
            match self.flush() {
                Ok(()) if !self.outgoing.is_empty() => return ControlFlow::Continue,
                Ok(()) => {}
                Err(error) => {
                    self.write_source.take();
                    self.fail_transport(error);
                    return ControlFlow::Break;
                }
            }
        }
        self.write_source.take();
        ControlFlow::Break
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut budget = IO_BYTES;
        for _ in 0..IO_CALLS {
            let bytes = self.outgoing.as_slices().0;
            let len = bytes.len().min(budget);
            if len == 0 {
                break;
            }
            match self.socket.write(&bytes[..len]) {
                Ok(0) => return Err(ErrorKind::WriteZero.into()),
                Ok(written) => {
                    self.outgoing.drain(..written);
                    budget -= written;
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    pub(crate) fn set_position(&mut self, position: f64) {
        self.pos = position;
        self.sampled_at = Instant::now();
    }

    pub(crate) fn position(&self, speed: f64) -> f64 {
        if self.paused || !self.loaded {
            return self.paused_lyric_position.unwrap_or(self.pos);
        }
        // mpv position notifications can be more than 120 ms apart. Continue
        // the audio clock between them, but bound drift if IPC stalls.
        let elapsed = self.sampled_at.elapsed().min(Duration::from_millis(250));
        if self.duration > 0.0 {
            (self.pos + elapsed.as_secs_f64() * speed).min(self.duration)
        } else {
            self.pos
        }
    }

    pub(crate) fn read_ready(&mut self, condition: IOCondition) -> ReadReady {
        let closing = condition.intersects(FD_ERRORS) || self.transport_error.is_some();
        let result = self.read_events();
        let closing = closing || self.transport_error.is_some();
        match result {
            Ok(ReadState::More) if closing => ReadReady::Closing,
            Ok(ReadState::Drained) if closing => ReadReady::Disconnected(
                self.transport_error
                    .take()
                    .unwrap_or_else(|| ErrorKind::ConnectionAborted.into()),
            ),
            Ok(_) => ReadReady::Open,
            Err(error) => ReadReady::Disconnected(self.transport_error.take().unwrap_or(error)),
        }
    }

    fn read_events(&mut self) -> io::Result<ReadState> {
        let mut chunk = [0u8; 4096];
        let mut budget = IO_BYTES;
        let mut outcome = Ok(ReadState::More);
        for _ in 0..IO_CALLS {
            let len = chunk.len().min(budget);
            if len == 0 {
                break;
            }
            match self.socket.read(&mut chunk[..len]) {
                Ok(0) => {
                    outcome = Err(ErrorKind::UnexpectedEof.into());
                    break;
                }
                Ok(n) => {
                    self.buffer.extend_from_slice(&chunk[..n]);
                    budget -= n;
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    outcome = Ok(ReadState::Drained);
                    break;
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => {
                    outcome = Err(error);
                    break;
                }
            }
        }

        // Parse every complete frame already read, even on EOF or a read error.
        // At a budget boundary only the partial frame remains in userspace.
        let mut buffer = std::mem::take(&mut self.buffer);
        let mut consumed = 0;
        for (offset, byte) in buffer[self.scanned..].iter().enumerate() {
            if *byte != b'\n' {
                continue;
            }
            let end = self.scanned + offset + 1;
            if end - consumed > MAX_FRAME {
                outcome = Err(io::Error::new(
                    ErrorKind::InvalidData,
                    "слишком длинное сообщение mpv",
                ));
                break;
            }
            if let Ok(value) = serde_json::from_slice::<Value>(&buffer[consumed..end]) {
                self.handle_message(&value);
            }
            consumed = end;
        }
        if outcome.is_ok() && buffer.len() - consumed >= MAX_FRAME {
            outcome = Err(io::Error::new(
                ErrorKind::InvalidData,
                "слишком длинное сообщение mpv",
            ));
        }
        if outcome.is_err() {
            buffer.clear();
        } else {
            buffer.drain(..consumed);
        }
        self.scanned = buffer.len();
        self.buffer = buffer;
        outcome
    }

    fn handle_message(&mut self, value: &Value) {
        match value["event"].as_str() {
            Some("property-change") => self.handle_property(value),
            Some("start-file") => {
                self.started = value["playlist_entry_id"].as_u64().map(|entry| Started {
                    entry,
                    loaded: false,
                    end: None,
                });
            }
            Some("file-loaded") => {
                // mpv emits this event without an ID, between start-file and end-file.
                if let Some(started) = &mut self.started {
                    started.loaded = true;
                }
                self.apply_started();
            }
            Some("end-file") => {
                if let Some(started) = self
                    .started
                    .as_mut()
                    .filter(|started| value["playlist_entry_id"].as_u64() == Some(started.entry))
                {
                    started.end = Some(match value["reason"].as_str() {
                        Some("eof") => End::Eof,
                        Some("error") => End::Error,
                        Some("redirect") => value["playlist_insert_id"]
                            .as_u64()
                            .map_or(End::Stopped, End::Redirect),
                        _ => End::Stopped,
                    });
                }
                self.apply_started();
            }
            Some("queue-overflow") => self.fail_transport(io::Error::other("mpv потерял события")),
            None => self.handle_reply(value),
            _ => {}
        }
    }

    fn handle_property(&mut self, value: &Value) {
        let data = &value["data"];
        let active = self.started.is_some_and(|started| {
            started.end.is_none() && self.selection == Selection::Entry(started.entry)
        });
        match value["name"].as_str() {
            Some("volume") if self.volume_request.is_none() => self.volume = data.as_f64(),
            Some("mute") if self.mute_request.is_none() => self.muted = data.as_bool(),
            Some("time-pos") if active => {
                if let Some(position) = data.as_f64() {
                    self.set_position(position);
                }
            }
            Some("duration") if active => self.duration = data.as_f64().unwrap_or(0.0),
            Some("pause") if active => {
                if let Some(paused) = data.as_bool() {
                    self.paused = paused;
                    self.sampled_at = Instant::now();
                }
            }
            _ => {}
        }
    }

    fn handle_reply(&mut self, value: &Value) {
        let Some(&(request, reply)) = self.pending.front() else {
            return;
        };
        // JSON IPC writes synchronous replies on this connection in command order.
        if value["request_id"].as_u64() != Some(request) {
            return;
        }
        self.pending.pop_front();
        let Some(error) = value["error"].as_str() else {
            return;
        };
        let success = error == "success";
        let data = &value["data"];
        let action = match reply {
            Reply::Load => {
                let entry = data["playlist_entry_id"].as_u64();
                self.load_returns_entry |= success && entry.is_some();
                if self.selection == Selection::Request(request) {
                    if !success {
                        self.playback_failed();
                    } else if let Some(entry) = entry {
                        self.accept_entry(entry);
                    }
                }
                return;
            }
            Reply::EntryId(load) => {
                if self.selection == Selection::Request(load) {
                    if let Some(entry) = data.as_u64().filter(|_| success) {
                        self.accept_entry(entry);
                    } else {
                        self.playback_failed();
                    }
                }
                return;
            }
            Reply::Volume => {
                if self.volume_request == Some(request) {
                    self.volume_request = None;
                    self.volume = data.as_f64().filter(|_| success);
                }
                "получить громкость"
            }
            Reply::Mute => {
                if self.mute_request == Some(request) {
                    self.mute_request = None;
                    self.muted = data.as_bool().filter(|_| success);
                }
                "получить состояние звука"
            }
            Reply::Pause(generation) if generation == self.generation => {
                if !success {
                    let _ = self.queue(
                        json!(["get_property", "pause"]),
                        Reply::ReadPause(generation),
                    );
                }
                "переключить паузу"
            }
            Reply::Seek(generation) if generation == self.generation => {
                if !success {
                    self.paused_lyric_position = None;
                    let _ = self.queue(
                        json!(["get_property", "time-pos"]),
                        Reply::ReadPosition(generation),
                    );
                }
                "перемотать трек"
            }
            Reply::ReadPause(generation) if generation == self.generation => {
                if let Some(paused) = data.as_bool().filter(|_| success) {
                    self.paused = paused;
                    self.sampled_at = Instant::now();
                }
                return;
            }
            Reply::ReadPosition(generation) if generation == self.generation => {
                if let Some(position) = data.as_f64().filter(|_| success) {
                    self.set_position(position);
                }
                return;
            }
            Reply::Command(action) => action,
            _ => return,
        };
        if !success {
            self.error = Some(format!("mpv не смог {action}: {error}."));
        }
    }

    fn accept_entry(&mut self, entry: u64) {
        self.selection = Selection::Entry(entry);
        self.apply_started();
    }

    fn apply_started(&mut self) {
        let Some(started) = self
            .started
            .filter(|s| self.selection == Selection::Entry(s.entry))
        else {
            return;
        };
        match started.end {
            None => self.loaded = started.loaded,
            Some(End::Eof) => {
                self.selection = Selection::Idle;
                self.ended = true;
                self.loaded = false;
            }
            Some(End::Error) => self.playback_failed(),
            Some(End::Redirect(entry)) => {
                self.selection = Selection::Entry(entry);
                self.loaded = false;
            }
            Some(End::Stopped) => self.reset_playback(),
        }
    }

    fn playback_failed(&mut self) {
        self.reset_playback();
        self.failed = true;
        self.error =
            Some("Не удалось воспроизвести файл. Проверь, что он доступен и не повреждён.".into());
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        if let Some(source) = self.event_source.take() {
            source.remove();
        }
        if let Some(source) = self.write_source.take() {
            source.remove();
        }
    }
}

#[cfg(test)]
mod tests;
