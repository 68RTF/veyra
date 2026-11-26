use super::*;
use std::io::{BufRead, BufReader};
use std::net::Shutdown;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

const FIRST_POSITION: &[u8] =
    b"{\"event\":\"property-change\",\"name\":\"time-pos\",\"data\":1.25}\n";
const SECOND_POSITION: &[u8] =
    b"{\"event\":\"property-change\",\"name\":\"time-pos\",\"data\":2.5}\n";

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "veyra-ipc-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }

    fn audio(&self, name: &str, milliseconds: u32) -> PathBuf {
        let path = self.0.join(name);
        let bytes = 16 * milliseconds;
        let mut wave = Vec::new();
        wave.extend(b"RIFF");
        wave.extend((36 + bytes).to_le_bytes());
        wave.extend(b"WAVEfmt ");
        wave.extend(16_u32.to_le_bytes());
        wave.extend(1_u16.to_le_bytes()); // PCM
        wave.extend(1_u16.to_le_bytes()); // Mono
        wave.extend(8000_u32.to_le_bytes());
        wave.extend(16000_u32.to_le_bytes());
        wave.extend(2_u16.to_le_bytes());
        wave.extend(16_u16.to_le_bytes());
        wave.extend(b"data");
        wave.extend(bytes.to_le_bytes());
        wave.resize(44 + bytes as usize, 0);
        fs::write(&path, wave).unwrap();
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn pair() -> (Mpv, UnixStream) {
    let (socket, peer) = UnixStream::pair().unwrap();
    socket.set_nonblocking(true).unwrap();
    peer.set_nonblocking(true).unwrap();
    let mut player = Mpv::new(socket);
    player.accept_entry(1);
    player.handle_message(&json!({"event": "start-file", "playlist_entry_id": 1}));
    (player, peer)
}

#[test]
fn all_small_chunk_sizes_recover_after_malformed_frames() {
    let bytes = [
        b"not json\n\xff\xfe\n\n".as_slice(),
        FIRST_POSITION,
        b"{\"event\":\n",
        SECOND_POSITION,
    ]
    .concat();
    for size in 1..=32 {
        let (mut player, mut peer) = pair();
        for chunk in bytes.chunks(size) {
            peer.write_all(chunk).unwrap();
            player.read_events().unwrap();
        }
        assert_eq!(player.pos, 2.5, "chunk_size={size}");
        assert!(player.buffer.is_empty());
        assert!(!player.failed);
    }
}

#[test]
fn newline_and_quote_in_path_do_not_split_an_ipc_command() {
    let (mut player, mut peer) = pair();
    let path = "/fixture/Я\n\"👩‍🚀.mp3";
    player
        .command(json!(["loadfile", path, "replace"]))
        .unwrap();
    player.flush().unwrap();
    let mut bytes = [0; 4096];
    let count = peer.read(&mut bytes).unwrap();
    assert_eq!(
        bytes[..count].iter().filter(|byte| **byte == b'\n').count(),
        1
    );
    let value: Value = serde_json::from_slice(&bytes[..count]).unwrap();
    assert_eq!(value["command"][1], path);
}

#[test]
fn multiple_messages_in_one_read() {
    let (mut player, mut peer) = pair();
    peer.write_all(&[FIRST_POSITION, SECOND_POSITION].concat())
        .unwrap();
    player.read_events().unwrap();
    assert_eq!(player.pos, 2.5);
    assert!(player.buffer.is_empty());
}

#[test]
fn message_split_between_reads() {
    let (mut player, mut peer) = pair();
    let (first, rest) = FIRST_POSITION.split_at(FIRST_POSITION.len() - 5);
    peer.write_all(first).unwrap();
    player.read_events().unwrap();
    assert_eq!(player.pos, 0.0);
    peer.write_all(rest).unwrap();
    player.read_events().unwrap();
    assert_eq!(player.pos, 1.25);
    assert!(player.buffer.is_empty());
}

#[test]
fn complete_messages_are_delivered_before_eof_once() {
    let (mut player, mut peer) = pair();
    peer.write_all(&[FIRST_POSITION, SECOND_POSITION].concat())
        .unwrap();
    peer.shutdown(Shutdown::Write).unwrap();
    assert_eq!(
        player.read_events().unwrap_err().kind(),
        ErrorKind::UnexpectedEof
    );
    assert_eq!(player.pos, 2.5);
    player.set_position(0.0);
    assert!(player.read_events().is_err());
    assert_eq!(player.pos, 0.0);
}

#[test]
fn incomplete_message_at_eof_is_discarded() {
    let (mut player, mut peer) = pair();
    peer.write_all(FIRST_POSITION).unwrap();
    peer.write_all(&SECOND_POSITION[..SECOND_POSITION.len() - 3])
        .unwrap();
    drop(peer);
    assert_eq!(
        player.read_events().unwrap_err().kind(),
        ErrorKind::UnexpectedEof
    );
    assert_eq!(player.pos, 1.25);
    assert!(player.buffer.is_empty());
}

#[test]
fn partial_write_keeps_pending_bytes_in_order() {
    let (mut player, mut peer) = pair();
    let fd = player.socket.as_raw_fd();
    let size: libc::c_int = 4096;
    // Force a write smaller than any normal callback budget.
    assert_eq!(
        unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&size as *const libc::c_int).cast(),
                std::mem::size_of_val(&size) as libc::socklen_t,
            )
        },
        0
    );
    let data = (0..65536).map(|i| (i % 251) as u8).collect::<Vec<_>>();
    player.outgoing.extend(&data);
    player.flush().unwrap();
    let remaining = player.outgoing.len();
    assert!((0..data.len()).contains(&remaining) && remaining > 0);
    player.flush().unwrap();
    assert_eq!(player.outgoing.len(), remaining);
    let mut received = Vec::new();
    for _ in 0..100 {
        match peer.read_to_end(&mut received) {
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            result => panic!("unexpected socket result: {result:?}"),
        }
        if received.len() == data.len() {
            break;
        }
        player.flush().unwrap();
    }
    assert_eq!(received, data);
    assert!(player.outgoing.is_empty());
}

struct Peer(BufReader<UnixStream>);

impl Peer {
    fn new(path: &Path) -> Self {
        let stream = UnixStream::connect(path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        Self(BufReader::new(stream))
    }

    fn command(&mut self, id: u64, command: Value) -> Value {
        writeln!(
            self.0.get_mut(),
            "{}",
            json!({"command": command, "request_id": id})
        )
        .unwrap();
        self.until(|value| value["request_id"].as_u64() == Some(id))
    }

    fn until(&mut self, matches: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "mpv event timed out");
            let mut line = String::new();
            assert_ne!(self.0.read_line(&mut line).unwrap(), 0);
            let value = serde_json::from_str(&line).unwrap();
            if matches(&value) {
                return value;
            }
        }
    }
}

fn pump_player(player: &mut Mpv, ready: impl Fn(&Mpv) -> bool) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        player.flush()?;
        player.read_events()?;
        if ready(player) {
            return Ok(());
        }
        assert!(Instant::now() < deadline, "player event timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
#[ignore = "requires mpv"]
fn mpv_switches_over_unstarted_track_and_reports_final_eof() {
    let fixture = Fixture::new();
    let a = fixture.audio("a.wav", 2000);
    let b = fixture.0.join("missing-b.wav");
    let c = fixture.audio("c.wav", 350);
    let process = MpvProcess::spawn(None).unwrap();
    let mut player = Mpv::new(MpvProcess::connect(&process.path, &process.cancel_connect).unwrap());
    let mut control = Peer::new(&process.path);
    let script = fixture.0.join("hold_load.lua");
    fs::write(
        &script,
        r#"
local held
mp.add_hook("on_load", 1, function(hook)
    hook:defer()
    held = hook
    mp.commandv("script-message", "veyra-test-hook")
end)
mp.register_script_message("veyra-test-resume", function()
    held:cont()
    held = nil
end)
mp.commandv("script-message", "veyra-test-ready")
"#,
    )
    .unwrap();
    assert_eq!(
        control.command(1, json!(["load-script", script]))["error"],
        "success"
    );
    control.until(|value| value["args"][0] == "veyra-test-ready");

    player.load(a.to_str().unwrap()).unwrap();
    player.flush().unwrap();
    control.until(|value| value["event"] == "start-file");
    control.until(|value| value["args"][0] == "veyra-test-hook");
    player.load(b.to_str().unwrap()).unwrap();
    player.load(c.to_str().unwrap()).unwrap();
    player.set_volume(36.0).unwrap();
    // The get reply on the player's socket proves both replacements have been applied.
    pump_player(&mut player, |player| player.volume == Some(36.0)).unwrap();
    let latest_entry = control.command(2, json!(["get_property", "playlist/0/id"]))["data"].clone();
    assert_eq!(
        control.command(
            3,
            json!(["script-message-to", "hold_load", "veyra-test-resume"])
        )["error"],
        "success"
    );
    let start = control.until(|value| value["event"] == "start-file");
    let entry = start["playlist_entry_id"].clone();
    assert_eq!(entry, latest_entry, "mpv must skip starting B");
    control.until(|value| value["args"][0] == "veyra-test-hook");
    assert_eq!(
        control.command(
            4,
            json!(["script-message-to", "hold_load", "veyra-test-resume"])
        )["error"],
        "success"
    );
    let end =
        control.until(|value| value["event"] == "end-file" && value["playlist_entry_id"] == entry);
    assert_eq!(end["reason"], "eof");
    pump_player(&mut player, |player| player.ended || player.failed).unwrap();
    assert!(
        !player.failed,
        "the intermediate request must not fail the final track"
    );
    assert!(player.ended, "the final track's EOF was lost");
    stop_and_check_process(process);
}

#[test]
fn disconnect_delivers_volume_to_model() {
    let (player, mut peer) = pair();
    let model = crate::ui_tests::test_model();
    model.borrow_mut().mpv = Some(player);
    peer.write_all(b"{\"event\":\"property-change\",\"name\":\"volume\",\"data\":37}\n")
        .unwrap();
    drop(peer);
    crate::tick_mpv(&model, IOCondition::HUP);
    let state = model.borrow();
    assert!(state.mpv.is_none());
    assert!(state
        .status
        .as_ref()
        .unwrap()
        .contains("Связь с mpv прервалась"));
    assert_eq!(state.volume, 37.0);
}

#[test]
fn reset_after_data_keeps_complete_frames() {
    let (mut player, mut peer) = pair();
    // On Linux the peer closing with unread incoming data causes ECONNRESET.
    player.socket.write_all(b"unread").unwrap();
    peer.write_all(FIRST_POSITION).unwrap();
    drop(peer);
    assert_eq!(
        player.read_events().unwrap_err().kind(),
        ErrorKind::ConnectionReset
    );
    assert_eq!(player.pos, 1.25);
}

#[test]
fn read_budget_leaves_the_next_frame_readable_at_hangup() {
    let (mut player, mut peer) = pair();
    let mut message = SECOND_POSITION[..SECOND_POSITION.len() - 1].to_vec();
    let padding = IO_BYTES - 1 - FIRST_POSITION.len();
    message.resize(padding + 20, b' ');
    message.push(b'\n');
    peer.write_all(&[FIRST_POSITION, &message].concat())
        .unwrap();
    drop(peer);

    assert_eq!(player.read_events().unwrap(), ReadState::More);
    assert_eq!(player.pos, 1.25);
    assert!(!player.buffer.is_empty());
    assert_eq!(
        player.read_events().unwrap_err().kind(),
        ErrorKind::UnexpectedEof
    );
    assert_eq!(player.pos, 2.5);
    assert!(player.buffer.is_empty());
}

#[test]
fn oversized_incoming_frame_aborts_after_delivering_valid_messages() {
    let (mut player, mut peer) = pair();
    peer.write_all(FIRST_POSITION).unwrap();
    for _ in 0..MAX_FRAME / IO_BYTES {
        peer.write_all(&[b' '; IO_BYTES]).unwrap();
        assert_eq!(player.read_events().unwrap(), ReadState::More);
    }
    assert_eq!(
        player.read_events().unwrap_err().kind(),
        ErrorKind::InvalidData
    );
    assert_eq!(player.pos, 1.25);
    assert!(player.buffer.is_empty());
}

#[test]
fn a_stalled_peer_cannot_grow_the_command_queue() {
    let (mut player, _peer) = pair();
    for _ in 0..MAX_REQUESTS {
        player.command(json!(["stop"])).unwrap();
    }
    let bytes = player.outgoing.len();
    assert!(player.command(json!(["stop"])).is_err());
    assert_eq!(player.pending.len(), MAX_REQUESTS);
    assert_eq!(player.outgoing.len(), bytes);
    assert!(player.transport_error.is_some());

    let (mut player, _peer) = pair();
    assert!(player.load(&"a".repeat(MAX_OUTGOING)).is_err());
    assert!(player.outgoing.is_empty());
    assert!(player.pending.is_empty());
}

#[test]
fn stale_load_reply_and_old_file_events_do_not_change_new_selection() {
    let (mut player, _peer) = pair();
    player.load("a.wav").unwrap();
    player.load("b.wav").unwrap();
    assert!(!player.loaded);
    for message in [
        json!({"request_id": 1, "error": "invalid parameter"}),
        json!({"request_id": 2, "error": "property unavailable"}),
        json!({"request_id": 3, "error": "success", "data": {"playlist_entry_id": 2}}),
        json!({"request_id": 4, "error": "success", "data": 2}),
        json!({"event": "property-change", "name": "duration", "data": 99}),
        json!({"event": "end-file", "reason": "error", "playlist_entry_id": 1, "file_error": "loading failed"}),
        json!({"event": "start-file", "playlist_entry_id": 2}),
        json!({"event": "file-loaded"}),
        json!({"event": "property-change", "name": "duration", "data": 10}),
    ] {
        player.handle_message(&message);
    }
    assert!(!player.failed && !player.ended && player.error.is_none());
    assert!(player.loaded);
    assert_eq!(player.duration, 10.0);
    assert!(player.pending.is_empty());
}

#[test]
fn command_rejections_report_action_and_seek_reads_back_position() {
    let (mut player, _peer) = pair();
    for (command, action) in [
        (json!(["observe_property", 1, "time-pos"]), "подписаться"),
        (json!(["set_property", "volume", 37]), "громкость"),
        (json!(["set_property", "mute", true]), "звук"),
        (json!(["set_property", "speed", 2]), "скорость"),
        (json!(["set_property", "loop-file", "inf"]), "повтор"),
        (json!(["stop"]), "остановить"),
    ] {
        player.command(command).unwrap();
        player.handle_message(&json!({"request_id": player.req, "error": "invalid parameter"}));
        assert!(player.error.take().unwrap().contains(action));
        assert!(!player.failed);
    }

    player.command(json!(["cycle", "pause"])).unwrap();
    player.handle_message(&json!({"request_id": player.req, "error": "command failed"}));
    assert!(player.error.take().unwrap().contains("паузу"));
    player.handle_message(&json!({"request_id": player.req, "error": "success", "data": false}));
    assert!(!player.paused);

    player
        .command(json!(["seek", 123, "absolute+exact"]))
        .unwrap();
    player.set_position(123.0);
    player.paused_lyric_position = Some(123.0);
    player.handle_message(&json!({"request_id": player.req, "error": "command failed"}));
    assert!(player.error.take().unwrap().contains("перемотать"));
    player.handle_message(&json!({"request_id": player.req, "error": "success", "data": 2}));
    assert_eq!(player.pos, 2.0);
    assert!(player.paused_lyric_position.is_none());
    assert!(player.pending.is_empty());

    player.load("a.wav").unwrap();
    player.handle_message(&json!({"request_id": player.req - 1, "error": "invalid parameter"}));
    assert!(player.failed);
    assert!(!player.has_track());
    assert!(player.error.take().unwrap().contains("воспроизвести файл"));
}

fn pump_context(ready: impl Fn() -> bool) {
    let context = glib::MainContext::default();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(Instant::now() < deadline, "GLib callback timed out");
        if !context.iteration(false) {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

#[test]
fn fd_sources_stop_on_drain_and_disconnect() {
    let (mut player, mut peer) = pair();
    let context = glib::MainContext::default();
    let model = crate::ui_tests::test_model();
    crate::watch_mpv(&mut player, &model);
    player.set_volume(37.0).unwrap();
    let input = context
        .find_source_by_id(player.event_source.as_ref().unwrap())
        .unwrap();
    let output = context
        .find_source_by_id(player.write_source.as_ref().unwrap())
        .unwrap();
    model.borrow_mut().mpv = Some(player);
    pump_context(|| output.is_destroyed());
    assert!(!input.is_destroyed());
    let mut command_data = Vec::new();
    assert_eq!(
        peer.read_to_end(&mut command_data).unwrap_err().kind(),
        ErrorKind::WouldBlock
    );
    assert!(String::from_utf8(command_data)
        .unwrap()
        .contains("set_property"));

    let mut final_volume =
        b"{\"event\":\"property-change\",\"name\":\"volume\",\"data\":39}".to_vec();
    final_volume.resize(IO_BYTES + 40, b' ');
    final_volume.push(b'\n');
    peer.write_all(b"{\"request_id\":1,\"error\":\"success\"}\n{\"request_id\":2,\"error\":\"success\",\"data\":37}\n")
        .unwrap();
    // Put the active track's EOF before the read budget boundary. The next
    // frame's tail must still arrive via the input source after HUP.
    model.borrow_mut().current = Some(1);
    peer.write_all(b"{\"event\":\"end-file\",\"playlist_entry_id\":1,\"reason\":\"eof\"}\n")
        .unwrap();
    peer.write_all(&final_volume).unwrap();
    drop(peer);
    pump_context(|| model.borrow().volume == 37.0);
    assert!(model.borrow().mpv.as_ref().unwrap().ended);
    pump_context(|| model.borrow().mpv.is_none());
    assert!(input.is_destroyed());
    assert_eq!(model.borrow().volume, 39.0);
}

#[test]
#[ignore = "requires mpv"]
fn mpv_error_from_intermediate_track_does_not_skip_current() {
    let fixture = Fixture::new();
    let a = fixture.audio("a.wav", 10_000);
    let b = fixture.0.join("missing-b.wav");
    let c = fixture.audio("c.wav", 350);
    let process = MpvProcess::spawn(None).unwrap();
    let mut player = Mpv::new(MpvProcess::connect(&process.path, &process.cancel_connect).unwrap());
    let mut control = Peer::new(&process.path);
    player.load(a.to_str().unwrap()).unwrap();
    pump_player(&mut player, |player| player.loaded).unwrap();
    player.load(b.to_str().unwrap()).unwrap();
    player.flush().unwrap();
    control.until(|value| value["event"] == "end-file" && value["reason"] == "error");

    // Leave B's event buffered in the app's socket until C has been selected.
    player.load(c.to_str().unwrap()).unwrap();
    pump_player(&mut player, |player| {
        assert!(!player.failed);
        player.ended
    })
    .unwrap();
    stop_and_check_process(process);
}

fn process_pid(process: &MpvProcess) -> libc::pid_t {
    process.child.identifier().unwrap().parse().unwrap()
}

fn assert_reaped(pid: libc::pid_t, path: &Path) {
    pump_context(|| !path.parent().unwrap().exists());
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
}

fn stop_and_check_process(process: MpvProcess) {
    let pid = process_pid(&process);
    let path = process.path.clone();
    drop(process);
    assert_reaped(pid, &path);
}

#[test]
#[ignore = "requires mpv"]
fn mpv_unexpected_exit_during_playback_is_reaped() {
    let fixture = Fixture::new();
    let track = fixture.audio("playing.wav", 10_000);
    let process = MpvProcess::spawn(None).unwrap();
    let pid = process_pid(&process);
    let path = process.path.clone();
    let socket = MpvProcess::connect(&path, &process.cancel_connect).unwrap();
    let model = crate::ui_tests::test_model();
    let mut player = Mpv::new(socket);
    crate::watch_mpv(&mut player, &model);
    player.observe();
    player.load(track.to_str().unwrap()).unwrap();
    model.borrow_mut().mpv = Some(player);
    model.borrow_mut().mpv_process = Some(process);

    pump_context(|| model.borrow().mpv.as_ref().unwrap().loaded);
    assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
    pump_context(|| model.borrow().mpv.is_none());
    assert!(model.borrow().mpv_process.is_none());
    assert!(model
        .borrow()
        .status
        .as_ref()
        .unwrap()
        .contains("Связь с mpv прервалась"));
    assert_reaped(pid, &path);
}

#[test]
#[ignore = "requires mpv"]
fn mpv_shutdown_cancels_pending_connection_and_reaps_process() {
    let process = MpvProcess::spawn(None).unwrap();
    let connected = process.connect_task();
    let cancelled = process.cancel_connect.clone();
    stop_and_check_process(process);
    assert!(cancelled.is_cancelled());
    // The connect task may already have returned a socket just before cancellation.
    // It holds neither the process nor the cleanup callback.
    drop(glib::MainContext::default().block_on(connected).unwrap());
}

#[test]
fn legacy_playlist_lookup_accepts_an_eof_buffered_before_the_reply() {
    let (mut player, _peer) = pair();
    player.load("short.wav").unwrap();
    for message in [
        json!({"request_id": 1, "error": "success", "data": null}),
        json!({"event": "start-file", "playlist_entry_id": 2}),
        json!({"event": "file-loaded"}),
        json!({"event": "end-file", "reason": "eof", "playlist_entry_id": 2}),
    ] {
        player.handle_message(&message);
    }
    assert!(!player.ended && !player.failed);
    player.handle_message(&json!({"request_id": 2, "error": "success", "data": 2}));
    assert!(player.ended);
    assert!(!player.has_track());
}

#[test]
#[ignore = "requires mpv"]
fn mpv_pause_seek_stop_and_repeat_still_work() {
    let fixture = Fixture::new();
    let track = fixture.audio("control.wav", 3000);
    let process = MpvProcess::spawn(None).unwrap();
    let mut player = Mpv::new(MpvProcess::connect(&process.path, &process.cancel_connect).unwrap());
    let mut control = Peer::new(&process.path);
    player.observe();
    player
        .command(json!(["set_property", "loop-file", "inf"]))
        .unwrap();
    player.load(track.to_str().unwrap()).unwrap();
    pump_player(&mut player, |player| player.loaded).unwrap();

    player
        .command(json!(["set_property", "pause", true]))
        .unwrap();
    pump_player(&mut player, |player| player.paused).unwrap();
    player
        .command(json!(["seek", 2.5, "absolute+exact"]))
        .unwrap();
    pump_player(&mut player, |player| (player.pos - 2.5).abs() < 0.1).unwrap();

    player.command(json!(["cycle", "pause"])).unwrap();
    pump_player(&mut player, |player| !player.paused).unwrap();
    pump_player(&mut player, |player| {
        assert!(!player.ended && !player.failed);
        player.pos < 0.75
    })
    .unwrap();

    player.command(json!(["stop"])).unwrap();
    pump_player(&mut player, |player| player.pending.is_empty()).unwrap();
    control.until(|value| value["event"] == "end-file" && value["reason"] == "stop");
    assert!(!player.has_track() && !player.loaded && player.error.is_none());
    stop_and_check_process(process);
}

#[test]
fn playback_clock_advances_between_sparse_position_events() {
    let (mut player, _peer) = pair();
    player.loaded = true;
    player.paused = false;
    player.duration = 100.0;
    player.set_position(10.0);
    player.sampled_at = Instant::now() - Duration::from_millis(200);
    assert!((player.position(1.0) - 10.2).abs() < 0.01);
    assert!((player.position(2.0) - 10.4).abs() < 0.02);
    player.paused = true;
    assert_eq!(player.position(1.0), 10.0);
    player.paused = false;
    player.set_position(3.0);
    assert!((player.position(1.0) - 3.0).abs() < 0.01);
    player.sampled_at = Instant::now() - Duration::from_secs(10);
    assert!(
        player.position(1.0) <= 3.251,
        "a stalled IPC clock must not run indefinitely"
    );
}

#[test]
#[ignore = "requires mpv"]
fn mpv_word_timeline_follows_speed_pause_and_seek() {
    let fixture = Fixture::new();
    let track = fixture.audio("word-clock.wav", 8000);
    let process = MpvProcess::spawn(None).unwrap();
    let mut player = Mpv::new(MpvProcess::connect(&process.path, &process.cancel_connect).unwrap());
    player.observe();
    player.load(track.to_str().unwrap()).unwrap();
    pump_player(&mut player, |p| p.loaded && p.pos > 0.1).unwrap();
    player
        .command(json!(["set_property", "speed", 2.0]))
        .unwrap();
    pump_player(&mut player, |p| p.pending.is_empty()).unwrap();
    let start = Instant::now();
    pump_player(&mut player, |_| {
        start.elapsed() >= Duration::from_millis(250)
    })
    .unwrap();
    let mut source = Peer::new(&process.path);
    let actual = source.command(1, json!(["get_property", "time-pos"]))["data"]
        .as_f64()
        .unwrap();
    player.read_events().unwrap();
    let shown = player.position(2.0);
    assert!((shown-actual).abs()<0.08,
        "display clock must follow mpv, including a speed-change transient: shown={shown} actual={actual}");
    player
        .command(json!(["set_property", "pause", true]))
        .unwrap();
    pump_player(&mut player, |p| p.paused).unwrap();
    player
        .command(json!(["seek", 1.5, "absolute+exact"]))
        .unwrap();
    pump_player(&mut player, |p| (p.pos - 1.5).abs() < 0.02).unwrap();
    let timeline = crate::lyric_timing::Timeline::new("<00:01>First <00:02>second", 1.0, 4.0, 6.5);
    let mut attrs = Vec::new();
    timeline.fill_alphas(player.position(2.0), &mut attrs);
    assert!(attrs
        .iter()
        .any(|&(start, _, alpha)| start == 6 && alpha == crate::lyric_timing::DIM));
    player
        .command(json!(["set_property", "speed", 0.5]))
        .unwrap();
    player
        .command(json!(["set_property", "pause", false]))
        .unwrap();
    pump_player(&mut player, |p| !p.paused && p.pos >= 2.1).unwrap();
    timeline.fill_alphas(player.position(0.5), &mut attrs);
    assert!(attrs.is_empty());
    player.command(json!(["stop"])).unwrap();
    pump_player(&mut player, |p| !p.has_track()).unwrap();
    stop_and_check_process(process);
}
