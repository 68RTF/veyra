use crate::{Lyric, Track};
use gtk::gio;
use gtk::gio::prelude::*;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
// Give a fast word-synchronised LRCLIB answer a chance to cancel LyricsPlus
// before it leaves the process. A slower provider is still started in parallel.
const PROVIDER_STAGGER: Duration = Duration::from_millis(140);
// Give LyricsPlus a chance to upgrade before spending an exact LRCLIB lookup.
const FALLBACK_DELAY: Duration = Duration::from_millis(150);
const CANCELLED: &str = "Поиск текста отменён";
const LYRICS_PLUS: &str = "https://lyricsplus.prjktla.my.id/v2/lyrics/get";

#[cfg(test)]
pub(crate) fn online_lyrics(track: &Track) -> Result<Vec<Lyric>, String> {
    online_lyrics_with(track, Arc::new(AtomicBool::new(false)), &mut |_| {})
}

/// Searches for the best available lyrics and reports every quality upgrade as
/// soon as it arrives. The callback runs on the calling worker thread; the GTK
/// layer forwards it to the main loop and never waits for the final result to
/// display an already usable text.
pub(crate) fn online_lyrics_with(
    track: &Track,
    cancel: Arc<AtomicBool>,
    progress: &mut dyn FnMut(&[Lyric]),
) -> Result<Vec<Lyric>, String> {
    let started = Instant::now();
    let mut search = Search {
        best: track.lyrics.clone(),
        best_quality: crate::lyric_timing::quality(&track.lyrics),
        failed: false,
        lrclib_available: true,
        progress,
        started,
    };
    trace(format!(
        "lookup start quality={} title={:?}",
        search.best_quality, track.title
    ));

    // Disk cache first: a warm result must never wait for a socket.
    if let Some(cached) = read_cache(track) {
        let quality = crate::lyric_timing::quality(&cached);
        if crate::lyric_timing::rank(&cached) > crate::lyric_timing::rank(&search.best) {
            search.best = cached;
            search.best_quality = quality;
            (search.progress)(&search.best);
            trace(format!(
                "cache hit quality={quality} at {} ms",
                started.elapsed().as_millis()
            ));
        }
    }
    if search.best_quality >= 3 {
        trace(format!(
            "done quality=3 from cache at {} ms",
            started.elapsed().as_millis()
        ));
        return Ok(search.best);
    }
    if let Some(cached) = read_lookup(track) {
        let quality = crate::lyric_timing::quality(&cached.lines);
        if crate::lyric_timing::rank(&cached.lines) > crate::lyric_timing::rank(&search.best) {
            search.best = cached.lines;
            search.best_quality = quality;
            (search.progress)(&search.best);
        }
        trace(format!(
            "lookup cache quality={} at {} ms",
            search.best_quality,
            started.elapsed().as_millis()
        ));
        return if search.best.is_empty() {
            Err(cached
                .error
                .unwrap_or_else(|| "Подходящий текст пока не найден".into()))
        } else {
            Ok(search.best)
        };
    }

    let title = track.title.trim();
    if title.is_empty() {
        return Err("У трека нет названия для поиска текста.".into());
    }
    let artist = track.artist.trim();
    let known_artist = !artist.is_empty() && artist != "Неизвестный исполнитель";
    let track_duration = track.duration;

    // Search all matching recordings first: the exact endpoint may choose a
    // weaker revision even when another matching record already has word cues.
    // Keep exact lookup as a fallback, with the same identity/duration checks.
    let provider_cancel = Arc::new(AtomicBool::new(cancel.load(Ordering::Relaxed)));
    let providers_started = Instant::now();
    let mut primary_rx = None;
    if known_artist {
        let cancel = provider_cancel.clone();
        let title = title.to_owned();
        let artist = artist.to_owned();
        primary_rx = Some(spawn_provider(move || {
            let params = [
                ("track_name", title.as_str()),
                ("artist_name", artist.as_str()),
            ];
            let value = request("search", &params, &cancel)?;
            Ok(value.and_then(|value| {
                value.as_array().and_then(|candidates| {
                    choose_candidate(candidates, &title, &artist, track_duration)
                })
            }))
        }));
    }
    let mut lyricsplus_started = !known_artist;
    let mut lyricsplus_rx = None;
    let mut primary_finished = primary_rx.is_none().then(Instant::now);
    let mut queries = Vec::new();
    if known_artist && track.duration > 0.0 {
        queries.push((
            "get",
            vec![
                ("track_name".to_owned(), title.to_owned()),
                ("artist_name".to_owned(), artist.to_owned()),
                ("album_name".to_owned(), track.album.trim().to_owned()),
                ("duration".to_owned(), track.duration.round().to_string()),
            ],
        ));
    } else if !known_artist {
        queries.push(("search", vec![("track_name".to_owned(), title.to_owned())]));
    }
    if short_title(title) != title {
        queries.push((
            "search",
            vec![("track_name".to_owned(), short_title(title).to_owned())],
        ));
    }
    let mut queries = queries.into_iter().peekable();
    let mut fallback_rx = None;
    loop {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if search.poll_provider(&mut primary_rx, false) {
            primary_finished = Some(Instant::now());
        }
        if search.best_quality >= 3 {
            break;
        }
        if !lyricsplus_started
            && (primary_rx.is_none() || providers_started.elapsed() >= PROVIDER_STAGGER)
        {
            lyricsplus_started = true;
            let cancel = provider_cancel.clone();
            let track = track.clone();
            lyricsplus_rx = Some(spawn_provider(move || {
                let duration = track.duration.to_string();
                let params = [
                    ("title", track.title.trim()),
                    ("artist", track.artist.trim()),
                    ("album", track.album.trim()),
                    ("duration", duration.as_str()),
                ];
                let value = request_url(LYRICS_PLUS, &params, &cancel)?;
                Ok(value.and_then(|value| crate::lyric_sources::lyricsplus(&value, &track)))
            }));
        }
        search.poll_provider(&mut lyricsplus_rx, false);
        if search.best_quality >= 3 {
            break;
        }
        let fallback_ready = lyricsplus_started && lyricsplus_rx.is_none()
            || search.best.is_empty()
            || primary_finished.is_some_and(|at| at.elapsed() >= FALLBACK_DELAY);
        if primary_rx.is_none()
            && search.lrclib_available
            && fallback_ready
            && fallback_rx.is_none()
        {
            if let Some((endpoint, params)) = queries.next() {
                let cancel = provider_cancel.clone();
                let title = title.to_owned();
                let artist = artist.to_owned();
                fallback_rx = Some(spawn_provider(move || {
                    let params: Vec<_> = params
                        .iter()
                        .map(|(key, value)| (key.as_str(), value.as_str()))
                        .collect();
                    let value = request(endpoint, &params, &cancel)?;
                    Ok(value.and_then(|value| {
                        if let Some(candidates) = value.as_array() {
                            choose_candidate(candidates, &title, &artist, track_duration)
                        } else {
                            choose_candidate(&[value], &title, &artist, track_duration)
                        }
                    }))
                }));
            }
        }
        search.poll_provider(&mut fallback_rx, true);
        if search.best_quality >= 3
            || primary_rx.is_none()
                && lyricsplus_started
                && lyricsplus_rx.is_none()
                && fallback_rx.is_none()
                && (!search.lrclib_available || queries.peek().is_none())
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // Release queued/blocked requests that are no longer needed. This is
    // separate from caller cancellation so a successful result is still cached.
    provider_cancel.store(true, Ordering::Relaxed);

    let elapsed = started.elapsed().as_millis();
    if cancel.load(Ordering::Relaxed) {
        // Keep a partial improvement for the next visit, but do not seal a
        // 7-day lookup entry from an interrupted search.
        if !search.best.is_empty() {
            if crate::lyric_timing::rank(&search.best) > crate::lyric_timing::rank(&track.lyrics) {
                write_cache(track, &search.best);
            }
            trace(format!(
                "cancelled quality={} at {elapsed} ms",
                search.best_quality
            ));
            return Ok(search.best);
        }
        trace(format!("cancelled with no text at {elapsed} ms"));
        return Err(CANCELLED.into());
    }

    let error = if search.best.is_empty() {
        Some(
            if search.failed {
                "Поиск временно недоступен. Попробуй позже."
            } else {
                "Подходящий текст не найден. Можно добавить .lrc рядом с треком."
            }
            .to_owned(),
        )
    } else {
        None
    };
    write_lookup(track, &search.best, error.clone(), search.failed);
    if !search.best.is_empty() {
        if crate::lyric_timing::rank(&search.best) > crate::lyric_timing::rank(&track.lyrics) {
            write_cache(track, &search.best);
        }
        trace(format!(
            "done quality={} at {elapsed} ms",
            search.best_quality
        ));
        Ok(search.best)
    } else {
        trace(format!("empty at {elapsed} ms failed={}", search.failed));
        Err(error.unwrap())
    }
}

struct Search<'a> {
    best: Vec<Lyric>,
    best_quality: u8,
    failed: bool,
    lrclib_available: bool,
    progress: &'a mut dyn FnMut(&[Lyric]),
    started: Instant,
}

impl Search<'_> {
    fn poll_provider(&mut self, receiver: &mut Option<ProviderReceiver>, lrclib: bool) -> bool {
        let Some(pending) = receiver.as_ref() else {
            return false;
        };
        let result = match pending.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => Err("Поиск текста прервался".into()),
        };
        receiver.take();
        match result {
            Ok(Some(lines)) => self.accept(lines),
            Ok(None) => (),
            Err(error) if error == CANCELLED => (),
            Err(_) => {
                self.failed = true;
                if lrclib {
                    self.lrclib_available = false;
                }
            }
        }
        true
    }

    fn accept(&mut self, lines: Vec<Lyric>) {
        let rank = crate::lyric_timing::rank(&lines);
        let quality = rank.0;
        if rank > crate::lyric_timing::rank(&self.best) {
            self.best = lines;
            self.best_quality = quality;
            (self.progress)(&self.best);
            trace(format!(
                "emit quality={quality} at {} ms",
                self.started.elapsed().as_millis()
            ));
        }
    }
}

type ProviderReceiver = mpsc::Receiver<Result<Option<Vec<Lyric>>, String>>;

fn spawn_provider<T, F>(f: F) -> mpsc::Receiver<Result<Option<T>, String>>
where
    T: Send + 'static,
    F: FnOnce() -> Result<Option<T>, String> + Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(f());
    });
    receiver
}

fn trace(message: impl AsRef<str>) {
    if std::env::var_os("VEYRA_LYRICS_TIMING").is_some() {
        eprintln!("[lyrics] {}", message.as_ref());
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct LookupCache {
    identity: (String, String, String, u64),
    expires: u64,
    lines: Vec<Lyric>,
    error: Option<String>,
    #[serde(default)]
    quality: Option<u8>,
}

impl LookupCache {
    fn timing_is_current(&self) -> bool {
        let quality = crate::lyric_timing::quality(&self.lines);
        if !self.lines.is_empty() && quality == 0 {
            return false;
        }
        self.quality.map_or_else(
            || {
                // Older caches could assign a week-long TTL to partial or
                // invalid word cues. Recheck those once under the new rules.
                quality >= 3
                    || !self
                        .lines
                        .iter()
                        .any(|line| !crate::lyric_timing::markup(&line.text).1.is_empty())
            },
            |cached_quality| cached_quality == quality,
        )
    }
}
fn identity(track: &Track) -> (String, String, String, u64) {
    (
        track.title.clone(),
        track.artist.clone(),
        track.album.clone(),
        track.duration.max(0.0).round() as u64,
    )
}
fn lookup_path(track: &Track) -> Option<PathBuf> {
    Some(cache_path(track)?.with_extension("lookup-v3.json"))
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |t| t.as_secs())
}
fn read_lookup(track: &Track) -> Option<LookupCache> {
    let cached: LookupCache = serde_json::from_slice(&fs::read(lookup_path(track)?).ok()?).ok()?;
    (cached.identity == identity(track) && cached.expires > now() && cached.timing_is_current())
        .then_some(cached)
}
fn write_lookup(track: &Track, lines: &[Lyric], error: Option<String>, failed: bool) {
    if let Some(path) = lookup_path(track) {
        let ttl = if failed {
            15 * 60
        } else if lines.is_empty() {
            24 * 3600
        } else if crate::lyric_timing::quality(lines) >= 3 {
            7 * 24 * 3600
        } else {
            // A line-level result is useful immediately, but re-check sooner
            // so newer word-level cues can replace it.
            24 * 3600
        };
        let cached = LookupCache {
            identity: identity(track),
            expires: now() + ttl,
            lines: lines.into(),
            error,
            quality: Some(crate::lyric_timing::quality(lines)),
        };
        if fs::create_dir_all(path.parent().unwrap()).is_ok() {
            if let Ok(data) = serde_json::to_vec(&cached) {
                atomic_write(&path, &data);
            }
        }
    }
}
pub(crate) fn clear_lookup(track: &Track) {
    if let Some(path) = lookup_path(track) {
        let _ = fs::remove_file(path);
    }
}

fn request(
    endpoint: &str,
    params: &[(&str, &str)],
    cancel: &Arc<AtomicBool>,
) -> Result<Option<Value>, String> {
    request_url(
        &format!("https://lrclib.net/api/{endpoint}"),
        params,
        cancel,
    )
}

fn request_url(
    url: &str,
    params: &[(&str, &str)],
    cancel: &Arc<AtomicBool>,
) -> Result<Option<Value>, String> {
    let host = url.split('/').nth(2).unwrap_or("").to_owned();
    let cooldowns = backoff();
    if cooldowns
        .lock()
        .unwrap()
        .get(&host)
        .is_some_and(|until| *until > Instant::now())
    {
        return Err("Источник временно ограничил запросы".into());
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(CANCELLED.into());
    }
    let reply = fetch(url, params, cancel)?;
    if cancel.load(Ordering::Relaxed) {
        return Err(CANCELLED.into());
    }
    let connect = reply
        .connect_ms
        .map_or_else(|| "reused".to_string(), |ms| format!("{ms} ms"));
    trace(format!(
        "http host={host} status={} total={} ms connect={connect} bytes={}",
        reply.status,
        reply.total_ms,
        reply.body.len()
    ));
    match reply.status {
        200 => serde_json::from_str(&reply.body)
            .map(Some)
            .map_err(|error| format!("Не удалось прочитать ответ LRCLIB: {error}")),
        404 => Ok(None),
        429 => {
            cooldowns.lock().unwrap().insert(
                host,
                Instant::now()
                    + Duration::from_secs(
                        reply.retry_after.unwrap_or(3600).clamp(60, 7 * 24 * 3600),
                    ),
            );
            Err("Источник временно ограничил запросы. Попробуй позже.".into())
        }
        code => Err(format!("Сервис текстов ответил с ошибкой HTTP {code}.")),
    }
}

fn backoff() -> &'static Mutex<std::collections::HashMap<String, Instant>> {
    static BACKOFF: OnceLock<Mutex<std::collections::HashMap<String, Instant>>> = OnceLock::new();
    BACKOFF.get_or_init(Default::default)
}

struct Reply {
    status: u16,
    body: String,
    retry_after: Option<u64>,
    connect_ms: Option<u128>,
    total_ms: u128,
}

fn fetch(url: &str, params: &[(&str, &str)], cancel: &Arc<AtomicBool>) -> Result<Reply, String> {
    if use_curl() {
        fetch_curl(url, params, cancel.as_ref())
    } else {
        fetch_gio(&build_url(url, params), cancel)
    }
}

fn build_url(base: &str, params: &[(&str, &str)]) -> String {
    let mut url = String::with_capacity(base.len() + 64);
    url.push_str(base);
    let mut separator = if base.contains('?') { '&' } else { '?' };
    for (name, value) in params {
        url.push(separator);
        separator = '&';
        url.push_str(&encode_component(name));
        url.push('=');
        url.push_str(&encode_component(value));
    }
    url
}

fn encode_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// The UI tests replace `curl` with a deterministic fixture. Production uses
/// GIO so DNS answers and TLS connections stay in the pool between tracks.
fn use_curl() -> bool {
    if std::env::var_os("OWN_PLAYER_CURL_LOG").is_some() {
        return true;
    }
    match std::env::var("VEYRA_HTTP_BACKEND").as_deref() {
        Ok("curl") => true,
        Ok("gio") => false,
        _ => !gio::TlsBackend::default().supports_tls(),
    }
}

fn fetch_curl(url: &str, params: &[(&str, &str)], cancel: &AtomicBool) -> Result<Reply, String> {
    let started = Instant::now();
    let mut command = Command::new("curl");
    command.args([
        "--silent",
        "--show-error",
        "--location",
        "--max-time",
        "8",
        "--connect-timeout",
        "3",
        "--max-filesize",
        "2097152",
        "--user-agent",
        "Veyra/0.8.0 (https://github.com/68RTF/veyra)",
        "--get",
    ]);
    for (name, value) in params {
        command
            .arg("--data-urlencode")
            .arg(format!("{name}={value}"));
    }
    let output = command
        .arg("--write-out")
        .arg("\n%{http_code}\n%header{retry-after}")
        .arg(url)
        .output()
        .map_err(|error| format!("Не удалось запустить поиск текста: {error}"))?;
    if cancel.load(Ordering::Relaxed) {
        return Err(CANCELLED.into());
    }
    if !output.status.success() {
        return Err(format!(
            "Нет связи с сервисом текстов: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let body = String::from_utf8(output.stdout)
        .map_err(|_| "Сервис текстов вернул ответ в неверной кодировке.".to_string())?;
    let (json, status, retry_after) =
        http_reply(&body).ok_or_else(|| "Сервис текстов вернул неполный ответ.".to_string())?;
    let status = status
        .trim()
        .parse::<u16>()
        .map_err(|_| "Сервис текстов вернул неверный статус.".to_string())?;
    Ok(Reply {
        status,
        body: json.to_owned(),
        retry_after,
        connect_ms: None,
        total_ms: started.elapsed().as_millis(),
    })
}

fn http_reply(body: &str) -> Option<(&str, &str, Option<u64>)> {
    let mut parts = body.rsplitn(3, '\n');
    let retry = parts.next()?;
    let status = parts.next()?;
    if status.trim().len() == 3
        && status
            .trim()
            .parse::<u16>()
            .is_ok_and(|code| (100..=599).contains(&code))
    {
        return Some((parts.next()?, status, retry.trim().parse().ok()));
    }
    // The deterministic curl fixture and older curl adapters emit just status.
    let (body, status) = body.rsplit_once('\n')?;
    Some((body, status, None))
}

struct HttpJob {
    host: String,
    port: u16,
    path: String,
    cancel: Arc<AtomicBool>,
    cancellable: gio::Cancellable,
    reply: mpsc::Sender<Result<Reply, String>>,
}

static HOSTS: OnceLock<Mutex<std::collections::HashMap<String, mpsc::Sender<HttpJob>>>> =
    OnceLock::new();

fn host_sender(host: &str, port: u16) -> mpsc::Sender<HttpJob> {
    let key = format!("{host}:{port}");
    let hosts = HOSTS.get_or_init(|| Mutex::new(Default::default()));
    let mut hosts = hosts.lock().unwrap();
    hosts
        .entry(key)
        .or_insert_with(|| {
            let (sender, receiver) = mpsc::channel();
            std::thread::spawn(move || http_worker(receiver));
            sender
        })
        .clone()
}

/// One long-lived worker per host owns its TLS connection. Keeping the
/// connection out of a shared pool avoids sending GIO objects across threads
/// while still reusing DNS answers, the TCP socket and the TLS session.
fn http_worker(receiver: mpsc::Receiver<HttpJob>) {
    let mut connection: Option<gio::SocketConnection> = None;
    while let Ok(job) = receiver.recv() {
        if job.cancel.load(Ordering::Relaxed) || job.cancellable.is_cancelled() {
            let _ = job.reply.send(Err(CANCELLED.into()));
            continue;
        }
        let mut result = Err("Нет связи с сервисом текстов.".to_string());
        let mut opened_ms = None;
        for attempt in 0..2 {
            if job.cancel.load(Ordering::Relaxed) || job.cancellable.is_cancelled() {
                result = Err(CANCELLED.into());
                break;
            }
            if connection.is_none() {
                let opened = Instant::now();
                match open_connection(&job.host, job.port, &job.cancellable) {
                    Ok(open) => {
                        opened_ms = Some(opened.elapsed().as_millis());
                        connection = Some(open);
                    }
                    Err(error) => {
                        result = Err(error);
                        break;
                    }
                }
            }
            let Some(open) = connection.as_ref() else {
                break;
            };
            match exchange(open, &job.host, &job.path, &job.cancel, &job.cancellable) {
                Ok((mut reply, keep_alive)) => {
                    reply.connect_ms = opened_ms.take();
                    if !keep_alive {
                        connection = None;
                    }
                    result = Ok(reply);
                    break;
                }
                Err(error) => {
                    connection = None;
                    result = Err(error);
                    // A keep-alive socket can be closed while idle. Every
                    // lyrics request is a GET, so one fresh retry is safe.
                    if attempt == 1
                        || job.cancel.load(Ordering::Relaxed)
                        || job.cancellable.is_cancelled()
                    {
                        break;
                    }
                }
            }
        }
        let _ = job.reply.send(result);
    }
}

fn open_connection(
    host: &str,
    port: u16,
    cancellable: &gio::Cancellable,
) -> Result<gio::SocketConnection, String> {
    let client = gio::SocketClient::new();
    client.set_tls(true);
    client.set_tls_validation_flags(gio::TlsCertificateFlags::VALIDATE_ALL);
    client.set_timeout(CONNECT_TIMEOUT.as_secs() as u32);
    let connection = client
        .connect_to_host(&format!("{host}:{port}"), port, Some(cancellable))
        .map_err(|error| format!("Нет связи с сервисом текстов: {error}"))?;
    connection
        .socket()
        .set_timeout(REQUEST_TIMEOUT.as_secs() as u32);
    Ok(connection)
}

fn fetch_gio(url: &str, cancel: &Arc<AtomicBool>) -> Result<Reply, String> {
    let (host, port, path) =
        split_https(url).ok_or_else(|| "Сервис текстов вернул неверный адрес.".to_string())?;
    let started = Instant::now();
    let (reply_sender, reply_receiver) = mpsc::channel();
    let cancellable = gio::Cancellable::new();
    host_sender(&host, port)
        .send(HttpJob {
            host,
            port,
            path,
            cancel: cancel.clone(),
            cancellable: cancellable.clone(),
            reply: reply_sender,
        })
        .map_err(|_| "Служба загрузки текста остановилась.".to_string())?;
    let mut reply = wait_for_reply(
        &reply_receiver,
        cancel,
        &cancellable,
        CONNECT_TIMEOUT + REQUEST_TIMEOUT + Duration::from_secs(1),
    )?;
    reply.total_ms = started.elapsed().as_millis();
    Ok(reply)
}

fn wait_for_reply(
    receiver: &mpsc::Receiver<Result<Reply, String>>,
    cancel: &AtomicBool,
    cancellable: &gio::Cancellable,
    timeout: Duration,
) -> Result<Reply, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if cancel.load(Ordering::Relaxed) {
            cancellable.cancel();
            return Err(CANCELLED.into());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            cancellable.cancel();
            return Err("Сервис текстов не ответил вовремя.".into());
        }
        match receiver.recv_timeout(remaining.min(Duration::from_millis(20))) {
            Ok(result) => return result,
            Err(mpsc::RecvTimeoutError::Timeout) => (),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("Служба загрузки текста остановилась.".into())
            }
        }
    }
}

fn split_https(url: &str) -> Option<(String, u16, String)> {
    let rest = url.strip_prefix("https://")?;
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(']') => (host, port.parse().ok()?),
        _ => (authority, 443),
    };
    Some((host.to_owned(), port, path.to_owned()))
}

fn exchange(
    connection: &gio::SocketConnection,
    host: &str,
    path: &str,
    cancel: &AtomicBool,
    cancellable: &gio::Cancellable,
) -> Result<(Reply, bool), String> {
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: Veyra/0.8.0 (https://github.com/68RTF/veyra)\r\nAccept: application/json\r\nAccept-Encoding: gzip\r\nConnection: keep-alive\r\n\r\n"
    );
    {
        let output = connection.output_stream();
        output
            .write_all(request.as_bytes(), Some(cancellable))
            .map_err(|error| format!("Нет связи с сервисом текстов: {error}"))?;
        output
            .flush(Some(cancellable))
            .map_err(|error| format!("Нет связи с сервисом текстов: {error}"))?;
    }
    let input = connection.input_stream().clone();
    let mut reader = ResponseReader {
        input,
        buffer: Vec::new(),
        position: 0,
        cancel,
        cancellable,
    };
    let header_end = loop {
        if let Some(index) = find_bytes(&reader.buffer, b"\r\n\r\n") {
            break index;
        }
        if reader.buffer.len() > 64 * 1024 {
            return Err("Сервис текстов вернул слишком большой заголовок.".into());
        }
        if !reader.fill()? {
            return Err("Сервис текстов закрыл соединение до ответа.".into());
        }
    };
    let head = String::from_utf8_lossy(&reader.buffer[..header_end]).into_owned();
    reader.position = header_end + 4;
    let mut lines = head.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| "Сервис текстов вернул неполный ответ.".to_string())?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|status| status.parse::<u16>().ok())
        .ok_or_else(|| "Сервис текстов вернул неверный статус.".to_string())?;
    let mut content_length = None;
    let mut chunked = false;
    let mut keep_alive = true;
    let mut retry_after = None;
    let mut encoding = String::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => content_length = value.trim().parse::<usize>().ok(),
            "transfer-encoding" => {
                chunked = value.to_ascii_lowercase().contains("chunked");
            }
            "connection" => {
                keep_alive = !value.to_ascii_lowercase().contains("close");
            }
            "retry-after" => retry_after = value.trim().parse::<u64>().ok(),
            "content-encoding" => encoding = value.trim().to_ascii_lowercase(),
            _ => (),
        }
    }
    let body = if chunked {
        reader.read_chunked()?
    } else if let Some(length) = content_length {
        if length > MAX_RESPONSE_BYTES {
            return Err("Сервис текстов вернул слишком большой ответ.".into());
        }
        reader.take(length)?
    } else {
        keep_alive = false;
        reader.read_to_end()?
    };
    if body.len() > MAX_RESPONSE_BYTES {
        return Err("Сервис текстов вернул слишком большой ответ.".into());
    }
    let body = decode_body(body, &encoding, cancellable)?;
    let body = String::from_utf8(body)
        .map_err(|_| "Сервис текстов вернул ответ в неверной кодировке.".to_string())?;
    Ok((
        Reply {
            status,
            body,
            retry_after,
            connect_ms: None,
            total_ms: 0,
        },
        keep_alive,
    ))
}

fn decode_body(
    body: Vec<u8>,
    encoding: &str,
    cancellable: &gio::Cancellable,
) -> Result<Vec<u8>, String> {
    match encoding {
        "" | "identity" => return Ok(body),
        "gzip" => (),
        _ => return Err("Сервис текстов вернул неподдерживаемое сжатие.".into()),
    }
    let input = gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(body));
    let decoder = gio::ZlibDecompressor::new(gio::ZlibCompressorFormat::Gzip);
    let stream = gio::ConverterInputStream::new(&input, &decoder);
    let mut decoded = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let count = stream
            .read(&mut chunk, Some(cancellable))
            .map_err(|_| "Не удалось распаковать ответ сервиса текстов.".to_string())?;
        if count == 0 {
            return Ok(decoded);
        }
        if decoded.len() + count > MAX_RESPONSE_BYTES {
            return Err("Сервис текстов вернул слишком большой ответ.".into());
        }
        decoded.extend_from_slice(&chunk[..count]);
    }
}

struct ResponseReader<'a> {
    input: gio::InputStream,
    buffer: Vec<u8>,
    position: usize,
    cancel: &'a AtomicBool,
    cancellable: &'a gio::Cancellable,
}

impl ResponseReader<'_> {
    fn available(&self) -> usize {
        self.buffer.len() - self.position
    }

    fn fill(&mut self) -> Result<bool, String> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(CANCELLED.into());
        }
        let mut chunk = [0u8; 8192];
        let read = self
            .input
            .read(&mut chunk, Some(self.cancellable))
            .map_err(|error| format!("Нет связи с сервисом текстов: {error}"))?;
        if read == 0 {
            return Ok(false);
        }
        if self.buffer.len() + read > MAX_RESPONSE_BYTES + 64 * 1024 {
            return Err("Сервис текстов вернул слишком большой ответ.".into());
        }
        self.buffer.extend_from_slice(&chunk[..read]);
        Ok(true)
    }

    fn ensure(&mut self, length: usize) -> Result<(), String> {
        while self.available() < length {
            if !self.fill()? {
                return Err("Сервис текстов закрыл соединение до конца ответа.".into());
            }
        }
        Ok(())
    }

    fn take(&mut self, length: usize) -> Result<Vec<u8>, String> {
        self.ensure(length)?;
        let end = self.position + length;
        let bytes = self.buffer[self.position..end].to_vec();
        self.position = end;
        Ok(bytes)
    }

    fn read_line(&mut self) -> Result<String, String> {
        loop {
            if let Some(index) = find_bytes(&self.buffer[self.position..], b"\r\n") {
                let end = self.position + index;
                let line = String::from_utf8_lossy(&self.buffer[self.position..end]).into_owned();
                self.position = end + 2;
                return Ok(line);
            }
            if !self.fill()? {
                return Err("Сервис текстов вернул неполный ответ.".into());
            }
        }
    }

    fn read_chunked(&mut self) -> Result<Vec<u8>, String> {
        let mut result = Vec::new();
        loop {
            let line = self.read_line()?;
            let token = line.split(';').next().unwrap_or("").trim();
            let size = usize::from_str_radix(token, 16)
                .map_err(|_| "Сервис текстов вернул неверный размер блока.".to_string())?;
            if size == 0 {
                while !self.read_line()?.is_empty() {}
                return Ok(result);
            }
            if result.len() + size > MAX_RESPONSE_BYTES {
                return Err("Сервис текстов вернул слишком большой ответ.".into());
            }
            result.extend_from_slice(&self.take(size)?);
            if self.take(2)? != b"\r\n" {
                return Err("Сервис текстов вернул неверный блок ответа.".into());
            }
        }
    }

    fn read_to_end(&mut self) -> Result<Vec<u8>, String> {
        let mut result = Vec::new();
        loop {
            if self.available() > 0 {
                result.extend_from_slice(&self.buffer[self.position..]);
                self.position = self.buffer.len();
            }
            if result.len() > MAX_RESPONSE_BYTES {
                return Err("Сервис текстов вернул слишком большой ответ.".into());
            }
            if !self.fill()? {
                return Ok(result);
            }
        }
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn lyric_lines(value: &Value) -> Option<Vec<Lyric>> {
    crate::lyric_sources::lrclib(value)
}

fn short_title(title: &str) -> &str {
    let cut = title
        .find(" (")
        .or_else(|| title.find(" ["))
        .unwrap_or(title.len());
    title[..cut].trim()
}

fn normalized(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|ch| ch.is_alphanumeric())
        .collect()
}

fn choose_candidate(
    candidates: &[Value],
    title: &str,
    artist: &str,
    duration: f64,
) -> Option<Vec<Lyric>> {
    let wanted_title = normalized(short_title(title));
    if wanted_title.is_empty() {
        return None;
    }
    let wanted_artist = normalized(artist);
    candidates
        .iter()
        .filter_map(|candidate| {
            let name = normalized(short_title(candidate["trackName"].as_str()?));
            if name.is_empty() {
                return None;
            }
            if name != wanted_title {
                return None;
            }
            let difference = (candidate["duration"].as_f64().unwrap_or(duration) - duration).abs();
            if duration > 0.0 && difference > 5.0 {
                return None;
            }
            let lines = lyric_lines(candidate)?;
            let candidate_artist = normalized(candidate["artistName"].as_str().unwrap_or(""));
            let artist_match =
                !wanted_artist.is_empty() && candidate_artist.contains(&wanted_artist);
            let known_artist =
                !wanted_artist.is_empty() && wanted_artist != normalized("Неизвестный исполнитель");
            if known_artist && !artist_match {
                return None;
            }
            let score =
                100 + (if artist_match { 40 } else { 0 }) - (difference.min(120.0) as i32 / 3);
            Some(((crate::lyric_timing::rank(&lines), score), lines))
        })
        .max_by_key(|(score, _)| *score)
        .map(|(_, lines)| lines)
}

fn cache_path(track: &Track) -> Option<PathBuf> {
    let root = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    if track.path.as_os_str().is_empty() {
        let signature = serde_json::to_vec(&identity(track)).ok()?;
        let hash = signature
            .into_iter()
            .fold(14695981039346656037u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(1099511628211)
            });
        return Some(
            root.join("own-player/lyrics")
                .join(format!("query-v3-{hash:016x}.json")),
        );
    }
    let metadata = fs::metadata(&track.path).ok()?;
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let key = track
        .path
        .to_string_lossy()
        .bytes()
        .fold(14695981039346656037u64, |hash, byte| {
            (hash ^ byte as u64).wrapping_mul(1099511628211)
        });
    Some(
        root.join("own-player/lyrics")
            // Previous results may have been searched with the album artist.
            .join(format!("v2-{key:016x}-{}-{modified}.json", metadata.len())),
    )
}

pub(crate) fn read_cache(track: &Track) -> Option<Vec<Lyric>> {
    let lines: Vec<Lyric> = serde_json::from_slice(&fs::read(cache_path(track)?).ok()?).ok()?;
    (crate::lyric_timing::quality(&lines) > 0).then_some(lines)
}

fn write_cache(track: &Track, lines: &[Lyric]) {
    if let Some(path) = cache_path(track) {
        if let Some(parent) = path.parent() {
            if fs::create_dir_all(parent).is_ok() {
                if let Ok(data) = serde_json::to_vec(lines) {
                    atomic_write(&path, &data);
                }
            }
        }
    }
}

fn atomic_write(path: &std::path::Path, data: &[u8]) {
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    if fs::write(&temporary, data).is_ok() {
        let _ = fs::rename(&temporary, path);
    }
    let _ = fs::remove_file(temporary);
}

#[cfg(test)]
mod tests {
    use super::{choose_candidate, online_lyrics, short_title};
    use crate::Track;
    use serde_json::json;

    #[test]
    fn rejects_wrong_artist_even_with_matching_duration() {
        let candidates = vec![
            json!({"trackName":"Song", "artistName":"Someone else", "duration":200, "plainLyrics":"wrong"}),
        ];
        assert!(choose_candidate(&candidates, "Song", "Artist", 200.0).is_none());
        let missing_title = vec![
            json!({"trackName":"", "artistName":"Artist", "duration":200, "plainLyrics":"wrong"}),
        ];
        assert!(choose_candidate(&missing_title, "Song", "Artist", 200.0).is_none());
    }

    #[test]
    fn title_fallback_finds_lyrics_and_ignores_unrelated_result() {
        let candidates = vec![
            json!({"trackName":"Different", "artistName":"Artist", "duration":200, "plainLyrics":"wrong"}),
            json!({"trackName":"Song", "artistName":"Artist", "duration":201, "syncedLyrics":"[00:01.00]right"}),
        ];
        let result = choose_candidate(&candidates, "Song (Remastered)", "Artist", 200.0).unwrap();
        assert_eq!(result[0].text, "right");
        assert_eq!(short_title("Song (Remastered)"), "Song");
    }

    #[test]
    #[ignore = "требует доступ к LRCLIB"]
    fn live_lrclib_search_returns_lyrics() {
        let track = Track {
            path: "/tmp/own-player-nonexistent-test.mp3".into(),
            title: "Never Gonna Give You Up".into(),
            artist: "Rick Astley".into(),
            album: String::new(),
            duration: 0.0,
            cover_path: None,
            lyrics: Vec::new(),
        };
        assert!(!online_lyrics(&track).unwrap().is_empty());
    }

    #[test]
    #[ignore = "требует доступ к LRCLIB"]
    fn live_title_only_fallback_returns_lyrics() {
        let track = Track {
            path: "/tmp/own-player-nonexistent-title-test.mp3".into(),
            title: "Never Gonna Give You Up".into(),
            artist: "Неизвестный исполнитель".into(),
            album: String::new(),
            duration: 0.0,
            cover_path: None,
            lyrics: Vec::new(),
        };
        assert!(!online_lyrics(&track).unwrap().is_empty());
    }

    #[test]
    #[ignore = "требует доступ к LRCLIB"]
    fn live_reported_track_returns_plain_lyrics() {
        let track = Track {
            path: "/tmp/own-player-nonexistent-reported-test.mp3".into(),
            title: "Splinter Cell".into(),
            artist: "YUNG TRAPPA".into(),
            album: String::new(),
            duration: 0.0,
            cover_path: None,
            lyrics: Vec::new(),
        };
        let lines = online_lyrics(&track).unwrap();
        assert!(!lines.is_empty());
        assert!(lines.iter().all(|line| line.time.is_none()));
    }
}

#[cfg(test)]
mod ranking_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn partial_word_upgrade_is_emitted_without_claiming_complete_timing() {
        let plain = crate::library::parse_lyrics("[00:01]One two\n[00:04]Three four");
        let partial =
            crate::library::parse_lyrics("[00:01]<00:01>One <00:02>two\n[00:04]Three four");
        let mut emissions = Vec::new();
        let mut progress = |lines: &[Lyric]| emissions.push(lines.to_vec());
        let mut search = Search {
            best: plain.clone(),
            best_quality: 2,
            failed: false,
            lrclib_available: true,
            progress: &mut progress,
            started: Instant::now(),
        };
        search.accept(partial.clone());
        assert_eq!(search.best, partial);
        assert_eq!(search.best_quality, 2, "keep looking for complete timing");
        search.accept(plain);
        assert_eq!(
            search.best, partial,
            "line-only text must not replace partial word cues"
        );
        drop(search);
        assert_eq!(emissions, vec![partial]);
    }

    #[test]
    fn blank_positive_cache_is_rechecked_but_negative_cache_is_kept() {
        for raw in ["[00:01]", "[00:01]<00:01>", " "] {
            for quality in [None, Some(0), Some(2)] {
                let lines = if raw.trim().is_empty() {
                    vec![Lyric {
                        time: None,
                        text: raw.into(),
                    }]
                } else {
                    crate::library::parse_lyrics(raw)
                };
                let cache = LookupCache {
                    identity: ("Song".into(), "Artist".into(), "".into(), 20),
                    expires: 0,
                    lines,
                    error: None,
                    quality,
                };
                assert!(
                    !cache.timing_is_current(),
                    "raw={raw:?} quality={quality:?}"
                );
            }
        }
        let cache = LookupCache {
            identity: ("Song".into(), "Artist".into(), "".into(), 20),
            expires: 0,
            lines: vec![],
            error: Some("not found".into()),
            quality: Some(0),
        };
        assert!(cache.timing_is_current());
    }

    #[test]
    fn legacy_partial_word_cache_is_rechecked_once() {
        for (text, reusable) in [
            ("[00:01]<00:01>One two", false),
            ("[00:01]<00:01>One <00:02>two\n[00:04]Three four", false),
            ("[00:01]<00:01>One <00:09>two\n[00:04]Three", false),
            ("[00:01]<00:01>One <00:02>two", true),
            ("[00:01]One two", true),
            ("Plain text", true),
            ("", true),
        ] {
            let mut cache: LookupCache = serde_json::from_value(json!({
                "identity": ["Song", "Artist", "", 20],
                "expires": 0,
                "lines": crate::library::parse_lyrics(text),
                "error": null
            }))
            .unwrap();
            assert_eq!(cache.timing_is_current(), reusable, "{text}");
            cache.quality = Some(crate::lyric_timing::quality(&cache.lines));
            assert!(
                cache.timing_is_current(),
                "a fresh result keeps its normal TTL"
            );
        }
    }

    #[test]
    fn complete_word_timing_beats_a_partially_timed_candidate() {
        let partial = json!({"trackName":"Song","artistName":"Artist","duration":200,"syncedLyrics":"[00:01]<00:01>One <00:02>two\n[00:04]Three four"});
        let complete = json!({"trackName":"Song","artistName":"Artist","duration":200,"syncedLyrics":"[00:01]<00:01>One <00:02>two\n[00:04]<00:04>Three <00:05>four"});
        let best = choose_candidate(&[complete, partial], "Song", "Artist", 200.0).unwrap();
        assert_eq!(crate::lyric_timing::quality(&best), 3);
    }

    #[test]
    fn word_sync_wins_over_line_and_plain_but_not_a_wrong_recording() {
        let words = json!({"trackName":"Song","artistName":"Artist","duration":200,"syncedLyrics":"[00:01]<00:01>Word <00:02>two"});
        let line = json!({"trackName":"Song","artistName":"Artist","duration":200,"syncedLyrics":"[00:01]line"});
        let plain =
            json!({"trackName":"Song","artistName":"Artist","duration":200,"plainLyrics":"plain"});
        let best = choose_candidate(
            &[words.clone(), line.clone(), plain],
            "Song",
            "Artist",
            200.0,
        )
        .unwrap();
        assert_eq!(crate::lyric_timing::quality(&best), 3);
        let mut wrong = words;
        wrong["duration"] = json!(300);
        assert_eq!(
            crate::lyric_timing::quality(
                &choose_candidate(&[wrong, line], "Song", "Artist", 200.0).unwrap()
            ),
            2
        );
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let output = gio::MemoryOutputStream::new_resizable();
        let compressor = gio::ZlibCompressor::new(gio::ZlibCompressorFormat::Gzip, -1);
        let stream = gio::ConverterOutputStream::new(&output, &compressor);
        stream.write_all(bytes, None::<&gio::Cancellable>).unwrap();
        stream.close(None::<&gio::Cancellable>).unwrap();
        output.steal_as_bytes().to_vec()
    }

    #[test]
    fn gzip_response_preserves_utf8_and_rejects_invalid_encoding() {
        let expected = "{\"text\":\"Тест 🌙\"}".as_bytes();
        let cancel = gio::Cancellable::new();
        assert_eq!(
            decode_body(gzip(expected), "gzip", &cancel).unwrap(),
            expected
        );
        assert_eq!(
            decode_body(expected.to_vec(), "", &cancel).unwrap(),
            expected
        );
        assert!(decode_body(b"invalid".to_vec(), "gzip", &cancel).is_err());
        assert!(decode_body(gzip(expected), "br", &cancel).is_err());
    }

    #[test]
    fn gzip_response_limit_applies_after_decompression() {
        let compressed = gzip(&vec![b'a'; MAX_RESPONSE_BYTES + 1]);
        assert!(compressed.len() < MAX_RESPONSE_BYTES);
        assert!(decode_body(compressed, "gzip", &gio::Cancellable::new()).is_err());
    }

    #[test]
    fn request_deadline_interrupts_a_blocked_gio_read() {
        use std::io::Read;
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (close_tx, close_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = [0; 1024];
            assert!(stream.read(&mut request).unwrap() > 0);
            ready_tx.send(()).unwrap();
            let _ = close_rx.recv_timeout(Duration::from_secs(2));
        });
        let cancellable = gio::Cancellable::new();
        let worker_cancel = cancellable.clone();
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let client = gio::SocketClient::new();
            client.set_timeout(2);
            let connection = client
                .connect_to_host(&address.to_string(), address.port(), Some(&worker_cancel))
                .unwrap();
            let result = exchange(
                &connection,
                "fixture",
                "/",
                &AtomicBool::new(false),
                &worker_cancel,
            );
            let _ = sender.send(result.map(|(reply, _)| reply));
        });
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let started = Instant::now();
        assert!(wait_for_reply(
            &receiver,
            &AtomicBool::new(false),
            &cancellable,
            Duration::from_millis(30)
        )
        .is_err());
        assert!(cancellable.is_cancelled());
        assert!(
            receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .is_err(),
            "deadline must interrupt I/O, not just stop waiting for it"
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        let _ = close_tx.send(());
        worker.join().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn caller_cancellation_interrupts_waiting_without_a_reply() {
        let (_sender, receiver) = mpsc::channel();
        let cancellable = gio::Cancellable::new();
        let result = wait_for_reply(
            &receiver,
            &AtomicBool::new(true),
            &cancellable,
            Duration::from_secs(12),
        );
        assert_eq!(result.err().as_deref(), Some(CANCELLED));
        assert!(cancellable.is_cancelled());
    }

    #[test]
    fn retry_after_and_legacy_fixture_status_are_read_without_extra_requests() {
        assert_eq!(
            super::http_reply("{}\n429\n120"),
            Some(("{}", "429", Some(120)))
        );
        assert_eq!(super::http_reply("{}\n200\n"), Some(("{}", "200", None)));
        assert_eq!(super::http_reply("{}\n200"), Some(("{}", "200", None)));
    }
}
