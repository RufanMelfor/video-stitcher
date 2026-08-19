# Async-detect benchmark vs v0.5.4

Real, measured export-speed comparison between the last stable release
(v0.5.4) and the `feat/async-detect-thread` branch, run on identical
source footage. Numbers below are wall-clock, not estimates.

**Test machine**: NVIDIA GeForce RTX 3060 Ti, 8192 MiB VRAM, driver
selected via Dx12 backend.

**Source**: 03 OJC match footage, `DJI_20260704095935_0028_D_L01.MP4` +
`..._0029_D_R01.MP4`, HEVC, `yuv420p10le` (10-bit), 3840x2880 @
29.97fps. Same calibration file, same 300.01s clip (0-300s, 8991
frames), same detection model (`yolo26s_tiled1920_full`) and same
panner/tracking settings across all three runs. Output: 2560x1440
HEVC.

## Configurations tested

| # | Build | Execution provider | Async detect | `--lookahead-reduced-bit-depth` |
|---|---|---|---|---|
| 1 | v0.5.4 release (`reco-cli-v0.5.4-windows-x86_64`) | DirectML | n/a (flag doesn't exist yet) | n/a (flag doesn't exist yet) |
| 2 | `feat/async-detect-thread` (local build) | TensorRT | off | on |
| 3 | `feat/async-detect-thread` (local build) | TensorRT | on | on |

Config 3 was measured twice to sanity-check run-to-run noise (287.06s
and 286.61s, 0.45s / <0.2% apart).

`--lookahead-reduced-bit-depth` is required on configs 2/3 for this
10-bit source - main has an unresolved D3D11 P010 copy-compatibility
bug that crashes the export without it (v0.5.4 predates the lookahead
buffer feature entirely, so it isn't affected). This is an unavoidable
difference between the two builds, not a methodology choice, and is
called out here rather than hidden.

All three runs used TensorRT execution provider confirmation and
async-detect thread activity read directly from `reco-gui.log`
(`ORT: TensorRT execution provider enabled`, `Export: EXPERIMENTAL
async detect thread active`) rather than inferred - config 1 was
verified as DirectML-only by the absence of any TensorRT provider DLL
in that release package.

## Results

| Config | Wall-clock export | Throughput | GPU util (active) | VRAM peak |
|---|---|---|---|---|
| 1. v0.5.4 (DirectML) | 500.03s | 18.0 fps | ~50-77% | 7171 MiB |
| 2. main, TensorRT, async off | 356.00s | 25.3 fps | avg 59%, peak 74% | 6067 MiB |
| 3. main, TensorRT, async on (avg of 2) | 286.84s | 31.3 fps | avg 79%, peak 89% | 7889 MiB |

Wall-clock was measured as `Export requested` -> `Encode thread: 8991
frames` in `reco-gui.log` for configs 2/3, and output file creation ->
last-write timestamp for config 1 (CLI, no equivalent log). Both
methods bound the same real work: command issued to fully-written
output.

## Isolated contribution

| Step | Calculation | Speedup |
|---|---|---|
| TensorRT backend + other accumulated main fixes since v0.5.4 (async still off) | 500.03s / 356.00s | **1.41x** |
| Async-detect alone (TensorRT held constant) | 356.00s / 286.84s | **1.24x** |
| **Total, v0.5.4 -> main with async** | 500.03s / 286.84s | **1.74x** |

The 1.24x async-only figure lands inside the previously measured
1.22-1.42x range from earlier profiling runs (see
`project_async_detect_thread_design` session notes), cross-validating
both measurements independently.

## Trade-off

Async detect uses more VRAM at peak (7889 MiB vs 6067 MiB, +30%) -
the cost of a second, separate detector instance running on its own
worker thread. On an 8 GiB card this leaves limited headroom; see
`docs/gpu-hardware-recommendation.md` for VRAM-sizing guidance for a
future GPU purchase.

## Caveat

Config 1 (v0.5.4) runs DirectML, not TensorRT - no TensorRT-enabled
v0.5.4 build was available for this test. The "1.41x" row above
therefore bundles the TensorRT-vs-DirectML backend change together
with every other fix merged into main since v0.5.4 (VRAM pool crash
fix, D3D11 start-time seek fix, 8-bit lookahead downconvert, etc.) -
it is not an async-specific number. Only the "1.24x" row isolates
async-detect's own contribution, holding the execution provider and
every other variable constant between configs 2 and 3.
