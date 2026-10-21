# RECO Panopitch user manual

> **Draft.** All chapters now exist but have not yet been tested against a fresh
> install, and screenshots are still to be added (the places that need one are
> listed at the bottom). Tell us where the text is unclear or wrong.

RECO Panopitch turns the footage of two cameras into one panoramic match video
and can steer a virtual camera by itself, following the ball and the players.
This manual explains what you can set, why, and what to try when the result is
not what you expected.

**Tested on:** Windows 11 with an NVIDIA GeForce RTX 3060 Ti (8 GB), DJI Osmo
Action 4 footage. Other systems and cameras are untested.

## How to read this manual

You do not need to read everything. Pick the part that matches what you are
doing:

| I want to... | Go to |
|---|---|
| Get from two videos to a finished export | Chapters [1](01-before-you-start.md) to [7](07-exporting.md), in order |
| Know which settings matter most, and what to start with | [The most important settings](top-settings.md) |
| Fix a problem | [When something goes wrong](08-troubleshooting.md) |
| Look up one specific setting | [Settings reference](settings-reference.md) |
| Fix a camera that follows the wrong thing or moves too much | [AI tracking and panner tuning](../ai-panner-tuning.md) |
| Understand export speed and memory use | [Async detection benchmark](../async-detect-benchmark-v054.md), [GPU notes](../gpu-hardware-recommendation.md) |

Every setting in the app has a small **?** badge. Click it to read the same
explanation that appears in the [settings reference](settings-reference.md).

## Contents

| # | Chapter |
|---|---|
| 1 | [Before you start](01-before-you-start.md): what you need, what is tested, installing, first start |
| 2 | [Cameras and footage](02-cameras-and-footage.md): recording, the match folder, loading a match |
| 3 | [Calibration](03-calibration.md): automatic and by hand, sync, detection zones |
| 4 | [Colour and sharpness](04-colour-and-sharpness.md) |
| 5 | [AI tracking](05-ai-tracking.md): overview and a good first export |
| 6 | [Cuts, scoreboard and Match Logger](06-cuts-scoreboard-match-logger.md) |
| 7 | [Exporting](07-exporting.md): quality, speed, video memory |
| 8 | [When something goes wrong](08-troubleshooting.md) |
| A | [The most important settings](top-settings.md) |
| B | [Settings reference](settings-reference.md) (generated from the app) |
| C | [AI tracking and panner tuning guide](../ai-panner-tuning.md) (detailed evidence and a symptom table) |

## About the settings reference

[`settings-reference.md`](settings-reference.md) is generated from the help
texts inside the app by `scripts/gen_settings_reference.py`. Never edit it by
hand: change the help text in `crates/reco-gui/ui/main.slint` and run

```
python scripts/gen_settings_reference.py
```

`python scripts/gen_settings_reference.py --check` fails when the file is out of
date.

## Screenshots to add later

Places marked `<!-- screenshot: ... -->` in the chapters:

- Chapter 1: the main window with the three areas labelled
- Chapter 3: the Files panel with the Auto Calibrate button; Overlap view in
  Difference mode (aligned and misaligned); the lens preview with the ROI polygon
- Chapter 4: the Color Mapping panel
- Chapter 5: the Export dialog, AI Tracking section
- Chapter 6: cut ranges on the timeline; the scoreboard section and the
  scoreboard on the preview
- Chapter 7: the Export dialog, Export tab
- Top settings: Seam section with the red seam line; Calibration panel sliders

Use footage without recognisable people (or blur faces) for every screenshot.
