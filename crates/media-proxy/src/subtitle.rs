//! Positive, full-document validation for a deliberately text-only subtitle subset.
//! Never search past arbitrary bytes for a signature, and never trust a MIME hint.
use anyhow::{anyhow, bail, ensure, Result};
use quick_xml::{
    events::{BytesEnd, BytesStart, BytesText, Event},
    Reader, Writer,
};
use tokio_util::sync::CancellationToken;

pub(super) const LIMIT: usize = 4 * 1024 * 1024;
const XML_EVENTS: usize = 65_536;
const XML_DEPTH: usize = 64;
const CUE_LINES: usize = 65_536;
const TTML_NS: &str = "http://www.w3.org/ns/ttml";
const STYLING_NS: &str = "http://www.w3.org/ns/ttml#styling";
const PARAMETER_NS: &str = "http://www.w3.org/ns/ttml#parameter";

pub(super) struct Validated {
    pub body: Vec<u8>,
    pub content_type: &'static str,
}

pub(super) fn parse(bytes: &[u8], cancel: &CancellationToken) -> Result<Validated> {
    ensure!(bytes.len() <= LIMIT, "subtitle too large");
    let text = std::str::from_utf8(bytes)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    valid_characters(text)?;
    if text.starts_with("WEBVTT") {
        webvtt(text, cancel)?;
        Ok(Validated {
            body: bytes.to_vec(),
            content_type: "text/vtt",
        })
    } else if text.trim_start_matches(xml_space).starts_with('<') {
        Ok(Validated {
            body: ttml(text, cancel)?,
            content_type: "application/ttml+xml",
        })
    } else {
        subrip(text, cancel)?;
        Ok(Validated {
            body: bytes.to_vec(),
            content_type: "application/x-subrip",
        })
    }
}

fn xml_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

fn valid_characters(text: &str) -> Result<()> {
    ensure!(
        text.chars()
            .all(|c| (!c.is_control() || xml_space(c)) && !matches!(c, '\u{fffe}' | '\u{ffff}')),
        "invalid subtitle character"
    );
    Ok(())
}

/// Parse the complete clock, rather than accepting a timestamp-shaped prefix.
fn timestamp(text: &str, vtt: bool) -> Result<u64> {
    let (clock, millis) = if vtt {
        text.rsplit_once('.')
    } else {
        text.rsplit_once([',', '.'])
    }
    .ok_or_else(|| anyhow!("invalid cue timestamp"))?;
    ensure!(
        millis.len() == 3 && millis.bytes().all(|b| b.is_ascii_digit()),
        "invalid milliseconds"
    );
    let mut split = clock.split(':');
    let p0 = split.next().ok_or_else(|| anyhow!("invalid cue clock"))?;
    let p1 = split.next().ok_or_else(|| anyhow!("invalid cue clock"))?;
    let p2 = split.next();
    ensure!(split.next().is_none(), "invalid cue clock");
    let (hours_str, minutes_str, seconds_str) = match p2 {
        Some(sec) => (Some(p0), p1, sec),
        None => {
            ensure!(vtt, "invalid cue clock");
            (None, p0, p1)
        }
    };
    ensure!(
        hours_str
            .is_none_or(|h| (2..=6).contains(&h.len()) && h.bytes().all(|b| b.is_ascii_digit()))
            && minutes_str.len() == 2
            && minutes_str.bytes().all(|b| b.is_ascii_digit())
            && seconds_str.len() == 2
            && seconds_str.bytes().all(|b| b.is_ascii_digit()),
        "invalid cue clock digits"
    );
    let seconds = seconds_str.parse::<u64>()?;
    let minutes = minutes_str.parse::<u64>()?;
    ensure!(seconds < 60 && minutes < 60, "invalid cue clock range");
    let hours = match hours_str {
        Some(h) => h.parse::<u64>()?,
        None => 0,
    };
    Ok(((hours * 60 + minutes) * 60 + seconds) * 1000 + millis.parse::<u64>()?)
}

fn percentage(value: &str) -> bool {
    value.strip_suffix('%').is_some_and(|n| {
        !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit() || b == b'.')
            && n.parse::<f64>().is_ok_and(|n| (0.0..=100.0).contains(&n))
    })
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().enumerate().all(|(i, b)| {
            b.is_ascii_alphabetic()
                || b == b'_'
                || (i > 0 && (b.is_ascii_digit() || matches!(b, b'-' | b'.')))
        })
}

fn vtt_settings(settings: impl Iterator<Item = impl AsRef<str>>) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for setting in settings {
        let (key, value) = setting
            .as_ref()
            .split_once(':')
            .ok_or_else(|| anyhow!("invalid WebVTT cue setting"))?;
        ensure!(seen.insert(key.to_string()), "duplicate WebVTT setting");
        let safe = match key {
            "vertical" => matches!(value, "rl" | "lr"),
            "align" => matches!(value, "start" | "center" | "end" | "left" | "right"),
            "size" => percentage(value),
            "region" => identifier(value),
            "position" => {
                let (position, align) = value.split_once(',').unwrap_or((value, "auto"));
                percentage(position)
                    && matches!(align, "line-left" | "center" | "line-right" | "auto")
            }
            "line" => {
                let (line, align) = value.split_once(',').unwrap_or((value, "start"));
                let number = line.strip_prefix('-').unwrap_or(line);
                (line == "auto"
                    || percentage(line)
                    || (!number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())))
                    && matches!(align, "start" | "center" | "end")
            }
            _ => false,
        };
        ensure!(safe, "unsupported WebVTT setting");
    }
    Ok(())
}

fn cue_timing(line: &str, vtt: bool) -> Result<()> {
    let (start, end) = line
        .split_once("-->")
        .ok_or_else(|| anyhow!("missing cue timing"))?;
    let start = timestamp(start.trim_matches(xml_space), vtt)?;
    let mut end = end.split_whitespace();
    let stop = timestamp(end.next().ok_or_else(|| anyhow!("missing cue end"))?, vtt)?;
    ensure!(start <= stop, "reversed cue timing");
    if vtt {
        vtt_settings(end)?;
    } else {
        ensure!(end.next().is_none(), "unsupported SubRip timing suffix");
    }
    Ok(())
}

/// Restrict cue markup to non-fetching subtitle formatting, not arbitrary HTML.
fn cue_text(text: &str, vtt: bool) -> Result<()> {
    let mut rest = text;
    while let Some((_, after)) = rest.split_once('<') {
        let (tag, tail) = after
            .split_once('>')
            .ok_or_else(|| anyhow!("unterminated subtitle markup"))?;
        let safe = if let Some(close) = tag.strip_prefix('/') {
            matches!(close, "b" | "i" | "u")
                || (vtt && matches!(close, "c" | "v" | "lang" | "ruby" | "rt"))
                || (!vtt && close == "font")
        } else if matches!(tag, "b" | "i" | "u") {
            true
        } else if vtt {
            matches!(tag, "c" | "ruby" | "rt")
                || tag.strip_prefix("c.").is_some_and(|classes| {
                    classes.split('.').all(|class| {
                        !class.is_empty()
                            && class
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                    })
                })
                || tag
                    .strip_prefix("v ")
                    .is_some_and(|voice| !voice.is_empty() && !voice.contains('<'))
                || tag.strip_prefix("lang ").is_some_and(|lang| {
                    !lang.is_empty() && lang.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                })
                || timestamp(tag, true).is_ok()
        } else if tag.starts_with("font ") {
            let start = BytesStart::from_content(tag, 4);
            let mut attrs = start.attributes();
            let attr = attrs.next().transpose()?;
            attr.is_some_and(|a| {
                a.key.as_ref() == b"color"
                    && !a.value.is_empty()
                    && a.value
                        .iter()
                        .all(|b| b.is_ascii_alphanumeric() || *b == b'#')
            }) && attrs.next().is_none()
        } else {
            false
        };
        ensure!(safe, "unsupported subtitle cue markup");
        rest = tail;
    }
    Ok(())
}

/// HLS WebVTT segments commonly carry this non-fetching clock mapping in the
/// header. Accept only the two numeric fields, not arbitrary metadata/URI lines.
fn timestamp_map(line: &str) -> Result<()> {
    let fields = line
        .strip_prefix("X-TIMESTAMP-MAP=")
        .ok_or_else(|| anyhow!("unsupported WebVTT header metadata"))?;
    let mut local = false;
    let mut mpeg = false;
    for field in fields.split(',') {
        let (key, value) = field
            .trim_matches(xml_space)
            .split_once(':')
            .ok_or_else(|| anyhow!("invalid timestamp mapping"))?;
        match key {
            "LOCAL" => {
                ensure!(!local, "duplicate LOCAL clock");
                timestamp(value, true)?;
                local = true;
            }
            "MPEGTS" => {
                ensure!(
                    !mpeg && !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()),
                    "invalid MPEGTS clock"
                );
                ensure!(
                    value.parse::<u64>()? < (1_u64 << 33),
                    "MPEGTS clock overflow"
                );
                mpeg = true;
            }
            _ => bail!("unsupported timestamp mapping field"),
        }
    }
    ensure!(local && mpeg, "incomplete timestamp mapping");
    Ok(())
}

fn webvtt(text: &str, cancel: &CancellationToken) -> Result<()> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = normalized.lines();
    let header = lines.next().ok_or_else(|| anyhow!("empty WebVTT"))?;
    let suffix = header.strip_prefix("WEBVTT").unwrap_or("invalid");
    ensure!(
        (suffix.is_empty() || suffix.starts_with([' ', '\t'])) && !header.contains("-->"),
        "invalid WebVTT signature"
    );
    let mut mapping_seen = false;
    loop {
        match lines.next() {
            Some(line) if line.trim_matches(xml_space).is_empty() => break,
            Some(line) => {
                ensure!(!mapping_seen, "duplicate WebVTT header metadata");
                timestamp_map(line)?;
                mapping_seen = true;
            }
            None => {
                ensure!(!mapping_seen, "missing WebVTT header separator");
                return Ok(()); // A signature-only empty WebVTT segment is valid.
            }
        }
    }
    // Bound line-index amplification as well as input bytes. Do not allocate an
    // entry for every newline in a malicious multi-megabyte document.
    let lines: Vec<_> = lines.take(CUE_LINES + 1).collect();
    ensure!(lines.len() <= CUE_LINES, "subtitle line limit");
    for block in lines.split(|line| line.trim_matches(xml_space).is_empty()) {
        ensure!(!cancel.is_cancelled(), "lease cancelled");
        if block.is_empty() {
            continue;
        }
        if block[0] == "NOTE" || block[0].starts_with("NOTE ") || block[0].starts_with("NOTE\t") {
            continue;
        }
        // STYLE (CSS) and REGION blocks are intentionally unsupported. In
        // particular, no CSS url(), imports, images or fonts reach the player.
        let timing = if block[0].contains("-->") { 0 } else { 1 };
        ensure!(
            !matches!(block[0], "STYLE" | "REGION") && block.len() > timing,
            "unsupported WebVTT block"
        );
        cue_timing(block[timing], true)?;
        for line in &block[timing + 1..] {
            ensure!(!line.contains("-->"), "unexpected WebVTT timing in payload");
            cue_text(line, true)?;
        }
    }
    Ok(())
}

fn subrip(text: &str, cancel: &CancellationToken) -> Result<()> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<_> = normalized.lines().take(CUE_LINES + 1).collect();
    ensure!(lines.len() <= CUE_LINES, "subtitle line limit");
    let mut cues = 0;
    for block in lines.split(|line| line.trim_matches(xml_space).is_empty()) {
        ensure!(!cancel.is_cancelled(), "lease cancelled");
        if block.is_empty() {
            continue;
        }
        ensure!(
            block.len() >= 3
                && !block[0].is_empty()
                && block[0].bytes().all(|b| b.is_ascii_digit()),
            "invalid SubRip cue"
        );
        cue_timing(block[1], false)?;
        for line in &block[2..] {
            ensure!(!line.contains("-->"), "unexpected SubRip timing in payload");
            cue_text(line, false)?;
        }
        cues += 1;
    }
    ensure!(cues > 0, "empty SubRip document");
    Ok(())
}

#[derive(Default)]
struct Namespaces {
    styling: bool,
    parameter: bool,
}

fn ttml_child(parent: Option<&str>, name: &str) -> bool {
    match parent {
        None => name == "tt",
        Some("tt") => matches!(name, "head" | "body"),
        Some("head") => matches!(name, "styling" | "layout"),
        Some("styling") => name == "style",
        Some("layout") => name == "region",
        Some("body" | "div") => matches!(name, "div" | "p"),
        Some("p" | "span") => matches!(name, "span" | "br"),
        _ => false,
    }
}

fn ttml_attribute(key: &str, value: &str, ns: &Namespaces) -> bool {
    match key {
        "xmlns" | "xmlns:tts" | "xmlns:ttp" => true, // Checked separately, on root only.
        "xml:id" | "id" | "region" => identifier(value),
        "style" => !value.is_empty() && value.split_whitespace().all(identifier),
        "xml:lang" => value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "xml:space" => matches!(value, "default" | "preserve"),
        "timeContainer" => matches!(value, "par" | "seq"),
        "begin" | "end" | "dur" => {
            value.bytes().any(|b| b.is_ascii_digit())
                && value.bytes().all(|b| {
                    b.is_ascii_digit()
                        || matches!(b, b':' | b'.' | b'h' | b'm' | b's' | b'f' | b't')
                })
        }
        _ => {
            let allowed = if let Some(key) = key.strip_prefix("tts:") {
                ns.styling
                    && matches!(
                        key,
                        "color"
                            | "backgroundColor"
                            | "fontFamily"
                            | "fontSize"
                            | "fontStyle"
                            | "fontWeight"
                            | "textDecoration"
                            | "textAlign"
                            | "displayAlign"
                            | "origin"
                            | "extent"
                            | "padding"
                            | "overflow"
                            | "writingMode"
                            | "direction"
                            | "unicodeBidi"
                            | "lineHeight"
                            | "wrapOption"
                            | "showBackground"
                            | "visibility"
                            | "display"
                            | "opacity"
                            | "zIndex"
                            | "textOutline"
                    )
            } else if let Some(key) = key.strip_prefix("ttp:") {
                ns.parameter
                    && matches!(
                        key,
                        "frameRate"
                            | "frameRateMultiplier"
                            | "subFrameRate"
                            | "tickRate"
                            | "cellResolution"
                            | "timeBase"
                            | "dropMode"
                            | "markerMode"
                    )
            } else {
                false
            };
            // Only scalar values of these known non-fetching properties. No
            // URI-valued styles, CSS functions, profile URLs or extension attrs.
            allowed
                && !value.is_empty()
                && value.bytes().all(|b| {
                    b.is_ascii_alphanumeric()
                        || matches!(
                            b,
                            b' ' | b'\t' | b'#' | b'.' | b',' | b'%' | b'+' | b'-' | b'\''
                        )
                })
        }
    }
}

fn ttml_start(
    event: &BytesStart<'_>,
    reader: &Reader<&[u8]>,
    parent: Option<&str>,
    ns: &mut Namespaces,
) -> Result<(String, Vec<(String, String)>)> {
    let name = std::str::from_utf8(event.name().as_ref())?.to_string();
    ensure!(
        ttml_child(parent, &name),
        "unsupported TTML element/placement"
    );
    let mut attrs = Vec::new();
    for attr in event.attributes() {
        ensure!(attrs.len() < 64, "TTML attribute limit");
        let attr = attr?; // Includes duplicate-name and XML attribute syntax checks.
        ensure!(
            !attr.value.contains(&b'<'),
            "invalid XML attribute character"
        );
        let key = std::str::from_utf8(attr.key.as_ref())?.to_string();
        let value = attr
            .decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, reader.decoder())?
            .into_owned();
        valid_characters(&value)?;
        if key == "xmlns" || key.starts_with("xmlns:") {
            ensure!(parent.is_none(), "TTML namespace rebinding unsupported");
            match key.as_str() {
                "xmlns" => ensure!(value == TTML_NS, "unsupported TTML namespace"),
                "xmlns:tts" => {
                    ensure!(value == STYLING_NS, "unsupported TTML styling namespace");
                    ns.styling = true;
                }
                "xmlns:ttp" => {
                    ensure!(
                        value == PARAMETER_NS,
                        "unsupported TTML parameter namespace"
                    );
                    ns.parameter = true;
                }
                _ => bail!("TTML namespace aliases/extensions unsupported"),
            }
        }
        attrs.push((key, value));
    }
    for (key, value) in &attrs {
        ensure!(
            ttml_attribute(key, value, ns),
            "unsupported TTML attribute/value"
        );
    }
    if parent.is_none() && !attrs.iter().any(|(key, _)| key == "xmlns") {
        attrs.push(("xmlns".to_string(), TTML_NS.to_string()));
    }
    Ok((name, attrs))
}

struct CappedOutput(Vec<u8>);
impl std::io::Write for CappedOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > LIMIT - self.0.len() {
            return Err(std::io::Error::other("canonical subtitle too large"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn xml_declaration(text: &str) -> Result<()> {
    let declaration = BytesStart::from_content(text, 3);
    ensure!(
        declaration.name().as_ref() == b"xml",
        "invalid XML declaration"
    );
    let mut last = 0;
    for (index, attr) in declaration.attributes().enumerate() {
        let attr = attr?;
        let rank = match attr.key.as_ref() {
            b"version" => {
                ensure!(
                    index == 0 && attr.value.as_ref() == b"1.0",
                    "XML 1.0 required"
                );
                1
            }
            b"encoding" => {
                ensure!(
                    attr.value.eq_ignore_ascii_case(b"utf-8")
                        || attr.value.eq_ignore_ascii_case(b"us-ascii"),
                    "TTML must be UTF-8"
                );
                2
            }
            b"standalone" => {
                ensure!(
                    matches!(attr.value.as_ref(), b"yes" | b"no"),
                    "invalid XML standalone"
                );
                3
            }
            _ => bail!("unsupported XML declaration attribute"),
        };
        ensure!(
            rank > last && (index > 0 || rank == 1),
            "invalid XML declaration order"
        );
        last = rank;
    }
    ensure!(last > 0, "missing XML version");
    Ok(())
}

fn ttml(text: &str, cancel: &CancellationToken) -> Result<Vec<u8>> {
    let mut reader = Reader::from_str(text);
    reader.config_mut().enable_all_checks(true);
    let mut writer = Writer::new(CappedOutput(Vec::new()));
    let mut stack: Vec<String> = Vec::new();
    let mut ns = Namespaces::default();
    let mut root_seen = false;
    let mut root_closed = false;
    let mut declaration_seen = false;
    let mut events = 0;
    loop {
        ensure!(!cancel.is_cancelled(), "lease cancelled");
        events += 1;
        ensure!(events <= XML_EVENTS, "TTML XML event limit");
        let position = reader.buffer_position();
        let event = reader.read_event()?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                ensure!(
                    !root_closed && stack.len() < XML_DEPTH,
                    "multiple roots/TTML nesting limit"
                );
                let (name, attrs) =
                    ttml_start(e, &reader, stack.last().map(String::as_str), &mut ns)?;
                if stack.is_empty() {
                    ensure!(!root_seen, "multiple TTML roots");
                    root_seen = true;
                }
                let mut start = BytesStart::new(&name);
                for (key, value) in &attrs {
                    start.push_attribute((key.as_str(), value.as_str()));
                }
                if matches!(event, Event::Empty(_)) {
                    writer.write_event(Event::Empty(start))?;
                    root_closed = stack.is_empty();
                } else {
                    writer.write_event(Event::Start(start))?;
                    stack.push(name);
                }
            }
            Event::End(e) => {
                let name = stack.pop().ok_or_else(|| anyhow!("unbalanced TTML"))?;
                ensure!(
                    e.name().as_ref() == name.as_bytes(),
                    "mismatched TTML end tag"
                );
                writer.write_event(Event::End(BytesEnd::new(name)))?;
                root_closed = stack.is_empty();
            }
            Event::Text(e) => {
                ensure!(
                    !e.as_ref().windows(3).any(|w| w == b"]]>"),
                    "invalid XML text delimiter"
                );
                let value = e.xml10_content()?;
                valid_characters(&value)?;
                if stack.is_empty() {
                    // Only literal XML whitespace is legal outside the root.
                    // An entity that decodes to whitespace is not XML prolog text.
                    ensure!(
                        std::str::from_utf8(e.as_ref())?.chars().all(xml_space),
                        "payload outside TTML root"
                    );
                } else {
                    ensure!(
                        matches!(stack.last().map(String::as_str), Some("p" | "span"))
                            || value.chars().all(xml_space),
                        "text outside TTML cue"
                    );
                    writer.write_event(Event::Text(BytesText::new(&value)))?;
                }
            }
            Event::GeneralRef(e) => {
                ensure!(!stack.is_empty(), "entity outside TTML root");
                let value = super::manifest::reference_value(&e)?;
                valid_characters(&value)?;
                ensure!(
                    matches!(stack.last().map(String::as_str), Some("p" | "span"))
                        || value.chars().all(xml_space),
                    "text outside TTML cue"
                );
                writer.write_event(Event::Text(BytesText::new(&value)))?;
            }
            Event::CData(e) => {
                ensure!(
                    matches!(stack.last().map(String::as_str), Some("p" | "span")),
                    "CDATA outside TTML cue"
                );
                let value = std::str::from_utf8(&e)?;
                valid_characters(value)?;
                writer.write_event(Event::Text(BytesText::new(value)))?;
            }
            Event::Decl(e) => {
                ensure!(
                    position == 0 && !declaration_seen && !root_seen,
                    "misplaced XML declaration"
                );
                xml_declaration(std::str::from_utf8(&e)?)?;
                declaration_seen = true;
            }
            Event::Comment(_) => {
                ensure!(!stack.is_empty(), "comments outside TTML root unsupported")
            }
            Event::DocType(_) | Event::PI(_) => {
                bail!("TTML DTD/processing instructions unsupported")
            }
            Event::Eof => break,
        }
    }
    ensure!(
        root_seen && root_closed && stack.is_empty(),
        "empty/truncated TTML"
    );
    Ok(writer.into_inner().0)
}
