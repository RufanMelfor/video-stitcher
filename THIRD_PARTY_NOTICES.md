# Third-Party Notices

## Acknowledgments

**Gyroflow** - The CUDA/Vulkan GPU interop architecture was inspired by
[Gyroflow](https://github.com/gyroflow/gyroflow)'s approach to zero-copy
GPU frame sharing. All implementations in reco are original.

**Gyroflow Lens Profiles** - The lens profile database is converted from
the [Gyroflow lens_profiles](https://github.com/gyroflow/lens_profiles)
repository, which is released under CC0-1.0 (public domain).

**telemetry-parser** - IMU telemetry extraction uses the
[telemetry-parser](https://github.com/AdrianEddy/telemetry-parser) crate
by Adrian Eddy (MIT OR Apache-2.0).

**AKAZE** - Feature detection is based on
[Christopher22/akaze](https://github.com/Christopher22/akaze) (MIT),
with local bug fixes.

**Lens Distortion** - The Kannala-Brandt fisheye model follows the
published paper and OpenCV documentation.

**Roboto** - The "PAUZE" cut-range overlay renders text with the
[Roboto](https://github.com/google/fonts/tree/main/ofl/roboto) variable
font by Google, vendored at
`crates/reco-core/assets/fonts/Roboto-Variable.ttf` under the SIL Open
Font License 1.1 (full text: `crates/reco-core/assets/fonts/Roboto-OFL.txt`).

## Upstream project and contributors

This repository is a modified version of
[reco-project/video-stitcher](https://github.com/reco-project/video-stitcher)
(AGPL-3.0), created by **Mohamed Taha Guelzim** and the reco-project
contributors. Commits by upstream contributors, including **Jordan Speicher**
(audio start-time sync, VAAPI hardware frame upload, quieter default logging,
GUI responsiveness during file dialogs, concat-seek fixes) and **zzswanny**
(frame_all GUI options), remain in this repository's history with their
original authorship wherever they were merged.

## HTML scoreboard overlay

`crates/reco-scoreboard/` and `scoreboards/` (manifest-driven package
discovery, offscreen Chrome/CDP renderer, scoreboard SDK, designer and agent
guides, the football and basketball reference packages) are the work of
**Bjorn Zelter** ([@wendibus](https://github.com/wendibus)), submitted upstream
as pull request #474 of reco-project/video-stitcher. This fork ported them onto
its own codebase and added Match Logger replay, design-size / DPI fixes and
calibration persistence on top. Licensed under the same AGPL-3.0.

## Detection models and training tools

- **Ultralytics YOLO26** (AGPL-3.0) is the detector architecture and training
  toolchain. Fine-tuned checkpoints published by this fork are AGPL-3.0 too.
- **Adit-jain/soccana** ([Hugging Face](https://huggingface.co/Adit-jain/soccana))
  produced the first-pass pre-labels that humans then corrected. The model page
  lists no license; none of its weights are redistributed here.
- **Label Studio** is used for annotation and is not bundled.
- **SAHI** ([obss/sahi](https://github.com/obss/sahi)): the left/right
  tiled-crop training and inference follows its sliced-inference idea. No SAHI
  code is included.
- Training frames and the datasets built from them show amateur youth football
  and are intentionally not published.

## Other dependencies

Built on FFmpeg (the CI downloads BtbN's GPL builds), wgpu, Slint with its
Skia renderer, ONNX Runtime, NVIDIA TensorRT and CUDA, GStreamer, the
`headless_chrome` crate and many more crates; see `Cargo.lock`. Crates under
`vendor/` are patched copies of wgpu, wgpu-core, wgpu-hal, naga, serde,
serde_core, serde_json, thiserror, aligned-vec, keyboard-types and
khronos_api; each keeps its own license file (khronos_api is Apache-2.0).
