# reco-detect friction log

## TensorRT's engine cache can silently serve a stale engine built for a different model's shape

**Symptom (found 2026-09-19, recurred 2026-09-20):** after swapping in a
new ONNX model with a different input shape (e.g. switching from a
1920x1920 square model to a 2560x1920 non-square one), every detection
call fails with `ort run: ... Got invalid dimensions for input: images`
or (via the CLI's raw ORT logging) `IExecutionContext::setInputShape:
... Static dimension mismatch ... Set dimensions are [1,3,H,W].
Expected dimensions are [1,3,H',W']`. In `reco-gui`, this is silent
and easy to miss: the export itself completes normally (all frames
processed, output file written, no crash), it just has zero detection
overlays anywhere in it - the failures only surface as repeated `WARN`
lines in `reco-gui.log`, not as an export error, because detection
failures are logged and skipped per-frame rather than aborting the run.

**Root cause:** ONNX Runtime's TensorRT execution provider caches
compiled engines under `reco_cache_dir("trt-cache")`
(`%LOCALAPPDATA%\reco\trt-cache` on Windows) keyed by a hash of the
ONNX graph, not by the model file's path or content hash. That
graph-hash key has been observed to collide across genuinely different
models exported around the same time (different input shape, different
training round) - the cache then hands back an engine literally
compiled for the *other* model's tensor shape, and TensorRT correctly
rejects the mismatched input at inference time. `reco-gui`'s wgpu
preprocessing path (`WgpuPreprocessor`, see `reco-detect::input_size`)
builds the correct-shaped tensor for whichever model is actually
loaded - confirmed via `reco-gui.log`'s
`WgpuPreprocessor: ... -> <correct new shape>` line - so this is purely
a TensorRT-side cache bug, not a preprocessing bug. It is most likely
to bite right after exporting a new non-square model to the same
weights directory an older square export used, or after any ONNX
re-export during active model iteration (training smoke tests, epoch
snapshots), which is exactly the workflow the 2026-09-19/20 non-square
`InputSize` work introduced.

**Fix (workaround, not yet a code fix):** clear
`%LOCALAPPDATA%\reco\trt-cache\` (all `*.engine` and `*.timing` files)
whenever a freshly-exported ONNX model produces zero detections despite
`reco-gui.log` showing the correct preprocessed shape. TensorRT
rebuilds every engine fresh on the next run (a few minutes of one-time
recompilation per model), and detections work again with no other
change. No data is lost - the cache directory is pure build output.

**Not yet fixed at the root:** `reco_cache_dir`/`create_ort_session`
(`crates/reco-detect/src/ort_session.rs`) should key the TensorRT cache
by something that actually distinguishes models - the ONNX file's
content hash or its path, not letting TensorRT derive the key from the
graph alone. Until that lands, clear the cache by hand after any model
swap during active training/export iteration rather than trusting a
zero-detection run to mean the model itself is bad.

## A TensorRT EP that fails to load is reported as enabled (silent fallback)

**Symptom (found 2026-11-23, release zip v1.0.0-beta.1):** the first
Windows release zip shipped without `onnxruntime_providers_shared.dll`
and the other provider DLLs. `reco-panopitch.log` then showed, in this
order:

```
ERROR ort::ep: An error occurred when attempting to register `TensorrtExecutionProvider`: ... Error loading ...\onnxruntime_providers_shared.dll which is missing (126)
WARN  ort::ep: No execution providers from session options registered successfully; may fall back to CPU.
INFO  reco_panopitch: AI: TensorRT, DirectML, CPU (hardware decode + inference)
```

The app start-up status line claims TensorRT is available, and nothing in
the UI says otherwise. The only trace is the ERROR/WARN lines in the log.
`ort_session.rs` has the same blind spot: it logs "ORT: TensorRT
execution provider enabled" right after `with_execution_providers`
returns `Ok`.

**Root cause:** in ort 2.0.0-rc.12 `SessionBuilder::with_execution_providers`
returns `Ok` when an EP fails to register at runtime; the failure is only
logged (`ort/src/ep/mod.rs`, "may fall back to CPU") unless the EP is
built with `.error_on_failure()`. So both `probe_execution_providers`
(`probe.rs`, which `reco-gui` shows via `ai_capability_summary`) and
`create_ort_session` treat a failed registration as success. The probe's
doc comment promises exactly the opposite ("will report TensorRT as
unavailable if the TensorRT shared libraries are not installed").

**Impact:** a user without the NVIDIA libraries (CUDA, TensorRT, cuDNN)
on PATH, or with a missing provider DLL, gets slower inference with no
visible reason. Observed on the first zip (2 starts, both with the error
above); the fixed zip registered TensorRT and exported 2733 frames at
16.12 fps.

**Fixed 2026-11-23:** `probe.rs` and `ort_session.rs` now build the
TensorRT and CUDA EPs with `.error_on_failure()`, so a failed registration
returns `Err` (probe: goes into `errors` and TensorRT drops out of
`providers`; session: the existing warn + `recover()` fallback runs).
`reco-gui` needed no change, its status line already prints `probe.errors`.
Checked with a throwaway example: with the provider DLLs next to the exe
the probe returns `["TensorRT", "DirectML", "CPU"]`; with the exe alone
(like the broken first zip) it returns `["DirectML", "CPU"]` plus the
`TensorRT: ... Error loading ...onnxruntime_providers_shared.dll` error.
Not done: DirectML and CoreML EPs still use the default (silent) mode, and
the case where only the NVIDIA libraries (not the provider DLL) are missing
was not tried.
