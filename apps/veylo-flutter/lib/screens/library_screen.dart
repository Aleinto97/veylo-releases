import 'dart:async';
import 'dart:ui' show PointerDeviceKind;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:material_ui/material_ui.dart' show InputDecoration, TextField;
import '../bridge/veylo_bridge.dart';
import '../state/app_state.dart';
import '../theme/veylo_theme.dart';
import '../widgets/glass.dart';
import '../widgets/veylo_icons.dart';

enum _LibFilter { all, movies, series, inProgress, watched, saved, downloads }

extension on _LibFilter {
  String get label => switch (this) {
        _LibFilter.all => 'All',
        _LibFilter.movies => 'Movies',
        _LibFilter.series => 'Series',
        _LibFilter.inProgress => 'In progress',
        _LibFilter.watched => 'Watched',
        _LibFilter.saved => 'Saved',
        _LibFilter.downloads => 'Downloads',
      };

  bool matches(MediaItem m) => switch (this) {
        _LibFilter.all => true,
        _LibFilter.movies => m.kind == 'movie',
        _LibFilter.series => m.kind != 'movie',
        _LibFilter.inProgress => m.inProgress,
        _LibFilter.watched => m.watched,
        _LibFilter.saved => m.saved,
        _LibFilter.downloads => false,
      };
}

class LibraryScreen extends StatefulWidget {
  const LibraryScreen({super.key});

  @override
  State<LibraryScreen> createState() => _LibraryScreenState();
}

class _LibraryScreenState extends State<LibraryScreen> {
  final ScrollController _scroll = ScrollController();
  final TextEditingController _queryCtrl = TextEditingController();
  final ValueNotifier<String?> _armedKey = ValueNotifier<String?>(null);
  _LibFilter _filter = _LibFilter.all;
  String _query = '';

  List<SeenItem>? _history;
  List<MediaItem> _saved = [];
  List<Map<String, dynamic>> _downloads = [];
  List<Map<String, dynamic>> _activeDownloads = [];
  String? _error;
  final Set<String> _gone = {};
  final Set<String> _unsaved = {};
  int _lastDirty = -1;
  Timer? _dlTimer;
  AppState? _appRef;

  @override
  void initState() {
    super.initState();
    _scroll.addListener(_onScroll);
    _dlTimer = Timer.periodic(const Duration(seconds: 2), (_) => _refreshDownloads());
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      final app = AppStateScope.read(context);
      _appRef = app;
      app.picksDirtyNotifier.addListener(_onPicksDirty);
      unawaited(app.setLampSource(''));
      _lastDirty = app.picksDirty;
      unawaited(_load());
    });
  }

  void _onPicksDirty() {
    if (!mounted || _appRef == null) return;
    if (_lastDirty != _appRef!.picksDirty) {
      _lastDirty = _appRef!.picksDirty;
      unawaited(_load());
    }
  }

  void _onScroll() {
    if (!mounted) return;
    AppStateScope.read(context).setScrolled(_scroll.offset > 4);
    if (_armedKey.value != null) _armedKey.value = null;
  }

  @override
  void dispose() {
    _appRef?.picksDirtyNotifier.removeListener(_onPicksDirty);
    _dlTimer?.cancel();
    _scroll.removeListener(_onScroll);
    _scroll.dispose();
    _queryCtrl.dispose();
    _armedKey.dispose();
    super.dispose();
  }

  Future<void> _load() async {
    final app = AppStateScope.read(context);
    final bridge = app.bridge;
    try {
      final results = await Future.wait([
        bridge.viewingHistory(),
        bridge.library(),
        bridge.downloadsList(),
      ]);
      if (!mounted) return;
      final hist = results[0] as List<SeenItem>;
      final lib = results[1] as List<MediaItem>;
      final dl = results[2] as Map<String, dynamic>;
      setState(() {
        _history = hist;
        _saved = lib.where((m) => m.saved).toList();
        _downloads = (dl['downloads'] as List<dynamic>? ?? []).cast<Map<String, dynamic>>();
        _activeDownloads = (dl['active'] as List<dynamic>? ?? []).cast<Map<String, dynamic>>();
        _gone.clear();
        _unsaved.clear();
        _error = null;
      });
    } catch (e) {
      if (!mounted) return;
      if (_history == null) {
        setState(() => _error = e.toString());
      } else {
        app.showToast('Could not refresh Library: $e', isError: true);
      }
    }
  }

  Future<void> _refreshDownloads() async {
    if (!mounted) return;
    final app = AppStateScope.read(context);
    if (app.activePlayer != null) return;
    if (_activeDownloads.isEmpty && _filter != _LibFilter.downloads) return;
    try {
      final dl = await app.bridge.downloadsList();
      if (!mounted) return;
      final nextDl = (dl['downloads'] as List<dynamic>? ?? []).cast<Map<String, dynamic>>();
      final nextAct = (dl['active'] as List<dynamic>? ?? []).cast<Map<String, dynamic>>();
      if (nextDl.length != _downloads.length || nextAct.isNotEmpty || _activeDownloads.isNotEmpty) {
        setState(() {
          _downloads = nextDl;
          _activeDownloads = nextAct;
        });
      }
    } catch (_) {}
  }

  String _formatBytes(int bytes) {
    if (bytes >= 1073741824) return '${(bytes / 1073741824).toStringAsFixed(1)} GB';
    if (bytes >= 1048576) return '${(bytes / 1048576).round()} MB';
    if (bytes >= 1024) return '${(bytes / 1024).round()} KB';
    return '$bytes B';
  }

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final width = MediaQuery.sizeOf(context).width;
    final gutter = VeyloLayout.gutter(width);
    final contentWidth = (width - gutter * 2).clamp(240.0, 2400.0);
    final posterCols = (contentWidth / 156).floor().clamp(2, 10);
    final weekCols = (contentWidth / 340).floor().clamp(1, 4);

    final q = _query.trim().toLowerCase();
    bool textMatch(String name) => q.isEmpty || name.toLowerCase().contains(q);

    final liveHistory = (_history ?? []).where((s) => !_gone.contains(s.media.key)).toList();
    final liveSaved = _saved.where((m) => !_gone.contains(m.key) && !_unsaved.contains(m.key)).toList();

    final wm = liveHistory.where((s) => s.media.kind == 'movie' && s.media.watched).length;
    final ws = liveHistory.where((s) => s.media.kind != 'movie' && s.media.watched).length;
    final ip = liveHistory.where((s) => s.media.inProgress).length;
    final dlBytes = _downloads.fold<int>(0, (acc, d) => acc + ((d['bytes'] as num?)?.toInt() ?? 0));
    final statsParts = <String>[
      if (wm > 0) '$wm film${wm == 1 ? '' : 's'} watched',
      if (ws > 0) '$ws series finished',
      if (ip > 0) '$ip in progress',
      if (liveSaved.isNotEmpty) '${liveSaved.length} saved',
      if (dlBytes > 0) '${_formatBytes(dlBytes)} offline',
    ];
    final statsLine = statsParts.join(' · ');

    final dlList = _downloads.where((d) => textMatch(d['title']?.toString() ?? '')).toList();
    final actList = _activeDownloads.where((d) => textMatch(d['title']?.toString() ?? '')).toList();
    final showDownloads = _filter == _LibFilter.downloads || (_filter == _LibFilter.all && (dlList.isNotEmpty || actList.isNotEmpty));
    final showSaved = {_LibFilter.all, _LibFilter.saved, _LibFilter.movies, _LibFilter.series}.contains(_filter);
    final showHistory = _filter != _LibFilter.downloads && _filter != _LibFilter.saved;

    final filteredSaved = liveSaved.where((m) => _filter.matches(m) && textMatch(m.name)).toList();
    final filteredHistory = liveHistory.where((s) => _filter.matches(s.media) && textMatch(s.media.name)).toList();
    final week = filteredHistory.where((s) => s.isThisWeek).toList();
    final months = <String, List<SeenItem>>{};
    for (final s in filteredHistory.where((s) => !s.isThisWeek)) {
      months.putIfAbsent(s.monthLabel, () => []).add(s);
    }

    return CallbackShortcuts(
      bindings: {
        const SingleActivator(LogicalKeyboardKey.escape): () {
          if (_armedKey.value != null) _armedKey.value = null;
        },
      },
      child: GestureDetector(
        behavior: HitTestBehavior.translucent,
        onTap: () {
          if (_armedKey.value != null) _armedKey.value = null;
        },
        child: CustomScrollView(
          controller: _scroll,
          slivers: [
            // Header + CSS-faithful Library toolbar (.lib-head + .lib-toolbar)
            SliverPadding(
              padding: EdgeInsets.fromLTRB(gutter, 96, gutter, 0),
              sliver: SliverToBoxAdapter(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    // .lib-head: baseline Row when fits, wraps stats left-aligned 10px below when narrow
                    SizedBox(
                      width: double.infinity,
                      child: Wrap(
                        alignment: WrapAlignment.spaceBetween,
                        crossAxisAlignment: WrapCrossAlignment.end,
                        spacing: 16,
                        runSpacing: 10,
                        children: [
                          const Text('Library', style: VeyloText.title),
                          if (statsLine.isNotEmpty)
                            Padding(
                              padding: const EdgeInsets.only(bottom: 3),
                              child: Text(
                                statsLine,
                                style: VeyloText.ui(13.5, weight: 500, color: VeyloColors.dust, tabular: true),
                              ),
                            ),
                        ],
                      ),
                    ),
                    const SizedBox(height: 14),

                    // .lib-toolbar: single horizontal-scrolling filter strip + flush-right search (220..300px, full-width <= 680)
                    _LibraryToolbar(
                      viewportWidth: width,
                      activeFilter: _filter,
                      onSelectFilter: (opt) {
                        _armedKey.value = null;
                        setState(() => _filter = opt);
                      },
                      queryController: _queryCtrl,
                      onQueryChanged: (v) => setState(() => _query = v),
                    ),
                  ],
                ),
              ),
            ),

            // Downloads section
            if (showDownloads)
              SliverPadding(
                padding: EdgeInsets.fromLTRB(gutter, 26, gutter, 0),
                sliver: SliverToBoxAdapter(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      GroupHead(icon: 'Download', title: 'Downloads', count: dlList.length),
                      const SizedBox(height: 14),
                      if (dlList.isEmpty && actList.isEmpty)
                        const _EmptyCard(
                          icon: 'Download',
                          title: 'No offline downloads yet',
                          subtitle: 'Open any title\'s Sources list and tap Download to save it for offline playback.',
                        )
                      else
                        Column(
                          children: [
                            for (final job in actList) _buildActiveDownloadRow(app, job),
                            for (final entry in dlList) _buildDownloadRow(app, entry),
                          ],
                        ),
                    ],
                  ),
                ),
              ),

            // Saved section (lazy SliverGrid)
            if (showSaved && filteredSaved.isNotEmpty) ...[
              SliverPadding(
                padding: EdgeInsets.fromLTRB(gutter, 26, gutter, 14),
                sliver: SliverToBoxAdapter(
                  child: GroupHead(
                    icon: 'Bookmark',
                    title: 'Saved',
                    count: filteredSaved.length,
                    trailing: filteredSaved.length >= 2
                        ? VeyloPill(
                            label: 'Pick from Saved',
                            icon: 'Sparkles',
                            small: true,
                            onTap: () {
                              app.picksSavedOnly = true;
                              app.setTab(VeyloTab.picks);
                            },
                          )
                        : null,
                  ),
                ),
              ),
              SliverPadding(
                padding: EdgeInsets.symmetric(horizontal: gutter),
                sliver: SliverGrid.builder(
                  gridDelegate: SliverGridDelegateWithFixedCrossAxisCount(
                    crossAxisCount: posterCols,
                    crossAxisSpacing: 16,
                    mainAxisSpacing: 22,
                    childAspectRatio: 2 / 3,
                  ),
                  itemCount: filteredSaved.length,
                  itemBuilder: (_, i) {
                    final m = filteredSaved[i];
                    return _LibGridItem(
                      key: ValueKey('saved-${m.key}'),
                      cardId: 'saved-${m.key}',
                      armedKey: _armedKey,
                      media: m,
                      actionLabel: 'Unsave',
                      onRemove: () async {
                        await app.bridge.setSaved(m, false);
                        if (!mounted) return;
                        setState(() => _unsaved.add(m.key));
                        app.bumpPicksDirty();
                      },
                    );
                  },
                ),
              ),
            ] else if (showSaved && _filter == _LibFilter.saved)
              SliverPadding(
                padding: EdgeInsets.fromLTRB(gutter, 26, gutter, 0),
                sliver: SliverToBoxAdapter(
                  child: _EmptyCard(
                    icon: 'Bookmark',
                    title: q.isEmpty ? 'No saved titles yet' : 'No saved titles match',
                    subtitle: q.isEmpty
                        ? 'Bookmark any film or series to build your watchlist.'
                        : 'Try a different filter or search phrase.',
                    action: q.isNotEmpty
                        ? VeyloPill(
                            label: 'Clear search',
                            icon: 'Undo',
                            small: true,
                            onTap: () {
                              _queryCtrl.clear();
                              setState(() => _query = '');
                            },
                          )
                        : null,
                  ),
                ),
              ),

            // Viewing history
            if (showHistory) ...[
              if (_error != null)
                SliverPadding(
                  padding: EdgeInsets.fromLTRB(gutter, 24, gutter, 0),
                  sliver: SliverToBoxAdapter(
                    child: Align(
                      alignment: Alignment.centerLeft,
                      child: GlassBox(
                        borderRadius: 28,
                        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 10),
                        child: Row(
                          mainAxisSize: MainAxisSize.min,
                          children: [
                            const VeyloIcon('Alert', size: 18, color: VeyloColors.amber),
                            const SizedBox(width: 10),
                            Text(_error!, style: VeyloText.meta),
                          ],
                        ),
                      ),
                    ),
                  ),
                )
              else if (_history == null)
                SliverPadding(
                  padding: EdgeInsets.fromLTRB(gutter, 26, gutter, 0),
                  sliver: const SliverToBoxAdapter(
                    child: Wrap(
                      spacing: 18,
                      runSpacing: 18,
                      children: [
                        SkeletonBox(width: 320, height: 180, borderRadius: 18),
                        SkeletonBox(width: 320, height: 180, borderRadius: 18),
                        SkeletonBox(width: 320, height: 180, borderRadius: 18),
                      ],
                    ),
                  ),
                )
              else if (_history!.isEmpty && liveSaved.isEmpty)
                SliverPadding(
                  padding: EdgeInsets.fromLTRB(gutter, 80, gutter, 0),
                  sliver: SliverToBoxAdapter(
                    child: Center(
                      child: GlassBox(
                        borderRadius: 24,
                        padding: const EdgeInsets.symmetric(horizontal: 36, vertical: 28),
                        child: const Column(
                          mainAxisSize: MainAxisSize.min,
                          children: [
                            VeyloIcon('Stack', size: 30, color: VeyloColors.dust),
                            SizedBox(height: 10),
                            Text('Nothing watched yet', style: VeyloText.body),
                          ],
                        ),
                      ),
                    ),
                  ),
                )
              else ...[
                if (filteredHistory.isEmpty && _history!.isNotEmpty)
                  SliverPadding(
                    padding: EdgeInsets.fromLTRB(gutter, 26, gutter, 0),
                    sliver: SliverToBoxAdapter(
                      child: _EmptyCard(
                        icon: 'Search',
                        title: 'No titles match this filter',
                        subtitle: 'Reset the filter or search query to see your full library.',
                        action: VeyloPill(
                          label: 'Show all',
                          icon: 'Undo',
                          small: true,
                          onTap: () {
                            _queryCtrl.clear();
                            setState(() {
                              _filter = _LibFilter.all;
                              _query = '';
                            });
                          },
                        ),
                      ),
                    ),
                  ),
                if (week.isNotEmpty) ...[
                  SliverPadding(
                    padding: EdgeInsets.fromLTRB(gutter, 26, gutter, 14),
                    sliver: SliverToBoxAdapter(child: GroupHead(icon: 'Clock', title: 'This week', count: week.length)),
                  ),
                  SliverPadding(
                    padding: EdgeInsets.symmetric(horizontal: gutter),
                    sliver: SliverGrid.builder(
                      gridDelegate: SliverGridDelegateWithFixedCrossAxisCount(
                        crossAxisCount: weekCols,
                        crossAxisSpacing: 18,
                        mainAxisSpacing: 18,
                        childAspectRatio: 16 / 9,
                      ),
                      itemCount: week.length,
                      itemBuilder: (_, i) => _WeekCard(
                        key: ValueKey('week-${week[i].media.key}'),
                        cardId: 'week-${week[i].media.key}',
                        armedKey: _armedKey,
                        seen: week[i],
                        onForget: () async {
                          final m = week[i].media;
                          await app.forgetTitle(m);
                          if (!mounted) return;
                          setState(() => _gone.add(m.key));
                        },
                      ),
                    ),
                  ),
                ],
                for (final entry in months.entries) ...[
                  SliverPadding(
                    padding: EdgeInsets.fromLTRB(gutter, 28, gutter, 14),
                    sliver: SliverToBoxAdapter(child: GroupHead(icon: 'Calendar', title: entry.key, count: entry.value.length)),
                  ),
                  SliverPadding(
                    padding: EdgeInsets.symmetric(horizontal: gutter),
                    sliver: SliverGrid.builder(
                      gridDelegate: SliverGridDelegateWithFixedCrossAxisCount(
                        crossAxisCount: posterCols,
                        crossAxisSpacing: 16,
                        mainAxisSpacing: 22,
                        childAspectRatio: 2 / 3,
                      ),
                      itemCount: entry.value.length,
                      itemBuilder: (_, i) {
                        final m = entry.value[i].media;
                        return _LibGridItem(
                          key: ValueKey('hist-${m.key}'),
                          cardId: 'hist-${m.key}',
                          armedKey: _armedKey,
                          media: m,
                          actionLabel: 'Remove',
                          onRemove: () async {
                            await app.forgetTitle(m);
                            if (!mounted) return;
                            setState(() => _gone.add(m.key));
                          },
                        );
                      },
                    ),
                  ),
                ],
              ],
            ],

            const SliverPadding(padding: EdgeInsets.only(bottom: 64)),
          ],
        ),
      ),
    );
  }

  Widget _buildActiveDownloadRow(AppState app, Map<String, dynamic> job) {
    final dl = (job['downloaded'] as num?)?.toDouble() ?? 0;
    final tot = (job['total'] as num?)?.toDouble();
    final pct = (tot != null && tot > 0) ? (dl / tot).clamp(0.0, 1.0) : null;
    final failed = job['state'] == 'failed';
    final status = failed
        ? 'Failed: ${job['error'] ?? ''}'
        : (pct != null ? '${(pct * 100).round()}% · ${job['downloaded_label']} / ${job['total_label']}' : 'Downloading… ${job['downloaded_label']}');
    return Padding(
      padding: const EdgeInsets.only(bottom: 8),
      child: GlassBox(
        borderRadius: 16,
        padding: const EdgeInsets.fromLTRB(18, 14, 14, 14),
        child: Column(
          children: [
            Row(
              children: [
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(job['title']?.toString() ?? '', style: VeyloText.ui(15, weight: 600)),
                      Text(status, style: VeyloText.ui(13, weight: 500, color: VeyloColors.dust, tabular: true)),
                    ],
                  ),
                ),
                VeyloPill(
                  label: failed ? 'Dismiss' : 'Cancel',
                  icon: 'Close',
                  small: true,
                  onTap: () async {
                    await app.bridge.cancelDownload(job['key'].toString());
                    unawaited(_refreshDownloads());
                  },
                ),
              ],
            ),
            if (pct != null) ...[
              const SizedBox(height: 10),
              ValueListenableBuilder<Color>(
                valueListenable: app.lampColorNotifier,
                builder: (_, lamp, _) => Container(
                  height: 3,
                  color: const Color.fromRGBO(255, 255, 255, 0.08),
                  alignment: Alignment.centerLeft,
                  child: FractionallySizedBox(widthFactor: pct, child: ColoredBox(color: lamp)),
                ),
              ),
            ],
          ],
        ),
      ),
    );
  }

  Widget _buildDownloadRow(AppState app, Map<String, dynamic> entry) {
    return Padding(
      padding: const EdgeInsets.only(bottom: 8),
      child: GlassBox(
        borderRadius: 16,
        padding: const EdgeInsets.fromLTRB(18, 12, 14, 12),
        child: Row(
          children: [
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(entry['title']?.toString() ?? '', style: VeyloText.ui(15, weight: 600)),
                  Text(
                    '${entry['subtitle'] ?? ''} · ${entry['size_label'] ?? ''}',
                    style: VeyloText.ui(13, weight: 500, color: VeyloColors.dust, tabular: true),
                  ),
                ],
              ),
            ),
            VeyloPill(
              label: 'Play',
              icon: 'Play',
              accent: true,
              small: true,
              onTap: () {
                final m = MediaItem.fromJson(entry['media'] as Map<String, dynamic>);
                app.startPlaySession(PlaySession(
                  title: entry['title']?.toString() ?? m.name,
                  subtitle: 'Offline · ${entry['subtitle'] ?? ''}',
                  url: entry['path']?.toString() ?? '',
                  media: m,
                  videoId: entry['video_id']?.toString(),
                ));
              },
            ),
            const SizedBox(width: 8),
            VeyloOrb(
              icon: 'Trash',
              size: 38,
              tooltip: 'Delete download',
              onTap: () async {
                await app.bridge.removeDownload(entry['key'].toString());
                unawaited(_refreshDownloads());
              },
            ),
          ],
        ),
      ),
    );
  }
}

/// Exact CSS `.lib-toolbar`: single-row horizontal-scrolling `.lib-filters` + flush-right `.lib-search`
/// (`flex: 0 1 300px; min-width: 220px`, wrapping cleanly on narrow screens and full-width <= 680px).
class _LibraryToolbar extends StatelessWidget {
  const _LibraryToolbar({
    required this.viewportWidth,
    required this.activeFilter,
    required this.onSelectFilter,
    required this.queryController,
    required this.onQueryChanged,
  });

  final double viewportWidth;
  final _LibFilter activeFilter;
  final ValueChanged<_LibFilter> onSelectFilter;
  final TextEditingController queryController;
  final ValueChanged<String> onQueryChanged;

  @override
  Widget build(BuildContext context) {
    final filterStrip = Transform.translate(
      offset: const Offset(-2, 0),
      child: ScrollConfiguration(
        behavior: ScrollConfiguration.of(context).copyWith(
          scrollbars: false,
          dragDevices: const {
            PointerDeviceKind.touch,
            PointerDeviceKind.mouse,
            PointerDeviceKind.trackpad,
          },
        ),
        child: SingleChildScrollView(
          scrollDirection: Axis.horizontal,
          padding: const EdgeInsets.fromLTRB(2, 2, 2, 4),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              for (int i = 0; i < _LibFilter.values.length; i++) ...[
                if (i > 0) const SizedBox(width: 8),
                _FilterPill(
                  label: _LibFilter.values[i].label,
                  active: activeFilter == _LibFilter.values[i],
                  onTap: () => onSelectFilter(_LibFilter.values[i]),
                ),
              ],
            ],
          ),
        ),
      ),
    );

    return LayoutBuilder(
      builder: (ctx, c) {
        final maxW = c.maxWidth;
        // 7 pills intrinsic width ≈ 650px; when pills + 16px gap + 220px min search fit, place in one Row.
        final singleRow = viewportWidth > 680 && maxW >= 886;
        if (singleRow) {
          final searchW = (maxW - 666).clamp(220.0, 300.0);
          return Row(
            crossAxisAlignment: CrossAxisAlignment.center,
            children: [
              Expanded(child: filterStrip),
              const SizedBox(width: 16),
              SizedBox(
                width: searchW,
                child: _LibrarySearchField(controller: queryController, onChanged: onQueryChanged),
              ),
            ],
          );
        }

        final searchW = viewportWidth <= 680 ? maxW : maxW.clamp(220.0, 300.0);
        return Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            filterStrip,
            const SizedBox(height: 12),
            SizedBox(
              width: searchW,
              child: _LibrarySearchField(controller: queryController, onChanged: onQueryChanged),
            ),
          ],
        );
      },
    );
  }
}

/// Isolated search field matching `.lib-search` (addons.css:184-190):
/// 40px height, neutral 2px bone focus ring + 11% white fill, 18px icon at left 14, 32x32 clear button at right 6.
class _LibrarySearchField extends StatefulWidget {
  const _LibrarySearchField({required this.controller, required this.onChanged});
  final TextEditingController controller;
  final ValueChanged<String> onChanged;

  @override
  State<_LibrarySearchField> createState() => _LibrarySearchFieldState();
}

class _LibrarySearchFieldState extends State<_LibrarySearchField> {
  final FocusNode _focus = FocusNode();

  @override
  void initState() {
    super.initState();
    _focus.addListener(_sync);
    widget.controller.addListener(_sync);
  }

  void _sync() {
    if (mounted) setState(() {});
  }

  @override
  void dispose() {
    _focus.removeListener(_sync);
    widget.controller.removeListener(_sync);
    _focus.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final focused = _focus.hasFocus;
    final hasQuery = widget.controller.text.trim().isNotEmpty;

    return AnimatedContainer(
      duration: const Duration(milliseconds: 140),
      height: 40,
      decoration: BoxDecoration(
        color: focused ? const Color.fromRGBO(255, 255, 255, 0.11) : const Color.fromRGBO(255, 255, 255, 0.075),
        borderRadius: BorderRadius.circular(20),
        border: Border.all(
          color: focused ? VeyloColors.bone : const Color.fromRGBO(255, 255, 255, 0.11),
          width: focused ? 2.0 : 1.0,
        ),
      ),
      child: Stack(
        alignment: Alignment.centerLeft,
        children: [
          const Positioned(
            left: 14,
            child: IgnorePointer(
              child: VeyloIcon('Search', size: 18, color: VeyloColors.dust),
            ),
          ),
          Padding(
            padding: const EdgeInsets.only(left: 40, right: 36),
            child: TextField(
              controller: widget.controller,
              focusNode: _focus,
              style: VeyloText.ui(14, weight: 500, height: 1.0),
              cursorColor: VeyloColors.bone,
              decoration: InputDecoration.collapsed(
                hintText: 'Filter library…',
                hintStyle: VeyloText.ui(14, weight: 500, color: VeyloColors.dust, height: 1.0),
              ),
              onChanged: widget.onChanged,
            ),
          ),
          if (hasQuery)
            Positioned(
              right: 4,
              child: Semantics(
                button: true,
                label: 'Clear filter',
                child: MouseRegion(
                  cursor: SystemMouseCursors.click,
                  child: GestureDetector(
                    behavior: HitTestBehavior.opaque,
                    onTap: () {
                      widget.controller.clear();
                      widget.onChanged('');
                    },
                    child: const SizedBox(
                      width: 32,
                      height: 32,
                      child: Center(child: VeyloIcon('Close', size: 18, color: VeyloColors.dust)),
                    ),
                  ),
                ),
              ),
            ),
        ],
      ),
    );
  }
}

/// Exact CSS `.lib-filter-pill` (addons.css:178-183):
/// 40px height, Inter 650, specular top sheen + edge rim when idle, bone + drop shadow when active,
/// 150ms smooth fill/text color transition, scale(0.94) press feedback, keyboard focus ring.
class _FilterPill extends StatefulWidget {
  const _FilterPill({required this.label, required this.active, required this.onTap});
  final String label;
  final bool active;
  final VoidCallback onTap;

  @override
  State<_FilterPill> createState() => _FilterPillState();
}

class _FilterPillState extends State<_FilterPill> {
  bool _hovered = false;
  bool _pressed = false;
  bool _focused = false;

  @override
  Widget build(BuildContext context) {
    final fg = widget.active ? const Color(0xFF000000) : (_hovered ? VeyloColors.bone : VeyloColors.dust);

    final pill = AnimatedContainer(
      duration: const Duration(milliseconds: 150),
      height: 40,
      padding: const EdgeInsets.symmetric(horizontal: 16),
      alignment: Alignment.center,
      decoration: BoxDecoration(
        color: widget.active
            ? VeyloColors.bone
            : (_hovered ? const Color.fromRGBO(255, 255, 255, 0.11) : const Color.fromRGBO(255, 255, 255, 0.06)),
        borderRadius: BorderRadius.circular(20),
        boxShadow: widget.active
            ? const [BoxShadow(color: Color.fromRGBO(0, 0, 0, 0.30), blurRadius: 20, offset: Offset(0, 6))]
            : null,
      ),
      child: AnimatedDefaultTextStyle(
        duration: const Duration(milliseconds: 150),
        style: VeyloText.ui(14, weight: 650, color: fg, height: 1.0),
        child: Text(widget.label),
      ),
    );

    final rimmed = widget.active
        ? pill
        : CustomPaint(
            foregroundPainter: const SpecularRimPainter(
              radius: 20,
              edgeColor: VeyloColors.edge,
              sheenColor: Color.fromRGBO(255, 255, 255, 0.13),
            ),
            child: pill,
          );

    return Semantics(
      button: true,
      selected: widget.active,
      label: widget.label,
      child: FocusableActionDetector(
        onShowFocusHighlight: (f) => setState(() => _focused = f),
        shortcuts: const {
          SingleActivator(LogicalKeyboardKey.enter): ActivateIntent(),
          SingleActivator(LogicalKeyboardKey.space): ActivateIntent(),
        },
        actions: {
          ActivateIntent: CallbackAction<ActivateIntent>(onInvoke: (_) {
            widget.onTap();
            return null;
          }),
        },
        child: MouseRegion(
          cursor: SystemMouseCursors.click,
          onEnter: (_) => setState(() => _hovered = true),
          onExit: (_) => setState(() {
            _hovered = false;
            _pressed = false;
          }),
          child: GestureDetector(
            behavior: HitTestBehavior.opaque,
            onTapDown: (_) => setState(() => _pressed = true),
            onTapUp: (_) => setState(() => _pressed = false),
            onTapCancel: () => setState(() => _pressed = false),
            onTap: widget.onTap,
            child: AnimatedScale(
              scale: _pressed ? 0.94 : 1.0,
              duration: const Duration(milliseconds: 120),
              curve: Curves.ease,
              child: Container(
                padding: _focused ? const EdgeInsets.all(2) : EdgeInsets.zero,
                decoration: _focused
                    ? BoxDecoration(
                        borderRadius: BorderRadius.circular(23),
                        border: Border.all(color: VeyloColors.bone, width: 2),
                      )
                    : null,
                child: rimmed,
              ),
            ),
          ),
        ),
      ),
    );
  }
}

class _EmptyCard extends StatelessWidget {
  const _EmptyCard({required this.icon, required this.title, required this.subtitle, this.action});
  final String icon;
  final String title;
  final String subtitle;
  final Widget? action;

  @override
  Widget build(BuildContext context) {
    return CustomPaint(
      foregroundPainter: const SpecularRimPainter(
        radius: 22,
        edgeColor: VeyloColors.edge,
        sheenColor: Color.fromRGBO(255, 255, 255, 0.12),
      ),
      child: Container(
        width: double.infinity,
        padding: const EdgeInsets.symmetric(horizontal: 24, vertical: 36),
        decoration: BoxDecoration(
          color: const Color.fromRGBO(255, 255, 255, 0.04),
          borderRadius: BorderRadius.circular(22),
        ),
        child: Column(
          children: [
            VeyloIcon(icon, size: 30, color: VeyloColors.dust),
            const SizedBox(height: 10),
            Text(title, style: VeyloText.bodyStrong),
            const SizedBox(height: 4),
            Text(subtitle, textAlign: TextAlign.center, style: VeyloText.ui(13.5, weight: 500, color: VeyloColors.dust)),
            if (action != null) ...[
              const SizedBox(height: 14),
              action!,
            ],
          ],
        ),
      ),
    );
  }
}

class _WeekCard extends StatefulWidget {
  const _WeekCard({
    super.key,
    required this.cardId,
    required this.armedKey,
    required this.seen,
    required this.onForget,
  });

  final String cardId;
  final ValueNotifier<String?> armedKey;
  final SeenItem seen;
  final Future<void> Function() onForget;

  @override
  State<_WeekCard> createState() => _WeekCardState();
}

class _WeekCardState extends State<_WeekCard> {
  bool _hovered = false;
  bool _busy = false;
  bool _removing = false;
  DateTime? _armedAt;
  Timer? _leaveTimer;

  bool get _armed => widget.armedKey.value == widget.cardId;

  @override
  void initState() {
    super.initState();
    widget.armedKey.addListener(_onArmedChange);
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      final app = AppStateScope.read(context);
      if (app.cachedArtwork(widget.seen.media.key) == null) {
        unawaited(app.fetchArtwork(widget.seen.media));
      }
    });
  }

  void _onArmedChange() {
    if (!mounted) return;
    if (_armed) {
      _armedAt = DateTime.now();
    } else {
      _leaveTimer?.cancel();
    }
    setState(() {});
  }

  @override
  void dispose() {
    _leaveTimer?.cancel();
    widget.armedKey.removeListener(_onArmedChange);
    super.dispose();
  }

  void _arm() {
    _leaveTimer?.cancel();
    widget.armedKey.value = widget.cardId;
  }

  void _disarm() {
    if (_armed && !_busy) widget.armedKey.value = null;
  }

  Future<void> _confirmRemove() async {
    if (_busy) return;
    if (_armedAt != null && DateTime.now().difference(_armedAt!) < const Duration(milliseconds: 250)) {
      return;
    }
    setState(() => _busy = true);
    try {
      setState(() => _removing = true);
      await Future.delayed(const Duration(milliseconds: 160));
      if (!mounted) return;
      await widget.onForget();
      if (_armed) widget.armedKey.value = null;
    } catch (_) {
      if (mounted) setState(() { _busy = false; _removing = false; });
    }
  }

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final m = widget.seen.media;
    final done = m.watched;
    final ep = widget.seen.episode;
    final remaining = ep != null && m.resumeContext.startsWith(ep)
        ? m.resumeContext.substring(ep.length).replaceFirst(RegExp(r'^,\s*'), '')
        : m.resumeContext;
    final stateText = done ? 'Watched' : (m.inProgress ? remaining : 'Started');
    final raised = _hovered || _armed;

    return RepaintBoundary(
      child: AnimatedOpacity(
        duration: const Duration(milliseconds: 160),
        opacity: _removing ? 0.0 : 1.0,
        child: AnimatedScale(
          duration: const Duration(milliseconds: 160),
          scale: _removing ? 0.92 : 1.0,
          child: MouseRegion(
            cursor: SystemMouseCursors.click,
            onEnter: (_) {
              _leaveTimer?.cancel();
              setState(() => _hovered = true);
            },
            onExit: (_) {
              setState(() => _hovered = false);
              if (_armed && !_busy) {
                _leaveTimer?.cancel();
                _leaveTimer = Timer(const Duration(milliseconds: 800), _disarm);
              }
            },
            child: AnimatedContainer(
              duration: const Duration(milliseconds: 240),
              curve: VeyloCurves.out,
              transform: Matrix4.identity()
                ..translateByDouble(0.0, raised ? -4.0 : 0.0, 0.0, 1.0)
                ..scaleByDouble(raised ? 1.02 : 1.0, raised ? 1.02 : 1.0, 1.0, 1.0),
              transformAlignment: Alignment.center,
              decoration: BoxDecoration(
                borderRadius: BorderRadius.circular(18),
                color: const Color.fromRGBO(255, 255, 255, 0.06),
                boxShadow: [
                  BoxShadow(
                    color: Color.fromRGBO(0, 0, 0, raised ? 0.78 : 0.42),
                    blurRadius: raised ? 48 : 32,
                    offset: Offset(0, raised ? 24 : 14),
                  ),
                ],
              ),
              child: ClipRRect(
                borderRadius: BorderRadius.circular(18),
                child: Stack(
                  fit: StackFit.expand,
                  children: [
                    GestureDetector(
                      onTap: _armed ? _disarm : () => app.openTitle(m),
                      child: Stack(
                        fit: StackFit.expand,
                        children: [
                          ValueListenableBuilder<ArtworkModel?>(
                            valueListenable: app.artworkNotifier(m.key),
                            builder: (_, art, _) {
                              final bgSources = <String>[
                                if (art?.backdrop != null && art!.backdrop!.isNotEmpty) art.backdrop!,
                                if (m.backdrop.isNotEmpty) m.backdrop,
                                if (m.poster.isNotEmpty) m.poster,
                              ];
                              return FallbackImage(sources: bgSources, cacheWidth: 640);
                            },
                          ),
                          const DecoratedBox(
                            decoration: BoxDecoration(
                              gradient: LinearGradient(
                                begin: Alignment.topCenter,
                                end: Alignment.bottomCenter,
                                stops: [0.28, 0.58, 1.0],
                                colors: [
                                  Color.fromRGBO(0, 0, 0, 0.0),
                                  Color.fromRGBO(0, 0, 0, 0.55),
                                  Color.fromRGBO(0, 0, 0, 0.90),
                                ],
                              ),
                            ),
                          ),
                        ],
                      ),
                    ),
                    // Top-right discard control inside transform (× lands on Trash's exact spot!)
                    Positioned(
                      top: 12,
                      right: 12,
                      child: IgnorePointer(
                        ignoring: !_hovered && !_armed,
                        child: AnimatedOpacity(
                          opacity: (_hovered || _armed) ? 1.0 : 0.0,
                          duration: const Duration(milliseconds: 160),
                          child: Row(
                            mainAxisSize: MainAxisSize.min,
                            children: [
                              if (_armed) ...[
                                VeyloPill(
                                  label: 'Remove',
                                  icon: 'Trash',
                                  confirmRed: true,
                                  small: true,
                                  disabled: _busy,
                                  onTap: _confirmRemove,
                                ),
                                const SizedBox(width: 6),
                              ],
                              VeyloOrb(
                                icon: _armed ? 'Close' : 'Trash',
                                size: 36,
                                iconSize: 17,
                                glass: false,
                                tooltip: _armed ? 'Keep' : 'Remove from Library',
                                backgroundOverride: const Color.fromRGBO(10, 11, 15, 0.80),
                                onTap: _armed ? _disarm : _arm,
                              ),
                            ],
                          ),
                        ),
                      ),
                    ),
                    Positioned(
                      left: 20,
                      right: 84,
                      bottom: 18,
                      child: IgnorePointer(
                        child: Column(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          mainAxisSize: MainAxisSize.min,
                          children: [
                            Text(
                              widget.seen.whenLabel,
                              style: VeyloText.ui(13, weight: 700, color: const Color.fromRGBO(242, 239, 234, 0.75)),
                            ),
                            const SizedBox(height: 3),
                            Text(
                              m.name,
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                              style: VeyloText.display(26, weight: 800, height: 1.05),
                            ),
                            const SizedBox(height: 6),
                            Row(
                              children: [
                                if (ep != null) ...[
                                  Container(
                                    height: 22,
                                    padding: const EdgeInsets.symmetric(horizontal: 8),
                                    alignment: Alignment.center,
                                    decoration: BoxDecoration(
                                      color: VeyloColors.bone,
                                      borderRadius: BorderRadius.circular(11),
                                    ),
                                    child: Text(
                                      ep,
                                      style: VeyloText.ui(12, weight: 700, color: const Color(0xFF000000), tabular: true),
                                    ),
                                  ),
                                  const SizedBox(width: 8),
                                ],
                                if (done) ...[
                                  Container(
                                    width: 20,
                                    height: 20,
                                    decoration: const BoxDecoration(shape: BoxShape.circle, color: VeyloColors.bone),
                                    alignment: Alignment.center,
                                    child: const VeyloIcon('Check', size: 13, color: Color(0xFF000000)),
                                  ),
                                  const SizedBox(width: 8),
                                ],
                                Flexible(
                                  child: Text(
                                    stateText,
                                    maxLines: 1,
                                    overflow: TextOverflow.ellipsis,
                                    style: VeyloText.ui(14, weight: 500, color: VeyloColors.synopsis),
                                  ),
                                ),
                              ],
                            ),
                          ],
                        ),
                      ),
                    ),
                    if (!done)
                      Positioned(
                        right: 16,
                        bottom: 16,
                        child: VeyloOrb(
                          icon: 'Play',
                          size: 46,
                          iconSize: 18,
                          glass: false,
                          backgroundOverride: const Color.fromRGBO(16, 17, 22, 0.78),
                          onTap: () => app.autoplayMedia(m),
                        ),
                      ),
                    if (m.progressFraction > 0 && !done)
                      Positioned(
                        left: 0,
                        right: 0,
                        bottom: 0,
                        child: ValueListenableBuilder<Color>(
                          valueListenable: app.lampColorNotifier,
                          builder: (_, lamp, _) => Container(
                            height: 3.5,
                            color: const Color.fromRGBO(0, 0, 0, 0.55),
                            alignment: Alignment.centerLeft,
                            child: TweenAnimationBuilder<double>(
                              tween: Tween(begin: 0, end: m.progressFraction.clamp(0.0, 1.0)),
                              duration: const Duration(milliseconds: 450),
                              curve: VeyloCurves.out,
                              builder: (_, v, _) => FractionallySizedBox(widthFactor: v, child: ColoredBox(color: lamp)),
                            ),
                          ),
                        ),
                      ),
                    IgnorePointer(
                      child: DecoratedBox(
                        decoration: BoxDecoration(
                          borderRadius: BorderRadius.circular(18),
                          border: Border.all(
                            color: _armed
                                ? VeyloColors.dangerRed
                                : (_hovered ? VeyloColors.sheen : const Color.fromRGBO(255, 255, 255, 0.12)),
                            width: _armed ? 1.5 : 1.0,
                          ),
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
    );
  }
}

/// Library poster grid item whose discard/unsave control lives INSIDE `PosterCard`'s animated transform
/// at the bottom-right corner (`right: 8, bottom: 10`), clearing all top BetterPosters badges,
/// top-right watched ticks, and bottom progress bars, and NEVER blacking out the artwork when armed.
class _LibGridItem extends StatefulWidget {
  const _LibGridItem({
    super.key,
    required this.cardId,
    required this.armedKey,
    required this.media,
    required this.actionLabel,
    required this.onRemove,
  });

  final String cardId;
  final ValueNotifier<String?> armedKey;
  final MediaItem media;
  final String actionLabel;
  final Future<void> Function() onRemove;

  @override
  State<_LibGridItem> createState() => _LibGridItemState();
}

class _LibGridItemState extends State<_LibGridItem> {
  bool _busy = false;
  bool _removing = false;
  DateTime? _armedAt;
  Timer? _leaveTimer;

  bool get _armed => widget.armedKey.value == widget.cardId;

  @override
  void initState() {
    super.initState();
    widget.armedKey.addListener(_onArmedChange);
  }

  void _onArmedChange() {
    if (!mounted) return;
    if (_armed) {
      _armedAt = DateTime.now();
    } else {
      _leaveTimer?.cancel();
    }
    setState(() {});
  }

  @override
  void dispose() {
    _leaveTimer?.cancel();
    widget.armedKey.removeListener(_onArmedChange);
    super.dispose();
  }

  void _arm() {
    _leaveTimer?.cancel();
    widget.armedKey.value = widget.cardId;
  }

  void _disarm() {
    if (_armed && !_busy) widget.armedKey.value = null;
  }

  Future<void> _confirmRemove() async {
    if (_busy) return;
    if (_armedAt != null && DateTime.now().difference(_armedAt!) < const Duration(milliseconds: 250)) {
      return;
    }
    setState(() => _busy = true);
    try {
      setState(() => _removing = true);
      await Future.delayed(const Duration(milliseconds: 160));
      if (!mounted) return;
      await widget.onRemove();
      if (_armed) widget.armedKey.value = null;
    } catch (_) {
      if (mounted) setState(() { _busy = false; _removing = false; });
    }
  }

  @override
  Widget build(BuildContext context) {
    return AnimatedOpacity(
      duration: const Duration(milliseconds: 160),
      opacity: _removing ? 0.0 : 1.0,
      child: AnimatedScale(
        duration: const Duration(milliseconds: 160),
        scale: _removing ? 0.92 : 1.0,
        child: PosterCard(
          media: widget.media,
          lifted: _armed,
          rimColor: _armed ? VeyloColors.dangerRed : null,
          onTap: _armed ? _disarm : null,
          onHoverChanged: (hovering) {
            if (hovering) {
              _leaveTimer?.cancel();
            } else if (_armed && !_busy) {
              _leaveTimer?.cancel();
              _leaveTimer = Timer(const Duration(milliseconds: 800), _disarm);
            }
          },
          overlayBuilder: (ctx, hovered) {
            final show = hovered || _armed;
            return Positioned(
              left: 8,
              right: 8,
              bottom: 10,
              child: IgnorePointer(
                ignoring: !show,
                child: AnimatedOpacity(
                  opacity: show ? 1.0 : 0.0,
                  duration: const Duration(milliseconds: 170),
                  curve: VeyloCurves.out,
                  child: LayoutBuilder(
                    builder: (_, c) {
                      final showIconInPill = c.maxWidth >= 140;
                      return Row(
                        mainAxisAlignment: MainAxisAlignment.end,
                        children: [
                          if (_armed) ...[
                            Expanded(
                              child: Semantics(
                                button: true,
                                label: '${widget.actionLabel} ${widget.media.name}',
                                child: MouseRegion(
                                  cursor: _busy ? SystemMouseCursors.basic : SystemMouseCursors.click,
                                  child: GestureDetector(
                                    behavior: HitTestBehavior.opaque,
                                    onTap: _busy ? null : _confirmRemove,
                                    child: AnimatedOpacity(
                                      opacity: _busy ? 0.65 : 1.0,
                                      duration: const Duration(milliseconds: 120),
                                      child: Container(
                                        height: 34,
                                        padding: const EdgeInsets.symmetric(horizontal: 10),
                                        decoration: BoxDecoration(
                                          color: VeyloColors.dangerRed,
                                          borderRadius: BorderRadius.circular(17),
                                          boxShadow: const [
                                            BoxShadow(color: Color.fromRGBO(0, 0, 0, 0.45), blurRadius: 14, offset: Offset(0, 4)),
                                          ],
                                        ),
                                        child: Row(
                                          mainAxisAlignment: MainAxisAlignment.center,
                                          children: [
                                            if (showIconInPill) ...[
                                              const VeyloIcon('Trash', size: 14, color: Color(0xFFFFFFFF)),
                                              const SizedBox(width: 5),
                                            ],
                                            Flexible(
                                              child: Text(
                                                widget.actionLabel,
                                                maxLines: 1,
                                                overflow: TextOverflow.ellipsis,
                                                style: VeyloText.ui(13, weight: 650, color: const Color(0xFFFFFFFF), height: 1.0),
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
                            const SizedBox(width: 6),
                          ],
                          // Resting Trash orb / Armed Cancel × orb in the EXACT same bottom-right spot
                          Semantics(
                            button: true,
                            label: _armed ? 'Keep ${widget.media.name}' : '${widget.actionLabel} ${widget.media.name}',
                            child: VeyloTooltip(
                              message: _armed ? 'Keep' : widget.actionLabel,
                              child: MouseRegion(
                                cursor: SystemMouseCursors.click,
                                child: GestureDetector(
                                  behavior: HitTestBehavior.opaque,
                                  onTap: _armed ? _disarm : _arm,
                                  child: CustomPaint(
                                    foregroundPainter: const SpecularRimPainter(
                                      radius: 17,
                                      edgeColor: Color.fromRGBO(255, 255, 255, 0.16),
                                      sheenColor: Color.fromRGBO(255, 255, 255, 0.20),
                                    ),
                                    child: Container(
                                      width: 34,
                                      height: 34,
                                      decoration: const BoxDecoration(
                                        shape: BoxShape.circle,
                                        color: Color.fromRGBO(10, 11, 15, 0.84),
                                        boxShadow: [
                                          BoxShadow(color: Color.fromRGBO(0, 0, 0, 0.45), blurRadius: 14, offset: Offset(0, 4)),
                                        ],
                                      ),
                                      alignment: Alignment.center,
                                      child: AnimatedSwitcher(
                                        duration: const Duration(milliseconds: 140),
                                        child: VeyloIcon(
                                          _armed ? 'Close' : 'Trash',
                                          key: ValueKey(_armed),
                                          size: 16,
                                          color: VeyloColors.bone,
                                        ),
                                      ),
                                    ),
                                  ),
                                ),
                              ),
                            ),
                          ),
                        ],
                      );
                    },
                  ),
                ),
              ),
            );
          },
        ),
      ),
    );
  }
}
