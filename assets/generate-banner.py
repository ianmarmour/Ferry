"""Generates assets/ferry-banner.svg, the README's banner.

The wordmark and tagline are converted to outlines, so the banner looks the same
wherever it is shown, whatever fonts are installed. Needs fontTools and the
Inter font:

    pip install fonttools
    python3 assets/generate-banner.py /usr/share/fonts/opentype/inter
"""

import re
import sys
from pathlib import Path

from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont

WIDTH, HEIGHT = 1280, 400
HERE = Path(__file__).parent


def outline(font_path, text, size, x, y, tracking=0.0):
    """The outline of `text` set in a font, its baseline starting at (x, y)."""
    font = TTFont(font_path)
    glyphs = font.getGlyphSet()
    cmap = font.getBestCmap()
    scale = size / font["head"].unitsPerEm
    pen = SVGPathPen(glyphs)
    for char in text:
        name = cmap[ord(char)]
        glyphs[name].draw(TransformPen(pen, (scale, 0, 0, -scale, x, y)))
        x += glyphs[name].width * scale + tracking
    return pen.getCommands(), x


def icon(x, y, size):
    """The app icon, from assets/ferry.svg, placed at (x, y)."""
    svg = (HERE / "ferry.svg").read_text()
    body = svg[svg.index("<defs>") : svg.rindex("</svg>")]
    body = re.sub(r"\s+", " ", body.replace('"green"', '"icon"').replace("#green", "#icon"))
    return f'<g transform="translate({x} {y}) scale({size / 256})">{body.strip()}</g>'


def main():
    fonts = Path(sys.argv[1] if len(sys.argv) > 1 else "/usr/share/fonts/opentype/inter")
    name, _ = outline(fonts / "Inter-Bold.otf", "Ferry", 132, 448, 214, tracking=-2.5)
    tagline, _ = outline(fonts / "Inter-Medium.otf", "Your iPhone's messages, on your computer.", 34, 454, 276)

    banner = f"""<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{HEIGHT}" viewBox="0 0 {WIDTH} {HEIGHT}" role="img" aria-labelledby="title">
  <title id="title">Ferry: your iPhone's messages, on your computer</title>
  <defs>
    <linearGradient id="background" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#0A7E86"/>
      <stop offset="1" stop-color="#05424D"/>
    </linearGradient>
    <filter id="shadow" x="-20%" y="-20%" width="140%" height="150%">
      <feDropShadow dx="0" dy="10" stdDeviation="14" flood-color="#012A30" flood-opacity="0.45"/>
    </filter>
  </defs>
  <rect width="{WIDTH}" height="{HEIGHT}" rx="32" fill="url(#background)"/>
  <g filter="url(#shadow)">{icon(150, 76, 248)}</g>
  <path fill="#FFFFFF" d="{name}"/>
  <path fill="#BFF1E1" d="{tagline}"/>
</svg>
"""
    (HERE / "ferry-banner.svg").write_text(banner)


if __name__ == "__main__":
    main()
