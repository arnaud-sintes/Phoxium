# Application icons

Generated files, do not edit by hand. The source is the procedural renderer in
`crates/appres/examples/gen_icon/` (`draw.rs` the artwork, `svg.rs` the vector master, `main.rs` the sizes and
the files).

| File | What |
|---|---|
| `icon.ico` | ultimateCRT, embedded in `ultimatecrt.exe` by `crates/ultimatecrt/build.rs` |
| `icon-256.png` | ultimateCRT at 256 px (README, release page) |
| `icon.svg` | Vector approximation of the 256 px design (no barrel warp, simplified glow); the raster files are not made from it |
| `pixel-demo.ico` | pixel-demo, embedded in `pixel-demo.exe` by `crates/pixel-demo/build.rs` |
| `pixel-demo-256.png` | pixel-demo at 256 px |

## Design

ultimateCRT: a CRT tube face seen straight on. A graphite casing (a superellipse, lit from above: light top rim,
darker chin, thin dark outline for light backgrounds) holds a recessed, rounder screen whose sides bulge like a
curved faceplate. On the dark glass glows a pixel-art disc in the app's amber accent (`#E9A23B`, from
`crates/ultimatecrt/src/ui/theme.rs`), warmer at the bottom, drawn the way the app draws a picture: through a
barrel warp, as scanlines (a beam profile per source row), with fine R, G, B aperture-grille stripes, a phosphor
glow around it, an inner shadow at the glass edge and a soft reflection in the upper left.

Sizes: 16, 20, 24, 32, 40, 48, 64, 96, 128, 256. Each is rendered at its own size (8 x 8 or 16 x 16 samples per
pixel in linear light), not scaled down from 256:

- 128 and 256: the full design (20 x 15 source pixels, stepped disc edge, stripes, warp).
- 20 to 96: pixel-snapped casing and screen, the disc as bars of 3 or 4 px with a 1 px scanline gap, no stripes.
- 16: the casing, the screen and a glowing disc only.

pixel-demo: the same casing with a flat screen and a small teal pixel-art creature, so the two taskbar buttons
differ at a glance.

The `.ico` files hold PNG frames from 64 px up and 32-bit BMP frames (with an AND mask) below, as Windows expects.

## Regenerate

```
cargo run -p appres --example gen_icon --release
```

It rewrites the files above (deterministic: same sources, same bytes) and writes `target/icon-preview/`
(not committed): every size as PNG, enlarged views (`icon-16-x8.png`, `icon-32-x4.png`, ...) and `preview.png`,
both icons at every size on a dark and a light background. Rebuild afterwards: the build scripts track the
`.ico` files.
