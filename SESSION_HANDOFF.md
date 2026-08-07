# Session handoff - 2026-08-07 (evening)

Continuation note for resuming work on a different machine/session -
git-tracked so it travels with `git pull`/`push` between the user's two
PCs. Overwrite wholesale at the end of a session rather than appending
history - git history is the append-only log, this file is just "where
things stand right now."

**No usernames/passwords/IP addresses in this file, ever** - see
feedback_no_credentials_in_tracked_files.md. Reference "see password
manager" / `zerotier-cli listnetworks` instead.

## Today's biggest thread: YOLO26 fine-tuning pipeline went from "not
started" to "first real fine-tuned checkpoints exist and are visually
reviewable" in one session

Full detail in project_yolo26n_training_pipeline.md. Summary, in order:

1. **Label Studio Pi setup finished and verified end-to-end** - images
   load, 300-task pilot batch imported (verified via direct DB query,
   not just trusting the UI). Two real Label Studio gotchas hit and
   fixed, documented in the memory file (local-files serving needs a
   *registered storage connection*, not just the env vars; UI
   drag-and-drop import silently failed once, REST API import is more
   reliable and verifiable).
2. **`ultralytics` set up locally** (Python 3.14, had to explicitly
   install the CUDA build - default `pip install torch` gives a
   CPU-only wheel on this setup, needed `--index-url
   https://download.pytorch.org/whl/cu128`). Confirmed working
   (`torch.cuda.is_available()` -> RTX 3060 Ti).
3. **User caught a real problem with the pilot dataset**: the original
   300-image pilot sampled every ~1s from only the first 2.5 minutes of
   one file - too temporally dense, frames looked near-duplicate.
   Rebuilt with a wider export: full ~20-minute file, sampled every
   ~10s (`--detection-interval 30` on the `reco stitch` side to keep
   runtime sane - only recompute real detections every ~1s, decode/
   stitch still every frame - cut an 11+ min detection run down
   proportionally). New 246-image set imported as its own Label Studio
   project.
4. **User asked to compare yolo26n vs yolo26s directly** (pre-trained,
   not yet fine-tuned) - built a second parallel export with yolo26n on
   the identical window, both in separate Label Studio projects. Raw
   finding: yolo26n found the ball in 60% of sampled frames vs yolo26s's
   20% (122 vs 27 raw detections) but at lower average confidence
   (0.29 vs 0.41) - opposite of the forum-based assumption that small is
   more accurate. Flagged this needed visual verification (could be
   recall vs precision, not settled by the numbers alone).
5. **User found and fixed a real calibration bug via this review**: the
   `field_roi` didn't cover the full frame width on either camera -
   specifically excluded a compounding gap in the *middle* (the L-shape
   rig's stitch-overlap zone), meaning `RoiFilteredDetector` was
   silently dropping real players/ball detections near the seam on
   *both* cameras, not just at the frame edges. User widened both ROIs
   to properly overlap. This matters for live tracking too, not just
   training data - the seam/center area is often where the ball is.
6. **User asked for an actual first "rough" fine-tuning pass on both
   models**, capped at 200 images, after the ROI fix. Re-ran detection
   for both models with the corrected ROI (ball-present rate jumped to
   36% / 50% just from the wider ROI), exported 200 images each
   (`--sample-every 300 --max-samples 100`), built ultralytics train/val
   splits (new script: `scripts/prepare_yolo_train_split.py`,
   committed), ran 30-epoch fine-tuning for both **using the
   pre-labels as-is, no human review yet** (explicitly a rough/coarse
   validation pass, not a final model).

   **Result - decisive, and it flipped the earlier raw-detection-count
   finding**: post-finetune, yolo26s clearly beats yolo26n on this data:

   |              | yolo26s | yolo26n |
   |--------------|---------|---------|
   | mAP50 (all)  | 0.840   | 0.614   |
   | mAP50 (ball) | 0.783   | 0.394   |

   yolo26n's higher *raw* pre-trained ball-detection count turned out to
   be mostly noise/false positives - poor training signal. yolo26s's
   fewer-but-more-confident raw detections gave it a much better
   fine-tuned result. **Confirms the original forum-based decision: yolo26s
   is the right target checkpoint.** Both checkpoints exist locally:
   - `D:\VOETBAL_VIDEO\RECO\training\train_roi2_s\runs\detect\runs\rough_v1\weights\best.pt`
   - `D:\VOETBAL_VIDEO\RECO\training\train_roi2_n\runs\detect\runs\rough_v1\weights\best.pt`

7. **Ran both fine-tuned checkpoints' own predictions back through
   Label Studio** (2 more projects, sharing one image upload since both
   models were evaluated on the identical 200-image set) so the user
   can visually judge the fine-tuned output quality, not just trust the
   mAP numbers. User was reviewing this when the session ended.

**Not done yet, in order**:
- User's visual review of the fine-tuned-model prediction projects
  (in progress, session ended before feedback came back).
- Human review/correction of the pre-label batches themselves (several
  Label Studio projects now exist - pilot, wide-sample v2 for both
  model sizes, and the two fine-tuned-prediction ones - none has had a
  real correction pass yet, all still just raw predictions).
- A *real* fine-tuning run (larger reviewed dataset, not 170 uncorrected
  images) once review happens.
- ONNX export of the fine-tuned checkpoint to actually test it inside
  `reco` itself (asked, not yet done).
- **Housekeeping note about the Pi**: there are now 8 Label Studio
  projects total (ids 2, 5, 6, 7, 8 confirmed this session, IDs 3-4 are
  likely stray/test - check before assuming a clean slate). User said
  "die eerste pilot project kan dan weg gegooid worden" (project 2) at
  one point - not actually deleted yet (got sidetracked into a token
  question), still there, safe to clean up next session if still
  wanted.

## Upstream sync question (v0.5.4) - assessed, deliberately NOT acted on

User asked what to do about upstream's new `v0.5.4` release without
disrupting current work. Checked: our fork's `main` and `origin/main`
have diverged substantially (91 fork-only commits vs 23 new upstream
commits). Test-merged v0.5.4 into a throwaway branch (never pushed,
cleaned up after) to see real conflict scope: **20 conflicting files**,
including a real structural one - upstream *deleted*
`crates/reco-core/src/stitch/{executor.rs,mod.rs}` entirely (likely
folded into `session::run_loop`/`session::frame_processing`, which also
conflict), while our fork kept modifying that now-gone module. This is
not a quick/safe sync - needs a dedicated, focused session to actually
understand and reconcile, not something to squeeze in as an aside.

**User's stance**: wants to stay up to date long-term, but agreed this
specific sync should wait for its own dedicated session rather than
being rushed now. **Not started, no branch created for it** - next
session, this needs real focused time, ideally starting with
understanding *why* upstream removed the `stitch` module (read the
relevant upstream PR/commit history first) before attempting a real
resolution.

Also worth noting for later: v0.5.4's release notes mention "Restore
yolo26n.onnx and yolo26n_640.onnx release assets with pinned checksum
verification" - the `yolo26n.onnx`/`yolo26n_640.onnx` files already
sitting in `D:\VOETBAL_VIDEO\RECO\` may be from this or a similar
official release rather than (or in addition to) the community
HuggingFace conversion noted earlier - worth a provenance check if it
ever matters (e.g. before redistributing).

## Goal-scored detection (branch `feat/goal-line-calibration`) - PAUSED,
no code changes this session

Explicitly paused per user's own call: the current ball model's
detection quality (16-20% ball-present rate on raw yolo26s, much worse
pre-finetune) makes it premature to keep debugging the goal-entry
polygon/timing - the YOLO fine-tuning thread above *is* the fix for
that underlying weakness. Resume once a meaningfully better checkpoint
exists (see above - a real, reviewed-dataset fine-tune, not just
today's rough 170-image pass). Branch still exists, pushed, nothing new
to do here until the model side catches up. Full prior context still in
project_goal_detection_idea.md and this file's git history.

## Other threads, unchanged

- **Veo Cam 3 competitive roadmap**: `docs/research-veo-cam3-comparison.md`.
- **Upstream PRs** (#422-435, #464): awaiting owner review/merge.
- reco-gui app icon: still waiting on a source image from the user.

## Housekeeping this session

- **Cross-machine git identity gap confirmed and documented** (not a
  bug): commits from RUFAN_LAPTOP show a different author name/email
  than this machine - see feedback_cross_machine_handoff.md.
- Label Studio API auth: legacy static tokens are disabled for this
  org (left that way deliberately - user said "voor nu even zo laten"
  rather than re-enable, since re-enabling is a security-relevant
  toggle needing explicit approval). Every new project-creation/import
  session needs a fresh JWT refresh token from Account & Settings -
  known friction, accepted for now.
- `reco stitch --model` with default lookahead can hit a VRAM error on
  an 8GB card even at modest output res - `--lookahead 0` avoids it for
  any run that doesn't need the smoothed AI-panned output itself (e.g.
  a detection-only `--events` dump). `--detection-interval N` is a
  separate, big speedup lever for exactly this kind of run - real
  detection only every Nth frame, "last known" reused on skipped
  frames; safe to use when only specific sampled frames matter (e.g.
  matching `export_yolo_labels.py --sample-every`), since the reused
  values are simply never sampled.

## Machine-specific reminders (still valid)

- FFMPEG_DIR and LLVM PATH needed per-shell for any Rust build - see
  env_build_requirements.md.
- Don't drive reco-gui's UI with synthetic mouse/keyboard input.
- reco-obs won't build on this machine right now (missing
  `obs-frontend-api.lib`) - expected, exclude it
  (`--exclude reco-obs`) from workspace-wide build/test commands.
- This machine's Python is 3.14 (no 3.12 install despite a stale `py`
  launcher registry entry claiming otherwise) - `ultralytics`/`torch`
  both have working cp314 wheels as of this session, but torch's
  default PyPI wheel is CPU-only; the CUDA build needs the explicit
  `--index-url https://download.pytorch.org/whl/cu128` (or whatever
  tag matches the installed driver's max CUDA version - check
  `nvidia-smi`'s reported "CUDA Version" first).
- `git fsck --full` before pushing, per feedback_git_object_corruption.md.
