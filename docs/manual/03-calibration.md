# 3. Calibration

Calibration tells the program exactly how your two cameras sit relative to each
other, so their pictures join into one without a visible seam. You usually do it
once per rig setup and then reuse it.

The result is stored in a small setup file (`.json`). It holds the alignment, the
lens data, the colour settings you chose and, if you set them, your AI defaults,
export defaults and scoreboard settings.

## The short version

1. Load the two videos (or a match folder, see [chapter 2](02-cameras-and-footage.md)).
2. Click **Auto Calibrate** and wait.
3. Look at the result with **Overlap view**. If it is good, click **Save
   Setup** and you are done.
4. If something is slightly off, fine-tune with the sliders described below, and
   save again.

## Step 1: Auto Calibrate

In the left panel, under *Setup*, click **Auto Calibrate**. After a
setup exists the same button reads **Re-calibrate**. While it runs the
button shows *Calibrating...* and the Debug log shows progress.

What the program does:

1. **Finds the time difference** between the two videos (the sync offset). It
   first tries the cameras' motion-sensor data and falls back to comparing the
   **sound**.
2. **Identifies the lens** from its built-in database of lens profiles, so the
   fisheye distortion can be corrected.
3. **Finds matching points** in both pictures (AKAZE feature matching) and
   solves for the alignment that makes them coincide.
4. **Refines the alignment on the pixels** along the seam: it compares what
   both cameras show there (lines, fences, advertising boards, players) and
   fine-tunes the alignment until they line up. Matching points are found
   mostly in the background, so this step is what removes steps in pitch lines
   and doubled players close to the cameras. It adds about two minutes and can
   be switched off under *Advanced* (*Refine alignment on pixels*).
5. **Levels the rig**: sets *Rig tilt* and *Rig roll* from vertical structures
   in the picture (light masts, goal posts, fence posts, buildings, standing
   players), which in reality all stand straight up. This replaces the
   cameras' motion-sensor orientation, which is not reliable on every camera.
   If too few vertical structures are in view, tilt and roll stay as they were.
   Can be switched off under *Advanced* (*Level the rig from vertical
   structures*).

When it finishes, a short line under the buttons tells you whether both cameras
carried usable motion-sensor data, or whether the program had to rely on audio
only. With audio only the calibration still works, but the program had less to go
on. Statistics such as the confidence and the number of matches are shown in the
**Stats** section.

> **Tip:** the calibration works best when the pictures contain plenty of detail
> in the overlapping area. If a result is poor, see *Choosing the moment* below.

<!-- screenshot: Files panel with the Auto Calibrate button and the status line -->

## Step 2: Check the result

The most useful check is the **Overlap view** (*Seam* section, preview only):

- **Right over left** or **Left over right** shows one camera faintly on top of
  the other. Where the calibration is right, lines and goals coincide. Where it
  is wrong you see them twice.
- **Difference** shows how much the two cameras differ wherever both see the same
  spot. Nearly black means aligned. Bright edges mean misaligned.
- **Both** shows both ghosts at once. Lines that cross the seam at an angle point
  to a **roll** error.
- **Overlap opacity** sets how strongly the ghost shows.

Judge on things that are **far away**, such as the far touchline and the goals.
Close to the cameras some doubling is normal because the cameras are not in the
same place (parallax).

Also switch on **Show seam line**. It draws a thin red line at the exact place
where the two pictures meet. This is an aid only: it never appears in an export.

<!-- screenshot: Overlap view in Difference mode, aligned and misaligned -->

## Step 3: Place the seam, then Refine Alignment

Because of parallax, no alignment fits every distance everywhere in the
overlap: the alignment is fine-tuned for where the seam is. Auto Calibrate tunes
it for the default seam position. When you move the seam (**Seam position**,
or drag the red seam line), click **Refine Alignment** under *Setup*
afterwards. It fine-tunes the alignment for the new seam position, using the
anchor frame (or the current frame) and three more frames 5 seconds apart, so
pick a moment with players near the seam. It takes about two minutes and can be
cancelled. Click **Save Setup** to keep the result.

## Step 4: Fine-tune by hand

Everything below is optional. Change one slider at a time and look at Overlap
view after each change. The sliders under **Calibration** are grouped per camera.

| Problem you see | What to try |
|---|---|
| Lines crossing the seam are turned against each other | **Roll** of the left or right camera |
| The two pictures still look skewed against each other | **Skew**; or tick *Force extra skew fit* under *Advanced* and re-calibrate |
| A step in ground lines close to the cameras | **Near-field tilt** (affects the bottom of the picture only) |
| A step in distant things (goal frames, skyline) | **Far-field tilt** (affects the top of the picture only, manual only) |
| The picture is shifted vertically between the cameras | **Vertical shift** (right camera) |
| The vertical lines at the panorama edges lean | **Rig tilt** in *Rig alignment* |
| The whole rig is tilted sideways | **Rig roll** in *Rig alignment* |
| The seam is in an awkward place (for example through a player or the goal) | **Seam position**: moves the seam left or right independent of the calibration. You can also drag the red seam line on the preview |

The **Intersect** and **Camera axis offset** sliders (under *Global parameters*)
change how the two camera planes meet. Use them with care: pushing *Intersect*
too far can open a **black gap**. *Seam position* is the safe way to move the
seam.

The reset buttons next to the sliders return a value to its default.

Every one of these settings is explained in the
[settings reference](settings-reference.md).

## Step 5: Check the sync

If the two videos are not exactly in step, players and the ball will look
doubled near the seam while things move. Two tools help:

- **Audio Sync Waveform** (right panel): shows the sound of the left camera in
  blue and the right camera in orange around the playhead. **Aligned peaks mean
  the cameras are in sync.** You can change the number of frames shown and the
  height of the display.
- **Sync offset** (*Rig alignment*, in frames): type a number and click **Apply**
  to set it by hand, or click **Detect Sync Offset** to measure it again without
  running a full calibration. This takes a minute or two on footage with a lot of
  sensor data. It is the quick way to fix the sync after you re-cut footage.

## Saving

When you change a calibration value a **Save Setup** button appears. Save
when you are happy. Everything in the setup (alignment, lens, colour
settings, AI defaults, export defaults, scoreboard settings) is stored in the one
file and restored when you load it again.

## Info

The **Info** button next to *Refine Alignment* opens a report about the
footage and the rig:

- per camera: the files and segments, duration and size, video codec,
  resolution, frame rate, bit depth and bitrate, the audio track, the recording
  time and the camera that wrote the file;
- the lens profile and the lens's field of view (horizontal, vertical and
  diagonal, in degrees);
- with a setup loaded: the sync offset, the angle between the two cameras, the
  **overlap in degrees** (the part of the scene both cameras see, at the
  horizon), the total panorama width and where the seam lies within the
  overlap;
- the rig's physical lean: how far it points down (tilt) and leans sideways
  (roll), worked out from the setup's *Rig tilt* and *Rig roll*.

The rig angles are real camera angles: they are worked out from the points the
setup says both cameras see and the lens profiles. On a rig whose cameras are
84.0 degrees apart in the CAD drawing, the report shows 84.4 to 84.5 degrees, so
expect them to be accurate to about half a degree. A setup with a poor alignment
gives poorer angles. **Copy to clipboard** copies the whole report, for example
to share it in the GitHub Discussions.

## Choosing the moment

Calibration uses frames spread over the video. If the automatic result is poor,
the Advanced settings (right panel, *Calibration > Advanced*) let you point it at
a better moment:

- **Anchor frame** and **Sample window:** keep all sample frames within a given
  number of seconds of one frame (or of the current playback position if the
  anchor is empty). Choose a moment with a lot of texture on the pitch, such as
  people or cones. A window of 0 means *off*.
- **AKAZE threshold:** how sensitive the feature detector is. Lower finds more,
  weaker features.
- **Detect Y min / Detect Y max:** the part of the picture, top to bottom, where
  features are searched.
- **Full-resolution features:** more precise on 4K footage, but slower.
- **Refine alignment on pixels:** Auto Calibrate step 4 (see *Step 1*).
  On by default.
- **Level the rig from vertical structures:** Auto Calibrate step 5 (see
  *Step 1*). On by default.
- **Force extra skew fit:** solves the skew of one camera even when no motion
  data suggests it.

## Lens

The **Lens** section shows the lens the program found. The **Lens correction**
and **Lens preview (single camera)** options show one camera's own picture with
its distortion handled, which is also where you draw the detection zones (below).
If the automatic lens is wrong, use **Browse profiles...** to pick another lens
profile or load one from a file. The individual lens values (focal length,
optical centre, distortion coefficients k1 to k4) are filled from the profile
and normally do not need touching.

## Detection zones: field area, goals and AI limits

These settings tell the AI where the pitch is, so people and balls outside it are
ignored. They are in *Rig alignment > Detection zones* and are part of the
setup.

- **Field ROI** (the playing area): click **Edit ROI / GOAL...** to open the lens
  preview. Click to add points, drag a point to move it, right-click a point to
  delete it. Use **Left** and **Right** to edit the other camera. **Paste ROI**
  accepts a field area copied from another setup. **If you export with AI
  tracking and no field ROI, the program warns you**, because without it the
  camera may frame badly.
- **Goal geometry:** the same editor, in goal mode. A goal needs at least 4
  points so that both width and height are defined.
- **AI pitch limits:** tick **Show AI pitch limits** to draw two draggable lines
  on the stitched preview. They mark how far up and down the AI camera may aim, a
  manual safety margin that keeps it away from the black wedge near the seam.
  They only affect AI tracking, not your own panning. **Clear** removes them.

<!-- screenshot: the lens preview with the ROI polygon -->

## Re-using a setup

A setup stays valid as long as the two cameras do not move relative to each
other and you keep the same lenses and settings. So you can reuse it for every
match you record with the same rig. Two things differ per recording and are
handled for you:

- **The sync offset** (the cameras never start at exactly the same moment). When
  you start a match from a *Default setup* the program offers to detect it
  for you: **Detect & Save**.
- **Colour** depends on the light of that day, so check
  [chapter 4](04-colour-and-sharpness.md) for each match.

To set it up, calibrate a good match, save the setup, and in *Preferences*
choose it as your **Default setup**. For a new match folder without a
setup the program then copies it in as the starting point. Because other
sessions fall back to the default, saving over it asks for confirmation first.

If you bump the rig, change the mounting or switch lenses, calibrate again.

Continue with [chapter 4: colour and sharpness](04-colour-and-sharpness.md).
