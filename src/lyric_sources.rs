//! Provider payloads are converted to the same Enhanced LRC representation.
use crate::{library::parse_lyrics, lyric_timing::stamp, Lyric, Track};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct Lyricsfile {
    version: String,
    #[serde(default)]
    metadata: Metadata,
    #[serde(default)]
    lines: Vec<SourceLine>,
    #[serde(default)]
    plain: String,
}
#[derive(Default, Deserialize)]
struct Metadata {
    #[serde(default)]
    offset_ms: i64,
}
#[derive(Deserialize)]
struct SourceLine {
    text: String,
    start_ms: u64,
    end_ms: Option<u64>,
    #[serde(default)]
    words: Vec<SourceWord>,
}
#[derive(Deserialize)]
struct SourceWord {
    text: String,
    start_ms: u64,
    end_ms: Option<u64>,
}

fn timed_line(
    text: &str,
    start: f64,
    end: Option<f64>,
    words: Vec<(String, f64, Option<f64>)>,
) -> Lyric {
    let valid = !words.is_empty()
        && words.len() <= 4096
        && end.is_none_or(|end| end.is_finite() && end > start)
        && words
            .iter()
            .map(|word| word.0.as_str())
            .collect::<String>()
            .trim()
            == text.trim()
        && words.iter().all(|(_, time, word_end)| {
            time.is_finite()
                && *time >= start
                && end.is_none_or(|end| *time < end)
                && word_end
                    .is_none_or(|e| e.is_finite() && e >= *time && end.is_none_or(|end| e <= end))
        })
        && words.windows(2).all(|pair| pair[0].1 <= pair[1].1);
    let mut raw = String::new();
    if valid {
        for (text, time, _) in &words {
            raw.push_str(&format!("<{}>{text}", stamp(*time)));
        }
        let word_end = words.last().and_then(|word| word.2).or(end);
        if let Some(end) = word_end.filter(|end| *end >= words.last().unwrap().1) {
            raw.push_str(&format!("<{}>", stamp(end)));
        }
    } else {
        raw.push_str(text.trim());
    }
    Lyric {
        time: Some(start),
        text: raw,
    }
}

pub(crate) fn lrclib(value: &Value) -> Option<Vec<Lyric>> {
    if let Some(raw) = value["lyricsfile"]
        .as_str()
        .filter(|s| s.len() <= 2 * 1024 * 1024)
    {
        if let Ok(file) = serde_yaml_ng::from_str::<Lyricsfile>(raw) {
            if file.version == "1.0" && file.lines.len() <= 4096 {
                let shift =
                    |ms: u64| (ms as f64 + file.metadata.offset_ms as f64).max(0.0) / 1000.0;
                let mut lines: Vec<_> = file
                    .lines
                    .into_iter()
                    .map(|line| {
                        timed_line(
                            &line.text,
                            shift(line.start_ms),
                            line.end_ms.map(shift),
                            line.words
                                .into_iter()
                                .map(|word| {
                                    (word.text, shift(word.start_ms), word.end_ms.map(shift))
                                })
                                .collect(),
                        )
                    })
                    .collect();
                lines.sort_by(|a, b| a.time.unwrap().total_cmp(&b.time.unwrap()));
                if crate::lyric_timing::quality(&lines) > 0 {
                    return Some(lines);
                }
                let plain = parse_lyrics(&file.plain);
                if crate::lyric_timing::quality(&plain) > 0 {
                    return Some(plain);
                }
            }
        }
    }
    ["syncedLyrics", "plainLyrics"]
        .into_iter()
        .find_map(|field| {
            let lines = parse_lyrics(value[field].as_str()?);
            (crate::lyric_timing::quality(&lines) > 0).then_some(lines)
        })
}

fn normalized(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .filter(|c| c.is_alphanumeric())
        .collect()
}

pub(crate) fn lyricsplus(value: &Value, track: &Track) -> Option<Vec<Lyric>> {
    // The provider reports the chosen recording separately from its lyrics.
    // Do not accept a similarly named cover/remix merely because it has words.
    let matched = &value["processingTime"]["selectedSongMetadata"];
    if normalized(matched["title"].as_str()?) != normalized(&track.title)
        || !normalized(matched["artist"].as_str()?).contains(&normalized(&track.artist))
    {
        return None;
    }
    let duration = matched["duration"].as_f64()?;
    if track.duration > 0.0 && (duration - track.duration).abs() > 5.0 {
        return None;
    }
    let kind = value["type"].as_str()?;
    let source = value["lyrics"].as_array()?;
    if source.len() > 4096 {
        return None;
    }
    let mut lines = Vec::new();
    for line in source {
        let text = line["text"].as_str().unwrap_or("");
        if kind == "None" {
            lines.push(Lyric {
                time: None,
                text: text.into(),
            });
            continue;
        }
        let start = line["time"].as_u64()? as f64 / 1000.0;
        let end = line["duration"].as_u64().map(|d| start + d as f64 / 1000.0);
        let words = if kind == "Word" {
            line["syllabus"]
                .as_array()
                .filter(|words| words.len() <= 4096)
                .and_then(|words| {
                    words
                        .iter()
                        .map(|word| {
                            let start = word["time"].as_u64()? as f64 / 1000.0;
                            Some((
                                word["text"].as_str()?.to_owned(),
                                start,
                                word["duration"].as_u64().map(|d| start + d as f64 / 1000.0),
                            ))
                        })
                        .collect::<Option<Vec<_>>>()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        lines.push(timed_line(text, start, end, words));
    }
    lines.sort_by(|a, b| a.time.unwrap_or(0.0).total_cmp(&b.time.unwrap_or(0.0)));
    (crate::lyric_timing::quality(&lines) > 0).then_some(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyric_timing::{markup, quality};
    use serde_json::json;
    #[test]
    fn empty_synced_payload_does_not_hide_plain_lyrics() {
        for synced in ["[ar:Artist]", "[00:00]\n[00:01]  ", "[00:00]<00:00>"] {
            let lines = lrclib(&json!({"syncedLyrics": synced, "plainLyrics": "Actual text"}))
                .expect("plain fallback must survive an empty synced payload");
            assert_eq!(lines[0].text, "Actual text");
        }
    }

    #[test]
    fn empty_lyricsfile_does_not_hide_other_provider_fields() {
        let lines = lrclib(&json!({
            "lyricsfile": "version: '1.0'\nlines:\n- text: ''\n  start_ms: 0\n",
            "syncedLyrics": "[00:01]Actual text"
        }))
        .unwrap();
        assert_eq!(lines[0].text, "Actual text");
    }

    #[test]
    fn empty_lyricsplus_response_is_not_a_found_text() {
        let track = crate::library::preview_track("Artist - Song.mp3".into());
        let result = lyricsplus(
            &json!({
                "processingTime": {"selectedSongMetadata": {"title":"Song", "artist":"Artist", "duration":20}},
                "type": "Line", "lyrics": [{"time":0,"text":"  "}]
            }),
            &track,
        );
        assert!(result.is_none());
    }

    #[test]
    fn unsupported_yaml_and_truncated_payload_use_synced_fallback() {
        for raw in [
            "version: '2.0'\nlines: []",
            "lines: [",
            "version: '1.0'\nlines: not-a-list",
        ] {
            let lines = lrclib(&json!({"lyricsfile":raw,"syncedLyrics":"[00:01]Text"})).unwrap();
            assert_eq!(lines[0].text, "Text");
        }
    }

    #[test]
    fn lyricsfile_retains_words_whitespace_and_explicit_timing() {
        let doc = json!({"syncedLyrics":"[00:01]fallback", "lyricsfile":"version: '1.0'\nlines:\n- text: 'Тихо мир'\n  start_ms: 1000\n  end_ms: 4000\n  words:\n  - text: 'Тихо '\n    start_ms: 1000\n  - text: мир\n    start_ms: 3000\n"});
        let lines = lrclib(&doc).unwrap();
        assert_eq!(quality(&lines), 3);
        assert_eq!(markup(&lines[0].text).0, "Тихо мир");
        assert_eq!(
            markup(&lines[0].text).1,
            vec![(0, 1.0), (9, 3.0), (15, 4.0)]
        );
    }
    #[test]
    fn invalid_word_text_does_not_misalign_the_line() {
        let line = timed_line("Hello", 1.0, Some(3.0), vec![("Help".into(), 1.0, None)]);
        assert_eq!(line.text, "Hello");
        assert_eq!(quality(&[line]), 2);
    }

    #[test]
    fn provider_words_must_fit_the_declared_line_interval() {
        for (onset, end) in [(4.0, None), (2.0, Some(5.0))] {
            let line = timed_line(
                "One two",
                1.0,
                Some(4.0),
                vec![("One ".into(), 1.0, None), ("two".into(), onset, end)],
            );
            assert_eq!(line.text, "One two");
            assert_eq!(quality(&[line]), 2);
        }
    }
}
