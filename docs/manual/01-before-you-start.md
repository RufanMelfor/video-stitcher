# 1. Before you start

This chapter covers what you need, what has and has not been tested, and how to
get the program running.

## What RECO Panopitch does

You film a match with **two cameras** mounted side by side. RECO Panopitch joins
the two pictures into one wide panorama (stitching) and can then act as a
virtual camera operator: an AI finds the ball and the players and the program
pans and zooms a smooth, TV-style view out of the panorama. You export that as a
normal video file.

The work happens in four steps, which are also the order of this manual:

1. **Record** the match with two cameras ([chapter 2](02-cameras-and-footage.md)).
2. **Calibrate** so the two pictures line up ([chapter 3](03-calibration.md)).
3. **Adjust** colour and sharpness ([chapter 4](04-colour-and-sharpness.md)) and,
   if you want, the AI camera ([chapter 5](05-ai-tracking.md)).
4. **Export** ([chapter 7](07-exporting.md)), optionally with the pauses cut out
   and a scoreboard added ([chapter 6](06-cuts-scoreboard-match-logger.md)).

## What has been tested

> **Tested on:** Windows 11 with an NVIDIA GeForce RTX 3060 Ti (8 GB), using DJI
> Osmo Action 4 cameras (3840x2880, HEVC, 8-bit and 10-bit).
>
> Other operating systems (Linux, macOS), other graphics cards (AMD, Intel, other
> NVIDIA models) and other cameras are **untested**. They may work, but there is
> no guarantee and no support.

If your setup differs from this, expect to find things that need adjusting, and
please report what you find (see [chapter 8](08-troubleshooting.md)).

## What you need

| You need | Why |
|---|---|
| A Windows 11 PC | The only tested system. |
| An NVIDIA graphics card with **8 GB of video memory or more** | Stitching and AI run on the GPU. With less memory you need a shorter lookahead and, for 10-bit footage, the *Reduce lookahead memory* option (see [chapter 7](07-exporting.md)). |
| An up-to-date NVIDIA driver | Hardware decoding and encoding rely on it. |
| Two cameras with the same resolution and frame rate | The program refuses inputs whose frame rates differ. |
| **Google Chrome or Chromium**, installed | Only if you use the scoreboard overlay (it is drawn by a browser). |
| An AI model file (ONNX) | Only if you use AI tracking. A football model is published on Hugging Face as `Dura-S/reco-yolo26s-football`. |
| Plenty of disk space | Source footage and exports are large. A 4K match is many gigabytes. |

Everything runs on your own computer. Your video never leaves it.

## Installing

### Option A: a release package

Release packages will be published on the **Releases** page of this project's
GitHub repository. Download the Windows zip, unpack it into a folder of your
choice and start `reco-gui.exe`. The package already contains the FFmpeg and
AI runtime files it needs. Keep all the files together in the same folder.

### Option B: build it yourself

If no package is available for your setup, build from source. The instructions
are in the main [README](../../README.md#building) (Rust 1.92 or newer, FFmpeg
development libraries and clang). On Windows the FFmpeg DLLs must sit **next to
the program** (`reco-gui.exe`) when you run it, not only when you build it.
Copy them from your FFmpeg `bin` folder.

### Do not mix with upstream Reco

This program is a fork of Reco Video Stitcher. It keeps its settings and caches
in its own `panopitch` folders and only checks this project's releases for
updates. Do not install a build from the original project over it, and do not
expect calibration files to move between the two. When you first start RECO
Panopitch it copies your existing Reco settings into its own folder; the
originals stay untouched.

## First start

1. Start `reco-gui.exe`. You see the welcome text with three steps: add the left
   and right videos, calibrate (or load a calibration), then preview, adjust and
   export.
2. Open **Preferences** (the gear button in the top bar). It is worth setting
   these once:
   - **AI model**: the default model file, so you do not have to browse for it
     for every export.
   - **Default codec** and **Default quality** for exports.
   - **Default calibration**: see [chapter 3](03-calibration.md#re-using-a-calibration).
3. Dark or light theme is also set there.

## The program at a glance

- **Left panel (Files):** choose the match folder or the left and right videos,
  load or create the calibration.
- **Centre:** the stitched preview, with a timeline and playback controls
  underneath.
- **Right panel:** the settings, in collapsible sections (Lens, Calibration,
  Color Mapping, Stats and more). A small **?** next to a setting explains it.
- **Top bar:** Export, Preferences, Debug (log window), a **?** button for
  keyboard shortcuts, and an *Update available* button when a newer release of
  this project exists.

<!-- screenshot: the main window with the three areas labelled -->

Continue with [chapter 2: cameras and footage](02-cameras-and-footage.md).
