use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

pub fn run(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err("Использование: ym-export PLAYLIST_URL [-o FILE.csv|FILE.json]".into());
    }
    let playlist_url = &args[0];
    let output_arg = args
        .windows(2)
        .find(|pair| pair[0] == "-o" || pair[0] == "--output")
        .map(|pair| PathBuf::from(&pair[1]));
    let after = playlist_url
        .split_once("/playlists/")
        .map(|(_, tail)| tail)
        .ok_or("Ссылка на плейлист должна содержать /playlists/<id>.")?;
    let playlist_id = after.split(['?', '#', '/']).next().unwrap_or("");
    if playlist_id.is_empty() {
        return Err("В ссылке не указан ID плейлиста.".into());
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
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if output
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .eq_ignore_ascii_case("json")
    {
        let objects: Vec<Value> = rows.iter().map(|row| json!({
            "position": row[0].parse::<usize>().unwrap_or(0), "title": row[1], "artists": row[2], "album": row[3],
            "duration_seconds": row[4].parse::<f64>().ok(), "track_id": row[5], "album_id": row[6], "url": row[7]
        })).collect();
        fs::write(
            &output,
            serde_json::to_vec_pretty(&json!({"playlist": title, "tracks": objects}))
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
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
        fs::write(&output, csv).map_err(|e| e.to_string())?;
    }
    println!(
        "Сохранено треков: {}. Плейлист: «{}». Файл: {}",
        rows.len(),
        title,
        output.display()
    );
    Ok(())
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
