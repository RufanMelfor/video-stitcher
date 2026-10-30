# 2. Cameras and footage

Good footage makes everything after it easier. This chapter explains how to
record, how to organise the files, and how to load a match.

## Recording

What the software needs from your recording:

- **Two cameras, mounted rigidly.** Their views must overlap in the middle, and
  they must not move relative to each other during the match or between matches.
  The setup describes exactly how the two cameras sit; if you bump the rig
  or mount the cameras again, calibrate again.
- **The same settings on both cameras:** the same resolution, frame rate and
  video codec. If the frame rates differ the program stops with an error.
  Matching exposure and white balance also helps (see
  [chapter 4](04-colour-and-sharpness.md)), but the program can correct a
  difference.
- **Start both cameras before kick-off** and let them run. The two videos do not
  have to start at exactly the same moment: the program measures the time
  difference between them (the *sync offset*) automatically, using the cameras'
  motion sensor data when it exists and otherwise the **sound** of the two
  recordings. So do not record the match muted.
- **Long matches** are split into several files by the camera (for example DJI
  cameras split recordings at 4 GB). That is fine: the program joins the pieces
  in recording order.

Tested with DJI Osmo Action 4 (3840x2880, HEVC, 8-bit or 10-bit). Other cameras
may work if they give normal MP4, MOV, AVI or MKV files.

> A note for later: this manual does not yet describe how to physically mount
> the two cameras. That depends on your rig; the program only needs the overlap
> and a rigid mount.

## Organising the files: one folder per match

The easiest way to load a match is the **match folder** layout:

```
2026-10-04 Home - Away/        <- one folder per match, name it as you like
    Left/                      <- every video file of the LEFT camera
        DJI_0028_D_L01.MP4
        DJI_0029_D_L01.MP4     <- later parts of the same recording
    Right/                     <- every video file of the RIGHT camera
        DJI_0028_D_R01.MP4
        DJI_0029_D_R01.MP4
```

Rules the program follows:

- The two subfolders must be called **Left** and **Right** (capital letters do
  not matter).
- Recognised video types are `mp4`, `mov`, `avi` and `mkv`.
- Several files in one folder are treated as one recording, in file-name order.
- The setup file for the match is stored **in the match folder itself**. The
  program uses any `.json` in that folder with `setup` in its name. If there is
  none, it is created named after the folder with `_setup` added, for example
  `2026-10-04 Home - Away_setup.json`. Characters Windows does not allow in file
  names are replaced by a dash. Older `..._calibration.json` files are still
  found.
- If you logged the match with the Match Logger (see
  [chapter 6](06-cuts-scoreboard-match-logger.md)), put its export file (a
  `.json`) directly in the match folder. The program finds it by its content, so
  the file name does not matter. If there are several, the most recently saved
  one is used.

## Loading a match

### With the match folder (recommended)

1. In the left panel click **Select Match Folder...** and choose the match
   folder.
2. The program fills in the left video, the right video and the setup in
   one go:
   - If the folder already has a setup, it is loaded.
   - If not, and you have set a **Default setup** in Preferences, that file
     is copied into the match folder as the starting point. You are then asked
     whether to **detect the sync offset** for this match, because camera start
     times differ from recording to recording ("Detect & Save").
   - If neither exists, click **Auto Calibrate** ([chapter 3](03-calibration.md)).
3. If a Match Logger export is in the folder it is loaded too.
4. When you open the Export dialog, the output file is suggested inside the
   match folder.

### By hand

Use the **+** buttons next to *Left* and *Right* to add the videos, and **Load...**
next to *Setup* to open an existing setup file. **Clear** removes a
video; the small **x** next to the setup path unloads it. **Recent...**
lists matches you worked on before.

## The preview and the timeline

Once both videos and a setup are loaded the stitched picture appears.
Use the timeline and play controls under the preview to move around, play and
change the playback speed (click the speed box to type an exact value). You can
look around and zoom in the preview; the **Reset** button in the preview
returns to the default view.

The preview uses the same stitching as the export, so what you see is what you
get, with two exceptions that only appear in the preview: the red *seam line* and
the *Overlap view* (both are calibration aids, see
[chapter 3](03-calibration.md)).

Continue with [chapter 3: calibration](03-calibration.md).
