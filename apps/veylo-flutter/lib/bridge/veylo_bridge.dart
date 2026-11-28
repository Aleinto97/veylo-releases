import 'dart:async';
import 'dart:convert';
import 'dart:ffi';
import 'dart:io';
import 'package:flutter/services.dart' show MethodChannel;
import 'package:path_provider/path_provider.dart';
import 'package:ffi/ffi.dart';

typedef _AsyncCallbackNative = Void Function(Uint64 reqId, Pointer<Utf8> respPtr);
typedef _VeyloCallAsyncNative = Void Function(
  Uint64 reqId,
  Pointer<Utf8> reqJson,
  Pointer<NativeFunction<_AsyncCallbackNative>> cb,
);
typedef _VeyloCallAsyncDart = void Function(
  int reqId,
  Pointer<Utf8> reqJson,
  Pointer<NativeFunction<_AsyncCallbackNative>> cb,
);
typedef _VeyloFreeStringNative = Void Function(Pointer<Utf8> ptr);
typedef _VeyloFreeStringDart = void Function(Pointer<Utf8> ptr);

class VeyloBridge {
  VeyloBridge._() {
    _lib = _openLibrary();
    _callAsync = _lib.lookupFunction<_VeyloCallAsyncNative, _VeyloCallAsyncDart>('veylo_call_async');
    _freeString = _lib.lookupFunction<_VeyloFreeStringNative, _VeyloFreeStringDart>('veylo_free_string');
    _callback = NativeCallable<_AsyncCallbackNative>.listener(_onResponse);
  }

  static final VeyloBridge instance = VeyloBridge._();

  late final DynamicLibrary _lib;
  late final _VeyloCallAsyncDart _callAsync;
  late final _VeyloFreeStringDart _freeString;
  late final NativeCallable<_AsyncCallbackNative> _callback;

  int _nextId = 1;
  final Map<int, Completer<Map<String, dynamic>>> _pending = {};

  static DynamicLibrary _openLibrary() {
    // Android ships it in jniLibs, so the loader finds it by name.
    if (Platform.isAndroid) return DynamicLibrary.open('libveylo_bridge.so');
    final name = Platform.isWindows
        ? 'veylo_bridge.dll'
        : (Platform.isMacOS ? 'libveylo_bridge.dylib' : 'libveylo_bridge.so');
    final exeDir = File(Platform.resolvedExecutable).parent.path;
    final cwd = Directory.current.path;
    final override = Platform.environment['VEYLO_BRIDGE_LIB'];
    final candidates = [
      if (override != null && override.isNotEmpty) override,
      '$exeDir/lib/$name',
      '$exeDir/$name',
      '$exeDir/../Frameworks/$name', // macOS .app bundle
      '$cwd/rust/target/release/$name',
      '$cwd/apps/veylo-flutter/rust/target/release/$name',
      '$cwd/rust/target/debug/$name',
    ];
    final errors = <String>[];
    for (final path in candidates) {
      if (!File(path).existsSync()) continue;
      try {
        return DynamicLibrary.open(path);
      } catch (e) {
        errors.add('$path: $e');
      }
    }
    try {
      return DynamicLibrary.open(name);
    } catch (e) {
      errors.add('$name: $e');
    }
    throw StateError('Could not load $name. Checked:\n${candidates.join('\n')}\n${errors.join('\n')}');
  }


  void _onResponse(int reqId, Pointer<Utf8> respPtr) {
    String raw = '{}';
    try {
      if (respPtr != nullptr) {
        raw = respPtr.toDartString();
      }
    } finally {
      if (respPtr != nullptr) {
        _freeString(respPtr);
      }
    }
    final completer = _pending.remove(reqId);
    if (completer == null || completer.isCompleted) return;
    try {
      final decoded = jsonDecode(raw) as Map<String, dynamic>;
      if (decoded['ok'] == true) {
        completer.complete((decoded['data'] as Map<String, dynamic>?) ?? <String, dynamic>{});
      } else {
        completer.completeError(Exception(decoded['error']?.toString() ?? 'Unknown backend error'));
      }
    } catch (e) {
      completer.completeError(e);
    }
  }

  Future<Map<String, dynamic>> call(String op, [Map<String, dynamic>? args]) {
    final id = _nextId++;
    final completer = Completer<Map<String, dynamic>>();
    _pending[id] = completer;
    final payload = <String, dynamic>{'op': op, ...?args};
    final reqPtr = jsonEncode(payload).toNativeUtf8();
    try {
      _callAsync(id, reqPtr, _callback.nativeFunction);
    } finally {
      malloc.free(reqPtr);
    }
    return completer.future;
  }

  // Convenience typed methods
  Future<Map<String, dynamic>> init() async {
    if (Platform.isAndroid) {
      // The shared backend keeps its store in the app's private files folder.
      final dir = await getApplicationSupportDirectory();
      await call('set_data_dir', {'path': dir.path});
      // Picks AI: model unpacked from the APK + ONNX Runtime from the native libs (MainActivity).
      try {
        final p = await const MethodChannel('veylo/picks').invokeMapMethod<String, String>('paths');
        if (p != null) await call('set_picks_ai_paths', {'model': p['model'], 'runtime': p['runtime']});
      } catch (_) {
        // Rules-only Picks when the model can't be unpacked (e.g. an emulator ABI without ONNX).
      }
    }
    return call('init');
  }

  Future<HomeSnapshot> homeCached() async {
    final res = await call('home_cached');
    List<MediaItem> items(Object? value) => (value as List<dynamic>? ?? [])
        .map((e) => MediaItem.fromJson(e as Map<String, dynamic>))
        .toList();
    return HomeSnapshot(
      rows: (res['rows'] as List<dynamic>? ?? [])
          .map((e) => CatalogRowModel.fromJson(e as Map<String, dynamic>))
          .toList(),
      shelves: (res['shelves'] as Map<String, dynamic>? ?? {}).map((key, value) => MapEntry(key, items(value))),
      tonight: items(res['tonight']),
      artwork: (res['artwork'] as Map<String, dynamic>? ?? {}).map(
        (key, value) => MapEntry(key, ArtworkModel.fromJson(value as Map<String, dynamic>)),
      ),
      library: items(res['library']),
    );
  }

  Future<List<CatalogRowModel>> homeRows() async {
    final res = await call('home_rows');
    return (res['rows'] as List<dynamic>? ?? [])
        .map((e) => CatalogRowModel.fromJson(e as Map<String, dynamic>))
        .toList();
  }

  Future<List<MediaItem>> catalog(CatalogRowModel row) async {
    final res = await call('catalog', {'row': row.toJson()});
    return (res['items'] as List<dynamic>? ?? [])
        .map((e) => MediaItem.fromJson(e as Map<String, dynamic>))
        .toList();
  }

  Future<List<MediaItem>> library() async {
    final res = await call('library');
    return (res['items'] as List<dynamic>? ?? [])
        .map((e) => MediaItem.fromJson(e as Map<String, dynamic>))
        .toList();
  }

  Future<List<MediaItem>> tonightPicks() async {
    final res = await call('tonight_picks');
    return (res['items'] as List<dynamic>? ?? [])
        .map((e) => MediaItem.fromJson(e as Map<String, dynamic>))
        .toList();
  }

  Future<ArtworkModel?> artwork(MediaItem media, {bool home = false}) async {
    final res = await call('artwork', {'media': media.toArg(), if (home) 'home': true});
    final a = res['artwork'];
    if (a is Map<String, dynamic>) return ArtworkModel.fromJson(a);
    return null;
  }

  Future<String?> accent(String url) async {
    if (url.isEmpty) return null;
    final res = await call('accent', {'url': url});
    return res['accent'] as String?;
  }

  Future<String> ambientUrl(String url) async {
    if (url.isEmpty) return '';
    final res = await call('ambient_url', {'url': url});
    return res['url'] as String? ?? url;
  }

  Future<List<SeenItem>> recentViewing({int limit = 4}) async {
    final res = await call('recent_viewing', {'limit': limit});
    return (res['items'] as List<dynamic>? ?? [])
        .map((e) => SeenItem.fromJson(e as Map<String, dynamic>))
        .toList();
  }

  Future<List<SeenItem>> viewingHistory() async {
    final res = await call('viewing_history');
    return (res['items'] as List<dynamic>? ?? [])
        .map((e) => SeenItem.fromJson(e as Map<String, dynamic>))
        .toList();
  }

  Future<Map<String, dynamic>> titleDetails(MediaItem media) =>
      call('title_details', {'media': media.toArg()});

  /// Session-cached artwork/meta only (no network) so a re-opened title paints final layout at once.
  Future<Map<String, dynamic>> titleCached(MediaItem media) =>
      call('title_cached', {'media': media.toArg()});

  /// Studio idents on their own fast path (keyless Wikidata API, cached on disk).
  Future<Map<String, dynamic>> titleStudios(MediaItem media) =>
      call('title_studios', {'media': media.toArg()});

  /// Review scores, today's rank and series status.
  Future<Map<String, dynamic>> titleScores(MediaItem media) =>
      call('title_scores', {'media': media.toArg()});

  /// Collection/saga shelf and "More like this" (the saga lookup can be slow).
  Future<Map<String, dynamic>> titleSimilar(MediaItem media) =>
      call('title_similar', {'media': media.toArg()});

  /// Collection/saga shelf alone (the saga lookup can take 10+ s cold).
  Future<Map<String, dynamic>> titleCollection(MediaItem media) =>
      call('title_collection', {'media': media.toArg()});

  /// Hero art + stills + free series art (posters per season, billing-order cast).
  Future<Map<String, dynamic>> titleArt(MediaItem media) =>
      call('title_art', {'media': media.toArg()});

  /// Warm the caches a title page reads (fire-and-forget), e.g. on card hover.
  // Insertion-ordered: the oldest entries roll off, so prefetching never stops for new titles.
  final Set<String> _prefetched = <String>{};
  void titlePrefetch(MediaItem media) {
    if (!_prefetched.add(media.key)) return;
    if (_prefetched.length > 400) _prefetched.remove(_prefetched.first);
    unawaited(call('title_prefetch', {'media': media.toArg()}).catchError((_) => <String, dynamic>{}));
  }

  /// Slow enrichment layer (Wikidata credits/certificate/studios + studio idents).
  Future<Map<String, dynamic>> titleExtras(MediaItem media) =>
      call('title_extras', {'media': media.toArg()});

  Future<Map<String, dynamic>> search(String query, {bool vibe = false}) =>
      call('search', {'query': query, 'vibe': vibe});

  Future<Map<String, dynamic>> searchHistory() => call('search_history');
  Future<void> rememberQuery(String q) => call('remember_query', {'query': q});
  Future<void> rememberItem(MediaItem m) => call('remember_item', {'media': m.toArg()});
  Future<void> clearHistory() => call('clear_history');

  Future<Map<String, dynamic>> sources(MediaItem media, {String? videoId, int? pack}) =>
      call('sources', {'media': media.toArg(), 'video_id': videoId ?? media.id, 'pack': pack});

  Future<Map<String, dynamic>> playTarget(MediaItem media) =>
      call('play_target', {'media': media.toArg()});

  Future<Map<String, dynamic>> autoplay(MediaItem media, {String? videoId, int? pack, int? ticket}) =>
      call('autoplay', {'media': media.toArg(), 'video_id': videoId ?? media.id, 'pack': pack, 'ticket': ticket});

  Future<void> cancelAutoplay(int ticket) => call('cancel_autoplay', {'ticket': ticket});

  Future<void> feedback(MediaItem media, int value) =>
      call('feedback', {'media': media.toArg(), 'value': value});

  Future<void> setSaved(MediaItem media, bool saved) =>
      call('set_saved', {'media': media.toArg(), 'saved': saved});

  Future<void> setWatched(MediaItem media, bool watched) =>
      call('set_watched', {'media': media.toArg(), 'watched': watched});

  Future<void> setEpisodesWatched(MediaItem media, List<String> ids, bool watched) =>
      call('set_episodes_watched', {'media': media.toArg(), 'video_ids': ids, 'watched': watched});

  Future<void> forget(MediaItem media) => call('forget', {'media': media.toArg()});

  Future<void> recordProgress(MediaItem media, {String? videoId, required int positionMs, required int durationMs, required int watchedMs, bool withMeta = false}) =>
      call('record_progress', {
        'media': media.toArg(),
        'video_id': videoId,
        'position_ms': positionMs,
        'duration_ms': durationMs,
        'watched_ms': watchedMs,
        'with_meta': withMeta,
      });

  Future<void> flushProgress() => call('flush_progress');

  Future<List<Map<String, dynamic>>> onlineSegments(String videoId, double duration) async {
    final res = await call('online_segments', {'video_id': videoId, 'duration': duration});
    return (res['segments'] as List<dynamic>? ?? []).cast<Map<String, dynamic>>();
  }

  /// Download and validate an online subtitle; returns the local file path for the player.
  Future<String> fetchSubtitle(String url) async {
    final r = await call('fetch_subtitle', {'url': url});
    return r['path'] as String;
  }

  Future<List<Map<String, dynamic>>> onlineSubtitles(String kind, String videoId, {String? filename, int? fileSize}) async {
    final res = await call('online_subtitles', {
      'kind': kind,
      'video_id': videoId,
      'filename': filename,
      'file_size': fileSize,
    });
    return (res['subtitles'] as List<dynamic>? ?? []).cast<Map<String, dynamic>>();
  }

  Future<Map<String, dynamic>> downloadsList() => call('downloads_list');
  Future<void> startDownload({
    required MediaItem media,
    required String videoId,
    required String title,
    required String subtitle,
    required String url,
    required List<List<String>> headers,
    String? filename,
  }) =>
      call('start_download', {
        'media': media.toArg(),
        'video_id': videoId,
        'title': title,
        'subtitle': subtitle,
        'url': url,
        'headers': headers,
        'filename': filename,
      });
  Future<void> cancelDownload(String key) => call('cancel_download', {'key': key});
  Future<void> removeDownload(String key) => call('remove_download', {'key': key});

  Future<Map<String, dynamic>> settingsGet() => call('settings_get');
  Future<Map<String, dynamic>> settingsUpdate(Map<String, dynamic> patch) =>
      call('settings_update', {'patch': patch});

  Future<Map<String, dynamic>> addonsOverview() => call('addons_overview');
  Future<Map<String, dynamic>> addonsCheck() => call('addons_check');
  Future<Map<String, dynamic>> addonInstall(String url) => call('addon_install', {'url': url});
  Future<void> addonRemove(int idx) => call('addon_remove', {'idx': idx});
  Future<void> addonUpdate(int idx) => call('addon_update', {'idx': idx});
  Future<void> addonConfigure(int idx) => call('addon_configure', {'idx': idx});
  Future<Map<String, dynamic>> addonImportStremio(String email, String password) =>
      call('addon_import_stremio', {'email': email, 'password': password});
  Future<Map<String, dynamic>> addonExportBundle() => call('addon_export_bundle');
  Future<Map<String, dynamic>> addonImportBundle(String raw) => call('addon_import_bundle', {'raw': raw});
  Future<Map<String, dynamic>> addonFetchDirectory(String url) => call('addon_fetch_directory', {'url': url});
  Future<Map<String, dynamic>> historyExport() => call('history_export');
  Future<Map<String, dynamic>> historyImport(String raw) => call('history_import', {'raw': raw});
  Future<Map<String, dynamic>> diagnosticReport() => call('diagnostic_report');

  /// Forward a Dart/Flutter error to the Rust side: redacted, logged locally, and sent to Sentry
  /// only when Settings → Opt-in diagnostics is on.
  Future<void> reportError(String source, Object error, StackTrace? stack, {bool fatal = false}) async {
    try {
      await call('report_error', {
        'source': source,
        'error': error.toString(),
        'stack': stack?.toString() ?? '',
        'fatal': fatal,
      });
    } catch (_) {}
  }

  /// {available: built with a Sentry DSN, enabled: available and opted in}.
  Future<Map<String, dynamic>> crashReporting() => call('crash_reporting');
  Future<void> flushReports() => call('flush_reports');

  Future<Map<String, dynamic>> accountState() => call('account_state');
  Future<Map<String, dynamic>> accountSignInGoogle() => call('account_sign_in_google');
  Future<Map<String, dynamic>> accountSignIn(String email, String password) =>
      call('account_sign_in', {'email': email, 'password': password});
  Future<Map<String, dynamic>> accountSignUp(String email, String password) =>
      call('account_sign_up', {'email': email, 'password': password});
  Future<void> accountResetPassword(String email) => call('account_reset_password', {'email': email});
  Future<Map<String, dynamic>> accountSyncNow() => call('account_sync_now');
  Future<Map<String, dynamic>> accountSetRecoveryCode(String code) =>
      call('account_set_recovery_code', {'code': code});
  Future<Map<String, dynamic>> accountSignOut() => call('account_sign_out');
  Future<Map<String, dynamic>> accountDelete() => call('account_delete');
  Future<Map<String, dynamic>> accountOfferLink() => call('account_offer_link');
  Future<Map<String, dynamic>> accountTakeLink({int? offerId}) =>
      call('account_take_link', {'offer_id': ?offerId});
  Future<void> accountCancelLink({int? offerId}) =>
      call('account_cancel_link', {'offer_id': ?offerId});
  Future<Map<String, dynamic>> accountTakeKeysArrived() => call('account_take_keys_arrived');

  Future<Map<String, dynamic>> picksLoad({bool savedOnly = false, bool force = false}) =>
      call('picks_load', {'saved_only': savedOnly, 'force': force});
  Future<Map<String, dynamic>> picksChoose({required bool left}) =>
      call('picks_choose', {'left': left});
  Future<Map<String, dynamic>> picksSkip({required bool neither}) =>
      call('picks_skip', {'neither': neither});
  Future<Map<String, dynamic>> picksFilters({
    String? kind,
    String style = 'both',
    List<String> genres = const [],
    String from = '',
    String to = '',
    bool savedOnly = false,
  }) =>
      call('picks_filters', {
        'kind': kind,
        'style': style,
        'genres': genres,
        'from': from,
        'to': to,
        'saved_only': savedOnly,
      });
  Future<Map<String, dynamic>> picksMood({String label = '', String? phrase, bool clear = false}) =>
      call('picks_mood', {'label': label, 'phrase': phrase, 'clear': clear});
  Future<Map<String, dynamic>> picksNewNight() => call('picks_new_night');
  Future<void> picksResetTaste() => call('picks_reset_taste');
  Future<Map<String, dynamic>> picksUnstar(String key) => call('picks_unstar', {'key': key});
  Future<Map<String, dynamic>> picksCardExtras(MediaItem media) =>
      call('picks_card_extras', {'media': media.toArg()});
  Future<Map<String, dynamic>> picksPlayed(MediaItem media) =>
      call('picks_played', {'media': media.toArg()});
  Future<Map<String, dynamic>> picksRate(MediaItem media, int value) =>
      call('picks_rate', {'media': media.toArg(), 'value': value});
  Future<void> openExternal(String url) => call('open_external', {'url': url});
  Future<Map<String, dynamic>> youtubeStream(String id) => call('youtube_stream', {'id': id});
}

class MediaItem {
  const MediaItem({
    required this.key,
    required this.id,
    required this.kind,
    required this.name,
    required this.addonUrl,
    required this.poster,
    required this.posters,
    required this.background,
    required this.backdrop,
    required this.description,
    required this.year,
    required this.genres,
    required this.reason,
    required this.liked,
    required this.watched,
    required this.saved,
    required this.videoWatched,
    required this.positionMs,
    required this.durationMs,
    required this.lastVideoId,
    required this.inProgress,
    required this.progressFraction,
    required this.resumeContext,
    required this.episodeLabel,
    required this.metaLine,
    required this.metaLineTyped,
    required this.rating,
    required this.runtime,
    required this.logo,
    required this.raw,
  });

  final String key;
  final String id;
  final String kind;
  final String name;
  final String addonUrl;
  final String poster;
  final List<String> posters;
  final String background;
  final String backdrop;
  final String description;
  final String year;
  final List<String> genres;
  final String reason;
  final int liked;
  final bool watched;
  final bool saved;
  final bool videoWatched;
  final int positionMs;
  final int durationMs;
  final String? lastVideoId;
  final bool inProgress;
  final double progressFraction;
  final String resumeContext;
  final String? episodeLabel;
  final String metaLine;
  final String metaLineTyped;
  final String rating;
  final String runtime;
  final String logo;
  final Map<String, dynamic> raw;

  factory MediaItem.fromJson(Map<String, dynamic> j) => MediaItem(
        key: j['key'] as String? ?? '',
        id: j['id'] as String? ?? '',
        kind: j['type'] as String? ?? 'movie',
        name: j['name'] as String? ?? 'Untitled',
        addonUrl: j['addon_url'] as String? ?? '',
        poster: j['poster'] as String? ?? '',
        posters: (j['posters'] as List<dynamic>? ?? []).map((e) => e.toString()).toList(),
        background: j['background'] as String? ?? '',
        backdrop: j['backdrop'] as String? ?? '',
        description: j['description'] as String? ?? '',
        year: j['year'] as String? ?? '',
        genres: (j['genres'] as List<dynamic>? ?? []).map((e) => e.toString()).toList(),
        reason: j['reason'] as String? ?? '',
        liked: (j['liked'] as num?)?.toInt() ?? 0,
        watched: j['watched'] as bool? ?? false,
        saved: j['saved'] as bool? ?? false,
        videoWatched: j['video_watched'] as bool? ?? false,
        positionMs: (j['position_ms'] as num?)?.toInt() ?? 0,
        durationMs: (j['duration_ms'] as num?)?.toInt() ?? 0,
        lastVideoId: j['last_video_id'] as String?,
        inProgress: j['in_progress'] as bool? ?? false,
        progressFraction: (j['progress_fraction'] as num?)?.toDouble() ?? 0.0,
        resumeContext: j['resume_context'] as String? ?? '',
        episodeLabel: j['episode_label'] as String?,
        metaLine: j['meta_line'] as String? ?? '',
        metaLineTyped: j['meta_line_typed'] as String? ?? '',
        rating: j['rating'] as String? ?? '',
        runtime: j['runtime'] as String? ?? '',
        logo: j['logo'] as String? ?? '',
        raw: (j['raw'] as Map<String, dynamic>?) ?? <String, dynamic>{},
      );

  Map<String, dynamic> toArg() => {
        'id': id,
        'type': kind,
        'name': name,
        'addon_url': addonUrl,
        'poster': poster,
        'background': background,
        'description': description,
        'releaseInfo': year,
        'genres': genres,
        'liked': liked,
        'watched': watched,
        'saved': saved,
        'video_watched': videoWatched,
        'position_ms': positionMs,
        'duration_ms': durationMs,
        'last_video_id': lastVideoId,
        'raw': raw,
      };
}

class HomeSnapshot {
  const HomeSnapshot({
    required this.rows,
    required this.shelves,
    required this.tonight,
    required this.artwork,
    required this.library,
  });

  final List<CatalogRowModel> rows;
  final Map<String, List<MediaItem>> shelves;
  final List<MediaItem> tonight;
  final Map<String, ArtworkModel> artwork;
  final List<MediaItem> library;
}

class CatalogRowModel {
  const CatalogRowModel({
    required this.key,
    required this.title,
    this.subtitle,
    required this.addonUrl,
    required this.kind,
    required this.id,
    this.extra,
    required this.ranked,
  });

  final String key;
  final String title;
  final String? subtitle;
  final String addonUrl;
  final String kind;
  final String id;
  final List<String>? extra;
  final bool ranked;

  factory CatalogRowModel.fromJson(Map<String, dynamic> j) => CatalogRowModel(
        key: j['key'] as String? ?? '',
        title: j['title'] as String? ?? '',
        subtitle: j['subtitle'] as String?,
        addonUrl: j['addon_url'] as String? ?? '',
        kind: j['kind'] as String? ?? '',
        id: j['id'] as String? ?? '',
        extra: (j['extra'] as List<dynamic>?)?.map((e) => e.toString()).toList(),
        ranked: j['ranked'] as bool? ?? false,
      );

  Map<String, dynamic> toJson() => {
        'key': key,
        'title': title,
        'subtitle': subtitle,
        'addon_url': addonUrl,
        'kind': kind,
        'id': id,
        'extra': extra,
        'ranked': ranked,
      };
}

class ArtworkModel {
  const ArtworkModel({
    this.backdrop,
    this.logo,
    this.poster,
    this.tagline,
    this.certificate,
    this.stills = const [],
    this.trailers = const [],
    this.seasonPosters = const {},
    this.cast = const [],
    this.crew = const [],
    this.studios = const [],
    this.videos = const [],
  });

  final String? backdrop;
  final String? logo;
  final String? poster;
  final String? tagline;
  final String? certificate;
  final List<String> stills;
  final List<String> trailers;
  final Map<int, String> seasonPosters;
  final List<Map<String, dynamic>> cast;
  final List<Map<String, dynamic>> crew;
  final List<Map<String, dynamic>> studios;
  final List<Map<String, dynamic>> videos;

  factory ArtworkModel.fromJson(Map<String, dynamic> j) {
    final spRaw = j['season_posters'] as Map<String, dynamic>? ?? {};
    final sp = <int, String>{};
    for (final e in spRaw.entries) {
      final k = int.tryParse(e.key);
      if (k != null && e.value is String) sp[k] = e.value as String;
    }
    return ArtworkModel(
      backdrop: j['backdrop'] as String?,
      logo: j['logo'] as String?,
      poster: j['poster'] as String?,
      tagline: j['tagline'] as String?,
      certificate: j['certificate'] as String?,
      stills: (j['stills'] as List<dynamic>? ?? []).map((e) => e.toString()).toList(),
      trailers: (j['trailers'] as List<dynamic>? ?? []).map((e) => e.toString()).toList(),
      seasonPosters: sp,
      cast: (j['cast'] as List<dynamic>? ?? []).cast<Map<String, dynamic>>(),
      crew: (j['crew'] as List<dynamic>? ?? []).cast<Map<String, dynamic>>(),
      studios: (j['studios'] as List<dynamic>? ?? []).cast<Map<String, dynamic>>(),
      videos: (j['videos'] as List<dynamic>? ?? []).cast<Map<String, dynamic>>(),
    );
  }
}

class SeenItem {
  const SeenItem({
    required this.media,
    required this.at,
    this.episode,
    required this.whenLabel,
    this.isThisWeek = false,
    this.year = 0,
    this.month = 0,
    this.monthLabel = '',
  });

  final MediaItem media;
  final int at;
  final String? episode;
  final String whenLabel;
  final bool isThisWeek;
  final int year;
  final int month;
  final String monthLabel;

  factory SeenItem.fromJson(Map<String, dynamic> j) => SeenItem(
        media: MediaItem.fromJson(j['media'] as Map<String, dynamic>),
        at: (j['at'] as num?)?.toInt() ?? 0,
        episode: j['episode'] as String?,
        whenLabel: j['when_label'] as String? ?? '',
        isThisWeek: j['is_this_week'] as bool? ?? false,
        year: (j['year'] as num?)?.toInt() ?? 0,
        month: (j['month'] as num?)?.toInt() ?? 0,
        monthLabel: j['month_label'] as String? ?? '',
      );
}

class SourceItem {
  const SourceItem({
    required this.addon,
    required this.name,
    required this.description,
    required this.url,
    this.audioUrl,
    required this.filename,
    this.size,
    this.sizeBytes,
    this.quality,
    this.hdr,
    required this.kind,
    required this.headers,
    required this.offTitle,
    this.verified = false,
    this.bitrateMbps,
    this.videoCodec,
  });

  final String addon;
  final String name;
  final String description;
  final String url;
  final String? audioUrl;
  final String filename;
  final String? size;
  final int? sizeBytes;
  final String? quality;
  final String? hdr;
  final String kind;
  final Map<String, String> headers;
  final bool offTitle;
  final bool verified;
  final double? bitrateMbps;
  final String? videoCodec;

  factory SourceItem.fromJson(Map<String, dynamic> j) {
    final hdrs = <String, String>{};
    for (final pair in (j['headers'] as List<dynamic>? ?? [])) {
      if (pair is List && pair.length >= 2) {
        hdrs[pair[0].toString()] = pair[1].toString();
      }
    }
    return SourceItem(
      addon: j['addon'] as String? ?? '',
      name: j['name'] as String? ?? '',
      description: j['description'] as String? ?? '',
      url: j['url'] as String? ?? '',
      audioUrl: (j['audio_url'] as String?)?.isNotEmpty == true ? j['audio_url'] as String : null,
      filename: j['filename'] as String? ?? '',
      size: j['size'] as String?,
      sizeBytes: (j['size_bytes'] as num?)?.toInt(),
      quality: j['quality'] as String?,
      hdr: j['hdr'] as String?,
      kind: j['kind'] as String? ?? 'direct',
      headers: hdrs,
      offTitle: j['off_title'] as bool? ?? false,
      verified: j['verified'] as bool? ?? false,
      bitrateMbps: (j['bitrate_mbps'] as num?)?.toDouble(),
      videoCodec: j['video_codec'] as String?,
    );
  }
}

class EpisodeItem {
  const EpisodeItem({
    required this.id,
    required this.season,
    required this.number,
    required this.title,
    required this.thumbnail,
    required this.overview,
    required this.released,
    required this.dateLabel,
    required this.label,
  });

  final String id;
  final int season;
  final int number;
  final String title;
  final String thumbnail;
  final String overview;
  final String released;
  final String dateLabel;
  final String label;

  factory EpisodeItem.fromJson(Map<String, dynamic> j) => EpisodeItem(
        id: j['id'] as String? ?? '',
        season: (j['season'] as num?)?.toInt() ?? 0,
        number: (j['number'] as num?)?.toInt() ?? 0,
        title: j['title'] as String? ?? '',
        thumbnail: j['thumbnail'] as String? ?? '',
        overview: j['overview'] as String? ?? '',
        released: j['released'] as String? ?? '',
        dateLabel: j['date_label'] as String? ?? '',
        label: j['label'] as String? ?? '',
      );
}

class TrailerClip {
  const TrailerClip({
    this.youtube,
    this.url,
    required this.name,
    this.kind = 'Trailer',
    this.official = false,
  });

  final String? youtube;
  final String? url;
  final String name;
  final String kind;
  final bool official;

  String get id => youtube ?? url ?? name;

  String get label {
    final k = kind == 'Behind the Scenes'
        ? 'Behind the scenes'
        : (kind.isEmpty ? 'Video' : kind);
    if (official && (kind == 'Trailer' || kind == 'Teaser')) {
      return 'Official ${k.toLowerCase()}';
    }
    return k;
  }
}

class TheatreAbout {
  const TheatreAbout({
    required this.title,
    this.backdrop = '',
    this.logo,
    this.facts = const [],
    this.rating,
    this.tagline,
    this.directorRole,
    this.directors = const [],
    this.cast = const [],
  });

  final String title;
  final String backdrop;
  final String? logo;
  final List<String> facts;
  final String? rating;
  final String? tagline;
  final String? directorRole;
  final List<String> directors;
  final List<String> cast;
}

class TheatreSession {
  const TheatreSession({
    required this.about,
    required this.clips,
    this.initialIndex = 0,
  });

  final TheatreAbout about;
  final List<TrailerClip> clips;
  final int initialIndex;
}

class PlaySession {
  const PlaySession({
    required this.title,
    required this.subtitle,
    required this.url,
    this.audioUrl,
    this.headers = const {},
    this.startMs = 0,
    this.size,
    this.hdr,
    this.quality,
    this.filename,
    this.fileSize,
    this.media,
    this.videoId,
    this.sources = const [],
    this.episodes = const [],
    this.nextEpisode,
    this.initialSubtitles = const [],
    this.clips = const [],
    this.clipIndex = 0,
    this.clipContextTitle,
    this.originalLanguage,
  });

  final String title;
  final String subtitle;
  final String url;
  final String? audioUrl;
  final Map<String, String> headers;
  final int startMs;
  final String? size;
  final String? hdr;
  final String? quality;
  final String? filename;
  final int? fileSize;
  final MediaItem? media;
  final String? videoId;
  final List<SourceItem> sources;
  final List<EpisodeItem> episodes;
  final EpisodeItem? nextEpisode;
  final List<Map<String, dynamic>> initialSubtitles;
  final List<TrailerClip> clips;
  final int clipIndex;
  final String? clipContextTitle;
  final String? originalLanguage;
}
