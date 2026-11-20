"""Builds the embedded UI font subsets from the Inter 4.1 release (https://github.com/rsms/inter, SIL OFL 1.1),
whose fonts carry the font version 4.001 (name record "Version 4.001;git-9221beed3", kept in the subsets).

Usage (fontTools 4.x):
    python subset.py <Inter-4.1/extras/ttf> <output dir>

For each of Inter-Regular.ttf and Inter-SemiBold.ttf: the digits 0-9 are mapped to Inter's tabular forms
(`zero.tf` ... `nine.tf`, the `tnum` feature, which Dear ImGui's stb_truetype loader cannot apply), so live
numbers do not jitter; then the font is cut to Basic Latin, Latin-1 and a few punctuation and symbol code points,
without hinting and without layout tables (Dear ImGui uses neither). The copyright, licence and name records are
kept. The result is a Modified Version under the OFL (Inter declares no Reserved Font Name).
"""

import sys
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont

UNICODES = (
    list(range(0x20, 0x7F))
    + list(range(0xA0, 0x100))
    + [0x2013, 0x2014, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2026, 0x2032, 0x2033]
    + [0x2190, 0x2191, 0x2192, 0x2193, 0x2212, 0x2248, 0x2264, 0x2265, 0x221E, 0x00B5, 0x2009, 0x202F]
    + [0x25B4, 0x25BE, 0x25B8, 0x25C2, 0x2715, 0x2713]
)

DIGITS = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine"]


def build(src: Path, dst: Path) -> None:
    font = TTFont(src)
    glyphs = set(font.getGlyphOrder())
    for table in font["cmap"].tables:
        if not table.isUnicode():
            continue
        for i, name in enumerate(DIGITS):
            tf = f"{name}.tf"
            if tf in glyphs and (0x30 + i) in table.cmap:
                table.cmap[0x30 + i] = tf
    opts = subset.Options()
    opts.layout_features = []
    opts.hinting = False
    opts.name_IDs = ["*"]
    opts.name_languages = ["*"]
    opts.notdef_outline = True
    opts.glyph_names = False
    opts.drop_tables += ["GSUB", "GPOS", "GDEF", "STAT", "DSIG"]
    sub = subset.Subsetter(opts)
    sub.populate(unicodes=[u for u in UNICODES])
    sub.subset(font)
    font.save(dst)


def main() -> None:
    src, out = Path(sys.argv[1]), Path(sys.argv[2])
    for name in ["Inter-Regular.ttf", "Inter-SemiBold.ttf"]:
        build(src / name, out / name)
        print(name, (out / name).stat().st_size)


if __name__ == "__main__":
    main()
