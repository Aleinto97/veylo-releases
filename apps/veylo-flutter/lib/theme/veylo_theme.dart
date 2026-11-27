import 'package:flutter/widgets.dart';

/// Veylo's exact design tokens from apps/veylo-dx/src/styles/base.css.
class VeyloColors {
  static const Color screen = Color(0xFF000000);
  static const Color seat = Color(0xFF141518);
  static const Color bone = Color(0xFFF2EFEA);
  static const Color dust = Color(0xFFA4A4AC);
  static const Color amber = Color(0xFFE8B23A);
  static const Color defaultLamp = Color(0xFFD8D4CE);
  static const Color synopsis = Color(0xFFD2D0CC);
  static const Color titleSynopsis = Color(0xFFDCDAD6);

  static const Color glass = Color.fromRGBO(24, 25, 30, 0.50);
  static const Color glassStrong = Color.fromRGBO(18, 19, 23, 0.82);
  static const Color surface = Color.fromRGBO(24, 25, 30, 0.72);
  static const Color surfaceHover = Color.fromRGBO(34, 36, 43, 0.82);
  static const Color divider = Color.fromRGBO(255, 255, 255, 0.08);
  static const Color edge = Color.fromRGBO(255, 255, 255, 0.13);
  static const Color sheen = Color.fromRGBO(255, 255, 255, 0.22);

  static const Color hwGreen = Color(0xFF7BE0A6);
  static const Color hwGreenBg = Color.fromRGBO(40, 199, 111, 0.16);
  static const Color swAmberBg = Color.fromRGBO(232, 178, 58, 0.14);
  static const Color dangerRed = Color.fromRGBO(200, 48, 48, 0.94);
  static const Color dangerText = Color(0xFFFFB4AB);

  /// Equivalent to CSS `color-mix(in srgb, lamp (pct*100)%, base)`.
  static Color mixLamp(Color lamp, Color base, double lampFraction) {
    final a = lamp.a * lampFraction + base.a * (1.0 - lampFraction);
    if (a <= 0.0001) return const Color(0x00000000);
    final r = (lamp.r * lamp.a * lampFraction + base.r * base.a * (1.0 - lampFraction)) / a;
    final g = (lamp.g * lamp.a * lampFraction + base.g * base.a * (1.0 - lampFraction)) / a;
    final b = (lamp.b * lamp.a * lampFraction + base.b * base.a * (1.0 - lampFraction)) / a;
    return Color.from(
      alpha: a.clamp(0.0, 1.0),
      red: r.clamp(0.0, 1.0),
      green: g.clamp(0.0, 1.0),
      blue: b.clamp(0.0, 1.0),
    );
  }

  static Color parseHex(String? hex, {Color fallback = defaultLamp}) {
    if (hex == null || hex.isEmpty) return fallback;
    final clean = hex.trim().replaceFirst('#', '');
    if (clean.length == 6) {
      final v = int.tryParse(clean, radix: 16);
      if (v != null) return Color(0xFF000000 | v);
    }
    return fallback;
  }
}

class VeyloLayout {
  static double gutter(double width) {
    if (width <= 760) return 20;
    if (width <= 899) return 40;
    return 48;
  }

  static double posterWidth(double width) {
    if (width <= 999) return 140;
    return 156;
  }
}

/// Variable-font-aware text styles: Outfit.ttf and Inter.ttf are variable fonts (`fvar`),
/// so Flutter needs `FontVariation('wght', w)` to render exact CSS weights (400..800, incl. 650/750).
class VeyloText {
  static const String displayFont = 'Outfit';
  static const String uiFont = 'Inter';
  static const List<FontFeature> tnum = [FontFeature.tabularFigures()];
  static const List<FontFeature> uiFeatures = [
    FontFeature('cv02'),
    FontFeature('cv03'),
    FontFeature('cv04'),
    FontFeature('cv11'),
  ];
  static const List<FontFeature> uiTnum = [
    FontFeature('cv02'),
    FontFeature('cv03'),
    FontFeature('cv04'),
    FontFeature('cv11'),
    FontFeature.tabularFigures(),
  ];

  static FontWeight _nearestWeight(double w) {
    if (w >= 750) return FontWeight.w800;
    if (w >= 650) return FontWeight.w700;
    if (w >= 550) return FontWeight.w600;
    if (w >= 450) return FontWeight.w500;
    return FontWeight.w400;
  }

  static TextStyle display(
    double size, {
    double weight = 800,
    double? height,
    double? letterSpacing,
    Color color = VeyloColors.bone,
    List<Shadow>? shadows,
    bool tabular = false,
  }) =>
      TextStyle(
        fontFamily: displayFont,
        fontSize: size,
        height: height,
        fontWeight: _nearestWeight(weight),
        fontVariations: [FontVariation('wght', weight)],
        letterSpacing: letterSpacing ?? (-0.02 * size),
        color: color,
        shadows: shadows,
        fontFeatures: tabular ? tnum : null,
        decoration: TextDecoration.none,
      );

  static TextStyle ui(
    double size, {
    double weight = 500,
    double? height,
    double? letterSpacing,
    Color color = VeyloColors.bone,
    FontStyle? fontStyle,
    List<Shadow>? shadows,
    bool tabular = false,
    TextDecoration decoration = TextDecoration.none,
    Color? decorationColor,
  }) =>
      TextStyle(
        fontFamily: uiFont,
        fontSize: size,
        height: height,
        fontWeight: _nearestWeight(weight),
        fontVariations: [FontVariation('wght', weight)],
        letterSpacing: letterSpacing,
        color: color,
        fontStyle: fontStyle,
        shadows: shadows,
        fontFeatures: tabular ? uiTnum : uiFeatures,
        decoration: decoration,
        decorationColor: decorationColor,
      );

  static const TextStyle body = TextStyle(
    fontFamily: uiFont,
    fontSize: 15,
    height: 1.45,
    fontWeight: FontWeight.w500,
    fontVariations: [FontVariation('wght', 500)],
    color: VeyloColors.bone,
    fontFeatures: uiFeatures,
    decoration: TextDecoration.none,
  );

  static const TextStyle wordmark = TextStyle(
    fontFamily: displayFont,
    fontSize: 23,
    fontWeight: FontWeight.w800,
    fontVariations: [FontVariation('wght', 800)],
    letterSpacing: -0.805,
    color: VeyloColors.bone,
    decoration: TextDecoration.none,
  );

  static const TextStyle title = TextStyle(
    fontFamily: displayFont,
    fontSize: 32,
    height: 1.06,
    fontWeight: FontWeight.w800,
    fontVariations: [FontVariation('wght', 800)],
    letterSpacing: -0.96,
    color: VeyloColors.bone,
    decoration: TextDecoration.none,
  );

  static const TextStyle section = TextStyle(
    fontFamily: displayFont,
    fontSize: 20,
    height: 1.2,
    fontWeight: FontWeight.w700,
    fontVariations: [FontVariation('wght', 700)],
    letterSpacing: -0.4,
    color: VeyloColors.bone,
    decoration: TextDecoration.none,
  );

  static const TextStyle groupName = TextStyle(
    fontFamily: displayFont,
    fontSize: 19.5,
    height: 1.1,
    fontWeight: FontWeight.w700,
    fontVariations: [FontVariation('wght', 700)],
    letterSpacing: -0.39,
    color: VeyloColors.bone,
    decoration: TextDecoration.none,
  );

  static const TextStyle meta = TextStyle(
    fontFamily: uiFont,
    fontSize: 14.5,
    height: 1.4,
    fontWeight: FontWeight.w500,
    fontVariations: [FontVariation('wght', 500)],
    color: VeyloColors.bone,
    fontFeatures: uiFeatures,
    decoration: TextDecoration.none,
  );

  static const TextStyle micro = TextStyle(
    fontFamily: uiFont,
    fontSize: 14,
    height: 1.43,
    fontWeight: FontWeight.w600,
    fontVariations: [FontVariation('wght', 600)],
    color: VeyloColors.dust,
    fontFeatures: uiFeatures,
    decoration: TextDecoration.none,
  );

  static const TextStyle bodyStrong = TextStyle(
    fontFamily: uiFont,
    fontSize: 16.5,
    height: 1.39,
    fontWeight: FontWeight.w600,
    fontVariations: [FontVariation('wght', 650)],
    color: VeyloColors.bone,
    fontFeatures: uiFeatures,
    decoration: TextDecoration.none,
  );

  static const TextStyle credit = TextStyle(
    fontFamily: uiFont,
    fontSize: 13,
    fontWeight: FontWeight.w500,
    fontVariations: [FontVariation('wght', 500)],
    color: VeyloColors.dust,
    fontFeatures: uiFeatures,
    decoration: TextDecoration.none,
  );
}

class VeyloCurves {
  static const Cubic out = Cubic(0.22, 1.0, 0.36, 1.0);
  static const Cubic spring = Cubic(0.34, 1.4, 0.64, 1.0);
  static const Cubic slider = Cubic(0.32, 1.15, 0.48, 1.0);
}
