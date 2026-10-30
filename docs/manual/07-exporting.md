# 7. Exporting

Exporting renders the finished video: the stitched panorama, with the AI camera
if you enabled it, the colour settings, the cuts and the scoreboard. Click
**Export...** in the top bar to open the Export dialog.

<!-- screenshot: Export dialog, Export tab -->

## The Export tab

| Setting | What to do |
|---|---|
| **Output file** | Where to save. You can type a path or browse. Whatever is in the box when you start the export is used exactly. For a match folder a path inside that folder is suggested. |
| **Resolution** | The size of the finished video. It does not depend on the camera resolution. |
| **Codec** | H.264, HEVC or AV1, whichever your machine can encode. If a hardware encoder cannot be opened, the program falls back to software and the debug log says which encoder it used. |
| **Quality** | *Fast*, *Balanced* (default) or *High*. The numbers show the target and peak bitrate of the NVIDIA hardware encoder for the chosen codec and resolution. Software fallback encoders use a quality target instead and have no fixed bitrate. |
| **Processing range** (*Start* / *End*) | Export only part of the clip, in seconds from the start. The default is the whole clip. |
| **Cut ranges** | Remove stretches from the middle ([chapter 6](06-cuts-scoreboard-match-logger.md)). |
| **PAUZE overlay on cuts**, **Fade**, **Black** | The dip-to-black at each cut ([chapter 6](06-cuts-scoreboard-match-logger.md)). |
| **Record replay** | Also saves the raw, pre-stitch source frames in a separate file next to the export, so you can re-stitch later without decoding the originals again. |
| **Save AI debug events** | Writes every detection and camera decision to a `.jsonl` file next to the export, for analysis. |

Under the *AI Tracking* heading you find the AI settings
([chapter 5](05-ai-tracking.md)).

Click **Start Export**. The top bar shows the progress and a **Cancel** button.
When it is done, **Show in folder** opens the file's location. The preview is
paused while exporting.

If the setup has no field ROI and AI tracking is on, you are asked to
confirm first: *Export Anyway* continues, or you can set the field area first
(see [chapter 3](03-calibration.md#detection-zones-field-area-goals-and-ai-limits)).

## Choosing quality

- Use **Fast** for test exports while you tune settings.
- Use **Balanced** for normal matches.
- Use **High** for the file you keep or upload.

Better quality means a larger file and a slower export. Your preferred defaults
can be set in *Preferences*, and they are also saved in the setup.

## Speed and video memory

An export with AI tracking is heavy on the graphics card. As a reference, on the
tested RTX 3060 Ti a 300-second clip took about 4 minutes 47 seconds with async
detection on, which is roughly 31 frames per second. Your speed will differ.

What makes it faster:

| Setting | Effect |
|---|---|
| **Detect every N frames** | Higher means the AI looks less often. The biggest lever. |
| **Async AI detection** | Runs the AI on a separate thread. About 1.24 times faster in the benchmark. Needs about 400 MB of extra video memory and lookahead above 0. |
| A lower **Resolution** or a faster **Quality** | Less to render and encode. |
| Closing other programs that use the graphics card | More memory for the export. |

## Lookahead and video memory

**Lookahead** buffers a number of seconds of future frames so the AI camera can
move smoothly. It is stored in the video memory of your graphics card, so a long
lookahead on 4K footage needs a lot of it.

- If the export stops with *"not enough VRAM for a ... lookahead"*, **reduce the
  lookahead**, use **Reduce lookahead memory (8-bit)**, use lower-resolution
  footage, or free up graphics memory. The message tells you the maximum that
  fits.
- **Reduce lookahead memory (8-bit)** stores the buffer in 8-bit instead of the
  source's 10-bit, which roughly halves its memory use. For **10-bit footage
  (such as DJI Osmo Action 4 HEVC) leave it switched on**: with it off the export
  currently crashes because of a known graphics-library problem.
- A lookahead of 0 switches the buffer off. The AI camera is then less smooth.

## What an export also writes

- A **`.log` file** with the same name next to the video. It starts with the
  settings you requested and then records everything the program did. Keep it if
  you ever need to report a problem.
- **Settings stored inside the video file** as metadata, so you can see later
  how it was made.

## Command line

Everything can also be done with the `reco` command, which is useful for
batches. For example:

```
reco stitch left.mp4 right.mp4 -c match.json -o out.mp4 --model yolo.onnx --tracking field --panner-preset action
```

`reco stitch --help` lists all options, such as `--lookahead`,
`--cut-range` and `--async-detect`. A setup made in the GUI can be used
directly with `-c`.

Continue with [chapter 8: when something goes wrong](08-troubleshooting.md).
