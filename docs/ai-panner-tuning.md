# AI camera tracking & panner tuning

Reference for the **AI Tracking** section of the Export dialog (`reco-gui`).
Explains what each control actually does and how to tune it for football.
Grounded in `crates/reco-autocam/src/panners/field.rs` and
`crates/reco-autocam/src/tracking_mode.rs` - not guessed.

## Top-level controls

**Tracking mode** - `field` (default): follows the player cluster + ball
together; falls back to ball-only if the model has no player class.
`ball`: follows only the ball (higher confidence floor, tighter max-jump
gate). `sweep`: no AI at all - a fixed sinusoidal left-right pan, useful
only as a debug/baseline mode.

**Detect every N frames** - how often the detector actually runs; frames
in between reuse the last detection. Lower = fresher positions during
fast action, higher = cheaper. `3` (every ~0.1s at 30fps) is a good
default; push to 10-15 only if you need the compute back.

**Style preset** - a one-shot action that overwrites every slider below
(framing, cluster mode, lock-pitch, cluster bandwidth, dead-zone, ball
weight, FOV) with a tuned bundle. You can still tweak any slider
afterward; picking a different preset later re-overwrites everything.
See [Presets](#presets) below for the exact values each one sets.

**Framing** - independent of the preset buttons, this is the actual
algorithm switch: `action` aims at the (optionally confidence-weighted)
player cluster with edge-push/pitch-bias/ball-blend and dynamic zoom.
`frame_all` aims at the plain geometric midpoint of every player's
bounding box - no trim, no weighting, no ball pull - the "whole team in
frame" mode (training footage, frisbee, etc.).

**Pitch - Lock (horizontal-only)** - off (default) lets the camera's tilt
track the action's vertical position too. On holds tilt fixed and pans in
yaw only.

**Lookahead (smoothness)** - buffers N seconds of future frames so the
panner can smooth over past+future and lead the play slightly instead of
reacting frame-by-frame. `0.5s` is a safe middle value; higher is
smoother but costs more VRAM (the export dialog's risk-coloured slider
warns if it won't fit the loaded source resolution).

## Advanced panner

**Cluster mode** - `density` (default): centers on the densest
concentration of players (most neighbors within `cluster_bandwidth_rad`)
and keeps that group - a distant knot of players can't drag the aim away
from the real action. `trimmed_mean`: averages *all* players, trimming
the farthest outliers - simpler, but a distant group can still pull the
mean before trimming kicks in.

**Ball weight** `[0-1]` - blend weight of the ball vs. the player cluster
(Action framing only). Effective pull each frame is
`ball_weight × ball_presence` (a value that ramps up while the ball is
near the cluster and decays once it isn't), so it only tugs the camera
while the ball is actually present and close to play. Forced to `1.0` in
Ball tracking mode.

**Cluster bandwidth** (radians) - neighborhood radius used by `density`
mode to decide which players belong to "the" cluster. Wider pulls a
looser/more spread-out formation into one group; narrower isolates a
tight core (but can lose the cluster faster in open play). No effect
under `trimmed_mean`.

**Dead-zone** (radians) - the camera holds still while the target stays
within this radius of the current aim; larger errors are eased in
instead of snapped to. Removes micro-wobble on near-static play. Larger
= calmer but slightly slower to react (lookahead is what makes this
affordable); smaller = more reactive but more prone to micro-adjustments.

**Field of view - Tight / Default / Wide** (degrees) - **not** three fixed
zoom levels. They're the *bounds* of a continuously-varying zoom
recomputed every frame from cluster spread plus distance/edge/velocity
biases (Action framing) or bounding-box extent (FrameAll). Tight = how
far in the panner may zoom on a compact cluster; Wide = how far out on
spread-out play; Default is only the starting FOV before any players are
detected. If a specific situation zooms in/out further than you'd like,
adjust these bounds - not a "preferred" midpoint.

## Presets

| Field | default() | `broadcast` | `action` | `frame_all` |
|---|---|---|---|---|
| framing | Action | Action | Action | **FrameAll** |
| confidence_weighted | true | true | true | **false** |
| dead_zone_rad | 0.20 | 0.20 | **0.12** | 0.20 |
| fov_tight / default / wide | 22 / 40 / 58 | 22 / 40 / 58 | **20 / 34 / 48** | 22 / 40 / **70** |
| ball_weight | 0.50 | **0.20** | **0.35** | **0.0** |
| edge_push | 0.15 | 0.15 | **0.20** | 0.15 |
| lookahead_reactivity | 2.5 | 2.5 | **3.0** | 2.5 |
| frame_all_margin_deg | 8.0 | 8.0 | 8.0 | **10.0** |

`broadcast` is the validated, calm default (only `ball_weight` lowered
from the base default). `action` is tighter and more reactive across the
board - smaller dead-zone, narrower/tighter FOV range, higher ball pull,
more edge push. `frame_all` switches framing algorithm entirely.

Source: `FieldPannerConfig::{broadcast, action, frame_all}` in
`crates/reco-autocam/src/panners/field.rs`.

## Practical tuning for football

- **Calm, few cuts (broadcast-style)**: leave the `broadcast` preset as-is,
  or lower `ball_weight` further (0.10-0.15) for even less ball-chasing.
- **Faster, more energetic follow on counters**: switch to the `action`
  preset wholesale rather than hand-tuning individual sliders - it's a
  tuned bundle, not a single knob.
- **Cluster keeps "getting lost" during spread-out midfield play**:
  raise `cluster bandwidth` (0.35-0.4 rad).
  Increase [`min_cluster`](#extra-parameters-not-yet-exposed-in-the-gui)
  if 2 players is too readily forming a cluster (not GUI-exposed today).
- **Picture feels jittery/twitchy on static play**: raise `dead-zone`, or
  raise `lookahead` for more lead-in smoothing.
- **Camera zooms further in/out than desired in a specific situation**:
  adjust the FOV Tight/Wide bounds directly - they're bounds, not
  targets, so widening/narrowing them changes the *range* the dynamic
  zoom is allowed to explore.

## Extra parameters (not yet exposed in the GUI)

A few `FieldPannerConfig` fields have no Export-dialog control today and
can currently only be changed via a config file / CLI flag consuming
`reco-autocam` directly: `min_cluster`, `edge_push`, `fov_alpha`,
`pitch_near`/`pitch_far`/`distance_bias_max`, `edge_bias_max`,
`cluster_alpha`, `max_velocity_rad_per_sec`, `velocity_alpha`,
`pitch_bias`, `ball_presence_decay`/`ball_presence_attack`,
`velocity_fov_bias_max`, `ball_frame_margin_deg`,
`ball_max_dist_from_cluster`, `lead_gain`/`lead_alpha`,
`keep_fraction` (trimmed-mean only). See the field-level doc comments in
`crates/reco-autocam/src/panners/field.rs` for what each does.
