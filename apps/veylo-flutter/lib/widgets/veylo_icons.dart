import 'package:flutter/widgets.dart';
import 'package:path_parsing/path_parsing.dart';
import 'veylo_icons.g.dart';

/// Exact vector geometry from apps/veylo-dx/src/icons.rs (24×24 viewBox, 1.85 round stroke,
/// and exact .fillable support), parsed once and cached as GPU Path objects.
class VeyloIcon extends StatelessWidget {
  const VeyloIcon(
    this.name, {
    super.key,
    this.size = 20,
    this.color,
    this.filled = false,
  });

  final String name;
  final double size;
  final Color? color;
  final bool filled;

  static final Map<String, List<Path>> _pathCache = {};

  static List<Path>? _pathsFor(String name) {
    final cached = _pathCache[name];
    if (cached != null) return cached;
    final prims = kVeyloIcons[name];
    if (prims == null) return null;
    final built = <Path>[];
    for (final p in prims) {
      switch (p) {
        case VeyloIconPath(:final d):
          final path = Path();
          writeSvgPathDataToPath(d, _FlutterPathProxy(path));
          built.add(path);
        case VeyloIconCircle(:final cx, :final cy, :final r):
          final path = Path()
            ..addOval(Rect.fromCircle(center: Offset(cx, cy), radius: r));
          built.add(path);
        case VeyloIconRect(:final x, :final y, :final w, :final h, :final rx, :final ry):
          final path = Path();
          final rect = Rect.fromLTWH(x, y, w, h);
          if (rx > 0 || ry > 0) {
            path.addRRect(RRect.fromRectXY(rect, rx, ry));
          } else {
            path.addRect(rect);
          }
          built.add(path);
      }
    }
    _pathCache[name] = built;
    return built;
  }

  @override
  Widget build(BuildContext context) {
    final c = color ?? DefaultTextStyle.of(context).style.color ?? const Color(0xFFF2EFEA);
    final paths = _pathsFor(name);
    if (paths == null) return SizedBox(width: size, height: size);
    final shouldFill = filled && kVeyloFillableIcons.contains(name);
    return SizedBox(
      width: size,
      height: size,
      child: CustomPaint(
        painter: _VeyloIconPainter(paths: paths, color: c, filled: shouldFill),
      ),
    );
  }
}

class _VeyloIconPainter extends CustomPainter {
  const _VeyloIconPainter({
    required this.paths,
    required this.color,
    required this.filled,
  });

  final List<Path> paths;
  final Color color;
  final bool filled;

  @override
  void paint(Canvas canvas, Size size) {
    canvas.save();
    canvas.scale(size.width / 24.0, size.height / 24.0);

    if (filled) {
      final fillPaint = Paint()
        ..color = color
        ..style = PaintingStyle.fill
        ..isAntiAlias = true;
      for (final path in paths) {
        canvas.drawPath(path, fillPaint);
      }
    }

    final strokePaint = Paint()
      ..color = color
      ..style = PaintingStyle.stroke
      ..strokeWidth = 1.85
      ..strokeCap = StrokeCap.round
      ..strokeJoin = StrokeJoin.round
      ..isAntiAlias = true;
    for (final path in paths) {
      canvas.drawPath(path, strokePaint);
    }

    canvas.restore();
  }

  @override
  bool shouldRepaint(_VeyloIconPainter old) =>
      old.paths != paths || old.color != color || old.filled != filled;
}

class _FlutterPathProxy extends PathProxy {
  _FlutterPathProxy(this.path);
  final Path path;

  @override
  void close() => path.close();

  @override
  void cubicTo(double x1, double y1, double x2, double y2, double x3, double y3) =>
      path.cubicTo(x1, y1, x2, y2, x3, y3);

  @override
  void lineTo(double x, double y) => path.lineTo(x, y);

  @override
  void moveTo(double x, double y) => path.moveTo(x, y);
}
