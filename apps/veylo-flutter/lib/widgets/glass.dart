import 'dart:async';
import 'dart:ui';
import 'package:cached_network_image/cached_network_image.dart';
import 'package:material_ui/material_ui.dart' show Tooltip;
import 'package:flutter/widgets.dart';
import 'package:flutter_animate/flutter_animate.dart';
import 'package:flutter_svg/flutter_svg.dart';
import '../bridge/veylo_bridge.dart';
import '../state/app_state.dart';
import '../theme/veylo_theme.dart';
import 'title_prefetch.dart';
import 'veylo_icons.dart';

/// CSS `saturate(140%)` color matrix matching `.glass` (`backdrop-filter: blur(...) saturate(140%)`).
const List<double> veyloSaturate140Matrix = <double>[
  1.31496, -0.28608, -0.02888, 0, 0,
  -0.08504, 1.11392, -0.02888, 0, 0,
  -0.08504, -0.28608, 1.37112, 0, 0,
  0, 0, 0, 1, 0,
];

/// Paints CSS `box-shadow: inset 0 1px 0 sheen, inset 0 0 0 1px edge` cleanly without muddying the fill.
class SpecularRimPainter extends CustomPainter {
  const SpecularRimPainter({
    required this.radius,
    required this.edgeColor,
    required this.sheenColor,
    this.borderWidth = 1.0,
  });

  final double radius;
  final Color edgeColor;
  final Color sheenColor;
  final double borderWidth;

  @override
  void paint(Canvas canvas, Size size) {
    if (size.isEmpty) return;
    final rect = Offset.zero & size;
    final rrect = RRect.fromRectAndRadius(rect.deflate(borderWidth / 2), Radius.circular(radius));

    final edgePaint = Paint()
      ..color = edgeColor
      ..style = PaintingStyle.stroke
      ..strokeWidth = borderWidth;
    canvas.drawRRect(rrect, edgePaint);

    if (sheenColor.a > 0) {
      canvas.save();
      canvas.clipRect(Rect.fromLTWH(0, 0, size.width, (radius + 2).clamp(2.0, size.height * 0.45)));
      final sheenPaint = Paint()
        ..shader = LinearGradient(
          begin: Alignment.topCenter,
          end: Alignment.bottomCenter,
          stops: const [0.0, 0.25, 1.0],
          colors: [sheenColor, sheenColor.withValues(alpha: sheenColor.a * 0.35), const Color(0x00FFFFFF)],
        ).createShader(Rect.fromLTWH(0, 0, size.width, (radius + 2).clamp(2.0, 14.0)))
        ..style = PaintingStyle.stroke
        ..strokeWidth = 1.0;
      canvas.drawRRect(
        RRect.fromRectAndRadius(rect.deflate(0.75), Radius.circular(radius)),
        sheenPaint,
      );
      canvas.restore();
    }
  }

  @override
  bool shouldRepaint(SpecularRimPainter old) =>
      old.radius != radius ||
      old.edgeColor != edgeColor ||
      old.sheenColor != sheenColor ||
      old.borderWidth != borderWidth;
}

/// Real GPU-accelerated smoked glass lit by the current title's --lamp color (.glass).
/// Uses Flutter's native BackdropFilter.grouped + TileMode.clamp so Impeller executes a single shared
/// backdrop blur pass with zero dark border bands.
class GlassBox extends StatelessWidget {
  const GlassBox({
    super.key,
    required this.child,
    this.borderRadius = 24,
    this.padding,
    this.strong = false,
    this.lamp,
    this.blur = true,
    this.blurSigma = 18,
    this.borderColor,
    this.sheenColor,
    this.borderWidth = 1,
    this.backgroundOverride,
    this.shadow = true,
  });

  final Widget child;
  final double borderRadius;
  final EdgeInsetsGeometry? padding;
  final bool strong;
  final Color? lamp;
  final bool blur;
  final double blurSigma;
  final Color? borderColor;
  final Color? sheenColor;
  final double borderWidth;
  final Color? backgroundOverride;
  final bool shadow;

  @override
  Widget build(BuildContext context) {
    if (lamp != null) return _buildWithLamp(lamp!);
    final app = AppStateScope.read(context);
    return ValueListenableBuilder<Color>(
      valueListenable: app.lampColorNotifier,
      builder: (_, l, _) => _buildWithLamp(l),
    );
  }

  Widget _buildWithLamp(Color l) {
    final baseBg = backgroundOverride ?? (strong ? VeyloColors.glassStrong : VeyloColors.glass);
    final bg = VeyloColors.mixLamp(l, baseBg, 0.08);
    final br = BorderRadius.circular(borderRadius);

    Widget inner = CustomPaint(
      foregroundPainter: SpecularRimPainter(
        radius: borderRadius,
        edgeColor: borderColor ?? VeyloColors.edge,
        sheenColor: sheenColor ?? VeyloColors.sheen,
        borderWidth: borderWidth,
      ),
      child: AnimatedContainer(
        duration: const Duration(milliseconds: 420),
        curve: VeyloCurves.out,
        padding: padding,
        color: bg,
        child: child,
      ),
    );

    if (blur) {
      inner = BackdropFilter.grouped(
        filter: ImageFilter.compose(
          outer: const ColorFilter.matrix(veyloSaturate140Matrix),
          inner: ImageFilter.blur(
            sigmaX: blurSigma,
            sigmaY: blurSigma,
            tileMode: TileMode.clamp,
          ),
        ),
        child: inner,
      );
    }

    return Container(
      decoration: shadow
          ? BoxDecoration(
              borderRadius: br,
              boxShadow: const [
                BoxShadow(color: Color.fromRGBO(0, 0, 0, 0.28), blurRadius: 32, offset: Offset(0, 12)),
              ],
            )
          : null,
      child: ClipRRect(borderRadius: br, child: inner),
    );
  }
}

/// Lightweight smoked surface for repeating/scrolling items (.surface) — zero BackdropFilter overhead.
class SurfaceBox extends StatelessWidget {
  const SurfaceBox({
    super.key,
    required this.child,
    this.borderRadius = 18,
    this.padding,
    this.hover = false,
    this.lamp,
  });

  final Widget child;
  final double borderRadius;
  final EdgeInsetsGeometry? padding;
  final bool hover;
  final Color? lamp;

  @override
  Widget build(BuildContext context) {
    if (lamp != null) return _buildWithLamp(lamp!);
    final app = AppStateScope.read(context);
    return ValueListenableBuilder<Color>(
      valueListenable: app.lampColorNotifier,
      builder: (_, l, _) => _buildWithLamp(l),
    );
  }

  Widget _buildWithLamp(Color l) {
    final bg = VeyloColors.mixLamp(l, hover ? VeyloColors.surfaceHover : VeyloColors.surface, hover ? 0.10 : 0.07);
    return CustomPaint(
      foregroundPainter: SpecularRimPainter(
        radius: borderRadius,
        edgeColor: VeyloColors.edge,
        sheenColor: const Color.fromRGBO(255, 255, 255, 0.16),
      ),
      child: AnimatedContainer(
        duration: const Duration(milliseconds: 160),
        padding: padding,
        decoration: BoxDecoration(
          color: bg,
          borderRadius: BorderRadius.circular(borderRadius),
        ),
        child: child,
      ),
    );
  }
}

/// Native Flutter Tooltip styled to Veylo's smoked-glass pill (auto viewport-clamped).
class VeyloTooltip extends StatelessWidget {
  const VeyloTooltip({super.key, required this.message, required this.child});
  final String message;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    if (message.isEmpty) return child;
    return Tooltip(
      message: message,
      waitDuration: const Duration(milliseconds: 380),
      preferBelow: true,
      verticalOffset: 10,
      child: child,
    );
  }
}

/// Circular glass / accent / bare orb button (.orb).
class VeyloOrb extends StatefulWidget {
  const VeyloOrb({
    super.key,
    required this.icon,
    required this.onTap,
    this.tooltip,
    this.size = 48,
    this.iconSize,
    this.glass = true,
    this.accent = false,
    this.active = false,
    this.disabled = false,
    this.lamp,
    this.backgroundOverride,
  });

  final String icon;
  final VoidCallback? onTap;
  final String? tooltip;
  final double size;
  final double? iconSize;
  final bool glass;
  final bool accent;
  final bool active;
  final bool disabled;
  final Color? lamp;
  final Color? backgroundOverride;

  @override
  State<VeyloOrb> createState() => _VeyloOrbState();
}

class _VeyloOrbState extends State<VeyloOrb> {
  bool _hovered = false;
  bool _pressed = false;

  @override
  Widget build(BuildContext context) {
    if (widget.lamp != null) return _buildWithLamp(widget.lamp!);
    final app = AppStateScope.read(context);
    return ValueListenableBuilder<Color>(
      valueListenable: app.lampColorNotifier,
      builder: (_, l, _) => _buildWithLamp(l),
    );
  }

  Widget _buildWithLamp(Color l) {
    final iSize = widget.iconSize ?? (widget.size >= 56 ? 24 : (widget.size <= 40 ? 18 : 22));
    final Color fg = (widget.accent || widget.active) ? const Color(0xFF000000) : VeyloColors.bone;

    Color bg;
    if (widget.accent) {
      bg = l;
    } else if (widget.active) {
      bg = VeyloColors.bone;
    } else if (widget.backgroundOverride != null) {
      bg = _hovered
          ? Color.alphaBlend(const Color.fromRGBO(255, 255, 255, 0.12), widget.backgroundOverride!)
          : widget.backgroundOverride!;
    } else if (widget.glass) {
      bg = VeyloColors.mixLamp(
        l,
        _hovered ? const Color.fromRGBO(40, 41, 46, 0.62) : VeyloColors.glass,
        _hovered ? 0.14 : 0.08,
      );
    } else {
      bg = _hovered ? const Color.fromRGBO(255, 255, 255, 0.12) : const Color(0x00000000);
    }

    Widget circle = AnimatedContainer(
      duration: const Duration(milliseconds: 120),
      width: widget.size,
      height: widget.size,
      decoration: BoxDecoration(
        shape: BoxShape.circle,
        color: bg,
        border: (widget.glass && !widget.accent && !widget.active)
            ? Border.all(color: _hovered ? VeyloColors.sheen : VeyloColors.edge, width: 1)
            : null,
        boxShadow: widget.accent
            ? [
                BoxShadow(
                  color: l.withValues(alpha: _hovered ? 0.44 : 0.30),
                  blurRadius: _hovered ? 32 : 26,
                  offset: const Offset(0, 10),
                ),
              ]
            : null,
      ),
      alignment: Alignment.center,
      child: VeyloIcon(widget.icon, size: iSize, color: fg, filled: widget.active || widget.accent),
    );

    if (widget.glass && !widget.accent && !widget.active && widget.backgroundOverride == null) {
      circle = ClipOval(
        child: BackdropFilter.grouped(
          filter: ImageFilter.blur(sigmaX: 14, sigmaY: 14, tileMode: TileMode.clamp),
          child: circle,
        ),
      );
    }

    Widget body = AnimatedScale(
      scale: widget.disabled ? 1.0 : (_pressed ? 0.94 : (_hovered ? 1.03 : 1.0)),
      duration: Duration(milliseconds: _pressed ? 70 : 250),
      curve: _pressed ? Curves.easeOut : VeyloCurves.spring,
      child: Opacity(
        opacity: widget.disabled ? 0.4 : 1.0,
        child: circle,
      ),
    );

    body = MouseRegion(
      cursor: widget.disabled ? SystemMouseCursors.basic : SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() {
        _hovered = false;
        _pressed = false;
      }),
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTapDown: widget.disabled ? null : (_) => setState(() => _pressed = true),
        onTapUp: widget.disabled ? null : (_) => setState(() => _pressed = false),
        onTapCancel: () => setState(() => _pressed = false),
        onTap: widget.disabled ? null : widget.onTap,
        child: body,
      ),
    );

    if (widget.tooltip != null && widget.tooltip!.isNotEmpty) {
      return VeyloTooltip(message: widget.tooltip!, child: body);
    }
    return body;
  }
}

/// Rounded glass or accent pill button (.pill).
class VeyloPill extends StatefulWidget {
  const VeyloPill({
    super.key,
    required this.label,
    required this.onTap,
    this.icon,
    this.trailing,
    this.accent = false,
    this.danger = false,
    this.confirmRed = false,
    this.active = false,
    this.big = false,
    this.small = false,
    this.disabled = false,
    this.lamp,
    this.tooltip,
  });

  final String label;
  final VoidCallback? onTap;
  final String? icon;
  final Widget? trailing;
  final bool accent;
  final bool danger;
  final bool confirmRed;
  final bool active;
  final bool big;
  final bool small;
  final bool disabled;
  final Color? lamp;
  final String? tooltip;

  @override
  State<VeyloPill> createState() => _VeyloPillState();
}

class _VeyloPillState extends State<VeyloPill> {
  bool _hovered = false;
  bool _pressed = false;

  @override
  Widget build(BuildContext context) {
    if (widget.lamp != null) return _buildWithLamp(widget.lamp!);
    final app = AppStateScope.read(context);
    return ValueListenableBuilder<Color>(
      valueListenable: app.lampColorNotifier,
      builder: (_, l, _) => _buildWithLamp(l),
    );
  }

  Widget _buildWithLamp(Color l) {
    final height = widget.big ? 56.0 : (widget.small ? 38.0 : 48.0);
    final radius = height / 2;
    final padLeft = widget.big ? 24.0 : (widget.small ? 14.0 : 20.0);
    final padRight = widget.big ? 30.0 : (widget.small ? 14.0 : 20.0);
    final fontSize = widget.big ? 18.0 : (widget.small ? 13.5 : 16.0);
    final iconSize = widget.big ? 22.0 : (widget.small ? 16.0 : 18.0);

    Color bg;
    Color fg;
    Color edgeColor = const Color(0x00000000);
    Color sheenColor = const Color(0x00000000);
    bool useGlassBlur = false;

    if (widget.confirmRed) {
      bg = VeyloColors.dangerRed;
      fg = const Color(0xFFFFFFFF);
    } else if (widget.accent) {
      bg = l;
      fg = const Color(0xFF000000);
      sheenColor = const Color.fromRGBO(255, 255, 255, 0.45);
    } else if (widget.danger) {
      bg = _hovered
          ? const Color.fromRGBO(210, 58, 58, 0.22)
          : VeyloColors.mixLamp(l, VeyloColors.glass, 0.08);
      fg = _hovered ? const Color(0xFFFFD7D2) : VeyloColors.dangerText;
      edgeColor = VeyloColors.edge;
      sheenColor = VeyloColors.sheen;
      useGlassBlur = true;
    } else {
      bg = VeyloColors.mixLamp(
        l,
        _hovered ? const Color.fromRGBO(40, 41, 46, 0.65) : VeyloColors.glass,
        _hovered ? 0.14 : 0.08,
      );
      fg = VeyloColors.bone;
      edgeColor = widget.active ? VeyloColors.bone : (_hovered ? VeyloColors.sheen : VeyloColors.edge);
      sheenColor = VeyloColors.sheen;
      useGlassBlur = true;
    }

    Widget pillInner = CustomPaint(
      foregroundPainter: SpecularRimPainter(
        radius: radius,
        edgeColor: edgeColor,
        sheenColor: sheenColor,
        borderWidth: widget.active ? 1.5 : 1.0,
      ),
      child: AnimatedContainer(
        duration: const Duration(milliseconds: 140),
        height: height,
        padding: EdgeInsets.only(left: padLeft, right: padRight),
        color: bg,
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            if (widget.icon != null) ...[
              VeyloIcon(widget.icon!, size: iconSize, color: fg, filled: widget.accent || widget.active),
              if (widget.label.isNotEmpty) const SizedBox(width: 8),
            ],
            if (widget.label.isNotEmpty)
              Text(
                widget.label,
                style: VeyloText.ui(fontSize, weight: 600, color: fg, height: 1.0),
              ),
            if (widget.trailing != null) ...[
              const SizedBox(width: 6),
              widget.trailing!,
            ],
          ],
        ),
      ),
    );

    if (useGlassBlur) {
      pillInner = BackdropFilter.grouped(
        filter: ImageFilter.blur(sigmaX: 14, sigmaY: 14, tileMode: TileMode.clamp),
        child: pillInner,
      );
    }

    final pill = AnimatedContainer(
      duration: const Duration(milliseconds: 180),
      curve: VeyloCurves.out,
      decoration: BoxDecoration(
        borderRadius: BorderRadius.circular(radius),
        boxShadow: widget.accent && !widget.disabled
            ? [
                BoxShadow(
                  color: l.withValues(alpha: _hovered ? 0.44 : 0.30),
                  blurRadius: _hovered ? 32 : 26,
                  offset: const Offset(0, 10),
                ),
              ]
            : const [BoxShadow(color: Color.fromRGBO(0, 0, 0, 0.24), blurRadius: 20, offset: Offset(0, 8))],
      ),
      child: ClipRRect(borderRadius: BorderRadius.circular(radius), child: pillInner),
    );

    Widget result = MouseRegion(
      cursor: widget.disabled ? SystemMouseCursors.basic : SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() {
        _hovered = false;
        _pressed = false;
      }),
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTapDown: widget.disabled ? null : (_) => setState(() => _pressed = true),
        onTapUp: widget.disabled ? null : (_) => setState(() => _pressed = false),
        onTapCancel: () => setState(() => _pressed = false),
        onTap: widget.disabled ? null : widget.onTap,
        child: AnimatedScale(
          scale: widget.disabled ? 1.0 : (_pressed ? 0.94 : (_hovered ? 1.03 : 1.0)),
          duration: Duration(milliseconds: _pressed ? 70 : 250),
          curve: _pressed ? Curves.easeOut : VeyloCurves.spring,
          child: Opacity(opacity: widget.disabled ? 0.45 : 1.0, child: pill),
        ),
      ),
    );

    if (widget.tooltip != null && widget.tooltip!.isNotEmpty) {
      return VeyloTooltip(message: widget.tooltip!, child: result);
    }
    return result;
  }
}

/// Declarative GPU lift-in entrance powered by `flutter_animate`.
class LiftIn extends StatelessWidget {
  const LiftIn({
    super.key,
    required this.child,
    this.delay = Duration.zero,
    this.duration = const Duration(milliseconds: 280),
    this.offsetY = 8.0,
  });

  final Widget child;
  final Duration delay;
  final Duration duration;
  final double offsetY;

  @override
  Widget build(BuildContext context) {
    return child
        .animate(delay: delay)
        .fadeIn(duration: duration, curve: VeyloCurves.out)
        .moveY(begin: offsetY, end: 0, duration: duration, curve: VeyloCurves.out);
  }
}

/// Ken Burns settle backdrop powered by `CachedNetworkImage` + `flutter_animate` (scale 1.045 -> 1.0 over 9s).
class KenBurnsImage extends StatelessWidget {
  const KenBurnsImage({
    super.key,
    required this.url,
    this.cacheWidth = 1600,
    this.alignment = const Alignment(0, -0.4),
    this.settle = true,
    this.fadeIn = const Duration(milliseconds: 300),
  });

  final String url;
  final int cacheWidth;
  final Alignment alignment;
  /// The slow 1.045 → 1.0 zoom; off where nothing may move (the title page).
  final bool settle;
  /// Decode fade; zero when the image was precached before being shown.
  final Duration fadeIn;

  @override
  Widget build(BuildContext context) {
    final image = CachedNetworkImage(
      imageUrl: url,
      fit: BoxFit.cover,
      alignment: alignment,
      width: double.infinity,
      height: double.infinity,
      memCacheWidth: cacheWidth,
      fadeInDuration: fadeIn,
      errorWidget: (_, _, _) => const SizedBox.shrink(),
    );
    if (!settle) return RepaintBoundary(child: image);
    return RepaintBoundary(
      child: image.animate().scale(
            begin: const Offset(1.045, 1.045),
            end: const Offset(1.0, 1.0),
            duration: const Duration(seconds: 9),
            curve: const Cubic(0.16, 1.0, 0.3, 1.0),
          ),
    );
  }
}

/// Keyboard shortcut keycap badge (.keycap).
class Keycap extends StatelessWidget {
  const Keycap(this.text, {super.key, this.dark = false, this.small = false});
  final String text;
  final bool dark;
  final bool small;

  @override
  Widget build(BuildContext context) {
    return Container(
      constraints: BoxConstraints(minWidth: small ? 22 : 30),
      height: small ? 22 : 28,
      padding: EdgeInsets.symmetric(horizontal: small ? 6 : 8),
      alignment: Alignment.center,
      decoration: BoxDecoration(
        color: dark ? const Color.fromRGBO(0, 0, 0, 0.14) : const Color.fromRGBO(255, 255, 255, 0.12),
        borderRadius: BorderRadius.circular(small ? 6 : 7),
        border: Border.all(
          color: dark ? const Color.fromRGBO(0, 0, 0, 0.25) : const Color.fromRGBO(255, 255, 255, 0.22),
          width: 1,
        ),
      ),
      child: Text(
        text,
        style: VeyloText.ui(
          small ? 12 : 13,
          weight: 700,
          color: dark ? const Color(0xFF000000) : VeyloColors.bone,
          height: 1.0,
        ),
      ),
    );
  }
}

/// Disk-and-memory cached image loader powered by `CachedNetworkImage`, trying each source URL
/// in priority order and decoding at exact `memCacheWidth`.
class FallbackImage extends StatefulWidget {
  const FallbackImage({
    super.key,
    required this.sources,
    this.fit = BoxFit.cover,
    this.alignment = Alignment.center,
    this.cacheWidth,
    this.fallback,
  });

  final List<String> sources;
  final BoxFit fit;
  final Alignment alignment;
  final int? cacheWidth;
  final Widget? fallback;

  @override
  State<FallbackImage> createState() => _FallbackImageState();
}

class _FallbackImageState extends State<FallbackImage> {
  int _index = 0;

  @override
  void didUpdateWidget(covariant FallbackImage oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.sources.join('|') != widget.sources.join('|')) {
      _index = 0;
    }
  }

  @override
  Widget build(BuildContext context) {
    final valid = widget.sources.where((s) => s.isNotEmpty).toList();
    if (_index >= valid.length) {
      return widget.fallback ?? const SizedBox.shrink();
    }
    final url = valid[_index];
    return CachedNetworkImage(
      key: ValueKey(url),
      imageUrl: url,
      fit: widget.fit,
      alignment: widget.alignment,
      memCacheWidth: widget.cacheWidth,
      filterQuality: FilterQuality.medium,
      fadeInDuration: const Duration(milliseconds: 180),
      fadeOutDuration: const Duration(milliseconds: 80),
      errorWidget: (context, _, _) {
        WidgetsBinding.instance.addPostFrameCallback((_) {
          if (mounted && _index < valid.length) {
            setState(() => _index++);
          }
        });
        return widget.fallback ?? const SizedBox.shrink();
      },
    );
  }
}

/// The room's ambient light behind all screens (.ambient).
/// Decodes at 96px wide + GPU blur with `TileMode.clamp` (zero dark edge bleeding!) inside a RepaintBoundary.
class AmbientBackground extends StatelessWidget {
  const AmbientBackground({super.key});

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    return RepaintBoundary(
      child: IgnorePointer(
        child: Stack(
          fit: StackFit.expand,
          children: [
            const ColoredBox(color: VeyloColors.screen),
            ValueListenableBuilder<Color>(
              valueListenable: app.lampColorNotifier,
              builder: (_, lamp, _) => AnimatedContainer(
                duration: const Duration(milliseconds: 500),
                decoration: BoxDecoration(
                  gradient: RadialGradient(
                    center: const Alignment(-0.4, -0.7),
                    radius: 1.15,
                    colors: [
                      lamp.withValues(alpha: 0.25),
                      const Color(0x00000000),
                    ],
                  ),
                ),
              ),
            ),
            ValueListenableBuilder<String>(
              valueListenable: app.ambientUrlNotifier,
              builder: (_, imageUrl, _) => AnimatedSwitcher(
                duration: const Duration(milliseconds: 550),
                child: imageUrl.isNotEmpty
                    ? ImageFiltered(
                        key: ValueKey(imageUrl),
                        imageFilter: ImageFilter.blur(
                          sigmaX: 28,
                          sigmaY: 28,
                          tileMode: TileMode.clamp,
                        ),
                        child: Opacity(
                          opacity: 0.44,
                          child: CachedNetworkImage(
                            imageUrl: imageUrl,
                            fit: BoxFit.cover,
                            width: double.infinity,
                            height: double.infinity,
                            memCacheWidth: 96,
                            fadeInDuration: Duration.zero,
                            errorWidget: (_, _, _) => const SizedBox.shrink(),
                          ),
                        ),
                      )
                    : const SizedBox.shrink(),
              ),
            ),
            const DecoratedBox(
              decoration: BoxDecoration(
                gradient: RadialGradient(
                  center: Alignment(-0.4, -0.6),
                  radius: 1.25,
                  stops: [0.0, 0.70, 1.0],
                  colors: [
                    Color.fromRGBO(0, 0, 0, 0.0),
                    Color.fromRGBO(0, 0, 0, 0.56),
                    Color(0xFF000000),
                  ],
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// Poster card matching `.card` + `.card-art.tall` with 120fps isolated glare ValueNotifier,
/// hover lift (`translateY(-4) scale(1.055)`), specular top sheen, watched check, progress bar,
/// and an optional `overlayBuilder` mounted *inside* the transform so card actions ride 1:1 with the lift.
class PosterCard extends StatefulWidget {
  const PosterCard({
    super.key,
    required this.media,
    this.width,
    this.onTap,
    this.showMetaBelow = false,
    this.lifted = false,
    this.animateMotion = true,
    this.rimColor,
    this.overlayBuilder,
    this.onHoverChanged,
  });

  final MediaItem media;
  final double? width;
  final VoidCallback? onTap;
  final bool showMetaBelow;
  final bool lifted;
  final bool animateMotion;
  final Color? rimColor;
  final Widget Function(BuildContext context, bool hovered)? overlayBuilder;
  final ValueChanged<bool>? onHoverChanged;

  @override
  State<PosterCard> createState() => _PosterCardState();
}

class _PosterCardState extends State<PosterCard> {
  bool _hovered = false;
  bool _pressed = false;
  Timer? _prefetchTimer;
  final ValueNotifier<Alignment> _glare = ValueNotifier(const Alignment(0, -1));

  @override
  void dispose() {
    _prefetchTimer?.cancel();
    _glare.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final m = widget.media;
    final done = m.watched;
    final frac = m.progressFraction;
    final raised = _hovered || widget.lifted;
    final showGlare = _hovered && widget.rimColor == null;

    Widget cardArt = RepaintBoundary(
      child: AspectRatio(
        aspectRatio: 2 / 3,
        child: AnimatedSlide(
          offset: Offset(0, widget.animateMotion && raised ? -0.018 : 0.0),
          duration: Duration(milliseconds: _pressed ? 75 : 280),
          curve: _pressed ? Curves.easeOut : VeyloCurves.spring,
          child: AnimatedScale(
            scale: widget.animateMotion ? (_pressed ? 0.965 : (raised ? 1.048 : 1.0)) : 1.0,
            duration: Duration(milliseconds: _pressed ? 75 : 280),
            curve: _pressed ? Curves.easeOut : VeyloCurves.spring,
            child: Stack(
              fit: StackFit.expand,
              clipBehavior: Clip.none,
              children: [
                // Static shadow layer animated via cheap opacity (avoids re-rasterizing blurRadius 44 every frame).
                IgnorePointer(
                  child: AnimatedOpacity(
                    opacity: raised ? 1.0 : 0.42,
                    duration: const Duration(milliseconds: 220),
                    curve: VeyloCurves.out,
                    child: DecoratedBox(
                      decoration: BoxDecoration(
                        borderRadius: BorderRadius.circular(12),
                        boxShadow: const [
                          BoxShadow(
                            color: Color.fromRGBO(0, 0, 0, 0.72),
                            blurRadius: 24,
                            offset: Offset(0, 12),
                          ),
                        ],
                      ),
                    ),
                  ),
                ),
                DecoratedBox(
                  decoration: BoxDecoration(
                    borderRadius: BorderRadius.circular(12),
                    color: const Color.fromRGBO(255, 255, 255, 0.06),
                  ),
                  child: ClipRRect(
                    borderRadius: BorderRadius.circular(12),
                    child: ValueListenableBuilder<Color>(
                      valueListenable: app.lampColorNotifier,
                      builder: (_, lamp, _) {
                        final targetEdge = widget.rimColor ??
                            (raised
                                ? VeyloColors.mixLamp(lamp, VeyloColors.bone, 0.45).withValues(alpha: 0.34)
                                : const Color.fromRGBO(255, 255, 255, 0.10));
                        final targetSheen = widget.rimColor != null
                            ? const Color(0x00FFFFFF)
                            : Color.fromRGBO(255, 255, 255, raised ? 0.34 : 0.20);
                        return TweenAnimationBuilder<Color?>(
                          tween: ColorTween(end: targetEdge),
                          duration: const Duration(milliseconds: 180),
                          curve: VeyloCurves.out,
                          builder: (_, animEdge, child) => CustomPaint(
                            foregroundPainter: SpecularRimPainter(
                              radius: 12,
                              edgeColor: animEdge ?? targetEdge,
                              sheenColor: targetSheen,
                              borderWidth: widget.rimColor != null ? 1.5 : 1.0,
                            ),
                            child: child,
                          ),
                          child: Stack(
                            fit: StackFit.expand,
                            children: [
                              // Poster tap surface (sibling of overlayBuilder so overlay buttons never press/open the card!)
                              GestureDetector(
                                behavior: HitTestBehavior.opaque,
                                onTapDown: (_) => setState(() => _pressed = true),
                                onTapUp: (_) => setState(() => _pressed = false),
                                onTapCancel: () => setState(() => _pressed = false),
                                onTap: widget.onTap ?? () => app.openTitle(m),
                                child: Stack(
                                  fit: StackFit.expand,
                                  children: [
                                    FallbackImage(
                                      sources: m.posters,
                                      alignment: Alignment.topCenter,
                                      cacheWidth: 320,
                                      fallback: Container(
                                        padding: const EdgeInsets.all(12),
                                        alignment: Alignment.center,
                                        child: Text(
                                          m.name,
                                          textAlign: TextAlign.center,
                                          style: VeyloText.display(18, weight: 700, height: 1.15, color: const Color.fromRGBO(255, 255, 255, 0.6)),
                                        ),
                                      ),
                                    ),
                                    IgnorePointer(
                                      child: AnimatedOpacity(
                                        opacity: showGlare ? 1.0 : 0.0,
                                        duration: const Duration(milliseconds: 220),
                                        curve: VeyloCurves.out,
                                        child: ValueListenableBuilder<Alignment>(
                                          valueListenable: _glare,
                                          builder: (_, align, _) => DecoratedBox(
                                            decoration: BoxDecoration(
                                              gradient: RadialGradient(
                                                center: align,
                                                radius: 0.75,
                                                colors: [
                                                  VeyloColors.mixLamp(lamp, const Color(0xFFFFFFFF), 0.25).withValues(alpha: 0.24),
                                                  const Color.fromRGBO(255, 255, 255, 0.05),
                                                  const Color.fromRGBO(255, 255, 255, 0.0),
                                                ],
                                                stops: const [0.0, 0.32, 0.62],
                                              ),
                                            ),
                                          ),
                                        ),
                                      ),
                                    ),
                        if (done)
                          Positioned(
                            top: 6,
                            right: 6,
                            child: Container(
                              width: 24,
                              height: 24,
                              decoration: BoxDecoration(
                                color: const Color.fromRGBO(0, 0, 0, 0.72),
                                borderRadius: BorderRadius.circular(12),
                                border: Border.all(color: const Color.fromRGBO(255, 255, 255, 0.16)),
                              ),
                              alignment: Alignment.center,
                              child: const VeyloIcon('Check', size: 14, color: VeyloColors.bone),
                            ),
                          ),
                        if (frac > 0 && !done)
                          Positioned(
                            left: 0,
                            right: 0,
                            bottom: 0,
                            child: Container(
                              height: 3,
                              color: const Color.fromRGBO(0, 0, 0, 0.55),
                              alignment: Alignment.centerLeft,
                              child: TweenAnimationBuilder<double>(
                                tween: Tween(begin: widget.animateMotion ? 0 : frac.clamp(0.0, 1.0), end: frac.clamp(0.0, 1.0)),
                                duration: const Duration(milliseconds: 450),
                                curve: VeyloCurves.out,
                                builder: (_, v, _) => FractionallySizedBox(
                                  widthFactor: v,
                                  child: const ColoredBox(color: VeyloColors.bone),
                                ),
                              ),
                            ),
                          ),
                      ],
                    ),
                  ),
                  // Optional overlay mounted inside the transform & MouseRegion (e.g. Library discard control)
                      if (widget.overlayBuilder != null) widget.overlayBuilder!(context, _hovered),
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
    );

    Widget content = Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: [
        cardArt,
        if (widget.showMetaBelow) ...[
          const SizedBox(height: 6),
          Text(
            m.metaLine,
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            style: VeyloText.ui(13, weight: 500, color: VeyloColors.dust, tabular: true),
          ),
        ],
      ],
    );

    if (widget.width != null) {
      content = SizedBox(width: widget.width, child: content);
    }

    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) {
        setState(() => _hovered = true);
        widget.onHoverChanged?.call(true);
        // A short dwell (not every card the pointer crosses), then warm the title page: data in
        // Rust's caches, hero backdrop + logo downloaded and decoded at the page's exact size.
        _prefetchTimer?.cancel();
        _prefetchTimer = Timer(const Duration(milliseconds: 150), () {
          if (mounted && _hovered) prefetchTitle(context, widget.media);
        });
      },
      onExit: (_) {
        setState(() {
          _hovered = false;
          _pressed = false;
        });
        widget.onHoverChanged?.call(false);
      },
      onHover: (e) {
        final box = context.findRenderObject() as RenderBox?;
        if (box != null && box.size.width > 0 && box.size.height > 0) {
          final nx = ((e.localPosition.dx / box.size.width) * 2 - 1).clamp(-1.0, 1.0);
          final ny = ((e.localPosition.dy / box.size.height) * 2 - 1).clamp(-1.0, 1.0);
          _glare.value = Alignment(nx, ny);
        }
      },
      child: content,
    );
  }
}

/// Horizontal scrollable shelf with CSS-exact edge-fade ShaderMask (`data-left` / `data-right`)
/// and desktop hover scroll chevrons (.strip-wrap).
class ShelfStrip extends StatefulWidget {
  const ShelfStrip({
    super.key,
    required this.children,
    required this.gutter,
    this.gap = 16,
    this.height = 266,
  });

  final List<Widget> children;
  final double gutter;
  final double gap;
  final double height;

  @override
  State<ShelfStrip> createState() => _ShelfStripState();
}

class _ShelfStripState extends State<ShelfStrip> {
  final ScrollController _ctrl = ScrollController();
  bool _hovered = false;
  bool _canLeft = false;
  bool _canRight = false;

  @override
  void initState() {
    super.initState();
    _ctrl.addListener(_sync);
    WidgetsBinding.instance.addPostFrameCallback((_) => _sync());
  }

  @override
  void didUpdateWidget(covariant ShelfStrip oldWidget) {
    super.didUpdateWidget(oldWidget);
    WidgetsBinding.instance.addPostFrameCallback((_) => _sync());
  }

  @override
  void dispose() {
    _ctrl.dispose();
    super.dispose();
  }

  void _sync() {
    if (!mounted || !_ctrl.hasClients) return;
    final pos = _ctrl.position;
    final l = pos.pixels > 4;
    final r = pos.pixels < pos.maxScrollExtent - 4;
    if (l != _canLeft || r != _canRight) {
      setState(() {
        _canLeft = l;
        _canRight = r;
      });
    }
  }

  void _page(int dir) {
    if (!_ctrl.hasClients) return;
    final step = (_ctrl.position.viewportDimension * 0.78).clamp(240.0, 1400.0);
    final target = (_ctrl.offset + dir * step).clamp(0.0, _ctrl.position.maxScrollExtent);
    _ctrl.animateTo(target, duration: const Duration(milliseconds: 340), curve: VeyloCurves.out);
  }

  @override
  Widget build(BuildContext context) {
    Widget list = ListView.separated(
      controller: _ctrl,
      scrollDirection: Axis.horizontal,
      clipBehavior: Clip.none,
      padding: EdgeInsets.symmetric(horizontal: widget.gutter, vertical: 14),
      itemCount: widget.children.length,
      separatorBuilder: (_, _) => SizedBox(width: widget.gap),
      itemBuilder: (_, i) => widget.children[i],
    );

    if (_canLeft || _canRight) {
      list = ShaderMask(
        shaderCallback: (bounds) {
          final w = bounds.width > 0 ? bounds.width : 1.0;
          final f = (widget.gutter / w).clamp(0.02, 0.14);
          return LinearGradient(
            begin: Alignment.centerLeft,
            end: Alignment.centerRight,
            stops: [0.0, _canLeft ? f : 0.0, _canRight ? (1.0 - f) : 1.0, 1.0],
            colors: [
              _canLeft ? const Color(0x00FFFFFF) : const Color(0xFFFFFFFF),
              const Color(0xFFFFFFFF),
              const Color(0xFFFFFFFF),
              _canRight ? const Color(0x00FFFFFF) : const Color(0xFFFFFFFF),
            ],
          ).createShader(bounds);
        },
        blendMode: BlendMode.dstIn,
        child: list,
      );
    }

    return RepaintBoundary(
      child: MouseRegion(
        onEnter: (_) {
          _sync();
          setState(() => _hovered = true);
        },
        onExit: (_) => setState(() => _hovered = false),
        child: SizedBox(
          height: widget.height,
          child: Stack(
            clipBehavior: Clip.none,
            children: [
              list,
              if (_canLeft)
                Positioned(
                  left: 12,
                  top: 0,
                  bottom: 0,
                  child: Center(
                    child: AnimatedOpacity(
                      opacity: _hovered ? 1.0 : 0.0,
                      duration: const Duration(milliseconds: 180),
                      child: _StripArrow(icon: 'ChevronLeft', onTap: () => _page(-1)),
                    ),
                  ),
                ),
              if (_canRight)
                Positioned(
                  right: 12,
                  top: 0,
                  bottom: 0,
                  child: Center(
                    child: AnimatedOpacity(
                      opacity: _hovered ? 1.0 : 0.0,
                      duration: const Duration(milliseconds: 180),
                      child: _StripArrow(icon: 'ChevronRight', onTap: () => _page(1)),
                    ),
                  ),
                ),
            ],
          ),
        ),
      ),
    );
  }
}

class _StripArrow extends StatefulWidget {
  const _StripArrow({required this.icon, required this.onTap});
  final String icon;
  final VoidCallback onTap;

  @override
  State<_StripArrow> createState() => _StripArrowState();
}

class _StripArrowState extends State<_StripArrow> {
  bool _hovered = false;
  bool _pressed = false;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
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
          scale: _pressed ? 0.94 : (_hovered ? 1.08 : 1.0),
          duration: const Duration(milliseconds: 160),
          curve: VeyloCurves.out,
          child: AnimatedContainer(
            duration: const Duration(milliseconds: 160),
            width: 42,
            height: 42,
            decoration: BoxDecoration(
              shape: BoxShape.circle,
              color: _hovered ? const Color.fromRGBO(242, 239, 234, 0.94) : const Color.fromRGBO(14, 15, 20, 0.80),
              border: Border.all(color: VeyloColors.edge),
              boxShadow: const [BoxShadow(color: Color.fromRGBO(0, 0, 0, 0.45), blurRadius: 16, offset: Offset(0, 6))],
            ),
            alignment: Alignment.center,
            child: VeyloIcon(widget.icon, size: 22, color: _hovered ? const Color(0xFF000000) : VeyloColors.bone),
          ),
        ),
      ),
    );
  }
}

/// Fact pill (.fact) used on TitlePage and Picks cards.
class FactPill extends StatelessWidget {
  const FactPill({super.key, required this.child, this.horizontalPadding = 12});
  final Widget child;
  final double horizontalPadding;

  @override
  Widget build(BuildContext context) {
    return CustomPaint(
      foregroundPainter: const SpecularRimPainter(
        radius: 17,
        edgeColor: Color.fromRGBO(255, 255, 255, 0.13),
        sheenColor: Color.fromRGBO(255, 255, 255, 0.14),
      ),
      child: Container(
        height: 34,
        padding: EdgeInsets.symmetric(horizontal: horizontalPadding),
        decoration: BoxDecoration(
          color: const Color.fromRGBO(255, 255, 255, 0.08),
          borderRadius: BorderRadius.circular(17),
        ),
        child: DefaultTextStyle(
          style: VeyloText.ui(14, weight: 600, color: VeyloColors.bone, height: 1.0),
          child: child,
        ),
      ),
    );
  }
}

/// Combined IMDb + Rotten Tomatoes + Metacritic score pill (.fact.scores).
class ScoresPill extends StatelessWidget {
  const ScoresPill({super.key, this.imdb, this.tomatometer, this.metascore});
  final String? imdb;
  final int? tomatometer;
  final int? metascore;

  @override
  Widget build(BuildContext context) {
    final hasImdb = imdb != null && imdb!.isNotEmpty;
    if (!hasImdb && tomatometer == null && metascore == null) {
      return const SizedBox.shrink();
    }
    final items = <Widget>[];
    if (hasImdb) {
      items.add(Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          SvgPicture.asset('assets/ratings/imdb.svg', height: 15),
          const SizedBox(width: 6),
          Text(imdb!, style: VeyloText.ui(14, weight: 700, tabular: true, height: 1.0)),
        ],
      ));
    }
    if (tomatometer != null) {
      items.add(Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          SvgPicture.asset(
            tomatometer! >= 60 ? 'assets/ratings/rotten-tomatoes-fresh.svg' : 'assets/ratings/rotten-tomatoes-rotten.svg',
            height: 15,
          ),
          const SizedBox(width: 6),
          Text('$tomatometer%', style: VeyloText.ui(14, weight: 700, tabular: true, height: 1.0)),
        ],
      ));
    }
    if (metascore != null) {
      items.add(Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          SvgPicture.asset('assets/ratings/metacritic.svg', height: 15),
          const SizedBox(width: 6),
          Text('$metascore', style: VeyloText.ui(14, weight: 700, tabular: true, height: 1.0)),
        ],
      ));
    }

    return FactPill(
      horizontalPadding: 13,
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          for (int i = 0; i < items.length; i++) ...[
            if (i > 0) ...[
              const SizedBox(width: 11),
              Container(width: 1, height: 14, color: const Color.fromRGBO(255, 255, 255, 0.16)),
              const SizedBox(width: 11),
            ],
            items[i],
          ],
        ],
      ),
    );
  }
}

/// Group section header (.group-head).
class GroupHead extends StatelessWidget {
  const GroupHead({super.key, required this.icon, required this.title, this.count, this.trailing});
  final String icon;
  final String title;
  final int? count;
  final Widget? trailing;

  @override
  Widget build(BuildContext context) {
    return Row(
      children: [
        VeyloIcon(icon, size: 19, color: VeyloColors.dust),
        const SizedBox(width: 9),
        Text(title, style: VeyloText.groupName),
        if (count != null) ...[
          const SizedBox(width: 8),
          Text(
            '$count',
            style: VeyloText.ui(14, weight: 600, color: const Color.fromRGBO(242, 239, 234, 0.45), tabular: true),
          ),
        ],
        const Spacer(),
        ?trailing,
      ],
    );
  }
}

/// GPU shimmer skeleton block powered by `flutter_animate`.
class SkeletonBox extends StatelessWidget {
  const SkeletonBox({super.key, this.width, this.height, this.borderRadius = 12});
  final double? width;
  final double? height;
  final double borderRadius;

  @override
  Widget build(BuildContext context) {
    return Container(
      width: width,
      height: height,
      decoration: BoxDecoration(
        borderRadius: BorderRadius.circular(borderRadius),
        color: const Color.fromRGBO(255, 255, 255, 0.05),
      ),
    )
        .animate(onPlay: (c) => c.repeat())
        .shimmer(
          duration: const Duration(milliseconds: 1400),
          color: const Color.fromRGBO(255, 255, 255, 0.08),
        );
  }
}

/// Segmented control with a physically gliding bone `.slider` pill matching motion.css:20-27.
class VeyloSegmented<T> extends StatefulWidget {
  const VeyloSegmented({
    super.key,
    required this.value,
    required this.options,
    required this.onChanged,
    this.compact = false,
  });

  final T value;
  final List<(T, String, String?)> options; // (value, label, optional icon)
  final ValueChanged<T> onChanged;
  final bool compact;

  @override
  State<VeyloSegmented<T>> createState() => _VeyloSegmentedState<T>();
}

class _VeyloSegmentedState<T> extends State<VeyloSegmented<T>> {
  final GlobalKey _rowKey = GlobalKey();
  late List<GlobalKey> _itemKeys;
  double _sliderLeft = 0;
  double _sliderWidth = 0;
  bool _measured = false;

  @override
  void initState() {
    super.initState();
    _itemKeys = List.generate(widget.options.length, (_) => GlobalKey());
    WidgetsBinding.instance.addPostFrameCallback((_) => _measure());
  }

  @override
  void didUpdateWidget(covariant VeyloSegmented<T> oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.options.length != widget.options.length) {
      _itemKeys = List.generate(widget.options.length, (_) => GlobalKey());
    }
    WidgetsBinding.instance.addPostFrameCallback((_) => _measure());
  }

  void _measure() {
    if (!mounted) return;
    final idx = widget.options.indexWhere((o) => o.$1 == widget.value);
    if (idx < 0 || idx >= _itemKeys.length) return;
    final rowBox = _rowKey.currentContext?.findRenderObject() as RenderBox?;
    final itemBox = _itemKeys[idx].currentContext?.findRenderObject() as RenderBox?;
    if (rowBox == null || itemBox == null || !rowBox.hasSize || !itemBox.hasSize) return;
    final pos = itemBox.localToGlobal(Offset.zero, ancestor: rowBox);
    if ((pos.dx - _sliderLeft).abs() > 0.5 || (itemBox.size.width - _sliderWidth).abs() > 0.5 || !_measured) {
      setState(() {
        _sliderLeft = pos.dx;
        _sliderWidth = itemBox.size.width;
        _measured = true;
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final h = widget.compact ? 32.0 : 40.0;
    return Container(
      padding: const EdgeInsets.all(3),
      decoration: BoxDecoration(
        color: const Color.fromRGBO(255, 255, 255, 0.06),
        borderRadius: BorderRadius.circular(24),
        border: Border.all(color: VeyloColors.edge),
      ),
      child: Stack(
        children: [
          if (_measured)
            AnimatedPositioned(
              duration: const Duration(milliseconds: 240),
              curve: VeyloCurves.slider,
              left: _sliderLeft,
              top: 0,
              width: _sliderWidth,
              height: h,
              child: Container(
                decoration: BoxDecoration(
                  color: VeyloColors.bone,
                  borderRadius: BorderRadius.circular(20),
                  border: const Border(top: BorderSide(color: Color.fromRGBO(255, 255, 255, 0.65), width: 1)),
                  boxShadow: const [BoxShadow(color: Color(0x33000000), blurRadius: 8, offset: Offset(0, 2))],
                ),
              ),
            ),
          Row(
            key: _rowKey,
            mainAxisSize: MainAxisSize.min,
            children: [
              for (int i = 0; i < widget.options.length; i++)
                _SegItem(
                  key: _itemKeys[i],
                  label: widget.options[i].$2,
                  icon: widget.options[i].$3,
                  active: widget.options[i].$1 == widget.value,
                  height: h,
                  compact: widget.compact,
                  onTap: () => widget.onChanged(widget.options[i].$1),
                ),
            ],
          ),
        ],
      ),
    );
  }
}

class _SegItem extends StatefulWidget {
  const _SegItem({
    super.key,
    required this.label,
    required this.icon,
    required this.active,
    required this.height,
    required this.compact,
    required this.onTap,
  });

  final String label;
  final String? icon;
  final bool active;
  final double height;
  final bool compact;
  final VoidCallback onTap;

  @override
  State<_SegItem> createState() => _SegItemState();
}

class _SegItemState extends State<_SegItem> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final targetFg = widget.active
        ? const Color(0xFF000000)
        : (_hovered ? VeyloColors.bone.withValues(alpha: 0.78) : VeyloColors.dust);
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: widget.onTap,
        child: Container(
          height: widget.height,
          padding: EdgeInsets.symmetric(horizontal: widget.compact ? 13 : 16),
          alignment: Alignment.center,
          child: TweenAnimationBuilder<Color?>(
            tween: ColorTween(end: targetFg),
            duration: const Duration(milliseconds: 180),
            curve: VeyloCurves.out,
            builder: (_, animFg, _) {
              final fg = animFg ?? targetFg;
              return Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  if (widget.icon != null) ...[
                    VeyloIcon(widget.icon!, size: 16, color: fg),
                    if (widget.label.isNotEmpty) const SizedBox(width: 6),
                  ],
                  if (widget.label.isNotEmpty)
                    Text(
                      widget.label,
                      style: VeyloText.ui(widget.compact ? 13 : 14, weight: 600, color: fg, height: 1.0),
                    ),
                ],
              );
            },
          ),
        ),
      ),
    );
  }
}
