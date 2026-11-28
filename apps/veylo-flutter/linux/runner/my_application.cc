#include "my_application.h"
#include "wayland_video.h"

#include <cstdlib>
#include <cstring>
#include <flutter_linux/flutter_linux.h>
#include <media_kit_libs_linux/media_kit_libs_linux_plugin.h>
#include <media_kit_video/media_kit_video_plugin.h>

#include "flutter/generated_plugin_registrant.h"

struct _MyApplication {
  GtkApplication parent_instance;
  char** dart_entrypoint_arguments;
  GtkWindow* window;
  gboolean fullscreen;
  FlMethodChannel* window_channel;
  FlMethodChannel* video_channel;
  VeyloWaylandVideo* wayland_video;
};

G_DEFINE_TYPE(MyApplication, my_application, GTK_TYPE_APPLICATION)

static gboolean on_window_state_event(GtkWidget* widget,
                                      GdkEventWindowState* event,
                                      gpointer user_data) {
  MyApplication* self = MY_APPLICATION(user_data);
  self->fullscreen =
      (event->new_window_state & GDK_WINDOW_STATE_FULLSCREEN) != 0;
  return FALSE;
}

static void window_channel_call_cb(FlMethodChannel* channel,
                                   FlMethodCall* method_call,
                                   gpointer user_data) {
  MyApplication* self = MY_APPLICATION(user_data);
  const gchar* method = fl_method_call_get_name(method_call);
  g_autoptr(FlMethodResponse) response = nullptr;

  if (std::strcmp(method, "setFullscreen") == 0) {
    FlValue* args = fl_method_call_get_args(method_call);
    gboolean enable = (fl_value_get_type(args) == FL_VALUE_TYPE_BOOL)
                          ? fl_value_get_bool(args)
                          : FALSE;
    if (self->window != nullptr) {
      if (enable) {
        gtk_window_fullscreen(self->window);
      } else {
        gtk_window_unfullscreen(self->window);
      }
      self->fullscreen = enable;
    }
    g_autoptr(FlValue) result = fl_value_new_bool(self->fullscreen);
    response = FL_METHOD_RESPONSE(fl_method_success_response_new(result));
  } else if (std::strcmp(method, "isFullscreen") == 0) {
    g_autoptr(FlValue) result = fl_value_new_bool(self->fullscreen);
    response = FL_METHOD_RESPONSE(fl_method_success_response_new(result));
  } else {
    response = FL_METHOD_RESPONSE(fl_method_not_implemented_response_new());
  }

  fl_method_call_respond(method_call, response, nullptr);
}

static void video_channel_call_cb(FlMethodChannel* channel,
                                  FlMethodCall* method_call,
                                  gpointer user_data) {
  MyApplication* self = MY_APPLICATION(user_data);
  const gchar* method = fl_method_call_get_name(method_call);
  g_autoptr(FlMethodResponse) response = nullptr;

  if (std::strcmp(method, "attach") == 0) {
    FlValue* args = fl_method_call_get_args(method_call);
    int64_t handle = (fl_value_get_type(args) == FL_VALUE_TYPE_INT)
                         ? fl_value_get_int(args)
                         : 0;
    char* err = nullptr;
    gboolean ok =
        veylo_wayland_video_attach(self->wayland_video, handle, &err);
    if (!ok && err != nullptr) {
      g_printerr("veylo: Wayland video attach fallback: %s\n", err);
      g_free(err);
    }
    g_autoptr(FlValue) result = fl_value_new_bool(ok);
    response = FL_METHOD_RESPONSE(fl_method_success_response_new(result));
  } else if (std::strcmp(method, "setRect") == 0) {
    FlValue* args = fl_method_call_get_args(method_call);
    if (args != nullptr && fl_value_get_type(args) == FL_VALUE_TYPE_MAP) {
      auto get_i = [&](const char* k) -> int {
        FlValue* v = fl_value_lookup_string(args, k);
        if (!v) return 0;
        if (fl_value_get_type(v) == FL_VALUE_TYPE_INT) return static_cast<int>(fl_value_get_int(v));
        if (fl_value_get_type(v) == FL_VALUE_TYPE_FLOAT) return static_cast<int>(fl_value_get_float(v) + 0.5);
        return 0;
      };
      veylo_wayland_video_set_rect(self->wayland_video, TRUE, get_i("x"),
                                   get_i("y"), get_i("w"), get_i("h"));
    } else {
      veylo_wayland_video_set_rect(self->wayland_video, FALSE, 0, 0, 1, 1);
    }
    g_autoptr(FlValue) result = fl_value_new_bool(TRUE);
    response = FL_METHOD_RESPONSE(fl_method_success_response_new(result));
  } else if (std::strcmp(method, "detach") == 0) {
    veylo_wayland_video_detach(self->wayland_video);
    g_autoptr(FlValue) result = fl_value_new_bool(TRUE);
    response = FL_METHOD_RESPONSE(fl_method_success_response_new(result));
  } else {
    response = FL_METHOD_RESPONSE(fl_method_not_implemented_response_new());
  }

  fl_method_call_respond(method_call, response, nullptr);
}

// Called when first Flutter frame received.
static void first_frame_cb(MyApplication* self, FlView* view) {
  gtk_widget_show(gtk_widget_get_toplevel(GTK_WIDGET(view)));
}

// Implements GApplication::activate.
static void my_application_activate(GApplication* application) {
  MyApplication* self = MY_APPLICATION(application);

  GtkWindow* window =
      GTK_WINDOW(gtk_application_window_new(GTK_APPLICATION(application)));
  self->window = window;
  self->fullscreen = FALSE;
  g_signal_connect(window, "window-state-event",
                   G_CALLBACK(on_window_state_event), self);
  gtk_window_set_default_size(window, 1360, 860);
  gtk_window_set_title(window, "Veylo");

  g_autoptr(FlDartProject) project = fl_dart_project_new();
  // Work around Flutter 3.50.1 Impeller OpenGLESSDF FP16 gradient overflow (#196865)
  // on Linux ARM64/Asahi by defaulting to Skia unless VEYLO_RENDERER=impeller is set.
  // Note: media_kit's Linux GL video texture path is also verified on Skia; impeller is opt-in.
  const char* renderer = std::getenv("VEYLO_RENDERER");
  const gboolean use_impeller =
      (renderer != nullptr && std::strcmp(renderer, "impeller") == 0) ? TRUE
                                                                       : FALSE;
  fl_dart_project_set_enable_impeller(project, use_impeller);
  fl_dart_project_set_dart_entrypoint_arguments(
      project, self->dart_entrypoint_arguments);

  FlView* view = fl_view_new(project);
  GdkRGBA background_color;
  // Background defaults to black, override it here if necessary, e.g. #00000000
  // for transparent.
  gdk_rgba_parse(&background_color, "#000000");
  fl_view_set_background_color(view, &background_color);
  gtk_widget_show(GTK_WIDGET(view));
  gtk_container_add(GTK_CONTAINER(window), GTK_WIDGET(view));

  // Show the window when Flutter renders.
  // Requires the view to be realized so we can start rendering.
  g_signal_connect_swapped(view, "first-frame", G_CALLBACK(first_frame_cb),
                           self);
  gtk_widget_realize(GTK_WIDGET(view));

  // Register media_kit plugins directly and skip volume_controller_plugin,
  // which throws std::runtime_error on PipeWire/ALSA hosts without a Master mixer.
  // Veylo uses NoVideoControls and controls volume natively through libmpv.
  FlPluginRegistry* registry = FL_PLUGIN_REGISTRY(view);
  g_autoptr(FlPluginRegistrar) media_kit_libs_linux_registrar =
      fl_plugin_registry_get_registrar_for_plugin(registry,
                                                  "MediaKitLibsLinuxPlugin");
  media_kit_libs_linux_plugin_register_with_registrar(
      media_kit_libs_linux_registrar);
  g_autoptr(FlPluginRegistrar) media_kit_video_registrar =
      fl_plugin_registry_get_registrar_for_plugin(registry,
                                                  "MediaKitVideoPlugin");
  media_kit_video_plugin_register_with_registrar(media_kit_video_registrar);

  g_autoptr(FlStandardMethodCodec) codec = fl_standard_method_codec_new();
  self->window_channel = fl_method_channel_new(
      fl_engine_get_binary_messenger(fl_view_get_engine(view)), "veylo/window",
      FL_METHOD_CODEC(codec));
  fl_method_channel_set_method_call_handler(
      self->window_channel, window_channel_call_cb, self, nullptr);

  self->wayland_video = veylo_wayland_video_new(window, view);
  self->video_channel = fl_method_channel_new(
      fl_engine_get_binary_messenger(fl_view_get_engine(view)), "veylo/video",
      FL_METHOD_CODEC(codec));
  fl_method_channel_set_method_call_handler(
      self->video_channel, video_channel_call_cb, self, nullptr);

  gtk_widget_grab_focus(GTK_WIDGET(view));
}

// Implements GApplication::local_command_line.
static gboolean my_application_local_command_line(GApplication* application,
                                                  gchar*** arguments,
                                                  int* exit_status) {
  MyApplication* self = MY_APPLICATION(application);
  // Strip out the first argument as it is the binary name.
  self->dart_entrypoint_arguments = g_strdupv(*arguments + 1);

  g_autoptr(GError) error = nullptr;
  if (!g_application_register(application, nullptr, &error)) {
    g_warning("Failed to register: %s", error->message);
    *exit_status = 1;
    return TRUE;
  }

  g_application_activate(application);
  *exit_status = 0;

  return TRUE;
}

// Implements GApplication::startup.
static void my_application_startup(GApplication* application) {
  // MyApplication* self = MY_APPLICATION(object);

  // Perform any actions required at application startup.

  G_APPLICATION_CLASS(my_application_parent_class)->startup(application);
}

// Implements GApplication::shutdown.
static void my_application_shutdown(GApplication* application) {
  // MyApplication* self = MY_APPLICATION(object);

  // Perform any actions required at application shutdown.

  G_APPLICATION_CLASS(my_application_parent_class)->shutdown(application);
}

// Implements GObject::dispose.
static void my_application_dispose(GObject* object) {
  MyApplication* self = MY_APPLICATION(object);
  if (self->wayland_video != nullptr) {
    veylo_wayland_video_free(self->wayland_video);
    self->wayland_video = nullptr;
  }
  g_clear_object(&self->video_channel);
  g_clear_object(&self->window_channel);
  g_clear_pointer(&self->dart_entrypoint_arguments, g_strfreev);
  G_OBJECT_CLASS(my_application_parent_class)->dispose(object);
}

static void my_application_class_init(MyApplicationClass* klass) {
  G_APPLICATION_CLASS(klass)->activate = my_application_activate;
  G_APPLICATION_CLASS(klass)->local_command_line =
      my_application_local_command_line;
  G_APPLICATION_CLASS(klass)->startup = my_application_startup;
  G_APPLICATION_CLASS(klass)->shutdown = my_application_shutdown;
  G_OBJECT_CLASS(klass)->dispose = my_application_dispose;
}

static void my_application_init(MyApplication* self) {}

MyApplication* my_application_new() {
  // Set the program name to the application ID, which helps various systems
  // like GTK and desktop environments map this running application to its
  // corresponding .desktop file. This ensures better integration by allowing
  // the application to be recognized beyond its binary name.
  g_set_prgname(APPLICATION_ID);

  return MY_APPLICATION(g_object_new(my_application_get_type(),
                                     "application-id", APPLICATION_ID, "flags",
                                     G_APPLICATION_NON_UNIQUE, nullptr));
}
