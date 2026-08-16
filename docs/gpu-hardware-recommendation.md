# GPU hardware recommendation

Notes for a future GPU purchase, based on real measurements from the
2026-08-16 async-detect optimization session (not vendor specs or
guesswork). Reference machine for all numbers below: RTX 3060 Ti,
8192 MiB VRAM. Full raw data: see the "Async-Detect Telemetry"
artifact linked from `SESSION_HANDOFF.md` / the
`project_async_detect_thread_design` memory note.

## What the pipeline actually spends GPU budget on

Measured via `--features profiling` traces on a real 300-frame export
(TensorRT FP16, `yolo26s_tiled1920_full`, 1920px input):

- **~90% of wall-clock time is TensorRT inference** (`yolo_inference`
  span) - compute-bound, not memory-bandwidth-bound.
- **No benefit from running two inference contexts concurrently**
  (tried: a dual-worker mode running Left/Right camera inference on
  two threads at once). Measured result: the per-call cost nearly
  *doubled* (40.1ms -> 77.3ms) once two TensorRT contexts contended
  for the same compute units, eating almost all of the theoretical
  parallelism gain, and a live GUI export showed no real speedup at
  all. This consumer GPU has no MPS (Multi-Process Service, a
  datacenter-tier feature) to truly time-slice multiple contexts -
  **more CUDA cores for concurrency does not help this workload**.
- **VRAM is already tight**: 6.3-7.3 GiB used out of 8 GiB during a
  normal export (lookahead buffer + two loaded detector model
  instances + render targets + NVENC), depending on which features are
  active. No headroom left for a longer lookahead buffer or a bigger
  model on this card.
- Encode (NVENC, h264_nvenc) and decode (NVDEC via D3D11VA zero-copy)
  together account for roughly 15% of the loop - real, but not the
  bottleneck.

## What this means for a purchase

**Don't pay for**: high CUDA-core-count aimed at multi-context
parallelism, or datacenter/workstation cards with MPS support - this
pipeline's workload (one sequential inference call per camera per
frame) doesn't benefit from that, confirmed by direct measurement, not
assumption.

**Do pay for**, in priority order:

1. **VRAM: 12 GiB minimum, 16 GiB comfortable.** Directly fixes the
   observed 8 GiB squeeze; headroom for a longer lookahead buffer or a
   larger detection model later.
2. **Newer tensor-core generation (Ada Lovelace > Ampere).** This is
   the lever that actually reduces the 90%-of-time bottleneck - raw
   per-call FP16 inference throughput, not core count.
3. **Recent-generation NVENC/NVDEC** (8th-gen or newer) - relevant
   since `reco stitch --codec av1` already exists in the CLI; newer
   encoders support AV1 and are more efficient per watt.
4. **Must be NVIDIA.** TensorRT is the confirmed-fastest execution
   path in this codebase; the DirectML fallback (AMD/Intel GPUs) is
   measurably slower, per an earlier same-project finding (TensorRT
   vs. DirectML backend-selection bug investigation, 2026-08-13).

## Suggested tiers

Picked for the project's actual audience (amateur/club budgets, not
enterprise spend):

| Tier | Card | Why |
|---|---|---|
| Budget | RTX 4060 Ti 16GB | Fixes the VRAM ceiling; modest compute uplift over the 3060 Ti |
| **Sweet spot** | **RTX 4070 Super (12GB) / RTX 4070 Ti Super (16GB)** | Clear Ada tensor-core jump over the 3060 Ti + comfortable VRAM margin |
| Future-proof | RTX 4080 Super (16GB) / RTX 4090 (24GB) | Headroom for higher-res sources, longer lookahead, or a bigger detection model |

## Caveat

All of the above is inferred from *this* pipeline's current bottleneck
shape (single-stream sequential TensorRT inference, no MPS benefit).
If the architecture changes later (e.g. a genuinely batched multi-
camera inference path ships - see the "batch L+R" idea, measured at a
modest 1.06x and paused, in the async-detect design note) the compute/
VRAM tradeoff could shift and this recommendation should be revisited.
