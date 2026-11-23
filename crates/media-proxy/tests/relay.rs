use rustmedia_media_proxy::MediaProxy;
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
    time::timeout,
};
use url::Url;
struct Server {
    base: String,
    task: JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn fixture<F>(handler: F) -> Server
where
    F: Fn(String) -> Vec<u8> + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let handler = Arc::new(handler);
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let handler = handler.clone();
            tokio::spawn(async move {
                let mut data = Vec::new();
                let mut buf = [0; 1024];
                loop {
                    let n = stream.read(&mut buf).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    data.extend_from_slice(&buf[..n]);
                    if data.windows(4).any(|v| v == b"\r\n\r\n") {
                        break;
                    }
                }
                let bytes = handler(String::from_utf8(data).unwrap());
                let _ = stream.write_all(&bytes).await;
            });
        }
    });
    Server { base, task }
}
fn reply(status: &str, headers: &str, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n{body}",
        body.len()
    )
    .into_bytes()
}
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap()
}
async fn prepare(proxy: &MediaProxy, url: &str) -> serde_json::Value {
    proxy.prepare(url, &json!({}), false).await.unwrap()
}
async fn raw(
    url: &str,
    method: &str,
    host: Option<&str>,
    path: Option<&str>,
    extra: &str,
) -> String {
    let u = Url::parse(url).unwrap();
    let authority = format!("127.0.0.1:{}", u.port().unwrap());
    let mut s = TcpStream::connect(&authority).await.unwrap();
    let target = format!(
        "{}{}",
        u.path(),
        u.query().map(|q| format!("?{q}")).unwrap_or_default()
    );
    s.write_all(
        format!(
            "{method} {} HTTP/1.1\r\nHost: {}\r\n{extra}\r\n",
            path.unwrap_or(&target),
            host.unwrap_or(&authority)
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    let mut out = Vec::new();
    let read = timeout(Duration::from_secs(15), s.read_to_end(&mut out))
        .await
        .unwrap();
    if let Err(e) = read {
        assert_eq!(e.kind(), std::io::ErrorKind::ConnectionReset);
    }
    String::from_utf8(out).unwrap()
}
#[tokio::test]
async fn range_head_and_status_are_preserved() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = seen.clone();
    let upstream=fixture(move|r|{captured.lock().unwrap().push(r.clone());
        if r.starts_with("HEAD "){b"HTTP/1.1 200 OK\r\nContent-Length: 999\r\nContent-Type: video/mp4\r\nAccept-Ranges: bytes\r\n\r\n".to_vec()}
        else {reply("206 Partial Content","Content-Type: video/mp4\r\nContent-Range: bytes 2-4/9\r\nAccept-Ranges: bytes\r\n","cde")}
    }).await;
    let p = MediaProxy::new();
    let v = prepare(&p, &format!("{}/movie.mkv", upstream.base)).await;
    let u = v["url"].as_str().unwrap();
    assert_eq!(p.source_size(), None, "unknown until the server answers");
    let r = client()
        .get(u)
        .header("range", "bytes=2-4")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 206);
    assert_eq!(r.headers()["content-range"], "bytes 2-4/9");
    assert_eq!(r.text().await.unwrap(), "cde");
    // The file size comes from the server's Content-Range total, not the label.
    assert_eq!(p.source_size(), Some(9));
    let r = client().head(u).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-length"], "999");
    assert!(r.bytes().await.unwrap().is_empty());
    assert!(seen.lock().unwrap()[0]
        .to_ascii_lowercase()
        .contains("range: bytes=2-4"));
}
#[tokio::test]
async fn token_host_methods_and_header_smuggling_rejected() {
    let upstream = fixture(|_| reply("200 OK", "", "\0abc")).await;
    let p = MediaProxy::new();
    let v = prepare(&p, &upstream.base).await;
    let u = v["url"].as_str().unwrap();
    assert!(raw(u, "GET", None, Some("/not-the-token/x"), "")
        .await
        .starts_with("HTTP/1.1 404"));
    assert!(raw(u, "GET", Some("localhost"), None, "")
        .await
        .starts_with("HTTP/1.1 400"));
    assert!(raw(u, "POST", None, None, "")
        .await
        .starts_with("HTTP/1.1 405"));
    assert!(raw(u, "GET", None, None, "Host: evil\r\n")
        .await
        .starts_with("HTTP/1.1 400"));
    assert!(raw(u, "GET", None, None, "Transfer-Encoding: chunked\r\n")
        .await
        .starts_with("HTTP/1.1 400"));
    let huge = format!("X-Large: {}\r\n", "a".repeat(9000));
    let rejected = raw(u, "GET", None, None, &huge).await;
    assert!(rejected.is_empty() || rejected.starts_with("HTTP/1.1 400"));
}
#[tokio::test]
async fn original_origin_only_headers_on_redirect_and_children() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let c = seen.clone();
    let child = fixture(move |r| {
        c.lock().unwrap().push(r);
        reply("200 OK", "Content-Type: video/mp2t\r\n", "\0segment")
    })
    .await;
    let b = child.base.clone();
    let origin_seen = Arc::new(Mutex::new(Vec::new()));
    let o = origin_seen.clone();
    let source = fixture(move |r| {
        o.lock().unwrap().push(r.clone());
        if r.starts_with("GET /redirect ") {
            reply("302 Found", &format!("Location: {b}/media\r\n"), "")
        } else {
            reply(
                "200 OK",
                "Content-Type: application/vnd.apple.mpegurl\r\n",
                &format!("#EXTM3U\n#EXTINF:1,\n{b}/segment.ts\n"),
            )
        }
    })
    .await;
    let p = MediaProxy::new();
    let v = p
        .prepare(
            &format!("{}/redirect", source.base),
            &json!({"Authorization":"Bearer secret","Cookie":"session=secret","X-Secret":"secret"}),
            false,
        )
        .await
        .unwrap();
    let r = client()
        .get(v["url"].as_str().unwrap())
        .header("Authorization", "from-engine")
        .send()
        .await
        .unwrap();
    assert_eq!(r.text().await.unwrap(), "\0segment");
    assert!(origin_seen.lock().unwrap()[0]
        .to_lowercase()
        .contains("authorization: bearer secret"));
    assert!(!seen.lock().unwrap()[0].contains("secret"));
    assert!(!seen.lock().unwrap()[0].contains("from-engine"));
    let v = p
        .prepare(
            &format!("{}/index.m3u8", source.base),
            &json!({"Authorization":"secret"}),
            false,
        )
        .await
        .unwrap();
    let playlist = client()
        .get(v["url"].as_str().unwrap())
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let segment = playlist.lines().find(|l| l.starts_with("http:")).unwrap();
    assert_eq!(
        client()
            .get(segment)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "\0segment"
    );
    assert!(!seen.lock().unwrap().last().unwrap().contains("secret"));
}
#[tokio::test]
async fn validates_initial_urls_and_all_configured_headers() {
    let p = MediaProxy::new();
    for url in [
        "http://example.invalid/a",
        "https://user:pass@example.invalid/a",
        "file:///etc/passwd",
        "ftp://example.invalid/a",
    ] {
        assert!(p.prepare(url, &json!({}), false).await.is_err(), "{url}");
    }
    assert!(p
        .prepare("http://example.invalid/a", &json!({}), true)
        .await
        .is_ok());
    for h in [
        json!({"Authorization":"a\r\nInjected: yes"}),
        json!({"X-Bad\n":"x"}),
        json!({"X-Test":"a\tb"}),
        json!({"Host":"evil"}),
        json!({"X-Test":3}),
        json!([]),
    ] {
        assert!(p
            .prepare("https://example.invalid/a", &h, false)
            .await
            .is_err());
    }
}
#[tokio::test]
async fn validates_every_redirect_before_contact() {
    let source = fixture(|_| {
        reply(
            "302 Found",
            "Location: http://example.invalid/forbidden\r\n",
            "",
        )
    })
    .await;
    let p = MediaProxy::new();
    let v = prepare(&p, &source.base).await;
    assert_eq!(
        client()
            .get(v["url"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        502
    );
    let credentials = fixture(|_| {
        reply(
            "302 Found",
            "Location: http://user:pass@127.0.0.1/no\r\n",
            "",
        )
    })
    .await;
    let v = prepare(&p, &credentials.base).await;
    assert_eq!(
        client()
            .get(v["url"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        502
    );
}
#[tokio::test]
async fn hls_rewrites_relative_root_absolute_and_key_uris() {
    let upstream=fixture(|r|{
        if r.starts_with("GET /dir/index.m3u8 "){reply("200 OK","Content-Type: application/vnd.apple.mpegurl\r\n","#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"../key\"\n#EXT-X-MAP:URI=\"/init.mp4\"\n#EXTINF:1,\nsegment.ts?x=1\n#EXTVLCOPT:http-referrer=secret\n")}
        else {reply("200 OK","Content-Type: video/mp2t\r\n",&format!("\0{}", r.lines().next().unwrap()))}
    }).await;
    let p = MediaProxy::new();
    let v = prepare(&p, &format!("{}/dir/index.m3u8", upstream.base)).await;
    let playlist = client()
        .get(v["url"].as_str().unwrap())
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!playlist.contains("EXTVLCOPT"));
    assert!(!playlist.contains(&upstream.base));
    let key = playlist
        .lines()
        .find(|l| l.starts_with("#EXT-X-KEY"))
        .unwrap()
        .split('"')
        .nth(1)
        .unwrap();
    assert!(key.starts_with("http://127.0.0.1:"));
    assert_eq!(
        client()
            .get(key)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "\0GET /key HTTP/1.1"
    );
    let seg = playlist.lines().find(|l| l.starts_with("http:")).unwrap();
    assert_eq!(
        client()
            .get(seg)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "\0GET /dir/segment.ts?x=1 HTTP/1.1"
    );
}
#[tokio::test]
async fn dash_inherits_baseurl_and_keeps_template_placeholders() {
    let upstream=fixture(|r|if r.starts_with("GET /manifest.mpd "){reply("200 OK","Content-Type: application/dash+xml\r\n",r#"<?xml version="1.0"?><MPD xmlns="urn:mpeg:dash:schema:mpd:2011"><BaseURL>/video/</BaseURL><Period><AdaptationSet><Representation id="v"><BaseURL>high/</BaseURL><SegmentTemplate media="$RepresentationID$/$Number%05d$.m4s" initialization="init-$RepresentationID$.mp4"/></Representation></AdaptationSet></Period></MPD>"#)}else{reply("200 OK","",&format!("\0{}", r.lines().next().unwrap()))}).await;
    let p = MediaProxy::new();
    let v = prepare(&p, &format!("{}/manifest.mpd", upstream.base)).await;
    let xml = client()
        .get(v["url"].as_str().unwrap())
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!xml.contains(&upstream.base));
    assert!(xml.contains("$RepresentationID$/$Number%05d$.m4s"));
    let media = xml
        .split("media=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .replace("$RepresentationID$", "v")
        .replace("$Number%05d$", "00001");
    assert_eq!(
        client()
            .get(media)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "\0GET /video/high/v/00001.m4s HTTP/1.1"
    );
}
#[tokio::test]
async fn unsafe_or_unsupported_manifests_fail_closed() {
    for (path, content) in [
        ("x.m3u8", "#EXTM3U\n#EXTINF:1,\nfile:///etc/passwd\n"),
        (
            "x.m3u8",
            "#EXTM3U\n#EXT-X-KEY:URI=\"http://example.invalid/key\"\n",
        ),
        ("x.m3u8", "#EXTM3U\n#EXT-X-DEFINE:NAME=\"x\",VALUE=\"y\"\n"),
        (
            "x.mpd",
            "<MPD><BaseURL>a/</BaseURL><BaseURL>b/</BaseURL></MPD>",
        ),
        (
            "x.mpd",
            "<!DOCTYPE MPD [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><MPD>&x;</MPD>",
        ),
        (
            "x.mpd",
            "<MPD><Period xlink:href=\"https://example.invalid/evil\"/></MPD>",
        ),
    ] {
        let c = content.to_string();
        let upstream = fixture(move |_| reply("200 OK", "", &c)).await;
        let p = MediaProxy::new();
        let v = prepare(&p, &format!("{}/{path}", upstream.base)).await;
        assert_eq!(
            client()
                .get(v["url"].as_str().unwrap())
                .send()
                .await
                .unwrap()
                .status(),
            502,
            "{content}"
        );
    }
}
#[tokio::test]
async fn stale_lease_stop_replacement_and_drop_cancel() {
    let upstream = fixture(|_| reply("200 OK", "", "\0ok")).await;
    let p = MediaProxy::new();
    let old = prepare(&p, &upstream.base).await;
    let current = prepare(&p, &upstream.base).await;
    p.stop(old["lease_id"].as_str()).await.unwrap();
    let u = current["url"].as_str().unwrap();
    assert_eq!(client().get(u).send().await.unwrap().status(), 200);
    assert!(client()
        .get(old["url"].as_str().unwrap())
        .send()
        .await
        .is_err());
    p.stop(current["lease_id"].as_str()).await.unwrap();
    tokio::task::yield_now().await;
    assert!(client().get(u).send().await.is_err());
    let v = prepare(&p, &upstream.base).await;
    drop(p);
    tokio::task::yield_now().await;
    assert!(client()
        .get(v["url"].as_str().unwrap())
        .send()
        .await
        .is_err());
}
#[tokio::test]
async fn progressive_stream_is_not_fully_buffered_and_stop_interrupts_body() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = format!("http://{}/movie.mp4", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut b = [0; 8192];
        let _ = s.read(&mut b).await;
        let mut body =
            b"HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: 999999\r\n\r\n"
                .to_vec();
        body.extend_from_slice(&[0; 1024]);
        s.write_all(&body).await.unwrap();
        let mut b = [0];
        let _ = s.read(&mut b).await;
    });
    let p = MediaProxy::new();
    let v = prepare(&p, &upstream).await;
    let mut r = timeout(
        Duration::from_secs(2),
        client().get(v["url"].as_str().unwrap()).send(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!r.chunk().await.unwrap().unwrap().is_empty());
    p.stop(None).await.unwrap();
    assert!(timeout(Duration::from_secs(2), r.bytes())
        .await
        .unwrap()
        .is_err());
    timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
}
#[tokio::test]
async fn occupied_header_slots_expire_and_are_bounded() {
    let upstream = fixture(|_| reply("200 OK", "", "\0ok")).await;
    let p = MediaProxy::new();
    let v = prepare(&p, &upstream.base).await;
    let u = v["url"].as_str().unwrap();
    let parsed = Url::parse(u).unwrap();
    let addr = format!("127.0.0.1:{}", parsed.port().unwrap());
    let mut streams = Vec::new();
    for _ in 0..8 {
        let mut s = TcpStream::connect(&addr).await.unwrap();
        s.write_all(b"GET /").await.unwrap();
        streams.push(s);
    }
    // Let the listener accept all eight before testing the ninth connection.
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    let mut ninth = TcpStream::connect(&addr).await.unwrap();
    let mut b = [0; 1];
    assert_eq!(
        timeout(Duration::from_secs(2), ninth.read(&mut b))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    let mut first = streams.remove(0);
    let mut out = Vec::new();
    timeout(Duration::from_secs(12), first.read_to_end(&mut out))
        .await
        .unwrap()
        .unwrap();
    assert!(out.starts_with(b"HTTP/1.1 400"));
    drop(streams);
    assert_eq!(client().get(u).send().await.unwrap().status(), 200);
}

async fn body_status(path: &str, mime: &str, body: Vec<u8>) -> reqwest::StatusCode {
    let mime = mime.to_string();
    let server = fixture(move |_| {
        let mut out = format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: {mime}\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
        out.extend_from_slice(&body);
        out
    }).await;
    let proxy = MediaProxy::new();
    let prepared = prepare(&proxy, &format!("{}{path}", server.base)).await;
    client()
        .get(prepared["url"].as_str().unwrap())
        .send()
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn unknown_or_misleading_mime_manifests_are_rewritten() {
    for (mime, body, expected) in [
        (
            "application/octet-stream",
            "#EXTM3U\n#EXTINF:1,\n/child.ts\n",
            "mpegurl",
        ),
        ("video/mp4", "#EXTM3U\n#EXTINF:1,\n/child.ts\n", "mpegurl"),
        (
            "",
            "<MPD><Period><SegmentTemplate media=\"/seg-$Number$.m4s\"/></Period></MPD>",
            "dash+xml",
        ),
        (
            "text/plain",
            "<?xml version=\"1.0\"?><MPD><BaseURL>/child.mp4</BaseURL></MPD>",
            "dash+xml",
        ),
    ] {
        let body = body.to_string();
        let mime = mime.to_string();
        let upstream =
            fixture(move |_| reply("200 OK", &format!("Content-Type: {mime}\r\n"), &body)).await;
        let proxy = MediaProxy::new();
        let p = prepare(&proxy, &format!("{}/opaque", upstream.base)).await;
        let r = client()
            .get(p["url"].as_str().unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        assert!(r.headers()["content-type"]
            .to_str()
            .unwrap()
            .contains(expected));
        let text = r.text().await.unwrap();
        assert!(text.contains("http://127.0.0.1:"));
        assert!(!text.contains(&upstream.base));
    }
}

#[tokio::test]
async fn binary_suffix_cannot_mask_invalid_utf8_manifests() {
    for header in [
        b"#EXTM3U\n#EXTINF:1,\nhttps://example.invalid/a\n".as_slice(),
        b"<MPD><BaseURL>https://example.invalid/a</BaseURL></MPD>".as_slice(),
    ] {
        let mut body = header.to_vec();
        body.extend_from_slice(&[0, 0xff, 0xfe]);
        assert_eq!(body_status("/opaque", "video/mp4", body).await, 502);
    }
}

#[tokio::test]
async fn unknown_text_html_legacy_and_wide_playlists_fail_closed() {
    for body in [
        b"https://example.invalid/plain-m3u.ts\n".to_vec(),
        b"<html><a href='https://example.invalid'>play</a></html>".to_vec(),
        b"[playlist]\nFile1=https://example.invalid/movie\n".to_vec(),
        b"<smil><body><video src='https://example.invalid'/></body></smil>".to_vec(),
        [
            vec![b' '; 1024],
            b"#EXTM3U\nhttps://example.invalid/a\n".to_vec(),
        ]
        .concat(),
        [
            vec![0xef, 0xbb, 0xbf],
            b"https://example.invalid/a\n".to_vec(),
        ]
        .concat(),
        [
            vec![0xff, 0xfe],
            "<MPD/>".encode_utf16().flat_map(u16::to_le_bytes).collect(),
        ]
        .concat(),
        "  <MPD/>"
            .encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect(),
        " #EXTM3U\nhttps://example.invalid/a"
            .chars()
            .flat_map(|c| u32::from(c).to_be_bytes())
            .collect(),
        vec![0x1f, 0x8b, 0, 0, 1, 0],
    ] {
        assert_eq!(
            body_status("/opaque", "application/octet-stream", body).await,
            502
        );
    }
    assert_eq!(
        body_status("/opaque", "text/html", vec![0, 1, 2]).await,
        502
    );
}

#[tokio::test]
async fn compressed_bodies_and_oversized_upstream_headers_are_rejected() {
    for headers in [
        "Content-Encoding: gzip\r\nContent-Type: video/mp4\r\n".to_string(),
        format!("X-Oversized: {}\r\n", "a".repeat(9000)),
        "X-Control: a\tb\r\n".to_string(),
    ] {
        let server = fixture(move |_| reply("200 OK", &headers, "\0binary")).await;
        let proxy = MediaProxy::new();
        let p = prepare(&proxy, &server.base).await;
        assert_eq!(
            client()
                .get(p["url"].as_str().unwrap())
                .send()
                .await
                .unwrap()
                .status(),
            502
        );
    }
}

#[tokio::test]
async fn redirect_url_normalization_cannot_hide_userinfo_or_authority() {
    for location in [
        "http:////@127.0.0.1/no",
        "http://@127.0.0.1/no",
        "http:/127.0.0.1/no",
        "http:\\\\127.0.0.1\\no",
        "//user@127.0.0.1/no",
        "http://127.0.0.1/no#fragment",
    ] {
        let server =
            fixture(move |_| reply("302 Found", &format!("Location: {location}\r\n"), "")).await;
        let proxy = MediaProxy::new();
        let p = prepare(&proxy, &server.base).await;
        assert_eq!(
            client()
                .get(p["url"].as_str().unwrap())
                .send()
                .await
                .unwrap()
                .status(),
            502,
            "{location}"
        );
    }
}

#[tokio::test]
async fn same_host_different_port_and_same_port_different_host_do_not_get_secrets() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = seen.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = Server {
        base: format!("http://127.0.0.1:{port}"),
        task: tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buf = vec![0; 8192];
                let n = stream.read(&mut buf).await.unwrap();
                let req = String::from_utf8(buf[..n].to_vec()).unwrap();
                captured.lock().unwrap().push(req.clone());
                let bytes = if req.starts_with("GET /source ") {
                    reply(
                        "302 Found",
                        &format!("Location: http://localhost:{port}/child\r\n"),
                        "",
                    )
                } else {
                    reply("200 OK", "", "\0binary")
                };
                stream.write_all(&bytes).await.unwrap();
            }
        }),
    };
    let proxy = MediaProxy::new();
    let p = proxy
        .prepare(
            &format!("{}/source", server.base),
            &json!({"Cookie":"private", "X-Unknown-Secret":"private"}),
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        client()
            .get(p["url"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let requests = seen.lock().unwrap();
    assert!(requests[0].contains("private"));
    assert!(!requests[1].contains("private"));
}

#[tokio::test]
async fn ambiguous_hls_and_dash_constructs_fail_closed() {
    for body in [
        "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"/a\",URI=\"https://example.invalid/b\"\n",
        "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"/a\",\n",
        "#EXTM3U\n#EXT-X-DATERANGE:ID=\"ad\",CLASS=\"com.apple.hls.interstitial\",X-ASSET-LIST=\"https://example.invalid/list.json\"\n",
        "#EXTM3U\n#EXT-X-DATERANGE:ID=\"ad\",X-ASSET-URI=\"https://example.invalid/ad\"\n",
        "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"/a\"\n",
        "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,KEYFORMAT=\"drm\",URI=\"/a\"\n",
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"{$var}\"\na\n",
        "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"http:\\\\127.0.0.1\\a\"\n",
        "<MPD><SupplementalProperty schemeIdUri=\"urn:mpeg:dash:mpd:chaining:2016\" value=\"https://example.invalid/next.mpd\"/></MPD>",
        "<MPD><EssentialProperty schemeIdUri=\"https://example.invalid/extension\" value=\"https://example.invalid/data\"/></MPD>",
        "<MPD xml:base=\"https://example.invalid/\"/>",
        "<MPD xmlns:a=\"http://www.w3.org/XML/1998/namespace\" a:base=\"https://example.invalid/\"/>",
        "<a:MPD xmlns:a=\"urn:mpeg:dash:schema:mpd:2011\"/>",
        "<MPD xmlns=\"https://example.invalid/unknown\"/>",
        "<MPD><Period xmlns=\"https://example.invalid/unknown\"/></MPD>",
        "<MPD><Period unknown=\"https://example.invalid/\"/></MPD>",
        "<MPD><BaseURL>$RepresentationID$/</BaseURL></MPD>",
        "<MPD><Period><SegmentTemplate media=\"https://$RepresentationID$.example.invalid/a\"/></Period></MPD>",
        "<MPD><Period><SegmentTemplate media=\"$Number%99999d$.m4s\"/></Period></MPD>",
        "<MPD><Period><SegmentTemplate media=\"$Unknown$.m4s\"/></Period></MPD>",
        "<MPD><Period><SegmentTemplate media=\"$Number.m4s\"/></Period></MPD>",
        "<MPD><Period><Representation id=\"../../escape\"><SegmentTemplate media=\"$RepresentationID$.m4s\"/></Representation></Period></MPD>",
        "<MPD><Period><SegmentTemplate><BaseURL>a</BaseURL></SegmentTemplate></Period></MPD>",
        "<MPD><BaseURL>&#104;ttp://example.invalid/no-consent</BaseURL></MPD>",
        "<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><MPD/>",
        "<MPD><Period></MPD>",
    ] {
        assert_eq!(body_status("/opaque", "application/octet-stream", body.as_bytes().to_vec()).await, 502, "{body}");
    }
}

#[tokio::test]
async fn decoded_xml_entities_and_percent_paths_keep_their_routing_semantics() {
    let upstream = fixture(|r| {
        if r.starts_with("GET /dir/manifest ") {
            reply("200 OK", "", "<MPD><BaseURL>/a%2Fb/</BaseURL><Period><SegmentTemplate media=\"../c%252Fd/$Number$.m4s?one=1&amp;two=2\"/></Period></MPD>")
        } else { reply("200 OK", "", &format!("\0{}", r.lines().next().unwrap())) }
    }).await;
    let proxy = MediaProxy::new();
    let p = prepare(&proxy, &format!("{}/dir/manifest", upstream.base)).await;
    let xml = client()
        .get(p["url"].as_str().unwrap())
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let url = xml
        .split("media=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .replace("$Number$", "1")
        .replace("&amp;", "&");
    assert_eq!(
        client()
            .get(url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "\0GET /c%252Fd/1.m4s?one=1&two=2 HTTP/1.1"
    );
}

#[tokio::test]
async fn manifests_are_bounded_on_input_expansion_nesting_and_nodes() {
    for body in [
        format!("#EXTM3U\n{}", " ".repeat(4 * 1024 * 1024)),
        format!("#EXTM3U\n{}", "a\n".repeat(160_000)),
        format!(
            "<MPD>{}{}</MPD>",
            "<Period>".repeat(65),
            "</Period>".repeat(65)
        ),
        format!("<MPD>{}</MPD>", "<Period/>".repeat(65_536)),
    ] {
        assert_eq!(
            body_status("/opaque", "application/octet-stream", body.into_bytes()).await,
            502
        );
    }
}

#[tokio::test]
async fn head_never_forwards_an_upstream_body_or_manifest_size() {
    let upstream = fixture(|r| {
        assert!(r.starts_with("HEAD "));
        reply(
            "200 OK",
            "Content-Type: application/dash+xml\r\n",
            "<MPD><BaseURL>https://example.invalid/secret</BaseURL></MPD>",
        )
    })
    .await;
    let proxy = MediaProxy::new();
    let p = prepare(&proxy, &format!("{}/manifest.mpd", upstream.base)).await;
    let response = raw(p["url"].as_str().unwrap(), "HEAD", None, None, "").await;
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(!response.to_ascii_lowercase().contains("content-length"));
    assert!(response.ends_with("\r\n\r\n"));
}

#[tokio::test]
async fn invalid_preparation_preserves_existing_lease() {
    let upstream = fixture(|_| reply("200 OK", "", "\0ok")).await;
    let proxy = MediaProxy::new();
    let p = prepare(&proxy, &upstream.base).await;
    assert!(proxy
        .prepare("http://example.invalid/no", &json!({}), false)
        .await
        .is_err());
    assert_eq!(
        client()
            .get(p["url"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
}

#[tokio::test]
async fn range_errors_keep_status_and_never_expose_error_playlists() {
    let server = fixture(|_| {
        reply(
            "416 Range Not Satisfiable",
            "Content-Range: bytes */99\r\nContent-Type: text/html\r\n",
            "<html>error</html>",
        )
    })
    .await;
    let proxy = MediaProxy::new();
    let p = prepare(&proxy, &format!("{}/movie.mp4", server.base)).await;
    let r = client()
        .get(p["url"].as_str().unwrap())
        .header("Range", "bytes=100-")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 416);
    assert_eq!(r.headers()["content-range"], "bytes */99");
    assert!(r.bytes().await.unwrap().is_empty());
}

#[tokio::test]
async fn malformed_single_ranges_and_multipart_ranges_fail_closed() {
    for headers in [
        "Content-Type: video/mp4\r\nContent-Range: bytes 4-2/9\r\n",
        "Content-Type: video/mp4\r\nContent-Range: bytes 2-4/4\r\n",
        "Content-Type: video/mp4\r\nContent-Range: bytes 2-5/9\r\n",
        "Content-Type: multipart/byteranges; boundary=abc\r\n",
    ] {
        let server = fixture(move |_| reply("206 Partial Content", headers, "cde")).await;
        let proxy = MediaProxy::new();
        let p = prepare(&proxy, &server.base).await;
        assert_eq!(
            client()
                .get(p["url"].as_str().unwrap())
                .send()
                .await
                .unwrap()
                .status(),
            502
        );
    }
}

#[tokio::test]
async fn cancellation_closes_pending_upstream_response_headers() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/movie", listener.local_addr().unwrap());
    let (tx, rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0; 8192];
        assert!(stream.read(&mut buf).await.unwrap() > 0);
        tx.send(()).unwrap();
        assert_eq!(stream.read(&mut buf).await.unwrap(), 0);
    });
    let proxy = MediaProxy::new();
    let p = prepare(&proxy, &url).await;
    let url = p["url"].as_str().unwrap().to_string();
    let request = tokio::spawn(async move { client().get(url).send().await });
    timeout(Duration::from_secs(2), rx).await.unwrap().unwrap();
    proxy.stop(None).await.unwrap();
    assert!(timeout(Duration::from_secs(2), request)
        .await
        .unwrap()
        .unwrap()
        .is_err());
    timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn stalled_progressive_body_expires_on_idle_not_total_movie_time() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/movie.mp4", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0; 8192];
        let _ = stream.read(&mut buf).await.unwrap();
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: 9999\r\n\r\n\0start012345678901234567890123456789").await.unwrap();
        assert_eq!(stream.read(&mut buf).await.unwrap(), 0);
    });
    // The production idle allowance is deliberately generous (60s CDN misses on
    // 4K remuxes); the test injects a short deadline to observe the SAME idle
    // expiry behavior in real time instead of waiting out the production value.
    let proxy = MediaProxy::with_stream_timeout(Duration::from_secs(2));
    let p = prepare(&proxy, &url).await;
    let mut r = client()
        .get(p["url"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.chunk().await.unwrap().unwrap(),
        b"\0start012345678901234567890123456789".as_slice()
    );
    // Idle expiry, not a whole-movie deadline: the body fails after the 2s idle
    // window even though total elapsed time stays far below the outer budget.
    assert!(timeout(Duration::from_secs(12), r.bytes())
        .await
        .unwrap()
        .is_err());
    timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn redirect_limits_and_relative_normalization_preserve_head_and_range() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = seen.clone();
    let upstream = fixture(move |r| {
        captured.lock().unwrap().push(r.clone());
        if r.starts_with("HEAD /a/start ") {
            reply("302 Found", "Location: ../target?x=1\r\n", "")
        } else if r.starts_with("HEAD /target?x=1 ") {
            reply("200 OK", "Content-Type: video/mp4\r\n", "body")
        } else {
            reply("302 Found", "Location: /loop\r\n", "")
        }
    })
    .await;
    let proxy = MediaProxy::new();
    let p = prepare(&proxy, &format!("{}/a/start", upstream.base)).await;
    let r = client()
        .head(p["url"].as_str().unwrap())
        .header("Range", "bytes=2-4")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.bytes().await.unwrap().is_empty());
    assert!(seen.lock().unwrap()[1]
        .to_ascii_lowercase()
        .contains("range: bytes=2-4"));
    seen.lock().unwrap().clear();
    let p = prepare(&proxy, &format!("{}/loop", upstream.base)).await;
    assert_eq!(
        client()
            .get(p["url"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        502
    );
    assert_eq!(seen.lock().unwrap().len(), 9);
}

#[tokio::test]
async fn fragmented_wide_whitespace_cannot_bypass_prefix_detection() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/opaque", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0; 8192];
        let _ = stream.read(&mut buf).await.unwrap();
        let bytes = format!(
            "{}<MPD><BaseURL>http://example.invalid/not-allowed</BaseURL></MPD>",
            " ".repeat(32)
        )
        .encode_utf16()
        .flat_map(u16::to_be_bytes)
        .collect::<Vec<_>>();
        stream
            .write_all(
                format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", bytes.len()).as_bytes(),
            )
            .await
            .unwrap();
        stream.write_all(&bytes[..1]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        let _ = stream.write_all(&bytes[1..64]).await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        let _ = stream.write_all(&bytes[64..]).await;
    });
    let proxy = MediaProxy::new();
    let p = prepare(&proxy, &url).await;
    assert_eq!(
        client()
            .get(p["url"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        502
    );
    task.await.unwrap();
}

#[tokio::test]
async fn same_origin_children_keep_configured_secrets_not_engine_secrets() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = seen.clone();
    let upstream = fixture(move |r| {
        captured.lock().unwrap().push(r.clone());
        if r.starts_with("GET /start ") {
            reply(
                "302 Found",
                "Location: /dir/main\r\nSet-Cookie: remote=not-a-jar\r\n",
                "",
            )
        } else if r.starts_with("GET /dir/main ") {
            reply(
                "200 OK",
                "Set-Cookie: remote=not-a-jar\r\n",
                "#EXTM3U\n#EXTINF:1,\nsegment.ts\n",
            )
        } else {
            reply("200 OK", "Set-Cookie: remote=not-a-jar\r\n", "\0binary")
        }
    })
    .await;
    let proxy = MediaProxy::new();
    let p = proxy
        .prepare(
            &format!("{}/start", upstream.base),
            &json!({"Cookie":"configured-secret", "X-Secret":"retained"}),
            false,
        )
        .await
        .unwrap();
    let r = client()
        .get(p["url"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert!(!r.headers().contains_key("set-cookie"));
    assert!(!r.headers().contains_key("location"));
    let playlist = r.text().await.unwrap();
    let child = playlist.lines().find(|l| l.starts_with("http:")).unwrap();
    assert_eq!(
        client()
            .get(child)
            .header("Cookie", "engine-secret")
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap(),
        b"\0binary".as_slice()
    );
    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 3);
    for r in requests.iter() {
        assert!(r.contains("configured-secret"));
        assert!(r.contains("retained"));
        assert!(!r.contains("engine-secret"));
        assert!(!r.contains("not-a-jar"));
    }
}

#[tokio::test]
async fn healthy_progress_continues_beyond_ten_seconds() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/movie.mp4", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0; 8192];
        let _ = stream.read(&mut buf).await.unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: 160\r\n\r\n")
            .await
            .unwrap();
        for index in 0..5 {
            if index > 0 {
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
            stream.write_all(&[0; 32]).await.unwrap();
        }
    });
    let proxy = MediaProxy::new();
    let p = prepare(&proxy, &url).await;
    let bytes = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap()
        .get(p["url"].as_str().unwrap())
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(bytes.len(), 160);
    task.await.unwrap();
}

#[tokio::test]
async fn chunked_ranges_gain_correct_length_and_truncation_is_detected() {
    for (body, declared, succeeds) in [("cde", 3, true), ("cde", 4, false)] {
        let upstream = fixture(move |_| {
            format!("HTTP/1.1 206 Partial Content\r\nContent-Type: video/mp4\r\nContent-Range: bytes 2-{}/99\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n{body}\r\n0\r\n\r\n", 2+declared-1).into_bytes()
        }).await;
        let proxy = MediaProxy::new();
        let p = prepare(&proxy, &upstream.base).await;
        let r = client()
            .get(p["url"].as_str().unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 206);
        assert_eq!(r.headers()["content-length"], declared.to_string());
        let body = r.bytes().await;
        assert_eq!(body.is_ok(), succeeds);
    }
}

#[tokio::test]
async fn hls_webvtt_subtitles_and_printable_aes_keys_are_relayed() {
    // Standard EXT-X-MEDIA subtitle rendition segment: plain text, not a playlist.
    let vtt = "WEBVTT\n\n00:00.000 --> 00:01.000\nHello\n";
    let upstream = fixture(move |_| reply("200 OK", "Content-Type: text/vtt\r\n", vtt)).await;
    let proxy = MediaProxy::new();
    let prepared = prepare(&proxy, &format!("{}/subs/en.vtt", upstream.base)).await;
    let response = client()
        .get(prepared["url"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "text/vtt");
    assert_eq!(response.text().await.unwrap(), vtt);
    // The same body must not be rejected when the origin mislabels it.
    let upstream =
        fixture(move |_| reply("200 OK", "Content-Type: application/octet-stream\r\n", vtt)).await;
    let prepared = prepare(&proxy, &format!("{}/opaque", upstream.base)).await;
    let response = client()
        .get(prepared["url"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    // Printable 16-byte AES-128 key, served with the common octet-stream MIME.
    let key: &'static [u8] = b"0123456789abcdef";
    let upstream = fixture(move |_| {
        let mut out = b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/octet-stream\r\nContent-Length: 16\r\n\r\n".to_vec();
        out.extend_from_slice(key);
        out
    })
    .await;
    let prepared = prepare(&proxy, &format!("{}/key", upstream.base)).await;
    let response = client()
        .get(prepared["url"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.bytes().await.unwrap().as_ref(), key);
    // A binary key whose first byte happens to be '<' is still key material, not
    // an XML subtitle/manifest candidate.
    let key: &'static [u8] = b"<0123456789abcde";
    let upstream = fixture(move |_| {
        let mut out = b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/octet-stream\r\nContent-Length: 16\r\n\r\n".to_vec();
        out.extend_from_slice(key);
        out
    })
    .await;
    let prepared = prepare(&proxy, &format!("{}/key2", upstream.base)).await;
    let response = client()
        .get(prepared["url"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.bytes().await.unwrap().as_ref(), key);
    // The same 16 printable bytes as a partial fragment are still not a whole
    // key and must stay rejected.
    let upstream = fixture(move |_| {
        let mut out = b"HTTP/1.1 206 Partial Content\r\nConnection: close\r\nContent-Type: application/octet-stream\r\nContent-Range: bytes 0-15/64\r\nContent-Length: 16\r\n\r\n".to_vec();
        out.extend_from_slice(key);
        out
    })
    .await;
    let prepared = prepare(&proxy, &format!("{}/key3", upstream.base)).await;
    let response = client()
        .get(prepared["url"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 502);
}

#[tokio::test]
async fn subtitle_mime_or_extension_never_exempts_a_manifest_body() {
    // A redirect can end on a WebVTT-labeled URL while the body is actually an
    // HLS playlist (optionally hidden behind an ID3 tag the native demuxer strips).
    let hidden = [
        b"ID3\x04\x00\x00\x00\x00\x00\x00".as_slice(),
        b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nhttps://example.invalid/child.m3u8\n".as_slice(),
    ]
    .concat();
    assert_eq!(body_status("/caption.vtt", "text/vtt", hidden).await, 502);
    // The same playlist without an ID3 mask is still recognized as HLS and
    // rewritten to loopback, so it is never relayed verbatim.
    let plain =
        b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nhttps://example.invalid/child.m3u8\n".to_vec();
    let plain_upstream = fixture(move |_| {
        let mut out = format!(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: text/vtt\r\nContent-Length: {}\r\n\r\n",
            plain.len()
        )
        .into_bytes();
        out.extend_from_slice(&plain);
        out
    })
    .await;
    let proxy = MediaProxy::new();
    let prepared = prepare(&proxy, &format!("{}/caption.vtt", plain_upstream.base)).await;
    let response = client()
        .get(prepared["url"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = response.text().await.unwrap();
    assert!(
        body.contains("http://127.0.0.1:"),
        "playlist must be rewritten before relay: {body}"
    );
    // Any other non-subtitle body behind a subtitle label fails closed too.
    for body in [
        b"<html><a href='https://example.invalid'>x</a></html>".to_vec(),
        b"https://example.invalid/plain.ts\n".to_vec(),
        vec![0, 1, 2, 3],
    ] {
        assert_eq!(body_status("/caption.vtt", "text/vtt", body).await, 502);
    }
    // Real subtitle bodies behind the same labels are still relayed unchanged.
    assert_eq!(
        body_status(
            "/caption.vtt",
            "text/vtt",
            b"WEBVTT\n\n00:00.000 --> 00:01.000\nHello\n".to_vec()
        )
        .await,
        200
    );
    assert_eq!(
        body_status(
            "/captions.srt",
            "application/x-subrip",
            b"1\n00:00:00,000 --> 00:01:00,000\nHello\n".to_vec()
        )
        .await,
        200
    );
    // TTML is accepted only as a text-only document.
    assert_eq!(
        body_status(
            "/captions.ttml",
            "application/ttml+xml",
            b"<?xml version=\"1.0\"?><tt xmlns=\"http://www.w3.org/ns/ttml\"><body><p>Hi</p></body></tt>".to_vec()
        )
        .await,
        200
    );
    assert_eq!(
        body_status(
            "/captions.ttml",
            "application/ttml+xml",
            b"<?xml version=\"1.0\"?><tt><body><image xlink:href=\"https://example.invalid/p.png\"/></body></tt>".to_vec()
        )
        .await,
        502
    );
    // Declared as a subtitle but carrying a playlist: rejected, not relayed.
    assert_eq!(
        body_status(
            "/captions.srt",
            "application/x-subrip",
            b"1\n#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nhttps://example.invalid/a.m3u8\n".to_vec(),
        )
        .await,
        502
    );
    // A DASH manifest behind a subtitle extension is classified and rewritten.
    let proxy = MediaProxy::new();
    let upstream = fixture(move |_| {
        reply(
            "200 OK",
            "Content-Type: text/vtt\r\n",
            "<?xml version=\"1.0\"?><MPD><BaseURL>https://example.invalid/</BaseURL></MPD>",
        )
    })
    .await;
    let prepared = prepare(&proxy, &format!("{}/caption.vtt", upstream.base)).await;
    let response = client()
        .get(prepared["url"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = response.text().await.unwrap();
    assert!(
        body.contains("http://127.0.0.1:"),
        "manifest must be rewritten before relay: {body}"
    );
}

#[tokio::test]
async fn opaque_binary_aes_keys_are_not_mistaken_for_unicode_plaintext() {
    assert_eq!(
        body_status("/key", "application/octet-stream", vec![0xff; 16]).await,
        200
    );
    assert_eq!(
        body_status("/key", "application/octet-stream", vec![0; 16]).await,
        200
    );
    assert_eq!(body_status("/key", "text/html", vec![0xff; 16]).await, 502);
}

fn subtitle_reply(mime: &str, body: &[u8]) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: {mime}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    ).into_bytes();
    response.extend_from_slice(body);
    response
}

async fn subtitle_result(path: &str, mime: &str, body: Vec<u8>) -> (u16, Vec<u8>) {
    let mime = mime.to_string();
    let upstream = fixture(move |_| subtitle_reply(&mime, &body)).await;
    let proxy = MediaProxy::new();
    let prepared = prepare(&proxy, &format!("{}{path}", upstream.base)).await;
    let response = client()
        .get(prepared["url"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.bytes().await.unwrap().to_vec())
}

async fn assert_subtitle_rejected(path: &str, mime: &str, body: Vec<u8>) {
    let description = String::from_utf8_lossy(&body[..body.len().min(128)]).to_string();
    let (status, exposed) = subtitle_result(path, mime, body).await;
    assert_eq!(status, 502, "{path}, {mime}: {description}");
    assert!(
        exposed.is_empty(),
        "rejection must not expose unvalidated bytes"
    );
}

/// Use real separately written body chunks, including split signatures/codepoints.
async fn split_subtitle_fixture(
    mime: &str,
    body: Vec<u8>,
    split_at: Vec<usize>,
    chunked: bool,
) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mime = mime.to_string();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buf = [0; 1024];
        while !request.windows(4).any(|v| v == b"\r\n\r\n") {
            let n = stream.read(&mut buf).await.unwrap();
            assert!(n > 0);
            request.extend_from_slice(&buf[..n]);
        }
        let framing = if chunked {
            "Transfer-Encoding: chunked\r\n".to_string()
        } else {
            format!("Content-Length: {}\r\n", body.len())
        };
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: {mime}\r\n{framing}\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut start = 0;
        for end in split_at.into_iter().chain(std::iter::once(body.len())) {
            assert!(start < end && end <= body.len());
            if chunked
                && stream
                    .write_all(format!("{:x}\r\n", end - start).as_bytes())
                    .await
                    .is_err()
            {
                return;
            }
            if stream.write_all(&body[start..end]).await.is_err() {
                return; // Early rejection is allowed (e.g. an advertised size limit).
            }
            if chunked && stream.write_all(b"\r\n").await.is_err() {
                return;
            }
            start = end;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        if chunked {
            let _ = stream.write_all(b"0\r\n\r\n").await;
        }
    });
    Server { base, task }
}

#[tokio::test]
async fn id3_hls_ttml_comment_redirect_attack_never_reaches_native_bytes() {
    // The <tt/> comment completes before byte 32: every prefix considered by
    // the old binary-prefix path could incorrectly approve this as a subtitle.
    let attack = [
        b"ID3\x04\x00\x00\x00\x00\x00\x00".as_slice(),
        b"#EXTM3U\n# <tt/>\n#EXT-X-STREAM-INF:BANDWIDTH=1\nhttp://example.invalid/no-consent-child.m3u8\n".as_slice(),
    ].concat();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = seen.clone();
    let upstream = fixture(move |request| {
        captured.lock().unwrap().push(request.clone());
        if request.starts_with("GET /original.m3u8 ") {
            reply("302 Found", "Location: /caption.vtt\r\n", "")
        } else {
            assert!(request.starts_with("GET /caption.vtt "));
            subtitle_reply("text/vtt", &attack)
        }
    })
    .await;
    let proxy = MediaProxy::new();
    let prepared = prepare(&proxy, &format!("{}/original.m3u8", upstream.base)).await;
    let response = client()
        .get(prepared["url"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 502);
    assert!(
        response.bytes().await.unwrap().is_empty(),
        "no original playlist bytes may escape"
    );
    assert_eq!(
        seen.lock().unwrap().len(),
        2,
        "only the original and redirect may be contacted"
    );
}

#[tokio::test]
async fn arbitrary_prefixes_cannot_be_skipped_to_find_a_ttml_root() {
    for prefix in [
        b"arbitrary garbage ".as_slice(),
        b"ID3\x04\x00\x00\x00\x00\x00\x00".as_slice(),
        b"\0".as_slice(),
        b"  \nnot XML ".as_slice(),
        b"<?ignored?>".as_slice(),
        b"<!--not a permitted prolog-->".as_slice(),
        b"\xef\xbb\xbf\xef\xbb\xbf".as_slice(),
    ] {
        let body = [prefix, b"<tt/>".as_slice()].concat();
        assert_subtitle_rejected("/caption.ttml", "application/ttml+xml", body.clone()).await;
        assert_subtitle_rejected("/caption.vtt", "application/octet-stream", body.clone()).await;
        assert_subtitle_rejected("/caption.srt", "video/mp4", body).await;
    }
}

#[tokio::test]
async fn malformed_ttml_documents_and_trailing_payloads_fail_closed() {
    for body in [
        "<tt>",
        "<tt><body><p>Hi</body></tt>",
        "<tt/><tt/>",
        "<tt/><MPD/>",
        "<tt/>trailing payload",
        "<tt/>\n#EXTM3U\nhttp://example.invalid/child\n",
        "<tt/> <!--trailing comment is not part of our text-only document-->",
        "prefix<tt/>",
        "<?xml?><tt/>",
        " <?xml version='1.0'?><tt/>",
        "<?xml version='1.1'?><tt/>",
        "<?xml version='1.0' encoding='ISO-8859-1'?><tt/>",
        "<?xml version='1.0' encoding='UTF-8' encoding='UTF-8'?><tt/>",
        "<?xml version='1.0' standalone='yes' encoding='UTF-8'?><tt/>",
        "<?xml version='1.0' unknown='x'?><tt/>",
        "<?xml version='1.0'?><?xml version='1.0'?><tt/>",
        "<tt><?xml version='1.0'?></tt>",
        "<tt><body><p>&undeclared;</p></body></tt>",
        "<tt><body><p>&#0;</p></body></tt>",
        "<tt><body><p>&#xFFFF;</p></body></tt>",
        "<tt><body><p>]]></p></body></tt>",
        "<tt><body><p><!--bad--comment--></p></body></tt>",
        "<tt xml:lang='en' xml:lang='fr'/>",
        "<tt xml:lang='<invalid'/>",
        "<tt><head><body/></head></tt>",
        "<tt><body><script>ignored?</script></body></tt>",
        "<!DOCTYPE tt><tt/>",
        "<!DOCTYPE tt SYSTEM 'http://example.invalid/external.dtd'><tt/>",
        "<!DOCTYPE tt [<!ENTITY ex SYSTEM 'file:///etc/passwd'>]><tt><body><p>&ex;</p></body></tt>",
        "<tt><?xml-stylesheet href='http://example.invalid/style.css'?></tt>",
    ] {
        assert_subtitle_rejected(
            "/caption.ttml",
            "application/ttml+xml",
            body.as_bytes().to_vec(),
        )
        .await;
    }
    let mut binary_suffix = b"<tt><body><p>Safe text</p></body></tt>".to_vec();
    binary_suffix.extend_from_slice(&[0xff, 0, 0xfe]);
    assert_subtitle_rejected("/caption.ttml", "application/ttml+xml", binary_suffix).await;
}

#[tokio::test]
async fn ttml_external_features_after_the_sniff_window_and_namespace_aliases_are_rejected() {
    let padding = "x".repeat(2048);
    let prefix = format!("<tt><body><p>{padding}</p>");
    for suffix in [
        "<image src='http://example.invalid/image.png'/></body></tt>",
        "<div src='http://example.invalid/reference'/></body></tt>",
        "<div href='http://example.invalid/reference'/></body></tt>",
        "<div xlink:href='http://example.invalid/reference'/></body></tt>",
        "<div xml:base='http://example.invalid/'/></body></tt>",
        "<div xmlns:r='http://www.w3.org/1999/xlink' r:href='http://example.invalid/reference'/></body></tt>",
        "<div xmlns:i='http://www.w3.org/ns/ttml'><i:image src='http://example.invalid/image.png'/></div></body></tt>",
        "<div xmlns='http://example.invalid/extension'/></body></tt>",
        "<div xmlns=''/></body></tt>",
        "<!DOCTYPE tt SYSTEM 'http://example.invalid/external.dtd'></body></tt>",
        "<?xml-stylesheet href='http://example.invalid/style.css'?></body></tt>",
    ] {
        assert_subtitle_rejected("/caption.ttml", "application/ttml+xml", format!("{prefix}{suffix}").into_bytes()).await;
    }
    for body in [
        format!("<tt xmlns:r='http://www.w3.org/1999/xlink'><body><p>{padding}</p><div r:href='http://example.invalid/reference'/></body></tt>"),
        format!("<tt xmlns:i='http://www.w3.org/ns/ttml'><body><p>{padding}</p><i:image src='http://example.invalid/image.png'/></body></tt>"),
        format!("<tt xmlns:r='http://www.w3.org/ns/ttml#styling'><body><p>{padding}</p><div r:backgroundImage='http://example.invalid/image.png'/></body></tt>"),
        format!("<tt xmlns:tts='http://www.w3.org/ns/ttml#styling'><body><p>{padding}</p><div tts:backgroundImage='http://example.invalid/image.png'/></body></tt>"),
        format!("<tt xmlns:ttp='http://www.w3.org/ns/ttml#parameter'><body><p>{padding}</p><div ttp:profile='http://example.invalid/profile'/></body></tt>"),
        "<t:tt xmlns:t='http://www.w3.org/ns/ttml'><t:body/></t:tt>".to_string(),
        "<tt xmlns:xml='http://www.w3.org/XML/1998/namespace' xml:base='http://example.invalid/'/>".to_string(),
        "<tt xmlns:tts='http://example.invalid/extension'><body/></tt>".to_string(),
        format!("{}<!DOCTYPE tt><tt/>", " ".repeat(1024)),
    ] {
        assert_subtitle_rejected("/caption.vtt", "text/vtt", body.into_bytes()).await;
    }
}

#[tokio::test]
async fn webvtt_and_subrip_validate_every_cue_and_the_entire_body() {
    for body in [
        "WEBVTTjunk\n\n00:00.000 --> 00:01.000\nHello\n".to_string(),
        "WEBVTT\nmissing separator\n".to_string(),
        "WEBVTT\n\n00:00.000 --> 00:99.000\nHello\n".to_string(),
        "WEBVTT\n\n00:00.000 --> 00:01.000\n<img src='http://example.invalid/image.png'>\n"
            .to_string(),
        "WEBVTT\n\nSTYLE\n::cue { background-image: url(http://example.invalid/image.png); }\n"
            .to_string(),
        format!(
            "WEBVTT\n\n00:00.000 --> 00:01.000\n{}\n\n#EXTM3U\nhttp://example.invalid/child\n",
            "x".repeat(1024)
        ),
        format!(
            "WEBVTT\n\n00:00.000 --> 00:01.000\n{}\0\n",
            "x".repeat(1024)
        ),
    ] {
        assert_subtitle_rejected("/opaque", "application/octet-stream", body.into_bytes()).await;
    }
    for body in [
        "1\n00:00:00,000garbage --> 00:01:00,000\nHello\n".to_string(),
        "1\n00:00:00,000 --> 00:01:00,000suffix\nHello\n".to_string(),
        "1\n00:01:00,000 --> 00:00:00,000\nHello\n".to_string(),
        format!(
            "1\n00:00:00,000 --> 00:01:00,000\n{}\n\n#EXTM3U\nhttp://example.invalid/child\n",
            "x".repeat(1024)
        ),
        format!("1\n00:00:00,000 --> 00:01:00,000\n{}\0\n", "x".repeat(1024)),
        "1\n00:00:00,000 --> 00:01:00,000\n<img src='http://example.invalid/image.png'>\n"
            .to_string(),
    ] {
        assert_subtitle_rejected("/caption.srt", "application/x-subrip", body.into_bytes()).await;
    }
    for (path, mime, header) in [
        (
            "/opaque",
            "application/octet-stream",
            "WEBVTT\n\n00:00.000 --> 00:01.000\n",
        ),
        (
            "/caption.srt",
            "application/x-subrip",
            "1\n00:00:00,000 --> 00:01:00,000\n",
        ),
    ] {
        let mut body = format!("{header}{}", "x".repeat(1024)).into_bytes();
        body.extend_from_slice(&[0xff, 0xfe]);
        assert_subtitle_rejected(path, mime, body).await;
    }
}

#[tokio::test]
async fn validated_text_subtitles_remain_usable_and_ttml_is_canonicalized() {
    for (path, mime, body) in [
        ("/caption.vtt", "text/vtt", "\u{feff}WEBVTT captions\r\n\r\nNOTE inert comment\r\nhttp://example.invalid/is-only-text\r\n\r\ncue-id\r\n00:00.000 --> 00:01.000 align:center position:50%\r\n<v Alice><b>Hello</b> &amp; <c.green>world</c></v>\r\n\r\n00:01.000 --> 00:02.000\r\nGoodbye\r\n"),
        ("/opaque", "application/octet-stream", "WEBVTT\n\n00:00.000 --> 00:01.000\nHello ☃\n"),
        ("/caption.srt", "application/x-subrip", "\u{feff}1\r\n00:00:00,000 --> 00:00:01,000\r\n<i>Hello</i> <font color='#ffcc00'>world</font>\r\n\r\n2\r\n00:00:01.000 --> 00:00:02.000\r\nGoodbye\r\n"),
    ] {
        let (status, relayed) = subtitle_result(path, mime, body.as_bytes().to_vec()).await;
        assert_eq!(status, 200, "{path}");
        assert_eq!(relayed, body.as_bytes());
    }
    let ttml = "\u{feff}<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n<tt xmlns='http://www.w3.org/ns/ttml' xmlns:tts='http://www.w3.org/ns/ttml#styling' xmlns:ttp='http://www.w3.org/ns/ttml#parameter' xml:lang='en' ttp:frameRate='25'><head><styling><style xml:id='white' tts:color='white'/></styling><layout><region xml:id='bottom' tts:origin='10% 80%' tts:extent='80% 20%'/></layout></head><body region='bottom'><div><p begin='00:00:00.000' end='00:00:01.000' style='white'>Hello &amp; <span tts:fontWeight='bold'>world</span><br/><![CDATA[<safe>]]><!-- stripped --></p></div></body></tt>\n";
    let (status, relayed) = subtitle_result(
        "/caption.ttml",
        "application/ttml+xml",
        ttml.as_bytes().to_vec(),
    )
    .await;
    assert_eq!(status, 200);
    let relayed = String::from_utf8(relayed).unwrap();
    assert!(relayed.starts_with("<tt "));
    assert!(relayed.contains("Hello &amp; "));
    assert!(relayed.contains("&lt;safe&gt;"));
    assert!(
        !relayed.contains("<?xml") && !relayed.contains("<!--") && !relayed.contains("<![CDATA[")
    );
    assert_eq!(
        subtitle_result(
            "/opaque",
            "application/octet-stream",
            b" \n<tt><body><p>Hi</p></body></tt> \n".to_vec()
        )
        .await
        .0,
        200
    );
}

#[tokio::test]
async fn subtitle_size_nesting_event_and_output_limits_are_enforced() {
    const LIMIT: usize = 4 * 1024 * 1024;
    let header = "WEBVTT\n\n00:00.000 --> 00:01.000\n";
    let at_limit = format!("{header}{}\n", "x".repeat(LIMIT - header.len() - 1));
    let (status, body) =
        subtitle_result("/opaque", "application/octet-stream", at_limit.into_bytes()).await;
    assert_eq!(status, 200);
    assert_eq!(body.len(), LIMIT);
    for (path, mime, body) in [
        (
            "/opaque",
            "application/octet-stream",
            format!("{header}{}\n", "x".repeat(LIMIT)),
        ),
        (
            "/caption.srt",
            "application/x-subrip",
            format!("1\n00:00:00,000 --> 00:00:01,000\n{}\n", "x".repeat(LIMIT)),
        ),
        (
            "/caption.ttml",
            "application/ttml+xml",
            format!("<tt><body><p>{}</p></body></tt>", "x".repeat(LIMIT)),
        ),
        (
            "/caption.ttml",
            "application/ttml+xml",
            format!(
                "<tt><body>{}{}</body></tt>",
                "<div>".repeat(65),
                "</div>".repeat(65)
            ),
        ),
        (
            "/caption.ttml",
            "application/ttml+xml",
            format!("<tt><body>{}</body></tt>", "<p/>".repeat(65_536)),
        ),
        (
            "/caption.ttml",
            "application/ttml+xml",
            format!(
                "<tt><body><p><![CDATA[{}]]></p></body></tt>",
                "&".repeat(LIMIT / 4)
            ),
        ),
    ] {
        assert_subtitle_rejected(path, mime, body.into_bytes()).await;
    }
    // No Content-Length hint: the streaming accumulator must enforce the cap too.
    let oversized = format!("{header}{}\n", "x".repeat(LIMIT)).into_bytes();
    let upstream = split_subtitle_fixture("text/vtt", oversized, vec![1, 32, 512], true).await;
    let proxy = MediaProxy::new();
    let prepared = prepare(&proxy, &format!("{}/caption.vtt", upstream.base)).await;
    let response = client()
        .get(prepared["url"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 502);
    assert!(response.bytes().await.unwrap().is_empty());
}

#[tokio::test]
async fn split_chunk_subtitles_are_validated_to_eof_not_just_the_first_chunk() {
    let unsafe_ttml = format!(
        "<tt><body><p>{}</p><image src='http://example.invalid/image.png'/></body></tt>",
        "x".repeat(2048)
    );
    for chunked in [false, true] {
        let upstream = split_subtitle_fixture(
            "application/ttml+xml",
            unsafe_ttml.as_bytes().to_vec(),
            vec![1, 3, 32, 512, 1024],
            chunked,
        )
        .await;
        let proxy = MediaProxy::new();
        let prepared = prepare(&proxy, &format!("{}/caption.ttml", upstream.base)).await;
        let response = client()
            .get(prepared["url"].as_str().unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 502);
        assert!(response.bytes().await.unwrap().is_empty());
    }
    for (path, mime, body) in [
        (
            "/opaque",
            "application/octet-stream",
            format!(
                "\u{feff}WEBVTT\n\n00:00.000 --> 00:01.000\n{}☃\n",
                "x".repeat(2048)
            ),
        ),
        (
            "/caption.srt",
            "application/x-subrip",
            format!("1\n00:00:00,000 --> 00:00:01,000\n{}☃\n", "x".repeat(2048)),
        ),
        (
            "/caption.ttml",
            "application/ttml+xml",
            format!(
                "<?xml version='1.0'?><tt><body><p>{}☃</p></body></tt>",
                "x".repeat(2048)
            ),
        ),
    ] {
        let utf8_split = body.as_bytes().iter().position(|b| *b == 0xe2).unwrap() + 1;
        let upstream = split_subtitle_fixture(
            mime,
            body.as_bytes().to_vec(),
            vec![1, 2, 3, 32, 512, utf8_split],
            true,
        )
        .await;
        let proxy = MediaProxy::new();
        let prepared = prepare(&proxy, &format!("{}{path}", upstream.base)).await;
        let response = client()
            .get(prepared["url"].as_str().unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{path}");
        let relayed = response.text().await.unwrap();
        assert!(relayed.contains('☃'));
        if path != "/caption.ttml" {
            assert_eq!(relayed.as_bytes(), body.as_bytes());
        }
    }
}

#[tokio::test]
async fn subtitle_hints_still_route_ordinary_manifests_through_rewriters() {
    for (path, mime, body, expected_type) in [
        (
            "/caption.vtt",
            "application/octet-stream",
            "#EXTM3U\n# <tt/>\n#EXT-X-STREAM-INF:BANDWIDTH=1\nhttps://example.invalid/child.m3u8\n",
            "mpegurl",
        ),
        (
            "/caption.srt",
            "application/x-subrip",
            "<MPD><BaseURL>https://example.invalid/movie/</BaseURL></MPD>",
            "dash+xml",
        ),
        (
            "/caption.ttml",
            "application/ttml+xml",
            "#EXTM3U\n#EXTINF:1,\nhttps://example.invalid/child.ts\n",
            "mpegurl",
        ),
    ] {
        let upstream =
            fixture(move |_| reply("200 OK", &format!("Content-Type: {mime}\r\n"), body)).await;
        let proxy = MediaProxy::new();
        let prepared = prepare(&proxy, &format!("{}{path}", upstream.base)).await;
        let response = client()
            .get(prepared["url"].as_str().unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert!(response.headers()["content-type"]
            .to_str()
            .unwrap()
            .contains(expected_type));
        let relayed = response.text().await.unwrap();
        assert!(relayed.contains("http://127.0.0.1:"));
        assert!(!relayed.contains("https://example.invalid/"));
    }
}

#[tokio::test]
async fn hls_webvtt_timestamp_maps_are_validated_without_losing_compatibility() {
    for mapping in [
        "X-TIMESTAMP-MAP=LOCAL:00:00:00.000,MPEGTS:900000",
        "X-TIMESTAMP-MAP=MPEGTS:8589934591,LOCAL:00:00.000",
    ] {
        let body = format!("WEBVTT\n{mapping}\n\n00:00.000 --> 00:01.000\nHello\n");
        let (status, relayed) = subtitle_result(
            "/opaque",
            "application/octet-stream",
            body.as_bytes().to_vec(),
        )
        .await;
        assert_eq!(status, 200, "{mapping}");
        assert_eq!(relayed, body.as_bytes());
    }
    for mapping in [
        "X-TIMESTAMP-MAP=LOCAL:00:00.000,MPEGTS:http://example.invalid/child",
        "X-TIMESTAMP-MAP=LOCAL:00:00.000,MPEGTS:8589934592",
        "X-TIMESTAMP-MAP=LOCAL:00:00.000,MPEGTS:1,MPEGTS:2",
        "X-TIMESTAMP-MAP=LOCAL:00:00.000,MPEGTS:1,URI:http://example.invalid/child",
        "X-TIMESTAMP-MAP=LOCAL:00:00.000",
        "X-TIMESTAMP-MAP=LOCAL:00:00.000,MPEGTS:1\nX-TIMESTAMP-MAP=LOCAL:00:00.000,MPEGTS:2",
    ] {
        assert_subtitle_rejected(
            "/caption.vtt",
            "text/vtt",
            format!("WEBVTT\n{mapping}\n\n00:00.000 --> 00:01.000\nHello\n").into_bytes(),
        )
        .await;
    }
}

#[tokio::test]
async fn subtitle_line_index_allocation_is_bounded() {
    for (path, mime, prefix) in [
        ("/caption.vtt", "text/vtt", "WEBVTT\n\n"),
        ("/caption.srt", "application/x-subrip", ""),
    ] {
        let body = format!("{prefix}{}", "\n".repeat(65_538));
        assert_subtitle_rejected(path, mime, body.into_bytes()).await;
    }
}

fn id3_wrapped(payload: &[u8], padding: usize) -> Vec<u8> {
    let mut body = b"ID3\x04\x00\x00\x00\x00\x00\x00".to_vec();
    for (i, shift) in [21, 14, 7, 0].into_iter().enumerate() {
        body[6 + i] = ((padding >> shift) & 0x7f) as u8;
    }
    body.resize(10 + padding, 0);
    body.extend_from_slice(payload);
    body
}

#[tokio::test]
async fn id3_wrapped_playlists_without_subtitle_hints_are_never_relayed_raw() {
    let playlist = b"#EXTM3U\n# <tt/>\n#EXT-X-STREAM-INF:BANDWIDTH=1\nhttp://example.invalid/no-consent-child.m3u8\n";
    for padding in [0, 1024, 65_536] {
        for mime in ["application/octet-stream", "audio/mpeg", "video/mp2t"] {
            assert_subtitle_rejected("/opaque", mime, id3_wrapped(playlist, padding)).await;
            assert_subtitle_rejected(
                "/opaque",
                mime,
                id3_wrapped(&id3_wrapped(playlist, padding), 0),
            )
            .await;
        }
    }
    let attack = id3_wrapped(playlist, 1024);
    let upstream = fixture(move |request| {
        if request.starts_with("GET /original.m3u8 ") {
            reply("302 Found", "Location: /opaque\r\n", "")
        } else {
            subtitle_reply("application/octet-stream", &attack)
        }
    })
    .await;
    let proxy = MediaProxy::new();
    let prepared = prepare(&proxy, &format!("{}/original.m3u8", upstream.base)).await;
    let response = client()
        .get(prepared["url"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 502);
    assert!(response.bytes().await.unwrap().is_empty());
}

#[tokio::test]
async fn id3_tagged_binary_audio_remains_streamable_with_split_metadata() {
    let audio = [vec![0xff, 0xfb, 0x90, 0x64], vec![0; 4096]].concat();
    let body = id3_wrapped(&audio, 65_536);
    let upstream = split_subtitle_fixture(
        "audio/mpeg",
        body.clone(),
        vec![1, 10, 32, 512, 65_546, 65_547],
        true,
    )
    .await;
    let proxy = MediaProxy::new();
    let prepared = prepare(&proxy, &format!("{}/audio.mp3", upstream.base)).await;
    let response = client()
        .get(prepared["url"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.bytes().await.unwrap().as_ref(), body.as_slice());
}

#[tokio::test]
async fn id3_metadata_bounds_and_unvalidated_partial_fragments_fail_closed() {
    let too_large = id3_wrapped(&[], 4 * 1024 * 1024);
    assert_subtitle_rejected("/audio.mp3", "audio/mpeg", too_large).await;
    let full = id3_wrapped(
        &[vec![0xff, 0xfb, 0x90, 0x64], vec![0; 2048]].concat(),
        1024,
    );
    let total = full.len();
    // Full metadata plus the complete post-tag sniff window are required when
    // this response does not reach representation EOF. Includes split ID3 magic.
    for span in [1, 2, 4, 9, 10, 32, 1024, 1034, 1035, 1545, 1546, total] {
        let partial = full[..span].to_vec();
        let served = partial.clone();
        let upstream = fixture(move |_| {
            let mut response = format!("HTTP/1.1 206 Partial Content\r\nConnection: close\r\nContent-Type: audio/mpeg\r\nContent-Range: bytes 0-{}/{total}\r\nContent-Length: {span}\r\n\r\n", span - 1).into_bytes();
            response.extend_from_slice(&served);
            response
        }).await;
        let proxy = MediaProxy::new();
        let prepared = prepare(&proxy, &format!("{}/audio.mp3", upstream.base)).await;
        let response = client()
            .get(prepared["url"].as_str().unwrap())
            .header("range", format!("bytes=0-{}", span - 1))
            .send()
            .await
            .unwrap();
        if span >= 1546 {
            assert_eq!(response.status(), 206);
            assert_eq!(response.bytes().await.unwrap().as_ref(), partial.as_slice());
        } else {
            assert_eq!(response.status(), 502, "span={span}");
            assert!(response.bytes().await.unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn redirected_id3_hls_reconnect_fragments_are_rejected_before_exposing_bytes() {
    let attack = id3_wrapped(b"#EXTM3U\n# <tt/>\n#EXT-X-STREAM-INF:BANDWIDTH=1\nhttp://example.invalid/no-consent-child.m3u8\n", 1024);
    for span in [1, 2, 4, 9, 10, 16, 32, 1024] {
        let total = attack.len();
        let fragment = attack[..span].to_vec();
        let upstream = fixture(move |request| {
            if request.starts_with("GET /original.m3u8 ") {
                reply("302 Found", "Location: /opaque\r\n", "")
            } else {
                let mut response = format!("HTTP/1.1 206 Partial Content\r\nConnection: close\r\nContent-Type: application/octet-stream\r\nContent-Range: bytes 0-{}/{total}\r\nContent-Length: {span}\r\n\r\n", span - 1).into_bytes();
                response.extend_from_slice(&fragment);
                response
            }
        }).await;
        let proxy = MediaProxy::new();
        let prepared = prepare(&proxy, &format!("{}/original.m3u8", upstream.base)).await;
        let response = client()
            .get(prepared["url"].as_str().unwrap())
            .header("range", "bytes=0-")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 502, "span={span}");
        assert!(response.bytes().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn aes_key_range_exemption_requires_the_whole_sixteen_byte_representation() {
    let body = id3_wrapped(&[], 1024)[..16].to_vec();
    for (status, total, expected) in [
        ("206 Partial Content", 16, 206),
        ("206 Partial Content", 2048, 502),
        ("200 OK", 2048, 502),
    ] {
        let served = body.clone();
        let upstream = fixture(move |_| {
            let mut response = format!("HTTP/1.1 {status}\r\nConnection: close\r\nContent-Type: application/octet-stream\r\nContent-Range: bytes 0-15/{total}\r\nContent-Length: 16\r\n\r\n").into_bytes();
            response.extend_from_slice(&served);
            response
        }).await;
        let proxy = MediaProxy::new();
        let prepared = prepare(&proxy, &format!("{}/key", upstream.base)).await;
        let response = client()
            .get(prepared["url"].as_str().unwrap())
            .header("range", "bytes=0-15")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), expected);
        let exposed = response.bytes().await.unwrap();
        if expected == 206 {
            assert_eq!(exposed.as_ref(), body.as_slice());
        } else {
            assert!(exposed.is_empty());
        }
    }
}

#[tokio::test]
async fn dash_rejects_excessive_attributes_before_rewriting() {
    let xml = format!(
        "<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" {}><Period/></MPD>",
        (0..2000)
            .map(|i| format!("a{i}=\"x\""))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let server =
        fixture(move |_| reply("200 OK", "Content-Type: application/dash+xml\r\n", &xml)).await;
    let proxy = MediaProxy::new();
    let lease = prepare(&proxy, &format!("{}/manifest.mpd", server.base)).await;
    assert_eq!(
        client()
            .get(lease["url"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        502
    );
}

#[tokio::test]
async fn probe_accepts_only_real_film_sized_sources() {
    let upstream = fixture(move |r| {
        let path = r.split_whitespace().nth(1).unwrap_or("").to_owned();
        match path.as_str() {
            "/film.mkv" => reply(
                "206 Partial Content",
                "Content-Type: video/x-matroska\r\nContent-Range: bytes 0-0/20864945180\r\n",
                "x",
            ),
            "/resolve/uncached" => {
                reply("302 Found", "Location: /videos/downloading_v2.mp4\r\n", "")
            }
            "/videos/downloading_v2.mp4" => reply(
                "206 Partial Content",
                "Content-Type: video/mp4\r\nContent-Range: bytes 0-0/812345\r\n",
                "x",
            ),
            "/tiny.mp4" => reply(
                "206 Partial Content",
                "Content-Type: video/mp4\r\nContent-Range: bytes 0-0/900000\r\n",
                "x",
            ),
            "/web/index.m3u8" | "/web/playlist" => reply(
                "200 OK",
                "Content-Type: application/vnd.apple.mpegurl\r\n",
                "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nlow.m3u8\n",
            ),
            "/error.html" => reply("200 OK", "Content-Type: text/html\r\n", "<html>nope</html>"),
            _ => reply(
                "503 Service Unavailable",
                "Content-Type: text/plain\r\n",
                "busy",
            ),
        }
    })
    .await;
    let p = MediaProxy::new();
    let probe = |path: &str| {
        let url = format!("{}{path}", upstream.base);
        let p = &p;
        async move { p.probe(&url, &json!({}), false).await.unwrap() }
    };
    let film = probe("/film.mkv").await;
    assert_eq!(film["ok"], true);
    assert_eq!(film["bytes"], 20_864_945_180u64);
    // Plain http test fixture must not populate https-only resolved_url.
    assert!(film["resolved_url"].is_null());
    let uncached = probe("/resolve/uncached").await;
    assert_eq!(uncached["ok"], false);
    assert_eq!(uncached["debrid_status"], "downloading");
    assert_eq!(probe("/tiny.mp4").await["ok"], false);
    // A web stream's HLS playlist is small and still a real source; a small page is not.
    assert_eq!(probe("/web/index.m3u8").await["ok"], true);
    assert_eq!(probe("/web/playlist").await["playlist"], true);
    assert_eq!(probe("/error.html").await["ok"], false);
    let busy = probe("/busy").await;
    assert_eq!(busy["ok"], false);
    assert_eq!(busy["status"], 503);
}
