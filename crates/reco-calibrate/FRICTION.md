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

**Bottom line:** the real constraint is the flat two-plane model's shape
being wrong close to the camera (parallax + perspective foreshortening it
can't represent) - not a shortage of matches, not detector choice, not
cost-function weighting. Every lever that touches *matching or weighting*
(wider AKAZE bands in either axis, AI features, more resolution, more
sample frames, wider vertical weighting) either does nothing or makes
things worse, and even feeding the optimizer near-field data directly
doesn't move its answer. A wider `blend_width` (see below) is the one
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
