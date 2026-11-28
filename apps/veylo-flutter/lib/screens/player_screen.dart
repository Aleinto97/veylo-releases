import 'dart:async';
import 'dart:convert';
import 'dart:io' show Platform;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:material_ui/material_ui.dart' show CircularProgressIndicator, InputDecoration, TextField;
import 'package:media_kit/media_kit.dart';
import 'package:media_kit_video/media_kit_video.dart';
import '../bridge/veylo_bridge.dart';
import '../player/mpv_profile.dart';
import '../state/app_state.dart';
import '../theme/veylo_theme.dart';
import '../widgets/finding_runner.dart';
import '../widgets/glass.dart';
import '../widgets/veylo_icons.dart';

enum _PlayerSheet { audio, subtitles, sources, episodes, clips, chapters, speed, sleep, picture, info, shortcuts }

typedef _Chapter = ({double start, String title});

int _chapterIndexAt(List<_Chapter> chapters, double seconds) {
  for (var i = chapters.length - 1; i >= 0; i--) {
    if (seconds >= chapters[i].start) return i;
  }
  return -1;
}

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
  SourceItem? _source;
  String get _sourceUrl => _source?.url ?? widget.session.url;
  String? get _sourceQuality => _source?.quality ?? (_source == null ? widget.session.quality : null);
  String? get _sourceHdr => _source?.hdr ?? (_source == null ? widget.session.hdr : null);
  String? get _sourceSize => _source?.size ?? (_source == null ? widget.session.size : null);
  int? get _sourceBytes => _source?.sizeBytes ?? (_source == null ? widget.session.fileSize : null);

  String? _defaultUserAgent;
  String? _reconnectOptions;
  int _sourceGeneration = 0;
  bool _monitorSource = false;
  bool _firstFrame = false;
  Timer? _firstFrameTimer;
  Timer? _stallTimer;
  Duration _sourcePosition = Duration.zero;
  Duration _resumePosition = Duration.zero;
  final _sourceClock = Stopwatch()..start();
  final List<int> _decodeFailures = [];
  final Set<String> _failedSources = {};
  int _automaticSwitches = 0;
  AudioTrack? _carriedAudio;
  SubtitleTrack? _carriedSubtitle;
  bool _restoredSubtitle = false;

  List<AudioTrack> _audioTracks = [];
  AudioTrack _activeAudio = AudioTrack.auto();
  String _preferredAudioLang = 'english';
  bool _preferredAudioApplied = false;
  String? _subtitleFallbackLanguage;
  Timer? _subtitleFallbackTimer;
  bool _manualTrackSelected = false;
  List<SubtitleTrack> _subTracks = [];
  SubtitleTrack _activeSub = SubtitleTrack.auto();
  List<Map<String, dynamic>> _onlineSubs = [];
  Future<void>? _onlineSubsRequest;
  Object? _subtitleDownload;
  final _subSearch = TextEditingController();
  final _subSearchFocus = FocusNode();
  bool _externalAudioAttached = false;
  List<Map<String, dynamic>> _segments = [];
  List<Map<String, dynamic>> _onlineSegments = [];
  bool _segmentsRequested = false;
  bool _chaptersReady = false;
  bool _autoSkipIntro = true;
  final Set<(String, int)> _skippedSegments = {};
  final List<double> _backwardSeekTargets = [];
  Map<String, dynamic>? _undoSegment;
  Timer? _undoTimer;
  List<_Chapter> _chapters = const [];
  bool _chaptersRequested = false;
  Timer? _chapterRetry;

  bool _chromeVisible = true;
  bool _showStatsHud = false;
  Timer? _hideTimer;
  _PlayerSheet? _sheet;
  double? _scrubValue;
  String? _osdText;
  Timer? _osdTimer;
  Timer? _progressTimer;
  int _watchedAccumMs = 0;
  int _lastRecordedMs = 0;
  bool _sentProgress = false;
  Duration _recordPosition = Duration.zero;
  Duration _recordDuration = Duration.zero;
  bool _seekPending = false;
  Future<void>? _progressWrite;
  Future<void>? _finalProgress;
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
  bool _autoNext = true;
  bool _upNextDismissed = false;

  // Center button kick animation counters
  int _rewKick = 0;
  int _fwdKick = 0;

  final List<StreamSubscription<dynamic>> _subs = [];

  @override
  void initState() {
    super.initState();
    if (Platform.isAndroid) {
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) {
          unawaited(SystemChrome.setEnabledSystemUIMode(SystemUiMode.immersiveSticky));
        }
      });
    }
    for (final source in widget.session.sources) {
      if (source.url == widget.session.url) {
        _source = source;
        break;
      }
    }
    _resumePosition = Duration(milliseconds: widget.session.startMs > 5000 ? widget.session.startMs : 0);
    _onlineSubs = List<Map<String, dynamic>>.from(widget.session.initialSubtitles);
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
      if (!mounted || !_monitorSource) return;
      if (p) _attachExternalAudio();
      if (_playing != p) setState(() => _playing = p);
      _updateStallWatchdog();
    }));
    _subs.add(_player.stream.position.listen(_onPosition));
    _subs.add(_player.stream.buffering.listen((_) => _updateStallWatchdog()));
    _subs.add(_player.stream.duration.listen((d) {
      if (!mounted || !_monitorSource) return;
      _durN.value = d;
      if (d <= Duration.zero) return;
      _attachExternalAudio();
      if (!_chaptersRequested) {
        _chaptersRequested = true;
        unawaited(_readChapters(_sourceGeneration));
      } else if (_chaptersReady) {
        _mergeSegments();
      }
      final videoId = widget.session.videoId ?? widget.session.media?.id;
      if (!_segmentsRequested && videoId != null) {
        _segmentsRequested = true;
        unawaited(_fetchSegments(videoId, d.inMilliseconds / 1000, _sourceGeneration));
      }
    }));
    _subs.add(_player.stream.completed.listen((done) {
      if (!done || !mounted || !_monitorSource || !_firstFrame) return;
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
      } else if (_autoNext && s.clips.length > 1 && s.clipIndex + 1 < s.clips.length) {
        final app = AppStateScope.read(context);
        unawaited(app.playTrailer(
          title: s.clipContextTitle ?? s.title,
          clips: s.clips,
          startIndex: s.clipIndex + 1,
        ));
      }
    }));
    _subs.add(_player.stream.buffer.listen((b) {
      if (mounted && _monitorSource) _bufN.value = b;
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
      if (mounted && _monitorSource) {
        setState(() {
          _audioTracks = t.audio;
          _subTracks = t.subtitle;
        });
        _ensurePreferredAudioTrack(t.audio);
        _restoreSubtitle(t.subtitle);
        // media_kit includes the synthetic Auto and Off tracks in every update.
        if (_subtitleFallbackTimer == null &&
            !_manualTrackSelected &&
            _carriedSubtitle == null &&
            t.audio.any((track) => _isMediaTrack(track.id))) {
          final generation = _sourceGeneration;
          _subtitleFallbackTimer = Timer(const Duration(milliseconds: 800), () {
            unawaited(_applySubtitleFallback(generation));
          });
        }
      }
    }));
    _subs.add(_player.stream.track.listen((t) {
      if (mounted && _monitorSource) {
        setState(() {
          _activeAudio = t.audio;
          _activeSub = t.subtitle;
        });
        if (!_preferredAudioApplied && _audioTracks.isNotEmpty) {
          _ensurePreferredAudioTrack(_audioTracks);
        }
      }
    }));
    _subs.add(_player.stream.error.listen((error) {
      if (!mounted || !_monitorSource || error.isEmpty || _isDecodeFailure(error)) return;
      if (isFatalPlayerError(error)) _failSource('fatal playback error');
    }));
    _subs.add(_player.stream.log.listen((log) {
      if (!mounted || !_monitorSource || !_isDecodeFailure(log.text)) return;
      final now = _sourceClock.elapsedMilliseconds;
      _decodeFailures.removeWhere((at) => now - at > 10000);
      _decodeFailures.add(now);
      if (_decodeFailures.length >= 10) _failSource('repeated decode errors');
    }));

    _pokeChrome();
    unawaited(_configureAndOpen());
    _progressTimer = Timer.periodic(const Duration(seconds: 15), (_) => _checkpointProgress());
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      _appRef = AppStateScope.read(context);
      _appRef!.onBeforeClosePlayer = _finishProgress;
      _focus.requestFocus();
    });
  }

  static List<String> _langCodeList(String? lang) => switch (lang?.toLowerCase().trim()) {
        'english' || 'en' || 'eng' => const ['en', 'eng', 'english'],
        'italian' || 'it' || 'ita' => const ['it', 'ita', 'italian', 'italiano'],
        'spanish' || 'es' || 'spa' => const ['es', 'spa', 'spanish', 'castellano'],
        'latino' => const ['es-419', 'es-mx', 'es', 'spa', 'latino'],
        'french' || 'fr' || 'fre' || 'fra' => const ['fr', 'fre', 'fra', 'french', 'français', 'francais'],
        'german' || 'de' || 'ger' || 'deu' => const ['de', 'ger', 'deu', 'german', 'deutsch'],
        'portuguese' || 'pt' || 'por' => const ['pt', 'por', 'pt-br', 'pob', 'portuguese'],
        'dutch' || 'nl' || 'dut' || 'nld' => const ['nl', 'dut', 'nld', 'dutch'],
        'polish' || 'pl' || 'pol' => const ['pl', 'pol', 'polish'],
        'russian' || 'ru' || 'rus' => const ['ru', 'rus', 'russian'],
        'ukrainian' || 'uk' || 'ukr' => const ['uk', 'ukr', 'ukrainian'],
        'turkish' || 'tr' || 'tur' => const ['tr', 'tur', 'turkish'],
        'greek' || 'el' || 'gre' || 'ell' => const ['el', 'gre', 'ell', 'greek'],
        'swedish' || 'sv' || 'swe' => const ['sv', 'swe', 'swedish'],
        'danish' || 'da' || 'dan' => const ['da', 'dan', 'danish'],
        'norwegian' || 'no' || 'nor' => const ['no', 'nor', 'nob', 'nb', 'norwegian'],
        'finnish' || 'fi' || 'fin' => const ['fi', 'fin', 'finnish'],
        'czech' || 'cs' || 'cze' || 'ces' => const ['cs', 'cze', 'ces', 'czech'],
        'hungarian' || 'hu' || 'hun' => const ['hu', 'hun', 'hungarian'],
        'romanian' || 'ro' || 'rum' || 'ron' => const ['ro', 'rum', 'ron', 'romanian'],
        'arabic' || 'ar' || 'ara' => const ['ar', 'ara', 'arabic'],
        'hebrew' || 'he' || 'heb' => const ['he', 'heb', 'hebrew'],
        'hindi' || 'hi' || 'hin' => const ['hi', 'hin', 'hindi'],
        'japanese' || 'ja' || 'jpn' => const ['ja', 'jpn', 'japanese'],
        'korean' || 'ko' || 'kor' => const ['ko', 'kor', 'korean'],
        'chinese' || 'zh' || 'chi' || 'zho' => const ['zh', 'chi', 'zho', 'chinese', 'mandarin'],
        _ => const [],
      };

  static bool _isMediaTrack(String id) => id != 'auto' && id != 'no';

  static bool _isExtraAudioTrack(AudioTrack t) {
    final hay = (t.title ?? '').toLowerCase();
    if (hay.isEmpty) return false;
    const markers = [
      'commentary', 'commento', 'comentario', 'commentaire', 'kommentar',
      'audio description', 'audio-description', 'audiodescription', 'descriptive',
      'visually impaired', 'isolated score', 'music only',
    ];
    return markers.any(hay.contains);
  }

  static bool _isExtraAudioForSubtitles(AudioTrack track) {
    if (_isExtraAudioTrack(track)) return true;
    final title = (track.title ?? '').toLowerCase();
    if (title.isEmpty) return false;
    const markers = [
      'description', 'visual impaired',
      '(ad)', '[ad]', '(vi)', '[vi]', '(comm)', '[comm]', 'audio comm',
      'cast & crew', 'cast and crew', 'cast/crew', 'film historian', 'trivia track',
      "director's track", 'directors track', 'with director', 'with the director',
      'by director', 'by the director', 'score only', 'music & effects', 'music and effects', 'm&e',
      'commenti', 'audiodescrizione', 'audio descrizione',
      'audiodescripcion', 'audiodescripción', 'audio descripcion', 'audio descripción',
      'audiodeskription', 'hörfilm', 'hoerfilm', 'bildbeschreibung',
      'comentário', 'audiodescrição', 'audiodescricao', 'commentaar', 'audiobeschrijving',
      'komentarz', 'audiodeskrypcja', 'комментар', 'тифлокоммент',
      'コメンタリー', '音声解説', '副音声', '코멘터리', '음성 해설',
      '评论音轨', '評論音軌', '导评', '導評', '口述影像', 'sesli betimleme', 'yönetmen yorumu',
    ];
    if (markers.any(title.contains)) return true;
    final words = title.split(RegExp(r'[^\p{L}\p{N}]+', unicode: true)).where((word) => word.isNotEmpty).toList();
    const adSuffixes = {'stereo', 'mono', 'surround', 'dolby', 'ac3', 'eac3', 'aac', 'dd', 'ddp', 'track', 'audio'};
    for (var i = 0; i < words.length; i++) {
      final word = words[i];
      if (word == 'comm' || word == 'comment' || word == 'dvs') return true;
      if (word == 'ad' &&
          (i + 1 == words.length || adSuffixes.contains(words[i + 1]) || RegExp(r'^\d').hasMatch(words[i + 1]))) {
        return true;
      }
    }
    return false;
  }

  static bool _trackMatchesLang(AudioTrack t, String langKey) {
    final codes = _langCodeList(langKey);
    if (codes.isEmpty) return false;
    final rawLang = (t.language ?? '').trim().toLowerCase().replaceAll('_', '-');
    final baseLang = rawLang.split('-').first;
    if (rawLang.isNotEmpty && codes.any((c) => c == rawLang || (!c.contains('-') && c == baseLang))) {
      return true;
    }
    final title = (t.title ?? '').toLowerCase();
    if (title.isEmpty) return false;
    final words = title.split(RegExp(r'[^a-z0-9à-ÿ]+')).where((w) => w.isNotEmpty).toList();
    return words.any((w) => codes.contains(w));
  }

  /// Ensures libmpv lands on the user's preferred audio language (e.g. English), or the film's
  /// original language (e.g. Korean for Parasite) when no preferred track exists — instead of
  /// falling back to a foreign dub marked default (e.g. French VFF in a MULTI release).
  void _ensurePreferredAudioTrack(List<AudioTrack> tracks) {
    if (_preferredAudioApplied) return;
    final real = tracks.where((t) => _isMediaTrack(t.id)).toList();
    if (real.isEmpty) return;
    _preferredAudioApplied = true;
    final main = real.where((t) => !_isExtraAudioTrack(t)).toList();
    final pool = main.isNotEmpty ? main : real;

    AudioTrack? pickFor(String? lang) {
      if (lang == null || lang.isEmpty) return null;
      for (final t in pool) {
        if (_trackMatchesLang(t, lang)) return t;
      }
      return null;
    }

    AudioTrack? carried;
    if (_carriedAudio case final previous?) {
      for (final track in real) {
        if (_sameTrackLanguage(track.language, previous.language) || _trackMatchesLang(track, previous.language ?? '')) {
          carried ??= track;
          if (track.title != null && track.title == previous.title) {
            carried = track;
            break;
          }
        }
      }
    }
    final want = carried ?? pickFor(_preferredAudioLang) ?? pickFor(widget.session.originalLanguage);
    if (want != null && want.id != _activeAudio.id) {
      unawaited(_player.setAudioTrack(want));
    } else if (_isExtraAudioTrack(_activeAudio) && main.isNotEmpty && main.first.id != _activeAudio.id) {
      unawaited(_player.setAudioTrack(main.first));
    }
  }

  Future<void> _configureAndOpen() async {
    String hwdecMode = 'auto';
    bool passthrough = false;
    bool nightMode = false;
    String streamLang = 'english';
    try {
      final res = await VeyloBridge.instance.settingsGet();
      final st = (res['settings'] as Map<String, dynamic>?) ?? {};
      hwdecMode = st['hwdec_mode']?.toString() ?? 'auto';
      passthrough = st['audio_passthrough'] as bool? ?? false;
      nightMode = st['audio_night_mode'] as bool? ?? false;
      final savedLang = st['stream_language']?.toString();
      _subtitleFallbackLanguage = savedLang;
      if (savedLang != null && savedLang.isNotEmpty) {
        streamLang = savedLang;
      }
      _preferredAudioLang = streamLang;
      if (mounted) {
        setState(() {
          _subScale = (st['sub_scale'] as num?)?.toDouble().clamp(0.6, 2.0) ?? 1.0;
          _subPos = (st['sub_pos'] as num?)?.toInt().clamp(70, 100) ?? 95;
          _subBg = st['sub_bg'] as bool? ?? false;
          _subColor = st['sub_color']?.toString() ?? '#F2EFEA';
          _nightMode = nightMode;
          _autoNext = st['auto_next'] as bool? ?? true;
          _autoSkipIntro = st['auto_skip_intro'] as bool? ?? true;
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

      // Audio policy: prefer configured language, then fall back to the film's original language
      // (so a Korean film with French+Korean tracks plays Korean rather than a French default dub).
      await setProp('audio-spdif', passthrough ? 'ac3,eac3,dts,dts-hd,truehd' : '');
      if (nightMode) {
        await setProp('af', 'lavfi=[dynaudnorm=f=150:g=15:p=0.9]');
      }
      final alangParts = <String>[
        ..._langCodeList(streamLang).where((c) => c.length <= 6),
        ..._langCodeList(widget.session.originalLanguage).where((c) => c.length <= 6),
      ];
      final uniqueAlang = <String>{...alangParts}.toList();
      if (uniqueAlang.isNotEmpty) {
        await setProp('alang', uniqueAlang.join(','));
      }
      try {
        _defaultUserAgent = await platform.getProperty('user-agent');
        _reconnectOptions = await platform.getProperty('stream-lavf-o');
      } catch (_) {}
    }

    if (mounted) await _openStream();
  }

  void _attachExternalAudio() {
    final audioUrl = _source == null ? widget.session.audioUrl : _source!.audioUrl;
    if (_externalAudioAttached || audioUrl == null || audioUrl.isEmpty) return;
    _externalAudioAttached = true;
    unawaited(
      _player.setAudioTrack(AudioTrack.uri(audioUrl, title: 'Original')).catchError((_) {}),
    );
  }

  Future<void> _openStream({SourceItem? source}) async {
    if (!mounted || _finalProgress != null) return;
    final replacing = _sourceGeneration != 0;
    final generation = ++_sourceGeneration;
    _monitorSource = false;
    _firstFrame = false;
    _firstFrameTimer?.cancel();
    _stallTimer?.cancel();
    _stallTimer = null;
    _decodeFailures.clear();
    _sourcePosition = _resumePosition;
    _seekPending = false;
    _externalAudioAttached = false;
    _preferredAudioApplied = false;
    _restoredSubtitle = false;
    _subtitleFallbackTimer?.cancel();
    _subtitleFallbackTimer = null;
    _subtitleDownload = null;
    _chapterRetry?.cancel();
    _chaptersRequested = false;
    _chaptersReady = false;
    _segmentsRequested = false;
    _undoTimer?.cancel();
    setState(() {
      _source = source ?? _source;
      _error = null;
      _loaded = false;
      _playing = true;
      _audioTracks = [];
      _subTracks = [];
      _activeAudio = AudioTrack.auto();
      _activeSub = SubtitleTrack.auto();
      _chapters = const [];
      _onlineSegments = [];
      _segments = [];
      _undoSegment = null;
    });
    _posN.value = _resumePosition;
    _durN.value = Duration.zero;
    _bufN.value = Duration.zero;
    try {
      // Stop the old file before changing file-specific HTTP options or accepting its events.
      if (replacing) await _player.stop();
      if (!mounted || _finalProgress != null || generation != _sourceGeneration) return;
      final platform = _player.platform;
      final url = _sourceUrl;
      final headers = _source?.headers ?? widget.session.headers;
      if (platform is NativePlayer) {
        final ua = headers['User-Agent'] ?? headers['user-agent'] ?? _defaultUserAgent;
        if (ua != null) await platform.setProperty('user-agent', ua);
        // HLS's demuxer can deadlock with reconnect_streamed enabled.
        final isHls = url.contains('.m3u8') || url.contains('/hls_variant/');
        if (isHls || _reconnectOptions != null) {
          await platform.setProperty('stream-lavf-o', isHls ? '' : _reconnectOptions!);
        }
      }
      if (!mounted || _finalProgress != null || generation != _sourceGeneration) return;
      _monitorSource = true;
      final slowStart = (_sourceBytes ?? 0) > 40000000000 || (_source?.bitrateMbps ?? 0) > 60;
      _firstFrameTimer = Timer(Duration(seconds: slowStart ? 20 : 12), () {
        if (generation == _sourceGeneration && !_firstFrame) _failSource('first frame timed out');
      });
      await _player.open(
        Media(url, httpHeaders: headers, start: _resumePosition > Duration.zero ? _resumePosition : null),
        play: true,
      );
    } catch (_) {
      if (mounted && generation == _sourceGeneration && _error == null) {
        _failSource('could not open source');
      }
    }
  }

  void _onPosition(Duration position) {
    if (!mounted || !_monitorSource) return;
    final step = position - _sourcePosition;
    final moved = step != Duration.zero;
    _sourcePosition = position;

    if (!_firstFrame && step > Duration.zero && position > _resumePosition && (_player.state.width ?? 0) > 0) {
      _firstFrame = true;
      _firstFrameTimer?.cancel();
      setState(() => _loaded = true);
    }
    if (_firstFrame && _duration > Duration.zero) {
      if (_player.state.playing && !_player.state.buffering && !_seekPending &&
          step > Duration.zero && step.inMilliseconds <= 2000 * _speed.clamp(1.0, 3.0)) {
        _watchedAccumMs += step.inMilliseconds;
      }
      _recordPosition = position;
      _recordDuration = _duration;
    }
    if (moved) {
      _seekPending = false;
      _stallTimer?.cancel();
      _stallTimer = null;
    }
    _updateStallWatchdog();

    if (_scrubValue != null) return;
    if (_firstFrame) _posN.value = position;
    if (_firstFrame && _playing && moved) _maybeAutoSkip();
  }

  void _updateStallWatchdog() {
    if (!mounted || !_monitorSource || !_firstFrame || !_player.state.playing || !_player.state.buffering) {
      _stallTimer?.cancel();
      _stallTimer = null;
      return;
    }
    _stallTimer ??= Timer(const Duration(seconds: 15), () {
      _stallTimer = null;
      if (_monitorSource && _player.state.playing && _player.state.buffering) {
        _failSource('buffering stalled');
      }
    });
  }

  static bool _isDecodeFailure(String message) {
    final lower = message.toLowerCase();
    return const ['hardware accelerator', 'failed to end picture', 'decode issue', 'error while decoding']
        .any(lower.contains);
  }

  void _rememberSourceChoices() {
    if (!_firstFrame) return;
    _resumePosition = _sourcePosition;
    if (_isMediaTrack(_activeAudio.id)) _carriedAudio ??= _activeAudio;
    if (_manualTrackSelected || _isMediaTrack(_activeSub.id)) _carriedSubtitle ??= _activeSub;
  }

  void _failSource(String reason) {
    if (!mounted || _error != null || _finalProgress != null) return;
    _monitorSource = false;
    _firstFrameTimer?.cancel();
    _stallTimer?.cancel();
    _stallTimer = null;
    _rememberSourceChoices();
    _failedSources.add(_sourceUrl);

    final candidates = widget.session.sources;
    final current = candidates.indexWhere((source) => source.url == _sourceUrl);
    if (_automaticSwitches < 3) {
      final next = candidates.indexWhere(
        (source) => source.verified && !_failedSources.contains(source.url),
        current + 1,
      );
      if (next >= 0) {
        _automaticSwitches++;
        _reportFailover(reason);
        final verified = candidates.where((source) => source.verified).toList();
        final replacement = candidates[next];
        _say('Source failed — trying the next one (${verified.indexOf(replacement) + 1}/${verified.length})');
        unawaited(_openStream(source: replacement));
        return;
      }
    }

    setState(() {
      _error = 'This source could not be played.';
      _playing = false;
    });
    unawaited(_player.stop().catchError((_) {}));
  }

  void _reportFailover(String reason) {
    final codec = switch (_source?.videoCodec) {
      'av1' => 'av1',
      'hevc' => 'hevc',
      'avc' => 'avc',
      'vp9' => 'vp9',
      _ => 'unknown',
    };
    final resolution = switch (_sourceQuality) {
      '4K' || '2160p' => '4K',
      '1440p' => '1440p',
      '1080p' => '1080p',
      '720p' => '720p',
      '480p' || 'SD' => 'SD',
      _ => 'unknown',
    };
    final bytes = _sourceBytes;
    final size = switch (bytes) {
      null => 'unknown',
      > 40000000000 => '>40 GB',
      >= 10000000000 => '10–40 GB',
      _ => '<10 GB',
    };
    unawaited(VeyloBridge.instance.reportError(
      'player:failover', '$reason · codec=$codec · resolution=$resolution · size=$size', null,
    ));
  }

  static bool _sameTrackLanguage(String? a, String? b) {
    final lang = _baseLanguage(b);
    if (lang.isEmpty || lang == 'und') return false;
    final actual = _baseLanguage(a);
    return actual == lang || _langCodeList(a).contains(lang) || _langCodeList(b).contains(actual);
  }

  void _restoreSubtitle(List<SubtitleTrack> tracks) {
    final previous = _carriedSubtitle;
    if (previous == null || _restoredSubtitle || !tracks.any((track) => _isMediaTrack(track.id)) &&
        !_player.state.tracks.audio.any((track) => _isMediaTrack(track.id))) {
      return;
    }
    if (previous.uri || previous.data || !_isMediaTrack(previous.id)) {
      _restoredSubtitle = true;
      unawaited(_player.setSubtitleTrack(previous).catchError((_) {}));
      return;
    }
    SubtitleTrack? match;
    for (final track in tracks) {
      if (!_isMediaTrack(track.id) || !_sameTrackLanguage(track.language, previous.language)) continue;
      match ??= track;
      if (track.title == previous.title) {
        match = track;
        break;
      }
    }
    if (match != null) {
      _restoredSubtitle = true;
      unawaited(_player.setSubtitleTrack(match).catchError((_) {}));
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
    if (Platform.isAndroid) {
      unawaited(SystemChrome.setEnabledSystemUIMode(SystemUiMode.edgeToEdge));
    }
    if (_appRef?.onBeforeClosePlayer == _finishProgress) {
      _appRef?.onBeforeClosePlayer = null;
    }
    if (_fullscreen) {
      unawaited(_windowChannel.invokeMethod<bool>('setFullscreen', false).catchError((_) => false));
    }
    if (_finalProgress == null && widget.session.media != null) {
      unawaited(_finishProgress().then((_) => VeyloBridge.instance.flushProgress()).catchError((_) {}));
    }
    _monitorSource = false;
    _firstFrameTimer?.cancel();
    _stallTimer?.cancel();
    _progressTimer?.cancel();
    _hideTimer?.cancel();
    _osdTimer?.cancel();
    _undoTimer?.cancel();
    _sleepTick?.cancel();
    _chapterRetry?.cancel();
    _subtitleFallbackTimer?.cancel();
    _subSearch.dispose();
    _subSearchFocus.dispose();
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

  void _checkpointProgress() {
    final media = widget.session.media;
    final position = _recordPosition.inMilliseconds;
    final duration = _recordDuration.inMilliseconds;
    if (media == null || !_monitorSource || !_firstFrame || !_playing ||
        _finalProgress != null || _progressWrite != null || duration <= 0 || position <= 0 ||
        (_watchedAccumMs == 0 && (position - _lastRecordedMs).abs() < 1000)) {
      return;
    }
    _progressWrite = _writeProgress(media, position, duration).whenComplete(() => _progressWrite = null);
  }

  Future<void> _writeProgress(MediaItem media, int position, int duration, {bool last = false}) async {
    final watched = _watchedAccumMs;
    _watchedAccumMs = 0;
    try {
      await VeyloBridge.instance.recordProgress(
        media,
        videoId: widget.session.videoId,
        positionMs: position,
        durationMs: duration,
        watchedMs: watched,
        withMeta: !_sentProgress || last,
      );
      _sentProgress = true;
      _lastRecordedMs = position;
    } catch (_) {
      _watchedAccumMs += watched;
    }
  }

  Future<void> _finishProgress() => _finalProgress ??= _flushFinalProgress();

  Future<void> _flushFinalProgress() async {
    _progressTimer?.cancel();
    _firstFrameTimer?.cancel();
    _stallTimer?.cancel();
    _monitorSource = false;
    final media = widget.session.media;
    final position = _recordPosition.inMilliseconds;
    final duration = _recordDuration.inMilliseconds;
    if (media == null || duration <= 0 || position <= 0) return;
    await _progressWrite;
    await _writeProgress(media, position, duration, last: true);
  }

  Future<void> _fetchSegments(String videoId, double durationSec, int generation) async {
    try {
      final segs = await VeyloBridge.instance.onlineSegments(videoId, durationSec);
      if (!mounted || generation != _sourceGeneration || !_monitorSource) return;
      _onlineSegments = segs;
      if (_chaptersReady) _mergeSegments();
    } catch (_) {}
  }

  Future<List<Map<String, dynamic>>> _readMpvList(String property) async {
    final platform = _player.platform;
    if (platform is! NativePlayer) return const [];
    try {
      final raw = await platform.getProperty(property);
      final decoded = jsonDecode(raw);
      if (decoded is List) return decoded.whereType<Map<String, dynamic>>().toList();
    } catch (_) {}
    return const [];
  }

  Future<void> _readChapters(int generation, {bool retry = true}) async {
    final entries = await _readMpvList('chapter-list');
    if (!mounted || generation != _sourceGeneration || !_monitorSource) return;
    final chapters = <_Chapter>[];
    for (final (index, entry) in entries.indexed) {
      final time = entry['time'];
      if (time is! num || !time.isFinite) continue;
      final rawTitle = entry['title'];
      final title = rawTitle is String ? rawTitle.trim() : '';
      chapters.add((
        start: time < 0 ? 0.0 : time.toDouble(),
        title: title.isEmpty ? 'Chapter ${index + 1}' : title,
      ));
    }
    if (chapters.isEmpty && retry) {
      _chapterRetry = Timer(const Duration(seconds: 2), () {
        unawaited(_readChapters(generation, retry: false));
      });
      return;
    }
    chapters.sort((a, b) => a.start.compareTo(b.start));
    _chapters = chapters;
    _chaptersReady = true;
    _mergeSegments();
  }

  static String? _chapterKind(String title) {
    final lower = title.toLowerCase();
    final words = lower.split(RegExp(r'[^\p{L}\p{N}]+', unicode: true)).where((word) => word.isNotEmpty).toList();
    if (words.isEmpty) return null;
    bool shortName(String prefix) =>
        RegExp('^$prefix[0-9]*\$').hasMatch(words.first) &&
        (words.length == 1 || (words.length == 2 && RegExp(r'^[0-9]+$').hasMatch(words[1])));
    if (words.contains('recap') || words.first == 'previously') return 'recap';
    if (words.contains('intro') || words.contains('opening') || shortName('op') ||
        lower.contains('title sequence') || lower.contains('main title')) {
      return 'intro';
    }
    if (words.contains('credits') || words.contains('outro') || words.contains('ending') ||
        shortName('ed') || lower.contains('end titles')) {
      return 'credits';
    }
    return null;
  }

  void _mergeSegments() {
    final duration = _duration.inMilliseconds / 1000;
    final local = <Map<String, dynamic>>[];
    for (final (index, chapter) in _chapters.indexed) {
      final kind = _chapterKind(chapter.title);
      if (kind == null) continue;
      final end = index + 1 < _chapters.length ? _chapters[index + 1].start : duration;
      final length = end - chapter.start;
      final fits = switch (kind) {
        'intro' => length >= 5 && length <= 240,
        'recap' => length >= 5 && length <= 300,
        _ => length >= 5 && length <= 900 && duration > 0 && chapter.start >= duration / 2,
      };
      if (fits) local.add({'kind': kind, 'start': chapter.start, 'end': end});
    }
    final localKinds = local.map((segment) => segment['kind']).toSet();
    setState(() {
      _segments = [...local, ..._onlineSegments.where((segment) => !localKinds.contains(segment['kind']))];
    });
  }

  static bool _segmentContains(Map<String, dynamic> segment, double position, {double tail = 0}) =>
      position >= (segment['start'] as num) && position < (segment['end'] as num) - tail;

  void _maybeAutoSkip() {
    if (!_autoSkipIntro || _seekPending) return;
    final position = _position.inMilliseconds / 1000;
    final indexes = <String, int>{};
    for (final segment in _segments) {
      final kind = segment['kind'] as String;
      if (kind == 'credits') continue;
      // The same intro may have different timestamps in an alternate encode.
      final index = indexes.update(kind, (i) => i + 1, ifAbsent: () => 0);
      if (!_segmentContains(segment, position) ||
          _backwardSeekTargets.any((target) => _segmentContains(segment, target)) ||
          !_skippedSegments.add((kind, index))) {
        continue;
      }
      _seekTo(_segmentBoundary(segment, 'end'), userInitiated: false);
      _undoTimer?.cancel();
      setState(() => _undoSegment = segment);
      _undoTimer = Timer(const Duration(seconds: 5), () {
        if (mounted) setState(() => _undoSegment = null);
      });
      break;
    }
  }

  static Duration _segmentBoundary(Map<String, dynamic> segment, String edge) =>
      Duration(milliseconds: ((segment[edge] as num) * 1000).round());

  void _undoSkip() {
    final segment = _undoSegment;
    if (segment == null) return;
    _undoTimer?.cancel();
    setState(() => _undoSegment = null);
    _seekTo(_segmentBoundary(segment, 'start'));
  }

  void _seekTo(Duration target, {bool userInitiated = true}) {
    if (!_monitorSource || !_firstFrame) return;
    if (userInitiated && target < _position) _backwardSeekTargets.add(target.inMilliseconds / 1000);
    _seekPending = true;
    unawaited(_player.seek(target).catchError((_) {}));
  }

  static String _baseLanguage(String? language) =>
      (language ?? '').trim().toLowerCase().split(RegExp(r'[-_]')).first;

  Future<void> _applySubtitleFallback(int generation) async {
    bool stillCurrent() => mounted && _monitorSource && generation == _sourceGeneration &&
        !_manualTrackSelected && _carriedSubtitle == null;
    final key = _subtitleFallbackLanguage?.trim().toLowerCase();
    final codes = _langCodeList(key).map(_baseLanguage).toSet();
    if (!stillCurrent() || key == null || codes.isEmpty) return;

    // media_kit provides the tracks but omits mpv's disposition and selection flags.
    final metadata = await _readMpvList('track-list');
    if (!stillCurrent()) return;
    final audioFlags = {
      for (final track in metadata)
        if (track['type'] == 'audio') track['id'].toString(): track,
    };
    final subtitleFlags = {
      for (final track in metadata)
        if (track['type'] == 'sub') track['id'].toString(): track,
    };

    final tracks = _player.state.tracks;
    final mainAudio = tracks.audio.where((track) {
      final flags = audioFlags[track.id];
      return _isMediaTrack(track.id) &&
          !_isExtraAudioForSubtitles(track) &&
          flags?['visual-impaired'] != true &&
          flags?['commentary'] != true &&
          flags?['comment'] != true;
    });
    const unknown = {'', 'und', 'mul', 'zxx', 'mis'};
    if (mainAudio.isEmpty ||
        mainAudio.any((track) {
          final language = _baseLanguage(track.language);
          return unknown.contains(language) || codes.contains(language);
        })) {
      return;
    }

    SubtitleTrack? textTrack;
    SubtitleTrack? imageTrack;
    for (final track in tracks.subtitle) {
      if (!_isMediaTrack(track.id) || !codes.contains(_baseLanguage(track.language))) continue;
      final flags = subtitleFlags[track.id];
      final title = (track.title ?? '').toLowerCase();
      if (flags?['forced'] == true || title.contains('forced') || title.contains('signs')) continue;
      if (flags?['selected'] == true || _activeSub.id == track.id) return;
      if (flags?['external'] == true || track.uri || track.data) continue;
      final codec = (track.codec ?? '').toLowerCase();
      if (track.image == true || const ['pgs', 'dvd', 'hdmv', 'dvb'].any(codec.contains)) {
        imageTrack ??= track;
      } else {
        textTrack ??= track;
      }
    }

    final label = key[0].toUpperCase() + key.substring(1);
    final embedded = textTrack ?? imageTrack;
    if (embedded != null) {
      try {
        await _player.setSubtitleTrack(embedded);
        if (stillCurrent()) {
          _say("Subtitles: $label (audio isn't in $label)");
        }
      } catch (_) {
        if (stillCurrent()) _say('Could not load subtitle');
      }
      return;
    }

    await _fetchOnlineSubs();
    if (!stillCurrent()) return;
    for (final sub in _onlineSubs) {
      if (!codes.contains(_baseLanguage(sub['code']?.toString()))) continue;
      await _loadOnlineSubtitle(sub, automatic: true);
      break;
    }
  }

  Future<void> _fetchOnlineSubs() async {
    final inFlight = _onlineSubsRequest;
    if (inFlight != null) return inFlight;
    final media = widget.session.media;
    if (media == null || _onlineSubs.isNotEmpty) return;

    final request = _requestOnlineSubs(media);
    setState(() => _onlineSubsRequest = request);
    await request;
    if (mounted) setState(() => _onlineSubsRequest = null);
  }

  Future<void> _requestOnlineSubs(MediaItem media) async {
    try {
      final list = await VeyloBridge.instance.onlineSubtitles(
        media.kind,
        widget.session.videoId ?? media.id,
        filename: _source == null ? widget.session.filename : _source!.filename,
        fileSize: _sourceBytes,
      );
      if (mounted) setState(() => _onlineSubs = list);
    } catch (_) {}
  }

  Future<void> _loadOnlineSubtitle(Map<String, dynamic> sub, {bool automatic = false}) async {
    if (_subtitleDownload != null || (automatic && _manualTrackSelected)) return;
    if (!automatic) {
      _manualTrackSelected = true;
      _carriedSubtitle = null;
    }
    final download = Object();
    setState(() => _subtitleDownload = download);

    bool stillWanted() =>
        mounted && identical(_subtitleDownload, download) && (!automatic || !_manualTrackSelected);

    try {
      final url = sub['url']?.toString() ?? '';
      if (url.isEmpty) throw StateError('Missing subtitle source');
      final path = await VeyloBridge.instance.fetchSubtitle(url);
      if (!stillWanted()) return;
      final language = sub['language']?.toString() ?? 'Subtitle';
      final track = SubtitleTrack.uri(path, title: language, language: sub['code']?.toString());
      await _player.setSubtitleTrack(track);
      if (!stillWanted()) return;

      // media_kit can report a rejected sub-add command through its log alone.
      final platform = _player.platform;
      if (platform is NativePlayer) {
        final activePath = await platform.getProperty('current-tracks/sub/external-filename');
        if (!stillWanted()) return;
        if (activePath != path) throw StateError('Subtitle file was not selected');
      }

      _carriedSubtitle = track;
      _say('Subtitles: $language (downloaded)');
      if (!automatic && _sheet == _PlayerSheet.subtitles) setState(() => _sheet = null);
    } catch (_) {
      if (stillWanted()) _say('Could not load subtitle');
    } finally {
      if (mounted && identical(_subtitleDownload, download)) {
        setState(() => _subtitleDownload = null);
      }
    }
  }

  void _chooseSubtitle(SubtitleTrack track) {
    _manualTrackSelected = true;
    _carriedSubtitle = track;
    _restoredSubtitle = true;
    unawaited(_player.setSubtitleTrack(track));
    setState(() {
      _subtitleDownload = null;
      _sheet = null;
    });
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
    _seekTo(Duration(seconds: target));
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

  void _toggleChapterSheet() {
    setState(() => _sheet = _sheet == _PlayerSheet.chapters ? null : _PlayerSheet.chapters);
    _pokeChrome();
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

  Map<String, dynamic>? _activeSkip({required bool credits}) {
    if (!_monitorSource || !_firstFrame || _sheet != null || (!credits && _autoSkipIntro)) return null;
    final position = _position.inMilliseconds / 1000;
    for (final segment in _segments) {
      if ((segment['kind'] == 'credits') == credits && _segmentContains(segment, position, tail: 1)) {
        return segment;
      }
    }
    return null;
  }

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final s = widget.session;
    final visible = _chromeVisible || !_playing || _sheet != null || _scrubValue != null || _error != null || !_loaded;
    final sleepBadge = _sleepBadgeLabel();
    final insets = MediaQuery.viewPaddingOf(context);
    final viewport = MediaQuery.sizeOf(context);
    final safeWidth = viewport.width - insets.horizontal;
    final compactControls = Platform.isAndroid && safeWidth < 660;
    final bottomRowOffset = compactControls ? 48.0 : 0.0;

    Widget centerOverlay(Widget child) => Positioned.fill(
      left: insets.left,
      top: insets.top,
      right: insets.right,
      bottom: insets.bottom,
      child: Center(child: child),
    );

    Widget scrollActions(List<Widget> actions) => SingleChildScrollView(
      scrollDirection: Axis.horizontal,
      child: Row(mainAxisSize: MainAxisSize.min, children: actions),
    );

    List<Widget> topActions(List<Widget> actions) => compactControls
        ? [Flexible(flex: 2, child: scrollActions(actions))]
        : actions;

    Widget bottomControls(Widget timeline, List<Widget> actions) {
      if (compactControls) {
        return Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.end,
          children: [timeline, scrollActions(actions)],
        );
      }
      return Row(children: [Expanded(child: timeline), ...actions]);
    }

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
        if (_subSearchFocus.hasFocus) return KeyEventResult.ignored;

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

        if (_sheet == null && (k == LogicalKeyboardKey.enter || k == LogicalKeyboardKey.numpadEnter)) {
          if (_undoSegment != null) {
            _undoSkip();
            return KeyEventResult.handled;
          }
          if (_activeSkip(credits: false) case final segment?) {
            _seekTo(_segmentBoundary(segment, 'end'));
            return KeyEventResult.handled;
          }
        }
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
        if (k == LogicalKeyboardKey.keyC && _chapters.length >= 2) {
          _toggleChapterSheet();
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
                    top: 86 + insets.top,
                    left: 24 + insets.left,
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
                              if (_sourceQuality != null) _hudLine('Quality', '$_sourceQuality${_sourceHdr != null ? ' · $_sourceHdr' : ''}'),
                              if (_sourceSize != null) _hudLine('Size', _sourceSize!),
                              _hudLine('Audio', _activeAudio.title ?? _activeAudio.language ?? _activeAudio.id),
                              _hudLine('Subtitles', _activeSub.id == 'no' ? 'Off' : (_activeSub.title ?? _activeSub.language ?? _activeSub.id)),
                            ],
                          ),
                        ),
                      ),
                    ),
                  ),

                // Center error panel
                if (_error != null)
                  centerOverlay(
                    ConstrainedBox(
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
                                    label: 'Switch source',
                                    icon: 'Layers',
                                    onTap: () => setState(() => _sheet = _PlayerSheet.sources),
                                  ),
                                VeyloPill(label: 'Close', icon: 'Close', onTap: () => app.closePlayer()),
                              ],
                            ),
                          ],
                        ),
                      ),
                    ),
                  ),

                // OSD pill with smooth enter & exit
                Positioned(
                  top: insets.top + (viewport.height - insets.vertical) * 0.18,
                  left: insets.left,
                  right: insets.right,
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
                    final seg = _activeSkip(credits: false);
                    final undo = _sheet == null ? _undoSegment : null;
                    final showUpNext = !_upNextDismissed &&
                        _sheet == null &&
                        s.nextEpisode != null &&
                        s.media != null &&
                        durSec > 60 &&
                        ((durSec - posSec) <= 25 && (durSec - posSec) > 0 || _activeSkip(credits: true) != null);
                    if (seg == null && undo == null && !showUpNext) return const SizedBox.shrink();
                    return Stack(
                      fit: StackFit.expand,
                      children: [
                        if (undo != null || seg != null)
                          Positioned(
                            right: 24 + insets.right,
                            bottom: insets.bottom + (visible ? 86 + bottomRowOffset : 32),
                            child: VeyloPill(
                              label: undo != null ? 'Undo' : seg!['kind'] == 'recap' ? 'Skip recap' : 'Skip intro',
                              icon: undo != null ? 'Undo' : 'Next',
                              onTap: undo != null ? _undoSkip : () => _seekTo(_segmentBoundary(seg!, 'end')),
                            ),
                          ),
                        if (showUpNext)
                          Positioned(
                            right: 24 + insets.right,
                            bottom: insets.bottom + (visible ? 88 + bottomRowOffset : 32) + (seg != null || undo != null ? 58.0 : 0.0),
                            child: LiftIn(
                              offsetY: 12,
                              duration: const Duration(milliseconds: 220),
                              child: SizedBox(
                                width: (safeWidth - 48).clamp(0.0, 336.0),
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
                                          LampCountdownBorder(
                                            progress: _autoNext ? ((durSec - posSec) / 25.0).clamp(0.0, 1.0) : 0.0,
                                            lamp: lamp,
                                            child: VeyloPill(
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
                  top: 20 + insets.top,
                  left: 20 + insets.left,
                  right: 20 + insets.right,
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
                                      if (_sourceQuality != null)
                                        Text('· $_sourceQuality', style: VeyloText.ui(13, weight: 500, color: const Color.fromRGBO(242, 239, 234, 0.72))),
                                      if (_sourceHdr != null)
                                        Text('· $_sourceHdr', style: VeyloText.ui(13, weight: 700, color: lamp)),
                                      if (_sourceSize != null)
                                        Text('· $_sourceSize', style: VeyloText.ui(13, weight: 500, color: const Color.fromRGBO(242, 239, 234, 0.72), tabular: true)),
                                    ],
                                  ),
                                ],
                              ),
                            ),
                            ...topActions([
                              if (sleepBadge != null) ...[
                                MouseRegion(
                                  cursor: SystemMouseCursors.click,
                                  child: GestureDetector(
                                    onTap: () => setState(() => _sheet = _sheet == _PlayerSheet.sleep ? null : _PlayerSheet.sleep),
                                    child: GlassBox(
                                      borderRadius: 999,
                                      lamp: lamp,
                                      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 7),
                                      child: Row(
                                        mainAxisSize: MainAxisSize.min,
                                        children: [
                                          BreathingMoonIcon(lamp: lamp, size: 16),
                                          const SizedBox(width: 6),
                                          Text(
                                            sleepBadge,
                                            style: VeyloText.ui(13, weight: 650, color: VeyloColors.bone, tabular: true),
                                          ),
                                        ],
                                      ),
                                    ),
                                  ),
                                ),
                                const SizedBox(width: 8),
                              ],
                              // Top right: what you reach for occasionally — navigation (episodes,
                              // clips, sources, chapters), then playback settings (speed, sleep, picture), then
                              // help/info. The bottom bar keeps only what you use while watching.
                              if (s.episodes.isNotEmpty)
                                VeyloOrb(
                                  icon: 'Stack',
                                  glass: false,
                                  active: _sheet == _PlayerSheet.episodes,
                                  tooltip: 'Episodes',
                                  onTap: () => setState(() => _sheet = _sheet == _PlayerSheet.episodes ? null : _PlayerSheet.episodes),
                                ),
                              if (s.clips.length > 1)
                                VeyloOrb(
                                  icon: 'Film',
                                  glass: false,
                                  active: _sheet == _PlayerSheet.clips,
                                  tooltip: 'Trailers & Clips',
                                  onTap: () => setState(() => _sheet = _sheet == _PlayerSheet.clips ? null : _PlayerSheet.clips),
                                ),
                              if (s.sources.length > 1)
                                VeyloOrb(
                                  icon: 'Layers',
                                  glass: false,
                                  active: _sheet == _PlayerSheet.sources,
                                  tooltip: 'Switch source',
                                  onTap: () => setState(() => _sheet = _sheet == _PlayerSheet.sources ? null : _PlayerSheet.sources),
                                ),
                              if (_chapters.length >= 2)
                                VeyloOrb(
                                  icon: 'List',
                                  glass: false,
                                  active: _sheet == _PlayerSheet.chapters,
                                  tooltip: 'Chapters',
                                  onTap: _toggleChapterSheet,
                                ),
                              if (s.episodes.isNotEmpty || s.clips.length > 1 || s.sources.length > 1 || _chapters.length >= 2)
                                const SizedBox(width: 12),
                              VeyloOrb(
                                icon: 'Gauge',
                                glass: false,
                                active: _sheet == _PlayerSheet.speed,
                                tooltip: 'Playback speed ([ / ])',
                                onTap: () => setState(() => _sheet = _sheet == _PlayerSheet.speed ? null : _PlayerSheet.speed),
                              ),
                              VeyloOrb(
                                icon: 'Moon',
                                glass: false,
                                active: _sheet == _PlayerSheet.sleep || sleepBadge != null,
                                tooltip: 'Sleep timer (T)',
                                onTap: () => setState(() => _sheet = _sheet == _PlayerSheet.sleep ? null : _PlayerSheet.sleep),
                              ),
                              VeyloOrb(
                                icon: 'Sliders',
                                glass: false,
                                active: _sheet == _PlayerSheet.picture ||
                                    _brightness != 0 ||
                                    _contrast != 0 ||
                                    _saturation != 0 ||
                                    _gamma != 0,
                                tooltip: 'Picture',
                                onTap: () => setState(() => _sheet = _sheet == _PlayerSheet.picture ? null : _PlayerSheet.picture),
                              ),
                              const SizedBox(width: 12),
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
                            ]),
                          ],
                        ),
                      ),
                    ),
                  ),
                ),

                // Center transport (.player-mid: scales 0.94 when hiding, icon-turn on play/pause, kick on ±10s)
                if (_loaded && _error == null)
                  centerOverlay(
                    IgnorePointer(
                      ignoring: !visible,
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
                  left: 24 + insets.left,
                  right: 20 + insets.right,
                  bottom: 16 + insets.bottom,
                  child: IgnorePointer(
                    ignoring: !visible,
                    child: AnimatedSlide(
                      offset: visible ? Offset.zero : const Offset(0, 0.35),
                      duration: const Duration(milliseconds: 260),
                      curve: VeyloCurves.out,
                      child: AnimatedOpacity(
                        opacity: visible ? 1.0 : 0.0,
                        duration: const Duration(milliseconds: 240),
                        child: bottomControls(
                          ListenableBuilder(
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
                                      chapters: _chapters,
                                      lamp: lamp,
                                      formatClock: (sec) => _clock(Duration(seconds: sec.round())),
                                      onScrub: (pct) {
                                        setState(() => _scrubValue = pct * durSec);
                                        _pokeChrome();
                                      },
                                      onCommit: (pct) {
                                        final targetMs = (pct * durSec * 1000).round();
                                        setState(() => _scrubValue = null);
                                        _seekTo(Duration(milliseconds: targetMs));
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
                          [
                            const SizedBox(width: 6),
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
                            ] else if (s.clips.length > 1 && s.clipIndex + 1 < s.clips.length) ...[
                              const SizedBox(width: 8),
                              VeyloPill(
                                label: 'Next clip',
                                icon: 'Next',
                                small: true,
                                onTap: () => app.playTrailer(
                                  title: s.clipContextTitle ?? s.title,
                                  clips: s.clips,
                                  startIndex: s.clipIndex + 1,
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
                  right: 20 + insets.right,
                  bottom: 74 + insets.bottom + bottomRowOffset,
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
                            constraints: BoxConstraints(
                              maxWidth: (safeWidth - 40).clamp(0.0, 420.0),
                              maxHeight: (viewport.height - insets.vertical - 180 - bottomRowOffset).clamp(0.0, double.infinity),
                            ),
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
                                        if (_sheet == _PlayerSheet.subtitles && _subtitleDownload != null)
                                          const Padding(
                                            padding: EdgeInsets.symmetric(horizontal: 8),
                                            child: SizedBox.square(
                                              dimension: 16,
                                              child: CircularProgressIndicator(
                                                strokeWidth: 2,
                                                semanticsLabel: 'Downloading subtitle',
                                              ),
                                            ),
                                          ),
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
        _PlayerSheet.clips => 'Trailers & Clips',
        _PlayerSheet.chapters => 'Chapters',
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
                  _manualTrackSelected = true;
                  _preferredAudioApplied = true;
                  _carriedAudio = t;
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
              onTap: () => _chooseSubtitle(SubtitleTrack.no()),
            ),
            for (final t in _subTracks.where((x) => x.id != 'auto' && x.id != 'no'))
              _PlayerOptionRow(
                label: t.title?.isNotEmpty == true ? t.title! : (t.language?.toUpperCase() ?? 'Subtitle ${t.id}'),
                about: t.language ?? 'Embedded',
                selected: _activeSub.id == t.id,
                onTap: () => _chooseSubtitle(t),
              ),
            if (s.media != null || _onlineSubs.isNotEmpty) _buildOnlineSubtitles(),
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
                selected: src.url == _sourceUrl,
                onTap: () {
                  setState(() => _sheet = null);
                  if (src.url == _sourceUrl && _error == null) return;
                  if (_monitorSource) _rememberSourceChoices();
                  unawaited(_openStream(source: src));
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
      _PlayerSheet.clips => Column(
          children: [
            for (int i = 0; i < s.clips.length; i++)
              _PlayerOptionRow(
                label: s.clips[i].name.isNotEmpty ? s.clips[i].name : s.clips[i].label,
                about: s.clips[i].label,
                selected: i == s.clipIndex,
                onTap: () {
                  setState(() => _sheet = null);
                  if (i == s.clipIndex) return;
                  unawaited(app.playTrailer(
                    title: s.clipContextTitle ?? s.title,
                    clips: s.clips,
                    startIndex: i,
                  ));
                },
              ),
          ],
        ),
      _PlayerSheet.chapters => ValueListenableBuilder<Duration>(
          valueListenable: _posN,
          builder: (_, position, _) {
            final current = _chapterIndexAt(_chapters, position.inMilliseconds / 1000.0);
            return Column(
              children: [
                for (final (index, chapter) in _chapters.indexed)
                  _PlayerOptionRow(
                    label: chapter.title,
                    about: _clock(Duration(milliseconds: (chapter.start * 1000).round())),
                    selected: index == current,
                    onTap: () {
                      _seekTo(Duration(milliseconds: (chapter.start * 1000).round()));
                      setState(() => _sheet = null);
                      _pokeChrome();
                    },
                  ),
              ],
            );
          },
        ),
      _PlayerSheet.info => Padding(
          padding: const EdgeInsets.all(12),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              _infoLine('Title', s.title),
              if (_sourceQuality != null) _infoLine('Quality', _sourceQuality!),
              if (_sourceHdr != null) _infoLine('Picture', _sourceHdr!),
              if (_sourceSize != null) _infoLine('Size', _sourceSize!),
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
                ('C', 'Toggle chapters sheet'),
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

  static bool _subtitleMatches(Map<String, dynamic> sub, String query) {
    if (query.isEmpty) return true;
    final language = sub['language']?.toString().toLowerCase() ?? '';
    return sub['code']?.toString().toLowerCase() == query ||
        language.startsWith(query) ||
        language.split(RegExp(r'[ (),]')).any((word) => word.startsWith(query));
  }

  Widget _buildOnlineSubtitles() {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        const Padding(
          padding: EdgeInsets.fromLTRB(12, 12, 12, 6),
          child: Text('Online subtitles', style: VeyloText.micro),
        ),
        Container(
          margin: const EdgeInsets.fromLTRB(8, 0, 8, 8),
          padding: const EdgeInsets.symmetric(horizontal: 12),
          height: 40,
          decoration: BoxDecoration(
            color: const Color.fromRGBO(255, 255, 255, 0.075),
            border: Border.all(color: VeyloColors.edge),
            borderRadius: BorderRadius.circular(20),
          ),
          child: Row(
            children: [
              const VeyloIcon('Search', size: 16, color: VeyloColors.dust),
              const SizedBox(width: 8),
              Expanded(
                child: TextField(
                  controller: _subSearch,
                  focusNode: _subSearchFocus,
                  style: VeyloText.ui(14, weight: 500, color: VeyloColors.bone),
                  decoration: InputDecoration.collapsed(
                    hintText: 'Search by language',
                    hintStyle: VeyloText.ui(13.5, weight: 500, color: VeyloColors.dust),
                  ),
                ),
              ),
            ],
          ),
        ),
        if (_onlineSubsRequest != null)
          const Padding(
            padding: EdgeInsets.all(12),
            child: Text('Searching online subtitles…', style: VeyloText.credit),
          )
        else ...[
          ValueListenableBuilder<TextEditingValue>(
            valueListenable: _subSearch,
            builder: (_, value, _) {
              final query = value.text.trim().toLowerCase();
              final matches = _onlineSubs.where((sub) => _subtitleMatches(sub, query)).take(60).toList();
              if (matches.isEmpty) {
                return Padding(
                  padding: const EdgeInsets.all(12),
                  child: Text(
                    query.isEmpty ? 'No online subtitles available' : 'No matching subtitles',
                    style: VeyloText.credit,
                  ),
                );
              }
              return Column(
                children: [
                  for (final sub in matches)
                    _PlayerOptionRow(
                      label: sub['language']?.toString() ?? 'Subtitle',
                      about: sub['release']?.toString() ?? '',
                      selected: false,
                      onTap: _subtitleDownload == null ? () => unawaited(_loadOnlineSubtitle(sub)) : null,
                    ),
                ],
              );
            },
          ),
        ],
      ],
    );
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
  final VoidCallback? onTap;

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
        cursor: widget.onTap == null ? SystemMouseCursors.basic : SystemMouseCursors.click,
        onEnter: (_) => setState(() => _hovered = true),
        onExit: (_) => setState(() => _hovered = false),
        child: GestureDetector(
          onTap: widget.onTap,
          child: Container(
            padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 10),
            decoration: BoxDecoration(
              color: widget.selected
                  ? lamp.withValues(alpha: 0.16)
                  : (_hovered && widget.onTap != null ? const Color.fromRGBO(255, 255, 255, 0.07) : const Color(0x00000000)),
              borderRadius: BorderRadius.circular(16),
            ),
            child: Row(
              children: [
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        widget.label,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: VeyloText.ui(15, weight: 600, color: widget.onTap == null ? VeyloColors.dust : VeyloColors.bone),
                      ),
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

class _ScrubberBar extends StatefulWidget {
  const _ScrubberBar({
    required this.played,
    required this.buffered,
    required this.durationSec,
    this.segments = const [],
    required this.chapters,
    required this.lamp,
    required this.formatClock,
    required this.onScrub,
    required this.onCommit,
  });

  final double played;
  final double buffered;
  final double durationSec;
  final List<Map<String, dynamic>> segments;
  final List<_Chapter> chapters;
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

  Widget _hoverTooltip(double pct, double width) {
    final seconds = pct * widget.durationSec;
    final index = _chapterIndexAt(widget.chapters, seconds);
    final chapter = index < 0 ? null : widget.chapters[index];
    final maxWidth = width.clamp(0.0, chapter == null ? 96.0 : 200.0);
    final left = (width * pct - maxWidth / 2).clamp(0.0, width - maxWidth);
    return Positioned(
      left: left,
      bottom: 30,
      width: maxWidth,
      child: Center(
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 4),
          decoration: BoxDecoration(
            color: const Color.fromRGBO(12, 12, 15, 0.90),
            borderRadius: BorderRadius.circular(10),
            border: Border.all(color: const Color.fromRGBO(242, 239, 234, 0.18)),
          ),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              if (chapter != null)
                Text(
                  chapter.title,
                  textAlign: TextAlign.center,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: VeyloText.ui(12, weight: 600, color: VeyloColors.bone),
                ),
              Text(
                widget.formatClock(seconds),
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: VeyloText.ui(12, weight: 600, color: VeyloColors.bone, tabular: true),
              ),
            ],
          ),
        ),
      ),
    );
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
                            chapters: widget.chapters,
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
                        _hoverTooltip(hoverPct, w),
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
    required this.chapters,
    required this.trackHeight,
    required this.lamp,
  });

  final double played;
  final double buffered;
  final double? hoverPct;
  final double durationSec;
  final List<Map<String, dynamic>> segments;
  final List<_Chapter> chapters;
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
    if (durationSec > 0) {
      final tick = Paint()
        ..color = const Color.fromRGBO(12, 12, 15, 0.74)
        ..strokeWidth = 1.5;
      for (final chapter in chapters) {
        if (chapter.start <= 0.5 || chapter.start >= durationSec) continue;
        final x = size.width * chapter.start / durationSec;
        canvas.drawLine(Offset(x, top), Offset(x, top + trackHeight), tick);
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
      old.durationSec != durationSec ||
      old.segments != segments ||
      old.chapters != chapters;
}
