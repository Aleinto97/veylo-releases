#!/usr/bin/env python3
"""Veylo's display face: Archivo at width 82 (veylo-dx sets `font-variation-settings: 'wdth' 82`
in CSS; Slint cannot set variation axes, so the width is pinned in static instances).

Writes apps/veylo-slint/ui/fonts/veylo-display-{600,700}.ttf from apps/veylo-dx/assets/fonts/archivo.ttf.
The result is a modified version under the SIL Open Font License 1.1, renamed "Veylo Display".
Requires fontTools: python3 -m pip install fonttools
"""
from pathlib import Path
from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "apps/veylo-dx/assets/fonts/archivo.ttf"
OUT = ROOT / "apps/veylo-slint/ui/fonts"
FAMILY = "Veylo Display"
STYLES = {600: "SemiBold", 700: "Bold"}

OUT.mkdir(parents=True, exist_ok=True)
for weight, style in STYLES.items():
    font = instantiateVariableFont(TTFont(SOURCE), {"wght": weight, "wdth": 82}, updateFontNames=False)
    names = font["name"]
    full = f"{FAMILY} {style}"
    postscript = f"VeyloDisplay-{style}"
    for record in list(names.names):
        if record.nameID in (1, 16): names.setName(FAMILY, record.nameID, record.platformID, record.platEncID, record.langID)
        elif record.nameID in (2, 17): names.setName(style, record.nameID, record.platformID, record.platEncID, record.langID)
        elif record.nameID == 4: names.setName(full, 4, record.platformID, record.platEncID, record.langID)
        elif record.nameID == 6: names.setName(postscript, 6, record.platformID, record.platEncID, record.langID)
        elif record.nameID == 3: names.setName(f"{postscript};instance", 3, record.platformID, record.platEncID, record.langID)
        elif record.nameID in (25,): names.removeNames(nameID=25)
    font["OS/2"].usWeightClass = weight
    font["OS/2"].usWidthClass = 4  # semi-condensed
    font.save(OUT / f"veylo-display-{weight}.ttf")
    print("wrote", OUT / f"veylo-display-{weight}.ttf")
