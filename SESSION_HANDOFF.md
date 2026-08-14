# Session handoff - 2026-08-13 (TGR_PC, continues 2026-08-12)

Continuation note for resuming work on a different machine/session -
git-tracked so it travels with `git pull`/`push` between the user's two
PCs. Overwrite wholesale at the end of a session rather than appending
history - git history is the append-only log, this file is just "where
things stand right now."

**No usernames/passwords/IP addresses in this file, ever** - see
feedback_no_credentials_in_tracked_files.md. Reference "see password
manager" / `zerotier-cli listnetworks` instead.

## Immediate state / what to do next

**YOLO26 round 4 done: yolo26s on the 258-task set (+winC hard
frames), plus a copy_paste/rect experiment - full detail in
`YOLO26_Training.md`'s "Round 4" section, condensed here.** Ball
recall is still low (0.985 precision / 0.444 recall, mAP50 0.465) - a
quick `copy_paste=0.3 rect=True` experiment (user-requested, before
collecting more data) did NOT move recall (0.435, within noise) - only
a small mAP uptick. **Conclusion: data quantity/diversity is the real
bottleneck, not training config** - the next real lever is more
labeled hard-frame data, not more hyperparameter tuning.
- Checkpoints: `round4/runs/{yolo26s_v4_3class_1280_b4_e300,yolo26s_v4_copypaste_rect}/weights/{best.pt,best.onnx}`,
  both ONNX-verified.
- **Real gotcha hit+fixed**: first training attempt crashed
  (`OSError: [WinError 1455]`, paging file too small) with the default
  `workers=8` - `reco-gui.exe` was open using ~3.9GB RAM at the time.
  Fixed with `workers=2`, retried clean, actually faster (41 min vs
  round3's 66). Didn't close `reco-gui.exe` unasked.
- **Self-correction, important**: an earlier claim in this same
  investigation ("the ball visually merges with a player during a
  dribble", from a specific frame-665 screenshot) was **wrong** -
  user pushed back ("dat weg smelten heb ik niet echt gezien"),
  re-tested with fresh evidence (not the old screenshot) and the round-4
  model, and the ball was actually isolated elsewhere in frame - I'd
  pointed at the wrong pixel location. Corrected in memory. **Lesson
  applied going forward**: when a user disputes a specific visual
  claim, regenerate the evidence fresh, don't defend from memory.
- **Round-4 now tested in the real `reco` app (2026-08-14), confirms
  the LS-validation-set conclusion on real footage.** Full 899-frame
  CLI render of `yolo26s_v4_3class_1280_b4_e300` against the same
  100-130s 03 OJC clip used for every prior ball_weight/FOV-Wide/Ball-
  reach A/B test, current full recommended settings applied
  (`--player-anchor-rad 0.4 --panner-preset action --panner-config`
  with `cluster_mode trimmed_mean, ball_weight 0.5, dead_zone_rad 0.07,
  ball_max_dist_from_cluster 1.0, fov_wide 68` `--fov-alpha 0.06
  --cluster-alpha 0.06 --lookahead 0.5 --lookahead-reduced-bit-depth`),
  built with `--features tensorrt`. Ran clean end-to-end, no crashes -
  the checkpoint itself is not the problem, recall is.
  - Overall raw-ball-detection rate: 441/899 frames (49.1%) - flat vs
    `yolo26s_v3`'s documented 48.7% on the same clip, no real gain.
  - **Frames 720-898** (`yolo26s_v3`'s documented zero-raw-detection
    gap across all 3 prior A/B renders): still **0/179 frames** with a
    raw ball detection at round-4 too. `world_state.ball` confirms:
    158/179 frames have no `ball` entry at all, 20 `Coasting`, 1
    `Lost` - genuinely untracked the whole stretch, unchanged from v3.
  - One brighter spot: frames 690-719 (the window dumped for LS
    review) hit 24/30 = 80% raw-detection at round-4 - but confidence
    is modest clip-wide (mean 0.46, range 0.10-0.92).
  - Confirms this session's earlier conclusion on the LS val set from a
    second, independent angle (real match footage, not just mAP): data
    quantity/diversity is the bottleneck, not training config. The
    720-898 gap is the clearest concrete target for the next hard-frame
    batch.
  - Test artifacts kept (not deleted, per
    [[feedback_keep_test_artifacts]]): `round4_test_100-130.mp4` +
    `.events.jsonl` in this session's scratchpad - not yet copied
    anywhere permanent.
  - **Still not done**: no rigorous unseen-frame recall test across
    multiple clips (this is one clip, one 30s window - a real full-clip
    test, not a spot-check, but still a single sample).
  - **Follow-up: frame-704 localization miss found + a stopped
    imgsz=1536 experiment (2026-08-14).** Physically re-viewed the
    690-719 dump with round-4's own predictions drawn (per
    [[feedback_no_credentials_in_tracked_files]], see password manager
    for the LS token used transiently). Frame 704's ball box (conf 0.55)
    landed ~20px off the LS-corrected ground truth - real but modest,
    less dramatic than an unreferenced crop first suggested. **Real
    process mistake, caught by the user**: re-pushed this same frame to
    LS as a "new" task without noticing it was already there as
    `winC_014` (task 1951, annotated 2026-08-13) - the winC batch *is*
    this tool's frame690-719 dump, just under an opaque renamed
    filename with no `frame_index` in it. Deleted the duplicate task
    immediately; **lesson for any future LS push: always keep
    `frameNNN_<Camera>` (or the source frame_index) in the filename**,
    never rename to an opaque batch name first.
    Tried `imgsz=1280 -> 1536` to see if more resolution tightens ball
    box precision (same data/hyperparams otherwise). Ran to epoch 187,
    manually stopped - best checkpoint stayed at epoch 28 the whole run
    (159 epochs with no improvement), and that checkpoint's ball metrics
    were *worse* than round-4's (mAP50-95 0.292 vs 0.321, recall 0.398
    vs 0.444) - only the diluted "all" number was better. Not a fair
    comparison (round-4 trained to its own patience-stop at epoch
    169/best@69, this run's best is from a much earlier relative point)
    - genuinely unresolved, not repeated further this session. Checkpoint
    kept at `.../yolo26s_v4_imgsz1536/weights/` in case worth resuming
    later, ideally overnight/unattended per the user's own suggestion.
    **Follow-up, same day: real-app tested anyway (ONNX-exported,
    `imgsz=1536`).** Mixed result vs round-4 on the same clip: overall
    raw-ball rate worse (37.0% vs 49.1%), but frames 720-898 - zero raw
    detections in *every* prior test ever run on this clip - got a
    nonzero hit rate for the first time (15/179). Frame 704 specifically:
    confidence rose 0.55->0.82 but the box landed at virtually the same
    (still ~20px off) location - the original localization complaint
    didn't improve, only confidence in the same slightly-wrong spot did.
    **Real gotcha hit+fixed**: first attempt returned 0 detections of
    any class, silently (no crash) - root cause was a stale TensorRT
    engine-cache collision (`%LOCALAPPDATA%\reco\trt-cache\` reused
    round-4's cached 1280-shaped engine for this 1536-shaped model,
    shape mismatch silently no-op'd inference). Fixed by clearing the
    cache dir and re-running. **Lesson for every future session: always
    clear `trt-cache` before testing a model with a different imgsz than
    whatever was last cached** - see [[project_tensorrt_sdk_setup]].
    **Next candidate lever, research-only, not built**: splitting each
    3840x2880 frame into overlapping left/right 2880x2880 square crops
    (SAHI-style tiling) as training-data augmentation - zero letterbox
    waste, more effective ball resolution, without imgsz's VRAM/
    BatchNorm risk. See [[project_yolo26n_training_pipeline]] for full
    detail.
- See [[project_yolo26n_training_pipeline]] and
  [[project_dump_detection_frames_tool]] for full detail.

**TensorRT now installed and working end-to-end on this PC, plus a
real crash bug found+fixed - merged into `main` (`a140731b`,
`ecf5ad9c`).** User asked why the Export dialog showed "AI: DirectML
(CPU path...)" and wanted TensorRT since they believed it was already
installed.
- Root causes found, in order: (1) reco-gui/reco-cli default features
  don't include `tensorrt` - needs `--features tensorrt` explicitly;
  (2) the zip the user had was the **TensorRT-OSS GitHub source repo**
  (parsers/plugins/samples, zero DLLs) - not the real NVIDIA SDK
  binary distribution, an easy mix-up; (3) cuDNN was also missing -
  NVIDIA's download page only offered arm64 for Windows at this
  version, real fix was the `nvidia-cudnn-cu13` PyPI wheel (has a
  win_amd64 build) via `pip install --target`.
- Installed permanently: TensorRT 10.16.1 +
  `nvidia-cudnn-cu13`/`nvidia-cublas` under `D:\SOFTWARE\`, added to
  the persistent User PATH via PowerShell
  `[Environment]::SetEnvironmentVariable(...,'User')` (not `setx` -
  the existing PATH is long enough that `setx` risked truncating it).
- **Second, more serious bug found+fixed**: first `--features
  tensorrt` reco-gui build crashed every export with "AI tracking
  failed: DML EP can only be used with CPU EPs" - reco-gui's Cargo.toml
  unconditionally forces `directml` on Windows regardless of other
  features, so this was the first binary ever combining TensorRT +
  DirectML in one ORT session (ORT hard-rejects that combination).
  Fixed in `reco-detect/src/ort_session.rs`: DirectML is now only
  queued when neither `tensorrt` nor `cuda` is compiled in. Verified
  by reproducing with `reco-cli --features tensorrt,directml` (crashed
  before, clean after) and confirming a full 899-frame export with
  real AI tracking completes end-to-end on TensorRT.
- **Confirmed this bug also exists in upstream `v0.5.4`** (identical
  code, identical forced-directml Cargo.toml) - opened
  [PR #467](https://github.com/reco-project/video-stitcher/pull/467)
  against `reco-project/video-stitcher`, cherry-picked cleanly onto
  `origin/main`, tested there too.
- **IMPORTANT for future sessions on this machine**: always add
  `--features tensorrt` when rebuilding reco-gui/reco-cli for this
  user, and make sure the three PATH entries are exported in the build
  shell (`FFMPEG_DIR`-style, see env_build_requirements.md) - a plain
  `cargo build` still works but silently regresses to DirectML with no
  warning. See [[project_tensorrt_sdk_setup]].

**New debug tool: `dump_detection_frames` - merged into `main`
(`a9b30bfe`).** User said the AI "still isn't great" after the
ball_weight fix; asked me to investigate where/why the model misses
the ball. Manual `ffmpeg -ss` frame extraction was too imprecise
(keyframe-seek, kept landing on the wrong frame). Built
`crates/reco-io/examples/dump_detection_frames.rs`: given the source
videos + `events.jsonl` + calibration + the original start-time/sync-
offset, decodes the *exact* detector-input frame sequentially (same
technique as `reco-calibrate/examples/dump_undistorted.rs`, not a
lossy seek) and draws detection boxes + the field ROI polygon on top.
`--clean` skips all overlay drawing for Label-Studio-ready frame
exports (full native res).
- Found: a high-confidence ball detection right before a tracking
  interruption was visually **merged with a player's body** during a
  close dribble; a separate low-confidence "last detection" before a
  179-frame gap turned out to be a **false positive on a player's
  shoe**, not the ball.
- **User's own catch, confirmed via the new ROI overlay**: a real ball
  sighting sat visibly outside the field ROI polygon on one frame -
  `RoiFilteredDetector` drops out-of-ROI detections *before*
  `detections_raw`, so this failure mode is invisible without drawing
  the ROI too. **User decided not to widen the ROI** - it's
  intentionally tight to keep out a kid playing with their own ball
  outside the actual pitch.
- **Next step (user's, not yet done by me)**: dumped 30 clean frames
  (690-719, right camera only) to session scratchpad
  (`ls_review_frames/frame690_Right.png`...`frame719_Right.png`, not
  yet copied anywhere permanent) for the user to review themselves in
  the "Finetuned yolo26n (rough v1)" Label Studio project - a new
  hard-frame training batch. See [[project_dump_detection_frames_tool]].

**Ball weight raised 0.35 -> 0.5, validated via real CLI A/B renders I
ran myself - merged into `main` (`9cf3fede`). Also found+fixed a real
VramPool crash bug, and found (not yet fixed) a deeper one.** User
reported a new test ("Ai Planner Test v9") still lost the ball around
frame 704 despite the fov_alpha/cluster_alpha fix below, then asked me
to run the A/B tests myself against the real 03 OJC 100-130s clip
instead of iterating manually in the GUI each time.
- Root-caused by computing `field.rs`'s actual `target_pitch =
  (cluster_pitch + pitch_bias)*(1-w) + ball_pitch*w` blend against the
  real v9 events: at `ball_weight 0.35`, when the ball breaks toward
  the near touchline (drops in *pitch* while players stay up-pitch),
  the blended aim target never gets close enough to the ball - not a
  gate problem (Ball anchor range/Ball reach/FOV Wide all already
  correct) or a smoothing problem (fov_alpha/cluster_alpha already
  raised), a blend-weight problem specific to vertical separation.
- **Verified via 3 real CLI renders** (`reco.exe stitch --start-time
  100 --end-time 130` on the actual `DJI_20260704095935_0028_D_L01` /
  `0029_D_R01` pair + `DJI Action4 Final_1.json` + `yolo26s_v3`,
  everything else held constant): `ball_weight 0.35` reproduces the
  exact symptom (ball outside half-FOV for 15 frames, 696-710);
  `0.50`/`0.60` both fully fix it. Cost: +33%/+32% mean/p95
  frame-to-frame camera movement at 0.5 vs 0.35 - real but far short of
  the wobble `1.0` was already known to cause. Docs updated (EN+NL):
  Ball weight recommendation 0.35 -> 0.5, corner-breakaway checklist
  now 5 steps (added Ball weight as the 5th).
- **Also found, separate, not settings-fixable**: frames 720-898 (last
  ~6s of the 100-130s window) have zero raw ball detections at all
  across all three renders - a genuine `yolo26s_v3` recall gap on this
  clip. Noted in the doc's "Model" paragraph.
- **Real bug found+fixed along the way**: the first CLI attempt (Native
  bit depth, matching the docs' then-current "off, only on a VRAM
  error" recommendation) crashed immediately with a wgpu validation
  panic (`RENDER_ATTACHMENT not allowed on R16Unorm`), misreported by
  the caller as "VRAM allocation failed". `VramPool::new`
  (`crates/reco-core/src/session/vram_pool.rs`) requested
  `RENDER_ATTACHMENT` on every pool texture unconditionally on a
  stated-but-false "harmless otherwise" assumption - P010's Y plane
  isn't renderable on this backend regardless. **Fixed**: only request
  it when the downconvert pass actually needs it.
- **Second, deeper bug found, NOT fixed** (documented in place in
  `copy_from_d3d11`'s doc comment, needs its own session): after that
  fix, texture *creation* succeeds but the actual D3D11 zero-copy plane
  copy still fails - `Source format (P010) and destination format
  (R16Unorm) are not copy-compatible`. **Practical upshot**:
  `LookaheadBitDepth::Native` is currently broken end-to-end for any
  10-bit source under zero-copy with lookahead on -
  `--lookahead-reduced-bit-depth` is a hard requirement right now, not
  an optional VRAM fallback. Corrected the CLI help text, GUI tooltip,
  and both docs (previously all three said some version of "leave off
  unless you hit a VRAM error", which would crash any 10-bit-source
  user who followed that advice).
- Build+test verified on merged `main`: `cargo test -p reco-core --lib`
  182/182 (excluding the 2 pre-existing unrelated CUDA-context
  failures), `cargo fmt`/`cargo clippy` clean. Both debug and release
  `reco-gui.exe`/`reco.exe` rebuilt from merged `main`. Pushed
  (`main` + `fix/vram-pool-native-10bit-render-attachment`).
- See [[project_ball_weight_vertical_break_fix]] for full detail.

**AI Tracking / panner settings now auto-persist app-wide, no Save
calibration needed - merged into `main` (`e96dfb5f`).** User asked, right
after the `fov_alpha`/`cluster_alpha` feature below shipped: "save all
AI planner parameters as soon as they change, I don't want to re-enter
them every time I restart the program." The existing
`Calibration::autocam_defaults` only survives a restart if you
explicitly click **Save calibration** - this closes that gap one layer
up.
- New `GuiSettings::autocam_defaults` (`crates/reco-gui/src/settings.rs`,
  `<config>/reco/gui.json`) - same `AutocamDefaults` struct, but
  app-level and independent of any calibration.
- `main.slint`: new `autocam-settings-changed` callback, fired by
  `changed export-xxx => {...}` on all 18 AI Tracking/panner properties
  (every field `AutocamDefaults` covers). Not wired for
  `export-model-path` (already has its own MRU-style persistence) or
  progress/status fields.
- `main.rs`: factored the previously-duplicated 18-field snapshot/restore
  code (was inline in both `do_save_calibration` and
  `try_init_and_update`) into shared `snapshot_autocam_defaults()` /
  `apply_autocam_defaults()` helpers, now used by three call sites:
  calibration save, calibration load, and the new save-on-change
  handler.
- **Restore priority, in order**: `GuiSettings`' last-used values apply
  at startup, before any video/calibration is loaded; a loaded
  calibration's own `autocam_defaults` then overrides them if present -
  calibration-level priority unchanged, just with a real fallback
  underneath instead of hardcoded `.slint` literals.
- Docs updated (EN+NL) to explain the auto-save/restore + priority order.
- Tests: 3 new in `settings.rs` (JSON roundtrip, absent-until-set,
  missing-field backward compat). `cargo test -p reco-gui --bin
  reco-gui settings::` - 12/12 pass. `cargo fmt --check` and `cargo
  clippy -p reco-gui` clean (modulo the two pre-existing, unrelated
  issues noted below).
- **Nothing outstanding** - build+test verified on merged `main`,
  pushed. Next slider drag in the Export dialog should already persist;
  next app restart should already restore it without touching a
  calibration file.

**`fov_alpha`/`cluster_alpha` (Zoom/Aim smoothing speed) now tunable,
merged into `main` (`40c532f3`).** Root-caused from a real trace ("Ai
Planner Test v7"): user reported "from frame 701 I no longer see the
ball." Even with Ball anchor range, Ball reach, and FOV Wide all raised
correctly, `FieldPannerConfig`'s own smoothing rates
(`fov_alpha`/`cluster_alpha`, both already fields, never wired to any
consumer) default to ~0.01/0.012 - a ~3s time constant at 30fps. On the
trace: FOV climbed 38.7 -> only 39.9deg (target was past 65deg) over the
~20 frames the ball stayed trackable; aim pitch barely moved while the
ball's pitch shifted 0.24 rad in the same window. The computed target
was correct - the smoothing just hadn't caught up before the ball left
frame.
- `reco-core`: `fov_alpha`/`cluster_alpha` added to
  `Calibration::AutocamDefaults` (and so also to the events.jsonl
  `run_config` header - see below). `#[serde(default = "...")]` falls
  back to `FieldPannerConfig`'s own defaults (0.01/0.012), not `0.0`
  ("never move"), for calibrations saved before this field existed.
- `reco-cli`: new `--fov-alpha`/`--cluster-alpha` flags, applied last
  (highest priority) over `--panner-preset`/`--panner-config`; starts
  from `FieldPannerConfig::default()` if neither preset nor config file
  was given but one of these flags was.
- `reco-gui`: two new sliders ("Zoom (FOV)", "Aim (cluster)") under a
  new "Smoothing speed" subsection in Advanced panner, wired through
  the same 4 sites as every other panner slider this session.
- Docs (EN+NL): new explainer section citing the v7 trace numbers, added
  as gate #4 on the corner-breakaway checklist (Ball anchor range ->
  Ball reach -> FOV Wide -> Zoom/Aim smoothing), removed from "not yet
  exposed in the GUI".
- Verified via a real CLI run (`--fov-alpha 0.06 --cluster-alpha 0.05`):
  events.jsonl's `run_config` line reflects both values correctly.
- **Also fixed in passing**: two pieces of pre-existing `cargo fmt`
  debt from earlier this session's Ball-anchor-range feature
  (`crates/reco-autocam/src/lib.rs`) - unrelated to this feature,
  committed separately (`40c532f3`'s parent).
- **Still pre-existing, not touched, both block a clean
  `cargo clippy --all-targets -D warnings` run**: (1)
  `cuda_nv12_frames` dead-code warning in
  `crates/reco-core/src/session/detection_dispatch.rs` (also already
  called out below under the Skia renderer entry - predates this
  session, from the upstream-merge commit `6ff32c37`); (2)
  `clippy::field_reassign_with_default` in two tests in
  `crates/reco-gui/src/settings.rs` (predates this session, commit
  `e99c1380`). Neither is new debt from this session's work - flagging
  so they don't get mistaken for a regression later, but not fixed
  here (out of scope for either feature that touched nearby code).
- **Nothing outstanding** on this feature itself.

**Both debug and release `reco-gui.exe` rebuilt from `main` with the
above two features** (per
[[feedback_rebuild_gui_before_user_test]] - the release build was the
part called out as "not yet done" in the previous handoff, now done).
`cargo test -p reco-core -p reco-cli -p reco-autocam -p reco-io -p
reco-gui` all green except the same 2 pre-existing CUDA-context test
failures noted below (untouched module, not a regression).
`reco-obs` doesn't build in this environment at all (missing
`libobs`/`OBS_INCLUDE_DIR`, pre-existing, unrelated) - excluded from
the workspace-wide build/test commands this session, built the
touched crates explicitly instead.

**Events JSONL is now self-describing, merged into `main`
(`8ba19adc`).** User asked: when AI logging is on, put all the AI/panner
parameters at the top of the events JSONL, in English, and mention
which YOLO model was used right at the top too. Done in two passes -
the very first line of any `--events` output is now
`{"kind":"run_config","model_path":"...","config":{...}}` with
`model_path` first (which checkpoint produced the trace is the first
thing worth knowing) and every field from `docs/ai-panner-tuning.md`'s
settings tables inside `config`, before any `frame_start` line. New
`PipelineEvent::RunConfig` variant in reco-core reuses the
`Calibration::AutocamDefaults` struct added earlier this session for
`config` (same schema, no duplication) but keeps `model_path` as a
sibling field, not part of that struct - a machine-local absolute path
doesn't belong in the calibration-persisted version. Wired through
`StitchJob::ai_run_config(model_path, config)` in reco-io, populated
from both reco-cli (resolved CLI args + panner preset/config overlay)
and reco-gui (`AutocamUiConfig` directly). Also asked to reformat the
doc's settings tables as a plain aligned block instead of markdown
tables - done in both EN/NL, matches the `run_config` field order.
Verified with a real CLI run
(`--player-anchor-rad 0.35`) - the JSONL's first line matched exactly.
Docs updated (EN+NL) to mention this. Nothing outstanding here.

**Round-3 yolo26n training done + a yolo26s comparison run alongside
it, both ONNX-exported, neither tested in the real app yet.** LS
project 8's 228 corrected tasks (200 original + 28 hard frames) used
for the first time as a training set. Full detail in the
`YOLO26_Training.md` "Round 3" section (2026-08-12) - condensed here:

- `yolo26n_v3_3class_1280_b4_e300` and `yolo26s_v3_3class_1280_b4_e300`,
  same data (194 train/34 val) and hyperparams (imgsz=1280, batch=4),
  fresh from stock weights. Both ONNX-exported and metadata-verified
  (`1x3x1280x1280` in, `1x300x6` out, `{0:person,1:ball,2:referee}`).
  Checkpoints:
  `D:\VOETBAL_VIDEO\RECO\training\round3\runs\{yolo26n_v3,yolo26s_v3}_3class_1280_b4_e300\weights\`.
- yolo26s clearly wins the direct comparison (all mAP50 0.759 vs 0.721,
  mAP50-95 0.573 vs 0.474) - consistent with the original forum-based
  preference for Small over Nano.
- **Real scare, resolved**: both new models' ball mAP50 (~0.51-0.53)
  looked like a big regression vs `v2`'s reported 0.694 - but re-running
  `v2` against the *same* round3 val set (instead of its own original
  val slice) also gives it only 0.524. **Not a regression** - the
  round3 val set (34 images/16 ball instances) is just a harder sample
  than the old one, for every model tested. `v3`/`yolo26s_v3` are at
  least as good as `v2`, slightly better on recall, on the more honest
  sample.
- **Not yet done**: real-app test for either new checkpoint (mAP alone
  isn't trusted in this project - matches every prior round's own
  practice) before picking one to actually ship over `v2`.
- Also fixed two real pipeline gaps while building the round3 dataset:
  the 28 hard-frame images from 2026-08-11 only existed in that
  session's scratchpad, not on disk here - re-downloaded from LS; and
  LS's YOLO export uses REST-uploaded images' *hash-prefixed* filename
  as the label stem (not the clean name) - stripping the hash silently
  breaks the match. Both now documented in `YOLO26_Training.md` for
  next time.

**6 hard-frame LS tasks: pushed, then reverted same session - LS
project 8 back to 228, local copies deleted too.** Found the exact
`t=123.7-125.1s` window of 03 OJC (right camera, user-confirmed ~124s
match time) where yolo26n produces zero raw detections at all - a
genuine model recall gap, not a pipeline issue (see the Ball anchor
range section below for the pipeline-side issue that was separately
ruled out/fixed). Extracted 6 frames there, soccana found the ball in
all 6 where yolo26n found nothing. **User then said they'd already
reviewed similar frames the day before (2026-08-11) - asked to delete
these 6 again as redundant.** Done: LS tasks removed via `DELETE
/api/tasks/<id>/`, project 8 back to 228; the matching local copies
also removed from
`D:\VOETBAL_VIDEO\RECO\training\finetuned_n_preds\ls_flat\images\`.
**No further action needed on these 6 specifically** - the underlying
`t=123.7-125.1s` recall gap is still real and documented (see
[[project_yolo26n_training_pipeline]]) if it turns out worth targeting
again later, just not via these exact frames.

Real, reusable lessons from getting the image format wrong twice before
the revert (kept in [[project_yolo26n_training_pipeline]] for next
time): (1) this LS instance requires `model_version: "undefined"`
(literal string) on every pushed prediction or it silently doesn't
render in the UI - don't use a descriptive/traceability value; (2)
`reco-detect`'s real inference always **letterboxes the whole,
uncropped frame** (confirmed in `crates/reco-detect/src/detectors/*.rs`
doc comments) - never crop or stretch training/review images, always
use full native resolution, matching this project's own existing 228
tasks.

**AI Tracking settings now persist in the calibration JSON, merged into
`main` (`9d8f778e`).** User asked to stop re-entering the same panner
settings after every build/session. New `Calibration::autocam_defaults`
(`crates/reco-core/src/calibration.rs`) holds the tunable subset of
`AutocamConfig`/`FieldPannerConfig` (tracking mode, detection interval,
**Ball anchor range**, lookahead, preset/framing, cluster mode/bandwidth,
dead-zone, ball weight, **Ball reach**, **FOV Wide/Tight/Default**) -
deliberately excludes the model path (already persisted separately via
`user_settings`) and the enabled toggle.
- `reco-gui`: `do_save_calibration` snapshots the current Export-dialog
  slider values into `cal.autocam_defaults` on every "Save calibration"
  (these aren't part of the live renderer like topology/lens sliders, so
  sync-on-save rather than sync-on-every-edit); `try_init_and_update`
  restores them onto the sliders right after a calibration loads
  (before the VRAM lookahead-safety clamp, so a restored value still
  gets clamped if it wouldn't fit).
- `#[serde(default)]` throughout, so calibrations saved before this
  change keep loading fine (verified via a dedicated test).
- **Not yet done**: user hasn't actually saved a calibration with the
  recommended settings dialed in yet - next time they open the Export
  dialog, set the values below, and hit **Save calibration**, that
  calibration file becomes self-contained and won't need re-entering
  them again.

**New GUI slider merged into `main` (`ce036017`) - "Ball anchor range",
the real fix for the corner-breakaway ball going "Lost".** Follow-on
from the FOV Wide finding below: user asked to test the FOV Wide fix,
checked ball detection around t=24s in `Ai Planner Test v3.mp4`, and the
ball tracker was coasting/going `Lost` right through a moment where the
raw YOLO26 model actually detected the ball at **0.97 confidence** -
confirmed via raw `detections_raw` events, ruling out a model/recall
problem.
- Root cause: `BallTracker` (`crates/reco-autocam/src/trackers/ball.rs`)
  has its own **player-anchor gate**, upstream of everything in
  `FieldPanner` - a raw ball detection is only accepted if it's within
  `player_anchor_max_rad` (hardcoded `DEFAULT_PLAYER_ANCHOR_RAD = 0.20`
  rad / ~11deg) of at least one tracked player. A genuine breakaway ball
  sits outside this on purpose (that's what makes it a breakaway), so it
  gets dropped before the panner - and before Ball reach or FOV Wide -
  ever sees it. `with_player_anchor_rad()` existed as a builder method
  but was dead code in production (only unit tests called it).
- **Built and merged**: `AutocamConfig::player_anchor_max_rad` (new
  field + builder in `reco-autocam/src/lib.rs`, wired into both
  `BallTracker` construction sites), a `reco-cli --player-anchor-rad`
  flag, and a new **"Ball anchor range"** GUI slider (0.1-0.8 rad,
  top-level AI Tracking controls, next to "Detect every N frames" - it's
  a tracker knob, not part of `FieldPannerConfig` presets). Docs updated
  with the full 3-gate order (Ball anchor range -> Ball reach -> FOV
  Wide). `cargo test -p reco-autocam -p reco-gui -p reco-cli` all green,
  build+launch smoke-tested clean.
- **Not yet done**: user hasn't re-tested with Ball anchor range raised
  (try 0.3-0.5+) to confirm the t=24s breakaway is now actually tracked
  end to end (check the events JSONL for `state: Tracking` instead of
  `Coasting`/`Lost` during that window) and that the resulting shot
  matches the Once AutoCam reference framing.

**Panner testing (earlier this session, continues 2026-08-11's
investigation) - FOV Wide is the missing piece, not new panner code.** User exported
`Ai Planner Test v1/v2.mp4` + `.events.jsonl` (in the `TEST VIDEO`
folder) with Ball reach already raised to 1.0 rad, and compared against
a competitor's output (`Once AutoCam 100-130sec.mp4`, same match, t=24s)
that keeps a breakaway ball-carrier and the main group in frame
together - something our export couldn't reproduce yet.
- Root-caused by reading `FieldPanner::target_fov`
  (`crates/reco-autocam/src/panners/field.rs:745-777`): the widen-for-
  the-ball logic **already exists** (`needed = (ball_offset_deg +
  ball_frame_margin_deg) * 2`, then `fov.max(needed)`) but is clamped to
  `fov_wide`, and the `action`/`broadcast` presets cap that at 48/58° -
  too low for a genuine breakaway to ever open the shot up enough.
  **Confirmed by a controlled CLI A/B render** (same clip/moment/every
  other setting held constant, only `fov_wide` changed): 48° clips one
  of the two actors, 70° holds both, matching the competitor's framing
  style. No panner code change needed - `fov_wide` is already
  GUI-tunable up to 90°.
- **Updated recommendation** (now in `docs/ai-panner-tuning.md`/`.nl.md`):
  action preset + cluster_mode trimmed_mean + dead_zone 0.05-0.08 +
  ball_weight 0.35 + **ball_max_dist_from_cluster (Ball reach) 1.0** +
  **fov_wide (FOV Wide) 65-70°** (was just Ball reach alone before this
  session - that wasn't sufficient by itself, this session found why).
- Wrote an English problem write-up for the user to discuss with an
  engineer, initially claiming this needed new panner logic - **that
  write-up was wrong and was corrected once the A/B test disproved it**;
  don't reuse the first version if it's referenced anywhere.
- Analysis gotcha for next time: the events JSONL's `timestamp_ms`
  field (`frame_start`/`world_state`/etc.) is wall-clock elapsed
  *processing* time, not video presentation timestamp - correctly
  documented as such in `crates/reco-core/src/detect/panner.rs`'s doc
  comments, just easy to misread as video-relative time by the field
  name alone. Use `frame_index / output_fps` for actual video-relative
  timing when correlating events to specific moments in the exported
  clip.
- **Not yet done**: user hasn't re-tested their own export with the new
  `fov_wide: 65-70` recommendation applied.

**Workflow change this session, apply going forward**: user wants every
feature branch merged into `main` and pushed as soon as it builds, not
held back on its own branch pending testing/confirmation - `main` is the
always-integrated local test build. See
[[feedback_merge_features_into_main_immediately]]. Upstream PRs (via the
fork) are cut from `main`'s history later, once a feature is actually
confirmed working - being merged into `main` and being "PR-ready" are
independent.

**Three things landed and are now all merged into `main`, pushed to
`github/main` (`45741a99`). None are upstream-PR'd yet.**

1. **`feat/goal-line-calibration`**: Goal editor/calibration/entry-
   detection now in `main`. **Known limitation carried over, unchanged**:
   real-footage verification (from that branch's own history) found the
   test goal polygon misplaced - not yet a confirmed-working feature,
   needs that fix before it's PR-ready upstream. See
   [[project_goal_detection_idea]].
2. **Skia renderer swap** (fixes the wobbly-text report,
   [[project_skia_renderer_future_task]]): `renderer-femtovg-wgpu` ->
   `renderer-skia` in `crates/reco-gui/Cargo.toml`, plus
   `default-font-family: "Segoe UI"` pinned on the root Window (fixes a
   slightly-larger-text side effect the user caught - Skia/DirectWrite
   and femtovg/fontdb were resolving the previously-unset generic
   sans-serif fallback to fonts with different em-box metrics). **User
   confirmed text looks good** after the font-family fix. **Still open**:
   `cargo clippy -D warnings` fails on a pre-existing, unrelated
   `cuda_nv12_frames` dead-code warning in `reco-core` (not caused by
   this change) - needs its own fix before this can pass CI for an
   upstream PR.
3. **Ball-reach GUI slider**: answers 2026-08-11's open question about
   `ball_max_dist_from_cluster` (user picked "add a GUI slider"). New
   "Ball reach" slider in the Export dialog's Advanced panner section.
   Build-verified only so far - **not yet tested against the actual
   corner-ball footage** from 2026-08-11's investigation (see that
   session's recommended test settings further down, and the new
   `D:\VOETBAL_VIDEO\Berghem Sport J011-1\03 OJC -Bergem Sport
   04072026\TEST VIDEO\` location the user moved test clips to).

**Build state**: debug `reco-gui.exe` rebuilt from `main` with all three
merged and smoke-tested clean (loads calibration, zero-copy preview
initializes, no panics). `cargo test -p reco-autocam -p reco-gui` all
green (102 tests, including the ROI-polygon tests the goal-editor merge
touches). `cargo test -p reco-core` has 2 pre-existing, unrelated
CUDA-context failures (`interop::cuda::tests::test_cuda_available`/
`test_shared_memory_allocation`, `cudaGetDevice` error code 3) -
untouched module, looks like GPU-context contention from another running
process rather than a real regression, not investigated further.
**Release build not yet done this session** - attempting it was blocked
by the harness's permission classifier; run `cargo build --release -p
reco-gui` manually (with the FFMPEG_DIR/LLVM PATH env vars, see
env_build_requirements.md) before relying on a release binary, per
[[feedback_rebuild_gui_before_user_test]].

- **What changed** (mirrors the existing `ball_weight` slider's plumbing
  exactly):
  - `crates/reco-gui/ui/main.slint`: new `export-ball-max-dist-from-cluster`
    property (default 0.5) + a "Ball reach" `LabeledSlider` (0.0-1.5 rad)
    in the Export dialog's "Advanced panner" section, directly under
    Dead-zone.
  - `crates/reco-gui/src/export.rs`: new field on `AutocamUiConfig`,
    included in the export-run JSON and mapped onto
    `reco_autocam::panners::FieldPannerConfig`.
  - `crates/reco-gui/src/main.rs`: wired both directions - preset
    application sets the slider, export-start reads it back out.
  - `docs/ai-panner-tuning.md` + `.nl.md`: new "Ball reach" explainer
    paragraph, added to the presets table (0.5 in every preset - no
    preset overrides it), removed from the "not yet exposed in the GUI"
    list, and a new "camera won't follow the ball into a corner"
    practical-tuning bullet pointing at this slider (or `Tracking mode ->
    ball` as the alternative).
- **Verified this session**: `cargo build -p reco-gui` clean (no new
  warnings), and the built exe was launched (not clicked through - no
  synthetic mouse input, per feedback_synthetic_gui_automation_risk.md)
  and confirmed to start cleanly: loaded the last-used calibration/inputs,
  GPU pipeline and zero-copy preview came up with no errors in the log.
  **Not yet visually confirmed in the Export dialog UI**, and not yet
  tested against the actual "camera won't follow the ball into the
  corner" symptom from 2026-08-11.
- **Next step**: user opens the Export dialog, confirms the "Ball reach"
  slider renders correctly under Advanced panner -> Dead-zone, raises it
  above 0.5 rad, and re-runs the same corner-ball clip from 2026-08-11's
  `--panner-config` A/B testing to confirm the camera now follows. Once
  confirmed, branch off `main` (e.g. `feat/ball-reach-gui-slider`), commit,
  and open the upstream PR (see project_upstream_pr_workflow.md for the
  fork/PR mechanics used for every other feature PR so far).

**Also unresolved from 2026-08-11** (unchanged, see that session's full
arc below for detail):

1. **Waiting on the user**: LS project 8 ("Finetuned yolo26n (rough v1)")
   had 28 new pending tasks (228 total, 200 finished) as of 2026-08-11 -
   hard ball-miss frames from real footage, soccana pre-labeled. Per the
   (uncommitted, unmerged) `feat/mlpipe-gui` branch's own handoff note,
   the user confirmed later that same evening (on RUFAN_LAPTOP) that all
   228 are now reviewed/corrected - but round-3 training itself is still
   blocked there (no CUDA/data-drive on that machine) and needs to run
   **here on TGR_PC**: re-run `prepare_yolo_train_split_from_ls_export.py`
   against project 8's fresh 228-image export, then train `yolo26n_v3`
   (same 1280/b4/e300 hyperparams as `v2`).
2. **New branch discovered this session**: `git pull` on `main` looked
   like a no-op ("already up to date"), but a full `git fetch --all` found
   the user had pushed real work to a new, unmerged branch overnight -
   `feat/mlpipe-gui` (1 commit, `d5071077`): a Streamlit GUI
   (`scripts/mlpipe/`) consolidating the yolo26n export/train/ONNX-export
   pipeline, 15 regression tests, verified against the real Pi-hosted LS
   instance. Local tracking branch `feat/mlpipe-gui` now created (tracks
   `github/feat/mlpipe-gui`). Per its own handoff note it still needs a
   real run-through on a CUDA+data-drive machine (i.e. here) and a manual
   browser click-through - not done yet this session, deferred in favor of
   the ball-reach slider work above.

## This session's full arc (continues yesterday's yolo26n pivot -
2026-08-10's session ended with review not started; today's picked back
up with project 8 confirmed fully reviewed, LS API used live to check
18/19's status)

1. **Confirmed via the LS API** (not by asking blind) that only project
   8 (200/200 finished) was ready to train on; projects 18 ("01
   Vierluik", 2/556) and 19 ("02 RPC", 19/200) are barely started -
   deferred those, trained on project 8 only for a clean comparison to
   the earlier yolo26s `rough_v7` round (identical data/split/hyperparams).

2. **Trained two yolo26n rounds** on project 8's 170/30 split (3-class,
   person/ball/referee), fresh from stock `yolo26n.pt`:
   - `yolo26n_v1_3class_1280_b4_e150` (150 epochs, fixed) - ball mAP50
     0.649, all mAP50 0.736.
   - `yolo26n_v2_3class_1280_b4_e300` (300-epoch budget, ultralytics'
     own early-stopping kicked in at epoch 181, best checkpoint from
     epoch 81) - ball mAP50 0.694 (better recall, worse precision than
     v1), all mAP50 0.710. **v2 is the checkpoint in active use** -
     chosen for the better ball recall, the actual project goal.
   - Both a bit behind yolo26s's `rough_v7` on identical data (expected,
     smaller model), ~25% faster to train.

3. **Exported v2 to ONNX** (`nms=True` requested; ultralytics forces it
   off for this end2end architecture but the output shape is already
   the needed `[1,300,6]` regardless - confirmed via direct `onnx.load`
   metadata inspection: input `1x3x1280x1280`, output `1x300x6`, names
   `{0:person,1:ball,2:referee}` in the exact dict-string format
   `reco-detect` parses).

4. **Tested in the real `reco` app end-to-end**, CPU then GPU:
   - CPU (default `ort` feature): worked, ~1.7 fps, confirmed correct
     class-id resolution and real ball-tracker acquire/lose behavior on
     a live 03 OJC clip.
   - **Found and fixed a real gap**: `reco-cli`'s `Cargo.toml` had no
     `directml` feature passthrough (had `cuda`/`tensorrt`/`coreml`, not
     `directml`) - added the missing line. Also explains why the CPU
     run's log claimed "DirectML execution provider enabled" when it
     had actually silently fallen back to CPU (`reco_detect::ort_session`
     logs "enabled" on any `Ok` result without checking the EP actually
     attached - not fixed, just understood).
   - GPU (DirectML) build: **~18 fps avg, ~10x the CPU speed**,
     comfortably real-time-capable. Hit and fixed a VRAM budget error
     along the way (`--lookahead-reduced-bit-depth`, the fix already
     shipped in an earlier session).

5. **Added 28 hard-frame training examples to LS project 8** - pulled
   raw frames from two confirmed ball-miss windows in real footage,
   soccana-pre-labeled (20/28 got a soccana ball box), pushed via the
   REST import+predictions API (recreated the push script fresh this
   session, scratchpad only). Hit and fixed one real bug: re-uploading
   the same filename twice in one run silently mis-attaches the
   prediction to the *first* matching task, leaving an orphaned
   zero-prediction duplicate - caught via a `total_predictions==0`
   sweep, deleted the orphan.

6. **User reported a real symptom** from their own export test: camera
   pans stuck for ~7s during real, continuous open play (not a
   stoppage - confirmed by pulling actual video frames) whenever the
   ball goes undetected for a few seconds. Root-caused through a long
   back-and-forth of `--panner-config` A/B tests against the identical
   clip:
   - Dead-zone alone: didn't fix it.
   - `cluster_mode: density -> trimmed_mean`: fixed the freeze (density
     was locking onto a static sub-group of players instead of
     following the whole formation).
   - User then reported the *opposite* problem (wobbly/jittery) after
     applying the fix - turned out their real GUI settings (shared via
     screenshot) differed from this session's CLI reproduction in ways
     that mattered: `ball_weight=1.0` (manually maxed, not the `action`
     preset's own 0.35), `detection_interval=3`, `lookahead=0.5s`.
     Reproduced the wobble exactly once matched. Swapped in a relabeled
     `soccana.onnx` (fixed a real gotcha: `model.names[i]=...` on the
     ultralytics Python wrapper doesn't persist to the exported ONNX,
     had to patch the ONNX metadata directly) with identical settings -
     jitter was the same or worse, **ruling out yolo26n's detection
     quality as the cause**. Confirmed `ball_weight=1.0` was the actual
     culprit - `0.35` roughly halved the jitter without bringing the
     freeze back.
   - User then asked if `field_roi` explains a separate "camera won't
     reach the corner" symptom. Tested directly (calibration copy with
     `field_roi` stripped) - ROI does filter real detections but didn't
     change the camera's pan/tilt range. Found the real mechanism
     instead by pulling the actual video frame at the exact timestamp:
     a genuine, isolated ball far from the main player cluster gets
     rejected by the panner's `ball_near_cluster` gate
     (`ball_max_dist_from_cluster`, default 0.5 rad, not GUI-exposed) -
     "Action" framing's designed behavior (stay with the crowd), not a
     bug. Left as an open question for the user (see above).

## Other threads, unchanged since 2026-08-07

- **Goal-scored detection** (`feat/goal-line-calibration` branch):
  still paused pending better ball-model quality - yolo26n_v2 is
  meaningfully better than the original blocker, worth revisiting once
  the two open items above are settled.
- **Veo Cam 3 competitive roadmap**: `docs/research-veo-cam3-comparison.md`.
- **Upstream PRs** (#422-435, #464): awaiting owner review/merge.
- reco-gui app icon: still waiting on a source image from the user.
- v0.5.4 upstream sync: still deliberately deferred to its own session
  (91 fork-only vs 23 upstream commits, 20 real conflicts incl. a
  structural one - see `project_v054_upstream_sync` memory).

## Machine-specific reminders (still valid)

- FFMPEG_DIR and LLVM PATH needed per-shell for any Rust build - see
  env_build_requirements.md.
- This machine (TGR_PC) has a CUDA-working `ultralytics`/`torch` (RTX
  3060 Ti) - the CUDA build needed the explicit
  `--index-url https://download.pytorch.org/whl/cu128` at install time,
  default `pip install torch` gives CPU-only.
- `python` isn't on PATH as a bare command in this session's shell -
  use the full path,
  `C:\Users\Rufan\AppData\Local\Programs\Python\Python314\python`.
- Don't drive reco-gui's UI with synthetic mouse/keyboard input.
- `git fsck --full` before pushing, per feedback_git_object_corruption.md.
- `soccana.pt` and the newly-relabeled `soccana.onnx` both live at
  `D:\VOETBAL_VIDEO\RECO\training\models\` on this machine only - not
  git-tracked (third-party-derived binaries), redownload/re-export from
  the Hugging Face URL + relabeling steps in `YOLO26_Training.md` if
  working from the other PC.
- GPU renders can hit a transient `GetData timed out (>1M polls)`
  D3D11VA staging error after many back-to-back runs in one session -
  not reproducible, just retry.
