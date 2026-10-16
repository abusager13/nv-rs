# Local MetalFX integration

This directory adapts the spatial-upscaling path from `bevy_metalfx` 0.4.2
for this viewer's Bevy 0.20 / wgpu 30 versions. The upstream source and its
MIT and Apache-2.0 license texts are retained here; upstream documentation is
kept in `UPSTREAM-README.md` and `UPSTREAM-CHANGELOG.md`.

The viewer uses spatial upscaling only. Its Display settings expose the toggle
on Apple Silicon macOS when the MetalFX framework is available. The setting
starts Off, renders at native resolution while Off, and renders at 50% per
dimension while On. Only 3D cameras targeting the primary window receive the
resolution override; the final writing camera gets the MetalFX pass.

This local adaptation has not been visually validated in the viewer yet.
