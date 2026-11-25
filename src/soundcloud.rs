//! SoundCloud downloads for tracks whose author enabled downloading.
//!
//! Only the original file offered by SoundCloud is fetched. Streams, HLS,
//! DRM and third-party resolvers are deliberately not used. The user's own
//! authorization is read from the keyring (or kept in memory for one run).

use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod api;
mod auth;
mod download;
mod files;
mod library;

use api::*;
use download::*;
use files::*;
use library::*;

pub(crate) use auth::{
    clear_stored_token, get_token, save_token, set_session_token, token_summary, validate_token,
};
pub(crate) use files::default_download_dir;

const API_DEFAULT: &str = "https://api-v2.soundcloud.com";
const WEB_DEFAULT: &str = "https://soundcloud.com";
const USER_AGENT: &str = "Veyra/0.7 (local SoundCloud download)";
const TOKEN_SERVICE: &str = "own-player";
const TOKEN_KIND: &str = "soundcloud-token";
const MAX_JSON: u64 = 8 * 1024 * 1024;
const MAX_COVER: u64 = 8 * 1024 * 1024;
const MAX_AUDIO: u64 = 2 * 1024 * 1024 * 1024;
const MAX_REDIRECTS: u32 = 5;
const POLL: Duration = Duration::from_millis(100);

static SESSION: Mutex<Option<String>> = Mutex::new(None);
static ACTIVE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug)]
pub(crate) struct Endpoints {
    pub api: String,
    pub web: String,
    pub allow_http: bool,
    pub client_id: Option<String>,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            api: API_DEFAULT.to_string(),
            web: WEB_DEFAULT.to_string(),
            allow_http: false,
            client_id: None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TrackInfo {
    pub id: u64,
    pub title: String,
    pub artist: String,
    pub artwork_url: Option<String>,
    pub duration_ms: u64,
    pub downloadable: Option<bool>,
    pub has_downloads_left: Option<bool>,
    pub permalink_url: String,
    pub genre: String,
    pub created_at: String,
    pub download_url: Option<String>,
}

#[derive(Debug)]
pub(crate) enum ScError {
    InvalidUrl(&'static str),
    NotDownloadable,
    DownloadStatusUnknown,
    DownloadsExhausted,
    AuthRequired,
    AuthRejected,
    NotFound,
    RateLimited,
    Unsupported(String),
    Network(String),
    Http(u16, String),
    Io(String),
    Cancelled,
    Redirect(String),
}

impl ScError {
    pub(crate) fn message(&self) -> String {
        match self {
            Self::InvalidUrl(detail) => format!("Некорректная ссылка: {detail}."),
            Self::NotDownloadable => {
                "Автор не разрешил скачивание этого трека. Доступны только треки с кнопкой Download."
                    .into()
            }
            Self::DownloadStatusUnknown => "SoundCloud не сообщил, доступно ли скачивание.".into(),
            Self::DownloadsExhausted => "Достигнут лимит скачиваний этого трека.".into(),
            Self::AuthRequired => {
                "SoundCloud требует авторизацию. Нажми «Авторизация…» и вставь токен своей сессии."
                    .into()
            }
            Self::AuthRejected => {
                "SoundCloud отклонил доступ. Проверь токен, срок его действия и права на трек.".into()
            }
            Self::NotFound => "Трек или файл не найден либо недоступен для скачивания.".into(),
            Self::RateLimited => "Слишком много запросов к SoundCloud. Попробуй позже.".into(),
            Self::Unsupported(detail) => format!("Файл не поддерживается: {detail}."),
            Self::Network(detail) => format!("Сетевая ошибка: {detail}."),
            Self::Http(status, body) if body.is_empty() => {
                format!("SoundCloud вернул HTTP {status}.")
            }
            Self::Http(status, body) => format!("SoundCloud ответил HTTP {status}: {body}"),
            Self::Io(detail) => format!("Ошибка файла: {detail}."),
            Self::Cancelled => "Загрузка отменена.".into(),
            Self::Redirect(_) => {
                "SoundCloud вернул ссылку на файл вместо самого файла.".into()
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct Outcome {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub already: bool,
}

#[derive(Debug)]
pub(crate) enum Event {
    Resolving,
    Meta {
        title: String,
        artist: String,
    },
    Downloading {
        received: u64,
        total: Option<u64>,
    },
    Processing,
    Done(Outcome),
    Failed(String),
    Cancelled,
    LibraryCount(usize),
    BatchStarted {
        total: usize,
        skipped: usize,
    },
    BatchItem {
        index: usize,
        total: usize,
        title: String,
        artist: String,
    },
    BatchFinished {
        downloaded: usize,
        already: usize,
        failed: usize,
        skipped: usize,
        cancelled: bool,
        report: Option<PathBuf>,
    },
}

pub(crate) struct Job {
    cancel: Arc<AtomicBool>,
}

impl Job {
    pub(crate) fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

pub(crate) fn start_library(dir: PathBuf, token: Option<String>, tx: Sender<Event>) -> Job {
    start_worker(dir, tx, move |dir, cancel, tx| {
        let token = token.ok_or(ScError::AuthRequired)?;
        let mut summary = BatchSummary::default();
        run_library(dir, &token, &Endpoints::default(), cancel, tx, &mut summary)?;
        Ok(Event::BatchFinished {
            downloaded: summary.downloaded,
            already: summary.already,
            failed: summary.failed,
            skipped: summary.skipped,
            cancelled: summary.cancelled,
            report: summary.report,
        })
    })
}

pub(crate) fn start(url: String, dir: PathBuf, token: Option<String>, tx: Sender<Event>) -> Job {
    start_worker(dir, tx, move |dir, cancel, tx| {
        run_job(
            &url,
            dir,
            token.as_deref(),
            &Endpoints::default(),
            cancel,
            tx,
        )
        .map(Event::Done)
    })
}

fn start_worker(
    dir: PathBuf,
    tx: Sender<Event>,
    work: impl FnOnce(&Path, &AtomicBool, &Sender<Event>) -> Result<Event, ScError> + Send + 'static,
) -> Job {
    let job = Job {
        cancel: Arc::new(AtomicBool::new(false)),
    };
    if ACTIVE.swap(true, Ordering::AcqRel) {
        let _ = tx.send(Event::Failed(
            "Загрузка уже идёт. Дождись её завершения или отмени.".into(),
        ));
        return job;
    }

    let cancel = job.cancel.clone();
    let events = tx.clone();
    let worker = std::thread::Builder::new()
        .name("soundcloud".into())
        .spawn(move || {
            struct Active;
            impl Drop for Active {
                fn drop(&mut self) {
                    ACTIVE.store(false, Ordering::Release);
                }
            }

            let active = Active;
            cleanup_stale_temp(&dir);
            let event = match work(&dir, &cancel, &events) {
                Ok(event) => event,
                Err(ScError::Cancelled) => Event::Cancelled,
                Err(error) => Event::Failed(error.message()),
            };
            drop(active);
            let _ = events.send(event);
        });
    if let Err(error) = worker {
        ACTIVE.store(false, Ordering::Release);
        let _ = tx.send(Event::Failed(format!(
            "Не удалось запустить загрузку: {error}"
        )));
    }
    job
}

fn run_job(
    input: &str,
    dir: &Path,
    token: Option<&str>,
    endpoints: &Endpoints,
    cancel: &AtomicBool,
    tx: &Sender<Event>,
) -> Result<Outcome, ScError> {
    if cancel.load(Ordering::SeqCst) {
        return Err(ScError::Cancelled);
    }
    let _ = tx.send(Event::Resolving);
    let normalized = normalize_track_url(input, endpoints.allow_http)?;
    let parsed =
        parse_url(&normalized).ok_or(ScError::InvalidUrl("не удалось разобрать ссылку"))?;
    let track_url = if is_short_host(&parsed.host) {
        resolve_short_link(&normalized, endpoints)?
    } else {
        normalized
    };

    let info = resolve_track_info(&track_url, endpoints)?;
    download_info(&info, dir, token, endpoints, cancel, tx)
}

#[cfg(test)]
mod tests {
    use super::auth::load_stored_token;
    use super::*;

    #[test]
    fn redirect_is_checked_before_it_can_be_followed() {
        let endpoints = Endpoints::default();
        let base = "https://api-v2.soundcloud.com/tracks/42/download";
        assert_eq!(
            redirect_target(base, "//cf-media.sndcdn.com/original", &endpoints).unwrap(),
            "https://cf-media.sndcdn.com/original"
        );
        assert_eq!(
            redirect_target(base, "/original", &endpoints).unwrap(),
            "https://api-v2.soundcloud.com/original"
        );
        assert!(redirect_target(base, "//example.org/file", &endpoints).is_err());
        assert!(redirect_target(base, "http://cf-media.sndcdn.com/file", &endpoints).is_err());
        assert!(validate_api_url("https://on.soundcloud.com/example", &endpoints).is_err());
    }

    #[test]
    fn accepts_soundcloud_track_links() {
        let url = normalize_track_url("soundcloud.com/artist/track?in=set", false).unwrap();
        assert_eq!(url, "https://soundcloud.com/artist/track?in=set");
        let url = normalize_track_url("https://on.soundcloud.com/abcDEF", false).unwrap();
        assert_eq!(url, "https://on.soundcloud.com/abcDEF");
        let url = normalize_track_url("http://soundcloud.com/a/b", true).unwrap();
        assert_eq!(url, "http://soundcloud.com/a/b");
    }

    #[test]
    fn rejects_foreign_or_empty_links() {
        assert!(matches!(
            normalize_track_url("https://example.com/a/b", false),
            Err(ScError::InvalidUrl(_))
        ));
        assert!(matches!(
            normalize_track_url("https://soundcloud.com/", false),
            Err(ScError::InvalidUrl(_))
        ));
        assert!(matches!(
            normalize_track_url("   ", false),
            Err(ScError::InvalidUrl(_))
        ));
        assert!(matches!(
            normalize_track_url("ftp://soundcloud.com/a/b", false),
            Err(ScError::InvalidUrl(_))
        ));
    }

    #[test]
    fn download_hosts_are_limited() {
        assert!(is_allowed_download_host("api-v2.soundcloud.com"));
        assert!(is_allowed_download_host("i1.sndcdn.com"));
        assert!(!is_allowed_download_host("soundcloud.com.evil.example"));
        assert!(!is_allowed_download_host("example.com"));
    }

    #[test]
    fn file_names_are_sanitized() {
        assert_eq!(
            build_stem("Artist / Name", "Track: One?"),
            "Artist _ Name - Track_ One_"
        );
        assert_eq!(build_stem("Неизвестный исполнитель", "  "), "track");
        assert_eq!(build_stem("", "Only title"), "Only title");
        let long = "a".repeat(400);
        assert!(sanitize_component(&long).chars().count() <= 120);
    }

    #[test]
    fn client_id_is_extracted_from_bundle() {
        let html = r#"<script>window.__sc={client_id:"AbCdEfGhIjKlMnOpQrStUv"}</script>"#;
        assert_eq!(
            extract_client_id(html).as_deref(),
            Some("AbCdEfGhIjKlMnOpQrStUv")
        );
        assert_eq!(extract_client_id("<html></html>"), None);
    }

    #[test]
    fn script_urls_are_restricted_to_soundcloud_cdn() {
        let html = r#"
            <script src="https://a-v2.sndcdn.com/assets/1-abc.js"></script>
            <script src='https://a-v2.sndcdn.com/assets/2-def.js'></script>
            <script src="https://example.com/tracker.js"></script>
            <script src="http://a-v2.sndcdn.com/assets/3-old.js"></script>
            <script src="/local.js"></script>
        "#;
        assert_eq!(
            script_urls(html),
            vec![
                "https://a-v2.sndcdn.com/assets/1-abc.js",
                "https://a-v2.sndcdn.com/assets/2-def.js"
            ]
        );
    }

    #[test]
    #[ignore = "uses the live SoundCloud API"]
    fn live_metadata_round_trip() {
        let endpoints = Endpoints::default();
        let cid = client_id(&endpoints).expect("public client_id");
        assert!(cid.len() >= 20);
        let charts = format!(
            "{}/search/tracks?q=ambient&client_id={cid}&limit=1",
            endpoints.api
        );
        let (status, body) =
            http_text(&charts, &api_headers(None), &endpoints, MAX_JSON).expect("search");
        assert_eq!(status, 200);
        let value: Value = serde_json::from_str(&body).unwrap();
        let permalink = value["collection"][0]["permalink_url"]
            .as_str()
            .expect("permalink")
            .to_string();
        let info = resolve_track_info(&permalink, &endpoints).expect("resolve");
        assert!(info.id > 0);
        assert!(!info.title.is_empty());
        assert!(!info.artist.is_empty());
    }

    #[test]
    fn preview_is_short_and_single_line() {
        let body = format!("{}\n\tend", "ю".repeat(500));
        let preview = body_preview(body.as_bytes());
        assert!(preview.chars().count() <= 300);
        assert!(!preview.contains('\n'));
    }

    #[test]
    fn archives_are_rejected_before_probing() {
        let dir = std::env::temp_dir().join(format!("own-player-sc-sniff-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("a.zip");
        fs::write(&archive, b"PK\x03\x04payload").unwrap();
        assert!(matches!(sniff(&archive), Err(ScError::Unsupported(_))));
        let page = dir.join("b.mp3");
        fs::write(&page, b"  <html>nope").unwrap();
        assert!(matches!(sniff(&page), Err(ScError::Unsupported(_))));
        let audio = dir.join("c.mp3");
        fs::write(&audio, b"ID3\x03\x00").unwrap();
        assert!(sniff(&audio).is_ok());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn extension_matches_probe_format() {
        let probe = |format: &str| AudioProbe {
            duration_ms: 1,
            format: format.into(),
        };
        assert_eq!(extension_for(&probe("mp3")).unwrap(), "mp3");
        assert_eq!(extension_for(&probe("flac")).unwrap(), "flac");
        assert_eq!(extension_for(&probe("mov,mp4,m4a")).unwrap(), "m4a");
        assert_eq!(extension_for(&probe("matroska,webm")).unwrap(), "webm");
        assert!(extension_for(&probe("gif")).is_err());
    }

    #[test]
    fn tags_and_sidecar_cover_are_written() {
        if Command::new("ffmpeg").arg("-version").output().is_err() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("own-player-sc-embed-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("raw.wav");
        let status = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1",
            ])
            .arg(&raw)
            .status()
            .unwrap();
        assert!(status.success());
        let cover = dir.join("cover.jpg");
        let status = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=0x335544:s=64x64",
                "-frames:v",
                "1",
            ])
            .arg(&cover)
            .status()
            .unwrap();
        assert!(status.success());

        let info = TrackInfo {
            id: 42,
            title: "Тестовый трек".into(),
            artist: "Артист".into(),
            artwork_url: None,
            duration_ms: 1_000,
            downloadable: Some(true),
            has_downloads_left: Some(true),
            permalink_url: "https://soundcloud.com/artist/test".into(),
            genre: "Ambient".into(),
            created_at: "2024-05-01T00:00:00Z".into(),
            download_url: None,
        };
        let destination = dir.join("Артист - Тестовый трек.wav");
        let mut guard = TempGuard::new();
        let embedded =
            embed_metadata(&raw, &destination, &info, Some(&cover), "wav", &mut guard).unwrap();
        assert!(!embedded);
        write_sidecar_cover(&cover, &destination);
        assert!(destination.is_file());
        assert!(dir.join("Артист - Тестовый трек.jpg").is_file());

        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "format_tags=title,artist",
                "-of",
                "json",
            ])
            .arg(&destination)
            .output()
            .unwrap();
        let tags = String::from_utf8_lossy(&probe.stdout);
        assert!(tags.contains("Тестовый трек"), "tags: {tags}");
        assert!(tags.contains("Артист"), "tags: {tags}");
        assert_eq!(
            existing_by_name(&dir, &info).as_deref(),
            Some(destination.as_path())
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    #[ignore = "downloads a real track; needs a token in the keyring"]
    fn live_download_authorized() {
        let token = load_stored_token().expect("token in keyring");
        let root = std::env::temp_dir().join(format!("own-player-sc-live-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = AtomicBool::new(false);
        let url = "https://soundcloud.com/georgeloukasofficial/george-loukas-down-on-my-knees";
        let outcome = run_job(
            url,
            &root,
            Some(&token),
            &Endpoints::default(),
            &cancel,
            &tx,
        )
        .expect("download");
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        assert!(events
            .iter()
            .any(|event| matches!(event, Event::Downloading { received, .. } if *received > 0)));
        assert!(!outcome.already);
        assert_eq!(outcome.path.parent(), Some(root.as_path()));
        let size = fs::metadata(&outcome.path).unwrap().len();
        assert!(size > 50_000, "size={size}");
        let probe = probe_audio(&outcome.path).expect("probe");
        assert!(probe.duration_ms > 30_000, "duration={}", probe.duration_ms);
        let title = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "format_tags=title,artist",
                "-of",
                "json",
            ])
            .arg(&outcome.path)
            .output()
            .unwrap();
        let tags = String::from_utf8_lossy(&title.stdout);
        assert!(tags.contains("On My Knees"), "tags: {tags}");
        assert!(
            probe.format.contains("mp3") || probe.format.contains("wav"),
            "unexpected original format {}",
            probe.format
        );
        let streams = Command::new("ffprobe")
            .args(["-v", "error", "-show_streams", "-of", "json"])
            .arg(&outcome.path)
            .output()
            .unwrap();
        let streams = String::from_utf8_lossy(&streams.stdout);
        assert!(
            streams.contains("attached_pic") || outcome.path.with_extension("jpg").is_file(),
            "no cover found"
        );

        let second = run_job(
            url,
            &root,
            Some(&token),
            &Endpoints::default(),
            &cancel,
            &tx,
        )
        .expect("second run");
        assert!(second.already);
        assert_eq!(second.path, outcome.path);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn page_urls_get_client_id_once() {
        let cid = "abc123";
        assert_eq!(
            with_client_id("https://api-v2.soundcloud.com/me", cid),
            "https://api-v2.soundcloud.com/me?client_id=abc123"
        );
        assert_eq!(
            with_client_id("https://api-v2.soundcloud.com/me?limit=5", cid),
            "https://api-v2.soundcloud.com/me?limit=5&client_id=abc123"
        );
        assert_eq!(
            with_client_id("https://api-v2.soundcloud.com/me?client_id=old", cid),
            "https://api-v2.soundcloud.com/me?client_id=old"
        );
    }

    #[test]
    fn downloadability_filters_library_tracks() {
        let mut library = LibraryTracks::default();
        let yes = serde_json::json!({
            "kind": "track", "id": 1, "title": "Yes", "downloadable": true,
            "user": {"username": "A"}, "permalink_url": "https://soundcloud.com/a/yes"
        });
        let no = serde_json::json!({
            "kind": "track", "id": 2, "title": "No", "downloadable": false,
            "user": {"username": "A"}, "permalink_url": "https://soundcloud.com/a/no"
        });
        let spent = serde_json::json!({
            "kind": "track", "id": 3, "title": "Spent", "downloadable": true,
            "has_downloads_left": false, "user": {"username": "A"},
            "permalink_url": "https://soundcloud.com/a/spent"
        });
        let partial = serde_json::json!({"id": 1});
        library.note_track(&partial);
        assert!(library.pending.contains_key(&1));
        assert!(library.skipped.is_empty());
        for track in [&yes, &no, &spent, &partial] {
            library.note_track(track);
        }
        assert!(library.found.contains_key(&1));
        assert!(library.pending.is_empty());
        assert!(!library.skipped.contains_key(&1));
        assert!(!library.found.contains_key(&2));
        assert!(!library.found.contains_key(&3));
        assert!(library.skipped[&2].reason.contains("не включил"));
        assert!(library.skipped[&3].reason.contains("лимит"));
    }

    #[test]
    fn skipped_report_is_written() {
        let dir = std::env::temp_dir().join(format!("own-player-sc-report-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let entries = vec![
            SkippedEntry {
                url: "https://soundcloud.com/a/b".into(),
                title: "B".into(),
                artist: "A".into(),
                reason: "автор запретил скачивание".into(),
            },
            SkippedEntry {
                url: "https://soundcloud.com/c/d".into(),
                title: "D".into(),
                artist: "C".into(),
                reason: "лимит скачиваний исчерпан".into(),
            },
        ];
        let path = write_skipped_report(&dir, &entries).expect("report");
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("https://soundcloud.com/a/b"));
        assert!(text.contains("https://soundcloud.com/c/d"));
        assert!(text.contains("запретил"));
        assert!(write_skipped_report(&dir, &[]).is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    #[ignore = "uses the live SoundCloud API and the token in the keyring"]
    fn live_library_enumeration() {
        let token = load_stored_token().expect("token in keyring");
        let (tx, _rx) = std::sync::mpsc::channel();
        let cancel = AtomicBool::new(false);
        let (tracks, skipped) =
            collect_library(&token, &Endpoints::default(), &cancel, &tx).expect("library");
        assert!(
            !tracks.is_empty() || !skipped.is_empty(),
            "library is empty"
        );
        assert!(tracks.iter().all(|track| track.downloadable == Some(true)));
        println!("downloadable={} skipped={}", tracks.len(), skipped.len());
    }

    #[test]
    fn token_validation_rejects_unsafe_values() {
        assert!(validate_token("abc123").is_ok());
        assert!(validate_token("").is_err());
        assert!(validate_token("a b").is_err());
        assert!(validate_token("a\nb").is_err());
    }
}
