//! Linux: the picture on a Wayland subsurface under the window, or in a GL area under the web view.
use crate::{Host, Path, Player};
use std::sync::Arc;
use gtk::glib::translate::ToGlibPtr;
use gtk::prelude::*;
use libmpv2_sys as sys;
use std::ffi::{c_char, c_void, CStr};

fn wayland() -> bool { gtk::gdk::Display::default().is_some_and(|d| d.type_().name().contains("Wayland")) }

pub fn candidates() -> &'static [Path] { crate::fallback::order("linux", wayland()) }

pub fn open(path: Path, host: &Host, player: &Arc<dyn Player>) -> Result<(), String> {
    match path {
        Path::WaylandSubsurface => subsurface::open(host, player),
        Path::GlArea => glarea::open(host, player),
        _ => Err("not a Linux path".into()),
    }
}

pub fn close(path: Path) {
    match path {
        Path::WaylandSubsurface => subsurface::close(),
        Path::GlArea => glarea::close(),
        _ => {}
    }
}

pub fn layout(_player: &dyn Player, _hdr_output: bool) {}

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
unsafe fn create_render(player: &dyn Player, native: Option<(u32, *mut c_void)>,
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
    let err = sys::mpv_render_context_create(&mut render, player.mpv().ctx.as_ptr(), params.as_mut_ptr());
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

    pub fn open(host: &Host, player: &Arc<dyn Player>) -> Result<(), String> {
        close();
        let video = unsafe { start(host, player) }?;
        VIDEO.with(|s| *s.borrow_mut() = Some(video));
        Ok(())
    }

    unsafe fn start(host: &Host, player: &Arc<dyn Player>) -> Result<Video, String> {
        // Resolve the GL entry points here: that asks GDK, which only answers on this thread.
        gl().ok_or("no OpenGL entry points")?;
        let web = host.web.clone();
        let toplevel = host.window.clone();
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

        let (thread_shared, thread_player) = (shared.clone(), player.clone());
        let (display_ptr, egl_ptr) = (SendPtr(wl_display), SendPtr(egl_window.ptr() as *mut c_void));
        let egl_window = std::sync::Mutex::new(Some(egl_window));
        let egl_holder = SendEgl(egl_window);
        let (init_tx, init_rx) = std::sync::mpsc::sync_channel::<Result<(), String>>(1);
        let thread = std::thread::Builder::new().name("veylo-video".into()).spawn(move || {
            let holder = egl_holder;
            if let Err(e) = render_thread(&thread_shared, &thread_player, display_ptr, egl_ptr, &holder, init_tx) { eprintln!("veylo: video: {e}"); }
            // The EGL window goes before its wl_surface (destroyed on the UI thread after join).
            drop(holder);
            drop(thread_player);
        }).map_err(|e| e.to_string())?;
        // Wait for EGL + mpv_render_context_create so a failure triggers fallback to GlArea.
        match init_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = thread.join();
                sub.destroy();
                surface.destroy();
                let _ = conn.flush();
                return Err(e);
            }
            Err(_) => {
                let _ = thread.join();
                sub.destroy();
                surface.destroy();
                let _ = conn.flush();
                return Err("render thread exited during init".into());
            }
        }

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

    pub fn close() {
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

    fn render_thread(shared: &Arc<Shared>, player: &Arc<dyn Player>, display: SendPtr, window: SendPtr, egl_window: &SendEgl,
                     init_tx: std::sync::mpsc::SyncSender<Result<(), String>>) -> Result<(), String> {
        unsafe {
            let e = match egl() { Ok(e) => e, Err(err) => { let _ = init_tx.send(Err(err.clone())); return Err(err); } };
            let dpy = (e.get_display)(display.0);
            if dpy.is_null() || (e.initialize)(dpy, std::ptr::null_mut(), std::ptr::null_mut()) == 0 {
                let err = "EGL display".to_owned(); let _ = init_tx.send(Err(err.clone())); return Err(err);
            }
            (e.bind_api)(0x30A2); // EGL_OPENGL_API
            // Opaque 8-bit RGB: the compositor never blends the picture with what is behind the window.
            let attrs = [0x3033, 0x0004, 0x3040, 0x0008, 0x3024, 8, 0x3023, 8, 0x3022, 8, 0x3021, 0, 0x3038];
            let (mut config, mut n) = (std::ptr::null_mut(), 0);
            if (e.choose_config)(dpy, attrs.as_ptr(), &mut config, 1, &mut n) == 0 || n < 1 {
                let err = "EGL config".to_owned(); let _ = init_tx.send(Err(err.clone())); return Err(err);
            }
            let ctx_attrs = [0x3098, 3, 0x30FB, 2, 0x30FD, 1, 0x3038];
            let ctx = (e.create_context)(dpy, config, std::ptr::null_mut(), ctx_attrs.as_ptr());
            if ctx.is_null() {
                let err = "EGL context".to_owned(); let _ = init_tx.send(Err(err.clone())); return Err(err);
            }
            let surf = (e.create_window_surface)(dpy, config, window.0, std::ptr::null());
            if surf.is_null() {
                (e.destroy_context)(dpy, ctx);
                let err = "EGL surface".to_owned(); let _ = init_tx.send(Err(err.clone())); return Err(err);
            }
            (e.make_current)(dpy, surf, surf, ctx);
            // mpv paces frames itself; never block on the compositor (a hidden window would stall it).
            (e.swap_interval)(dpy, 0);
            let cb = Arc::into_raw(shared.clone()) as *mut c_void;
            let native = Some((sys::mpv_render_param_type_MPV_RENDER_PARAM_WL_DISPLAY, display.0));
            let result = create_render(&**player, native, on_update, cb);
            let _ = init_tx.send(result.as_ref().map(|_| ()).map_err(Clone::clone));
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
                    let resized = (w, h) != size;
                    if resized {
                        size = (w, h);
                        if let Ok(win) = egl_window.0.lock() { if let Some(win) = win.as_ref() { win.resize(w, h, 0, 0); } }
                    }
                    let flags = sys::mpv_render_context_update(render);
                    if resized || (flags & sys::mpv_render_update_flag_MPV_RENDER_UPDATE_FRAME as u64) != 0 {
                        render_frame(render, 0, w, h);
                        (e.swap_buffers)(dpy, surf);
                        sys::mpv_render_context_report_swap(render);
                    }
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
        player: Option<Arc<dyn Player>>,
    }
    thread_local! { static VIDEO: RefCell<Option<Video>> = const { RefCell::new(None) }; }
    static REDRAW_QUEUED: AtomicBool = AtomicBool::new(false);

    /// Put the GL area under the web view, once: the web view moves into an overlay above it.
    fn install(host: &Host) -> Result<(), String> {
        if VIDEO.with(|v| v.borrow().is_some()) { return Ok(()); }
        let vbox = host.vbox.clone().ok_or("no box holding the web view")?;
        let web = host.web.clone();
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
        VIDEO.with(|v| *v.borrow_mut() = Some(Video { area, web, render: std::ptr::null_mut(), player: None }));
        Ok(())
    }

    pub fn open(host: &Host, player: &Arc<dyn Player>) -> Result<(), String> {
        install(host)?;
        free_render();
        VIDEO.with(|v| {
            let mut v = v.borrow_mut();
            let v = v.as_mut().ok_or("no GL area")?;
            // Just put in place: realize it now, which creates its GL context.
            v.area.realize();
            v.area.make_current();
            if let Some(e) = v.area.error() { return Err(format!("GL unavailable: {e}")); }
            // The display mpv's hardware decoders share frames on: X11's, or Wayland's when this is
            // the Wayland fallback (never ask a Wayland display for its X11 one).
            let (getter, kind) = if wayland() {
                (c"gdk_wayland_display_get_wl_display", sys::mpv_render_param_type_MPV_RENDER_PARAM_WL_DISPLAY)
            } else {
                (c"gdk_x11_display_get_xdisplay", sys::mpv_render_param_type_MPV_RENDER_PARAM_X11_DISPLAY)
            };
            let native = gtk::gdk::Display::default().map(|d| unsafe {
                let f = gdk_fn(getter);
                let p = if f.is_null() { std::ptr::null_mut() } else {
                    let f: unsafe extern "C" fn(*mut gtk::gdk::ffi::GdkDisplay) -> *mut c_void = std::mem::transmute(f);
                    f(d.to_glib_none().0)
                };
                (kind, p)
            });
            let render = unsafe { create_render(&**player, native, on_update, std::ptr::null_mut()) }?;
            v.render = render;
            v.player = Some(player.clone());
            Ok(())
        })
    }

    pub fn close() { free_render(); }

    fn free_render() {
        let player = VIDEO.with(|v| {
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
            v.player.take()
        });
        // Destroying the core waits for its threads: never on the UI thread.
        if let Some(player) = player { std::thread::spawn(move || drop(player)); }
    }

    /// From mpv's threads: a frame is due. Always deferred: mpv also calls this from inside our calls.
    unsafe extern "C" fn on_update(_: *mut c_void) {
        if REDRAW_QUEUED.swap(true, Ordering::AcqRel) { return; }
        gtk::glib::idle_add_once(|| {
            REDRAW_QUEUED.store(false, Ordering::Release);
            VIDEO.with(|v| if let Ok(v) = v.try_borrow() {
                if let Some(v) = v.as_ref().filter(|v| !v.render.is_null()) { v.area.queue_render(); v.web.queue_draw(); }
            });
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
                let _ = sys::mpv_render_context_update(render);
                render_frame(render, fbo, area.allocated_width() * scale, area.allocated_height() * scale);
                sys::mpv_render_context_report_swap(render);
            }
        });
    }
}
