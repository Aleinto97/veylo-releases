import 'dart:async';
import 'dart:ui';
import 'package:cached_network_image/cached_network_image.dart';
import 'package:material_ui/material_ui.dart' show InputDecoration, TextField;
import 'package:flutter/widgets.dart';
import '../bridge/veylo_bridge.dart';
import '../state/app_state.dart';
import '../theme/veylo_theme.dart';
import '../widgets/glass.dart';
import '../widgets/veylo_icons.dart';

class SearchOverlay extends StatefulWidget {
  const SearchOverlay({super.key});

  @override
  State<SearchOverlay> createState() => _SearchOverlayState();
}

class _SearchOverlayState extends State<SearchOverlay> with SingleTickerProviderStateMixin {
  final TextEditingController _ctrl = TextEditingController();
  final FocusNode _focus = FocusNode();
  late final AnimationController _openCtrl = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 200),
  )..forward();
  late final Animation<double> _openAnim = CurvedAnimation(parent: _openCtrl, curve: VeyloCurves.out);

  bool _vibeMode = false;
  bool _searching = false;
  String? _error;
  int _gen = 0;
  Timer? _debounce;

  List<(String, MediaItem)> _results = [];
  int _topRank = 99;
  List<String> _failed = [];
  int _catalogs = 1;
  bool _hasOutcome = false;
  List<MediaItem>? _vibeResults;

  List<String> _pastQueries = [];
  List<MediaItem> _recentItems = [];
  List<MediaItem> _ideas = [];
  bool _armClear = false;

  @override
  void initState() {
    super.initState();
    _focus.addListener(_onFocusChange);
    WidgetsBinding.instance.addPostFrameCallback((_) {
      final app = AppStateScope.read(context);
      final initial = app.pendingSearchQuery ?? app.searchDraft;
      app.pendingSearchQuery = null;
      if (initial.isNotEmpty) {
        _ctrl.text = initial;
        _ctrl.selection = TextSelection.collapsed(offset: initial.length);
        _runSearch(initial, delayMs: 0);
      }
      _focus.requestFocus();
      unawaited(_loadEmptyState());
    });
  }

  void _onFocusChange() {
    if (mounted) setState(() {});
  }

  @override
  void dispose() {
    _debounce?.cancel();
    _focus.removeListener(_onFocusChange);
    _openCtrl.dispose();
    _ctrl.dispose();
    _focus.dispose();
    super.dispose();
  }

  Future<void> _loadEmptyState() async {
    final app = AppStateScope.read(context);
    try {
      final hist = await app.bridge.searchHistory();
      if (!mounted) return;
      setState(() {
        _pastQueries = (hist['queries'] as List<dynamic>? ?? []).map((e) => e.toString()).toList();
        _recentItems = (hist['items'] as List<dynamic>? ?? [])
            .map((e) => MediaItem.fromJson(e as Map<String, dynamic>))
            .toList();
      });
      if (app.tonightPicks.isNotEmpty) {
        setState(() => _ideas = app.tonightPicks);
      }
    } catch (_) {}
  }

  void _onQueryChanged(String q) {
    AppStateScope.read(context).searchDraft = q;
    _runSearch(q, delayMs: _vibeMode ? 420 : 280);
  }

  void _runSearch(String q, {required int delayMs}) {
    _debounce?.cancel();
    final id = ++_gen;
    final trimmed = q.trim();
    if (trimmed.isEmpty) {
      setState(() {
        _searching = false;
        _hasOutcome = false;
        _results = [];
        _vibeResults = null;
        _error = null;
      });
      return;
    }

    Future<void> execute() async {
      if (!mounted || _gen != id) return;
      setState(() {
        _searching = true;
        _error = null;
      });
      try {
        final res = await AppStateScope.read(context).bridge.search(trimmed, vibe: _vibeMode);
        if (!mounted || _gen != id) return;
        if (_vibeMode) {
          final items = (res['items'] as List<dynamic>? ?? [])
              .map((e) => MediaItem.fromJson(e as Map<String, dynamic>))
              .toList();
          setState(() {
            _searching = false;
            _vibeResults = items;
            _hasOutcome = false;
          });
        } else {
          final list = <(String, MediaItem)>[];
          for (final item in (res['results'] as List<dynamic>? ?? [])) {
            final map = item as Map<String, dynamic>;
            list.add((map['addon']?.toString() ?? '', MediaItem.fromJson(map['media'] as Map<String, dynamic>)));
          }
          setState(() {
            _searching = false;
            _hasOutcome = true;
            _results = list;
            _topRank = (res['top_rank'] as num?)?.toInt() ?? 99;
            _catalogs = (res['catalogs'] as num?)?.toInt() ?? 1;
            _failed = (res['failed'] as List<dynamic>? ?? []).map((e) => e.toString()).toList();
            _vibeResults = null;
          });
        }
      } catch (e) {
        if (!mounted || _gen != id) return;
        setState(() {
          _searching = false;
          _error = e.toString();
        });
      }
    }

    if (delayMs <= 0) {
      unawaited(execute());
    } else {
      _debounce = Timer(Duration(milliseconds: delayMs), () => unawaited(execute()));
    }
  }

  void _openResult(MediaItem m) {
    final app = AppStateScope.read(context);
    unawaited(app.bridge.rememberItem(m));
    app.openTitle(m);
    app.closeSearch();
  }

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final size = MediaQuery.sizeOf(context);
    final gutter = VeyloLayout.gutter(size.width);
    final q = _ctrl.text;

    final top = (!_vibeMode && _results.isNotEmpty && _topRank <= 3) ? _results.first : null;
    final groups = <String, List<MediaItem>>{};
    for (final (_, m) in _results) {
      if (top != null && m.key == top.$2.key) continue;
      groups.putIfAbsent(m.kind, () => []).add(m);
    }
    final orderedKinds = groups.keys.toList()
      ..sort((a, b) {
        int order(String k) => switch (k) { 'movie' => 0, 'series' => 1, 'anime' => 2, _ => 3 };
        return order(a).compareTo(order(b));
      });
    final sideBySide = size.width >= 700 && groups.containsKey('movie') && groups.containsKey('series');

    return Positioned.fill(
      child: AnimatedBuilder(
        animation: _openAnim,
        builder: (_, child) => Opacity(
          opacity: _openAnim.value,
          child: Transform.scale(
            scale: 0.985 + 0.015 * _openAnim.value,
            child: child,
          ),
        ),
        child: Stack(
          fit: StackFit.expand,
          children: [
            const ColoredBox(color: Color(0xFF000000)),
            RepaintBoundary(
              child: ValueListenableBuilder<String>(
                valueListenable: app.ambientUrlNotifier,
                builder: (_, url, _) => url.isNotEmpty
                    ? IgnorePointer(
                        child: ImageFiltered(
                          imageFilter: ImageFilter.blur(sigmaX: 28, sigmaY: 28, tileMode: TileMode.clamp),
                          child: Opacity(
                            opacity: 0.36,
                            child: CachedNetworkImage(
                              imageUrl: url,
                              fit: BoxFit.cover,
                              width: double.infinity,
                              height: double.infinity,
                              memCacheWidth: 96,
                              fadeInDuration: Duration.zero,
                              errorWidget: (_, _, _) => const SizedBox.shrink(),
                            ),
                          ),
                        ),
                      )
                    : const SizedBox.shrink(),
              ),
            ),
            const IgnorePointer(
              child: DecoratedBox(
                decoration: BoxDecoration(
                  gradient: RadialGradient(
                    center: Alignment(0, -1),
                    radius: 1.3,
                    colors: [Color(0x00000000), Color.fromRGBO(0, 0, 0, 0.55), Color.fromRGBO(0, 0, 0, 0.90)],
                  ),
                ),
              ),
            ),

            Column(
              children: [
                // Search head (.search-head)
                Center(
                  child: ConstrainedBox(
                    constraints: const BoxConstraints(maxWidth: 1280),
                    child: Padding(
                      padding: EdgeInsets.fromLTRB(gutter, 24, gutter, 0),
                      child: Row(
                        children: [
                          Expanded(
                            child: ValueListenableBuilder<Color>(
                              valueListenable: app.lampColorNotifier,
                              builder: (_, lamp, _) => SizedBox(
                                height: 76,
                                child: ClipRRect(
                                  borderRadius: BorderRadius.circular(38),
                                  // expand: the glass fills the whole 76 px field. Without it the
                                  // glass shrank to ~50 px at the top (misaligned with the buttons
                                  // and its corners cut by the 38 px clip).
                                  child: Stack(
                                    fit: StackFit.expand,
                                    children: [
                                      GlassBox(
                                        borderRadius: 38,
                                        borderColor: _focus.hasFocus ? lamp : VeyloColors.edge,
                                        borderWidth: _focus.hasFocus ? 1.5 : 1,
                                        padding: const EdgeInsets.fromLTRB(24, 0, 14, 0),
                                        child: Row(
                                          children: [
                                            VeyloIcon(
                                              _vibeMode ? 'Sparkles' : 'Search',
                                              size: 28,
                                              color: VeyloColors.bone,
                                              filled: _vibeMode,
                                            ),
                                            const SizedBox(width: 14),
                                            Expanded(
                                              child: TextField(
                                                controller: _ctrl,
                                                focusNode: _focus,
                                                style: VeyloText.display(34, weight: 700),
                                                cursorColor: lamp,
                                                decoration: InputDecoration.collapsed(
                                                  hintText: _vibeMode ? 'Describe a vibe or plot…' : 'Search',
                                                  hintStyle: VeyloText.display(
                                                    34,
                                                    weight: 700,
                                                    color: const Color.fromRGBO(242, 239, 234, 0.35),
                                                  ),
                                                ),
                                                onChanged: _onQueryChanged,
                                                onSubmitted: (v) {
                                                  if (v.trim().length >= 2) {
                                                    unawaited(app.bridge.rememberQuery(v.trim()));
                                                  }
                                                  _runSearch(v, delayMs: 0);
                                                },
                                              ),
                                            ),
                                            if (q.isNotEmpty)
                                              VeyloOrb(
                                                icon: 'Backspace',
                                                size: 42,
                                                glass: false,
                                                tooltip: 'Clear',
                                                onTap: () {
                                                  _ctrl.clear();
                                                  _onQueryChanged('');
                                                },
                                              ),
                                          ],
                                        ),
                                      ),
                                      if (_searching)
                                        Positioned(
                                          left: 0,
                                          right: 0,
                                          bottom: 0,
                                          child: _SweepBar(color: lamp),
                                        ),
                                    ],
                                  ),
                                ),
                              ),
                            ),
                          ),
                          if (!app.aiFailed) ...[
                            const SizedBox(width: 12),
                            VeyloPill(
                              label: 'Vibe',
                              icon: 'Sparkles',
                              accent: _vibeMode,
                              tooltip: 'On-device semantic search by mood, plot or vibe',
                              onTap: () {
                                setState(() => _vibeMode = !_vibeMode);
                                if (_ctrl.text.trim().isNotEmpty) {
                                  _runSearch(_ctrl.text, delayMs: 0);
                                }
                              },
                            ),
                          ],
                          const SizedBox(width: 10),
                          VeyloOrb(
                            icon: 'Keyboard',
                            tooltip: 'Page shortcuts (?)',
                            onTap: () => app.setShortcutsOpen(true),
                          ),
                          const SizedBox(width: 10),
                          VeyloOrb(
                            icon: 'Close',
                            tooltip: 'Close (Esc)',
                            onTap: app.closeSearch,
                          ),
                        ],
                      ),
                    ),
                  ),
                ),

                // Search body
                Expanded(
                  child: Center(
                    child: ConstrainedBox(
                      constraints: const BoxConstraints(maxWidth: 1280),
                      child: SingleChildScrollView(
                        padding: EdgeInsets.fromLTRB(gutter, 20, gutter, 48),
                        child: q.trim().isEmpty
                            ? _buildEmpty(app, gutter)
                            : (_vibeMode ? _buildVibeResults() : _buildStandardResults(top, orderedKinds, groups, sideBySide)),
                      ),
                    ),
                  ),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }

  Widget _buildEmpty(AppState app, double gutter) {
    final recentKeys = _recentItems.map((e) => e.key).toSet();
    final ideaList = _ideas.where((m) => !recentKeys.contains(m.key)).take(10).toList();
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        if (_pastQueries.isNotEmpty || _recentItems.isNotEmpty)
          Wrap(
            spacing: 8,
            runSpacing: 8,
            crossAxisAlignment: WrapCrossAlignment.center,
            children: [
              for (final past in _pastQueries.take(8))
                MouseRegion(
                  cursor: SystemMouseCursors.click,
                  child: GestureDetector(
                    onTap: () {
                      _ctrl.text = past;
                      _ctrl.selection = TextSelection.collapsed(offset: past.length);
                      _onQueryChanged(past);
                    },
                    child: Container(
                      height: 40,
                      padding: const EdgeInsets.fromLTRB(12, 0, 15, 0),
                      decoration: BoxDecoration(
                        color: const Color.fromRGBO(255, 255, 255, 0.075),
                        borderRadius: BorderRadius.circular(20),
                        border: Border.all(color: VeyloColors.edge),
                      ),
                      child: Row(
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          const VeyloIcon('History', size: 17, color: VeyloColors.dust),
                          const SizedBox(width: 8),
                          Text(past, style: VeyloText.ui(14, weight: 600)),
                        ],
                      ),
                    ),
                  ),
                ),
              if (_armClear) ...[
                VeyloPill(
                  label: 'Clear',
                  icon: 'Trash',
                  confirmRed: true,
                  small: true,
                  onTap: () async {
                    setState(() {
                      _armClear = false;
                      _pastQueries = [];
                      _recentItems = [];
                    });
                    await app.bridge.clearHistory();
                  },
                ),
                VeyloOrb(icon: 'Close', size: 36, onTap: () => setState(() => _armClear = false)),
              ] else
                VeyloOrb(
                  icon: 'Trash',
                  size: 38,
                  glass: false,
                  tooltip: 'Clear history',
                  onTap: () => setState(() => _armClear = true),
                ),
            ],
          ),
        if (_recentItems.isNotEmpty) ...[
          const SizedBox(height: 24),
          const GroupHead(icon: 'History', title: 'Recent'),
          const SizedBox(height: 12),
          SizedBox(
            height: 236,
            child: ListView.separated(
              scrollDirection: Axis.horizontal,
              itemCount: _recentItems.take(10).length,
              separatorBuilder: (_, _) => const SizedBox(width: 16),
              itemBuilder: (_, i) => PosterCard(
                media: _recentItems[i],
                width: 132,
                showMetaBelow: true,
                onTap: () => _openResult(_recentItems[i]),
              ),
            ),
          ),
        ],
        if (ideaList.isNotEmpty) ...[
          const SizedBox(height: 24),
          const GroupHead(icon: 'Sparkles', title: 'From your picks'),
          const SizedBox(height: 12),
          SizedBox(
            height: 236,
            child: ListView.separated(
              scrollDirection: Axis.horizontal,
              itemCount: ideaList.length,
              separatorBuilder: (_, _) => const SizedBox(width: 16),
              itemBuilder: (_, i) => PosterCard(
                media: ideaList[i],
                width: 132,
                showMetaBelow: true,
                onTap: () => _openResult(ideaList[i]),
              ),
            ),
          ),
        ],
      ],
    );
  }

  Widget _buildVibeResults() {
    if (_searching && (_vibeResults == null || _vibeResults!.isEmpty)) {
      return _buildSkeletonGrid();
    }
    if (_error != null) {
      return GlassBox(
        borderRadius: 28,
        padding: const EdgeInsets.symmetric(horizontal: 18, vertical: 12),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            const VeyloIcon('Alert', size: 18, color: VeyloColors.amber),
            const SizedBox(width: 10),
            Text(_error!, style: VeyloText.meta),
          ],
        ),
      );
    }
    final list = _vibeResults ?? [];
    if (list.isEmpty) {
      return Center(
        child: Padding(
          padding: const EdgeInsets.only(top: 64),
          child: GlassBox(
            borderRadius: 24,
            padding: const EdgeInsets.symmetric(horizontal: 32, vertical: 28),
            child: const Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                VeyloIcon('Sparkles', size: 30, color: VeyloColors.dust),
                SizedBox(height: 10),
                Text('No matching vibes in your catalog yet', style: VeyloText.body),
              ],
            ),
          ),
        ),
      );
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        GroupHead(icon: 'Sparkles', title: 'Vibe matches', count: list.length),
        const SizedBox(height: 14),
        _resultGrid(list),
      ],
    );
  }

  Widget _buildStandardResults(
    (String, MediaItem)? top,
    List<String> orderedKinds,
    Map<String, List<MediaItem>> groups,
    bool sideBySide,
  ) {
    if (_searching && _results.isEmpty) {
      return _buildSkeletonGrid();
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        if (top != null) ...[
          LiftIn(child: _TopMatchCard(media: top.$2, source: top.$1, onTap: () => _openResult(top.$2))),
          const SizedBox(height: 24),
        ],
        if (sideBySide)
          Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Expanded(child: _buildGroupSection('movie', groups['movie']!)),
              const SizedBox(width: 40),
              Expanded(child: _buildGroupSection('series', groups['series']!)),
            ],
          ),
        for (final kind in orderedKinds)
          if (!(sideBySide && (kind == 'movie' || kind == 'series'))) ...[
            _buildGroupSection(kind, groups[kind]!),
            const SizedBox(height: 24),
          ],
        if (_failed.isNotEmpty || _error != null) ...[
          const SizedBox(height: 20),
          GlassBox(
            borderRadius: 28,
            padding: const EdgeInsets.fromLTRB(16, 6, 8, 6),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                const VeyloIcon('Alert', size: 18, color: VeyloColors.amber),
                const SizedBox(width: 10),
                Text(_error != null ? 'Search unavailable' : '${_failed.join(', ')} offline', style: VeyloText.meta),
                const SizedBox(width: 8),
                VeyloOrb(icon: 'Refresh', size: 36, glass: false, onTap: () => _runSearch(_ctrl.text, delayMs: 0)),
              ],
            ),
          ),
        ],
        if (!_searching && _hasOutcome && _results.isEmpty && _failed.isEmpty)
          Center(
            child: Padding(
              padding: const EdgeInsets.only(top: 64),
              child: GlassBox(
                borderRadius: 24,
                padding: const EdgeInsets.symmetric(horizontal: 32, vertical: 28),
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    const VeyloIcon('Search', size: 30, color: VeyloColors.dust),
                    const SizedBox(height: 10),
                    Text(_catalogs == 0 ? 'No add-on can search' : 'No results', style: VeyloText.body),
                  ],
                ),
              ),
            ),
          ),
      ],
    );
  }

  Widget _buildGroupSection(String kind, List<MediaItem> list) {
    final icon = switch (kind) {
      'movie' => 'Film',
      'series' => 'Tv',
      'anime' => 'Blossom',
      _ => 'Grid',
    };
    final label = switch (kind) {
      'movie' => 'Films',
      'series' => 'Series',
      'anime' => 'Anime',
      _ => kind.isEmpty ? 'Other' : '${kind[0].toUpperCase()}${kind.substring(1)}',
    };
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        GroupHead(icon: icon, title: label, count: list.length),
        const SizedBox(height: 14),
        _resultGrid(list),
      ],
    );
  }

  Widget _resultGrid(List<MediaItem> list) {
    return LayoutBuilder(
      builder: (ctx, c) {
        final cols = (c.maxWidth / 144).floor().clamp(2, 8);
        return GridView.builder(
          shrinkWrap: true,
          physics: const NeverScrollableScrollPhysics(),
          gridDelegate: SliverGridDelegateWithFixedCrossAxisCount(
            crossAxisCount: cols,
            crossAxisSpacing: 16,
            mainAxisSpacing: 22,
            childAspectRatio: 0.58,
          ),
          itemCount: list.length,
          itemBuilder: (_, i) => PosterCard(
            media: list[i],
            showMetaBelow: true,
            onTap: () => _openResult(list[i]),
          ),
        );
      },
    );
  }

  Widget _buildSkeletonGrid() {
    return Wrap(
      spacing: 16,
      runSpacing: 22,
      children: [for (int i = 0; i < 8; i++) const SkeletonBox(width: 132, height: 198, borderRadius: 12)],
    );
  }
}

/// Animated sweeping --lamp progress line matching CSS `@keyframes sweep` (1s infinite).
class _SweepBar extends StatefulWidget {
  const _SweepBar({required this.color});
  final Color color;

  @override
  State<_SweepBar> createState() => _SweepBarState();
}

class _SweepBarState extends State<_SweepBar> with SingleTickerProviderStateMixin {
  late final AnimationController _ctrl = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 1000),
  )..repeat();

  @override
  void dispose() {
    _ctrl.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return SizedBox(
      height: 2.5,
      child: LayoutBuilder(
        builder: (_, c) {
          final w = c.maxWidth;
          final barW = w * 0.36;
          return AnimatedBuilder(
            animation: _ctrl,
            builder: (_, _) {
              final x = -barW + (w + barW) * _ctrl.value;
              return Stack(
                children: [
                  Positioned(
                    left: x,
                    top: 0,
                    bottom: 0,
                    width: barW,
                    child: DecoratedBox(
                      decoration: BoxDecoration(
                        gradient: LinearGradient(
                          colors: [
                            widget.color.withValues(alpha: 0.0),
                            widget.color,
                            widget.color.withValues(alpha: 0.0),
                          ],
                        ),
                      ),
                    ),
                  ),
                ],
              );
            },
          );
        },
      ),
    );
  }
}

class _TopMatchCard extends StatefulWidget {
  const _TopMatchCard({required this.media, required this.source, required this.onTap});
  final MediaItem media;
  final String source;
  final VoidCallback onTap;

  @override
  State<_TopMatchCard> createState() => _TopMatchCardState();
}

class _TopMatchCardState extends State<_TopMatchCard> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final m = widget.media;
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      child: GestureDetector(
        onTap: widget.onTap,
        child: AnimatedContainer(
          duration: const Duration(milliseconds: 220),
          curve: VeyloCurves.out,
          transform: Matrix4.identity()..translateByDouble(0.0, _hovered ? -2.0 : 0.0, 0.0, 1.0),
          child: ClipRRect(
            borderRadius: BorderRadius.circular(24),
            child: GlassBox(
              borderRadius: 24,
              child: Stack(
                children: [
                  if (m.background.isNotEmpty)
                    Positioned.fill(
                      left: 220,
                      child: ShaderMask(
                        shaderCallback: (r) => const LinearGradient(
                          colors: [Color(0x00FFFFFF), Color(0xFFFFFFFF)],
                          stops: [0.0, 0.45],
                        ).createShader(r),
                        blendMode: BlendMode.dstIn,
                        child: Opacity(
                          opacity: 0.62,
                          child: CachedNetworkImage(
                            imageUrl: m.background,
                            fit: BoxFit.cover,
                            memCacheWidth: 640,
                            errorWidget: (_, _, _) => const SizedBox.shrink(),
                          ),
                        ),
                      ),
                    ),
                  Padding(
                    padding: const EdgeInsets.all(18),
                    child: Row(
                      children: [
                        SizedBox(
                          width: 100,
                          height: 150,
                          child: ClipRRect(
                            borderRadius: BorderRadius.circular(12),
                            child: FallbackImage(sources: m.posters, cacheWidth: 240),
                          ),
                        ),
                        const SizedBox(width: 20),
                        Expanded(
                          child: ConstrainedBox(
                            constraints: const BoxConstraints(maxWidth: 620),
                            child: Column(
                              crossAxisAlignment: CrossAxisAlignment.start,
                              mainAxisSize: MainAxisSize.min,
                              children: [
                                ValueListenableBuilder<Color>(
                                  valueListenable: app.lampColorNotifier,
                                  builder: (_, lamp, _) => Container(
                                    height: 26,
                                    padding: const EdgeInsets.fromLTRB(9, 0, 11, 0),
                                    decoration: BoxDecoration(
                                      color: VeyloColors.mixLamp(lamp, const Color.fromRGBO(16, 17, 22, 0.72), 0.20),
                                      borderRadius: BorderRadius.circular(13),
                                      border: Border.all(
                                        color: VeyloColors.mixLamp(lamp, const Color.fromRGBO(255, 255, 255, 0.16), 0.45),
                                      ),
                                    ),
                                    child: Row(
                                      mainAxisSize: MainAxisSize.min,
                                      children: [
                                        VeyloIcon('Star', size: 13, color: lamp, filled: true),
                                        const SizedBox(width: 6),
                                        Text('Best match', style: VeyloText.ui(12, weight: 700, color: VeyloColors.bone)),
                                      ],
                                    ),
                                  ),
                                ),
                                const SizedBox(height: 6),
                                Text(m.name, style: VeyloText.display(34, weight: 800, height: 1.04)),
                                const SizedBox(height: 4),
                                Text(m.metaLineTyped, style: VeyloText.meta),
                                if (m.description.isNotEmpty) ...[
                                  const SizedBox(height: 8),
                                  Text(
                                    m.description,
                                    maxLines: 2,
                                    overflow: TextOverflow.ellipsis,
                                    style: VeyloText.ui(15, weight: 400, color: VeyloColors.synopsis),
                                  ),
                                ],
                              ],
                            ),
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
      ),
    );
  }
}
