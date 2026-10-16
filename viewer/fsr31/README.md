# FSR 3.1.4 wgpu integration

This local package adapts [`cwfitzgerald/wgpu-ffx`](https://github.com/cwfitzgerald/wgpu-ffx)
0.1.0 to the viewer's wgpu 30 dependency. Its precompiled shader assets come
from the companion `wgpu-ffx-shaders-spv` 0.1.0 crate, built from AMD FidelityFX
SDK 1.1.4 (FSR 3.1.4). Both projects are MIT licensed; see `LICENSE-MIT`.

The upstream implementation is experimental. Its `GenerateReactive` pass is
not wired, and no macOS/Metal validation has been done in this viewer. Keep the
feature off by default until the integration is built and visually checked.
