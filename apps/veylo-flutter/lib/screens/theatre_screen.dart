import 'dart:async';
import 'dart:io' show Platform;
import 'dart:math' as math;
import 'dart:ui' show ImageFilter, PathFillType;
import 'package:cached_network_image/cached_network_image.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:media_kit/media_kit.dart';
import 'package:media_kit_video/media_kit_video.dart';
import '../bridge/veylo_bridge.dart';
import '../player/mpv_profile.dart';
import '../state/app_state.dart';
import '../theme/veylo_theme.dart';
import '../widgets/finding_runner.dart';
import '../widgets/glass.dart';
import '../widgets/veylo_icons.dart';

/// In-app Theatre modal matching the Dioxus app's TheatreSheet layout & interactions,
/// powered natively by Veylo's own libmpv player (zero YouTube iframe, zero external browser).
class TheatreScreen extends StatefulWidget {
  const TheatreScreen({super.key, required this.session});
  final TheatreSession session;

  @override
  State<TheatreScreen> createState() => _TheatreScreenState();
}

class _TheatreScreenState extends State<TheatreScreen> {
  static const MethodChannel _windowChannel = MethodChannel('veylo/window');
  static const MethodChannel _videoChannel = MethodChannel('veylo/video');

  late final Player _player;
  VideoController? _controller;
  bool _nativeWaylandVideo = false;
  bool _playerReady = false;
  final FocusNode _focus = FocusNode();

  int _index = 0;
  int _loadSeq = 0;
  bool _resolving = true;
  bool _buffering = true;
  bool _playing = true;
  bool _loaded = false;
  bool _fullscreen = false;
  bool _chromeVisible = true;
  bool _muted = false;
  double _volume = 100;
  double _lastNonZeroVolume = 100;
  double? _scrubValue;
  String? _error;
  String? _quality;
  String? _hdr;
  List<Map<String, dynamic>> _captions = const [];
  List<SourceItem> _clipSources = const [];
  int _sourceIdx = 0;
  bool _captionsOn = false;
  final Set<String> _failedLogos = {};

  final ValueNotifier<Duration> _posN = ValueNotifier<Duration>(Duration.zero);
  final ValueNotifier<Duration> _durN = ValueNotifier<Duration>(Duration.zero);
  final ValueNotifier<Duration> _bufN = ValueNotifier<Duration>(Duration.zero);
  late final Listenable _timelineListenable = Listenable.merge([_posN, _durN, _bufN]);

  Timer? _hideTimer;
  final List<StreamSubscription<dynamic>> _subs = [];

  // Last logical rect pushed to Wayland subsurface (avoid redundant IPC).
  int _sentX = -1, _sentY = -1, _sentW = -1, _sentH = -1;

  @override
  void initState() {
    super.initState();
    final n = widget.session.clips.length;
    _index = n > 0 ? widget.session.initialIndex.clamp(0, n - 1) : 0;

    _player = Player(
      configuration: const PlayerConfiguration(
        title: 'Veylo',
        vo: 'libmpv',
        osc: false,
        bufferSize: 128 << 20,
        libass: true,
        logLevel: MPVLogLevel.warn,
      ),
    );
    if (!Platform.isLinux) {
      _controller = VideoController(_player);
    }

    _subs.add(_player.stream.playing.listen((p) {
      if (!mounted) return;
      if (_playing != p || (p && !_loaded)) {
        setState(() {
          _playing = p;
          if (p) _loaded = true;
        });
      }
      if (!p) {
        _hideTimer?.cancel();
        if (!_chromeVisible && mounted) setState(() => _chromeVisible = true);
      } else {
        _scheduleHide();
      }
    }));
    _subs.add(_player.stream.buffering.listen((b) {
      if (mounted && _buffering != b) setState(() => _buffering = b);
    }));
    _subs.add(_player.stream.position.listen((p) {
      if (!mounted || _scrubValue != null) return;
      final moved = p > Duration.zero && p != _posN.value;
      _posN.value = p;
      if ((p > Duration.zero && !_loaded) || (moved && _error != null)) {
        setState(() {
          if (p > Duration.zero) _loaded = true;
          if (moved) _error = null;
        });
      }
    }));
    _subs.add(_player.stream.duration.listen((d) {
      if (!mounted) return;
      _durN.value = d;
      if (d > Duration.zero && !_loaded) setState(() => _loaded = true);
    }));
    _subs.add(_player.stream.buffer.listen((b) {
      if (mounted) _bufN.value = b;
    }));
    _subs.add(_player.stream.volume.listen((v) {
      if (!mounted) return;
      setState(() {
        _volume = v;
        if (v > 0) {
          _lastNonZeroVolume = v;
          _muted = false;
        }
      });
    }));
    _subs.add(_player.stream.error.listen((e) {
      if (!mounted || e.isEmpty) return;
      if (isFatalPlayerError(e) && !_loaded && !_playing) {
        if (_sourceIdx + 1 < _clipSources.length) {
          _sourceIdx++;
          unawaited(_openSource(_clipSources[_sourceIdx], _loadSeq));
          return;
        }
        setState(() => _error = e);
      }
    }));
    _subs.add(_player.stream.completed.listen((done) {
      if (!done || !mounted) return;
      if (_index + 1 < widget.session.clips.length) {
        _selectClip(_index + 1);
      } else {
        setState(() => _chromeVisible = true);
      }
    }));

    unawaited(_initPlayer());
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) _focus.requestFocus();
    });
  }

  Future<void> _initPlayer() async {
    String hwdecMode = 'auto';
    try {
      final res = await VeyloBridge.instance.settingsGet();
      final st = (res['settings'] as Map<String, dynamic>?) ?? {};
      hwdecMode = st['hwdec_mode']?.toString() ?? 'auto';
    } catch (_) {}

    if (!mounted) return;
    final platform = _player.platform;
    if (platform is NativePlayer) {
      if (Platform.isLinux) {
        try {
          final handle = await platform.handle;
          if (!mounted) return;
          final attached = await _videoChannel.invokeMethod<bool>('attach', handle) ?? false;
          if (!mounted) return;
          if (attached) {
            setState(() => _nativeWaylandVideo = true);
          } else {
            setState(() => _controller = VideoController(_player));
          }
        } catch (_) {
          if (mounted) setState(() => _controller = VideoController(_player));
        }
      }
      if (!mounted) return;
      await applyMpvProfile(
        platform,
        hwdecMode: hwdecMode,
        fit: BoxFit.contain,
        subColor: '#F2EFEA',
        subScale: 1.0,
        subPos: 94,
        subBg: false,
      );
    }
    if (!mounted) return;
    _playerReady = true;
    unawaited(_loadClip(_index));
  }

  void _syncWaylandRect(Rect rect) {
    if (!_nativeWaylandVideo) return;
    final x = rect.left.round();
    final y = rect.top.round();
    final w = rect.width.round().clamp(2, 16384);
    final h = rect.height.round().clamp(2, 16384);
    if (x == _sentX && y == _sentY && w == _sentW && h == _sentH) return;
    _sentX = x;
    _sentY = y;
    _sentW = w;
    _sentH = h;
    unawaited(
      _videoChannel.invokeMethod<bool>('setRect', {'x': x, 'y': y, 'w': w, 'h': h}).catchError((_) => false),
    );
  }

  void _selectClip(int nextIdx) {
    final n = widget.session.clips.length;
    if (n == 0) return;
    final clamped = ((nextIdx % n) + n) % n;
    if (clamped == _index && _error == null) return;
    setState(() {
      _index = clamped;
      _error = null;
      _loaded = false;
      _resolving = true;
      _buffering = true;
      _quality = null;
      _hdr = null;
      _captions = const [];
      _captionsOn = false;
    });
    _posN.value = Duration.zero;
    _durN.value = Duration.zero;
    _bufN.value = Duration.zero;
    if (_playerReady) {
      unawaited(_loadClip(clamped));
    }
  }

  Future<void> _loadClip(int idx) async {
    final seq = ++_loadSeq;
    final clip = widget.session.clips[idx];
    setState(() {
      _resolving = true;
      _buffering = true;
      _loaded = false;
      _error = null;
      _captionsOn = false;
    });

    try {
      List<SourceItem> sources;
      List<Map<String, dynamic>> captions = const [];

      if ((clip.youtube == null || clip.youtube!.isEmpty) && clip.url != null && clip.url!.isNotEmpty) {
        sources = [
          SourceItem(
            addon: 'Direct',
            name: clip.label,
            description: '',
            url: clip.url!,
            filename: '',
            kind: 'direct',
            headers: const {},
            offTitle: false,
          ),
        ];
      } else {
        final ytId = clip.youtube ?? '';
        if (ytId.isEmpty) throw Exception('No video source available');
        final res = await VeyloBridge.instance.youtubeStream(ytId);
        if (!mounted || seq != _loadSeq) return;
        sources = (res['sources'] as List<dynamic>? ?? const [])
            .whereType<Map<String, dynamic>>()
            .map(SourceItem.fromJson)
            .toList();
        if (sources.isEmpty) {
          final bestMap = res['best'] as Map<String, dynamic>?;
          if (bestMap != null) sources = [SourceItem.fromJson(bestMap)];
        }
        if (sources.isEmpty) throw Exception('No playable stream found');
        captions = (res['captions'] as List<dynamic>? ?? const [])
            .whereType<Map<String, dynamic>>()
            .toList();
      }

      if (!mounted || seq != _loadSeq) return;
      _clipSources = sources;
      _sourceIdx = 0;
      setState(() {
        _resolving = false;
        _captions = captions;
      });
      await _openSource(sources.first, seq);
    } catch (e) {
      if (!mounted || seq != _loadSeq) return;
      setState(() {
        _resolving = false;
        _buffering = false;
        _error = 'Could not load video: $e';
      });
    }
  }

  Future<void> _openSource(SourceItem src, int seq) async {
    if (!mounted || seq != _loadSeq) return;
    final platform = _player.platform;
    if (platform is NativePlayer) {
      final ua = src.headers['User-Agent'] ?? src.headers['user-agent'] ?? '';
      final isHls = src.url.contains('.m3u8') || src.url.contains('/hls_variant/');
      try {
        await platform.setProperty('user-agent', ua);
        // FFmpeg's HLS demuxer deadlocks when `reconnect_streamed=1` is set in `stream-lavf-o`.
        await platform.setProperty(
          'stream-lavf-o',
          isHls
              ? ''
              : 'reconnect=1,reconnect_streamed=1,reconnect_on_network_error=1,reconnect_on_http_error=5xx,reconnect_delay_max=5',
        );
      } catch (_) {}
    }
    if (!mounted || seq != _loadSeq) return;
    setState(() {
      _quality = src.quality;
      _hdr = src.hdr;
      _buffering = true;
      _error = null;
    });
    // Pass only non-User-Agent headers to Media so libmpv uses `user-agent` cleanly without duplicate headers.
    final extraHeaders = Map<String, String>.from(src.headers)
      ..removeWhere((k, _) => k.toLowerCase() == 'user-agent');
    await _player.open(
      Media(src.url, httpHeaders: extraHeaders.isEmpty ? null : extraHeaders),
      play: true,
    );
  }

  Future<void> _setFullscreen(bool enable) async {
    if (_fullscreen == enable) return;
    setState(() => _fullscreen = enable);
    try {
      await _windowChannel.invokeMethod<bool>('setFullscreen', enable);
    } catch (_) {}
    _pokeChrome();
  }

  void _pokeChrome() {
    if (!_chromeVisible && mounted) setState(() => _chromeVisible = true);
    _scheduleHide();
  }

  void _scheduleHide() {
    _hideTimer?.cancel();
    if (!_playing) return;
    _hideTimer = Timer(const Duration(seconds: 3), () {
      if (mounted && _playing && _scrubValue == null) {
        setState(() => _chromeVisible = false);
      }
    });
  }

  void _seekBy(int deltaSecs) {
    final dur = _durN.value.inMilliseconds;
    final target = (_posN.value.inMilliseconds + deltaSecs * 1000).clamp(0, dur > 0 ? dur : 1 << 30);
    unawaited(_player.seek(Duration(milliseconds: target)));
    _pokeChrome();
  }

  void _toggleMute() {
    if (_muted || _volume <= 0) {
      _muted = false;
      unawaited(_player.setVolume(_lastNonZeroVolume > 0 ? _lastNonZeroVolume : 100));
    } else {
      _lastNonZeroVolume = _volume;
      _muted = true;
      unawaited(_player.setVolume(0));
    }
    setState(() {});
    _pokeChrome();
  }

  void _toggleCaptions() {
    if (_captions.isEmpty) return;
    if (_captionsOn) {
      unawaited(_player.setSubtitleTrack(SubtitleTrack.no()));
      setState(() => _captionsOn = false);
    } else {
      final first = _captions.first;
      final url = first['url']?.toString() ?? '';
      if (url.isNotEmpty) {
        unawaited(_player.setSubtitleTrack(SubtitleTrack.uri(url, title: first['language']?.toString())));
        setState(() => _captionsOn = true);
      }
    }
    _pokeChrome();
  }

  Future<void> _shutdownPlayer() async {
    if (_nativeWaylandVideo || Platform.isLinux) {
      try {
        await _videoChannel.invokeMethod<void>('detach');
      } catch (_) {}
    }
    try {
      await _player.dispose();
    } catch (_) {}
  }

  @override
  void dispose() {
    _loadSeq++;
    _hideTimer?.cancel();
    if (_fullscreen) {
      unawaited(_windowChannel.invokeMethod<bool>('setFullscreen', false).catchError((_) => false));
    }
    for (final s in _subs) {
      unawaited(s.cancel());
    }
    _posN.dispose();
    _durN.dispose();
    _bufN.dispose();
    unawaited(_shutdownPlayer());
    _focus.dispose();
    super.dispose();
  }

  KeyEventResult _handleKey(KeyEvent event) {
    if (event is! KeyDownEvent) return KeyEventResult.ignored;
    final k = event.logicalKey;
    final n = widget.session.clips.length;
    if (k == LogicalKeyboardKey.escape) {
      if (_fullscreen) {
        unawaited(_setFullscreen(false));
      } else {
        AppStateScope.read(context).closeTheatre();
      }
      return KeyEventResult.handled;
    }
    if (k == LogicalKeyboardKey.space || k == LogicalKeyboardKey.keyK || k == LogicalKeyboardKey.mediaPlayPause) {
      unawaited(_player.playOrPause());
      _pokeChrome();
      return KeyEventResult.handled;
    }
    if (k == LogicalKeyboardKey.keyJ) {
      _seekBy(-10);
      return KeyEventResult.handled;
    }
    if (k == LogicalKeyboardKey.keyL) {
      _seekBy(10);
      return KeyEventResult.handled;
    }
    if (k == LogicalKeyboardKey.keyM) {
      _toggleMute();
      return KeyEventResult.handled;
    }
    if (k == LogicalKeyboardKey.keyF) {
      unawaited(_setFullscreen(!_fullscreen));
      return KeyEventResult.handled;
    }
    if (k == LogicalKeyboardKey.arrowLeft && n > 1) {
      _selectClip(_index - 1);
      return KeyEventResult.handled;
    }
    if (k == LogicalKeyboardKey.arrowRight && n > 1) {
      _selectClip(_index + 1);
      return KeyEventResult.handled;
    }
    final ch = event.character;
    if (ch != null && ch.length == 1) {
      final d = int.tryParse(ch);
      if (d != null && d >= 1 && d <= 9 && d - 1 < n) {
        _selectClip(d - 1);
        return KeyEventResult.handled;
      }
    }
    return KeyEventResult.ignored;
  }

  static String _clock(Duration d) {
    final s = d.inSeconds.clamp(0, 86400);
    final h = s ~/ 3600;
    final m = (s % 3600) ~/ 60;
    final sec = s % 60;
    final ss = sec.toString().padLeft(2, '0');
    if (h > 0) return '$h:${m.toString().padLeft(2, '0')}:$ss';
    return '$m:$ss';
  }

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final insets = MediaQuery.viewPaddingOf(context);
    final about = widget.session.about;
    final clips = widget.session.clips;
    final clip = clips[_index.clamp(0, clips.length - 1)];
    final logo = (about.logo?.isNotEmpty == true && !_failedLogos.contains(about.logo)) ? about.logo : null;

    return Focus(
      focusNode: _focus,
      autofocus: true,
      onKeyEvent: (_, ev) => _handleKey(ev),
      child: DefaultTextStyle(
        style: VeyloText.body,
        child: ValueListenableBuilder<Color>(
          valueListenable: app.lampColorNotifier,
          builder: (_, lamp, _) {
            return LayoutBuilder(
              builder: (_, constraints) {
                final winW = constraints.maxWidth;
                final winH = constraints.maxHeight;
                final safeWidth = winW - insets.horizontal;
                final safeHeight = winH - insets.vertical;
                final compact = safeWidth < 820;

                // Compute exact deterministic stageRect for both Wayland subsurface & Flutter overlay.
                late final Rect cardRect;
                late final Rect stageRect;
                late final Rect sideRect;
                final double stageRadius = _fullscreen ? 0.0 : 20.0;

                if (_fullscreen) {
                  cardRect = Rect.fromLTWH(0, 0, winW, winH);
                  stageRect = cardRect;
                  sideRect = Rect.zero;
                } else {
                  final cardW = math.min(1760.0, math.max(math.min(320.0, safeWidth), safeWidth - 24));
                  final cardH = math.max(math.min(320.0, safeHeight), safeHeight - 24);
                  final cardL = insets.left + (safeWidth - cardW) / 2.0;
                  final cardT = insets.top + (safeHeight - cardH) / 2.0;
                  cardRect = Rect.fromLTWH(cardL, cardT, cardW, cardH);

                  const pad = 12.0;
                  const gap = 16.0;
                  if (compact) {
                    final availW = cardW - pad * 2;
                    final maxH = (cardH - pad * 2) * 0.48;
                    final fw = math.min(availW, maxH * 16.0 / 9.0);
                    final fh = fw * 9.0 / 16.0;
                    stageRect = Rect.fromLTWH(cardL + pad + (availW - fw) / 2.0, cardT + pad, fw, fh);
                    sideRect = Rect.fromLTWH(
                      cardL + pad,
                      stageRect.bottom + 12.0,
                      availW,
                      math.max(0.0, cardRect.bottom - pad - (stageRect.bottom + 12.0)),
                    );
                  } else {
                    final sideW = (safeWidth * 0.24).clamp(260.0, 340.0);
                    final areaW = math.max(160.0, cardW - pad * 2 - sideW - gap);
                    final areaH = math.max(120.0, cardH - pad * 2);
                    final fw = math.min(areaW, areaH * 16.0 / 9.0);
                    final fh = fw * 9.0 / 16.0;
                    stageRect = Rect.fromLTWH(
                      cardL + pad + (areaW - fw) / 2.0,
                      cardT + pad + (areaH - fh) / 2.0,
                      fw,
                      fh,
                    );
                    sideRect = Rect.fromLTWH(
                      cardL + pad + areaW + gap,
                      cardT + pad,
                      sideW,
                      areaH,
                    );
                  }
                }

                // Keep Wayland subsurface tightly aligned with stageRect.
                _syncWaylandRect(stageRect);

                // Background + smoked glass card clipped with an even-odd cutout for stageRect
                // so the hardware Wayland video subsurface shows through with true 20px rounded corners.
                final Widget backdropAndCard = ClipPath(
                  clipper: _nativeWaylandVideo ? _HoleClipper(stageRect, stageRadius) : null,
                  child: Stack(
                    fit: StackFit.expand,
                    children: [
                      // Dimmed cinema scrim (tap outside card to dismiss)
                      GestureDetector(
                        behavior: HitTestBehavior.opaque,
                        onTap: () {
                          if (!_fullscreen) app.closeTheatre();
                        },
                        child: Stack(
                          fit: StackFit.expand,
                          children: [
                            const ColoredBox(color: Color.fromRGBO(0, 0, 0, 0.78)),
                            if (about.backdrop.isNotEmpty)
                              Opacity(
                                opacity: 0.34,
                                child: ImageFiltered(
                                  imageFilter: ImageFilter.blur(sigmaX: 48, sigmaY: 48),
                                  child: CachedNetworkImage(
                                    imageUrl: about.backdrop,
                                    fit: BoxFit.cover,
                                    memCacheWidth: 320,
                                    errorWidget: (_, _, _) => const SizedBox.shrink(),
                                  ),
                                ),
                              ),
                          ],
                        ),
                      ),
                      // Floating smoked glass Theatre shell (.theatre.glass)
                      if (!_fullscreen)
                        Positioned.fromRect(
                          rect: cardRect,
                          child: GestureDetector(
                            onTap: () {}, // absorb clicks inside the card
                            child: Container(
                              decoration: BoxDecoration(
                                borderRadius: BorderRadius.circular(30),
                                color: VeyloColors.mixLamp(
                                  lamp,
                                  const Color.fromRGBO(16, 17, 22, 0.88),
                                  0.08,
                                ),
                                border: Border.all(
                                  color: VeyloColors.mixLamp(lamp, VeyloColors.edge, 0.14),
                                  width: 1,
                                ),
                                boxShadow: const [
                                  BoxShadow(
                                    color: Color.fromRGBO(0, 0, 0, 0.65),
                                    blurRadius: 64,
                                    offset: Offset(0, 28),
                                  ),
                                ],
                              ),
                            ),
                          ),
                        ),
                    ],
                  ),
                );

                return Stack(
                  fit: StackFit.expand,
                  children: [
                    backdropAndCard,

                    // Stage: native video (Video widget on macOS/Windows/Android/X11; transparent hole on Wayland)
                    Positioned.fromRect(
                      rect: stageRect,
                      child: ClipRRect(
                        borderRadius: BorderRadius.circular(stageRadius),
                        child: MouseRegion(
                          onHover: (_) => _pokeChrome(),
                          onExit: (_) {
                            if (_playing && _scrubValue == null) _scheduleHide();
                          },
                          child: Stack(
                            fit: StackFit.expand,
                            children: [
                              if (!_nativeWaylandVideo)
                                ColoredBox(
                                  color: const Color(0xFF000000),
                                  child: _controller != null
                                      ? Video(
                                          controller: _controller!,
                                          fit: BoxFit.contain,
                                          controls: NoVideoControls,
                                          fill: const Color(0xFF000000),
                                        )
                                      : const SizedBox.shrink(),
                                ),
                              // Click stage to play/pause; double-click for fullscreen
                              GestureDetector(
                                behavior: HitTestBehavior.translucent,
                                onTap: () {
                                  if (_loaded) {
                                    unawaited(_player.playOrPause());
                                  }
                                  _pokeChrome();
                                },
                                onDoubleTap: () => unawaited(_setFullscreen(!_fullscreen)),
                                child: const SizedBox.expand(),
                              ),

                              // Minimal film-leader countdown vignette while resolving/buffering
                              if ((_resolving || (_buffering && !_loaded)) && _error == null)
                                Center(
                                  child: IgnorePointer(
                                    child: FindingRunner(
                                      lamp: lamp,
                                      scene: CinemaScene.countdown,
                                      width: 240,
                                      height: 86,
                                    ),
                                  ),
                                ),

                              // Error card with Retry
                              if (_error != null)
                                Center(
                                  child: Container(
                                    constraints: const BoxConstraints(maxWidth: 420),
                                    padding: const EdgeInsets.all(20),
                                    decoration: BoxDecoration(
                                      color: const Color.fromRGBO(12, 13, 18, 0.90),
                                      borderRadius: BorderRadius.circular(20),
                                      border: Border.all(color: VeyloColors.edge),
                                    ),
                                    child: Column(
                                      mainAxisSize: MainAxisSize.min,
                                      children: [
                                        const VeyloIcon('Alert', size: 26, color: VeyloColors.amber),
                                        const SizedBox(height: 10),
                                        Text(
                                          _error!,
                                          textAlign: TextAlign.center,
                                          style: VeyloText.ui(14, weight: 500, color: VeyloColors.dust),
                                        ),
                                        const SizedBox(height: 14),
                                        VeyloPill(
                                          label: 'Retry',
                                          icon: 'Undo',
                                          accent: true,
                                          small: true,
                                          onTap: () => unawaited(_loadClip(_index)),
                                        ),
                                      ],
                                    ),
                                  ),
                                ),

                              // Subtle transport bar inside the 16:9 stage
                              Positioned(
                                left: _fullscreen ? insets.left : 0,
                                right: _fullscreen ? insets.right : 0,
                                bottom: _fullscreen ? insets.bottom : 0,
                                child: IgnorePointer(
                                  ignoring: !_chromeVisible,
                                  child: AnimatedOpacity(
                                    opacity: _chromeVisible ? 1.0 : 0.0,
                                    duration: const Duration(milliseconds: 200),
                                    child: Container(
                                      padding: const EdgeInsets.fromLTRB(14, 34, 14, 12),
                                      decoration: const BoxDecoration(
                                        gradient: LinearGradient(
                                          begin: Alignment.topCenter,
                                          end: Alignment.bottomCenter,
                                          colors: [
                                            Color.fromRGBO(0, 0, 0, 0.0),
                                            Color.fromRGBO(0, 0, 0, 0.82),
                                          ],
                                        ),
                                      ),
                                      child: Column(
                                        mainAxisSize: MainAxisSize.min,
                                        children: [
                                          // Scrubber bar
                                          ListenableBuilder(
                                            listenable: _timelineListenable,
                                            builder: (_, _) {
                                              final durSec = _durN.value.inMilliseconds / 1000.0;
                                              final posSec = _scrubValue ?? (_posN.value.inMilliseconds / 1000.0);
                                              final pct = durSec > 0 ? (posSec / durSec).clamp(0.0, 1.0) : 0.0;
                                              final bufPct = durSec > 0 ? (_bufN.value.inMilliseconds / 1000.0 / durSec).clamp(0.0, 1.0) : 0.0;
                                              return Row(
                                                children: [
                                                  Text(
                                                    _clock(Duration(milliseconds: (posSec * 1000).round())),
                                                    style: VeyloText.ui(12, weight: 600, tabular: true),
                                                  ),
                                                  const SizedBox(width: 10),
                                                  Expanded(
                                                    child: _TheatreScrubber(
                                                      played: pct,
                                                      buffered: bufPct,
                                                      lamp: lamp,
                                                      onScrub: (p) {
                                                        if (durSec <= 0) return;
                                                        setState(() => _scrubValue = p * durSec);
                                                        _pokeChrome();
                                                      },
                                                      onCommit: (p) {
                                                        if (durSec <= 0) return;
                                                        final targetMs = (p * durSec * 1000).round();
                                                        setState(() => _scrubValue = null);
                                                        unawaited(_player.seek(Duration(milliseconds: targetMs)));
                                                        _pokeChrome();
                                                      },
                                                    ),
                                                  ),
                                                  const SizedBox(width: 10),
                                                  Text(
                                                    _clock(_durN.value),
                                                    style: VeyloText.ui(12, weight: 600, color: VeyloColors.dust, tabular: true),
                                                  ),
                                                ],
                                              );
                                            },
                                          ),
                                          const SizedBox(height: 6),
                                          // Transport controls row
                                          Row(
                                            children: [
                                              VeyloOrb(
                                                icon: _playing ? 'Pause' : 'Play',
                                                glass: false,
                                                size: 38,
                                                iconSize: 20,
                                                tooltip: _playing ? 'Pause (Space)' : 'Play (Space)',
                                                onTap: () {
                                                  unawaited(_player.playOrPause());
                                                  _pokeChrome();
                                                },
                                              ),
                                              VeyloOrb(
                                                icon: 'Rewind10',
                                                glass: false,
                                                size: 36,
                                                iconSize: 19,
                                                tooltip: 'Back 10s (J)',
                                                onTap: () => _seekBy(-10),
                                              ),
                                              VeyloOrb(
                                                icon: 'Forward10',
                                                glass: false,
                                                size: 36,
                                                iconSize: 19,
                                                tooltip: 'Forward 10s (L)',
                                                onTap: () => _seekBy(10),
                                              ),
                                              const SizedBox(width: 8),
                                              if (_quality != null)
                                                Container(
                                                  padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 3),
                                                  decoration: BoxDecoration(
                                                    color: const Color.fromRGBO(255, 255, 255, 0.12),
                                                    borderRadius: BorderRadius.circular(8),
                                                  ),
                                                  child: Text(
                                                    _hdr != null ? '$_quality · $_hdr' : _quality!,
                                                    style: VeyloText.ui(11.5, weight: 700, tabular: true),
                                                  ),
                                                ),
                                              const Spacer(),
                                              if (_captions.isNotEmpty)
                                                VeyloOrb(
                                                  icon: 'Subtitles',
                                                  glass: false,
                                                  size: 36,
                                                  iconSize: 19,
                                                  active: _captionsOn,
                                                  tooltip: _captionsOn ? 'Turn off captions' : 'Captions',
                                                  onTap: _toggleCaptions,
                                                ),
                                              VeyloOrb(
                                                icon: _muted || _volume <= 0 ? 'Muted' : 'Audio',
                                                glass: false,
                                                size: 36,
                                                iconSize: 19,
                                                tooltip: 'Mute (M)',
                                                onTap: _toggleMute,
                                              ),
                                              VeyloOrb(
                                                icon: 'Expand',
                                                glass: false,
                                                size: 36,
                                                iconSize: 19,
                                                active: _fullscreen,
                                                tooltip: _fullscreen ? 'Exit fullscreen (F)' : 'Fullscreen (F)',
                                                onTap: () => unawaited(_setFullscreen(!_fullscreen)),
                                              ),
                                            ],
                                          ),
                                        ],
                                      ),
                                    ),
                                  ),
                                ),
                              ),

                              // Subtle hairline frame rim around the 16:9 video box
                              if (!_fullscreen)
                                IgnorePointer(
                                  child: DecoratedBox(
                                    decoration: BoxDecoration(
                                      borderRadius: BorderRadius.circular(stageRadius),
                                      border: Border.all(color: const Color.fromRGBO(255, 255, 255, 0.12)),
                                    ),
                                  ),
                                ),
                            ],
                          ),
                        ),
                      ),
                    ),

                    // Right side column (.theatre-side) — exact DX layout
                    if (!_fullscreen && sideRect.width > 0 && sideRect.height > 0)
                      Positioned.fromRect(
                        rect: sideRect,
                        child: Padding(
                          padding: const EdgeInsets.only(top: 4, right: 4),
                          child: Column(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            children: [
                              // .theatre-tools
                              Row(
                                children: [
                                  if (clips.length > 1) ...[
                                    VeyloOrb(
                                      icon: 'ArrowLeft',
                                      size: 38,
                                      iconSize: 18,
                                      tooltip: 'Previous video (←)',
                                      onTap: () => _selectClip(_index - 1),
                                    ),
                                    SizedBox(
                                      width: 46,
                                      child: Text(
                                        '${_index + 1} / ${clips.length}',
                                        textAlign: TextAlign.center,
                                        style: VeyloText.ui(13, weight: 700, color: VeyloColors.dust, tabular: true),
                                      ),
                                    ),
                                    VeyloOrb(
                                      icon: 'ArrowRight',
                                      size: 38,
                                      iconSize: 18,
                                      tooltip: 'Next video (→)',
                                      onTap: () => _selectClip(_index + 1),
                                    ),
                                  ],
                                  const Spacer(),
                                  VeyloOrb(
                                    icon: 'Expand',
                                    size: 38,
                                    iconSize: 18,
                                    tooltip: 'Fullscreen (F)',
                                    onTap: () => unawaited(_setFullscreen(true)),
                                  ),
                                  const SizedBox(width: 6),
                                  VeyloOrb(
                                    icon: 'Close',
                                    size: 38,
                                    iconSize: 18,
                                    tooltip: 'Close (Esc)',
                                    onTap: app.closeTheatre,
                                  ),
                                ],
                              ),

                              const SizedBox(height: 14),

                              // .theatre-titles (Logo or Display Title + Rating & Facts)
                              if (logo != null)
                                ConstrainedBox(
                                  constraints: const BoxConstraints(maxHeight: 52, maxWidth: 280),
                                  child: CachedNetworkImage(
                                    imageUrl: logo,
                                    fit: BoxFit.contain,
                                    alignment: Alignment.centerLeft,
                                    memCacheWidth: 560,
                                    errorWidget: (_, _, _) {
                                      WidgetsBinding.instance.addPostFrameCallback((_) {
                                        if (mounted) setState(() => _failedLogos.add(logo));
                                      });
                                      return Text(
                                        about.title,
                                        maxLines: 2,
                                        overflow: TextOverflow.ellipsis,
                                        style: VeyloText.display(26, weight: 800, height: 1.05),
                                      );
                                    },
                                  ),
                                )
                              else
                                Text(
                                  about.title,
                                  maxLines: 2,
                                  overflow: TextOverflow.ellipsis,
                                  style: VeyloText.display(26, weight: 800, height: 1.05),
                                ),

                              if ((about.rating?.isNotEmpty == true) || about.facts.isNotEmpty) ...[
                                const SizedBox(height: 8),
                                Wrap(
                                  spacing: 12,
                                  runSpacing: 4,
                                  crossAxisAlignment: WrapCrossAlignment.center,
                                  children: [
                                    if (about.rating?.isNotEmpty == true)
                                      Row(
                                        mainAxisSize: MainAxisSize.min,
                                        children: [
                                          const VeyloIcon('Star', size: 15, color: VeyloColors.amber, filled: true),
                                          const SizedBox(width: 5),
                                          Text(
                                            about.rating!,
                                            style: VeyloText.ui(13.5, weight: 700, color: VeyloColors.bone, tabular: true),
                                          ),
                                        ],
                                      ),
                                    for (final f in about.facts)
                                      Text(
                                        f,
                                        style: VeyloText.ui(13.5, weight: 600, color: VeyloColors.dust),
                                      ),
                                  ],
                                ),
                              ],

                              const SizedBox(height: 12),

                              // .theatre-now (Official/kind pill + clip title)
                              Container(
                                height: 28,
                                padding: const EdgeInsets.symmetric(horizontal: 11),
                                decoration: BoxDecoration(
                                  color: clip.official ? lamp : const Color.fromRGBO(255, 255, 255, 0.10),
                                  borderRadius: BorderRadius.circular(14),
                                ),
                                child: Row(
                                  mainAxisSize: MainAxisSize.min,
                                  children: [
                                    VeyloIcon(
                                      clip.official ? 'Check' : 'Clapper',
                                      size: 14,
                                      color: clip.official ? const Color(0xFF000000) : VeyloColors.bone,
                                    ),
                                    const SizedBox(width: 6),
                                    Text(
                                      clip.label,
                                      style: VeyloText.ui(
                                        12.5,
                                        weight: 700,
                                        color: clip.official ? const Color(0xFF000000) : VeyloColors.bone,
                                      ),
                                    ),
                                  ],
                                ),
                              ),
                              if (clip.name.isNotEmpty && clip.name.toLowerCase() != clip.label.toLowerCase()) ...[
                                const SizedBox(height: 6),
                                Text(
                                  clip.name,
                                  maxLines: 2,
                                  overflow: TextOverflow.ellipsis,
                                  style: VeyloText.ui(14.5, weight: 600, height: 1.3),
                                ),
                              ],

                              // .theatre-tagline
                              if (about.tagline?.isNotEmpty == true) ...[
                                const SizedBox(height: 10),
                                Text(
                                  '“${about.tagline!}”',
                                  maxLines: 3,
                                  overflow: TextOverflow.ellipsis,
                                  style: VeyloText.ui(
                                    14.5,
                                    weight: 400,
                                    fontStyle: FontStyle.italic,
                                    color: const Color(0xFFE6E3DE),
                                    height: 1.35,
                                  ),
                                ),
                              ],

                              // .theatre-credits
                              if (about.directors.isNotEmpty || about.cast.isNotEmpty) ...[
                                const SizedBox(height: 10),
                                if (about.directors.isNotEmpty)
                                  Text.rich(
                                    TextSpan(
                                      style: VeyloText.ui(13, weight: 500, height: 1.35),
                                      children: [
                                        TextSpan(
                                          text: '${about.directorRole ?? 'Directed by'} ',
                                          style: const TextStyle(color: VeyloColors.dust),
                                        ),
                                        TextSpan(text: about.directors.join(', ')),
                                      ],
                                    ),
                                    maxLines: 2,
                                    overflow: TextOverflow.ellipsis,
                                  ),
                                if (about.cast.isNotEmpty) ...[
                                  const SizedBox(height: 3),
                                  Text.rich(
                                    TextSpan(
                                      style: VeyloText.ui(13, weight: 500, height: 1.35),
                                      children: [
                                        const TextSpan(text: 'With ', style: TextStyle(color: VeyloColors.dust)),
                                        TextSpan(text: about.cast.join(', ')),
                                      ],
                                    ),
                                    maxLines: 2,
                                    overflow: TextOverflow.ellipsis,
                                  ),
                                ],
                              ],

                              const SizedBox(height: 12),

                              // .theatre-list (scrollable thumbnail strip of all clips)
                              if (clips.length > 1)
                                Expanded(
                                  child: ListView.separated(
                                    padding: const EdgeInsets.symmetric(vertical: 2),
                                    itemCount: clips.length,
                                    separatorBuilder: (_, _) => const SizedBox(height: 6),
                                    itemBuilder: (_, i) => _TheatreClipTile(
                                      clip: clips[i],
                                      index: i,
                                      active: i == _index,
                                      lamp: lamp,
                                      onTap: () => _selectClip(i),
                                    ),
                                  ),
                                )
                              else
                                const Spacer(),

                              // .theatre-keys (keyboard shortcut legend)
                              Container(
                                padding: const EdgeInsets.only(top: 10, bottom: 2),
                                decoration: const BoxDecoration(
                                  border: Border(top: BorderSide(color: Color.fromRGBO(255, 255, 255, 0.08))),
                                ),
                                child: Wrap(
                                  spacing: 12,
                                  runSpacing: 6,
                                  children: [
                                    _keyHint(['Space'], 'Play'),
                                    _keyHint(['J', 'L'], '10 s'),
                                    _keyHint(['M'], 'Mute'),
                                    _keyHint(['F'], 'Full'),
                                    if (clips.length > 1) _keyHint(['←', '→'], 'Videos'),
                                  ],
                                ),
                              ),
                            ],
                          ),
                        ),
                      ),
                  ],
                );
              },
            );
          },
        ),
      ),
    );
  }

  Widget _keyHint(List<String> keys, String label) => Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          for (final k in keys) ...[
            Keycap(k, small: true),
            const SizedBox(width: 4),
          ],
          Text(label, style: VeyloText.ui(12, weight: 500, color: VeyloColors.dust)),
        ],
      );
}

class _HoleClipper extends CustomClipper<Path> {
  const _HoleClipper(this.hole, this.radius);
  final Rect hole;
  final double radius;

  @override
  Path getClip(Size size) {
    final p = Path()..fillType = PathFillType.evenOdd;
    p.addRect(Offset.zero & size);
    if (radius > 0) {
      p.addRRect(RRect.fromRectAndRadius(hole, Radius.circular(radius)));
    } else {
      p.addRect(hole);
    }
    return p;
  }

  @override
  bool shouldReclip(covariant _HoleClipper old) => old.hole != hole || old.radius != radius;
}

class _TheatreClipTile extends StatefulWidget {
  const _TheatreClipTile({
    required this.clip,
    required this.index,
    required this.active,
    required this.lamp,
    required this.onTap,
  });

  final TrailerClip clip;
  final int index;
  final bool active;
  final Color lamp;
  final VoidCallback onTap;

  @override
  State<_TheatreClipTile> createState() => _TheatreClipTileState();
}

class _TheatreClipTileState extends State<_TheatreClipTile> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final yt = widget.clip.youtube ?? '';
    final thumb = yt.isNotEmpty ? 'https://i.ytimg.com/vi/$yt/mqdefault.jpg' : '';
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      child: GestureDetector(
        onTap: widget.onTap,
        child: AnimatedContainer(
          duration: const Duration(milliseconds: 180),
          padding: const EdgeInsets.all(5),
          decoration: BoxDecoration(
            borderRadius: BorderRadius.circular(14),
            color: widget.active
                ? const Color.fromRGBO(255, 255, 255, 0.09)
                : (_hovered ? const Color.fromRGBO(255, 255, 255, 0.05) : const Color(0x00000000)),
          ),
          child: AnimatedOpacity(
            duration: const Duration(milliseconds: 180),
            opacity: (widget.active || _hovered) ? 1.0 : 0.72,
            child: Row(
              children: [
                Container(
                  width: 112,
                  height: 63,
                  decoration: BoxDecoration(
                    borderRadius: BorderRadius.circular(10),
                    border: Border.all(
                      color: widget.active ? widget.lamp : VeyloColors.edge,
                      width: widget.active ? 2 : 1,
                    ),
                    color: const Color.fromRGBO(255, 255, 255, 0.06),
                  ),
                  child: ClipRRect(
                    borderRadius: BorderRadius.circular(widget.active ? 8 : 9),
                    child: Stack(
                      fit: StackFit.expand,
                      children: [
                        if (thumb.isNotEmpty)
                          CachedNetworkImage(
                            imageUrl: thumb,
                            fit: BoxFit.cover,
                            memCacheWidth: 240,
                            errorWidget: (_, _, _) => const ColoredBox(color: Color(0xFF14151A)),
                          )
                        else
                          const ColoredBox(
                            color: Color(0xFF14151A),
                            child: Center(child: VeyloIcon('Clapper', size: 20, color: VeyloColors.dust)),
                          ),
                        if (widget.active)
                          const ColoredBox(
                            color: Color.fromRGBO(0, 0, 0, 0.45),
                            child: Center(child: VeyloIcon('Play', size: 22, color: VeyloColors.bone, filled: true)),
                          ),
                        if (widget.index < 9)
                          Positioned(
                            top: 5,
                            left: 5,
                            child: Container(
                              constraints: const BoxConstraints(minWidth: 20),
                              height: 20,
                              padding: const EdgeInsets.symmetric(horizontal: 5),
                              alignment: Alignment.center,
                              decoration: BoxDecoration(
                                color: const Color.fromRGBO(0, 0, 0, 0.62),
                                borderRadius: BorderRadius.circular(5),
                                border: Border.all(color: const Color.fromRGBO(255, 255, 255, 0.16)),
                              ),
                              child: Text(
                                '${widget.index + 1}',
                                style: VeyloText.ui(11, weight: 700, color: VeyloColors.bone, tabular: true),
                              ),
                            ),
                          ),
                      ],
                    ),
                  ),
                ),
                const SizedBox(width: 10),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      Text(
                        widget.clip.name.isNotEmpty ? widget.clip.name : widget.clip.label,
                        maxLines: 2,
                        overflow: TextOverflow.ellipsis,
                        style: VeyloText.ui(13.5, weight: 600, height: 1.25),
                      ),
                      const SizedBox(height: 3),
                      Text(
                        widget.clip.label,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: VeyloText.ui(
                          12,
                          weight: 600,
                          color: widget.clip.official ? widget.lamp : VeyloColors.dust,
                        ),
                      ),
                    ],
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

class _TheatreScrubber extends StatelessWidget {
  const _TheatreScrubber({
    required this.played,
    required this.buffered,
    required this.lamp,
    required this.onScrub,
    required this.onCommit,
  });

  final double played;
  final double buffered;
  final Color lamp;
  final ValueChanged<double> onScrub;
  final ValueChanged<double> onCommit;

  double _pct(BuildContext ctx, Offset local) {
    final box = ctx.findRenderObject() as RenderBox?;
    if (box == null || box.size.width <= 0) return 0.0;
    return (local.dx / box.size.width).clamp(0.0, 1.0);
  }

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTapDown: (d) => onCommit(_pct(context, d.localPosition)),
        onHorizontalDragUpdate: (d) => onScrub(_pct(context, d.localPosition)),
        onHorizontalDragEnd: (_) => onCommit(played),
        child: SizedBox(
          height: 22,
          child: Center(
            child: ClipRRect(
              borderRadius: BorderRadius.circular(3),
              child: SizedBox(
                height: 5,
                width: double.infinity,
                child: Stack(
                  fit: StackFit.expand,
                  children: [
                    const ColoredBox(color: Color.fromRGBO(255, 255, 255, 0.16)),
                    FractionallySizedBox(
                      alignment: Alignment.centerLeft,
                      widthFactor: buffered.clamp(0.0, 1.0),
                      child: const ColoredBox(color: Color.fromRGBO(255, 255, 255, 0.28)),
                    ),
                    FractionallySizedBox(
                      alignment: Alignment.centerLeft,
                      widthFactor: played.clamp(0.0, 1.0),
                      child: ColoredBox(color: lamp),
                    ),
                  ],
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}
