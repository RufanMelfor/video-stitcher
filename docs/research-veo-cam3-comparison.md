# Veo Cam 3 - feature research and gap comparison

Research note, not a roadmap. Purpose: survey what a leading commercial
competitor (Veo Cam 3, ~1000+ EUR camera + mandatory subscription) does,
and note where reco already matches, partially matches, or has a real
gap. No issues opened, no work committed from this note by itself -
see "Ideas worth a closer look" at the end for candidates to file
separately if/when prioritized.

Sources: [Veo Cam 3 product page](https://www.veo.com/en-us/product/veo-cam-3),
[Veo - sports video camera systems compared](https://www.veo.com/article/sports-video-camera-systems-compared),
[Veo Help Center - About Match Events](https://support.veo.co/hc/en-us/articles/24763703494161-About-Match-Events),
[Veo Help Center - About Match stats](https://support.veo.co/hc/en-us/articles/11466847094417-About-Match-stats-Veo-Analytics),
[Veo - automatic event tagging article](https://www.veo.co/en-us/article/how-automatic-event-tagging-transforms-post-match-analysis-workflow),
[ReelMind - Veo Cam 3 technical deep dive](https://reelmind.ai/blog/veo-cam-3-technical-specifications-deep-dive-into-ai-camera-tech).

## What Veo Cam 3 actually does

**Hardware**: dual 4K sensors, fixed tripod rig, 180-degree combined
field of view, 1.25kg, IP54 weatherproof (-10C to 45C), 6.5h battery,
Wi-Fi (5G model available), records while charging.

**Core pipeline**: the two synchronized streams are stitched into one
panoramic video. An onboard "AI Director" digitally pans/zooms within
that panorama to produce a 1080p broadcast-style follow-cam feed in
real time - no physical camera movement, no operator. Post-recording
"SteadyView" stabilization pass removes residual shake. Wind-noise
reduction on the audio track.

**App**: single-tap record start/stop, live phone-based preview and
remote control from the sideline, "Interactive Mode" to manually
pan/zoom across the full 180-degree capture independent of what the
follow-cam is doing, auto-upload to their cloud after the match.

**AI event tagging** (this is the part directly relevant to the goal-
detection idea discussed earlier): automatically detects and tags
kickoff, goals, corners, free kicks, goal kicks, penalty kicks, and
shots on goal, building a searchable event timeline with no manual
tagging needed. Confirms goal-detection specifically is a proven,
commercially validated feature, not a speculative one.

**Analytics** (paid add-on, "Veo Analytics"): match stats table (event
counts per team, AI-generated and manually-tagged combined), 2D
positional maps for players and referee, shot charts, per-player stats
once events are assigned to a player. "Player Spotlight" add-on follows
one specific player through the match - this needs individual player
identity, not just a generic "person" detection.

**Live/sharing** (paid add-ons): "Veo Live" streams to multiple
platforms at once; "League Exchange" lets clubs share recordings across
teams/leagues.

**Business model**: camera purchase + mandatory per-camera subscription
to watch/analyze anything, tiered by feature set (video download itself
is gated above the Starter tier).

## Comparison against reco's current crates

| Veo feature | reco equivalent | Status |
|---|---|---|
| Dual-sensor panoramic stitching | `reco-core` GPU stitching engine | Match - same core architecture (two fixed cameras -> stitched panorama) |
| AI Director digital pan/zoom | `reco-autocam` (directors, trajectory smoothing, ROI filtering) | Match - same concept, tuning documented in `docs/ai-panner-tuning.md` |
| Ball/player detection | `reco-detect` (person + ball classes) | Match at the detection level |
| Manual "Interactive Mode" pan/zoom | `reco-gui` Expert Mode / FOV controls | Partial - desktop only, no phone-based remote control |
| Livestreaming | `reco-obs` (OBS Studio plugin) | Different approach, arguably more flexible - reco streams anywhere OBS can, Veo Live is a fixed set of integrations. Not a gap, a different tradeoff |
| SteadyView post-stabilization | none identified | Gap - worth checking if stitching/calibration precision already makes this moot, or if it's a real missing polish pass |
| Wind noise reduction (audio) | none identified | Gap, but scope question - unclear if reco's audio path (via `reco-io`/FFmpeg) is meant to do DSP at all, or just passthrough |
| Automatic event tagging (goals, corners, free kicks, cards, kickoff, etc.) | none yet - only "goal" discussed so far, no code | Gap - the goal-detection idea already being scoped is the first slice of a much bigger feature category Veo already ships in full |
| Match stats / analytics dashboard | none | Gap - would need per-team and per-player event aggregation, no UI for it today |
| Player Spotlight (follow one named player) | none - `reco-detect`'s "person" class has no identity, just presence | Gap, and a hard one - needs player re-identification (tracking identity across frames/occlusions, possibly jersey-number OCR), a materially harder CV problem than person/ball detection |
| Cloud upload + subscription-gated viewing | none - reco is self-hosted/local by design | Deliberate non-goal, not a gap. Matches the project's "open alternative to proprietary sports camera solutions" positioning (see AGENTS.md) - copying Veo's subscription model would work against that |
| League Exchange (cross-club sharing marketplace) | none | Out of scope for reco-core - a platform/community feature, not a video-pipeline one |

## Goal, not a copy

Not aiming to clone Veo. Aiming to reach and exceed the same
capability set, in the open, self-hosted and free for the community -
that's the actual differentiator (see "Deliberate non-goal" row above).
The breakdown below turns the gaps into a phased, sized task list so
this can be picked up incrementally later, each item small enough to
become its own GitHub issue and PR (matching this project's normal
workflow - see AGENTS.md) rather than one giant effort.

## Task breakdown - phased roadmap

Sizes are rough gut-feel (S = a day or two, M = the size of a normal PR
here, i.e. `feat/color-matching-multiband`-scale, L = multi-PR effort,
XL = its own sub-project). Nothing here is filed as a real GitHub issue
yet - `gh` isn't set up on this machine, see SESSION_HANDOFF.md. File
these individually once picked up, don't build the whole roadmap as one
PR.

### Phase 0 - already in motion (tracked elsewhere, listed for context)

- YOLO26s fine-tuning pipeline (person+ball) - see SESSION_HANDOFF.md,
  `yolo26n_training_pipeline` memory. Everything below depends on this
  being solid first: bad ball detection means bad event detection means
  bad stats.
- `reco-calibrate` goal-line/goal-area geometry - already logged as a
  task in SESSION_HANDOFF.md, prerequisite for the first item in
  Phase 1. Owner crate: `reco-calibrate`.

### Phase 1 - event detection and tagging (M, builds directly on Phase 0)

Do this as one reusable primitive plus per-event instances, not four
separate one-off detectors:

1. **[M] Ball-in-zone event primitive** - generic "ball trajectory
   intersects calibrated zone X, matching pattern Y (crossing / resting
   / restart-from)" building block in `reco-autocam`. Everything else
   in this phase is a config of this, not new detection code.
2. **[S] Goal detection** - already scoped: crossing the calibrated
   goal line + confirmed by ball returning to/restarting from the
   center circle shortly after (the validation rule from this session).
   First concrete user of item 1.
3. **[S] Kickoff/restart detection** - ball placed at center circle,
   play resumes. Doubles as the confirmation signal for item 2, and is
   also a match event in its own right (Veo tags it separately too).
4. **[M] Corner detection** - ball exits over the byline outside the
   goal frame. Needs `reco-calibrate` to also mark touchline/byline
   extent, not just the goal - small extension of the Phase 0
   calibration task, not a separate calibration feature.
5. **[M] Goal-kick detection** - ball dead in the defensive area,
   restart taken from inside the box. Same zone-primitive pattern as
   the rest.
6. **[M] Event timeline data model** - where detected events actually
   get recorded (extend the existing `detections.jsonl`-style pipeline
   with an `events.jsonl`, or fold events into the same stream with a
   type tag - needs a decision, not just implementation). Foundational
   for Phase 2, do this alongside item 2 rather than after item 5.

Explicitly out of Phase 1: free kicks and cards. Veo's tagging for
those almost certainly leans on referee whistle/gesture recognition,
not ball position - a different, harder problem, own future phase if
ever picked up.

### Phase 2 - match statistics (S/M, thin layer on Phase 1's data)

1. **[S] Per-team event counts** - aggregate whatever Phase 1 produces.
   No new CV work, pure aggregation once events exist.
2. **[M] Stats surface** - decide where this shows up (a `reco-gui`
   panel vs. an exported report file vs. both) and build it. Per-player
   breakdown is blocked on Phase 3, but per-team counts alone are
   already useful and don't need to wait.

### Phase 3 - player identity (L/XL, hardest, explicitly lowest priority)

Only pick this up if a specific need shows up for it - biggest lift of
everything here, genuinely different CV problem from detection.

1. **[M] Spike: re-identification feasibility** - research task, not
   implementation. Establish whether tracking-based re-id (following
   an identity across frames/occlusions) is realistic at this project's
   frame rate/resolution before committing.
2. **[M] Spike: jersey-number OCR as an alternative/complement** - may
   be simpler and more robust than full visual re-id for team-sport
   footage specifically (numbers are large, high-contrast, mostly
   front/back-facing).
3. **[L] "Follow this player" autocam mode** - only after 1 or 2 land,
   built in `reco-autocam` on top of whichever identity signal works.

### Phase 4 - polish/parity, independent of the above (low priority)

1. **[S] Spike: is stabilization actually needed?** - investigation
   only. reco's fixed-rig calibration may already make Veo's
   SteadyView pass moot; confirm before treating it as a real gap.
2. **[S] Spike: audio DSP scope decision** - is wind-noise reduction
   (or any audio processing) even meant to be in scope for reco, or is
   audio strictly passthrough via `reco-io`/FFmpeg today? Needs a scope
   decision before it can become a sized task.
3. **[XL] Mobile/sideline remote control** - Veo's phone-based record-
   start/stop and remote pan/zoom. No `reco-mobile`-scale crate exists
   today - this is its own sub-project, not a quick task, flagged here
   for completeness only.

Not investigated here: pricing/competitor positioning beyond the
feature list, other competitors (Pixellot, Trace, etc.) - flag if a
broader competitive landscape doc would be useful.
