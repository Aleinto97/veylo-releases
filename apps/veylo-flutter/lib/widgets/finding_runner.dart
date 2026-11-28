import 'dart:math' as math;
import 'package:flutter/scheduler.dart';
import 'package:flutter/widgets.dart';
import '../theme/veylo_theme.dart';

enum CinemaScene {
  dino,      // Comedy / Animation / Family: minimalist dino hopping clapperboards & popping popcorn
  orbit,     // Sci-Fi / Fantasy: sleek rocket weaving past ringed planets & crescent moons
  noir,      // Horror / Thriller / Mystery: cheeky little ghost hiding from a sweeping spotlight
  chase,     // Action / Adventure / Crime: bouncy retro getaway coupe popping turbo sparks
  projector, // Drama / Romance / Music: twin-reel vintage projector casting a shimmering beam
  glider,    // Documentary / Anime / Other: minimalist paper plane threading glowing rings
  countdown, // Trailers / previews: vintage minimalist film leader crosshair & snapping clapper
}

/// Curated set of ultra-minimal, playful cinema intermission vignettes matched to the film's
/// genres and rotated so consecutive plays never repeat the same animation.
/// Pure floating line-art & silhouettes on the dark canvas — zero box, zero text, zero game UI.
class FindingRunner extends StatefulWidget {
  const FindingRunner({
    super.key,
    required this.lamp,
    this.genres = const [],
    this.mediaKind = 'movie',
    this.title = '',
    this.scene,
    this.width = 340,
    this.height = 118,
  });

  final Color lamp;
  final List<String> genres;
  final String mediaKind;
  final String title;
  final CinemaScene? scene;
  final double width;
  final double height;

  @override
  State<FindingRunner> createState() => _FindingRunnerState();
}

class _FindingRunnerState extends State<FindingRunner> with SingleTickerProviderStateMixin {
  static CinemaScene? _lastKind;
  static int _playCount = 0;

  late final CinemaScene _kind;
  late final Ticker _ticker;
  late final _VignetteState _state;
  Duration _lastElapsed = Duration.zero;

  static CinemaScene _chooseScene(List<String> genres, String mediaKind, String title) {
    final g = genres.map((e) => e.toLowerCase()).toList();
    bool hasAny(List<String> keys) => g.any((x) => keys.any((k) => x.contains(k)));

    final pool = <CinemaScene>[];
    if (hasAny(['sci-fi', 'science fiction', 'fantasy', 'space'])) {
      pool.add(CinemaScene.orbit);
    }
    if (hasAny(['horror', 'thriller', 'mystery', 'noir'])) {
      pool.add(CinemaScene.noir);
    }
    if (hasAny(['action', 'adventure', 'crime', 'western', 'war'])) {
      pool.add(CinemaScene.chase);
    }
    if (hasAny(['comedy', 'animation', 'family', 'kids']) || mediaKind == 'anime') {
      pool.add(CinemaScene.dino);
    }
    if (hasAny(['romance', 'drama', 'music', 'musical', 'history', 'biography'])) {
      pool.add(CinemaScene.projector);
    }
    if (hasAny(['documentary', 'nature', 'anime']) || pool.isEmpty) {
      pool.add(CinemaScene.glider);
    }
    // Always keep at least 2 complementary scenes in the pool so consecutive plays of the same
    // genre still feel fresh and never repeat back-to-back.
    if (pool.length < 2) {
      for (final extra in const [
        CinemaScene.dino,
        CinemaScene.projector,
        CinemaScene.glider,
        CinemaScene.orbit,
        CinemaScene.chase,
        CinemaScene.noir,
      ]) {
        if (!pool.contains(extra)) {
          pool.add(extra);
          if (pool.length >= 3) break;
        }
      }
    }

    final seed = (_playCount++) + title.hashCode.abs();
    var pick = pool[seed % pool.length];
    if (pick == _lastKind && pool.length > 1) {
      pick = pool[(seed + 1) % pool.length];
    }
    _lastKind = pick;
    return pick;
  }

  @override
  void initState() {
    super.initState();
    _kind = widget.scene ?? _chooseScene(widget.genres, widget.mediaKind, widget.title);
    _state = _VignetteState(_kind);
    _ticker = createTicker(_onTick)..start();
  }

  void _onTick(Duration elapsed) {
    final dt = _lastElapsed == Duration.zero
        ? 0.016
        : ((elapsed - _lastElapsed).inMicroseconds / 1e6).clamp(0.0, 0.05);
    _lastElapsed = elapsed;
    _state.step(dt);
  }

  @override
  void dispose() {
    _ticker.dispose();
    _state.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return SizedBox(
      width: widget.width,
      height: widget.height,
      child: RepaintBoundary(
        child: CustomPaint(
          painter: _VignettePainter(state: _state, lamp: widget.lamp),
        ),
      ),
    );
  }
}

class _Prop {
  _Prop({required this.x, required this.kind});
  double x;
  final int kind;
  bool triggered = false;
  double react = 0;
}

class _Spark {
  _Spark(this.x, this.y, this.vx, this.vy, this.life, this.size, this.accent);
  double x, y, vx, vy, life, size;
  final bool accent;
}

class _VignetteState extends ChangeNotifier {
  _VignetteState(this.kind) {
    if (kind == CinemaScene.dino) {
      props.add(_Prop(x: 200, kind: 0));
      props.add(_Prop(x: 360, kind: 1));
      props.add(_Prop(x: 520, kind: 2));
    } else if (kind == CinemaScene.chase) {
      props.add(_Prop(x: 225, kind: 0));
      props.add(_Prop(x: 395, kind: 0));
    }
  }

  static const double w = 340;
  static const double h = 118;
  static const double groundY = 90;

  final CinemaScene kind;
  final math.Random _rng = math.Random();
  final List<_Prop> props = [];
  final List<_Spark> sparks = [];

  double time = 0;
  double phase = 0;

  // Dino / Chase shared hop physics
  double y = 0;
  double vy = 0;
  double squash = 1.0;
  double tilt = 0.0;
  double blinkTimer = 1.9;
  double blink = 0.0;
  int _nextProp = 0;

  // Noir ghost horizontal position & hiding state
  double ghostX = 74;
  double ghostHide = 0; // 0 = trotting, 1 = crouched hiding from spotlight

  void step(double dt) {
    time += dt;

    // Gentle eyes blink
    blinkTimer -= dt;
    if (blinkTimer <= 0) {
      blink = 1.0;
      blinkTimer = 2.0 + _rng.nextDouble() * 2.2;
    } else if (blink > 0) {
      blink = (blink - dt * 8.0).clamp(0.0, 1.0);
    }

    switch (kind) {
      case CinemaScene.dino:
        _stepDino(dt);
      case CinemaScene.chase:
        _stepChase(dt);
      case CinemaScene.noir:
        _stepNoir(dt);
      case CinemaScene.orbit:
        _stepOrbit(dt);
      case CinemaScene.projector:
        _stepProjector(dt);
      case CinemaScene.glider:
        _stepGlider(dt);
      case CinemaScene.countdown:
        _stepCountdown(dt);
    }

    // Update particles
    for (int i = sparks.length - 1; i >= 0; i--) {
      final s = sparks[i];
      s.x += s.vx * dt;
      s.y += s.vy * dt;
      if (kind == CinemaScene.dino || kind == CinemaScene.chase) {
        s.vy += 190 * dt;
      }
      s.life -= dt;
      if (s.life <= 0) sparks.removeAt(i);
    }

    notifyListeners();
  }

  void _stepDino(double dt) {
    const speed = 134.0;
    const rx = 96.0;
    phase = (phase + speed * dt) % 24.0;

    for (int i = props.length - 1; i >= 0; i--) {
      final p = props[i];
      p.x -= speed * dt;
      if (!p.triggered && p.x < rx + 18) {
        p.triggered = true;
        if (p.kind == 1) {
          for (int k = 0; k < 3; k++) {
            sparks.add(_Spark(p.x + (k - 1) * 3.0, groundY - 16, -34 + (k - 1) * 22.0, -88 - _rng.nextDouble() * 40, 0.65, 2.2, true));
          }
        } else if (p.kind == 0) {
          sparks.add(_Spark(p.x, groundY - 18, -28, -56, 0.42, 1.8, true));
          sparks.add(_Spark(p.x + 4, groundY - 18, 12, -52, 0.42, 1.8, true));
        }
      }
      if (p.triggered && p.react < 1.0) p.react = (p.react + dt * 3.2).clamp(0.0, 1.0);
      if (p.x < -36) props.removeAt(i);
    }

    final rightMost = props.isEmpty ? 0.0 : props.map((p) => p.x).reduce(math.max);
    if (rightMost < w + 40) {
      props.add(_Prop(x: math.max(w + 28, rightMost + 148 + _rng.nextDouble() * 40), kind: _nextProp % 3));
      _nextProp++;
    }

    if (y <= 0.5) {
      for (final p in props) {
        final dist = p.x - rx;
        if (dist > 14 && dist < 50) {
          vy = 328;
          squash = 1.15;
          for (int k = 0; k < 3; k++) {
            sparks.add(_Spark(rx - 4 + _rng.nextDouble() * 8, groundY - 1, -26 - _rng.nextDouble() * 34, -12 - _rng.nextDouble() * 20, 0.36, 1.5, false));
          }
          break;
        }
      }
    }

    final wasUp = y > 0;
    if (y > 0 || vy > 0) {
      y += vy * dt;
      vy -= 1180.0 * dt;
      tilt = (vy / 360.0).clamp(-0.22, 0.22);
      if (y <= 0) {
        y = 0;
        vy = 0;
        tilt = 0;
        if (wasUp) squash = 0.86;
      }
    }
    squash += (1.0 - squash) * (dt * 12.0).clamp(0.0, 1.0);
  }

  void _stepChase(double dt) {
    const speed = 178.0;
    const carX = 104.0;
    phase = (phase + speed * dt) % 28.0;

    for (int i = props.length - 1; i >= 0; i--) {
      final p = props[i];
      p.x -= speed * dt;
      if (!p.triggered && p.x < carX + 16) {
        p.triggered = true;
        vy = 185;
        squash = 1.12;
        for (int k = 0; k < 4; k++) {
          sparks.add(_Spark(carX - 20, groundY - 6, -55 - _rng.nextDouble() * 60, -25 - _rng.nextDouble() * 45, 0.42, 1.8, true));
        }
      }
      if (p.x < -30) props.removeAt(i);
    }

    final rightMost = props.isEmpty ? 0.0 : props.map((p) => p.x).reduce(math.max);
    if (rightMost < w + 40) {
      props.add(_Prop(x: math.max(w + 26, rightMost + 150 + _rng.nextDouble() * 55), kind: 0));
    }

    // Subtle exhaust puffs
    if (_rng.nextDouble() < dt * 11) {
      sparks.add(_Spark(carX - 24, groundY - 5 - y, -60 - _rng.nextDouble() * 35, -14 - _rng.nextDouble() * 16, 0.34, 1.5, false));
    }

    final wasUp = y > 0;
    if (y > 0 || vy > 0) {
      y += vy * dt;
      vy -= 1100.0 * dt;
      tilt = (vy / 520.0).clamp(-0.18, 0.18);
      if (y <= 0) {
        y = 0;
        vy = 0;
        tilt = 0;
        if (wasUp) squash = 0.88;
      }
    }
    squash += (1.0 - squash) * (dt * 12.0).clamp(0.0, 1.0);
  }

  void _stepNoir(double dt) {
    // Spotlight sweeps smoothly left <-> right across x = 90..260
    final spotX = 175.0 + math.sin(time * 1.45) * 95.0;
    final caught = (ghostX - spotX).abs() < 34.0;
    final targetHide = caught ? 1.0 : 0.0;
    ghostHide += (targetHide - ghostHide) * (dt * 14.0).clamp(0.0, 1.0);

    // Tiptoe forward briskly whenever the beam isn't shining right on it!
    if (ghostHide < 0.45) {
      ghostX += 76.0 * dt;
      if (ghostX > w - 42) ghostX = 42;
    }
    phase = (phase + 40 * dt) % 24.0;
  }

  void _stepOrbit(double dt) {
    phase += dt;
    // Gentle stardust ribbon trailing the tiny rocket
    if (_rng.nextDouble() < dt * 22) {
      final ry = 58 + math.sin(time * 2.4) * 14;
      sparks.add(_Spark(126, ry + (_rng.nextDouble() - 0.5) * 4, -85 - _rng.nextDouble() * 35, (_rng.nextDouble() - 0.5) * 10, 0.55, 1.6, _rng.nextBool()));
    }
  }

  void _stepProjector(double dt) {
    phase += dt;
    // Floating cinema motes / tiny sparkles drifting down the projector beam
    if (_rng.nextDouble() < dt * 7.5) {
      final t = _rng.nextDouble();
      final bx = 128 + t * 175;
      final spread = 12 + t * 32;
      final by = 56 + (_rng.nextDouble() * 2 - 1) * spread;
      sparks.add(_Spark(bx, by, 14 + _rng.nextDouble() * 14, -6 + _rng.nextDouble() * 12, 0.95, 1.4 + _rng.nextDouble() * 1.4, _rng.nextBool()));
    }
  }

  void _stepGlider(double dt) {
    phase += dt;
    // Silky vapor trail behind the origami plane
    if (_rng.nextDouble() < dt * 24) {
      final py = 56 + math.sin(time * 2.6) * 18;
      sparks.add(_Spark(138, py, -95, (_rng.nextDouble() - 0.5) * 6, 0.62, 1.5, _rng.nextDouble() < 0.35));
    }
  }

  void _stepCountdown(double dt) {
    final prevBeat = (time / 1.15).floor();
    final currBeat = ((time + dt) / 1.15).floor();
    phase = (time % 1.15) / 1.15;
    if (currBeat != prevBeat) {
      // Tiny clapper spark on every film leader beat
      for (int k = 0; k < 4; k++) {
        final a = -math.pi * 0.25 - k * 0.42;
        sparks.add(_Spark(170 + math.cos(a) * 26, 59 + math.sin(a) * 26, math.cos(a) * 34, math.sin(a) * 34, 0.45, 1.7, true));
      }
    }
  }
}

class _VignettePainter extends CustomPainter {
  _VignettePainter({required this.state, required this.lamp}) : super(repaint: state);

  final _VignetteState state;
  final Color lamp;

  @override
  void paint(Canvas canvas, Size size) {
    final sx = size.width / _VignetteState.w;
    final sy = size.height / _VignetteState.h;
    canvas.save();
    canvas.scale(sx, sy);

    switch (state.kind) {
      case CinemaScene.dino:
        _paintDinoScene(canvas);
      case CinemaScene.chase:
        _paintChaseScene(canvas);
      case CinemaScene.noir:
        _paintNoirScene(canvas);
      case CinemaScene.orbit:
        _paintOrbitScene(canvas);
      case CinemaScene.projector:
        _paintProjectorScene(canvas);
      case CinemaScene.glider:
        _paintGliderScene(canvas);
      case CinemaScene.countdown:
        _paintCountdownScene(canvas);
    }

    canvas.restore();
  }

  void _drawHorizon(Canvas canvas, {double y = _VignetteState.groundY, bool ticks = true}) {
    const w = _VignetteState.w;
    final horizon = Paint()
      ..shader = const LinearGradient(
        colors: [Color(0x00F2EFEA), Color(0x40F2EFEA), Color(0x40F2EFEA), Color(0x00F2EFEA)],
        stops: [0.0, 0.16, 0.84, 1.0],
      ).createShader(Rect.fromLTWH(0, y, w, 1.2))
      ..strokeWidth = 1.2;
    canvas.drawLine(Offset(0, y), Offset(w, y), horizon);

    if (ticks) {
      for (double x = 24 - state.phase; x < w - 16; x += 24) {
        final fade = (math.min(x, w - x) / 48.0).clamp(0.0, 1.0);
        canvas.drawRRect(
          RRect.fromRectAndRadius(Rect.fromLTWH(x, y + 5, 4, 1.5), const Radius.circular(0.8)),
          Paint()..color = VeyloColors.bone.withValues(alpha: 0.14 * fade),
        );
      }
    }
  }

  void _drawSparks(Canvas canvas) {
    for (final s in state.sparks) {
      final a = (s.life * 1.6).clamp(0.0, 0.85);
      canvas.drawCircle(
        Offset(s.x, s.y),
        s.size,
        Paint()..color = (s.accent ? lamp : VeyloColors.bone).withValues(alpha: a),
      );
    }
  }

  // ── 1. Cinema Dino (Comedy / Animation / Family) ──────────────────────────
  void _paintDinoScene(Canvas canvas) {
    const w = _VignetteState.w;
    const gY = _VignetteState.groundY;
    _drawHorizon(canvas);

    for (final p in state.props) {
      final fade = (math.min(p.x + 12, w - p.x + 12) / 42.0).clamp(0.0, 1.0);
      if (fade <= 0.01) continue;
      final bone = Paint()..color = VeyloColors.bone.withValues(alpha: 0.62 * fade);
      final accent = Paint()..color = lamp.withValues(alpha: 0.84 * fade);
      final stroke = Paint()
        ..color = VeyloColors.bone.withValues(alpha: 0.58 * fade)
        ..style = PaintingStyle.stroke
        ..strokeWidth = 1.5;

      if (p.kind == 0) {
        final clap = p.triggered
            ? -0.45 * math.exp(-p.react * 5.0) * math.cos(p.react * 10.0)
            : -0.34;
        canvas.save();
        canvas.translate(p.x, gY);
        canvas.drawRRect(RRect.fromRectAndRadius(const Rect.fromLTWH(-9, -13, 18, 13), const Radius.circular(2.5)), bone);
        canvas.save();
        canvas.translate(-9, -13);
        canvas.rotate(clap);
        canvas.drawRRect(RRect.fromRectAndRadius(const Rect.fromLTWH(0, -4, 18, 4), const Radius.circular(1.5)), accent);
        canvas.restore();
        canvas.restore();
      } else if (p.kind == 1) {
        final bounce = p.triggered ? math.sin(p.react * math.pi) * 2.5 : 0.0;
        canvas.save();
        canvas.translate(p.x, gY - bounce);
        final tub = Path()
          ..moveTo(-7, -14)
          ..lineTo(7, -14)
          ..lineTo(5, 0)
          ..lineTo(-5, 0)
          ..close();
        canvas.drawPath(tub, bone);
        canvas.drawCircle(const Offset(-3.5, -15.5), 3.0, accent);
        canvas.drawCircle(const Offset(0.5, -17.0), 3.4, accent);
        canvas.drawCircle(const Offset(4.0, -15.2), 2.8, accent);
        canvas.restore();
      } else {
        const r = 8.5;
        canvas.save();
        canvas.translate(p.x, gY - r);
        canvas.rotate(-state.time * 5.5);
        canvas.drawCircle(Offset.zero, r, stroke);
        canvas.drawCircle(Offset.zero, 2.2, accent);
        for (int i = 0; i < 4; i++) {
          final a = i * math.pi / 2;
          canvas.drawCircle(Offset(math.cos(a) * 4.8, math.sin(a) * 4.8), 1.4, bone);
        }
        canvas.restore();
      }
    }

    _drawSparks(canvas);

    // Dino
    const x = 96.0;
    final trotBob = state.y <= 0.5 ? math.sin(state.time * 22.0).abs() * 1.6 : 0.0;
    final bottom = gY - state.y - trotBob;
    final sy = state.squash;
    final sx = 1.0 / math.sqrt(sy);
    final sh = (1.0 - state.y / 85.0).clamp(0.28, 1.0);
    canvas.drawOval(
      Rect.fromCenter(center: const Offset(x, gY + 0.5), width: 20 * sh, height: 3 * sh),
      Paint()..color = lamp.withValues(alpha: 0.16 * sh),
    );
    canvas.save();
    canvas.translate(x, bottom);
    canvas.rotate(-state.tilt);
    canvas.scale(sx, sy);
    canvas.translate(-12, -26);
    final bone = Paint()..color = VeyloColors.bone;
    final dark = Paint()..color = const Color(0xFF050506);
    final accent = Paint()..color = lamp;
    final wag = math.sin(state.time * 12.0) * 1.1;
    canvas.drawRRect(RRect.fromRectAndRadius(Rect.fromLTWH(0, 11 + wag * 0.5, 6, 5.5), const Radius.circular(2.5)), bone);
    canvas.drawRRect(RRect.fromRectAndRadius(const Rect.fromLTWH(5, 10, 11.5, 11.5), const Radius.circular(4.5)), bone);
    canvas.drawRRect(RRect.fromRectAndRadius(const Rect.fromLTWH(15.5, 13.5, 4, 2.3), const Radius.circular(1.2)), bone);
    canvas.drawRRect(RRect.fromRectAndRadius(const Rect.fromLTWH(10.5, 1, 13.5, 10.5), const Radius.circular(4.2)), bone);
    canvas.drawRRect(RRect.fromRectAndRadius(const Rect.fromLTWH(18.5, 8.2, 6, 1.8), const Radius.circular(1)), dark);
    canvas.drawCircle(const Offset(9.2, 9.2), 1.6, accent);
    if (state.blink > 0.35) {
      canvas.drawLine(const Offset(14.8, 5.0), const Offset(17.4, 5.0), Paint()..color = const Color(0xFF050506)..strokeWidth = 1.4..strokeCap = StrokeCap.round);
    } else {
      canvas.drawCircle(const Offset(16.1, 5.0), 1.4, dark);
    }
    final air = state.y > 2.0;
    final ph = math.sin(state.time * 22.0);
    canvas.drawRRect(RRect.fromRectAndRadius(Rect.fromLTWH(7.2, 20, 3.2, air ? 3.8 : 4.8 + math.max(0.0, ph) * 2.2), const Radius.circular(1.6)), bone);
    canvas.drawRRect(RRect.fromRectAndRadius(Rect.fromLTWH(12.2, 20, 3.2, air ? 3.8 : 4.8 + math.max(0.0, -ph) * 2.2), const Radius.circular(1.6)), bone);
    canvas.restore();
  }

  // ── 2. Getaway Car (Action / Adventure / Crime) ───────────────────────────
  void _paintChaseScene(Canvas canvas) {
    const w = _VignetteState.w;
    const gY = _VignetteState.groundY;
    _drawHorizon(canvas);

    // Tiny playful chopper bobbing lazily high in the background
    final heliX = 246 + math.sin(state.time * 1.1) * 14;
    final heliY = 26 + math.sin(state.time * 2.8) * 3;
    final faint = Paint()..color = VeyloColors.bone.withValues(alpha: 0.32);
    canvas.drawRRect(RRect.fromRectAndRadius(Rect.fromCenter(center: Offset(heliX, heliY), width: 16, height: 7), const Radius.circular(3.5)), faint);
    canvas.drawLine(Offset(heliX + 7, heliY), Offset(heliX + 16, heliY - 1), Paint()..color = VeyloColors.bone.withValues(alpha: 0.32)..strokeWidth = 1.4);
    final rotorW = 12 + math.sin(state.time * 42.0).abs() * 14;
    canvas.drawLine(Offset(heliX - rotorW / 2, heliY - 6), Offset(heliX + rotorW / 2, heliY - 6), Paint()..color = lamp.withValues(alpha: 0.5)..strokeWidth = 1.3);

    // Speed-bumps gliding along the road
    for (final p in state.props) {
      final fade = (math.min(p.x + 10, w - p.x + 10) / 40.0).clamp(0.0, 1.0);
      if (fade <= 0.01) continue;
      final arc = Path()
        ..moveTo(p.x - 10, gY)
        ..quadraticBezierTo(p.x, gY - 7, p.x + 10, gY);
      canvas.drawPath(
        arc,
        Paint()
          ..color = lamp.withValues(alpha: 0.7 * fade)
          ..style = PaintingStyle.stroke
          ..strokeWidth = 1.8
          ..strokeCap = StrokeCap.round,
      );
    }

    _drawSparks(canvas);

    // Sleek minimalist retro coupe
    const cx = 104.0;
    final hum = state.y <= 0.5 ? math.sin(state.time * 32.0) * 0.6 : 0.0;
    final bottom = gY - state.y + hum;
    final sh = (1.0 - state.y / 65.0).clamp(0.35, 1.0);
    canvas.drawOval(
      Rect.fromCenter(center: const Offset(cx, gY + 0.5), width: 42 * sh, height: 3.5 * sh),
      Paint()..color = lamp.withValues(alpha: 0.16 * sh),
    );

    canvas.save();
    canvas.translate(cx, bottom);
    canvas.rotate(-state.tilt);
    canvas.scale(1.0 / math.sqrt(state.squash), state.squash);
    final bone = Paint()..color = VeyloColors.bone;
    final dark = Paint()..color = const Color(0xFF050506);
    final accent = Paint()..color = lamp;

    // Lower body
    canvas.drawRRect(RRect.fromRectAndRadius(const Rect.fromLTWH(-21, -13, 42, 9), const Radius.circular(3.5)), bone);
    // Cabin roofline
    final cabin = Path()
      ..moveTo(-13, -13)
      ..lineTo(-7, -21)
      ..lineTo(7, -21)
      ..lineTo(13, -13)
      ..close();
    canvas.drawPath(cabin, bone);
    // Windshield cutout
    final win = Path()
      ..moveTo(-9, -13.8)
      ..lineTo(-5.5, -19)
      ..lineTo(5.5, -19)
      ..lineTo(9.5, -13.8)
      ..close();
    canvas.drawPath(win, dark);
    // Headlight beam cone
    final beam = Path()
      ..moveTo(21, -9.5)
      ..lineTo(54, -15)
      ..lineTo(54, -2)
      ..lineTo(21, -7.5)
      ..close();
    canvas.drawPath(
      beam,
      Paint()
        ..shader = LinearGradient(
          colors: [lamp.withValues(alpha: 0.34), lamp.withValues(alpha: 0.0)],
        ).createShader(const Rect.fromLTWH(21, -15, 33, 13)),
    );
    // Taillight dot
    canvas.drawCircle(const Offset(-20, -9.5), 1.6, accent);
    // Wheels
    for (final wx in const [-12.0, 12.0]) {
      canvas.drawCircle(Offset(wx, -4.5), 4.6, dark);
      canvas.drawCircle(Offset(wx, -4.5), 3.6, Paint()..color = VeyloColors.bone..style = PaintingStyle.stroke..strokeWidth = 1.6);
      canvas.drawCircle(Offset(wx, -4.5), 1.2, accent);
    }
    canvas.restore();
  }

  // ── 3. Noir Spotlight Ghost (Horror / Thriller / Mystery) ─────────────────
  void _paintNoirScene(Canvas canvas) {
    const gY = _VignetteState.groundY;
    _drawHorizon(canvas, ticks: false);

    // Sweeping warm lamp spotlight from top center
    final spotX = 175.0 + math.sin(state.time * 1.45) * 95.0;
    final cone = Path()
      ..moveTo(175, 4)
      ..lineTo(spotX - 38, gY)
      ..lineTo(spotX + 38, gY)
      ..close();
    canvas.drawPath(
      cone,
      Paint()
        ..shader = LinearGradient(
          begin: Alignment.topCenter,
          end: Alignment.bottomCenter,
          colors: [lamp.withValues(alpha: 0.24), lamp.withValues(alpha: 0.06), lamp.withValues(alpha: 0.0)],
        ).createShader(const Rect.fromLTWH(60, 4, 240, gY)),
    );
    // Ground pool of light
    canvas.drawOval(
      Rect.fromCenter(center: Offset(spotX, gY), width: 76, height: 6),
      Paint()..color = lamp.withValues(alpha: 0.20),
    );
    // Tiny hanging lamp bulb at top
    canvas.drawCircle(const Offset(175, 6), 3.2, Paint()..color = lamp.withValues(alpha: 0.85));

    // Cheeky little minimalist ghost tiptoeing & ducking when caught in the beam
    final gx = state.ghostX;
    final hide = state.ghostHide;
    final bob = (1.0 - hide) * math.sin(state.time * 10.0) * 2.2;
    final ghostH = 24.0 - hide * 9.0;
    final ghostW = 18.0 + hide * 4.5;
    final gy = gY - 2.0 - bob;

    canvas.save();
    canvas.translate(gx, gy);
    final body = Path()
      ..moveTo(-ghostW / 2, 0)
      ..lineTo(-ghostW / 2, -ghostH * 0.55)
      ..arcToPoint(Offset(ghostW / 2, -ghostH * 0.55), radius: Radius.circular(ghostW / 2))
      ..lineTo(ghostW / 2, 0)
      ..quadraticBezierTo(ghostW * 0.25, -3.5, 0, 0)
      ..quadraticBezierTo(-ghostW * 0.25, -3.5, -ghostW / 2, 0)
      ..close();
    canvas.drawPath(body, Paint()..color = VeyloColors.bone.withValues(alpha: 0.92 - hide * 0.22));

    // Expressive eyes (wide surprised circles when spotlight hits!)
    final eyeY = -ghostH * 0.58;
    final eyeR = 1.4 + hide * 0.8;
    final dark = Paint()..color = const Color(0xFF050506);
    if (state.blink > 0.35 && hide < 0.3) {
      final lp = Paint()..color = const Color(0xFF050506)..strokeWidth = 1.3..strokeCap = StrokeCap.round;
      canvas.drawLine(Offset(-4.2, eyeY), Offset(-1.8, eyeY), lp);
      canvas.drawLine(Offset(1.8, eyeY), Offset(4.2, eyeY), lp);
    } else {
      canvas.drawCircle(Offset(-3.0, eyeY), eyeR, dark);
      canvas.drawCircle(Offset(3.0, eyeY), eyeR, dark);
    }
    canvas.restore();
  }

  // ── 4. Cosmic Voyager (Sci-Fi / Fantasy) ──────────────────────────────────
  void _paintOrbitScene(Canvas canvas) {
    const w = _VignetteState.w;
    // Twinkling stars gliding left
    const stars = [
      (34.0, 24.0, 1.2), (92.0, 82.0, 1.0), (156.0, 20.0, 1.4),
      (218.0, 92.0, 1.1), (264.0, 34.0, 1.3), (308.0, 78.0, 1.0),
    ];
    for (int i = 0; i < stars.length; i++) {
      final (bx, by, r) = stars[i];
      final x = ((bx - state.time * (18 + i * 4)) % w + w) % w;
      final fade = (math.min(x, w - x) / 36.0).clamp(0.0, 1.0);
      final tw = 0.25 + 0.5 * (0.5 + 0.5 * math.sin(state.time * 3.0 + i));
      canvas.drawCircle(Offset(x, by), r, Paint()..color = VeyloColors.bone.withValues(alpha: tw * fade));
    }

    // Ringed planet drifting gently on the right
    final px = ((270.0 - state.time * 24.0) % (w + 90) + (w + 90)) % (w + 90) - 45;
    final py = 40.0 + math.sin(state.time * 0.9) * 4.0;
    final pFade = (math.min(px + 20, w - px + 20) / 48.0).clamp(0.0, 1.0);
    if (pFade > 0.02) {
      canvas.save();
      canvas.translate(px, py);
      canvas.drawCircle(Offset.zero, 9.5, Paint()..color = VeyloColors.bone.withValues(alpha: 0.22 * pFade));
      canvas.rotate(-0.32);
      canvas.drawOval(
        Rect.fromCenter(center: Offset.zero, width: 32, height: 8),
        Paint()..color = lamp.withValues(alpha: 0.55 * pFade)..style = PaintingStyle.stroke..strokeWidth = 1.4,
      );
      canvas.restore();
    }

    _drawSparks(canvas);

    // Sleek minimalist rocket banking gently along a smooth wave
    final ry = 58.0 + math.sin(state.time * 2.4) * 14.0;
    final slope = math.cos(state.time * 2.4) * 0.18;
    canvas.save();
    canvas.translate(148, ry);
    canvas.rotate(slope);
    final bone = Paint()..color = VeyloColors.bone;
    final accent = Paint()..color = lamp;
    // Thruster flame
    final flameLen = 7 + math.sin(state.time * 34.0).abs() * 5;
    final flame = Path()
      ..moveTo(-14, -3.5)
      ..lineTo(-14 - flameLen, 0)
      ..lineTo(-14, 3.5)
      ..close();
    canvas.drawPath(flame, Paint()..color = lamp.withValues(alpha: 0.85));
    // Rocket fuselage
    final body = Path()
      ..moveTo(-13, -5.5)
      ..quadraticBezierTo(4, -7.5, 16, 0)
      ..quadraticBezierTo(4, 7.5, -13, 5.5)
      ..close();
    canvas.drawPath(body, bone);
    // Fins
    final topFin = Path()..moveTo(-12, -5)..lineTo(-17, -10)..lineTo(-7, -5)..close();
    final botFin = Path()..moveTo(-12, 5)..lineTo(-17, 10)..lineTo(-7, 5)..close();
    canvas.drawPath(topFin, accent);
    canvas.drawPath(botFin, accent);
    // Porthole window
    canvas.drawCircle(const Offset(1, 0), 2.8, Paint()..color = const Color(0xFF050506));
    canvas.drawCircle(const Offset(1, 0), 1.3, accent);
    canvas.restore();
  }

  // ── 5. Vintage Projector (Drama / Romance / Music) ────────────────────────
  void _paintProjectorScene(Canvas canvas) {
    const gY = _VignetteState.groundY;
    _drawHorizon(canvas, ticks: false);

    // Softly flickering cinema light cone from projector lens
    final flicker = 0.88 + 0.12 * math.sin(state.time * 28.0);
    final beam = Path()
      ..moveTo(124, 62)
      ..lineTo(312, 22)
      ..lineTo(312, 90)
      ..lineTo(124, 68)
      ..close();
    canvas.drawPath(
      beam,
      Paint()
        ..shader = LinearGradient(
          colors: [lamp.withValues(alpha: 0.30 * flicker), lamp.withValues(alpha: 0.0)],
        ).createShader(const Rect.fromLTWH(124, 22, 188, 68)),
    );

    _drawSparks(canvas);

    // Minimalist projector silhouette on left
    final bone = Paint()..color = VeyloColors.bone;
    final stroke = Paint()..color = VeyloColors.bone..style = PaintingStyle.stroke..strokeWidth = 1.6;
    final accent = Paint()..color = lamp;

    // Legs
    canvas.drawLine(const Offset(94, 78), Offset(86, gY), stroke);
    canvas.drawLine(const Offset(110, 78), Offset(118, gY), stroke);
    // Body + lens
    canvas.drawRRect(RRect.fromRectAndRadius(const Rect.fromLTWH(84, 58, 34, 20), const Radius.circular(4.5)), bone);
    final lens = Path()
      ..moveTo(118, 63)
      ..lineTo(126, 60)
      ..lineTo(126, 70)
      ..lineTo(118, 67)
      ..close();
    canvas.drawPath(lens, accent);

    // Twin spinning film reels on top
    for (final (rx, ry, rad, dir) in const [(92.0, 47.0, 10.5, 1.0), (112.0, 49.0, 8.5, -1.25)]) {
      canvas.save();
      canvas.translate(rx, ry);
      canvas.rotate(state.time * 3.8 * dir);
      canvas.drawCircle(Offset.zero, rad, stroke);
      canvas.drawCircle(Offset.zero, 2.2, accent);
      for (int i = 0; i < 3; i++) {
        final a = i * (2 * math.pi / 3);
        canvas.drawCircle(Offset(math.cos(a) * (rad * 0.52), math.sin(a) * (rad * 0.52)), 1.5, bone);
      }
      canvas.restore();
    }
  }

  // ── 6. Origami Paper Glider (Documentary / Anime / Default) ───────────────
  void _paintGliderScene(Canvas canvas) {
    const w = _VignetteState.w;
    // Glowing minimalist hoops drifting past
    for (int i = 0; i < 2; i++) {
      final rx = ((260.0 + i * 175.0 - state.time * 90.0) % (w + 60) + (w + 60)) % (w + 60) - 30;
      final ry = 56.0 + math.sin((rx / 120.0) + state.time * 0.6) * 14.0;
      final fade = (math.min(rx + 15, w - rx + 15) / 45.0).clamp(0.0, 1.0);
      if (fade > 0.02) {
        canvas.drawOval(
          Rect.fromCenter(center: Offset(rx, ry), width: 10, height: 28),
          Paint()
            ..color = lamp.withValues(alpha: 0.45 * fade)
            ..style = PaintingStyle.stroke
            ..strokeWidth = 1.6,
        );
      }
    }

    _drawSparks(canvas);

    // Sleek origami paper plane swooping along sinusoidal air current
    final py = 56.0 + math.sin(state.time * 2.6) * 18.0;
    final slope = math.cos(state.time * 2.6) * 0.24;
    canvas.save();
    canvas.translate(154, py);
    canvas.rotate(slope);
    final topWing = Path()
      ..moveTo(-14, -7)
      ..lineTo(16, 0)
      ..lineTo(-10, 2)
      ..close();
    final fold = Path()
      ..moveTo(-10, 2)
      ..lineTo(16, 0)
      ..lineTo(-8, 7.5)
      ..close();
    canvas.drawPath(topWing, Paint()..color = VeyloColors.bone);
    canvas.drawPath(fold, Paint()..color = lamp.withValues(alpha: 0.82));
    canvas.restore();
  }

  // ── 7. Vintage Film Countdown Leader (Trailers & Previews) ───────────────
  void _paintCountdownScene(Canvas canvas) {
    const cx = 170.0;
    const cy = 59.0;
    const r = 34.0;
    final sweep = (state.time % 1.15) / 1.15;
    final sweepAngle = sweep * math.pi * 2;

    // Subtle film sprocket rails flanking the leader
    final railPaint = Paint()
      ..color = const Color.fromRGBO(242, 239, 234, 0.14)
      ..strokeWidth = 1.2;
    canvas.drawLine(const Offset(56, 20), const Offset(56, 98), railPaint);
    canvas.drawLine(const Offset(284, 20), const Offset(284, 98), railPaint);
    final sprocketOff = (state.time * 42.0) % 18.0;
    final sprocketFill = Paint()..color = const Color.fromRGBO(242, 239, 234, 0.22);
    for (double y = 16 + sprocketOff; y < 98; y += 18) {
      canvas.drawRRect(RRect.fromRectAndRadius(Rect.fromLTWH(44, y, 7, 10), const Radius.circular(2)), sprocketFill);
      canvas.drawRRect(RRect.fromRectAndRadius(Rect.fromLTWH(289, y, 7, 10), const Radius.circular(2)), sprocketFill);
    }

    // Soft radar sweep wedge
    canvas.drawArc(
      Rect.fromCircle(center: const Offset(cx, cy), radius: r),
      -math.pi / 2,
      sweepAngle,
      true,
      Paint()..color = lamp.withValues(alpha: 0.16),
    );

    // Outer & inner hairline target circles + crosshairs
    final ring = Paint()
      ..color = const Color.fromRGBO(242, 239, 234, 0.34)
      ..style = PaintingStyle.stroke
      ..strokeWidth = 1.3;
    canvas.drawCircle(const Offset(cx, cy), r, ring);
    canvas.drawCircle(const Offset(cx, cy), r * 0.72, Paint()..color = const Color.fromRGBO(242, 239, 234, 0.16)..style = PaintingStyle.stroke..strokeWidth = 1.0);
    canvas.drawLine(const Offset(cx - r - 8, cy), const Offset(cx + r + 8, cy), Paint()..color = const Color.fromRGBO(242, 239, 234, 0.16)..strokeWidth = 1.0);
    canvas.drawLine(const Offset(cx, cy - r - 8), const Offset(cx, cy + r + 8), Paint()..color = const Color.fromRGBO(242, 239, 234, 0.16)..strokeWidth = 1.0);

    // Sweeping lamp needle
    final nx = cx + math.cos(-math.pi / 2 + sweepAngle) * r;
    final ny = cy + math.sin(-math.pi / 2 + sweepAngle) * r;
    canvas.drawLine(
      const Offset(cx, cy),
      Offset(nx, ny),
      Paint()
        ..color = lamp
        ..strokeWidth = 1.8
        ..strokeCap = StrokeCap.round,
    );

    // Center pulsing clapper / aperture mark
    final clap = sweep < 0.16 ? (1.0 - sweep / 0.16) : 0.0;
    canvas.save();
    canvas.translate(cx, cy);
    canvas.drawRRect(
      RRect.fromRectAndRadius(const Rect.fromLTWH(-10, -4, 20, 12), const Radius.circular(2.5)),
      Paint()..color = VeyloColors.bone,
    );
    canvas.save();
    canvas.translate(-10, -4);
    canvas.rotate(-0.42 * (1.0 - clap));
    canvas.drawRRect(
      RRect.fromRectAndRadius(const Rect.fromLTWH(0, -4.5, 20, 4.5), const Radius.circular(1.5)),
      Paint()..color = lamp,
    );
    canvas.restore();
    canvas.restore();

    _drawSparks(canvas);
  }

  @override
  bool shouldRepaint(covariant _VignettePainter oldDelegate) => oldDelegate.lamp != lamp;
}

/// Delicate radial lamp spark micro-burst around an action orb (e.g. Like / Save in Picks).
/// Zero animation cost when idle; fires a brief 420ms burst whenever [trigger] increments.
class LampSparkBurst extends StatefulWidget {
  const LampSparkBurst({
    super.key,
    required this.trigger,
    required this.lamp,
    required this.child,
  });

  final int trigger;
  final Color lamp;
  final Widget child;

  @override
  State<LampSparkBurst> createState() => _LampSparkBurstState();
}

class _LampSparkBurstState extends State<LampSparkBurst> with SingleTickerProviderStateMixin {
  late final AnimationController _ctrl = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 420),
  );

  @override
  void didUpdateWidget(covariant LampSparkBurst oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (widget.trigger > oldWidget.trigger) {
      _ctrl.forward(from: 0);
    }
  }

  @override
  void dispose() {
    _ctrl.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return CustomPaint(
      foregroundPainter: _LampSparkPainter(progress: _ctrl, lamp: widget.lamp),
      child: widget.child,
    );
  }
}

class _LampSparkPainter extends CustomPainter {
  _LampSparkPainter({required this.progress, required this.lamp}) : super(repaint: progress);

  final Animation<double> progress;
  final Color lamp;

  @override
  void paint(Canvas canvas, Size size) {
    final t = progress.value;
    if (t <= 0.0 || t >= 1.0) return;
    final eased = Curves.easeOutCubic.transform(t);
    final fade = (1.0 - t).clamp(0.0, 1.0);
    final center = Offset(size.width / 2, size.height / 2);
    final baseR = math.min(size.width, size.height) * 0.45;

    // Faint expanding lamp ring
    canvas.drawCircle(
      center,
      baseR + eased * 14,
      Paint()
        ..color = lamp.withValues(alpha: 0.42 * fade)
        ..style = PaintingStyle.stroke
        ..strokeWidth = 1.6 * fade,
    );

    // 7 tiny sparks shooting outward
    for (int i = 0; i < 7; i++) {
      final angle = (i * (2 * math.pi / 7)) - math.pi / 2 + 0.18;
      final dist = baseR + 4 + eased * (i.isEven ? 18.0 : 13.0);
      final pos = Offset(center.dx + math.cos(angle) * dist, center.dy + math.sin(angle) * dist);
      canvas.drawCircle(
        pos,
        (i.isEven ? 2.4 : 1.7) * fade,
        Paint()..color = (i.isEven ? lamp : VeyloColors.bone).withValues(alpha: 0.92 * fade),
      );
    }
  }

  @override
  bool shouldRepaint(covariant _LampSparkPainter oldDelegate) => oldDelegate.lamp != lamp;
}

/// Traces a smooth glowing [lamp] countdown arc around a rounded pill (used on Player Up Next).
class LampCountdownBorder extends StatelessWidget {
  const LampCountdownBorder({
    super.key,
    required this.progress,
    required this.lamp,
    required this.child,
    this.radius = 999,
  });

  /// 1.0 = full countdown remaining, 0.0 = about to trigger.
  final double progress;
  final Color lamp;
  final double radius;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    return CustomPaint(
      foregroundPainter: _LampCountdownBorderPainter(
        progress: progress.clamp(0.0, 1.0),
        lamp: lamp,
        radius: radius,
      ),
      child: child,
    );
  }
}

class _LampCountdownBorderPainter extends CustomPainter {
  const _LampCountdownBorderPainter({
    required this.progress,
    required this.lamp,
    required this.radius,
  });

  final double progress;
  final Color lamp;
  final double radius;

  @override
  void paint(Canvas canvas, Size size) {
    if (progress <= 0.01) return;
    final r = math.min(radius, size.height / 2);
    final rrect = RRect.fromRectAndRadius(Offset.zero & size, Radius.circular(r));
    final path = Path()..addRRect(rrect.deflate(1.0));
    final metrics = path.computeMetrics().toList();
    if (metrics.isEmpty) return;
    final metric = metrics.first;
    final sub = metric.extractPath(0, metric.length * progress);
    canvas.drawPath(
      sub,
      Paint()
        ..color = lamp.withValues(alpha: 0.92)
        ..style = PaintingStyle.stroke
        ..strokeWidth = 2.0
        ..strokeCap = StrokeCap.round,
    );
  }

  @override
  bool shouldRepaint(covariant _LampCountdownBorderPainter old) =>
      old.progress != progress || old.lamp != lamp || old.radius != radius;
}

/// Minimal breathing crescent moon + drifting star icon for the Player's sleep timer pill.
class BreathingMoonIcon extends StatefulWidget {
  const BreathingMoonIcon({super.key, required this.lamp, this.size = 16});

  final Color lamp;
  final double size;

  @override
  State<BreathingMoonIcon> createState() => _BreathingMoonIconState();
}

class _BreathingMoonIconState extends State<BreathingMoonIcon> with SingleTickerProviderStateMixin {
  late final AnimationController _ctrl = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 2800),
  )..repeat();

  @override
  void dispose() {
    _ctrl.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return SizedBox(
      width: widget.size,
      height: widget.size,
      child: CustomPaint(
        painter: _BreathingMoonPainter(anim: _ctrl, lamp: widget.lamp),
      ),
    );
  }
}

class _BreathingMoonPainter extends CustomPainter {
  _BreathingMoonPainter({required this.anim, required this.lamp}) : super(repaint: anim);

  final Animation<double> anim;
  final Color lamp;

  @override
  void paint(Canvas canvas, Size size) {
    final t = anim.value * math.pi * 2;
    final breathe = 0.84 + 0.16 * math.sin(t);
    final sx = size.width / 16.0;
    final sy = size.height / 16.0;
    canvas.save();
    canvas.scale(sx, sy);

    // Subtle crescent moon built from two intersecting circles via Path.combine
    final outer = Path()..addOval(Rect.fromCircle(center: const Offset(7.5, 8.5), radius: 5.5));
    final cutout = Path()..addOval(Rect.fromCircle(center: const Offset(10.0, 6.2), radius: 4.8));
    final moon = Path.combine(PathOperation.difference, outer, cutout);
    canvas.drawPath(moon, Paint()..color = VeyloColors.bone.withValues(alpha: breathe));

    // Tiny gently twinkling lamp star beside the crescent
    final starY = 4.2 + math.sin(t) * 0.9;
    final starA = 0.45 + 0.55 * (0.5 + 0.5 * math.sin(t + 1.1));
    canvas.drawCircle(Offset(12.2, starY), 1.35, Paint()..color = lamp.withValues(alpha: starA));

    canvas.restore();
  }

  @override
  bool shouldRepaint(covariant _BreathingMoonPainter old) => old.lamp != lamp;
}
