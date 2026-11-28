// This file is a part of media_kit
// (https://github.com/media-kit/media-kit).
//
// Copyright © 2021 & onwards, Hitesh Kumar Saini <saini123hitesh@gmail.com>.
// All rights reserved.
// Use of this source code is governed by MIT license that can be found in the
// LICENSE file.

#include "include/media_kit_video/video_output.h"
#include "include/media_kit_video/texture_gl.h"
#include "include/media_kit_video/texture_sw.h"

#include <epoxy/gl.h>
#include <gdk/gdkwayland.h>
#include <gdk/gdkx.h>

struct _VideoOutput {
  GObject parent_instance;
  TextureGL* texture_gl;
  GdkGLContext* gdk_gl_context;
  GdkDisplay* display;
  EGLDisplay egl_display;
  EGLContext egl_context;
  EGLSurface egl_draw_surface;
  EGLSurface egl_read_surface;
  guint8* pixel_buffer;
  TextureSW* texture_sw;
  GMutex mutex;
  mpv_handle* handle;
  mpv_render_context* render_context;
  gint64 width;
  gint64 height;
  VideoOutputConfiguration configuration;
  TextureUpdateCallback texture_update_callback;
  gpointer texture_update_callback_context;
  FlTextureRegistrar* texture_registrar;
  gboolean destroyed;
};

G_DEFINE_TYPE(VideoOutput, video_output, G_TYPE_OBJECT)

static void video_output_dispose(GObject* object) {
  VideoOutput* self = VIDEO_OUTPUT(object);
  g_mutex_lock(&self->mutex);
  g_atomic_int_set(&self->destroyed, TRUE);
  g_mutex_unlock(&self->mutex);
  // H/W
  if (self->texture_gl) {
    fl_texture_registrar_unregister_texture(self->texture_registrar,
                                            FL_TEXTURE(self->texture_gl));
    g_mutex_lock(&self->mutex);
    {
      VideoOutputGLContext context(self);
      if (context.is_current()) {
        if (self->render_context) {
          mpv_render_context_free(self->render_context);
        }
        texture_gl_release_resources(self->texture_gl);
      } else if (self->render_context) {
        mpv_render_context_set_update_callback(self->render_context, nullptr,
                                               nullptr);
        g_warning("media_kit: No OpenGL context available for video cleanup");
      }
      self->render_context = NULL;
    }
    g_clear_object(&self->texture_gl);
    g_mutex_unlock(&self->mutex);
  }
  // S/W
  if (self->texture_sw) {
    fl_texture_registrar_unregister_texture(self->texture_registrar,
                                            FL_TEXTURE(self->texture_sw));
    g_mutex_lock(&self->mutex);
    if (self->render_context) {
      mpv_render_context_free(self->render_context);
      self->render_context = NULL;
    }
    g_free(self->pixel_buffer);
    self->pixel_buffer = NULL;
    g_object_unref(self->texture_sw);
    self->texture_sw = NULL;
    g_mutex_unlock(&self->mutex);
  }
  g_clear_object(&self->gdk_gl_context);
  g_print("media_kit: VideoOutput: video_output_dispose: %ld\n",
          (gint64)self->handle);
  G_OBJECT_CLASS(video_output_parent_class)->dispose(object);
}

static void video_output_finalize(GObject* object) {
  g_mutex_clear(&VIDEO_OUTPUT(object)->mutex);
  G_OBJECT_CLASS(video_output_parent_class)->finalize(object);
}

static void video_output_class_init(VideoOutputClass* klass) {
  G_OBJECT_CLASS(klass)->dispose = video_output_dispose;
  G_OBJECT_CLASS(klass)->finalize = video_output_finalize;
}

static void video_output_init(VideoOutput* self) {
  self->texture_gl = NULL;
  self->gdk_gl_context = NULL;
  self->egl_display = EGL_NO_DISPLAY;
  self->egl_context = EGL_NO_CONTEXT;
  self->egl_draw_surface = EGL_NO_SURFACE;
  self->egl_read_surface = EGL_NO_SURFACE;
  self->texture_sw = NULL;
  self->pixel_buffer = NULL;
  self->handle = NULL;
  self->render_context = NULL;
  self->width = 0;
  self->height = 0;
  self->configuration = VideoOutputConfiguration{};
  self->texture_update_callback = NULL;
  self->texture_update_callback_context = NULL;
  self->texture_registrar = NULL;
  self->destroyed = FALSE;
  g_mutex_init(&self->mutex);
}

static bool video_output_register_gl(VideoOutput* self) {
  self->texture_gl = texture_gl_new(self);
  if (fl_texture_registrar_register_texture(self->texture_registrar,
                                            FL_TEXTURE(self->texture_gl))) {
    return true;
  }
  g_warning("media_kit: Could not register OpenGL video texture");
  g_clear_object(&self->texture_gl);
  return false;
}

static bool video_output_create_gl_renderer(VideoOutput* self) {
  mpv_opengl_init_params gl_init_params{
      [](void*, const char* name) { return (void*)eglGetProcAddress(name); },
      nullptr,
  };
  mpv_render_param params[] = {
      {MPV_RENDER_PARAM_API_TYPE, (void*)MPV_RENDER_API_TYPE_OPENGL},
      {MPV_RENDER_PARAM_OPENGL_INIT_PARAMS, &gl_init_params},
      {MPV_RENDER_PARAM_INVALID, nullptr},
      {MPV_RENDER_PARAM_INVALID, nullptr},
  };
  // VA-API needs the native display.
  if (GDK_IS_WAYLAND_DISPLAY(self->display)) {
    params[2] = {MPV_RENDER_PARAM_WL_DISPLAY,
                 gdk_wayland_display_get_wl_display(self->display)};

  }
  int status =
      mpv_render_context_create(&self->render_context, self->handle, params);
  if (status < 0) {
    g_warning("media_kit: Could not create OpenGL video renderer: %s",
              mpv_error_string(status));
    return false;
  }
  mpv_render_context_set_update_callback(
      self->render_context,
      [](void* data) {
        VideoOutput* self = VIDEO_OUTPUT(data);
        if (!video_output_is_destroyed(self)) {
          fl_texture_registrar_mark_texture_frame_available(
              self->texture_registrar, FL_TEXTURE(self->texture_gl));
        }
      },
      self);
  g_print("media_kit: VideoOutput: Using H/W rendering.\n");
  return true;
}

#ifdef MPV_RENDER_API_TYPE_SW
// Platform callbacks may still be queued when Dart disposes the mpv handle.
static void video_output_add_idle(VideoOutput* self, GSourceFunc callback) {
  GWeakRef* ref = g_new0(GWeakRef, 1);
  g_weak_ref_init(ref, self);
  gdk_threads_add_idle_full(G_PRIORITY_DEFAULT_IDLE, callback, ref,
                            [](gpointer data) {
                              GWeakRef* ref = static_cast<GWeakRef*>(data);
                              g_weak_ref_clear(ref);
                              g_free(ref);
                            });
}

static bool video_output_create_sw_renderer(VideoOutput* self) {
  self->texture_sw = texture_sw_new(self);
  if (!fl_texture_registrar_register_texture(self->texture_registrar,
                                             FL_TEXTURE(self->texture_sw))) {
    g_clear_object(&self->texture_sw);
    return false;
  }
  mpv_render_param params[] = {
      {MPV_RENDER_PARAM_API_TYPE, (void*)MPV_RENDER_API_TYPE_SW},
      {MPV_RENDER_PARAM_INVALID, nullptr},
  };
  int status =
      mpv_render_context_create(&self->render_context, self->handle, params);
  if (status < 0) {
    fl_texture_registrar_unregister_texture(self->texture_registrar,
                                            FL_TEXTURE(self->texture_sw));
    g_clear_object(&self->texture_sw);
    g_warning("media_kit: Could not create software video renderer: %s",
              mpv_error_string(status));
    return false;
  }

  self->width = CLAMP(self->width, 0, SW_RENDERING_MAX_WIDTH);
  self->height = CLAMP(self->height, 0, SW_RENDERING_MAX_HEIGHT);
  self->pixel_buffer = g_new0(guint8, SW_RENDERING_PIXEL_BUFFER_SIZE);
  mpv_render_context_set_update_callback(
      self->render_context,
      [](void* data) {
        VideoOutput* self = VIDEO_OUTPUT(data);
        if (video_output_is_destroyed(self)) {
          return;
        }
        video_output_add_idle(self, [](gpointer data) -> gboolean {
          g_autoptr(VideoOutput) self = static_cast<VideoOutput*>(
              g_weak_ref_get(static_cast<GWeakRef*>(data)));
          if (self == nullptr || video_output_is_destroyed(self)) {
            return G_SOURCE_REMOVE;
          }
          g_mutex_lock(&self->mutex);
          gint64 width = video_output_get_width(self);
          gint64 height = video_output_get_height(self);
          if (self->render_context && width > 0 && height > 0) {
            gint32 size[]{(gint32)width, (gint32)height};
            size_t pitch = 4 * (size_t)width;
            mpv_render_param params[]{
                {MPV_RENDER_PARAM_SW_SIZE, size},
                {MPV_RENDER_PARAM_SW_FORMAT, (void*)"rgb0"},
                {MPV_RENDER_PARAM_SW_STRIDE, &pitch},
                {MPV_RENDER_PARAM_SW_POINTER, self->pixel_buffer},
                {MPV_RENDER_PARAM_INVALID, nullptr},
            };
            if (mpv_render_context_render(self->render_context, params) == 0) {
              // mpv's rgb0 format leaves the alpha byte unspecified.
              for (size_t i = 3; i < pitch * (size_t)height; i += 4) {
                self->pixel_buffer[i] = 255;
              }
              fl_texture_registrar_mark_texture_frame_available(
                  self->texture_registrar, FL_TEXTURE(self->texture_sw));
            }
          }
          g_mutex_unlock(&self->mutex);
          return G_SOURCE_REMOVE;
        });
      },
      self);
  g_print("media_kit: VideoOutput: Using S/W rendering.\n");
  return true;
}
#endif

VideoOutput* video_output_new(FlTextureRegistrar* texture_registrar,
                              FlView* view,
                              gint64 handle,
                              VideoOutputConfiguration configuration) {
  g_print("media_kit: VideoOutput: video_output_new: %ld\n", handle);
  VideoOutput* self = VIDEO_OUTPUT(g_object_new(video_output_get_type(), NULL));
  self->texture_registrar = texture_registrar;
  self->display = gtk_widget_get_display(GTK_WIDGET(view));
  self->handle = (mpv_handle*)handle;
  self->width = configuration.width;
  self->height = configuration.height;
  self->configuration = configuration;
#ifndef MPV_RENDER_API_TYPE_SW
  // MPV_RENDER_API_TYPE_SW must be available for S/W rendering.
  if (!self->configuration.enable_hardware_acceleration) {
    g_printerr("media_kit: VideoOutput: S/W rendering is not supported.\n");
  }
  self->configuration.enable_hardware_acceleration = TRUE;
#endif
  mpv_set_option_string(self->handle, "video-sync", "audio");
  mpv_set_option_string(self->handle, "video-timing-offset", "0");
  // GTK's X11 GLX context cannot reliably share textures with Flutter's
  // raster context. Initialize mpv's software renderer before opening media.
  if (self->configuration.enable_hardware_acceleration &&
      !GDK_IS_X11_DISPLAY(self->display)) {
    GError* error = nullptr;
    GdkWindow* window = gtk_widget_get_window(GTK_WIDGET(view));
    self->gdk_gl_context = gdk_window_create_gl_context(window, &error);
    if (self->gdk_gl_context &&
        gdk_gl_context_realize(self->gdk_gl_context, &error)) {
      gdk_gl_context_make_current(self->gdk_gl_context);
      self->egl_display = eglGetCurrentDisplay();
      self->egl_context = eglGetCurrentContext();
      self->egl_draw_surface = eglGetCurrentSurface(EGL_DRAW);
      self->egl_read_surface = eglGetCurrentSurface(EGL_READ);
      if (self->egl_context != EGL_NO_CONTEXT &&
          video_output_register_gl(self) &&
          !video_output_create_gl_renderer(self)) {
        fl_texture_registrar_unregister_texture(
            texture_registrar, FL_TEXTURE(self->texture_gl));
        g_clear_object(&self->texture_gl);
      }
      // Let the raster thread bind the GDK EGL context.
      gdk_gl_context_clear_current();
      if (self->egl_display != EGL_NO_DISPLAY &&
          eglGetCurrentContext() != EGL_NO_CONTEXT) {
        eglMakeCurrent(self->egl_display, EGL_NO_SURFACE, EGL_NO_SURFACE,
                       EGL_NO_CONTEXT);
      }
    }
    if (error) {
      g_warning("media_kit: Could not initialize GDK video context: %s",
                error->message);
      g_error_free(error);
    }
  }
#ifdef MPV_RENDER_API_TYPE_SW
  if (!self->texture_gl) {
    g_clear_object(&self->gdk_gl_context);
    video_output_create_sw_renderer(self);
  }
#endif
  return self;
}

void video_output_set_texture_update_callback(
    VideoOutput* self,
    TextureUpdateCallback texture_update_callback,
    gpointer texture_update_callback_context) {
  self->texture_update_callback = texture_update_callback;
  self->texture_update_callback_context = texture_update_callback_context;
  // Notify initial dimensions as (1, 1) if |width| & |height| are 0 i.e.
  // texture & video frame size is based on playing file's resolution. This
  // will make sure that `Texture` widget on Flutter's widget tree is actually
  // mounted & |fl_texture_registrar_mark_texture_frame_available| actually
  // invokes the |TextureGL| or |TextureSW| callbacks. Otherwise it will be a
  // never ending deadlock where no video frames are ever rendered.
  gint64 texture_id = video_output_get_texture_id(self);
  if (self->width == 0 || self->height == 0) {
    self->texture_update_callback(texture_id, 1, 1,
                                  self->texture_update_callback_context);
  } else {
    self->texture_update_callback(texture_id, self->width, self->height,
                                  self->texture_update_callback_context);
  }
}

void video_output_set_size(VideoOutput* self, gint64 width, gint64 height) {
  // Ideally, a mutex should be used here & |video_output_get_width| +
  // |video_output_get_height|. However, that is throwing everything into a
  // deadlock. Flutter itself seems to have some synchronization mechanism in
  // rendering & platform channels AFAIK.

  // H/W
  if (self->texture_gl) {
    self->width = width;
    self->height = height;
  }
  // S/W
  if (self->texture_sw) {
    self->width = CLAMP(width, 0, SW_RENDERING_MAX_WIDTH);
    self->height = CLAMP(height, 0, SW_RENDERING_MAX_HEIGHT);
  }
}

mpv_render_context* video_output_get_render_context(VideoOutput* self) {
  return self->render_context;
}

guint8* video_output_get_pixel_buffer(VideoOutput* self) {
  return self->pixel_buffer;
}

gint64 video_output_get_width(VideoOutput* self) {
  // Fixed width.
  if (self->width) {
    return self->width;
  }

  // Video resolution dependent width.
  gint64 width = 0;
  gint64 height = 0;

  mpv_node params{};
  mpv_get_property(self->handle, "video-out-params", MPV_FORMAT_NODE, &params);

  int64_t dw = 0, dh = 0, rotate = 0;
  if (params.format == MPV_FORMAT_NODE_MAP) {
    for (int32_t i = 0; i < params.u.list->num; i++) {
      char* key = params.u.list->keys[i];
      auto value = params.u.list->values[i];
      if (value.format == MPV_FORMAT_INT64) {
        if (strcmp(key, "dw") == 0) {
          dw = value.u.int64;
        }
        if (strcmp(key, "dh") == 0) {
          dh = value.u.int64;
        }
        if (strcmp(key, "rotate") == 0) {
          rotate = value.u.int64;
        }
      }
    }
    mpv_free_node_contents(&params);
  }

  width = rotate == 0 || rotate == 180 ? dw : dh;
  height = rotate == 0 || rotate == 180 ? dh : dw;

  if (self->texture_sw != NULL) {
    // Make sure |width| & |height| fit between |SW_RENDERING_MAX_WIDTH| &
    // |SW_RENDERING_MAX_HEIGHT| while maintaining aspect ratio.
    if (height > SW_RENDERING_MAX_HEIGHT) {
      width = width * SW_RENDERING_MAX_HEIGHT / height;
    }
    return MIN(width, SW_RENDERING_MAX_WIDTH);
  }

  return width;
}

gint64 video_output_get_height(VideoOutput* self) {
  // Fixed height.
  if (self->width) {
    return self->height;
  }

  // Video resolution dependent height.
  gint64 width = 0;
  gint64 height = 0;

  mpv_node params{};
  mpv_get_property(self->handle, "video-out-params", MPV_FORMAT_NODE, &params);

  int64_t dw = 0, dh = 0, rotate = 0;
  if (params.format == MPV_FORMAT_NODE_MAP) {
    for (int32_t i = 0; i < params.u.list->num; i++) {
      char* key = params.u.list->keys[i];
      auto value = params.u.list->values[i];
      if (value.format == MPV_FORMAT_INT64) {
        if (strcmp(key, "dw") == 0) {
          dw = value.u.int64;
        }
        if (strcmp(key, "dh") == 0) {
          dh = value.u.int64;
        }
        if (strcmp(key, "rotate") == 0) {
          rotate = value.u.int64;
        }
      }
    }
    mpv_free_node_contents(&params);
  }

  width = rotate == 0 || rotate == 180 ? dw : dh;
  height = rotate == 0 || rotate == 180 ? dh : dw;

  if (self->texture_sw != NULL) {
    // Make sure |width| & |height| fit between |SW_RENDERING_MAX_WIDTH| &
    // |SW_RENDERING_MAX_HEIGHT| while maintaining aspect ratio.
    if (width > SW_RENDERING_MAX_WIDTH) {
      height = height * SW_RENDERING_MAX_WIDTH / width;
    }
    return MIN(height, SW_RENDERING_MAX_HEIGHT);
  }

  return height;
}

gint64 video_output_get_texture_id(VideoOutput* self) {
  // H/W
  if (self->texture_gl) {
    return (gint64)self->texture_gl;
  }
  // S/W
  if (self->texture_sw) {
    return (gint64)self->texture_sw;
  }
  return -1;
}

void video_output_notify_texture_update(VideoOutput* self) {
  gint64 id = video_output_get_texture_id(self);
  gint64 width = video_output_get_width(self);
  gint64 height = video_output_get_height(self);
  gpointer context = self->texture_update_callback_context;
  if (self->texture_update_callback != NULL) {
    self->texture_update_callback(id, width, height, context);
  }
}

void video_output_lock(VideoOutput* self) { g_mutex_lock(&self->mutex); }

void video_output_unlock(VideoOutput* self) { g_mutex_unlock(&self->mutex); }

gboolean video_output_is_destroyed(VideoOutput* self) {
  return g_atomic_int_get(&self->destroyed);
}

static gboolean video_output_make_egl_current(VideoOutput* self) {
  if (self->egl_display != EGL_NO_DISPLAY &&
      self->egl_context != EGL_NO_CONTEXT) {
    if (eglMakeCurrent(self->egl_display, self->egl_draw_surface,
                       self->egl_read_surface, self->egl_context)) {
      return TRUE;
    }
    if (eglMakeCurrent(self->egl_display, EGL_NO_SURFACE, EGL_NO_SURFACE,
                       self->egl_context)) {
      return TRUE;
    }
  }
  return FALSE;
}

VideoOutputGLContext::VideoOutputGLContext(VideoOutput* output)
    : output_(output) {
  egl_display_ = eglGetCurrentDisplay();
  egl_context_ = eglGetCurrentContext();
  egl_draw_ = eglGetCurrentSurface(EGL_DRAW);
  egl_read_ = eglGetCurrentSurface(EGL_READ);

  switched_egl_ = video_output_make_egl_current(output_);
  if (switched_egl_) {
    is_current_ = true;
    return;
  }
  GdkGLContext* gdk_context = output_->gdk_gl_context;
  if (gdk_context == nullptr) {
    return;
  }
  // GTK can still track a context after a native API has unbound it.
  if (gdk_gl_context_get_current() == gdk_context &&
      eglGetCurrentContext() == EGL_NO_CONTEXT) {
    gdk_gl_context_clear_current();
  }
  switched_gdk_ = true;
  gdk_gl_context_make_current(gdk_context);
  is_current_ = gdk_gl_context_get_current() == gdk_context &&
                eglGetCurrentContext() != EGL_NO_CONTEXT;
}

VideoOutputGLContext::~VideoOutputGLContext() { restore(); }

void VideoOutputGLContext::restore() {
  if (switched_egl_) {
    eglMakeCurrent(output_->egl_display, EGL_NO_SURFACE, EGL_NO_SURFACE,
                   EGL_NO_CONTEXT);
  } else if (switched_gdk_) {
    gdk_gl_context_clear_current();
  }
  if ((switched_egl_ || switched_gdk_) && egl_context_ != EGL_NO_CONTEXT) {
    eglMakeCurrent(egl_display_, egl_draw_, egl_read_, egl_context_);
  }
  switched_egl_ = false;
  switched_gdk_ = false;
}
