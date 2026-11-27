import 'dart:math' as math;
import 'dart:ui';
import 'package:flutter/widgets.dart';
import '../bridge/veylo_bridge.dart';
import '../state/app_state.dart';
import '../theme/veylo_theme.dart';
import 'glass.dart';
import 'veylo_icons.dart';

/// Exact 84px Veylo top bar (.topbar) with 32px feathered scroll smoke below (.topbar::before),
/// physically sliding glass pill inside .dock, and animated LastPlayed ring pill.
class VeyloTopBar extends StatelessWidget {
  const VeyloTopBar({super.key});

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.of(context);
    final width = MediaQuery.sizeOf(context).width;
    final gutter = VeyloLayout.gutter(width);
    final compact = width <= 760;

    return SizedBox(
      height: 116, // 84px bar + 32px feathered scroll smoke extension (base.css:133-147)
      child: Stack(
        fit: StackFit.expand,
        children: [
          // Top resting gradient (.topbar)
          const Positioned(
            top: 0,
            left: 0,
            right: 0,
            height: 84,
            child: IgnorePointer(
              child: DecoratedBox(
                decoration: BoxDecoration(
                  gradient: LinearGradient(
                    begin: Alignment.topCenter,
                    end: Alignment.bottomCenter,
                    colors: [Color.fromRGBO(0, 0, 0, 0.55), Color(0x00000000)],
                  ),
                ),
              ),
            ),
          ),

          // Feathered frosted scroll smoke (.scrolled .topbar::before):
          // blur(14px) saturate(140%) + smoke gradient, feathered smoothly from 58% -> 100%.
          // Uses a clipped top blur strip (0..82px) + feathered bottom gradient so BackdropFilter
          // is NEVER isolated inside a ShaderMask saveLayer!
          ValueListenableBuilder<bool>(
            valueListenable: app.pageScrolledNotifier,
            builder: (_, scrolled, _) => IgnorePointer(
              child: AnimatedOpacity(
                opacity: scrolled ? 1.0 : 0.0,
                duration: const Duration(milliseconds: 180),
                curve: Curves.ease,
                child: Stack(
                  fit: StackFit.expand,
                  children: [
                    // Multi-band progressive blur fade across 0..116px without ShaderMask isolating BackdropFilter!
                    Positioned(
                      top: 0,
                      left: 0,
                      right: 0,
                      height: 70,
                      child: ClipRect(
                        child: BackdropFilter(
                          filter: ImageFilter.compose(
                            outer: const ColorFilter.matrix(veyloSaturate140Matrix),
                            inner: ImageFilter.blur(sigmaX: 14, sigmaY: 14, tileMode: TileMode.clamp),
                          ),
                          child: const SizedBox.expand(),
                        ),
                      ),
                    ),
                    Positioned(
                      top: 70,
                      left: 0,
                      right: 0,
                      height: 22,
                      child: ClipRect(
                        child: BackdropFilter(
                          filter: ImageFilter.blur(sigmaX: 8, sigmaY: 8, tileMode: TileMode.clamp),
                          child: const SizedBox.expand(),
                        ),
                      ),
                    ),
                    Positioned(
                      top: 92,
                      left: 0,
                      right: 0,
                      height: 24,
                      child: ClipRect(
                        child: BackdropFilter(
                          filter: ImageFilter.blur(sigmaX: 3, sigmaY: 3, tileMode: TileMode.clamp),
                          child: const SizedBox.expand(),
                        ),
                      ),
                    ),
                    const DecoratedBox(
                      decoration: BoxDecoration(
                        gradient: LinearGradient(
                          begin: Alignment.topCenter,
                          end: Alignment.bottomCenter,
                          stops: [0.0, 0.58, 1.0],
                          colors: [
                            Color.fromRGBO(0, 0, 0, 0.78),
                            Color.fromRGBO(0, 0, 0, 0.52),
                            Color(0x00000000),
                          ],
                        ),
                      ),
                    ),
                  ],
                ),
              ),
            ),
          ),

          // Foreground 84px controls row in its own BackdropGroup
          Positioned(
            top: 0,
            left: 0,
            right: 0,
            height: 84,
            child: BackdropGroup(
              child: Padding(
                padding: EdgeInsets.symmetric(horizontal: gutter),
                child: Row(
                  children: [
                    AnimatedSwitcher(
                      duration: const Duration(milliseconds: 190),
                      switchInCurve: VeyloCurves.out,
                      switchOutCurve: Curves.easeIn,
                      transitionBuilder: (child, anim) => FadeTransition(
                        opacity: anim,
                        child: ScaleTransition(
                          scale: Tween<double>(begin: 0.94, end: 1.0).animate(anim),
                          child: child,
                        ),
                      ),
                      child: app.openedTitle != null
                          ? KeyedSubtree(
                              key: const ValueKey('topbar-back'),
                              child: VeyloOrb(
                                icon: 'ArrowLeft',
                                tooltip: 'Back (Esc)',
                                onTap: app.closeTitle,
                              ),
                            )
                          : Row(
                              key: const ValueKey('topbar-dock'),
                              mainAxisSize: MainAxisSize.min,
                              children: [
                                if (!compact) ...[
                                  const Text('Veylo', style: VeyloText.wordmark),
                                  const SizedBox(width: 20),
                                ],
                                ValueListenableBuilder<int>(
                                  valueListenable: app.addonUpdatesNotifier,
                                  builder: (_, updates, _) => _SlidingDock(
                                    activeTab: app.tab,
                                    compact: compact,
                                    addonUpdates: updates,
                                    onSelect: app.setTab,
                                  ),
                                ),
                              ],
                            ),
                    ),
                    const Spacer(),
                    ValueListenableBuilder<SeenItem?>(
                      valueListenable: app.lastPlayedNotifier,
                      builder: (_, last, _) {
                        if (last == null) return const SizedBox.shrink();
                        return Padding(
                          padding: const EdgeInsets.only(right: 12),
                          child: _LastPlayedPill(key: ValueKey(last.media.key), seen: last, compact: compact),
                        );
                      },
                    ),
                    VeyloOrb(
                      icon: 'Keyboard',
                      tooltip: 'Page shortcuts (?)',
                      onTap: () => app.setShortcutsOpen(true),
                    ),
                    const SizedBox(width: 12),
                    VeyloOrb(
                      icon: 'Search',
                      tooltip: 'Search (Ctrl+K)',
                      onTap: () => app.openSearch(),
                    ),
                  ],
                ),
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// Glass navigation dock with a physically gliding `.slider` pill matching motion.css:9-17.
class _SlidingDock extends StatefulWidget {
  const _SlidingDock({
    required this.activeTab,
    required this.compact,
    required this.addonUpdates,
    required this.onSelect,
  });

  final VeyloTab activeTab;
  final bool compact;
  final int addonUpdates;
  final ValueChanged<VeyloTab> onSelect;

  @override
  State<_SlidingDock> createState() => _SlidingDockState();
}

class _SlidingDockState extends State<_SlidingDock> {
  final GlobalKey _rowKey = GlobalKey();
  final Map<VeyloTab, GlobalKey> _tabKeys = {for (final t in VeyloTab.values) t: GlobalKey()};
  double _sliderLeft = 0;
  double _sliderWidth = 48;
  bool _measured = false;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addPostFrameCallback((_) => _measure());
  }

  @override
  void didUpdateWidget(covariant _SlidingDock oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.activeTab != widget.activeTab || oldWidget.compact != widget.compact) {
      WidgetsBinding.instance.addPostFrameCallback((_) => _measure());
    }
  }

  void _measure() {
    if (!mounted) return;
    final rowBox = _rowKey.currentContext?.findRenderObject() as RenderBox?;
    final tabBox = _tabKeys[widget.activeTab]?.currentContext?.findRenderObject() as RenderBox?;
    if (rowBox == null || tabBox == null || !rowBox.hasSize || !tabBox.hasSize) return;
    final pos = tabBox.localToGlobal(Offset.zero, ancestor: rowBox);
    if ((pos.dx - _sliderLeft).abs() > 0.5 || (tabBox.size.width - _sliderWidth).abs() > 0.5 || !_measured) {
      setState(() {
        _sliderLeft = pos.dx;
        _sliderWidth = tabBox.size.width;
        _measured = true;
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    return GlassBox(
      borderRadius: 30,
      padding: const EdgeInsets.all(5),
      child: Stack(
        children: [
          if (_measured)
            AnimatedPositioned(
              duration: const Duration(milliseconds: 260),
              curve: VeyloCurves.slider,
              left: _sliderLeft,
              top: 0,
              width: _sliderWidth,
              height: 48,
              child: Container(
                decoration: BoxDecoration(
                  color: const Color.fromRGBO(255, 255, 255, 0.14),
                  borderRadius: BorderRadius.circular(24),
                  border: const Border(top: BorderSide(color: Color.fromRGBO(255, 255, 255, 0.18), width: 1)),
                ),
              ),
            ),
          Row(
            key: _rowKey,
            mainAxisSize: MainAxisSize.min,
            children: [
              for (int i = 0; i < VeyloTab.values.length; i++) ...[
                if (i > 0) const SizedBox(width: 4),
                _DockTab(
                  key: _tabKeys[VeyloTab.values[i]],
                  tab: VeyloTab.values[i],
                  active: widget.activeTab == VeyloTab.values[i],
                  compact: widget.compact,
                  hasDot: VeyloTab.values[i] == VeyloTab.settings && widget.addonUpdates > 0,
                  onTap: () => widget.onSelect(VeyloTab.values[i]),
                ),
              ],
            ],
          ),
        ],
      ),
    );
  }
}

class _DockTab extends StatefulWidget {
  const _DockTab({
    super.key,
    required this.tab,
    required this.active,
    required this.compact,
    required this.hasDot,
    required this.onTap,
  });

  final VeyloTab tab;
  final bool active;
  final bool compact;
  final bool hasDot;
  final VoidCallback onTap;

  @override
  State<_DockTab> createState() => _DockTabState();
}

class _DockTabState extends State<_DockTab> {
  bool _hovered = false;
  bool _pressed = false;

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final fg = (widget.active || _hovered) ? VeyloColors.bone : VeyloColors.dust;

    return VeyloTooltip(
      message: widget.tab.title,
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
            scale: _pressed ? 0.92 : 1.0,
            duration: Duration(milliseconds: _pressed ? 60 : 180),
            curve: VeyloCurves.out,
            child: Stack(
              clipBehavior: Clip.none,
              children: [
                Container(
                  height: 48,
                  constraints: const BoxConstraints(minWidth: 48),
                  padding: EdgeInsets.only(
                    left: widget.active && !widget.compact ? 13 : 12,
                    right: widget.active && !widget.compact ? 16 : 12,
                  ),
                  child: TweenAnimationBuilder<Color?>(
                    tween: ColorTween(end: fg),
                    duration: const Duration(milliseconds: 180),
                    curve: VeyloCurves.out,
                    builder: (_, animFg, _) {
                      final c = animFg ?? fg;
                      return Row(
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          VeyloIcon(widget.tab.iconName, size: 22, color: c),
                          AnimatedSize(
                            duration: const Duration(milliseconds: 220),
                            curve: VeyloCurves.slider,
                            alignment: Alignment.centerLeft,
                            child: (widget.active && !widget.compact)
                                ? AnimatedOpacity(
                                    opacity: 1.0,
                                    duration: const Duration(milliseconds: 200),
                                    curve: VeyloCurves.out,
                                    child: Padding(
                                      padding: const EdgeInsets.only(left: 8),
                                      child: Text(
                                        widget.tab.title,
                                        style: VeyloText.ui(15, weight: 650, color: c, height: 1.0),
                                      ),
                                    ),
                                  )
                                : const SizedBox.shrink(),
                          ),
                        ],
                      );
                    },
                  ),
                ),
                if (widget.hasDot)
                  Positioned(
                    top: 8,
                    right: 8,
                    child: ValueListenableBuilder<Color>(
                      valueListenable: app.lampColorNotifier,
                      builder: (_, lamp, _) => Container(
                        width: 9,
                        height: 9,
                        decoration: BoxDecoration(
                          shape: BoxShape.circle,
                          color: lamp,
                          border: Border.all(color: const Color.fromRGBO(0, 0, 0, 0.5), width: 1.5),
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

class _LastPlayedPill extends StatefulWidget {
  const _LastPlayedPill({super.key, required this.seen, required this.compact});
  final SeenItem seen;
  final bool compact;

  @override
  State<_LastPlayedPill> createState() => _LastPlayedPillState();
}

class _LastPlayedPillState extends State<_LastPlayedPill> {
  bool _pressed = false;

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final m = widget.seen.media;
    final resumes = m.inProgress;
    final frac = resumes ? m.progressFraction.clamp(0.0, 1.0) : 0.0;
    final line = resumes
        ? (m.resumeContext.isNotEmpty && m.resumeContext != 'In progress' ? m.resumeContext : 'Carry on')
        : (m.kind == 'movie' ? 'Watch again' : (m.videoWatched ? 'Next episode' : 'Carry on'));

    return VeyloTooltip(
      message: '$line: ${m.name}',
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        child: GestureDetector(
          onTapDown: (_) => setState(() => _pressed = true),
          onTapUp: (_) => setState(() => _pressed = false),
          onTapCancel: () => setState(() => _pressed = false),
          onTap: () => app.autoplayMedia(m),
          child: AnimatedScale(
            scale: _pressed ? 0.96 : 1.0,
            duration: const Duration(milliseconds: 100),
            child: ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 280),
              child: GlassBox(
                borderRadius: 28,
                padding: EdgeInsets.fromLTRB(4, 4, widget.compact ? 4 : 18, 4),
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    SizedBox(
                      width: 48,
                      height: 48,
                      child: TweenAnimationBuilder<double>(
                        tween: Tween(begin: 0, end: frac),
                        duration: const Duration(milliseconds: 550),
                        curve: VeyloCurves.out,
                        builder: (_, animatedFrac, child) => CustomPaint(
                          painter: _RingPainter(progress: animatedFrac),
                          child: child,
                        ),
                        child: Container(
                          decoration: const BoxDecoration(
                            shape: BoxShape.circle,
                            color: Color.fromRGBO(255, 255, 255, 0.06),
                          ),
                          alignment: Alignment.center,
                          child: const Padding(
                            padding: EdgeInsets.only(left: 2),
                            child: VeyloIcon('Play', size: 17, color: VeyloColors.bone, filled: true),
                          ),
                        ),
                      ),
                    ),
                    if (!widget.compact) ...[
                      const SizedBox(width: 12),
                      Flexible(
                        child: Column(
                          mainAxisAlignment: MainAxisAlignment.center,
                          crossAxisAlignment: CrossAxisAlignment.start,
                          mainAxisSize: MainAxisSize.min,
                          children: [
                            Text(
                              m.name,
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                              style: VeyloText.ui(15, weight: 650, height: 1.15),
                            ),
                            const SizedBox(height: 2),
                            Text(
                              line,
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                              style: VeyloText.ui(
                                12.5,
                                weight: 500,
                                color: const Color.fromRGBO(255, 255, 255, 0.58),
                                tabular: true,
                                height: 1.15,
                              ),
                            ),
                          ],
                        ),
                      ),
                    ],
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

class _RingPainter extends CustomPainter {
  _RingPainter({required this.progress});
  final double progress;

  @override
  void paint(Canvas canvas, Size size) {
    final center = Offset(size.width / 2, size.height / 2);
    final radius = size.width / 2 - 1.5;
    final track = Paint()
      ..color = const Color.fromRGBO(255, 255, 255, 0.10)
      ..style = PaintingStyle.stroke
      ..strokeWidth = 2;
    canvas.drawCircle(center, radius, track);
    if (progress > 0) {
      final done = Paint()
        ..color = VeyloColors.bone
        ..style = PaintingStyle.stroke
        ..strokeWidth = 2
        ..strokeCap = StrokeCap.round;
      canvas.drawArc(
        Rect.fromCircle(center: center, radius: radius),
        -math.pi / 2,
        math.pi * 2 * progress,
        false,
        done,
      );
    }
  }

  @override
  bool shouldRepaint(_RingPainter oldDelegate) => oldDelegate.progress != progress;
}
