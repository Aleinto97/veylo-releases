import 'dart:async';
import 'package:flutter/widgets.dart';
import '../bridge/veylo_bridge.dart';
import '../theme/veylo_theme.dart';

enum VeyloTab { home, picks, library, settings }

extension VeyloTabExt on VeyloTab {
  String get title => switch (this) {
        VeyloTab.home => 'Home',
        VeyloTab.picks => 'Picks',
        VeyloTab.library => 'Library',
        VeyloTab.settings => 'Settings',
      };

  String get iconName => switch (this) {
        VeyloTab.home => 'House',
        VeyloTab.picks => 'Sparkles',
        VeyloTab.library => 'Stack',
        VeyloTab.settings => 'Sliders',
      };
}

class AppState extends ChangeNotifier {
  final VeyloBridge bridge = VeyloBridge.instance;

  bool initialized = false;
  String? initError;
  String version = '0.13.7';
  String demoStream = '';
  bool aiFailed = false;
  bool accountsAvailable = false;
  bool googleAvailable = false;
  final ValueNotifier<bool> syncKeyNeededNotifier = ValueNotifier<bool>(false);

  VeyloTab tab = VeyloTab.home;
  int tabDirection = 1;
  MediaItem? openedTitle;
  bool searchOpen = false;
  String searchDraft = '';
  String? pendingSearchQuery;
  bool shortcutsOpen = false;
  bool picksSavedOnly = false;

  // Fine-grained notifiers: keep hot-path updates isolated to tiny subtrees!
  final ValueNotifier<bool> pageScrolledNotifier = ValueNotifier<bool>(false);
  final ValueNotifier<Color> lampColorNotifier = ValueNotifier<Color>(VeyloColors.defaultLamp);
  final ValueNotifier<String> ambientUrlNotifier = ValueNotifier<String>('');
  final ValueNotifier<int> homeHeroIndexNotifier = ValueNotifier<int>(0);
  final ValueNotifier<SeenItem?> lastPlayedNotifier = ValueNotifier<SeenItem?>(null);
  final ValueNotifier<int> addonUpdatesNotifier = ValueNotifier<int>(0);

  String lampSource = '';
  Color get lampColor => lampColorNotifier.value;
  String get ambientImageUrl => ambientUrlNotifier.value;
  bool get pageScrolled => pageScrolledNotifier.value;
  int get homeHeroIndex => homeHeroIndexNotifier.value;
  SeenItem? get lastPlayed => lastPlayedNotifier.value;
  int get addonUpdates => addonUpdatesNotifier.value;

  int picksDirty = 0;
  final ValueNotifier<int> picksDirtyNotifier = ValueNotifier<int>(0);

  // Player & toast state
  String? pendingPlayerTitle;
  String pendingPlayerStep = 'Finding a source';
  MediaItem? pendingPlayerMedia;
  String? _inflightPlayKey;
  int _playTicket = 0;
  int? _autoplayTicket;
  PlaySession? activePlayer;
  TheatreSession? activeTheatre;
  Future<void> Function()? onBeforeClosePlayer;
  String? toastMessage;
  bool toastIsError = false;
  Timer? _toastTimer;
  Timer? _syncPollTimer;
  AppLifecycleListener? _syncLifecycle;
  bool _pollingSyncArrival = false;
  bool _disposed = false;
  int _fastSyncPollUsers = 0;

  void setFastSyncPolling(bool active) {
    if (_disposed) return;
    _fastSyncPollUsers += active ? 1 : -1;
    if (_fastSyncPollUsers == (active ? 1 : 0)) _scheduleSyncPolling();
  }

  void _scheduleSyncPolling() {
    _syncPollTimer?.cancel();
    if (!accountsAvailable || !initialized) return;
    final interval = Duration(seconds: _fastSyncPollUsers > 0 ? 4 : 60);
    _syncPollTimer = Timer.periodic(interval, (_) => unawaited(_pollSyncArrival()));
  }

  @override
  void dispose() {
    _disposed = true;
    _syncPollTimer?.cancel();
    _syncLifecycle?.dispose();
    _toastTimer?.cancel();
    _advancePlayTicket();
    super.dispose();
  }

  // Cached Home data with per-shelf notifiers (loading shelf 3 never rebuilds hero or other shelves!)
  List<CatalogRowModel> homeRows = [];
  final Map<String, ValueNotifier<List<MediaItem>?>> _shelfNotifiers = {};
  final Map<String, ValueNotifier<String?>> _shelfErrorNotifiers = {};
  final ValueNotifier<List<MediaItem>> libraryItemsNotifier = ValueNotifier<List<MediaItem>>([]);
  final ValueNotifier<List<MediaItem>> tonightPicksNotifier = ValueNotifier<List<MediaItem>>([]);
  final ValueNotifier<bool> homeLoadingNotifier = ValueNotifier<bool>(true);
  String? homeError;
  bool homeLibraryReady = false;
  bool _homeTonightShown = false;
  Future<void>? _homeRefresh;
  bool _refreshHomeAgain = false;
  final Map<String, ArtworkModel> homeArtwork = {};
  final Map<String, Future<void>> _shelfLoads = {};


  // Per-title artwork cache with isolated notifiers
  final Map<String, ValueNotifier<ArtworkModel?>> _artworkNotifiers = {};
  final Map<String, Color> accentCache = {};

  List<MediaItem> get libraryItems => libraryItemsNotifier.value;
  List<MediaItem> get tonightPicks => tonightPicksNotifier.value;
  bool get homeLoading => homeLoadingNotifier.value;

  ValueNotifier<List<MediaItem>?> shelfNotifier(String key) =>
      _shelfNotifiers.putIfAbsent(key, () => ValueNotifier<List<MediaItem>?>(null));

  ValueNotifier<String?> shelfErrorNotifier(String key) =>
      _shelfErrorNotifiers.putIfAbsent(key, () => ValueNotifier<String?>(null));

  ValueNotifier<ArtworkModel?> artworkNotifier(String key) =>
      _artworkNotifiers.putIfAbsent(key, () => ValueNotifier<ArtworkModel?>(null));

  ArtworkModel? cachedArtwork(String key) => _artworkNotifiers[key]?.value;

  Future<void> bootstrap() async {
    try {
      final info = await bridge.init();
      if (_disposed) return;
      version = info['version'] as String? ?? '0.13.7';
      demoStream = info['demo_stream'] as String? ?? '';
      aiFailed = info['ai_failed'] as bool? ?? false;
      accountsAvailable = info['accounts_available'] as bool? ?? false;
      googleAvailable = info['google_available'] as bool? ?? false;
      syncKeyNeededNotifier.value = info['sync_key_needed'] as bool? ?? false;
      await _restoreHome();
      if (_disposed) return;
      initialized = true;
      notifyListeners();
      unawaited(loadHome());
      unawaited(refreshLastPlayed());
      unawaited(refreshAddonUpdates());
      if (accountsAvailable) {
        _syncLifecycle ??= AppLifecycleListener(onResume: () {
          _scheduleSyncPolling();
          unawaited(_pollSyncArrival());
        });
        _scheduleSyncPolling();
      }
    } catch (e) {
      if (_disposed) return;
      initError = e.toString();
      notifyListeners();
    }
  }

  Future<void> _pollSyncArrival() async {
    if (_disposed || _pollingSyncArrival) return;
    _pollingSyncArrival = true;
    try {
      final res = await bridge.accountTakeKeysArrived();
      if (_disposed) return;
      final from = res['from'] as String?;
      if (from != null && from.isNotEmpty) {
        showToast('Synced settings & add-ons from $from', durationMs: 6500);
        unawaited(refreshAddonUpdates());
        bumpPicksDirty();
        unawaited(loadHome(force: true));
      }
    } catch (_) {
    } finally {
      _pollingSyncArrival = false;
    }
  }

  Future<void> _restoreHome() async {
    try {
      final snapshot = await bridge.homeCached();
      homeRows = snapshot.rows;
      for (final entry in snapshot.shelves.entries) {
        shelfNotifier(entry.key).value = entry.value;
      }
      homeArtwork.addAll(snapshot.artwork);
      tonightPicksNotifier.value = snapshot.tonight;
      _homeTonightShown = snapshot.tonight.isNotEmpty;
      libraryItemsNotifier.value = snapshot.library;
      homeLibraryReady = true;
    } catch (_) {
      await _refreshLibrarySilent();
    }
  }

  Future<void> loadHome({bool force = false}) {
    if (_homeRefresh case final running?) {
      _refreshHomeAgain |= force;
      return running;
    }
    return _homeRefresh = _refreshHome().whenComplete(() => _homeRefresh = null);
  }

  Future<void> _refreshHome() async {
    do {
      _refreshHomeAgain = false;
      homeLoadingNotifier.value = true;
      await Future.wait([
        _refreshLibrarySilent(),
        _loadRows(),
        _loadTonight(),
      ]);
      // A fresh store may have no recommendation candidates until its first
      // catalogs have populated the backend's local index.
      if (!_homeTonightShown &&
          homeRows.any((row) => shelfNotifier(row.key).value?.isNotEmpty == true)) {
        await _loadTonight();
      }
      homeLoadingNotifier.value = false;
    } while (_refreshHomeAgain);
  }

  Future<void> _refreshLibrarySilent() async {
    try {
      final library = await bridge.library();
      homeLibraryReady = true;
      libraryItemsNotifier.value = library;
    } catch (_) {}
  }

  Future<void> _loadRows() async {
    try {
      final rows = await bridge.homeRows();
      final existing = homeRows.map((row) => row.key).toSet();
      final appended = rows.where((row) => existing.add(row.key)).toList();
      if (appended.isNotEmpty || homeError != null) {
        homeRows = [...homeRows, ...appended];
        homeError = null;
        notifyListeners();
      }
      // Use today's routing fields when available. A removed add-on keeps its
      // visible slot until the next launch.
      final loads = rows.map(_loadShelf).toList();
      final live = rows.map((row) => row.key).toSet();
      for (final row in homeRows) {
        if (!live.contains(row.key) && shelfNotifier(row.key).value == null) {
          shelfNotifier(row.key).value = [];
        }
      }
      await Future.wait(loads);
    } catch (e) {
      if (homeRows.isEmpty) {
        homeError = e.toString();
        notifyListeners();
      }
      await Future.wait(homeRows.map(_loadShelf));
    }
  }

  Future<void> _loadShelf(CatalogRowModel row) => _shelfLoads.putIfAbsent(
        row.key,
        () => _refreshShelf(row).whenComplete(() {
          _shelfLoads.remove(row.key);
        }),
      );

  Future<void> _refreshShelf(CatalogRowModel row) async {
    final shelf = shelfNotifier(row.key);
    final error = shelfErrorNotifier(row.key);
    try {
      error.value = null;
      final items = await bridge.catalog(row);
      if (shelf.value?.isNotEmpty != true) shelf.value = items;
    } catch (e) {
      if (shelf.value?.isNotEmpty != true) error.value = e.toString();
    }
  }

  Future<void> retryShelf(CatalogRowModel row) => _loadShelf(row);

  Future<void> _loadTonight() async {
    try {
      final picks = await bridge.tonightPicks();
      if (!_homeTonightShown && picks.isNotEmpty) {
        _homeTonightShown = true;
        tonightPicksNotifier.value = picks;
      }
      final disliked = libraryItems.where((m) => m.liked < 0).map((m) => m.id).toSet();
      for (final pick in picks.where((m) => !disliked.contains(m.id)).take(4)) {
        unawaited(fetchArtwork(pick, home: true));
      }
    } catch (_) {}
  }

  final Map<String, Future<ArtworkModel?>> _artworkInflight = {};

  Future<ArtworkModel?> fetchArtwork(MediaItem m, {bool home = false}) {
    final notifier = artworkNotifier(m.key);
    if (notifier.value != null) return Future.value(notifier.value);
    return _artworkInflight.putIfAbsent(
      m.key,
      () async {
        try {
          final art = await bridge.artwork(m, home: home);
          if (art != null) notifier.value = art;
          return art;
        } catch (_) {
          return null;
        } finally {
          _artworkInflight.remove(m.key);
        }
      },
    );
  }

  Future<Color?> fetchAccent(String url) async {
    if (url.isEmpty) return null;
    if (accentCache.containsKey(url)) return accentCache[url];
    try {
      final hex = await bridge.accent(url);
      if (hex != null) {
        final c = VeyloColors.parseHex(hex);
        accentCache[url] = c;
        return c;
      }
    } catch (_) {}
    return null;
  }

  Future<void> setLampSource(String url) async {
    if (lampSource == url) return;
    lampSource = url;
    if (url.isEmpty) {
      ambientUrlNotifier.value = '';
      lampColorNotifier.value = VeyloColors.defaultLamp;
      return;
    }
    try {
      final amb = await bridge.ambientUrl(url);
      if (lampSource == url) {
        ambientUrlNotifier.value = amb;
      }
      final c = await fetchAccent(url);
      if (lampSource == url && c != null) {
        lampColorNotifier.value = c;
      }
    } catch (_) {}
  }

  Future<void> refreshLastPlayed() async {
    try {
      final list = await bridge.recentViewing(limit: 4);
      lastPlayedNotifier.value = list.isNotEmpty ? list.first : null;
    } catch (_) {}
  }

  Future<void> refreshAddonUpdates() async {
    try {
      final ov = await bridge.addonsOverview();
      addonUpdatesNotifier.value = (ov['updates'] as num?)?.toInt() ?? 0;
    } catch (_) {}
  }

  void setAddonUpdates(int count) {
    addonUpdatesNotifier.value = count;
  }

  void setTab(VeyloTab next) {
    if (openedTitle != null) {
      openedTitle = null;
    }
    if (tab != next) {
      tabDirection = next.index >= tab.index ? 1 : -1;
      tab = next;
      pageScrolledNotifier.value = false;
      if (next == VeyloTab.library || next == VeyloTab.settings) {
        unawaited(setLampSource(''));
      }
    }
    notifyListeners();
  }

  void openTitle(MediaItem media) {
    openedTitle = media;
    pageScrolledNotifier.value = false;
    notifyListeners();
  }

  void closeTitle() {
    openedTitle = null;
    pageScrolledNotifier.value = false;
    notifyListeners();
  }

  void openSearch({String? query}) {
    if (query != null) {
      pendingSearchQuery = query;
      searchDraft = query;
    }
    searchOpen = true;
    notifyListeners();
  }

  void closeSearch() {
    if (searchDraft.trim().length >= 2) {
      unawaited(bridge.rememberQuery(searchDraft.trim()));
    }
    searchDraft = '';
    searchOpen = false;
    notifyListeners();
  }

  void setShortcutsOpen(bool open) {
    if (shortcutsOpen != open) {
      shortcutsOpen = open;
      notifyListeners();
    }
  }

  void setScrolled(bool scrolled) {
    if (pageScrolledNotifier.value != scrolled) {
      pageScrolledNotifier.value = scrolled;
    }
  }

  void setHomeHeroIndex(int idx) {
    if (homeHeroIndexNotifier.value != idx) {
      homeHeroIndexNotifier.value = idx;
    }
  }

  void bumpPicksDirty() {
    picksDirty++;
    picksDirtyNotifier.value = picksDirty;
    unawaited(_refreshLibrarySilent());
    unawaited(refreshLastPlayed());
    notifyListeners();
  }

  Future<void> dislikeHero(MediaItem m) async {
    tonightPicksNotifier.value = tonightPicks.where((x) => x.key != m.key).toList();
    try {
      await bridge.feedback(m, -1);
      bumpPicksDirty();
    } catch (_) {}
  }

  Future<void> forgetTitle(MediaItem m) async {
    libraryItemsNotifier.value = libraryItems.where((x) => x.key != m.key).toList();
    try {
      await bridge.forget(m);
      bumpPicksDirty();
    } catch (_) {}
  }

  void showToast(String message, {bool isError = false, int durationMs = 5000}) {
    _toastTimer?.cancel();
    toastMessage = message;
    toastIsError = isError;
    notifyListeners();
    _toastTimer = Timer(Duration(milliseconds: durationMs), () {
      if (toastMessage == message) {
        toastMessage = null;
        notifyListeners();
      }
    });
  }

  void dismissToast() {
    _toastTimer?.cancel();
    toastMessage = null;
    notifyListeners();
  }

  int _advancePlayTicket() {
    final previous = _autoplayTicket;
    _autoplayTicket = null;
    if (previous != null) unawaited(bridge.cancelAutoplay(previous).catchError((_) {}));
    return ++_playTicket;
  }

  void cancelPendingPlay() {
    _advancePlayTicket();
    _inflightPlayKey = null;
    pendingPlayerTitle = null;
    pendingPlayerMedia = null;
    notifyListeners();
  }

  void startPlaySession(PlaySession session) {
    _advancePlayTicket();
    _inflightPlayKey = null;
    pendingPlayerTitle = null;
    pendingPlayerMedia = null;
    activeTheatre = null;
    activePlayer = session;
    notifyListeners();
  }

  void openTheatre(TheatreSession session) {
    if (session.clips.isEmpty) return;
    _advancePlayTicket();
    _inflightPlayKey = null;
    pendingPlayerTitle = null;
    activeTheatre = session;
    notifyListeners();
  }

  void closeTheatre() {
    if (activeTheatre == null) return;
    activeTheatre = null;
    notifyListeners();
  }

  Future<void> closePlayer() async {
    _advancePlayTicket();
    _inflightPlayKey = null;
    pendingPlayerTitle = null;
    final closing = activePlayer;
    final hook = onBeforeClosePlayer;
    onBeforeClosePlayer = null;
    if (hook != null) {
      try {
        await hook();
      } catch (_) {}
    }
    if (identical(activePlayer, closing)) {
      activePlayer = null;
      notifyListeners();
    }
    try {
      await bridge.flushProgress();
    } catch (_) {}
    bumpPicksDirty();
  }

  Future<void> playDemo() async {
    if (demoStream.isEmpty) {
      showToast('Test player stream is unavailable.', isError: true);
      return;
    }
    startPlaySession(PlaySession(
      title: 'Sintel trailer',
      subtitle: 'Test player',
      url: demoStream,
    ));
  }

  /// Play a trailer or extra clip directly inside Veylo's own PlayerScreen (never an external browser).
  /// Leaves `media: null` so watching trailers never affects Continue Watching or watch history.
  Future<void> playTrailer({
    required String title,
    required List<TrailerClip> clips,
    int startIndex = 0,
  }) async {
    if (clips.isEmpty) return;
    final idx = startIndex.clamp(0, clips.length - 1);
    final clip = clips[idx];
    final requestKey = 'trailer|$title|${clip.id}';
    if (pendingPlayerTitle != null && _inflightPlayKey == requestKey) return;

    _inflightPlayKey = requestKey;
    dismissToast();
    final ticket = _advancePlayTicket();

    final displayName = clip.name.isNotEmpty && clip.name.toLowerCase() != clip.label.toLowerCase()
        ? clip.name
        : '$title · ${clip.label}';
    final subtitle = clips.length > 1
        ? '$title · ${clip.label} (${idx + 1}/${clips.length})'
        : '$title · ${clip.label}';

    // Direct stream URL from an add-on needs no YouTube resolution.
    if ((clip.youtube == null || clip.youtube!.isEmpty) && clip.url != null && clip.url!.isNotEmpty) {
      startPlaySession(PlaySession(
        title: displayName,
        subtitle: subtitle,
        url: clip.url!,
        clips: clips,
        clipIndex: idx,
        clipContextTitle: title,
      ));
      return;
    }

    final ytId = clip.youtube ?? '';
    if (ytId.isEmpty) {
      _inflightPlayKey = null;
      showToast('Trailer stream is unavailable.', isError: true);
      return;
    }

    pendingPlayerTitle = displayName;
    pendingPlayerStep = 'Loading ${clip.label.toLowerCase()}';
    notifyListeners();

    try {
      final res = await bridge.youtubeStream(ytId);
      if (_playTicket != ticket) return;

      final bestMap = res['best'] as Map<String, dynamic>?;
      if (bestMap == null) {
        throw Exception('No playable stream returned');
      }
      final best = SourceItem.fromJson(bestMap);
      final sources = (res['sources'] as List<dynamic>? ?? const [])
          .whereType<Map<String, dynamic>>()
          .map(SourceItem.fromJson)
          .toList();
      final captions = (res['captions'] as List<dynamic>? ?? const [])
          .whereType<Map<String, dynamic>>()
          .toList();

      startPlaySession(PlaySession(
        title: displayName,
        subtitle: subtitle,
        url: best.url,
        audioUrl: best.audioUrl,
        headers: best.headers,
        size: best.size,
        hdr: best.hdr,
        quality: best.quality,
        sources: sources.isNotEmpty ? sources : [best],
        initialSubtitles: captions,
        clips: clips,
        clipIndex: idx,
        clipContextTitle: title,
      ));
    } catch (e) {
      if (_playTicket != ticket) return;
      _inflightPlayKey = null;
      pendingPlayerTitle = null;
      showToast('Could not load trailer: $e', isError: true, durationMs: 6000);
    }
  }

  Future<void> autoplayMedia(
    MediaItem media, {
    String? videoId,
    String? episodeLabel,
    int? pack,
    List<EpisodeItem> seasonEpisodes = const [],
    EpisodeItem? nextEpisode,
  }) async {
    final requestKey = '${media.key}|${videoId ?? ''}';
    if (pendingPlayerTitle != null && _inflightPlayKey == requestKey) {
      return;
    }
    _inflightPlayKey = requestKey;
    dismissToast();

    final ticket = _advancePlayTicket();
    pendingPlayerTitle = episodeLabel != null ? '${media.name} $episodeLabel' : media.name;
    pendingPlayerMedia = media;
    pendingPlayerStep = (videoId == null && media.kind != 'movie') ? 'Resolving episode' : 'Finding a source';
    notifyListeners();

    String targetVideo = videoId ?? media.id;
    String? resolvedLabel = episodeLabel;
    int? resolvedPack = pack;
    List<EpisodeItem> resolvedEpisodes = seasonEpisodes;
    EpisodeItem? resolvedNext = nextEpisode;
    int resolvedStartMs = (media.inProgress && (media.lastVideoId == targetVideo || media.kind == 'movie'))
        ? media.positionMs
        : 0;

    try {
      // Resolve series/anime episode via DX's card_playback rules when no explicit videoId was given
      // (fixes sending bare tt… series IDs from Home/Picks and replaying finished episodes in Continue Watching).
      if (videoId == null && media.kind != 'movie') {
        final target = await bridge.playTarget(media);
        if (_playTicket != ticket) return;
        targetVideo = target['video_id'] as String? ?? '${media.id}:1:1';
        resolvedLabel = target['episode_label'] as String? ?? resolvedLabel;
        resolvedPack = (target['pack'] as num?)?.toInt() ?? resolvedPack;
        resolvedStartMs = (target['start_ms'] as num?)?.toInt() ?? 0;
        final epsRaw = target['season_episodes'] as List<dynamic>? ?? const [];
        if (epsRaw.isNotEmpty) {
          resolvedEpisodes = epsRaw.map((e) => EpisodeItem.fromJson(e as Map<String, dynamic>)).toList();
        }
        if (target['next_episode'] is Map<String, dynamic>) {
          resolvedNext = EpisodeItem.fromJson(target['next_episode'] as Map<String, dynamic>);
        }
        pendingPlayerTitle = resolvedLabel != null ? '${media.name} $resolvedLabel' : media.name;
        pendingPlayerStep = 'Finding a source';
        notifyListeners();
      }

      if (resolvedNext == null && resolvedEpisodes.isNotEmpty) {
        final idx = resolvedEpisodes.indexWhere((e) => e.id == targetVideo);
        if (idx >= 0 && idx + 1 < resolvedEpisodes.length) {
          resolvedNext = resolvedEpisodes[idx + 1];
        }
      }

      final displayTitle = resolvedLabel != null ? '${media.name} $resolvedLabel' : media.name;
      if (_playTicket != ticket) return;
      _autoplayTicket = ticket;
      final Map<String, dynamic> res;
      try {
        res = await bridge.autoplay(media, videoId: targetVideo, pack: resolvedPack, ticket: ticket);
      } finally {
        if (_autoplayTicket == ticket) _autoplayTicket = null;
      }
      if (_playTicket != ticket) return;

      if (res['offline'] is Map<String, dynamic>) {
        final off = res['offline'] as Map<String, dynamic>;
        startPlaySession(PlaySession(
          title: off['title'] as String? ?? displayTitle,
          subtitle: 'Offline · ${off['subtitle'] ?? ''}',
          url: off['path'] as String? ?? '',
          startMs: resolvedStartMs,
          media: media,
          videoId: targetVideo != media.id ? targetVideo : null,
          episodes: resolvedEpisodes,
          nextEpisode: resolvedNext,
        ));
        return;
      }

      if (res['best'] is Map<String, dynamic>) {
        final best = SourceItem.fromJson(res['best'] as Map<String, dynamic>);
        final rest = (res['rest'] as List<dynamic>? ?? [])
            .map((e) => SourceItem.fromJson(e as Map<String, dynamic>))
            .toList();
        final allSources = [best, ...rest];
        startPlaySession(PlaySession(
          title: resolvedLabel ?? media.name,
          subtitle: targetVideo != media.id ? media.name : media.metaLineTyped,
          url: best.url,
          headers: best.headers,
          startMs: resolvedStartMs,
          size: best.size,
          hdr: best.hdr,
          quality: best.quality,
          filename: best.filename.isNotEmpty ? best.filename : null,
          fileSize: best.sizeBytes,
          media: media,
          videoId: targetVideo != media.id ? targetVideo : null,
          sources: allSources,
          episodes: resolvedEpisodes,
          nextEpisode: resolvedNext,
          originalLanguage: res['original_language'] as String?,
        ));
      } else {
        _inflightPlayKey = null;
        pendingPlayerTitle = null;
        pendingPlayerMedia = null;
        final msg = res['message'] as String? ?? 'No playable source found';
        showToast(msg, isError: true, durationMs: 7000);
        if (activePlayer == null && openedTitle?.key != media.key) {
          openTitle(media);
        }
      }
    } catch (e) {
      if (_playTicket != ticket) return;
      _inflightPlayKey = null;
      pendingPlayerTitle = null;
      pendingPlayerMedia = null;
      showToast(e.toString(), isError: true, durationMs: 7000);
    }
  }
}

class AppStateScope extends InheritedNotifier<AppState> {
  const AppStateScope({super.key, required AppState state, required super.child}) : super(notifier: state);

  /// Subscribes to top-level navigation / overlay changes.
  static AppState of(BuildContext context) {
    final scope = context.dependOnInheritedWidgetOfExactType<AppStateScope>();
    assert(scope != null, 'AppStateScope not found');
    return scope!.notifier!;
  }

  /// Reads AppState WITHOUT registering a widget rebuild dependency (use with ValueListenableBuilder!).
  static AppState read(BuildContext context) {
    final elem = context.getElementForInheritedWidgetOfExactType<AppStateScope>();
    assert(elem != null, 'AppStateScope not found');
    return (elem!.widget as AppStateScope).notifier!;
  }
}
