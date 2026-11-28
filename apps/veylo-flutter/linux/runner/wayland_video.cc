#include "wayland_video.h"

#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <gdk/gdkwayland.h>
#include <mpv/client.h>
#include <mpv/render.h>
#include <mpv/render_gl.h>
#include <pthread.h>
#include <wayland-client.h>
#include <wayland-egl.h>

#include <atomic>
#include <condition_variable>
#include <cstring>
#include <future>
#include <mutex>
#include <string>
#include <thread>

namespace {

struct WaylandGlobals {
  wl_compositor* compositor = nullptr;
  wl_subcompositor* subcompositor = nullptr;
};

void registry_global_cb(void* data,
                        wl_registry* registry,
                        uint32_t name,
                        const char* interface,
                        uint32_t version) {
  auto* g = static_cast<WaylandGlobals*>(data);
  if (std::strcmp(interface, wl_compositor_interface.name) == 0) {
    uint32_t v = version < 4 ? version : 4;
    g->compositor = static_cast<wl_compositor*>(
        wl_registry_bind(registry, name, &wl_compositor_interface, v));
  } else if (std::strcmp(interface, wl_subcompositor_interface.name) == 0) {
    g->subcompositor = static_cast<wl_subcompositor*>(
        wl_registry_bind(registry, name, &wl_subcompositor_interface, 1));
  }
}

void registry_global_remove_cb(void*, wl_registry*, uint32_t) {}

const wl_registry_listener kRegistryListener = {
    registry_global_cb,
    registry_global_remove_cb,
};

void* mpv_egl_get_proc_address(void*, const char* name) {
  return reinterpret_cast<void*>(eglGetProcAddress(name));
}

struct RenderShared {
  std::mutex mutex;
  std::condition_variable cv;
  int width = 1;
  int height = 1;
  bool frame = true;
  bool quit = false;
};

void mpv_update_cb(void* ctx) {
  auto* shared = static_cast<RenderShared*>(ctx);
  {
    std::lock_guard<std::mutex> lock(shared->mutex);
    shared->frame = true;
  }
  shared->cv.notify_all();
}

void set_opaque_region(wl_compositor* compositor,
                       wl_surface* surface,
                       int32_t w,
                       int32_t h) {
  if (!compositor || !surface) return;
  wl_region* region = wl_compositor_create_region(compositor);
  wl_region_add(region, 0, 0, w, h);
  wl_surface_set_opaque_region(surface, region);
  wl_region_destroy(region);
}

void get_view_geometry(GtkWidget* view,
                       GtkWidget* toplevel,
                       int* out_x,
                       int* out_y,
                       int* out_w,
                       int* out_h,
                       int* out_scale) {
  gint x = 0, y = 0;
  if (!gtk_widget_translate_coordinates(view, toplevel, 0, 0, &x, &y)) {
    x = 0;
    y = 0;
  }
  int w = gtk_widget_get_allocated_width(view);
  int h = gtk_widget_get_allocated_height(view);
  int scale = gtk_widget_get_scale_factor(view);
  *out_x = x;
  *out_y = y;
  *out_w = w > 1 ? w : 1;
  *out_h = h > 1 ? h : 1;
  *out_scale = scale > 1 ? scale : 1;
}

}  // namespace

struct _VeyloWaylandVideo {
  GtkWindow* window = nullptr;
  FlView* view = nullptr;

  wl_display* wl_dpy = nullptr;
  wl_event_queue* queue = nullptr;
  wl_compositor* compositor = nullptr;
  wl_subcompositor* subcompositor = nullptr;
  wl_surface* surface = nullptr;
  wl_subsurface* subsurface = nullptr;
  wl_egl_window* egl_window = nullptr;

  std::shared_ptr<RenderShared> shared;
  std::thread render_thread;
  gulong size_alloc_handler = 0;
  gulong scale_notify_handler = 0;
  bool attached = false;
  bool has_rect = false;
  int rect_x = 0;
  int rect_y = 0;
  int rect_w = 1;
  int rect_h = 1;
};

static void compute_target_geometry(VeyloWaylandVideo* self,
                                    int* out_x,
                                    int* out_y,
                                    int* out_w,
                                    int* out_h,
                                    int* out_scale) {
  int vx = 0, vy = 0, vw = 1, vh = 1, scale = 1;
  get_view_geometry(GTK_WIDGET(self->view), GTK_WIDGET(self->window), &vx, &vy,
                    &vw, &vh, &scale);
  if (self->has_rect && self->rect_w > 1 && self->rect_h > 1) {
    *out_x = vx + self->rect_x;
    *out_y = vy + self->rect_y;
    *out_w = self->rect_w;
    *out_h = self->rect_h;
  } else {
    *out_x = vx;
    *out_y = vy;
    *out_w = vw;
    *out_h = vh;
  }
  *out_scale = scale;
}

static void apply_geometry(VeyloWaylandVideo* self) {
  if (!self->attached || !self->view || !self->window || !self->surface ||
      !self->subsurface) {
    return;
  }
  int x = 0, y = 0, w = 1, h = 1, scale = 1;
  compute_target_geometry(self, &x, &y, &w, &h, &scale);
  wl_subsurface_set_position(self->subsurface, x, y);
  wl_surface_set_buffer_scale(self->surface, scale);
  set_opaque_region(self->compositor, self->surface, w, h);
  if (self->wl_dpy) {
    wl_display_flush(self->wl_dpy);
  }
  if (self->shared) {
    {
      std::lock_guard<std::mutex> lock(self->shared->mutex);
      self->shared->width = w * scale;
      self->shared->height = h * scale;
      self->shared->frame = true;
    }
    self->shared->cv.notify_all();
  }
  gtk_widget_queue_draw(GTK_WIDGET(self->window));
}

void veylo_wayland_video_set_rect(VeyloWaylandVideo* self,
                                  gboolean active,
                                  int x,
                                  int y,
                                  int width,
                                  int height) {
  if (!self) return;
  self->has_rect = active != FALSE;
  self->rect_x = x;
  self->rect_y = y;
  self->rect_w = width > 1 ? width : 1;
  self->rect_h = height > 1 ? height : 1;
  if (self->attached) {
    apply_geometry(self);
  }
}

static void on_size_allocate(GtkWidget*, GtkAllocation*, gpointer user_data) {
  apply_geometry(static_cast<VeyloWaylandVideo*>(user_data));
}

static void on_scale_notify(GObject*, GParamSpec*, gpointer user_data) {
  apply_geometry(static_cast<VeyloWaylandVideo*>(user_data));
}

VeyloWaylandVideo* veylo_wayland_video_new(GtkWindow* window, FlView* view) {
  auto* self = new VeyloWaylandVideo();
  self->window = window;
  self->view = view;
  return self;
}

gboolean veylo_wayland_video_attach(VeyloWaylandVideo* self,
                                    int64_t mpv_handle_ptr,
                                    char** error_out) {
  if (!self || mpv_handle_ptr == 0) {
    if (error_out) *error_out = g_strdup("Invalid mpv handle");
    return FALSE;
  }
  veylo_wayland_video_detach(self);

  GdkDisplay* gdk_dpy = gdk_display_get_default();
  if (!gdk_dpy || !GDK_IS_WAYLAND_DISPLAY(gdk_dpy)) {
    if (error_out) *error_out = g_strdup("Not a Wayland display");
    return FALSE;
  }
  GdkWindow* gdk_win = gtk_widget_get_window(GTK_WIDGET(self->window));
  if (!gdk_win || !GDK_IS_WAYLAND_WINDOW(gdk_win)) {
    if (error_out) *error_out = g_strdup("Window has no Wayland surface");
    return FALSE;
  }

  wl_display* wl_dpy = gdk_wayland_display_get_wl_display(gdk_dpy);
  wl_surface* parent = gdk_wayland_window_get_wl_surface(gdk_win);
  if (!wl_dpy || !parent) {
    if (error_out) *error_out = g_strdup("Missing Wayland display or surface");
    return FALSE;
  }

  wl_event_queue* queue = wl_display_create_queue(wl_dpy);
  if (!queue) {
    if (error_out) *error_out = g_strdup("Failed to create Wayland event queue");
    return FALSE;
  }

  auto* dpy_wrapper =
      static_cast<wl_display*>(wl_proxy_create_wrapper(wl_dpy));
  wl_proxy_set_queue(reinterpret_cast<wl_proxy*>(dpy_wrapper), queue);

  WaylandGlobals globals;
  wl_registry* registry = wl_display_get_registry(dpy_wrapper);
  wl_registry_add_listener(registry, &kRegistryListener, &globals);
  wl_display_roundtrip_queue(wl_dpy, queue);
  wl_registry_destroy(registry);
  wl_proxy_wrapper_destroy(dpy_wrapper);

  if (!globals.compositor || !globals.subcompositor) {
    if (globals.compositor) wl_compositor_destroy(globals.compositor);
    if (globals.subcompositor) wl_subcompositor_destroy(globals.subcompositor);
    wl_event_queue_destroy(queue);
    if (error_out) *error_out = g_strdup("Missing wl_compositor/subcompositor");
    return FALSE;
  }

  wl_surface* surface = wl_compositor_create_surface(globals.compositor);
  wl_region* empty_input = wl_compositor_create_region(globals.compositor);
  wl_surface_set_input_region(surface, empty_input);
  wl_region_destroy(empty_input);

  wl_subsurface* sub = wl_subcompositor_get_subsurface(
      globals.subcompositor, surface, parent);
  // Stack immediately above the opaque GTK toplevel parent, which keeps us
  // strictly BELOW Flutter's already-placed transparent FlViewRendererSubsurface.
  wl_subsurface_place_above(sub, parent);
  wl_subsurface_set_desync(sub);

  int x = 0, y = 0, w = 1, h = 1, scale = 1;
  compute_target_geometry(self, &x, &y, &w, &h, &scale);
  wl_surface_set_buffer_scale(surface, scale);
  wl_subsurface_set_position(sub, x, y);
  set_opaque_region(globals.compositor, surface, w, h);

  int phys_w = w * scale;
  int phys_h = h * scale;
  wl_egl_window* egl_win = wl_egl_window_create(surface, phys_w, phys_h);
  if (!egl_win) {
    wl_subsurface_destroy(sub);
    wl_surface_destroy(surface);
    wl_subcompositor_destroy(globals.subcompositor);
    wl_compositor_destroy(globals.compositor);
    wl_event_queue_destroy(queue);
    if (error_out) *error_out = g_strdup("wl_egl_window_create failed");
    return FALSE;
  }

  auto shared = std::make_shared<RenderShared>();
  shared->width = phys_w;
  shared->height = phys_h;
  shared->frame = true;
  shared->quit = false;

  std::promise<std::string> init_promise;
  auto init_future = init_promise.get_future();
  auto* mpv = reinterpret_cast<mpv_handle*>(mpv_handle_ptr);

  self->wl_dpy = wl_dpy;
  self->queue = queue;
  self->compositor = globals.compositor;
  self->subcompositor = globals.subcompositor;
  self->surface = surface;
  self->subsurface = sub;
  self->egl_window = egl_win;
  self->shared = shared;

  self->render_thread = std::thread([wl_dpy, egl_win, mpv, shared,
                                     init_promise = std::move(init_promise)]() mutable {
    pthread_setname_np(pthread_self(), "veylo-video");

    EGLDisplay egl_dpy = eglGetDisplay(reinterpret_cast<EGLNativeDisplayType>(wl_dpy));
    if (egl_dpy == EGL_NO_DISPLAY || !eglInitialize(egl_dpy, nullptr, nullptr)) {
      init_promise.set_value("eglInitialize failed");
      return;
    }

    // Prefer desktop OpenGL (matches crates/video-surface), fallback to GLES3.
    EGLint api = EGL_OPENGL_API;
    EGLint renderable = EGL_OPENGL_BIT;
    if (!eglBindAPI(EGL_OPENGL_API)) {
      api = EGL_OPENGL_ES_API;
      renderable = EGL_OPENGL_ES3_BIT;
      if (!eglBindAPI(EGL_OPENGL_ES_API)) {
        init_promise.set_value("eglBindAPI failed");
        return;
      }
    }

    const EGLint cfg_attrs[] = {
        EGL_SURFACE_TYPE, EGL_WINDOW_BIT,
        EGL_RENDERABLE_TYPE, renderable,
        EGL_RED_SIZE, 8,
        EGL_GREEN_SIZE, 8,
        EGL_BLUE_SIZE, 8,
        EGL_ALPHA_SIZE, 0,
        EGL_NONE,
    };
    EGLConfig config = nullptr;
    EGLint num_cfg = 0;
    if (!eglChooseConfig(egl_dpy, cfg_attrs, &config, 1, &num_cfg) ||
        num_cfg < 1) {
      init_promise.set_value("eglChooseConfig failed");
      return;
    }

    const EGLint gl_ctx_attrs[] = {
        EGL_CONTEXT_MAJOR_VERSION, 3,
        EGL_CONTEXT_MINOR_VERSION, 2,
        EGL_CONTEXT_OPENGL_PROFILE_MASK, EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT,
        EGL_NONE,
    };
    const EGLint gles_ctx_attrs[] = {
        EGL_CONTEXT_CLIENT_VERSION, 3,
        EGL_NONE,
    };
    EGLContext ctx = eglCreateContext(
        egl_dpy, config, EGL_NO_CONTEXT,
        api == EGL_OPENGL_API ? gl_ctx_attrs : gles_ctx_attrs);
    if (ctx == EGL_NO_CONTEXT) {
      init_promise.set_value("eglCreateContext failed");
      return;
    }

    EGLSurface egl_surf = eglCreateWindowSurface(
        egl_dpy, config, reinterpret_cast<EGLNativeWindowType>(egl_win),
        nullptr);
    if (egl_surf == EGL_NO_SURFACE) {
      eglDestroyContext(egl_dpy, ctx);
      init_promise.set_value("eglCreateWindowSurface failed");
      return;
    }

    if (!eglMakeCurrent(egl_dpy, egl_surf, egl_surf, ctx)) {
      eglDestroySurface(egl_dpy, egl_surf);
      eglDestroyContext(egl_dpy, ctx);
      init_promise.set_value("eglMakeCurrent failed");
      return;
    }

    // mpv paces frames itself; never block on compositor swap interval.
    eglSwapInterval(egl_dpy, 0);

    mpv_opengl_init_params gl_init = {mpv_egl_get_proc_address, nullptr};
    mpv_render_param create_params[] = {
        {MPV_RENDER_PARAM_API_TYPE, const_cast<char*>(MPV_RENDER_API_TYPE_OPENGL)},
        {MPV_RENDER_PARAM_OPENGL_INIT_PARAMS, &gl_init},
        {MPV_RENDER_PARAM_WL_DISPLAY, wl_dpy},
        {MPV_RENDER_PARAM_INVALID, nullptr},
    };
    mpv_render_context* render = nullptr;
    int rc = mpv_render_context_create(&render, mpv, create_params);
    if (rc < 0 || !render) {
      eglMakeCurrent(egl_dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
      eglDestroySurface(egl_dpy, egl_surf);
      eglDestroyContext(egl_dpy, ctx);
      init_promise.set_value("mpv_render_context_create failed: " +
                             std::to_string(rc));
      return;
    }

    mpv_render_context_set_update_callback(render, mpv_update_cb, shared.get());
    g_print("veylo: Wayland subsurface video active (%dx%d)\n",
            shared->width, shared->height);
    init_promise.set_value("");

    int cur_w = 0;
    int cur_h = 0;
    while (true) {
      int w = 1, h = 1;
      bool quit = false;
      {
        std::unique_lock<std::mutex> lock(shared->mutex);
        shared->cv.wait(lock, [&] { return shared->frame || shared->quit; });
        shared->frame = false;
        w = shared->width;
        h = shared->height;
        quit = shared->quit;
      }
      if (quit) break;

      bool resized = (w != cur_w || h != cur_h);
      if (resized) {
        cur_w = w;
        cur_h = h;
        wl_egl_window_resize(egl_win, w, h, 0, 0);
      }

      uint64_t flags = mpv_render_context_update(render);
      if (resized || (flags & MPV_RENDER_UPDATE_FRAME)) {
        mpv_opengl_fbo fbo{0, w, h, 0};
        int flip_y = 1;
        mpv_render_param rparams[] = {
            {MPV_RENDER_PARAM_OPENGL_FBO, &fbo},
            {MPV_RENDER_PARAM_FLIP_Y, &flip_y},
            {MPV_RENDER_PARAM_INVALID, nullptr},
        };
        mpv_render_context_render(render, rparams);
        eglSwapBuffers(egl_dpy, egl_surf);
        mpv_render_context_report_swap(render);
      }
    }

    mpv_render_context_set_update_callback(render, nullptr, nullptr);
    mpv_render_context_free(render);
    eglMakeCurrent(egl_dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
    eglDestroySurface(egl_dpy, egl_surf);
    eglDestroyContext(egl_dpy, ctx);
  });

  std::string err = init_future.get();
  if (!err.empty()) {
    if (self->render_thread.joinable()) self->render_thread.join();
    wl_egl_window_destroy(self->egl_window);
    wl_subsurface_destroy(self->subsurface);
    wl_surface_destroy(self->surface);
    wl_subcompositor_destroy(self->subcompositor);
    wl_compositor_destroy(self->compositor);
    wl_event_queue_destroy(self->queue);
    self->egl_window = nullptr;
    self->subsurface = nullptr;
    self->surface = nullptr;
    self->subcompositor = nullptr;
    self->compositor = nullptr;
    self->queue = nullptr;
    self->shared.reset();
    if (error_out) *error_out = g_strdup(err.c_str());
    return FALSE;
  }

  self->attached = true;
  self->size_alloc_handler = g_signal_connect(
      self->view, "size-allocate", G_CALLBACK(on_size_allocate), self);
  self->scale_notify_handler = g_signal_connect(
      self->view, "notify::scale-factor", G_CALLBACK(on_scale_notify), self);
  wl_display_flush(wl_dpy);
  gtk_widget_queue_draw(GTK_WIDGET(self->window));
  return TRUE;
}

void veylo_wayland_video_detach(VeyloWaylandVideo* self) {
  if (!self) return;
  self->has_rect = false;
  if (!self->attached) return;
  self->attached = false;

  if (self->size_alloc_handler && self->view) {
    g_signal_handler_disconnect(self->view, self->size_alloc_handler);
    self->size_alloc_handler = 0;
  }
  if (self->scale_notify_handler && self->view) {
    g_signal_handler_disconnect(self->view, self->scale_notify_handler);
    self->scale_notify_handler = 0;
  }
  if (self->shared) {
    {
      std::lock_guard<std::mutex> lock(self->shared->mutex);
      self->shared->quit = true;
    }
    self->shared->cv.notify_all();
  }
  if (self->render_thread.joinable()) {
    self->render_thread.join();
  }
  self->shared.reset();

  if (self->egl_window) {
    wl_egl_window_destroy(self->egl_window);
    self->egl_window = nullptr;
  }
  if (self->subsurface) {
    wl_subsurface_destroy(self->subsurface);
    self->subsurface = nullptr;
  }
  if (self->surface) {
    wl_surface_destroy(self->surface);
    self->surface = nullptr;
  }
  if (self->subcompositor) {
    wl_subcompositor_destroy(self->subcompositor);
    self->subcompositor = nullptr;
  }
  if (self->compositor) {
    wl_compositor_destroy(self->compositor);
    self->compositor = nullptr;
  }
  if (self->queue) {
    wl_event_queue_destroy(self->queue);
    self->queue = nullptr;
  }
  if (self->wl_dpy) {
    wl_display_flush(self->wl_dpy);
    self->wl_dpy = nullptr;
  }
  if (self->window) {
    gtk_widget_queue_draw(GTK_WIDGET(self->window));
  }
  g_print("veylo: Wayland subsurface video detached\n");
}

void veylo_wayland_video_free(VeyloWaylandVideo* self) {
  if (!self) return;
  veylo_wayland_video_detach(self);
  delete self;
}
