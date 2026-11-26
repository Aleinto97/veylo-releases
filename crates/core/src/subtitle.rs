//! Tolerant SRT/WebVTT subtitle parsing for the local player.
//!
//! This mirrors VLC's subtitle-file approach rather than any VLC C code: the
//! format is detected from the content instead of the file extension, damaged
//! files are parsed permissively, markup/ASS overrides/HTML entities are
//! stripped, and unknown header or NOTE/STYLE/REGION blocks are ignored.
//! Downloading and caching stay elsewhere; this module only turns subtitle
//! bytes into timed text cues.

use anyhow::{bail, Result};

/// Whole-document bound in bytes. Subtitle files are small; larger input is
/// hostile or not a subtitle at all (VLC applies a comparable sanity cap).
pub(crate) const LIMIT: usize = 4 * 1024 * 1024;

/// Retained-cue bound, so damaged or hostile files cannot grow without limit.
const MAX_CUES: usize = 20_000;
/// Inspected-line bound: a 4 MiB file of newlines must not cost unbounded work.
const MAX_LINES: usize = 500_000;
/// Longest retained cue text, in characters.
const MAX_TEXT_CHARS: usize = 500;

/// One timed text cue, ready for the player overlay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Cue {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

/// Cue currently being accumulated until the next blank or timing line.
struct Pending {
    start_ms: u64,
    end_ms: u64,
    lines: Vec<(String, bool)>,
    chars: usize,
    capped: bool,
}

/// Parses subtitle bytes as SRT, WebVTT, or a tolerant mixture of both.
///
/// Cues are sorted by start then end; cues with a non-positive duration or no
/// text are dropped. Malformed input is tolerated: whatever timed text can be
/// extracted is returned, and only oversized documents are an error.
pub(crate) fn parse(bytes: &[u8]) -> Result<Vec<Cue>> {
    if bytes.len() > LIMIT {
        bail!("subtitle too large");
    }
    if bytes.is_empty() {
        return Ok(Vec::new());
    }

    let document = decode(bytes);
    let mut cues: Vec<Cue> = Vec::new();
    let mut pending: Option<Pending> = None;
    let mut line_count = 0usize;

    for raw_line in document.split('\n') {
        line_count += 1;
        if line_count > MAX_LINES || cues.len() >= MAX_CUES {
            break;
        }
        let line = raw_line.trim();
        if line.is_empty() {
            if let Some(p) = pending.take() {
                push_cue(&mut cues, p);
            }
            continue;
        }
        if line.contains("-->") {
            if let Some((start_ms, end_ms)) = parse_timing(line) {
                if let Some(mut previous) = pending.take() {
                    // Dense malformed SRT may place the next numeric cue
                    // identifier here without a blank separator. Preserve
                    // ordinary text: it can be the previous cue's final line.
                    if previous.lines.len() > 1
                        && previous.lines.last().is_some_and(|(_, numeric)| *numeric)
                    {
                        previous.lines.pop();
                    }
                    push_cue(&mut cues, previous);
                }
                pending = Some(Pending {
                    start_ms,
                    end_ms,
                    lines: Vec::new(),
                    chars: 0,
                    capped: false,
                });
                continue;
            }
            // An arrow line that is not a valid timing is plain text inside a
            // cue; outside a cue it is ignored like any other marker line.
        }
        if let Some(p) = pending.as_mut() {
            if p.capped {
                continue;
            }
            let numeric = !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit());
            for part in sanitize_line(line).split('\n') {
                let trimmed = part.trim();
                let n = trimmed.chars().count();
                if p.chars + n > MAX_TEXT_CHARS {
                    let take = MAX_TEXT_CHARS.saturating_sub(p.chars);
                    if take > 0 {
                        p.lines.push((trimmed.chars().take(take).collect(), false));
                        p.chars = MAX_TEXT_CHARS;
                    }
                    p.capped = true;
                    break;
                }
                let sep = usize::from(!p.lines.is_empty());
                p.lines.push((trimmed.to_owned(), numeric));
                p.chars += n + sep;
            }
        }
    }

    if let Some(p) = pending.take() {
        if cues.len() < MAX_CUES {
            push_cue(&mut cues, p);
        }
    }

    cues.sort_by(|a, b| a.start_ms.cmp(&b.start_ms).then(a.end_ms.cmp(&b.end_ms)));
    Ok(cues)
}

/// Sanitizes and appends one pending cue, dropping unsound or empty ones.
fn push_cue(cues: &mut Vec<Cue>, pending: Pending) {
    if cues.len() >= MAX_CUES || pending.end_ms <= pending.start_ms {
        return;
    }
    let Some(first) = pending.lines.iter().position(|(line, _)| !line.is_empty()) else {
        return;
    };
    let Some(last) = pending.lines.iter().rposition(|(line, _)| !line.is_empty()) else {
        return;
    };
    let mut text = pending.lines[first..=last]
        .iter()
        .map(|(l, _)| l.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if text.chars().count() > MAX_TEXT_CHARS {
        text = text.chars().take(MAX_TEXT_CHARS).collect();
    }
    if text.is_empty() {
        return;
    }
    cues.push(Cue {
        start_ms: pending.start_ms,
        end_ms: pending.end_ms,
        text,
    });
}

/// Decodes the whole document as UTF-8, falling back to Windows-1252, removes
/// every U+FEFF, and normalizes line endings to '\n'.
fn decode(bytes: &[u8]) -> String {
    let bytes = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        &bytes[3..]
    } else {
        bytes
    };
    let mut text = match std::str::from_utf8(bytes) {
        Ok(valid) => valid.to_string(),
        Err(_) => bytes.iter().map(|&byte| cp1252(byte)).collect(),
    };
    if text.contains('\u{feff}') {
        text = text.replace('\u{feff}', "");
    }
    if text.contains('\r') {
        text = text.replace("\r\n", "\n").replace('\r', "\n");
    }
    text
}

/// Windows-1252 decoding: the 0x80..=0x9F table plus 0xA0..=0xFF pass-through.
fn cp1252(byte: u8) -> char {
    const HIGH: [char; 32] = [
        '\u{20AC}', '\u{FFFD}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{FFFD}',
        '\u{017D}', '\u{FFFD}', '\u{FFFD}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}',
        '\u{2022}', '\u{2013}', '\u{2014}', '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}',
        '\u{0153}', '\u{FFFD}', '\u{017E}', '\u{0178}',
    ];
    match byte {
        0x80..=0x9F => HIGH[(byte - 0x80) as usize],
        other => other as char,
    }
}

/// Splits one timing line ("start --> end [settings]") into millisecond bounds.
fn parse_timing(line: &str) -> Option<(u64, u64)> {
    let (left, right) = line.split_once("-->")?;
    let start_ms = parse_timestamp(left.trim())?;
    // Cue settings such as "align:start position:10%" follow the end stamp.
    let end_token = right.split_whitespace().next()?;
    let end_ms = parse_timestamp(end_token)?;
    Some((start_ms, end_ms))
}

/// Parses [HH:]MM:SS.mmm or [HH:]MM:SS,mmm with 1..=3 fraction digits.
fn parse_timestamp(token: &str) -> Option<u64> {
    let token = token.trim();
    if token.is_empty() || !token.is_ascii() {
        return None;
    }
    let fraction_at = token.find(['.', ','])?;
    let (whole, fraction) = token.split_at(fraction_at);
    let fraction = &fraction[1..];
    if fraction.is_empty() || fraction.len() > 3 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let fraction_ms = match fraction.len() {
        1 => fraction.parse::<u64>().ok()? * 100,
        2 => fraction.parse::<u64>().ok()? * 10,
        _ => fraction.parse::<u64>().ok()?,
    };
    let mut parts = whole.split(':').rev();
    let seconds = parse_clock_part(parts.next()?, 59)?;
    let minutes = parse_clock_part(parts.next()?, 59)?;
    let hours = match parts.next() {
        Some(hours) => {
            if hours.is_empty() || hours.len() > 3 || !hours.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            hours.parse::<u64>().ok()?
        }
        None => 0,
    };
    if parts.next().is_some() {
        return None;
    }
    let total_seconds = hours
        .checked_mul(3_600)?
        .checked_add(minutes.checked_mul(60)?)?
        .checked_add(seconds)?;
    total_seconds.checked_mul(1_000)?.checked_add(fraction_ms)
}

/// Parses one minutes/seconds clock component: one or two digits, at most limit.
fn parse_clock_part(part: &str, limit: u64) -> Option<u64> {
    if part.is_empty() || part.len() > 2 || !part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value = part.parse::<u64>().ok()?;
    (value <= limit).then_some(value)
}

/// Cleans one line of cue text: markup, ASS overrides, escapes, entities, controls.
fn sanitize_line(line: &str) -> String {
    let stripped = strip_markup(line);
    let unescaped = decode_escapes(&stripped);
    let decoded = decode_entities(&unescaped);
    clean_controls(&decoded)
}

/// Removes <...> markup tags and {...} ASS/SSA override blocks.
///
/// A < or { that does not open a real tag/override is kept literally, so
/// ordinary text such as 5 < 10 survives.
fn strip_markup(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' => {
                let (inner, closed) = take_until(&mut chars, '>', 64);
                if closed && looks_like_tag(&inner) {
                    continue;
                }
                output.push('<');
                output.push_str(&inner);
                if closed {
                    output.push('>');
                }
            }
            '{' => {
                let (inner, closed) = take_until(&mut chars, '}', 512);
                if closed {
                    continue;
                }
                output.push('{');
                output.push_str(&inner);
            }
            _ => output.push(c),
        }
    }
    output
}

/// Collects up to max chars until stop (consumed); reports whether it was found.
fn take_until<I: Iterator<Item = char>>(
    chars: &mut std::iter::Peekable<I>,
    stop: char,
    max: usize,
) -> (String, bool) {
    let mut inner = String::new();
    let mut count = 0usize;
    for c in chars.by_ref() {
        if c == stop {
            return (inner, true);
        }
        inner.push(c);
        count += 1;
        if count >= max {
            break;
        }
    }
    (inner, false)
}

/// Heuristic: does the content of a <...> construct look like markup?
fn looks_like_tag(inner: &str) -> bool {
    let inner = inner.trim_start_matches('/');
    match inner.chars().next() {
        Some(first) if first.is_ascii_alphabetic() => true,
        Some('!') => true,
        // WebVTT timestamp tags, e.g. <00:00:01.500>, start with digits.
        Some(first) if first.is_ascii_digit() => inner.contains(':'),
        _ => false,
    }
}

/// Turns ASS-style \N / \n line breaks into newlines and \h into spaces.
fn decode_escapes(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            output.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some('N') | Some('n') => {
                chars.next();
                output.push('\n');
            }
            Some('h') => {
                chars.next();
                output.push(' ');
            }
            _ => output.push('\\'),
        }
    }
    output
}

/// Decodes the HTML entities found in real subtitle files.
///
/// Unknown entities are kept verbatim; numeric entities must be valid Unicode
/// scalar values to be decoded.
fn decode_entities(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '&' {
            output.push(c);
            continue;
        }
        let mut name = String::new();
        let mut closed = false;
        while let Some(&next) = chars.peek() {
            if next == ';' {
                chars.next();
                closed = true;
                break;
            }
            if next.is_whitespace() || next == '&' || name.len() >= 10 {
                break;
            }
            name.push(next);
            chars.next();
        }
        match (closed, entity_char(&name)) {
            (true, Some(decoded)) => output.push(decoded),
            _ => {
                output.push('&');
                output.push_str(&name);
                if closed {
                    output.push(';');
                }
            }
        }
    }
    output
}

/// Maps one entity name (without & and ;) to its decoded character.
fn entity_char(name: &str) -> Option<char> {
    match name {
        "gt" => return Some('>'),
        "lt" => return Some('<'),
        "amp" => return Some('&'),
        "quot" => return Some('"'),
        "apos" => return Some('\''),
        "nbsp" => return Some(' '),
        _ => {}
    }
    let digits = name.strip_prefix('#')?;
    if let Some(hex) = digits
        .strip_prefix('x')
        .or_else(|| digits.strip_prefix('X'))
    {
        u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
    } else {
        digits.parse::<u32>().ok().and_then(char::from_u32)
    }
}

/// Keeps printable text and newlines; tabs become spaces; other controls vanish.
fn clean_controls(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '\n' => output.push('\n'),
            '\t' => output.push(' '),
            _ if c.is_control() => {}
            _ => output.push(c),
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(cues: &[Cue]) -> Vec<&str> {
        cues.iter().map(|cue| cue.text.as_str()).collect()
    }

    #[test]
    fn parses_srt_with_indexes_crlf_markup_and_entities() {
        let input = "1\r\n00:00:01,500 --> 00:00:04,250\r\n<i>Hello</i> &amp; welcome\r\nsecond line &gt; third\r\n\r\n2\r\n00:00:05,000 --> 00:00:06,000\r\n<font color=\"red\">Next</font>\r\n";
        let cues = parse(input.as_bytes()).expect("srt parses");
        assert_eq!(cues.len(), 2);
        assert_eq!(
            cues[0],
            Cue {
                start_ms: 1_500,
                end_ms: 4_250,
                text: "Hello & welcome\nsecond line > third".to_string(),
            }
        );
        assert_eq!(cues[1].start_ms, 5_000);
        assert_eq!(cues[1].end_ms, 6_000);
        assert_eq!(cues[1].text, "Next");
    }

    #[test]
    fn parses_reference_opensubtitles_v3_mixed_file() {
        let input = "WEBVTT\n\nheader\n00:00:01.000 --> 00:00:06.000\n&gt;&gt;OpenSubtitles v3+ v0.0.4&lt;&lt;\n<u>=&gt;The.Terminal.2004.720p.BluRay.X264-AMIABLE.srt</u>\n\nWEBVTT\n\n\u{feff}1\n00:00:41.625 --> 00:00:43.252\n<i>United Airlines announcing</i>\n\n2\n00:00:43.335 --> 00:00:46.714\n<i>the arrival of Flight\n9435 from Beijing.</i>";
        let cues = parse(input.as_bytes()).expect("mixed file parses");
        assert_eq!(cues.len(), 3);
        assert_eq!(
            cues[0],
            Cue {
                start_ms: 1_000,
                end_ms: 6_000,
                text:
                    ">>OpenSubtitles v3+ v0.0.4<<\n=>The.Terminal.2004.720p.BluRay.X264-AMIABLE.srt"
                        .to_string(),
            }
        );
        assert_eq!(cues[1].start_ms, 41_625);
        assert_eq!(cues[1].end_ms, 43_252);
        assert_eq!(cues[1].text, "United Airlines announcing");
        assert_eq!(cues[2].start_ms, 43_335);
        assert_eq!(cues[2].end_ms, 46_714);
        assert_eq!(cues[2].text, "the arrival of Flight\n9435 from Beijing.");
    }

    #[test]
    fn skips_vtt_metadata_blocks_and_ignores_cue_settings() {
        let input = "\u{feff}WEBVTT\n\nNOTE this note is ignored\nand so is this line\n\nSTYLE\n::cue { color: yellow }\n\nREGION\nid:fred width:40%\n\nintro\n00:01.000 --> 00:02.500 align:start position:10%\nShort timestamp cue\n\n2\n00:03.000 --> 00:04.000\n<i>Second</i>\n";
        let cues = parse(input.as_bytes()).expect("vtt parses");
        assert_eq!(cues.len(), 2);
        assert_eq!((cues[0].start_ms, cues[0].end_ms), (1_000, 2_500));
        assert_eq!(cues[0].text, "Short timestamp cue");
        assert_eq!((cues[1].start_ms, cues[1].end_ms), (3_000, 4_000));
        assert_eq!(cues[1].text, "Second");
    }

    #[test]
    fn drops_invalid_cues_and_tolerates_garbage() {
        let input = "1\n00:00:05,000 --> 00:00:04,000\nreversed range\n\n2\n00:00:06,000 --> 00:00:06,000\nzero length\n\n3\n00:00:07,000 --> 00:00:08,000\n   \n\nnot a subtitle at all\n";
        assert!(parse(input.as_bytes()).expect("tolerant").is_empty());
        assert!(parse(b"").expect("empty input").is_empty());
        assert!(parse(b"\n\n\r\n").expect("blank input").is_empty());
        assert!(parse(b"hello world\nno timestamps here")
            .expect("prose")
            .is_empty());

        let oversized = vec![b'x'; LIMIT + 1];
        let error = parse(&oversized).expect_err("oversized rejected");
        assert_eq!(error.to_string(), "subtitle too large");
    }

    #[test]
    fn decodes_cp1252_and_utf8_bom() {
        let windows = b"Caf\xe9\n\n00:00:01,000 --> 00:00:02,000\nCaf\xe9";
        let cues = parse(windows).expect("cp1252");
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "Café");

        let mut bom = Vec::from(&b"\xef\xbb\xbf"[..]);
        bom.extend_from_slice("1\n00:00:01,000 --> 00:00:02,000\nHel\u{feff}lo\n".as_bytes());
        let cues = parse(&bom).expect("utf-8 bom");
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "Hello");
    }

    #[test]
    fn strips_ass_overrides_and_decodes_entities() {
        let input = "1\n00:00:01,000 --> 00:00:02,000\n{\\i1}Tom &amp; Jerry{\\i0} &#65;&#x42; &lt;tag&gt; &quot;q&quot; &apos;a&apos; &nbsp;z\n\n2\n00:00:03,000 --> 00:00:04,000\nline one\\Nline two\\nline three\n";
        let cues = parse(input.as_bytes()).expect("markup");
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "Tom & Jerry AB <tag> \"q\" 'a'  z");
        assert_eq!(cues[1].text, "line one\nline two\nline three");
    }

    #[test]
    fn sorts_cues_by_start_then_end() {
        let input = "1\n00:00:09,000 --> 00:00:10,000\nlate\n\n2\n00:00:02,000 --> 00:00:04,000\nearly long\n\n3\n00:00:02,000 --> 00:00:03,000\nearly short\n";
        let cues = parse(input.as_bytes()).expect("sorting");
        assert_eq!(texts(&cues), vec!["early short", "early long", "late"]);
        assert_eq!(cues[0].end_ms, 3_000);
        assert_eq!(cues[1].end_ms, 4_000);
        assert_eq!(cues[2].start_ms, 9_000);
    }

    #[test]
    fn parses_hour_timestamps_and_short_forms() {
        let input = "1\n01:02:03,004 --> 02:03:04.500\nhours\n";
        let cues = parse(input.as_bytes()).expect("hours");
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start_ms, 3_723_004);
        assert_eq!(cues[0].end_ms, 7_384_500);
    }

    #[test]
    fn drops_identifier_before_timing_without_blank_line() {
        let input = "1\n00:00:01,000 --> 00:00:02,000\nfirst text\n2\n00:00:03,000 --> 00:00:04,000\nsecond text\n";
        let cues = parse(input.as_bytes()).expect("dense srt");
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "first text");
        assert_eq!(cues[1].text, "second text");
    }

    #[test]
    fn preserves_final_text_without_separator_or_identifier() {
        let input = "00:00:01,000 --> 00:00:02,000\nfirst text\n00:00:03,000 --> 00:00:04,000\nsecond text\n";
        let cues = parse(input.as_bytes()).expect("dense cues");
        assert_eq!(texts(&cues), vec!["first text", "second text"]);
    }

    #[test]
    fn caps_cue_text_length() {
        let long = "y".repeat(600);
        let input = format!("1\n00:00:01,000 --> 00:00:02,000\n{long}\n");
        let cues = parse(input.as_bytes()).expect("long text");
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text.chars().count(), MAX_TEXT_CHARS);
        assert!(cues[0].text.chars().all(|c| c == 'y'));
    }

    #[test]
    fn bounds_retained_cue_count() {
        let mut input = String::new();
        for index in 0..(MAX_CUES + 25) {
            input.push_str(&format!(
                "{index}\n00:00:01,000 --> 00:00:02,000\ntext {index}\n\n"
            ));
        }
        let cues = parse(input.as_bytes()).expect("many cues");
        assert_eq!(cues.len(), MAX_CUES);
    }
}
