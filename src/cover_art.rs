use crate::Track;
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::SystemTime;

static ALBUM_DOWNLOADS: [Mutex<()>; 32] = [const { Mutex::new(()) }; 32];
static ALBUM_INDEX: Mutex<Option<AlbumIndex>> = Mutex::new(None);

// A known album name with no ID represents conflicting IDs in the CSV.
type AlbumsByTrack = HashMap<(String, String), HashMap<String, Option<String>>>;

struct AlbumIndex {
    path: PathBuf,
    modified: SystemTime,
    len: u64,
    albums: AlbumsByTrack,
}

fn data_root() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .map(|root| root.join("own-player"))
}

fn index_path() -> Option<PathBuf> {
    Some(data_root()?.join("playlist.csv"))
}

pub(crate) fn has_index() -> bool {
    index_path().is_some_and(|path| path.is_file())
}

pub(crate) fn install_index(source: &Path) -> Result<(), String> {
    let content =
        fs::read_to_string(source).map_err(|error| format!("Не удалось прочитать CSV: {error}"))?;
    let rows = parse_csv(&content);
    let valid_header = rows.first().is_some_and(|row| {
        row.get(1).is_some_and(|cell| cell == "title")
            && row.get(2).is_some_and(|cell| cell == "artists")
            && row.get(6).is_some_and(|cell| cell == "album_id")
    });
    if !valid_header || rows.iter().skip(1).all(|row| row.len() < 7) {
        return Err("CSV не содержит данных экспорта Яндекс Музыки.".into());
    }
    let target = index_path().ok_or("Не найден домашний каталог")?;
    let parent = target.parent().ok_or("Некорректный путь")?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = target.with_extension(format!("csv.tmp-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|error| error.to_string())?;
    file.write_all(content.as_bytes())
        .map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    fs::rename(&temporary, &target).map_err(|error| error.to_string())?;
    if let Ok(mut index) = ALBUM_INDEX.lock() {
        *index = None;
    }
    Ok(())
}

pub(crate) fn online_cover(track: &Track) -> Option<PathBuf> {
    let album_id = album_for_track(&track.title, &track.artist, &track.album)?;
    let root = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    let cache = root
        .join("own-player/covers-online")
        .join(format!("{album_id}.jpg"));
    if cache.is_file() {
        return Some(cache);
    }
    // Keep parallel requests for the same album from downloading or writing it twice.
    let slot = album_id.bytes().fold(0usize, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(usize::from(byte))
    }) % ALBUM_DOWNLOADS.len();
    let _download = ALBUM_DOWNLOADS[slot].lock().ok()?;
    if cache.is_file() {
        return Some(cache);
    }
    let response = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "--max-time",
            "10",
            "--user-agent",
            concat!("Veyra/", env!("CARGO_PKG_VERSION")),
        ])
        .arg(format!("https://api.music.yandex.net/albums/{album_id}"))
        .output()
        .ok()?;
    if !response.status.success() {
        return None;
    }
    let data: Value = serde_json::from_slice(&response.stdout).ok()?;
    let uri = data["result"]["coverUri"].as_str()?;
    if !uri.starts_with("avatars.yandex.net/") || !uri.contains("%%") {
        return None;
    }
    let url = format!("https://{}", uri.replace("%%", "600x600"));
    let image = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "--max-time",
            "12",
            "--max-filesize",
            "2000000",
            "--user-agent",
            concat!("Veyra/", env!("CARGO_PKG_VERSION")),
        ])
        .arg(url)
        .output()
        .ok()?;
    if !image.status.success() || !image.stdout.starts_with(&[0xff, 0xd8, 0xff]) {
        return None;
    }
    fs::create_dir_all(cache.parent()?).ok()?;
    let temporary = cache.with_extension(format!("jpg.tmp-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .ok()?;
    file.write_all(&image.stdout).ok()?;
    file.sync_all().ok()?;
    fs::rename(&temporary, &cache).ok()?;
    Some(cache)
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|ch| ch.is_alphanumeric())
        .collect()
}

fn album_for_track(title: &str, artist: &str, album: &str) -> Option<String> {
    let key = (normalize(title), normalize(artist));
    if key.0.is_empty() || key.1.is_empty() {
        return None;
    }
    let path = index_path()?;
    let mut cached = ALBUM_INDEX.lock().ok()?;
    let metadata = fs::metadata(&path).ok()?;
    let modified = metadata.modified().ok()?;
    let len = metadata.len();
    if cached
        .as_ref()
        .is_none_or(|index| index.path != path || index.modified != modified || index.len != len)
    {
        let csv = fs::read_to_string(&path).ok()?;
        *cached = Some(AlbumIndex {
            path,
            modified,
            len,
            albums: index_albums(&csv),
        });
    }
    find_album(&cached.as_ref()?.albums, &key, album).map(str::to_owned)
}

fn find_album<'a>(
    albums: &'a AlbumsByTrack,
    key: &(String, String),
    album: &str,
) -> Option<&'a str> {
    let candidates = albums.get(key)?;
    let album = normalize(album);
    if !album.is_empty() {
        if let Some(id) = candidates.get(&album) {
            return id.as_deref();
        }
    }
    let mut ids = candidates.values();
    let id = ids.next()?.as_deref()?;
    ids.all(|candidate| candidate.as_deref() == Some(id))
        .then_some(id)
}

fn index_albums(csv: &str) -> AlbumsByTrack {
    let mut albums = AlbumsByTrack::new();
    for row in parse_csv(csv)
        .into_iter()
        .skip(1)
        .filter(|row| row.len() >= 7)
    {
        let id = row[6].trim();
        if id.len() <= 20 && !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()) {
            albums
                .entry((normalize(&row[1]), normalize(&row[2])))
                .or_default()
                .entry(normalize(&row[3]))
                .and_modify(|previous| {
                    if previous.as_deref() != Some(id) {
                        *previous = None;
                    }
                })
                .or_insert_with(|| Some(id.to_string()));
        }
    }
    albums
}

fn parse_csv(input: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = input.trim_start_matches('\u{feff}').chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => row.push(std::mem::take(&mut field)),
            '\n' if !quoted => {
                if field.ends_with('\r') {
                    field.pop();
                }
                row.push(std::mem::take(&mut field));
                if !row.is_empty() {
                    rows.push(std::mem::take(&mut row));
                }
            }
            _ => field.push(ch),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::{find_album, index_albums, normalize, parse_csv};

    #[test]
    fn csv_handles_quoted_fields_and_matches_album() {
        let csv = "position,title,artists,album,duration,track_id,album_id,url\n1,Song,Artist,Album,200,1,42,url\n2,\"Other, Song\",\"A, B\",Album,180,2,43,url\n";
        assert_eq!(parse_csv(csv).len(), 3);
        let albums = index_albums(csv);
        let find = |title, artist| find_album(&albums, &(normalize(title), normalize(artist)), "");
        assert_eq!(find("Song", "Artist"), Some("42"));
        assert_eq!(find("Other, Song", "A, B"), Some("43"));
        assert_eq!(find("Song", "Different"), None);
    }

    #[test]
    fn prefers_matching_album_and_rejects_ambiguous_fallback() {
        let albums = index_albums(concat!(
            "position,title,artists,album,duration,track_id,album_id,url\n",
            "1,Song,Artist,Studio Album,120,1,42,url\n",
            "2,Song,Artist,Live Album,151,2,84,url\n",
            "3,Song,Artist,,151,2,84,url\n",
        ));
        let key = (normalize("Song"), normalize("Artist"));
        assert_eq!(find_album(&albums, &key, "Studio Album"), Some("42"));
        assert_eq!(find_album(&albums, &key, "LIVE ALBUM"), Some("84"));
        assert_eq!(find_album(&albums, &key, "Unknown Album"), None);
        assert_eq!(find_album(&albums, &key, ""), None);
    }

    #[test]
    fn duplicate_ids_allow_fallback_and_conflicting_normalized_albums_do_not_match() {
        let albums = index_albums(concat!(
            "position,title,artists,album,duration,track_id,album_id,url\n",
            "1,Song,Artist,Live Album,120,1,42,url\n",
            "2,Song,Artist,Live-Album,120,1,84,url\n",
            "3,Song,Artist,Live Album,120,1,42,url\n",
            "4,Song,Artist,Studio Album,151,2,42,url\n",
            "5,Other Song,Artist,Original Album,151,2,42,url\n",
            "6,Other Song,Artist,Original Album,151,2,42,url\n",
            "7,Other Song,Artist,Reissue,151,2,42,url\n",
        ));
        let key = (normalize("Song"), normalize("Artist"));
        assert_eq!(find_album(&albums, &key, "Live Album"), None);
        assert_eq!(find_album(&albums, &key, "Studio Album"), Some("42"));
        assert_eq!(find_album(&albums, &key, ""), None);
        let same_id = (normalize("Other Song"), normalize("Artist"));
        assert_eq!(find_album(&albums, &same_id, "Unknown Album"), Some("42"));
    }

    #[test]
    #[ignore = "требует доступ к Яндекс Музыке и подключённый CSV"]
    fn live_album_cover_downloads() {
        let track = crate::Track {
            path: "/tmp/own-player-cover-test.mp3".into(),
            title: "Splinter Cell".into(),
            artist: "YUNG TRAPPA".into(),
            album: String::new(),
            duration: 0.0,
            cover_path: None,
            lyrics: Vec::new(),
        };
        let path = super::online_cover(&track).expect("обложка должна загрузиться");
        let bytes = std::fs::read(path).unwrap();
        assert!(bytes.starts_with(&[0xff, 0xd8, 0xff]));
    }
}
