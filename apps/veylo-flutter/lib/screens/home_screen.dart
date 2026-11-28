import 'dart:async';
import 'package:cached_network_image/cached_network_image.dart';
import 'package:flutter/widgets.dart';
import '../bridge/veylo_bridge.dart';
import '../state/app_state.dart';
import '../theme/veylo_theme.dart';
import '../widgets/finding_runner.dart';
import '../widgets/glass.dart';
import '../widgets/veylo_icons.dart';

/// Home screen: 120fps scroll parallax via ValueNotifier, per-shelf isolated rebuilds,
/// Ken Burns hero settle, staggered hero copy entrance, and horizontal edge-fade shelves.
class HomeScreen extends StatefulWidget {
  const HomeScreen({super.key});

  @override
  State<HomeScreen> createState() => _HomeScreenState();
}

class _HomeScreenState extends State<HomeScreen> {
  final ScrollController _scroll = ScrollController();
  final ValueNotifier<double> _heroScrollY = ValueNotifier<double>(0);

  @override
  void initState() {
    super.initState();
    _scroll.addListener(_onScroll);
  }

  @override
  void dispose() {
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

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.of(context);
    final size = MediaQuery.sizeOf(context);
    final gutter = VeyloLayout.gutter(size.width);
    final posterW = VeyloLayout.posterWidth(size.width);

    return CustomScrollView(
      controller: _scroll,
      scrollCacheExtent: const ScrollCacheExtent.pixels(600),
      slivers: [
        // Hero section (isolated rebuilds on tonightPicks / library / homeHeroIndex / homeLoading)
        SliverToBoxAdapter(
          child: ListenableBuilder(
            listenable: Listenable.merge([
              app.tonightPicksNotifier,
              app.libraryItemsNotifier,
              app.homeHeroIndexNotifier,
              app.homeLoadingNotifier,
            ]),
            builder: (_, _) {
              final lib = app.libraryItems;
              final disliked = lib.where((m) => m.liked < 0).map((m) => m.id).toSet();
              final heroPicks = app.tonightPicks.where((m) => !disliked.contains(m.id)).take(4).toList();
              final continuing = lib.where((m) => m.inProgress).toList();

              if (heroPicks.isEmpty && continuing.isNotEmpty) {
                final fb = continuing.first.backdrop;
                if (fb.isNotEmpty && app.lampSource != fb) {
                  WidgetsBinding.instance.addPostFrameCallback((_) => app.setLampSource(fb));
                }
              }

              if (heroPicks.isNotEmpty) {
                return _HomeHero(
                  items: heroPicks,
                  activeIndex: app.homeHeroIndex % heroPicks.length,
                  scrollYNotifier: _heroScrollY,
                  gutter: gutter,
                  viewportHeight: size.height,
                  viewportWidth: size.width,
                  onSelect: app.setHomeHeroIndex,
                  onDislike: app.dislikeHero,
                );
              }
              if (app.homeLoading && app.tonightPicks.isEmpty) {
                return _HeroSkeleton(gutter: gutter, viewportHeight: size.height);
              }
              return const SizedBox(height: 96);
            },
          ),
        ),

        // Continue watching widescreen shelf
        SliverToBoxAdapter(
          child: ValueListenableBuilder<List<MediaItem>>(
            valueListenable: app.libraryItemsNotifier,
            builder: (_, lib, _) {
              final continuing = lib.where((m) => m.inProgress).toList();
              if (continuing.isEmpty) return const SizedBox(height: 8);
              return Padding(
                padding: const EdgeInsets.only(top: 8, bottom: 26),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Padding(
                      padding: EdgeInsets.symmetric(horizontal: gutter),
                      child: const _SectionHeader(title: 'Continue watching'),
                    ),
                    ShelfStrip(
                      gutter: gutter,
                      gap: 18,
                      height: 214,
                      children: [
                        for (final m in continuing)
                          _ContinueCard(
                            key: ValueKey('cw-${m.key}'),
                            media: m,
                            onForget: () => app.forgetTitle(m),
                          ),
                      ],
                    ),
                  ],
                ),
              );
            },
          ),
        ),

        if (app.homeRows.isEmpty)
          SliverToBoxAdapter(
            child: ValueListenableBuilder<bool>(
              valueListenable: app.homeLoadingNotifier,
              builder: (_, loading, _) => loading
                  ? _SkeletonShelf(gutter: gutter, posterWidth: posterW)
                  : const SizedBox.shrink(),
            ),
          ),

        if (app.homeError != null)
          SliverToBoxAdapter(
            child: Padding(
              padding: EdgeInsets.symmetric(horizontal: gutter, vertical: 16),
              child: Row(
                children: [
                  const VeyloIcon('Alert', size: 20, color: VeyloColors.amber),
                  const SizedBox(width: 10),
                  Expanded(child: Text(app.homeError!, style: VeyloText.ui(15, color: VeyloColors.amber))),
                ],
              ),
            ),
          ),

        // Lazily virtualized catalog shelves: each shelf updates independently!
        SliverList.builder(
          itemCount: app.homeRows.length,
          itemBuilder: (ctx, i) {
            final row = app.homeRows[i];
            return Padding(
              key: ValueKey(row.key),
              padding: const EdgeInsets.only(bottom: 26),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Padding(
                    padding: EdgeInsets.symmetric(horizontal: gutter),
                    child: _SectionHeader(title: row.title, subtitle: row.subtitle),
                  ),
                  _IsolatedShelfRow(row: row, gutter: gutter, posterWidth: posterW),
                ],
              ),
            );
          },
        ),

        // Test player (Sintel) row
        SliverToBoxAdapter(
          child: Padding(
            padding: EdgeInsets.fromLTRB(gutter, 8, gutter, 56),
            child: Align(
              alignment: Alignment.centerLeft,
              child: _DemoRow(onTap: app.playDemo),
            ),
          ),
        ),
      ],
    );
  }
}

class _IsolatedShelfRow extends StatelessWidget {
  const _IsolatedShelfRow({required this.row, required this.gutter, required this.posterWidth});
  final CatalogRowModel row;
  final double gutter;
  final double posterWidth;

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    return ListenableBuilder(
      listenable: Listenable.merge([
        app.shelfNotifier(row.key),
        app.shelfErrorNotifier(row.key),
        app.libraryItemsNotifier,
      ]),
      builder: (_, _) {
        final err = app.shelfErrorNotifier(row.key).value;
        if (err != null) {
          return Padding(
            padding: EdgeInsets.symmetric(horizontal: gutter, vertical: 12),
            child: Row(
              children: [
                const VeyloIcon('Alert', size: 20, color: VeyloColors.amber),
                const SizedBox(width: 10),
                Expanded(child: Text(err, style: VeyloText.ui(15, color: VeyloColors.amber))),
                VeyloOrb(icon: 'Refresh', size: 38, tooltip: 'Try again', onTap: () => app.retryShelf(row)),
              ],
            ),
          );
        }
        final items = app.shelfNotifier(row.key).value;
        if (items == null) {
          return _SkeletonShelf(gutter: gutter, posterWidth: posterWidth);
        }
        final disliked = app.libraryItems.where((m) => m.liked < 0).map((m) => m.id).toSet();
        final visible = items.where((m) => !disliked.contains(m.id)).take(40).toList();
        if (visible.isEmpty) {
          return Padding(
            padding: EdgeInsets.symmetric(horizontal: gutter, vertical: 8),
            child: Text('Empty', style: VeyloText.meta.copyWith(color: VeyloColors.dust)),
          );
        }
        return ShelfStrip(
          gutter: gutter,
          gap: 16,
          height: posterWidth * 1.5 + 32,
          children: [
            for (final m in visible)
              PosterCard(
                key: ValueKey('${row.key}-${m.key}'),
                media: m,
                width: posterWidth,
              ),
          ],
        );
      },
    );
  }
}

class _HomeHero extends StatefulWidget {
  const _HomeHero({
    required this.items,
    required this.activeIndex,
    required this.scrollYNotifier,
    required this.gutter,
    required this.viewportHeight,
    required this.viewportWidth,
    required this.onSelect,
    required this.onDislike,
  });

  final List<MediaItem> items;
  final int activeIndex;
  final ValueNotifier<double> scrollYNotifier;
  final double gutter;
  final double viewportHeight;
  final double viewportWidth;
  final ValueChanged<int> onSelect;
  final ValueChanged<MediaItem> onDislike;

  @override
  State<_HomeHero> createState() => _HomeHeroState();
}

class _HomeHeroState extends State<_HomeHero> {
  final Set<String> _failedLogos = {};

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    _syncLamp();
  }

  @override
  void didUpdateWidget(covariant _HomeHero oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.activeIndex != widget.activeIndex || oldWidget.items.length != widget.items.length) {
      _syncLamp();
    }
  }

  void _syncLamp() {
    final app = AppStateScope.read(context);
    final media = widget.items[widget.activeIndex];
    final art = app.cachedArtwork(media.key);
    if (art == null) {
      unawaited(app.fetchArtwork(media).then((a) {
        if (mounted && a?.backdrop != null && a!.backdrop!.isNotEmpty) {
          app.setLampSource(a.backdrop!);
        }
      }));
    }
    final bg = _backdropOf(media, art);
    if (bg.isNotEmpty && app.lampSource != bg) {
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) app.setLampSource(bg);
      });
    }
  }

  String _backdropOf(MediaItem m, ArtworkModel? art) {
    if (art?.backdrop != null && art!.backdrop!.isNotEmpty) return art.backdrop!;
    if (m.background.isNotEmpty) return m.background;
    return m.backdrop;
  }

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final media = widget.items[widget.activeIndex];
    final heroHeight = (widget.viewportHeight * 0.64).clamp(440.0, 660.0);
    final narrow = widget.viewportWidth <= 960;

    return ValueListenableBuilder<ArtworkModel?>(
      valueListenable: app.artworkNotifier(media.key),
      builder: (_, art, _) {
        final bg = _backdropOf(media, art);
        final rawLogo = (art?.logo?.isNotEmpty == true ? art!.logo! : media.logo);
        final logo = rawLogo.isNotEmpty && !_failedLogos.contains(rawLogo) ? rawLogo : null;
        final blurb = (art?.tagline?.isNotEmpty == true ? art!.tagline! : media.description);
        final facts = <String>[
          if (media.year.isNotEmpty) media.year,
          if (media.genres.isNotEmpty) media.genres.first,
          if (media.runtime.isNotEmpty) media.runtime,
        ];

        return SizedBox(
          height: heroHeight,
          width: double.infinity,
          child: Stack(
            fit: StackFit.expand,
            children: [
              // Masked parallax Ken Burns crossfade backdrop (.home-art-stack: melts into AmbientBackground)
              RepaintBoundary(
                child: ShaderMask(
                  shaderCallback: (bounds) => const LinearGradient(
                    begin: Alignment.topCenter,
                    end: Alignment.bottomCenter,
                    stops: [0.0, 0.54, 1.0],
                    colors: [Color(0xFFFFFFFF), Color(0xFFFFFFFF), Color(0x00FFFFFF)],
                  ).createShader(bounds),
                  blendMode: BlendMode.dstIn,
                  child: ClipRect(
                    child: ValueListenableBuilder<double>(
                      valueListenable: widget.scrollYNotifier,
                      builder: (_, scrollY, child) => Transform.translate(
                        offset: Offset(0, scrollY * 0.28),
                        child: child,
                      ),
                      child: AnimatedSwitcher(
                        duration: const Duration(milliseconds: 800),
                        switchInCurve: const Cubic(0.4, 0.0, 0.2, 1.0),
                        child: bg.isNotEmpty
                            ? KenBurnsImage(key: ValueKey(bg), url: bg)
                            : const SizedBox.shrink(),
                      ),
                    ),
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
                    colors: [Color(0xFFFFFFFF), Color(0xFFFFFFFF), Color(0x00FFFFFF)],
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

              // Foreground copy + floating switcher rail
              Padding(
                padding: EdgeInsets.fromLTRB(widget.gutter, 108, widget.gutter, 34),
                child: narrow
                    ? Column(
                        mainAxisAlignment: MainAxisAlignment.end,
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          _heroCopy(app, media, logo, blurb, facts),
                          if (widget.items.length > 1) ...[
                            const SizedBox(height: 18),
                            _heroRail(app),
                          ],
                        ],
                      )
                    : Row(
                        crossAxisAlignment: CrossAxisAlignment.end,
                        mainAxisAlignment: MainAxisAlignment.spaceBetween,
                        children: [
                          Flexible(
                            child: ConstrainedBox(
                              constraints: const BoxConstraints(maxWidth: 640),
                              child: _heroCopy(app, media, logo, blurb, facts),
                            ),
                          ),
                          if (widget.items.length > 1) ...[
                            const SizedBox(width: 32),
                            _heroRail(app),
                          ],
                        ],
                      ),
              ),
            ],
          ),
        );
      },
    );
  }

  Widget _heroCopy(AppState app, MediaItem media, String? logo, String blurb, List<String> facts) {
    return Column(
      key: ValueKey(media.key),
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        LiftIn(
          key: ValueKey('logo-${media.key}'),
          child: MouseRegion(
            cursor: SystemMouseCursors.click,
            child: GestureDetector(
              onTap: () => app.openTitle(media),
              child: logo != null
                  ? ConstrainedBox(
                      constraints: const BoxConstraints(maxWidth: 400, maxHeight: 120),
                      child: CachedNetworkImage(
                        imageUrl: logo,
                        fit: BoxFit.contain,
                        alignment: Alignment.bottomLeft,
                        memCacheWidth: 800,
                        fadeInDuration: const Duration(milliseconds: 220),
                        errorWidget: (_, _, _) {
                          WidgetsBinding.instance.addPostFrameCallback((_) {
                            if (mounted) setState(() => _failedLogos.add(logo));
                          });
                          return _heroNameText(media.name);
                        },
                      ),
                    )
                  : _heroNameText(media.name),
            ),
          ),
        ),
        if (facts.isNotEmpty || media.rating.isNotEmpty) ...[
          const SizedBox(height: 12),
          LiftIn(
            key: ValueKey('facts-${media.key}'),
            delay: const Duration(milliseconds: 30),
            child: Wrap(
              crossAxisAlignment: WrapCrossAlignment.center,
              children: [
                if (media.rating.isNotEmpty) ...[
                  const VeyloIcon('Star', size: 14, color: VeyloColors.amber, filled: true),
                  const SizedBox(width: 5),
                  Text(media.rating, style: VeyloText.ui(14.5, weight: 600, tabular: true)),
                ],
                for (int i = 0; i < facts.length; i++) ...[
                  if (media.rating.isNotEmpty || i > 0)
                    Padding(
                      padding: const EdgeInsets.symmetric(horizontal: 9),
                      child: Text('·', style: VeyloText.ui(14.5, weight: 600, color: const Color.fromRGBO(242, 239, 234, 0.38))),
                    ),
                  Text(
                    facts[i],
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
        ],
        if (blurb.isNotEmpty) ...[
          const SizedBox(height: 10),
          LiftIn(
            key: ValueKey('blurb-${media.key}'),
            delay: const Duration(milliseconds: 60),
            child: ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 560),
              child: Text(
                blurb,
                maxLines: 2,
                overflow: TextOverflow.ellipsis,
                style: VeyloText.ui(
                  16,
                  weight: 400,
                  height: 1.48,
                  color: const Color.fromRGBO(242, 239, 234, 0.84),
                  shadows: const [Shadow(color: Color.fromRGBO(0, 0, 0, 0.65), blurRadius: 14)],
                ),
              ),
            ),
          ),
        ],
        const SizedBox(height: 18),
        LiftIn(
          key: ValueKey('act-${media.key}'),
          delay: const Duration(milliseconds: 90),
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
        if (media.reason.isNotEmpty) ...[
          const SizedBox(height: 12),
          LiftIn(
            key: ValueKey('rsn-${media.key}'),
            delay: const Duration(milliseconds: 110),
            child: ValueListenableBuilder<Color>(
              valueListenable: app.lampColorNotifier,
              builder: (_, lamp, _) => Row(
                mainAxisSize: MainAxisSize.min,
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Padding(
                    padding: const EdgeInsets.only(top: 2.5),
                    child: VeyloIcon(
                      'Sparkles',
                      size: 13,
                      color: VeyloColors.mixLamp(lamp, const Color.fromRGBO(242, 239, 234, 0.55), 0.65),
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
      ],
    );
  }

  Widget _heroNameText(String name) {
    final fs = (widget.viewportWidth * 0.054).clamp(40.0, 68.0);
    return Text(
      name,
      maxLines: 2,
      overflow: TextOverflow.ellipsis,
      style: VeyloText.display(
        fs,
        weight: 800,
        height: 0.95,
        shadows: const [Shadow(color: Color.fromRGBO(0, 0, 0, 0.55), blurRadius: 24, offset: Offset(0, 4))],
      ),
    );
  }

  Widget _heroRail(AppState app) {
    return GlassBox(
      borderRadius: 18,
      padding: const EdgeInsets.all(8),
      backgroundOverride: const Color.fromRGBO(16, 17, 22, 0.56),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          for (int i = 0; i < widget.items.length; i++) ...[
            if (i > 0) const SizedBox(width: 10),
            _HeroRailThumb(
              media: widget.items[i],
              active: i == widget.activeIndex,
              onTap: () {
                if (i == widget.activeIndex) {
                  app.openTitle(widget.items[i]);
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
  const _HeroRailThumb({required this.media, required this.active, required this.onTap});
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
            width: 68,
            height: 102,
            transform: Matrix4.identity()
              ..translateByDouble(0.0, widget.active ? -4.0 : (_hovered ? -2.0 : 0.0), 0.0, 1.0)
              ..scaleByDouble(widget.active ? 1.06 : 1.0, widget.active ? 1.06 : 1.0, 1.0, 1.0),
            transformAlignment: Alignment.center,
            decoration: BoxDecoration(
              borderRadius: BorderRadius.circular(11),
              border: Border.all(
                color: widget.active ? VeyloColors.bone : const Color.fromRGBO(255, 255, 255, 0.14),
                width: widget.active ? 2 : 1,
              ),
              boxShadow: widget.active
                  ? const [BoxShadow(color: Color.fromRGBO(0, 0, 0, 0.65), blurRadius: 30, offset: Offset(0, 14))]
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
                      style: VeyloText.display(11, weight: 700, color: VeyloColors.bone),
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
  const _HeroSkeleton({required this.gutter, required this.viewportHeight});
  final double gutter;
  final double viewportHeight;

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final h = (viewportHeight * 0.64).clamp(440.0, 660.0);
    return Container(
      height: h,
      padding: EdgeInsets.fromLTRB(gutter, 108, gutter, 34),
      decoration: const BoxDecoration(
        gradient: LinearGradient(
          begin: Alignment.topCenter,
          end: Alignment.bottomCenter,
          colors: [Color.fromRGBO(255, 255, 255, 0.03), Color.fromRGBO(255, 255, 255, 0.01), Color(0x00000000)],
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
      if (mounted) unawaited(AppStateScope.read(context).fetchArtwork(widget.media));
    });
  }

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final m = widget.media;
    final ep = m.episodeLabel;
    final remaining = ep != null && m.resumeContext.startsWith(ep)
        ? m.resumeContext.substring(ep.length).replaceFirst(RegExp(r'^,\s*'), '')
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
          transform: Matrix4.identity()
            ..translateByDouble(0.0, _hovered ? -4.0 : 0.0, 0.0, 1.0)
            ..scaleByDouble(_hovered ? 1.025 : 1.0, _hovered ? 1.025 : 1.0, 1.0, 1.0),
          transformAlignment: Alignment.center,
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
                            if (art?.backdrop != null && art!.backdrop!.isNotEmpty) art.backdrop!,
                            if (m.backdrop.isNotEmpty) m.backdrop,
                            if (m.poster.isNotEmpty) m.poster,
                          ];
                          return AnimatedScale(
                            scale: _hovered ? 1.04 : 1.0,
                            duration: const Duration(milliseconds: 350),
                            curve: VeyloCurves.out,
                            child: FallbackImage(sources: bgSources, cacheWidth: 640),
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
                        border: Border.all(color: const Color.fromRGBO(255, 255, 255, 0.16)),
                      ),
                      child: Text(ep, style: VeyloText.ui(12, weight: 750, tabular: true)),
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
                                backgroundOverride: const Color.fromRGBO(10, 11, 15, 0.78),
                                onTap: () => setState(() => _armed = false),
                              ),
                            ],
                          )
                        : VeyloOrb(
                            icon: 'Trash',
                            size: 36,
                            iconSize: 17,
                            tooltip: 'Remove from Continue watching',
                            backgroundOverride: const Color.fromRGBO(10, 11, 15, 0.78),
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
                            shadows: const [Shadow(color: Color.fromRGBO(0, 0, 0, 0.7), blurRadius: 12)],
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
                              boxShadow: [BoxShadow(color: lamp.withValues(alpha: 0.65), blurRadius: 10)],
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
                        color: _hovered ? const Color.fromRGBO(255, 255, 255, 0.22) : const Color.fromRGBO(255, 255, 255, 0.11),
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
  const _SectionHeader({required this.title, this.subtitle});
  final String title;
  final String? subtitle;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.only(bottom: 4),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(title, style: VeyloText.section),
          if (subtitle != null && subtitle!.isNotEmpty) ...[
            const SizedBox(height: 3),
            Text(subtitle!, style: VeyloText.micro),
          ],
        ],
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
      height: posterWidth * 1.5 + 28,
      child: ListView.separated(
        scrollDirection: Axis.horizontal,
        padding: EdgeInsets.symmetric(horizontal: gutter, vertical: 14),
        itemCount: 7,
        separatorBuilder: (_, _) => const SizedBox(width: 16),
        itemBuilder: (_, _) => SkeletonBox(width: posterWidth, height: posterWidth * 1.5),
      ),
    );
  }
}

class _DemoRow extends StatefulWidget {
  const _DemoRow({required this.onTap});
  final VoidCallback onTap;

  @override
  State<_DemoRow> createState() => _DemoRowState();
}

class _DemoRowState extends State<_DemoRow> {
  bool _pressed = false;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTapDown: (_) => setState(() => _pressed = true),
        onTapUp: (_) => setState(() => _pressed = false),
        onTapCancel: () => setState(() => _pressed = false),
        onTap: widget.onTap,
        child: AnimatedScale(
          scale: _pressed ? 0.975 : 1.0,
          duration: const Duration(milliseconds: 100),
          child: GlassBox(
            borderRadius: 32,
            padding: const EdgeInsets.fromLTRB(14, 10, 22, 10),
            child: const Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                VeyloIcon('PlayCircle', size: 36, color: VeyloColors.bone),
                SizedBox(width: 14),
                Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Text('Test player', style: VeyloText.bodyStrong),
                    Text('Sintel, CC BY 3.0 Blender Foundation', style: VeyloText.credit),
                  ],
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
