import 'dart:async';
import 'dart:io' show Platform;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:media_kit/media_kit.dart';
import 'package:media_kit_video/media_kit_video.dart';
import '../bridge/veylo_bridge.dart';
import '../player/mpv_profile.dart';
import '../state/app_state.dart';
import '../theme/veylo_theme.dart';
import '../widgets/glass.dart';
import '../widgets/veylo_icons.dart';

enum _PlayerSheet { audio, subtitles, sources, episodes, speed, sleep, picture, info, shortcuts }

class PlayerScreen extends StatefulWidget {
  const PlayerScreen({super.key, required this.session});
  final PlaySession session;

  /// The mpv player of the screen currently open, for runtime tests (codec/size/drop metrics).
  @visibleForTesting
  static final ValueNotifier<Player?> debugActivePlayer = ValueNotifier<Player?>(null);

  @override
  State<PlayerScreen> createState() => _PlayerScreenState();
}

class _PlayerScreenState extends State<PlayerScreen> {
  static const MethodChannel _windowChannel = MethodChannel('veylo/window');
  static const MethodChannel _videoChannel = MethodChannel('veylo/video');

  late final Player _player;
  VideoController? _controller;
  bool _nativeWaylandVideo = false;
  final FocusNode _focus = FocusNode();

  bool _playing = true;
  bool _buffering = true;
  bool _loaded = false;
  bool _fullscreen = false;
  final ValueNotifier<Duration> _posN = ValueNotifier<Duration>(Duration.zero);
  final ValueNotifier<Duration> _durN = ValueNotifier<Duration>(Duration.zero);
  final ValueNotifier<Duration> _bufN = ValueNotifier<Duration>(Duration.zero);
  late final Listenable _timelineListenable = Listenable.merge([_posN, _durN, _bufN]);
  Duration get _position => _posN.value;
  Duration get _duration => _durN.value;
  Duration get _buffered => _bufN.value;
  double _volume = 100;
  double _lastNonZeroVolume = 100;
  bool _muted = false;
  double _speed = 1.0;
  BoxFit _fit = BoxFit.contain;
  String? _error;

  List<AudioTrack> _audioTracks = [];
  AudioTrack _activeAudio = AudioTrack.auto();
  List<SubtitleTrack> _subTracks = [];
  SubtitleTrack _activeSub = SubtitleTrack.auto();
  List<Map<String, dynamic>> _onlineSubs = [];
  bool _loadingOnlineSubs = false;
  List<Map<String, dynamic>> _segments = [];
  bool _segmentsRequested = false;

  bool _chromeVisible = true;
  bool _showStatsHud = false;
  Timer? _hideTimer;
  _PlayerSheet? _sheet;
  double? _scrubValue;
  String? _osdText;
  Timer? _osdTimer;
  Timer? _progressTimer;
  int _watchedAccumMs = 0;
  bool _savingProgress = false;
  DateTime? _playingSince;
  AppState? _appRef;

  // Sleep timer
  DateTime? _sleepUntil;
  bool _sleepEndOfItem = false;
  Timer? _sleepTick;

  // Subtitle appearance, sync delays, audio night mode & picture adjustments
  double _subScale = 1.0;
  int _subPos = 95;
  bool _subBg = false;
  String _subColor = '#F2EFEA';
  bool _nightMode = false;
  double _audioDelay = 0.0;
  double _subDelay = 0.0;
  int _brightness = 0;
  int _contrast = 0;
  int _saturation = 0;
  int _gamma = 0;
  String? _subLangFilter;
  bool _autoNext = true;
  bool _upNextDismissed = false;

  // Center button kick animation counters
  int _rewKick = 0;
  int _fwdKick = 0;

  final List<StreamSubscription<dynamic>> _subs = [];

  @override
  void initState() {
    super.initState();
    _player = Player(
      configuration: const PlayerConfiguration(
        title: 'Veylo',
        vo: 'libmpv',
        osc: false,
        bufferSize: 256 << 20,
        libass: true,
        logLevel: MPVLogLevel.warn,
      ),
    );
    if (!Platform.isLinux) {
      _controller = VideoController(_player);
    }
    PlayerScreen.debugActivePlayer.value = _player;

    _subs.add(_player.stream.playing.listen((p) {
      if (!mounted) return;
      final now = DateTime.now();
      if (p && _playingSince == null) {
        _playingSince = now;
      } else if (!p && _playingSince != null) {
        _watchedAccumMs += now.difference(_playingSince!).inMilliseconds.clamp(0, 600000);
        _playingSince = null;
      }
      if (_playing != p || (p && !_loaded)) {
        setState(() {
          _playing = p;
          if (p) _loaded = true;
        });
      }
    }));
    _subs.add(_player.stream.buffering.listen((b) {
      if (mounted && _buffering != b) setState(() => _buffering = b);
    }));
    _subs.add(_player.stream.position.listen((p) {
      if (!mounted || _scrubValue != null) return;
      final moved = p > Duration.zero && p != _posN.value;
      _posN.value = p;
      final needState = (p > Duration.zero && !_loaded) || (moved && _error != null);
      if (needState) {
        setState(() {
          if (p > Duration.zero) _loaded = true;
          if (moved && _error != null) _error = null;
        });
      }
    }));
    _subs.add(_player.stream.duration.listen((d) {
      if (!mounted) return;
      _durN.value = d;
      if (d > Duration.zero && !_loaded) {
        setState(() => _loaded = true);
      }
      if (!_segmentsRequested && d > Duration.zero && widget.session.videoId != null) {
        _segmentsRequested = true;
        unawaited(_fetchSegments(widget.session.videoId!, d.inSeconds.toDouble()));
      }
    }));
    _subs.add(_player.stream.completed.listen((done) {
      if (!done || !mounted) return;
      if (_sleepEndOfItem) {
        unawaited(_player.pause());
        _say('Sleep timer finished');
        setState(() => _sleepEndOfItem = false);
        return;
      }
      final s = widget.session;
      if (_autoNext && !_upNextDismissed && s.nextEpisode != null && s.media != null) {
        final app = AppStateScope.read(context);
        unawaited(app.autoplayMedia(
          s.media!,
          videoId: s.nextEpisode!.id,
          episodeLabel: s.nextEpisode!.label,
          seasonEpisodes: s.episodes,
        ));
      }
    }));
    _subs.add(_player.stream.buffer.listen((b) {
      if (mounted) _bufN.value = b;
    }));
    _subs.add(_player.stream.volume.listen((v) {
      if (mounted) {
        setState(() {
          _volume = v;
          if (v > 0) {
            _lastNonZeroVolume = v;
            _muted = false;
          }
        });
      }
    }));
    _subs.add(_player.stream.rate.listen((r) {
      if (mounted) setState(() => _speed = r);
    }));
    _subs.add(_player.stream.tracks.listen((t) {
      if (mounted) {
        setState(() {
          _audioTracks = t.audio;
          _subTracks = t.subtitle;
        });
      }
    }));
    _subs.add(_player.stream.track.listen((t) {
      if (mounted) {
        setState(() {
          _activeAudio = t.audio;
          _activeSub = t.subtitle;
        });
      }
    }));
    _subs.add(_player.stream.error.listen((e) {
      if (!mounted || e.isEmpty) return;
      if (_loaded && _playing) {
        _say('Playback notice: $e');
      } else {
        setState(() => _error = e);
      }
    }));

    _pokeChrome();
    unawaited(_configureAndOpen());
    _progressTimer = Timer.periodic(const Duration(seconds: 5), (_) => unawaited(_saveProgress()));
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      _appRef = AppStateScope.read(context);
      _appRef!.onBeforeClosePlayer = _saveProgress;
      _focus.requestFocus();
    });
  }

  static String? _langCodes(String? lang) => switch (lang?.toLowerCase()) {
        'english' => 'en,eng',
        'italian' => 'it,ita',
        'spanish' => 'es,spa',
        'french' => 'fr,fre,fra',
        'german' => 'de,ger,deu',
        'portuguese' => 'pt,por',
        'japanese' => 'ja,jpn',
        'korean' => 'ko,kor',
        'chinese' => 'zh,chi,zho',
        'russian' => 'ru,rus',
        'dutch' => 'nl,dut,nld',
        'polish' => 'pl,pol',
        'turkish' => 'tr,tur',
        'arabic' => 'ar,ara',
        'hindi' => 'hi,hin',
        _ => null,
      };

  Future<void> _configureAndOpen() async {
    String hwdecMode = 'auto';
    bool passthrough = false;
    bool nightMode = false;
    String? streamLang;
    try {
      final res = await VeyloBridge.instance.settingsGet();
      final st = (res['settings'] as Map<String, dynamic>?) ?? {};
      hwdecMode = st['hwdec_mode']?.toString() ?? 'auto';
      passthrough = st['audio_passthrough'] as bool? ?? false;
      nightMode = st['audio_night_mode'] as bool? ?? false;
      streamLang = st['stream_language']?.toString();
      if (mounted) {
        setState(() {
          _subScale = (st['sub_scale'] as num?)?.toDouble().clamp(0.6, 2.0) ?? 1.0;
          _subPos = (st['sub_pos'] as num?)?.toInt().clamp(70, 100) ?? 95;
          _subBg = st['sub_bg'] as bool? ?? false;
          _subColor = st['sub_color']?.toString() ?? '#F2EFEA';
          _nightMode = nightMode;
          _autoNext = st['auto_next'] as bool? ?? true;
        });
      }
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
        fit: _fit,
        subColor: _subColor,
        subScale: _subScale,
        subPos: _subPos,
        subBg: _subBg,
      );
      Future<void> setProp(String k, String v) async {
        try {
          await platform.setProperty(k, v);
        } catch (_) {}
      }

      // Audio policy.
      await setProp('audio-spdif', passthrough ? 'ac3,eac3,dts,dts-hd,truehd' : '');
      if (nightMode) {
        await setProp('af', 'lavfi=[dynaudnorm=f=150:g=15:p=0.9]');
      }
      final codes = _langCodes(streamLang);
      if (codes != null) {
        await setProp('alang', codes);
      }
      if (widget.session.startMs > 5000) {
        await setProp('start', '+${(widget.session.startMs / 1000.0).toStringAsFixed(1)}');
      } else {
        await setProp('start', 'none');
      }
    }

    if (!mounted) return;
    await _openStream();
  }

  Future<void> _openStream() async {
    if (!mounted) return;
    setState(() {
      _error = null;
      _buffering = true;
    });
    try {
      await _player.open(
        Media(widget.session.url, httpHeaders: widget.session.headers),
        play: true,
      );
    } catch (e) {
      if (mounted) {
        setState(() {
          _buffering = false;
          _error = 'Failed to open stream: $e';
        });
      }
    }
  }

  void _setAudioDelay(double next) {
    final clamped = (next.clamp(-5.0, 5.0) * 100).round() / 100.0;
    setState(() => _audioDelay = clamped);
    final p = _player.platform;
    if (p is NativePlayer) {
      unawaited(p.setProperty('audio-delay', clamped.toStringAsFixed(2)).catchError((_) {}));
    }
    _say('Audio delay ${(clamped * 1000).round()} ms');
  }

  void _setSubDelay(double next) {
    final clamped = (next.clamp(-10.0, 10.0) * 10).round() / 10.0;
    setState(() => _subDelay = clamped);
    final p = _player.platform;
    if (p is NativePlayer) {
      unawaited(p.setProperty('sub-delay', clamped.toStringAsFixed(1)).catchError((_) {}));
    }
    _say('Subtitle delay ${(clamped * 1000).round()} ms');
  }

  void _toggleNightMode() {
    final next = !_nightMode;
    setState(() => _nightMode = next);
    final p = _player.platform;
    if (p is NativePlayer) {
      unawaited(
        p.setProperty('af', next ? 'lavfi=[dynaudnorm=f=150:g=15:p=0.9]' : '').catchError((_) {}),
      );
    }
    unawaited(VeyloBridge.instance.settingsUpdate({'audio_night_mode': next}));
    _say(next ? 'Night mode on' : 'Night mode off');
  }

  void _setPicture({int? brightness, int? contrast, int? saturation, int? gamma}) {
    final p = _player.platform;
    setState(() {
      if (brightness != null) {
        _brightness = brightness.clamp(-50, 50);
        if (p is NativePlayer) unawaited(p.setProperty('brightness', '$_brightness').catchError((_) {}));
      }
      if (contrast != null) {
        _contrast = contrast.clamp(-50, 50);
        if (p is NativePlayer) unawaited(p.setProperty('contrast', '$_contrast').catchError((_) {}));
      }
      if (saturation != null) {
        _saturation = saturation.clamp(-50, 50);
        if (p is NativePlayer) unawaited(p.setProperty('saturation', '$_saturation').catchError((_) {}));
      }
      if (gamma != null) {
        _gamma = gamma.clamp(-50, 50);
        if (p is NativePlayer) unawaited(p.setProperty('gamma', '$_gamma').catchError((_) {}));
      }
    });
  }

  Future<void> _setFullscreen(bool enable) async {
    try {
      final actual = await _windowChannel.invokeMethod<bool>('setFullscreen', enable) ?? enable;
      if (mounted) {
        setState(() => _fullscreen = actual);
        _say(actual ? 'Fullscreen' : 'Windowed');
      }
    } catch (_) {}
  }

  void _updateSubPrefs({double? scale, int? pos, bool? bg, String? color}) {
    final patch = <String, dynamic>{};
    final platform = _player.platform;
    setState(() {
      if (scale != null) {
        _subScale = scale.clamp(0.6, 2.0);
        patch['sub_scale'] = double.parse(_subScale.toStringAsFixed(2));
        if (platform is NativePlayer) {
          unawaited(platform.setProperty('sub-scale', _subScale.toStringAsFixed(2)));
        }
      }
      if (pos != null) {
        _subPos = pos.clamp(70, 100);
        patch['sub_pos'] = _subPos;
        if (platform is NativePlayer) {
          unawaited(platform.setProperty('sub-pos', '$_subPos'));
        }
      }
      if (bg != null) {
        _subBg = bg;
        patch['sub_bg'] = _subBg;
        if (platform is NativePlayer) {
          unawaited(platform.setProperty('sub-back-color', _subBg ? '#A8000000' : '#00000000'));
        }
      }
      if (color != null) {
        _subColor = color;
        patch['sub_color'] = _subColor;
        if (platform is NativePlayer) {
          unawaited(platform.setProperty('sub-color', _subColor));
        }
      }
    });
    if (patch.isNotEmpty) {
      unawaited(VeyloBridge.instance.settingsUpdate(patch));
    }
  }

  Future<void> _shutdownPlayer() async {
    if (identical(PlayerScreen.debugActivePlayer.value, _player)) PlayerScreen.debugActivePlayer.value = null;
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
    if (_appRef?.onBeforeClosePlayer == _saveProgress) {
      _appRef?.onBeforeClosePlayer = null;
    }
    if (_fullscreen) {
      unawaited(_windowChannel.invokeMethod<bool>('setFullscreen', false).catchError((_) => false));
    }
    unawaited(_saveProgress());
    _hideTimer?.cancel();
    _osdTimer?.cancel();
    _progressTimer?.cancel();
    _sleepTick?.cancel();
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

  void _setSleepMinutes(int? minutes, {bool endOfItem = false}) {
    _sleepTick?.cancel();
    setState(() {
      _sleepEndOfItem = endOfItem;
      _sleepUntil = minutes != null ? DateTime.now().add(Duration(minutes: minutes)) : null;
      _sheet = null;
    });
    if (minutes != null) {
      _say('Sleep in $minutes min');
      _sleepTick = Timer.periodic(const Duration(seconds: 1), (_) {
        if (!mounted || _sleepUntil == null) return;
        if (DateTime.now().isAfter(_sleepUntil!)) {
          _sleepTick?.cancel();
          setState(() => _sleepUntil = null);
          unawaited(_player.pause());
          _say('Sleep timer paused playback');
        } else {
          setState(() {});
        }
      });
    } else if (endOfItem) {
      _say('Sleep at end of video');
    } else {
      _say('Sleep timer off');
    }
  }

  String? _sleepBadgeLabel() {
    if (_sleepEndOfItem) return 'End';
    if (_sleepUntil == null) return null;
    final rem = _sleepUntil!.difference(DateTime.now());
    if (rem.isNegative) return null;
    final m = rem.inMinutes;
    final s = rem.inSeconds % 60;
    return m > 0 ? '${m}m' : '${s}s';
  }

  Future<void> _saveProgress() async {
    final m = widget.session.media;
    if (m == null || _duration.inMilliseconds <= 0 || _savingProgress) return;
    _savingProgress = true;
    try {
      if (_playing && _playingSince != null) {
        final now = DateTime.now();
        _watchedAccumMs += now.difference(_playingSince!).inMilliseconds.clamp(0, 600000);
        _playingSince = now;
      }
      final deltaMs = _watchedAccumMs;
      _watchedAccumMs = 0;
      await VeyloBridge.instance.recordProgress(
        m,
        videoId: widget.session.videoId,
        positionMs: _position.inMilliseconds,
        durationMs: _duration.inMilliseconds,
        watchedMs: deltaMs,
      );
    } catch (_) {
    } finally {
      _savingProgress = false;
    }
  }

  Future<void> _fetchSegments(String videoId, double durationSec) async {
    try {
      final segs = await VeyloBridge.instance.onlineSegments(videoId, durationSec);
      if (mounted) setState(() => _segments = segs);
    } catch (_) {}
  }

  Future<void> _fetchOnlineSubs() async {
    final m = widget.session.media;
    if (m == null || _loadingOnlineSubs || _onlineSubs.isNotEmpty) return;
    setState(() => _loadingOnlineSubs = true);
    try {
      final list = await VeyloBridge.instance.onlineSubtitles(
        m.kind,
        widget.session.videoId ?? m.id,
        filename: widget.session.filename,
        fileSize: widget.session.fileSize,
      );
      if (mounted) {
        setState(() {
          _onlineSubs = list;
          _loadingOnlineSubs = false;
        });
      }
    } catch (_) {
      if (mounted) setState(() => _loadingOnlineSubs = false);
    }
  }

  void _pokeChrome() {
    if (!_chromeVisible) setState(() => _chromeVisible = true);
    _hideTimer?.cancel();
    _hideTimer = Timer(const Duration(seconds: 3), () {
      if (mounted && _playing && _sheet == null && _scrubValue == null) {
        setState(() => _chromeVisible = false);
      }
    });
  }

  void _say(String text) {
    setState(() => _osdText = text);
    _osdTimer?.cancel();
    _osdTimer = Timer(const Duration(milliseconds: 1400), () {
      if (mounted) setState(() => _osdText = null);
    });
  }

  void _seekBy(int secs) {
    final rawTarget = _position.inSeconds + secs;
    final target = _duration > Duration.zero
        ? rawTarget.clamp(0, _duration.inSeconds.clamp(0, 86400))
        : rawTarget.clamp(0, 86400);
    unawaited(_player.seek(Duration(seconds: target)));
    setState(() {
      if (secs < 0) {
        _rewKick++;
      } else {
        _fwdKick++;
      }
    });
    _say(secs >= 0 ? '+$secs s' : '−${secs.abs()} s');
    _pokeChrome();
  }

  void _toggleFit() {
    final next = _fit == BoxFit.contain ? BoxFit.cover : BoxFit.contain;
    setState(() => _fit = next);
    final platform = _player.platform;
    if (platform is NativePlayer) {
      unawaited(platform.setProperty('panscan', next == BoxFit.cover ? '1.0' : '0.0').catchError((_) {}));
    }
    _say(next == BoxFit.cover ? 'Fill' : 'Fit');
  }

  void _toggleMute() {
    setState(() => _muted = !_muted);
    final next = _muted ? 0.0 : (_lastNonZeroVolume > 0 ? _lastNonZeroVolume : 100.0);
    unawaited(_player.setVolume(next));
    _say(_muted ? 'Muted' : 'Volume ${next.round()}%');
  }

  String _clock(Duration d) {
    final total = d.inSeconds.clamp(0, 999999);
    final h = total ~/ 3600;
    final m = (total % 3600) ~/ 60;
    final s = total % 60;
    final mm = m.toString().padLeft(2, '0');
    final ss = s.toString().padLeft(2, '0');
    return h > 0 ? '$h:$mm:$ss' : '$m:$ss';
  }

  Map<String, dynamic>? _activeSegment() {
    final sec = _position.inMilliseconds / 1000.0;
    for (final s in _segments) {
      final start = (s['start'] as num?)?.toDouble() ?? 0;
      final end = (s['end'] as num?)?.toDouble() ?? 0;
      if (sec >= start && sec < end - 1.0) return s;
    }
    return null;
  }

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final s = widget.session;
    final visible = _chromeVisible || !_playing || _sheet != null || _scrubValue != null || _error != null || !_loaded;
    final sleepBadge = _sleepBadgeLabel();

    return Focus(
      focusNode: _focus,
      autofocus: true,
      onKeyEvent: (node, event) {
        final isDown = event is KeyDownEvent;
        final isRepeat = event is KeyRepeatEvent;
        if (!isDown && !isRepeat) return KeyEventResult.ignored;

        final ctrl = HardwareKeyboard.instance.isControlPressed || HardwareKeyboard.instance.isMetaPressed;
        final alt = HardwareKeyboard.instance.isAltPressed;
        if (ctrl || alt) return KeyEventResult.ignored;

        final k = event.logicalKey;
        if (isDown && k == LogicalKeyboardKey.escape) {
          if (app.pendingPlayerTitle != null) {
            app.cancelPendingPlay();
          } else if (_sheet != null) {
            setState(() => _sheet = null);
          } else if (_fullscreen) {
            unawaited(_setFullscreen(false));
          } else {
            unawaited(app.closePlayer());
          }
          return KeyEventResult.handled;
        }
        if (k == LogicalKeyboardKey.arrowLeft || (isDown && k == LogicalKeyboardKey.keyJ)) {
          _seekBy(-10);
          return KeyEventResult.handled;
        }
        if (k == LogicalKeyboardKey.arrowRight || (isDown && k == LogicalKeyboardKey.keyL)) {
          _seekBy(10);
          return KeyEventResult.handled;
        }
        if (k == LogicalKeyboardKey.arrowUp) {
          final next = (_volume + 5).clamp(0.0, 130.0);
          unawaited(_player.setVolume(next));
          _say('Volume ${next.round()}%');
          return KeyEventResult.handled;
        }
        if (k == LogicalKeyboardKey.arrowDown) {
          final next = (_volume - 5).clamp(0.0, 130.0);
          unawaited(_player.setVolume(next));
          _say('Volume ${next.round()}%');
          return KeyEventResult.handled;
        }
        if (k == LogicalKeyboardKey.bracketLeft) {
          final next = (_speed - 0.25).clamp(0.25, 3.0);
          unawaited(_player.setRate(next));
          _say('Speed $next×');
          return KeyEventResult.handled;
        }
        if (k == LogicalKeyboardKey.bracketRight) {
          final next = (_speed + 0.25).clamp(0.25, 3.0);
          unawaited(_player.setRate(next));
          _say('Speed $next×');
          return KeyEventResult.handled;
        }

        if (!isDown) return KeyEventResult.ignored;

        if (k == LogicalKeyboardKey.space || k == LogicalKeyboardKey.keyK) {
          unawaited(_player.playOrPause());
          _pokeChrome();
          return KeyEventResult.handled;
        }
        if (k == LogicalKeyboardKey.keyF) {
          unawaited(_setFullscreen(!_fullscreen));
          return KeyEventResult.handled;
        }
        if (k == LogicalKeyboardKey.keyM) {
          _toggleMute();
          return KeyEventResult.handled;
        }
        if (k == LogicalKeyboardKey.keyZ) {
          _toggleFit();
          return KeyEventResult.handled;
        }
        if (k == LogicalKeyboardKey.keyI) {
          setState(() => _showStatsHud = !_showStatsHud);
          return KeyEventResult.handled;
        }
        if (k == LogicalKeyboardKey.keyT) {
          setState(() => _sheet = _sheet == _PlayerSheet.sleep ? null : _PlayerSheet.sleep);
          _pokeChrome();
          return KeyEventResult.handled;
        }
        if (k == LogicalKeyboardKey.keyH || event.character == '?') {
          setState(() => _sheet = _sheet == _PlayerSheet.shortcuts ? null : _PlayerSheet.shortcuts);
          _pokeChrome();
          return KeyEventResult.handled;
        }
        return KeyEventResult.ignored;
      },
      child: ValueListenableBuilder<Color>(
        valueListenable: app.lampColorNotifier,
        builder: (_, lamp, _) => MouseRegion(
          cursor: visible ? SystemMouseCursors.basic : SystemMouseCursors.none,
          onHover: (_) => _pokeChrome(),
          child: ColoredBox(
            color: _nativeWaylandVideo ? const Color(0x00000000) : const Color(0xFF000000),
            child: Stack(
              fit: StackFit.expand,
              children: [
                // Native Wayland subsurface sits cleanly behind Flutter's transparent subsurface;
                // on non-Wayland hosts, fall back to media_kit_video's Video widget.
                if (_nativeWaylandVideo)
                  GestureDetector(
                    behavior: HitTestBehavior.translucent,
                    onTap: () {
                      if (_sheet != null) {
                        setState(() => _sheet = null);
                      } else {
                        unawaited(_player.playOrPause());
                        _pokeChrome();
                      }
                    },
                    onDoubleTap: () => unawaited(_setFullscreen(!_fullscreen)),
                    child: const SizedBox.expand(),
                  )
                else if (_controller != null)
                  RepaintBoundary(
                    child: GestureDetector(
                      behavior: HitTestBehavior.opaque,
                      onTap: () {
                        if (_sheet != null) {
                          setState(() => _sheet = null);
                        } else {
                          unawaited(_player.playOrPause());
                          _pokeChrome();
                        }
                      },
                      onDoubleTap: () => unawaited(_setFullscreen(!_fullscreen)),
                      child: Video(
                        controller: _controller!,
                        controls: NoVideoControls,
                        fit: _fit,
                        fill: const Color(0xFF000000),
                        subtitleViewConfiguration: const SubtitleViewConfiguration(visible: false),
                      ),
                    ),
                  ),

                // Top & Bottom shade gradients when chrome visible
                IgnorePointer(
                  child: AnimatedOpacity(
                    opacity: visible ? 1.0 : 0.0,
                    duration: const Duration(milliseconds: 280),
                    curve: VeyloCurves.out,
                    child: Column(
                      mainAxisAlignment: MainAxisAlignment.spaceBetween,
                      children: [
                        Container(
                          height: 152,
                          decoration: const BoxDecoration(
                            gradient: LinearGradient(
                              begin: Alignment.topCenter,
                              end: Alignment.bottomCenter,
                              colors: [Color.fromRGBO(0, 0, 0, 0.58), Color(0x00000000)],
                            ),
                          ),
                        ),
                        Container(
                          height: 152,
                          decoration: const BoxDecoration(
                            gradient: LinearGradient(
                              begin: Alignment.topCenter,
                              end: Alignment.bottomCenter,
                              colors: [Color(0x00000000), Color.fromRGBO(0, 0, 0, 0.64)],
                            ),
                          ),
                        ),
                      ],
                    ),
                  ),
                ),

                // Stats-for-Nerds HUD (toggle with I)
                if (_showStatsHud)
                  Positioned(
                    top: 86,
                    left: 24,
                    child: IgnorePointer(
                      child: ListenableBuilder(
                        listenable: _timelineListenable,
                        builder: (_, _) => GlassBox(
                          borderRadius: 16,
                          strong: true,
                          padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
                          child: Column(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            mainAxisSize: MainAxisSize.min,
                            children: [
                              Text('Playback telemetry (I)', style: VeyloText.ui(12, weight: 700, color: lamp)),
                              const SizedBox(height: 6),
                              _hudLine('Engine', _nativeWaylandVideo ? 'libmpv (Wayland subsurface)' : 'libmpv (GPU / media_kit)'),
                              _hudLine('Position', '${_clock(_position)} / ${_clock(_duration)}'),
                              _hudLine('Buffered', '${(_buffered - _position).inSeconds.clamp(0, 9999)}s ahead'),
                              _hudLine('Rate / Vol', '$_speed×  ·  ${_muted ? 0 : _volume.round()}%'),
                              if (s.quality != null) _hudLine('Quality', '${s.quality}${s.hdr != null ? ' · ${s.hdr}' : ''}'),
                              if (s.size != null) _hudLine('Size', s.size!),
                              _hudLine('Audio', _activeAudio.title ?? _activeAudio.language ?? _activeAudio.id),
                              _hudLine('Subtitles', _activeSub.id == 'no' ? 'Off' : (_activeSub.title ?? _activeSub.language ?? _activeSub.id)),
                            ],
                          ),
                        ),
                      ),
                    ),
                  ),

                // Center spinner or error panel
                if (_error != null)
                  Center(
                    child: ConstrainedBox(
                      constraints: const BoxConstraints(maxWidth: 520),
                      child: GlassBox(
                        borderRadius: 24,
                        strong: true,
                        padding: const EdgeInsets.all(28),
                        child: Column(
                          mainAxisSize: MainAxisSize.min,
                          children: [
                            const VeyloIcon('Alert', size: 30, color: VeyloColors.amber),
                            const SizedBox(height: 10),
                            Text(_error!, textAlign: TextAlign.center, style: VeyloText.bodyStrong),
                            const SizedBox(height: 16),
                            Wrap(
                              spacing: 10,
                              runSpacing: 10,
                              children: [
                                VeyloPill(
                                  label: 'Retry',
                                  icon: 'Refresh',
                                  accent: true,
                                  onTap: () => unawaited(_openStream()),
                                ),
                                if (s.sources.length > 1)
                                  VeyloPill(
                                    label: 'Try next source',
                                    icon: 'Layers',
                                    onTap: () {
                                      final curIdx = s.sources.indexWhere((x) => x.url == s.url);
                                      final next = s.sources[(curIdx + 1) % s.sources.length];
                                      app.startPlaySession(PlaySession(
                                        title: s.title,
                                        subtitle: s.subtitle,
                                        url: next.url,
                                        headers: next.headers,
                                        startMs: _position.inMilliseconds > 5000 ? _position.inMilliseconds : s.startMs,
                                        size: next.size,
                                        hdr: next.hdr,
                                        quality: next.quality,
                                        filename: next.filename.isNotEmpty ? next.filename : null,
                                        fileSize: next.sizeBytes,
                                        media: s.media,
                                        videoId: s.videoId,
                                        sources: s.sources,
                                        episodes: s.episodes,
                                        nextEpisode: s.nextEpisode,
                                      ));
                                    },
                                  ),
                                VeyloPill(label: 'Close', icon: 'Close', onTap: () => app.closePlayer()),
                              ],
                            ),
                          ],
                        ),
                      ),
                    ),
                  )
                else if (_buffering || !_loaded)
                  Center(
                    child: SizedBox(
                      width: 52,
                      height: 52,
                      child: _Spinner(color: lamp),
                    ),
                  ),

                // OSD pill with smooth enter & exit
                Positioned(
                  top: MediaQuery.sizeOf(context).height * 0.18,
                  left: 0,
                  right: 0,
                  child: IgnorePointer(
                    child: Center(
                      child: AnimatedSwitcher(
                        duration: const Duration(milliseconds: 170),
                        reverseDuration: const Duration(milliseconds: 130),
                        switchInCurve: VeyloCurves.out,
                        switchOutCurve: Curves.easeIn,
                        transitionBuilder: (child, anim) => FadeTransition(
                          opacity: anim,
                          child: ScaleTransition(
                            scale: Tween<double>(begin: 0.94, end: 1.0).animate(anim),
                            child: child,
                          ),
                        ),
                        child: _osdText != null
                            ? GlassBox(
                                key: ValueKey(_osdText),
                                borderRadius: 18,
                                strong: true,
                                padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
                                child: Text(_osdText!, style: VeyloText.bodyStrong),
                              )
                            : const SizedBox.shrink(),
                      ),
                    ),
                  ),
                ),

                // Skip Intro / Recap pill + Floating Up Next countdown card (isolated to timeline updates)
                ListenableBuilder(
                  listenable: _timelineListenable,
                  builder: (_, _) {
                    final durSec = _duration.inMilliseconds / 1000.0;
                    final posSec = _scrubValue ?? (_position.inMilliseconds / 1000.0);
                    final seg = _sheet == null ? _activeSegment() : null;
                    final showUpNext = !_upNextDismissed &&
                        _sheet == null &&
                        s.nextEpisode != null &&
                        s.media != null &&
                        durSec > 60 &&
                        ((durSec - posSec) <= 25 && (durSec - posSec) > 0 || seg?['kind'] == 'credits');
                    if (seg == null && !showUpNext) return const SizedBox.shrink();
                    return Stack(
                      fit: StackFit.expand,
                      children: [
                        if (seg != null)
                          Positioned(
                            right: 24,
                            bottom: visible ? 86 : 32,
                            child: VeyloPill(
                              label: switch (seg['kind']) {
                                'recap' => 'Skip recap',
                                'credits' => 'Next episode',
                                _ => 'Skip intro',
                              },
                              icon: 'Next',
                              onTap: () {
                                if (seg['kind'] == 'credits' && s.nextEpisode != null && s.media != null) {
                                  unawaited(app.autoplayMedia(
                                    s.media!,
                                    videoId: s.nextEpisode!.id,
                                    episodeLabel: s.nextEpisode!.label,
                                    seasonEpisodes: s.episodes,
                                  ));
                                } else {
                                  final end = (seg['end'] as num?)?.toDouble() ?? 0;
                                  unawaited(_player.seek(Duration(milliseconds: (end * 1000).round())));
                                }
                              },
                            ),
                          ),
                        if (showUpNext)
                          Positioned(
                            right: 24,
                            bottom: (visible ? 88.0 : 32.0) + (seg != null ? 58.0 : 0.0),
                            child: LiftIn(
                              offsetY: 12,
                              duration: const Duration(milliseconds: 220),
                              child: SizedBox(
                                width: 336,
                                child: GlassBox(
                                  borderRadius: 20,
                                  strong: true,
                                  padding: const EdgeInsets.fromLTRB(16, 14, 16, 14),
                                  child: Column(
                                    mainAxisSize: MainAxisSize.min,
                                    crossAxisAlignment: CrossAxisAlignment.start,
                                    children: [
                                      Text(
                                        _autoNext
                                            ? 'UP NEXT · ${((durSec - posSec).ceil().clamp(1, 99))}S'
                                            : 'UP NEXT',
                                        style: VeyloText.ui(12, weight: 700, color: lamp, letterSpacing: 0.7, tabular: true),
                                      ),
                                      const SizedBox(height: 5),
                                      Text(
                                        '${s.nextEpisode!.label}${s.nextEpisode!.title.isNotEmpty ? ' · ${s.nextEpisode!.title}' : ''}',
                                        maxLines: 2,
                                        overflow: TextOverflow.ellipsis,
                                        style: VeyloText.ui(16, weight: 700, height: 1.25),
                                      ),
                                      const SizedBox(height: 10),
                                      Wrap(
                                        spacing: 8,
                                        runSpacing: 8,
                                        children: [
                                          VeyloPill(
                                            label: 'Play now',
                                            icon: 'Play',
                                            accent: true,
                                            small: true,
                                            onTap: () => app.autoplayMedia(
                                              s.media!,
                                              videoId: s.nextEpisode!.id,
                                              episodeLabel: s.nextEpisode!.label,
                                              seasonEpisodes: s.episodes,
                                            ),
                                          ),
                                          VeyloPill(
                                            label: 'Dismiss',
                                            small: true,
                                            onTap: () => setState(() => _upNextDismissed = true),
                                          ),
                                        ],
                                      ),
                                    ],
                                  ),
                                ),
                              ),
                            ),
                          ),
                      ],
                    );
                  },
                ),

                // Top Chrome (.player-top: edge-slides -12px when hiding)
                Positioned(
                  top: 20,
                  left: 20,
                  right: 20,
                  child: IgnorePointer(
                    ignoring: !visible,
                    child: AnimatedSlide(
                      offset: visible ? Offset.zero : const Offset(0, -0.35),
                      duration: const Duration(milliseconds: 260),
                      curve: VeyloCurves.out,
                      child: AnimatedOpacity(
                        opacity: visible ? 1.0 : 0.0,
                        duration: const Duration(milliseconds: 240),
                        child: Row(
                          children: [
                            VeyloOrb(icon: 'ArrowLeft', glass: false, tooltip: 'Back (Esc)', onTap: () => app.closePlayer()),
                            const SizedBox(width: 10),
                            Expanded(
                              child: Column(
                                crossAxisAlignment: CrossAxisAlignment.start,
                                mainAxisSize: MainAxisSize.min,
                                children: [
                                  Text(
                                    s.title,
                                    maxLines: 1,
                                    overflow: TextOverflow.ellipsis,
                                    style: VeyloText.display(22, weight: 700, height: 1.15),
                                  ),
                                  const SizedBox(height: 2),
                                  Wrap(
                                    spacing: 8,
                                    children: [
                                      if (s.subtitle.isNotEmpty)
                                        Text(s.subtitle, style: VeyloText.ui(13, weight: 500, color: const Color.fromRGBO(242, 239, 234, 0.72))),
                                      if (s.quality != null)
                                        Text('· ${s.quality}', style: VeyloText.ui(13, weight: 500, color: const Color.fromRGBO(242, 239, 234, 0.72))),
                                      if (s.hdr != null)
                                        Text('· ${s.hdr}', style: VeyloText.ui(13, weight: 700, color: lamp)),
                                      if (s.size != null)
                                        Text('· ${s.size}', style: VeyloText.ui(13, weight: 500, color: const Color.fromRGBO(242, 239, 234, 0.72), tabular: true)),
                                    ],
                                  ),
                                ],
                              ),
                            ),
                            if (sleepBadge != null) ...[
                              VeyloPill(
                                label: sleepBadge,
                                icon: 'Moon',
                                small: true,
                                active: true,
                                onTap: () => setState(() => _sheet = _sheet == _PlayerSheet.sleep ? null : _PlayerSheet.sleep),
                              ),
                              const SizedBox(width: 8),
                            ],
                            VeyloOrb(
                              icon: 'Keyboard',
                              glass: false,
                              active: _sheet == _PlayerSheet.shortcuts,
                              tooltip: 'Shortcuts (H)',
                              onTap: () => setState(() => _sheet = _sheet == _PlayerSheet.shortcuts ? null : _PlayerSheet.shortcuts),
                            ),
                            const SizedBox(width: 4),
                            VeyloOrb(
                              icon: 'Info',
                              glass: false,
                              active: _sheet == _PlayerSheet.info,
                              tooltip: 'Details & Stats (I)',
                              onTap: () => setState(() => _sheet = _sheet == _PlayerSheet.info ? null : _PlayerSheet.info),
                            ),
                          ],
                        ),
                      ),
                    ),
                  ),
                ),

                // Center transport (.player-mid: scales 0.94 when hiding, icon-turn on play/pause, kick on ±10s)
                if (_loaded && _error == null)
                  IgnorePointer(
                    ignoring: !visible,
                    child: Center(
                      child: AnimatedScale(
                        scale: visible ? 1.0 : 0.94,
                        duration: const Duration(milliseconds: 240),
                        curve: VeyloCurves.out,
                        child: AnimatedOpacity(
                          opacity: visible ? 1.0 : 0.0,
                          duration: const Duration(milliseconds: 220),
                          child: Row(
                            mainAxisSize: MainAxisSize.min,
                            children: [
                              _KickIconButton(
                                kick: _rewKick,
                                direction: -1,
                                child: VeyloOrb(
                                  icon: 'Rewind10',
                                  glass: false,
                                  size: 54,
                                  iconSize: 30,
                                  tooltip: 'Back 10s (←)',
                                  onTap: () => _seekBy(-10),
                                ),
                              ),
                              const SizedBox(width: 52),
                              _CenterPlayButton(
                                playing: _playing,
                                onTap: () {
                                  unawaited(_player.playOrPause());
                                  _pokeChrome();
                                },
                              ),
                              const SizedBox(width: 52),
                              _KickIconButton(
                                kick: _fwdKick,
                                direction: 1,
                                child: VeyloOrb(
                                  icon: 'Forward10',
                                  glass: false,
                                  size: 54,
                                  iconSize: 30,
                                  tooltip: 'Forward 10s (→)',
                                  onTap: () => _seekBy(10),
                                ),
                              ),
                            ],
                          ),
                        ),
                      ),
                    ),
                  ),

                // Bottom bar (.player-bottom: edge-slides +12px when hiding)
                Positioned(
                  left: 24,
                  right: 20,
                  bottom: 16,
                  child: IgnorePointer(
                    ignoring: !visible,
                    child: AnimatedSlide(
                      offset: visible ? Offset.zero : const Offset(0, 0.35),
                      duration: const Duration(milliseconds: 260),
                      curve: VeyloCurves.out,
                      child: AnimatedOpacity(
                        opacity: visible ? 1.0 : 0.0,
                        duration: const Duration(milliseconds: 240),
                        child: Row(
                          children: [
                            Expanded(
                              child: ListenableBuilder(
                                listenable: _timelineListenable,
                                builder: (_, _) {
                                  final durSec = _duration.inMilliseconds / 1000.0;
                                  final posSec = _scrubValue ?? (_position.inMilliseconds / 1000.0);
                                  final playedPct = durSec > 0 ? (posSec / durSec).clamp(0.0, 1.0) : 0.0;
                                  final bufPct = durSec > 0 ? (_buffered.inMilliseconds / 1000.0 / durSec).clamp(0.0, 1.0) : 0.0;
                                  return Row(
                                    children: [
                                      SizedBox(
                                        width: 58,
                                        child: Text(
                                          _clock(Duration(milliseconds: (posSec * 1000).round())),
                                          textAlign: TextAlign.center,
                                          style: VeyloText.ui(13, weight: 600, color: VeyloColors.bone, tabular: true),
                                        ),
                                      ),
                                      const SizedBox(width: 8),
                                      Expanded(
                                        child: _ScrubberBar(
                                          played: playedPct,
                                          buffered: bufPct,
                                          durationSec: durSec,
                                          segments: _segments,
                                          lamp: lamp,
                                          formatClock: (sec) => _clock(Duration(seconds: sec.round())),
                                          onScrub: (pct) {
                                            setState(() => _scrubValue = pct * durSec);
                                            _pokeChrome();
                                          },
                                          onCommit: (pct) {
                                            final targetMs = (pct * durSec * 1000).round();
                                            setState(() => _scrubValue = null);
                                            unawaited(_player.seek(Duration(milliseconds: targetMs)));
                                            _pokeChrome();
                                          },
                                        ),
                                      ),
                                      const SizedBox(width: 8),
                                      SizedBox(
                                        width: 62,
                                        child: Text(
                                          durSec > 0 ? '-${_clock(Duration(milliseconds: ((durSec - posSec) * 1000).round()))}' : '',
                                          textAlign: TextAlign.center,
                                          style: VeyloText.ui(13, weight: 600, color: VeyloColors.dust, tabular: true),
                                        ),
                                      ),
                                    ],
                                  );
                                },
                              ),
                            ),
                            const SizedBox(width: 6),
                            if (s.episodes.isNotEmpty)
                              VeyloOrb(
                                icon: 'Stack',
                                glass: false,
                                size: 42,
                                active: _sheet == _PlayerSheet.episodes,
                                tooltip: 'Episodes',
                                onTap: () => setState(() => _sheet = _sheet == _PlayerSheet.episodes ? null : _PlayerSheet.episodes),
                              ),
                            if (s.sources.length > 1)
                              VeyloOrb(
                                icon: 'Layers',
                                glass: false,
                                size: 42,
                                active: _sheet == _PlayerSheet.sources,
                                tooltip: 'Switch source',
                                onTap: () => setState(() => _sheet = _sheet == _PlayerSheet.sources ? null : _PlayerSheet.sources),
                              ),
                            VeyloOrb(
                              icon: 'Headphones',
                              glass: false,
                              size: 42,
                              active: _sheet == _PlayerSheet.audio,
                              tooltip: 'Audio tracks',
                              onTap: () => setState(() => _sheet = _sheet == _PlayerSheet.audio ? null : _PlayerSheet.audio),
                            ),
                            VeyloOrb(
                              icon: 'Subtitles',
                              glass: false,
                              size: 42,
                              active: _sheet == _PlayerSheet.subtitles,
                              tooltip: 'Subtitles',
                              onTap: () {
                                final next = _sheet == _PlayerSheet.subtitles ? null : _PlayerSheet.subtitles;
                                setState(() => _sheet = next);
                                if (next == _PlayerSheet.subtitles) unawaited(_fetchOnlineSubs());
                              },
                            ),
                            VeyloOrb(
                              icon: 'Gauge',
                              glass: false,
                              size: 42,
                              active: _sheet == _PlayerSheet.speed,
                              tooltip: 'Playback speed ([ / ])',
                              onTap: () => setState(() => _sheet = _sheet == _PlayerSheet.speed ? null : _PlayerSheet.speed),
                            ),
                            VeyloOrb(
                              icon: 'Moon',
                              glass: false,
                              size: 42,
                              active: _sheet == _PlayerSheet.sleep || sleepBadge != null,
                              tooltip: 'Sleep timer (T)',
                              onTap: () => setState(() => _sheet = _sheet == _PlayerSheet.sleep ? null : _PlayerSheet.sleep),
                            ),
                            VeyloOrb(
                              icon: 'Sliders',
                              glass: false,
                              size: 42,
                              active: _sheet == _PlayerSheet.picture ||
                                  _brightness != 0 ||
                                  _contrast != 0 ||
                                  _saturation != 0 ||
                                  _gamma != 0,
                              tooltip: 'Picture',
                              onTap: () => setState(() => _sheet = _sheet == _PlayerSheet.picture ? null : _PlayerSheet.picture),
                            ),
                            VeyloOrb(
                              icon: 'Resize',
                              glass: false,
                              size: 42,
                              tooltip: 'Fit / Fill (Z)',
                              onTap: _toggleFit,
                            ),
                            VeyloOrb(
                              icon: 'Expand',
                              glass: false,
                              size: 42,
                              active: _fullscreen,
                              tooltip: _fullscreen ? 'Exit fullscreen (F)' : 'Fullscreen (F)',
                              onTap: () => unawaited(_setFullscreen(!_fullscreen)),
                            ),
                            VeyloOrb(
                              icon: _muted || _volume <= 0 ? 'Muted' : 'Audio',
                              glass: false,
                              size: 42,
                              tooltip: 'Mute (M)',
                              onTap: _toggleMute,
                            ),
                            _VolumeSlider(
                              value: _muted ? 0 : _volume,
                              lamp: lamp,
                              onChanged: (v) {
                                _muted = v <= 0;
                                unawaited(_player.setVolume(v));
                                _pokeChrome();
                              },
                            ),
                            if (s.nextEpisode != null && s.media != null) ...[
                              const SizedBox(width: 8),
                              VeyloPill(
                                label: 'Next',
                                icon: 'Next',
                                small: true,
                                onTap: () => app.autoplayMedia(
                                  s.media!,
                                  videoId: s.nextEpisode!.id,
                                  episodeLabel: s.nextEpisode!.label,
                                  seasonEpisodes: s.episodes,
                                ),
                              ),
                            ],
                          ],
                        ),
                      ),
                    ),
                  ),
                ),

                // Floating glass player sheet (.player-sheet) with smooth enter & exit
                Positioned(
                  right: 20,
                  bottom: 74,
                  child: AnimatedSwitcher(
                    duration: const Duration(milliseconds: 180),
                    reverseDuration: const Duration(milliseconds: 130),
                    switchInCurve: VeyloCurves.out,
                    switchOutCurve: Curves.easeIn,
                    transitionBuilder: (child, anim) => FadeTransition(
                      opacity: anim,
                      child: ScaleTransition(
                        scale: Tween<double>(begin: 0.96, end: 1.0).animate(anim),
                        alignment: Alignment.bottomRight,
                        child: SlideTransition(
                          position: Tween<Offset>(begin: const Offset(0, 0.05), end: Offset.zero).animate(anim),
                          child: child,
                        ),
                      ),
                    ),
                    child: _sheet != null
                        ? ConstrainedBox(
                            key: ValueKey(_sheet),
                            constraints: BoxConstraints(maxWidth: 420, maxHeight: MediaQuery.sizeOf(context).height - 180),
                            child: GlassBox(
                              borderRadius: 24,
                              strong: true,
                              child: Column(
                                mainAxisSize: MainAxisSize.min,
                                children: [
                                  Container(
                                    padding: const EdgeInsets.fromLTRB(20, 12, 14, 12),
                                    decoration: const BoxDecoration(border: Border(bottom: BorderSide(color: VeyloColors.divider))),
                                    child: Row(
                                      children: [
                                        Expanded(child: Text(_sheetTitle(_sheet!), style: VeyloText.groupName)),
                                        VeyloOrb(icon: 'Close', size: 38, onTap: () => setState(() => _sheet = null)),
                                      ],
                                    ),
                                  ),
                                  Flexible(
                                    child: SingleChildScrollView(
                                      padding: const EdgeInsets.all(8),
                                      child: _buildSheetBody(app),
                                    ),
                                  ),
                                ],
                              ),
                            ),
                          )
                        : const SizedBox.shrink(),
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }

  Widget _hudLine(String k, String v) => Padding(
        padding: const EdgeInsets.symmetric(vertical: 1.5),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            SizedBox(width: 74, child: Text(k, style: VeyloText.ui(12, weight: 500, color: VeyloColors.dust))),
            Text(v, style: VeyloText.ui(12, weight: 600, color: VeyloColors.bone, tabular: true)),
          ],
        ),
      );

  String _sheetTitle(_PlayerSheet s) => switch (s) {
        _PlayerSheet.audio => 'Audio',
        _PlayerSheet.subtitles => 'Subtitles',
        _PlayerSheet.sources => 'Sources',
        _PlayerSheet.episodes => 'Episodes',
        _PlayerSheet.speed => 'Speed',
        _PlayerSheet.sleep => 'Sleep timer',
        _PlayerSheet.picture => 'Picture',
        _PlayerSheet.info => 'Details',
        _PlayerSheet.shortcuts => 'Player shortcuts',
      };

  Widget _buildSheetBody(AppState app) {
    final s = widget.session;
    return switch (_sheet!) {
      _PlayerSheet.audio => Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            for (final t in _audioTracks.where((x) => x.id != 'auto' && x.id != 'no'))
              _PlayerOptionRow(
                label: t.title?.isNotEmpty == true ? t.title! : (t.language?.toUpperCase() ?? 'Track ${t.id}'),
                about: t.language ?? '',
                selected: _activeAudio.id == t.id,
                onTap: () {
                  unawaited(_player.setAudioTrack(t));
                  setState(() => _sheet = null);
                },
              ),
            const Padding(
              padding: EdgeInsets.fromLTRB(12, 12, 12, 4),
              child: Text('Sound & Sync', style: VeyloText.micro),
            ),
            _PlayerToggleRow(
              label: 'Night mode (leveling)',
              enabled: _nightMode,
              onToggle: _toggleNightMode,
            ),
            _PlayerStepperRow(
              label: 'Audio delay',
              valueLabel: '${(_audioDelay * 1000).round()} ms',
              onMinus: () => _setAudioDelay(_audioDelay - 0.05),
              onPlus: () => _setAudioDelay(_audioDelay + 0.05),
            ),
          ],
        ),
      _PlayerSheet.subtitles => Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            _PlayerOptionRow(
              label: 'Off',
              selected: _activeSub.id == 'no',
              onTap: () {
                unawaited(_player.setSubtitleTrack(SubtitleTrack.no()));
                setState(() => _sheet = null);
              },
            ),
            for (final t in _subTracks.where((x) => x.id != 'auto' && x.id != 'no'))
              _PlayerOptionRow(
                label: t.title?.isNotEmpty == true ? t.title! : (t.language?.toUpperCase() ?? 'Subtitle ${t.id}'),
                about: t.language ?? 'Embedded',
                selected: _activeSub.id == t.id,
                onTap: () {
                  unawaited(_player.setSubtitleTrack(t));
                  setState(() => _sheet = null);
                },
              ),
            if (_loadingOnlineSubs)
              const Padding(padding: EdgeInsets.all(12), child: Text('Searching online subtitles…', style: VeyloText.credit))
            else if (_onlineSubs.isNotEmpty) ...[
              const Padding(
                padding: EdgeInsets.fromLTRB(12, 12, 12, 6),
                child: Text('Online subtitles', style: VeyloText.micro),
              ),
              Builder(builder: (_) {
                final langs = <String>{
                  for (final sub in _onlineSubs)
                    if ((sub['language']?.toString() ?? '').isNotEmpty) sub['language'].toString(),
                }.toList();
                final filtered = _subLangFilter == null
                    ? _onlineSubs
                    : _onlineSubs.where((sub) => sub['language']?.toString() == _subLangFilter).toList();
                return Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    if (langs.length > 1)
                      Padding(
                        padding: const EdgeInsets.fromLTRB(8, 0, 8, 8),
                        child: Wrap(
                          spacing: 6,
                          runSpacing: 6,
                          children: [
                            _SubFilterChip(
                              label: 'All',
                              active: _subLangFilter == null,
                              onTap: () => setState(() => _subLangFilter = null),
                            ),
                            for (final lang in langs.take(8))
                              _SubFilterChip(
                                label: lang,
                                active: _subLangFilter == lang,
                                onTap: () => setState(() => _subLangFilter = _subLangFilter == lang ? null : lang),
                              ),
                          ],
                        ),
                      ),
                    for (final sub in filtered.take(15))
                      _PlayerOptionRow(
                        label: sub['language']?.toString() ?? 'Subtitle',
                        about: sub['release']?.toString() ?? '',
                        selected: false,
                        onTap: () {
                          final u = sub['url']?.toString() ?? '';
                          if (u.isNotEmpty) {
                            unawaited(_player.setSubtitleTrack(SubtitleTrack.uri(u, title: sub['language']?.toString())));
                            _say('Loaded ${sub['language']} subtitle');
                            setState(() => _sheet = null);
                          }
                        },
                      ),
                  ],
                );
              }),
            ],
            const Padding(
              padding: EdgeInsets.fromLTRB(12, 14, 12, 4),
              child: Text('Timing & Appearance', style: VeyloText.micro),
            ),
            _PlayerStepperRow(
              label: 'Subtitle delay',
              valueLabel: '${(_subDelay * 1000).round()} ms',
              onMinus: () => _setSubDelay(_subDelay - 0.1),
              onPlus: () => _setSubDelay(_subDelay + 0.1),
            ),
            _PlayerStepperRow(
              label: 'Size',
              valueLabel: '${(_subScale * 100).round()}%',
              onMinus: () => _updateSubPrefs(scale: _subScale - 0.1),
              onPlus: () => _updateSubPrefs(scale: _subScale + 0.1),
            ),
            _PlayerStepperRow(
              label: 'Position',
              valueLabel: '$_subPos%',
              minusLabel: 'Higher',
              plusLabel: 'Lower',
              onMinus: () => _updateSubPrefs(pos: _subPos - 2),
              onPlus: () => _updateSubPrefs(pos: _subPos + 2),
            ),
            _PlayerToggleRow(
              label: 'Background box',
              enabled: _subBg,
              onToggle: () => _updateSubPrefs(bg: !_subBg),
            ),
            Container(
              margin: const EdgeInsets.symmetric(vertical: 3),
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
              decoration: BoxDecoration(
                color: const Color.fromRGBO(255, 255, 255, 0.04),
                borderRadius: BorderRadius.circular(16),
              ),
              child: Row(
                children: [
                  Expanded(child: Text('Color', style: VeyloText.ui(14, weight: 600))),
                  for (final (hex, label) in const [
                    ('#F2EFEA', 'Bone'),
                    ('#FFFFFF', 'White'),
                    ('#F5C518', 'Yellow'),
                    ('#7DD3FC', 'Cyan'),
                  ])
                    Padding(
                      padding: const EdgeInsets.only(left: 6),
                      child: VeyloTooltip(
                        message: label,
                        child: MouseRegion(
                          cursor: SystemMouseCursors.click,
                          child: GestureDetector(
                            onTap: () => _updateSubPrefs(color: hex),
                            child: AnimatedContainer(
                              duration: const Duration(milliseconds: 140),
                              width: 24,
                              height: 24,
                              decoration: BoxDecoration(
                                shape: BoxShape.circle,
                                color: VeyloColors.parseHex(hex),
                                border: Border.all(
                                  color: _subColor.toUpperCase() == hex
                                      ? VeyloColors.bone
                                      : const Color.fromRGBO(255, 255, 255, 0.2),
                                  width: _subColor.toUpperCase() == hex ? 2.5 : 1,
                                ),
                              ),
                            ),
                          ),
                        ),
                      ),
                    ),
                ],
              ),
            ),
          ],
        ),
      _PlayerSheet.picture => Column(
          children: [
            _PlayerStepperRow(
              label: 'Brightness',
              valueLabel: '$_brightness',
              onMinus: () => _setPicture(brightness: _brightness - 5),
              onPlus: () => _setPicture(brightness: _brightness + 5),
            ),
            _PlayerStepperRow(
              label: 'Contrast',
              valueLabel: '$_contrast',
              onMinus: () => _setPicture(contrast: _contrast - 5),
              onPlus: () => _setPicture(contrast: _contrast + 5),
            ),
            _PlayerStepperRow(
              label: 'Saturation',
              valueLabel: '$_saturation',
              onMinus: () => _setPicture(saturation: _saturation - 5),
              onPlus: () => _setPicture(saturation: _saturation + 5),
            ),
            _PlayerStepperRow(
              label: 'Gamma',
              valueLabel: '$_gamma',
              onMinus: () => _setPicture(gamma: _gamma - 5),
              onPlus: () => _setPicture(gamma: _gamma + 5),
            ),
            const SizedBox(height: 8),
            Align(
              alignment: Alignment.centerRight,
              child: VeyloPill(
                label: 'Reset picture',
                icon: 'Undo',
                small: true,
                disabled: _brightness == 0 && _contrast == 0 && _saturation == 0 && _gamma == 0,
                onTap: () {
                  _setPicture(brightness: 0, contrast: 0, saturation: 0, gamma: 0);
                  _say('Picture reset');
                },
              ),
            ),
          ],
        ),
      _PlayerSheet.speed => Column(
          children: [
            for (final r in const [0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0])
              _PlayerOptionRow(
                label: r == 1.0 ? 'Normal (1×)' : '$r×',
                selected: (_speed - r).abs() < 0.02,
                onTap: () {
                  unawaited(_player.setRate(r));
                  setState(() => _sheet = null);
                },
              ),
          ],
        ),
      _PlayerSheet.sleep => Column(
          children: [
            _PlayerOptionRow(
              label: 'Off',
              selected: _sleepUntil == null && !_sleepEndOfItem,
              onTap: () => _setSleepMinutes(null),
            ),
            for (final mins in const [15, 30, 45, 60, 90])
              _PlayerOptionRow(
                label: '$mins minutes',
                selected: false,
                onTap: () => _setSleepMinutes(mins),
              ),
            _PlayerOptionRow(
              label: 'End of current video',
              selected: _sleepEndOfItem,
              onTap: () => _setSleepMinutes(null, endOfItem: true),
            ),
          ],
        ),
      _PlayerSheet.sources => Column(
          children: [
            for (final src in s.sources)
              _PlayerOptionRow(
                label: src.filename.isNotEmpty ? src.filename : src.name,
                about: [if (src.quality != null) src.quality!, if (src.hdr != null) src.hdr!, if (src.size != null) src.size!, src.addon].join(' · '),
                selected: src.url == s.url,
                onTap: () {
                  setState(() => _sheet = null);
                  if (src.url == s.url) return;
                  app.startPlaySession(PlaySession(
                    title: s.title,
                    subtitle: s.subtitle,
                    url: src.url,
                    headers: src.headers,
                    startMs: _position.inMilliseconds,
                    size: src.size,
                    hdr: src.hdr,
                    quality: src.quality,
                    filename: src.filename.isNotEmpty ? src.filename : null,
                    fileSize: src.sizeBytes,
                    media: s.media,
                    videoId: s.videoId,
                    sources: s.sources,
                    episodes: s.episodes,
                    nextEpisode: s.nextEpisode,
                  ));
                },
              ),
          ],
        ),
      _PlayerSheet.episodes => Column(
          children: [
            for (final ep in s.episodes)
              _PlayerOptionRow(
                label: '${ep.label} · ${ep.title}',
                selected: ep.id == s.videoId,
                onTap: () {
                  setState(() => _sheet = null);
                  if (ep.id == s.videoId) return;
                  if (s.media != null) {
                    unawaited(app.autoplayMedia(s.media!, videoId: ep.id, episodeLabel: ep.label, seasonEpisodes: s.episodes));
                  }
                },
              ),
          ],
        ),
      _PlayerSheet.info => Padding(
          padding: const EdgeInsets.all(12),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              _infoLine('Title', s.title),
              if (s.quality != null) _infoLine('Quality', s.quality!),
              if (s.hdr != null) _infoLine('Picture', s.hdr!),
              if (s.size != null) _infoLine('Size', s.size!),
              _infoLine('Backend', 'libmpv (media_kit)'),
              const SizedBox(height: 12),
              VeyloPill(
                label: _showStatsHud ? 'Hide live stats HUD' : 'Show live stats HUD',
                icon: 'Gauge',
                active: _showStatsHud,
                small: true,
                onTap: () => setState(() => _showStatsHud = !_showStatsHud),
              ),
            ],
          ),
        ),
      _PlayerSheet.shortcuts => Padding(
          padding: const EdgeInsets.all(10),
          child: Column(
            children: [
              for (final (k, desc) in const [
                ('Space / K', 'Play or pause'),
                ('← / →  (J / L)', 'Seek back / forward 10s'),
                ('↑ / ↓', 'Volume up / down'),
                ('M', 'Mute / unmute'),
                ('F / Double-click', 'Toggle fullscreen'),
                ('[ / ]', 'Decrease / increase speed'),
                ('Z', 'Toggle fit / fill crop'),
                ('T', 'Sleep timer'),
                ('I', 'Toggle live stats HUD'),
                ('H / ?', 'Toggle shortcuts sheet'),
                ('Esc', 'Close sheet, leave fullscreen, or exit'),
              ])
                Padding(
                  padding: const EdgeInsets.symmetric(vertical: 5, horizontal: 6),
                  child: Row(
                    children: [
                      Keycap(k, small: true),
                      const SizedBox(width: 12),
                      Expanded(child: Text(desc, style: VeyloText.ui(13.5, weight: 500))),
                    ],
                  ),
                ),
            ],
          ),
        ),
    };
  }

  Widget _infoLine(String k, String v) => Padding(
        padding: const EdgeInsets.symmetric(vertical: 5),
        child: Row(
          children: [
            SizedBox(width: 96, child: Text(k, style: VeyloText.ui(14, weight: 500, color: VeyloColors.dust))),
            Expanded(child: Text(v, style: VeyloText.ui(14.5, weight: 600))),
          ],
        ),
      );
}

class _KickIconButton extends StatelessWidget {
  const _KickIconButton({required this.kick, required this.direction, required this.child});
  final int kick;
  final int direction;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    return TweenAnimationBuilder<double>(
      key: ValueKey(kick),
      tween: Tween(begin: kick == 0 ? 0.0 : 1.0, end: 0.0),
      duration: const Duration(milliseconds: 260),
      curve: VeyloCurves.out,
      builder: (_, v, c) => Transform.rotate(
        angle: direction * v * 0.34,
        child: c,
      ),
      child: child,
    );
  }
}

class _VolumeSlider extends StatefulWidget {
  const _VolumeSlider({required this.value, required this.lamp, required this.onChanged});
  final double value; // 0..130
  final Color lamp;
  final ValueChanged<double> onChanged;

  @override
  State<_VolumeSlider> createState() => _VolumeSliderState();
}

class _VolumeSliderState extends State<_VolumeSlider> {
  bool _hovered = false;

  void _updateFromLocal(BuildContext ctx, Offset local) {
    final box = ctx.findRenderObject() as RenderBox?;
    if (box == null || box.size.width <= 0) return;
    final pct = (local.dx / box.size.width).clamp(0.0, 1.0);
    widget.onChanged((pct * 130.0).roundToDouble());
  }

  @override
  Widget build(BuildContext context) {
    final frac = (widget.value / 130.0).clamp(0.0, 1.0);
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTapDown: (e) => _updateFromLocal(context, e.localPosition),
        onHorizontalDragUpdate: (e) => _updateFromLocal(context, e.localPosition),
        child: SizedBox(
          width: 78,
          height: 36,
          child: Center(
            child: Stack(
              clipBehavior: Clip.none,
              alignment: Alignment.centerLeft,
              children: [
                Container(
                  width: 78,
                  height: _hovered ? 4.5 : 3.0,
                  decoration: BoxDecoration(
                    color: const Color.fromRGBO(255, 255, 255, 0.22),
                    borderRadius: BorderRadius.circular(3),
                  ),
                ),
                Container(
                  width: 78 * frac,
                  height: _hovered ? 4.5 : 3.0,
                  decoration: BoxDecoration(
                    color: widget.value > 100 ? VeyloColors.amber : widget.lamp,
                    borderRadius: BorderRadius.circular(3),
                  ),
                ),
                Positioned(
                  left: (78 * frac - 5.5).clamp(0.0, 67.0),
                  child: AnimatedScale(
                    scale: _hovered ? 1.2 : 1.0,
                    duration: const Duration(milliseconds: 120),
                    child: Container(
                      width: 11,
                      height: 11,
                      decoration: const BoxDecoration(
                        shape: BoxShape.circle,
                        color: VeyloColors.bone,
                        boxShadow: [BoxShadow(color: Color.fromRGBO(0, 0, 0, 0.55), blurRadius: 4)],
                      ),
                    ),
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

class _PlayerStepperRow extends StatelessWidget {
  const _PlayerStepperRow({
    required this.label,
    required this.valueLabel,
    required this.onMinus,
    required this.onPlus,
    this.minusLabel = '−',
    this.plusLabel = '+',
  });

  final String label;
  final String valueLabel;
  final VoidCallback onMinus;
  final VoidCallback onPlus;
  final String minusLabel;
  final String plusLabel;

  @override
  Widget build(BuildContext context) {
    return Container(
      margin: const EdgeInsets.symmetric(vertical: 3),
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
      decoration: BoxDecoration(
        color: const Color.fromRGBO(255, 255, 255, 0.04),
        borderRadius: BorderRadius.circular(16),
      ),
      child: Row(
        children: [
          Expanded(child: Text(label, style: VeyloText.ui(14, weight: 600))),
          _StepBtn(label: minusLabel, onTap: onMinus),
          SizedBox(
            width: 58,
            child: Text(
              valueLabel,
              textAlign: TextAlign.center,
              style: VeyloText.ui(13, weight: 650, tabular: true),
            ),
          ),
          _StepBtn(label: plusLabel, onTap: onPlus),
        ],
      ),
    );
  }
}

class _PlayerToggleRow extends StatelessWidget {
  const _PlayerToggleRow({required this.label, required this.enabled, required this.onToggle});
  final String label;
  final bool enabled;
  final VoidCallback onToggle;

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    return ValueListenableBuilder<Color>(
      valueListenable: app.lampColorNotifier,
      builder: (_, lamp, _) => Container(
        margin: const EdgeInsets.symmetric(vertical: 3),
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
        decoration: BoxDecoration(
          color: const Color.fromRGBO(255, 255, 255, 0.04),
          borderRadius: BorderRadius.circular(16),
        ),
        child: Row(
          children: [
            Expanded(child: Text(label, style: VeyloText.ui(14, weight: 600))),
            _StepBtn(label: enabled ? 'On' : 'Off', pressed: enabled, lamp: lamp, onTap: onToggle),
          ],
        ),
      ),
    );
  }
}

class _StepBtn extends StatefulWidget {
  const _StepBtn({required this.label, required this.onTap, this.pressed = false, this.lamp});
  final String label;
  final VoidCallback onTap;
  final bool pressed;
  final Color? lamp;

  @override
  State<_StepBtn> createState() => _StepBtnState();
}

class _StepBtnState extends State<_StepBtn> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final bg = widget.pressed
        ? (widget.lamp ?? VeyloColors.defaultLamp)
        : (_hovered ? const Color.fromRGBO(255, 255, 255, 0.16) : const Color.fromRGBO(255, 255, 255, 0.09));
    final fg = widget.pressed ? const Color(0xFF000000) : VeyloColors.bone;
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      child: GestureDetector(
        onTap: widget.onTap,
        child: AnimatedContainer(
          duration: const Duration(milliseconds: 140),
          constraints: const BoxConstraints(minWidth: 36),
          height: 34,
          padding: const EdgeInsets.symmetric(horizontal: 10),
          alignment: Alignment.center,
          decoration: BoxDecoration(color: bg, borderRadius: BorderRadius.circular(10)),
          child: Text(widget.label, style: VeyloText.ui(13, weight: 650, color: fg, height: 1.0)),
        ),
      ),
    );
  }
}

class _PlayerOptionRow extends StatefulWidget {
  const _PlayerOptionRow({required this.label, this.about = '', required this.selected, required this.onTap});
  final String label;
  final String about;
  final bool selected;
  final VoidCallback onTap;

  @override
  State<_PlayerOptionRow> createState() => _PlayerOptionRowState();
}

class _PlayerOptionRowState extends State<_PlayerOptionRow> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    return ValueListenableBuilder<Color>(
      valueListenable: app.lampColorNotifier,
      builder: (_, lamp, _) => MouseRegion(
        cursor: SystemMouseCursors.click,
        onEnter: (_) => setState(() => _hovered = true),
        onExit: (_) => setState(() => _hovered = false),
        child: GestureDetector(
          onTap: widget.onTap,
          child: Container(
            padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 10),
            decoration: BoxDecoration(
              color: widget.selected
                  ? lamp.withValues(alpha: 0.16)
                  : (_hovered ? const Color.fromRGBO(255, 255, 255, 0.07) : const Color(0x00000000)),
              borderRadius: BorderRadius.circular(16),
            ),
            child: Row(
              children: [
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(widget.label, maxLines: 1, overflow: TextOverflow.ellipsis, style: VeyloText.ui(15, weight: 600)),
                      if (widget.about.isNotEmpty)
                        Text(widget.about, maxLines: 1, overflow: TextOverflow.ellipsis, style: VeyloText.ui(13, weight: 400, color: VeyloColors.dust)),
                    ],
                  ),
                ),
                if (widget.selected) VeyloIcon('Check', size: 18, color: lamp),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

class _CenterPlayButton extends StatefulWidget {
  const _CenterPlayButton({required this.playing, required this.onTap});
  final bool playing;
  final VoidCallback onTap;

  @override
  State<_CenterPlayButton> createState() => _CenterPlayButtonState();
}

class _CenterPlayButtonState extends State<_CenterPlayButton> {
  bool _hovered = false;
  bool _pressed = false;

  @override
  Widget build(BuildContext context) {
    return VeyloTooltip(
      message: widget.playing ? 'Pause (Space)' : 'Play (Space)',
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        onEnter: (_) => setState(() => _hovered = true),
        onExit: (_) => setState(() {
          _hovered = false;
          _pressed = false;
        }),
        child: GestureDetector(
          onTapDown: (_) => setState(() => _pressed = true),
          onTapUp: (_) => setState(() => _pressed = false),
          onTapCancel: () => setState(() => _pressed = false),
          onTap: widget.onTap,
          child: AnimatedScale(
            scale: _pressed ? 0.91 : (_hovered ? 1.06 : 1.0),
            duration: Duration(milliseconds: _pressed ? 70 : 240),
            curve: _pressed ? Curves.easeOut : VeyloCurves.spring,
            child: AnimatedContainer(
              duration: const Duration(milliseconds: 160),
              width: 84,
              height: 84,
              decoration: BoxDecoration(
                shape: BoxShape.circle,
                color: _hovered
                    ? const Color.fromRGBO(18, 19, 24, 0.58)
                    : const Color.fromRGBO(10, 11, 15, 0.42),
                border: Border.all(color: _hovered ? VeyloColors.sheen : VeyloColors.edge),
              ),
              alignment: Alignment.center,
              child: AnimatedSwitcher(
                duration: const Duration(milliseconds: 200),
                transitionBuilder: (child, anim) => RotationTransition(
                  turns: Tween(begin: 0.92, end: 1.0).animate(CurvedAnimation(parent: anim, curve: VeyloCurves.out)),
                  child: ScaleTransition(
                    scale: Tween(begin: 0.84, end: 1.0).animate(anim),
                    child: FadeTransition(opacity: anim, child: child),
                  ),
                ),
                child: VeyloIcon(
                  widget.playing ? 'Pause' : 'Play',
                  key: ValueKey(widget.playing),
                  size: 44,
                  color: VeyloColors.bone,
                  filled: true,
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

class _SubFilterChip extends StatefulWidget {
  const _SubFilterChip({required this.label, required this.active, required this.onTap});
  final String label;
  final bool active;
  final VoidCallback onTap;

  @override
  State<_SubFilterChip> createState() => _SubFilterChipState();
}

class _SubFilterChipState extends State<_SubFilterChip> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    return ValueListenableBuilder<Color>(
      valueListenable: app.lampColorNotifier,
      builder: (_, lamp, _) => MouseRegion(
        cursor: SystemMouseCursors.click,
        onEnter: (_) => setState(() => _hovered = true),
        onExit: (_) => setState(() => _hovered = false),
        child: GestureDetector(
          onTap: widget.onTap,
          child: AnimatedContainer(
            duration: const Duration(milliseconds: 140),
            padding: const EdgeInsets.symmetric(horizontal: 11, vertical: 5),
            decoration: BoxDecoration(
              color: widget.active
                  ? lamp.withValues(alpha: 0.28)
                  : (_hovered ? const Color.fromRGBO(255, 255, 255, 0.12) : const Color.fromRGBO(255, 255, 255, 0.07)),
              borderRadius: BorderRadius.circular(999),
            ),
            child: Text(widget.label, style: VeyloText.ui(12.5, weight: 600, color: VeyloColors.bone)),
          ),
        ),
      ),
    );
  }
}

class _ScrubberBar extends StatefulWidget {
  const _ScrubberBar({
    required this.played,
    required this.buffered,
    required this.durationSec,
    this.segments = const [],
    required this.lamp,
    required this.formatClock,
    required this.onScrub,
    required this.onCommit,
  });

  final double played;
  final double buffered;
  final double durationSec;
  final List<Map<String, dynamic>> segments;
  final Color lamp;
  final String Function(double) formatClock;
  final ValueChanged<double> onScrub;
  final ValueChanged<double> onCommit;

  @override
  State<_ScrubberBar> createState() => _ScrubberBarState();
}

class _ScrubberBarState extends State<_ScrubberBar> {
  final ValueNotifier<double?> _hoverPct = ValueNotifier<double?>(null);
  bool _hovered = false;
  bool _dragging = false;

  @override
  void dispose() {
    _hoverPct.dispose();
    super.dispose();
  }

  double _pctAt(BuildContext ctx, Offset local) {
    final box = ctx.findRenderObject() as RenderBox?;
    if (box == null || box.size.width <= 0) return 0;
    return (local.dx / box.size.width).clamp(0.0, 1.0);
  }

  @override
  Widget build(BuildContext context) {
    final active = _hovered || _dragging;
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) {
        _hoverPct.value = null;
        setState(() => _hovered = false);
      },
      onHover: (e) => _hoverPct.value = _pctAt(context, e.localPosition),
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onHorizontalDragStart: (e) {
          setState(() => _dragging = true);
          widget.onScrub(_pctAt(context, e.localPosition));
        },
        onHorizontalDragUpdate: (e) {
          final pct = _pctAt(context, e.localPosition);
          _hoverPct.value = pct;
          widget.onScrub(pct);
        },
        onHorizontalDragEnd: (_) {
          setState(() => _dragging = false);
          widget.onCommit(widget.played);
        },
        onTapUp: (e) => widget.onCommit(_pctAt(context, e.localPosition)),
        child: SizedBox(
          height: 36,
          child: LayoutBuilder(
            builder: (ctx, c) {
              final w = c.maxWidth;
              return TweenAnimationBuilder<double>(
                tween: Tween<double>(end: active ? 5.5 : 3.0),
                duration: const Duration(milliseconds: 120),
                curve: VeyloCurves.out,
                builder: (_, trackH, _) => ValueListenableBuilder<double?>(
                  valueListenable: _hoverPct,
                  builder: (_, hoverPct, _) => Stack(
                    clipBehavior: Clip.none,
                    alignment: Alignment.centerLeft,
                    children: [
                      Positioned.fill(
                        child: CustomPaint(
                          painter: _ScrubberTrackPainter(
                            played: widget.played,
                            buffered: widget.buffered,
                            hoverPct: hoverPct,
                            durationSec: widget.durationSec,
                            segments: widget.segments,
                            trackHeight: trackH,
                            lamp: widget.lamp,
                          ),
                        ),
                      ),
                      Positioned(
                        left: (w * widget.played - 7).clamp(0.0, (w - 14).clamp(0.0, double.infinity)),
                        child: AnimatedScale(
                          scale: _dragging ? 1.35 : (_hovered ? 1.2 : 0.85),
                          duration: const Duration(milliseconds: 150),
                          curve: VeyloCurves.spring,
                          child: Container(
                            width: 14,
                            height: 14,
                            decoration: BoxDecoration(
                              shape: BoxShape.circle,
                              color: VeyloColors.bone,
                              boxShadow: [
                                const BoxShadow(color: Color.fromRGBO(0, 0, 0, 0.6), blurRadius: 6),
                                if (active) BoxShadow(color: widget.lamp.withValues(alpha: 0.45), blurRadius: 10, spreadRadius: 2),
                              ],
                            ),
                          ),
                        ),
                      ),
                      if (hoverPct != null && widget.durationSec > 0)
                        Positioned(
                          left: (w * hoverPct - 28).clamp(0.0, (w - 56).clamp(0.0, double.infinity)),
                          bottom: 30,
                          child: Container(
                            padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 4),
                            decoration: BoxDecoration(
                              color: const Color.fromRGBO(12, 12, 15, 0.90),
                              borderRadius: BorderRadius.circular(10),
                              border: Border.all(color: const Color.fromRGBO(242, 239, 234, 0.18)),
                            ),
                            child: Text(
                              widget.formatClock(hoverPct * widget.durationSec),
                              style: VeyloText.ui(12, weight: 600, color: VeyloColors.bone, tabular: true),
                            ),
                          ),
                        ),
                    ],
                  ),
                ),
              );
            },
          ),
        ),
      ),
    );
  }
}

class _ScrubberTrackPainter extends CustomPainter {
  const _ScrubberTrackPainter({
    required this.played,
    required this.buffered,
    required this.hoverPct,
    required this.durationSec,
    required this.segments,
    required this.trackHeight,
    required this.lamp,
  });

  final double played;
  final double buffered;
  final double? hoverPct;
  final double durationSec;
  final List<Map<String, dynamic>> segments;
  final double trackHeight;
  final Color lamp;

  @override
  void paint(Canvas canvas, Size size) {
    final cy = size.height / 2;
    final top = cy - trackHeight / 2;
    final r = Radius.circular(trackHeight / 2);

    canvas.drawRRect(
      RRect.fromRectAndRadius(Rect.fromLTWH(0, top, size.width, trackHeight), r),
      Paint()..color = const Color.fromRGBO(255, 255, 255, 0.18),
    );
    if (buffered > 0) {
      canvas.drawRRect(
        RRect.fromRectAndRadius(Rect.fromLTWH(0, top, size.width * buffered.clamp(0.0, 1.0), trackHeight), r),
        Paint()..color = const Color.fromRGBO(242, 239, 234, 0.36),
      );
    }
    if (hoverPct != null && hoverPct! > played) {
      final x1 = size.width * played.clamp(0.0, 1.0);
      final x2 = size.width * hoverPct!.clamp(0.0, 1.0);
      canvas.drawRRect(
        RRect.fromRectAndRadius(Rect.fromLTWH(x1, top, (x2 - x1).clamp(0.0, size.width), trackHeight), r),
        Paint()..color = const Color.fromRGBO(242, 239, 234, 0.22),
      );
    }
    if (played > 0) {
      canvas.drawRRect(
        RRect.fromRectAndRadius(Rect.fromLTWH(0, top, size.width * played.clamp(0.0, 1.0), trackHeight), r),
        Paint()..color = lamp,
      );
    }
    if (durationSec > 0 && segments.isNotEmpty) {
      final segPaint = Paint()..color = const Color.fromRGBO(242, 239, 234, 0.58);
      for (final seg in segments) {
        final st = (((seg['start'] as num?)?.toDouble() ?? 0) / durationSec).clamp(0.0, 1.0);
        final en = (((seg['end'] as num?)?.toDouble() ?? 0) / durationSec).clamp(0.0, 1.0);
        final left = st * size.width;
        final segW = ((en - st) * size.width).clamp(2.0, size.width);
        canvas.drawRRect(RRect.fromRectAndRadius(Rect.fromLTWH(left, top, segW, trackHeight), r), segPaint);
      }
    }
  }

  @override
  bool shouldRepaint(_ScrubberTrackPainter old) =>
      old.played != played ||
      old.buffered != buffered ||
      old.hoverPct != hoverPct ||
      old.trackHeight != trackHeight ||
      old.lamp != lamp ||
      old.segments != segments;
}

class _Spinner extends StatefulWidget {
  const _Spinner({required this.color});
  final Color color;

  @override
  State<_Spinner> createState() => _SpinnerState();
}

class _SpinnerState extends State<_Spinner> with SingleTickerProviderStateMixin {
  late final AnimationController _ctrl = AnimationController(vsync: this, duration: const Duration(milliseconds: 900))..repeat();

  @override
  void dispose() {
    _ctrl.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return RotationTransition(
      turns: _ctrl,
      child: CustomPaint(painter: _SpinnerPainter(widget.color)),
    );
  }
}

class _SpinnerPainter extends CustomPainter {
  _SpinnerPainter(this.color);
  final Color color;

  @override
  void paint(Canvas canvas, Size size) {
    final rect = Offset.zero & size;
    final bg = Paint()
      ..color = const Color.fromRGBO(255, 255, 255, 0.18)
      ..style = PaintingStyle.stroke
      ..strokeWidth = 4;
    final fg = Paint()
      ..color = color
      ..style = PaintingStyle.stroke
      ..strokeWidth = 4
      ..strokeCap = StrokeCap.round;
    canvas.drawArc(rect.deflate(2), 0, 6.283, false, bg);
    canvas.drawArc(rect.deflate(2), -1.57, 1.7, false, fg);
  }

  @override
  bool shouldRepaint(_SpinnerPainter old) => old.color != color;
}
