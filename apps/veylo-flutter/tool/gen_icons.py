#!/usr/bin/env python3
"""Generate exact Flutter vector paths from apps/veylo-dx/src/icons.rs."""
import re, pathlib

ROOT = pathlib.Path(__file__).resolve().parents[3]
ICONS_RS = ROOT / "apps" / "veylo-dx" / "src" / "icons.rs"
OUT_DART = ROOT / "apps" / "veylo-flutter" / "lib" / "widgets" / "veylo_icons.g.dart"

src = ICONS_RS.read_text(encoding="utf-8")

line_re = re.compile(r'^#\[component\]\s*pub fn (\w+)\(\)\s*->\s*Element\s*\{\s*rsx!\s*\{\s*Glyph\s*\{(.*)\}\s*\}\s*\}\s*$')
elem_re = re.compile(r'\b(path|circle|rect)\s*\{([^}]*)\}')
attr_re = re.compile(r'(\w+)\s*:\s*"([^"]*)"')

icons = {}
fillable = []

for line in src.splitlines():
    m = line_re.match(line.strip())
    if not m:
        continue
    name, body = m.group(1), m.group(2)
    if re.search(r'\bfillable\s*:\s*true\b', body):
        fillable.append(name)
    prims = []
    for em in elem_re.finditer(body):
        kind, attrs_str = em.group(1), em.group(2)
        attrs = dict(attr_re.findall(attrs_str))
        if kind == "path":
            d = attrs["d"]
            prims.append(f"VeyloIconPath('{d}')")
        elif kind == "circle":
            cx, cy, r = float(attrs["cx"]), float(attrs["cy"]), float(attrs["r"])
            prims.append(f"VeyloIconCircle({cx}, {cy}, {r})")
        elif kind == "rect":
            x = float(attrs.get("x", "0"))
            y = float(attrs.get("y", "0"))
            w = float(attrs["width"])
            h = float(attrs["height"])
            rx = float(attrs.get("rx", "0"))
            ry = float(attrs.get("ry", str(rx)))
            prims.append(f"VeyloIconRect({x}, {y}, {w}, {h}, {rx}, {ry})")
        else:
            raise ValueError(f"Unsupported SVG primitive: {kind}")
    assert prims, f"No primitives parsed for icon {name}"
    icons[name] = prims

assert len(icons) >= 60, f"Expected >= 60 icons, got {len(icons)}"

lines = [
    "// GENERATED CODE - DO NOT MODIFY BY HAND.",
    "// Generated from apps/veylo-dx/src/icons.rs by tool/gen_icons.py.",
    "",
    "sealed class VeyloIconPrim {",
    "  const VeyloIconPrim();",
    "}",
    "",
    "final class VeyloIconPath extends VeyloIconPrim {",
    "  const VeyloIconPath(this.d);",
    "  final String d;",
    "}",
    "",
    "final class VeyloIconCircle extends VeyloIconPrim {",
    "  const VeyloIconCircle(this.cx, this.cy, this.r);",
    "  final double cx;",
    "  final double cy;",
    "  final double r;",
    "}",
    "",
    "final class VeyloIconRect extends VeyloIconPrim {",
    "  const VeyloIconRect(this.x, this.y, this.w, this.h, this.rx, this.ry);",
    "  final double x;",
    "  final double y;",
    "  final double w;",
    "  final double h;",
    "  final double rx;",
    "  final double ry;",
    "}",
    "",
    "const Set<String> kVeyloFillableIcons = {",
]
for n in fillable:
    lines.append(f"  '{n}',")
lines.append("};")
lines.append("")
lines.append("const Map<String, List<VeyloIconPrim>> kVeyloIcons = {")
for name, prims in icons.items():
    joined = ", ".join(prims)
    lines.append(f"  '{name}': [{joined}],")
lines.append("};")
lines.append("")

with open(OUT_DART, "w", encoding="utf-8") as f:
    f.write("\n".join(lines))

print(f"Generated {len(icons)} icons ({len(fillable)} fillable) -> {OUT_DART}")
