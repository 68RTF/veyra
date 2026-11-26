use crate::{Lyric, Track};
use serde_json::Value;
use std::fs;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, UNIX_EPOCH};

pub(crate) fn collect_audio(path: &Path, out: &mut Vec<PathBuf>) {
    // Canonical directory identities prevent cycles through symbolic links.
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
                    if let Some(lines) = crate::lyrics::read_cache(&track) {
                        if crate::lyric_timing::rank(&lines)
                            > crate::lyric_timing::rank(&track.lyrics)
                        {
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
    let output = run_media_tool(
        Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_format",
                "-show_streams",
                "-of",
                "json",
            ])
            .arg(&path),
        Duration::from_secs(8),
    )
    .ok()
    .flatten();
    let output_ok = output.is_some();
    let mut title = preview.title;
    let mut artist = preview.artist;
    let mut album = String::new();
    let mut duration = 0.0;
    let mut embedded_lyrics = String::new();
    if let Some(output) = output {
        if let Ok(value) = serde_json::from_slice::<Value>(&output) {
            if let Some(format) = value.get("format") {
                duration = format["duration"]
                    .as_str()
                    .and_then(|v| v.parse::<f64>().ok())
                    .filter(|duration| duration.is_finite() && *duration >= 0.0)
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
    let lyrics = [lrc, txt]
        .into_iter()
        .find_map(|candidate| {
            fs::read_to_string(candidate)
                .ok()
                .map(|text| parse_lyrics(&text))
                .filter(|lines| crate::lyric_timing::quality(lines) > 0)
        })
        .or_else(|| {
            let lines = parse_lyrics(&embedded_lyrics);
            (crate::lyric_timing::quality(&lines) > 0).then_some(lines)
        })
        .unwrap_or_default();
    let mut track = Track {
        path,
        title,
        artist,
        album,
        duration,
        cover_path: None,
        lyrics,
    };
    if let Some(cached) = crate::lyrics::read_cache(&track) {
        if crate::lyric_timing::rank(&cached) > crate::lyric_timing::rank(&track.lyrics) {
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
    // Reparse sidecars cached before fallback from empty LRC files was fixed.
    let mut signature = format!("v5:{}", path.to_string_lossy());
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
    for extension in ["jpg", "jpeg", "png", "webp"] {
        let candidate = path.with_extension(extension);
        if candidate.is_file() {
            return Some(candidate);
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
    if fs::metadata(&output).is_ok_and(|metadata| metadata.len() > 0) {
        return Some(output);
    }
    let temporary = output.with_extension(format!(
        "{}-{}.part.jpg",
        std::process::id(),
        COVER_TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let result = run_media_tool(
        Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y", "-i"])
            .arg(path)
            .args([
                "-map",
                "0:v:0",
                "-frames:v",
                "1",
                "-vf",
                "scale=900:900:force_original_aspect_ratio=decrease",
            ])
            .arg(&temporary),
        Duration::from_secs(12),
    );
    if matches!(result, Ok(Some(_))) && fs::rename(&temporary, &output).is_ok() {
        Some(output)
    } else {
        let _ = fs::remove_file(&temporary);
        None
    }
}

static COVER_TEMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn run_media_tool(command: &mut Command, timeout: Duration) -> io::Result<Option<Vec<u8>>> {
    const MAX_OUTPUT: usize = 2 * 1024 * 1024;
    let (mut reader, writer) = UnixStream::pair()?;
    reader.set_nonblocking(true)?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(OwnedFd::from(writer)))
        .stderr(Stdio::null())
        .spawn()?;
    // Command retains the writer after spawn. Close the parent's copy.
    command.stdout(Stdio::null());
    let deadline = Instant::now() + timeout;
    let result = (|| {
        let mut buffer = Vec::new();
        let mut chunk = [0; 8192];
        let mut status: Option<bool> = None;
        loop {
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            match reader.read(&mut chunk) {
                Ok(0) => {}
                Ok(length) => {
                    buffer.extend_from_slice(&chunk[..length]);
                    if buffer.len() > MAX_OUTPUT {
                        return Err(io::ErrorKind::InvalidData.into());
                    }
                    continue;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
            if let Some(status) = status {
                return Ok(status.then_some(buffer));
            }
            if let Some(done) = child.try_wait()? {
                // Drain bytes written between the previous read and process exit.
                status = Some(done.success());
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

fn tag(value: &Value, keys: &[&str]) -> Option<String> {
    let object = value.as_object()?;
    for key in keys {
        for (name, item) in object {
            if !key.eq_ignore_ascii_case(name) {
                continue;
            }
            if let Some(text) = item.as_str().map(str::trim).filter(|text| !text.is_empty()) {
                return Some(text.to_string());
            }
        }
    }
    None
}

pub(crate) fn parse_lyrics(text: &str) -> Vec<Lyric> {
    let text = text.trim_start_matches('\u{feff}');
    let offset = text
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("[offset:")?
                .strip_suffix(']')?
                .trim()
                .parse::<i64>()
                .ok()
        })
        .next_back()
        .unwrap_or(0) as f64
        / 1000.0;
    let mut timed = Vec::new();
    let mut plain = Vec::new();
    for raw in text.lines() {
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
                    time: Some((time - offset).max(0.0)),
                    text: crate::lyric_timing::offset_markup(rest.trim(), offset),
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
    fn artist_tag_takes_precedence_over_album_artist() {
        let keys = ["artist", "album_artist", "performer"];
        let tags = serde_json::json!({
            "album_artist": "Various Artists",
            "artist": "Track Artist",
            "performer": "Performer"
        });
        assert_eq!(super::tag(&tags, &keys).as_deref(), Some("Track Artist"));

        let tags = serde_json::json!({
            "artist": "  ",
            "Album_Artist": " Various Artists ",
            "performer": "Performer"
        });
        assert_eq!(super::tag(&tags, &keys).as_deref(), Some("Various Artists"));
    }

    #[test]
    fn media_tool_times_out_and_captures_stdout() {
        use std::process::Command;
        use std::time::Duration;

        let output =
            super::run_media_tool(Command::new("printf").arg("tags"), Duration::from_secs(2))
                .unwrap();
        assert_eq!(output.as_deref(), Some(&b"tags"[..]));
        let error =
            super::run_media_tool(Command::new("sleep").arg("5"), Duration::from_millis(30))
                .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    }

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
    fn lrc_offset_moves_lines_and_word_cues_together() {
        use crate::lyric_timing::{markup, quality};
        // Global metadata also applies when it appears after the lyrics.
        let lines = parse_lyrics("\u{feff}[00:02]<00:02>Тихо <00:03>мир<00:04>\n[offset:+1000]");
        assert_eq!(lines[0].time, Some(1.0));
        assert_eq!(
            markup(&lines[0].text),
            ("Тихо мир".into(), vec![(0, 1.0), (9, 2.0), (15, 3.0)])
        );
        assert_eq!(quality(&lines), 3);
        let delayed = parse_lyrics("[offset:-500]\n[00:02]<00:02>One <00:03>two");
        assert_eq!(delayed[0].time, Some(2.5));
        assert_eq!(markup(&delayed[0].text).1, vec![(0, 2.5), (4, 3.5)]);
        let clamped = parse_lyrics("[offset:2000]\n[00:01]<00:01>One <00:03>two");
        assert_eq!(clamped[0].time, Some(0.0));
        assert_eq!(markup(&clamped[0].text).1, vec![(0, 0.0), (4, 1.0)]);
    }

    #[test]
    fn lrc_offset_handles_repeated_lines_and_ignores_invalid_values() {
        let lines = parse_lyrics("[offset:500]\n[00:01][00:03]Again\n[offset:NaN]");
        assert_eq!(
            lines.iter().map(|line| line.time).collect::<Vec<_>>(),
            vec![Some(0.5), Some(2.5)]
        );
        let unchanged = parse_lyrics("[offset:inf]\n[00:01]<00:01.125>Word");
        assert_eq!(unchanged[0].time, Some(1.0));
        assert_eq!(unchanged[0].text, "<00:01.125>Word");
        assert_eq!(
            parse_lyrics("[offset:1000]\nPlain text")[0].text,
            "Plain text"
        );
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
        // A sidecar containing only LRC tags has no lyrics and must not hide TXT.
        std::fs::write(path.with_extension("lrc"), "[ar:Artist]\n[ti:Song]\n").unwrap();
        std::fs::write(path.with_extension("txt"), "Local lyrics\nSecond line").unwrap();
        let cache = metadata_cache_path(&path);
        let track = read_track(path.clone());
        assert_eq!(track.title, "Splinter Cell");
        assert_eq!(track.artist, "YUNG TRAPPA");
        assert_eq!(track.lyrics.len(), 2);
        assert_eq!(track.lyrics[0].text, "Local lyrics");
        if let Some(cache) = cache {
            let _ = std::fs::remove_file(cache);
        }
        std::fs::write(path.with_extension("lrc"), "[00:00]<00:00>\n").unwrap();
        let cache = metadata_cache_path(&path);
        assert_eq!(read_track(path).lyrics[0].text, "Local lyrics");
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

    #[test]
    fn extreme_offsets_and_duplicate_timestamps_stay_finite_and_sorted() {
        for offset in [i64::MIN, i64::MAX, -1, 0, 1] {
            let raw =
                format!("\u{feff}[offset:{offset}]\r\n[00:02][00:02]<00:02>Я 👩‍🚀\r\n[00:01]Earlier");
            let lines = parse_lyrics(&raw);
            assert_eq!(lines.len(), 3);
            assert!(lines.iter().all(|line| line
                .time
                .is_some_and(|time| time.is_finite() && time >= 0.0)));
            assert!(lines.windows(2).all(|pair| pair[0].time <= pair[1].time));
        }
    }

    #[test]
    fn malformed_timestamp_prefixes_never_create_invalid_times() {
        for raw in [
            "[",
            "[00:",
            "[00:60]text",
            "[18446744073709551616:00]text",
            "[00:1e999]text",
            "[00:00]\0Я",
            "\u{feff}\u{feff}[00:01]text",
        ] {
            let lines = parse_lyrics(raw);
            assert!(lines
                .iter()
                .filter_map(|line| line.time)
                .all(|time| time.is_finite() && time >= 0.0));
        }
    }

    #[test]
    fn media_tool_rejects_excessive_stdout_and_failed_exit() {
        use std::{process::Command, time::Duration};
        let error = super::run_media_tool(
            Command::new("head").args(["-c", "2100000", "/dev/zero"]),
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        let result = super::run_media_tool(
            Command::new("sh").args(["-c", "printf partial; exit 3"]),
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn import_tolerates_broken_links_and_non_audio_files() {
        let root = std::env::temp_dir().join(format!("veyra-import-links-{}", std::process::id()));
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::write(root.join("nested/А\n\"Б.OPUS"), b"fixture").unwrap();
        std::fs::write(root.join("notes.txt"), b"not audio").unwrap();
        std::os::unix::fs::symlink(root.join("missing"), root.join("broken.mp3")).unwrap();
        std::os::unix::fs::symlink(&root, root.join("nested/loop")).unwrap();
        let mut found = Vec::new();
        super::collect_audio(&root, &mut found);
        assert_eq!(
            found,
            vec![root.join("nested/А\n\"Б.OPUS").canonicalize().unwrap()]
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
