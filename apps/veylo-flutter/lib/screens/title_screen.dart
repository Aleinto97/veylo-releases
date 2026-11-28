import 'dart:async';
import 'dart:convert';
import 'dart:ui';
import 'package:cached_network_image/cached_network_image.dart';
import 'package:flutter/widgets.dart';
import '../bridge/veylo_bridge.dart';
import '../state/app_state.dart';
import '../theme/veylo_theme.dart';
import '../widgets/finding_runner.dart';
import '../widgets/glass.dart';
import '../widgets/title_prefetch.dart';
import '../widgets/veylo_icons.dart';

class TitleScreen extends StatefulWidget {
  const TitleScreen({super.key, required this.media});
  final MediaItem media;

  @override
  State<TitleScreen> createState() => _TitleScreenState();
}

class _TitleScreenState extends State<TitleScreen> {
  final ScrollController _scroll = ScrollController();
  final ValueNotifier<double> _heroScrollY = ValueNotifier(0);

  bool _loading = true;
  bool _quickDownloading = false;
  String? _metaError;
  late MediaItem _media;
  ArtworkModel? _art;
  bool _identsPending = true;
  bool _identsFinal = false;
  bool _bodyReady = false;
  bool _scoresKnown = false;
  bool _identsHold = false;
  // One reveal: hero art, logo, tagline, facts and body appear together, already decoded, with
  // values frozen for this visit (fresher data shows next visit via the disk cache).
  bool _revealed = false;
  bool _revealing = false;
  Map<String, Object?> _frozen = const {};
  Timer? _bodyTimer;
  Map<String, dynamic> _meta = {};
  List<EpisodeItem> _episodes = [];
  List<Map<String, dynamic>> _idents = [];
  int? _tomatometer;
  int? _metascore;
  String? _seriesStatus;
  int? _rank;
  Set<String> _watchedEps = {};
  Map<String, Map<String, dynamic>> _epProgress = {};
  Map<String, dynamic>? _collection;
  List<MediaItem> _similar = [];

  int? _selectedSeason;
  final ValueNotifier<int> _frameIndexN = ValueNotifier<int>(0);
  Timer? _frameTimer;
  final Set<String> _failedLogos = {};

  // Source picker sheet state
  String? _chooserVideoId;
  String? _chooserLabel;
  List<SourceItem>? _chooserSources;
  String? _chooserError;
  bool _chooserLoading = false;

  late bool _saved;
  late bool _watched;
  late int _liked;

  @override
  void initState() {
    super.initState();
    _media = widget.media;
    _saved = _media.saved;
    _watched = _media.watched;
    _liked = _media.liked;
    _scroll.addListener(_onScroll);
    // Fallback: a slow/failed details fetch never keeps the body hidden longer than this.
    _bodyTimer = Timer(const Duration(milliseconds: 900), () {
      if (mounted && !_bodyReady) setState(() => _bodyReady = true);
    });
    _frameTimer = Timer.periodic(const Duration(seconds: 9), (_) {
      if (mounted) _frameIndexN.value++;
    });
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      unawaited(_loadDetails());
    });
  }

  @override
  void dispose() {
    _frameTimer?.cancel();
    _bodyTimer?.cancel();
    _frameIndexN.dispose();
    _scroll.removeListener(_onScroll);
    _scroll.dispose();
    _heroScrollY.dispose();
    super.dispose();
  }

  void _onScroll() {
    final y = _scroll.offset;
    AppStateScope.read(context).setScrolled(y > 4);
    final clamped = y.clamp(0.0, 320.0);
    if ((clamped - _heroScrollY.value).abs() >= 0.5) {
      _heroScrollY.value = clamped;
    }
  }

  /// JSON of the last payload applied per part: a live answer identical to what's on screen
  /// (usually, after the disk cache painted) triggers no rebuild, so nothing moves.
  final Map<String, String> _appliedParts = {};

  bool _changed(String part, Map<String, dynamic> d) {
    final sig = jsonEncode(d);
    if (_appliedParts[part] == sig) return false;
    _appliedParts[part] = sig;
    return true;
  }

  // Artwork from two kinds of sources, merged by role instead of arrival order: the hero
  // sources (details' first-paint art, then `title_art` with stills/series art) own the
  // backdrop, logo, stills and videos; Wikidata (`title_extras`) only enriches credits,
  // studios, certificate and language.
  Map<String, dynamic>? _heroArt;
  bool _heroArtFull = false; // `title_art` answered (a superset of details' art)
  Map<String, dynamic>? _enrichArt;
  static const _heroKeys = ['backdrop', 'backdrop_clean', 'logo', 'poster', 'stills', 'trailers', 'tagline', 'videos', 'season_posters'];

  void _rebuildArt() {
    final hero = _heroArt, enrich = _enrichArt;
    if (hero == null && enrich == null) return;
    final merged = <String, dynamic>{...?enrich};
    if (hero != null) {
      for (final e in hero.entries) {
        final v = e.value;
        final empty = v == null || (v is String && v.isEmpty) || (v is List && v.isEmpty) || (v is Map && v.isEmpty);
        if (_heroKeys.contains(e.key) ? !empty : (merged[e.key] == null || (merged[e.key] is List && (merged[e.key] as List).isEmpty))) {
          merged[e.key] = v;
        }
      }
    }
    _art = ArtworkModel.fromJson(merged);
  }

  // Set by the bookmark/like/watched buttons: a library snapshot read before the tap must not
  // undo it when it lands.
  bool _localEdit = false;
  bool _studiosKnown = false;
  bool _extrasDone = false;

  void _maybeBodyReady() {
    // Details only: scores fill in at the end of the facts row and studio logos in their
    // reserved row, so neither has to hold the page back.
    if (!_bodyReady && _detailsKnown) _bodyReady = true;
  }

  bool _detailsKnown = false;

  Future<void> _loadDetails() async {
    if (!mounted) return;
    final app = AppStateScope.read(context);
    final media = _media;
    // Every part starts at once; each is painted as soon as it lands.
    final live = <Future<void>>[
      app.bridge.titleDetails(media).then((d) => _applyDetails(app, d, live: true)).catchError((Object e) {
        if (mounted && _loading) {
          setState(() {
            _loading = false;
            _detailsKnown = true;
            _maybeBodyReady();
            if (_meta.isEmpty) _metaError = e.toString();
          });
        }
      }),
      app.bridge.titleArt(media).then(_applyArt).catchError((_) {}),
      app.bridge.titleScores(media).then(_applyScores).catchError((_) {
        if (mounted && !_scoresKnown) {
          setState(() {
            _scoresKnown = true;
            _maybeBodyReady();
          });
        }
      }),
      app.bridge.titleSimilar(media).then(_applySimilar).catchError((_) {}),
      app.bridge.titleCollection(media).then(_applyCollection).catchError((_) {}),
      app.bridge.titleStudios(media).then(_applyStudios).catchError((_) {
        // Unavailable (offline, timeout): the Wikidata path stays eligible as a fallback.
        if (mounted && !_studiosKnown) {
          setState(() {
            _studiosKnown = true;
            _maybeBodyReady();
          });
        }
      }),
      app.bridge.titleExtras(media).then(_applyExtras).catchError((_) {
        if (mounted) {
          setState(() {
            _extrasDone = true;
            if (!_identsFinal) _identsPending = false;
          });
        }
      }),
    ];
    // Meanwhile: session cache + the disk copy of the whole page from the last visit (even
    // after a restart), painted in one frame before any live answer.
    try {
      final c = await app.bridge.titleCached(media);
      if (mounted) {
        final stored = c['stored'] as Map<String, dynamic>?;
        final artMap = c['artwork'] as Map<String, dynamic>?;
        final meta = c['meta'] as Map<String, dynamic>?;
        if (stored != null) {
          for (final part in ['details', 'art', 'scores', 'similar', 'collection', 'studios', 'extras']) {
            final d = stored[part];
            if (d is! Map<String, dynamic> || _appliedParts.containsKey(part)) continue;
            switch (part) {
              case 'details':
                _applyDetails(app, d, live: false);
              case 'art':
                _applyArt(d);
              case 'scores':
                _applyScores(d);
              case 'similar':
                _applySimilar(d);
              case 'collection':
                _applyCollection(d);
              case 'studios':
                _applyStudios(d);
              case 'extras':
                _applyExtras(d);
            }
          }
        } else if ((artMap != null || meta != null) && !_appliedParts.containsKey('details')) {
          setState(() {
            if (artMap != null) {
              _heroArt ??= artMap;
              _rebuildArt();
            }
            if (meta != null) _meta = meta;
          });
        }
      }
    } catch (_) {}
    await Future.wait(live);
  }

  void _applyArt(Map<String, dynamic> d) {
    if (!mounted || !_changed('art', d)) return;
    final art = d['artwork'] as Map<String, dynamic>?;
    if (art == null) return;
    setState(() {
      _heroArt = art;
      _heroArtFull = true;
      _rebuildArt();
    });
  }

  void _applyScores(Map<String, dynamic> d) {
    if (!mounted) return;
    if (!_changed('scores', d)) {
      if (!_scoresKnown) {
        setState(() {
          _scoresKnown = true;
          _maybeBodyReady();
        });
      }
      return;
    }
    final scores = (d['scores'] as Map<String, dynamic>?) ?? {};
    setState(() {
      _scoresKnown = true;
      _maybeBodyReady();
      _tomatometer = (scores['tomatometer'] as num?)?.toInt();
      _metascore = (scores['metascore'] as num?)?.toInt();
      _seriesStatus = d['status'] as String?;
      _rank = (d['rank'] as num?)?.toInt();
    });
  }

  void _applySimilar(Map<String, dynamic> d) {
    if (!mounted || !_changed('similar', d)) return;
    setState(() {
      _similar = (d['similar'] as List<dynamic>? ?? [])
          .map((e) => MediaItem.fromJson(e as Map<String, dynamic>))
          .toList();
    });
  }

  void _applyCollection(Map<String, dynamic> d) {
    if (!mounted || !_changed('collection', d)) return;
    setState(() => _collection = d['collection'] as Map<String, dynamic>?);
  }

  void _applyStudios(Map<String, dynamic> x) {
    if (!mounted) return;
    final idents = (x['idents'] as List<dynamic>? ?? []).cast<Map<String, dynamic>>();
    if (!_changed('studios', x)) {
      if (!_studiosKnown || _identsPending) {
        setState(() {
          _studiosKnown = true;
          _identsPending = false;
          _maybeBodyReady();
        });
      }
      return;
    }
    setState(() {
      // An empty answer is final only for "Wikidata lists none"; extras may still add some.
      if (idents.isNotEmpty || !_identsFinal) _idents = idents;
      _identsFinal = idents.isNotEmpty;
      _identsPending = false;
      _studiosKnown = true;
      _maybeBodyReady();
    });
  }

  void _applyExtras(Map<String, dynamic> x) {
    if (!mounted || !_changed('extras', x)) return;
    final artMap = x['artwork'] as Map<String, dynamic>?;
    final idents = (x['idents'] as List<dynamic>? ?? []).cast<Map<String, dynamic>>();
    setState(() {
      _extrasDone = true;
      if (artMap != null) {
        _enrichArt = artMap;
        _rebuildArt();
      }
      // Only a fallback: once the fast studio path found logos, they stay put.
      if (!_identsFinal && idents.isNotEmpty) {
        _idents = idents;
        _identsFinal = true;
      }
      _identsPending = false;
    });
  }

  void _applyDetails(AppState app, Map<String, dynamic> d, {required bool live}) {
    if (!mounted) return;
    final stored = d['stored'] is Map<String, dynamic> ? MediaItem.fromJson(d['stored'] as Map<String, dynamic>) : null;
    // Library state (progress, saved, liked, watched) always comes from the live answer, even
    // when the rest of the payload matches the disk copy — and never undoes a tap made since.
    void applyStored() {
      if (!live || stored == null) return;
      _media = stored;
      if (!_localEdit) {
        _saved = stored.saved;
        _watched = stored.watched;
        _liked = stored.liked;
      }
    }

    final meta = (d['meta'] as Map<String, dynamic>?) ?? {};
    final failed = meta['error'] is String;
    // A failed live refresh keeps the good copy on screen (seasons, episodes, synopsis) and
    // only updates progress.
    if (live && failed && _meta.isNotEmpty && _meta['error'] == null) {
      setState(() {
        _loading = false;
        _detailsKnown = true;
        _maybeBodyReady();
        applyStored();
        _watchedEps = (d['watched_episodes'] as List<dynamic>? ?? []).map((e) => e.toString()).toSet();
      });
      return;
    }
    if (!_changed('details', d)) {
      setState(() {
        if (live) _loading = false;
        _detailsKnown = true;
        _maybeBodyReady();
        applyStored();
      });
      return;
    }
    final artMap = d['artwork'] as Map<String, dynamic>?;
    final eps = (meta['episodes'] as List<dynamic>? ?? [])
        .map((e) => EpisodeItem.fromJson(e as Map<String, dynamic>))
        .toList();
    final epProgRaw = (d['episode_progress'] as Map<String, dynamic>?) ?? {};

    int? initSeason = _selectedSeason;
    if (initSeason == null && eps.isNotEmpty) {
      final lv = stored?.lastVideoId ?? _media.lastVideoId;
      final match = lv != null ? eps.where((e) => e.id == lv).firstOrNull : null;
      initSeason = match?.season ?? eps.where((e) => e.season > 0).firstOrNull?.season ?? eps.first.season;
    }

    setState(() {
      _detailsKnown = true;
      _maybeBodyReady();
      // The disk copy paints the final layout; "loading" (Play disabled for series) ends only
      // with the live answer, which carries fresh progress.
      if (live) _loading = false;
      _meta = meta;
      _metaError = meta['error'] as String?;
      if (artMap != null && !_heroArtFull) {
        _heroArt = artMap;
        _rebuildArt();
      }
      _episodes = eps;
      _selectedSeason = initSeason;
      _watchedEps = (d['watched_episodes'] as List<dynamic>? ?? []).map((e) => e.toString()).toSet();
      _epProgress = epProgRaw.map((k, v) => MapEntry(k, (v as Map<String, dynamic>)));
      applyStored();
    });

  }

  /// Decode the hero backdrop and logo at their final sizes, then reveal everything at once —
  /// nothing pops in, fades separately or gets swapped afterwards. Bounded so a slow image
  /// never holds the page (it then shows the title's text heading / the scrim alone).
  Future<void> _prepareReveal(Map<String, Object?> snap) async {
    final app = AppStateScope.read(context);
    Future<bool> warm(ImageProvider provider) async {
      var ok = true;
      await precacheImage(provider, context, onError: (_, _) => ok = false);
      return ok;
    }

    final frames = (snap['frames'] as List<String>?) ?? const [];
    final logo = snap['logo'] as String?;
    final dpr = MediaQuery.devicePixelRatioOf(context);
    final width = MediaQuery.sizeOf(context).width;
    final heroWidth = heroDecodeWidth(width, dpr);
    final jobs = <Future<void>>[
      if (frames.isNotEmpty)
        warm(heroProvider(frames.first, heroWidth)).then((_) {}),
      if (logo != null)
        warm(logoProvider(logo)).then((ok) {
          if (!ok) snap['logo'] = null;
        }),
    ];
    await Future.any([Future.wait(jobs), Future<void>.delayed(const Duration(milliseconds: 900))]);
    if (!mounted) return;
    setState(() {
      _frozen = snap;
      _revealed = true;
      _identsHold = _identsPending || _idents.isNotEmpty;
    });
    if (frames.isNotEmpty) unawaited(app.setLampSource(frames.first));
  }

  Future<void> _openSourceSheet(String videoId, String? label) async {
    setState(() {
      _chooserVideoId = videoId;
      _chooserLabel = label;
      _chooserSources = null;
      _chooserError = null;
      _chooserLoading = true;
    });
    final seasonEps = _episodes.where((e) => e.season == _selectedSeason).toList();
    try {
      final res = await AppStateScope.read(context).bridge.sources(
            _media,
            videoId: videoId,
            pack: label != null && seasonEps.isNotEmpty ? seasonEps.length : null,
          );
      if (!mounted || _chooserVideoId != videoId) return;
      setState(() {
        _chooserLoading = false;
        _chooserSources = (res['sources'] as List<dynamic>? ?? [])
            .map((e) => SourceItem.fromJson(e as Map<String, dynamic>))
            .toList();
      });
    } catch (e) {
      if (!mounted || _chooserVideoId != videoId) return;
      setState(() {
        _chooserLoading = false;
        _chooserError = e.toString();
      });
    }
  }

  Future<void> _quickDownloadFilm() async {
    if (_quickDownloading) return;
    final app = AppStateScope.read(context);
    setState(() => _quickDownloading = true);
    try {
      final res = await app.bridge.autoplay(_media);
      if (res['offline'] is Map<String, dynamic>) {
        app.showToast('${_media.name} is already downloaded offline.');
        return;
      }
      final bestMap = res['best'] as Map<String, dynamic>?;
      if (bestMap == null) {
        app.showToast(res['message']?.toString() ?? 'No direct source available to download.', isError: true);
        return;
      }
      final src = SourceItem.fromJson(bestMap);
      await app.bridge.startDownload(
        media: _media,
        videoId: _media.id,
        title: _media.name,
        subtitle: [if (src.quality != null) src.quality!, if (src.hdr != null) src.hdr!].join(' '),
        url: src.url,
        headers: src.headers.entries.map((e) => [e.key, e.value]).toList(),
        filename: src.filename.isNotEmpty ? src.filename : null,
      );
      app.showToast('Downloading ${_media.name} — see Library → Downloads.');
    } catch (e) {
      app.showToast('Download failed: $e', isError: true);
    } finally {
      if (mounted) setState(() => _quickDownloading = false);
    }
  }

  EpisodeItem? _upNextEpisode() {
    if (_episodes.isEmpty) return null;
    if (_media.lastVideoId != null && _media.inProgress && !_watchedEps.contains(_media.lastVideoId)) {
      final cur = _episodes.where((e) => e.id == _media.lastVideoId).firstOrNull;
      if (cur != null) return cur;
    }
    final regular = _episodes.where((e) => e.season > 0).toList();
    if (regular.isEmpty) return _episodes.first;
    final lastSeenIdx = regular.lastIndexWhere((e) => _watchedEps.contains(e.id));
    final start = lastSeenIdx >= 0 ? lastSeenIdx + 1 : 0;
    for (int i = start; i < regular.length; i++) {
      if (!_watchedEps.contains(regular[i].id)) return regular[i];
    }
    return regular.first;
  }

  EpisodeItem? _nextAfter(String videoId) {
    final idx = _episodes.indexWhere((e) => e.id == videoId);
    if (idx < 0 || idx + 1 >= _episodes.length) return null;
    final nxt = _episodes[idx + 1];
    return nxt.season == 0 ? null : nxt;
  }

  /// Every trailer/clip of this title, deduplicated (TMDB/ElfHosted videos first, then add-on trailers).
  List<TrailerClip> _allClips() {
    final clips = <TrailerClip>[];
    final seenYt = <String>{};
    final seenUrl = <String>{};

    for (final v in (_art?.videos ?? const [])) {
      final yt = v['youtube']?.toString() ?? v['key']?.toString() ?? v['id']?.toString() ?? '';
      if (yt.isEmpty || !seenYt.add(yt)) continue;
      final kind = v['kind']?.toString() ?? v['type']?.toString() ?? 'Trailer';
      final name = v['name']?.toString() ?? v['title']?.toString() ?? kind;
      clips.add(TrailerClip(
        youtube: yt,
        name: name.isNotEmpty ? name : kind,
        kind: kind.isNotEmpty ? kind : 'Trailer',
        official: v['official'] == true,
      ));
    }

    int trailerNum = clips.where((c) => c.kind == 'Trailer').length;
    void addYt(String yt) {
      if (yt.isEmpty || !seenYt.add(yt)) return;
      trailerNum++;
      clips.add(TrailerClip(
        youtube: yt,
        name: trailerNum == 1 ? 'Trailer' : 'Trailer $trailerNum',
        kind: 'Trailer',
      ));
    }

    for (final yt in (_art?.trailers ?? const <String>[])) {
      addYt(yt);
    }

    final metaTrailers = _meta['trailers'] as List<dynamic>? ?? const [];
    for (final item in metaTrailers) {
      if (item is! Map) continue;
      final yt = item['youtube']?.toString() ?? '';
      final url = item['url']?.toString() ?? '';
      if (yt.isNotEmpty) {
        addYt(yt);
      } else if (url.isNotEmpty && seenUrl.add(url)) {
        trailerNum++;
        clips.add(TrailerClip(
          url: url,
          name: trailerNum == 1 ? 'Trailer' : 'Trailer $trailerNum',
          kind: 'Trailer',
        ));
      }
    }
    return clips;
  }

  TheatreAbout _theatreAbout({
    required String year,
    required String runtime,
    required List<String> genres,
    required String imdb,
    required String? logo,
  }) {
    final facts = <String>[
      if (year.isNotEmpty) year,
      if (runtime.isNotEmpty) runtime,
      if (_art?.certificate?.isNotEmpty == true) _art!.certificate!,
      if (genres.isNotEmpty) genres.take(2).join(', '),
    ];
    String? directorRole;
    var directors = <String>[];
    for (final g in (_art?.crew ?? const [])) {
      final role = g['role']?.toString() ?? '';
      if (role.startsWith('Directed') || role.startsWith('Created')) {
        final people = (g['people'] as List<dynamic>? ?? const [])
            .map((p) => (p as Map?)?['name']?.toString() ?? '')
            .where((n) => n.isNotEmpty)
            .toList();
        if (people.isNotEmpty) {
          directorRole = role;
          directors = people.take(2).toList();
          break;
        }
      }
    }
    if (directors.isEmpty) {
      final metaDirs = (_meta['directors'] as List<dynamic>? ?? const [])
          .map((e) => e.toString())
          .where((s) => s.isNotEmpty)
          .take(2)
          .toList();
      if (metaDirs.isNotEmpty) {
        directorRole = 'Directed by';
        directors = metaDirs;
      }
    }
    final cast = <String>[];
    for (final p in (_art?.cast ?? const [])) {
      final name = p['name']?.toString() ?? '';
      if (name.isNotEmpty) cast.add(name);
      if (cast.length >= 4) break;
    }
    if (cast.isEmpty) {
      for (final c in (_meta['cast'] as List<dynamic>? ?? const [])) {
        final name = c.toString();
        if (name.isNotEmpty) cast.add(name);
        if (cast.length >= 4) break;
      }
    }
    return TheatreAbout(
      title: _media.name,
      backdrop: (_art?.backdrop?.isNotEmpty == true) ? _art!.backdrop! : _media.backdrop,
      logo: logo,
      facts: facts,
      rating: imdb.isNotEmpty ? imdb : null,
      tagline: _art?.tagline,
      directorRole: directorRole,
      directors: directors,
      cast: cast,
    );
  }

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final size = MediaQuery.sizeOf(context);
    final gutter = VeyloLayout.gutter(size.width);
    final insets = MediaQuery.viewPaddingOf(context);
    final horizontalInsets = insets.copyWith(top: 0, bottom: 0);
    final posterW = VeyloLayout.posterWidth(size.width);
    final film = _media.kind == 'movie';

    // Rotating clean backdrops
    final frames = {
      if (_art?.backdrop?.isNotEmpty == true) _art!.backdrop!,
      ...(_art?.stills ?? []),
      if (_art?.backdrop == null && _media.background.isNotEmpty) _media.background,
    }.where((u) => u.isNotEmpty).toList();

    final rawLogo = (_art?.logo?.isNotEmpty == true)
        ? _art!.logo!
        : ((_meta['logo'] as String?)?.isNotEmpty == true ? _meta['logo'] as String : _media.logo);
    final imdb = (_meta['rating'] as String?)?.isNotEmpty == true ? _meta['rating'] as String : _media.rating;
    final runtime = (_meta['runtime'] as String?)?.isNotEmpty == true ? _meta['runtime'] as String : _media.runtime;
    final released = _meta['released']?.toString() ?? '';
    final year = _media.year.isNotEmpty
        ? _media.year
        : (RegExp(r'^\d{4}').firstMatch(released)?.group(0) ?? '');
    final metaGenres = (_meta['genres'] as List<dynamic>? ?? []).map((e) => e.toString()).toList();
    final genres = _media.genres.isNotEmpty
        ? _media.genres
        : metaGenres;
    // The title's own metadata first: identical in the disk copy and the live answer, while the
    // card/library entry's description can differ in length and re-wrap the text (a layout shift
    // the runtime CI caught on a phone-width screen).
    final metaSynopsis = _meta['description'] as String? ?? '';
    final synopsis = metaSynopsis.isNotEmpty ? metaSynopsis : _media.description;
    if (_bodyReady && !_revealed && !_revealing) {
      _revealing = true;
      final snap = <String, Object?>{
        'frames': frames,
        'logo': (rawLogo.isNotEmpty && !_failedLogos.contains(rawLogo)) ? rawLogo : null,
        'tagline': _art?.tagline,
        'imdb': imdb,
        'runtime': runtime,
        'year': year,
        'genres': genres,
        'tomatometer': _tomatometer,
        'metascore': _metascore,
        'rank': _rank,
        'certificate': _art?.certificate,
        'status': _seriesStatus,
        'trailers': _allClips().isNotEmpty,
      };
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) unawaited(_prepareReveal(snap));
      });
    }
    final f = _frozen;
    final heroFrames = _revealed ? (f['frames'] as List<String>) : const <String>[];
    final shownLogo = _revealed ? f['logo'] as String? : null;
    final logo = shownLogo != null && !_failedLogos.contains(shownLogo) ? shownLogo : null;
    final tagline = _revealed ? f['tagline'] as String? : null;
    final factImdb = _revealed ? f['imdb'] as String : imdb;
    final factRuntime = _revealed ? f['runtime'] as String : runtime;
    final factYear = _revealed ? f['year'] as String : year;
    final factGenres = _revealed ? f['genres'] as List<String> : genres;
    // Scores stay live: their pill is last in the row, so filling in moves nothing.
    final factTomato = _tomatometer;
    final factMeta = _metascore;
    final factRank = _revealed ? f['rank'] as int? : _rank;
    final factCert = _revealed ? f['certificate'] as String? : _art?.certificate;
    final factStatus = _revealed ? f['status'] as String? : _seriesStatus;
    final upNext = film ? null : _upNextEpisode();
    final seasons = _episodes.map((e) => e.season).toSet().toList()..sort();
    final currentSeasonEps = _episodes.where((e) => e.season == _selectedSeason).toList();
    final regularEps = _episodes.where((e) => e.season > 0).toList();
    final allSeriesWatched = regularEps.isNotEmpty && regularEps.every((e) => _watchedEps.contains(e.id));
    final allClips = _allClips();

    return Stack(
      fit: StackFit.expand,
      children: [
        // The left reading scrim spans the whole page, not just the hero: ending it at the hero's
        // bottom left a hard horizontal line where the artwork met the ambient blur.
        const Positioned.fill(
          child: IgnorePointer(
            child: DecoratedBox(
              decoration: BoxDecoration(
                gradient: LinearGradient(
                  begin: Alignment.centerLeft,
                  end: Alignment.centerRight,
                  stops: [0.0, 0.50, 0.85],
                  colors: [
                    Color.fromRGBO(0, 0, 0, 0.80),
                    Color.fromRGBO(0, 0, 0, 0.35),
                    Color.fromRGBO(0, 0, 0, 0.0),
                  ],
                ),
              ),
            ),
          ),
        ),
        SingleChildScrollView(
          controller: _scroll,
          padding: EdgeInsets.only(bottom: 72 + insets.bottom),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              // Full-bleed hero (.title-hero) with zero-rebuild parallax + KenBurns settle
              SizedBox(
                height: (size.height * 0.68).clamp(440.0, 680.0) + insets.top,
                width: double.infinity,
                child: Stack(
                  fit: StackFit.expand,
                  children: [
                    RepaintBoundary(
                      child: ShaderMask(
                        shaderCallback: (r) => const LinearGradient(
                          begin: Alignment.topCenter,
                          end: Alignment.bottomCenter,
                          stops: [0.0, 0.45, 0.72, 0.90, 1.0],
                          colors: [
                            Color(0xFFFFFFFF),
                            Color(0xFFFFFFFF),
                            Color(0x99FFFFFF),
                            Color(0x26FFFFFF),
                            Color(0x00FFFFFF),
                          ],
                        ).createShader(r),
                        blendMode: BlendMode.dstIn,
                        // The reading scrim rides inside the same fade as the artwork, so at the
                        // hero's bottom both dissolve into the page-wide scrim underneath.
                        child: Stack(
                          fit: StackFit.expand,
                          children: [
                            ClipRect(
                          child: ValueListenableBuilder<double>(
                            valueListenable: _heroScrollY,
                            builder: (_, scrollY, child) => Transform.translate(
                              offset: Offset(0, scrollY * 0.28),
                              child: child,
                            ),
                            child: ValueListenableBuilder<int>(
                              valueListenable: _frameIndexN,
                              builder: (_, idx, _) {
                                final activeBg = heroFrames.isNotEmpty ? heroFrames[idx % heroFrames.length] : '';
                                return AnimatedSwitcher(
                                  duration: const Duration(milliseconds: 900),
                                  child: activeBg.isNotEmpty
                                      ? KenBurnsImage(
                                          key: ValueKey(activeBg),
                                          url: activeBg,
                                          cacheWidth: heroDecodeWidth(size.width, MediaQuery.devicePixelRatioOf(context)),
                                          alignment: const Alignment(0, -0.4),
                                          settle: false,
                                          fadeIn: Duration.zero,
                                        )
                                      : const SizedBox.shrink(),
                                );
                              },
                            ),
                          ),
                        ),
                            const IgnorePointer(
                              child: DecoratedBox(
                                decoration: BoxDecoration(
                                  gradient: LinearGradient(
                                    begin: Alignment.centerLeft,
                                    end: Alignment.centerRight,
                                    stops: [0.0, 0.50, 0.85],
                                    colors: [
                                      Color.fromRGBO(0, 0, 0, 0.55),
                                      Color.fromRGBO(0, 0, 0, 0.20),
                                      Color.fromRGBO(0, 0, 0, 0.0),
                                    ],
                                  ),
                                ),
                              ),
                            ),
                          ],
                        ),
                      ),
                    ),
                    Padding(
                      padding: EdgeInsets.fromLTRB(gutter, 96 + insets.top, gutter, 12) + horizontalInsets,
                      child: Align(
                        alignment: Alignment.bottomLeft,
                        child: ConstrainedBox(
                          constraints: const BoxConstraints(maxWidth: 780),
                          // Revealed once, in place (opacity only), when everything below is ready.
                          child: IgnorePointer(
                            ignoring: !_revealed,
                            child: AnimatedOpacity(
                            opacity: _revealed ? 1 : 0,
                            duration: const Duration(milliseconds: 220),
                            child: Column(
                            mainAxisSize: MainAxisSize.min,
                            crossAxisAlignment: CrossAxisAlignment.start,
                            children: [
                              // Fixed-height, bottom-anchored logo slot: swapping the wordmark for the
                              // logo (or the logo image decoding late) never moves anything below it.
                              KeyedSubtree(
                                key: const ValueKey('shift.title.logo'),
                                child: SizedBox(
                                  height: 144,
                                  width: 460,
                                  child: Builder(
                                    builder: (_) => logo != null
                                        ? CachedNetworkImage(
                                            key: ValueKey(logo),
                                            imageUrl: logo,
                                            fit: BoxFit.contain,
                                            alignment: Alignment.bottomLeft,
                                            memCacheWidth: 920,
                                            fadeInDuration: Duration.zero,
                                            placeholder: (_, _) => const SizedBox.expand(),
                                            errorWidget: (_, _, _) {
                                              WidgetsBinding.instance.addPostFrameCallback((_) {
                                                if (mounted) setState(() => _failedLogos.add(logo));
                                              });
                                              return Align(
                                                alignment: Alignment.bottomLeft,
                                                child: _titleHeading(_media.name, size.width),
                                              );
                                            },
                                          )
                                        : (!_revealed
                                            ? const SizedBox.expand(key: ValueKey('logo-pending'))
                                            : Align(
                                                key: const ValueKey('logo-text'),
                                                alignment: Alignment.bottomLeft,
                                                child: _titleHeading(_media.name, size.width),
                                              )),
                                  ),
                                ),
                              ),
                              // Frozen at the reveal with the logo: never appears or grows later.
                              SizedBox(
                                width: double.infinity,
                                child: tagline?.isNotEmpty == true
                                    ? Padding(
                                        padding: const EdgeInsets.only(top: 12),
                                        child: Text(
                                          tagline!,
                                          maxLines: 2,
                                          overflow: TextOverflow.ellipsis,
                                          style: VeyloText.ui(
                                            18,
                                            weight: 500,
                                            fontStyle: FontStyle.italic,
                                            color: const Color.fromRGBO(242, 239, 234, 0.86),
                                          ),
                                        ),
                                      )
                                    : const SizedBox.shrink(),
                              ),
                              const SizedBox(height: 18),
                              KeyedSubtree(
                                key: const ValueKey('shift.title.pills'),
                                // One line, never wrapping: late facts (scores, rank, certificate,
                                // series status) are appended at the end, so when they land nothing
                                // moves — a wrapped second line used to push the logo up.
                                child: SizedBox(
                                  width: double.infinity,
                                  child: SingleChildScrollView(
                                  scrollDirection: Axis.horizontal,
                                  clipBehavior: Clip.none,
                                  child: Row(
                                  mainAxisSize: MainAxisSize.min,
                                  spacing: 8,
                                  children: [
                                    if (factRuntime.isNotEmpty)
                                      FactPill(
                                        child: Row(
                                          mainAxisSize: MainAxisSize.min,
                                          children: [
                                            const VeyloIcon('Clock', size: 16, color: VeyloColors.dust),
                                            const SizedBox(width: 6),
                                            Text(factRuntime),
                                          ],
                                        ),
                                      ),
                                    if (factYear.isNotEmpty)
                                      FactPill(
                                        child: Row(
                                          mainAxisSize: MainAxisSize.min,
                                          children: [
                                            const VeyloIcon('Calendar', size: 16, color: VeyloColors.dust),
                                            const SizedBox(width: 6),
                                            Text(factYear, style: const TextStyle(fontFeatures: VeyloText.tnum)),
                                          ],
                                        ),
                                      ),
                                    FactPill(
                                      child: Row(
                                        mainAxisSize: MainAxisSize.min,
                                        children: [
                                          VeyloIcon(film ? 'Film' : 'Tv', size: 16, color: VeyloColors.dust),
                                          const SizedBox(width: 6),
                                          Text(film ? 'Film' : 'Series'),
                                        ],
                                      ),
                                    ),
                                    if (factGenres.isNotEmpty)
                                      FactPill(
                                        child: Row(
                                          mainAxisSize: MainAxisSize.min,
                                          children: [
                                            const VeyloIcon('Tag', size: 16, color: VeyloColors.dust),
                                            const SizedBox(width: 6),
                                            Text(factGenres.take(3).join(', ')),
                                          ],
                                        ),
                                      ),
                                    if (factRank != null)
                                      FactPill(
                                        child: Row(
                                          mainAxisSize: MainAxisSize.min,
                                          children: [
                                            const VeyloIcon('Trophy', size: 16, color: VeyloColors.bone),
                                            const SizedBox(width: 6),
                                            Text('#$factRank today', style: const TextStyle(fontFeatures: VeyloText.tnum)),
                                          ],
                                        ),
                                      ),
                                    if (factCert?.isNotEmpty == true)
                                      FactPill(
                                        child: Text(
                                          factCert!,
                                          style: VeyloText.ui(13.5, weight: 800),
                                        ),
                                      ),
                                    if (factStatus != null)
                                      FactPill(child: Text(factStatus)),
                                    // Last on purpose: the only fact allowed to fill in after the reveal
                                    // (Rotten Tomatoes / Metacritic can land late), growing to the right
                                    // without moving anything.
                                    ScoresPill(imdb: factImdb, tomatometer: factTomato, metascore: factMeta),
                                  ],
                                ),
                                ),
                                ),
                              ),
                              const SizedBox(height: 22),
                              // Action bar (.title-actions)
                              KeyedSubtree(
                                key: const ValueKey('shift.title.actions'),
                                // One line too: the trailer button landing late used to wrap the bar
                                // onto a second line and lift the whole hero by 58 px.
                                child: SingleChildScrollView(
                                  scrollDirection: Axis.horizontal,
                                  clipBehavior: Clip.none,
                                  child: Row(
                                  mainAxisSize: MainAxisSize.min,
                                  spacing: 10,
                                  children: [
                                    // Series: room for "Play S1E1" from the start, so the label filling in
                                    // when episodes land doesn't widen the bar and wrap it.
                                    ConstrainedBox(
                                      constraints: BoxConstraints(minWidth: film ? 0 : 196),
                                      child: VeyloPill(
                                      label: film
                                          ? (_media.inProgress ? 'Resume' : 'Play')
                                          : (upNext != null
                                              ? '${(_media.lastVideoId == upNext.id && _media.inProgress) ? 'Resume' : 'Play'} ${upNext.label}'
                                              : (_loading ? 'Loading…' : 'Play')),
                                      icon: 'Play',
                                      accent: true,
                                      big: true,
                                      disabled: !film && _loading && upNext == null,
                                      onTap: () {
                                        if (film) {
                                          unawaited(app.autoplayMedia(_media));
                                        } else if (upNext != null) {
                                          final seps = _episodes.where((e) => e.season == upNext.season).toList();
                                          unawaited(app.autoplayMedia(
                                            _media,
                                            videoId: upNext.id,
                                            episodeLabel: upNext.label,
                                            pack: seps.length,
                                            seasonEpisodes: seps,
                                            nextEpisode: _nextAfter(upNext.id),
                                          ));
                                        } else {
                                          unawaited(app.autoplayMedia(_media));
                                        }
                                      },
                                    ),
                                    ),
                                    VeyloOrb(
                                      icon: 'Bookmark',
                                      active: _saved,
                                      tooltip: _saved ? 'Saved to Library (S)' : 'Save to Library (S)',
                                      onTap: () async {
                                        setState(() {
                                          _localEdit = true;
                                          _saved = !_saved;
                                        });
                                        await app.bridge.setSaved(_media, _saved);
                                        app.bumpPicksDirty();
                                      },
                                    ),
                                    VeyloOrb(
                                      icon: 'ThumbUp',
                                      active: _liked > 0,
                                      tooltip: _liked > 0 ? 'Liked' : 'Like',
                                      onTap: () async {
                                        final next = _liked == 1 ? 0 : 1;
                                        setState(() {
                                          _localEdit = true;
                                          _liked = next;
                                        });
                                        await app.bridge.feedback(_media, next);
                                        app.bumpPicksDirty();
                                      },
                                    ),
                                    VeyloOrb(
                                      icon: 'ThumbDown',
                                      active: _liked < 0,
                                      tooltip: 'Not for me',
                                      onTap: () async {
                                        final next = _liked == -1 ? 0 : -1;
                                        setState(() {
                                          _localEdit = true;
                                          _liked = next;
                                        });
                                        await app.bridge.feedback(_media, next);
                                        app.bumpPicksDirty();
                                      },
                                    ),
                                    if (film) ...[
                                      VeyloPill(
                                        label: 'Already watched',
                                        icon: _watched ? 'Check' : 'EyeOff',
                                        active: _watched,
                                        onTap: () async {
                                          setState(() {
                                            _localEdit = true;
                                            _watched = !_watched;
                                          });
                                          await app.bridge.setWatched(_media, _watched);
                                          app.bumpPicksDirty();
                                        },
                                      ),
                                      VeyloOrb(
                                        icon: 'Download',
                                        disabled: _quickDownloading,
                                        tooltip: 'Download best source',
                                        onTap: _quickDownloadFilm,
                                      ),
                                      VeyloOrb(
                                        icon: 'Layers',
                                        tooltip: 'Choose a source (O)',
                                        onTap: () => _openSourceSheet(_media.id, null),
                                      ),
                                    ] else if (regularEps.isNotEmpty) ...[
                                      VeyloPill(
                                        label: allSeriesWatched ? 'Series watched' : 'Mark series watched',
                                        icon: allSeriesWatched ? 'Check' : 'EyeOff',
                                        active: allSeriesWatched,
                                        onTap: () async {
                                          final next = !allSeriesWatched;
                                          final ids = regularEps.map((e) => e.id).toList();
                                          setState(() {
                                            if (next) {
                                              _watchedEps.addAll(ids);
                                            } else {
                                              _watchedEps.removeAll(ids);
                                            }
                                          });
                                          await app.bridge.setEpisodesWatched(_media, ids, next);
                                          app.bumpPicksDirty();
                                        },
                                      ),
                                    ],
                                    if (_revealed ? f['trailers'] == true : allClips.isNotEmpty)
                                      VeyloOrb(
                                        icon: 'Clapper',
                                        tooltip: 'Trailer and videos',
                                        onTap: () => app.openTheatre(TheatreSession(
                                          about: _theatreAbout(
                                            year: year,
                                            runtime: runtime,
                                            genres: genres,
                                            imdb: imdb,
                                            logo: logo,
                                          ),
                                          clips: allClips,
                                          initialIndex: 0,
                                        )),
                                      ),
                                  ],
                                ),
                                ),
                              ),
                            ],
                          ),
                          ),
                          ),
                        ),
                      ),
                    ),
                  ],
                ),
              ),

              // Body (.title-body): built once the page has its details (disk cache or live,
              // ≤700 ms), so the synopsis landing late can't push the studios/trailers down —
              // it fades in already laid out at its final size.
              Padding(
                padding: EdgeInsets.fromLTRB(gutter, 12, gutter, 0) + horizontalInsets,
                child: ConstrainedBox(
                  constraints: const BoxConstraints(maxWidth: 1180),
                  child: !_revealed
                      ? const SizedBox(height: 320, width: double.infinity)
                      : TweenAnimationBuilder<double>(
                          tween: Tween(begin: 0, end: 1),
                          duration: const Duration(milliseconds: 260),
                          curve: Curves.easeOutCubic,
                          builder: (_, v, child) => Opacity(opacity: v, child: child),
                          child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      if (synopsis.isNotEmpty)
                        ConstrainedBox(
                          key: const ValueKey('shift.title.synopsis'),
                          constraints: const BoxConstraints(maxWidth: 760),
                          child: Text(
                            synopsis,
                            style: VeyloText.ui(
                              17,
                              weight: 400,
                              height: 1.55,
                              color: VeyloColors.titleSynopsis,
                            ),
                          ),
                        ),

                      // Studio idents arrive last (Wikidata + a logo check): the row's space is
                      // held while they load and every logo has a fixed box, so nothing below
                      // moves when they appear; without studios the row closes smoothly.
                      AnimatedSize(
                        key: const ValueKey('shift.title.idents'),
                        duration: const Duration(milliseconds: 260),
                        curve: Curves.easeOutCubic,
                        alignment: Alignment.topLeft,
                        // Reserved at the reveal while still loading, and kept for the visit even
                        // if no logos come: collapsing it later would move everything below.
                        child: ((_revealed && _identsHold) || _identsPending || _idents.isNotEmpty)
                            ? Padding(
                                padding: const EdgeInsets.only(top: 22),
                                child: SizedBox(
                                  height: 24,
                                  width: double.infinity,
                                  child: AnimatedSwitcher(
                                    duration: const Duration(milliseconds: 320),
                                    layoutBuilder: (current, previous) => Stack(
                                      alignment: Alignment.centerLeft,
                                      children: [...previous, ?current],
                                    ),
                                    child: Row(
                                      key: ValueKey(_idents.length),
                                      mainAxisSize: MainAxisSize.min,
                                      children: [
                                        for (final (i, st) in _idents.take(6).indexed) ...[
                                          if (i > 0) const SizedBox(width: 28),
                                          VeyloTooltip(
                                            message: st['name']?.toString() ?? '',
                                            // Opacity baked into the filter colour: an Opacity over a
                                            // ColorFiltered is what Impeller rejected on macOS/Android.
                                            child: ColorFiltered(
                                              colorFilter: ColorFilter.mode(
                                                VeyloColors.bone.withValues(alpha: 0.72),
                                                BlendMode.srcIn,
                                              ),
                                              child: CachedNetworkImage(
                                                imageUrl: st['logo']?.toString() ?? '',
                                                width: 104,
                                                height: 24,
                                                fit: BoxFit.contain,
                                                alignment: Alignment.centerLeft,
                                                memCacheHeight: 48,
                                                fadeInDuration: const Duration(milliseconds: 200),
                                                placeholder: (_, _) => const SizedBox(width: 104, height: 24),
                                                errorWidget: (_, _, _) => const SizedBox(width: 104, height: 24),
                                              ),
                                            ),
                                          ),
                                        ],
                                      ],
                                    ),
                                  ),
                                ),
                              )
                            : const SizedBox(width: double.infinity),
                      ),

                      if (_metaError != null) ...[
                        const SizedBox(height: 20),
                        GlassBox(
                          borderRadius: 20,
                          padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
                          child: Row(
                            mainAxisSize: MainAxisSize.min,
                            children: [
                              const VeyloIcon('Alert', size: 18, color: VeyloColors.amber),
                              const SizedBox(width: 10),
                              Text(_metaError!, style: VeyloText.meta),
                            ],
                          ),
                        ),
                      ],

                      // Seasons & Episodes
                      if (!film && _episodes.isNotEmpty) ...[
                        const SizedBox(height: 28),
                        SizedBox(
                          height: 196,
                          child: ListView.separated(
                            scrollDirection: Axis.horizontal,
                            itemCount: seasons.length,
                            separatorBuilder: (_, _) => const SizedBox(width: 14),
                            itemBuilder: (_, i) {
                              final s = seasons[i];
                              final seps = _episodes.where((e) => e.season == s).toList();
                              final seenCount = seps.where((e) => _watchedEps.contains(e.id)).length;
                              final poster = _art?.seasonPosters[s];
                              final active = s == _selectedSeason;
                              return _SeasonCard(
                                season: s,
                                poster: poster,
                                count: seps.length,
                                seenCount: seenCount,
                                active: active,
                                onTap: () => setState(() => _selectedSeason = s),
                              );
                            },
                          ),
                        ),
                        const SizedBox(height: 12),
                        LayoutBuilder(
                          builder: (ctx, c) {
                            final cols = (c.maxWidth / 280).floor().clamp(1, 4);
                            return GridView.builder(
                              shrinkWrap: true,
                              physics: const NeverScrollableScrollPhysics(),
                              gridDelegate: SliverGridDelegateWithFixedCrossAxisCount(
                                crossAxisCount: cols,
                                crossAxisSpacing: 18,
                                mainAxisSpacing: 18,
                                childAspectRatio: 0.92,
                              ),
                              itemCount: currentSeasonEps.length,
                              itemBuilder: (_, i) {
                                final ep = currentSeasonEps[i];
                                final isNext = upNext?.id == ep.id;
                                final isSeen = _watchedEps.contains(ep.id);
                                final prog = _epProgress[ep.id];
                                final pos = (prog?['position_ms'] as num?)?.toDouble() ?? 0;
                                final dur = (prog?['duration_ms'] as num?)?.toDouble() ?? 0;
                                final frac = dur > 0 ? (pos / dur).clamp(0.0, 1.0) : 0.0;
                                return _EpisodeTile(
                                  episode: ep,
                                  isNext: isNext,
                                  isSeen: isSeen,
                                  progress: frac,
                                  onPlay: () => app.autoplayMedia(
                                    _media,
                                    videoId: ep.id,
                                    episodeLabel: ep.label,
                                    pack: currentSeasonEps.length,
                                    seasonEpisodes: currentSeasonEps,
                                    nextEpisode: _nextAfter(ep.id),
                                  ),
                                  onSources: () => _openSourceSheet(ep.id, ep.label),
                                  onToggleSeen: () async {
                                    final nextSeen = !isSeen;
                                    setState(() {
                                      if (nextSeen) {
                                        _watchedEps.add(ep.id);
                                      } else {
                                        _watchedEps.remove(ep.id);
                                      }
                                    });
                                    await app.bridge.setEpisodesWatched(_media, [ep.id], nextSeen);
                                    app.bumpPicksDirty();
                                  },
                                );
                              },
                            );
                          },
                        ),
                      ] else if (!film && _loading) ...[
                        const SizedBox(height: 28),
                        const SkeletonBox(height: 56, borderRadius: 16),
                        const SizedBox(height: 10),
                        const SkeletonBox(height: 56, borderRadius: 16),
                      ],

                      // Videos & Trailers strip (.videos-block)
                      if (_art != null && _art!.videos.isNotEmpty) ...[
                        const SizedBox(height: 32),
                        GroupHead(
                          key: const ValueKey('shift.title.trailers'),
                          icon: 'Film',
                          title: 'Trailers & Clips',
                          count: _art!.videos.length,
                        ),
                        const SizedBox(height: 12),
                        SizedBox(
                          height: 196,
                          child: ListView.separated(
                            scrollDirection: Axis.horizontal,
                            itemCount: _art!.videos.take(12).length,
                            separatorBuilder: (_, _) => const SizedBox(width: 16),
                            itemBuilder: (_, i) {
                              final v = _art!.videos[i];
                              final ytKey = v['youtube']?.toString() ?? v['key']?.toString() ?? '';
                              final vTitle = v['name']?.toString() ?? v['title']?.toString() ?? 'Video';
                              final vKind = v['kind']?.toString() ?? v['type']?.toString() ?? '';
                              return _VideoCard(
                                ytKey: ytKey,
                                title: vTitle,
                                kind: vKind,
                                onTap: () {
                                  if (ytKey.isNotEmpty) {
                                    final idx = allClips.indexWhere((c) => c.youtube == ytKey);
                                    app.openTheatre(TheatreSession(
                                      about: _theatreAbout(
                                        year: year,
                                        runtime: runtime,
                                        genres: genres,
                                        imdb: imdb,
                                        logo: logo,
                                      ),
                                      clips: allClips.isNotEmpty
                                          ? allClips
                                          : [TrailerClip(youtube: ytKey, name: vTitle, kind: vKind)],
                                      initialIndex: idx >= 0 ? idx : 0,
                                    ));
                                  }
                                },
                              );
                            },
                          ),
                        ),
                      ],

                      // Cast (.people-block)
                      if (_castFor().isNotEmpty) ...[
                        const SizedBox(height: 32),
                        const GroupHead(key: ValueKey('shift.title.cast'), icon: 'Users', title: 'Cast'),
                        const SizedBox(height: 12),
                        SizedBox(
                          height: 242,
                          child: ListView.separated(
                            scrollDirection: Axis.horizontal,
                            itemCount: _castFor().take(18).length,
                            separatorBuilder: (_, _) => const SizedBox(width: 16),
                            itemBuilder: (_, i) {
                              final p = _castFor()[i];
                              final name = p['name']?.toString() ?? '';
                              final role = p['role']?.toString() ?? '';
                              final photo = p['photo']?.toString() ?? '';
                              return _CastCard(
                                name: name,
                                role: role,
                                photo: photo,
                                onTap: () => app.openSearch(query: name),
                              );
                            },
                          ),
                        ),
                      ],

                      // More like this
                      if (_similar.isNotEmpty) ...[
                        const SizedBox(height: 28),
                        const GroupHead(key: ValueKey('shift.title.similar'), icon: 'Sparkles', title: 'More like this'),
                        const SizedBox(height: 8),
                        SizedBox(
                          height: posterW * 1.5 + 28,
                          child: ListView.separated(
                            scrollDirection: Axis.horizontal,
                            itemCount: _similar.length,
                            separatorBuilder: (_, _) => const SizedBox(width: 16),
                            itemBuilder: (_, i) => PosterCard(media: _similar[i], width: posterW),
                          ),
                        ),
                      ],
                      // Order = arrival order (last to land goes lowest, so it moves nothing):
                      // similar (~1 s), crew (Wikidata, 1–3 s), then the saga (up to 10+ s).
                      // Crew credits (.crew-grid)
                      if (_crewFor().isNotEmpty) ...[
                        const SizedBox(height: 28),
                        _CrewGrid(
                          crew: _crewFor(),
                          onSelectPerson: (name) => app.openSearch(query: name),
                        ),
                      ],

                      // Collection shelf
                      // Waits for the crew above it (its height is only known then).
                      if (_collection != null && _extrasDone) ...[
                        const SizedBox(height: 28),
                        GroupHead(
                          key: const ValueKey('shift.title.collection'),
                          icon: 'Stack',
                          title: _collection!['name']?.toString() ?? 'Collection',
                          count: (_collection!['parts'] as List<dynamic>? ?? []).length,
                        ),
                        const SizedBox(height: 8),
                        SizedBox(
                          height: posterW * 1.5 + 28,
                          child: ListView.separated(
                            scrollDirection: Axis.horizontal,
                            itemCount: (_collection!['parts'] as List<dynamic>? ?? []).length,
                            separatorBuilder: (_, _) => const SizedBox(width: 16),
                            itemBuilder: (_, i) {
                              final item = MediaItem.fromJson((_collection!['parts'] as List<dynamic>)[i] as Map<String, dynamic>);
                              return PosterCard(media: item, width: posterW);
                            },
                          ),
                        ),
                      ],

                    ],
                  ),
                        ),
                ),
              ),
            ],
          ),
        ),

        // Source picker bottom sheet (.bottom-sheet)
        if (_chooserVideoId != null)
          _SourceBottomSheet(
            title: _chooserLabel != null ? '${_media.name} · $_chooserLabel' : _media.name,
            genres: _media.genres,
            mediaKind: _media.kind,
            loading: _chooserLoading,
            error: _chooserError,
            sources: _chooserSources ?? [],
            onClose: () => setState(() => _chooserVideoId = null),
            onPlay: (src) {
              final vid = _chooserVideoId!;
              final lbl = _chooserLabel;
              final all = _chooserSources ?? [src];
              setState(() => _chooserVideoId = null);
              app.startPlaySession(PlaySession(
                title: lbl ?? _media.name,
                subtitle: vid != _media.id ? _media.name : _media.metaLineTyped,
                url: src.url,
                headers: src.headers,
                size: src.size,
                hdr: src.hdr,
                quality: src.quality,
                filename: src.filename.isNotEmpty ? src.filename : null,
                fileSize: src.sizeBytes,
                media: _media,
                videoId: vid != _media.id ? vid : null,
                sources: all,
                episodes: currentSeasonEps,
                nextEpisode: _nextAfter(vid),
                originalLanguage: (_meta['language']?.toString().isNotEmpty == true)
                    ? _meta['language'].toString()
                    : null,
              ));
            },
            onDownload: (src) async {
              final vid = _chooserVideoId!;
              final lbl = _chooserLabel;
              final t = lbl != null ? '${_media.name} $lbl' : _media.name;
              setState(() => _chooserVideoId = null);
              try {
                await app.bridge.startDownload(
                  media: _media,
                  videoId: vid,
                  title: t,
                  subtitle: [if (src.quality != null) src.quality!, if (src.hdr != null) src.hdr!].join(' '),
                  url: src.url,
                  headers: src.headers.entries.map((e) => [e.key, e.value]).toList(),
                  filename: src.filename.isNotEmpty ? src.filename : null,
                );
                app.showToast('Downloading $t — see Library → Downloads.');
              } catch (e) {
                app.showToast('Download failed: $e', isError: true);
              }
            },
          ),
      ],
    );
  }

  /// TMDB/TVmaze/Wikidata crew when present, else the addon meta's directors & writers.
  List<Map<String, dynamic>>? _crewShown;

  /// The crew grid shown in this visit never changes once visible: Wikidata's credits vary a
  /// little from one answer to the next (its result limit), which grew or shrank the grid and
  /// moved the saga below it. The fresher list shows next visit.
  List<Map<String, dynamic>> _crewFor() {
    final shown = _crewShown;
    if (shown != null) return shown;
    final crew = _crewCandidate();
    if (crew.isNotEmpty) _crewShown = crew;
    return crew;
  }

  List<Map<String, dynamic>> _crewCandidate() {
    // Shown once its final source answered (Wikidata credits, or the add-on's directors and
    // writers when that failed), so the grid doesn't grow a row later.
    if (!_extrasDone) return const [];
    if (_art != null && _art!.crew.isNotEmpty) return _art!.crew;
    List<String> names(String k) => (_meta[k] as List<dynamic>? ?? []).map((e) => e.toString()).toList();
    final directors = names('directors');
    final writers = names('writers');
    return [
      if (directors.isNotEmpty) {'job': directors.length > 1 ? 'Directors' : 'Director', 'people': directors},
      if (writers.isNotEmpty) {'job': writers.length > 1 ? 'Writers' : 'Writer', 'people': writers},
    ];
  }

  /// Rich cast (photos, roles) when present, else the addon meta's billing names.
  List<Map<String, dynamic>> _castFor() {
    if (_art != null && _art!.cast.isNotEmpty) return _art!.cast;
    return (_meta['cast'] as List<dynamic>? ?? [])
        .map((e) => <String, dynamic>{'name': e.toString(), 'role': '', 'photo': ''})
        .toList();
  }

  Widget _titleHeading(String name, double width) => Text(
        name,
        maxLines: 2,
        overflow: TextOverflow.ellipsis,
        style: VeyloText.display(
          (width * 0.056).clamp(42.0, 74.0),
          weight: 800,
          height: 0.95,
          letterSpacing: -1.4,
        ),
      );
}

class _VideoCard extends StatefulWidget {
  const _VideoCard({required this.ytKey, required this.title, required this.kind, required this.onTap});
  final String ytKey;
  final String title;
  final String kind;
  final VoidCallback onTap;

  @override
  State<_VideoCard> createState() => _VideoCardState();
}

class _VideoCardState extends State<_VideoCard> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final thumb = widget.ytKey.isNotEmpty ? 'https://i.ytimg.com/vi/${widget.ytKey}/hqdefault.jpg' : '';
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      child: GestureDetector(
        onTap: widget.onTap,
        child: SizedBox(
          width: 260,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              AnimatedContainer(
                duration: const Duration(milliseconds: 220),
                curve: VeyloCurves.out,
                transform: Matrix4.identity()..translateByDouble(0.0, _hovered ? -3.0 : 0.0, 0.0, 1.0),
                decoration: BoxDecoration(
                  borderRadius: BorderRadius.circular(14),
                  border: Border.all(color: _hovered ? VeyloColors.sheen : VeyloColors.edge),
                  boxShadow: [
                    BoxShadow(
                      color: Color.fromRGBO(0, 0, 0, _hovered ? 0.65 : 0.35),
                      blurRadius: _hovered ? 28 : 16,
                      offset: Offset(0, _hovered ? 12 : 6),
                    ),
                  ],
                ),
                child: AspectRatio(
                  aspectRatio: 16 / 9,
                  child: ClipRRect(
                    borderRadius: BorderRadius.circular(13),
                    child: Stack(
                      fit: StackFit.expand,
                      children: [
                        FallbackImage(
                          sources: [if (thumb.isNotEmpty) thumb],
                          cacheWidth: 520,
                          fallback: const ColoredBox(color: Color(0xFF14151A)),
                        ),
                        const DecoratedBox(
                          decoration: BoxDecoration(
                            gradient: LinearGradient(
                              begin: Alignment.topCenter,
                              end: Alignment.bottomCenter,
                              colors: [Color.fromRGBO(0, 0, 0, 0.15), Color.fromRGBO(0, 0, 0, 0.55)],
                            ),
                          ),
                        ),
                        Center(
                          child: AnimatedScale(
                            scale: _hovered ? 1.08 : 1.0,
                            duration: const Duration(milliseconds: 180),
                            child: Container(
                              width: 46,
                              height: 46,
                              decoration: BoxDecoration(
                                shape: BoxShape.circle,
                                color: _hovered ? VeyloColors.bone : const Color.fromRGBO(12, 13, 18, 0.78),
                                border: Border.all(color: VeyloColors.edge),
                              ),
                              alignment: Alignment.center,
                              child: VeyloIcon('Play', size: 20, color: _hovered ? const Color(0xFF000000) : VeyloColors.bone, filled: true),
                            ),
                          ),
                        ),
                        if (widget.kind.isNotEmpty)
                          Positioned(
                            top: 8,
                            left: 8,
                            child: Container(
                              padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 3),
                              decoration: BoxDecoration(
                                color: const Color.fromRGBO(10, 11, 16, 0.78),
                                borderRadius: BorderRadius.circular(10),
                                border: Border.all(color: VeyloColors.edge),
                              ),
                              child: Text(widget.kind, style: VeyloText.ui(11, weight: 700, color: VeyloColors.bone)),
                            ),
                          ),
                      ],
                    ),
                  ),
                ),
              ),
              const SizedBox(height: 8),
              Text(widget.title, maxLines: 1, overflow: TextOverflow.ellipsis, style: VeyloText.ui(14, weight: 600)),
            ],
          ),
        ),
      ),
    );
  }
}

class _CastCard extends StatefulWidget {
  const _CastCard({required this.name, required this.role, required this.photo, required this.onTap});
  final String name;
  final String role;
  final String photo;
  final VoidCallback onTap;

  @override
  State<_CastCard> createState() => _CastCardState();
}

class _CastCardState extends State<_CastCard> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      child: GestureDetector(
        onTap: widget.onTap,
        child: SizedBox(
          width: 124,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              AnimatedContainer(
                duration: const Duration(milliseconds: 220),
                curve: VeyloCurves.out,
                transform: Matrix4.identity()..translateByDouble(0.0, _hovered ? -3.0 : 0.0, 0.0, 1.0),
                decoration: BoxDecoration(
                  borderRadius: BorderRadius.circular(12),
                  border: Border.all(color: _hovered ? VeyloColors.sheen : const Color.fromRGBO(255, 255, 255, 0.10)),
                ),
                child: AspectRatio(
                  aspectRatio: 2 / 3,
                  child: ClipRRect(
                    borderRadius: BorderRadius.circular(11),
                    child: Container(
                      color: const Color.fromRGBO(255, 255, 255, 0.08),
                      child: FallbackImage(
                        sources: [if (widget.photo.isNotEmpty) widget.photo],
                        alignment: Alignment.topCenter,
                        cacheWidth: 260,
                        fallback: Center(
                          child: Text(
                            widget.name.isNotEmpty ? widget.name[0] : '?',
                            style: VeyloText.display(28, weight: 800, color: VeyloColors.dust),
                          ),
                        ),
                      ),
                    ),
                  ),
                ),
              ),
              const SizedBox(height: 8),
              Text(widget.name, maxLines: 1, overflow: TextOverflow.ellipsis, style: VeyloText.ui(14, weight: 600)),
              if (widget.role.isNotEmpty)
                Text(widget.role, maxLines: 1, overflow: TextOverflow.ellipsis, style: VeyloText.ui(13, weight: 500, color: VeyloColors.dust)),
            ],
          ),
        ),
      ),
    );
  }
}

class _CrewGrid extends StatelessWidget {
  const _CrewGrid({required this.crew, required this.onSelectPerson});
  final List<Map<String, dynamic>> crew;
  final ValueChanged<String> onSelectPerson;

  @override
  Widget build(BuildContext context) {
    final grouped = <String, List<String>>{};
    for (final c in crew) {
      final role = c['job']?.toString() ?? c['role']?.toString() ?? '';
      final people = c['people'] is List
          ? (c['people'] as List).map((p) => p is Map ? p['name']?.toString() ?? '' : p.toString())
          : [c['name']?.toString() ?? ''];
      for (final name in people) {
        if (role.isEmpty || name.isEmpty) continue;
        final list = grouped.putIfAbsent(role, () => []);
        if (!list.contains(name)) list.add(name);
      }
    }
    if (grouped.isEmpty) return const SizedBox.shrink();

    return Wrap(
      spacing: 36,
      runSpacing: 14,
      children: [
        for (final entry in grouped.entries)
          Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisSize: MainAxisSize.min,
            children: [
              Text(entry.key, style: VeyloText.ui(12.5, weight: 600, color: VeyloColors.dust)),
              const SizedBox(height: 4),
              Wrap(
                spacing: 10,
                runSpacing: 4,
                children: [
                  for (final name in entry.value.take(5))
                    _CrewLink(name: name, onTap: () => onSelectPerson(name)),
                ],
              ),
            ],
          ),
      ],
    );
  }
}

class _CrewLink extends StatefulWidget {
  const _CrewLink({required this.name, required this.onTap});
  final String name;
  final VoidCallback onTap;

  @override
  State<_CrewLink> createState() => _CrewLinkState();
}

class _CrewLinkState extends State<_CrewLink> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      child: GestureDetector(
        onTap: widget.onTap,
        child: Text(
          widget.name,
          style: VeyloText.ui(
            14.5,
            weight: 600,
            color: VeyloColors.bone,
            decoration: _hovered ? TextDecoration.underline : TextDecoration.none,
            decorationColor: const Color.fromRGBO(242, 239, 234, 0.5),
          ),
        ),
      ),
    );
  }
}

class _SeasonCard extends StatelessWidget {
  const _SeasonCard({
    required this.season,
    required this.poster,
    required this.count,
    required this.seenCount,
    required this.active,
    required this.onTap,
  });

  final int season;
  final String? poster;
  final int count;
  final int seenCount;
  final bool active;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final label = season == 0 ? 'Specials' : 'Season $season';
    final done = count > 0 && seenCount >= count;
    final frac = count > 0 ? (seenCount / count).clamp(0.0, 1.0) : 0.0;

    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        child: SizedBox(
          width: 102,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              AnimatedContainer(
                duration: const Duration(milliseconds: 220),
                curve: VeyloCurves.out,
                transform: Matrix4.identity()..translateByDouble(0.0, active ? -4.0 : 0.0, 0.0, 1.0),
                decoration: BoxDecoration(
                  borderRadius: BorderRadius.circular(12),
                  border: Border.all(
                    color: active ? VeyloColors.bone : const Color.fromRGBO(255, 255, 255, 0.12),
                    width: active ? 2 : 1,
                  ),
                ),
                child: AspectRatio(
                  aspectRatio: 2 / 3,
                  child: ClipRRect(
                    borderRadius: BorderRadius.circular(11),
                    child: Stack(
                      fit: StackFit.expand,
                      children: [
                        Container(
                          color: const Color.fromRGBO(255, 255, 255, 0.06),
                          child: FallbackImage(
                            sources: [?poster],
                            cacheWidth: 240,
                            fallback: Center(child: Text('$season', style: VeyloText.display(24, weight: 800, color: VeyloColors.dust))),
                          ),
                        ),
                        Positioned(
                          top: 6,
                          right: 6,
                          child: Container(
                            height: 20,
                            padding: const EdgeInsets.symmetric(horizontal: 6),
                            alignment: Alignment.center,
                            decoration: BoxDecoration(
                              color: done ? VeyloColors.bone : const Color.fromRGBO(10, 11, 16, 0.76),
                              borderRadius: BorderRadius.circular(10),
                            ),
                            child: done
                                ? const VeyloIcon('Check', size: 11, color: Color(0xFF000000))
                                : Text(
                                    '$count',
                                    style: VeyloText.ui(10.5, weight: 700, color: VeyloColors.bone, tabular: true),
                                  ),
                          ),
                        ),
                        if (frac > 0 && !done)
                          Positioned(
                            left: 0,
                            right: 0,
                            bottom: 0,
                            child: ValueListenableBuilder<Color>(
                              valueListenable: app.lampColorNotifier,
                              builder: (_, lamp, _) => Container(
                                height: 3,
                                color: const Color.fromRGBO(0, 0, 0, 0.55),
                                alignment: Alignment.centerLeft,
                                child: FractionallySizedBox(widthFactor: frac, child: ColoredBox(color: lamp)),
                              ),
                            ),
                          ),
                      ],
                    ),
                  ),
                ),
              ),
              const SizedBox(height: 6),
              Text(
                label,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: VeyloText.ui(13.5, weight: 600, color: active ? VeyloColors.bone : VeyloColors.dust),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _EpisodeTile extends StatefulWidget {
  const _EpisodeTile({
    required this.episode,
    required this.isNext,
    required this.isSeen,
    required this.progress,
    required this.onPlay,
    required this.onSources,
    required this.onToggleSeen,
  });

  final EpisodeItem episode;
  final bool isNext;
  final bool isSeen;
  final double progress;
  final VoidCallback onPlay;
  final VoidCallback onSources;
  final VoidCallback onToggleSeen;

  @override
  State<_EpisodeTile> createState() => _EpisodeTileState();
}

class _EpisodeTileState extends State<_EpisodeTile> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final ep = widget.episode;

    return RepaintBoundary(
      child: ValueListenableBuilder<Color>(
        valueListenable: app.lampColorNotifier,
        builder: (_, l, _) => MouseRegion(
          cursor: SystemMouseCursors.click,
          onEnter: (_) => setState(() => _hovered = true),
          onExit: (_) => setState(() => _hovered = false),
          child: AnimatedContainer(
            duration: const Duration(milliseconds: 220),
            curve: VeyloCurves.out,
            transform: Matrix4.identity()..translateByDouble(0.0, _hovered ? -3.0 : 0.0, 0.0, 1.0),
            decoration: BoxDecoration(
              borderRadius: BorderRadius.circular(20),
              gradient: LinearGradient(
                begin: Alignment.topCenter,
                end: Alignment.bottomCenter,
                colors: widget.isNext
                    ? [
                        VeyloColors.mixLamp(l, const Color.fromRGBO(255, 255, 255, 0.08), 0.12),
                        VeyloColors.mixLamp(l, const Color.fromRGBO(255, 255, 255, 0.035), 0.05),
                      ]
                    : [
                        VeyloColors.mixLamp(l, const Color.fromRGBO(255, 255, 255, 0.065), 0.04),
                        const Color.fromRGBO(255, 255, 255, 0.032),
                      ],
              ),
              border: Border.all(
                color: widget.isNext
                    ? VeyloColors.mixLamp(l, const Color.fromRGBO(255, 255, 255, 0.18), 0.55)
                    : (_hovered ? const Color.fromRGBO(255, 255, 255, 0.22) : VeyloColors.edge),
              ),
              boxShadow: [
                BoxShadow(
                  color: Color.fromRGBO(0, 0, 0, _hovered ? 0.72 : 0.44),
                  blurRadius: _hovered ? 42 : 26,
                  offset: Offset(0, _hovered ? 20 : 10),
                ),
              ],
            ),
            child: ClipRRect(
              borderRadius: BorderRadius.circular(20),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  GestureDetector(
                    onTap: widget.onPlay,
                    child: AspectRatio(
                      aspectRatio: 16 / 9,
                      child: Stack(
                        fit: StackFit.expand,
                        children: [
                          Opacity(
                            opacity: widget.isSeen && !_hovered ? 0.62 : 1.0,
                            child: FallbackImage(
                              sources: [if (ep.thumbnail.isNotEmpty) ep.thumbnail],
                              cacheWidth: 640,
                              fallback: Container(
                                color: VeyloColors.mixLamp(l, const Color(0xFF1A1C24), 0.12),
                                alignment: Alignment.center,
                                child: Text(
                                  '${ep.number}',
                                  style: VeyloText.display(44, weight: 800, color: const Color.fromRGBO(255, 255, 255, 0.14)),
                                ),
                              ),
                            ),
                          ),
                          const DecoratedBox(
                            decoration: BoxDecoration(
                              gradient: LinearGradient(
                                begin: Alignment.topCenter,
                                end: Alignment.bottomCenter,
                                stops: [0.0, 0.35, 0.65, 1.0],
                                colors: [
                                  Color.fromRGBO(0, 0, 0, 0.36),
                                  Color.fromRGBO(0, 0, 0, 0.0),
                                  Color.fromRGBO(0, 0, 0, 0.16),
                                  Color.fromRGBO(10, 11, 15, 0.86),
                                ],
                              ),
                            ),
                          ),
                          Positioned(
                            top: 10,
                            left: 10,
                            right: 10,
                            child: Row(
                              mainAxisAlignment: MainAxisAlignment.spaceBetween,
                              children: [
                                Container(
                                  height: 24,
                                  padding: const EdgeInsets.symmetric(horizontal: 9),
                                  alignment: Alignment.center,
                                  decoration: BoxDecoration(
                                    color: const Color.fromRGBO(10, 11, 16, 0.72),
                                    borderRadius: BorderRadius.circular(12),
                                    border: Border.all(color: const Color.fromRGBO(255, 255, 255, 0.14)),
                                  ),
                                  child: Text(ep.label, style: VeyloText.ui(11.5, weight: 700, tabular: true)),
                                ),
                                if (widget.isNext)
                                  Container(
                                    height: 24,
                                    padding: const EdgeInsets.symmetric(horizontal: 10),
                                    alignment: Alignment.center,
                                    decoration: BoxDecoration(color: l, borderRadius: BorderRadius.circular(12)),
                                    child: Text('Up next', style: VeyloText.ui(11.5, weight: 800, color: const Color(0xFF000000))),
                                  )
                                else if (widget.isSeen)
                                  Container(
                                    height: 24,
                                    padding: const EdgeInsets.symmetric(horizontal: 8),
                                    decoration: BoxDecoration(
                                      color: const Color.fromRGBO(10, 11, 16, 0.72),
                                      borderRadius: BorderRadius.circular(12),
                                      border: Border.all(color: const Color.fromRGBO(255, 255, 255, 0.14)),
                                    ),
                                    child: Row(
                                      mainAxisSize: MainAxisSize.min,
                                      children: [
                                        const VeyloIcon('Check', size: 12, color: VeyloColors.bone),
                                        const SizedBox(width: 4),
                                        Text('Watched', style: VeyloText.ui(11, weight: 600)),
                                      ],
                                    ),
                                  ),
                              ],
                            ),
                          ),
                          Positioned(
                            left: 10,
                            bottom: 10,
                            child: VeyloOrb(
                              icon: 'Play',
                              size: 34,
                              iconSize: 15,
                              accent: _hovered,
                              active: widget.isNext && !_hovered,
                              backgroundOverride: widget.isNext || _hovered ? null : const Color.fromRGBO(12, 13, 18, 0.78),
                              onTap: widget.onPlay,
                            ),
                          ),
                          if (widget.progress > 0 && !widget.isSeen)
                            Positioned(
                              left: 0,
                              right: 0,
                              bottom: 0,
                              child: Container(
                                height: 3,
                                color: const Color.fromRGBO(255, 255, 255, 0.14),
                                alignment: Alignment.centerLeft,
                                child: FractionallySizedBox(widthFactor: widget.progress, child: ColoredBox(color: l)),
                              ),
                            ),
                        ],
                      ),
                    ),
                  ),
                  Expanded(
                    child: Padding(
                      padding: const EdgeInsets.fromLTRB(14, 10, 14, 6),
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(
                            ep.title.isEmpty ? 'Episode ${ep.number}' : ep.title,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: VeyloText.ui(
                              15.5,
                              weight: 700,
                              color: widget.isSeen ? const Color.fromRGBO(242, 239, 234, 0.74) : VeyloColors.bone,
                            ),
                          ),
                          if (ep.overview.isNotEmpty) ...[
                            const SizedBox(height: 4),
                            Expanded(
                              child: Text(
                                ep.overview,
                                maxLines: 2,
                                overflow: TextOverflow.ellipsis,
                                style: VeyloText.ui(13, weight: 400, height: 1.4, color: const Color.fromRGBO(226, 224, 220, 0.74)),
                              ),
                            ),
                          ],
                        ],
                      ),
                    ),
                  ),
                  Container(
                    padding: const EdgeInsets.fromLTRB(14, 6, 10, 8),
                    decoration: const BoxDecoration(
                      border: Border(top: BorderSide(color: Color.fromRGBO(255, 255, 255, 0.06))),
                    ),
                    child: Row(
                      children: [
                        Expanded(
                          child: Text(
                            ep.dateLabel,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: VeyloText.ui(12.5, weight: 500, color: VeyloColors.dust),
                          ),
                        ),
                        VeyloOrb(
                          icon: 'Check',
                          size: 32,
                          iconSize: 14,
                          active: widget.isSeen,
                          tooltip: widget.isSeen ? 'Mark unwatched' : 'Mark watched',
                          backgroundOverride: widget.isSeen ? null : const Color.fromRGBO(255, 255, 255, 0.05),
                          onTap: widget.onToggleSeen,
                        ),
                        const SizedBox(width: 6),
                        VeyloOrb(
                          icon: 'Layers',
                          size: 32,
                          iconSize: 14,
                          tooltip: 'Choose a source',
                          backgroundOverride: const Color.fromRGBO(255, 255, 255, 0.05),
                          onTap: widget.onSources,
                        ),
                      ],
                    ),
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}

class _SourceBottomSheet extends StatefulWidget {
  const _SourceBottomSheet({
    required this.title,
    this.genres = const [],
    this.mediaKind = 'movie',
    required this.loading,
    required this.error,
    required this.sources,
    required this.onClose,
    required this.onPlay,
    required this.onDownload,
  });

  final String title;
  final List<String> genres;
  final String mediaKind;
  final bool loading;
  final String? error;
  final List<SourceItem> sources;
  final VoidCallback onClose;
  final ValueChanged<SourceItem> onPlay;
  final ValueChanged<SourceItem> onDownload;

  @override
  State<_SourceBottomSheet> createState() => _SourceBottomSheetState();
}

class _SourceBottomSheetState extends State<_SourceBottomSheet> {
  int? _expandedIdx;

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final insets = MediaQuery.viewPaddingOf(context);
    return Positioned.fill(
      child: GestureDetector(
        onTap: widget.onClose,
        child: BackdropFilter.grouped(
          filter: ImageFilter.blur(sigmaX: 12, sigmaY: 12, tileMode: TileMode.clamp),
          child: Container(
            color: const Color.fromRGBO(0, 0, 0, 0.62),
            alignment: Alignment.bottomCenter,
            padding: insets,
            child: GestureDetector(
              onTap: () {},
              child: LiftIn(
                offsetY: 28,
                duration: const Duration(milliseconds: 220),
                child: ConstrainedBox(
                  constraints: BoxConstraints(
                    maxWidth: 900,
                    maxHeight: MediaQuery.sizeOf(context).height * 0.82,
                  ),
                  child: ClipRRect(
                    borderRadius: const BorderRadius.vertical(top: Radius.circular(28)),
                    child: GlassBox(
                      borderRadius: 0,
                      strong: true,
                      child: Column(
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          Container(
                            margin: const EdgeInsets.only(top: 10),
                            width: 40,
                            height: 4,
                            decoration: BoxDecoration(
                              color: const Color.fromRGBO(255, 255, 255, 0.22),
                              borderRadius: BorderRadius.circular(2),
                            ),
                          ),
                          Container(
                            padding: const EdgeInsets.fromLTRB(20, 12, 16, 12),
                            decoration: const BoxDecoration(border: Border(bottom: BorderSide(color: VeyloColors.divider))),
                            child: Row(
                              children: [
                                Expanded(child: Text(widget.title, maxLines: 1, overflow: TextOverflow.ellipsis, style: VeyloText.groupName)),
                                VeyloOrb(icon: 'Close', size: 40, onTap: widget.onClose),
                              ],
                            ),
                          ),
                          Flexible(
                            child: widget.loading
                                ? Padding(
                                    padding: const EdgeInsets.symmetric(horizontal: 20, vertical: 34),
                                    child: Center(
                                      child: ValueListenableBuilder<Color>(
                                        valueListenable: app.lampColorNotifier,
                                        builder: (_, lamp, _) => FindingRunner(
                                          lamp: lamp,
                                          genres: widget.genres,
                                          mediaKind: widget.mediaKind,
                                          title: widget.title,
                                          width: 280,
                                          height: 96,
                                        ),
                                      ),
                                    ),
                                  )
                                : widget.error != null
                                    ? Padding(
                                        padding: const EdgeInsets.all(24),
                                        child: Text(widget.error!, style: VeyloText.ui(15, color: VeyloColors.amber)),
                                      )
                                    : widget.sources.isEmpty
                                        ? Padding(
                                            padding: const EdgeInsets.symmetric(horizontal: 24, vertical: 34),
                                            child: Column(
                                              mainAxisSize: MainAxisSize.min,
                                              children: [
                                                ValueListenableBuilder<Color>(
                                                  valueListenable: app.lampColorNotifier,
                                                  builder: (_, lamp, _) => FindingRunner(
                                                    lamp: lamp,
                                                    scene: CinemaScene.noir,
                                                    width: 220,
                                                    height: 78,
                                                  ),
                                                ),
                                                const SizedBox(height: 10),
                                                const Text('No sources found for this title.', style: VeyloText.meta),
                                              ],
                                            ),
                                          )
                                        : ValueListenableBuilder<Color>(
                                            valueListenable: app.lampColorNotifier,
                                            builder: (_, lamp, _) => ListView.separated(
                                              padding: const EdgeInsets.fromLTRB(16, 14, 16, 24),
                                              itemCount: widget.sources.length,
                                              separatorBuilder: (_, _) => const SizedBox(height: 8),
                                              itemBuilder: (_, i) {
                                                final s = widget.sources[i];
                                                final direct = s.kind == 'direct';
                                                final displayName = s.filename.isNotEmpty
                                                    ? s.filename
                                                    : (s.description.split('\n').firstOrNull ?? s.name);
                                                final meta = [if (s.size != null) s.size!, s.addon].where((x) => x.isNotEmpty).join(' · ');
                                                final expanded = _expandedIdx == i;
                                                final hasDetails = s.description.trim().isNotEmpty;

                                                return AnimatedContainer(
                                                  duration: const Duration(milliseconds: 160),
                                                  padding: const EdgeInsets.fromLTRB(12, 8, 8, 8),
                                                  decoration: BoxDecoration(
                                                    color: expanded
                                                        ? const Color.fromRGBO(255, 255, 255, 0.085)
                                                        : const Color.fromRGBO(255, 255, 255, 0.05),
                                                    borderRadius: BorderRadius.circular(18),
                                                    border: Border.all(
                                                      color: expanded ? VeyloColors.edge : const Color.fromRGBO(255, 255, 255, 0.09),
                                                    ),
                                                  ),
                                                  child: Column(
                                                    crossAxisAlignment: CrossAxisAlignment.start,
                                                    children: [
                                                      Row(
                                                        children: [
                                                          SizedBox(
                                                            width: 66,
                                                            child: Column(
                                                              children: [
                                                                if (s.quality != null)
                                                                  Container(
                                                                    height: 24,
                                                                    padding: const EdgeInsets.symmetric(horizontal: 8),
                                                                    alignment: Alignment.center,
                                                                    decoration: BoxDecoration(
                                                                      color: VeyloColors.bone,
                                                                      borderRadius: BorderRadius.circular(12),
                                                                    ),
                                                                    child: Text(
                                                                      s.quality!,
                                                                      style: VeyloText.ui(12, weight: 800, color: const Color(0xFF000000)),
                                                                    ),
                                                                  ),
                                                                if (s.hdr != null) ...[
                                                                  const SizedBox(height: 4),
                                                                  Container(
                                                                    height: 22,
                                                                    padding: const EdgeInsets.symmetric(horizontal: 7),
                                                                    alignment: Alignment.center,
                                                                    decoration: BoxDecoration(
                                                                      color: lamp.withValues(alpha: 0.25),
                                                                      borderRadius: BorderRadius.circular(11),
                                                                      border: Border.all(color: lamp.withValues(alpha: 0.6)),
                                                                    ),
                                                                    child: Text(s.hdr!, style: VeyloText.ui(11.5, weight: 700)),
                                                                  ),
                                                                ],
                                                              ],
                                                            ),
                                                          ),
                                                          const SizedBox(width: 12),
                                                          Expanded(
                                                            child: MouseRegion(
                                                              cursor: direct ? SystemMouseCursors.click : SystemMouseCursors.basic,
                                                              child: GestureDetector(
                                                                onTap: direct ? () => widget.onPlay(s) : null,
                                                                child: Opacity(
                                                                  opacity: direct ? 1.0 : 0.55,
                                                                  child: Column(
                                                                    crossAxisAlignment: CrossAxisAlignment.start,
                                                                    children: [
                                                                      Text(
                                                                        displayName,
                                                                        maxLines: 1,
                                                                        overflow: TextOverflow.ellipsis,
                                                                        style: VeyloText.ui(15, weight: 600),
                                                                      ),
                                                                      if (meta.isNotEmpty)
                                                                        Text(
                                                                          meta,
                                                                          maxLines: 1,
                                                                          overflow: TextOverflow.ellipsis,
                                                                          style: VeyloText.ui(13, weight: 500, color: VeyloColors.dust, tabular: true),
                                                                        ),
                                                                    ],
                                                                  ),
                                                                ),
                                                              ),
                                                            ),
                                                          ),
                                                          if (hasDetails) ...[
                                                            VeyloOrb(
                                                              icon: 'Info',
                                                              size: 36,
                                                              iconSize: 16,
                                                              active: expanded,
                                                              tooltip: expanded ? 'Hide source info' : 'Source info',
                                                              onTap: () => setState(() => _expandedIdx = expanded ? null : i),
                                                            ),
                                                            const SizedBox(width: 6),
                                                          ],
                                                          if (direct) ...[
                                                            VeyloOrb(
                                                              icon: 'Download',
                                                              size: 38,
                                                              iconSize: 17,
                                                              tooltip: 'Download',
                                                              onTap: () => widget.onDownload(s),
                                                            ),
                                                            const SizedBox(width: 6),
                                                            VeyloOrb(
                                                              icon: 'Play',
                                                              size: 40,
                                                              iconSize: 18,
                                                              accent: true,
                                                              tooltip: 'Play',
                                                              onTap: () => widget.onPlay(s),
                                                            ),
                                                          ],
                                                        ],
                                                      ),
                                                      if (expanded && hasDetails) ...[
                                                        const SizedBox(height: 8),
                                                        Container(
                                                          width: double.infinity,
                                                          padding: const EdgeInsets.all(10),
                                                          decoration: BoxDecoration(
                                                            color: const Color.fromRGBO(0, 0, 0, 0.35),
                                                            borderRadius: BorderRadius.circular(12),
                                                          ),
                                                          child: Text(
                                                            s.description.trim(),
                                                            style: VeyloText.ui(12.5, weight: 400, height: 1.45, color: VeyloColors.synopsis),
                                                          ),
                                                        ),
                                                      ],
                                                    ],
                                                  ),
                                                );
                                              },
                                            ),
                                          ),
                          ),
                        ],
                      ),
                    ),
                  ),
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}
