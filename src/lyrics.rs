use crate::{library::parse_lyrics, Lyric, Track};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::UNIX_EPOCH;

pub(crate) fn online_lyrics(track: &Track) -> Result<Vec<Lyric>, String> {
    if let Some(lines) = read_cache(track) {
        return Ok(lines);
    }
    let title = track.title.trim();
    if title.is_empty() {
        return Err("Не получилось начать поиск текста: у трека нет названия.".into());
    }
    let artist = track.artist.trim();
    let known_artist = !artist.is_empty() && artist != "Неизвестный исполнитель";

    if known_artist && !track.album.trim().is_empty() && track.duration > 0.0 {
        let mut params = vec![("track_name", title), ("artist_name", artist)];
        if !track.album.trim().is_empty() {
            params.push(("album_name", track.album.trim()));
        }
        let duration = track.duration.round().to_string();
        if track.duration > 0.0 {
            params.push(("duration", &duration));
        }
        if let Some(value) = request("get", &params)? {
            if let Some(lines) = lyric_lines(&value) {
                write_cache(track, &lines);
                return Ok(lines);
            }
        }
    }

    let short = short_title(title);
    let broad = format!("{artist} {short}");
    let mut queries = Vec::new();
    if known_artist {
        queries.push(vec![("track_name", title), ("artist_name", artist)]);
    }
    queries.push(vec![("track_name", title)]);
    if short != title {
        queries.push(vec![("track_name", short)]);
    }
    if known_artist {
        queries.push(vec![("q", broad.as_str())]);
    }
    queries.push(vec![("q", short)]);
    let mut candidates = Vec::new();
    for params in queries {
        if let Some(value) = request("search", &params)? {
            if let Some(found) = value.as_array() {
                candidates.extend(found.iter().cloned());
            }
        }
        if let Some(lines) = choose_candidate(&candidates, title, artist, track.duration) {
            write_cache(track, &lines);
            return Ok(lines);
        }
    }
    Err("В LRCLIB не нашлось подходящего текста. Проверь теги трека или положи .lrc рядом с аудиофайлом.".into())
}

fn request(endpoint: &str, params: &[(&str, &str)]) -> Result<Option<Value>, String> {
    let mut command = Command::new("curl");
    command.args([
        "--silent",
        "--show-error",
        "--location",
        "--max-time",
        "12",
        "--user-agent",
        "Veyra/0.7",
        "--get",
    ]);
    for (name, value) in params {
        command
            .arg("--data-urlencode")
            .arg(format!("{name}={value}"));
    }
    let output = command
        .arg("--write-out")
        .arg("\n%{http_code}")
        .arg(format!("https://lrclib.net/api/{endpoint}"))
        .output()
        .map_err(|error| format!("Не удалось запустить поиск текста: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Нет связи с LRCLIB: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let body = String::from_utf8(output.stdout)
        .map_err(|_| "LRCLIB вернул ответ в неверной кодировке.".to_string())?;
    let (json, status) = body
        .rsplit_once('\n')
        .ok_or_else(|| "LRCLIB вернул неполный ответ.".to_string())?;
    match status.trim() {
        "200" => serde_json::from_str(json)
            .map(Some)
            .map_err(|error| format!("Не удалось прочитать ответ LRCLIB: {error}")),
        "404" => Ok(None),
        "429" => Err("LRCLIB временно ограничил запросы. Попробуй позже.".into()),
        code => Err(format!("LRCLIB вернул ошибку HTTP {code}.")),
    }
}

fn lyric_lines(value: &Value) -> Option<Vec<Lyric>> {
    let text = value["syncedLyrics"]
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .or_else(|| {
            value["plainLyrics"]
                .as_str()
                .filter(|text| !text.trim().is_empty())
        })?;
    let lines = parse_lyrics(text);
    (!lines.is_empty()).then_some(lines)
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
            let name = normalized(candidate["trackName"].as_str()?);
            if name.is_empty() {
                return None;
            }
            if name != wanted_title
                && !name.starts_with(&wanted_title)
                && !wanted_title.starts_with(&name)
            {
                return None;
            }
            let lines = lyric_lines(candidate)?;
            let candidate_artist = normalized(candidate["artistName"].as_str().unwrap_or(""));
            let artist_match =
                !wanted_artist.is_empty() && candidate_artist.contains(&wanted_artist);
            let difference = (candidate["duration"].as_f64().unwrap_or(duration) - duration).abs();
            let known_artist =
                !wanted_artist.is_empty() && wanted_artist != normalized("Неизвестный исполнитель");
            if known_artist && !artist_match {
                return None;
            }
            let score = (if name == wanted_title { 100 } else { 60 })
                + (if artist_match { 40 } else { 0 })
                - (difference.min(120.0) as i32 / 3);
            Some((score, lines))
        })
        .max_by_key(|(score, _)| *score)
        .map(|(_, lines)| lines)
}

fn cache_path(track: &Track) -> Option<PathBuf> {
    let root = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
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
            .join(format!("{key:016x}-{}-{modified}.json", metadata.len())),
    )
}

pub(crate) fn read_cache(track: &Track) -> Option<Vec<Lyric>> {
    let lines: Vec<Lyric> = serde_json::from_slice(&fs::read(cache_path(track)?).ok()?).ok()?;
    (!lines.is_empty()).then_some(lines)
}

fn write_cache(track: &Track, lines: &[Lyric]) {
    if let Some(path) = cache_path(track) {
        if let Some(parent) = path.parent() {
            if fs::create_dir_all(parent).is_ok() {
                if let Ok(data) = serde_json::to_vec(lines) {
                    let _ = fs::write(path, data);
                }
            }
        }
    }
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
