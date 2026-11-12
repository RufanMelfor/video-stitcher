# RECO Panopitch

**Open-source GPU-accelerated panoramic sports camera software. Desktop App with AI tracking, CLI, and OBS Plugin.**

[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue)](LICENSE)

Stitch two camera feeds into a seamless panoramic sports view with AI-powered automatic camera control. No subscriptions, no vendor lock-in. Ships as a CLI, a Slint-based desktop app, and an OBS Studio source plugin that all consume the same Rust engine.

## Features

- **GPU-first pipeline** - Real-time stitching via wgpu 28 (Vulkan / Metal / DX12). Upstream reports 500+ fps for stitching alone on modern desktop GPUs; on the tested RTX 3060 Ti a full export with AI tracking runs at roughly 18-31 fps (see `docs/async-detect-benchmark-v054.md`)
- **Zero-copy decode** - Windows: D3D11VA (NVIDIA tested). NVDEC on Linux and VideoToolbox on macOS are upstream code, **untested in this fork**
- **AI ball + player tracking** - YOLO detection on ONNX (CPU / CUDA / CoreML / NCNN) or native TensorRT `.engine`, with anticipatory lookahead smoothing and density-peak player clustering
- **Automatic calibration** - AKAZE feature matching with IMU / audio cross-correlation for temporal sync, Gyroflow lens profiles
- **Live production** - *(upstream code, untested in this fork)* GStreamer camera ingest (Linux / Jetson CSI), push-based `StitchCore::submit_frame_*` for OBS and live streams, replay ring buffer, stacked-video pack/unpack
- **Platforms** - **Windows 11 + NVIDIA (RTX 3060 Ti) is the only tested combination.** The Linux, macOS, Jetson and OBS code paths come from upstream and have not been run in this fork, and **`reco-core` currently does not compile on Linux or macOS** (a Windows-only function is not gated, found by CI on 2026-10-21). Other changes made here also affect every platform (Skia renderer, patched `vendor/` crates). See [Tested hardware](#tested-hardware).

> **Do not install an upstream reco build over this one.** Settings and
> calibration files are not interchangeable between the two.

## Tested hardware

This fork has been developed and tested **only on Windows 11 with an NVIDIA
GeForce RTX 3060 Ti (8 GB)**, using the TensorRT build and DJI Osmo Action 4
footage (3840x2880 HEVC, 8/10-bit). Other operating systems (Linux, macOS),
other GPUs (AMD, Intel, other NVIDIA cards) and other cameras are **untested**:
they may work, but there is no guarantee and no support. The inherited
Linux / macOS / Jetson / OBS code paths from upstream have not been exercised
since this fork diverged.

## What this fork adds

RECO Panopitch 1.0.0-beta.1 is based on reco v0.5.4 plus our own additions. It is a modified version of [reco-project/video-stitcher](https://github.com/reco-project/video-stitcher)
(AGPL-3.0). It diverged at upstream's v0.5.x line (July 2026); later upstream
fixes up to v0.5.4 were ported by hand. Roughly 350 commits and 80k added lines
sit on top of upstream, covering the items below. Features marked *(PR)* are
also offered upstream as pull requests; everything is Windows-tested only.

**Seam, calibration and image quality**
- Per-camera **color matching** measured on the seam band, with multi-band
  blending, auto-fitted gamma, correction limits, and an experimental
  *overlap fit* mode that uses both cameras' views of the same rays (sky
  included). *(PR)*
- **Vivid color grade** and **GPU sharpening** in the Color Mapping panel,
  stored in the setup.
- Manual **ground / top-plane tilt** correction with adjustable band width,
  split per-camera **Roll and Skew** sliders, **manual seam positioning** with
  a draggable seam line. *(PR)*
- **Overlap view** calibration aid: ghost the right over the left, the left
  over the right, both at once, or a difference image (GUI and CLI).
- Calibration tools in `reco-calibrate`: hand-click line-seam fitting,
  photometric / row-profile experiments, live AKAZE detection preview.

**AI tracking**
- **Tiled dual-crop inference** (two overlapping crops per camera frame) for
  TensorRT and ONNX GPU detectors, and non-square model input support.
- **Async detection thread**: detection overlaps the render/encode loop.
  Measured 1.24x export speed-up on the reference machine (356 s -> 287 s for
  a 300 s clip; details in `docs/async-detect-benchmark-v054.md`).
- Ball tracker fixes and tunables: hold and coast time, implausible-jump
  rejection, player-anchor gate with near/far radius, ball reach, ball margin,
  `fov_alpha` / `cluster_alpha`, AI pitch safety lines, visible per-mode
  detection confidence. Settings are saved in the setup and as app
  defaults. See `docs/ai-panner-tuning.md`.
- In-app **field ROI editor** (replaces the browser tool). *(PR)*
- `reco ai-debug-raw`: side-by-side raw-camera export with detection boxes, and
  an `events.jsonl` that starts with a `run_config` record of all AI settings.

**Production workflow**
- **Cut ranges** (skip stoppages) with a timeline UI, audio that follows the
  cuts, and a "PAUZE" dip-to-black overlay at each cut.
- **Goal highlight reel** export.
- **Match Folder picker** (auto-fills left / right / setup, suggests the
  output path), **sync-offset detection** and an **audio waveform** panel. *(PR)*
- Export quality presets with bitrate shown and a per-export `.log` sidecar.
- Export settings embedded as container metadata, and an 8-bit lookahead
  down-convert that halves lookahead VRAM for 10-bit sources. *(PR)*
- **HTML scoreboard overlay** driven by the **Match Logger** phone web app
  (`scripts/match-logger/`). The scoreboard runtime (`reco-scoreboard`,
  `scoreboards/`) is the work of Bjorn Zelter ([@wendibus](https://github.com/wendibus),
  upstream PR #474), ported onto this codebase by us; our additions are the
  Match Logger replay, design-size / DPI fixes and calibration persistence.

**GUI**
- Slint renderer switched from femtovg to **Skia** (fixes wobbly text) *(PR)*,
  flat restyle with grouped cards *(PR)*, click-to-open help badges *(PR)*, update-available
  toolbar button *(PR)*, app icon, and a setup file that also restores
  the export and AI settings.

**Training tooling (`scripts/`, `docs/YOLO26_Training.md`)**
- A Label Studio based pipeline for fine-tuning the YOLO26 ball / person /
  referee detector: frame mining by model failure, upload with pre-labels,
  export and dataset merge / tiling, class-weighted training. The published
  model is on Hugging Face: [Dura-S/reco-yolo26s-football](https://huggingface.co/Dura-S/reco-yolo26s-football).
  Training frames are never published (amateur youth football).

**Windows / build fixes**
- DirectML execution provider actually compiled in, DirectML vs TensorRT
  provider conflict, D3D11VA fast start-time seek and staging-frame safety,
  concat-demuxer multi-segment seek, vendored crates that work around Windows
  Defender blocking build scripts (`vendor/`, see `THIRD_PARTY_NOTICES.md`).

New crates compared to upstream: `reco-scoreboard`, `reco-stitch-img` (image
stitching benchmark tool).

## Documentation

The user manual starts at [docs/manual/](docs/manual/README.md): the most important settings, a full settings reference generated from the app, and the AI tracking tuning guide. It is a draft and is still being written.

## Quick start

```bash
# Build the CLI
cargo build --release -p reco-cli

# Calibrate two cameras (AKAZE features + IMU/audio sync)
./target/release/reco calibrate left.mp4 right.mp4 -o match.json

# Stitch a panoramic video
./target/release/reco stitch left.mp4 right.mp4 -c match.json -o panorama.mp4

# Stitch with AI tracking (field mode: ball + players)
./target/release/reco stitch left.mp4 right.mp4 -c match.json -o tracked.mp4 \
    --model yolo.onnx --tracking field

# Pick a framing preset (broadcast | action | frame_all); --panner-config
# JSON overlays individual knobs on top
./target/release/reco stitch left.mp4 right.mp4 -c match.json -o tracked.mp4 \
    --model yolo.onnx --tracking field --panner-preset action

# Interactive preview with pan/zoom
./target/release/reco preview left.mp4 right.mp4 -c match.json

# Live camera stitching (GStreamer, Jetson CSI)
./target/release/reco camera --left-device 0 --right-device 1 \
    -c match.json -o live.mkv --container mkv \
    --model yolo.engine --tracking field
```

## Logging

Reco uses `RUST_LOG` for its own CLI/library logs. If `RUST_LOG` is unset,
the CLI defaults to `info`; set it explicitly when you want more or less
output.

```bash
# Show debug messages from Reco and dependencies
RUST_LOG=debug ./target/release/reco stitch left.mp4 right.mp4 -c match.json -o out.mp4

# Set 'info' level debug for default, 'debug' only for certain crates
RUST_LOG='info,reco_io=debug,reco_autocam=debug' \
    ./target/release/reco stitch ...

# Quieter normal run
RUST_LOG=warn ./target/release/reco stitch left.mp4 right.mp4 -c match.json -o out.mp4
```

Raw FFmpeg and libva messages are controlled separately because they write
outside Rust's logger. Reco keeps them quiet by default; opt in when debugging
codec, hardware decode, or driver setup issues:

```bash
# Show FFmpeg's own diagnostics
RECO_FFMPEG_LOG=debug ./target/release/reco stitch left.mp4 right.mp4 -c match.json -o out.mp4

# Linux: show libva driver messages again
LIBVA_MESSAGING_LEVEL=2 ./target/release/reco stitch left.mp4 right.mp4 -c match.json -o out.mp4
```

## Architecture

Eleven Rust crates (upstream's nine plus `reco-scoreboard` and `reco-stitch-img`; the
deprecated `rig-calib` prototype is not counted). Strict dependency direction keeps the
engine reusable as a library.

```
reco-core        GPU stitching engine (wgpu). No I/O, no domain logic.
reco-io          FFmpeg decode/encode, GStreamer cameras, StitchJob API, stacked-video pack/unpack.
reco-detect      Inference backends: ORT (CPU/CUDA/CoreML), NCNN, native TensorRT.
reco-autocam     Panners (field, ball, sweep), lookahead smoothing, ROI filters.
reco-calibrate   AKAZE features, stereo optimization, lens database, live-calibration source trait.
reco-control     Operator intent vocabulary (keyboard today; gopro/mobile/websocket scaffolds).
reco-cli         Terminal consumer: stitch / calibrate / preview / camera / analyze / info.
reco-gui         Slint desktop consumer with wgpu preview + export UI.
reco-obs         OBS Studio source plugin (async-frame ingestion, BGRA, interactive pan/zoom).
reco-scoreboard  HTML scoreboard package discovery + offscreen runtime (from upstream PR #474, B. Zelter).
reco-stitch-img  Standalone GPU image stitcher for pipeline benchmarking.
```

**Dependency direction:** consumers (`cli` / `gui` / `obs`) depend on the four library crates (`autocam`, `calibrate`, `detect`, `io`); all four depend on `reco-core`. `reco-control` is consumed by `cli` / `gui` / `obs`.

Push-based is the canonical ingestion path: consumers call `StitchCore::submit_frame_yuv` / `submit_frame_bgra` per frame. Batch file processing (`StitchSession::run`) is a thin pull-adapter on top.

## Building

Prerequisites: Rust **1.92+** (workspace MSRV), FFmpeg development libraries, pkg-config, clang.

```bash
# Ubuntu / Debian (apt)
sudo apt install libavcodec-dev libavformat-dev libavutil-dev \
    libswscale-dev libavdevice-dev libavfilter-dev libswresample-dev \
    pkg-config clang

# macOS (Homebrew)
brew install ffmpeg pkg-config

# Windows: FFmpeg 7.x binaries + LLVM/Clang, set FFMPEG_DIR env var

# Build everything
cargo build --release
```

**Windows runtime note:** the FFmpeg DLLs (`avcodec-61.dll`, `avdevice-61.dll`,
`avfilter-10.dll`, `avformat-61.dll`, `avutil-59.dll`, `postproc-58.dll`,
`swresample-5.dll`, `swscale-8.dll`) are required next to `reco-panopitch.exe` /
`reco-cli.exe` at run time, not only at link time. `cargo build` does not copy
them into `target/debug` or `target/release` automatically, so running a
freshly built exe straight from `target/` without copying these DLLs next to
it fails with `avformat-61.dll was not found` (or a similar message) at startup. Copy
them from `$FFMPEG_DIR/bin` (or from an existing packaged release, e.g.
`reco-panopitch-vX.Y.Z-windows-x86_64/`) into the same folder as the exe.

### Feature flags

| Feature | Crate | Purpose |
|---|---|---|
| `ort` (default) | reco-detect | ONNX Runtime inference (CPU + CUDA + CoreML EPs) |
| `tensorrt-native` | reco-detect | Native TensorRT for `.engine` files (Jetson, no ORT glibc dep) |
| `ncnn` | reco-detect | NCNN backend for mobile / embedded |
| `load-dynamic` | reco-detect | Dynamically load `libonnxruntime.so` at runtime |
| `gstreamer` | reco-io | GStreamer camera ingest (Linux / Jetson CSI) |
| `stacked-output` | reco-io | FFmpeg-backed stacked-video encoder / source |
| `profiling` | workspace | `tracing` + `tracing-chrome` instrumentation, zero-cost when off |
| `gopro` | reco-control | Optional OpenGoPro device helper (recording control) |

Example combined build:

```bash
cargo build --release -p reco-cli --features tensorrt-native,gstreamer,profiling
```

## Development

```bash
cargo test --all              # Run all tests
cargo clippy --all-targets -- -D warnings   # Lint (zero warnings policy)
cargo fmt --all -- --check    # Format check
cargo doc --no-deps --open    # Generate docs
cargo deny check              # Supply-chain gates (advisories / licenses / bans / sources)
```

Profiling a 300-frame stitch:

```bash
cargo run --release -p reco-cli --features profiling -- \
    stitch left.mp4 right.mp4 -c match.json -o out.mp4 --max-frames 300
# Produces reco-trace.json — open in https://ui.perfetto.dev
```

## Privacy

RECO Panopitch collects no usage data and sends no telemetry. The only network request it makes on its own is one anonymous GET to the GitHub releases API at startup to check for a newer version, plus those you explicitly configure (RTMP streaming, etc.). Bug reports are copied to your clipboard; nothing is uploaded. All performance statistics shown in the Stats panel are computed locally in memory and never leave your machine.

## Acknowledgements

This fork exists because of other people's work. Thank you:

- **Mohamed Taha Guelzim** and the **reco-project** community, authors of
  [reco-project/video-stitcher](https://github.com/reco-project/video-stitcher):
  the stitching engine, AI tracking, calibration and apps this fork is built on.
  Also **Jordan Speicher** (audio start-time sync, VAAPI upload, quieter default
  logging, responsive file dialogs, concat seeking) and **zzswanny**
  (frame_all GUI options) for their upstream contributions.
- **Bjorn Zelter** ([@wendibus](https://github.com/wendibus)): the HTML scoreboard
  system (`reco-scoreboard`, `scoreboards/`, upstream PR #474), which we ported.
- **[Gyroflow](https://github.com/gyroflow/gyroflow)** and **Adrian Eddy**:
  the architecture that inspired our GPU interop, the CC0 lens-profile database
  and the `telemetry-parser` crate used for IMU data.
- **Adit-jain** ([soccana](https://huggingface.co/Adit-jain/soccana) on Hugging Face):
  the football detection model we used to pre-label training frames.
- **[Ultralytics](https://github.com/ultralytics/ultralytics)** (YOLO26 and the
  training toolchain), **[Label Studio](https://github.com/HumanSignal/label-studio)**
  (annotation), and the authors of
  [SAHI](https://github.com/obss/sahi), whose tiled-inference idea our dual-crop
  training and inference follow.
- Open-source libraries we rely on, among them FFmpeg, wgpu, Slint and Skia,
  ONNX Runtime, the `akaze` crate by Christopher22, headless_chrome, and the
  Roboto font by Google. Notices and licenses: [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
- Everyone who tested footage, reviewed thousands of training frames and gave
  feedback on the forum.

## License

**AGPL-3.0-only.** See [LICENSE](LICENSE). Contributor terms are in [CONTRIBUTING.md](CONTRIBUTING.md#contributor-license-agreement): by submitting a PR you grant the maintainer the right to relicense contributions, which keeps dual-licensing on the table for future commercial distribution.

Questions about commercial licensing can be directed to the project maintainer; there is no public paid tier today.

## Community

- [Issues](https://github.com/RufanMelfor/video-stitcher/issues) - Bug reports + feature tracking
- [Discussions](https://github.com/RufanMelfor/video-stitcher/discussions) - Questions, showcase, feature requests
- [Upstream project](https://www.reco-project.org) - reco-project, the original this fork is based on
- [v1 archive](https://github.com/reco-project/video-stitcher-v1) - Legacy Electron / Python version (predecessor to this Rust rewrite)
