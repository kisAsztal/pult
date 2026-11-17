#!/usr/bin/env python3
"""Generates the blurred tab background images: colour blobs, circles and the word "pult", as if seen through
frosted glass. WezTerm stretches the image and lays the terminal background colour over it, so a small resolution
is enough (the blur hides the pixels anyway).

    bg.py <dir> <prefix> <count> <mantle> <magenta> <blue> <cyan> <yellow> <orange> <green> <red>

Only the missing images are made (<dir>/<prefix>-<i>.png); image i is always the same (seed = i).
Needs Pillow. The font of the word comes from $PULT_BG_FONT (default: DejaVuSansMono-Bold.ttf).
"""
import os
import random
import sys

from PIL import Image, ImageChops, ImageDraw, ImageEnhance, ImageFilter, ImageFont

W, H = 480, 300  # the blur hides the resolution; a smaller texture loads faster on tab switch
SS = 2  # anti-aliasing: draw at double size


def rgb(h):
    h = h.lstrip('#')
    return tuple(int(h[i:i + 2], 16) for i in (0, 2, 4))


def blob(size, cx, cy, rx, ry, color, strength):
    """Radial blob: colour of the given strength in the middle, transparent at the edge (like a CSS radial-gradient)."""
    g = Image.radial_gradient('L').resize((max(1, int(rx * 2)), max(1, int(ry * 2))))
    mask = Image.new('L', size, 255)
    mask.paste(g, (int(cx - rx), int(cy - ry)))
    mask = mask.point(lambda v: int(max(0, 255 - v * 1.25) * strength))
    return Image.new('RGB', size, color), mask


def calm_mask(w, h):
    """The top left stays calm, where the tab label sits: sharp shapes fade strongly there."""
    m = Image.new('L', (w, h), 0)
    d = ImageDraw.Draw(m)
    d.rectangle((0, 0, int(w * .55), int(h * .6)), fill=255)
    return m.filter(ImageFilter.GaussianBlur(w * .12)).point(lambda v: int(v * .75))


def make(path, seed, pal, dark):
    rnd = random.Random(seed)
    w, h = W * SS, H * SS
    # dark: related cool hues only (warm ones and green turn brown under the veil); 2-3 colours per image, to
    # keep it from getting garish
    pool = ['magenta', 'blue', 'cyan'] if dark else ['magenta', 'blue', 'cyan', 'yellow', 'orange', 'green', 'red']
    names = rnd.sample(pool, min(len(pool), rnd.choice([2, 3])))
    accents = [pal[n] for n in names]
    if dark and rnd.random() < .35:
        accents.append(pal[rnd.choice(['red', 'orange'])])  # sometimes a small warm accent, but only as a sharp shape
    soft = accents[:len(names)]

    # layer 1: large, heavily blurred blobs, mostly right and bottom
    base = Image.new('RGB', (w, h), pal['mantle'])
    for _ in range(3):
        layer, mask = blob((w, h), rnd.uniform(.35, 1.0) * w, rnd.uniform(.2, 1.0) * h, rnd.uniform(.35, .6) * w,
                           rnd.uniform(.45, .8) * h, rnd.choice(soft), rnd.uniform(.2, .34))
        base.paste(layer, (0, 0), mask)
    base = base.filter(ImageFilter.GaussianBlur(w * .03))

    # layer 2: circles and the word, blurred less so the shapes survive. The colour is drawn on a copy of the base
    # image and the coverage into a separate mask, so the blurred edge melts into the base colour, not into black.
    color = base.copy()
    mask = Image.new('L', (w, h), 0)
    cd, md = ImageDraw.Draw(color), ImageDraw.Draw(mask)

    def shape(draw_fn, col, cover=255):
        draw_fn(cd, col)
        draw_fn(md, cover)

    for _ in range(rnd.randint(1, 3)):
        r = rnd.uniform(.1, .2) * w
        x, y = rnd.uniform(.45, 1.05) * w, rnd.uniform(-.05, 1.05) * h
        shape(lambda d, f: d.ellipse((x - r, y - r, x + r, y + r), fill=f), rnd.choice(accents))
    if rnd.random() < .5:
        try:
            # the word always lies completely within the right part of the image
            size = int(rnd.uniform(.18, .26) * w)
            font = ImageFont.truetype(os.environ.get('PULT_BG_FONT', 'DejaVuSansMono-Bold.ttf'), size)
            x0, y0, x1, y1 = cd.textbbox((0, 0), 'pult', font=font)
            if x1 > w * .55:
                font = ImageFont.truetype(font.path, int(size * w * .55 / x1))
                x0, y0, x1, y1 = cd.textbbox((0, 0), 'pult', font=font)
            pos = (rnd.uniform(w * .42, w - x1 - w * .02), rnd.uniform(-y0, h - y1))
            col = rnd.choice(accents)
            shape(lambda d, f: d.text(pos, 'pult', font=font, fill=f), col, 170)
        except OSError:
            pass
    blur = ImageFilter.GaussianBlur(w * .012)
    color, mask = color.filter(blur), mask.filter(blur)
    base.paste(color, (0, 0), ImageChops.subtract(mask, calm_mask(w, h)))

    img = base.resize((W, H), Image.LANCZOS)
    img = ImageEnhance.Color(img).enhance(1.2)
    tmp = path + '.tmp.png'
    img.save(tmp, optimize=True)
    os.replace(tmp, path)


def main():
    out, prefix, n = sys.argv[1], sys.argv[2], int(sys.argv[3])
    dark = prefix.startswith('dark')
    names = ['mantle', 'magenta', 'blue', 'cyan', 'yellow', 'orange', 'green', 'red']
    pal = {k: rgb(v) for k, v in zip(names, sys.argv[4:12])}
    os.makedirs(out, exist_ok=True)
    for i in range(1, n + 1):
        p = os.path.join(out, f'{prefix}-{i}.png')
        if not os.path.exists(p):
            make(p, i, pal, dark)


if __name__ == '__main__':
    main()
