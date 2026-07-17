# Session handoff — 2026-07-16

Continuation note for resuming work on a different machine/session -
git-tracked so it travels with `git pull`/`push` between the user's two
PCs. Overwrite wholesale at the end of a session rather than appending
history - git history is the append-only log, this file is just "where
things stand right now."

## Current state

`main` at `0d661b87`. Working tree clean - `seam_debug.txt`/
`seam_debug2.txt`/`seam_debug.log` (harmless debug dumps) and `sticher/`
(349MB, gitignored duplicate of `target/`, no Cargo references) were all
deleted 2026-07-16/17 at the user's request ("verwijder alle bestanden
die niet meer gebruikt worden"). `vendor/` was investigated and kept -
it's actively patched in via the root `Cargo.toml`'s `[patch]` section
(wgpu/wgpu-core/naga/serde/etc., see [[project_reco_obs_wgpu_feature_bug]]),
not unused.

## Two research-only findings, noted as possible future tasks (not started)

**1. Direct YouTube upload after export.** User asked to investigate
only, nothing built. Findings: existing RTMP output (`reco-io/src/
output.rs`) is live-streaming-only, not applicable. A real "upload the
finished file" feature needs the YouTube Data API v3 (`videos.insert`),
which requires OAuth 2.0 user consent (a loopback HTTP server to catch
the redirect + secure refresh-token storage - current `reco-gui`
settings are plain JSON, not great for a secret) and a resumable
chunked-upload protocol (files are multi-GB). Real blocker: YouTube's
default API quota is 10,000 units/day and `videos.insert` costs 1600
units/call - **~6 uploads/day, shared across every user of the
distributed app** unless each user registers their own Google Cloud
project, or the project goes through Google's app-verification review
for a quota increase. No official Google Rust SDK; `google-youtube3`
(community, `google-apis-rs`) exists but feels heavy/inconsistently
maintained - hand-rolling with `reqwest`+`oauth2` would fit the
project's existing lightweight-dependency style better (precedent:
`reco-control`'s optional `gopro` feature already uses `reqwest`).
Quota is the real showstopper for open-source-scale rollout, not the
Rust implementation.

**2. Cut pieces out of a source video before stitching** (e.g. remove a
mid-match pause), not just the existing single start/end trim window.
Findings: the good news is the encoder already uses its own
continuously-incrementing output PTS counter (`reco-io/src/ffmpeg/
encoder.rs`'s `next_pts`), not input timestamps passed through - so
output-stream continuity across a skipped gap is already solved for
free. What's missing: (a) `FrameSource::skip_frames()` is a brute-force
decode-and-discard loop with no real seek in the streaming decode path
used during export (a faster `seek_to_secs` exists only in the separate
calibration-only decoder) - fine for skipping the very start, wasteful
for a multi-minute mid-video gap; (b) autocam's trajectory smoothing has
no reset hook for a sudden timeline jump - would need one at every cut
point to avoid a camera "snap"; (c) the lookahead VRAM buffer would need
an explicit flush at each cut so pre-cut frames don't leak into the
post-cut lookahead window; (d) audio must skip the same ranges to stay
in sync; (e) the existing `SegmentList`/`InputPath::Chained` mechanism
only chains whole separate files (e.g. DJI 4GB splits) - a new
"multiple time-ranges within one file" data structure + UI would be
needed, `SegmentList`'s reorder-list UI is a plausible starting point to
adapt. Feasible, comparable effort to the seam-positioning port, no
fundamental blocker.

## Upstream PRs: all 12 opened

All twelve prepared feature branches were verified (fmt/build/test
against `origin/main`) and opened as separate PRs against
`reco-project/video-stitcher`, pushed via the `fork` remote
(`RufanMelfor/video-stitcher`, the real GitHub-recognized fork - **not**
the day-to-day `github` remote, which isn't fork-linked):

- #422 `feat/windows-portability-fixes`
- #423 `fix/d3d11-stage-frame-unsafe`
- #424 `fix/concat-multisegment-seek`
- #425 `feat/inapp-roi-editor`
- #426 `feat/seam-positioning`
- #427 `feat/color-matching-multiband`
- #428 `feat/export-metadata-comment`
- #429 `feat/ground-top-tilt`
- #430 `feat/export-roi-confirm`
- #431 `feat/lookahead-8bit-downconvert` (added 2026-07-16)
- #432 `feat/audio-sync-waveform` (added 2026-07-16, see below)
- #433 `feat/reco-gui-flat-restyle` (added 2026-07-16, see below)

As of 2026-07-17 none of the 12 have been reviewed/merged yet
(`reviewDecision` empty on all, all still `MERGEABLE`).

**CLA bot fix**: all 9 original PRs initially showed a CLA-bot warning
("Rufan seems not to be a GitHub user") because commits were authored
with `info@thegrid-racing.com`, which isn't a verified email on the
user's GitHub account - `r.melfor@outlook.com` (the account's Primary
email) is. Fixed by rewriting commit author on all 9 branches
(`git commit --amend --author=...` for single-commit branches,
`git rebase --exec` for multi-commit ones) and force-pushing
(`--force-with-lease`) to `fork`. Local `git config user.email` was
also updated to `r.melfor@outlook.com` so future commits don't repeat
this. User confirmed fixed.

**10th branch, `feat/lookahead-8bit-downconvert` (PR #431)**: the 8-bit
lookahead VRAM downconvert feature (fork commit `c209b822`) was missing
from the original 9-branch batch - it only existed on this fork's own
`main`. Cherry-picked onto a fresh branch off `origin/main`; resolved 3
conflicts (dropped `FRICTION.md` - fork-only doc, never upstream; the
`render/mod.rs` module-declaration list; and
`session/frame_processing.rs`'s `stage_d3d11_frames`/
`copy_to_vram_pool_platform` blocks, using the fork's already-merged
`main` as the reference for the correct fixed-`n_slots=4` VramPool
design). Hit one build-only issue git's cherry-pick didn't flag as a
conflict: `main.slint` referenced a `Tip { tip: "..." }` tooltip
wrapper that doesn't exist on this branch at all (`Tip` is defined by a
separate, not-cherry-picked fork-only commit) - fixed by dropping the
wrapper and keeping a plain `CheckBox` with the explanation as a code
comment instead, matching upstream's tooltip-less UI convention.
Verified: `cargo fmt --check` clean, `cargo build --workspace` clean,
tests/clippy clean except for issues confirmed (by running the exact
same commands on plain `main`/`origin/main`) to be **pre-existing and
unrelated**: two `interop::cuda` test failures (`cudaGetDevice` /
`CudaError code: 3`, a local driver/environment state issue, not code),
`matroska_reader_sees_partial_writes` (a timing-sensitive
writer-still-running integration test, flaky/racy on this machine
regardless of branch), and the known `reco-core` dead-code +
`not_unsafe_ptr_arg_deref` clippy failures already tracked by open PR
#423.

**11th branch, `feat/audio-sync-waveform` (PR #432, added 2026-07-16)**:
user asked which PR shipped the audio-sync waveform, then asked for one.
It was **not** in the original 9/10 - it only existed in this fork's
`waveform.rs`/`main.slint`/`calibration_io.rs`, already fully ported
from `rig-calib` into `reco-gui` on this fork's own `main` (tracked in
`crates/reco-gui/PORTED_FROM_RIG_CALIB.md`), just never split out as its
own upstream PR. The original fork commit (`ba37aee5`) bundles this
together with 7 unrelated features (full UI restyle, sync-offset
auto-detection, playback speed, ground_tilt sliders, blend_flip_direction,
reopen-last-files) - too big and unfocused to cherry-pick wholesale, so
this branch is a **hand-extracted, minimal port**: just the
`waveform` module (`compute_envelope`/`normalize_pair_to_peak`/
`smooth_envelope`/`extract_window_envelope`, 12 unit tests), reco-io's
`extract_audio_pcm_window()`, the `AppState` fields + polling logic
(`maybe_recompute_audio_envelope`, throttled + recentered, polled from
the timer tick so it keeps updating while paused), and a Slint panel.
Deliberately excludes the sync-offset auto-detect button/feature
(`sync_offset.rs`) - unrelated logic, not asked for, can be its own PR.
Also deliberately used plain `LineEdit`/no `Tip` tooltip instead of the
fork's `NumEdit`/`Tip` components (neither exists on `origin/main`, same
lesson as the lookahead-8bit PR's `Tip` issue) - matches upstream's
existing numeric-field convention (`edited(v) => v.to-float()`) instead
of introducing new components for two fields. Verified: `cargo fmt
--check` clean, `cargo check -p reco-gui --all-targets` clean (full
`cargo build` link step deliberately skipped to avoid clobbering the
running dev-build exe the user had open - see gotcha below), `cargo test
-p reco-gui -p reco-io` all green (18 reco-gui tests incl. 12 new
waveform tests, 28 reco-io tests), clippy clean except the same
pre-existing `reco-core` failures tracked by #423.

**12th branch, `feat/reco-gui-flat-restyle` (PR #433, added
2026-07-16)**: user asked for the "full UI restyle" as its own PR too -
this had deliberately been left un-split in an earlier session (see
`ba65939d`'s note) since it's bundled in fork commit `ba37aee5` together
with 8 unrelated functional features. Scoped down further via
AskUserQuestion to **pure visual restyle only** (user picked this over
also including NumEdit or the Debug-panel feature). Hand-extracted:
`FlatButton`/`TransportButton` components, `SectionHeader` rebuild
(yellow expanded-indicator + chevron), color token re-base (cool-biased
dark neutrals, brighter accent green - light-mode values untouched),
`SegmentList` row restyle + red hover on remove ×, a non-interactive FOV
pill overlaid on the preview, and a real bug fix noticed along the way:
the toolbar's panel-toggle was a bare `Rectangle` with no `x` sibling to
the toolbar's `HorizontalBox` - Slint centers an unpositioned element
in its parent, so it likely floated in the middle of the toolbar on
`origin/main` too. Moved into the row's own flow. 46 `Button {` call
sites mechanically swapped to `FlatButton {` (verified none relied on
`Button`-only properties first). Pure `.slint` change, zero Rust edits.
Verified: `cargo check`/`test`/`clippy` all clean except the same
pre-existing #423-tracked failures.

**Not yet done**: none of the 12 PRs have been reviewed/merged upstream
yet. Live AKAZE detection preview was not started. Two ideas were
researched-only (not built, see the section above): direct YouTube
upload after export, and cutting time-ranges out of a source video
before stitching (e.g. removing a mid-match pause). No new upstream
contribution work is queued right now - next session should check PR
review status/comments on `reco-project/video-stitcher` before starting
anything new.

## Stale/locked reco-gui.exe gotcha (recurring - stay vigilant)

Checking out multiple branches in sequence for verification builds
(`cargo build`) repeatedly overwrites `target/debug/reco-gui.exe` with
whichever branch was built last. Always confirm `git branch
--show-current` = `main` and rebuild (`cargo build -p reco-gui`)
immediately before telling the user to test - this has bitten the
session multiple times now. **User explicitly complained about this
2026-07-16** ("je hebt weer mijn reco-gui debug overschreven met de oude
software") - rebuilt debug + release from `main` in response, and
confirmed both exes' mtimes.

Related, newly learned this session: if the user has a just-built
`reco-gui.exe` open to test, a later verification build on a *different*
PR branch will fail with `Toegang geweigerd` (access denied, exit code
5) trying to relink that same exe. Don't kill the user's running
instance to force it through - use `cargo check`/`cargo test` instead,
which don't touch `target/debug/reco-gui.exe`, and only rebuild the real
binary once back on `main`.
