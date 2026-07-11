# reco-core friction log

## wgpu `gles` feature disabled by default

**Symptom:** Build fails on Windows with `khronos_api` build-script error
(`webgl_exts.rs` not generated, os error 2 or 3).

**Root cause:** `khronos_api 3.1.0` (transitive via `wgpu/gles → wgpu-hal/gles →
glow → gl_generator → khronos_api`) has a build script that uses
`env::current_dir()` instead of `env::var("CARGO_MANIFEST_DIR")` to locate its
bundled `api_webgl/extensions/` XML tree. On Windows, Cargo does not guarantee
the working directory is the package root during build-script execution, so the
path lookup fails silently and `webgl_exts.rs` is never generated.

**Impact:** The GL/OpenGL ES backend is compiled out. This breaks the Raspberry
Pi 5 (V3D GPU) path in `gpu/mod.rs` which falls back to `wgpu::Backends::GL`.
All other platforms (Windows DX12, Linux Vulkan, macOS Metal) are unaffected.

**Workaround (RPi5):** Add `wgpu` as a direct dependency of any binary crate
targeting RPi5 and enable `wgpu/gles`. Or wait for a `khronos_api` update that
uses `CARGO_MANIFEST_DIR` in its build script.

**Proposed fix:** Replace the `rpi` runtime check in `gpu/mod.rs` with a
compile-time feature gate (`#[cfg(feature = "gles")]`), and add a `gles` /
`rpi` feature to reco-core that re-enables `wgpu/gles`.

## Seam blend was single-band - now has an opt-in 2-band spatial alternative

**Original symptom:** `ViewportConfig::blend_width` beyond ~0.12 washes out
ball tracking in the overlap region (see that field's doc comment; confirmed
in prior Jetson CSI IMX477 production testing), because the default seam
blend (`fisheye.wgsl`'s `fs_main` alpha smoothstep) is a single-band linear
crossfade over raw pixels - widening it just blurs/doubles any residual
misalignment or fast-moving content instead of hiding it.

**Status: 2-band spatial blend built (`ViewportConfig::multiband_blend_enabled`,
default `false`).** Not a full N-level Laplacian pyramid (see "not a real
pyramid" below), but the same core idea: blend low frequencies over a wide
band, high frequencies over a narrow band, so `blend_width` reads as wider
without doubling static structure (goal lines, goalposts) near the seam.

Implementation lives in `render/renderer.rs`'s `encode_multiband_stitch_pass`
(+ `shaders/blur.wgsl`, `shaders/multiband_composite.wgsl`). Ten render
passes per frame, all reusing the *existing* fisheye pipeline for the
per-camera and seam-mask renders (no shader changes needed there, just
different uniform values - `ground_tilt.w`/`blend_width` overrides):

1. Render left camera alone (hard 0/1 FOV-coverage alpha, no seam fade) → `tex_a`.
2. Render right camera alone likewise → `tex_b`.
3. Render whichever plane fades (per `blend_flip_direction`) again, with a
   near-zero blend width, for a hard seam-position mask → `tex_mask`.
4. Separably Gaussian-blur all three (`blur.wgsl`, premultiplied-alpha
   correct so blur doesn't pull black in at coverage edges).
5. Composite (`multiband_composite.wgsl`): `low = blend(blur_a, blur_b, wide mask)`,
   `high = blend(a - blur_a, b - blur_b, narrow mask)`, `final = low + high`.
   The narrow mask isn't a second blur pass - it's a steep `smoothstep`
   re-derived from the wide mask around its 0.5 crossover, in-shader.

**Not a real pyramid - a capped approximation.** The blur is a fixed-tap
loop (`blur.wgsl`'s `MAX_RADIUS = 32`), not a mip chain, so sigma is capped
(`(blend_width * width * 0.12).clamp(2.0, 10.0)` texels) well below what
`blend_width`'s UV-space fraction might suggest, to stay well-sampled. A
real N-level pyramid would scale the blur radius with a downsample chain
instead of a fixed-resolution loop - more work, not started, would remove
this cap.

**Verified (2026-07-11):** rendered the same real DJI Osmo Action 4 footage
through `reco-cli stitch --blend 0.2` with and without `--multiband`,
diffed the two outputs - differences are confined to a single vertical
band exactly at the seam position, identical everywhere else (no corruption,
no whole-frame drift). Stitch cost roughly doubled (10.4ms → 19.9ms/frame at
960x540 on a Quadro K3100M) - consistent with 10 passes vs 1, bounded, not
catastrophic. GUI exposes it as "Multi-band blend (experimental)" next to
"Seam blend" in the Stitching panel; CLI via `reco stitch --multiband`.
Unlike `color_match_enabled`, this is a pure GPU technique (no CPU pixel
access needed) so it applies uniformly across every render path, including
BGRA and zero-copy.

**Important limit even with this built:** multi-band blending only helps
*static* seam-adjacent structure. It does not fix ball-position ghosting
during fast motion through the overlap band - that's genuine parallax (the
ball sits at a slightly different position in each camera's frame), not
something any blending algorithm can fix. reco-calibrate's FRICTION.md notes
even ActionStitch accepts this as a known limit.

## Per-camera exposure/color mismatch at the seam: now measured and corrected

**Status: fixed (see `render/color_match.rs`).** The shader's
`color_scale`/`color_offset_blend` uniforms existed since the YUV-space
color-transfer rewrite but were always wired to identity - no consumer ever
computed a real correction. Two independently-metering action cameras can
disagree on exposure/white balance, showing up as a color/brightness step
at the seam independent of geometric alignment.

`render::color_match::ColorMatchState` now periodically (every 15 frames)
samples a coarse grid in the seam-adjacent band of each camera's raw frame
(reusing `lens::undistorted_to_distorted`, the same KB4 forward mapping the
shader itself uses), decodes it into the shader's own color-transfer YUV
space, and derives a small EMA-smoothed, clamped per-camera offset that
nudges both cameras toward their shared mean. Wired into
`StitchPipeline`'s YUV420P/NV12 render paths; the BGRA and GPU zero-copy
paths have no CPU pixel access at that point and always render with
identity correction (documented limitation, not a bug).

All tuning knobs are live `ViewportConfig` fields (`color_match_enabled`,
`color_match_band_width`, `color_match_grid_cols`/`_rows`,
`color_match_interval_frames`, `color_match_ema_alpha`,
`color_match_max_y_offset`/`_max_chroma_offset`) bundled internally into
`color_match::ColorMatchParams`. `StitchRenderer`'s `set_color_match_*`
setters call `StitchPipeline::force_color_match_remeasure()` after writing
the field, so a consumer GUI slider is reflected on the very next rendered
frame instead of waiting up to `color_match_interval_frames` frames - see
`ColorMatchState::force_remeasure`. reco-gui exposes all of these as
sliders under the right-panel "Color Mapping" section.
