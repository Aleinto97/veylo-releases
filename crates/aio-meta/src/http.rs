use crate::catalog;
use crate::ids::Kind;
use crate::image;
use crate::manifest;
use crate::meta;
use crate::state::State;
use axum::{
    body::{Body, Bytes},
    extract::State as AxumState,
    http::{header, Method, Request, StatusCode},
    response::Response,
    Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) fn router(state: Arc<State>) -> Router {
    Router::new().fallback(handle).with_state(state)
}

async fn handle(AxumState(state): AxumState<Arc<State>>, request: Request<Body>) -> Response {
    let method = request.method().clone();
    if method == Method::OPTIONS {
        return Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .header(header::ACCESS_CONTROL_ALLOW_METHODS, "GET, HEAD, OPTIONS")
            .header(header::ACCESS_CONTROL_ALLOW_HEADERS, "*")
            .body(Body::empty())
            .unwrap_or_else(|_| Response::new(Body::empty()));
    }
    if method != Method::GET && method != Method::HEAD {
        return error(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "Only GET and HEAD are supported",
            false,
        );
    }
    let is_head = method == Method::HEAD;
    let raw_path = request.uri().path().to_owned();
    let query = request.uri().query().unwrap_or("").to_owned();
    let parts = raw_path
        .trim_start_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();

    if parts.is_empty() {
        return html(landing(), is_head);
    }
    match parts[0] {
        "manifest.json" if parts.len() == 1 => json_response(
            StatusCode::OK,
            manifest::manifest(&state.config()),
            300,
            "manifest",
            is_head,
        ),
        "health" if parts.len() == 1 => {
            let config = state.config();
            json_response(
                StatusCode::OK,
                json!({
                    "status": if config.has_tmdb() { "ok" } else { "degraded" },
                    "version": env!("CARGO_PKG_VERSION"),
                    "tmdb": config.has_tmdb(),
                    "language": config.language,
                    "cache_entries": state.cache.len(),
                    "image_entries": state.images.len()
                }),
                0,
                "health",
                is_head,
            )
        }
        "attribution" if parts.len() == 1 => html(attribution(), is_head),
        "catalog" => catalog_route(&state, &parts, is_head).await,
        "meta" => meta_route(&state, &parts, is_head).await,
        "poster-cache" if parts.len() >= 5 && parts[1] == "proxy" => {
            art_route(&state, parts[2], parts[3], parts[4], &query, is_head).await
        }
        _ => error(StatusCode::NOT_FOUND, "not_found", "Unknown route", is_head),
    }
}

async fn art_route(
    state: &State,
    class: &str,
    _kind: &str,
    _id: &str,
    query: &str,
    is_head: bool,
) -> Response {
    let params = url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect::<HashMap<String, String>>();
    let requested = params.get("url").map(String::as_str).unwrap_or("");
    let fallback = params
        .get("fallback")
        .map(String::as_str)
        .filter(|value| image::allowed_art_url(value));
    if requested.is_empty() {
        return match fallback {
            Some(location) => redirect(location),
            None => error(
                StatusCode::NOT_FOUND,
                "not_found",
                "Artwork unavailable",
                is_head,
            ),
        };
    }
    if !image::allowed_art_url(requested) {
        return match fallback {
            Some(location) => redirect(location),
            None => bad_request("Unsupported artwork host", is_head),
        };
    }
    if let Some((body, content_type)) = state.images.get(requested) {
        return image_response(body, content_type, is_head, "hit");
    }
    let Some((body, content_type)) = fetch_image(&state.http, requested).await else {
        return match fallback {
            Some(location) => redirect(location),
            None => error(
                StatusCode::BAD_GATEWAY,
                "upstream_unavailable",
                "Artwork upstream unavailable",
                is_head,
            ),
        };
    };
    if matches!(class, "poster" | "background") && body.len() < 100 {
        return match fallback {
            Some(location) => redirect(location),
            None => error(
                StatusCode::BAD_GATEWAY,
                "upstream_unavailable",
                "Artwork upstream returned a tiny image",
                is_head,
            ),
        };
    }
    state
        .images
        .put(requested.to_owned(), body.clone(), content_type.clone());
    image_response(body, content_type, is_head, "miss")
}

const IMAGE_LIMIT: usize = 16 * 1024 * 1024;

/// Fetch one allowlisted image under a hard cap; None means use the fallback.
async fn fetch_image(client: &reqwest::Client, url: &str) -> Option<(Bytes, String)> {
    let response = client.get(url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.starts_with("image/"))
        .map(str::to_owned)?;
    if response
        .content_length()
        .is_some_and(|length| length > IMAGE_LIMIT as u64)
    {
        return None;
    }
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if bytes.len() + chunk.len() > IMAGE_LIMIT {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return None;
    }
    Some((Bytes::from(bytes), content_type))
}

fn redirect(location: &str) -> Response {
    Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, location)
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .body(Body::empty())
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn image_response(body: Bytes, content_type: String, is_head: bool, cache_state: &str) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "public, max-age=604800, immutable")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header("X-AIO-Image-Cache", cache_state)
        .header("X-Content-Type-Options", "nosniff")
        .body(if is_head {
            Body::empty()
        } else {
            Body::from(body)
        })
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn bad_request(message: &str, is_head: bool) -> Response {
    error(StatusCode::BAD_REQUEST, "invalid_request", message, is_head)
}

async fn catalog_route(state: &State, parts: &[&str], is_head: bool) -> Response {
    if parts.len() < 3 || parts.len() > 4 {
        return bad_request("Invalid catalog path", is_head);
    }
    let Some(kind) = Kind::parse(&decode(parts[1])) else {
        return bad_request("Unknown catalog type", is_head);
    };
    let catalog_id = decode(strip_json(parts[2]));
    if catalog_id.is_empty() {
        return bad_request("Missing catalog id", is_head);
    }
    let extras = if parts.len() == 4 {
        parse_extras(strip_json(parts[3]))
    } else {
        HashMap::new()
    };
    match catalog::catalog(state, kind, &catalog_id, &extras).await {
        Ok((body, cache)) => json_response(StatusCode::OK, body, 1800, cache, is_head),
        Err(error) => map_error(error, is_head),
    }
}

async fn meta_route(state: &State, parts: &[&str], is_head: bool) -> Response {
    if parts.len() != 3 {
        return bad_request("Invalid meta path", is_head);
    }
    let Some(kind) = Kind::parse(&decode(parts[1])) else {
        return bad_request("Unknown meta type", is_head);
    };
    let id = decode(strip_json(parts[2]));
    match meta::meta(state, kind, &id).await {
        Ok((body, cache)) => json_response(
            StatusCode::OK,
            json!({ "meta": body }),
            7200,
            cache,
            is_head,
        ),
        Err(error) => map_error(error, is_head),
    }
}

fn strip_json(value: &str) -> &str {
    value.strip_suffix(".json").unwrap_or(value)
}

fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = (bytes[index + 1] as char).to_digit(16);
            let low = (bytes[index + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                out.push((high * 16 + low) as u8);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_extras(value: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(value.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

fn map_error(failure: anyhow::Error, is_head: bool) -> Response {
    let message = format!("{failure}");
    let lower = message.to_ascii_lowercase();
    let (status, code, public) = if lower.contains("tmdb api key rejected") {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "invalid_tmdb_key",
            "TMDB rejected the configured API key".to_owned(),
        )
    } else if lower.contains("not configured") {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "source_not_configured",
            message,
        )
    } else if lower.contains("not found") {
        (
            StatusCode::NOT_FOUND,
            "not_found",
            "Metadata not found".to_owned(),
        )
    } else if lower.contains("invalid") || lower.contains("required") {
        (StatusCode::BAD_REQUEST, "invalid_request", message)
    } else {
        eprintln!("[aio-meta] upstream error: {message}");
        (
            StatusCode::BAD_GATEWAY,
            "upstream_unavailable",
            "Upstream metadata unavailable".to_owned(),
        )
    };
    error(status, code, &public, is_head)
}

fn json_response(
    status: StatusCode,
    body: Value,
    cache_seconds: u64,
    cache_state: &str,
    is_head: bool,
) -> Response {
    let payload = if is_head {
        String::new()
    } else {
        body.to_string()
    };
    let cache = if cache_state == "stale" {
        "no-cache".to_owned()
    } else if cache_seconds == 0 {
        "no-store".to_owned()
    } else {
        format!("public, max-age={cache_seconds}")
    };
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
        .header(header::CACHE_CONTROL, cache)
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header("X-AIO-Meta-Cache", cache_state)
        .header("X-Content-Type-Options", "nosniff")
        .body(Body::from(payload))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn error(status: StatusCode, code: &str, message: &str, is_head: bool) -> Response {
    let payload = if is_head {
        String::new()
    } else {
        json!({"error": message, "code": code}).to_string()
    };
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header("X-Content-Type-Options", "nosniff")
        .body(Body::from(payload))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn html(body: String, is_head: bool) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "public, max-age=3600")
        .header(header::REFERRER_POLICY, "no-referrer")
        .header("X-Content-Type-Options", "nosniff")
        .body(Body::from(if is_head { String::new() } else { body }))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn landing() -> String {
    format!(
        "<!doctype html><html><head><meta charset=utf-8><title>AIO Metadata (Rust)</title></head><body>         <h1>AIO Metadata (Rust)</h1><p>Version {}. Local metadata addon. Install the manifest at /manifest.json.</p>         <p>Metadata from TMDB and TVMaze. GPL-3.0 port of AIOMetadata.</p></body></html>",
        env!("CARGO_PKG_VERSION")
    )
}

fn attribution() -> String {
    "<!doctype html><html><head><meta charset=utf-8><title>Attribution</title></head><body>     <h1>Attribution</h1><p>This addon is a GPL-3.0 Rust port of AIOMetadata (cedya77/aiometadata).</p>     <p>Movie and TV metadata and artwork: TMDB. This product uses the TMDB API but is not endorsed or certified by TMDB.</p>     <p>TV data: TVMaze (CC BY-SA 4.0).</p></body></html>"
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejected_key_is_an_actionable_error() {
        let response = map_error(anyhow::anyhow!("TMDB API key rejected (HTTP 401)"), false);
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn malformed_upstream_json_is_not_a_client_error() {
        let response = map_error(anyhow::anyhow!("TMDB returned malformed JSON"), false);
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn stale_bodies_are_not_cached_downstream() {
        let response = json_response(StatusCode::OK, json!({}), 1800, "stale", false);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-cache");
    }
}
