import 'package:flutter/widgets.dart' show BoxFit;
import 'package:media_kit/media_kit.dart';

/// The mpv properties every Veylo playback uses (buffering, HTTP resilience, scaling, HDR
/// tone-mapping, libass subtitles). Shared by the player screen and the runtime player tests so
/// CI exercises exactly what users run.
Future<void> applyMpvProfile(
  NativePlayer platform, {
  String hwdecMode = 'auto',
  BoxFit fit = BoxFit.contain,
  String subColor = '#F2EFEA',
  double subScale = 1.0,
  int subPos = 95,
  bool subBg = false,
}) async {
  final mpvHwdec = switch (hwdecMode) {
    'copy' => 'auto-copy',
    'no' => 'no',
    _ => 'auto-safe',
  };
  Future<void> setProp(String k, String v) async {
    try {
      await platform.setProperty(k, v);
    } catch (_) {}
  }
  // Enable video decoding when using native Wayland subsurface (no VideoController).
  await setProp('vo', 'libmpv');
  await setProp('vid', 'auto');
  await setProp('panscan', fit == BoxFit.cover ? '1.0' : '0.0');

  // Disable user config & Lua scripts (prevents lua/stats LuaJIT unwind crash on Linux ARM64).
  await setProp('config', 'no');
  await setProp('load-scripts', 'no');
  await setProp('load-stats-overlay', 'no');
  await setProp('load-osd-console', 'no');
  await setProp('ytdl', 'no');
  await setProp('osc', 'no');
  await setProp('hr-seek', 'yes');

  // Match apps/veylo-dx/src/engine.rs buffering & resilient HTTP streaming.
  await setProp('cache', 'yes');
  await setProp('demuxer-max-bytes', '256MiB');
  await setProp('demuxer-max-back-bytes', '64MiB');
  await setProp('demuxer-readahead-secs', '60');
  await setProp('network-timeout', '20');
  await setProp(
    'stream-lavf-o',
    'reconnect=1,reconnect_streamed=1,reconnect_on_network_error=1,reconnect_on_http_error=5xx,reconnect_delay_max=5',
  );
  await setProp('tls-verify', 'yes');

  // Match DX GPU video quality & HDR-to-SDR tone-mapping (override media_kit's bilinear defaults).
  await setProp('hwdec', mpvHwdec);
  await setProp('hdr-compute-peak', 'yes');
  await setProp('scale', 'lanczos');
  await setProp('dscale', 'hermite');
  await setProp('dither', 'auto');
  await setProp('correct-downscaling', 'yes');
  await setProp('linear-downscaling', 'yes');
  await setProp('sigmoid-upscaling', 'yes');

  // Native GPU-composited libass subtitles matching DX.
  await setProp('sid', 'no');
  await setProp('sub-font', 'sans-serif');
  await setProp('sub-color', subColor);
  await setProp('sub-border-color', '#DD000000');
  await setProp('sub-scale', subScale.toStringAsFixed(2));
  await setProp('sub-pos', '$subPos');
  await setProp('sub-back-color', subBg ? '#A8000000' : '#00000000');

}
