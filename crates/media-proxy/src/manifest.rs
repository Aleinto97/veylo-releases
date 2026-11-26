use super::*;
use quick_xml::{
    events::{BytesEnd, BytesStart, BytesText, Event},
    Reader, Writer,
};

pub(super) fn hls(lease: &Lease, base: &Url, text: &str) -> Result<String> {
    ensure!(
        !text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\r' | '\n')),
        "HLS control character"
    );
    ensure!(
        text.trim_start_matches('\u{feff}').lines().next() == Some("#EXTM3U"),
        "invalid HLS header"
    );
    ensure!(
        !text.contains("{$"),
        "HLS variable substitution is unsupported"
    );
    let mut out = String::new();
    for line in text.trim_start_matches('\u{feff}').lines() {
        ensure!(!lease.cancel.is_cancelled(), "lease cancelled");
        let line = line.trim();
        if line.is_empty() {
            out.push('\n');
            continue;
        }
        if !line.starts_with('#') {
            // A native engine must never interpret a protocol/options line itself.
            out.push_str(&lease.map_reference(base, line)?);
            out.push('\n');
            ensure!(out.len() <= MANIFEST_LIMIT * 4, "rewritten HLS too large");
            continue;
        }
        if line != "#EXTM3U" && !line.starts_with("#EXT-") && !line.starts_with("#EXTINF:") {
            continue;
        }
        let tag = line.split(':').next().unwrap_or(line);
        ensure!(
            matches!(
                tag,
                "#EXTM3U"
                    | "#EXTINF"
                    | "#EXT-X-VERSION"
                    | "#EXT-X-TARGETDURATION"
                    | "#EXT-X-MEDIA-SEQUENCE"
                    | "#EXT-X-DISCONTINUITY-SEQUENCE"
                    | "#EXT-X-ENDLIST"
                    | "#EXT-X-PLAYLIST-TYPE"
                    | "#EXT-X-I-FRAMES-ONLY"
                    | "#EXT-X-INDEPENDENT-SEGMENTS"
                    | "#EXT-X-START"
                    | "#EXT-X-ALLOW-CACHE"
                    | "#EXT-X-DATERANGE"
                    | "#EXT-X-SERVER-CONTROL"
                    | "#EXT-X-SKIP"
                    | "#EXT-X-BITRATE"
                    | "#EXT-X-GAP"
                    | "#EXT-X-DISCONTINUITY"
                    | "#EXT-X-PROGRAM-DATE-TIME"
                    | "#EXT-X-BYTERANGE"
                    | "#EXT-X-KEY"
                    | "#EXT-X-SESSION-KEY"
                    | "#EXT-X-MAP"
                    | "#EXT-X-MEDIA"
                    | "#EXT-X-I-FRAME-STREAM-INF"
                    | "#EXT-X-STREAM-INF"
                    | "#EXT-X-PART"
                    | "#EXT-X-PART-INF"
                    | "#EXT-X-PRELOAD-HINT"
                    | "#EXT-X-RENDITION-REPORT"
                    | "#EXT-X-IMAGE-STREAM-INF"
                    | "#EXT-X-IMAGES-ONLY"
                    | "#EXT-X-TILES"
            ),
            "unsupported HLS tag (external metadata/steering/variables are not supported)"
        );
        let attribute_list = matches!(
            tag,
            "#EXT-X-START"
                | "#EXT-X-DATERANGE"
                | "#EXT-X-SERVER-CONTROL"
                | "#EXT-X-SKIP"
                | "#EXT-X-KEY"
                | "#EXT-X-SESSION-KEY"
                | "#EXT-X-MAP"
                | "#EXT-X-MEDIA"
                | "#EXT-X-I-FRAME-STREAM-INF"
                | "#EXT-X-STREAM-INF"
                | "#EXT-X-PART"
                | "#EXT-X-PART-INF"
                | "#EXT-X-PRELOAD-HINT"
                | "#EXT-X-RENDITION-REPORT"
                | "#EXT-X-IMAGE-STREAM-INF"
                | "#EXT-X-TILES"
        );
        let rewritten = if attribute_list {
            rewrite_attributes(lease, base, line)?
        } else {
            line.to_string()
        };
        out.push_str(&rewritten);
        out.push('\n');
        ensure!(out.len() <= MANIFEST_LIMIT * 4, "rewritten HLS too large");
    }
    ensure!(out.len() <= MANIFEST_LIMIT * 4, "rewritten HLS too large");
    Ok(out)
}

fn rewrite_attributes(lease: &Lease, base: &Url, line: &str) -> Result<String> {
    let (tag, mut rest) = line
        .split_once(':')
        .ok_or_else(|| anyhow!("missing HLS attribute list"))?;
    let mut out = format!("{tag}:");
    let mut first = true;
    let mut keys = std::collections::HashSet::new();
    while !rest.is_empty() {
        let (key, after) = rest
            .split_once('=')
            .ok_or_else(|| anyhow!("invalid HLS attribute"))?;
        ensure!(
            !key.is_empty()
                && key
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-'),
            "invalid HLS attribute name"
        );
        ensure!(
            allowed_hls_attribute(tag, key),
            "unsupported HLS attribute/external metadata"
        );
        ensure!(keys.insert(key), "duplicate HLS attribute");
        let (value, next) = if let Some(quoted) = after.strip_prefix('"') {
            let end = quoted
                .find('"')
                .ok_or_else(|| anyhow!("unterminated HLS quote"))?;
            let tail = &quoted[end + 1..];
            ensure!(
                tail.is_empty() || tail.starts_with(','),
                "invalid HLS attribute separator"
            );
            (&after[..end + 2], tail.strip_prefix(',').unwrap_or(tail))
        } else {
            after.split_once(',').unwrap_or((after, ""))
        };
        ensure!(!value.is_empty(), "empty HLS attribute");
        ensure!(!rest.ends_with(','), "trailing HLS attribute separator");
        ensure!(
            !value.contains('\\') && !value.bytes().any(|b| b < 32 || b == 127),
            "invalid HLS attribute value"
        );
        if matches!(tag, "#EXT-X-KEY" | "#EXT-X-SESSION-KEY") {
            if key == "METHOD" {
                ensure!(
                    matches!(value, "NONE" | "AES-128"),
                    "unsupported HLS encryption method"
                );
            }
            if key == "KEYFORMAT" {
                ensure!(value == "\"identity\"", "HLS DRM key format unsupported");
            }
        }
        ensure!(
            tag != "#EXT-X-DATERANGE"
                || key != "CLASS"
                || value != "\"com.apple.hls.interstitial\"",
            "HLS interstitial metadata unsupported"
        );
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(key);
        out.push('=');
        if key == "URI" || key.ends_with("-URI") {
            ensure!(
                value.starts_with('"') && value.ends_with('"'),
                "unquoted HLS URI"
            );
            out.push('"');
            out.push_str(&lease.map_reference(base, &value[1..value.len() - 1])?);
            out.push('"');
        } else {
            out.push_str(value);
        }
        rest = next;
    }
    Ok(out)
}

fn allowed_hls_attribute(tag: &str, key: &str) -> bool {
    match tag {
        "#EXT-X-START" => matches!(key, "TIME-OFFSET" | "PRECISE"),
        "#EXT-X-DATERANGE" => matches!(
            key,
            "ID" | "CLASS"
                | "START-DATE"
                | "END-DATE"
                | "DURATION"
                | "PLANNED-DURATION"
                | "END-ON-NEXT"
                | "SCTE35-CMD"
                | "SCTE35-OUT"
                | "SCTE35-IN"
        ),
        "#EXT-X-SERVER-CONTROL" => matches!(
            key,
            "CAN-SKIP-UNTIL"
                | "CAN-SKIP-DATERANGES"
                | "CAN-BLOCK-RELOAD"
                | "HOLD-BACK"
                | "PART-HOLD-BACK"
        ),
        "#EXT-X-SKIP" => matches!(key, "SKIPPED-SEGMENTS" | "RECENTLY-REMOVED-DATERANGES"),
        "#EXT-X-KEY" | "#EXT-X-SESSION-KEY" => matches!(
            key,
            "METHOD" | "URI" | "IV" | "KEYFORMAT" | "KEYFORMATVERSIONS"
        ),
        "#EXT-X-MAP" => matches!(key, "URI" | "BYTERANGE"),
        "#EXT-X-MEDIA" => matches!(
            key,
            "TYPE"
                | "URI"
                | "GROUP-ID"
                | "LANGUAGE"
                | "ASSOC-LANGUAGE"
                | "NAME"
                | "DEFAULT"
                | "AUTOSELECT"
                | "FORCED"
                | "INSTREAM-ID"
                | "CHARACTERISTICS"
                | "CHANNELS"
                | "STABLE-RENDITION-ID"
        ),
        "#EXT-X-STREAM-INF" | "#EXT-X-I-FRAME-STREAM-INF" => matches!(
            key,
            "BANDWIDTH"
                | "AVERAGE-BANDWIDTH"
                | "CODECS"
                | "RESOLUTION"
                | "FRAME-RATE"
                | "HDCP-LEVEL"
                | "AUDIO"
                | "VIDEO"
                | "SUBTITLES"
                | "CLOSED-CAPTIONS"
                | "VIDEO-RANGE"
                | "SCORE"
                | "STABLE-VARIANT-ID"
                | "URI"
        ),
        "#EXT-X-PART" => matches!(
            key,
            "URI" | "DURATION" | "INDEPENDENT" | "BYTERANGE" | "GAP"
        ),
        "#EXT-X-PART-INF" => key == "PART-TARGET",
        "#EXT-X-PRELOAD-HINT" => {
            matches!(key, "TYPE" | "URI" | "BYTERANGE-START" | "BYTERANGE-LENGTH")
        }
        "#EXT-X-RENDITION-REPORT" => matches!(key, "URI" | "LAST-MSN" | "LAST-PART"),
        "#EXT-X-IMAGE-STREAM-INF" => matches!(key, "BANDWIDTH" | "CODECS" | "RESOLUTION" | "URI"),
        "#EXT-X-TILES" => matches!(key, "RESOLUTION" | "LAYOUT" | "DURATION"),
        _ => false,
    }
}

#[derive(Clone, Debug)]
struct Element {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
}
#[derive(Clone, Debug)]
enum Node {
    Element(Element),
    Text(String),
}
fn start(event: &BytesStart<'_>, reader: &Reader<&[u8]>) -> Result<Element> {
    let name = std::str::from_utf8(event.name().as_ref())?.to_string();
    ensure!(!name.contains(':'), "prefixed DASH elements unsupported");
    let mut attrs = Vec::new();
    for (index, attr) in event.attributes().enumerate() {
        ensure!(index < 64, "DASH attribute limit");
        let attr = attr?;
        let key = std::str::from_utf8(attr.key.as_ref())?.to_string();
        let value = attr
            .decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, reader.decoder())?
            .to_string();
        ensure!(
            !key.contains(':') || key == "xml:lang",
            "DASH namespaced attributes unsupported"
        );
        if key == "xmlns" {
            ensure!(
                name == "MPD" && value == "urn:mpeg:dash:schema:mpd:2011",
                "unsupported DASH namespace"
            );
        }
        attrs.push((key, value));
    }
    Ok(Element {
        name,
        attrs,
        children: Vec::new(),
    })
}
fn parse_xml(text: &str) -> Result<Element> {
    ensure!(
        !text
            .chars()
            .any(|c| (c.is_control() && !matches!(c, '\t' | '\r' | '\n'))
                || matches!(c, '\u{fffe}' | '\u{ffff}')),
        "invalid XML character"
    );
    let mut reader = Reader::from_str(text);
    let mut stack: Vec<Element> = Vec::new();
    let mut root = None;
    let mut nodes = 0usize;
    loop {
        nodes += 1;
        ensure!(nodes <= 65536, "DASH XML node limit");
        match reader.read_event()? {
            Event::Start(e) => {
                ensure!(stack.len() < 64, "DASH XML nesting limit");
                stack.push(start(&e, &reader)?);
            }
            Event::Empty(e) => {
                let el = start(&e, &reader)?;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(Node::Element(el));
                } else {
                    ensure!(root.is_none(), "multiple XML roots");
                    root = Some(el);
                }
            }
            Event::Text(e) => {
                let value = e.xml10_content()?.to_string();
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(Node::Text(value));
                } else {
                    ensure!(value.trim().is_empty(), "text outside MPD");
                }
            }
            Event::GeneralRef(e) => {
                let value = reference_value(&e)?;
                stack
                    .last_mut()
                    .ok_or_else(|| anyhow!("entity outside MPD"))?
                    .children
                    .push(Node::Text(value));
            }
            Event::CData(e) => {
                let value = std::str::from_utf8(&e)?.to_string();
                stack
                    .last_mut()
                    .ok_or_else(|| anyhow!("CDATA outside MPD"))?
                    .children
                    .push(Node::Text(value));
            }
            Event::End(_) => {
                let el = stack.pop().ok_or_else(|| anyhow!("unbalanced XML"))?;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(Node::Element(el));
                } else {
                    ensure!(root.is_none(), "multiple XML roots");
                    root = Some(el);
                }
            }
            Event::Decl(d) => {
                if let Some(encoding) = d.encoding() {
                    let encoding = encoding?;
                    ensure!(
                        encoding.eq_ignore_ascii_case(b"utf-8")
                            || encoding.eq_ignore_ascii_case(b"us-ascii"),
                        "DASH must be UTF-8"
                    );
                }
            }
            Event::Comment(_) => {}
            Event::DocType(_) | Event::PI(_) => {
                bail!("DASH DTD/processing instructions unsupported")
            }
            Event::Eof => break,
        }
    }
    ensure!(stack.is_empty(), "truncated XML");
    root.ok_or_else(|| anyhow!("empty XML"))
}
fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}
fn text_content(el: &Element) -> Result<String> {
    let mut text = String::new();
    for child in &el.children {
        match child {
            Node::Text(t) => text.push_str(t),
            _ => bail!("nested BaseURL"),
        }
    }
    Ok(text.trim().to_string())
}
fn rewrite_element(
    el: &mut Element,
    lease: &Lease,
    inherited: &Url,
    budget: &mut usize,
) -> Result<()> {
    ensure!(!lease.cancel.is_cancelled(), "lease cancelled");
    let name = local(&el.name).to_string();
    ensure!(matches!(name.as_str(),
        "MPD"|"ProgramInformation"|"Title"|"Source"|"Copyright"|"Period"|
        "AdaptationSet"|"Representation"|"SubRepresentation"|"SegmentBase"|"SegmentList"|
        "SegmentTemplate"|"SegmentTimeline"|"S"|"SegmentURL"|"Initialization"|
        "RepresentationIndex"|"BitstreamSwitching"|"ContentComponent"|"Role"|"Rating"|
        "Accessibility"|"Viewpoint"|"FramePacking"|"AudioChannelConfiguration"|
        "EssentialProperty"|"SupplementalProperty"|"Subset"|"Label"|"GroupLabel"|
        "ServiceDescription"|"Latency"|"PlaybackRate"|"OperatingQuality"|"OperatingBandwidth"),
        "unsupported DASH element (external metadata/steering/license/extensions are not supported)");
    for (key, value) in &el.attrs {
        ensure!(
            matches!(
                key.as_str(),
                "xmlns"
                    | "xml:lang"
                    | "id"
                    | "profiles"
                    | "type"
                    | "availabilityStartTime"
                    | "availabilityEndTime"
                    | "publishTime"
                    | "mediaPresentationDuration"
                    | "minimumUpdatePeriod"
                    | "minBufferTime"
                    | "timeShiftBufferDepth"
                    | "suggestedPresentationDelay"
                    | "maxSegmentDuration"
                    | "maxSubsegmentDuration"
                    | "start"
                    | "duration"
                    | "bitstreamSwitching"
                    | "lang"
                    | "contentType"
                    | "par"
                    | "minBandwidth"
                    | "maxBandwidth"
                    | "minWidth"
                    | "maxWidth"
                    | "minHeight"
                    | "maxHeight"
                    | "minFrameRate"
                    | "maxFrameRate"
                    | "segmentAlignment"
                    | "subsegmentAlignment"
                    | "subsegmentStartsWithSAP"
                    | "startsWithSAP"
                    | "bandwidth"
                    | "qualityRanking"
                    | "dependencyId"
                    | "associationId"
                    | "associationType"
                    | "mimeType"
                    | "codecs"
                    | "width"
                    | "height"
                    | "frameRate"
                    | "sar"
                    | "audioSamplingRate"
                    | "scanType"
                    | "maxPlayoutRate"
                    | "codingDependency"
                    | "selectionPriority"
                    | "timescale"
                    | "presentationTimeOffset"
                    | "indexRange"
                    | "indexRangeExact"
                    | "availabilityTimeOffset"
                    | "availabilityTimeComplete"
                    | "media"
                    | "initialization"
                    | "index"
                    | "sourceURL"
                    | "mediaRange"
                    | "range"
                    | "startNumber"
                    | "endNumber"
                    | "t"
                    | "d"
                    | "r"
                    | "k"
                    | "n"
                    | "schemeIdUri"
                    | "value"
                    | "level"
                    | "contains"
                    | "target"
                    | "min"
                    | "max"
                    | "referenceId"
                    | "contentComponentId"
            ),
            "unsupported DASH attribute"
        );
        if name == "Representation" && key == "id" {
            ensure!(
                !value.is_empty()
                    && value != "."
                    && value != ".."
                    && value
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')),
                "unsafe DASH RepresentationID substitution"
            );
        }
        let lower = local(key).to_ascii_lowercase();
        ensure!(
            !key.eq_ignore_ascii_case("xml:base"),
            "DASH xml:base is unsupported"
        );
        ensure!(
            !(lower.contains("url") || lower.contains("uri"))
                || matches!(local(key), "sourceURL" | "schemeIdUri"),
            "unsupported DASH URL attribute"
        );
        ensure!(
            !key.starts_with("xlink:")
                && !matches!(local(key), "href" | "licenseUrl" | "licenseURL"),
            "DASH external link unsupported"
        );
    }
    if let Some((_, scheme)) = el.attrs.iter().find(|(key, _)| key == "schemeIdUri") {
        let allowed = match name.as_str() {
            "Role" => scheme == "urn:mpeg:dash:role:2011",
            "AudioChannelConfiguration" => matches!(
                scheme.as_str(),
                "urn:mpeg:dash:23003:3:audio_channel_configuration:2011"
                    | "tag:dolby.com,2014:dash:audio_channel_configuration:2011"
                    | "urn:mpeg:mpegB:cicp:ChannelConfiguration"
            ),
            "EssentialProperty" | "SupplementalProperty" => matches!(
                scheme.as_str(),
                "urn:mpeg:dash:adaptation-set-switching:2016"
                    | "http://dashif.org/guidelines/trickmode"
                    | "urn:mpeg:dash:trick-mode:2016"
                    | "urn:mpeg:dash:srd:2014"
                    | "urn:mpeg:dash:period-connectivity:2015"
                    | "urn:mpeg:dash:period-continuity:2015"
            ),
            "Accessibility" => matches!(
                scheme.as_str(),
                "urn:scte:dash:cc:cea-608:2015" | "urn:scte:dash:cc:cea-708:2015"
            ),
            "FramePacking" => {
                scheme == "urn:mpeg:dash:14496:10:frame_packing_arrangement_type:2011"
            }
            _ => false,
        };
        ensure!(
            allowed,
            "unsupported DASH descriptor scheme/external metadata"
        );
        if let Some((_, value)) = el.attrs.iter().find(|(key, _)| key == "value") {
            ensure!(
                value.bytes().all(|b| b.is_ascii_alphanumeric()
                    || matches!(b, b' ' | b'.' | b',' | b';' | b'=' | b'_' | b'-')),
                "unsupported DASH descriptor value"
            );
        }
    } else {
        ensure!(
            !matches!(
                name.as_str(),
                "EssentialProperty"
                    | "SupplementalProperty"
                    | "Role"
                    | "AudioChannelConfiguration"
                    | "Accessibility"
                    | "FramePacking"
                    | "Rating"
                    | "Viewpoint"
            ),
            "DASH descriptor scheme required"
        );
    }
    let bases = el
        .children
        .iter()
        .filter_map(|n| match n {
            Node::Element(e) if local(&e.name) == "BaseURL" => Some(e),
            _ => None,
        })
        .collect::<Vec<_>>();
    ensure!(
        bases.len() <= 1,
        "multiple alternative DASH BaseURLs unsupported"
    );
    ensure!(
        bases.is_empty()
            || matches!(
                name.as_str(),
                "MPD" | "Period" | "AdaptationSet" | "Representation"
            ),
        "unsupported DASH BaseURL placement"
    );
    let owned_base;
    let base: &Url = if let Some(b) = bases.first() {
        let text = text_content(b)?;
        ensure!(!text.is_empty(), "empty DASH BaseURL");
        ensure!(!text.contains('$'), "DASH BaseURL templates unsupported");
        validate_reference(&text)?;
        let url = inherited.join(&text)?;
        ensure!(url.as_str().len() <= 16_384, "DASH BaseURL too long");
        lease.validate_url(&url)?;
        owned_base = url;
        &owned_base
    } else {
        inherited
    };
    // If an AdaptationSet declares a SegmentTemplate while any child Representation declares its
    // own BaseURL, push the SegmentTemplate down into each Representation so relative template URLs
    // resolve against each Representation's effective BaseURL instead of the parent's.
    if name == "AdaptationSet" {
        let rep_has_base = el.children.iter().any(|n| match n {
            Node::Element(rep) if local(&rep.name) == "Representation" => rep
                .children
                .iter()
                .any(|c| matches!(c, Node::Element(e) if local(&e.name) == "BaseURL")),
            _ => false,
        });
        let parent_tpl_idx = el
            .children
            .iter()
            .position(|n| matches!(n, Node::Element(e) if local(&e.name) == "SegmentTemplate"));
        if rep_has_base {
            if let Some(idx) = parent_tpl_idx {
                if let Node::Element(parent_tpl) = el.children.remove(idx) {
                    for child in &mut el.children {
                        if let Node::Element(rep) = child {
                            if local(&rep.name) != "Representation" {
                                continue;
                            }
                            if let Some(Node::Element(rep_tpl)) = rep.children.iter_mut().find(|c| {
                                matches!(c, Node::Element(e) if local(&e.name) == "SegmentTemplate")
                            }) {
                                for (k, v) in &parent_tpl.attrs {
                                    if !rep_tpl.attrs.iter().any(|(rk, _)| local(rk) == local(k)) {
                                        rep_tpl.attrs.push((k.clone(), v.clone()));
                                    }
                                }
                                if rep_tpl.children.is_empty() && !parent_tpl.children.is_empty() {
                                    rep_tpl.children = parent_tpl.children.clone();
                                }
                            } else {
                                rep.children.push(Node::Element(parent_tpl.clone()));
                            }
                        }
                    }
                }
            }
        }
    }
    for (key, value) in &mut el.attrs {
        if matches!(
            local(key),
            "media" | "initialization" | "sourceURL" | "index"
        ) || (name == "SegmentTemplate" && local(key) == "bitstreamSwitching")
        {
            validate_template(value, base)?;
            let mapped = lease.map_reference(base, value)?;
            *budget += mapped.len();
            ensure!(*budget <= MANIFEST_LIMIT * 4, "rewritten DASH too large");
            *value = mapped;
        } else {
            ensure!(
                !value.contains('$'),
                "DASH templates only supported in segment URL attributes"
            );
        }
    }
    for child in &mut el.children {
        if let Node::Element(child) = child {
            if local(&child.name) == "BaseURL" {
                // BaseURL never reaches the engine with its upstream authority.
                let mapped = lease.local_url(base)?;
                *budget += mapped.len();
                ensure!(*budget <= MANIFEST_LIMIT * 4, "rewritten DASH too large");
                child.children = vec![Node::Text(mapped)];
                for (key, _) in &child.attrs {
                    ensure!(
                        matches!(
                            key.as_str(),
                            "serviceLocation"
                                | "byteRange"
                                | "availabilityTimeOffset"
                                | "availabilityTimeComplete"
                        ),
                        "unsupported DASH BaseURL attribute"
                    );
                }
            } else {
                rewrite_element(child, lease, base, budget)?;
            }
        }
    }
    Ok(())
}
// Only standard numeric / RepresentationID tokens in path/query are safe to
// preserve. In an authority they would be hidden by origin encoding; in a
// BaseURL or identifier they can change how the native engine resolves URLs.
pub(super) fn validate_template(reference: &str, base: &Url) -> Result<()> {
    let url = base.join(reference)?;
    ensure!(
        !url[..url::Position::BeforePath].contains('$'),
        "DASH authority template unsupported"
    );
    let mut rest = reference;
    while let Some((_, after)) = rest.split_once('$') {
        let (token, tail) = after
            .split_once('$')
            .ok_or_else(|| anyhow!("unterminated DASH template"))?;
        let (name, format) = token.split_once('%').unwrap_or((token, ""));
        ensure!(
            matches!(name, "Number" | "Time" | "Bandwidth" | "RepresentationID"),
            "unsupported DASH template"
        );
        if !format.is_empty() {
            ensure!(
                name != "RepresentationID",
                "formatted RepresentationID unsupported"
            );
            let digits = format
                .strip_prefix('0')
                .and_then(|s| s.strip_suffix('d'))
                .ok_or_else(|| anyhow!("unsupported DASH template format"))?;
            ensure!(
                !digits.is_empty()
                    && digits.len() <= 2
                    && digits.bytes().all(|b| b.is_ascii_digit())
                    && matches!(digits.parse::<usize>()?, 1..=20),
                "DASH template width limit"
            );
        } else {
            ensure!(!token.contains('%'), "empty DASH template format");
        }
        rest = tail;
    }
    Ok(())
}
struct CappedOutput(Vec<u8>);
impl std::io::Write for CappedOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MANIFEST_LIMIT * 4 - self.0.len() {
            return Err(std::io::Error::other("rewritten DASH too large"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn emit(el: &Element, writer: &mut Writer<CappedOutput>) -> Result<()> {
    let mut start = BytesStart::new(&el.name);
    for (key, value) in &el.attrs {
        start.push_attribute((key.as_str(), value.as_str()));
    }
    writer.write_event(Event::Start(start))?;
    for child in &el.children {
        match child {
            Node::Text(t) => writer.write_event(Event::Text(BytesText::new(t)))?,
            Node::Element(e) => emit(e, writer)?,
        }
    }
    writer.write_event(Event::End(BytesEnd::new(&el.name)))?;
    Ok(())
}
pub(super) fn dash(lease: &Lease, base: &Url, text: &str) -> Result<String> {
    let mut root = parse_xml(text)?;
    ensure!(local(&root.name) == "MPD", "unsupported XML manifest");
    rewrite_element(&mut root, lease, base, &mut 0)?;
    let mut writer = Writer::new(CappedOutput(Vec::new()));
    emit(&root, &mut writer)?;
    let bytes = writer.into_inner().0;
    ensure!(
        bytes.len() <= MANIFEST_LIMIT * 4,
        "rewritten DASH too large"
    );
    Ok(String::from_utf8(bytes)?)
}

/// DTDs are rejected, so only XML's predefined and numeric references resolve.
pub(super) fn reference_value(reference: &quick_xml::events::BytesRef<'_>) -> Result<String> {
    Ok(quick_xml::escape::unescape(&format!("&{};", reference.decode()?))?.into_owned())
}
