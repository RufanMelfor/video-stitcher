# Session handoff — 2026-07-18

Continuation note for resuming work on a different machine/session -
git-tracked so it travels with `git pull`/`push` between the user's two
PCs. Overwrite wholesale at the end of a session rather than appending
history - git history is the append-only log, this file is just "where
things stand right now."

## Current state

`main` has a large pile of **pre-existing, uncommitted WIP** that predates
this session and was not finished or touched beyond what's noted below:
`crates/reco-calibrate/examples/fit_ground_tilt*.rs`, `fit_photometric.rs`,
`match_png_frames.rs`, `reco-calibrate/src/optimizer.rs`,
`reco-core/src/lens/mod.rs`, `reco-core/src/render/{pipeline,renderer,
scene}.rs`, `reco-core/src/stitch/executor.rs`, `docs/ai-panner-tuning.md`
(+ untracked `docs/ai-panner-tuning.nl.md`). Judging by `main.slint`'s diff
this includes independent left/right camera rotation sliders
(`cal-x-rx`/`cal-z-rz`, "Force extra rotation fit"), Shift-held vertical-only
drag panning while "Show seam line" is on, and a lens-zoom scroll-direction
fix. **Not evaluated or verified this session** - whoever picks this back up
should treat it as someone else's in-progress work, not assume it's done.

Two things layered on top of that same dirty tree this session, **committed
nowhere, debug-build-tested only**:
- Color-match "Measurement band" slider (`crates/reco-gui/ui/main.slint`):
  max raised 0.4 → 1.0, reordered above "Max Y offset", renamed to "MEAS Band
  Width". User confirmed a wider band visibly fixes the left/right color
  mismatch they reported (right camera looked much cooler/bluer than left
  across the whole frame - the old 0.4 cap wasn't wide enough to correct it).
- Nothing else from this session touches the fork's `main` - the Default
  Calibration feature below was built fresh against `origin/main` in a
  separate worktree, not layered onto this dirty tree.

## PR #427 (`feat/color-matching-multiband`) - blocked, waiting on the owner

Investigating "add the band-width tweaks to the existing PR" found PR #427
is an old, toggle-only snapshot of color-match - none of the tunable sliders
above exist there, and neither do the later `seam_offset`/
`blend_flip_direction` measurement-band bug fixes documented in
`crates/reco-core/FRICTION.md`. The two small tweaks can't be cherry-picked
onto that branch in isolation since the sliders themselves don't exist yet
on it.

Translated the situation to English for the user to relay to the
`reco-project` owner, asking how to proceed: (a) bring PR #427 fully up to
date with the current feature then layer the tweaks on top, (b) leave PR
#427 as-is for now, or (c) discuss process first since **all 12 original
PRs may have similar drift** (all were snapshotted off `origin/main` at
whatever point they were prepared, then fork `main` kept moving).

**Still pending as of 2026-07-18 - do not touch PR #427 until the user
comes back with an answer.**

## New feature: Default Calibration preference - DONE, PR #435 open

User asked for a Preferences-menu "Default Calibration" file that reco-gui
falls back to whenever no calibration is otherwise loaded, plus a
confirmation popup before saving over that specific file (protects it from
being silently overwritten by an in-progress session's edits).

Built as a 13th branch/PR, **directly off `origin/main`** in a throwaway
`git worktree` (not layered onto the fork's dirty `main` above, which has
unrelated WIP mixed into the exact files this touches). Confirmed
`origin/main` already has all the base infrastructure needed (`GuiSettings`,
Preferences dialog, `save_calibration`/`try_init`) with no fork-only
dependency, so this was a clean from-scratch implementation rather than a
port.

- `GuiSettings::default_calibration_path: Option<PathBuf>` +
  `default_calibration()` accessor (only returns the path if it still
  exists on disk).
- Preferences dialog: new "Default calibration" row (LineEdit + Browse…).
- Fallback wired into `try_init_and_update` - the one point every
  left/right/calibration pick path converges on before init - so an
  explicit pick always wins, the default only applies when nothing else is
  loaded.
- `AppState::is_default_calibration()` + a new `overwrite-default-cal-
  warning-open` modal: saving over the configured default now asks for
  confirmation first instead of silently overwriting it.

Verified: `cargo build`/`fmt --check` clean, `cargo test -p reco-gui
settings::` 5/5 (3 new tests), clippy's 4 errors are the same pre-existing
`reco-core` dead-code/unsafe-ptr issues already tracked by PR #423 (not
caused by this change). **User manually tested the running feature and
confirmed it works** - the PR's test-plan checkbox for manual verification
was updated to checked (user approved that edit first).

PR: [reco-project/video-stitcher#435](https://github.com/reco-project/video-stitcher/pull/435),
branch `feat/default-calibration-preference`, pushed via `fork`
(`RufanMelfor/video-stitcher`). Awaiting owner review/merge like the rest
of the batch.

## Upstream PRs: 13 opened, none merged yet

- #422 `feat/windows-portability-fixes`
- #423 `fix/d3d11-stage-frame-unsafe`
- #424 `fix/concat-multisegment-seek`
- #425 `feat/inapp-roi-editor`
- #426 `feat/seam-positioning`
- #427 `feat/color-matching-multiband` - **behind current `main`, fix pending owner's answer (see above)**
- #428 `feat/export-metadata-comment`
- #429 `feat/ground-top-tilt`
- #430 `feat/export-roi-confirm`
- #431 `feat/lookahead-8bit-downconvert`
- #432 `feat/audio-sync-waveform`
- #433 `feat/reco-gui-flat-restyle`
- #435 `feat/default-calibration-preference` (added 2026-07-18, see above)

CLA-bot email issue was already fixed in an earlier session (commits
authored with the GitHub-verified `r.melfor@outlook.com`, not
`info@thegrid-racing.com`) - `git config user.email` is already correct on
this machine, confirmed again while preparing PR #435.

## Not yet done / next-session starting points

1. **PR #427**: waiting on the owner's answer (relayed by the user) on how
   to handle the drift described above. Don't start on it until that comes
   back.
2. **The uncommitted WIP on `main`** described in "Current state" above
   (camera-rotation sliders, shift-drag panning, lens-zoom fix, plus the
   color-match slider tweaks) - none of it is committed, tested with
   `fmt`/`clippy`/`cargo test`, or turned into a PR. Needs picking back up
   deliberately, not assumed finished.
3. None of the 13 PRs have been reviewed/merged upstream yet - check
   review status/comments on `reco-project/video-stitcher` before starting
   new upstream work.
4. Live AKAZE detection preview PR was never started.
5. Two research-only findings from 2026-07-16, still not built:
   - **Direct YouTube upload after export**: needs YouTube Data API v3
     OAuth + resumable chunked upload; real blocker is the default API
     quota (~6 uploads/day shared across every user of the app) unless
     users register their own Google Cloud project or the project goes
     through Google's quota-increase review. No official Google Rust SDK;
     hand-rolling with `reqwest`+`oauth2` fits the project's style better
     than the community `google-youtube3` crate.
   - **Cutting time-ranges out of a source video before stitching** (not
     just the existing single start/end trim): output PTS is already a
     free-running counter decoupled from input timestamps, so the hard
     part (output continuity across a skipped gap) is solved. Missing:
     a real seek in the streaming decode path (current `skip_frames()` is
     brute-force decode-and-discard), an autocam trajectory-smoothing
     reset hook at cut points, a lookahead-buffer flush at each cut, audio
     skipping the same ranges, and a new "multiple time-ranges in one
     file" data structure/UI (`SegmentList`'s reorder UI is a plausible
     starting point to adapt). Feasible, no fundamental blocker.

## Stale/locked reco-gui.exe gotcha (recurring - stay vigilant)

Checking out multiple branches in sequence for verification builds
(`cargo build`) repeatedly overwrites `target/debug/reco-gui.exe` with
whichever branch was built last. Always confirm `git branch
--show-current` = `main` and rebuild (`cargo build -p reco-gui`)
immediately before telling the user to test.

If the user has a just-built `reco-gui.exe` open to test, a verification
build on a *different* branch/worktree will fail with `Toegang geweigerd`
(access denied) trying to relink that same exe if it somehow shares a
target dir. Building PR branches in a separate `git worktree` (own target
dir) - as done for PR #435 this session - avoids this entirely and is the
preferred approach going forward for any PR that isn't meant to touch the
user's day-to-day debug build.

## Build environment reminder (this machine, TGR_PC)

Any fresh `target/` (e.g. a new worktree) needs, per-shell:

```bash
export FFMPEG_DIR="C:/Users/Rufan/AppData/Local/Microsoft/WinGet/Packages/BtbN.FFmpeg.GPL.Shared.7.1_Microsoft.Winget.Source_8wekyb3d8bbwe/ffmpeg-n7.1.4-7-gadcf20da26-win64-gpl-shared-7.1"
export PATH="/c/Program Files/LLVM/bin:$PATH"
```

Without it, `ffmpeg-sys-next`'s build script fails looking for vcpkg/
pkg-config. See [[env_build_requirements]] for full detail.
