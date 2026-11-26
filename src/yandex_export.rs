use serde_json::{json, Value};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

fn parse_args(args: &[OsString]) -> Result<(&str, Option<PathBuf>), String> {
    const USAGE: &str = "Использование: ym-export PLAYLIST_URL [-o FILE.csv|FILE.json]";
    let Some((url, options)) = args.split_first() else {
        return Err(USAGE.into());
    };
    let output = match options {
        [] => None,
        [flag] if flag == "-o" || flag == "--output" => {
            return Err(format!(
                "После {} укажи путь к файлу. {USAGE}",
                flag.to_string_lossy()
            ));
        }
        [flag, path] if flag == "-o" || flag == "--output" => {
            if path.is_empty() || path == "-o" || path == "--output" {
                return Err(format!(
                    "После {} укажи путь к файлу. {USAGE}",
                    flag.to_string_lossy()
                ));
            }
            Some(PathBuf::from(path))
        }
        _ => return Err(USAGE.into()),
    };
    let url = url
        .to_str()
        .ok_or("Ссылка на плейлист должна быть в UTF-8.")?;
    Ok((url, output))
}

pub fn run(args: &[OsString]) -> Result<(), String> {
    let (playlist_url, output_arg) = parse_args(args)?;
    let after = playlist_url
        .split_once("/playlists/")
        .map(|(_, tail)| tail)
        .ok_or("Ссылка на плейлист должна содержать /playlists/<id>.")?;
    let playlist_id = after.split(['?', '#', '/']).next().unwrap_or("");
    if playlist_id.is_empty() {
        return Err("В ссылке нет идентификатора плейлиста.".into());
    }
    let api = format!("https://api.music.yandex.net/playlist/{playlist_id}");
    let response = Command::new("curl")
        .args(["-fsSL", "--max-time", "25", &api])
        .output()
        .map_err(|e| format!("Не удалось запустить curl: {e}"))?;
    if !response.status.success() {
        return Err(format!(
            "API Яндекс Музыки вернул {}: {}",
            response.status,
            String::from_utf8_lossy(&response.stderr).trim()
        ));
    }
    let data: Value = serde_json::from_slice(&response.stdout)
        .map_err(|e| format!("Не удалось прочитать ответ Яндекс Музыки: {e}"))?;
    let playlist = data.get("result").unwrap_or(&data);
    let title = playlist["title"].as_str().unwrap_or("yandex-playlist");
    let tracks = playlist["tracks"]
        .as_array()
        .ok_or("В ответе нет треков. Возможно, плейлист закрыт или недоступен.")?;
    let mut rows: Vec<Vec<String>> = Vec::with_capacity(tracks.len());
    for (position, item) in tracks.iter().enumerate() {
        let track = item.get("track").unwrap_or(item);
        let track_id = value_string(&track["id"]);
        let album_id = track["albums"]
            .as_array()
            .and_then(|albums| albums.first())
            .map(|album| value_string(&album["id"]))
            .unwrap_or_default();
        let album = track["albums"]
            .as_array()
            .map(|albums| {
                albums
                    .iter()
                    .filter_map(|v| v["title"].as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let artists = track["artists"]
            .as_array()
            .map(|artists| {
                artists
                    .iter()
                    .filter_map(|v| v["name"].as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let duration = track["durationMs"]
            .as_f64()
            .map(|ms| format!("{:.3}", ms / 1000.0))
            .unwrap_or_default();
        let url = if !track_id.is_empty() && !album_id.is_empty() {
            format!("https://music.yandex.ru/album/{album_id}/track/{track_id}")
        } else {
            String::new()
        };
        rows.push(vec![
            (position + 1).to_string(),
            track["title"].as_str().unwrap_or("").to_string(),
            artists,
            album,
            duration,
            track_id,
            album_id,
            url,
        ]);
    }
    let output = output_arg.unwrap_or_else(|| {
        let safe: String = title
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        PathBuf::from(format!("{}.csv", safe.trim_matches('_')))
    });
    let result = if output
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .eq_ignore_ascii_case("json")
    {
        let objects: Vec<Value> = rows.iter().map(|row| json!({
            "position": row[0].parse::<usize>().unwrap_or(0), "title": row[1], "artists": row[2], "album": row[3],
            "duration_seconds": row[4].parse::<f64>().ok(), "track_id": row[5], "album_id": row[6], "url": row[7]
        })).collect();
        let json = serde_json::to_vec_pretty(&json!({"playlist": title, "tracks": objects}))
            .map_err(|e| e.to_string())?;
        write_export(&output, |file| file.write_all(&json))
    } else {
        let mut csv = String::from(
            "\u{feff}position,title,artists,album,duration_seconds,track_id,album_id,url\n",
        );
        for row in &rows {
            csv.push_str(
                &row.iter()
                    .map(|field| csv_field(field))
                    .collect::<Vec<_>>()
                    .join(","),
            );
            csv.push('\n');
        }
        write_export(&output, |file| file.write_all(csv.as_bytes()))
    };
    result.map_err(|error| format!("Не удалось записать {}: {error}", output.display()))?;
    println!(
        "Сохранено треков: {}. Плейлист: «{}». Файл: {}",
        rows.len(),
        title,
        output.display()
    );
    Ok(())
}

fn write_export(output: &Path, write: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()> {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mode = match fs::symlink_metadata(output) {
        Ok(metadata) if metadata.is_file() => metadata.permissions().mode() & 0o777,
        Ok(_) => 0o666,
        Err(error) if error.kind() == io::ErrorKind::NotFound => 0o666,
        Err(error) => return Err(error),
    };
    let (temporary, mut file) = loop {
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".veyra-export-{}-{id}.tmp", std::process::id()));
        // A caller can choose any filename, including one in the temporary namespace.
        if path.file_name() == output.file_name() {
            continue;
        }
        match File::options()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&path)
        {
            Ok(file) => break (path, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    let result = write(&mut file).and_then(|()| file.sync_all());
    drop(file);
    // Rename is the commit point. The file contents are synced, but without
    // syncing the parent directory the rename may be lost after a power failure.
    let result = result.and_then(|()| fs::rename(&temporary, output));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn value_string(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_u64().map(|v| v.to_string()))
        .unwrap_or_default()
}

fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn output_flag_requires_a_path() {
        for flag in ["-o", "--output"] {
            let args = ["https://example.invalid/playlists/1", flag].map(OsString::from);
            let error = run(&args).unwrap_err();
            assert!(error.contains(&format!("После {flag} укажи путь к файлу.")));
        }
    }

    #[test]
    fn output_path_can_be_non_utf8_but_url_cannot() {
        let path = OsString::from_vec(b"export-\xff.json".to_vec());
        let mut args = [
            OsString::from("https://example.invalid/playlists/1"),
            OsString::from("--output"),
            path.clone(),
        ];
        assert_eq!(
            parse_args(&args).unwrap().1,
            Some(PathBuf::from(path.clone()))
        );
        args[0] = path;
        assert!(parse_args(&args).unwrap_err().contains("UTF-8"));
    }

    #[test]
    fn atomic_export_keeps_old_file_and_cleans_up_after_a_partial_write() {
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let directory =
            std::env::temp_dir().join(format!("veyra-export-test-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let fixture = Fixture(directory);
        let output = fixture.0.join("playlist.csv");
        fs::write(&output, b"previous export").unwrap();
        fs::set_permissions(&output, fs::Permissions::from_mode(0o600)).unwrap();

        let error = write_export(&output, |file| {
            file.write_all(b"partial export")?;
            assert_eq!(fs::read(&output)?, b"previous export");
            Err(io::Error::other("write failed"))
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "write failed");
        assert_eq!(fs::read(&output).unwrap(), b"previous export");
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 1);

        write_export(&output, |file| file.write_all(b"complete export")).unwrap();
        assert_eq!(fs::read(&output).unwrap(), b"complete export");
        assert_eq!(
            fs::metadata(&output).unwrap().permissions().mode() & 0o077,
            0
        );
        let directory = fixture.0.join("directory.json");
        fs::create_dir(&directory).unwrap();
        assert!(write_export(&directory, |file| file.write_all(b"new data")).is_err());
        assert!(directory.is_dir());
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 2);
    }
}
