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

## Color-match measurement band didn't track `seam_offset` - could make the seam worse than disabled

**Symptom (reported 2026-07-13 on real match footage):** with Auto Color
Match on, the left/right brightness step at the seam was visibly *worse*
than with it off - a hard, unnatural vertical discontinuity, confirmed via
screenshot and reproduced on the user's actual calibration
(`resources/test-data/match_clicks.json`, `seam_offset: 0.157`) and source
footage.

**Root cause:** `measure_band_mean` picked its sampling band from the
plane's *fixed* UV edge (`is_right` → `[0, band_width]`, else →
`[1-band_width, 1]`) and never accounted for `seam_offset`, even though
`fisheye.wgsl`'s `fs_main` shifts its alpha threshold by exactly that amount
(`smoothstep(seam_offset, seam_offset + blend_width, uv.x)` and the mirror
for the other edge). Once a calibration has a non-trivial `seam_offset` -
this one's `0.157` exceeded the default `band_width` of `0.15` outright -
the measurement band sits entirely past where the seam actually renders.
The correction then reflects whatever's in that unrelated strip (crowd,
signage, a sunlit vs. shaded patch of pitch) rather than the two cameras'
real difference at the visible seam, and - being a uniform per-frame offset
via `apply_color_transfer` - gets applied everywhere, which can easily make
the true seam *more* mismatched than doing nothing. Verified on real
footage: rendering the same clip/calibration before and after the fix
showed materially different measured means (band moved as intended) and
the pre-fix render reproduced the reported hard vertical seam split;
post-fix it did not.

**Fix:** `ColorMatchParams` carries `seam_offset` (from
`ViewportConfig::seam_offset`, wired in
`StitchPipeline::color_match_params`); `measure_band_mean`'s band bounds are
now computed by `seam_band_bounds`, which shifts the same way the shader
does (`seam_band_bounds_tracks_seam_offset` test) and clamps to `[0, 1]`
instead of reading past the frame when `seam_offset` pushes the band out of
range (`seam_band_bounds_clamps_to_valid_uv_range`).

**Follow-up (same day): the above fix was still incomplete with
`blend_flip_direction: true`.** Re-tested on the user's exact calibration
(which has `blend_flip_direction: true`) with a controlled before/after
render (same source frames, `--no-color-match` added to `reco stitch` for
a clean A/B) - the seam step *still* grew over time as the correction
converged (13 -> 28 gray levels across a 20s clip), while the
correction-off baseline stayed flat around 10-12.

**Root cause:** `seam_offset` only ever moves the alpha threshold of
whichever plane `renderer.rs` currently designates as *fading*
(`left_uniforms.ground_tilt[3] = if flip {1.0} else {0.0}`, mirrored for
right) - the other, fixed/opaque plane renders at `alpha = 1.0`
unconditionally (the `if u.ground_tilt.w > 0.5` gate in `fisheye.wgsl`
never runs for it), so its on-screen boundary never moves with
`seam_offset` at all. The first fix shifted *both* planes' measurement
bands by `seam_offset` unconditionally. With the default
`blend_flip_direction: false` that's harmless by coincidence (right is
always the fading plane there), but with `blend_flip_direction: true` -
this calibration - right is the *fixed* plane, so shifting its band moved
it away from its actual (unshifted) on-screen boundary, feeding the
correction a `right_mean` that didn't represent what's really adjacent to
the seam.

**Fix:** `measure_band_mean` now resolves `is_fading_plane(is_right,
blend_flip_direction)` and zeroes the shift for whichever plane isn't
fading before calling `seam_band_bounds`, matching `renderer.rs`'s
`ground_tilt[3]` assignment exactly (`is_fading_plane_matches_renderer_convention`
test). `ColorMatchParams` gained `blend_flip_direction`, wired from
`ViewportConfig::blend_flip_direction` in `color_match_params`. Verified
on the user's real footage/calibration: the seam step with correction on
now tracks the correction-off baseline (~11-13 vs ~10-12) instead of
growing unbounded. A smaller, stable brightness/tint difference remains
even with color-match off - likely each lens' own vignetting/metering, not
something a single per-frame additive offset can fully erase - but the
specific regression (*on* being worse than *off*) is gone.

**Second follow-up: that grayscale-only measurement undersold a real
remaining problem - the chroma correction specifically is still net-harmful
on this footage.** The luma-only check above used single-channel grayscale,
which hid a per-channel effect. Re-measured in full RGB on the same
calibration/footage (mean absolute R+G+B difference across the seam,
20s-converged):

| Config | Total \|ΔRGB\| |
|---|---|
| Color match off | 35.4 |
| Y-only (chroma clamp forced to 0) | **24.5 - genuinely better** |
| Y+chroma, default `band_width` 0.15 | 42.1 - worse than off |
| Y+chroma, narrower `band_width` 0.03 | 57.3 - worse still |

Luma correction is now correctly targeted (see above) and measurably helps.
Chroma correction reflects a real, consistently-measured Cb difference
between the cameras' seam-adjacent content (not noise - stable across
measurements), but converting that mean-band Cb/Cr difference back to RGB
via the standard BT.709 matrix (`apply_color_transfer`) has an outsized
effect on blue specifically (`ΔB ≈ 1.8556 × ΔCb`), and applying it
uniformly overshoots rather than fixing the true near-seam mismatch -
narrowing the band makes this worse, not better, meaning it isn't simply
"the band samples the wrong pixels" the way the luma bug was. Likely the
measured band still isn't scene-content-independent enough for chroma (a
green pitch's Cb/Cr sensitivity to metering/white-balance differs from a
grey card), needing a smarter approach than a plain mean - not yet solved.

**Practical workaround, verified:** set "Max chroma offset" to `0` in
reco-gui's Color Mapping section (`color_match_max_chroma_offset`) - keeps
the (now correctly beneficial) luma correction while dropping the harmful
chroma correction. Left as a live user-adjustable slider rather than
changed as the shipped default, since this is one rig's real-world
measurement, not proof the chroma correction is harmful in general.

## `PlaneLayout::intersect` is not a safe "move the seam" control

**Symptom (confirmed 2026-07-11 on real DJI Osmo Action 4 footage):**
lowering `intersect` by ~0.4 from a calibrated value opened a large black
void between the two cameras' visible content - neither camera had
coverage there anymore. The seam did move, but coupled with a coverage
regression that makes `intersect` unsafe to drag freely.

**Root cause:** `intersect` repositions the plane geometry itself
(`PlaneLayout::intersect`'s doc: "each plane is translated by
`(plane_width/2) × (1-intersect)`"), which changes how much the two
cameras' angular coverage overlaps in 3D. Push it far enough and the
overlap margin a given calibration happens to have runs out, leaving a
gap neither plane covers. It's the right lever for the *auto-calibrate
optimizer* (which searches this parameter jointly with the others against
real feature matches, so it never wanders outside safe coverage) but the
wrong lever for a human dragging a slider with no such constraint.

**Fix: `MatchCalibration::seam_offset` / `ViewportConfig::seam_offset`
(default `0.0`, safe range `SEAM_OFFSET_RANGE` = ±0.3).** A dedicated,
render-time-only nudge that shifts *only* the alpha-crossfade threshold
(`fisheye.wgsl`'s `seam_offset` uniform, `lens_preview.w`) within the
coverage the calibration already guarantees - it never touches plane
geometry, so it can't open a gap the way `intersect` can. Verified: at
`seam_offset` -0.2 / 0.0 / +0.2 the seam line moved cleanly across ~36% /
43% / 53% of frame width with full coverage throughout, vs. `intersect`'s
black void at a much smaller perturbation.

Persisted like `blend_width` (survives save/reload), and unlike
`color_match_enabled` needs no CPU pixel access, so it applies uniformly
across every render path. GUI: "Seam position" slider *or* drag directly
on the preview while "Show seam line" is active (`reco-gui`'s `drag`
`TouchArea` switches from panning to seam-editing based on that flag).
The drag handler flips sign under `blend_flip_direction` so a
screen-space drag always feels the same direction regardless of which
camera is currently fading - see `AppState::seam_drag`'s doc for why the
raw uniform's sign doesn't have that property on its own. CLI:
`reco stitch --seam-offset`.

**Companion debug aid: `ViewportConfig::show_seam_line`.** Draws a thin
line at the exact seam position (offset included) by reusing the
existing alpha-fade threshold math in `fisheye.wgsl` - no separate
position calculation, so the line can never disagree with where the
blend actually is. Works identically in single-band and multi-band mode
since both reuse the same fragment shader for the per-camera renders.

## Lookahead pool VRAM cost scales with source bit depth

**Symptom:** Export fails outright on lower-VRAM cards with high-
resolution 10-bit sources: `"not enough VRAM for a 1.5s lookahead: reduce
the lookahead to <= 1.0s, use lower-resolution source footage, or free
GPU memory. The frame pool needs ~4.7 GB (71 slots @ 3840x2880); usable
budget is ~3.2 GB of 7.6 GB total."` Reported against real DJI Osmo
Action 4 HEVC 10-bit footage (3840x2880) on a 7.6 GB card at the default
1.5s lookahead.

**Not a bug in the budget check itself.** `session::vram_pool`'s pre-
flight math (`lookahead_budget_bytes`, `max_lookahead_frames`) is
deliberately conservative and fails safely with an actionable message -
see the "clean_small_card_trusts_free_no_false_green" and
"bogus_free_falls_back_to_total" tests for the production incidents that
shaped it. The failure is real: the pool it's sizing is genuinely that
big.

**Root cause:** the lookahead pool (`session::vram_pool::VramPool` on
Linux/macOS, `interop::d3d11::D3d11StagingPool` on Windows) holds *raw
decoded* stereo frames pre-stitch, at the source's native bit depth via
`render::renderer::GpuPixelFormat` (`P010`/10-bit for HEVC 10-bit
sources, `Nv12`/8-bit otherwise - see `reco_io::adapters::pixel_format()`
upstream in `reco-io`). A 10-bit source therefore costs 2x the VRAM per
buffered frame of an otherwise-identical 8-bit source. This pool is not
an AI-only side buffer that could shrink for free: the *same* buffered
frames are what the final stitch render consumes once it catches up to
them (`frame_processing.rs`'s `render_d3d11_from_slot` /
`render_d3d11_staged` on Windows read directly from the staging pool;
the Linux/macOS render path reads from `VramPool` slots the same way).
So the lookahead pool's bit depth is a genuine, if subtle (banding risk
in smooth gradients - sky, pitch grass, floodlit surfaces), whole-export
quality knob, not just an AI-precision one. This is a different buffer
from the final render target, which is already 8-bit `Rgba8Unorm` - no
contradiction, just two separate pipeline stages.

**Competitive research:** a competing product ("Once Autocam") solves the
same problem by (a) keeping only a 4-8 frame lookahead instead of a
default-1.5s/~45-frame window, (b) forcing 8-bit (`yuv420p`) internally
even for sources it can encode at 10-bit, and (c) running AI detection on
frames downscaled well below source resolution. Its buffering is CPU/
system-RAM-resident (a PyInstaller-frozen Python process, not a GPU-
resident pool), which is measurably slower in practice (GPU<->CPU round-
trips) - not a tradeoff reco wants to copy. Only lever (b) - bit depth -
is both GPU-resident-compatible and additive to what reco already does.

**Fix, shipped for Linux/macOS: `session::vram_pool::LookaheadBitDepth`
(opt-in, default `Native` = unchanged behavior).** Setting
`Reduced8Bit` via `StitchSession::set_lookahead_bit_depth` makes the pool
always allocate 8-bit NV12 slots regardless of source format, and routes
`VramPool::copy_from_textures` through a new GPU downconvert pass
(`render::lookahead_downconvert::LookaheadDownconverter`,
`shaders/lookahead_downconvert.wgsl`) instead of the previous bit-exact
`copy_texture_to_texture` whenever the pool's format differs from the
source's. `Native` sources (already 8-bit, or `Reduced8Bit` not
requested) take the exact same `copy_texture_to_texture` path as before -
zero behavior change unless explicitly opted in. Roughly halves the
pool's VRAM footprint for 10-bit sources with no CPU round-trip (stays
entirely GPU-resident), and the pre-flight budget check
(`run_loop.rs`) now sizes itself off the pool's *actual* post-
downconvert format, so a session with `Reduced8Bit` set correctly gets a
larger lookahead ceiling in the same VRAM budget - verified by
`LookaheadBitDepth` unit tests plus a GPU-executing round-trip test
(`lookahead_downconvert::tests::y_plane_downconvert_matches_expected_8bit_values`,
runs the real shader against known 16-bit input values on this machine's
adapter and checks the 8-bit output against hand-computed expected
values, not just that it compiles).

Why a full render pass instead of `copy_texture_to_texture` with a
different format: wgpu's texture-to-texture copy is a raw byte copy, not
a format conversion - it requires matching formats. The downconvert pass
is a small fullscreen-triangle shader per plane (Y and UV separately,
since they differ in channel count and resolution) that relies on wgpu's
existing Unorm normalization (already exploited elsewhere in this
codebase - `GpuPixelFormat`'s own doc comment - to let one shader body
sample both 8- and 16-bit sources uniformly): `textureLoad` normalizes
the 16-bit source to `[0,1]`, the 8-bit render target format quantizes it
back down on write. No manual bit-shift/rescale math needed. Uses
`textureLoad` at the fragment's own pixel coordinate rather than a
filtered `textureSample`, since source and destination are always the
same resolution (this only changes bit depth, never scale) - exact,
1:1, and avoids depending on 16-bit Unorm formats being filterable
(not guaranteed on every backend).

**Windows path (`interop::d3d11::D3d11StagingPool`) - initially scoped
out, then implemented and verified on real hardware in the same
session once a live NVIDIA GPU was available to test against.** The
initial concern (recorded here for anyone reviewing the history): the
pool sized its *own* D3D11-imported (P010/NV12, shared-handle) textures
directly to the full lookahead depth (`n_slots = (lookahead_frames +
post_smooth_half + 4) * 2`), entangled with `enable_cuda` (the same
shared-handle textures are also importable into CUDA for GPU-resident
detection, `cuda_import_d3d11_nv12`/`StagingState::cuda_nv12`), which
made "just shrink the pool" look risky without being able to verify
cross-API synchronization on real hardware.

Investigation before writing any code resolved this: `cuda_nv12_ptrs()`
has **zero callers anywhere in the repo** - the Windows detection
dispatch (`detection_dispatch.rs::detect_and_track_only`'s
`D3d11Resident` arm) unconditionally uses `run_detection_wgpu_nv12` (a
wgpu compute-shader preprocessor), never `DetectorFrame::Cuda`. So the
CUDA-import machinery on this pool is currently dead weight, not a live
consumer - there was no existing reader to race against a smaller pool.
(If a future Windows CUDA detection path is wired up, it must add its
own explicit stream synchronization before this pool's next
`stage_frame` reuses a slot - the small pool no longer provides the
old sizing's large implicit margin for free; flagged in a code comment
at the `n_slots` sizing site.)

With that cleared, the fix: `stage_d3d11_frames` now always sizes the
D3D11-imported pool to a small, fixed, double-buffered-per-camera count
(4 slots) regardless of lookahead depth - it is now purely a short-lived
bridge for the D3D11 shared-handle import, not the long-lived buffer.
The long-lived buffer is `VramPool` (the same struct Linux/macOS already
uses), now constructed on Windows too. `VramPool::copy_from_d3d11`
(new) copies each staged frame from the D3D11 pool into a `VramPool`
slot immediately after staging: a plane-aspect-selected
`copy_texture_to_texture` (`TextureAspect::Plane0`/`Plane1` off the raw
imported multi-planar texture - exposed via a new
`D3d11StagingPool::plane_source`/`D3d11PlaneSource`) when the pool's
format matches the source (`Native`, bit-exact, no shader), or the same
`LookaheadDownconverter` render pass `copy_from_textures` already uses
when `Reduced8Bit` is active. Detection and the final render
(`frame_processing.rs`'s D3D11 render arm) both now read from `VramPool`
bind groups when buffered, mirroring Linux's `render_gpu_resident`
exactly instead of diverging from it.

One real synchronization subtlety, resolved and documented at the call
site (`copy_to_vram_pool_platform`): a wgpu (DX12) render pass reading
the D3D11-imported shared texture has no automatic ordering against the
*next* produce's D3D11-side `CopySubresourceRegion` overwriting that same
slot two frames later - D3D11 and DX12/wgpu are different device/queue
objects, so sharing the underlying VRAM allocation via an NT handle does
not imply shared scheduling. Fixed with an explicit
`device.poll(PollType::wait_indefinitely())` after the copy, before the
staging slot can be reused - the same pattern already used after every
Linux/macOS `copy_from_textures` call for the identical reason (see
`copy_nvmm_to_vram_pool`), not a novel synchronization scheme.

A separate discovery narrowed the implementation: an `R16Unorm`/
`Rg16Unorm` texture cannot be created with `RENDER_ATTACHMENT` usage on
this machine's adapter (`wgpu` validation error, confirmed empirically,
not assumed) even with `TEXTURE_FORMAT_16BIT_NORM` enabled - so
`LookaheadDownconverter`'s render pass can only ever target an 8-bit
destination. This is not a limitation in practice: the downconverter is
only ever invoked when `Reduced8Bit` is active, which always targets
8-bit `Nv12` by construction, so `LookaheadDownconverter` never needed a
16-bit-output mode. The `Native` (bit-depth-preserving) case uses the
plain aspect-selected copy instead, which has no such restriction.

**Verified 2026-07-14 end-to-end on real hardware**, not just unit
tests: `reco-cli stitch` against real 3840x2880 10-bit HEVC DJI Osmo
Action 4 footage, `--lookahead 1.5 --model <yolo onnx>` (real AI
tracking, ball tracker acquired a real target), on an NVIDIA RTX 3060
Ti. At `Native` (default), reproduced the original bug exactly ("not
enough VRAM for a 1.5s lookahead... needs ~4.7 GB"). With
`--lookahead-reduced-bit-depth` passed (the real, shipped CLI flag -
`reco-cli`'s `stitch` subcommand; also exposed in `reco-gui`'s export
panel as "Reduce lookahead memory (8-bit)", both via the new
`StitchJob::lookahead_reduced_bit_depth` builder), the same export
completed cleanly: budget line reported "needs 2.36 GB" (half of 4.7 GB,
as expected), `VramPool: 71 stereo Nv12 slots ... (downconverted from
source P010)`,
`D3D11VA staging pool created: ... 4 P010 slots` (the shrunk bridge
pool), 150/150 frames encoded, no hangs, no errors, no NVENC/driver
faults. Output verified as a valid, correctly-dimensioned, correctly-
timed H.264 file; a decoded frame was visually inspected and shows no
color corruption, no banding, no plane-swap artifacts - a normal-looking
stitched frame.
