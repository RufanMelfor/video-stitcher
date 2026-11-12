# 8. When something goes wrong

Find your problem in the table. Each line says what is probably going on and
where to look. If nothing here helps, see *Getting help* at the end.

## The program does not start or cannot open files

| What you see | What is going on | What to do |
|---|---|---|
| A message that `avformat-61.dll` (or another FFmpeg `.dll`) **was not found** | The FFmpeg DLLs must sit **next to** `reco-panopitch.exe`, also when you run a build you made yourself. | Copy all FFmpeg DLLs from the FFmpeg `bin` folder (or from a release package) into the same folder as `reco-panopitch.exe`. |
| A video will not load | Unsupported type, or the folder layout is wrong. | Use `mp4`, `mov`, `avi` or `mkv`. For a match folder you need `Left` and `Right` subfolders with the videos in them ([chapter 2](02-cameras-and-footage.md)). The message says which folder is missing or empty. |
| The two videos are refused | The cameras recorded at different frame rates. | Record both cameras at the same frame rate. A difference of up to half a frame per second (such as 29.97 and 30) is accepted. |
| *"calibration is no longer supported"*, or an error when loading a setup | The file was made by another version of the program or by the original Reco. | Run **Auto Calibrate** again. Setup files are not guaranteed to work between this program and upstream Reco. |

## The picture does not look right

| What you see | Look at |
|---|---|
| A visible line or step where the cameras meet | [Chapter 3](03-calibration.md): check with Overlap view, fine-tune Roll, Skew and Seam position. |
| Things look doubled near the seam while they move | The cameras are out of sync. Check the *Audio Sync Waveform* and *Detect Sync Offset* ([chapter 3](03-calibration.md#step-5-check-the-sync)). |
| One half is brighter or has other colours | [Chapter 4](04-colour-and-sharpness.md): Auto color match, then Gamma. If it changes during the match, try *Auto gamma*. |
| A black gap or wedge in the picture | You may have pushed **Intersect** too far; move the seam with *Seam position* instead. For the AI camera, set the **AI pitch limits** so it does not aim into the wedge ([chapter 3](03-calibration.md#detection-zones-field-area-goals-and-ai-limits)). |
| Lines at the edge of the panorama lean | **Rig tilt** and **Rig roll** in *Rig alignment*. |
| The picture is soft after zooming in | The AI camera is zoomed in further than the lens resolves. Above roughly 1.5 times upscaling the picture looks soft whatever the encoder setting is. Widen the tightest zoom (raise the *Tight* value of the field of view in the Advanced panner) or use **Sharpening** a little. |

## The AI camera misbehaves

The [tuning guide](../ai-panner-tuning.md#fix-a-symptom) has a full table. The
most common ones:

| What you see | Try |
|---|---|
| The camera loses the ball for long stretches | **Ball height limit** first. A value copied from another rig can throw away most real detections. Set it to 0 (off), then measure it for your own rig. Use an **AI Debug export** to see what the AI saw. |
| It follows a warm-up or a neighbouring pitch | **Player height limit** and **Ball height limit**, **Ball acquire range** ([tuning guide](../ai-panner-tuning.md)). |
| It jumps between two balls | **Ball max speed** and **Ball jump confidence**. |
| It drifts off the ball and swings back | Raise **Ball coast time** and **Ball hold time**. |
| It freezes on a group of players during open play | Switch **Cluster mode** to *Trimmed mean* and lower the **Dead zone**. |
| It wobbles | Lower **Ball weight**, raise the **Dead zone** in small steps, or raise the **Lookahead**. |
| It does not follow a breakaway into a corner | Raise **Ball reach** and the wide limit of the field of view. |
| Weak detections yank the camera | Raise **Detection confidence**. |
| The AI finds nothing at all after you changed model | See *TensorRT cache* below. |

## Export problems

| What you see | What is going on | What to do |
|---|---|---|
| *"not enough VRAM for a ... lookahead"* | The lookahead buffer does not fit in your graphics memory. | Lower the **Lookahead** (the message gives the maximum), tick **Reduce lookahead memory (8-bit)**, use lower-resolution footage, or close other programs that use the graphics card. |
| The export crashes at the start on 10-bit footage | Known problem with the lookahead buffer at 10-bit. | Keep **Reduce lookahead memory (8-bit)** switched on, or set the lookahead to 0. |
| The export is slow | AI detection is the bottleneck. | Raise **Detect every N frames**, switch on **Async AI detection**, lower the resolution or choose *Fast* quality. |
| The first AI export with a new model takes several minutes to start (TensorRT builds only) | The program builds an optimised engine for your GPU once. | Wait. Later exports start quickly. |
| The encoder is not the one you expected | A hardware encoder could not be opened, so the program used software. | Look in the Debug log or the export's `.log` file. |
| The scoreboard does not appear, or *"Chrome or Chromium was not found"* | The scoreboard is drawn by a browser. | Install Google Chrome or Chromium. For a non-standard location set the `CHROME` environment variable to the program file. |

## TensorRT cache (TensorRT builds only)

TensorRT keeps its optimised engines in a `trt-cache` folder under
`%LOCALAPPDATA%\panopitch\`. Two models with the same structure but a different
input size can confuse it. If the AI suddenly finds **nothing** after you switched
to a model with a different image size, close the program, delete the files in
that `trt-cache` folder and try again. The engine is rebuilt (this takes a few
minutes once).

## Where to find logs

- **Debug** button in the top bar opens a window with the log.
- On Windows the program also writes **`reco-panopitch.log`** next to the program file.
- Every export writes a **`.log` file** next to the video with the same name.
- The settings folder is `%APPDATA%\panopitch\`. If you want to reset the
  program to a clean state, close it and rename that folder.

## Getting help

1. Note what you did, the exact error message, and your graphics card and driver.
2. Open the **Debug** window and copy the lines around the problem. The
   `reco-panopitch.log` file (see above) holds the full log; neither contains video
   content.
3. Open a new issue in this project's GitHub repository and paste what you
   collected. Attach the export's `.log` file if there is one.

Remember that only **Windows 11 with an NVIDIA GeForce RTX 3060 Ti** is tested.
Problems on other setups are welcome as reports, but may not be fixable.
