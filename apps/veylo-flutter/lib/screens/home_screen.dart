import 'dart:async';

import 'package:flutter/widgets.dart';

import '../bridge/veylo_bridge.dart';
import '../state/app_state.dart';
import '../theme/veylo_theme.dart';
import '../widgets/finding_runner.dart';
import '../widgets/glass.dart';
import '../widgets/title_prefetch.dart';
import '../widgets/veylo_icons.dart';

double _heroHeight(double width, double height) => (height * 0.64).clamp(
  width < 400
      ? 620.0
      : width <= 960
      ? 560.0
      : 440.0,
  660.0,
);

// ShelfStrip pads 14 px on each side of the 2:3 art. Home uses PosterCard's
// caption-free variant, with 4 px of clearance below the padded strip.
double _shelfHeight(double posterWidth) => posterWidth * 1.5 + 32;

class HomeScreen extends StatefulWidget {
  const HomeScreen({super.key});

  @override
  State<HomeScreen> createState() => _HomeScreenState();
}

class _HomeScreenState extends State<HomeScreen> {
  final ScrollController _scroll = ScrollController();
  List<MediaItem>? _initialContinue;
  bool? _showContinue;
  int? _demoAfter;
  Set<String>? _initialDislikes;

  @override
  void initState() {
    super.initState();
    _scroll.addListener(_onScroll);
  }

  @override
  void dispose() {
    _scroll.dispose();
    super.dispose();
  }

  void _onScroll() =>
      AppStateScope.read(context).setScrolled(_scroll.offset > 4);

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.of(context);
    final size = MediaQuery.sizeOf(context);
    final gutter = VeyloLayout.gutter(size.width);
    final posterW = VeyloLayout.posterWidth(size.width);
    final insets = MediaQuery.viewPaddingOf(context);
    final shelfInsets = insets.copyWith(top: 0, bottom: 0);
    final height = _heroHeight(size.width, size.height) + insets.top;
    if (app.initialized) {
      if (_showContinue == null) {
        if (app.homeLibraryReady) {
          _initialContinue = app.libraryItems
              .where((m) => m.inProgress)
              .toList();
        }
        _showContinue = _initialContinue?.isNotEmpty ?? true;
      }
      _initialDislikes ??= app.libraryItems
          .where((m) => m.liked < 0)
          .map((m) => m.id)
          .toSet();
    }

    final rows = app.homeRows;
    if (rows.isNotEmpty) _demoAfter ??= rows.length;

    return CustomScrollView(
      controller: _scroll,
      scrollCacheExtent: const ScrollCacheExtent.pixels(600),
      slivers: [
        SliverToBoxAdapter(
          child: SizedBox(
            key: const ValueKey('shift.home.hero'),
            height: height,
            child: !app.initialized
                ? _HeroSkeleton(gutter: gutter)
                : ListenableBuilder(
                    listenable: Listenable.merge([
                      app.tonightPicksNotifier,
                      app.homeHeroIndexNotifier,
                      app.homeLoadingNotifier,
                    ]),
                    builder: (_, _) {
                      final picks = app.tonightPicks
                          .where((m) => !_initialDislikes!.contains(m.id))
                          .take(4)
                          .toList();
                      return _HomeHero(
                        items: picks,
                        activeIndex: picks.isEmpty
                            ? 0
                            : app.homeHeroIndex % picks.length,
                        loading: app.homeLoading,
                        gutter: gutter,
                        viewportWidth: size.width,
                        onSelect: app.setHomeHeroIndex,
                        onDislike: app.dislikeHero,
                      );
                    },
                  ),
          ),
        ),
        if (app.initialized)
          SliverToBoxAdapter(
            child: _showContinue!
                ? Padding(
                    padding: shelfInsets,
                    child: _ContinueShelf(gutter: gutter, initial: _initialContinue),
                  )
                : const SizedBox(height: 8),
          ),
        if (app.initialized && rows.isEmpty)
          SliverToBoxAdapter(
            child: Padding(
              padding: const EdgeInsets.only(bottom: 26) + shelfInsets,
              child: Column(
                children: [
                  const SizedBox(height: 56),
                  SizedBox(
                    height: _shelfHeight(posterW),
                    child: app.homeError == null
                        ? _SkeletonShelf(gutter: gutter, posterWidth: posterW)
                        : _ShelfMessage(gutter: gutter, error: app.homeError),
                  ),
                ],
              ),
            ),
          ),
        SliverList.builder(
          itemCount: rows.isEmpty ? 0 : rows.length + 1,
          itemBuilder: (ctx, i) {
            if (i == _demoAfter) {
              return Padding(
                key: const ValueKey('home-demo'),
                padding: EdgeInsets.fromLTRB(gutter, 8, gutter, 56) + shelfInsets,
                child: Align(
                  alignment: Alignment.centerLeft,
                  child: _DemoRow(onTap: app.playDemo),
                ),
              );
            }
            final index = i < _demoAfter! ? i : i - 1;
            final row = rows[index];
            return Padding(
              key: ValueKey(row.key),
              padding: const EdgeInsets.only(bottom: 26) + shelfInsets,
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Padding(
                    padding: EdgeInsets.symmetric(horizontal: gutter),
                    child: _SectionHeader(
                      key: ValueKey('shift.home.row.$index'),
                      title: row.title,
                      subtitle: row.subtitle,
                    ),
                  ),
                  _IsolatedShelfRow(
                    key: ValueKey('shift.home.strip.$index'),
                    row: row,
                    gutter: gutter,
                    posterWidth: posterW,
                    disliked: _initialDislikes!,
                  ),
                ],
              ),
            );
          },
        ),
        SliverPadding(padding: EdgeInsets.only(bottom: insets.bottom)),
      ],
    );
  }
}

class _ContinueShelf extends StatefulWidget {
  const _ContinueShelf({required this.gutter, required this.initial});
  final double gutter;
  final List<MediaItem>? initial;

  @override
  State<_ContinueShelf> createState() => _ContinueShelfState();
}

class _ContinueShelfState extends State<_ContinueShelf> {
  List<MediaItem>? _items;

  @override
  void initState() {
    super.initState();
    _items = widget.initial;
  }

  @override
  Widget build(BuildContext context) {
    final gutter = widget.gutter;
    final app = AppStateScope.read(context);
    return Padding(
      padding: const EdgeInsets.only(top: 8, bottom: 26),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Padding(
            padding: EdgeInsets.symmetric(horizontal: gutter),
            child: const _SectionHeader(
              key: ValueKey('shift.home.continue'),
              title: 'Continue watching',
              height: 38,
            ),
          ),
          SizedBox(
            key: const ValueKey('shift.home.continue-strip'),
            width: double.infinity,
            height: 214,
            child: ValueListenableBuilder<List<MediaItem>>(
              valueListenable: app.libraryItemsNotifier,
              builder: (_, library, _) {
                if (app.homeLibraryReady) {
                  _items ??= library.where((m) => m.inProgress).toList();
                }
                final continuing = _items ?? const <MediaItem>[];
                if (continuing.isEmpty) {
                  return _ShelfMessage(
                    gutter: gutter,
                    message: 'Nothing to continue',
                  );
                }
                return ShelfStrip(
                  gutter: gutter,
                  gap: 18,
                  height: 214,
                  children: [
                    for (final m in continuing)
                      _ContinueCard(
                        key: ValueKey('cw-${m.key}'),
                        media: m,
                        onForget: () {
                          setState(
                            () => _items = continuing
                                .where((item) => item.key != m.key)
                                .toList(),
                          );
                          unawaited(app.forgetTitle(m));
                        },
                      ),
                  ],
                );
              },
            ),
          ),
        ],
      ),
    );
  }
}

class _IsolatedShelfRow extends StatelessWidget {
  const _IsolatedShelfRow({
    super.key,
    required this.row,
    required this.gutter,
    required this.posterWidth,
    required this.disliked,
  });
  final CatalogRowModel row;
  final double gutter;
  final double posterWidth;
  final Set<String> disliked;

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final height = _shelfHeight(posterWidth);
    final shelf = app.shelfNotifier(row.key);
    final error = app.shelfErrorNotifier(row.key);
    return SizedBox(
      height: height,
      width: double.infinity,
      child: ListenableBuilder(
        listenable: Listenable.merge([shelf, error]),
        builder: (_, _) {
          if (error.value case final message?) {
            return _ShelfMessage(
              gutter: gutter,
              error: message,
              onRetry: () => app.retryShelf(row),
            );
          }
          final items = shelf.value;
          if (items == null) {
            return _SkeletonShelf(gutter: gutter, posterWidth: posterWidth);
          }
          final visible = items
              .where((m) => !disliked.contains(m.id))
              .take(40)
              .toList();
          if (visible.isEmpty) return _ShelfMessage(gutter: gutter);
          return ShelfStrip(
            key: const ValueKey('home.content'),
            gutter: gutter,
            gap: 16,
            height: height,
            children: [
              for (final m in visible)
                PosterCard(
                  key: ValueKey('${row.key}-${m.key}'),
                  media: m,
                  width: posterWidth,
                  animateMotion: false,
                ),
            ],
          );
        },
      ),
    );
  }
}

class _ShelfMessage extends StatelessWidget {
  const _ShelfMessage({
    required this.gutter,
    this.message = 'Empty',
    this.error,
    this.onRetry,
  });
  final double gutter;
  final String message;
  final String? error;
  final VoidCallback? onRetry;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: EdgeInsets.symmetric(horizontal: gutter, vertical: 14),
      child: Align(
        alignment: Alignment.topLeft,
        child: Row(
          children: [
            if (error != null) ...[
              const VeyloIcon('Alert', size: 20, color: VeyloColors.amber),
              const SizedBox(width: 10),
            ],
            Expanded(
              child: Text(
                error ?? message,
                maxLines: 3,
                overflow: TextOverflow.ellipsis,
                style: VeyloText.ui(
                  15,
                  color: error == null ? VeyloColors.dust : VeyloColors.amber,
                ),
              ),
            ),
            if (onRetry != null)
              VeyloOrb(
                icon: 'Refresh',
                size: 38,
                tooltip: 'Try again',
                onTap: onRetry!,
              ),
          ],
        ),
      ),
    );
  }
}

class _HomeHero extends StatefulWidget {
  const _HomeHero({
    required this.items,
    required this.activeIndex,
    required this.loading,
    required this.gutter,
    required this.viewportWidth,
    required this.onSelect,
    required this.onDislike,
  });

  final List<MediaItem> items;
  final int activeIndex;
  final bool loading;
  final double gutter;
  final double viewportWidth;
  final ValueChanged<int> onSelect;
  final ValueChanged<MediaItem> onDislike;

  @override
  State<_HomeHero> createState() => _HomeHeroState();
}

typedef _HeroArt = ({String backdrop, String? logo, String blurb});

class _HomeHeroState extends State<_HomeHero> {
  static const _logoWidth = 800;
  final Map<String, Future<_HeroArt>> _resolved = {};
  final Map<String, _HeroArt> _frozen = {};
  ({MediaItem media, _HeroArt art, int width})? _shown;
  int _preparation = 0;

  MediaItem? get _selected =>
      widget.items.isEmpty ? null : widget.items[widget.activeIndex];
  int get _decodeWidth => heroDecodeWidth(
    widget.viewportWidth,
    MediaQuery.devicePixelRatioOf(context),
  );

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    _scheduleReveal();
  }

  @override
  void didUpdateWidget(covariant _HomeHero oldWidget) {
    super.didUpdateWidget(oldWidget);
    final old = oldWidget.items.isEmpty
        ? null
        : oldWidget.items[oldWidget.activeIndex];
    if (old?.key != _selected?.key ||
        oldWidget.viewportWidth != widget.viewportWidth) {
      _scheduleReveal();
    }
  }

  void _scheduleReveal() {
    final ticket = ++_preparation;
    final media = _selected;
    if (media == null) {
      _shown = null;
      return;
    }
    final width = _decodeWidth;
    if (_shown?.media.key == media.key && _shown?.width == width) return;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted && ticket == _preparation) {
        unawaited(_prepareReveal(media, width, ticket));
      }
    });
  }

  Future<_HeroArt> _resolveArt(AppState app, MediaItem media) async {
    final art =
        app.homeArtwork[media.key] ?? await app.fetchArtwork(media, home: true);
    // Hold the chosen art on tab return as well. Live refresh writes the next
    // launch's snapshot without changing the artwork already chosen here.
    if (art != null) app.homeArtwork.putIfAbsent(media.key, () => art);
    final logo = art?.logo?.isNotEmpty == true ? art!.logo! : media.logo;
    return (
      backdrop: art?.backdrop?.isNotEmpty == true
          ? art!.backdrop!
          : media.backdrop,
      logo: logo.isEmpty ? null : logo,
      blurb: art?.tagline?.isNotEmpty == true
          ? art!.tagline!
          : media.description,
    );
  }

  Future<void> _prepareReveal(MediaItem media, int width, int ticket) async {
    final app = AppStateScope.read(context);
    for (final item in widget.items) {
      _resolved.putIfAbsent(item.key, () => _resolveArt(app, item));
    }
    final frozen = _frozen[media.key];
    final art = frozen ?? await _resolved[media.key]!;
    if (!mounted || ticket != _preparation) return;

    Future<String?> prepare(
      String? url,
      String fallback,
      ImageProvider Function(String) provider,
    ) async {
      if (url != null &&
          url.isNotEmpty &&
          await precacheArtwork(provider(url), context)) {
        return url;
      }
      if (frozen == null &&
          mounted &&
          ticket == _preparation &&
          fallback.isNotEmpty &&
          fallback != url &&
          await precacheArtwork(provider(fallback), context)) {
        return fallback;
      }
      return null;
    }

    final prepared = await Future.wait([
      prepare(art.backdrop, media.backdrop, (url) => heroProvider(url, width)),
      prepare(
        art.logo,
        media.logo,
        (url) => logoProvider(url, decodeWidth: _logoWidth),
      ),
    ]);
    if (!mounted || ticket != _preparation) return;

    final ready = (
      backdrop: prepared[0] ?? '',
      logo: prepared[1],
      blurb: art.blurb,
    );
    _frozen.putIfAbsent(media.key, () => ready);
    final changedItem = _shown?.media.key != media.key;
    setState(() => _shown = (media: media, art: ready, width: width));
    if (changedItem &&
        app.tab == VeyloTab.home &&
        app.openedTitle == null &&
        app.activePlayer == null) {
      unawaited(app.setLampSource(ready.backdrop));
    }
  }

  @override
  Widget build(BuildContext context) {
    final shown = _shown;
    final app = AppStateScope.read(context);
    final insets = MediaQuery.viewPaddingOf(context);
    final width = widget.viewportWidth - insets.horizontal;
    final narrow = width <= 960;
    final phone = width < 400;
    final railVisible = widget.items.length > 1;
    final thumbWidth = ((width - 2 * widget.gutter - 46) / 4)
        .clamp(32.0, 68.0);
    final railWidth = thumbWidth * 4 + 46;

    if (shown == null) {
      return widget.loading || widget.items.isNotEmpty
          ? _HeroSkeleton(gutter: widget.gutter)
          : const Center(
              child: Text('No picks available', style: VeyloText.meta),
            );
    }

    return TweenAnimationBuilder<double>(
      tween: Tween(begin: 0, end: 1),
      duration: const Duration(milliseconds: 180),
      builder: (_, opacity, child) => Opacity(opacity: opacity, child: child),
      child: Stack(
        fit: StackFit.expand,
        children: [
          RepaintBoundary(
            child: ShaderMask(
              shaderCallback: (bounds) => const LinearGradient(
                begin: Alignment.topCenter,
                end: Alignment.bottomCenter,
                stops: [0.0, 0.54, 1.0],
                colors: [
                  Color(0xFFFFFFFF),
                  Color(0xFFFFFFFF),
                  Color(0x00FFFFFF),
                ],
              ).createShader(bounds),
              blendMode: BlendMode.dstIn,
              child: AnimatedSwitcher(
                duration: const Duration(milliseconds: 500),
                child: shown.art.backdrop.isNotEmpty
                    ? KenBurnsImage(
                        key: ValueKey((shown.art.backdrop, shown.width)),
                        url: shown.art.backdrop,
                        cacheWidth: shown.width,
                        settle: false,
                        fadeIn: Duration.zero,
                      )
                    : const SizedBox.shrink(),
              ),
            ),
          ),
          // Exact CSS `.home-hero::before` scrims (pages.css:228-236)
          IgnorePointer(
            child: ShaderMask(
              shaderCallback: (bounds) => const LinearGradient(
                begin: Alignment.topCenter,
                end: Alignment.bottomCenter,
                stops: [0.0, 0.54, 1.0],
                colors: [
                  Color(0xFFFFFFFF),
                  Color(0xFFFFFFFF),
                  Color(0x00FFFFFF),
                ],
              ).createShader(bounds),
              blendMode: BlendMode.dstIn,
              child: const Stack(
                fit: StackFit.expand,
                children: [
                  DecoratedBox(
                    decoration: BoxDecoration(
                      gradient: LinearGradient(
                        begin: Alignment.centerLeft,
                        end: Alignment.centerRight,
                        stops: [0.0, 0.44, 0.78],
                        colors: [
                          Color.fromRGBO(0, 0, 0, 0.84),
                          Color.fromRGBO(0, 0, 0, 0.46),
                          Color.fromRGBO(0, 0, 0, 0.10),
                        ],
                      ),
                    ),
                  ),
                  DecoratedBox(
                    decoration: BoxDecoration(
                      gradient: LinearGradient(
                        begin: Alignment.topCenter,
                        end: Alignment.bottomCenter,
                        stops: [0.0, 0.28, 0.72, 1.0],
                        colors: [
                          Color.fromRGBO(0, 0, 0, 0.52),
                          Color.fromRGBO(0, 0, 0, 0.0),
                          Color.fromRGBO(0, 0, 0, 0.28),
                          Color.fromRGBO(0, 0, 0, 0.72),
                        ],
                      ),
                    ),
                  ),
                ],
              ),
            ),
          ),
          Positioned(
            left: widget.gutter + insets.left,
            right: widget.gutter + insets.right + (narrow ? 0 : railWidth + 32),
            bottom: narrow ? 170 : 34,
            child: Align(
              alignment: Alignment.bottomLeft,
              child: ConstrainedBox(
                constraints: const BoxConstraints(maxWidth: 640),
                child: SizedBox(
                  key: const ValueKey('shift.home.hero-content'),
                  width: double.infinity,
                  child: _heroCopy(
                    app,
                    shown.media,
                    shown.art,
                    phone: phone,
                    narrow: narrow,
                  ),
                ),
              ),
            ),
          ),
          if (railVisible)
            Positioned(
              left: narrow ? widget.gutter + insets.left : null,
              right: narrow ? null : widget.gutter + insets.right,
              bottom: 34,
              child: SizedBox(
                key: const ValueKey('shift.home.rail'),
                width: railWidth,
                height: 118,
                child: _heroRail(app, shown.media, thumbWidth),
              ),
            ),
        ],
      ),
    );
  }

  Widget _heroCopy(
    AppState app,
    MediaItem media,
    _HeroArt art, {
    required bool phone,
    required bool narrow,
  }) {
    final facts = [
      if (media.year.isNotEmpty) media.year,
      if (media.genres.isNotEmpty) media.genres.first,
      if (media.runtime.isNotEmpty) media.runtime,
    ];
    final logo = art.logo;
    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        SizedBox(
          height: narrow ? 108 : 120,
          width: 400,
          child: MouseRegion(
            cursor: SystemMouseCursors.click,
            child: GestureDetector(
              onTap: () => app.openTitle(media),
              child: logo == null
                  ? Align(
                      alignment: Alignment.bottomLeft,
                      child: _heroNameText(media.name),
                    )
                  : Image(
                      image: logoProvider(logo, decodeWidth: _logoWidth),
                      fit: BoxFit.contain,
                      alignment: Alignment.bottomLeft,
                      errorBuilder: (_, _, _) => Align(
                        alignment: Alignment.bottomLeft,
                        child: _heroNameText(media.name),
                      ),
                    ),
            ),
          ),
        ),
        const SizedBox(height: 8),
        SizedBox(
          height: phone ? 42 : 22,
          child: Wrap(
            crossAxisAlignment: WrapCrossAlignment.center,
            children: [
              if (media.rating.isNotEmpty) ...[
                const VeyloIcon(
                  'Star',
                  size: 14,
                  color: VeyloColors.amber,
                  filled: true,
                ),
                const SizedBox(width: 5),
                Text(
                  media.rating,
                  style: VeyloText.ui(14.5, weight: 600, tabular: true),
                ),
              ],
              for (int i = 0; i < facts.length; i++) ...[
                if (media.rating.isNotEmpty || i > 0)
                  Padding(
                    padding: const EdgeInsets.symmetric(horizontal: 9),
                    child: Text(
                      '·',
                      style: VeyloText.ui(
                        14.5,
                        weight: 600,
                        color: const Color.fromRGBO(242, 239, 234, 0.38),
                      ),
                    ),
                  ),
                Text(
                  facts[i],
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: VeyloText.ui(
                    14.5,
                    weight: 600,
                    color: const Color.fromRGBO(242, 239, 234, 0.82),
                    tabular: true,
                  ),
                ),
              ],
            ],
          ),
        ),
        const SizedBox(height: 8),
        SizedBox(
          height: 48,
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 560),
            child: Text(
              art.blurb,
              maxLines: 2,
              overflow: TextOverflow.ellipsis,
              style: VeyloText.ui(
                16,
                height: 1.48,
                color: const Color.fromRGBO(242, 239, 234, 0.84),
                shadows: const [
                  Shadow(color: Color.fromRGBO(0, 0, 0, 0.65), blurRadius: 14),
                ],
              ),
            ),
          ),
        ),
        const SizedBox(height: 12),
        SizedBox(
          height: phone ? 106 : 56,
          child: Wrap(
            spacing: 10,
            runSpacing: 10,
            crossAxisAlignment: WrapCrossAlignment.center,
            children: [
              VeyloPill(
                label: 'Play',
                icon: 'Play',
                accent: true,
                big: true,
                onTap: () => app.autoplayMedia(media),
              ),
              VeyloPill(
                label: 'Details',
                icon: 'Info',
                onTap: () => app.openTitle(media),
              ),
              VeyloOrb(
                icon: 'ThumbDown',
                tooltip: 'Not for me',
                onTap: () => widget.onDislike(media),
              ),
              VeyloOrb(
                icon: 'ArrowRight',
                tooltip: 'More picks',
                onTap: () => app.setTab(VeyloTab.picks),
              ),
            ],
          ),
        ),
        const SizedBox(height: 8),
        SizedBox(
          height: 38,
          child: media.reason.isEmpty
              ? null
              : ValueListenableBuilder<Color>(
                  valueListenable: app.lampColorNotifier,
                  builder: (_, lamp, _) => Row(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Padding(
                        padding: const EdgeInsets.only(top: 2.5),
                        child: VeyloIcon(
                          'Sparkles',
                          size: 13,
                          color: VeyloColors.mixLamp(
                            lamp,
                            const Color.fromRGBO(242, 239, 234, 0.55),
                            0.65,
                          ),
                          filled: true,
                        ),
                      ),
                      const SizedBox(width: 6),
                      Flexible(
                        child: Text(
                          media.reason,
                          maxLines: 2,
                          overflow: TextOverflow.ellipsis,
                          style: VeyloText.ui(
                            13,
                            weight: 500,
                            height: 1.38,
                            color: const Color.fromRGBO(242, 239, 234, 0.54),
                          ),
                        ),
                      ),
                    ],
                  ),
                ),
        ),
      ],
    );
  }

  Widget _heroNameText(String name) {
    final fs = (widget.viewportWidth * 0.054).clamp(40.0, 62.0);
    return Text(
      name,
      maxLines: 2,
      overflow: TextOverflow.ellipsis,
      style: VeyloText.display(
        fs,
        weight: 800,
        height: 0.95,
        shadows: const [
          Shadow(
            color: Color.fromRGBO(0, 0, 0, 0.55),
            blurRadius: 24,
            offset: Offset(0, 4),
          ),
        ],
      ),
    );
  }

  Widget _heroRail(AppState app, MediaItem shown, double thumbWidth) {
    return GlassBox(
      borderRadius: 18,
      padding: const EdgeInsets.all(8),
      backgroundOverride: const Color.fromRGBO(16, 17, 22, 0.56),
      child: Row(
        children: [
          for (int i = 0; i < widget.items.length; i++) ...[
            if (i > 0) const SizedBox(width: 10),
            _HeroRailThumb(
              key: ValueKey(widget.items[i].key),
              media: widget.items[i],
              width: thumbWidth,
              active: widget.items[i].key == shown.key,
              onTap: () {
                if (widget.items[i].key == shown.key &&
                    i == widget.activeIndex) {
                  app.openTitle(shown);
                } else {
                  widget.onSelect(i);
                }
              },
            ),
          ],
        ],
      ),
    );
  }
}

class _HeroRailThumb extends StatefulWidget {
  const _HeroRailThumb({
    super.key,
    required this.media,
    required this.width,
    required this.active,
    required this.onTap,
  });
  final double width;
  final MediaItem media;
  final bool active;
  final VoidCallback onTap;

  @override
  State<_HeroRailThumb> createState() => _HeroRailThumbState();
}

class _HeroRailThumbState extends State<_HeroRailThumb> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    return VeyloTooltip(
      message: widget.media.name,
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        onEnter: (_) => setState(() => _hovered = true),
        onExit: (_) => setState(() => _hovered = false),
        child: GestureDetector(
          onTap: widget.onTap,
          child: AnimatedContainer(
            duration: const Duration(milliseconds: 240),
            curve: VeyloCurves.out,
            width: widget.width,
            height: 102,
            decoration: BoxDecoration(
              borderRadius: BorderRadius.circular(11),
              border: Border.all(
                color: widget.active
                    ? VeyloColors.bone
                    : const Color.fromRGBO(255, 255, 255, 0.14),
                width: 2,
              ),
              boxShadow: widget.active
                  ? const [
                      BoxShadow(
                        color: Color.fromRGBO(0, 0, 0, 0.65),
                        blurRadius: 30,
                        offset: Offset(0, 14),
                      ),
                    ]
                  : null,
            ),
            child: AnimatedOpacity(
              duration: const Duration(milliseconds: 200),
              opacity: widget.active ? 1.0 : (_hovered ? 0.94 : 0.56),
              child: ClipRRect(
                borderRadius: BorderRadius.circular(9.5),
                child: FallbackImage(
                  sources: widget.media.posters,
                  cacheWidth: 160,
                  fallback: Container(
                    color: const Color.fromRGBO(255, 255, 255, 0.08),
                    alignment: Alignment.center,
                    padding: const EdgeInsets.all(6),
                    child: Text(
                      widget.media.name,
                      textAlign: TextAlign.center,
                      maxLines: 3,
                      overflow: TextOverflow.ellipsis,
                      style: VeyloText.display(
                        11,
                        weight: 700,
                        color: VeyloColors.bone,
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

class _HeroSkeleton extends StatelessWidget {
  const _HeroSkeleton({required this.gutter});
  final double gutter;

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final insets = MediaQuery.viewPaddingOf(context);
    return Container(
      padding: EdgeInsets.fromLTRB(gutter, 108, gutter, 34) + insets.copyWith(bottom: 0),
      decoration: const BoxDecoration(
        gradient: LinearGradient(
          begin: Alignment.topCenter,
          end: Alignment.bottomCenter,
          colors: [
            Color.fromRGBO(255, 255, 255, 0.03),
            Color.fromRGBO(255, 255, 255, 0.01),
            Color(0x00000000),
          ],
        ),
      ),
      alignment: Alignment.center,
      child: ValueListenableBuilder<Color>(
        valueListenable: app.lampColorNotifier,
        builder: (_, lamp, _) => FindingRunner(
          lamp: lamp,
          scene: CinemaScene.projector,
          width: 300,
          height: 104,
        ),
      ),
    );
  }
}

/// 16:9 widescreen Continue Watching card (.cw-card) with isolated artwork & lamp listeners.
class _ContinueCard extends StatefulWidget {
  const _ContinueCard({super.key, required this.media, required this.onForget});
  final MediaItem media;
  final VoidCallback onForget;

  @override
  State<_ContinueCard> createState() => _ContinueCardState();
}

class _ContinueCardState extends State<_ContinueCard> {
  bool _hovered = false;
  bool _armed = false;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) {
        unawaited(AppStateScope.read(context).fetchArtwork(widget.media));
      }
    });
  }

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final m = widget.media;
    final ep = m.episodeLabel;
    final remaining = ep != null && m.resumeContext.startsWith(ep)
        ? m.resumeContext
              .substring(ep.length)
              .replaceFirst(RegExp(r'^,\s*'), '')
        : m.resumeContext;

    return RepaintBoundary(
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        onEnter: (_) => setState(() => _hovered = true),
        onExit: (_) => setState(() => _hovered = false),
        child: AnimatedContainer(
          duration: const Duration(milliseconds: 260),
          curve: VeyloCurves.out,
          width: 312,
          height: 176,
          decoration: BoxDecoration(
            borderRadius: BorderRadius.circular(18),
            color: const Color.fromRGBO(255, 255, 255, 0.06),
            boxShadow: [
              BoxShadow(
                color: Color.fromRGBO(0, 0, 0, _hovered ? 0.78 : 0.42),
                blurRadius: _hovered ? 48 : 28,
                spreadRadius: _hovered ? -12 : 0,
                offset: Offset(0, _hovered ? 24 : 12),
              ),
            ],
          ),
          child: ClipRRect(
            borderRadius: BorderRadius.circular(18),
            child: Stack(
              fit: StackFit.expand,
              children: [
                GestureDetector(
                  onTap: () => app.openTitle(m),
                  child: Stack(
                    fit: StackFit.expand,
                    children: [
                      ValueListenableBuilder<ArtworkModel?>(
                        valueListenable: app.artworkNotifier(m.key),
                        builder: (_, art, _) {
                          final bgSources = <String>[
                            if (art?.backdrop != null &&
                                art!.backdrop!.isNotEmpty)
                              art.backdrop!,
                            if (m.backdrop.isNotEmpty) m.backdrop,
                            if (m.poster.isNotEmpty) m.poster,
                          ];
                          return FallbackImage(
                            sources: bgSources,
                            cacheWidth: 640,
                          );
                        },
                      ),
                      const DecoratedBox(
                        decoration: BoxDecoration(
                          gradient: LinearGradient(
                            begin: Alignment.topCenter,
                            end: Alignment.bottomCenter,
                            stops: [0.24, 0.64, 1.0],
                            colors: [
                              Color.fromRGBO(0, 0, 0, 0.04),
                              Color.fromRGBO(0, 0, 0, 0.52),
                              Color.fromRGBO(0, 0, 0, 0.92),
                            ],
                          ),
                        ),
                      ),
                    ],
                  ),
                ),
                if (ep != null)
                  Positioned(
                    top: 12,
                    left: 12,
                    child: Container(
                      height: 24,
                      padding: const EdgeInsets.symmetric(horizontal: 9),
                      alignment: Alignment.center,
                      decoration: BoxDecoration(
                        color: const Color.fromRGBO(10, 11, 14, 0.82),
                        borderRadius: BorderRadius.circular(12),
                        border: Border.all(
                          color: const Color.fromRGBO(255, 255, 255, 0.16),
                        ),
                      ),
                      child: Text(
                        ep,
                        style: VeyloText.ui(12, weight: 750, tabular: true),
                      ),
                    ),
                  ),
                Positioned(
                  top: 10,
                  right: 10,
                  child: AnimatedOpacity(
                    opacity: (_hovered || _armed) ? 1.0 : 0.0,
                    duration: const Duration(milliseconds: 160),
                    child: _armed
                        ? Row(
                            mainAxisSize: MainAxisSize.min,
                            children: [
                              VeyloPill(
                                label: 'Remove',
                                icon: 'Trash',
                                confirmRed: true,
                                small: true,
                                onTap: () {
                                  setState(() => _armed = false);
                                  widget.onForget();
                                },
                              ),
                              const SizedBox(width: 6),
                              VeyloOrb(
                                icon: 'Close',
                                size: 36,
                                backgroundOverride: const Color.fromRGBO(
                                  10,
                                  11,
                                  15,
                                  0.78,
                                ),
                                onTap: () => setState(() => _armed = false),
                              ),
                            ],
                          )
                        : VeyloOrb(
                            icon: 'Trash',
                            size: 36,
                            iconSize: 17,
                            tooltip: 'Remove from Continue watching',
                            backgroundOverride: const Color.fromRGBO(
                              10,
                              11,
                              15,
                              0.78,
                            ),
                            onTap: () => setState(() => _armed = true),
                          ),
                  ),
                ),
                Positioned(
                  left: 16,
                  right: 72,
                  bottom: 16,
                  child: IgnorePointer(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        Text(
                          m.name,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: VeyloText.display(
                            22,
                            weight: 750,
                            height: 1.05,
                            shadows: const [
                              Shadow(
                                color: Color.fromRGBO(0, 0, 0, 0.7),
                                blurRadius: 12,
                              ),
                            ],
                          ),
                        ),
                        if (remaining.isNotEmpty) ...[
                          const SizedBox(height: 3),
                          Text(
                            remaining,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: VeyloText.ui(
                              13,
                              weight: 600,
                              color: const Color.fromRGBO(242, 239, 234, 0.75),
                              tabular: true,
                            ),
                          ),
                        ],
                      ],
                    ),
                  ),
                ),
                Positioned(
                  right: 14,
                  bottom: 14,
                  child: VeyloOrb(
                    icon: 'Play',
                    size: 44,
                    iconSize: 18,
                    tooltip: 'Resume ${m.name}',
                    backgroundOverride: const Color.fromRGBO(16, 17, 22, 0.80),
                    onTap: () => app.autoplayMedia(m),
                  ),
                ),
                if (m.progressFraction > 0)
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
                        child: FractionallySizedBox(
                          widthFactor: m.progressFraction.clamp(0.0, 1.0),
                          child: Container(
                            decoration: BoxDecoration(
                              color: lamp,
                              boxShadow: [
                                BoxShadow(
                                  color: lamp.withValues(alpha: 0.65),
                                  blurRadius: 10,
                                ),
                              ],
                            ),
                          ),
                        ),
                      ),
                    ),
                  ),
                IgnorePointer(
                  child: DecoratedBox(
                    decoration: BoxDecoration(
                      borderRadius: BorderRadius.circular(18),
                      border: Border.all(
                        color: _hovered
                            ? const Color.fromRGBO(255, 255, 255, 0.22)
                            : const Color.fromRGBO(255, 255, 255, 0.11),
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

class _SectionHeader extends StatelessWidget {
  const _SectionHeader({
    super.key,
    required this.title,
    this.subtitle,
    this.height = 56,
  });
  final double height;
  final String title;
  final String? subtitle;

  @override
  Widget build(BuildContext context) {
    return SizedBox(
      height: height,
      child: Align(
        alignment: Alignment.bottomLeft,
        child: Padding(
          padding: const EdgeInsets.only(bottom: 4),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisSize: MainAxisSize.min,
            children: [
              Text(
                title,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: VeyloText.section,
              ),
              if (subtitle != null && subtitle!.isNotEmpty) ...[
                const SizedBox(height: 3),
                Text(
                  subtitle!,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: VeyloText.micro,
                ),
              ],
            ],
          ),
        ),
      ),
    );
  }
}

class _SkeletonShelf extends StatelessWidget {
  const _SkeletonShelf({required this.gutter, required this.posterWidth});
  final double gutter;
  final double posterWidth;

  @override
  Widget build(BuildContext context) {
    return SizedBox(
      height: _shelfHeight(posterWidth),
      child: ListView.separated(
        scrollDirection: Axis.horizontal,
        padding: EdgeInsets.symmetric(horizontal: gutter, vertical: 14),
        itemCount: 7,
        separatorBuilder: (_, _) => const SizedBox(width: 16),
        itemBuilder: (_, _) => Align(
          alignment: Alignment.topCenter,
          child: SkeletonBox(width: posterWidth, height: posterWidth * 1.5),
        ),
      ),
    );
  }
}

class _DemoRow extends StatelessWidget {
  const _DemoRow({required this.onTap});
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        child: GlassBox(
          borderRadius: 32,
          padding: const EdgeInsets.fromLTRB(14, 10, 22, 10),
          child: const Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              VeyloIcon('PlayCircle', size: 36, color: VeyloColors.bone),
              SizedBox(width: 14),
              Flexible(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Text('Test player', style: VeyloText.bodyStrong),
                    Text(
                      'Sintel, CC BY 3.0 Blender Foundation',
                      style: VeyloText.credit,
                    ),
                  ],
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
