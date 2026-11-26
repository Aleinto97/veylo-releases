use anyhow::{anyhow, bail, ensure, Context, Result};
use reqwest::{header, Client, StatusCode};
use serde_json::{json, Map, Value};
use std::{net::IpAddr, time::Duration};
use url::Url;

#[derive(Clone, Debug)]
pub(crate) struct Addon {
    pub url: String,
    /// Shared: the parsed manifest is cached and handed to every request without copying.
    pub manifest: std::sync::Arc<Value>,
    pub allow_http: bool,
    pub generation: String,
}
impl Addon {
    pub fn descriptor(&self) -> Value {
        let mut v = (*self.manifest).clone();
        v["manifest_url"] = json!(self.url);
        v
    }
    pub fn name(&self) -> String {
        self.manifest["name"]
            .as_str()
            .unwrap_or("Unnamed addon")
            .to_owned()
    }
    pub fn supports(&self, resource: &str, kind: &str, id: &str) -> bool {
        let Some(resources) = self.manifest["resources"].as_array() else {
            return false;
        };
        resources.iter().any(|r| {
            let (name, types, prefixes) = if let Some(name) = r.as_str() {
                (
                    Some(name),
                    self.manifest.get("types"),
                    self.manifest.get("idPrefixes"),
                )
            } else {
                (
                    r["name"].as_str(),
                    r.get("types").or_else(|| self.manifest.get("types")),
                    r.get("idPrefixes")
                        .or_else(|| self.manifest.get("idPrefixes")),
                )
            };
            name == Some(resource)
                && includes(types, kind)
                && prefixes
                    .map(|p| {
                        p.as_array().is_some_and(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .any(|prefix| id.starts_with(prefix))
                        })
                    })
                    .unwrap_or(true)
        })
    }
    pub fn catalog(&self, kind: &str, id: &str) -> Option<&Value> {
        self.manifest["catalogs"]
            .as_array()?
            .iter()
            .find(|c| c["type"] == kind && c["id"] == id)
    }
}
fn includes(values: Option<&Value>, needle: &str) -> bool {
    values
        .map(|v| {
            v.as_array()
                .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(needle)))
        })
        .unwrap_or(true)
}
/// Errors cross into UI and logs; configured addon URLs often carry provider
/// tokens in the path or query. Keep only scheme + host so secrets never escape.
/// Schemes match case-insensitively. A URL token ends only at whitespace or a
/// quote/angle bracket, because `(`, `)`, `'` and `[` are legal in paths and
/// stopping there would leak the tail of a secret-bearing path.
pub(crate) fn redact(input: impl AsRef<str>) -> String {
    const SCHEMES: [&str; 3] = ["https://", "http://", "stremio://"];
    let input = input.as_ref();
    // ASCII lowercasing keeps byte offsets identical to `input`.
    let lower = input.to_ascii_lowercase();
    let mut out = String::with_capacity(input.len());
    let mut at = 0;
    while let Some((start, scheme_len)) = SCHEMES
        .iter()
        .filter_map(|s| lower[at..].find(s).map(|p| (at + p, s.len())))
        .min_by_key(|(p, _)| *p)
    {
        out.push_str(&input[at..start]);
        let bytes = input.as_bytes();
        let mut end = start + scheme_len;
        while end < bytes.len()
            && !bytes[end].is_ascii_whitespace()
            && !b"<>\"`".contains(&bytes[end])
        {
            end += 1;
        }
        let token = &input[start..end];
        let trimmed = token.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '\'']);
        let trailing = &token[trimmed.len()..];
        let safe = match Url::parse(trimmed) {
            Ok(u) => format!("{}://{}/...", u.scheme(), u.host_str().unwrap_or("...")),
            Err(_) => "<url>".to_owned(),
        };
        out.push_str(&safe);
        out.push_str(trailing);
        at = end;
    }
    out.push_str(&input[at..]);
    out
}

pub(crate) fn normalize(input: &str, allow_http: bool) -> Result<String> {
    let input = input.trim();
    ensure!(
        !input.is_empty() && input.len() <= 16384,
        "Invalid addon URL"
    );
    let converted;
    let input = match input.get(..10) {
        Some(scheme) if scheme.eq_ignore_ascii_case("stremio://") => {
            converted = format!("https://{}", &input[10..]);
            &converted
        }
        _ => input,
    };
    let mut url = Url::parse(input).map_err(|_| anyhow!("Invalid addon URL"))?;
    validate_transport(&url, allow_http)?;
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "URL credentials are unsupported; use configured addon paths"
    );
    url.set_fragment(None);
    // `/manifest.json/` is the same manifest as `/manifest.json`.
    let trimmed = url.path().trim_end_matches('/');
    let path = if trimmed.ends_with("/manifest.json") {
        trimmed.to_owned()
    } else {
        format!("{trimmed}/manifest.json")
    };
    if path != url.path() {
        url.set_path(&path);
    }
    Ok(url.to_string())
}
/// Loopback, including forms that reach the local machine without saying
/// `127.0.0.1`: `*.localhost`, the unspecified address (`0.0.0.0`, `::`) and
/// IPv4-mapped IPv6 (`::ffff:127.0.0.1`).
pub(crate) fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            domain == "localhost"
                || domain.ends_with(".localhost")
                || domain
                    .trim_matches(['[', ']'])
                    .parse::<IpAddr>()
                    .is_ok_and(ip_is_local)
        }
        Some(url::Host::Ipv4(ip)) => ip_is_local(IpAddr::V4(ip)),
        Some(url::Host::Ipv6(ip)) => ip_is_local(IpAddr::V6(ip)),
        None => false,
    }
}
fn ip_is_local(ip: IpAddr) -> bool {
    let ip = ip.to_canonical();
    ip.is_loopback() || ip.is_unspecified()
}
pub(crate) fn validate_transport(url: &Url, allow_http: bool) -> Result<()> {
    let loopback = is_loopback(url);
    ensure!(url.host_str().is_some(), "Addon URL needs a host");
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "URL credentials are unsupported"
    );
    ensure!(
        url.scheme() == "https" || (url.scheme() == "http" && (loopback || allow_http)),
        "Non-loopback HTTP requires allow_http: true"
    );
    Ok(())
}
fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            use std::fmt::Write;
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}
pub(crate) fn endpoint(
    manifest: &str,
    resource: &str,
    kind: &str,
    id: &str,
    extra: &Map<String, Value>,
) -> Result<String> {
    ensure!(
        !matches!(kind, "." | "..") && !matches!(id, "." | ".."),
        "Dot path segments are unsupported media identities"
    );
    let mut url = Url::parse(manifest).map_err(|_| anyhow!("Invalid addon URL"))?;
    let base = url
        .path()
        .strip_suffix("/manifest.json")
        .context("Expected manifest.json URL")?;
    let mut path = format!(
        "{base}/{}/{}/{}",
        encode(resource),
        encode(kind),
        encode(id)
    );
    if !extra.is_empty() {
        path.push('/');
        let parts = extra
            .iter()
            .map(|(k, v)| {
                let v = v
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| v.to_string());
                format!("{}={}", encode(k), encode(&v))
            })
            .collect::<Vec<_>>();
        path.push_str(&parts.join("&"));
    }
    path.push_str(".json");
    url.set_path(&path);
    url.set_fragment(None);
    Ok(url.to_string())
}
pub(crate) fn validate_manifest(mut v: Value) -> Result<Value> {
    ensure!(v.is_object(), "Manifest must be an object");
    for key in ["id", "name", "version"] {
        ensure!(
            v.get(key)
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty()),
            "Manifest missing {key}"
        );
    }
    for key in ["resources", "types", "catalogs"] {
        if v.get(key).is_none() {
            v[key] = json!([]);
        }
        ensure!(v[key].is_array(), "Manifest {key} must be an array");
    }
    v.as_object_mut().unwrap().remove("bootstrap");
    Ok(v)
}
pub(crate) fn validate_extras(catalog: &Value, extras: &Map<String, Value>) -> Result<()> {
    if let Some(list) = catalog["extra"].as_array() {
        for e in list {
            if e["isRequired"] == true {
                let name = e["name"]
                    .as_str()
                    .context("Invalid required catalog extra")?;
                ensure!(
                    extras
                        .get(name)
                        .is_some_and(|v| !v.is_null() && v.as_str() != Some("")),
                    "Required catalog extra missing: {name}"
                );
            }
        }
    }
    if let Some(list) = catalog["extraRequired"].as_array() {
        for name in list.iter().filter_map(Value::as_str) {
            ensure!(
                extras
                    .get(name)
                    .is_some_and(|v| !v.is_null() && v.as_str() != Some("")),
                "Required catalog extra missing: {name}"
            );
        }
    }
    Ok(())
}
pub(crate) fn searchable(catalog: &Value) -> bool {
    catalog["extra"]
        .as_array()
        .is_some_and(|a| a.iter().any(|e| e["name"] == "search"))
        || catalog["extraSupported"]
            .as_array()
            .is_some_and(|a| a.iter().any(|e| e == "search"))
        || catalog["extraRequired"]
            .as_array()
            .is_some_and(|a| a.iter().any(|e| e == "search"))
}
pub(crate) struct Fetched {
    pub body: Value,
    pub ttl: i64,
    pub persist: bool,
    pub must_revalidate: bool,
}
pub(crate) async fn fetch(
    client: &Client,
    target: &str,
    allow_http: bool,
    default_ttl: i64,
    max_ttl: i64,
) -> Result<Fetched> {
    let mut url = Url::parse(target).map_err(|_| anyhow!("Invalid addon URL"))?;
    let allow_loopback = is_loopback(&url);
    for redirects in 0..=3 {
        validate_transport(&url, allow_http)?;
        ensure!(
            allow_loopback || !is_loopback(&url),
            "Remote addon cannot redirect to loopback"
        );
        let mut response = client.get(url.clone()).send().await.map_err(|e| {
            if e.is_timeout() {
                anyhow!("Addon request timed out")
            } else {
                anyhow!("Addon request failed")
            }
        })?;
        if response.status().is_redirection() {
            ensure!(redirects < 3, "Too many addon redirects");
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .context("Invalid addon redirect")?;
            url = url
                .join(location)
                .map_err(|_| anyhow!("Invalid addon redirect"))?;
            continue;
        }
        ensure!(
            response.status() == StatusCode::OK,
            "Addon returned HTTP {}",
            response.status().as_u16()
        );
        // Repeated Cache-Control field lines are one directive list. In particular,
        // a restrictive second line must never be hidden by a permissive first one.
        let directives = response
            .headers()
            .get_all(header::CACHE_CONTROL)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|line| line.split(','))
            .map(|directive| {
                let (name, value) = directive
                    .trim()
                    .split_once('=')
                    .unwrap_or((directive.trim(), ""));
                (
                    name.trim().to_ascii_lowercase(),
                    value.trim().trim_matches('"').to_owned(),
                )
            })
            .collect::<Vec<_>>();
        let persist = !directives
            .iter()
            .any(|(name, _)| name == "no-store" || name == "private");
        let must_revalidate = directives.iter().any(|(name, _)| name == "no-cache");
        let mut ttl = directives
            .iter()
            .filter(|(name, _)| name == "max-age")
            .filter_map(|(_, value)| value.parse::<i64>().ok())
            .min()
            .unwrap_or(default_ttl)
            .clamp(0, max_ttl);
        if must_revalidate {
            ttl = 0;
        }
        if let Some(age) = response
            .headers()
            .get(header::AGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok())
        {
            ttl = ttl.saturating_sub(age.max(0));
        }
        ensure!(
            response.content_length().unwrap_or(0) <= 8 * 1024 * 1024,
            "Addon response too large"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow!("Could not read addon response"))?
        {
            ensure!(
                bytes.len() + chunk.len() <= 8 * 1024 * 1024,
                "Addon response too large"
            );
            bytes.extend_from_slice(&chunk);
        }
        let body: Value =
            serde_json::from_slice(&bytes).map_err(|_| anyhow!("Addon returned invalid JSON"))?;
        ensure!(body.is_object(), "Addon response must be an object");
        return Ok(Fetched {
            body,
            ttl,
            persist,
            must_revalidate,
        });
    }
    bail!("Too many addon redirects")
}
pub(crate) fn client() -> Result<Client> {
    Ok(Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .user_agent("RustMedia/0.1 (Stremio addon protocol)")
        .build()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transport_consent_and_namespaces() {
        assert!(normalize("http://example.com/manifest.json", false).is_err());
        assert!(normalize("http://example.com/manifest.json", true).is_ok());
        assert!(normalize("http://127.0.0.1:1234/manifest.json", false).is_ok());
        assert!(normalize("http://[::1]:1234/manifest.json", false).is_ok());
        assert!(normalize("file:///manifest.json", true).is_err());
        assert_eq!(
            normalize("stremio://example.com/config/manifest.json?key=a", false).unwrap(),
            "https://example.com/config/manifest.json?key=a"
        );
        assert_eq!(
            normalize("Stremio://example.com/config/manifest.json", false).unwrap(),
            "https://example.com/config/manifest.json"
        );
        assert_eq!(
            normalize("https://example.com/config/manifest.json/", false).unwrap(),
            "https://example.com/config/manifest.json"
        );
        assert_eq!(
            normalize("https://example.com/config/", false).unwrap(),
            "https://example.com/config/manifest.json"
        );
    }
    #[test]
    fn loopback_detection_covers_mapped_unspecified_and_localhost_subdomains() {
        let loopback = |u: &str| is_loopback(&Url::parse(u).unwrap());
        for u in [
            "http://localhost/",
            "http://LOCALHOST:1/",
            "http://foo.localhost/",
            "http://API.Localhost./",
            "http://127.0.0.1/",
            "http://127.9.9.9/",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://0.0.0.0/",
            "http://[::]/",
            "http://[::ffff:0.0.0.0]/",
        ] {
            assert!(loopback(u), "{u}");
        }
        for u in [
            "https://example.com/",
            "https://notlocalhost/",
            "https://localhost.example.com/",
            "http://8.8.8.8/",
            "http://[2001:db8::1]/",
            "http://[::ffff:8.8.8.8]/",
        ] {
            assert!(!loopback(u), "{u}");
        }
    }
}

#[cfg(test)]
mod protocol_tests {
    use super::*;
    #[test]
    fn configured_paths_queries_and_resource_overrides() {
        let endpoint = endpoint(
            "https://example.com/config%2Fsecret/manifest.json?x=a%2Bb",
            "meta",
            "anime",
            "kitsu:123/1",
            &Map::new(),
        )
        .unwrap();
        assert_eq!(
            endpoint,
            "https://example.com/config%2Fsecret/meta/anime/kitsu%3A123%2F1.json?x=a%2Bb"
        );
        let a = Addon {
            url: "https://example.com/manifest.json".into(),
            allow_http: false,
            generation: String::new(),
            manifest: json!({
                "types":["series"],"idPrefixes":["tt"],"resources":["subtitles",{"name":"stream","types":["anime"],"idPrefixes":["kitsu:"]}]
            })
            .into(),
        };
        assert!(a.supports("subtitles", "series", "tt1"));
        assert!(!a.supports("subtitles", "anime", "kitsu:1"));
        assert!(a.supports("stream", "anime", "kitsu:1"));
        assert!(!a.supports("stream", "series", "tt1"));
        assert!(super::endpoint(&a.url, "meta", "..", "tt1", &Map::new()).is_err());
    }
}

#[cfg(test)]
mod redaction_tests {
    use super::redact;
    #[test]
    fn strips_path_query_and_credentials() {
        let msg = redact("GET https://user:pass@torrentio.strem.fun/sort=size|realdebrid=SECRETKEY/manifest.json?api=ALSO_SECRET failed");
        assert!(!msg.contains("SECRET"), "{msg}");
        assert!(!msg.contains("user"), "{msg}");
        assert!(msg.contains("https://torrentio.strem.fun/..."), "{msg}");
    }
    #[test]
    fn keeps_non_url_text_and_punctuation() {
        assert_eq!(redact("plain error, no url."), "plain error, no url.");
    }
    #[test]
    fn handles_ipv6_hosts_and_case_insensitive_schemes() {
        let msg = redact("failed HTTP://[2001:db8::1]:8080/tok_SECRET/manifest.json now");
        assert_eq!(msg, "failed http://[2001:db8::1]/... now");
        let msg = redact("bad Stremio://host.example/SECRET/manifest.json");
        assert!(!msg.contains("SECRET"), "{msg}");
        assert!(msg.contains("stremio://host.example/..."), "{msg}");
        let msg = redact("HttpS://Host.Example/SECRET");
        assert!(!msg.contains("SECRET"), "{msg}");
    }
    #[test]
    fn parentheses_and_apostrophes_in_paths_do_not_leak_the_tail() {
        let msg = redact(
            "error sending request for url (https://h.example/a(b)c'SECRET_TAIL/x?k=SECRET2)",
        );
        assert!(!msg.contains("SECRET"), "{msg}");
        assert!(msg.contains("https://h.example/..."), "{msg}");
        let msg = redact("'https://h.example/it's_SECRET/manifest.json' failed");
        assert!(!msg.contains("SECRET"), "{msg}");
        assert!(msg.ends_with("failed"), "{msg}");
    }
}
