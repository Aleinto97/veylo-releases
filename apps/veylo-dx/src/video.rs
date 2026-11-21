//! Where the picture goes: under the app's WebView, which turns transparent while a title
//! plays, so the Dioxus controls (player_ui.rs) float over the video on every platform.
//! Linux, Wayland: a subsurface of the window placed below it, filled by mpv's render API on its
//!   own EGL context and thread; the compositor blends the page over the picture (no copies).
//! Linux, X11: a GtkGLArea below the WebKit view, put in place when the first title plays: a window
//!   holding a GL area is composited through GL, and where GL can't draw (software GL, some VMs) the
//!   whole page would stay black.
//! Android: a SurfaceView below the WebView (VeyloVideo.kt), handed to mpv as its window.
//! Windows: mpv's own child window inside the app window, kept below the WebView2.
//! macOS: a Core Animation OpenGL layer below the WKWebView, filled by mpv's render API; EDR (HDR)
//!   on screens with headroom.
use crate::engine::Live;
use std::sync::Arc;

/// Put the session's picture on screen (call on the UI thread).
pub fn attach(live: &Arc<Live>) { imp::attach(live) }
/// Take the picture away and give the app its opaque background back.
pub fn detach() { imp::detach() }
/// The picture's aspect or fit changed (any thread).
pub fn layout(live: &Live) { imp::layout(live) }
/// Desktop window fullscreen; Android is always full screen while playing.
pub fn set_fullscreen(on: bool) { imp::set_fullscreen(on) }
/// macOS self-test: the OpenGL context the picture uses, and mpv's renderer on it.
#[cfg(target_os = "macos")]
pub fn check(mpv: &libmpv2::Mpv) -> Result<String, String> { imp::check(mpv) }
/// macOS: the most EDR headroom the window's screen can reach (1.0: SDR only; 0: not read yet).
#[cfg(target_os = "macos")]
pub fn potential_headroom() -> f64 { imp::potential_headroom() }
/// Windows: the app window, for the screen's HDR state (display.rs).
#[cfg(target_os = "windows")]
pub fn hwnd() -> Option<isize> {
    use dioxus::desktop::tao::platform::windows::WindowExtWindows;
    Some(dioxus::desktop::window().window.hwnd() as isize)
}

#[cfg(target_os = "android")]
mod imp {
    use super::*;
    use crate::engine::Fit;
    pub fn attach(live: &Arc<Live>) {
        let _ = crate::android::video_show();
        crate::android::attach_surface(live);
    }
    pub fn detach() { let _ = crate::android::video_hide(); }
    pub fn layout(live: &Live) {
        let s = live.state();
        let fit = match s.fit { Fit::Fit => 0, Fit::Fill => 1, Fit::Stretch => 2 };
        let _ = crate::android::video_layout(s.aspect, fit);
    }
    pub fn set_fullscreen(_on: bool) {}
}

#[cfg(target_os = "windows")]
mod imp {
    //! Windows: mpv gets the app window (`wid`) and opens its own child window filling it; we keep
    //! that child at the bottom of the window's stack, under WebView2's, and make the page
    //! transparent while playing, so the Dioxus controls float over the picture as elsewhere.
    use super::*;
    use dioxus::desktop::tao::platform::windows::WindowExtWindows;
    use std::sync::atomic::{AtomicU64, Ordering};

    type Hwnd = isize;
    #[link(name = "user32")]
    extern "system" {
        fn EnumChildWindows(parent: Hwnd, f: unsafe extern "system" fn(Hwnd, isize) -> i32, data: isize) -> i32;
        fn GetClassNameW(hwnd: Hwnd, name: *mut u16, len: i32) -> i32;
        fn GetParent(hwnd: Hwnd) -> Hwnd;
        fn GetWindow(hwnd: Hwnd, cmd: u32) -> Hwnd;
        fn SetWindowPos(hwnd: Hwnd, after: Hwnd, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> i32;
    }
    const HWND_BOTTOM: Hwnd = 1;
    const GW_HWNDLAST: u32 = 1;
    const SWP_NOSIZE: u32 = 0x1;
    const SWP_NOMOVE: u32 = 0x2;
    const SWP_NOACTIVATE: u32 = 0x10;
    const SWP_NOOWNERZORDER: u32 = 0x200;
    const SWP_ASYNCWINDOWPOS: u32 = 0x4000;

    /// Bumped on every attach and detach: the keeper thread of an old session stops.
    static SESSION: AtomicU64 = AtomicU64::new(0);

    /// mpv's own child window ("mpv" class) directly under the app window, if it exists yet.
    fn mpv_window(top: Hwnd) -> Option<Hwnd> {
        unsafe extern "system" fn visit(hwnd: Hwnd, data: isize) -> i32 {
            let found = &mut *(data as *mut (Hwnd, Hwnd));
            let mut name = [0u16; 16];
            let n = GetClassNameW(hwnd, name.as_mut_ptr(), name.len() as i32).max(0) as usize;
            if GetParent(hwnd) == found.0 && String::from_utf16_lossy(&name[..n]) == "mpv" { found.1 = hwnd; return 0; }
            1
        }
        let mut found: (Hwnd, Hwnd) = (top, 0);
        unsafe { EnumChildWindows(top, visit, &mut found as *mut _ as isize); }
        (found.1 != 0).then_some(found.1)
    }

    pub fn attach(live: &Arc<Live>) {
        let top = dioxus::desktop::window().window.hwnd();
        let _ = live.mpv.set_property("wid", top as i64);
        let _ = dioxus::desktop::window().webview.set_background_color((0, 0, 0, 0));
        let session = SESSION.fetch_add(1, Ordering::SeqCst) + 1;
        // mpv creates its window on its own thread once the file opens, on top of the stack:
        // keep sending it to the bottom (the WebView may also be re-stacked while it lives).
        let _ = std::thread::Builder::new().name("veylo-video".into()).spawn(move || {
            while SESSION.load(Ordering::SeqCst) == session {
                if let Some(video) = mpv_window(top) {
                    if unsafe { GetWindow(video, GW_HWNDLAST) } != video {
                        unsafe { SetWindowPos(video, HWND_BOTTOM, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS); }
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
        });
        live.mark_ready();
    }
    pub fn detach() {
        SESSION.fetch_add(1, Ordering::SeqCst);
        let _ = dioxus::desktop::window().webview.set_background_color((0, 0, 0, 255));
        set_fullscreen(false);
    }
    pub fn layout(_live: &Live) {}
    pub fn set_fullscreen(on: bool) {
        let w = dioxus::desktop::window();
        w.window.set_fullscreen(on.then_some(dioxus::desktop::tao::window::Fullscreen::Borderless(None)));
    }
}

#[cfg(target_os = "macos")]
#[allow(deprecated)] // Apple marks OpenGL deprecated; it remains the API mpv's renderer offers on macOS.
mod imp {
    //! macOS: mpv can't draw into another app's view there (no `wid`), so it draws through its render
    //! API into a Core Animation OpenGL layer of ours, on a view placed below the WKWebView inside the
    //! same window (the way IINA and other libmpv players on macOS do it). The WebView doesn't draw a
    //! background (main.rs makes the window transparent): the page paints its own black, and nothing
    //! while a title plays. HDR: on a screen with EDR headroom the layer turns EDR with a PQ colour
    //! space and mpv sends PQ to it; anywhere else mpv tone-maps to SDR.
    //!
    //! Threads: AppKit work on the main thread (attach, detach); Core Animation asks `canDraw`/`draw` on
    //! its own thread with our GL context current; mpv's update callback only raises a flag.
    use super::*;
    use dioxus::desktop::tao::platform::macos::WindowExtMacOS;
    use libmpv2_sys as sys;
    use objc2::rc::Retained;
    use objc2::runtime::NSObject;
    use objc2::{define_class, msg_send, ClassType, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{NSAutoresizingMaskOptions, NSView, NSWindow, NSWindowOrderingMode};
    use objc2_core_graphics::{CGColor, CGColorSpace};
    use objc2_core_video::CVTimeStamp;
    use objc2_open_gl::{CGLChoosePixelFormat, CGLContextObj, CGLCreateContext, CGLLockContext, CGLPixelFormatAttribute as A,
        CGLPixelFormatObj, CGLRetainContext, CGLRetainPixelFormat, CGLSetCurrentContext, CGLGetCurrentContext, CGLUnlockContext};
    use objc2_quartz_core::{CALayer, CAOpenGLLayer, CATransaction};
    use std::cell::RefCell;
    use std::ffi::{c_char, c_void};
    use std::ptr::{null_mut, NonNull};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Mutex;

    #[link(name = "OpenGL", kind = "framework")]
    extern "C" {
        fn glGetIntegerv(name: u32, data: *mut i32);
        fn glClearColor(r: f32, g: f32, b: f32, a: f32);
        fn glClear(mask: u32);
    }
    const GL_DRAW_FRAMEBUFFER_BINDING: u32 = 0x8CA6;
    const GL_VIEWPORT: u32 = 0x0BA2;
    const GL_COLOR_BUFFER_BIT: u32 = 0x4000;

    /// The session being drawn and mpv's renderer for it (created with our GL context current).
    /// `_live` keeps mpv's core alive until the renderer is freed (mpv requires that order).
    struct Session { _live: Arc<Live>, render: *mut sys::mpv_render_context }
    // SAFETY: the renderer is only used under RENDER's lock with our GL context current.
    unsafe impl Send for Session {}
    static RENDER: Mutex<Option<Session>> = Mutex::new(None);

    /// One GL pixel format and context for the app's lifetime, shared with the layer.
    struct Gl { format: CGLPixelFormatObj, context: CGLContextObj, float: bool, label: &'static str }
    // SAFETY: CGL objects are reference-counted C objects usable from any thread under CGLLockContext.
    unsafe impl Send for Gl {}
    static GL: Mutex<Option<Gl>> = Mutex::new(None);

    /// mpv has a new frame; or the layer must redraw (new session, resize) whatever mpv says.
    static PENDING: AtomicBool = AtomicBool::new(false);
    static DIRTY: AtomicBool = AtomicBool::new(false);
    /// The screen's EDR headroom, read on the main thread at attach (f64 bits); 1.0 is SDR only.
    static HEADROOM: AtomicU64 = AtomicU64::new(0);
    static POTENTIAL: AtomicU64 = AtomicU64::new(0);
    /// What the layer and mpv are set to: 0 SDR, 1 PQ in BT.2020, 2 PQ in Display P3.
    static HDR_MODE: AtomicU64 = AtomicU64::new(0);

    /// The video layer, for HDR changes from the engine thread (layer properties are thread-safe in
    /// an explicit CATransaction).
    struct SendLayer(Retained<VideoLayer>);
    // SAFETY: only Core Animation's thread-safe property setters are used from other threads.
    unsafe impl Send for SendLayer {}
    static LAYER: Mutex<Option<SendLayer>> = Mutex::new(None);
    thread_local! { static VIEW: RefCell<Option<Retained<NSView>>> = const { RefCell::new(None) }; }

    define_class!(
        // SAFETY: CAOpenGLLayer may be subclassed; the overrides keep its contracts (retained returns).
        #[unsafe(super(CAOpenGLLayer, CALayer, NSObject))]
        #[name = "VeyloVideoLayer"]
        struct VideoLayer;

        impl VideoLayer {
            #[unsafe(method(copyCGLPixelFormatForDisplayMask:))]
            fn copy_pixel_format(&self, _mask: u32) -> CGLPixelFormatObj {
                match gl() { Some((format, _, _)) => unsafe { CGLRetainPixelFormat(format) }, None => null_mut() }
            }

            #[unsafe(method(copyCGLContextForPixelFormat:))]
            fn copy_context(&self, _pf: CGLPixelFormatObj) -> CGLContextObj {
                match gl() { Some((_, context, _)) => unsafe { CGLRetainContext(context) }, None => null_mut() }
            }

            #[unsafe(method(canDrawInCGLContext:pixelFormat:forLayerTime:displayTime:))]
            fn can_draw(&self, _ctx: CGLContextObj, _pf: CGLPixelFormatObj, _t: f64, _ts: *const CVTimeStamp) -> bool {
                wants_frame()
            }

            #[unsafe(method(drawInCGLContext:pixelFormat:forLayerTime:displayTime:))]
            fn draw(&self, ctx: CGLContextObj, pf: CGLPixelFormatObj, t: f64, ts: *const CVTimeStamp) {
                unsafe { draw_frame(); }
                // The superclass flushes the frame to the layer.
                unsafe { let _: () = msg_send![super(self), drawInCGLContext: ctx, pixelFormat: pf, forLayerTime: t, displayTime: ts]; }
            }
        }
    );

    /// The pixel format and context, created once: 16-bit float colour (what EDR needs) on an OpenGL 3.2
    /// core profile when the GPU offers it, then 8-bit, the legacy profile, any renderer, and last
    /// Apple's software renderer (virtual machines without GPU OpenGL).
    fn gl() -> Option<(CGLPixelFormatObj, CGLContextObj, bool)> {
        let mut gl = GL.lock().ok()?;
        if gl.is_none() {
            let accelerated = |profile: u32| vec![A::CGLPFAOpenGLProfile.0, profile, A::CGLPFAAccelerated.0, A::CGLPFADoubleBuffer.0, A::CGLPFAAllowOfflineRenderers.0];
            let float = [accelerated(0x3200), vec![A::CGLPFAColorSize.0, 64, A::CGLPFAColorFloat.0]].concat();
            let any = vec![A::CGLPFAOpenGLProfile.0, 0x3200, A::CGLPFADoubleBuffer.0];
            let software = vec![A::CGLPFAOpenGLProfile.0, 0x3200, A::CGLPFARendererID.0, objc2_open_gl::kCGLRendererGenericFloatID, A::CGLPFADoubleBuffer.0];
            let candidates = [("float, accelerated", float, true), ("8-bit, accelerated", accelerated(0x3200), false),
                ("8-bit, legacy profile", accelerated(0x1000), false), ("8-bit, any renderer", any, false), ("software", software, false)];
            let mut tried = Vec::new();
            for (label, attrs, is_float) in candidates {
                let mut attrs: Vec<A> = attrs.into_iter().map(A).collect();
                attrs.push(A(0));
                let (mut format, mut count): (CGLPixelFormatObj, i32) = (null_mut(), 0);
                let err = unsafe { CGLChoosePixelFormat(NonNull::new_unchecked(attrs.as_mut_ptr()), NonNull::from(&mut format), NonNull::from(&mut count)) };
                if format.is_null() { tried.push(format!("{label}: pixel format error {}", err.0)); continue; }
                let mut context: CGLContextObj = null_mut();
                let err = unsafe { CGLCreateContext(format, null_mut(), NonNull::from(&mut context)) };
                if context.is_null() { tried.push(format!("{label}: context error {}", err.0)); continue; }
                *gl = Some(Gl { format, context, float: is_float, label });
                break;
            }
            if gl.is_none() { eprintln!("veylo: no OpenGL context ({})", tried.join("; ")); }
        }
        gl.as_ref().map(|g| (g.format, g.context, g.float))
    }

    /// Self-test: the GL context the picture would use, and mpv's renderer on it.
    pub fn check(mpv: &libmpv2::Mpv) -> Result<String, String> {
        gl().ok_or("no OpenGL context")?;
        let label = GL.lock().ok().and_then(|g| g.as_ref().map(|g| g.label)).unwrap_or("?");
        let render = unsafe { create_render_for(mpv.ctx.as_ptr())? };
        unsafe { with_context(|| sys::mpv_render_context_free(render)); }
        Ok(format!("OpenGL {label}; mpv's renderer starts on it"))
    }

    unsafe extern "C" fn proc_address(_: *mut c_void, name: *const c_char) -> *mut c_void { libc::dlsym(libc::RTLD_DEFAULT, name) }
    unsafe extern "C" fn on_update(_: *mut c_void) { PENDING.store(true, Ordering::Release); }

    /// Run `f` with our GL context locked and current (main thread), restoring whatever was current.
    unsafe fn with_context<T>(f: impl FnOnce() -> T) -> Option<T> {
        let (_, context, _) = gl()?;
        CGLLockContext(context);
        let previous = CGLGetCurrentContext();
        CGLSetCurrentContext(context);
        let out = f();
        CGLSetCurrentContext(previous);
        CGLUnlockContext(context);
        Some(out)
    }

    fn wants_frame() -> bool {
        let Ok(session) = RENDER.lock() else { return false };
        let Some(s) = session.as_ref() else { return DIRTY.swap(false, Ordering::AcqRel) };
        let dirty = DIRTY.swap(false, Ordering::AcqRel);
        let frame = PENDING.swap(false, Ordering::AcqRel)
            && unsafe { sys::mpv_render_context_update(s.render) } & sys::mpv_render_update_flag_MPV_RENDER_UPDATE_FRAME as u64 != 0;
        dirty || frame
    }

    unsafe fn draw_frame() {
        let (mut fbo, mut viewport) = (0i32, [0i32; 4]);
        glGetIntegerv(GL_DRAW_FRAMEBUFFER_BINDING, &mut fbo);
        glGetIntegerv(GL_VIEWPORT, viewport.as_mut_ptr());
        let float = gl().is_some_and(|g| g.2);
        let session = RENDER.lock();
        match session.as_ref().ok().and_then(|s| s.as_ref()) {
            Some(s) => {
                let mut target = sys::mpv_opengl_fbo { fbo, w: viewport[2], h: viewport[3], internal_format: 0 };
                let (mut flip, mut depth): (i32, i32) = (1, if float { 16 } else { 8 });
                let mut params = [
                    sys::mpv_render_param { type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_FBO, data: &mut target as *mut _ as *mut c_void },
                    sys::mpv_render_param { type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_FLIP_Y, data: &mut flip as *mut _ as *mut c_void },
                    sys::mpv_render_param { type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_DEPTH, data: &mut depth as *mut _ as *mut c_void },
                    sys::mpv_render_param { type_: 0, data: null_mut() },
                ];
                sys::mpv_render_context_render(s.render, params.as_mut_ptr());
            }
            None => { glClearColor(0.0, 0.0, 0.0, 1.0); glClear(GL_COLOR_BUFFER_BIT); }
        }
    }

    /// mpv's renderer on our context; it must exist before mpv opens the file (vo=libmpv).
    unsafe fn create_render(live: &Live) -> Result<*mut sys::mpv_render_context, String> { create_render_for(live.mpv.ctx.as_ptr()) }

    unsafe fn create_render_for(mpv: *mut sys::mpv_handle) -> Result<*mut sys::mpv_render_context, String> {
        with_context(|| {
            let mut init = sys::mpv_opengl_init_params { get_proc_address: Some(proc_address), get_proc_address_ctx: null_mut() };
            let mut params = [
                sys::mpv_render_param { type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_API_TYPE, data: sys::MPV_RENDER_API_TYPE_OPENGL.as_ptr() as *mut c_void },
                sys::mpv_render_param { type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_INIT_PARAMS, data: &mut init as *mut _ as *mut c_void },
                sys::mpv_render_param { type_: 0, data: null_mut() },
            ];
            let mut render: *mut sys::mpv_render_context = null_mut();
            let err = sys::mpv_render_context_create(&mut render, mpv, params.as_mut_ptr());
            if err < 0 { return Err(format!("mpv renderer: error {err}")); }
            sys::mpv_render_context_set_update_callback(render, Some(on_update), null_mut());
            Ok(render)
        }).unwrap_or_else(|| Err("no OpenGL context".into()))
    }

    /// The video view under the WebView, created once per window.
    fn view(mtm: MainThreadMarker) -> Option<Retained<NSView>> {
        if let Some(v) = VIEW.with(|v| v.borrow().clone()) { return Some(v); }
        let window = window()?;
        let content = window.contentView()?;
        let layer: Retained<VideoLayer> = unsafe { msg_send![VideoLayer::class(), new] };
        layer.setAsynchronous(true);
        layer.setNeedsDisplayOnBoundsChange(true);
        layer.setContentsScale(window.backingScaleFactor());
        layer.setBackgroundColor(Some(&CGColor::new_generic_gray(0.0, 1.0)));
        if gl().is_some_and(|g| g.2) { layer.setContentsFormat(unsafe { objc2_quartz_core::kCAContentsFormatRGBA16Float }); }
        let view = NSView::initWithFrame(NSView::alloc(mtm), content.bounds());
        view.setLayer(Some(&layer));
        view.setWantsLayer(true);
        view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
        // Below every other subview: the WebView stays on top and keeps every click.
        content.addSubview_positioned_relativeTo(&view, NSWindowOrderingMode::Below, None);
        view.setHidden(true);
        *LAYER.lock().ok()? = Some(SendLayer(layer));
        VIEW.with(|v| *v.borrow_mut() = Some(view.clone()));
        Some(view)
    }

    fn window() -> Option<Retained<NSWindow>> {
        let ptr = dioxus::desktop::window().window.ns_window();
        // SAFETY: tao's NSWindow pointer is valid while the window lives; retain our own reference.
        unsafe { Retained::retain(ptr as *mut NSWindow) }
    }

    /// The window's screen: its EDR headroom now, and the most it can reach.
    fn read_headroom() {
        let screen = window().and_then(|w| w.screen());
        let (now, potential) = screen.map_or((1.0, 1.0), |s| (s.maximumExtendedDynamicRangeColorComponentValue(), s.maximumPotentialExtendedDynamicRangeColorComponentValue()));
        HEADROOM.store(now.to_bits(), Ordering::Release);
        POTENTIAL.store(potential.to_bits(), Ordering::Release);
    }
    /// Read fresh on the main thread; elsewhere what the last read found.
    pub fn potential_headroom() -> f64 {
        if MainThreadMarker::new().is_some() { read_headroom(); }
        f64::from_bits(POTENTIAL.load(Ordering::Acquire))
    }

    pub fn attach(live: &Arc<Live>) {
        free_render();
        let ready = MainThreadMarker::new().and_then(view).map(|view| {
            read_headroom();
            match unsafe { create_render(live) } {
                Ok(render) => {
                    if let Ok(mut s) = RENDER.lock() { *s = Some(Session { _live: live.clone(), render }); }
                    HDR_MODE.store(0, Ordering::Release);
                    if let Ok(layer) = LAYER.lock() { if let Some(l) = layer.as_ref() { set_edr(&l.0, None); } }
                    DIRTY.store(true, Ordering::Release);
                    view.setHidden(false);
                    true
                }
                Err(e) => { eprintln!("veylo: video layer: {e}"); false }
            }
        });
        if ready != Some(true) { eprintln!("veylo: no video layer; playing without a picture"); }
        live.mark_ready();
    }

    /// Free mpv's renderer (before mpv itself, which our Session keeps alive until then).
    fn free_render() {
        let Some(session) = RENDER.lock().ok().and_then(|mut s| s.take()) else { return };
        unsafe { with_context(|| sys::mpv_render_context_free(session.render)); }
        drop(session);
    }

    pub fn detach() {
        free_render();
        VIEW.with(|v| if let Some(v) = v.borrow().as_ref() { v.setHidden(true); });
        DIRTY.store(true, Ordering::Release);
        set_fullscreen(false);
    }

    /// The layer's colour space and EDR, in one explicit transaction (safe from any thread).
    fn set_edr(layer: &VideoLayer, pq: Option<&objc2_core_foundation::CFString>) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        layer.setWantsExtendedDynamicRangeContent(pq.is_some());
        let space = pq.and_then(|name| CGColorSpace::with_name(Some(name)));
        layer.setColorspace(space.as_deref());
        CATransaction::commit();
    }

    /// The video's format is known (or changed): HDR video on an EDR screen goes out as PQ.
    pub fn layout(live: &Live) {
        let primaries: String = live.mpv.get_property("video-params/primaries").unwrap_or_default();
        let gamma: String = live.mpv.get_property("video-params/gamma").unwrap_or_default();
        let headroom = f64::from_bits(HEADROOM.load(Ordering::Acquire)).max(f64::from_bits(POTENTIAL.load(Ordering::Acquire)));
        let float = gl().is_some_and(|g| g.2);
        let mode = match (gamma.as_str(), primaries.as_str()) {
            _ if !float || headroom <= 1.0 || !crate::backend::hdr_output() => 0,
            ("pq" | "hlg", "bt.2020") => 1,
            ("pq" | "hlg", "display-p3") => 2,
            _ => 0,
        };
        if HDR_MODE.swap(mode, Ordering::AcqRel) == mode { return; }
        let Ok(layer) = LAYER.lock() else { return };
        let Some(layer) = layer.as_ref() else { return };
        if mode == 0 {
            set_edr(&layer.0, None);
            for (k, v) in [("target-trc", "auto"), ("target-prim", "auto"), ("target-peak", "auto")] { let _ = live.mpv.set_property(k, v); }
            return;
        }
        // SAFETY: CoreGraphics' colour space names are immutable statics.
        let name = unsafe { if mode == 1 { objc2_core_graphics::kCGColorSpaceITUR_2100_PQ } else { objc2_core_graphics::kCGColorSpaceDisplayP3_PQ } };
        set_edr(&layer.0, Some(name));
        // EDR 1.0 is SDR white (203 nits by BT.2408); mpv tone-maps what the screen can't reach.
        let peak = (headroom * 203.0).round().clamp(203.0, 10000.0) as i64;
        let _ = live.mpv.set_property("icc-profile", "");
        let _ = live.mpv.set_property("target-prim", primaries.as_str());
        let _ = live.mpv.set_property("target-trc", "pq");
        let _ = live.mpv.set_property("target-peak", peak);
    }

    pub fn set_fullscreen(on: bool) {
        let w = dioxus::desktop::window();
        w.window.set_fullscreen(on.then_some(dioxus::desktop::tao::window::Fullscreen::Borderless(None)));
    }
}

#[cfg(not(any(target_os = "android", target_os = "linux", target_os = "windows", target_os = "macos")))]
mod imp {
    use super::*;
    // No picture bridge on this OS: mpv still plays the sound.
    pub fn attach(live: &Arc<Live>) { live.mark_ready(); }
    pub fn detach() {}
    pub fn layout(_live: &Live) {}
    pub fn set_fullscreen(on: bool) {
        let w = dioxus::desktop::window();
        w.window.set_fullscreen(on.then_some(dioxus::desktop::tao::window::Fullscreen::Borderless(None)));
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use dioxus::desktop::{tao::platform::unix::WindowExtUnix, wry::WebViewExtUnix};
    use gtk::glib::translate::ToGlibPtr;
    use gtk::prelude::*;
    use libmpv2_sys as sys;
    use std::ffi::{c_char, c_void, CStr};

    fn wayland() -> bool { gtk::gdk::Display::default().is_some_and(|d| d.type_().name().contains("Wayland")) }

    pub fn attach(live: &Arc<Live>) {
        let ok = if wayland() { subsurface::attach(live) } else { glarea::attach(live) };
        if ok { let _ = dioxus::desktop::window().webview.set_background_color((0, 0, 0, 0)); }
        // Without a picture mpv still plays the sound.
        live.mark_ready();
    }
    pub fn detach() {
        if wayland() { subsurface::detach() } else { glarea::detach() }
        let _ = dioxus::desktop::window().webview.set_background_color((0, 0, 0, 255));
        set_fullscreen(false);
    }
    pub fn layout(_live: &Live) {}
    pub fn set_fullscreen(on: bool) {
        let w = dioxus::desktop::window();
        w.window.set_fullscreen(on.then_some(dioxus::desktop::tao::window::Fullscreen::Borderless(None)));
    }

    /// A GDK symbol by name (the backend-specific getters are not in gtk-rs).
    unsafe fn gdk_fn(name: &CStr) -> *mut c_void { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) }

    /// A looked-up symbol as the function-pointer type its use site names.
    unsafe fn symbol<F: Copy>(p: *mut c_void) -> F {
        assert_eq!(std::mem::size_of::<F>(), std::mem::size_of::<*mut c_void>(), "a symbol is one pointer");
        std::mem::transmute_copy(&p)
    }

    // ------------------------------------------------------------ GL entry points

    struct Gl {
        lookup: unsafe extern "C" fn(*const c_char) -> *mut c_void,
        clear_color: unsafe extern "C" fn(f32, f32, f32, f32),
        clear: unsafe extern "C" fn(u32),
        get_integerv: unsafe extern "C" fn(u32, *mut i32),
    }

    /// EGL on Wayland, GLX (or EGL) on X11; mpv resolves everything through the same lookup.
    fn gl() -> Option<&'static Gl> {
        static GL: std::sync::OnceLock<Option<Gl>> = std::sync::OnceLock::new();
        GL.get_or_init(|| unsafe {
            let candidates: &[(&CStr, &CStr)] = if wayland() { &[(c"libEGL.so.1", c"eglGetProcAddress")] }
                else { &[(c"libGL.so.1", c"glXGetProcAddressARB"), (c"libGLX.so.0", c"glXGetProcAddressARB"), (c"libEGL.so.1", c"eglGetProcAddress")] };
            let lookup = candidates.iter().find_map(|(lib, sym)| {
                let h = libc::dlopen(lib.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL);
                if h.is_null() { return None; }
                let f = libc::dlsym(h, sym.as_ptr());
                (!f.is_null()).then(|| std::mem::transmute::<*mut c_void, unsafe extern "C" fn(*const c_char) -> *mut c_void>(f))
            })?;
            let get = |name: &CStr| { let p = lookup(name.as_ptr()); (!p.is_null()).then_some(p) };
            Some(Gl {
                lookup,
                clear_color: symbol(get(c"glClearColor")?),
                clear: symbol(get(c"glClear")?),
                get_integerv: symbol(get(c"glGetIntegerv")?),
            })
        }).as_ref()
    }

    unsafe extern "C" fn proc_address(_: *mut c_void, name: *const c_char) -> *mut c_void {
        gl().map(|gl| (gl.lookup)(name)).unwrap_or(std::ptr::null_mut())
    }

    /// mpv's renderer for the current GL context; `native` lets hardware decoders hand frames to GL
    /// without a copy (VAAPI over DMA-BUF).
    unsafe fn create_render(live: &Live, native: Option<(u32, *mut c_void)>,
                            on_update: unsafe extern "C" fn(*mut c_void), cb: *mut c_void) -> Result<*mut sys::mpv_render_context, String> {
        gl().ok_or("no OpenGL entry points")?;
        let mut init = sys::mpv_opengl_init_params { get_proc_address: Some(proc_address), get_proc_address_ctx: std::ptr::null_mut() };
        let mut params = vec![
            sys::mpv_render_param { type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_API_TYPE, data: sys::MPV_RENDER_API_TYPE_OPENGL.as_ptr() as *mut c_void },
            sys::mpv_render_param { type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_INIT_PARAMS, data: &mut init as *mut _ as *mut c_void },
        ];
        if let Some((kind, ptr)) = native.filter(|(_, p)| !p.is_null()) { params.push(sys::mpv_render_param { type_: kind, data: ptr }); }
        params.push(sys::mpv_render_param { type_: 0, data: std::ptr::null_mut() });
        let mut render: *mut sys::mpv_render_context = std::ptr::null_mut();
        let err = sys::mpv_render_context_create(&mut render, live.mpv.ctx.as_ptr(), params.as_mut_ptr());
        if err < 0 { return Err(format!("mpv renderer: error {err}")); }
        sys::mpv_render_context_set_update_callback(render, Some(on_update), cb);
        Ok(render)
    }

    unsafe fn render_frame(render: *mut sys::mpv_render_context, fbo: i32, w: i32, h: i32) {
        let mut target = sys::mpv_opengl_fbo { fbo, w, h, internal_format: 0 };
        let mut flip: i32 = 1;
        let mut params = [
            sys::mpv_render_param { type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_FBO, data: &mut target as *mut _ as *mut c_void },
            sys::mpv_render_param { type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_FLIP_Y, data: &mut flip as *mut _ as *mut c_void },
            sys::mpv_render_param { type_: 0, data: std::ptr::null_mut() },
        ];
        sys::mpv_render_context_render(render, params.as_mut_ptr());
    }

    // ------------------------------------------------------------ Wayland: a subsurface below the window

    mod subsurface {
        use super::*;
        use std::cell::RefCell;
        use std::sync::{Condvar, Mutex};
        use wayland_backend::client::{Backend, ObjectId};
        use wayland_client::{delegate_noop, protocol::{wl_compositor::WlCompositor, wl_region::WlRegion, wl_registry, wl_subcompositor::WlSubcompositor,
            wl_subsurface::WlSubsurface, wl_surface::WlSurface}, Connection, Dispatch, Proxy, QueueHandle};

        #[derive(Default)]
        struct Globals { compositor: Option<WlCompositor>, subcompositor: Option<WlSubcompositor> }
        impl Dispatch<wl_registry::WlRegistry, ()> for Globals {
            fn event(state: &mut Self, registry: &wl_registry::WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
                if let wl_registry::Event::Global { name, interface, version } = event {
                    match interface.as_str() {
                        "wl_compositor" => state.compositor = Some(registry.bind(name, version.min(4), qh, ())),
                        "wl_subcompositor" => state.subcompositor = Some(registry.bind(name, 1, qh, ())),
                        _ => {}
                    }
                }
            }
        }
        delegate_noop!(Globals: ignore WlCompositor);
        delegate_noop!(Globals: ignore WlSubcompositor);
        delegate_noop!(Globals: ignore WlSurface);
        delegate_noop!(Globals: ignore WlSubsurface);
        delegate_noop!(Globals: ignore WlRegion);

        /// What the UI thread tells the render thread.
        #[derive(Default)]
        struct Control { width: i32, height: i32, frame: bool, quit: bool }
        type Shared = (Mutex<Control>, Condvar);

        struct Video {
            conn: Connection,
            surface: WlSurface,
            sub: WlSubsurface,
            compositor: WlCompositor,
            qh: QueueHandle<Globals>,
            shared: Arc<Shared>,
            thread: Option<std::thread::JoinHandle<()>>,
            resize: Option<gtk::glib::SignalHandlerId>,
            web: gtk::Widget,
        }
        thread_local! { static VIDEO: RefCell<Option<Video>> = const { RefCell::new(None) }; }

        struct SendPtr(*mut c_void);
        unsafe impl Send for SendPtr {}

        pub fn attach(live: &Arc<Live>) -> bool {
            detach();
            match unsafe { open(live) } { Ok(v) => { VIDEO.with(|s| *s.borrow_mut() = Some(v)); true } Err(e) => { eprintln!("veylo: video surface: {e}"); false } }
        }

        unsafe fn open(live: &Arc<Live>) -> Result<Video, String> {
            // Resolve the GL entry points here: that asks GDK, which only answers on this thread.
            gl().ok_or("no OpenGL entry points")?;
            let ctx = dioxus::desktop::window();
            let web: gtk::Widget = ctx.webview.webview().upcast();
            let toplevel = ctx.window.gtk_window();
            let gdk_window = toplevel.window().ok_or("window not realized")?;
            let display = gtk::gdk::Display::default().ok_or("no display")?;
            let get_display = gdk_fn(c"gdk_wayland_display_get_wl_display");
            let get_surface = gdk_fn(c"gdk_wayland_window_get_wl_surface");
            if get_display.is_null() || get_surface.is_null() { return Err("GDK Wayland symbols missing".into()); }
            let get_display: unsafe extern "C" fn(*mut gtk::gdk::ffi::GdkDisplay) -> *mut c_void = std::mem::transmute(get_display);
            let get_surface: unsafe extern "C" fn(*mut gtk::gdk::ffi::GdkWindow) -> *mut c_void = std::mem::transmute(get_surface);
            let wl_display = get_display(display.to_glib_none().0);
            let parent_ptr = get_surface(gdk_window.to_glib_none().0);
            if wl_display.is_null() || parent_ptr.is_null() { return Err("no Wayland surface".into()); }

            // GTK's own connection, our own event queue.
            let conn = Connection::from_backend(Backend::from_foreign_display(wl_display.cast()));
            let mut queue = conn.new_event_queue::<Globals>();
            let qh = queue.handle();
            let _registry = conn.display().get_registry(&qh, ());
            let mut globals = Globals::default();
            queue.roundtrip(&mut globals).map_err(|e| format!("roundtrip: {e}"))?;
            let compositor = globals.compositor.ok_or("no wl_compositor")?;
            let subcompositor = globals.subcompositor.ok_or("no wl_subcompositor")?;
            let parent = WlSurface::from_id(&conn, ObjectId::from_ptr(WlSurface::interface(), parent_ptr.cast()).map_err(|_| "bad parent surface")?)
                .map_err(|_| "bad parent surface")?;

            let surface = compositor.create_surface(&qh, ());
            // Clicks and touches go to the page above, never to the picture.
            let none = compositor.create_region(&qh, ());
            surface.set_input_region(Some(&none));
            none.destroy();
            let sub = subcompositor.get_subsurface(&surface, &parent, &qh, ());
            sub.place_below(&parent);
            sub.set_desync();

            let (x, y, w, h, scale) = geometry(&web, toplevel.upcast_ref());
            surface.set_buffer_scale(scale);
            sub.set_position(x, y);
            set_opaque(&compositor, &qh, &surface, w, h);
            let egl_window = wayland_egl::WlEglSurface::new(surface.id(), (w * scale).max(1), (h * scale).max(1)).map_err(|e| format!("wl_egl_window: {e}"))?;
            let shared: Arc<Shared> = Arc::new((Mutex::new(Control { width: w * scale, height: h * scale, frame: true, quit: false }), Condvar::new()));

            let (thread_shared, thread_live) = (shared.clone(), live.clone());
            let (display_ptr, egl_ptr) = (SendPtr(wl_display), SendPtr(egl_window.ptr() as *mut c_void));
            let egl_window = std::sync::Mutex::new(Some(egl_window));
            let egl_holder = SendEgl(egl_window);
            let thread = std::thread::Builder::new().name("veylo-video".into()).spawn(move || {
                let holder = egl_holder;
                if let Err(e) = render_thread(&thread_shared, &thread_live, display_ptr, egl_ptr, &holder) { eprintln!("veylo: video: {e}"); }
                // The EGL window goes before its wl_surface (destroyed on the UI thread after join).
                drop(holder);
                drop(thread_live);
            }).map_err(|e| e.to_string())?;

            // Follow the page's size (window resizes, full screen).
            let follow = shared.clone();
            let (c2, q2, s2, sub2) = (compositor.clone(), qh.clone(), surface.clone(), sub.clone());
            let resize = web.connect_size_allocate(move |web, _| {
                let Some(top) = web.toplevel() else { return };
                let (x, y, w, h, scale) = geometry(web, &top);
                sub2.set_position(x, y);
                s2.set_buffer_scale(scale);
                set_opaque(&c2, &q2, &s2, w, h);
                let (lock, cv) = &*follow;
                if let Ok(mut c) = lock.lock() { c.width = w * scale; c.height = h * scale; c.frame = true; cv.notify_all(); }
                top.queue_draw(); // the parent's next commit applies the new position
            });
            let _ = conn.flush();
            toplevel.queue_draw();
            Ok(Video { conn, surface, sub, compositor, qh, shared, thread: Some(thread), resize: Some(resize), web })
        }

        struct SendEgl(std::sync::Mutex<Option<wayland_egl::WlEglSurface>>);
        unsafe impl Send for SendEgl {}

        fn set_opaque(compositor: &WlCompositor, qh: &QueueHandle<Globals>, surface: &WlSurface, w: i32, h: i32) {
            let region = compositor.create_region(qh, ());
            region.add(0, 0, w, h);
            surface.set_opaque_region(Some(&region));
            region.destroy();
        }

        /// The page's place in the window's surface, in surface coordinates, and the scale.
        fn geometry(web: &gtk::Widget, top: &gtk::Widget) -> (i32, i32, i32, i32, i32) {
            let (x, y) = web.translate_coordinates(top, 0, 0).unwrap_or((0, 0));
            (x, y, web.allocated_width().max(1), web.allocated_height().max(1), web.scale_factor().max(1))
        }

        pub fn detach() {
            let Some(mut v) = VIDEO.with(|s| s.borrow_mut().take()) else { return };
            { let (lock, cv) = &*v.shared; if let Ok(mut c) = lock.lock() { c.quit = true; cv.notify_all(); } }
            if let Some(t) = v.thread.take() { let _ = t.join(); }
            if let Some(id) = v.resize.take() { v.web.disconnect(id); }
            v.sub.destroy();
            v.surface.destroy();
            let _ = (&v.compositor, &v.qh);
            let _ = v.conn.flush();
            if let Some(top) = v.web.toplevel() { top.queue_draw(); }
        }

        // -------------------------------------------------------- the render thread

        type EglFn<T> = unsafe extern "C" fn() -> T;
        struct Egl {
            get_display: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
            initialize: unsafe extern "C" fn(*mut c_void, *mut i32, *mut i32) -> u32,
            bind_api: unsafe extern "C" fn(u32) -> u32,
            choose_config: unsafe extern "C" fn(*mut c_void, *const i32, *mut *mut c_void, i32, *mut i32) -> u32,
            create_context: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const i32) -> *mut c_void,
            create_window_surface: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const isize) -> *mut c_void,
            make_current: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> u32,
            swap_buffers: unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32,
            swap_interval: unsafe extern "C" fn(*mut c_void, i32) -> u32,
            destroy_surface: unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32,
            destroy_context: unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32,
            _marker: Option<EglFn<()>>,
        }

        unsafe fn egl() -> Result<Egl, String> {
            let h = libc::dlopen(c"libEGL.so.1".as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL);
            if h.is_null() { return Err("libEGL missing".into()); }
            macro_rules! f { ($n:literal) => {{ let p = libc::dlsym(h, $n.as_ptr()); if p.is_null() { return Err(format!("{:?} missing", $n)); } symbol(p) }}; }
            Ok(Egl {
                get_display: f!(c"eglGetDisplay"), initialize: f!(c"eglInitialize"), bind_api: f!(c"eglBindAPI"),
                choose_config: f!(c"eglChooseConfig"), create_context: f!(c"eglCreateContext"),
                create_window_surface: f!(c"eglCreateWindowSurface"), make_current: f!(c"eglMakeCurrent"),
                swap_buffers: f!(c"eglSwapBuffers"), swap_interval: f!(c"eglSwapInterval"),
                destroy_surface: f!(c"eglDestroySurface"), destroy_context: f!(c"eglDestroyContext"), _marker: None,
            })
        }

        unsafe extern "C" fn on_update(cb: *mut c_void) {
            let shared = &*(cb as *const Shared);
            let (lock, cv) = shared;
            if let Ok(mut c) = lock.lock() { c.frame = true; cv.notify_all(); }
        }

        fn render_thread(shared: &Arc<Shared>, live: &Arc<Live>, display: SendPtr, window: SendPtr, egl_window: &SendEgl) -> Result<(), String> {
            unsafe {
                let e = egl()?;
                let dpy = (e.get_display)(display.0);
                if dpy.is_null() || (e.initialize)(dpy, std::ptr::null_mut(), std::ptr::null_mut()) == 0 { return Err("EGL display".into()); }
                (e.bind_api)(0x30A2); // EGL_OPENGL_API
                // Opaque 8-bit RGB: the compositor never blends the picture with what is behind the window.
                let attrs = [0x3033, 0x0004, 0x3040, 0x0008, 0x3024, 8, 0x3023, 8, 0x3022, 8, 0x3021, 0, 0x3038];
                let (mut config, mut n) = (std::ptr::null_mut(), 0);
                if (e.choose_config)(dpy, attrs.as_ptr(), &mut config, 1, &mut n) == 0 || n < 1 { return Err("EGL config".into()); }
                let ctx_attrs = [0x3098, 3, 0x30FB, 2, 0x30FD, 1, 0x3038];
                let ctx = (e.create_context)(dpy, config, std::ptr::null_mut(), ctx_attrs.as_ptr());
                if ctx.is_null() { return Err("EGL context".into()); }
                let surf = (e.create_window_surface)(dpy, config, window.0, std::ptr::null());
                if surf.is_null() { (e.destroy_context)(dpy, ctx); return Err("EGL surface".into()); }
                (e.make_current)(dpy, surf, surf, ctx);
                // mpv paces frames itself; never block on the compositor (a hidden window would stall it).
                (e.swap_interval)(dpy, 0);
                let cb = Arc::into_raw(shared.clone()) as *mut c_void;
                let native = Some((sys::mpv_render_param_type_MPV_RENDER_PARAM_WL_DISPLAY, display.0));
                let result = create_render(live, native, on_update, cb);
                let mut size = (0, 0);
                if let Ok(render) = result {
                    loop {
                        let (w, h, quit) = {
                            let (lock, cv) = &**shared;
                            let mut c = lock.lock().map_err(|_| "lock")?;
                            while !c.frame && !c.quit { c = cv.wait(c).map_err(|_| "lock")?; }
                            c.frame = false;
                            (c.width, c.height, c.quit)
                        };
                        if quit { break; }
                        if (w, h) != size {
                            size = (w, h);
                            if let Ok(win) = egl_window.0.lock() { if let Some(win) = win.as_ref() { win.resize(w, h, 0, 0); } }
                        }
                        render_frame(render, 0, w, h);
                        (e.swap_buffers)(dpy, surf);
                    }
                    sys::mpv_render_context_set_update_callback(render, None, std::ptr::null_mut());
                    sys::mpv_render_context_free(render);
                }
                drop(Arc::from_raw(cb as *const Shared));
                (e.make_current)(dpy, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
                (e.destroy_surface)(dpy, surf);
                (e.destroy_context)(dpy, ctx);
                if let Ok(mut win) = egl_window.0.lock() { win.take(); }
                result.map(|_| ())
            }
        }
    }

    // ------------------------------------------------------------ X11: a GL area under the WebView

    mod glarea {
        use super::*;
        use std::cell::RefCell;
        use std::sync::atomic::{AtomicBool, Ordering};

        struct Video {
            area: gtk::GLArea,
            /// The WebKit view over it, repainted with every frame (it has its own GDK window).
            web: gtk::Widget,
            render: *mut sys::mpv_render_context,
            /// Keeps the mpv core alive until the render context is freed (mpv requires that order).
            live: Option<Arc<Live>>,
        }
        thread_local! { static VIDEO: RefCell<Option<Video>> = const { RefCell::new(None) }; }
        static REDRAW_QUEUED: AtomicBool = AtomicBool::new(false);

        pub fn install() {
            if VIDEO.with(|v| v.borrow().is_some()) { return; }
            let ctx = dioxus::desktop::window();
            let Some(vbox) = ctx.window.default_vbox() else { return };
            let web: gtk::Widget = ctx.webview.webview().upcast();
            let area = gtk::GLArea::new();
            area.set_auto_render(false);
            area.set_has_alpha(false);
            area.connect_render(|area, _| { draw(area); gtk::glib::Propagation::Stop });
            area.connect_unrealize(|_| free_render());
            let overlay = gtk::Overlay::new();
            vbox.remove(&web);
            overlay.add(&area);
            overlay.add_overlay(&web);
            vbox.pack_start(&overlay, true, true, 0);
            overlay.show_all();
            VIDEO.with(|v| *v.borrow_mut() = Some(Video { area, web, render: std::ptr::null_mut(), live: None }));
        }

        pub fn attach(live: &Arc<Live>) -> bool {
            install();
            free_render();
            VIDEO.with(|v| {
                let mut v = v.borrow_mut();
                let Some(v) = v.as_mut() else { return false };
                // Just put in place: realize it now, which creates its GL context.
                v.area.realize();
                v.area.make_current();
                if let Some(e) = v.area.error() { eprintln!("veylo: GL unavailable: {e}"); return false; }
                let native = gtk::gdk::Display::default().map(|d| unsafe {
                    let f = gdk_fn(c"gdk_x11_display_get_xdisplay");
                    let p = if f.is_null() { std::ptr::null_mut() } else {
                        let f: unsafe extern "C" fn(*mut gtk::gdk::ffi::GdkDisplay) -> *mut c_void = std::mem::transmute(f);
                        f(d.to_glib_none().0)
                    };
                    (sys::mpv_render_param_type_MPV_RENDER_PARAM_X11_DISPLAY, p)
                });
                match unsafe { create_render(live, native, on_update, std::ptr::null_mut()) } {
                    Ok(render) => { v.render = render; v.live = Some(live.clone()); true }
                    Err(e) => { eprintln!("veylo: {e}"); false }
                }
            })
        }

        pub fn detach() { free_render(); }

        fn free_render() {
            let live = VIDEO.with(|v| {
                let mut v = v.borrow_mut();
                let v = v.as_mut()?;
                if !v.render.is_null() {
                    v.area.make_current();
                    unsafe {
                        sys::mpv_render_context_set_update_callback(v.render, None, std::ptr::null_mut());
                        sys::mpv_render_context_free(v.render);
                    }
                    v.render = std::ptr::null_mut();
                    v.area.queue_render();
                    v.web.queue_draw();
                }
                v.live.take()
            });
            // Destroying the core waits for its threads: never on the UI thread.
            if let Some(live) = live { std::thread::spawn(move || drop(live)); }
        }

        /// From mpv's threads: a frame is due. Always deferred: mpv also calls this from inside our calls.
        unsafe extern "C" fn on_update(_: *mut c_void) {
            if REDRAW_QUEUED.swap(true, Ordering::AcqRel) { return; }
            gtk::glib::idle_add_once(|| {
                REDRAW_QUEUED.store(false, Ordering::Release);
                VIDEO.with(|v| if let Ok(v) = v.try_borrow() { if let Some(v) = v.as_ref() { v.area.queue_render(); v.web.queue_draw(); } });
            });
        }

        fn draw(area: &gtk::GLArea) {
            let Some(gl) = gl() else { return };
            VIDEO.with(|v| {
                let render = v.borrow().as_ref().map(|v| v.render).unwrap_or(std::ptr::null_mut());
                unsafe {
                    if render.is_null() {
                        (gl.clear_color)(0.0, 0.0, 0.0, 1.0);
                        (gl.clear)(0x4000); // GL_COLOR_BUFFER_BIT
                        return;
                    }
                    let mut fbo = 0;
                    (gl.get_integerv)(0x8CA6, &mut fbo); // GL_DRAW_FRAMEBUFFER_BINDING: GtkGLArea's own
                    let scale = area.scale_factor();
                    render_frame(render, fbo, area.allocated_width() * scale, area.allocated_height() * scale);
                }
            });
        }
    }
}
