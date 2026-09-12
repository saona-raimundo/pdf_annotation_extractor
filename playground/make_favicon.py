#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["pillow>=11"]
# ///
# 
# Run by
# > uv run make_favicon.ico
#
"""Draw assets/favicon.ico.

Checked in as a script rather than left as an opaque binary: the icon is
geometry, and geometry belongs in a file that can be diffed. Run it after
changing a colour or proportion.

    uv run playground/make_favicon.py

Each size is drawn at its own resolution rather than downsampled from one
large image. A 16-pixel favicon has nine pixels of page to work with, and
resampling turns a one-pixel rule into two rows of grey.

Lives beside assets/ rather than inside it because trunk copies that whole
directory into the bundle, and a build tool is not an asset.
"""

from pathlib import Path

from PIL import Image, ImageDraw

# Sizes a browser or an OS may ask for. 256 is what Windows uses for a
# pinned-site tile; 16 is the tab strip, and the one that has to survive.
SIZES = (16, 32, 48, 64, 128, 256)

BACKDROP = (33, 36, 41, 255)  # dark, so the mark holds on a pale tab strip
PAGE = (250, 250, 247, 255)
RULE = (150, 152, 156, 255)
HIGHLIGHT = (242, 194, 48, 255)  # the yellow of a default highlight annotation
MARKED = (74, 60, 20, 255)  # the text under the highlight, dark enough to hold on it

# Fractions of the square, so every size is the same drawing.
PAGE_BOX = (0.22, 0.10, 0.78, 0.90)
RULES = (0.26, 0.40)  # y centres of the plain text lines above the highlight
RULE_BELOW = 0.78
HIGHLIGHT_BAND = (0.52, 0.68)  # top and bottom of the highlighted line
RULE_INSET = 0.06  # margin from the page edge to the end of a text line


def draw(size: int) -> Image.Image:
    image = Image.new("RGBA", (size, size), BACKDROP)
    pen = ImageDraw.Draw(image)

    def px(fraction: float) -> float:
        return fraction * size

    left, top, right, bottom = (px(f) for f in PAGE_BOX)
    pen.rectangle((left, top, right - 1, bottom - 1), fill=PAGE)

    # A rule is at least one pixel tall, or it disappears at 16.
    thickness = max(1, round(size * 0.055))
    inset = px(RULE_INSET)

    for centre in (*RULES, RULE_BELOW):
        y = px(centre)
        pen.rectangle(
            (left + inset, y, right - 1 - inset, y + thickness - 1),
            fill=RULE,
        )

    band_top, band_bottom = (px(f) for f in HIGHLIGHT_BAND)
    # Edge to edge of the page: a highlight that stops short of the margin
    # reads as a third text line at small sizes.
    pen.rectangle((left, band_top, right - 1, band_bottom - 1), fill=HIGHLIGHT)

    # The highlighted line itself, where the band is tall enough to hold one
    # with a row of clear yellow above and below. At 16 pixels the band is
    # three rows and there is no room, which is fine: a solid bar still reads
    # as a highlight, whereas a rule filling it reads as mud.
    if band_bottom - band_top >= thickness + 2:
        y = (band_top + band_bottom - thickness) / 2
        pen.rectangle(
            (left + inset, y, right - 1 - inset, y + thickness - 1),
            fill=MARKED,
        )

    return image


def main() -> None:
    target = Path(__file__).parent / "assets" / "favicon.ico"
    frames = [draw(size) for size in SIZES]
    # `append_images` is what stops Pillow resampling the largest frame down
    # to the other sizes; without it the drawing above is done six times and
    # five of the results are thrown away.
    frames[-1].save(
        target,
        format="ICO",
        sizes=[(s, s) for s in SIZES],
        append_images=frames[:-1],
    )
    print(f"wrote {target} ({target.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
