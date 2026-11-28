#ifndef VEYLO_WAYLAND_VIDEO_H_
#define VEYLO_WAYLAND_VIDEO_H_

#include <flutter_linux/flutter_linux.h>
#include <gtk/gtk.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct _VeyloWaylandVideo VeyloWaylandVideo;

VeyloWaylandVideo* veylo_wayland_video_new(GtkWindow* window, FlView* view);
gboolean veylo_wayland_video_attach(VeyloWaylandVideo* self,
                                    int64_t mpv_handle_ptr,
                                    char** error_out);
void veylo_wayland_video_set_rect(VeyloWaylandVideo* self,
                                  gboolean active,
                                  int x,
                                  int y,
                                  int width,
                                  int height);
void veylo_wayland_video_detach(VeyloWaylandVideo* self);
void veylo_wayland_video_free(VeyloWaylandVideo* self);

#ifdef __cplusplus
}
#endif

#endif  // VEYLO_WAYLAND_VIDEO_H_
