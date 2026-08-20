# Session handoff - 2026-08-20 (TGR_PC)

**SESSION PAUSED 2026-08-20, user said "dit werkt, maar moet nog veel
aan gebeuren. Dit gaan we later doen."** (works, but needs a lot more -
picking this up later). Reviewed community PR #474 against
`reco-project/video-stitcher` (external contributor `wendibus` / Björn
Zelter): an HTML/CSS/JS scoreboard-overlay system - manifest-driven
package discovery, a sandboxed Chrome/CDP renderer, a sport-neutral
wgpu RGBA compositor after Autocam/before encode. This is exactly what
[[project_overlay_features_scoreboard_logo]]'s 2026-08-14 research
concluded reco-core needed; someone else built it first.

**First correction from the user mid-session**: I initially built the
PR's branch as-is (based on the v0.5.4 release point) and told the user
to test that. User caught it - **wrong base, doesn't have async-detect
or anything merged since.** Fixed by creating a fresh branch
`scoreboard-on-main` (worktree `D:\CLAUDE\worktrees\scoreboard-on-main`)
and rebasing the PR's 7 scoreboard-specific commits directly onto
current `main` tip (not a merge - PR history predates the v0.5.4-sync
hand-port, so `main` and the PR branch share no clean merge-base;
`git rebase --onto main <fork-point>` replays just the scoreboard
commits, keeps original wendibus/Zelter authorship).

**Conflicts resolved by hand, not blind accept-theirs** (each verified
against current architecture, not guessed):
- `render/mod.rs`/`render/pipeline.rs`: new `overlay` module + composite
  wrapper combined with `color_match` and the current
  `render_to_target*` signatures (`color_correction`/
  `multiband_blend_enabled`/`show_seam_line` args added to `main` after
  the PR's fork point).
- `session/mod.rs`: the PR's struct-field diff dragged in stale
  `detection`/`ball_tracker`/`panner`/etc. fields as merge context -
  those already moved into `StitchCore` in the post-refactor
  architecture; kept only the genuinely new `overlay_source` field.
- `main.rs`: dropped the PR's dead `MatchCalibration`/`IntentTranslator`/
  old `detect::director::ViewportPosition` imports (pre-v0.5.4-sync
  naming, see [[project_v054_upstream_sync]]), kept current
  `Calibration`/`geometry::ViewportPosition`, added the new
  `OverlayFrame`/`OverlayFrameSource` imports.
- `main.slint`: two additive UI blocks (Audio Sync vs. Scoreboard
  section) - kept both.
- Cargo.toml/Cargo.lock: unioned workspace members, regenerated the
  lock via `cargo build`.
- One real post-rebase compile error: `PreviewBridge`'s field is
  `engine` (`StitchCore`), not `renderer` - PR predates that rename.
  Fixed and committed separately.

`cargo test -p reco-scoreboard` all green (13 tests, including a real
Chrome DOM/transparent-capture/live-editor-publish test) on the ported
branch. Noticed but did NOT fix: `cargo clippy --all-targets` fails on
`crates/reco-gui/src/settings.rs:398`/`406`
(`field_reassign_with_default`) - pre-existing on `main` itself (from
the ball-coast-secs commit), unrelated to this port.

**Second ask: user's real product is football/soccer, not basketball**
("mijn scoreboard moet niet basketball zijn maar voetbal"). The PR's
`scoreboards/basketball/` is explicitly documented as a reference
fixture only (`scoreboards/README.md`), and `scoreboards/AGENTS.md`
already has a full "add football" walkthrough as its worked example -
followed it directly. Added `scoreboards/football/` (manifest, HTML,
CSS, JS, `assets/`), sport-neutral contract only (no Rust/Slint
changes):
- match clock counts up and carries across halves (not basketball's
  per-quarter countdown); "Next half" advances the period and clears
  added time only, never resets the clock or cards.
- period label mapped from `game.period` (1st/2nd Half, Extra Time 1/2,
  Penalties) instead of "Q2 / 4".
- yellow/red card badges per team (hidden at zero) replace fouls; cards
  accumulate for the whole match, unlike basketball's per-quarter
  team-foul reset.
- added-time badge (`+N'`) shown when `sport.addedTime > 0`.

Added 2 new Rust tests mirroring the basketball coverage exactly
(`bundled_football_is_discovered` in `discovery.rs`,
`football_api_updates_dom_and_keeps_transparent_pixels` in
`runtime.rs` - real Chrome, real DOM assertions, real editor-publish
round trip). `cargo test -p reco-scoreboard` now 15/15 green. No Rust
rebuild needed to pick up the package itself (filesystem-discovered at
runtime) - just restarted the running debug `reco-gui.exe`.

**Both debug+release `reco-gui.exe` built and verified running**, in
the worktree's own `target/` (not the main checkout's usual build
output):

```
D:\CLAUDE\worktrees\scoreboard-on-main\target\debug\reco-gui.exe
D:\CLAUDE\worktrees\scoreboard-on-main\target\release\reco-gui.exe
```

User confirmed the football scoreboard renders and works, but said "a
lot still needs to happen" without specifying what yet - **paused here
on the user's call, nothing more decided this session.**

**Not done**: no commit/push to any remote (all work is local commits
on `scoreboard-on-main`, on top of `main`, in the dedicated worktree -
`main` itself untouched). No merge/PR decision made about PR #474
itself (still an open external PR upstream, untouched by any of this).
The original `D:\CLAUDE\worktrees\pr-474-scoreboard` worktree still
exists holding the PR's untouched original branch, purely as a
reference to compare against - **not** the one to keep testing from,
superseded by `scoreboard-on-main`. Next session: ask the user what
specifically needs to change before picking this back up (design pass?
more fields? GUI polish? something else) - no todo list exists yet
beyond "much more to do."

# Session handoff - 2026-08-17 (TGR_PC, continues 2026-08-16)

**SESSION CLOSED 2026-08-17, user said "ik ga afsluiten voor vandaag"
- pick up from here next time.** Short session: built a new "Ai
Learning" ball-rich frame batch for Label Studio (4 new source videos,
LS project 24, 100 tasks/100 predictions) using the documented
`yolo26s_v4_imgsz1920` checkpoint, not soccana and not the unvalidated
tiled checkpoint - full detail in the dated section right below. The
big GPU-optimization thread from 2026-08-16 (further below) is
unchanged/still paused - nothing new there today.

One-line state on that thread (unchanged from yesterday): single-worker
async-detect is the one shipped win (1.22-1.42x, has a reco-gui
checkbox, commit history on `feat/async-detect-thread`); dual-worker
and CUDA-graphs were tried and reverted; batch-L+R was measured (1.06x)
but not integrated - pick that up first if resuming the optimization
thread. Nothing merged to `main`.

## 2026-08-17: "Ai Learning" ball-rich frame batch exported to Label Studio, model-choice correction, re-hit a known LS API quirk

User asked for a Label Studio pre-label export from 4 new raw videos in
`D:\VOETBAL_VIDEO\Berghem Sport J011-1\Ai Learning` - 2 recordings
(`0001` = 2026-05-30, `0005` = 2026-06-03), each with L+R cameras
(3840x2880 HEVC, ~20.4 min each), max 25 frames/video, only frames
with >=1 ball. Asked the user how to scope the LS project - **1
combined project for all 4 videos** (chosen, same pattern as the
earlier "01 Vierluik"/"02 RPC" per-source projects) vs. 2 separate
projects vs. direct-into-project-8.

**Model correction mid-session**: first pipeline run used `soccana.pt`
(matching the old convention) but the user stopped it before upload and
said to use **"de laatste ONNX"** instead. Found 2 undocumented-as-
"latest" candidates on disk with no clear winner from the filename
alone - asked the user which:
- `round4/runs/yolo26s_v4_imgsz1920/weights/best.pt` (2026-08-15,
  **documented, real-app-tested**, "best-tested checkpoint of the whole
  project" per `YOLO26_Training.md` / [[project_yolo26n_training_pipeline]])
- `round4/runs/yolo26s_tiled1920_full/weights/best.onnx` (2026-08-16,
  newer by file timestamp, but **undocumented/unvalidated** - needs 2x
  tiled L/R inference that isn't wired into `select_ball_rich_frames.py`
  or any production `reco-detect` path yet)

**User picked `yolo26s_v4_imgsz1920`.** Used the `.pt` weights (not
`best.onnx`) for the actual local pre-labeling pass: this machine's
`onnxruntime` only has `CPUExecutionProvider` (no `onnxruntime-gpu`
installed), while `.pt` runs on GPU via torch/CUDA - identical trained
weights, just a much faster backend for ~1600 candidate frames.
`best.onnx` remains the file that would eventually ship to
`reco-detect` itself; wasn't needed for this pass.

**Pipeline** (`select_ball_rich_frames.py`'s `process_video()`,
interval=3s, top_k=25, conf=0.15, imgsz=1920, camera tags
`0001_left`/`0001_right`/`0005_left`/`0005_right`):

```
0001_left:  408/408 candidates had >=1 ball, kept 25
0001_right: 250/408 candidates had >=1 ball, kept 25
0005_left:  408/408 candidates had >=1 ball, kept 25
0005_right: 399/408 candidates had >=1 ball, kept 25
```

Flattened via `package_yolo_for_labelstudio.py`'s convention (100
images total), created **LS project 24**, title corrected afterward to
`"Ai Learning - yolo26s_v4_imgsz1920 pre-labels"` (briefly carried a
leftover "(soccana)" title from the original plan).

**Real bug hit, and it was avoidable**: the fresh upload driver script
assumed `POST /api/projects/<id>/import`'s response includes
`task_ids` - it doesn't on this LS instance (only `task_count`/
`file_upload_ids`). **This exact quirk was already documented** in
[[project_yolo26n_training_pipeline]] from the 2026-08-09 and
2026-08-12 batches ("this API's response omits `task_ids` on LS 1.23.0
- match tasks back by filename via `GET /api/tasks` instead") - missed
because the new script wasn't checked against that memory before being
written. All 100 images were still correctly uploaded as tasks
(`task_count:1` each, confirmed via `GET /api/tasks?project=24` ->
`total=100`, zero duplicates) - just missing predictions. Fixed with a
short follow-up script: fetched the real task list, matched each
task's hash-prefixed `data.image` filename back to the flat label files
(filenames unique in this batch, so suffix-matching is safe - no repeat
of the earlier duplicate-filename orphan bug), posted predictions
retroactively. No re-upload, no data loss. Final verified state: 100
tasks / 100 predictions / 0 annotations, 0 unmatched.

**Lesson, now hit 3 times (2026-08-09, 2026-08-12, 2026-08-17): always
check [[project_yolo26n_training_pipeline]] for known LS-instance API
quirks before writing a new upload/import driver script**, don't
re-derive the response shape from scratch each session.

Also explained the SAHI-style tiled-1920 training session to the user
in English on request (pure recap, no new facts - already fully logged
in the 2026-08-15/16 sections below and in `YOLO26_Training.md`);
reconfirmed the tiled checkpoint is still not wired into any production
path, which is why today's batch deliberately used the non-tiled,
documented checkpoint instead.

**Not yet done**: user's review/correction pass on LS project 24 (100
tasks, currently all raw predictions). Dataset artifacts kept on disk
at `D:\VOETBAL_VIDEO\RECO\training\ai_learning_dataset\` (per
[[feedback_keep_test_artifacts]]). Nothing committed/pushed this
session - only scratchpad Python driver scripts were touched, not part
of the repo.

**2026-08-16: async detect thread - built, tested, measured end-to-end,
real 1.42x export speedup, VRAM cost measured, reco-gui checkbox added.**
Picks up the design from the entry below ("export-speed fix #2") that
was deliberately left unstarted on 2026-08-15. User asked to build it
this time, wanted it kept revertible - everything lives on an isolated
worktree (`D:\VOETBAL_VIDEO\RECO\worktree-async-detect`) / branch
(`feat/async-detect-thread`, 5 commits: foundation, session wiring, CLI
flag, VRAM measurement docs, GUI checkbox), `main` never touched.

**What it does**: moves only the ORT `session.run()` inference call to
a dedicated worker thread (GPU texture readback/preprocess stays
synchronous - textures aren't safe to hand to another thread). New
`UnifiedDetector::detect_split()` trait method (default = today's sync
behavior, zero risk to any backend that doesn't override it),
`AsyncDetectThread` (new, modeled on `async_encode.rs`), and solved the
"ROI wrinkle" that stopped the original design - `RoiFilteredDetector`
composes its own filtering onto the deferred result's `finish` closure
instead of silently skipping it, explicitly regression-tested. Session
wiring: `BufferedFrame.world_state` is now ready-or-pending, resolved
lazily right before a frame is actually consumed
(`run_panner_once`), with a stale-reuse fallback for the panner's
lookahead peek - same pattern already used elsewhere for stale
detections. New `reco-cli --async-detect` EXPERIMENTAL flag builds a
second, separate detector instance for the worker thread.

**Real measured result** (300-frame test, 03 OJC clip, RTX 3060 Ti,
TensorRT, same model/settings otherwise):

```
baseline:        28.8s (10.4fps avg)
--async-detect:  20.3s (14.8fps avg)   1.42x faster
```

Functional correctness verified bit-for-bit identical - both runs
produce exactly 242/300 raw-ball-detection frames (80.7%), mean
confidence 0.574.

**VRAM cost, measured** (`nvidia-smi` polled every 0.5s, same test,
steady-state average not raw peak - baseline had a misleading transient
startup spike above its own steady state):

```
baseline steady-state:       5226 MiB
--async-detect steady-state: 5622 MiB   (+395 MiB, +7.6%)
```

Real but modest - not the "doubles the whole session" risk originally
flagged, because only the detector's own footprint (TensorRT
engine+workspace) duplicates, not the much larger shared lookahead
VramPool.

**reco-gui checkbox added**: Export dialog, "Async AI detection
(experimental, faster export)" right below "Reduce lookahead memory
(8-bit)". Mirrors `--async-detect` 1:1. Deliberately NOT persisted in
AutocamDefaults (calibration or app-level) - always resets to off on
restart, unlike every other AI Tracking slider, so the experimental
flag can't silently stay on via a saved calibration. `check`/`clippy
-D warnings`/`fmt` all clean, committed (`fdffb740`). Both debug and
release `reco-gui.exe` built **in the worktree's own `target/`** (not
the main checkout's usual build output):

```
D:\VOETBAL_VIDEO\RECO\worktree-async-detect\target\debug\reco-gui.exe
D:\VOETBAL_VIDEO\RECO\worktree-async-detect\target\release\reco-gui.exe
```

To test: load a calibration, AI Tracking on with a model + lookahead >
0, tick the checkbox in Export, run an export and compare against one
with it unticked.

**GUI checkbox validated by the user's own real exports, 2026-08-16
(nvidia-smi polled live during each run).** First two attempts were
apples-to-oranges (debug-vs-debug showed no gain; a release pair had
mismatched clip lengths, 42s vs 130s) - flagged both times rather than
reporting a misleading number, then re-measured with a matched release
pair, same clip, only the checkbox differing:

```
                    async ON    async OFF
active duration     ~134s       ~164s        1.22x faster
GPU util (avg)      73.2%       56.8%
VRAM                6907 MiB    6488 MiB     +419 MiB
power (avg)         169W        152W
```

Confirms the earlier CLI-only 1.42x/+395MiB measurements in a real
end-to-end GUI export, not just a synthetic benchmark - same direction
and order of magnitude, slightly lower speedup here (expected, real
GUI overhead vs a controlled CLI run).

**Same day, follow-on: user asked to push GPU utilization further.**
Profiled the shipped async-detect build for real (not guesswork) -
found `yolo_inference` at ~90% of wall-clock, already well-overlapped.
Two more attempts, each measured via a true A/B (same clip/command,
code toggled):

1. Fixed `wgpu_preprocess.rs`'s blocking readback wait to target our
   own GPU submission index instead of "whatever's most recent" -
   **measured no real effect** (28.33s vs 28.51s, noise). Kept anyway,
   strictly more correct. Also confirmed TensorRT+FP16 genuinely
   active (ruled out a silent-DirectML-fallback explanation).
2. Added dual-worker mode (`AsyncDetectThread::new_dual`, one thread
   per camera instead of one shared) so Left/Right inference can
   overlap - CLI A/B measured a modest ~5-6% further speedup (24.7s ->
   23.3s), not the near-halving hoped for: two concurrent TensorRT
   contexts on this consumer GPU (no MPS) don't truly run in
   parallel, they contend and each call gets slower (40.1ms -> 77.3ms
   avg), eating most of the theoretical gain. Wired as
   `--async-detect-dual` / a second GUI checkbox, handed to the user
   to test themselves in the real GUI.
   **User's own GUI test found no win at all**: single-worker 72.0%
   GPU/6784MiB/~138s vs dual-worker 66.4% GPU (lower!)/7339MiB
   (+555MiB confirmed)/~136s (same). User decided to revert it
   ("andere strategie bedenken") - reverted clean, commit `f945d4eb`.
   Lesson: should have checked in before building the full GUI wiring
   for a change whose own CLI measurement was already a weak ~5-6%
   signal - over-invested before the user weighed in.

Poll-wait fix (`0868ead4`) kept, dual-worker (`978f62ae`) reverted
(`f945d4eb`) - all on `feat/async-detect-thread`, still not merged.
Both debug+release reco-gui rebuilt again, back to just the
single-worker async-detect checkbox - test paths:

```
D:\VOETBAL_VIDEO\RECO\worktree-async-detect\target\debug\reco-gui.exe
D:\VOETBAL_VIDEO\RECO\worktree-async-detect\target\release\reco-gui.exe
```

**Next ideas raised** - user asked for a different strategy: (a)
`--detection-interval > 1` - turned out to already be in daily use,
same thing as the GUI's existing "Detect every N frames" slider
(default 3), nothing to build; (b) batch Left+Right into one TensorRT
call instead of two sequential ones (`best_batch2.onnx` already
exported at
`D:\VOETBAL_VIDEO\RECO\training\round4\runs\yolo26s_tiled1920_full\weights\`
from an earlier experiment) - **not tried yet**, the only untried idea
left; (c) TensorRT CUDA graphs - **tried, CRASHED**.

**(c) CUDA graphs: `.with_cuda_graph(true)` on the TensorRT EP builder
crashed the async detect thread mid-export** (`expected typeinfo_ptr
to not be null` in ort-rs's value/mod.rs, ~150-200 frames into a
300-frame run). Worse than just "no speedup" - the thread died and the
export **silently kept running to completion** with frozen/stale
detections for the rest of the clip instead of erroring loudly. A real
correctness risk. Reverted immediately (commit `04796401`), TensorRT
engine cache restored from a pre-attempt backup, no second timing run
attempted. Left a code comment on the likely real fix (IoBinding with
pinned buffers instead of a fresh `Vec<f32>` per call) for whoever
revisits this.

**(b) batch L+R into one TensorRT call, measured via a standalone
throwaway benchmark instead of building full integration first**
(`crates/reco-detect/examples/bench_lr_batch.rs`, commit `d1df4e0c`) -
pure inference-latency comparison, no video/pipeline involved. **1.06x
- real but modest** (51.1ms sequential vs 48.3ms batched per produce
index), no downside risk unlike the other two attempts, but requires a
specially-exported fixed-batch=2 ONNX (`best_batch2.onnx`) rather than
whatever a normal training run produces - real UX friction for a small
win. **Session paused here on the user's call** ("we stoppen even
hier") rather than building the `AsyncDetectThread`/CLI/GUI
integration for it.

**Summary of all four post-single-worker attempts this session**:
poll-wait fix (no effect, kept, harmless); dual-worker (no real win,
reverted after the user tested it themselves in the GUI); CUDA graphs
(CRASHED the detect thread, reverted); batch L+R (modest 1.06x,
measured but not integrated, paused). The shipped single-worker
async-detect fix (1.22-1.42x, confirmed both CLI and GUI) remains the
one solid win from today. If picking this back up: batching is the
only remaining real (if small) lever, blocked on deciding how to
surface the "needs a batch2-exported model" requirement to a
non-technical user.

**Not done**: not merged to `main`, only the Windows D3D11VA path gets
real async behavior (every other residency still runs synchronously,
unaffected). Whether to merge/promote out of "EXPERIMENTAL" is the
user's call, not decided this session. See
[[project_async_detect_thread_design]].

Continuation note for resuming work on a different machine/session -
git-tracked so it travels with `git pull`/`push` between the user's two
PCs. Overwrite wholesale at the end of a session rather than appending
history - git history is the append-only log, this file is just "where
things stand right now."

**No usernames/passwords/IP addresses in this file, ever** - see
feedback_no_credentials_in_tracked_files.md. Reference "see password
manager" / `zerotier-cli listnetworks` instead.

## Immediate state / what to do next

**2026-08-16: dual-tile inference cost, profiled for real (not
estimated) - research concluded by the user, nothing merged.** Direct
follow-on from the overnight tiled-training entry below. User asked for
a real `--features profiling` measurement before deciding whether to
build production tiled-inference support, explicitly wanted it
revertible - built entirely on an isolated branch,
`experiment/tiled-dual-inference-profiling` (2 commits, off `main`, not
merged - revert is `git checkout main` or delete the branch; `main` was
never touched).

**Two variants measured, same 300-frame profiling run each time (03 OJC
clip, RTX 3060 Ti, TensorRT), same `yolo26s_tiled1920_full` weights
throughout so only the call pattern differs**:

```
                        baseline (1x)   sequential 2x calls   batched 1x(batch=2)
detect/camera-frame     100.9ms         213.6ms (2.12x)       324.3ms (3.21x)
throughput              9.1fps          4.1fps                2.8fps
raw-ball-detection      80.7%           88.0%                 88.0% (identical)
```

**Sequential wins over batched** - counterintuitive, verified not a bug:
functionally identical detection rate confirms the batching logic itself
is correct (same weights, same math, just one combined ORT call instead
of two) - the regression is a real TensorRT/GPU characteristic on this
hardware (a fixed batch=2 engine apparently isn't as well-optimized as
batch=1 for this model/GPU combo), not a code defect. Needed a separate
`best_batch2.onnx` export (fixed batch=2 input shape - ORT rejects a
mismatched batch axis) kept apart from the normal `best.onnx`.

**Verification method worth remembering for next time**: confirmed which
code path actually ran via trace **span counts** (`yolo_inference`
occurrences), not log output or wall-clock alone - `--features
profiling` builds silently drop ALL `log::*` calls (`init_profiling()`
in `reco-cli/src/main.rs` never bridges the `log` facade, unlike the
non-profiling `init_tracing()` path) - a real gap, not something this
session broke, worth knowing before trusting log output in any future
profiling session.

**Bonus**: this doubled as the frame-accurate real-pipeline back-to-back
test that the overnight entry below flagged as missing - these
raw-ball-detection numbers (80.7%/88.0%) come from the real Rust decode+
detect pipeline, not the ffmpeg-approximate Python re-test that had the
frame-index-alignment caveat.

**Conclusion, user's call, research closed**: the tiling win is real (on
both the LS val split and now real frame-accurate footage), but costs
~2.1x detection time at best (unbatched; batching made it worse here) -
user decided not to pursue production wiring further this session. If
picked up again later: the sequential variant
(`TiledWgpuPreprocessingDetector`, `RECO_TILED_DETECT=1`) is the one
worth building on, not the batched one. See
[[project_yolo26n_training_pipeline]].

**Overnight, 2026-08-15/16: SAHI-style left/right tiled training -
built, trained, exported, real-footage tested end-to-end while the user
slept - a real, sizeable win, but the real-footage numbers need an
honest asterisk (see below).** User asked to try the research idea noted
in the 2026-08-14 entry (splitting each 3840x2880 frame into two
overlapping square crops instead of one letterboxed square), explicitly
wanted a small feasibility test first before committing to more, then
delegated the whole rest of the sequence (full train -> ONNX -> real-
footage test -> repo cleanup) before going to bed.

**Dataset built and verified before training anything**: each round4
image -> two overlapping 2880x2880 crops (left x=[0,2880], right
x=[960,3840], 1920px overlap band) resized to 1920x1920 (2880 itself is
too large to train at directly, per the user's own sizing call).
Zero letterbox waste vs the old approach's ~25% wasted canvas on a
straight 3840x2880->square fit - and critically, the *scale factor* is
much gentler: old approach scales the whole frame 3840->1920 (0.5x, an
~18px ball becomes ~9px); this approach only scales 2880->1920 (0.67x,
same ball stays ~12px). Labels transformed (crop-offset + scale, box kept
if its center falls in the tile, so overlap-band objects appear in both
tiles) from 258 source images -> 516 tiles (438 train/78 val). **Verified
correct before training** by drawing transformed boxes back onto a random
sample of tiles - all tight and correctly placed, not assumed.

**8-epoch feasibility smoke test, as the user asked for first**: fresh
`yolo26s.pt`, imgsz=1920 (matches tile size exactly), batch=2/workers=2.
Result far exceeded the non-tiled 1920 8-epoch smoke test from
2026-08-14/15 at the same epoch count - ball P/R/mAP50/mAP50-95
1.000/0.703/0.719/0.456 vs the old approach's 0.448/0.389/-/0.230.
Verdict: clearly worth a full run.

**Full training, patience=100 (same convention as every other round)**:
early-stopped at epoch 159 (best @ epoch **59**). Held up after full
convergence, not just an early-epoch artifact:

```
              old (round4 1920-final, non-tiled, post-labelfix)   new (tiled, best@59)
ball P              0.921                                          0.987
ball R               0.471                                          0.708
ball mAP50           0.500                                          0.742
ball mAP50-95         0.338                                          0.507
```

~50% relative improvement on recall and mAP50-95 on the LS held-out val
split (same caveat as always - 17-24 ball instances, real but small
sample). Checkpoint:
`round4/runs/yolo26s_tiled1920_full/weights/{best.pt,best.onnx}`.

**ONNX export**: `nms=True` forced off by ultralytics itself (YOLO26 is
end2end/NMS-free, expected, zero-effect flag per every prior round's own
finding) - verified metadata directly: `1x3x1920x1920` in, `1x300x6` out,
names `{0:person,1:ball,2:referee}`, matches every prior checkpoint's
convention.

**Back-to-back real-footage test - a genuinely important methodology
caveat found, not swept under the rug.** Extracted all 899 frames
(t=100-130s, both cameras) fresh via `ffmpeg -ss 100 -c:v hevc_cuvid`
sequential decode (the Rust `dump_detection_frames` tool wasn't
rebuilt this run, `target/` had just been cleaned - see below), then ran
both the old and new checkpoint's own `model.predict()` directly in
Python on every frame (both cameras, tiled model gets both crops merged),
identical methodology for both models, conf floor 0.10:

```
                          old (full-frame)   new (tiled)
overall raw-ball rate      765/899 (85.1%)    860/899 (95.7%)
frames 720-898 window       85/179 (47.5%)    140/179 (78.2%)
frames 690-719 window       30/30 (100%)       30/30 (100%)
mean confidence (hits)      0.64                0.55
```

New model detects more, in both the overall rate and the window
historically labeled "hard" - **but the absolute numbers here do NOT
match every previous round's documented numbers on this exact clip/
window** (round4's own real-app test reported 49.1% overall / 0-18/179
in frames 720-898, not 85%/47.5%). Investigated rather than just
reporting the flattering framing: spot-checked frame 720 directly - the
**old** model scores 0.92 confidence there in this test, nothing like the
near-zero the historical "hard zone" would predict. **Conclusion: this
re-extraction's frame indices don't line up with the historical
frame-720-898 window** (ffmpeg's `-ss 100` sequential decode start point
isn't guaranteed to land on the same real match instant as the Rust
pipeline's own D3D11 seek+decode path used in every prior round) - this
test is sampling different real seconds of the match, not the same
"known-hard" stretch. **What this test IS still valid evidence for**:
old vs new ran under byte-identical methodology (same frames, same conf
floor, same code path) - the new model detecting the ball meaningfully
more often across this fresh, arbitrary 30s window is a real, independent
signal in the same direction as the controlled LS-metrics result above,
just not a confirmation that the *specific* historical 720-898 gap is
now 78% solved. **A real frame-index-accurate re-test (via the actual
Rust decode pipeline, once dual-tile inference exists there - see below)
is the next real validation step, not done tonight.**

**Not done, deliberately, both genuinely bigger jobs**:
- **No dual-tile inference wiring in `reco-detect`/`reco-autocam`** - this
  checkpoint cannot be dropped into the live app as-is; it needs 2 forward
  passes per camera per frame (left+right tile) merged with overlap-band
  dedup, not the 1 pass every other checkpoint uses. Given last night's
  own finding that AI detection already consumes ~87% of frame time
  (see the async-detect-thread entry below), doubling per-camera
  inference cost is a real, serious trade-off to weigh deliberately with
  the user, not decide unsupervised overnight.
- **No frame-accurate re-validation via the real Rust pipeline** - the
  back-to-back test above is Python-side and has the frame-indexing
  caveat spelled out above.

**Repository cleanup** (`D:\VOETBAL_VIDEO\RECO\repository`): removed
stray untracked clutter - `reco-trace.json` (20MB stale profiling
trace), `yolo26n.pt`/`yolo26s.pt` (26MB, misplaced in the repo root, not
part of the Rust project), `scripts/__pycache__/`. Ran `cargo clean` to
reclaim `target/` disk space - **partially failed** (`target/debug`
couldn't be removed, Windows file-lock, "os error 32" - some process
still had a handle on it). Deliberately left untouched: `.cargo/`
(checked - just `target-dir = "target"`, harmless but pointless to
delete) and `.claude/` (Claude Code's own directory - never touch).
**Consequence for next session**: the next `reco-gui.exe`/`reco.exe`
build will be a full rebuild, not incremental - budget more time than
usual. 1 commit (the label-QA audit log) is committed locally but not
pushed, per the "push only when asked" default.

**Next step, user's call**: decide whether the tiling win is worth the
2x-inference-cost trade-off enough to build the dual-tile
`reco-detect`/`reco-autocam` wiring, and/or get a frame-accurate
real-pipeline re-test before fully trusting the back-to-back numbers
above. See [[project_yolo26n_training_pipeline]].

**2026-08-15: full independent ball-label QA audit on round4's 258-task
set - found + fixed 6 real mislabels, a genuine recurring pattern (not
isolated as the same-day root-cause note below originally concluded).**
Follow-on from the frame-704/`winC_014` root-cause work below: user
rightly pushed back on "isolated, not a pattern" right after that fix,
insisting more frames likely carried the same kind of miss. Re-
investigated properly with methods that avoid the earlier pass's real
blind spot (checking a model's own predictions against labels it was
*trained on* means it can silently "agree" with a bad label it
memorized - agreement there isn't independent evidence):
1. Cross-checked all 214 ball instances against `soccana.pt` (different
   model lineage, never trained on this data) at imgsz=1920 (default
   imgsz=640 first attempt was itself worthless - shrinks an 18px ball to
   ~3px, produced 65% nonsense "disagreement"). Found 1 new confirmed
   mislabel (`right_frame_0029400`, a white field-marking dot labeled as
   ball).
2. Full official `model.val()` run of `yolo26s_v4_imgsz1920` against a
   **freshly re-pulled** LS project 8 export (not the possibly-stale
   local `round4` copy) - confirmed 258/258 tasks matched with zero drift
   beyond the 1 known fix; ball val-metrics nudged up slightly purely
   from the corrected count (R 0.444->0.471, mAP50 0.474->0.500).
3. Full 258-set audit using the production checkpoint itself (caveat:
   trained-on-same-data agreement isn't independent evidence, but
   confident *disagreement* despite training pressure to conform is still
   meaningful) - found 5 more confident disagreements, then a full visual
   contact-sheet scan of all 213 ball-label crops (not just the
   automated flags) caught 2 of those the nearest-neighbor method missed
   entirely (no competing detection nearby to flag against - a real
   coverage gap in the automated approach).

**Root cause, confirmed with hard pixel-coordinate evidence, not
guessed**: a single painted field marking (a flat white oval, no ball
seam pattern) sits at a fixed on-screen position for the static camera -
4 of 6 confirmed mislabels (all `right_frame_*`, from the original
200-frame auto-labeled batch, never the later winA/winB/winC hard-frame
batches) cluster within 7px of the exact same pixel coordinate (~731,1517
in the 3840x2880 right-camera frame). The original AI auto-labeler
evidently had a recurring false positive there; at 258-task review volume
a technically-round, already-pre-labeled small box doesn't visually stand
out as wrong without deliberately zooming in - not a diligence gap in the
user's review, a genuinely subtle recurring miss.

**Fixed both copies for all 6** (`right_frame_0029400`,
`left_frame_0023400`, `right_frame_0006000`, `right_frame_0020700`,
`right_frame_0021000`, `right_frame_0029700`): local
`round4/labels/{train,val}/*.txt` (deleted the spurious ball line, kept
the real second ball's line where one existed), and the LS project 8
source annotations - user did the LS-side deletions themselves via the LS
UI after being given the exact task-id/region-id list (kept it in their
own review flow rather than an API-side edit bypassing it), verified
after the fact via a fresh API re-pull that all 6 offending boxes are
gone and every real ball box is untouched.

**Revises the root-cause note directly below**
([[project_yolo26n_training_pipeline]]'s "winC_014 confirmed as an
isolated miss, not a pattern" section) - that conclusion was too
optimistic, since it only ever checked a same-family model's own
confident disagreements, which is blind to exactly this failure mode.
**Corrected picture: not zero-pattern, but a small (6/214 = ~2.8%), fully
explained, fully fixed, single-cause pattern** - no evidence of any other
kind of systematic label error after this pass (person/referee not
separately audited, but score well above ball in the metrics, so lower
priority).

**Not yet done**: no retrain triggered by this - same reasoning as
`winC_014` below, doesn't retroactively change any existing checkpoint,
worth folding into whenever the next real training round happens (more
hard-frame data is still the bigger lever, per the round-4 conclusion).
See [[project_yolo26n_training_pipeline]].

**2026-08-15: export-speed fix #2 (overlap AI detection with render/
encode) - thoroughly researched and designed, deliberately NOT started,
zero code changed.** User asked to continue optimizing export speed
after fix #1 (D3D11 seek, below) landed. Same profiling trace that
found fix #1 also found this: AI detection (readback+preprocess+
inference) consumes ~87% of "active" per-frame time, running
synchronously and blocking the frame loop, while decode/stitch/encode
together take ~4-5ms/frame and are already well-overlapped (async
encoder, 0 backpressure stalls). Went through a full plan-mode research
pass (2 parallel Explore agents + extensive direct reading) before
touching any code - this session's own earlier v0.5.4-sync experience
made clear that skipping this step on architecturally risky work is
how things go sideways. **User decided (end of session) to pick this
up fresh next session rather than push through now** - documenting the
complete findings here so the next session doesn't have to re-derive
any of this.

**Confirmed via direct code reading (file:line references), not
guessed:**
- The lookahead buffer (`session/run_loop.rs`'s `run_buffered`,
  `session/frame_buffer.rs`) already decouples *when* detection runs
  from *when* a frame renders (pre-detects N frames ahead so the panner
  sees future WorldStates) - but NOT *which thread* pays for it.
  `produce_one` (`run_loop.rs:382-420`) calls
  `detect_and_track_only` (`detection_dispatch.rs:216`) and blocks on
  it before the frame enters the buffer - same thread that later
  renders+encodes. No threading exists anywhere in the current loop.
- A **directly reusable precedent already exists in this exact
  codebase**: `async_encode.rs`'s `AsyncEncodeThread` - dedicated
  thread + bounded `mpsc::sync_channel` + buffer-pool return channel +
  non-blocking submit with backpressure stats. This is the template to
  follow, not a new pattern to invent.
- **Everything needed to run detection async is already `Send`-safe,
  verified directly in source, zero changes needed there**:
  `ort::session::Session` is `unsafe impl Send + Sync` (ort 2.0.0-rc.12
  vendored source, citing ORT's own C API contract); `TrtGpuDetector`
  is `Send` and re-asserts its CUDA context per-call via
  `cuda_ensure_context()` rather than pinning to a creation thread;
  `GpuContext`/wgpu `Device`/`Queue` are plain `Clone` with no interior
  mutability (`WgpuPreprocessingDetector` already holds its own cloned
  device/queue - direct precedent); `UnifiedDetector: Send` and
  `Tracker: Send` are already trait bounds; tracker/panner state has no
  interior-mutability tricks.
- **The one real constraint**: GPU-resident detection *borrows* texture
  views straight out of a 2-slot-per-camera decode/staging ring
  (Windows `D3d11StagingPool` `n_slots=4`; Linux `gpu_shared_views`,
  same effective size) that's explicitly documented (and, on Linux,
  backpressure-enforced) as unsafe to reuse until detection has
  *finished reading* it - today guaranteed only because detection is
  synchronous. Moving the *whole* detect call (GPU readback+preprocess
  +inference) async would need new owned-copy or GPU-pool-slot
  machinery to avoid a real race - genuinely new, unverified-by-
  compilation wgpu texture code, higher risk.

**Scope decision, made explicit with the user mid-implementation**:
of the ~127ms/detection-call cost, roughly ~58ms is GPU readback+
preprocess (NV12 → CHW tensor, existing compute shader +
`device.poll(wait_indefinitely)` blocking readback,
`wgpu_preprocess.rs:337-349`) and ~69ms is the actual ORT/TensorRT
`session.run()` inference (`reco-detect/src/detectors/cpu.rs:322`,
`yolo_inference` span, 2 calls/detection = one per camera). **Chose the
safer partial fix: move only the inference call async, leave
preprocessing synchronous** (unchanged, already-tested code, zero new
GPU plumbing) - real ~54% reduction in blocking time instead of the
theoretical ~87% full-async ceiling, but no new wgpu texture-readback
code that can't be incrementally compile-tested. The GPU-pool-slot
approach for the fuller ~87% win (modeled on `VramPool::acquire`/
`release`) remains a valid future refinement, just bigger/riskier -
not pursued this pass.

**The clean split point, already present in the code, no new
abstraction needed to find it**: `WgpuPreprocessingDetector::detect()`
(`crates/reco-autocam/src/wgpu_detector.rs:50-81`) already separates
these two steps internally - `self.preprocessor.preprocess(...)`
(GPU work, stays sync) produces an owned `tensor: Vec<f32>`, then
`self.inner.detect(camera, &DetectorFrame::PreprocessedChw{data:
&tensor, ...})` (the actual ORT inference, `self.inner` is the raw
`CpuYoloDetector`) - this inner call is exactly what should move to
the async thread. `Vec<f32>` is trivially `Send`, no GPU concerns at
all on the async side.

**The wrinkle found right before stopping, not yet resolved**: the ROI
filter (`RoiFilteredDetector`, `crates/reco-autocam/src/roi_filter.rs:159-176`)
wraps the *outside* of `WgpuPreprocessingDetector` and applies
`filter_by_roi(...)` to the `Vec<Detection>` *after* `detect()`
returns. Naively extracting just `WgpuPreprocessingDetector.inner` to
the async thread would bypass the ROI filter entirely (wrong -
detections outside the field would stop being filtered). Needs a small
`UnifiedDetector` trait extension before implementation continues:
```rust
enum DetectSplit {
    Done(Vec<Detection>),      // default: existing synchronous behavior, zero risk to every other detector
    Pending(/* owned tensor + enough to finish + re-apply any wrapper's post-processing */),
}
trait UnifiedDetector: Send {
    fn detect(&mut self, camera, frame) -> Result<Vec<Detection>, DetectorError>; // unchanged
    fn detect_split(&mut self, camera, frame) -> Result<DetectSplit, DetectorError> {
        Ok(DetectSplit::Done(self.detect(camera, frame)?))  // default = today's behavior
    }
}
```
`WgpuPreprocessingDetector` overrides `detect_split` to return
`Pending` with the owned tensor; `RoiFilteredDetector::detect_split`
delegates to `self.inner.detect_split(...)` and, when it gets back a
`Pending`, needs to carry its own ROI-filter step through to whenever
the async thread's inference result comes back (e.g. the `Pending`
payload could carry a boxed closure/continuation, or `DetectSplit`
could be structured so the caller applies a stack of pending
post-processing steps in order - not yet designed, this is where to
pick up).

**Rest of the plan (should still hold, wasn't invalidated by the ROI
wrinkle)**: new `crates/reco-core/src/async_detect.rs` (`AsyncDetectThread`,
modeled on `async_encode.rs`) submitting `(produce_index, owned tensor
+ metadata)` jobs and receiving `(produce_index, Vec<Detection>)`
results in strict FIFO order (single worker thread preserves ordering
for free, which the tracker contract requires -
`detect/tracker.rs:139-151` - exactly-once-per-frame, in order).
`run_loop.rs`'s `produce_one` submits without blocking; `run_panner_once`
becomes the resolution point (blocks on the result channel only if the
detect thread hasn't caught up yet - in steady state, with N frames of
lookahead buffer between produce and this point, it usually won't
need to). `FrameBuffer`/`BufferedFrame`'s `world_state` field needs a
pending-or-ready wrapper; `future_world_states()` needs a defined
fallback for not-yet-resolved entries (reuse the previous resolved
WorldState, matching the existing stale-detection-reuse pattern).
Scope: `run_buffered` (export path) only - the immediate/live-preview
path (`process_frame_any`) is unaffected, out of scope.

**Full context, all file:line references, the two research agents'
complete raw findings**: this transcript's own history has the full
detail (this session, after the v0.5.4 sync section below) - re-derive
from there if this summary isn't enough, rather than re-running the
same research agents from scratch.

**2026-08-15: v0.5.4 upstream sync finally done - long-deferred, turned
out to be a fundamentally different task than expected, merged+pushed
to `main`.** User asked to execute the sync (91+ fork-only commits / 23
behind `origin/main`), with two explicit constraints: don't lose any
built features, and prefer upstream's base since future updates need
to keep applying. Backed up first: tag `pre-v054-sync-backup-2026-08-15`
+ a full offline bundle at `D:\VOETBAL_VIDEO\RECO\backups\repo-backup-pre-v054-sync-2026-08-15.bundle`
(507MB, verified). A trial `git merge origin/main` produced 28
conflicts - investigating the biggest one (`calibration.rs`) uncovered
the real story: **upstream's current `main` is not ahead of us on the
calibration/render architecture, it regressed.** A "v0.5.3" merge
upstream (`c2705c2c`) combined the post-refactor state (our
`Calibration`/`Topology`/`Framing`/`Executor` model, which our fork
already adopted from upstream via `6ff32c37`) with a 20-commit bugfix
chain branched off an older pre-refactor point, and the resolution
discarded the newer architecture entirely - confirmed directly, zero
occurrences of `color_match_enabled`/`multiband_blend_enabled`/
`seam_offset`/`ground_tilt_x`/`Executor` anywhere in `origin/main`'s
reco-core today. Taking upstream's side would have broken compilation
in 8+ places and deleted every seam/color-match/tilt feature this fork
has built - rejected as a real downgrade, not a catch-up.

**Second, nastier discovery**: git's 3-way merge was silently
corrupting "unconflicted" regions in files affected by this
divergence - found 6+ separate cases (wrong type names, dropped
`[features]` blocks, phantom function calls to things that don't
exist on our side) in files git never even flagged as conflicted. Not
safe to trust *any* file the merge touched without a full diff against
real `main`, marked-conflict or not. Abandoned that merge entirely;
rebuilt the sync as a clean, minimal, hand-verified patch set applied
directly onto an untouched copy of `main` instead (11 files, no merge
commit lineage risk).

**What actually landed**: the 23 real upstream-only commits (merge-base
`ab553d35` to `origin/main` tip) mostly turned out to already be in our
history under different hashes (DirectML EP enable, Slint wgpu-backend
default, reco-obs Windows build.rs fixes, VRAM budget calc, CUDA
context-before-free drop fixes, audio-across-chained-segments,
recent-files video-input wiring, libcamera error logging - all
independently ported at some earlier point). Six were genuinely new
and got manually ported: frame-rate-mismatch fail-loud (`reco-io`),
stream-first fps probe fixing camera-original-HEVC-without-VUI-timing
silently defaulting to 30fps (exactly this session's own DJI Action 4
footage), encoder output stream fps-stamping, audio-passthrough now
respecting `sync_offset` (previously audio and video could start
misaligned by the sync correction itself), GUI export status/seek math
using real probed fps, and reco-detect's ORT-dylib-probe-before-ort
fix (prevents a real self-deadlock when load-dynamic + missing
runtime). Two lower-priority fixes (telemetry bug-report truncation,
replay-recording fps) identified but deliberately not ported -
low-risk, deferred.

**Verified**: `cargo build/test/clippy(-D warnings)/fmt` clean across
all 7 affected crates (241 tests passing, same 2 pre-existing CUDA
failures as always). Real end-to-end stitch against a real calibration
file + real DJI footage confirms calibration loading, audio
passthrough, and D3D11VA decode all still work. Both debug+release
`reco-gui.exe` rebuilt from the new `main`.

**Not done, deliberately**: no attempt to revert
`Calibration`/`Topology`/`Executor` naming to match upstream's
regressed shape - would touch ~40 files/92+ references for zero
functional gain and a second painful calibration-file-format
migration. If upstream ever fixes their own regression, worth
re-evaluating the "keep upstream's base" question then, not before.
See [[project_v054_upstream_sync]].

**2026-08-15: found + fixed a real export-throughput bug (D3D11VA
start-time doesn't seek), merged+pushed to `main`.** User asked to
watch the GPU during an export test, suspecting it wasn't fully
utilized (~13fps observed). Live nvidia-smi/WMI monitoring during a
real export showed nothing actually saturated (GPU SM ~58% avg, CPU
only ~1.4/24 cores) - not a raw compute limit. A `--features profiling`
perfetto trace (300 frames, 2560x1440, same settings/model as the
user's real export) found two distinct causes:
1. **`--start-time`/the GUI export in-point doesn't seek - it decodes
   and discards every frame from the start of the source up to the
   target.** Isolated cleanly with two 5-frame runs differing only in
   start time: `start-time 0` = 5.2s, `start-time 100` = 33.2s. That
   ~28s is invisible in the app's own "Processed X frames" counter
   (which only starts once real output begins) - it just looks like
   "takes a while to start", worse the further into a long recording
   the export begins. **Fixed** (Windows D3D11VA path): decode threads
   now do a real keyframe seek (`VideoDecoder::seek_to_secs`, which the
   CPU decode path already used) instead of brute-force decoding
   through the pre-roll. Verified: same 5-frame/start-100 test dropped
   to 8.9s; first-frame output confirmed pixel-identical to the old
   path at the same timestamp (correct seek target, not just faster).
   Also fixed a small unrelated pre-existing bug found via
   `clippy --all-targets` (`cuda_nv12_frames` cfg'd for
   `any(linux,windows)` but only ever called from Linux - dead code on
   Windows). **Scope: Windows D3D11VA only** - Linux CUDA zero-copy and
   macOS Metal zero-copy have the identical brute-force pattern and
   would benefit from the same fix, not done here (can't verify on
   either platform from this machine). See
   [[project_d3d11_start_time_seek_fix]].
2. **Once past startup, AI detection dominates frame time, not
   decode/stitch/encode** - the render/encode pipeline itself is fast
   and already well-overlapped (~4-5ms/frame, encoder async, 0
   backpressure stalls), but TensorRT detection (readback + preprocess
   + inference) ran ~87% of the "active" per-frame time in the same
   trace, blocking the main frame loop rather than overlapping with
   render/encode on its own thread. This explains the earlier
   nvidia-smi/CPU findings (nothing saturated - the process bounces
   between waiting on TensorRT and doing GPU render work). **Not yet
   fixed** - real architecture change (detection needs its own
   overlapped thread), bigger and riskier than fix #1, not started
   this session.
   Confirmed TensorRT genuinely active throughout (found a fresh
   matching `.engine` cache file) - ruled out the known silent-DirectML-
   fallback bug as an explanation here.
   Both debug+release `reco-gui.exe` rebuilt with fix #1.

**2026-08-15: "Ball coast time" tunable built, merged+pushed to
`main`, both debug+release `reco-gui.exe` rebuilt.** User exported a
test video with the new imgsz=1920 checkpoint (see below), confirmed
it's a real improvement, but flagged a UX gap: when the ball crosses
the field ROI line the tracker loses it immediately (the ROI filter
drops the detection before the tracker ever sees it - looks identical
to "no detection this frame"), so the camera never follows a player
stepping over the line to retrieve it. Diagnosed that `BallTracker`
already had exactly the right mechanism for this - a `Coaster`
frame-countdown holding an already-tracked ball's last position for a
brief gap - just hardcoded at `DEFAULT_COAST_FRAMES = 20` (0.67s
@30fps) and not exposed. Exposed it as `ball_coast_secs` end-to-end:
`AutocamConfig` field/builder, both `BallTracker` construction sites,
`reco-cli --ball-coast-secs` flag + run_config JSON, `AutocamDefaults`
(calibration-persisted, serde default preserves old-file behavior),
new reco-gui "Ball coast time" slider (0.2-5.0s) wired through the
usual snapshot/apply/export sites, plus new parameter docs + a
troubleshooting bullet in both `docs/ai-panner-tuning.md` and the NL
translation (explicitly distinguished from the existing
ball-anchor-range/ball-reach/FOV-wide breakaway checklist - ROI
filtering and panner gating are different failure modes with different
fixes). Raising it only affects an already-tracked ball, so it doesn't
make the panner more likely to trigger on e.g. kids playing with a
ball just outside the pitch. `cargo test/fmt/clippy` clean across
reco-core/reco-autocam/reco-cli/reco-gui; `--ball-coast-secs 2.5`
confirmed threading through into the events.jsonl run_config record on
a real export. **User-confirmed on real footage same day**:
"ball_coast: 2,5s lijkt aardig te werken" (works nicely) - docs (EN+NL)
and the settings table updated to recommend `2.5s` as the starting
value. See [[project_ball_coast_time_slider]].

**Overnight, 2026-08-14/15: imgsz=1920 full training run delegated
end-to-end while the user slept - a real, if modest, win over round-4.**
User spoke with the forum engineer (source of the original yolo26s
call) who trains at 1280 and 1920; feasibility-tested (8ep, `batch=1`,
15% data - only ~3.76GB VRAM), then a full run (`batch=2`, 258 tasks)
stopped naturally at epoch 225 (best@125, `patience=100`), 2.55h. Real-
app test on the standard 100-130s clip: overall raw-ball 51.1% (best of
every checkpoint ever tested, vs round-4's 49.1%), the persistent
720-898 dead-zone (always exactly 0/179 before, across every prior
round) now 18/179. **Not a clean win**: an intermediate 8-epoch
checkpoint scored *higher* on two curated review windows than the
fully-converged final - unexplained, not chased. **Frame-704
localization still doesn't improve** - same ~25px y-offset across all
4 checkpoints tested this session despite very different training,
looks systematic, worth its own investigation later. Also delegated:
ONNX export + a fresh frames-3705-3715 re-dump (labels + the fixed ROI
overlay, see below) for physical review. Checkpoint:
`round4/runs/yolo26s_v4_imgsz1920/weights/{best.pt,best.onnx}`. Full
detail in `YOLO26_Training.md`'s "imgsz=1920 experiment" section and
[[project_yolo26n_training_pipeline]]. **Not yet promoted to
production default** - user hasn't reviewed the overnight results yet
as of this note.

**Immediate follow-up, root-caused and fixed**: user (rightly) refused
to keep training until the "frame-704 offset looks systematic across
all 4 checkpoints" observation above was explained. Investigated
properly rather than guessing - turned out to be **one mislabeled
training example** (`winC_014` = frame 704), not a pipeline/model bug:
every other detection in that frame matched its ground truth almost
perfectly (rules out a general coordinate bug), 3 other ball labels
checked elsewhere in the dataset were all correct (rules out a
systemic labeling problem), and a pixel-grid zoom showed neither the
model nor the LS "ground truth" annotation actually touched the real
ball - the human reviewer had technically touched the box
(`origin: "prediction-changed"`) without ever moving it, same
confidence score as the original AI pre-label. All 4 checkpoints
tested this session were trained on this same image with this same bad
label, which is why the offset looked consistent - never a fair
generalization test to begin with. **Fixed both copies** (local
training label file + the LS source annotation via a transient token,
not saved to any file). Doesn't retroactively fix the 4 already-trained
checkpoints or by itself justify an immediate retrain. **Open
question, not yet done**: how many other ball labels have the same
kind of miss - only 4 spot-checked (3 clean, 1 bad); a real systematic
QA pass would be the principled next step. Full detail in
`YOLO26_Training.md`'s dedicated root-cause section and
[[project_yolo26n_training_pipeline]].

**That systematic QA pass done, same session, right after**: ran the
best checkpoint against every ball-labeled image in both train/val
splits (214 instances), matched each label to its nearest prediction.
164 OK, 16 LOW_IOU, 34 MISSED (the last almost certainly just the
known recall gap, not a labeling issue). Visually checked 5 of the 16
LOW_IOU cases (smallest-distance + the single largest) - found zero
*new* label errors: 2 were label-correct frames with multiple balls
where the model picked a different real ball, 2 were label-correct
with an unrelated model false positive elsewhere, 1 was `winC_014`
itself (still flags, expected - the current checkpoint trained on the
old label). **`winC_014` looks genuinely isolated, not a pattern -
safe to keep training/using this dataset without a full manual
re-audit.**

**Same session, earlier: found + fixed a real production bug in the
field-ROI filter, both merged+pushed to `main`.** User spotted (from a
`dump_detection_frames` overlay) that the ROI outline didn't sit on the
true sideline. Root cause: the GUI's ROI editor correctly converts
*points* from its rectified preview to raw-distorted storage space, but
not the *edges* between them - a straight line on the rectified preview
becomes a curve in the fisheye-distorted raw frame, so straight-lining
between the stored vertices drifts from the true boundary. This wasn't
just a debug-drawing issue - `RoiFilteredDetector`'s real point-in-
polygon filter had the exact same straight-edge approximation. Fixed by
moving a new `reco_core::lens::densify_polygon()` (+
`FieldRoi::densified()`) into `reco-core` so both the debug tool and
the production filter share one implementation; wired at the 3 call
sites that build `AutocamConfig` from a loaded `Calibration`. New unit
tests (caught one real bug in my own first test's assumptions along the
way - fixed, not hidden). `cargo test` green across reco-core/
reco-autocam/reco-io/reco-gui. Both debug+release `reco-gui.exe`
rebuilt. See [[project_roi_inapp_editor]] for full detail.

**Also same session: reco-gui's auto-opened update-browser replaced
with a toolbar button, merged+pushed to `main`, and an upstream PR
opened.** User found it annoying that every startup with a newer
release auto-opened a browser tab (even on debug builds). Now the
check result just sets `update-available`/`update-tag`; a new toolbar
button (visible only when true) opens the release page on click
instead. Turned out an upstream issue already existed for this
(#466, filed by a different contributor, scoped to debug builds only) -
our fix resolves it more completely (all builds, not just debug).
Opened **PR #468** against `reco-project/video-stitcher` referencing
`Closes #466`, built fresh off current `origin/main` in a throwaway
worktree (adapted to upstream's plain `Button` component and toolbar
layout, which lacks the fork-only FOV badge/`FlatButton` restyle).
Not yet reviewed/merged upstream. See
[[project_update_toolbar_button]].

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
