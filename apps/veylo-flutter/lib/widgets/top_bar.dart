import 'dart:math' as math;
import 'dart:ui';
import 'package:flutter/widgets.dart';
import '../bridge/veylo_bridge.dart';
import '../state/app_state.dart';
import '../theme/veylo_theme.dart';
import 'glass.dart';
import 'veylo_icons.dart';

const _barHeight = 84.0;
const _smokeHeight = 32.0;
const _dockPadding = 5.0;
const _dockGap = 4.0;
const _controlGap = 12.0;
const _wordmarkGap = 20.0;
const _iconSize = 22.0;
const _labelGap = 8.0;
const _labelLeft = 13.0;
const _labelRight = 16.0;
const _lastLeft = 4.0;
const _lastCompactRight = 4.0;
const _lastTextRight = 18.0;
const _lastTextGap = 12.0;
const _lastMaxWidth = 280.0;
const _upgradeMargin = 12.0;

TextStyle get _dockLabelStyle => VeyloText.ui(15, weight: 650, height: 1);
TextStyle get _lastTitleStyle => VeyloText.ui(15, weight: 650, height: 1.15);
TextStyle get _lastLineStyle => VeyloText.ui(
  12.5,
  weight: 500,
  color: const Color.fromRGBO(255, 255, 255, 0.58),
  tabular: true,
  height: 1.15,
);

String _lastPlayedLine(MediaItem media) {
  if (media.inProgress) {
    final line = media.resumeContext;
    return line.isNotEmpty && line != 'In progress' ? line : 'Carry on';
  }
  if (media.kind == 'movie') return 'Watch again';
  return media.videoWatched ? 'Next episode' : 'Carry on';
}

enum _DockMode {
  icons,
  active,
  hover,
  hoverAndLastPlayed;

  bool get showsActive => this != icons;
  bool get expandsOnHover => index >= hover.index;
}

class VeyloTopBar extends StatefulWidget {
  const VeyloTopBar({super.key});

  @override
  State<VeyloTopBar> createState() => _VeyloTopBarState();
}

class _VeyloTopBarState extends State<VeyloTopBar> {
  _DockMode? _mode;
  bool? _showWordmark;

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.of(context);
    final insets = MediaQuery.viewPaddingOf(context);

    return SizedBox(
      height: insets.top + _barHeight + _smokeHeight,
      child: Stack(
        fit: StackFit.expand,
        children: [
          // Top resting gradient (.topbar)
          Positioned(
            top: 0,
            left: 0,
            right: 0,
            height: insets.top + _barHeight,
            child: IgnorePointer(
              child: DecoratedBox(
                decoration: BoxDecoration(
                  gradient: LinearGradient(
                    begin: Alignment.topCenter,
                    end: Alignment.bottomCenter,
                    stops: [insets.top / (insets.top + _barHeight), 1],
                    colors: const [Color.fromRGBO(0, 0, 0, 0.55), Color(0x00000000)],
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
                      height: insets.top + 70,
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
                      top: insets.top + 70,
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
                      top: insets.top + 92,
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
                    DecoratedBox(
                      decoration: BoxDecoration(
                        gradient: LinearGradient(
                          begin: Alignment.topCenter,
                          end: Alignment.bottomCenter,
                          stops: [
                            0.0,
                            (insets.top + 0.58 * (_barHeight + _smokeHeight)) /
                                (insets.top + _barHeight + _smokeHeight),
                            1.0,
                          ],
                          colors: const [
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

          Positioned(
            top: insets.top,
            left: insets.left,
            right: insets.right,
            height: _barHeight,
            child: BackdropGroup(
              child: ValueListenableBuilder<SeenItem?>(
                valueListenable: app.lastPlayedNotifier,
                builder: (context, last, _) => LayoutBuilder(
                  builder: (context, constraints) => _buildControls(
                    context,
                    app,
                    last,
                    constraints.maxWidth,
                  ),
                ),
              ),
            ),
          ),
        ],
      ),
    );
  }

  Widget _buildControls(
    BuildContext context,
    AppState app,
    SeenItem? last,
    double width,
  ) {
    final tabs = VeyloTab.values;
    final dockChrome = 2 * _dockPadding + (tabs.length - 1) * _dockGap;
    final trailingChrome = _controlGap * 2 + _lastLeft + _lastCompactRight;
    // Keep the existing phone control sizes where possible. Use up the gutters
    // before shrinking below 32 px on a viewport reduced by display cutouts.
    final preferredMinimum = dockChrome + trailingChrome + (tabs.length + 3) * 32;
    final gutter = VeyloLayout.gutter(width).clamp(
      0.0,
      math.max(0.0, (width - preferredMinimum) / 2),
    ).toDouble();
    final available = width - 2 * gutter;
    final controlSize = ((available - dockChrome - trailingChrome) /
            (tabs.length + 3))
        .clamp(0.0, 48.0);
    final style = DefaultTextStyle.of(context).style;
    final scaler = MediaQuery.textScalerOf(context);
    final boldText = MediaQuery.boldTextOf(context);
    final locale = Localizations.maybeLocaleOf(context);
    final direction = Directionality.of(context);
    double measure(String text, TextStyle textStyle) {
      final effectiveStyle = style.merge(textStyle);
      final painter = TextPainter(
        text: TextSpan(
          text: text,
          style: boldText ? effectiveStyle.copyWith(fontWeight: FontWeight.bold) : effectiveStyle,
        ),
        maxLines: 1,
        textScaler: scaler,
        locale: locale,
        textDirection: direction,
      )..layout();
      final width = painter.width.ceilToDouble();
      painter.dispose();
      return width;
    }

    final labelWidths = [
      for (final tab in tabs)
        math.max(
          0.0,
          _labelLeft + _iconSize + _labelGap + measure(tab.title, _dockLabelStyle) +
              _labelRight - controlSize,
        ),
    ];
    final activeExtra = labelWidths[app.tab.index];
    final hoverExtra = [
      for (final tab in tabs)
        if (tab != app.tab) labelWidths[tab.index],
    ].reduce(math.max);
    final iconDock = dockChrome + tabs.length * controlSize;
    final lastIconWidth = controlSize + _lastLeft + _lastCompactRight;
    final lastTextWidth = last == null
        ? 0.0
        : math.min(
            _lastMaxWidth,
            controlSize + _lastLeft + _lastTextRight + _lastTextGap +
                math.max(
                  measure(last.media.name, _lastTitleStyle),
                  measure(_lastPlayedLine(last.media), _lastLineStyle),
                ),
          );
    final trailingWidth = 2 * controlSize + _controlGap +
        (last == null ? 0 : _controlGap + lastIconWidth);
    final needs = [
      iconDock + trailingWidth,
      iconDock + trailingWidth + activeExtra,
      iconDock + trailingWidth + activeExtra + hoverExtra,
      iconDock + trailingWidth + activeExtra + hoverExtra +
          (last == null ? 0 : lastTextWidth - lastIconWidth),
    ];
    final previous = _mode;
    final mode = _DockMode.values.reversed.firstWhere(
      (mode) =>
          available >= needs[mode.index] +
              (previous != null && mode.index > previous.index
                  ? _upgradeMargin
                  : 0),
      orElse: () => _DockMode.icons,
    );
    _mode = mode;

    final wordmarkWidth = measure('Veylo', VeyloText.wordmark) + _wordmarkGap;
    final showWordmark = mode == _DockMode.hoverAndLastPlayed &&
        available >= needs[mode.index] + wordmarkWidth +
            (_showWordmark == false ? _upgradeMargin : 0);
    _showWordmark = showWordmark;
    final dockWidth = iconDock + (mode.showsActive ? activeExtra : 0) +
        (mode.expandsOnHover ? hoverExtra : 0);

    return Padding(
      padding: EdgeInsets.symmetric(horizontal: gutter),
      child: Row(
        textDirection: TextDirection.ltr,
        children: [
          Expanded(
            child: Align(
              alignment: Alignment.centerLeft,
              child: AnimatedSwitcher(
                duration: const Duration(milliseconds: 190),
                switchInCurve: VeyloCurves.out,
                switchOutCurve: Curves.easeIn,
                layoutBuilder: (current, previous) => Stack(
                  alignment: Alignment.centerLeft,
                  children: [...previous, ?current],
                ),
                transitionBuilder: (child, anim) => FadeTransition(
                  opacity: anim,
                  child: ScaleTransition(
                    scale: Tween<double>(begin: 0.94, end: 1.0).animate(anim),
                    child: child,
                  ),
                ),
                child: app.openedTitle != null
                    ? VeyloOrb(
                        key: const ValueKey('topbar-back'),
                        icon: 'ArrowLeft',
                        tooltip: 'Back (Esc)',
                        onTap: app.closeTitle,
                      )
                    : Row(
                        key: const ValueKey('topbar-dock'),
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          if (showWordmark) ...[
                            const Text('Veylo', style: VeyloText.wordmark),
                            const SizedBox(width: _wordmarkGap),
                          ],
                          Flexible(
                            child: ValueListenableBuilder<int>(
                              valueListenable: app.addonUpdatesNotifier,
                              builder: (_, updates, _) => _SlidingDock(
                                activeTab: app.tab,
                                mode: mode,
                                dockWidth: dockWidth,
                                controlSize: controlSize,
                                labelWidths: labelWidths,
                                addonUpdates: updates,
                                onSelect: app.setTab,
                              ),
                            ),
                          ),
                        ],
                      ),
              ),
            ),
          ),
          if (last != null) ...[
            KeyedSubtree(
              key: const ValueKey('topbar-last-played'),
              child: _LastPlayedPill(
                key: ValueKey(last.media.key),
                seen: last,
                showText: mode == _DockMode.hoverAndLastPlayed,
                width: mode == _DockMode.hoverAndLastPlayed ? lastTextWidth : lastIconWidth,
                controlSize: controlSize,
              ),
            ),
            const SizedBox(width: _controlGap),
          ],
          VeyloOrb(
            key: const ValueKey('topbar-shortcuts'),
            size: controlSize,
            icon: 'Keyboard',
            tooltip: 'Page shortcuts (?)',
            onTap: () => app.setShortcutsOpen(true),
          ),
          const SizedBox(width: _controlGap),
          VeyloOrb(
            key: const ValueKey('topbar-search'),
            size: controlSize,
            icon: 'Search',
            tooltip: 'Search (Ctrl+K)',
            onTap: () => app.openSearch(),
          ),
        ],
      ),
    );
  }
}

/// The hover widths and sliding glass use the same tab positions on every frame.
class _SlidingDock extends StatefulWidget {
  const _SlidingDock({
    required this.activeTab,
    required this.mode,
    required this.dockWidth,
    required this.controlSize,
    required this.labelWidths,
    required this.addonUpdates,
    required this.onSelect,
  });

  final VeyloTab activeTab;
  final _DockMode mode;
  final double dockWidth;
  final double controlSize;
  final List<double> labelWidths;
  final int addonUpdates;
  final ValueChanged<VeyloTab> onSelect;

  @override
  State<_SlidingDock> createState() => _SlidingDockState();
}

class _SlidingDockState extends State<_SlidingDock>
    with SingleTickerProviderStateMixin {
  late final AnimationController _layout = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 220),
    value: 1,
  );
  final _caption = OverlayPortalController()..show();
  final _captionLinks = [for (final _ in VeyloTab.values) LayerLink()];
  VeyloTab? _hovered;
  VeyloTab? _focused;
  VeyloTab? _expanded;
  VeyloTab? get _highlighted => _hovered ?? _focused;
  List<double> _previous = List.filled(VeyloTab.values.length, 0.0);

  @override
  void didUpdateWidget(covariant _SlidingDock oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.mode != widget.mode || oldWidget.activeTab != widget.activeTab) {
      _expanded = _highlighted;
      _layout.value = 1;
    }
  }

  @override
  void dispose() {
    _layout.dispose();
    super.dispose();
  }

  double _hoverFraction(VeyloTab tab) => lerpDouble(
    _previous[tab.index],
    _expanded == tab ? 1 : 0,
    Curves.easeOutCubic.transform(_layout.value),
  )!;

  void _followLayout() {
    if (_expanded == _highlighted) return;
    _previous = [for (final tab in VeyloTab.values) _hoverFraction(tab)];
    _expanded = _highlighted;
    _layout.forward(from: 0);
  }

  void _hover(VeyloTab? tab) {
    if (tab == _hovered) return;
    setState(() {
      _hovered = tab;
      _followLayout();
    });
  }

  void _focus(VeyloTab tab, bool focused) {
    setState(() {
      if (focused) {
        _hovered = null;
        _focused = tab;
      } else if (_focused == tab) {
        _focused = null;
      }
      _followLayout();
    });
  }

  @override
  Widget build(BuildContext context) {
    const tabs = VeyloTab.values;
    final highlighted = _highlighted;
    final showCaption = highlighted != null &&
        !widget.mode.expandsOnHover &&
        !(widget.mode.showsActive && highlighted == widget.activeTab);

    return OverlayPortal(
      controller: _caption,
      overlayChildBuilder: (context) => showCaption
          ? UnconstrainedBox(
              alignment: Alignment.topLeft,
              child: CompositedTransformFollower(
                link: _captionLinks[highlighted.index],
                showWhenUnlinked: false,
                targetAnchor: Alignment.bottomCenter,
                followerAnchor: Alignment.topCenter,
                offset: const Offset(0, 10),
                child: IgnorePointer(
                  child: ExcludeSemantics(
                    child: GlassBox(
                      strong: true,
                      borderRadius: 12,
                      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 7),
                      child: Text(highlighted.title, style: _dockLabelStyle),
                    ),
                  ),
                ),
              ),
            )
          : const SizedBox.shrink(),
      child: AnimatedBuilder(
        animation: _layout,
        builder: (context, _) {
          final fractions = [
            for (final tab in tabs)
              if (widget.activeTab == tab)
                widget.mode.showsActive ? 1.0 : 0.0
              else
                widget.mode.expandsOnHover ? _hoverFraction(tab) : 0.0,
          ];
          final widths = [
            for (final tab in tabs)
              widget.controlSize + widget.labelWidths[tab.index] * fractions[tab.index],
          ];
          var left = 0.0;
          final offsets = <double>[];
          for (final width in widths) {
            offsets.add(left);
            left += width + _dockGap;
          }
          final active = widget.activeTab.index;

          return MouseRegion(
            onExit: (_) => _hover(null),
            onHover: (event) {
              final x = event.localPosition.dx - _dockPadding;
              final tab = tabs.where((tab) =>
                  x >= offsets[tab.index] &&
                  x < offsets[tab.index] + widths[tab.index]).firstOrNull;
              _hover(tab);
            },
            child: SizedBox(
              width: widget.dockWidth,
              child: GlassBox(
                borderRadius: 30,
                padding: const EdgeInsets.all(_dockPadding),
                child: SizedBox(
                  height: widget.controlSize,
                  child: Stack(
                    children: [
                      AnimatedPositioned(
                        duration: _layout.isAnimating ? Duration.zero : const Duration(milliseconds: 260),
                        curve: VeyloCurves.slider,
                        left: offsets[active],
                        top: 0,
                        width: widths[active],
                        height: widget.controlSize,
                        child: DecoratedBox(
                          decoration: BoxDecoration(
                            color: const Color.fromRGBO(255, 255, 255, 0.14),
                            borderRadius: BorderRadius.circular(24),
                            border: const Border(top: BorderSide(color: Color.fromRGBO(255, 255, 255, 0.18))),
                          ),
                        ),
                      ),
                      for (final tab in tabs)
                        Positioned(
                          left: offsets[tab.index],
                          width: widths[tab.index],
                          top: 0,
                          bottom: 0,
                          child: _DockTab(
                            key: ValueKey('dock-tab-${tab.name}'),
                            tab: tab,
                            active: tab == widget.activeTab,
                            highlighted: tab == highlighted,
                            labelFraction: fractions[tab.index],
                            controlSize: widget.controlSize,
                            captionLink: _captionLinks[tab.index],
                            hasDot: tab == VeyloTab.settings && widget.addonUpdates > 0,
                            onFocus: (focused) => _focus(tab, focused),
                            onTap: () => widget.onSelect(tab),
                          ),
                        ),
                    ],
                  ),
                ),
              ),
            ),
          );
        },
      ),
    );
  }
}

class _DockTab extends StatefulWidget {
  const _DockTab({
    super.key,
    required this.tab,
    required this.active,
    required this.highlighted,
    required this.labelFraction,
    required this.controlSize,
    required this.captionLink,
    required this.hasDot,
    required this.onFocus,
    required this.onTap,
  });

  final VeyloTab tab;
  final bool active;
  final bool highlighted;
  final double labelFraction;
  final double controlSize;
  final LayerLink captionLink;
  final bool hasDot;
  final ValueChanged<bool> onFocus;
  final VoidCallback onTap;

  @override
  State<_DockTab> createState() => _DockTabState();
}

class _DockTabState extends State<_DockTab> {
  bool _pressed = false;

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final fg = widget.active || widget.highlighted ? VeyloColors.bone : VeyloColors.dust;
    final iconSize = math.min(_iconSize, widget.controlSize);
    final left = lerpDouble(
      (widget.controlSize - iconSize) / 2,
      _labelLeft,
      widget.labelFraction,
    )!;

    return CompositedTransformTarget(
      link: widget.captionLink,
      child: Semantics(
        label: widget.tab.title,
        button: true,
        selected: widget.active,
        child: FocusableActionDetector(
          mouseCursor: SystemMouseCursors.click,
          onShowFocusHighlight: widget.onFocus,
          actions: {
            ActivateIntent: CallbackAction<ActivateIntent>(onInvoke: (_) => widget.onTap()),
          },
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
              child: AnimatedContainer(
                duration: const Duration(milliseconds: 180),
                decoration: BoxDecoration(
                  color: widget.highlighted && !widget.active
                      ? const Color.fromRGBO(255, 255, 255, 0.07)
                      : const Color(0x00FFFFFF),
                  borderRadius: BorderRadius.circular(24),
                ),
                child: Stack(
                  alignment: Alignment.centerLeft,
                  children: [
                    Positioned(
                      left: left,
                      child: VeyloIcon(widget.tab.iconName, size: iconSize, color: fg),
                    ),
                    if (widget.labelFraction > 0)
                      Positioned(
                        left: left + iconSize + _labelGap,
                        child: ExcludeSemantics(
                          child: Opacity(
                            opacity: widget.labelFraction.clamp(0.0, 1.0),
                            child: Text(
                              widget.tab.title,
                              maxLines: 1,
                              style: _dockLabelStyle.copyWith(color: fg),
                            ),
                          ),
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
        ),
      ),
    );
  }
}

class _LastPlayedPill extends StatefulWidget {
  const _LastPlayedPill({
    super.key,
    required this.seen,
    required this.showText,
    required this.width,
    required this.controlSize,
  });
  final SeenItem seen;
  final bool showText;
  final double width;
  final double controlSize;

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
    final line = _lastPlayedLine(m);

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
            child: SizedBox(
              width: widget.width,
              child: GlassBox(
                borderRadius: 28,
                child: Padding(
                  padding: EdgeInsets.fromLTRB(
                    _lastLeft,
                    4,
                    widget.showText ? _lastTextRight : _lastCompactRight,
                    4,
                  ),
                  child: Row(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      SizedBox(
                        width: widget.controlSize,
                        height: widget.controlSize,
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
                      if (widget.showText) ...[
                        const SizedBox(width: _lastTextGap),
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
                                style: _lastTitleStyle,
                              ),
                              const SizedBox(height: 2),
                              Text(
                                line,
                                maxLines: 1,
                                overflow: TextOverflow.ellipsis,
                                style: _lastLineStyle,
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
