# AI camera tracking & panner tuning

Reference for the **AI Tracking** section of the Export dialog
(`reco-gui`). Grounded in `crates/reco-autocam/src/panners/field.rs` and
`crates/reco-autocam/src/trackers/ball.rs` - measured, not guessed.

**Read this first:** [Start here](#start-here) has the settings.
Everything below it is the evidence, in collapsible blocks - open one
only when a value surprises you or you need to change it.

---

## Start here

Copy these, then adjust only what your footage disagrees with.

### Tracking

| Setting | Value | Default |
|---|---|---|
| Tracking mode | `field` | `field` |
| Detect every N frames | 3 | 3 |
| Detection confidence | 0.10-0.20 | 0.10 |
| Ball anchor range | 0.3-0.5 rad | 0.20 |
| Ball coast time | 8s | 0.67s |
| Ball hold time | 8s | 0 |

### Framing

| Setting | Value | Default |
|---|---|---|
| Style preset | `action` | - |
| Framing | `action` | `action` |
| Pitch - Lock (horizontal-only) | off | off |
| Lookahead | 0.5s | 0.5s |
| Reduce lookahead memory (8-bit) | **on** for 10-bit sources | off |

### Advanced panner

| Setting | Value | Default |
|---|---|---|
| Cluster mode | `trimmed_mean` | `density` |
| Cluster bandwidth | 0.3-0.35 rad | 0.3 |
| Ball weight | 0.5-0.6 | 0.35 (action) |
| Ball reach | 1.0 rad | 0.5 |
| Ball margin | 5 deg (Ball mode), 3 (Field) | 3.0 |
| Dead-zone | 0.05-0.08 rad | 0.12 (action) |
| FOV Tight / Default / Wide | 20 / 34 / 65-70 deg | 20 / 34 / 48 |
| Zoom smoothing (`fov_alpha`) | 0.05-0.08 | 0.01 |
| Aim (cluster) (`cluster_alpha`) | 0.05-0.20 | 0.012 |
| Reactivity (lookahead) | 1.5 | 2.5 |

### Stray-ball guards

| Setting | Value | Default |
|---|---|---|
| Ball max speed | 0.13 rad/frame | 0.13 |
| **Ball height limit** | **measure per rig** | 0 (off) |
| Ball jump confidence | 0.5 | 0.5 |
| Ball acquire range | 0 (off) | 0 |
| Player height limit | 0 (off) | 0 |

> **Ball height limit is the one setting you must never copy between
> cameras.** A value from another rig can silently discard most of your
> ball detections.
> [How to measure it](#ball-height-limit-measure-it-per-rig).

---

## Fix a symptom

| Symptom | Try this |
|---|---|
| Camera loses the ball for long stretches | [Ball height limit](#ball-height-limit-measure-it-per-rig) first - measure it |
| Won't follow a breakaway / into a corner | [Five settings, in order](#camera-wont-follow-a-breakaway) |
| Drifts off the ball and swings back | Ball coast time + Ball hold time to 8s |
| Loses the ball at the touchline (throw-in) | Ball coast time 2.5s+ |
| Jittery on static play | Raise Dead-zone (0.15 fixed a real case), or raise Lookahead |
| Every small ball move drags the frame | Raise Dead-zone first, then lower Ball weight |
| Camera "gives up" mid-breakaway | Raise Zoom + Aim smoothing to 0.05-0.08 |
| Zooms too far in/out | Adjust FOV Tight/Wide - they are bounds, not targets |
| Follows the wrong ball | [Second ball in view](#when-a-second-ball-is-in-view) |
| Cluster gets lost in midfield | Cluster bandwidth 0.35-0.4 |

---

## Ball height limit: measure it per rig

**Default 0 = off.** A ball on a neighbouring pitch sits higher in frame
and is about half the size, so a ceiling on world pitch can exclude it.
That works - but the correct value is a property of *your camera mount*,
not of football.

Two real rigs, same software:

| Rig | Match ball sits at | Correct ceiling |
|---|---|---|
| `03 Away_Club - sv O.S.S` | pitch **-0.16 .. -0.31** | 0.12 |
| `05 GVV Away-Club` | pitch **+0.28 .. +0.357** | 0.40 |

Copying `0.12` to the second rig put the ceiling *below the entire ball
distribution*. It discarded **81.8% of all ball detections** before the
tracker saw them, leaving one continuous **40.3s** stretch with no ball.
The symptom looked like a panner problem ("camera loses the ball after
20 seconds"), which is why this deserves its own section.

**To measure it:**

1. Export with `--events trace.jsonl`.
2. Read the `pitch` of every ball entry in `detections_raw`. Rejected
   detections still appear there, so one run with the filter set wrong
   still gives you the right answer.
3. Set the ceiling just above the observed maximum.
4. Leave it at `0` if no neighbouring pitch is in view.

<details>
<summary>Measured effect (500-560s, all else identical)</summary>

| Ball height limit | Tracking | Coasting | No ball | Longest gap |
|---|---|---|---|---|
| 0.12 (inherited from other rig) | 11.7% | 20.2% | 68.1% | 40.3s |
| 0.40 (measured on this rig) | 56.7% | 42.4% | 0.8% | 0.5s |

Four panner variants (dead-zone 0.15 -> 0.04, cluster_alpha 0.12 -> 0.25,
fov_tight 38 -> 32, ball margin 5 -> 8) produced **bit-identical**
tracking numbers. That is the cleanest possible proof that no panner
control compensates for this: the ball was already gone before the
panner ran.

Validated across 160-220s, 500-560s, 720-780s and 1420-1460s - no ball
detection anywhere above 0.40. The detector was never the problem: 1565
ball detections, mean confidence 0.55.

Coasting stays high (42-63%) after the fix because the ball is still only
~16x17 px at the model. Ball coast/hold time at 8s absorbs that.
(2026-09-19)
</details>

---

## Camera won't follow a breakaway

Five settings gate each other **in this order**. Raising one without the
others will not fix it.

1. **Ball anchor range** -> 0.3-0.5. If the tracker rejects the far
   detection, nothing downstream matters. Confirm via `--events` that
   the ball's `state` reads `Tracking`, not `Coasting`/`Lost`.
2. **Ball reach** -> 1.0. Lets the panner pull the aim toward it.
3. **FOV Wide** -> 65-70 deg. The widen-for-ball calculation clamps to
   this; at 48 deg the ball-carrier and the group do not both fit.
4. **Zoom + Aim smoothing** -> 0.05-0.08. The defaults have a ~3s time
   constant; a breakaway is over before the camera arrives.
5. **Ball weight** -> 0.5. Only if the ball separates *vertically*
   (toward the near touchline). Below this the aim target itself never
   gets close enough.

<details>
<summary>How each was verified</summary>

**Ball anchor range** - raw-detection logs showed the model detecting a
breakaway ball at 0.97 confidence while the tracker coasted, because the
detection sat outside the gate.

**FOV Wide** - controlled A/B render, same clip and moment, everything
else constant: at 48 deg both subjects do not fit; at 70 deg they do.

**Smoothing** - real trace on a corner breakaway: FOV climbed 38.7 ->
39.9 deg over the ~20 frames the ball stayed trackable, with the target
well past 65 deg. Aim pitch barely moved while the ball's pitch shifted
0.24 rad.

**Ball weight** - CLI A/B render: at 0.35 the *computed aim target* never
gets close enough on a vertical break. Not a gate or smoothing problem.
Cost: +30-33% average frame-to-frame pose delta vs 0.35 - real, but far
short of the visible wobble `1.0` causes. (2026-08-12)

This is Action framing's designed trade-off - an isolated ball is
rejected by default so a stray detection cannot drag the camera off play.
If ball plays matter more than staying with the crowd, **Tracking mode ->
ball** is often the better fit for that match.
</details>

---

## Ball coast time + Ball hold time

The pair that matters most after Ball height limit. Both to **8s**.

They solve different halves of "the camera drifts off the ball and comes
back". **Coast time** = how long the *tracker* keeps reporting a position
through a detection gap. **Hold time** = how long the *camera* keeps
using that last position after the tracker gives up.

<details>
<summary>Why 8s, and the measured effect</summary>

From the gap distribution, not taste: the problem window had 17 detection
gaps >0.1s, the longest 7.9s. 2.5s bridged 10, 5s bridged 11, **8s
bridged all 17**.

```
coast 5s, hold 0   25 deg off ball   48% in frame   <- old GUI default
coast 8s, hold 0   23 deg            47%
coast 8s, hold 3   19 deg            47%
coast 8s, hold 8   13 deg            65%            <- recommended
```

The calm window was unchanged by all four (22 deg / 33%), so this is free
rather than a trade. Total camera travel also *fell* (4.82 -> 3.79 rad):
the shot gets steadier, not busier.

The swing this prevents is wasted motion - a ball that disappears
reappears a median of **1.4 deg** (near play) to **8 deg** (far play)
from where it vanished, within half a frame width in **65-91%** of cases.

A ball that was never tracked (kids warming up outside the pitch) never
starts the countdown, so raising these does not make false triggers more
likely. (2026-09-18)
</details>

---

## When a second ball is in view

A warm-up or adjacent-match ball entering the field ROI can steal the
camera. On `03 Away_Club - sv O.S.S` 120-180s the tracker spent 30%
of the window on the wrong ball.

**What works:**

- **Ball max speed** (0.13 rad/frame). A real ball cannot cross the
  panorama between two frames, so anything faster is a different ball.
  Physics, not a heuristic - which is why it generalises. Results were
  flat across 0.08-0.20, so the value is not delicate.
- **Ball height limit** - cut time-on-wrong-ball from 30% to 1%, but
  [measure it per rig](#ball-height-limit-measure-it-per-rig).

<details>
<summary>What was measured and rejected (do not re-try)</summary>

- **Detector confidence** cannot separate the two balls: both detected at
  0.79-0.80. Lowering 0.20 -> 0.05 made it *worse* (our ball 21% -> 22%,
  stray ball 30% -> 46%).
- **Proximity to the player cluster** is backwards here: the stray ball
  had 5 players within 0.40 rad, the real ball had 2 - the kids playing
  with the other ball are tracked people too. This is what `Ball acquire
  range` implements; leave it off.
- **Motion / "the ball is stationary"** fails because the match ball is
  frequently stationary too (throw-ins, restarts, dead time).
- **Player height limit** appears to help for the wrong reason: at 0.10
  it leaves a median of one player, the cluster collapses and the panner
  falls back to ball-only follow in 61% of frames. Flatters that window,
  misbehaves the moment play moves upfield. (2026-09-18)
</details>

<details>
<summary>The real limit is detection, not tuning</summary>

Production preprocessing scales the whole 3840x2880 frame into the
model's 1920x1920 square, so a ball measuring 32x34 px in the source
reaches the model at **16x17 px**, with 25% of the input wasted on
letterbox padding.

When the ball is seen at all, confidence is high (median 0.83) - the
model is not unsure, it misses small balls. This is why neither
`detection_interval` 1 vs 3 (21% detection rate either way) nor any
confidence threshold moves the needle, and it caps what panner tuning can
achieve.

Tiled inference would present the same ball at ~21x23 px; the code exists
for the TensorRT/CUDA path but production on Windows runs the wgpu path,
which has no tiling. Exporting at a non-square 1440x1920 input does *not*
help: `CpuYoloDetector::input_size` is a single square dimension, so reco
reads it as 1440x1440 and scales the ball down further (tested
2026-09-18, also ~4x slower).
</details>

---

## What each control does

<details>
<summary>Tracking mode, Detect every N frames, Detection confidence</summary>

**Tracking mode** - `field` (default) follows the player cluster + ball
together, falling back to ball-only if the model has no player class.
`ball` follows only the ball (no player cluster at all, `ball_weight`
forced to 1.0, tighter max-jump gate). `sweep` is a fixed sinusoidal pan,
for debugging only.

**Detect every N frames** - how often the detector runs; frames in
between reuse the last detection. `3` (~0.1s at 30fps) is a good default.
Going 3 -> 10 nearly tripled framerate (19 -> 52fps) on a live export -
by far the biggest performance lever in the panner. The tracking-quality
cost of that specific change has not been re-validated: treat the fps win
as confirmed and the quality impact as unknown, not zero. (2026-08-29)

**Detection confidence** - floor the detector applies before anything
downstream sees a candidate. Default `0.10`. Replaces a previously
hidden behaviour where Ball mode silently forced `0.25`; if you relied on
that, set it explicitly now. A competing product's runtime log shows
`0.55`, 5.5x this default - not proof, but a data point if raising it.
</details>

<details>
<summary>Ball anchor range (and the near/far ramp)</summary>

A gate inside the ball *tracker*, upstream of everything else here. A raw
ball detection is only accepted if it lies within this distance of at
least one tracked player. Default `0.20 rad` (~11 deg).

This runs *before* Ball reach, so a genuine isolated ball - the exact
breakaway case Ball reach and FOV Wide exist for - can be silently
discarded here first, making those two look broken.

**Near/far ramp (CLI-only).** A flat radius under-covers a ball close to
a downward-tilted rig: the same real-world distance maps to a much larger
panorama gap near the camera than far up the pitch.
`--player-anchor-rad-near` sets a separate radius at world pitch
<= -0.05 rad, ramping to `--player-anchor-rad` at pitch >= 0.20 rad.
**Tested and rejected at 30 deg** (2026-09-03): admits a static white
field-marker disc as a false ball for 65+ seconds. The mechanism is
sound; that specific wide value is not. Do not raise without new
evidence.
</details>

<details>
<summary>Cluster mode, Ball weight, Ball reach, Cluster bandwidth, Dead-zone</summary>

**Cluster mode** - `density` centers on the densest concentration of
players, so a distant knot cannot drag the aim away. `trimmed_mean`
averages all players and trims outliers - simpler, but a distant group
can still pull the mean before trimming kicks in. `trimmed_mean` fixes
the multi-second freeze during ball-less stretches, which is why it is
recommended above; it was tested together with Dead-zone.

**Ball weight** - blend weight of ball vs player cluster (Action framing
only). Effective pull is `ball_weight x ball_presence`, so it only tugs
while the ball is actually present. Forced to `1.0` in Ball mode.

**Ball reach** - how far the ball may be from the cluster centroid and
still blend into the aim. Beyond it, the ball is treated as
off-the-action and ignored.

**Cluster bandwidth** - neighbourhood radius for `density` mode. Wider
pulls a spread formation into one group; narrower isolates a tight core.
No effect under `trimmed_mean`.

**Dead-zone** - the camera holds still while the target stays within this
radius; larger errors ease in rather than snap. Note it also *subtracts*
from larger errors (`scale = (mag - dz) / mag`), so a high value delays
the start of every move, not just small ones. At 0.15 rad (8.6 deg) a
10 deg error leaves only 1.4 deg to act on.
</details>

<details>
<summary>Field of view, and why Wide is also a ceiling</summary>

Tight / Default / Wide are **not** three zoom levels. They are the bounds
of a continuously-varying zoom recomputed every frame from cluster spread
plus distance/edge/velocity biases. Default is only the starting FOV
before any players are detected.

`target_fov` already widens the shot to keep a tracked ball in frame
(`ball_offset + ball margin`, doubled), but that value is then clamped to
**FOV Wide**. On a real breakaway the needed width exceeds the
action/broadcast presets' 48-58 deg ceiling, so the shot never opens up -
even with Ball reach raised.
</details>

<details>
<summary>Smoothing: Zoom, Aim (cluster), and Reactivity</summary>

**Zoom / Aim smoothing** (`fov_alpha` / `cluster_alpha`) - how fast the
smoothed zoom and aim catch up to their targets each frame, as an EMA
rate. Separate from Lookahead: Lookahead controls how much future/past
goes into the target; these control how fast the presented value chases
it. Defaults have a ~3-second time constant at 30fps.

**Reactivity (lookahead)** -> 1.5, down from the action preset's 3.0.
Fixes the camera visibly "sprinting" to top speed instead of easing there
on a large sudden target move. This multiplier raises both the velocity
clamp and the ramp-up rate whenever lookahead is active, so a high value
overshoots before the smoother can remove the jitter. Lowering it cut
peak per-frame jump and peak half-second swing ~45% in a real A/B render,
with ball presence and typical smoothness unchanged. That asymmetry
(peaks down, average unchanged) is this parameter's signature.
(2026-09-03)
</details>

<details>
<summary>Dead-zone vs Ball margin - easy to conflate</summary>

**Dead-zone** is a *reaction threshold*: how far the target must move
before the camera moves at all. It has nothing to do with how close the
ball gets to the edge of frame.

**Ball margin** (`ball_frame_margin_deg`, slider added 2026-09-19,
default 3.0 deg) widens the FOV to keep that much clearance around the
ball. `frame_all_margin_deg` does the same for the player bounding box in
FrameAll and still has no GUI control.

**Ball margin rarely does anything in Field mode, by design.** It is a
*floor* on the FOV - the zoom widens to at least
`2 x (ball_offset + margin)` - and `ball_offset` is measured from the
already ball-blended aim, so it is small whenever Ball weight is doing
its job. With a normal cluster, `2 x spread` plus the biases is almost
always larger, so `max()` picks that and the margin never binds. Measured
on real footage: **3.0 and 15.0 produced identical framing.**

It bites in **Ball mode**, where there is no cluster, spread collapses
and the FOV falls back to Tight. There the margin is the binding term and
5-8 deg is sensible.
</details>

<details>
<summary>Lookahead, and the 10-bit requirement</summary>

**Lookahead** buffers N seconds of future frames so the panner can smooth
over past+future and lead the play. `0.5s` is a safe middle; higher is
smoother but costs VRAM.

**Reduce lookahead memory (8-bit) is currently required for 10-bit
sources**, not just a VRAM fallback. With it off, a 10-bit source (DJI
Action 4 HEVC, `P010`) under zero-copy decode hits a reproducible crash:
wgpu's Dx12 backend rejects the lookahead pool's plane copy with `Source
format (P010) and destination format (R16Unorm) are not copy-compatible`.
Not yet fixed - leave it **on** for any 10-bit source with lookahead, or
pass `--no-zero-copy` (real decode cost) or `--lookahead 0`. 8-bit
sources are unaffected. (2026-08-12)
</details>

<details>
<summary>Jittery picture: two causes, two fixes</summary>

**Children milling around a stationary ball** kept nudging the camera at
a low dead-zone (0.048 rad). Raising to **0.15 rad** fixed it with no
observed downside. (2026-09-03)

**Every small ball movement drags the frame.** Unlike the player cluster,
which is EMA-smoothed via `cluster_alpha` before reaching the aim, the
ball's position is blended in **raw, with no smoothing of its own**:
`target = cluster x (1 - w) + last_ball_pos x w`. So whenever the ball is
confidently tracked, every frame's detection noise feeds straight into
where the camera points.

1. **Raise Dead-zone** (0.05 -> 0.09-0.10) - absorbs jiggle without
   reducing how hard a real ball move pulls the aim.
2. **If that is not enough, lower Ball weight** (0.5 -> 0.35). Trade-off:
   0.5 is what the breakaway checklist recommends raising *to*.

A cleaner fix would be a dedicated smoothing rate for the ball's own
contribution, parallel to `cluster_alpha`. Not built. (reported
2026-08-29, reasoned from code, not yet A/B-validated)
</details>

---

## Presets

| Field | default() | `broadcast` | `action` | `frame_all` |
|---|---|---|---|---|
| framing | Action | Action | Action | **FrameAll** |
| confidence_weighted | true | true | true | **false** |
| dead_zone_rad | 0.20 | 0.20 | **0.12** | 0.20 |
| fov tight/default/wide | 22/40/58 | 22/40/58 | **20/34/48** | 22/40/**70** |
| ball_weight | 0.50 | **0.20** | **0.35** | **0.0** |
| ball_max_dist_from_cluster | 0.50 | 0.50 | 0.50 | 0.50 |
| edge_push | 0.15 | 0.15 | **0.20** | 0.15 |
| lookahead_reactivity | 2.5 | 2.5 | **3.0** | 2.5 |
| frame_all_margin_deg | 8.0 | 8.0 | 8.0 | **10.0** |

`broadcast` is the calm default. `action` is tighter and more reactive
across the board. `frame_all` switches framing algorithm entirely.

Picking a preset **overwrites every slider below it**; you can still
tweak afterward, but choosing another preset re-overwrites everything.

Source: `FieldPannerConfig::{broadcast, action, frame_all}`.

---

## Where settings are stored

- **Per calibration** - hit **Save calibration** in the Export dialog.
  See `Calibration::autocam_defaults`.
- **App-level "last used"** - every slider auto-saves the moment you
  change it, no click needed. Restored on restart, then overridden by a
  calibration that has its own saved values. See
  `GuiSettings::autocam_defaults`.
- **In the export** - with `--events` on and AI tracking enabled, the
  first line of the JSONL is a `{"kind":"run_config", ...}` record with
  every field above, so a trace is self-describing. See
  `PipelineEvent::RunConfig`.

## Extra parameters (not exposed in the GUI)

Changeable only via a config file or CLI flag consuming `reco-autocam`
directly: `min_cluster`, `edge_push`,
`pitch_near`/`pitch_far`/`distance_bias_max`, `edge_bias_max`,
`max_velocity_rad_per_sec`, `velocity_alpha`, `pitch_bias`,
`ball_presence_decay`/`ball_presence_attack`, `velocity_fov_bias_max`,
`lead_gain`/`lead_alpha`, `frame_all_margin_deg`, `keep_fraction`
(trimmed-mean only).

See the field-level doc comments in
`crates/reco-autocam/src/panners/field.rs`.
