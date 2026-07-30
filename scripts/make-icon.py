#!/usr/bin/env python3
"""Render the app icon from the design system, rather than shipping a stock one.

The default Tauri icon — a glossy teal/orange swirl — shares nothing with Field Notes
V2: not the palette, not the geometry, not the idea. This draws the mark the app
already uses for itself, `CLUSTERS.brand` from PixelCluster.tsx: a 3x3 grid with
two cells knocked out, like a fragment of a QR code (§4.4, which replaces all
stroke icons).

Two languages on purpose. The outer shape is Apple's squircle, because an icon
that ignores it reads as broken next to every other app in the Dock. Everything
inside it is hard-cornered, because §1 says the rectangle is the signature and
that is what makes the icon ours at a glance.

The mark is flat by definition — seven hard-edged cells — and flat cells drawn
large enough read to the eye as *magnified pixels* rather than as a logo, which
is why the first cut looked low-resolution even though every slice is rendered
natively at full size. Three changes fix that without touching the brand
geometry: the cluster is smaller relative to the tile, so it reads as a mark
sitting on a surface instead of a sprite scaled up to fill one; the cells cast a
soft shadow, so they have a height; and the tile gets a lit corner and a rim, so
it is an object rather than a swatch. Depth is what separates "icon" from
"pixel art" when the glyph itself is a grid.

Writes a 1024px master; `sips` and `iconutil` derive the rest.
"""
from __future__ import annotations

import struct
import sys
import zlib
from pathlib import Path

import numpy as np

# -- palette (styles.css §3) ------------------------------------------------
FOREST_TOP = (0x1E, 0x26, 0x18)  # --color-forest
FOREST_BOT = (0x14, 0x18, 0x0F)  # --color-forest-950
SAGE = (0xB8, 0xD4, 0xA4)  # --color-sage
SAGE_BRIGHT = (0xD2, 0xE6, 0xC2)  # --color-sage-bright

SIZE = 1024
SS = 3  # supersample factor — the squircle and the cells both need clean edges

# Apple's macOS icon template: an 824px shape centred in a 1024px canvas. Using
# the real numbers matters; a squircle that fills the canvas looks oversized
# next to native apps.
SHAPE = 824
SQUIRCLE_N = 5.0  # superellipse exponent ~= Apple's continuous corner

# CLUSTERS.brand — index 2 and 6 knocked out.
BRAND = [True, True, False, True, True, True, False, True, True]
CLUSTER_EXTENT = SHAPE * 0.50
GAP_RATIO = 0.46

# Below this the tile is only a few pixels across; a shadow and a rim collapse
# into grey haze and cost more legibility than the depth is worth. The 16 and 32
# slices stay deliberately flat.
DEPTH_FROM = 64


def _box_blur(a: np.ndarray, r: int) -> np.ndarray:
    """Separable box blur, run twice — close enough to a Gaussian for a shadow."""
    if r < 1:
        return a
    for _ in range(2):
        for axis in (0, 1):
            pad = [(0, 0), (0, 0)]
            pad[axis] = (r, r)
            p = np.pad(a, pad, mode="constant")
            zeros = np.zeros_like(np.take(p, [0], axis=axis))
            c = np.concatenate([zeros, np.cumsum(p, axis=axis)], axis=axis)
            hi = np.take(c, range(2 * r + 1, c.shape[axis]), axis=axis)
            lo = np.take(c, range(0, c.shape[axis] - 2 * r - 1), axis=axis)
            a = (hi - lo) / (2 * r + 1)
    return a


def render(size: int = SIZE) -> np.ndarray:
    """RGBA float array in [0,1], shape (size, size, 4)."""
    n = size * SS
    scale = n / SIZE

    yy, xx = np.mgrid[0:n, 0:n].astype(np.float64)
    # Sample at pixel centres so the shape isn't a half-pixel off.
    xx = (xx + 0.5) / scale
    yy = (yy + 0.5) / scale

    cx = cy = SIZE / 2.0
    a = SHAPE / 2.0

    # -- squircle mask ------------------------------------------------------
    d = (np.abs(xx - cx) / a) ** SQUIRCLE_N + (np.abs(yy - cy) / a) ** SQUIRCLE_N
    mask = (d <= 1.0).astype(np.float64)

    # -- background ---------------------------------------------------------
    # A vertical gradient plus a broad light off the top-left corner. The light
    # is what stops the tile reading as one flat swatch of colour.
    t = np.clip((yy - (cy - a)) / (2 * a), 0.0, 1.0)[..., None]
    top = np.array(FOREST_TOP, dtype=np.float64) / 255.0
    bot = np.array(FOREST_BOT, dtype=np.float64) / 255.0
    rgb = top * (1.0 - t) + bot * t

    lit = np.hypot(xx - (cx - a * 0.62), yy - (cy - a * 0.62)) / (a * 2.1)
    rgb = rgb + (np.clip(1.0 - lit, 0.0, 1.0) ** 2)[..., None] * 0.055

    # -- the pixel cluster --------------------------------------------------
    cell = CLUSTER_EXTENT / (3 + 2 * GAP_RATIO)
    gap = cell * GAP_RATIO
    origin = cx - CLUSTER_EXTENT / 2.0

    sage = np.array(SAGE, dtype=np.float64) / 255.0
    sage_bright = np.array(SAGE_BRIGHT, dtype=np.float64) / 255.0

    cells = np.zeros_like(mask)
    for idx, on in enumerate(BRAND):
        if not on:
            continue
        col, row = idx % 3, idx // 3
        x0 = origin + col * (cell + gap)
        y0 = origin + row * (cell + gap)
        inside = (xx >= x0) & (xx < x0 + cell) & (yy >= y0) & (yy < y0 + cell)
        cells = np.maximum(cells, inside.astype(np.float64))
        # The centre cell lifts to sage-bright: one focal point, so the mark has
        # somewhere to look instead of reading as flat texture.
        colour = sage_bright if idx == 4 else sage
        rgb = np.where(inside[..., None], colour, rgb)

    out = np.concatenate([rgb, mask[..., None]], axis=-1)
    out[..., :3] *= mask[..., None]  # premultiply so edges don't fringe

    # -- downsample (box filter) -------------------------------------------
    out = out.reshape(size, SS, size, SS, 4).mean(axis=(1, 3))
    cells = cells.reshape(size, SS, size, SS).mean(axis=(1, 3))
    alpha = out[..., 3:4]

    if size >= DEPTH_FROM:
        # Shadow, applied only outside the cells so it sits under them rather
        # than dirtying their faces. Working at final resolution keeps this
        # cheap; a shadow is soft anyway, so supersampling buys nothing.
        drop = max(1, round(size * 0.013))
        cast = _box_blur(np.roll(cells, drop, axis=0), max(1, round(size * 0.016)))
        shade = np.clip(cast - cells, 0.0, 1.0) * alpha[..., 0] * 0.5
        out[..., :3] *= (1.0 - shade)[..., None]

        # Rim light along the top edge of the tile — the standard macOS read of
        # a solid object catching the light, and it keeps the silhouette from
        # dissolving into a dark Dock background.
        band = max(1, round(size * 0.006))
        rim = np.clip(alpha[..., 0] - _box_blur(alpha[..., 0], band), 0.0, 1.0)
        yn = np.linspace(0.0, 1.0, size)[:, None]
        out[..., :3] += (rim * np.clip(1.0 - yn / 0.45, 0.0, 1.0) * 0.30)[..., None]

    # Un-premultiply for straight-alpha PNG.
    with np.errstate(divide="ignore", invalid="ignore"):
        out[..., :3] = np.where(alpha > 0, out[..., :3] / alpha, 0.0)
    return np.clip(out, 0.0, 1.0)


def render_tray(size: int = 44) -> np.ndarray:
    """The menu-bar mark: the cells alone, on transparency.

    A macOS template image carries no colour — the system reads its *alpha* and
    redraws the shape in black or white to match the menu bar, and inverts it
    when the item is highlighted. Handing the app icon to a template slot
    therefore does not produce a small app icon: the tile is fully opaque, so
    every pixel of the rounded square is "shape", and the menu bar shows a solid
    white blob. The mark has to be knocked out of the tile and shipped as
    coverage only.

    Black fill by convention; the system tints it either way.
    """
    n = size * SS
    yy, xx = np.mgrid[0:n, 0:n].astype(np.float64)
    xx = (xx + 0.5) / SS
    yy = (yy + 0.5) / SS

    # A little inset — a mark that runs to the edge crowds the clock and the
    # neighbouring status items.
    extent = size * 0.86
    cell = extent / (3 + 2 * GAP_RATIO)
    gap = cell * GAP_RATIO
    origin = (size - extent) / 2.0

    alpha = np.zeros((n, n), dtype=np.float64)
    for idx, on in enumerate(BRAND):
        if not on:
            continue
        col, row = idx % 3, idx // 3
        x0 = origin + col * (cell + gap)
        y0 = origin + row * (cell + gap)
        inside = (xx >= x0) & (xx < x0 + cell) & (yy >= y0) & (yy < y0 + cell)
        alpha = np.maximum(alpha, inside.astype(np.float64))

    alpha = alpha.reshape(size, SS, size, SS).mean(axis=(1, 3))
    out = np.zeros((size, size, 4), dtype=np.float64)
    out[..., 3] = alpha
    return out


def write_png(path: Path, img: np.ndarray) -> None:
    h, w, _ = img.shape
    data = (img * 255.0 + 0.5).astype(np.uint8)
    raw = b"".join(b"\x00" + data[y].tobytes() for y in range(h))

    def chunk(tag: bytes, payload: bytes) -> bytes:
        return (
            struct.pack(">I", len(payload))
            + tag
            + payload
            + struct.pack(">I", zlib.crc32(tag + payload) & 0xFFFFFFFF)
        )

    path.write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


if __name__ == "__main__":
    args = [a for a in sys.argv[1:] if a != "--tray"]
    tray = "--tray" in sys.argv
    dest = Path(args[0] if args else "icon-master.png")
    size = int(args[1]) if len(args) > 1 else (44 if tray else SIZE)
    write_png(dest, render_tray(size) if tray else render(size))
    print(f"  {dest.name}  {size}px{'  (template)' if tray else ''}"
          f"  {dest.stat().st_size / 1024:.1f} KB")
