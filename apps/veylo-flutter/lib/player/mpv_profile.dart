import 'dart:io';

import 'package:flutter/services.dart' show rootBundle;
import 'package:flutter/widgets.dart' show BoxFit;
import 'package:media_kit/media_kit.dart';
import 'package:path_provider/path_provider.dart';

String? _caPath;

/// mpv's TLS on Android, Windows and macOS (media_kit's libmpv builds) has no system certificate
/// store, so with `tls-verify=yes` every HTTPS stream fails to open. Like the DX app on Android,
/// hand it Mozilla's CA bundle (curl's cacert.pem, assets/tls/), written once to app support.
/// Linux uses the distro's mpv, which reads the system store.
Future<String?> caBundlePath() async {
  if (Platform.isLinux) return null;
  if (_caPath != null) return _caPath;
  try {
    final dir = await getApplicationSupportDirectory();
    final file = File('${dir.path}${Platform.pathSeparator}cacert.pem');
    final data = await rootBundle.load('assets/tls/cacert.pem');
    if (!file.existsSync() || file.lengthSync() != data.lengthInBytes) {
      await file.parent.create(recursive: true);
      await file.writeAsBytes(data.buffer.asUint8List(data.offsetInBytes, data.lengthInBytes), flush: true);
    }
    return _caPath = file.path;
  } catch (_) {
    return null;
  }
}

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
  // Android/iOS: media_kit's video controller owns vo/wid/hwdec (vo=gpu on the Android surface);
  // forcing vo=libmpv there left every video black (found by the runtime CI on the emulators).
  final mobile = Platform.isAndroid || Platform.isIOS;
  if (!mobile) await setProp('vo', 'libmpv');
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
  final ca = await caBundlePath();
  if (ca != null) await setProp('tls-ca-file', ca);

  // Match DX GPU video quality & HDR-to-SDR tone-mapping (override media_kit's bilinear defaults).
  if (!mobile || hwdecMode == 'no') await setProp('hwdec', mpvHwdec);
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

/// mpv reports some problems on its error stream that don't stop playback: no audio device
/// (video still plays, silently), a non-seekable stream, a missing subtitle font. Only the
/// others mean the file can't be played. Shared by the player screen and the runtime tests.
bool isFatalPlayerError(String message) {
  final m = message.toLowerCase();
  const nonFatal = [
    // No audio output: video plays silently.
    'audio device', 'no sound', 'audio output', 'ao: ',
    // Not seekable: playback from the start still works.
    'cannot seek', 'seek',
    // Hardware decoding unavailable: mpv falls back to software decoding.
    'avhwdevicecontext', 'hwdec', 'hardware decod', 'vaapi', 'vdpau', 'cuda', 'nvdec',
    'd3d11va', 'dxva', 'videotoolbox', 'mediacodec', 'vulkan', 'imgutils',
    'picture size 0x0', 'failed to initialize decoder',
    // HLS keepalive / transient transport chatter while playback continues.
    'keepalive', 'packet corrupt', 'pes packet', 'tls packet', 'error reading http response',
    // Subtitles/fonts: the picture is unaffected.
    'subtitle', 'libass', 'font',
  ];
  return !nonFatal.any(m.contains);
}
