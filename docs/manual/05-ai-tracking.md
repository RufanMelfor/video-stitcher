# 5. AI tracking

With AI tracking the program acts as a camera operator. It detects the ball and
the players in every camera picture, decides where the action is, and pans and
zooms a virtual camera through the panorama to follow it. The result is a normal
video of the match that looks as if one camera followed the play.

AI tracking is applied **when you export**. Its settings are in the **Export
dialog**, under *AI Tracking*.

This chapter is a practical overview. The [AI tracking and panner tuning
guide](../ai-panner-tuning.md) holds the detailed evidence for every value and a
table of symptoms with fixes. Use it when the camera does something you do not
like.

<!-- screenshot: Export dialog, AI Tracking section -->

## What you need

1. **A setup** with a **field ROI** set (see
   [chapter 3](03-calibration.md#detection-zones-field-area-goals-and-ai-limits)).
   Without one the program warns you before exporting, because the AI then has
   no idea which people are on your pitch.
2. **An AI model** (a YOLO model in ONNX format) with a `person` and a `ball`
   class. A `referee` class is allowed and simply ignored by the camera. Set it once in *Preferences* so it is filled in for you.
   A football model is published on Hugging Face as
   `Dura-S/reco-yolo26s-football`.
3. **Video memory** for the lookahead buffer (see [chapter 7](07-exporting.md)).

## Turning it on

In the Export dialog tick **Enable** in the *AI Tracking* section and check the
model path. Then choose how the camera should behave.

### Tracking mode

| Mode | What it does |
|---|---|
| **Field** (recommended) | Follows the group of players and the ball together. Falls back to the ball alone if the model has no player class. |
| **Ball** | Follows only the ball. |
| **Sweep** | A fixed left-right pan with no AI. A baseline for comparison. |

### Style preset and Framing

A **Style preset** (for example *action*) sets every framing slider below it to
tuned values in one go. You can still change any slider afterwards. **Framing**
chooses what the camera aims at:

- **Action:** aims at the player group, pulled toward the ball, with dynamic zoom.
- **Frame all:** aims at the middle of all players and zooms to fit the whole team.

**Lock (horizontal-only)** keeps the tilt fixed and only pans left and right.

## A good first export

Start with these values, then change only what your footage disagrees with.
They are the same values the built-in Football preset applies after your first
Auto Calibrate (see [chapter 3](03-calibration.md#what-is-in-a-setup-file-and-the-starting-preset)),
so you normally do not need to type them in:

| Setting | Start with |
|---|---|
| Tracking mode | Field |
| Detect every N frames | 3 |
| Detection confidence | 0.20 |
| Style preset / Framing | action |
| Lookahead | 0.5 s (0.52) |
| Reduce lookahead memory (8-bit) | **on** for 10-bit footage |
| Ball coast time / Ball hold time | 8 s each |
| Cluster mode (Advanced panner) | Trimmed mean |
| Ball weight | 0.8 |
| Dead zone | 0.15 |

Do a **short test export first** (a minute or two, set with *Processing range*)
and watch it before you export the whole match.

**The first AI export is slow to start.** On a TensorRT build, the program
builds an optimised engine for your GPU the first time you export with AI
tracking, again when you use a different AI model, and possibly after a GPU or
TensorRT change.
This can take several minutes during which the progress bar shows no frames
and the status reads *Initializing*. The program has not hung: wait. The
result is saved in the `trt-cache` folder (see
[troubleshooting](08-troubleshooting.md#tensorrt-cache-tensorrt-builds-only)),
so later exports start quickly.

## Reading what the AI did: the AI Debug export

When the camera misbehaves you need to see what the AI actually saw. The **AI
Debug** tab of the Export dialog exports each camera's own picture with the
detection boxes drawn on it, as two videos (`_L` and `_R` are added to the file
name). You choose the model, which camera(s), the processing range, and the
detection interval and confidence. Use it to check, for example, whether the AI
finds the ball, whether it mistakes something else for a ball, and where the far
touchline of your own pitch sits.

The **Save AI debug events** option in the normal export tab writes every
detection, filter decision and pan decision to a `.jsonl` file next to the
export, for offline analysis.

## The settings that matter most

These are explained in plain language in
[the most important settings](top-settings.md#4-ai-tracking). In short:

- **Detect every N frames:** the biggest speed lever. Higher is faster but slower
  to follow quick action.
- **Detection confidence:** raise it if weak detections pull the camera, lower it
  if real balls are missed.
- **Lookahead:** lets the camera lead the action and move smoothly.
- **Ball coast time and Ball hold time:** how patiently the camera waits for a
  ball it has lost.
- **Cluster mode, Dead zone and Ball weight:** how calm or twitchy the camera is
  when nothing dramatic happens.
- **Ball height limit:** ignores balls on a neighbouring pitch. **Measure it per
  rig and never copy it from another setup.**

## When the camera does something odd

Look up the symptom in the [tuning guide](../ai-panner-tuning.md#fix-a-symptom).
It covers, among others: losing the ball for long stretches, not following a
breakaway, drifting off and swinging back, jitter during static play, and
following the wrong ball.

## Remember

- AI tracking shows up in the exported file. To get a feeling for it before you
  export, the *zoom range* picture in the AI Tracking section draws boxes for
  the widest and tightest framing the camera will use, on the current frame.
- Settings you choose are remembered, and are saved in the setup so that
  the same rig starts with the same AI settings.

Continue with [chapter 6: cuts, scoreboard and Match Logger](06-cuts-scoreboard-match-logger.md).
