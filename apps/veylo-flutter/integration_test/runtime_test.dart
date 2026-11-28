// Runtime test of the whole Veylo Flutter app, run by CI on Linux, macOS, Windows, Android and
// Android TV (`.github/workflows/app-runtime.yml`). Every step reports to Sentry: a transaction
// with its timing (and, for clips, decode metrics), an error event on failure, plus a screenshot
// sent back to the driver for Sentry's snapshot comparison.
//
//   flutter drive --profile --driver=test_driver/integration_test.dart \
//     --target=integration_test/runtime_test.dart -d <device> \
//     --dart-define=SENTRY_DSN=... --dart-define=VEYLO_TARGET=linux \
//     --dart-define=CLIP_BASE=http://127.0.0.1:8765
import 'dart:async';
import 'dart:convert';
import 'dart:io' show HttpClient, Platform;
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:media_kit/media_kit.dart';
import 'package:sentry/sentry.dart';
import 'package:veylo_flutter/bridge/veylo_bridge.dart';
import 'package:veylo_flutter/main.dart';
import 'package:veylo_flutter/player/mpv_profile.dart';
import 'package:veylo_flutter/screens/home_screen.dart';
import 'package:veylo_flutter/screens/player_screen.dart';
import 'package:veylo_flutter/screens/title_screen.dart';
import 'package:veylo_flutter/state/app_state.dart';

const _dsn = String.fromEnvironment('SENTRY_DSN');
const _target = String.fromEnvironment('VEYLO_TARGET', defaultValue: 'local');
const _clipBase = String.fromEnvironment('CLIP_BASE');
const _release = String.fromEnvironment('VEYLO_RELEASE', defaultValue: 'veylo-flutter@dev');
const _runId = String.fromEnvironment('RUN_ID', defaultValue: 'local');
const _onlyScreens = bool.fromEnvironment('ONLY_SCREENS');

final _shots = <String, String>{}; // name -> base64 PNG, for Sentry snapshots
final _results = <Map<String, Object?>>[];
final _shotKey = GlobalKey();

/// Same scrubbing idea as the app's Rust `redact_diagnostic`: URLs (incl. tokens) and home paths.
String _redact(String s) => s
    .replaceAll(RegExp(r'(?:https?|wss?)://[^\s"<>]+'), '[URL]')
    .replaceAll(RegExp(r'/(?:home|Users)/[^/\s"<>]+'), '[HOME]')
    .replaceAll(RegExp(r'[A-Z]:\\Users\\[^\\\s"<>]+', caseSensitive: false), '[HOME]');

Future<void> _initSentry() async {
  if (_dsn.isEmpty) return;
  await Sentry.init((o) {
    o.dsn = _dsn;
    o.release = _release;
    o.environment = 'ci-runtime';
    o.tracesSampleRate = 1.0;
    o.sendDefaultPii = false;
    o.beforeSend = (event, hint) {
      event.serverName = null;
      final msg = event.message;
      if (msg != null) event.message = SentryMessage(_redact(msg.formatted));
      for (final e in event.exceptions ?? const <SentryException>[]) {
        if (e.value != null) e.value = _redact(e.value!);
      }
      return event;
    };
  });
  await Sentry.configureScope((s) async {
    await s.setTag('test_target', _target);
    await s.setTag('os', Platform.operatingSystem);
    await s.setTag('run_id', _runId);
  });
}

/// One reported step: a Sentry transaction, a result row, failures captured but never fatal to
/// the rest of the walkthrough (so one broken screen still lets every other part be tested).
Future<bool> _step(
  String name,
  Future<Map<String, Object?>?> Function(ISentrySpan tx) body, {
  Map<String, String> tags = const {},
}) async {
  final tx = Sentry.startTransaction('runtime.$name', 'test.step', bindToScope: true);
  tags.forEach(tx.setTag);
  final sw = Stopwatch()..start();
  final row = <String, Object?>{'step': name, ...tags};
  try {
    final extra = await body(tx);
    if (extra != null) row.addAll(extra);
    row['ok'] = true;
    await tx.finish(status: const SpanStatus.ok());
  } catch (e, st) {
    row['ok'] = false;
    row['error'] = _redact(e.toString());
    await Sentry.captureException(e, stackTrace: st, withScope: (s) async {
      await s.setTag('step', name);
      for (final t in tags.entries) {
        await s.setTag(t.key, t.value);
      }
    });
    await tx.finish(status: const SpanStatus.internalError());
    debugPrint('RUNTIME FAIL $name: ${row['error']}');
  }
  row['ms'] = sw.elapsedMilliseconds;
  _results.add(row);
  debugPrint('RUNTIME ${row['ok'] == true ? 'ok  ' : 'FAIL'} $name ${row['ms']}ms');
  return row['ok'] == true;
}

/// Pump real frames until [done] or [timeout] (the app does real network I/O).
Future<void> _until(WidgetTester t, bool Function() done, Duration timeout, String what) async {
  final end = DateTime.now().add(timeout);
  while (!done()) {
    if (DateTime.now().isAfter(end)) throw TimeoutException('timed out waiting for $what', timeout);
    await t.pump(const Duration(milliseconds: 100));
  }
}

Future<void> _settle(WidgetTester t, [Duration d = const Duration(milliseconds: 1500)]) async {
  final end = DateTime.now().add(d);
  while (DateTime.now().isBefore(end)) {
    await t.pump(const Duration(milliseconds: 100));
  }
}

/// PNG of the app, scaled to at most 1280 px wide, kept for Sentry snapshots.
Future<void> _shot(WidgetTester t, String name) async {
  final boundary = _shotKey.currentContext?.findRenderObject() as RenderRepaintBoundary?;
  if (boundary == null) return;
  await t.runAsync(() async {
    final w = boundary.size.width;
    final ratio = w > 1280 ? 1280 / w : 1.0;
    final img = await boundary.toImage(pixelRatio: ratio);
    final data = await img.toByteData(format: ui.ImageByteFormat.png);
    img.dispose();
    if (data != null) _shots['$_target/$name'] = base64Encode(data.buffer.asUint8List());
  });
}

/// mpv's own decoded frame (independent of how the platform composites video) downscaled to PNG,
/// plus a check that it isn't a blank/black frame.
Future<bool> _frameShot(WidgetTester t, Player p, String name) async {
  var varied = false;
  await t.runAsync(() async {
    final raw = await p.screenshot(format: 'image/png');
    if (raw == null) return;
    final codec = await ui.instantiateImageCodec(raw, targetWidth: 640);
    final frame = (await codec.getNextFrame()).image;
    final rgba = await frame.toByteData(format: ui.ImageByteFormat.rawRgba);
    if (rgba != null) varied = _isVaried(rgba.buffer.asUint8List());
    final png = await frame.toByteData(format: ui.ImageByteFormat.png);
    frame.dispose();
    if (png != null) _shots['$_target/frames/$name'] = base64Encode(png.buffer.asUint8List());
  });
  return varied;
}

bool _isVaried(Uint8List rgba) {
  final seen = <int>{};
  for (var i = 0; i < rgba.length; i += 4 * 97) {
    seen.add((rgba[i] >> 4) << 8 | (rgba[i + 1] >> 4) << 4 | (rgba[i + 2] >> 4));
    if (seen.length > 24) return true;
  }
  return false;
}

Future<String> _prop(Player p, String name) async {
  final n = p.platform;
  if (n is! NativePlayer) return '';
  try {
    return await n.getProperty(name);
  } catch (_) {
    return '';
  }
}

/// Plays one session through the real PlayerScreen and measures it.
Future<Map<String, Object?>> _play(
  WidgetTester t,
  AppState app,
  ISentrySpan tx,
  PlaySession session, {
  required String shotName,
  int? expectW,
  int? expectH,
  Duration openTimeout = const Duration(seconds: 30),
}) async {
  final sw = Stopwatch()..start();
  String? playerError;
  final notices = <String>[];
  final mpvLog = <String>[];
  StreamSubscription<String>? errSub;
  StreamSubscription<PlayerLog>? logSub;
  app.startPlaySession(session);
  try {
    await _until(t, () => PlayerScreen.debugActivePlayer.value != null, const Duration(seconds: 10), 'player screen');
    final p = PlayerScreen.debugActivePlayer.value!;
    logSub = p.stream.log.listen((l) {
      mpvLog.add(_redact('[${l.level}] ${l.prefix}: ${l.text.trim()}'));
      if (mpvLog.length > 25) mpvLog.removeAt(0);
    });
    errSub = p.stream.error.listen((e) {
      if (isFatalPlayerError(e)) {
        playerError ??= e;
      } else if (notices.length < 5) {
        notices.add(_redact(e));
      }
    });
    await _until(t, () => playerError != null || (p.state.width ?? 0) > 0, openTimeout, 'first video frame');
    if (playerError != null) throw StateError('player error: $playerError');
    final ttff = sw.elapsedMilliseconds;
    final startPos = p.state.position;
    // Watch ~3 s of wall time and measure how far playback really got.
    await _settle(t, const Duration(seconds: 3));
    final advanced = p.state.position - startPos;
    if (playerError != null) throw StateError('player error: $playerError');
    final m = <String, Object?>{
      'ttff_ms': ttff,
      'advanced_ms': advanced.inMilliseconds,
      'width': p.state.width,
      'height': p.state.height,
      'video_codec': await _prop(p, 'video-codec'),
      'pixfmt': await _prop(p, 'video-params/pixelformat'),
      'hwdec': await _prop(p, 'hwdec-current'),
      'audio_codec': await _prop(p, 'audio-codec-name'),
      'channels': await _prop(p, 'audio-params/channel-count'),
      'fps_out': await _prop(p, 'estimated-vf-fps'),
      'dropped_vo': int.tryParse(await _prop(p, 'frame-drop-count')) ?? -1,
      'dropped_dec': int.tryParse(await _prop(p, 'decoder-frame-drop-count')) ?? -1,
      'notices': notices,
    };
    await _shot(t, 'player/$shotName');
    // Real streams often open on black (a trailer's fade-in): look again before calling it blank.
    var varied = await _frameShot(t, p, shotName);
    for (var i = 0; i < 3 && !varied; i++) {
      await _settle(t, const Duration(milliseconds: 1500));
      varied = await _frameShot(t, p, shotName);
    }
    m['frame_varied'] = varied;
    tx.setMeasurement('ttff', ttff, unit: DurationSentryMeasurementUnit.milliSecond);
    tx.setMeasurement('advanced', advanced.inMilliseconds, unit: DurationSentryMeasurementUnit.milliSecond);
    tx.setMeasurement('frames_dropped', ((m['dropped_vo'] as int).clamp(0, 1 << 30)) + ((m['dropped_dec'] as int).clamp(0, 1 << 30)));
    tx.setTag('hwdec', '${m['hwdec']}'.isEmpty ? 'none' : '${m['hwdec']}');
    if (expectW != null && m['width'] != expectW) throw StateError('decoded width ${m['width']} != $expectW');
    if (expectH != null && m['height'] != expectH) throw StateError('decoded height ${m['height']} != $expectH');
    if (advanced.inMilliseconds < 500) throw StateError('playback stalled (advanced ${advanced.inMilliseconds} ms in 3 s)');
    // Android decodes straight onto the display surface (MediaCodec): mpv's screenshot can't read
    // those frames and returns black, so there the pixel check is informational — first frame,
    // decoded size and advancing playback above still prove the video plays.
    final hw = '${m['hwdec']}';
    final hardwareFrames = Platform.isAndroid || (hw.isNotEmpty && hw != 'no');
    if (m['frame_varied'] == false) {
      // Hardware-decoded frames (MediaCodec surfaces, VA-API dma-bufs) can't be read back by
      // mpv's screenshot ("Input image format vaapi not supported by libswscale").
      if (hardwareFrames) {
        m['frame_check'] = 'unavailable (hardware surface)';
      } else {
        throw StateError('decoded frame is blank');
      }
    }
    return m;
  } catch (e) {
    // The last mpv log lines say *why* (TLS, demuxer, decoder…): keep them with the failure.
    throw StateError('$e${mpvLog.isEmpty ? '' : '\nmpv log:\n${mpvLog.join('\n')}'}');
  } finally {
    await logSub?.cancel();
    await errSub?.cancel();
    await app.closePlayer();
    await _until(t, () => PlayerScreen.debugActivePlayer.value == null, const Duration(seconds: 10), 'player close');
    await _settle(t, const Duration(milliseconds: 600));
  }
}

/// Layout-shift detector (like the web's CLS): samples every `shift.*`-keyed widget of [root]
/// every 100 ms and reports any that moves or resizes by more than 2 px after it first appeared
/// — data or images landing late and pushing the page around.
/// When the title's logo slot first became visible (ms after opening): time-to-reveal.
int? _lastRevealMs;
int? _lastHomeContentMs;

Future<List<String>> _watchShifts(
  WidgetTester t,
  Finder root,
  Duration window, {
  Stopwatch? clock,
  bool home = false,
}) async {
  _lastRevealMs = null;
  _lastHomeContentMs = null;
  final first = <String, Rect>{};
  final firstAt = <String, int>{};
  final shifts = <String, String>{};
  final sw = clock ?? (Stopwatch()..start());
  while (sw.elapsed < window) {
    if (home) {
      await t.pump();
    } else {
      await t.pump(const Duration(milliseconds: 100));
    }
    if (root.evaluate().isEmpty) {
      if (home) {
      await Future<void>.delayed(Duration(milliseconds: _lastHomeContentMs == null ? 16 : 100));
    }
      continue;
    }
    if (home && _lastHomeContentMs == null) {
      final content = find.descendant(
        of: root.first,
        matching: find.byKey(const ValueKey('home.content')),
      );
      if (content.evaluate().any((e) {
        final ro = e.renderObject;
        return ro != null && _effectiveOpacity(ro) >= 0.01;
      })) {
        _lastHomeContentMs = sw.elapsedMilliseconds;
      }
    }
    final keyed = find.descendant(
      of: root.first,
      matching: find.byWidgetPredicate((w) => w.key is ValueKey<String> && (w.key! as ValueKey<String>).value.startsWith('shift.')),
    );
    for (final e in keyed.evaluate()) {
      final name = ((e.widget.key! as ValueKey<String>).value).substring(6);
      final ro = e.renderObject;
      if (ro is! RenderBox || !ro.hasSize || !ro.attached) continue;
      // Only what the user can see: content still hidden behind the reveal may settle freely.
      if (_effectiveOpacity(ro) < 0.01) continue;
      // Painted corners on screen (all ancestor transforms applied): catches slides and scales
      // from entrance animations or page transitions, not only layout moves.
      final tl = ro.localToGlobal(Offset.zero);
      final br = ro.localToGlobal(ro.size.bottomRight(Offset.zero));
      final r = Rect.fromPoints(tl, br);
      if (const bool.fromEnvironment('SHIFT_TRACE') && name == 'title.actions') {
        debugPrint('TRACE ${sw.elapsedMilliseconds} actions=$r');
      }
      final base = first[name];
      if (base == null) {
        first[name] = r;
        firstAt[name] = sw.elapsedMilliseconds;
        if (name == (home ? 'home.hero-content' : 'title.logo')) {
          _lastRevealMs = sw.elapsedMilliseconds;
          if (home) _lastHomeContentMs ??= _lastRevealMs;
        }
        continue;
      }
      final dy = r.top - base.top, dx = r.left - base.left, dh = r.height - base.height, dw = r.width - base.width;
      if ((dy.abs() > 1.5 || dx.abs() > 1.5 || dh.abs() > 1.5 || dw.abs() > 1.5) && !shifts.containsKey(name)) {
        shifts[name] = '$name moved dy=${dy.round()} dx=${dx.round()} dh=${dh.round()} dw=${dw.round()} at ${sw.elapsedMilliseconds} ms '
            '(appeared at ${firstAt[name]} ms)';
      }
    }
    if (home) {
      await Future<void>.delayed(Duration(milliseconds: _lastHomeContentMs == null ? 16 : 100));
    }
  }
  return shifts.values.toList();
}

/// Product of every ancestor opacity (Opacity / AnimatedOpacity / FadeTransition).
double _effectiveOpacity(RenderObject ro) {
  var o = 1.0;
  RenderObject? n = ro;
  while (n != null) {
    if (n is RenderOpacity) o *= n.opacity;
    if (n is RenderAnimatedOpacity) o *= n.opacity.value;
    n = n.parent;
  }
  return o;
}

MediaItem? _firstOf(Map<String, dynamic> res, String type) {
  for (final r in (res['results'] as List<dynamic>? ?? [])) {
    final m = (r as Map<String, dynamic>)['media'] as Map<String, dynamic>?;
    if (m != null && m['type'] == type) return MediaItem.fromJson(m);
  }
  return null;
}

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  binding.framePolicy = LiveTestWidgetsFlutterBindingFramePolicy.fullyLive;

  testWidgets('Veylo runtime: whole app + player matrix ($_target)', (t) async {
    await _initSentry();
    final frameworkErrors = <String>[];
    final previousOnError = FlutterError.onError;
    // Collect framework errors (overflows, build exceptions) per step instead of aborting the run.
    FlutterError.onError = (d) {
      if (d.library == 'image resource service') return; // dead poster URLs are expected
      final text = _redact(d.exceptionAsString());
      frameworkErrors.add(text);
      unawaited(Sentry.captureException(d.exception, stackTrace: d.stack, withScope: (s) async {
        await s.setTag('kind', 'flutter-error');
        await s.setTag('library', d.library ?? 'framework');
      }));
    };

    MediaKit.ensureInitialized();
    Future<void> watchHome(String pass, Duration window, Future<void> Function() enter) async {
      await _step('shift.home-$pass', (tx) async {
        final before = frameworkErrors.length;
        final clock = Stopwatch()..start();
        await enter();
        final shifts = await _watchShifts(t, find.byType(HomeScreen), window, clock: clock, home: true);
        final reveal = _lastHomeContentMs;
        tx.setMeasurement('layout_shifts', shifts.length);
        if (reveal != null) {
          tx.setMeasurement('home_reveal', reveal, unit: DurationSentryMeasurementUnit.milliSecond);
        }
        await _shot(t, 'home-$pass');
        if (shifts.isNotEmpty) throw StateError('layout shifts: ${shifts.join(' | ')}');
        if (frameworkErrors.length > before) {
          throw StateError('framework errors: ${frameworkErrors.sublist(before).take(3).join(' | ')}');
        }
        if (reveal == null) throw StateError('Home showed no content during the watch');
        return {'shifts': 0, 'home_reveal_ms': reveal, 'home_hero_reveal_ms': _lastRevealMs};
      });
    }

    // Pin the boot viewport on Linux so the compositor's initial window
    // configure cannot be mistaken for content shifting inside a viewport.
    if (Platform.isLinux) {
      t.view.devicePixelRatio = 1;
      t.view.physicalSize = const Size(1280, 800);
      addTearDown(t.view.resetPhysicalSize);
      addTearDown(t.view.resetDevicePixelRatio);
    }
    await watchHome('cold', const Duration(seconds: 8), () async {
      await t.pumpWidget(RepaintBoundary(key: _shotKey, child: const VeyloApp()));
    });
    late AppState app;

    await _step('boot', (tx) async {
      final inside = find.descendant(of: find.byType(AppStateScope), matching: find.byWidgetPredicate((_) => true));
      app = AppStateScope.read(t.element(inside.first));
      await _until(t, () => app.initialized || app.initError != null, const Duration(seconds: 60), 'backend init');
      if (app.initError != null) throw StateError('init failed: ${app.initError}');
      final f = await VeyloBridge.instance.call('force_reports', {'target': _target});
      return {'version': app.version, 'rust_sentry': f['available']};
    });
    if (!app.initialized) {
      FlutterError.onError = previousOnError;
      fail('backend did not start; see Sentry for details');
    }

    await _step('picks.ai', (tx) async {
      final res = await VeyloBridge.instance.call('picks_ai_check');
      if (res['ok'] != true || res['ai_failed'] == true) {
        throw StateError('Picks AI unavailable: $res');
      }
      return {
        'dim': res['dim'],
        'near': (res['near'] as num?)?.toStringAsFixed(3),
        'far': (res['far'] as num?)?.toStringAsFixed(3),
      };
    });

    Future<void> screen(String name, Future<void> Function() go, {Duration settle = const Duration(seconds: 3)}) =>
        _step('screen.$name', (tx) async {
          final before = frameworkErrors.length;
          await go();
          await _settle(t, settle);
          await _shot(t, name);
          if (frameworkErrors.length > before) {
            throw StateError('framework errors: ${frameworkErrors.sublist(before).take(3).join(' | ')}');
          }
          return null;
        });

    await screen('home', () async {
      await _until(t, () => app.homeRows.isNotEmpty, const Duration(seconds: 45), 'home rows');
    }, settle: const Duration(seconds: 5));

    app.setTab(VeyloTab.library);
    await _settle(t, const Duration(milliseconds: 600));
    await watchHome('warm', const Duration(seconds: 6), () async => app.setTab(VeyloTab.home));

    // Picks AI on this platform: a vibe (plot) search only works with the on-device model loaded.
    await _step('ai.vibe-search', (tx) async {
      final res = await t.runAsync(() => VeyloBridge.instance.search('a crew of thieves plans an elaborate heist', vibe: true));
      final items = (res?['items'] as List<dynamic>? ?? []);
      tx.setMeasurement('vibe_results', items.length);
      if (items.isEmpty) throw StateError('vibe search returned nothing (Picks AI not loaded?)');
      return {'vibe_results': items.length};
    });

    MediaItem? movie;
    MediaItem? series;
    await screen('search', () async {
      app.openSearch(query: 'The Matrix');
      final res = await VeyloBridge.instance.search('The Matrix');
      movie = _firstOf(res, 'movie');
      series = _firstOf(await VeyloBridge.instance.search('Breaking Bad'), 'series');
      if (movie == null) throw StateError('search found no film');
    }, settle: const Duration(seconds: 5));

    // Title pages: opened cold (first visit) and again (disk/session cache), watched for layout
    // shifts while their data and images land.
    Future<void> titlePage(String name, MediaItem m) async {
      for (final pass in ['cold', 'reopen']) {
        await _step('shift.$name-$pass', (tx) async {
          app.closeSearch();
          app.closeTitle();
          await _settle(t, const Duration(milliseconds: 600));
          app.openTitle(m);
          final shifts = await _watchShifts(t, find.byType(TitleScreen), const Duration(seconds: 7));
          tx.setMeasurement('layout_shifts', shifts.length);
          if (_lastRevealMs != null) {
            tx.setMeasurement('reveal', _lastRevealMs!, unit: DurationSentryMeasurementUnit.milliSecond);
          }
          await _shot(t, '$name-$pass');
          if (shifts.isNotEmpty) throw StateError('layout shifts: ${shifts.join(' | ')}');
          return {'shifts': 0, 'reveal_ms': _lastRevealMs};
        });
      }
    }
    if (movie != null) await titlePage('title-movie', movie!);
    if (series != null) await titlePage('title-series', series!);
    for (final tab in [VeyloTab.picks, VeyloTab.library, VeyloTab.settings, VeyloTab.home]) {
      await screen('tab-${tab.name}', () async {
        app.closeTitle();
        app.setTab(tab);
      }, settle: const Duration(seconds: 4));
    }

    if (!Platform.isAndroid) {
      final original = t.view.physicalSize;
      final originalRatio = t.view.devicePixelRatio;
      final lastPlayed = app.lastPlayed;
      addTearDown(t.platformDispatcher.clearTextScaleFactorTestValue);
      addTearDown(t.view.resetViewPadding);
      final viewPadding = t.view.viewPadding;
      expect([viewPadding.left, viewPadding.top, viewPadding.right, viewPadding.bottom], everyElement(0));
      app.lastPlayedNotifier.value = SeenItem(
        media: MediaItem.fromJson(const {
          'id': 'dock-test',
          'type': 'movie',
          'name': 'A feature film with a long title for the Last Played pill',
        }),
        at: 0,
        whenLabel: '',
      );
      try {
        app.closeTitle();
        app.setTab(VeyloTab.home);
        for (final (width, scale) in const [
          (320, 1.0),
          (390, 1.0),
          (560, 1.0),
          (608, 1.0),
          (760, 1.0),
          (1280, 1.0),
          (2560, 1.0),
          (2560, 1.3),
          (608, 1.3),
          (320, 1.3),
        ]) {
          final name = 'dock-$width-${scale.toStringAsFixed(1)}';
          await _step('screen.$name', (tx) async {
            final before = frameworkErrors.length;
            t.view.devicePixelRatio = 1.0;
            t.view.physicalSize = Size(width.toDouble(), 800);
            t.platformDispatcher.textScaleFactorTestValue = scale;
            await _settle(t, const Duration(milliseconds: 350));

            Finder tab(VeyloTab tab) => find.byKey(ValueKey('dock-tab-${tab.name}'));
            List<Rect> controls() => [
              for (final id in ['last-played', 'shortcuts', 'search'])
                t.getRect(find.byKey(ValueKey('topbar-$id'))),
            ];
            void checkGeometry(List<Rect> trailing) {
              final rects = [
                for (final item in VeyloTab.values) t.getRect(tab(item)),
                ...trailing,
              ];
              expect(rects.first.left, greaterThanOrEqualTo(0));
              expect(rects.last.right, lessThanOrEqualTo(width));
              for (var i = 1; i < rects.length; i++) {
                expect(rects[i - 1].right, lessThanOrEqualTo(rects[i].left));
              }
            }

            final trailing = controls();
            checkGeometry(trailing);
            if (width >= 560) {
              final active = tab(VeyloTab.home);
              final label = find.descendant(of: active, matching: find.text('Home'));
              expect(label, findsOneWidget);
              expect(t.getRect(label).right, lessThanOrEqualTo(t.getRect(active).right));
            }
            await _shot(t, name);

            final mouse = await t.createGesture(kind: ui.PointerDeviceKind.mouse);
            await mouse.addPointer(location: const Offset(2, 2));
            try {
              await mouse.moveTo(t.getCenter(tab(VeyloTab.picks)));
              await _settle(t, const Duration(milliseconds: 350));
              expect(controls(), trailing);
              checkGeometry(trailing);
              expect(find.text('Picks'), findsOneWidget);
              await _shot(t, '$name-hover');
            } finally {
              await mouse.removePointer();
            }
            if (frameworkErrors.length > before) {
              throw StateError('framework errors: ${frameworkErrors.sublist(before).take(3).join(' | ')}');
            }
            return null;
          });
        }
        await screen('dock-608-insets', () async {
          const insets = FakeViewPadding(top: 32, left: 24, right: 18, bottom: 24);
          t.view.physicalSize = const Size(608, 800);
          t.view.viewPadding = insets;
          t.platformDispatcher.textScaleFactorTestValue = 1.3;
          await _settle(t, const Duration(milliseconds: 350));

          final active = find.byKey(const ValueKey('dock-tab-home'));
          final settings = find.byKey(const ValueKey('dock-tab-settings'));
          final search = find.byKey(const ValueKey('topbar-search'));
          final activeRect = t.getRect(active);
          expect(activeRect.center.dy, insets.top + 42);
          expect(activeRect.left, greaterThan(insets.left));
          expect(t.getRect(search).right, lessThan(608 - insets.right));

          final strategy = FocusManager.instance.highlightStrategy;
          FocusManager.instance.highlightStrategy = FocusHighlightStrategy.alwaysTraditional;
          final button = find.descendant(of: settings, matching: find.byType(GestureDetector));
          final focus = Focus.of(t.element(button));
          try {
            focus.requestFocus();
            await _settle(t, const Duration(milliseconds: 350));
            expect(find.text('Settings'), findsOneWidget);
            await _shot(t, 'dock-608-insets-focus');
          } finally {
            focus.unfocus();
            FocusManager.instance.highlightStrategy = strategy;
          }
        }, settle: const Duration(milliseconds: 350));
      } finally {
        t.view.resetViewPadding();
        app.lastPlayedNotifier.value = lastPlayed;
        t.view.physicalSize = original;
        t.view.devicePixelRatio = originalRatio;
        t.platformDispatcher.clearTextScaleFactorTestValue();
        await _settle(t);
      }
    }

    // Window/viewport sizes (desktop): phone-ish, laptop, 1080p, 1440p, TV 4K-scaled.
    if (!Platform.isAndroid) {
      final original = t.view.physicalSize;
      final originalRatio = t.view.devicePixelRatio;
      for (final s in const [Size(1024, 640), Size(1280, 720), Size(1920, 1080), Size(2560, 1440)]) {
        final label = '${s.width.toInt()}x${s.height.toInt()}';
        await screen('size-$label-home', () async {
          t.view.devicePixelRatio = 1.0;
          t.view.physicalSize = s;
          app.setTab(VeyloTab.home);
        });
        if (movie != null) {
          await screen('size-$label-title', () async => app.openTitle(movie!), settle: const Duration(seconds: 4));
          app.closeTitle();
        }
      }
      await screen('size-390x844-home', () async {
        t.view.devicePixelRatio = 1.0;
        t.view.physicalSize = const Size(390, 844);
        app.setTab(VeyloTab.home);
      });
      t.view.physicalSize = original;
      t.view.devicePixelRatio = originalRatio;
      await _settle(t);
    }

    // Real streaming: the app's demo stream over the internet.
    if (app.demoStream.isNotEmpty && !_onlyScreens) {
      await _step('player.demo-stream', (tx) => _play(t, app, tx,
          PlaySession(title: 'Runtime test', subtitle: 'Demo stream', url: app.demoStream),
          shotName: 'demo-stream', openTimeout: const Duration(seconds: 45)));
    }

    // Codec × size matrix over HTTP from the runner (like a real stream).
    if (_clipBase.isNotEmpty && !_onlyScreens) {
      Map<String, dynamic>? manifest;
      await _step('player.manifest', (tx) async {
        manifest = await t.runAsync(() async {
          final client = HttpClient();
          try {
            final req = await client.getUrl(Uri.parse('$_clipBase/manifest.json'));
            final resp = await req.close();
            return jsonDecode(await resp.transform(utf8.decoder).join()) as Map<String, dynamic>;
          } finally {
            client.close();
          }
        });
        if (manifest == null) throw StateError('clip manifest unavailable');
        return {'clips': (manifest!['clips'] as List).length, 'skipped': manifest!['skipped']};
      });
      for (final c in (manifest?['clips'] as List<dynamic>? ?? []).cast<Map<String, dynamic>>()) {
        final name = c['name'] as String;
        await _step('clip.$name', (tx) => _play(t, app, tx,
            PlaySession(title: name, subtitle: 'Runtime matrix', url: '$_clipBase/${c['file']}'),
            shotName: name,
            expectW: (c['width'] as num).toInt(),
            expectH: (c['height'] as num).toInt()),
          tags: {
            'vcodec': '${c['vcodec']}',
            'acodec': '${c['acodec']}',
            'resolution': '${c['width']}x${c['height']}',
            if (c['hdr'] != null) 'hdr': '${c['hdr']}',
          });
      }
    }

    FlutterError.onError = previousOnError;
    final failed = _results.where((r) => r['ok'] != true).toList();
    binding.reportData = {
      'target': _target,
      'results': _results,
      'screenshots': _shots,
    };
    await t.runAsync(() async {
      await VeyloBridge.instance.flushReports();
      await Sentry.close();
    });
    expect(failed, isEmpty, reason: failed.map((r) => '${r['step']}: ${r['error']}').join('\n'));
  }, timeout: const Timeout(Duration(minutes: 25)));
}
