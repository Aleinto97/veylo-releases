//! Veylo's one playback engine: libmpv, driven from Rust, on every platform. The picture is
//! drawn under the app's WebView (video.rs) and the controls are Dioxus (player_ui.rs), so the
//! player looks and behaves the same everywhere; only the way frames reach the screen differs:
//!
//! - Linux/desktop: mpv's render API into a GL area under a transparent WebView. Decoding uses
//!   the GPU (`hwdec=auto-safe`: VAAPI, NVDEC, Vulkan, VideoToolbox, D3D11VA), and HDR is
//!   tone-mapped by mpv with per-scene peak detection (GTK3 can't hand HDR to the compositor).
//! - Windows: `vo=gpu-next` on Direct3D 11 in mpv's own child window under the WebView2,
//!   with the GPU decoding (`hwdec=auto-safe`: D3D11VA, NVDEC).
//! - Android, direct (default): `vo=mediacodec_embed`. MediaCodec decodes straight into a
//!   SurfaceView, so HDR10, HLG and HDR10+ (its dynamic metadata stays in the bitstream) reach an
//!   HDR panel untouched, at the lowest power. Subtitles are drawn by the Dioxus layer.
//! - Android, rendered: `vo=gpu-next`. mpv draws the picture (ASS and image subtitles, tone
//!   mapping); chosen by the viewer, or automatically for image subtitles.
//!
//! URLs and headers are never logged.
use crate::backend;
use crate::player::{Facts, PlayRequest, PlayerEvent, PlayerHandle};
use libmpv2::{events::{Event, PropertyData}, mpv_end_file_reason, Format, Mpv};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc::UnboundedSender, watch};

/// How the picture fills the screen.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
#[allow(clippy::enum_variant_names)] // "Fit" is what the menu says
pub enum Fit { #[default] Fit, Fill, Stretch }

impl Fit {
    pub const ALL: [Fit; 3] = [Fit::Fit, Fit::Fill, Fit::Stretch];
    pub fn name(self) -> &'static str { match self { Fit::Fit => "Fit", Fit::Fill => "Fill", Fit::Stretch => "Stretch" } }
    pub fn about(self) -> &'static str {
        match self { Fit::Fit => "The whole picture", Fit::Fill => "Fill the screen, crop the edges", Fit::Stretch => "Fill the screen, stretch the picture" }
    }
}

/// One audio or subtitle track as the viewer sees it.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Track { pub id: i64, pub audio: bool, pub label: String, pub selected: bool, pub image: bool, pub styled: bool }

/// What the screen can show, as the platform declares it (Android); desktop output is SDR.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Screen { pub hdr10: bool, pub hlg: bool, pub hdr10_plus: bool, pub dolby_vision: bool }

/// Everything the controls draw. Positions are quantised so the UI repaints a few times a second.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct State {
    pub loaded: bool,
    pub position: f64,
    pub duration: f64,
    /// Seconds buffered ahead of `position`.
    pub buffered: f64,
    pub paused: bool,
    pub buffering: bool,
    pub ended: bool,
    pub hwdec: String,
    pub codec: String,
    pub width: i64,
    pub height: i64,
    /// Display aspect (width / height), 0 until known.
    pub aspect: f64,
    pub transfer: String,
    pub target_transfer: String,
    pub tracks: Vec<Track>,
    pub speed: f64,
    pub volume: f64,
    pub muted: bool,
    /// The current text subtitle, for the Dioxus layer in direct mode.
    pub subtitle: String,
    pub error: Option<String>,
    /// Android: MediaCodec writes straight to the display.
    pub direct: bool,
    pub fit: Fit,
    /// Dolby Vision profile of the video track (0: none). Profile 5 has no HDR10 layer.
    pub dv_profile: i64,
    /// Frames the player had to drop (decoder or display too slow).
    pub dropped: i64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Finish { Close, Next, Another }

/// A playing session, shared by the engine thread, the video surface and the controls.
pub struct Live {
    pub mpv: Mpv,
    pub request: PlayRequest,
    pub screen: Screen,
    state: watch::Sender<State>,
    finish: Mutex<Option<Finish>>,
    ready: (Mutex<bool>, Condvar),
}

/// A session handle for Dioxus props and signals (equal when it is the same session).
#[derive(Clone)]
pub struct LiveRef(pub Arc<Live>);
impl PartialEq for LiveRef { fn eq(&self, other: &Self) -> bool { Arc::ptr_eq(&self.0, &other.0) } }
impl std::fmt::Debug for LiveRef { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "Live({})", self.0.request.title) } }

/// The session that owns the screen, for the platform surface callbacks.
static ACTIVE: Mutex<Option<Arc<Live>>> = Mutex::new(None);

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn active() -> Option<Arc<Live>> { ACTIVE.lock().ok().and_then(|a| a.clone()) }

impl Live {
    pub fn subscribe(&self) -> watch::Receiver<State> { self.state.subscribe() }
    pub fn state(&self) -> State { self.state.borrow().clone() }
    fn update(&self, f: impl FnOnce(&mut State) -> bool) { self.state.send_if_modified(f); }

    /// The video surface is in place: mpv may open the file.
    pub fn mark_ready(&self) {
        let (lock, cv) = &self.ready;
        if let Ok(mut r) = lock.lock() { *r = true; cv.notify_all(); }
    }
    fn wait_ready(&self, timeout: Duration) -> bool {
        let (lock, cv) = &self.ready;
        let Ok(guard) = lock.lock() else { return false };
        cv.wait_timeout_while(guard, timeout, |r| !*r).map(|(r, _)| *r).unwrap_or(false)
    }

    // ------------------------------------------------------------ commands from the controls
    pub fn toggle_pause(&self) {
        let s = self.state();
        if s.ended { let _ = self.mpv.command("seek", &["0", "absolute"]); }
        let _ = self.mpv.set_property("pause", !s.paused);
    }
    pub fn seek_by(&self, seconds: f64) { let _ = self.mpv.command("seek", &[&format!("{seconds}"), "relative+exact"]); }
    pub fn seek_to(&self, seconds: f64) { let _ = self.mpv.command("seek", &[&format!("{:.3}", seconds.max(0.0)), "absolute"]); }
    pub fn set_speed(&self, speed: f64) { let _ = self.mpv.set_property("speed", speed); }
    pub fn set_volume(&self, volume: f64) { let _ = self.mpv.set_property("volume", volume.clamp(0.0, 130.0)); let _ = self.mpv.set_property("mute", false); }
    pub fn toggle_mute(&self) { let _ = self.mpv.set_property("mute", !self.state().muted); }
    pub fn select(&self, audio: bool, id: Option<i64>) {
        let value = id.map(|i| i.to_string()).unwrap_or_else(|| "no".into());
        let _ = self.mpv.set_property(if audio { "aid" } else { "sid" }, value.as_str());
        if !audio { self.fit_subtitles(id); }
    }
    /// Add a subtitle file (a local path or a link) as a track and turn it on; returns its track id.
    /// Blocks while mpv reads the file, so call it off the UI thread.
    pub fn add_subtitle(&self, file: &str, title: &str, lang: &str) -> anyhow::Result<i64> {
        self.mpv.command("sub-add", &[file, "select", title, lang]).map_err(|e| anyhow::anyhow!("{e}"))?;
        refresh_tracks(self);
        let id = self.mpv.get_property::<i64>("sid").map_err(|e| anyhow::anyhow!("{e}"))?;
        self.fit_subtitles(Some(id));
        Ok(id)
    }
    /// The next audio track, or the next subtitle track then off; returns what is on now.
    pub fn cycle_track(&self, audio: bool) -> String {
        let s = self.state();
        let list: Vec<&Track> = s.tracks.iter().filter(|t| t.audio == audio).collect();
        let at = list.iter().position(|t| t.selected);
        // Subtitles cycle through "off" after the last track; audio wraps around.
        let next = match at { Some(i) if i + 1 < list.len() => Some(list[i + 1]), Some(_) if !audio => None, _ => list.first().copied() };
        self.select(audio, next.map(|t| t.id));
        next.map(|t| t.label.clone()).unwrap_or_else(|| "Off".into())
    }
    pub fn seek_percent(&self, percent: f64) {
        let d = self.state().duration;
        if d > 0.0 { self.seek_to(d * percent / 100.0); }
    }
    /// Subtitles mpv must draw itself leave the direct path: image subtitles (PGS, DVD) always,
    /// styled ASS when the picture is SDR (on HDR the HDR wins, and the text is drawn plainly).
    fn fit_subtitles(&self, id: Option<i64>) {
        let s = self.state();
        if !s.direct { return; }
        let hdr = matches!(s.transfer.as_str(), "pq" | "hlg") || s.dv_profile > 0;
        if s.tracks.iter().any(|t| !t.audio && Some(t.id) == id && (t.image || (t.styled && !hdr))) { self.set_direct(false); }
    }
    pub fn set_fit(&self, fit: Fit) {
        self.update(|s| { let changed = s.fit != fit; s.fit = fit; changed });
        crate::video::layout(self);
        if cfg!(not(target_os = "android")) {
            let _ = self.mpv.set_property("keepaspect", fit != Fit::Stretch);
            let _ = self.mpv.set_property("panscan", if fit == Fit::Fill { 1.0 } else { 0.0 });
        }
    }
    /// Android: switch between MediaCodec straight to the display and mpv's renderer.
    pub fn set_direct(&self, direct: bool) {
        if !cfg!(target_os = "android") || self.state().direct == direct { return; }
        self.update(|s| { s.direct = direct; true });
        let _ = self.mpv.set_property("hwdec", if direct { "mediacodec" } else { "mediacodec-copy" });
        let _ = self.mpv.set_property("vo", if direct { "mediacodec_embed" } else { "gpu-next" });
    }
    /// Dolby Vision profile 5 without a Dolby Vision panel: the RPU that rebuilds its colours only
    /// survives FFmpeg's own decoder, so mpv decodes on the CPU and reshapes on the GPU.
    pub fn render_dolby_vision(&self) {
        self.update(|s| { s.direct = false; true });
        let _ = self.mpv.set_property("hwdec", "no");
        let _ = self.mpv.set_property("vo", if cfg!(any(target_os = "android", target_os = "windows")) { "gpu-next" } else { "libmpv" });
    }
    /// Reload the source where it stopped (after a network error).
    pub fn retry(&self) {
        let at = self.state().position;
        self.update(|s| { s.error = None; true });
        let _ = self.mpv.set_property("start", format!("+{at}").as_str());
        let _ = self.mpv.command("loadfile", &[self.request.url.as_str(), "replace"]);
    }
    pub fn finish(&self, how: Finish) {
        if let Ok(mut f) = self.finish.lock() { if f.is_none() { *f = Some(how); } }
        let _ = self.mpv.command("quit", &[]);
    }
}

/// Start a session: mpv is created here; the file opens once the video surface is ready.
pub fn start(req: PlayRequest, events: UnboundedSender<PlayerEvent>, stop: PlayerHandle) -> anyhow::Result<()> {
    let screen = platform_screen();
    let direct = cfg!(target_os = "android");
    let mpv = create(&req, direct)?;
    let (state, _) = watch::channel(State { speed: 1.0, volume: 100.0, direct, ..Default::default() });
    let live = Arc::new(Live { mpv, request: req, screen, state, finish: Mutex::new(None), ready: (Mutex::new(false), Condvar::new()) });
    if let Ok(mut a) = ACTIVE.lock() { *a = Some(live.clone()); }
    let _ = events.send(PlayerEvent::Attached(LiveRef(live.clone())));
    std::thread::Builder::new().name("veylo-engine".into()).spawn(move || run(live, events, stop))?;
    Ok(())
}

#[cfg(target_os = "android")]
fn platform_screen() -> Screen { crate::android::screen().unwrap_or_default() }
#[cfg(not(target_os = "android"))]
fn platform_screen() -> Screen { Screen::default() }

fn create(req: &PlayRequest, direct: bool) -> anyhow::Result<Mpv> {
    // libmpv refuses to start unless numbers parse the C way; GTK/WebKit set the user's locale.
    unsafe { libc::setlocale(libc::LC_NUMERIC, c"C".as_ptr()); }
    #[cfg(target_os = "android")]
    let ca = crate::android::prepare_mpv()?;
    let _ = direct;
    Mpv::with_initializer(|o| {
        // Nothing personal is read: the same behaviour on every machine.
        o.set_property("config", false)?;
        o.set_property("terminal", false)?;
        o.set_property("idle", true)?;
        o.set_property("keep-open", true)?;
        o.set_property("osc", false)?;
        o.set_property("input-default-bindings", false)?;
        o.set_property("input-vo-keyboard", false)?;
        o.set_property("ytdl", false)?;
        o.set_property("hr-seek", "yes")?;
        o.set_property("cache", "yes")?;
        o.set_property("demuxer-max-back-bytes", "64MiB")?;
        // HDR on an SDR target: per-scene peak detection (what HDR10+ metadata is for) and the default curve.
        o.set_property("hdr-compute-peak", "yes")?;
        o.set_property("sub-font", "sans-serif")?;
        o.set_property("sub-color", "#F2EFEA")?;
        o.set_property("sub-border-color", "#DD000000")?;
        o.set_property("sub-scale", 1.05)?;
        if req.start_ms > 0 { o.set_property("start", format!("+{}", req.start_ms as f64 / 1000.0).as_str())?; }
        // mpv's list syntax splits on commas, so a header value containing one can't be passed safely.
        let fields: Vec<String> = req.headers.iter().filter(|(k, v)| !k.contains(',') && !v.contains(','))
            .map(|(k, v)| format!("{k}: {v}")).collect();
        if !fields.is_empty() { o.set_property("http-header-fields", fields.join(",").as_str())?; }
        // Windows: mpv draws into its own child window of the app window (video.rs) with Direct3D 11.
        #[cfg(target_os = "windows")]
        {
            o.set_property("vo", "gpu-next")?;
            o.set_property("gpu-api", "d3d11")?;
            o.set_property("force-window", "no")?;
            o.set_property("cursor-autohide", "no")?;
            // Clicks and the cursor pass through to the WebView above it (newer mpv; older ones ignore it).
            let _ = o.set_property("cursor-passthrough", true);
            o.set_property("hwdec", "auto-safe")?;
            o.set_property("demuxer-max-bytes", "256MiB")?;
        }
        #[cfg(not(any(target_os = "android", target_os = "windows")))]
        {
            o.set_property("vo", "libmpv")?;
            o.set_property("hwdec", "auto-safe")?;
            o.set_property("demuxer-max-bytes", "256MiB")?;
        }
        #[cfg(target_os = "android")]
        {
            o.set_property("vo", if direct { "mediacodec_embed" } else { "gpu-next" })?;
            o.set_property("gpu-context", "android")?;
            o.set_property("opengl-es", "yes")?;
            o.set_property("hwdec", if direct { "mediacodec" } else { "mediacodec-copy" })?;
            o.set_property("hwdec-codecs", "all")?;
            o.set_property("force-window", "no")?;
            o.set_property("ao", "audiotrack,opensles")?;
            o.set_property("audio-set-media-role", "yes")?;
            o.set_property("tls-verify", "yes")?;
            o.set_property("tls-ca-file", ca.as_str())?;
            o.set_property("demuxer-max-bytes", "96MiB")?;
            // The SurfaceView is sized to the picture (video.rs); mpv fills it exactly.
            o.set_property("keepaspect", "no")?;
        }
        Ok(())
    }).map_err(|e| anyhow::anyhow!("mpv couldn't start: {e}"))
}

const OBSERVED: [(&str, Format); 18] = [
    ("frame-drop-count", Format::Int64),
    ("time-pos", Format::Double), ("duration", Format::Double), ("demuxer-cache-time", Format::Double),
    ("pause", Format::Flag), ("paused-for-cache", Format::Flag), ("seeking", Format::Flag), ("eof-reached", Format::Flag),
    ("speed", Format::Double), ("volume", Format::Double), ("mute", Format::Flag), ("sub-text", Format::String),
    ("hwdec-current", Format::String), ("video-format", Format::String), ("track-list", Format::String),
    ("aid", Format::String), ("sid", Format::String), ("core-idle", Format::Flag),
];

fn run(live: Arc<Live>, events: UnboundedSender<PlayerEvent>, stop: PlayerHandle) {
    let mpv = &live.mpv;
    let _ = mpv.disable_deprecated_events();
    for (i, (name, format)) in OBSERVED.iter().enumerate() { let _ = mpv.observe_property(name, *format, i as u64 + 1); }
    if !live.wait_ready(Duration::from_secs(10)) { eprintln!("veylo: video surface not ready, opening anyway"); }
    if let Err(e) = mpv.command("loadfile", &[live.request.url.as_str(), "replace"]) {
        live.update(|s| { s.error = Some(format!("This source couldn't be opened ({e}).")); true });
    }
    let (mut position, mut duration, mut cache_end) = (0.0f64, 0.0f64, 0.0f64);
    let (mut announced, mut quitting, mut played) = (false, false, false);
    let mut checkpoint = Instant::now();
    let record = |position: f64, duration: f64| if let Some((media, video)) = &live.request.record {
        if position > 0.0 && duration > 0.0 { backend::record_progress(media.clone(), video.clone(), (position * 1000.0) as i64, (duration * 1000.0) as i64); }
    };
    loop {
        if stop.stopped() && !quitting { quitting = true; live.finish(Finish::Close); }
        if checkpoint.elapsed() > Duration::from_secs(15) { checkpoint = Instant::now(); record(position, duration); }
        let Some(event) = mpv.wait_event(0.25) else { continue };
        match event {
            Ok(Event::FileLoaded) => {
                refresh_tracks(&live);
                live.update(|s| { s.loaded = true; s.error = None; true });
                // Profile 5 carries no HDR10 layer: without a Dolby Vision panel only mpv's renderer
                // (libplacebo's reshaping) shows its colours right.
                let s = live.state();
                if s.direct && s.dv_profile == 5 && !live.screen.dolby_vision { live.render_dolby_vision(); }
            }
            Ok(Event::VideoReconfig) => {
                let p = |n: &str| mpv.get_property::<i64>(n).unwrap_or(0);
                let (w, h, dw, dh) = (p("video-params/w"), p("video-params/h"), p("video-params/dw"), p("video-params/dh"));
                let transfer: String = mpv.get_property("video-params/gamma").unwrap_or_default();
                let target: String = mpv.get_property("video-target-params/gamma").unwrap_or_default();
                let codec: String = mpv.get_property("video-format").unwrap_or_default();
                let hwdec: String = mpv.get_property("hwdec-current").unwrap_or_default();
                live.update(|s| {
                    s.width = w; s.height = h; s.aspect = if dw > 0 && dh > 0 { dw as f64 / dh as f64 } else { 0.0 };
                    s.transfer = transfer; s.target_transfer = target; s.codec = codec; s.hwdec = hwdec; true
                });
                crate::video::layout(&live);
                // A subtitle chosen by default gets the same treatment as one picked by hand.
                let sid = live.state().tracks.iter().find(|t| !t.audio && t.selected).map(|t| t.id);
                if sid.is_some() { live.fit_subtitles(sid); }
                let s = live.state();
                #[cfg(target_os = "android")]
                crate::android::log(&format!("video {}x{} {} transfer={} hwdec={} direct={}", s.width, s.height, s.codec, s.transfer, s.hwdec, s.direct));
                if !announced && !s.codec.is_empty() {
                    announced = true;
                    let _ = events.send(PlayerEvent::Started { decoder: hardware(&s), codec: codec_name(&s.codec), facts: facts(&live, &s) });
                }
            }
            Ok(Event::PropertyChange { name, change, .. }) => match (name, change) {
                ("time-pos", PropertyData::Double(v)) => {
                    position = v;
                    if v > 1.0 { played = true; }
                    live.update(|s| { let moved = (s.position - v).abs() >= 0.25; if moved { s.position = v; s.buffered = (cache_end - v).max(0.0); } moved });
                }
                ("duration", PropertyData::Double(v)) => { duration = v; live.update(|s| { s.duration = v; true }); }
                ("demuxer-cache-time", PropertyData::Double(v)) => cache_end = v,
                ("pause", PropertyData::Flag(v)) => live.update(|s| { s.paused = v; true }),
                ("paused-for-cache", PropertyData::Flag(v)) => live.update(|s| { s.buffering = v; true }),
                ("seeking", PropertyData::Flag(v)) => live.update(|s| { let c = s.buffering != v; s.buffering = v; c }),
                ("eof-reached", PropertyData::Flag(true)) if played => {
                    live.update(|s| { s.ended = true; true });
                    // The end of an episode moves on; anything else closes, as on Android.
                    live.finish(if live.request.has_next { Finish::Next } else { Finish::Close });
                }
                ("eof-reached", PropertyData::Flag(false)) => live.update(|s| { let c = s.ended; s.ended = false; c }),
                ("frame-drop-count", PropertyData::Int64(v)) => live.update(|s| { let c = s.dropped != v; s.dropped = v; c }),
                ("speed", PropertyData::Double(v)) => live.update(|s| { s.speed = v; true }),
                ("volume", PropertyData::Double(v)) => live.update(|s| { s.volume = v; true }),
                ("mute", PropertyData::Flag(v)) => live.update(|s| { s.muted = v; true }),
                ("sub-text", PropertyData::Str(t)) => { let t = t.to_owned(); live.update(|s| { let c = s.subtitle != t; s.subtitle = t; c }) }
                ("sub-text", PropertyData::None) => live.update(|s| { let c = !s.subtitle.is_empty(); s.subtitle.clear(); c }),
                ("hwdec-current", PropertyData::Str(t)) => { let t = t.to_owned(); live.update(|s| { s.hwdec = t; true }) }
                ("track-list" | "aid" | "sid", _) => refresh_tracks(&live),
                _ => {}
            },
            Ok(Event::EndFile(reason)) if reason == mpv_end_file_reason::Error && !quitting => {
                #[cfg(target_os = "android")]
                crate::android::log("end-file: error");
                live.update(|s| { s.error = Some(if played { "The stream stopped." } else { "This source couldn't be played." }.into()); true });
            }
            Ok(Event::Shutdown) => break,
            _ => {}
        }
        if live.finish.lock().map(|f| f.is_some()).unwrap_or(false) { quitting = true; }
    }
    record(position, duration);
    if let Ok(mut a) = ACTIVE.lock() { if a.as_ref().is_some_and(|a| Arc::ptr_eq(a, &live)) { *a = None; } }
    let how = live.finish.lock().ok().and_then(|f| *f).unwrap_or(Finish::Close);
    let _ = events.send(PlayerEvent::Finished {
        position_ms: (position * 1000.0) as i64, duration_ms: (duration * 1000.0) as i64,
        next: how == Finish::Next, another: how == Finish::Another,
    });
}

fn refresh_tracks(live: &Live) {
    let json: String = live.mpv.get_property("track-list").unwrap_or_default();
    let tracks = parse_tracks(&json);
    let dv = dv_profile(&json);
    live.update(|s| { let c = s.tracks != tracks || s.dv_profile != dv; s.tracks = tracks; s.dv_profile = dv; c });
}

/// The selected video track's Dolby Vision profile, 0 without one.
pub fn dv_profile(json: &str) -> i64 {
    let list: Vec<serde_json::Value> = serde_json::from_str(json).unwrap_or_default();
    list.iter().find(|t| t["type"] == "video" && t["selected"] == true)
        .and_then(|t| t["dolby-vision-profile"].as_i64()).unwrap_or(0)
}

pub fn parse_tracks(json: &str) -> Vec<Track> {
    let list: Vec<serde_json::Value> = serde_json::from_str(json).unwrap_or_default();
    list.iter().filter_map(|t| {
        let audio = match t["type"].as_str()? { "audio" => true, "sub" => false, _ => return None };
        let id = t["id"].as_i64()?;
        let title = t["title"].as_str().unwrap_or("").trim();
        let language = t["lang"].as_str().map(language_name).unwrap_or_default();
        let codec = t["codec"].as_str().unwrap_or("");
        let channels = t["demux-channel-count"].as_i64().unwrap_or(0);
        let mut parts: Vec<String> = Vec::new();
        if !title.is_empty() { parts.push(title.to_owned()); }
        if !language.is_empty() && !title.to_lowercase().contains(&language.to_lowercase()) { parts.push(language); }
        if audio && channels > 0 { parts.push(match channels { 1 => "Mono".into(), 2 => "Stereo".into(), 6 => "5.1".into(), 8 => "7.1".into(), n => format!("{n} ch") }); }
        if let Some(c) = pretty_codec(codec) { parts.push(c); }
        if t["external"].as_bool() == Some(true) { parts.push("External".into()); }
        let image = t["image"].as_bool().unwrap_or(matches!(codec, "hdmv_pgs_subtitle" | "dvd_subtitle" | "dvb_subtitle"));
        let styled = matches!(codec, "ass" | "ssa");
        Some(Track { id, audio, label: if parts.is_empty() { format!("Track {id}") } else { parts.join(", ") }, selected: t["selected"].as_bool() == Some(true), image, styled })
    }).collect()
}

fn pretty_codec(c: &str) -> Option<String> {
    Some(match c {
        "truehd" => "Dolby TrueHD", "eac3" => "Dolby Digital Plus", "ac3" => "Dolby Digital", "dts" => "DTS", "aac" => "AAC",
        "opus" => "Opus", "flac" => "FLAC", "subrip" => "SRT", "ass" => "ASS", "hdmv_pgs_subtitle" => "PGS", "webvtt" => "WebVTT",
        "dvd_subtitle" => "VobSub", "mov_text" => "Text", "" => return None, other => return Some(other.to_uppercase()),
    }.into())
}

/// A language code (ISO 639-1 or -2, as mpv and the subtitle add-ons write them) in English.
pub fn language_name(code: &str) -> String {
    let c = code.to_lowercase();
    match c.as_str() {
        "en" | "eng" => "English", "it" | "ita" => "Italian", "es" | "spa" => "Spanish", "fr" | "fre" | "fra" => "French",
        "de" | "ger" | "deu" => "German", "pt" | "por" => "Portuguese", "ja" | "jpn" => "Japanese", "ko" | "kor" => "Korean",
        "zh" | "chi" | "zho" => "Chinese", "ru" | "rus" => "Russian", "nl" | "dut" | "nld" => "Dutch", "pl" | "pol" => "Polish",
        "sv" | "swe" => "Swedish", "da" | "dan" => "Danish", "no" | "nor" | "nob" => "Norwegian", "fi" | "fin" => "Finnish",
        "tr" | "tur" => "Turkish", "ar" | "ara" => "Arabic", "hi" | "hin" => "Hindi", "el" | "gre" | "ell" => "Greek",
        "he" | "heb" => "Hebrew", "cs" | "cze" | "ces" => "Czech", "hu" | "hun" => "Hungarian", "ro" | "rum" | "ron" => "Romanian",
        "pb" | "pob" | "pt-br" => "Portuguese (Brazil)", "uk" | "ukr" => "Ukrainian", "bg" | "bul" => "Bulgarian", "hr" | "hrv" => "Croatian",
        "sr" | "srp" | "scc" => "Serbian", "sl" | "slv" => "Slovenian", "sk" | "slo" | "slk" => "Slovak", "et" | "est" => "Estonian",
        "lv" | "lav" => "Latvian", "lt" | "lit" => "Lithuanian", "fa" | "per" | "fas" => "Persian", "id" | "ind" => "Indonesian",
        "ms" | "may" | "msa" => "Malay", "th" | "tha" => "Thai", "vi" | "vie" => "Vietnamese", "ca" | "cat" => "Catalan",
        "eu" | "baq" | "eus" => "Basque", "gl" | "glg" => "Galician", "is" | "ice" | "isl" => "Icelandic", "mk" | "mac" | "mkd" => "Macedonian",
        "sq" | "alb" | "sqi" => "Albanian", "bs" | "bos" => "Bosnian", "bn" | "ben" => "Bengali", "ta" | "tam" => "Tamil",
        "te" | "tel" => "Telugu", "ml" | "mal" => "Malayalam", "ur" | "urd" => "Urdu", "tl" | "tgl" | "fil" => "Filipino",
        "zt" | "zht" => "Chinese (traditional)", "ze" => "Chinese (bilingual)", "ea" | "spl" | "spn" => "Spanish (Latin America)",
        "ka" | "geo" | "kat" => "Georgian", "hy" | "arm" | "hye" => "Armenian", "km" | "khm" => "Khmer", "my" | "bur" | "mya" => "Burmese",
        "af" | "afr" => "Afrikaans", "sw" | "swa" => "Swahili", "kk" | "kaz" => "Kazakh", "mn" | "mon" => "Mongolian", "ne" | "nep" => "Nepali",
        "si" | "sin" => "Sinhala", "eo" | "epo" => "Esperanto", "ga" | "gle" => "Irish", "cy" | "wel" | "cym" => "Welsh", "la" | "lat" => "Latin",
        "und" | "" => "", _ => return code.to_uppercase(),
    }.into()
}

/// The hardware API decoding the picture, or None in software.
pub fn hardware(s: &State) -> Option<String> {
    Some(s.hwdec.clone()).filter(|h| !h.is_empty() && h != "no").map(|h| match h.as_str() {
        "mediacodec" | "mediacodec-copy" => "MediaCodec".into(), "vaapi" | "vaapi-copy" => "VAAPI".into(), "nvdec" | "nvdec-copy" | "cuda" => "NVDEC".into(),
        "videotoolbox" | "videotoolbox-copy" => "VideoToolbox".into(), "d3d11va" | "d3d11va-copy" => "D3D11VA".into(), "vulkan" => "Vulkan".into(), other => other.to_owned(),
    })
}

pub fn codec_name(codec: &str) -> String {
    match codec { "hevc" => "HEVC".into(), "h264" => "H.264".into(), "av1" => "AV1".into(), "vp9" => "VP9".into(), "" => String::new(), c => c.to_uppercase() }
}

/// What the screen really shows. The stream's transfer decides HDR; the source label only adds
/// what a stream can't state (the "+" of HDR10+, Dolby Vision). Unknown stays empty.
pub fn picture(transfer: &str, label: Option<&str>, dv_profile: i64, screen: Screen, direct: bool) -> String {
    let label = label.unwrap_or("");
    let plus = label.contains("10+");
    let dv = dv_profile > 0 || label.to_lowercase().contains("dolby") || label.contains("DV");
    // Profile 5 is PQ underneath even where the stream's colour tags are blank.
    let transfer = if dv_profile == 5 && !transfer.is_empty() { "pq" } else { transfer };
    match transfer {
        "pq" => {
            if direct && screen.hdr10 {
                // The panel only takes what it declares; otherwise it shows the static HDR10 layer.
                if dv && !screen.dolby_vision { "HDR10 (Dolby Vision source)".into() }
                else if dv { "Dolby Vision".into() }
                else if plus && !screen.hdr10_plus { "HDR10 (HDR10+ source)".into() }
                else if plus { "HDR10+".into() } else { "HDR10".into() }
            } else {
                let name = if dv { "Dolby Vision" } else if plus { "HDR10+" } else { "HDR10" };
                if direct { format!("{name}, tone-mapped by the system") } else { format!("{name}, tone-mapped to SDR") }
            }
        }
        "hlg" => if direct && screen.hlg { "HLG".into() } else if direct { "HLG, tone-mapped by the system".into() } else { "HLG, tone-mapped to SDR".into() },
        "" => String::new(),
        _ => "SDR".into(),
    }
}

pub fn facts(live: &Live, s: &State) -> Facts {
    Facts {
        resolution: Some(crate::player::resolution_label(s.width, s.height)).filter(|r| !r.is_empty()).or(live.request.quality.clone()).unwrap_or_default(),
        picture: picture(&s.transfer, live.request.hdr.as_deref(), s.dv_profile, live.screen, s.direct),
        size: live.request.size.clone().unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hdr_is_named_by_what_reaches_the_panel() {
        let hdr = Screen { hdr10: true, hlg: true, hdr10_plus: true, dolby_vision: false };
        assert_eq!(picture("pq", Some("HDR10+"), 0, hdr, true), "HDR10+");
        assert_eq!(picture("pq", Some("HDR10+"), 0, Screen { hdr10: true, ..Default::default() }, true), "HDR10 (HDR10+ source)");
        assert_eq!(picture("pq", Some("HDR10+"), 0, hdr, false), "HDR10+, tone-mapped to SDR");
        assert_eq!(picture("pq", None, 0, Screen::default(), true), "HDR10, tone-mapped by the system");
        assert_eq!(picture("hlg", None, 0, hdr, true), "HLG");
        assert_eq!(picture("bt.1886", Some("HDR10"), 0, hdr, true), "SDR");
        assert_eq!(picture("", None, 0, hdr, true), "");
        // Dolby Vision is named only when the panel takes it.
        assert_eq!(picture("pq", Some("Dolby Vision"), 8, hdr, true), "HDR10 (Dolby Vision source)");
        assert_eq!(picture("pq", None, 8, Screen { dolby_vision: true, ..hdr }, true), "Dolby Vision");
        assert_eq!(picture("bt.1886", None, 5, hdr, false), "Dolby Vision, tone-mapped to SDR");
    }
    #[test]
    fn tracks_read_like_a_menu() {
        let json = r#"[{"id":1,"type":"video","codec":"hevc"},
            {"id":1,"type":"audio","lang":"eng","codec":"eac3","demux-channel-count":6,"selected":true},
            {"id":2,"type":"audio","lang":"ita","title":"Italiano","codec":"ac3","demux-channel-count":2},
            {"id":1,"type":"sub","lang":"eng","codec":"hdmv_pgs_subtitle","image":true}]"#;
        let t = parse_tracks(json);
        assert_eq!(t.len(), 3);
        assert_eq!(t[0].label, "English, 5.1, Dolby Digital Plus");
        assert!(t[0].selected && t[0].audio);
        assert_eq!(t[1].label, "Italiano, Stereo, Dolby Digital");
        assert!(t[2].image && !t[2].audio);
    }
}
