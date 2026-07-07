# reco-calibrate friction log

## Bottom-of-frame (near-camera) seam misalignment survives an otherwise-good calibration

**Symptom:** With blend width set to 0 (hard seam), ground markings far from
the camera (halfway line, center circle, goal area) line up perfectly across
the stitch seam, but a line close to the camera - bottom ~10-15% of the frame
- visibly steps/jogs at the seam, even though the optimizer reports near-zero
residual error and 100% confidence.

**Investigated and ruled out (2026-07-04, real DJI Osmo Action 4 match
footage, `crates/reco-calibrate/src/optimizer.rs` / `geometry.rs`):**

1. **Not an AKAZE detection/matching band issue.** Widening
   `AkazeConfig::detect_y_max` from 0.85 to 0.97 and `MatchConfig::spatial_y_high`
   from 0.8 to 0.95 (so features are actually looked for closer to the bottom
   edge) barely changed the matched-point y-range (max plane-y moved from
   0.013/0.014 to 0.011/0.014) and *reduced* total matches slightly (100 to
   94). Close-up turf simply lacks distinctive AKAZE-matchable texture -
   there's little to gain by looking there, regardless of band width.

2. **Not a lens undistortion / distortion-coefficient problem.** Dumped the
   GPU-undistorted single-camera debug frames (`reco calibrate --debug-dir`)
   and quantitatively fit a known-straight real-world line (pitch sideline)
   near the bottom of frame: linear fit RMSE 1.3px on a ~250px, full-4K-res
   segment; a quadratic (curved) fit only reduced RMSE by 4.6% - i.e. no
   detectable residual bowing. The embedded lens profile for this rig came
   from the camera's own factory telemetry (DJI `ClipMeta`), not a generic
   database fallback, and it rectifies straight lines correctly even at the
   frame edge closest to the camera.

   **2a. Correction + follow-up (2026-07-05) - "from ClipMeta" does not mean
   per-unit.** User raised a sharp question: the lens calibration pulled from
   metadata is the same for any camera of this model - what if the two
   physical lenses have a small individual manufacturing deviation from that
   shared model, and it happens to show up worst exactly at the bottom of
   frame (where fisheye distortion error is largest, same region as the
   seam)? Checked directly: `reco calibrate`'s own log for this rig prints
   `embedded lens (from ClipMeta): focal=1457.07, d=[0.1551, 0.1371,
   -0.0939, 0.0042]` **identically for both the left and right video** -
   confirming the "from ClipMeta" wording above is technically accurate
   (it's not the Gyroflow database fallback) but was misleading: DJI bakes
   one fixed distortion model per camera *model* into every Osmo Action 4's
   metadata, not a per-unit factory measurement - there is no serial number
   or per-device field anywhere in this data or the code that consumes it.
   So this system currently has no way to see a real per-unit lens
   difference even if one exists.

   To test whether that blind spot is actually costing anything on this
   rig's two specific units: repeated the same straight-line RMSE method
   from point 2 independently on **both** cameras (previously only checked
   one) using two different real sidelines, one per camera, both near the
   bottom of frame. Result: left 0.611px RMSE (568 points, 567px span),
   right 0.630px RMSE (532 points, 540px span) - a 1.03x ratio, i.e.
   essentially identical, and both far too small (sub-pixel) to explain a
   visually obvious seam step. **This specific pair of lenses shows no
   meaningful difference in rectification accuracy from each other** on the
   region that matters. The architecture-level blind spot (identical,
   non-per-unit distortion data) is real and worth knowing about, but for
   these two units it doesn't appear to be the actual driver of the seam
   problem - reinforces that the root cause is the extrinsic placement
   model (below), not lens calibration.

**Root cause:** the placement/extrinsic model
(`geometry::apply_transformations`) represents the two camera views as two
*flat* vertical planes meeting at a virtual camera corner ("L-shape"). This is
a reasonable approximation of the real, continuous ground-plane perspective
far from the camera, but it diverges from it fastest close to the camera,
where true ground-plane foreshortening is most extreme. No combination of the
existing placement parameters (`cam_d`, `intersect`, `x_ty`, `x_rz`, `z_rx`,
`x_rx`, `z_rz`) can correct this, because it isn't a placement error - it's
the model's shape being wrong for that region.

Separately (and independently limiting): the seam-weighted cost function
(`geometry::per_point_seam_weighted_errors_full`) applies a vertical Gaussian
weight centered near image-center (`SeamWeightConfig::y_center = -0.05`,
`sigma_y = 0.08`), which already collapses the weight of any near-bottom
points toward zero. So even where a few near-camera matches do exist, they
barely influence the fit today.

**Impact:** cosmetic only, and only visible with little to no seam blending
(default rig-calib blend width is 0.05, not 0) - but worth knowing before
spending time re-tuning AKAZE bands, seam sigma, or lens profiles to chase
this specific symptom; none of those are the actual lever.

**Proposed fix (not started, substantial - not a tuning fix):** replace the
flat two-plane extrinsic model with one that follows the true ground plane
near the camera (e.g. a ground-plane homography or dome/hemisphere model for
the near field, blending into the current flat-plane model further out).
This is a geometry-model change, not an optimizer-parameter addition.

**Implemented instead (2026-07-04): empirical `ground_correction` render
tunable.** A proper ground-plane model needs the camera's height and tilt
relative to gravity, which this rig doesn't have (no raw IMU gyro), and
deriving it from known pitch-marking dimensions is a separate, much larger
feature. Given that, added a manually-tunable per-fragment remap instead of
another AKAZE-fit optimizer parameter (already shown above not to work: too
few reliable near-field matches to fit against). `ViewportConfig::ground_correction`
(persisted on `MatchCalibration`, live-tunable in rig-calib exactly like
`blend_width`) nudges each plane's vertical UV mapping near the bottom edge
in `fisheye.wgsl`, opposite sign per plane, ramped smoothly from 0 at
`uv.y = 0.75` to full strength at the bottom edge. Verified on the real
Berghem footage: `+0.02` visibly closed the seam step with no distortion;
`+0.04` overcorrected (step reappears the other way); `-0.02` had no
effect (confirms direction is footage/rig-specific, not universal - this
is a per-rig hand-tuned value, not a fixed default). Does not touch
`projection/mod.rs`'s ray-cast math (used by the autocam panner/no-black
viewport bounds) - visual/render-only, deliberately out of scope since it
only affects the already-AKAZE-excluded near-bottom band.

**Known limitation found after shipping (2026-07-04):** `ground_correction`
shifts pixels by image row, not real depth. Ground content (a fixed line)
has one row per position, so the shift is exact. A standing player spans
several rows while sitting at nearly one real depth (their height is small
next to their distance from the rig), so their feet and head get different
corrections - visible as shearing/ghosting on anyone standing in the
corrected band. Confirmed on real match footage: `+0.02` closed the seam
step cleanly on an empty-pitch frame, but a player crossing that band
later in the same clip showed a clear double-image artifact. Only use
`ground_correction` for stretches where nobody is in the bottom of frame;
the UI tooltip and CLI help text both carry this warning.

**Removed (2026-07-05):** per user decision, the player-ghosting caveat
above makes this an unacceptable fix, not just a documented tradeoff -
"not the perfect solution". `ground_correction` was pulled out entirely
(struct field, GPU uniform, shader ramp, CLI flags, rig-calib slider/
callback) rather than kept as a footgun. Left here as a record that it
was tried and why it didn't survive; the underlying near-field misalignment
this was compensating for is still open (see "Bottom line" below).

**Investigated and ruled out further (2026-07-04, same session, in
response to the ghosting finding above) - is there a way to do this
properly using only this footage/metadata, no new capture:**

1. **User's hypothesis: since the two cameras are rigidly mounted, isn't
   the correction just a fixed 2D warp, no depth needed?** No - confirmed
   via a concrete parallax argument (equivalent to the "hold a finger up,
   blink each eye" test): a fixed baseline between two lens centers still
   produces a *depth-dependent* pixel correspondence between the two
   views. A single 2D transform (however derived) cannot simultaneously
   align near and far content when there's real parallax, regardless of
   whether that transform comes from a 3D model or a plain homography.

2. **Reverse-engineered a real competing product (ActionStitch,
   actionstitch.com) that the user had prior success with**, via its
   installed `actionstitch.log` (`%LOCALAPPDATA%\actionstitch.com\
   ActionStitch\actionstitch.log`): it fits a single global 2D homography
   (`cv2.findHomography`-style DLT + RANSAC) from feature matches, e.g.
   `feature matches: 3719 -> refined feature matches: 387 -> homography:
   [[...]]`. Not a 3D ground-plane model, not multiple local regions -
   same category of "one global rigid fit" as our own optimizer, just an
   unconstrained 8-DOF projective transform instead of our
   physically-parameterized 7-parameter one. Their own guide documents
   that parallax exists (*"the ball position is slightly different in
   both frames - this is normal"*) and recommends placing the seam away
   from busy areas rather than modeling depth - i.e. they don't solve
   this either, they mitigate visibility.

3. **Tested fitting a plain global homography to our own existing AKAZE
   matches** (added a temporary `matched_points.json` debug dump in
   `reco-cli/src/calibrate.rs` - kept as a permanent debug-dir output,
   it's cheap and reuses already-serializable data): a robust
   (outlier-trimmed) fit scored a deceptively good sub-2px median
   residual on both near and far points. But visually warping one camera
   into the other's frame with that homography produced a nonsensical
   result outside the fitted region - because our ~17-49 matches per
   frame all cluster tightly in one small area of the image (confirmed by
   viewing the existing `matches_00_left/right.png` debug visualizations
   before assuming the numbers were trustworthy), so an 8-DOF homography
   is under-constrained everywhere else and extrapolates wildly. The good
   residual numbers were an overfitting artifact, not a real result -
   this cost real time to discover and should have been checked visually
   from the start rather than trusting the residual statistic alone.

4. **Tried the already-built XFeat AI feature matcher**
   (`reco-calibrate`'s `ai-features` Cargo flag; added a
   `--use-ai-matching` flag to `reco-cli`'s `calibrate` subcommand -
   `rig-calib` already had this wired to a checkbox but `reco-cli` didn't)
   as an alternative to AKAZE, hoping for more, better-distributed
   matches. Found far more raw candidates (500-570/frame vs AKAZE's
   2000-descriptor pool yielding only 24-69 post-ratio-test) but RANSAC
   had to reject ~95% of them, and the calibration that resulted was much
   *worse* (residual 0.0598 vs AKAZE's 0.000011; `cameraAxisOffset`,
   `xRz`, and `zRx` all pinned at their bounds - a clear sign of a bad
   fit). The surviving AI matches clustered around the players rather
   than field lines (likely confusing visually-similar players with each
   other) instead of being more widely spread. Not usable on this
   footage - **the whole XFeat/AI-matching integration (`ai_features.rs`,
   the `ai-features` Cargo feature everywhere, the ONNX models, the
   rig-calib checkbox) was removed** (2026-07-04) rather than kept
   as unused infrastructure, per user decision to commit fully to AKAZE.

5. **Widening the horizontal overlap region (`--detect-x` /
   `MatchConfig::spatial_x_threshold`, default 0.5) toward the fitted
   `intersect` (~0.656) does not help either, and isn't actually the
   right lever.** `spatial_x_threshold` only widens which *horizontal*
   slice of each camera's frame AKAZE searches - it has no effect on how
   close to the *bottom* edge (the near-field problem) matches can be
   found; that's `detect_y_max`, already tested in point 1 above.
   Confirmed empirically anyway: widening `--detect-x` from 0.5 to 0.35
   *reduced* match count (e.g. 66 → 43 on the same 2 frames) because the
   detection crop is downscaled to a fixed `detect_max_width` (1920px by
   default) before AKAZE runs - a wider crop hits the same pixel budget
   harder, lowering effective resolution. Added a `--detect-max-width`
   CLI flag to `reco-cli` (mirrors rig-calib's existing "Full-resolution
   features" checkbox, which was the only way to control this before) to
   properly isolate this effect; even with the cap removed entirely,
   match count still didn't improve and the y-range didn't extend any
   closer to the bottom (unsurprising - `detect-x` doesn't touch `y` at
   all). Not a wasted CLI addition though - useful for future
   full-resolution-detection experiments in general.

6. **Sampling many more frames (15 instead of 4) does surface real
   near-field matches - but the optimizer still can't use them to fix the
   visible seam, which closes off "more/better data" as a possible fix.**
   With 15 sampled frames spread across the match, matched points reached
   plane-y up to 0.148 (vs 0.013-0.014 with 4 frames) - genuinely deep into
   the previously-empty near-field band, 402 points total. But the
   existing seam-weighting (`SeamWeightConfig::sigma_y`, hardcoded to 0.08
   until now) collapses the weight of anything that far from center to
   near-zero, so those points barely influenced the fit. Exposed
   `sigma_y` as `OptimizerConfig::seam_sigma_y` (new `--seam-sigma-y` CLI
   flag, default unchanged at 0.08) and swept it from 0.08 to 1.0 against
   the same 402 points (re-running just the optimizer against the cached
   `matched_points.json`, skipping the slow AKAZE step): the core placement
   parameters (`cam_d`, `intersect`, `x_ty`, `x_rz`, `z_rx`) stayed
   essentially frozen across the entire sweep, and residual error got
   *slightly worse*, not better. In other words: even when the near-field
   data is available and given full weight, the model finds no different
   (better) fit - it's already at the best compromise between near and far
   content, and that compromise still shows a visible seam step up close.
   This is strong direct confirmation that the root cause is the model's
   shape (see below), not a data or weighting shortfall - "sample more
   frames" and "reweight the cost function" are now both ruled out
   experimentally, not just by argument.

7. **Narrowing `--detect-x` toward the seam (the opposite of point 5's
   "widen" experiment) fails even harder - confirmed empirically on the
   real Berghem footage (2026-07-05).** The idea: restrict AKAZE to a
   thin strip immediately at the overlap edge instead of the default
   half-frame, on the theory that only seam-adjacent matches should
   matter for seam alignment. Tested `--detect-x 0.75` and `0.9` against
   the `0.5` baseline (6 frame pairs, DJI Osmo Action 4, otherwise
   identical settings):
   - `0.5` (baseline): 171 total matches, 6/6 frames usable, per-frame
     keypoint counts 12-58.
   - `0.75` (strip width ~992px of ~3840px): spatial filter yielded only
     0-3 matches per frame (`< 6 required`) - **0/6 frames usable**,
     calibration failed outright (`no usable frame pairs`).
   - `0.9` (strip width ~417px): same failure, 0/6 frames usable.

   Root cause: the pitch markings AKAZE actually locks onto (goal area
   lines, center circle, sideline) are distributed across the overlap
   region, not clustered right at its outer edge - a strip that thin
   just doesn't contain enough distinct texture for the ratio test +
   spatial filter to survive on, regardless of how relevant that area is
   to the seam. Combined with point 5, this means `spatial_x_threshold`
   is already close to a local optimum for match count in both
   directions at its default (`0.5`): widening it dilutes resolution
   against the downscale cap, narrowing it starves the match count
   entirely. Not a viable lever for the seam problem - reverted, no code
   changes kept from this experiment (only this record).

8. **Ground-plane perspective correction via a single fitted `ground_tilt`
   parameter (2026-07-05) - tested on real data, doesn't work as hoped, but
   the reason why is instructive.** User asked for a universal (no manual
   measurement, no sport-specific markings, works for any third-party rig)
   software fix, ruling out the physical "move the cameras closer" option.
   Hypothesis: replace the flat plane's linear pixel-to-plane mapping with
   an Inverse-Perspective-Mapping-style correction - one new scalar `c`
   (`ground_tilt`, `= tan(theta)` for some additional downward tilt `theta`
   the flat-plane model doesn't account for), applied via the
   tangent-addition identity `warp(t, c) = (c + t) / (1 - c*t)` to each
   matched point's plane-y coordinate before the existing rotation/
   translation math (`geometry::apply_transformations`, `geometry::
   warp_ground_y`). Deliberately one shared scalar instead of separate
   height + tilt params, to minimize new degrees of freedom given this
   project's two prior overfitting failures (homography, XFeat) on sparse
   AKAZE data.

   Kept entirely inside `reco-calibrate` for this phase - no config flag,
   no CLI flag, no renderer/GPU changes. Validated via a new standalone
   harness (`cargo run --release -p reco-calibrate --example
   fit_ground_tilt -- <matched_points.json>`) that pre-warps points by
   candidate `c` values and re-fits the existing, completely unmodified
   5-parameter optimizer against them - mathematically identical to
   `apply_transformations` applying the warp internally, so it validates
   the idea without touching the optimizer/config/CLI at all.

   Also fixed a real, unrelated bug found while reading this code: `OptParams
   .x_rx` was declared and consumed at render time
   (`SceneGeometry::from_layout_with_aspect`) but never read inside
   `apply_transformations`, so enabling `--enable-x-rx` gave the CPU
   optimizer a parameter with zero gradient - it could never actually be
   fit, only left at its IMU-seeded start value while the renderer applied
   that unfit value. Fixed alongside the ground_tilt work since it's the
   same function and the same "does this parameter actually change
   anything" class of question. Added regression tests for both parameters
   proving they change `reprojection_error` (`x_rx_changes_reprojection_error`,
   `ground_tilt_changes_reprojection_error`) so neither can silently regress
   into a no-op again.

   Regenerated real 15-frame match data on the Berghem footage (402 points,
   same dataset shape as point 6 above, y-range up to 0.148) and ran the
   grid-search harness. Result:

   - Baseline (`c=0`, today's model): near-field residual 1.348, far-field
     residual 0.149, `cam_d=0.189`.
   - **Far-field residual got monotonically worse for every single
     non-zero `c` tested, in both directions** - 0.149 at `c=0` up to 0.94
     at `c=±0.30`. There is no `c` that improves near-field without costing
     far-field.
   - **`cam_d` pegs exactly at its bound (0.30) for most of the useful `c`
     range** - this project's own documented tell for a bad/under-constrained
     fit (same symptom as the XFeat failure in point 4 above).
   - The best-behaved (non-pegged) candidates were small `|c|` (~0.02-0.06):
     e.g. `c=-0.04` gave near=0.395 (large improvement over 1.348) but
     far=0.187 (26% worse than baseline) and `cam_d=0.283` (pushed most of
     the way to its bound). `c=+0.04` was more conservative (near=0.943,
     far=0.156, `cam_d` barely moved) but the near-field improvement was
     much smaller. One grid point (`c=+0.02`) produced a wild divergence
     (near=191) - a sign the added dimension makes the optimization
     landscape rougher, not just shifted.

   **Root cause of the trade-off, confirmed mathematically, not just
   empirically:** `warp(t=0, c) = c`, not `0` - i.e. the tangent-addition
   formula shifts *every* point by approximately `c`, near-field and
   far-field alike, because it's mathematically a global reparametrization
   of the camera's effective tilt angle, not a correction localized to the
   near-field band. Far-field content that was already well-calibrated gets
   perturbed by the same global shift, and the existing hinge parameters
   (especially `cam_d`) partially re-absorb it rather than the new
   parameter adding clean, orthogonal information - exactly the
   identifiability risk flagged before running this experiment.

   **Conclusion: a single globally-applied ground-plane tilt parameter is
   not a viable fix as implemented** - it trades far-field accuracy for
   near-field accuracy rather than improving both, and pushes `cam_d` to
   its bound doing so. If this direction is revisited, the fix suggested by
   the math is to make the correction *localized* (e.g. a smooth
   band-limited falloff toward zero away from the near-field region,
   similar to `ground_correction`'s `smoothstep(0.75, 1.0, uv.y)` ramp,
   rather than a global reparametrization) so far-field content is
   mathematically guaranteed untouched - not attempted here, out of scope
   for this phase. Code kept (the `warp_ground_y`/`ground_tilt` field, the
   `x_rx` fix, the harness, and the tests) since the `x_rx` fix is a real
   bug fix independent of this result, and the harness is reusable if a
   band-limited variant is tried later; `ground_tilt` stays `None` in every
   production code path (optimizer, config, CLI, renderer all untouched).

9. **Band-limited variant of point 8, tried immediately after (2026-07-05,
   same day) - fixes the trade-off, but the honest improvement is small.**
   Implemented `geometry::band_limited_ground_warp`: blends between
   identity and `warp_ground_y` via a smoothstep on `|t|`, mathematically
   guaranteeing points with `|t| <= 0.08` (`GROUND_TILT_BAND_START`) are
   *exactly* untouched (bit-identical, not just approximately close),
   ramping to the full correction by `|t| >= 0.16` (`GROUND_TILT_BAND_FULL`)
   - the same "correction ramps in, provably zero outside the band" shape
   as the reverted `ground_correction` shader ramp, applied to the fit
   instead of a render-time pixel shift. `apply_transformations` now calls
   this instead of the raw warp; `warp_ground_y` itself is unchanged and
   kept (still has its own passing unit tests, still the building block
   the banded version wraps).

   Re-ran the same `fit_ground_tilt` harness against the same real 402-point
   dataset. **This fixed exactly the failure mode point 8 predicted and
   found:**
   - Far-field residual is now flat across the *entire* grid
     (0.1487-0.1489 for every `c` from -0.30 to +0.30, vs. baseline
     0.148785) - compare to point 8's result, where it ranged up to 0.94.
   - `cam_d` stays in a tight 0.185-0.191 band across the *entire* grid,
     nowhere near its bounds (vs. point 8's result, where it pegged at
     0.30 for most of the range) - the aliasing-with-`cam_d` failure mode
     is gone.
   - The other core parameters (`intersect`, `x_ty`, `x_rz`, `z_rx`) also
     stay close to their baseline-fitted values across the whole grid -
     no wild swings.
   - **But the actual near-field improvement is modest:** best candidate
     (`c=+0.02`) reduces near-field residual from 1.348 to 1.283 - about
     6%, not the 30-70% swings point 8 saw (which, as established, were
     an artifact of `cam_d` cheating rather than a real fit). Sensitivity
     check: tightening `GROUND_TILT_BAND_FULL` from 0.16 to 0.12 (so more
     of this dataset's actual near-field points, which only reach 0.148,
     get closer to full correction strength) barely changed this - 6%
     became 6.1%. Not a band-width artifact; genuinely the ceiling this
     single parameter can reach on this data.

   **Conclusion: band-limiting is the mathematically correct fix for the
   trade-off found in point 8** - it does exactly what it was designed to
   do, confirmed on real data, not just by argument. But once the model
   can no longer "cheat" by trading far-field accuracy for near-field
   accuracy through `cam_d`, the honest amount of near-field correction a
   *single* scalar parameter can extract from this real, sparse match data
   is small - nowhere near enough to visually close the seam step on its
   own. This doesn't mean the direction is dead, but a single band-limited
   parameter isn't sufficient by itself. Two live options if this is
   picked up again: (a) allow separate near-field parameters per camera
   plane instead of one shared scalar, now that band-limiting removes the
   `cam_d`-aliasing pathway that made minimizing parameter count critical
   before; or (b) accept that meaningfully fixing this needs real
   ground-truth scale (the pitch-marking homography approach already
   documented above), not something derivable from AKAZE correspondences
   alone. Code kept (constants `GROUND_TILT_BAND_START`/`_FULL`,
   `band_limited_ground_warp`, its tests, the updated harness) for the
   same reason as point 8 - still fully `None`/unwired in every production
   path.

10. **Tried option (a) from point 9 immediately after - separate
    `ground_tilt_x`/`ground_tilt_z` per plane instead of one shared scalar
    (2026-07-05, same day).** Split `OptParams.ground_tilt` into two
    independent fields, each warping only its own plane
    (`apply_transformations` now calls `band_limited_ground_warp`
    separately on `left[1]` with `ground_tilt_x` and on `right[1]` with
    `ground_tilt_z`). Extended `fit_ground_tilt` to a 2D grid search (11x11
    steps, -0.20 to +0.20) over `(cx, cz)` pairs, re-fitting the same
    unmodified 5-parameter optimizer at each point, on the same real
    402-point dataset.

    **Result: splitting into two parameters doesn't unlock anything beyond
    what the single shared scalar already found.** The top of the sorted
    120-combination grid is dominated by *symmetric* pairs (`cx == cz`):
    best was `cx=+0.04, cz=+0.04` (near-field residual 1.293, essentially
    identical to the shared-parameter version's result at the same value),
    with every asymmetric combination (`cx != cz`) tested ranking worse.
    In other words, given full freedom to pick different corrections per
    camera, the optimizer's own preferred answer is still "the same value
    for both" - there's no evidence in this real data that the two planes
    actually need different near-field corrections. Far-field residual
    stayed flat across the entire grid (0.1479-0.1489, matching baseline
    0.1488) and `cam_d` stayed in a tight 0.186-0.191 band, confirming the
    band-limiting fix from point 9 remains robust with the extra degree of
    freedom - no aliasing/trade-off came back.

    **Conclusion:** the ~4-6% near-field improvement ceiling found in
    point 9 is not an artifact of forcing symmetry between the two planes;
    it appears to be a real ceiling for what *any* band-limited
    single-or-double-scalar tilt correction can extract from this specific
    real dataset via AKAZE correspondences alone. Reverted this avenue as
    a dead end for now (kept the `ground_tilt_x`/`ground_tilt_z` split and
    the 2D harness as the more general form going forward - it subsumes
    the single-scalar case exactly when `cx == cz`, so nothing was lost by
    generalizing). Option (b) - real metric ground truth, e.g. a one-time
    user-provided camera mount height rather than pitch-specific markings,
    so the correction is grounded in actual scale instead of guessed from
    unscaled correspondences - is the remaining untried path if this
    problem is picked up again.

11. **User asked to explore option (b) - a one-time user-provided camera
    height (this rig: ~5.5m) - which surfaced a real bug in points 8-10's
    formula (2026-07-05, same day).** Before using the height, traced the
    exact pixel -> GPU-undistort -> `normalize_to_plane` path (`fisheye.wgsl`'s
    `fs_main`, cross-checked against the CPU mirror in `lens/mod.rs`) to
    confirm the precise relationship between a plane-y value and the real
    vertical angle `phi` from the optical axis. Result: `plane_y = tan(phi)
    * k`, where `k = fy / (2 * image_width)` - **not** `plane_y = tan(phi)`
    directly. The factor of 2 comes from the undistort shader's `uv * 2.0 -
    0.5` remap (confirmed independently via `lens/mod.rs`'s `out_fy = fy /
    2.0`). For this project's DJI Osmo Action 4 rig (`fy=1457.07`,
    `width=3840`), `k ~= 0.19` - nowhere near `1.0`.

    This matters because `warp_ground_y` (points 8-10) applied the
    tangent-addition identity directly to `plane_y` as if it already equaled
    `tan(phi)`, which is only correct when `k = 1.0`. For `k != 1.0`, that's
    a different (not merely rescaled) family of functions from the
    physically-correct one - **all of points 8-10's fitted `c` values were
    fit against a dimensionally-wrong formula.** Fixed: `OptParams` gained
    `k_x`/`k_z` (known constants derived from each camera's own intrinsics,
    not fitted - default `1.0` to keep every existing call site's behavior
    unchanged), and `warp_ground_y`/`band_limited_ground_warp` now take a
    `k` parameter and use the corrected formula `warp(t, c, k) = (k*c + t) /
    (1 - c*t/k)` (still exactly `warp(t, 0, k) = t` for any `k`, so the
    "off by default" backward-compat guarantee is unaffected). New unit
    tests directly verify the true-angle round-trip (`t/k -> +theta ->
    *k`) and that a realistic `k` (0.19) changes the result vs `k=1.0`.

    Re-ran the same 2D grid-search harness against the same real 402-point
    dataset with the corrected `k=0.19` for both planes (same DJI model,
    same resolution, both cameras). Result: **a somewhat better, but still
    modest, near-field improvement** - best candidate `cx=+0.12, cz=+0.08`
    gives near-field residual 1.264 (6.3% better than baseline 1.348,
    up from point 10's 4.1% with the buggy formula), while far-field stays
    flat (0.1478-0.1489, matching baseline 0.1488) and `cam_d` stays
    non-pegged (0.185-0.191) - the band-limiting property survives the fix
    intact. Notably, the top-10 list is no longer dominated by symmetric
    `cx == cz` pairs the way point 10's (buggy) results were - correcting
    the formula reveals a bit more genuine asymmetry between the two
    planes, though the swing in outcome is still single-digit-percent, not
    transformative.

    **On the original question (does knowing the 5.5m height help?):** not
    directly, and this is worth being precise about why. `k` only fixes the
    *shape* of the per-plane angular relationship (a property of the lens/
    camera intrinsics, already fully known without any height input - `fy`
    and `width` come from the existing lens profile). The camera height
    doesn't enter this formula at all; `theta` (the fitted tilt correction,
    `c = tan(theta)`) stays exactly as scale-free as before. Actually
    *using* a known height to break that scale-freedom - so `theta` is
    fitted against a real metric constraint instead of floating - would
    require anchoring the *entire* two-camera model in real-world units
    (both cameras' 3D position and tilt relative to one shared, metric
    ground plane), not a small patch to this scalar warp. That is
    substantially bigger than everything built in this Phase 1 track so
    far and was intentionally deferred (see the conversation/session notes)
    pending a decision on whether it's worth a proper design pass.

    **Conclusion:** the k-scaling fix was a real, worthwhile correctness
    fix (kept), and confirms the ~5-6% ceiling from points 9-10 was
    approximately right even though the underlying formula was wrong -
    fixing it moved the number a little, not qualitatively. The 5.5m
    height itself remains unused; it only becomes useful with the bigger
    metric ground-plane model, not this scalar-warp family.

12. **User manually tuned rig-calib's live sliders and got a better-looking
    near-field overlap by eye than the AKAZE-fitted result - tested whether
    a photometric (direct pixel-comparison) objective function could find
    that same fit automatically (2026-07-05).** The hypothesis: the
    limiting factor might be the *objective function* (sparse AKAZE feature
    reprojection error), not only the flat two-plane geometric model -
    since the user's eye was judging pixel-level agreement between the two
    cameras' rendered contributions, a signal the AKAZE optimizer never
    sees directly.

    Built a Phase 1 validation harness (`examples/fit_photometric.rs`,
    `src/photometric.rs`), reusing the "validate on real data before wiring
    into production" pattern from every other experiment in this file. New,
    otherwise-unused pieces added: `reco_core::render::single_camera::SingleCameraRenderer`
    (renders one camera's contribution in isolation, clearing to
    `TRANSPARENT` instead of production's `BLACK` so alpha becomes a clean
    per-pixel coverage mask), `photometric::overlap_mask_from_alpha` (ANDs
    both cameras' alpha), `photometric::windowed_zncc` (patch-based
    zero-mean normalized cross-correlation, chosen over raw pixel
    difference because it's robust to inter-camera exposure/brightness
    differences - and patch-based rather than one global number
    specifically so a degenerate/exploited overlap region shows up as a low
    valid-patch count, not hidden inside a deceptively good scalar, per this
    file's own homography/XFeat lesson). The harness seeds a small
    multi-start Nelder-Mead (same `argmin` machinery as `optimizer.rs`)
    close to the existing AKAZE fit and maximizes ZNCC over the overlap
    region, then reports a full checklist (parameter deltas vs. seed,
    convergence spread across starts, bound-pegging, and - when
    `--matched-points` is given - the *existing* AKAZE near/far residual for
    both the seed and refined layout, as an independent cross-check) plus a
    visual dump (rendered crops, overlap mask, and a false-colored
    left/right luma-difference heatmap for both seed and refined layouts).

    Ran against the same real Berghem DJI Osmo Action 4 footage
    (`LEFT_PART.mp4`/`RIGTH_PART.mp4`) and the existing `no_xrx.json` seed:
    ZNCC mean improved (0.388 -> 0.528), but the checklist flagged the
    result as **not clearly trustworthy** - `x_ty`, `x_rz`, and `z_rx` all
    swung by 21-100% relative to the seed (the harness's own "large swing"
    warning threshold is 15%), and critically, the *independent* AKAZE
    near-field residual got slightly **worse** (1.302 -> 1.360) while
    far-field stayed flat. The visual diff-heatmap comparison confirms this
    at a glance: the same red (high-diff) horizontal band survives in
    roughly the same place in both the seed and refined renders - the
    photometric refinement did not visibly close the near-field seam step
    on this run.

    **Conclusion so far:** a straight ZNCC-over-overlap objective, seeded
    near the AKAZE fit with the default tuning knobs (100 deg FOV, 1280x720
    eval resolution, 16px patches), does not yet reproduce what the user
    found by hand, and the checklist mechanism (built specifically so a
    misleadingly-improved aggregate metric couldn't be trusted alone) did
    its job here - it would have been easy to report the ZNCC improvement
    as a win without also checking the independent residual and the visual
    diff. Plausible next steps if this is picked up again, not yet tried:
    the Berghem grass is visually repetitive (already flagged as a texture
    concern in this file), so `windowed_zncc` may be scoring texture
    aliasing in some patches rather than true alignment - inspecting which
    patches score highest and whether they land on grass vs. line markings
    would clarify this; the eval resolution/FOV/patch-size are untuned
    guesses and may be cropping out the specific near-field band the user
    was adjusting by eye; and only 5-param placement was searched (`x_rx`/
    `z_rz` held fixed at the seed) - if the user's manual tuning session
    also touched those sliders, this harness can't currently reproduce it.
    All generated visual artifacts are kept (never deleted) under
    `D:/VOETBAL_VIDEO/CALIB/photometric_test/` for anyone revisiting this.

13. **User clarified point 12's manual tuning was actually of the camera
    INTRINSICS (`left_uniforms`/`right_uniforms`: `fx/fy/cx/cy/d0-d3`),
    not placement - tested whether per-camera intrinsic correction (a
    hypothesis never touched by anything else in this file) could explain
    the near-perfect-by-eye result (2026-07-05/06).** Motivation: KB4
    undistortion happens *before* any placement math, and its nonlinearity
    is strongest at wide field angles - exactly the near-field edges where
    the seam lives. Also noted: this rig's baseline `cx`/`cy` are exactly
    `width/2`/`height/2` - a strong tell of a generic default rather than
    a measured principal point, meaning real slack plausibly exists here.

    Built `examples/fit_photometric_intrinsics.rs`: holds placement FIXED
    at the AKAZE seed, searches only `fx/fy/cx/cy` per camera (8 params,
    bounds +-5% focal / +-40px principal point) via the same ZNCC/Nelder-
    Mead machinery as point 12, on the same single frame. Result:
    ZNCC improved similarly to point 12 (0.388 -> 0.528), fitted deltas
    were all physically plausible (nothing pegged at bounds), but **the
    visual diff-heatmap showed the same unchanged near-field red band** -
    and convergence spread across the 7 starts was large relative to the
    fitted values (e.g. left.fx delta ranged 15-33px depending on start),
    signaling a fairly flat/multi-modal objective landscape rather than a
    single well-identified correction.

    Extended the same harness with `--fit-distortion` to also search the
    four KB4 coefficients `d0-d3` per camera (+-0.03 absolute bound each,
    16 params total, deliberately not sensitivity-tuned). Result: ZNCC
    improved marginally more (0.388 -> 0.549), but **the visual result was
    unchanged again** - same red band, same magnitude. One delta
    (`left.d3`) showed a 116% relative swing, but this is a reporting
    artifact of a near-zero baseline value (0.0042 -> -0.0007), not a real
    red flag by itself.

    **Conclusion:** three different parameterizations now (point 12's
    placement, this point's intrinsics, intrinsics+distortion) - same
    ZNCC objective, same footage - all produced "improved" aggregate ZNCC
    while leaving the visible near-field band completely unchanged. That
    consistency was itself the useful signal: it pointed at a bug in the
    *objective*, not in any of the three parameter spaces tried. See point
    14.

14. **Root-caused why points 12-13 never moved the visible seam despite
    "improving" ZNCC each time, and fixed it (2026-07-06).** `windowed_zncc`
    averaged every patch across the *whole* overlap region with equal
    weight. The near-field band is a thin strip - a handful of patches out
    of hundreds spread across the full hourglass-shaped overlap region -
    so whatever gains the optimizer found elsewhere (consistently, a much
    larger background/skyline mismatch near the top of the region) diluted
    away any pressure to fix the thin near-field strip specifically. This
    is exactly the failure mode the production optimizer's own seam
    weighting exists to prevent - which this harness's objective never had
    an equivalent for.

    Fix: added `photometric::windowed_zncc_banded` (row-restricted variant,
    `min_row_frac` parameter, unit-tested for the exact "excluded rows are
    skipped, not scored-and-discarded" semantics) and switched
    `fit_photometric.rs`'s optimizer objective to it, restricted to the
    bottom `NEAR_FIELD_ROW_FRAC = 0.75` of the rendered frame. Also added
    multi-frame evaluation (`--frames N1,N2,N3`, via
    `reco_io::ffmpeg::calibration_io::extract_frames`) to guard against
    fitting to one frame's incidental content - the risk flagged in every
    prior single-frame run in this file.

    Re-ran against 3 real frames spread across the Berghem footage
    (indices 300/1200/2400, ~90s apart). Result, clearly different from
    points 12-13: the near-field-banded ZNCC mean jumped from 0.052 to
    0.364 (baseline near-field score was near-random correlation - much
    worse than the whole-region average of 0.39 ever suggested, itself
    confirming the band really was badly misaligned and hidden by
    averaging), and the improvement was consistent across all three frames
    individually (0.34/0.33/0.43), not just the best-case one. The
    *independent* AKAZE near-field residual improved too (1.302 -> 1.253,
    ~4%) while far-field only regressed slightly (0.1489 -> 0.1514, ~1.7%,
    under the 5% warning threshold) - the first time in this whole
    investigation that the photometric metric and the independent AKAZE
    metric agreed in the same direction. A zoomed crop of the exact scored
    row band (not visible in the full-frame thumbnail) confirmed a real,
    if modest, visual reduction in the seam line's saturation/continuity.

    Caveats, not yet resolved: `x_rz` showed a 404% relative swing
    (baseline 0.003 rad, refined 0.015 rad - small in absolute terms,
    ~0.87 deg, but the largest mover by far); convergence spread across
    the 7 starts was wide for `intersect`/`z_rx` (best-of-7 was used, not
    a tightly clustered answer); and `NEAR_FIELD_ROW_FRAC = 0.75` was a
    visual guess from a fixed-FOV/straight-ahead evaluation render, not
    verified to correspond exactly to where "near field" appears in the
    production viewport's actual pan/tilt/FOV. This is the most promising
    result of the whole photometric investigation so far, but not yet
    strong enough to treat as validated - more frames, tighter convergence
    checking, and confirming the row-band choice against production
    viewport geometry are the natural next steps if continued. All
    artifacts kept under `D:/VOETBAL_VIDEO/CALIB/photometric_multiframe_test/`
    (and the earlier intrinsics runs under `photometric_intrinsics_test/`
    and `photometric_intrinsics_distortion_test/`).

15. **Generalization test on a second, different piece of footage - the
    near-field-banded approach that helped point 14's match REGRESSED this
    one (2026-07-06).** Ran the exact same `fit_photometric.rs` harness
    (unchanged) against `PATTERN_TEST/L/PATTERN2_L01.MP4` +
    `R/PATTERN2_R01.MP4` (the cone-pattern DJI Osmo Action 4 calibration
    footage from the very first investigation in this file, `x_rx` already
    seeded at a real ~8.1 deg from that earlier work), 3 frames spread
    across the clip (indices 150/400/650, sync_offset=0).

    The near-field-banded ZNCC metric "improved" the same way as point 14
    (-0.031 -> 0.187, consistent across all 3 frames) - but this time the
    checklist showed much worse instability: **three** parameters flagged
    large relative swings (`x_ty` 31.5%, `x_rz` 87.7%, `z_rx` 108.6%,
    vs. point 14's one flagged swing), `z_rx` flipped sign entirely
    (-0.030 -> +0.003 rad), and convergence spread across the 7 starts was
    wide (`intersect` ranged 0.633-0.770 depending on start - not a stable
    answer). Rendered a real full production stitch (`reco stitch`,
    `--blend 0`) with both the seed and refined placement and visually
    compared a zoomed crop of the center-circle arc line crossing the
    seam: **the refined result showed a clearly larger, more visible break
    in the line than the seed** - the opposite of point 14's result on the
    Berghem match.

    **Conclusion: the near-field-banded ZNCC objective does not generalize
    across footage yet.** It produced a real, independently-corroborated
    improvement on the Berghem match (point 14) and a real, visually
    confirmed regression on this PATTERN_TEST match, using the identical
    harness and row-band choice on both. Likely explanation: this
    footage's near-field band (bottom 25% of frame) is mostly grass/cone-
    shadow texture rather than a clean reference line the way the
    Berghem footage's near-field band happened to contain one - so the
    objective found *something* photometrically agreeable in that band
    (plausibly grass-texture or shadow aliasing) that isn't the same thing
    as true geometric alignment. This is the "objective might be scoring
    texture aliasing rather than true alignment" risk flagged (but not yet
    caught) back in point 12 - now caught directly on real footage.
    **Not recommended for any real use as-is** - would need to be
    validated as reliably helpful across several different matches (not
    just several frames of the same match) before it's worth trusting,
    let alone wiring in. Artifacts (both stitched full frames, zoomed seam
    crops, `refined_match.json`) kept under
    `D:/VOETBAL_VIDEO/CALIB/PATTERN_TEST/photometric_test/`.

16. **Third footage tested (a different real match) - initially looked
    like another win, but the user caught a serious regression the
    checklist completely missed (2026-07-06).** Ran the same unchanged
    harness against a third clip: `Berghem Sport J011-1/03 OJC -Bergem
    Sport 04072026` (a 2026-07-04 youth match recording, different day/
    lighting/pitch markings from both points 14 and 15), 3 frames spread
    across the ~20-minute clip (indices 300/10000/20000, sync_offset=4),
    seeded from that match's own already-existing AKAZE calibration.
    Near-field-banded ZNCC improved 0.124 -> 0.474 (whole-region metric
    dropped slightly, 0.406 -> 0.306). Checklist warnings fired same as
    point 15's failure case (`x_ty`/`x_rz`/`z_rx` large swings, `x_rz`
    moved 320%, `intersect` spread 0.604-0.658 across the 7 starts). A
    zoomed crop of the goal-box line crossing the near-field seam looked
    like a clean win: visible step in baseline, continuous in refined -
    initially reported as a second real success.

    **The user then inspected `stitch_refined.png` themselves at a
    different zoom level (mid-frame, not the near-field band) and found a
    clear, ugly misalignment - a duplicated/ghosted goal structure and a
    visible jump in the background treeline right at the seam**, worse
    than the same region in the baseline. Confirmed by cropping that exact
    region side by side: baseline's two goals and treeline line up
    reasonably; refined's goal frame is doubled and the treeline steps
    at the seam.

    **This is not a minor caveat - it inverts the conclusion.** Restricting
    ZNCC scoring to the bottom `NEAR_FIELD_ROW_FRAC` band (point 14's fix
    for the "whole-region average dilutes the near-field defect" problem)
    did not remove that dilution problem - it *relocated* it. Nothing
    scores the middle band (between the near-field strip and the sky), so
    the optimizer was completely free to degrade it in exchange for
    near-field gain, and the whole-region reference metric - the only
    thing that might have caught this - is itself diluted across hundreds
    of other patches, so a real, visible new defect in one region only
    dragged it down by ~0.1 (0.406 -> 0.306), nowhere near alarming enough
    to flag on its own. The exact same failure mode this whole
    investigation has now hit **twice**, just at a different location in
    the frame each time: any single-region-restricted or whole-frame-
    averaged photometric objective can silently trade quality from an
    unmonitored region for quality in the monitored one, and a global
    scalar metric is structurally unable to catch a regression that's
    local to a region it dilutes across.

    **Corrected running tally across all three footages, after checking
    mid-frame quality on all of them:** Berghem `LEFT_PART`/`RIGTH_PART`
    (point 14) - near-field win, but re-checking the same goal-post/fence
    structures at mid-frame height shows a visible (subtler than OJC's,
    but real) misalignment there too in the refined result vs. baseline.
    `PATTERN_TEST` cone footage (point 15) - regressed even at the near
    field. This OJC-Bergem match (point 16) - near-field win, clear
    mid-frame regression (duplicated goal structure). **None of the three
    tests is a validated win once mid-frame quality is actually checked.**

    **Conclusion: a single fixed-row-band (or whole-region) photometric
    objective is not a safe basis for automatic calibration refinement.**
    Any real attempt at this would need to score (and report) alignment
    quality across *multiple* independent horizontal bands spanning the
    full frame - near field, mid field, and far field/sky separately -
    and require every band to not regress, not just the one band being
    optimized. That is a materially bigger harness than what exists today
    and has not been built. Until then, none of this session's photometric
    refinement results (points 12-16) should be treated as usable, and
    point 14's Berghem result specifically should be re-verified at other
    frame heights before repeating the "near field improved" claim again.
    Artifacts under `D:/VOETBAL_VIDEO/CALIB/OJC_test/photometric_test/`
    (including the mid-band zoom that exposed this).

17. **Overnight autonomous session (user asked for "a solution," gave full
    autonomy, went to sleep): researched how professional stitching tools
    actually handle this, then built a properly multi-band-validated
    row-varying local correction - still not a validated win, but for a
    well-understood reason, and the tooling is sound (2026-07-06/07).**

    **Research first** (as asked - checked how Photoshop/PTGui/Hugin/
    video-specific stitchers like VideoStitch/Insta360/GoPro handle
    exactly this). Findings, cited in the session transcript: even Adobe's
    own documentation calls large-parallax stitching "still an intractable
    problem"; Hugin/PTGui's real practice is to put control points only on
    the background and mask out/seam around near/moving content rather
    than force alignment everywhere; dedicated video stitchers (VideoStitch,
    Insta360, GoPro Fusion) use per-frame optical flow for this, but even
    their own marketing describes it as slower and still imperfect. No
    "perfect" industry solution exists - confirms this is genuinely hard,
    not a sign anything so far was done carelessly. Two cloned-repo
    references the user pointed at
    (`gain2217/Robust_Elastic_Warping`,
    `mahaveer0suthar/Parallax-Tolerant-Image-Stitching`) both implement Li
    et al., "Parallax-Tolerant Image Stitching Based on Robust Elastic
    Warping" (IEEE TMM 2017) - MATLAB, offline, still-image mesh warping
    from sparse point matches. Real technique, not directly portable
    (different language/runtime, still-image assumption) - not cloned/run,
    per this project's own standing rule about not executing third-party
    code without sign-off, and because reimplementing the *underlying
    idea* in Rust was more useful than running unfamiliar MATLAB anyway.

    **Design, informed by that research and by points 12-16's own
    failure mode:** every earlier attempt searched for ONE set of global
    numbers to explain misalignment that is fundamentally local/depth-
    dependent - mathematically doomed, since a single global answer that
    helps one region must hurt another whenever their true corrections
    differ. Built `reco_calibrate::row_profile` instead: measure real
    vertical disparity *per horizontal band* (16px tall, small-window ZNCC
    search, reusing `photometric::zncc`) between the two cameras' isolated
    renders, reject bands unstable across multiple frames (the signature
    of a moving player, not static geometry), fit a smooth interpolated
    profile across rows, and band-limit it to exactly zero beyond the
    outermost measured band (smoothstep taper, same discipline as
    `band_limited_ground_warp`). Deliberately 1D (vertical shift only) -
    every visible defect all session was a vertical step in an otherwise-
    horizontal line, never a horizontal offset, so this targets the actual
    observed failure mode rather than a hypothetical more general one.
    9 new unit tests, all passing, no GPU needed.

    Built `examples/fit_row_profile.rs` with the multi-band + held-out-
    frame discipline **designed in from the start this time**, not bolted
    on after the fact: every run checks near/mid/far thirds of the overlap
    region independently (not just the band being corrected), and the last
    frame passed via `--frames` is always held out from fitting, used only
    to check whether the fitted profile generalizes to a frame it never
    saw. The correction lives in rendered-pixel space (not the abstract
    `PlaneLayout`), so it can't be expressed as a `match.json` or tested
    via `reco stitch` - the harness builds its own full left-over-right
    composite (matching production's alpha-blend math) for a real visual
    before/after, not just an isolated diff heatmap.

    **Result on the Berghem footage (4 frames, 3 fit + 1 held out):** 12
    of 45 candidate bands were confidently measured and stable across
    frames (row centers 200-520 of the ~720px eval height). The MID band
    genuinely improved, consistently, on every single frame including the
    held-out one (ZNCC roughly 0.37-0.39 -> 0.38-0.41). But the NEAR band -
    the actual target all along - got *worse* on every frame, again
    including the held-out one (roughly 0.10-0.13 -> 0.04-0.06, about
    half). FAR stayed essentially flat. A visual spot-check at one
    near-sideline crossing showed no visible change (consistent with the
    fitted profile reading `dy≈0` at that specific spot).

    Investigated why: none of the 45 candidate bands beyond row~520 (the
    deepest, truest near-field content, closest to camera) passed the
    confidence/stability bar - the block-matching search couldn't get a
    trustworthy read there, plausibly the same "Berghem grass is visually
    repetitive" texture concern already flagged earlier in this file,
    now confirmed as an obstacle for a *different* technique too, not
    specific to ZNCC-based global optimization. Tried widening the search
    radius (`MAX_DY` 15 -> 40) to force a deeper reading: this backfired
    outright - produced one new "match" pegged at the search boundary
    (`dy=-40px`, confidence 0.301, barely above the acceptance bar) that
    then corrupted the *far* band too (0.69 -> 0.57). Classic block-
    matching pitfall: a wider search window raises false-positive risk on
    low-texture/repetitive content, and this footage's grass is exactly
    that. Reverted to `MAX_DY=15`.

    **Conclusion:** this is the most rigorously self-checked experiment of
    the whole investigation (multi-band and held-out-frame validation
    built in before running it, not added after a mistake was found), and
    it is architecturally sound - genuinely spatially-varying, provably
    band-limited, robust to movers by construction. It is still **not a
    validated fix**: it traded near-field quality for mid-field quality on
    this footage, the opposite of the goal, because the deepest near-field
    rows don't yield a confident photometric (block-correlation) read on
    this specific footage's grass texture. This suggests the next lever,
    if pursued, is switching the *measurement* technique for the deepest
    rows specifically - e.g. explicit line/edge detection on real pitch
    markings (a strong, sparse, high-contrast target exactly where generic
    block ZNCC is weakest) rather than generic photometric correlation -
    not a fix to this harness's math, which behaved correctly and
    honestly reported its own limits. The module and harness are real,
    tested, reusable infrastructure either way. Not tested against
    PATTERN_TEST or OJC-Bergem footage this session (time-boxed - the
    Berghem result was already inconclusive on its primary target, so
    further-footage testing had low expected value before the underlying
    measurement-confidence problem is addressed). Artifacts under
    `D:/VOETBAL_VIDEO/CALIB/row_profile_test/berghem/`.

**Bottom line:** the real constraint is the flat two-plane model's shape
being wrong close to the camera (parallax + perspective foreshortening it
can't represent) - not a shortage of matches, not detector choice, not
cost-function weighting. Every lever that touches *matching or weighting*
(wider AKAZE bands in either axis, AI features, more resolution, more
sample frames, wider vertical weighting) either does nothing or makes
things worse, and even feeding the optimizer near-field data directly
doesn't move its answer. A single globally-fitted ground-plane tilt
parameter (point 8) does move the near-field answer, but trades away
far-field accuracy to do it. Band-limiting it (point 9) removes that
trade-off entirely (confirmed on real data: far-field stays flat, `cam_d`
never pegs), but the honest near-field improvement a single scalar can
extract this way is small (~6%) - nowhere near enough to visually fix the
seam by itself. A wider `blend_width` (see below) is the one
practical, low-effort lever still standing after `ground_correction` was
removed for its player-ghosting problem, without a much bigger rework (a
proper ground-plane model needs camera height/tilt this rig can't supply;
a pitch-marking-based homography calibration is a real but substantial
separate feature).

**Practical mitigation confirmed (2026-07-04), independent of calibration:**
this whole investigation used `--blend 0` (hard seam) for diagnostic
clarity. At the shipping default `blend_width = 0.05`, the seam step is
already visibly softer than the diagnostic hard-cut crops in this file
suggest. At `blend_width = 0.12`, it's close to invisible on the tested
frame. This doesn't fix the underlying misalignment - it just crossfades
over it - and the renderer's own comment on `blend_width` already warns
that wider blends can wash out ball tracking in the overlap region (from
prior Jetson production testing), so this is a tradeoff to dial in per
use case, not a free win. Still, it's the lowest-risk, already-shipping
lever available today, and worth trying before anything more invasive.

**How to actually make progress on the parallax problem (2026-07-05
discussion, not yet started):** parallax can't be removed in software -
two lenses centimeters apart genuinely capture different light, and no
transform recovers information neither camera saw. Three real paths,
most to least impactful:

1. **Shrink the physical baseline at the rig.** Parallax error scales
   with (distance between the two lenses' optical centers) / (distance
   to subject). Mount the cameras as close together as physically
   possible, and prefer pivoting them around a common point near the
   front lens elements (angled outward from near-coincident entrance
   pupils) over mounting them side-by-side body-to-body - action cams
   have their entrance pupil close behind the front glass, so a
   front-pivoted mount can get the effective baseline near zero. Since
   the far field already aligns perfectly, halving the baseline should
   roughly halve the remaining near-field step. Worth testing before any
   further software work - cheapest possible experiment, one afternoon
   of re-mounting + re-calibrating.

2. **Align on the ground plane instead of the seam-weighted compromise
   (real, substantial software feature, not started).** Today's
   optimizer finds a compromise fit weighted toward the seam center,
   which aligns the mid/far field and lets the near ground drift - this
   is the flat two-plane model's inherent shape problem documented
   above. The alternative: compute a true ground-plane homography from
   known real-world pitch-marking dimensions (click 4-8 known points -
   penalty-area corners, center-circle intersections, halfway line
   crossings - once per camera) and align that surface exactly. Every
   ground line, near and far, then lines up; the residual parallax error
   moves to objects *above* the ground plane (e.g. slight ghosting on
   players' upper bodies right at the seam, not the pitch itself). This
   also derives the camera height/tilt this rig can't otherwise supply
   (no raw IMU gyro on the DJI Osmo Action 4) - the same missing
   ingredient a "real" ground-plane/dome model would need. This is the
   one approach that fixes the near line *without* the row-based
   shearing that made `ground_correction` unacceptable (see above) -
   because it's keyed to real-world geometry, not image rows.

3. **Manage where the residual error lives (what's already shipping).**
   Since some surface must lose when there's real parallax and no
   ground-truth geometry, put the seam where nobody's looking and
   crossfade the rest: seam placement away from busy play, `blend_width`
   up to ~0.12 (see above). This mirrors ActionStitch's own approach -
   their docs admit the ball position differs slightly between frames
   at the seam and recommend seam placement over any depth model, i.e.
   they manage the error rather than remove it, same as this codebase.

Recommended order: try (1) first since it's nearly free to test and
could resolve this on its own; if the rig can't be remounted enough or
it's not sufficient, (2) is the real feature to plan and build next.

18. **Manual field-line seam-continuity input (2026-07-06/07) - built and
    unit-tested, NOT YET validated on real footage.** User's idea: instead
    of any automatic near-field measurement (all of which failed above -
    AKAZE lacks texture there, ZNCC gets fooled by grass, `row_profile`'s
    block-matching can't get a confident read on the deepest rows), let
    the user manually click 2 points along a real field line in each
    camera's own GPU-undistorted frame (not 1 point - a single point on a
    straight line is ambiguous/slidable; 2 points fix both position and
    slope), independently per camera (no need to click the exact same
    physical point in both - each line is just traced in its own camera's
    view).

    New module `src/line_seam.rs`: extrapolates each camera's clicked line
    to that camera's own seam pixel column (`geometry::seam_columns`, a
    new public helper factored out of the inline formula
    `per_point_seam_weighted_errors_full` already used) and turns the two
    extrapolated points into one `MatchedPoint` - identical in kind to an
    AKAZE match, so it reuses `apply_transformations`/
    `reprojection_error` and, critically, the already-built-but-never-
    wired-into-production `ground_tilt_x`/`ground_tilt_z` band-limited
    parameters from points 8-11 (which stalled at a ~6% ceiling for lack
    of confident near-field AKAZE data - manual clicks sidestep that
    limitation directly rather than trying to extract more signal from
    the same weak automatic measurement).

    New example `examples/fit_ground_tilt_manual.rs`: loads an existing
    `match.json` (base 5-7 params frozen), loads a small manual-lines JSON
    (pixel clicks), converts to `MatchedPoint`s, and grid-searches
    `ground_tilt_x`/`ground_tilt_z` against them exactly like
    `fit_ground_tilt.rs` already does for AKAZE-derived near-field points -
    falling back to a single shared parameter if only 1 line is given
    (2 independent parameters from 1 constraint is under-determined).
    Optional `--matched-points` cross-checks that AKAZE far-field residual
    is unaffected - expected to be exactly flat by construction
    (`band_limited_ground_warp` is provably identity beyond
    `GROUND_TILT_BAND_FULL`), but verified rather than assumed, per this
    file's own standing rule.

    7 new unit tests (line extrapolation math, the swap-convention wiring,
    JSON round-trip, the extracted `seam_columns` helper), all passing;
    `cargo test -p reco-calibrate --lib` 88/88 green; clippy/fmt clean on
    the changed/new files (pre-existing, unrelated clippy errors in
    `reco-core`'s session/d3d11 code were confirmed present on `main`
    before this change too, via `git stash`).

    Also added `tools/manual_line_picker.html` - a small, dependency-free
    static page (open directly in a browser, nothing uploaded anywhere)
    for actually producing a `manual_lines.json`: load the two
    `--debug-dir`-dumped PNGs, click two points per line on each side,
    "Add line pair", export. Exists because without it the only way to
    get real pixel coordinates off these images was an external image
    editor plus hand-typing JSON - exactly the kind of friction this
    project's own rule ("document friction, don't work around it")
    argues for fixing directly rather than working around.

    **Not yet done, and the honest reason why:** this has no real click
    data run through it yet - producing that requires a human looking at
    real GPU-undistorted footage and clicking actual field-line points,
    which isn't something this session can do without display/video
    access. Next real step is running `fit_ground_tilt_manual` against a
    couple of manually-clicked lines on real Berghem or PATTERN_TEST
    footage and checking the same way every other experiment in this file
    was checked: does the near-field residual actually drop, and does a
    real rendered composite crop confirm it visually (not just trust the
    number) - before any GUI work or production wiring (`PlaneLayout` +
    `fisheye.wgsl` shader ramp) is worth doing.

19. **First real-data run of `fit_ground_tilt_manual` (2026-07-07, OJC
    Werkplaats match) - inconclusive, overfitting risk identified, do not
    treat as validation.** `resources/test-data/` now has a real
    `match_werkplaats.json` (from `clicks_to_match.py` + the OJC lens
    profile) and a real `manual_lines.json` (one field line, clicked via
    `click_line_calib_v1.html`'s plane-coordinate export). Running
    `cargo run --release -p reco-calibrate --example fit_ground_tilt_manual
    -- resources/test-data/match_werkplaats.json
    resources/test-data/manual_lines.json` end-to-end for the first time:

    - `cargo test -p reco-calibrate --lib`: 90/90 passing (up from 88,
      the plane-coord-space tests from point 18's follow-up commit).
      `cargo fmt --all --check` clean. Clippy on `reco-calibrate` itself
      clean; the same pre-existing, unrelated `reco-core` session/d3d11
      dead-code and raw-pointer errors noted in point 18 are still the
      only clippy failures workspace-wide - untouched by anything in this
      entry.
    - Only 1 line was clicked, so (as the tool itself warns) this is the
      under-determined 1-constraint fallback (`ground_tilt_x = ground_tilt_z`,
      one shared scalar), not the real 2-independent-parameter fit the
      feature is designed for.
    - At the harness's default grid range (`GRID_RANGE = 0.3`, matching
      `fit_ground_tilt.rs`'s AKAZE-tuned range), the fit **pinned exactly
      at the search boundary** (`-0.3000`), a 73.8% error reduction. A
      result sitting exactly on a search-range edge is a red flag, not a
      converged answer - it means the true minimum (if one exists) lies
      outside the range that was checked, or the objective is
      monotonically improving without bound (degenerate fit). Widening
      the range tenfold (`GRID_RANGE = 5.0`, `GRID_STEP = 0.01`, tested
      locally, not committed - this file's own point 8 already flagged an
      unconstrained `c` as able to "wander into NaN/infinite territory"
      via `warp_ground_y`'s pole) found a genuine interior minimum at
      `c ≈ -1.18`, a 99.6% error reduction (0.00198 → 0.0000075) - so this
      specific fit isn't unbounded/degenerate, just outside the default
      range.
    - **But `c ≈ -1.18` is `tan(theta)` for `theta ≈ -49.7°`** - an
      implausibly large "extra" ground tilt to stack on top of the rig's
      own ~19° `rig_tilt`, for what this file has consistently described
      as a subtle near-field parallax residual. A single clicked line
      gives the 1-parameter fit exactly one scalar constraint to satisfy,
      with nothing to stop it from reaching for whatever value zeroes
      that one number, however physically implausible - the same
      "risks overfitting on sparse match sets" concern already logged
      for `z_rz` in `calibration_alignment_fix_summary.txt` section 4
      (repo root), now observed directly rather than theorized.
    - No AKAZE far-field matched-points file exists yet for this
      Werkplaats clip, so the `--matched-points` cross-check (does this
      corrupt far-field alignment?) could not be run either - one more
      reason not to trust this number as a real result.

    **Conclusion: the code path works correctly end-to-end on real data
    (this was the actual gap - point 18 had unit tests but zero real
    clicks run through it) - but one clicked line is not enough evidence
    to judge the manual-line-seam idea itself.** Next real step: click a
    second, independent field line on the same Werkplaats frame pair (a
    different real line, not a re-click of the same one) so the fit
    becomes properly 2-constraint/2-parameter as designed, re-run, and
    check whether the fitted `ground_tilt_x`/`ground_tilt_z` land on
    physically sane values (single-digit-degree range, not ~50°) *before*
    trusting any error-reduction percentage - and generate an AKAZE
    matched-points file for the same clip so the far-field cross-check
    this file's own standing rule calls for can actually run.

    **Harness hardened same day:** `fit_ground_tilt_manual.rs` previously
    reported a grid-search result with no indication that it had pinned at
    the search boundary - exactly what happened above, and easy to miss
    since the error-reduction percentage alone looks like a win. Added a
    boundary-pin check (warns loudly if `|best_c| >= GRID_RANGE -
    GRID_STEP`) and a `theta = atan(c)` degrees readout next to each fitted
    value, so an implausible angle (like the ~-49.7° above) is visible at
    a glance instead of requiring a manual conversion. Verified: rerunning
    against `match_werkplaats.json`/`manual_lines.json` now prints the
    warning and `theta = -16.7 deg` (at the unwidened default range - still
    clearly too large for a "subtle" correction, reinforcing this isn't
    ready to trust). fmt/clippy clean on the changed file; no other files
    touched.

    Blocked on real assets, not on more code: the two concrete next steps
    (a second independent clicked line; an AKAZE matched-points file for
    far-field cross-check) both need either a human clicking in
    `click_line_calib_v1.html` (precision + its own in-browser undistort
    matter, not something to approximate from a still image) or the raw
    OJC source `.MP4`s that `export_synced_frames.py`'s docstring points
    at - not present on this machine (checked `D:\VOETBAL VIDEO\` and
    `C:\...\OneDrive\Voetbal Berghem Sport JO11-3`, no OJC/J011 match
    folder or matching DJI raw files under either).

20. **Proper 3-line run (2026-07-07, same day): the fit is now trustworthy,
    and the answer it gives is that the band-limited ground_tilt correction
    has a ~7-8% ceiling even with good manual near-field data - closing the
    open question from points 8-11.** After the click tool's detector was
    reworked (Hough-transform centerline detection, cross-side pairing by
    seam-edge height - `resources/click_line_calib_v1.html`), the user
    exported a fresh `manual_lines.json` with 3 independent lines: one in
    the untouched band (|y| < `GROUND_TILT_BAND_START`, provides a control),
    one in the ramp zone, one deep near-field (|y| ≈ 0.27, the thick near
    sideline). Left/right seam heights agree to ~0.001-0.005 plane units
    per line - the clicks are clean.

    Results (`fit_ground_tilt_manual`, `match_werkplaats.json`):
    - All 3 lines, true 2-parameter fit: interior minimum (no boundary
      pin), `ground_tilt_x = -0.090` (θ = -5.1°), `ground_tilt_z = -0.085`
      (θ = -4.9°), error -7.3%. The two *independently fitted* parameters
      agreeing at ~-5° is strong evidence they measure a real, shared
      physical effect rather than noise - and directly confirms point 19's
      overfitting diagnosis of the single-line -49.7° result.
    - Lines 1+2 only (the two the correction can touch): same fitted
      values, -7.7%. The control line contributes constant error as
      expected.
    - Line 2 alone (deepest near-field, 1-param fallback): pins at the
      search boundary again, wants ~-50°, -74% - reproducing the point 19
      overfit exactly. Its baseline error (0.00207) is ~63% of the total
      (0.00331), so the near line dominates the correctable error, but
      zeroing it demands a tilt that the mid-field line immediately vetoes
      in the constrained fit.

    **Conclusion: the ~6% ceiling from points 8-11 was never a
    data-quality problem.** The manual clicks provided exactly the
    confident near-field measurement AKAZE/photometric methods couldn't,
    and the ceiling barely moved (~6% → ~7-8%). A single tan-warp scalar
    per plane - even band-limited, even fed perfect near-field data -
    cannot fix the near-field step without breaking the mid-field, because
    the residual isn't shaped like a shared ground-plane tilt. This is the
    flat two-plane model-shape problem again, measured cleanly for the
    first time.

    **Owner decisions (2026-07-07) - these supersede this file's earlier
    option ranking in the "How to actually make progress" section:**
    - A ~7-8% improvement IS worth shipping. The acceptance bar for this
      seam is ~0.1% residual continuity error (visually seamless), and
      every honest increment toward it counts. Production wiring of
      `ground_tilt_x`/`ground_tilt_z` (`PlaneLayout` + `fisheye.wgsl`
      band-limited ramp) is wanted.
    - Shrinking the physical camera baseline (the old option 1) is **not
      an option for this rig - ruled out by the owner. Do not propose it
      again.**
    - The ground-plane homography calibration from known pitch-marking
      dimensions (the old option 2) is to be built **as an optional,
      opt-in feature** - not forced into the default calibration flow.
      The manual-line clicking workflow built for this experiment is
      directly reusable for it: clicking known points/lines per camera is
      the same interaction, and the tool now has reliable auto-detection
      to speed it up.

    (Far-field cross-check still not run - no AKAZE matched-points file
    exists for this clip, raw source videos not on this machine. With
    production wiring now planned this check matters again and should be
    run before the wiring ships: `band_limited_ground_warp` is provably
    identity beyond the band, but verifying on real data is this file's
    own standing rule.)

## Calibration "confidence" metric measures match count, not fit quality

**Symptom, documented since 2026-07-03:** `CalibrationResult::confidence`
stays at (or near) 100% even when the underlying calibration is badly
misaligned. First observed in `calibration_alignment_fix_summary.txt`
section 2.2: an AKAZE detection-band bug (`[0.05, 0.95]` used instead of
the correct `[0.25, 0.85]`) produced a **64x-worse reprojection error**
on identical footage (`total_reproj`: 0.047 -> 2.994, `angular_error`:
0.668 -> 1.859) while `confidence` sat at 100% throughout, in both the
good and the badly-miscalibrated run. Also called out at the very top of
this file (line 9): the seam misalignment this whole file investigates
"visibly steps... even though the optimizer reports near-zero residual
error and 100% confidence."

**Root cause, confirmed by reading the code
(`crates/reco-calibrate/src/lib.rs:107-114` and `:547`):**

```rust
const FULL_CONFIDENCE_MATCHES: f64 = 50.0;
...
let confidence = (total_matches as f64 / FULL_CONFIDENCE_MATCHES).min(1.0);
```

`confidence` is purely `min(total_matched_points / 50, 1.0)` - a raw
count of matched feature points, saturating at 50. It has **zero
dependency** on any of the fit-quality metrics the same function already
computes a few lines later in the same block: `best_residual` (exposed
as `CalibrationResult::residual_error`), or `total_reproj` /
`trimmed_err` / `angular_err` (bundled into
`CalibrationResult::quality: Option<CalibrationQuality>`). A calibration
can have 50+ well-distributed matches (100% confidence) and a
catastrophically wrong fit at the same time - that is exactly what
happened in the 64x-worse run above; matches were plentiful (the wide
band finds more keypoints, not fewer), the fit was just bad.

**User-facing impact on both consumer surfaces:**
- CLI (`reco-cli/src/calibrate.rs:294`): prints `Confidence: X%` as a
  headline stat. `residual_error` is printed too (line 295) but as a raw
  dimensionless number with no interpretive threshold attached, so a user
  has no way to tell "0.047 good, 2.994 bad" without already knowing what
  a healthy number looks like for their rig.
- GUI (`reco-gui/src/main.rs:4610-4622`): the *only* automatic "Low
  calibration confidence" warning dialog check is `if confidence < 0.5`
  (i.e. fewer than 25 matches) - it does not read `residual_error` or
  `quality` at all. A calibration with plenty of matches but a badly
  wrong fit - the exact failure mode that caused the original rig-calib
  bug report this whole investigation started from - triggers **zero**
  warning in the GUI.

**Fixed (2026-07-07).** `confidence` is now `match_confidence *
fit_confidence` (`crates/reco-calibrate/src/lib.rs`):
`match_confidence` is the original `total_matches / 50` term, unchanged;
`fit_confidence` (new `quality_confidence()` fn) is 1.0 at or below
`GOOD_MEAN_REPROJECTION_ERROR = 0.001`, 0.0 at or above
`BAD_MEAN_REPROJECTION_ERROR = 0.01`, and linear in between, evaluated
against the mean (not summed) per-point reprojection error. Both
thresholds are anchored to the one confirmed real before/after pair this
repo has - the section 2.2 good run (0.047 total / 100 matches ≈ 0.00047
per point) and bad run (2.994 total / 94 matches ≈ 0.0319 per point) -
with safety margins (~2x above the good measurement, ~3x below the bad
one) so this doesn't just barely trip on that one specific regression.
**This is a first-pass calibration, not an extensively-tuned curve** -
revisit if a real run gets flagged low-confidence despite looking
visually fine, or the reverse.

Also fixed in passing: `CalibrationQuality::mean_reprojection_error` was
mislabeled - it held `geometry::reprojection_error`'s raw *summed* total,
which scales with match count, not an actual mean. Now divided by
`total_matches` so the field matches its name and is comparable across
calibrations with different match counts. Confirmed via grep that no
other code in the workspace read this field before the fix, so this
doesn't change behavior anywhere else. `trimmed_reprojection_error` and
`angular_error` are still raw sums (their names don't claim otherwise);
left alone to keep this change minimal.

4 new unit tests added (`confidence_tests` module in `lib.rs`), including
one that reproduces the documented 64x-worse case from stored numbers
(no real footage needed) and confirms it now separates to 100% vs 0%
confidence instead of 100% vs 100%. `cargo test -p reco-calibrate --lib`:
94/94 passing (up from 90). fmt clean; clippy clean on `reco-calibrate`
(the same 4 pre-existing, unrelated `reco-core` session/d3d11 errors
noted throughout this file are still the only clippy failures
workspace-wide).

**Not done:** wiring the CLI/GUI to show `match_confidence` and
`fit_confidence` as separate numbers (right now only the combined
`confidence` changes) - the GUI's `if confidence < 0.5` warning
threshold and the telemetry field both still work unchanged, since
they consume the same `confidence` value, now just a more honest one.
