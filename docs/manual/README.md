# RECO Panopitch user manual

> **Draft.** This manual is being written chapter by chapter. Chapters marked
> *planned* do not exist yet. Screenshots will be added later; the places that
> need one are listed at the bottom.

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
| Know which settings matter most, and what to start with | [The most important settings](top-settings.md) |
| Look up one specific setting | [Settings reference](settings-reference.md) |
| Fix a camera that follows the wrong thing or moves too much | [AI tracking and panner tuning](../ai-panner-tuning.md) |
| Understand export speed and memory use | [Async detection benchmark](../async-detect-benchmark-v054.md), [GPU notes](../gpu-hardware-recommendation.md) |

Every setting in the app has a small **?** badge. Click it to read the same
explanation that appears in the [settings reference](settings-reference.md).

## Contents

| # | Chapter | Status |
|---|---|---|
| 1 | Before you start: what you need, supported hardware, installing | planned |
| 2 | Setting up the cameras and loading your footage (Match Folder) | planned |
| 3 | Calibrating the two cameras: automatic and by hand | planned |
| 4 | Colour and sharpness | summary in [top settings](top-settings.md) |
| 5 | AI tracking | [existing guide](../ai-panner-tuning.md) |
| 6 | Cutting out pauses, the scoreboard and the Match Logger | planned |
| 7 | Exporting: quality, speed, memory | summary in [top settings](top-settings.md) |
| 8 | When something goes wrong (common problems and fixes) | planned |
| A | [The most important settings](top-settings.md) | draft |
| B | [Settings reference](settings-reference.md) (generated from the app) | generated |

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

These places in [top-settings.md](top-settings.md) are marked
`<!-- screenshot: ... -->`:

- Seam section with the red seam line visible
- Overlap view in Difference mode, aligned and misaligned
- Calibration panel: Roll, Skew and tilt sliders
- Color Mapping panel
- Export dialog, Export tab
- Export dialog, AI Tracking section
- Cut ranges on the timeline

Use footage without recognisable people (or blur faces) for every screenshot.
