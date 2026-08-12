# AI camera tracking & panner tuning

Reference for the **AI Tracking** section of the Export dialog (`reco-gui`).
Explains what each control actually does and how to tune it for football.
Grounded in `crates/reco-autocam/src/panners/field.rs` and
`crates/reco-autocam/src/tracking_mode.rs` - not guessed.

## Recommended starting settings

Validated end-to-end (2026-08-12) against a real corner-breakaway clip -
the values below fix "camera won't follow the ball into a corner /
breakaway" as far as settings alone can. Start here, don't hand-tune
from scratch. These are also saved per-calibration once you hit **Save
calibration** in the Export dialog - see
[`Calibration::autocam_defaults`](../crates/reco-core/src/calibration.rs).

**Model & top-level**

| Setting | Value |
|---|---|
| Model | current best checkpoint - `yolo26n_v2` in production use as of this writing; `yolo26n_v3`/`yolo26s_v3` trained and ONNX-exported but not yet app-tested, see `YOLO26_Training.md` |
| Tracking mode | `field` |
| Detect every N frames | `3` |
| Ball anchor range | `0.3-0.5 rad` (default `0.20`) |
| Style preset | `action` (as the base, then override below) |
| Framing | `action` |
| Pitch - Lock (horizontal-only) | off |
| Lookahead (smoothness) | `0.5s` |
| Reduce lookahead memory (8-bit) | off (only on a VRAM error) |

**Advanced panner**

| Setting | Value | Why |
|---|---|---|
| Cluster mode | `trimmed_mean` | fixes the multi-second freeze during ball-less stretches |
| Dead-zone | `0.05-0.08 rad` | needed alongside `trimmed_mean`, tested together |
| Ball weight | `0.35` | `1.0` caused visible wobble regardless of model quality |
| Ball reach | `1.0 rad` (default `0.5`) | lets the panner pull toward a genuinely isolated ball instead of ignoring it |
| FOV Wide | `65-70°` (`action` preset default `48°`) | without this the ball-reach widen logic clamps before it can actually open the shot up |
| FOV Tight / Default | leave at preset (`20°` / `34°`) | not separately tuned |
| Cluster bandwidth | leave at preset | not separately tuned |

The three ball-related settings gate each other, in this order: **Ball
anchor range -> Ball reach -> FOV Wide**. Ball anchor range decides
whether the *tracker* accepts a far detection at all; Ball reach decides
whether the *panner* lets it pull the aim; FOV Wide decides whether the
*shot* can actually widen enough to show it. Raising only one without
the others won't fully fix a missed breakaway - see the "Camera won't
follow the ball into a corner" bullet further down for the full
reasoning and how each was verified (not just recommended by guess).

**Every settings table above is also written into the events JSONL.**
When "Record pipeline events" (`--events` on the CLI) is on *and* AI
tracking is enabled, the very first line of the output file is a
`{"kind":"run_config", ...}` record with every field from the tables
above - so a trace file is self-describing without cross-referencing
the export command or GUI state separately. See
[`PipelineEvent::RunConfig`](../crates/reco-core/src/detect/pipeline_event.rs).

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

**Ball anchor range** (radians, `player_anchor_max_rad`) - a gate inside
the ball *tracker* (`crates/reco-autocam/src/trackers/ball.rs`), upstream
of everything else in this doc. A raw ball detection is only accepted at
all if it lies within this distance of at least one currently-tracked
player; farther detections are dropped as likely false positives (a
ball-shaped object in the crowd or background) before the panner ever
sees them. `0.20 rad` (~11deg) is the default. This runs *before* Ball
reach (below), so a genuine, isolated ball - the exact breakaway/corner
scenario Ball reach and FOV Wide are meant to handle - can be silently
discarded here first, making those two settings look like they aren't
working. Confirmed via raw-detection log inspection (not guessed): the
model correctly detected a breakaway ball at 0.97 confidence while the
tracker was coasting/losing it, because the detection sat outside this
gate. Widen it (0.3-0.5+) if the panner never seems to acquire a ball
that's genuinely far from the pack; keep it tight if the model
false-positives on background clutter.

**Style preset** - a one-shot action that overwrites every slider below
(framing, cluster mode, lock-pitch, cluster bandwidth, dead-zone, ball
weight, ball reach, FOV) with a tuned bundle. You can still tweak any
slider afterward; picking a different preset later re-overwrites
everything. See [Presets](#presets) below for the exact values each one
sets.

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

**Ball reach** (radians, `ball_max_dist_from_cluster`) - how far the ball
may be from the player cluster centroid and still blend into the aim
(Action framing only). Beyond this radius the ball is treated as
off-the-action - a stray detection or the far goal - and ignored, so it
can't drag the camera off the play. `0.5 rad` is the default. Raise it if
the camera won't follow a real, isolated ball (e.g. into a corner, on a
long ball or a breakaway); lower it to keep the camera glued to the
crowd even when the ball briefly separates.

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

**Wide is also the ceiling on the ball-reach widen** - `target_fov` (the
same function computing the bounds above) already widens the shot to
keep a tracked ball in frame (`ball_offset` + `ball_frame_margin_deg`,
doubled), but that computed value is then clamped to `fov_wide`. On a
genuine breakaway the needed width can exceed the `action`/`broadcast`
presets' 48-58° ceiling, so the shot never actually opens up enough to
hold both the ball-carrier and the main group, even with **Ball reach**
raised - see the corner-breakaway bullet below.

**Dead-zone vs. frame margin - two different things, easy to conflate.**
Dead-zone is a *reaction threshold*: how far the target must move before
the camera moves at all (see above). It has nothing to do with how close
a player/ball is allowed to get to the edge of frame. That's controlled
by `ball_frame_margin_deg` (Action framing, widens the FOV to keep this
much clearance around the ball) and `frame_all_margin_deg` (FrameAll,
padding around the full player bounding box so nobody is clipped at the
viewport edge) - see [Extra parameters](#extra-parameters-not-yet-exposed-in-the-gui);
`ball_frame_margin_deg` has no GUI/CLI control today.

## Presets

| Field | default() | `broadcast` | `action` | `frame_all` |
|---|---|---|---|---|
| framing | Action | Action | Action | **FrameAll** |
| confidence_weighted | true | true | true | **false** |
| dead_zone_rad | 0.20 | 0.20 | **0.12** | 0.20 |
| fov_tight / default / wide | 22 / 40 / 58 | 22 / 40 / 58 | **20 / 34 / 48** | 22 / 40 / **70** |
| ball_weight | 0.50 | **0.20** | **0.35** | **0.0** |
| ball_max_dist_from_cluster | 0.50 | 0.50 | 0.50 | 0.50 |
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
- **Camera won't follow the ball into a corner / on a breakaway**: check
  three settings together, in this order (each gates the next):
  1. **Ball anchor range** - if the tracker never accepts the far
     detection in the first place, nothing downstream matters. Widen it
     (0.3-0.5+) first and confirm via `--events`/the events JSONL that
     the ball's `state` goes `Tracking` rather than staying
     `Coasting`/`Lost` during the breakaway.
  2. Raise `ball reach` (`ball_max_dist_from_cluster`) above its 0.5 rad
     default - **and**
  3. raise FOV Wide too (try 65-70°). Ball reach alone only unlocks
  the aim-pull toward the ball; the shot still needs `fov_wide` high
  enough for the widen-for-ball calculation (see above) to actually reach
  its target instead of clamping. Verified via a controlled A/B render on
  the same clip/moment, every other setting held constant: at `fov_wide:
  48°` (the `action` preset's default) the ball-carrier and the main
  group don't both fit; at `70°` they do. This is Action framing's
  designed trade-off - a genuine, isolated ball far from the main group
  is rejected by default so a stray detection or the far goal can't drag
  the camera off the play; raising both knobs trusts the detector more
  and accepts occasionally chasing a false positive. If ball plays like
  this matter more than staying with the crowd, switching **Tracking
  mode -> ball** for that match is often a better fit.

## Extra parameters (not yet exposed in the GUI)

A few `FieldPannerConfig` fields have no Export-dialog control today and
can currently only be changed via a config file / CLI flag consuming
`reco-autocam` directly: `min_cluster`, `edge_push`, `fov_alpha`,
`pitch_near`/`pitch_far`/`distance_bias_max`, `edge_bias_max`,
`cluster_alpha`, `max_velocity_rad_per_sec`, `velocity_alpha`,
`pitch_bias`, `ball_presence_decay`/`ball_presence_attack`,
`velocity_fov_bias_max`, `ball_frame_margin_deg`,
`lead_gain`/`lead_alpha`,
`keep_fraction` (trimmed-mean only). See the field-level doc comments in
`crates/reco-autocam/src/panners/field.rs` for what each does.
