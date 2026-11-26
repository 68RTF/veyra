//! Display-only timing. Exact word cues never use the line interpolation path.
use crate::Lyric;
use serde_json::json;
use std::{ffi::OsString, path::PathBuf, time::Instant};

pub(crate) const DIM: u16 = 36044;
pub(crate) type AlphaRange = (u32, u32, u16);

#[derive(Clone, Debug)]
struct Segment {
    start: u32,
    end: u32,
    onset: f64,
    fade: f64,
}

#[derive(Clone, Debug)]
pub(crate) struct Timeline {
    pub raw: String,
    pub text: String,
    pub exact: bool,
    pub invalid_cues: bool,
    pub singing_end: f64,
    segments: Vec<Segment>,
}

pub(crate) fn timestamp(value: &str) -> Option<f64> {
    let (minutes, seconds) = value.split_once(':')?;
    let minutes = minutes.parse::<u32>().ok()?;
    let seconds = seconds.replace(',', ".").parse::<f64>().ok()?;
    (seconds.is_finite() && (0.0..60.0).contains(&seconds))
        .then_some(f64::from(minutes) * 60.0 + seconds)
}

pub(crate) fn stamp(seconds: f64) -> String {
    let millis = (seconds.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}.{:03}",
        millis / 60000,
        millis / 1000 % 60,
        millis % 1000
    )
}

// Timed markup stays in the existing cache representation; labels only receive
// the clean string. No persistent schema migration or frame-time parsing.
pub(crate) fn markup(raw: &str) -> (String, Vec<(usize, f64)>) {
    if !raw.contains('<') {
        return (raw.to_owned(), Vec::new());
    }
    let mut text = String::with_capacity(raw.len());
    let mut cues = Vec::new();
    let mut rest = raw;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('<') {
            if let Some((tag, tail)) = after.split_once('>') {
                if let Some(time) = timestamp(tag) {
                    cues.push((text.len(), time));
                    rest = tail;
                    continue;
                }
            }
        }
        let character = rest.chars().next().unwrap();
        text.push(character);
        rest = &rest[character.len_utf8()..];
    }
    (text, cues)
}

// LRC's positive offset advances the display, including its inline word cues.
pub(crate) fn offset_markup(raw: &str, offset: f64) -> String {
    if offset == 0.0 {
        return raw.to_owned();
    }
    let (text, cues) = markup(raw);
    let mut shifted = String::with_capacity(raw.len());
    let mut previous = 0;
    for (byte, time) in cues {
        shifted.push_str(&text[previous..byte]);
        shifted.push_str(&format!("<{}>", stamp(time - offset)));
        previous = byte;
    }
    shifted.push_str(&text[previous..]);
    shifted
}

pub(crate) fn quality(lines: &[Lyric]) -> u8 {
    rank(lines).0
}

// Partial word cues improve a line-synced result without claiming complete
// word timing. Keep the existing ordering of fully timed and plain results.
pub(crate) fn rank(lines: &[Lyric]) -> (u8, usize) {
    let mut times: Vec<_> = lines.iter().filter_map(|line| line.time).collect();
    times.sort_by(f64::total_cmp);
    let mut has_text = false;
    let mut all_exact = true;
    let mut word_lines = 0;
    for line in lines {
        let (text, cues) = markup(&line.text);
        if text.trim().is_empty() {
            continue;
        }
        has_text = true;
        let exact = line.time.is_some_and(|start| {
            let end = times
                .get(times.partition_point(|time| *time <= start))
                .copied()
                .unwrap_or(f64::INFINITY);
            valid_word_cues(&text, &cues, start, end)
        });
        all_exact &= exact;
        word_lines += usize::from(exact);
    }
    let quality = if !has_text {
        0
    } else if all_exact {
        3
    } else if !times.is_empty() {
        2
    } else if !lines.is_empty() {
        1
    } else {
        0
    };
    (quality, if quality == 2 { word_lines } else { 0 })
}

pub(crate) fn typical_rate(lines: &[Lyric]) -> f64 {
    let mut rates = Vec::new();
    for pair in lines.windows(2) {
        if let (Some(start), Some(end)) = (pair[0].time, pair[1].time) {
            let text = markup(&pair[0].text).0;
            let letters = text.chars().filter(|c| c.is_alphanumeric()).count();
            let duration = end - start;
            let rate = letters as f64 / duration;
            if letters >= 4 && (0.5..=12.0).contains(&duration) && (2.0..=18.0).contains(&rate) {
                rates.push(rate);
            }
        }
    }
    rates.sort_by(f64::total_cmp);
    rates.get(rates.len() / 2).copied().unwrap_or(6.5)
}

fn valid_word_cues(text: &str, cues: &[(usize, f64)], start: f64, end: f64) -> bool {
    // A nanosecond only absorbs f64 roundoff between mm:ss and integer ms.
    !text.trim().is_empty()
        && !cues.is_empty()
        && cues[0].0 == 0
        && cues.iter().all(|(byte, time)| {
            *time + 1e-9 >= start
                && if *byte == text.len() {
                    *time <= end + 1e-9
                } else {
                    *time < end
                }
        })
        && cues.iter().enumerate().all(|(index, &(byte, _))| {
            let next = cues.get(index + 1).map_or(text.len(), |cue| cue.0);
            // A single cue for a whole phrase is still line-level timing.
            // Syllables and one-word lines remain valid.
            text[byte..next]
                .split_whitespace()
                .filter(|word| word.chars().any(char::is_alphanumeric))
                .take(2)
                .count()
                <= 1
        })
        && cues.windows(2).all(|pair| pair[0].1 <= pair[1].1)
}

impl Timeline {
    pub fn new(raw: &str, start: f64, end: f64, typical_rate: f64) -> Self {
        let (text, cues) = markup(raw);
        let valid = valid_word_cues(&text, &cues, start, end);
        let mut result = Self {
            raw: raw.into(),
            text,
            exact: valid,
            invalid_cues: !cues.is_empty() && !valid,
            singing_end: end,
            segments: Vec::new(),
        };
        if valid {
            // A cue applies to its entire word/syllable. Its onset is taken
            // directly from the file; no invented per-letter timing inside it.
            for (index, &(byte, time)) in cues.iter().enumerate() {
                let next = cues.get(index + 1).map_or(result.text.len(), |cue| cue.0);
                if next > byte {
                    result.segments.push(Segment {
                        start: byte as u32,
                        end: next as u32,
                        onset: time,
                        fade: 0.18,
                    });
                } else if byte == result.text.len() {
                    result.singing_end = time;
                }
            }
            return result;
        }
        if !end.is_finite() || end <= start {
            return result;
        }
        let letters = result.text.chars().filter(|c| c.is_alphanumeric()).count();
        let typical_duration = letters as f64 / typical_rate.clamp(2.0, 18.0);
        // Only trim clearly long gaps. The result remains an estimate; no
        // heuristic claims to detect a real vocal ending from line timestamps.
        if end - start > 8.0_f64.max(typical_duration * 1.8) {
            result.singing_end = start + (typical_duration * 1.15).max(0.8).min(end - start);
        }
        let weights: Vec<_> = result
            .text
            .char_indices()
            .map(|(byte, c)| {
                let weight = if c.is_alphanumeric() {
                    1.0
                } else {
                    match c {
                        ',' | ';' | ':' => 0.5,
                        '.' | '!' | '?' | '…' => 0.8,
                        _ => 0.0,
                    }
                };
                (byte, c, weight)
            })
            .collect();
        let total: f64 = weights.iter().map(|(_, _, weight)| weight).sum();
        if total == 0.0 {
            return result;
        }
        let slot = (result.singing_end - start) / total;
        let mut elapsed = 0.0;
        let mut previous = start;
        for (byte, character, weight) in weights {
            let onset = if character.is_alphanumeric() {
                start + elapsed * slot
            } else {
                previous
            };
            result.segments.push(Segment {
                start: byte as u32,
                end: (byte + character.len_utf8()) as u32,
                onset,
                fade: slot.min(0.12),
            });
            elapsed += weight;
            previous = onset;
        }
        result
    }

    // The frame clock is only needed during a fade. Between word onsets a
    // finite wake-up is sufficient, including long instrumental gaps.
    fn fade_duration(&self, index: usize, speed: f64) -> f64 {
        let segment = &self.segments[index];
        if !self.exact {
            return segment.fade;
        }
        // Keep the visual transition steady in wall time. Rapid cues still
        // leave the following word dim until after the previous cue.
        let fade = segment.fade * speed.clamp(0.25, 4.0);
        let gap = index
            .checked_sub(1)
            .map(|previous| segment.onset - self.segments[previous].onset);
        gap.filter(|gap| *gap > 0.0)
            .map_or(fade, |gap| fade.min(gap * 0.8))
    }

    pub fn next_change(&self, position: f64, speed: f64) -> Option<f64> {
        let mut next: Option<f64> = None;
        for (index, segment) in self.segments.iter().enumerate() {
            if position < segment.onset {
                let begin = segment.onset - self.fade_duration(index, speed);
                if position >= begin {
                    return Some(position);
                }
                next = Some(next.map_or(begin, |previous| previous.min(begin)));
            }
        }
        next
    }

    #[cfg(test)]
    pub fn fill_alphas(&self, position: f64, result: &mut Vec<AlphaRange>) {
        self.fill_alphas_at_speed(position, 1.0, result);
    }

    pub fn fill_alphas_at_speed(&self, position: f64, speed: f64, result: &mut Vec<AlphaRange>) {
        result.clear();
        for (index, segment) in self.segments.iter().enumerate() {
            let fade = self.fade_duration(index, speed);
            let progress = ((position - segment.onset + fade) / fade).clamp(0.0, 1.0);
            let eased = if self.exact {
                // Zero velocity and acceleration at both ends, as in lyric scrolling.
                progress.powi(3) * (10.0 + progress * (-15.0 + 6.0 * progress))
            } else {
                1.0 - (1.0 - progress).powi(3)
            };
            let alpha = (f64::from(DIM) + f64::from(u16::MAX - DIM) * eased).round() as u16;
            if alpha < u16::MAX {
                if let Some((_, end, previous)) = result.last_mut() {
                    if *end == segment.start && *previous == alpha {
                        *end = segment.end;
                        continue;
                    }
                }
                result.push((segment.start, segment.end, alpha));
            }
        }
    }
}

pub(crate) fn run(args: &[OsString]) -> Result<(), String> {
    let mut duration = None;
    let mut online = false;
    let mut title = None;
    let mut artist = None;
    let mut album = String::new();
    let mut path = None;
    let mut summary = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--duration") => {
                duration = Some(
                    args.next()
                        .and_then(|s| s.to_str())
                        .and_then(|s| s.parse::<f64>().ok())
                        .filter(|n| n.is_finite() && *n > 0.0)
                        .ok_or("Нужна положительная длительность в секундах")?,
                )
            }
            Some("--online") => online = true,
            Some("--summary") => summary = true,
            Some("--title") => {
                title = Some(
                    args.next()
                        .and_then(|s| s.to_str())
                        .ok_or("Нужно название")?
                        .to_owned(),
                )
            }
            Some("--artist") => {
                artist = Some(
                    args.next()
                        .and_then(|s| s.to_str())
                        .ok_or("Нужен исполнитель")?
                        .to_owned(),
                )
            }
            Some("--album") => {
                album = args
                    .next()
                    .and_then(|s| s.to_str())
                    .ok_or("Нужен альбом")?
                    .to_owned()
            }
            Some("--help") => {
                println!("veyra lyrics-check FILE.lrc [--duration SECONDS] [--summary]\nveyra lyrics-check --online --title TITLE --artist ARTIST [--album ALBUM] --duration SECONDS [--summary]\nJSON не содержит текстов и путей. Проверка не доказывает совпадение с вокалом.");
                return Ok(());
            }
            Some(flag) if flag.starts_with('-') => {
                return Err(format!("Неизвестный параметр: {flag}"))
            }
            _ if path.is_none() => path = Some(PathBuf::from(arg)),
            _ => return Err("Нужен один файл".into()),
        }
    }
    let mut lookup = None;
    let lines = if online {
        let track = crate::Track {
            path: PathBuf::new(),
            title: title.ok_or("Нужно --title")?,
            artist: artist.ok_or("Нужно --artist")?,
            album,
            duration: duration.unwrap_or(0.0),
            cover_path: None,
            lyrics: Vec::new(),
        };
        let started = Instant::now();
        let mut upgrades = Vec::new();
        let lines = crate::lyrics::online_lyrics_with(
            &track,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            &mut |lines| {
                upgrades.push(json!({"at_ms": started.elapsed().as_secs_f64() * 1000.0,
                    "quality": quality(lines)}));
            },
        )?;
        lookup = Some(json!({"total_ms": started.elapsed().as_secs_f64() * 1000.0,
            "first_result_ms": upgrades.first().map(|event| &event["at_ms"]), "upgrades": upgrades}));
        lines
    } else {
        let path = path.ok_or("Укажи FILE.lrc или --online; справка: veyra lyrics-check --help")?;
        if std::fs::metadata(&path)
            .map_err(|_| "Не удалось открыть файл")?
            .len()
            > 2 * 1024 * 1024
        {
            return Err("Файл больше 2 МиБ".into());
        }
        crate::library::parse_lyrics(
            &std::fs::read_to_string(path).map_err(|_| "Не удалось прочитать LRC в UTF-8")?,
        )
    };
    let analysis_started = Instant::now();
    let rate = if summary { 6.5 } else { typical_rate(&lines) };
    let mut times: Vec<_> = lines.iter().filter_map(|line| line.time).collect();
    times.sort_by(f64::total_cmp);
    let mut intervals = Vec::new();
    let mut word_lines = 0;
    let mut invalid_lines = 0;
    for (index, line) in lines.iter().enumerate() {
        let Some(start) = line.time else { continue };
        let end = times
            .get(times.partition_point(|time| *time <= start))
            .copied()
            .or(duration)
            .unwrap_or(start);
        let (exact, invalid) = if summary {
            let (text, cues) = markup(&line.text);
            let exact = valid_word_cues(&text, &cues, start, end);
            (exact, !cues.is_empty() && !exact)
        } else {
            let timeline = Timeline::new(&line.text, start, end, rate);
            intervals.push(json!({"line": index + 1, "start": start, "end": end,
                "estimated_singing_end": (!timeline.exact).then_some(timeline.singing_end),
                "mode": if timeline.exact { "word-cues" } else { "line-estimate" },
                "invalid_cues": timeline.invalid_cues, "units": timeline.segments.len(),
                "onsets": timeline.segments.iter().map(|segment| segment.onset).collect::<Vec<_>>() }));
            (timeline.exact, timeline.invalid_cues)
        };
        word_lines += usize::from(exact);
        invalid_lines += usize::from(invalid);
    }
    let mut report = json!({"lines": lines.len(), "quality": quality(&lines),
        "reference": "file timestamps, not measured vocals",
        "invalid_lines": invalid_lines, "word_lines": word_lines,
        "estimated_lines": times.len() - word_lines, "untimed_lines": lines.len() - times.len(),
        "analysis_ms": analysis_started.elapsed().as_secs_f64() * 1000.0});
    if let Some(lookup) = lookup {
        report["lookup"] = lookup;
    }
    if !summary {
        report["intervals"] = json!(intervals);
    }
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn diagnostic_cli_rejects_invalid_arguments_without_starting_a_lookup() {
        for args in [
            vec![],
            vec!["--duration"],
            vec!["--duration", "NaN"],
            vec!["--duration", "inf"],
            vec!["--duration", "0"],
            vec!["--duration", "-1"],
            vec!["--title"],
            vec!["--artist"],
            vec!["--album"],
            vec!["--unknown"],
            vec!["a.lrc", "b.lrc"],
            vec!["--online"],
            vec!["--online", "--title", "Song"],
        ] {
            let args: Vec<_> = args.into_iter().map(std::ffi::OsString::from).collect();
            assert!(super::run(&args).is_err());
        }
    }
    #[test]
    fn blank_lines_and_timestamp_only_lines_have_no_lyric_quality() {
        for time in [None, Some(0.0)] {
            for text in ["", "  \t", "<00:00>", "<00:00>\u{2003}"] {
                assert_eq!(
                    super::quality(&[crate::Lyric {
                        time,
                        text: text.into()
                    }]),
                    0
                );
            }
        }
    }
    use super::*;

    #[test]
    fn quality_requires_word_cues_on_every_nonempty_line() {
        let parse = crate::library::parse_lyrics;
        assert_eq!(quality(&parse("[00:01]<00:01>One two three")), 2);
        assert_eq!(
            quality(&parse("[00:01]<00:01>One <00:02>two\n[00:04]Three four")),
            2
        );
        assert_eq!(
            quality(&parse(
                "[00:01]<00:01>One <00:02>two\n[00:04]<00:04>Three\n[00:05]"
            )),
            3
        );
        assert_eq!(quality(&parse("[00:01]")), 0);
        assert_eq!(quality(&parse("[00:01]<00:01>")), 0);
        assert_eq!(
            quality(&parse("[00:01]<00:01>One <00:09>two\n[00:04]<00:04>Three")),
            2
        );
        let mut lines = parse("[00:01]<00:01>One <00:02>two\n[00:04]<00:04>Three");
        lines.reverse();
        assert_eq!(quality(&lines), 3);
        lines.push(Lyric {
            time: None,
            text: "Untimed verse".into(),
        });
        assert_eq!(quality(&lines), 2);
    }

    #[test]
    fn word_onsets_must_precede_line_end_but_terminal_cue_can_equal_it() {
        for raw in [
            "<00:01>One <00:09>two",
            "<00:01>One <00:04>two",
            "<00:01>One <00:02>two<00:05>",
        ] {
            let timeline = Timeline::new(raw, 1.0, 4.0, 6.5);
            assert!(!timeline.exact && timeline.invalid_cues, "{raw}");
        }
        assert!(Timeline::new("<00:01>One <00:02>two<00:04>", 1.0, 4.0, 6.5).exact);
    }

    #[test]
    fn a_phrase_under_one_cue_is_not_word_sync() {
        for raw in [
            "<00:01>One two",
            "<00:01>One two<00:03>",
            "<00:01>One <00:02>two three",
        ] {
            assert!(!Timeline::new(raw, 1.0, 4.0, 6.5).exact, "{raw}");
        }
        assert!(Timeline::new("<00:01>One", 1.0, 4.0, 6.5).exact);
        assert!(Timeline::new("<00:01>Ти<00:02>хо <00:03>мир", 1.0, 4.0, 6.5).exact);
    }

    fn alpha(timeline: &Timeline, byte: u32, position: f64) -> u16 {
        let mut ranges = Vec::new();
        timeline.fill_alphas(position, &mut ranges);
        ranges
            .iter()
            .find(|&&(start, end, _)| start <= byte && byte < end)
            .map_or(u16::MAX, |r| r.2)
    }
    #[test]
    fn enhanced_words_use_exact_onsets_not_character_interpolation() {
        let t = Timeline::new(
            "<00:04.000>Тихо <00:06.000>🌙 мир<00:07.000>",
            4.0,
            12.0,
            6.5,
        );
        assert!(t.exact);
        assert_eq!(t.text, "Тихо 🌙 мир");
        assert_eq!(alpha(&t, 2, 4.0), u16::MAX);
        assert_eq!(alpha(&t, 9, 5.5), DIM);
        assert_eq!(alpha(&t, 9, 6.0), u16::MAX);
        assert_eq!(t.singing_end, 7.0);
        assert_eq!(
            alpha(&t, 9, 5.5),
            DIM,
            "seek backwards recomputes without history"
        );
    }
    #[test]
    fn ordinary_lrc_skips_space_slots_and_does_not_stretch_over_long_silence() {
        let t = Timeline::new("Тихо, мир", 4.0, 30.0, 6.5);
        assert!(!t.exact);
        assert!(t.singing_end < 8.0);
        let a = Timeline::new("Тихо мир", 0.0, 4.0, 6.5);
        let b = Timeline::new("Тихо    мир", 0.0, 4.0, 6.5);
        assert_eq!(
            a.segments
                .iter()
                .filter(|s| a.text[s.start as usize..s.end as usize]
                    .chars()
                    .all(char::is_alphanumeric))
                .map(|s| s.onset)
                .collect::<Vec<_>>(),
            b.segments
                .iter()
                .filter(|s| b.text[s.start as usize..s.end as usize]
                    .chars()
                    .all(char::is_alphanumeric))
                .map(|s| s.onset)
                .collect::<Vec<_>>()
        );
    }
    #[test]
    fn exact_word_fade_is_soft_and_has_same_wall_duration_at_different_speeds() {
        let t = Timeline::new("<00:01>One <00:02>two<00:03>", 1.0, 3.0, 6.5);
        for speed in [0.5, 1.0, 1.5, 2.0] {
            let mut ranges = Vec::new();
            let alpha_at = |wall_before: f64, ranges: &mut Vec<AlphaRange>| {
                t.fill_alphas_at_speed(2.0 - wall_before * speed, speed, ranges);
                ranges
                    .iter()
                    .find(|r| r.0 <= 4 && r.1 > 4)
                    .map_or(u16::MAX, |r| r.2)
            };
            assert_eq!(alpha_at(0.18, &mut ranges), DIM);
            let middle = alpha_at(0.09, &mut ranges);
            assert!((i32::from(middle) - (i32::from(DIM) + i32::from(u16::MAX)) / 2).abs() <= 1);
            assert_eq!(alpha_at(0.0, &mut ranges), u16::MAX);
            assert_eq!(
                t.next_change(2.0 - 0.09 * speed, speed),
                Some(2.0 - 0.09 * speed)
            );
        }
        let rapid = Timeline::new("<00:01>One <00:01.100>two", 1.0, 2.0, 6.5);
        let mut ranges = Vec::new();
        rapid.fill_alphas_at_speed(1.0, 2.0, &mut ranges);
        assert!(
            ranges.iter().any(|r| r.0 == 4 && r.2 == DIM),
            "the next word must not fade before the previous cue"
        );
    }

    #[test]
    fn millisecond_cues_do_not_fall_back_due_to_floating_point_roundoff() {
        let parsed = timestamp("01:15.120").unwrap();
        let start = f64::from_bits(parsed.to_bits() + 1);
        let t = Timeline::new("<01:15.120>Word <01:16.000>two", start, 80.0, 6.5);
        assert!(t.exact && !t.invalid_cues);
        assert!(!Timeline::new("<01:14.000>Word", start, 80.0, 6.5).exact);
    }

    #[test]
    fn malformed_cues_fall_back_and_utf8_ranges_are_valid() {
        let t = Timeline::new("<00:05.0>Я <00:04.0>🌙", 4.0, 8.0, 6.5);
        assert!(!t.exact && t.invalid_cues);
        for segment in &t.segments {
            assert!(t.text.is_char_boundary(segment.start as usize));
            assert!(t.text.is_char_boundary(segment.end as usize));
        }
        assert!(Timeline::new("", 0.0, 0.0, 6.5).segments.is_empty());
    }

    #[test]
    fn generated_utf8_markup_keeps_highlight_ranges_valid() {
        let atoms = [
            "Я",
            "e\u{301}",
            "👩‍🚀",
            "\u{202e}",
            "\0",
            " ",
            "<",
            ">",
            "[",
            "<00:01.001>",
            "<00:02>",
            "<NaN>",
            "<99:99>",
        ];
        let mut seed = 0x5eed_u64;
        for case in 0..2000 {
            let mut raw = String::new();
            for _ in 0..case % 64 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                raw.push_str(atoms[seed as usize % atoms.len()]);
            }
            let (clean, cues) = markup(&raw);
            assert!(cues
                .iter()
                .all(|(byte, time)| clean.is_char_boundary(*byte) && time.is_finite()));
            for offset in [-2.0, 0.0, 2.0] {
                assert_eq!(markup(&offset_markup(&raw, offset)).0, clean, "case={case}");
            }
            for end in [0.0, 1.0, 10.0] {
                let timeline = Timeline::new(&raw, 1.0, end, 6.5);
                for position in [-1.0, 0.0, 1.01, 2.0, 100.0] {
                    for speed in [0.5, 1.0, 2.0] {
                        let mut ranges = Vec::new();
                        timeline.fill_alphas_at_speed(position, speed, &mut ranges);
                        let mut previous = 0;
                        for (start, end, alpha) in ranges {
                            assert!(start >= previous && start < end);
                            assert!(clean.is_char_boundary(start as usize));
                            assert!(clean.is_char_boundary(end as usize));
                            assert!(alpha >= DIM, "case={case}");
                            previous = end;
                        }
                        assert!(timeline
                            .next_change(position, speed)
                            .is_none_or(|next| next.is_finite() && next >= position));
                    }
                }
            }
        }
    }

    #[test]
    fn timestamp_rejects_extreme_and_malformed_values() {
        for value in [
            "",
            ":",
            "-1:00",
            "0:60",
            "0:NaN",
            "0:inf",
            "4294967296:00",
            "0:1e999",
            "1:00:00",
        ] {
            assert!(timestamp(value).is_none(), "{value}");
        }
        assert_eq!(timestamp("00:01,125"), Some(1.125));
        assert_eq!(
            timestamp("4294967295:59.999"),
            Some(4294967295.0 * 60.0 + 59.999)
        );
    }
}
