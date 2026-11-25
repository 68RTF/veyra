use crate::{Lyric, Track};
use serde_json::Value;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::UNIX_EPOCH;

pub(crate) fn collect_audio(path: &Path, out: &mut Vec<PathBuf>) {
    // Use canonical directory identities to avoid symlink cycles.
    let mut pending = vec![path.to_path_buf()];
    let mut visited = std::collections::HashSet::new();
    while let Some(path) = pending.pop() {
        let Ok(path) = path.canonicalize() else {
            continue;
        };
        if path.is_dir() {
            if !visited.insert(path.clone()) {
                continue;
            }
            if let Ok(entries) = fs::read_dir(&path) {
                pending.extend(entries.flatten().map(|entry| entry.path()));
            }
        } else if path.is_file() && is_audio(&path) {
            out.push(path);
        }
    }
}

fn is_audio(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase()
            .as_str(),
        "mp3"
            | "m4a"
            | "flac"
            | "wav"
            | "ogg"
            | "opus"
            | "aac"
            | "aiff"
            | "ape"
            | "oga"
            | "wma"
            | "webm"
    )
}

pub(crate) fn preview_track(path: PathBuf) -> Track {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Без названия");
    let (artist, title) = parse_filename(stem);
    Track {
        path,
        title,
        artist: artist.unwrap_or_else(|| "Неизвестный исполнитель".to_string()),
        album: String::new(),
        duration: 0.0,
        cover_path: None,
        lyrics: Vec::new(),
    }
}

pub(crate) fn read_track(path: PathBuf) -> Track {
    let cache = metadata_cache_path(&path);
    if let Some(ref cache_path) = cache {
        if let Ok(data) = fs::read(cache_path) {
            if let Ok(mut track) = serde_json::from_slice::<Track>(&data) {
                if track.path == path {
                    if track.lyrics.is_empty() {
                        if let Some(lines) = crate::lyrics::read_cache(&track) {
                            track.lyrics = lines;
                        }
                    }
                    return track;
                }
            }
        }
    }
    let preview = preview_track(path.clone());
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Без названия");
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_format",
            "-show_streams",
            "-of",
            "json",
        ])
        .arg(&path)
        .output()
        .ok();
    let output_ok = output
        .as_ref()
        .map(|result| result.status.success())
        .unwrap_or(false);
    let mut title = preview.title;
    let mut artist = preview.artist;
    let mut album = String::new();
    let mut duration = 0.0;
    let mut embedded_lyrics = String::new();
    if let Some(output) = output {
        if let Ok(value) = serde_json::from_slice::<Value>(&output.stdout) {
            if let Some(format) = value.get("format") {
                duration = format["duration"]
                    .as_str()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0.0);
                if let Some(tags) = format.get("tags") {
                    if let Some(tag_title) = tag(tags, &["title"]) {
                        if tag_title.trim() != stem && !tag_title.trim().is_empty() {
                            title = tag_title;
                        }
                    }
                    artist = tag(tags, &["artist", "album_artist", "performer"]).unwrap_or(artist);
                    album = tag(tags, &["album"]).unwrap_or_default();
                    embedded_lyrics = tag(tags, &["lyrics", "unsyncedlyrics", "syncedlyrics"])
                        .unwrap_or_default();
                }
            }
            if embedded_lyrics.is_empty() {
                if let Some(streams) = value["streams"].as_array() {
                    for stream in streams {
                        if let Some(tags) = stream.get("tags") {
                            if let Some(text) =
                                tag(tags, &["lyrics", "unsyncedlyrics", "syncedlyrics"])
                            {
                                embedded_lyrics = text;
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
    let lrc = path.with_extension("lrc");
    let txt = path.with_extension("txt");
    let text = [lrc, txt]
        .into_iter()
        .find_map(|candidate| {
            fs::read_to_string(candidate)
                .ok()
                .filter(|text| !text.trim().is_empty())
        })
        .unwrap_or(embedded_lyrics);
    let mut track = Track {
        path,
        title,
        artist,
        album,
        duration,
        cover_path: None,
        lyrics: parse_lyrics(&text),
    };
    if track.lyrics.is_empty() {
        if let Some(cached) = crate::lyrics::read_cache(&track) {
            track.lyrics = cached;
        }
    }
    if output_ok {
        write_metadata_cache(cache.as_deref(), &track);
    }
    track
}

fn metadata_cache_path(path: &Path) -> Option<PathBuf> {
    let root = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    let mut signature = format!("v1:{}", path.to_string_lossy());
    for candidate in [
        path.to_path_buf(),
        path.with_extension("lrc"),
        path.with_extension("txt"),
    ] {
        if let Ok(metadata) = fs::metadata(candidate) {
            let modified = metadata
                .modified()
                .ok()?
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_nanos();
            signature.push_str(&format!("|{}:{modified}", metadata.len()));
        } else {
            signature.push_str("|missing");
        }
    }
    let hash = signature
        .bytes()
        .fold(14695981039346656037u64, |hash, byte| {
            (hash ^ byte as u64).wrapping_mul(1099511628211)
        });
    Some(
        root.join("own-player/metadata")
            .join(format!("{hash:016x}.json")),
    )
}

fn write_metadata_cache(path: Option<&Path>, track: &Track) {
    let Some(path) = path else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(data) = serde_json::to_vec(track) else {
        return;
    };
    if let Ok(mut file) = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
    {
        let _ = file.write_all(&data);
    }
}

fn parse_filename(stem: &str) -> (Option<String>, String) {
    let mut name = stem.trim();
    let digits = name.bytes().take_while(u8::is_ascii_digit).count();
    if (1..=3).contains(&digits) {
        let tail = name[digits..].trim_start();
        let after_number = tail
            .strip_prefix('-')
            .or_else(|| tail.strip_prefix(". "))
            .or_else(|| tail.strip_prefix(") "));
        if let Some(rest) = after_number.filter(|rest| !rest.trim().is_empty()) {
            name = rest.trim();
        }
    }
    let separator = [" - ", " – ", " — "]
        .iter()
        .filter_map(|separator| name.find(separator).map(|index| (index, *separator)))
        .min_by_key(|(index, _)| *index);
    if let Some((index, separator)) = separator {
        let artist = &name[..index];
        let title = &name[index + separator.len()..];
        if !artist.trim().is_empty() && !title.trim().is_empty() {
            return (Some(artist.trim().to_string()), title.trim().to_string());
        }
    }
    (None, name.to_string())
}

pub(crate) fn extract_cover(path: &Path) -> Option<PathBuf> {
    for name in [
        "cover.jpg",
        "cover.jpeg",
        "cover.png",
        "cover.webp",
        "folder.jpg",
        "front.jpg",
    ] {
        let candidate = path.parent()?.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    if let Some(stem) = path.file_stem() {
        for extension in ["jpg", "jpeg", "png", "webp"] {
            let candidate = path.parent()?.join(stem).with_extension(extension);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let root = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    let cache = root.join("own-player").join("covers");
    fs::create_dir_all(&cache).ok()?;
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let key = path
        .to_string_lossy()
        .bytes()
        .fold(14695981039346656037u64, |hash, byte| {
            (hash ^ byte as u64).wrapping_mul(1099511628211)
        });
    let name = format!("{key:016x}-{}-{modified}.jpg", metadata.len());
    let output = cache.join(name);
    if output.is_file() {
        return Some(output);
    }
    let result = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(path)
        .args([
            "-map",
            "0:v:0",
            "-frames:v",
            "1",
            "-vf",
            "scale=900:900:force_original_aspect_ratio=decrease",
        ])
        .arg(&output)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    if result.success() && output.is_file() {
        Some(output)
    } else {
        None
    }
}

fn tag(value: &Value, keys: &[&str]) -> Option<String> {
    let object = value.as_object()?;
    for (name, item) in object {
        if keys.iter().any(|key| key.eq_ignore_ascii_case(name)) {
            if let Some(text) = item.as_str() {
                if !text.trim().is_empty() {
                    return Some(text.trim().to_string());
                }
            }
        }
    }
    None
}

pub(crate) fn parse_lyrics(text: &str) -> Vec<Lyric> {
    let mut timed = Vec::new();
    let mut plain = Vec::new();
    for raw in text.trim_start_matches('\u{feff}').lines() {
        let mut rest = raw.trim();
        let mut stamps = Vec::new();
        while let Some(after_open) = rest.strip_prefix('[') {
            let Some((stamp, after)) = after_open.split_once(']') else {
                break;
            };
            let Some((minutes, seconds)) = stamp.split_once(':') else {
                break;
            };
            let (Ok(minutes), Ok(seconds)) = (
                minutes.parse::<u64>(),
                seconds.replace(',', ".").parse::<f64>(),
            ) else {
                break;
            };
            if !seconds.is_finite() || !(0.0..60.0).contains(&seconds) {
                break;
            }
            stamps.push(minutes as f64 * 60.0 + seconds);
            rest = after.trim_start();
        }
        if !stamps.is_empty() {
            for time in stamps {
                timed.push(Lyric {
                    time: Some(time),
                    text: rest.trim().to_string(),
                });
            }
        } else if !rest.is_empty() && !(rest.starts_with('[') && rest.ends_with(']')) {
            plain.push(Lyric {
                time: None,
                text: rest.to_string(),
            });
        }
    }
    if !timed.is_empty() {
        timed.sort_by(|a, b| a.time.unwrap_or(0.0).total_cmp(&b.time.unwrap_or(0.0)));
        timed
    } else {
        plain
    }
}

#[cfg(test)]
mod tests {
    use super::{metadata_cache_path, parse_filename, parse_lyrics, read_track};

    #[test]
    fn directory_import_handles_symlink_cycles() {
        let root = std::env::temp_dir().join(format!("own-player-cycle-{}", std::process::id()));
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::write(root.join("song.MP3"), b"fixture").unwrap();
        std::os::unix::fs::symlink(&root, root.join("nested/loop")).unwrap();
        let mut tracks = Vec::new();
        super::collect_audio(&root, &mut tracks);
        assert_eq!(tracks, vec![root.join("song.MP3").canonicalize().unwrap()]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_non_finite_and_negative_timestamps() {
        let lines = parse_lyrics("[00:NaN]bad\n[00:-1]bad\n[00:inf]bad\n[00:02]good");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].time, Some(2.0));
    }

    #[test]
    fn parses_multiple_timestamps_and_sorts_lines() {
        let lines = parse_lyrics("[00:20.50]later\n[00:02.25][00:04.50]hello\n[ar:Artist]");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].time, Some(2.25));
        assert_eq!(lines[1].time, Some(4.5));
        assert_eq!(lines[2].text, "later");
    }

    #[test]
    fn keeps_plain_lyrics_without_lrc_metadata() {
        let lines = parse_lyrics("\u{feff}[ar:Artist]\nFirst line\nSecond line");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "First line");
        assert_eq!(lines[1].time, None);
    }

    #[test]
    fn filename_separates_number_artist_and_title() {
        assert_eq!(
            parse_filename("01 - YUNG TRAPPA - Splinter Cell"),
            (Some("YUNG TRAPPA".into()), "Splinter Cell".into())
        );
        assert_eq!(
            parse_filename("YUNG TRAPPA - Splinter Cell"),
            (Some("YUNG TRAPPA".into()), "Splinter Cell".into())
        );
        assert_eq!(
            parse_filename("01 - Splinter Cell"),
            (None, "Splinter Cell".into())
        );
        assert_eq!(
            parse_filename("02. Artist — Song"),
            (Some("Artist".into()), "Song".into())
        );
        assert_eq!(
            parse_filename("Artist – Song"),
            (Some("Artist".into()), "Song".into())
        );
        assert_eq!(
            parse_filename("1979 - Song"),
            (Some("1979".into()), "Song".into())
        );
    }

    #[test]
    fn imports_untagged_audio_with_searchable_metadata() {
        let directory =
            std::env::temp_dir().join(format!("own-player-filename-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("01 - YUNG TRAPPA - Splinter Cell.wav");
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "anullsrc=r=8000:cl=mono",
                "-t",
                "0.1",
                "-y",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        let cache = metadata_cache_path(&path);
        let track = read_track(path);
        assert_eq!(track.title, "Splinter Cell");
        assert_eq!(track.artist, "YUNG TRAPPA");
        if let Some(cache) = cache {
            let _ = std::fs::remove_file(cache);
        }
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn metadata_cache_invalidates_when_sidecar_changes() {
        let directory =
            std::env::temp_dir().join(format!("own-player-cache-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let audio = directory.join("song.mp3");
        std::fs::write(&audio, b"audio").unwrap();
        let before = metadata_cache_path(&audio).unwrap();
        std::fs::write(audio.with_extension("lrc"), b"[00:01]line").unwrap();
        let after = metadata_cache_path(&audio).unwrap();
        assert_ne!(before, after);
        let _ = std::fs::remove_dir_all(directory);
    }
}
