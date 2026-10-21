# The most important settings

Out of well over a hundred settings, these are the ones that decide whether your
video looks right. They are in the order you meet them: line up the cameras, fix
the colour, set up the export, then let the AI steer.

Values marked **start with** are sensible first choices, not rules. A full list
of every setting is in the [settings reference](settings-reference.md).

> Change one thing at a time and look at the result before changing the next.
> Most "mystery" problems come from two settings pulling against each other.

---

## 1. Lining up the two cameras

### Seam blend

*Stitching panel > Seam* - range 0 to 0.3, starts at 0.05.

The seam is the line where the two camera pictures meet. Seam blend is how wide
a soft crossfade you want there. A wider blend hides small colour or brightness
differences. The price is that moving things inside the blend, such as the ball
or a player's legs, can look slightly doubled.

- **Start with** the default.
- **Change it** if you see a hard line or colour step at the seam that the colour
  settings (section 2) cannot remove.
- **Experimental alternative:** *Multi-band blend* blends wide for smooth areas
  and narrow for fine detail. It looks better but is slower (10 render passes
  instead of 1).

<!-- screenshot: Seam section with "Show seam line" switched on -->

### Overlap view

*Stitching panel > Seam* - preview only, never in your export.

The most useful aid for checking a calibration. It shows one camera
semi-transparently over the other where they overlap:

- **Right over left / Left over right:** where the calibration is good, lines
  and goals line up. Where it is off, you see them twice.
- **Difference:** nearly black means aligned; bright edges mean misaligned.
- **Both:** shows both ghosts at once. Lines that cross the seam at an angle
  point to a roll error.

Judge on things that are far away (far touchline, goals). Close to the cameras
some doubling is normal because the cameras sit apart (parallax).

<!-- screenshot: Overlap view in Difference mode, once aligned and once misaligned -->

### Roll (left camera and right camera)

*Calibration panel > Left camera / Right camera* - range -0.3 to 0.3, starts at 0.

Turns one camera's picture around its own viewing axis, like rotating a picture
frame. Automatic calibration already solves this. Use the slider to fine-tune
afterwards, watching a line that crosses the seam in *Overlap view*.

### Skew (left camera and right camera)

*Calibration panel > Left camera / Right camera* - range -0.3 to 0.3, starts at 0.

Tips one camera's plane forward or backward. On screen it looks like a keystone
or skew rather than a turn. If the two pictures still look rotated against each
other after calibrating, try this slider, or tick *Force extra skew fit* under
Advanced and calibrate again.

### Near-field tilt

*Calibration panel > Left camera / Right camera* - range -0.3 to 0.3, starts at 0.

Fixes a step in ground lines **close to the camera** that the two-plane model
cannot otherwise remove. It only affects the bottom of the picture. There is a
matching *Far-field tilt* for distant things such as goal frames and the
skyline, which only affects the top. Far-field tilt is manual only.

---

## 2. Colour and sharpness

<!-- screenshot: Color Mapping panel -->

### Auto color match

*Color Mapping panel* - on by default.

Measures the colour of each camera in a narrow band next to the seam and nudges
both toward their shared average, so an exposure or white-balance difference
does not show as a colour step. It works in the preview and in every export.

It shifts the average only. If the two cameras differ in **contrast**, use the
next setting.

### Gamma left and Gamma right

*Color Mapping panel > Manual gamma* - range 0.5 to 2.0, starts at 1.0.

Reshapes the mid-tones of one camera. Above 1.0 lifts them, below 1.0 lowers
them. Set the two against each other until both halves match in the mid-tones,
then let Auto color match take care of what is left. *Auto gamma (dynamic)*
keeps re-fitting them during the match; turn it off before you set a fixed value,
because it overwrites your numbers at the next measurement.

### Vivid

*Color Mapping panel > Vivid color grade* - off by default.

One switch that perks up grey, dull footage by raising brightness, saturation and
gamma to a tuned preset. The three sliders under it (1.0 means no change) let you
fine-tune. Saved with the calibration.

### Sharpen amount

*Color Mapping panel > Sharpening* - range 0 to 2, starts at 0 (off).

Sharpens the final picture, which helps when the AI camera zooms into a crop of
the panorama. Above about 1.5 you start to see bright halos around high-contrast
edges. Radius controls how coarse the sharpened detail is.

---

## 3. Exporting

<!-- screenshot: Export dialog, Export tab -->

### Resolution, Codec and Quality

*Export dialog > Export tab*

- **Resolution** is independent of the camera resolution.
- **Codec:** H.264, HEVC or AV1, depending on what your machine can encode. If a
  hardware encoder cannot be opened, the app falls back to software and tells
  you in the debug log which one it used.
- **Quality:** *Fast*, *Balanced* (default) or *High*. The numbers next to each
  are the target and peak bitrate the NVIDIA hardware encoder uses. Higher is a
  bigger file and a slower export. Use *Fast* for test exports and *High* for the
  final file.

### Cut ranges

*Export dialog > Export tab, and the timeline*

Removes a stretch of time from the export and joins what remains with no gap:
useful for injuries, half-time, or warm-up. Drag the reddish band on the
scrubber, or press **+ Add cut** and type the numbers. With *PAUZE overlay on
cuts* switched on, each cut shows a short "PAUZE" dip to black instead of an
instant jump. The audio follows the cuts.

<!-- screenshot: cut ranges on the timeline -->

### Reduce lookahead memory (8-bit)

*Export dialog > AI Tracking*

Stores the buffered frames in 8-bit instead of the source's 10-bit, which
roughly halves the video memory they need. **Leave it on for 10-bit footage**
(such as DJI Osmo Action 4 HEVC): with it off, the export currently crashes on
a known graphics-library problem.

### Async AI detection

*Export dialog > AI Tracking* - off by default.

Runs the AI on a background thread so it no longer holds up the export. On the
reference machine (RTX 3060 Ti) it made a 300-second export about 1.24 times
faster. It needs about 400 MB of extra video memory and only works when
lookahead is above 0. Turn it on if your card has memory to spare.

---

## 4. AI tracking

<!-- screenshot: Export dialog, AI Tracking section -->

The AI tracking options are explained in depth in the
[AI tracking and panner tuning guide](../ai-panner-tuning.md). The few that
matter most are below.

### Enable, and the AI model

*Export dialog > AI Tracking* - off by default.

Turns AI camera tracking on for this export. It needs a YOLO model file (ONNX).
Set a default model once in *Preferences* so you do not have to browse for it
every time.

### Tracking mode

*Export dialog > AI Tracking*

- **Field** (recommended): follows the group of players and the ball together.
- **Ball:** follows only the ball.
- **Sweep:** a fixed left-right pan with no AI at all, useful as a baseline.

### Style preset and Framing

*Export dialog > AI Tracking*

A preset (such as *action*) sets all framing sliders to tuned values in one go.
You can change any slider afterwards. **Framing** chooses between *Action*
(follows the players, pulled toward the ball, with dynamic zoom) and *Frame all*
(centres on all players and zooms to fit them).

### Detect every N frames

*Export dialog > AI Tracking* - start with 3.

The AI looks at every Nth frame; the frames in between reuse the last result.
A higher number is much faster but follows quick action a little late. This is
the biggest speed lever for AI exports.

### Detection confidence

*Export dialog > AI Tracking* - range 0.05 to 0.9, starts at 0.10.

Detections scored below this never reach the tracker. **Raise** it if a weak,
noisy detection (a "ball" on background clutter) keeps pulling the camera.
**Lower** it if real but faint detections are missed.

### Lookahead

*Export dialog > AI Tracking* - range 0 to 2.5 seconds; the tuning guide
recommends starting at 0.5.

Buffers future frames so the camera can lead the action and move smoothly
instead of reacting to every frame. It is the most important lever for smooth
panning. 0 turns it off. More lookahead needs more video memory.

### Ball coast time and Ball hold time

*Export dialog > AI Tracking* - coast starts at 0.67 s, hold at 0 s; the tuning
guide suggests 8 s for both as a starting point.

When the AI loses the ball, **coast time** is how long the tracker keeps
reporting its last position, and **hold time** is how long the camera stays
pointed there afterwards. A lost ball is usually just briefly hidden and
reappears close to where it vanished, so holding still looks calmer than
swinging away and back.

### Cluster mode, Dead zone and Ball weight

*Export dialog > AI Tracking > Advanced panner*

These three decide how the camera behaves when nothing dramatic happens:

- **Cluster mode:** *Density* follows the tightest knot of players. *Trimmed
  mean* averages all players. If the camera gets stuck on a static group during
  open play, try *Trimmed mean*.
- **Dead zone:** the camera stays still while the target is inside this radius.
  Larger values remove wobble; too large and the camera stays frozen too long.
- **Ball weight:** how strongly the ball pulls the aim away from the players.
  A very high value makes the camera jumpy.

Raising dead zone fixes wobble but brings the freeze back, so adjust it in small
steps.

### Ball height limit

*Export dialog > AI Tracking* - off (0) by default.

Ignores balls that sit higher in the picture than your own pitch reaches, which
filters out balls being played on a neighbouring field. **Never copy this value
between cameras or locations.** The right value depends on how your camera is
mounted, and a value from another setup can silently throw away most of your real
ball detections. Measure it per setup, as described in the
[tuning guide](../ai-panner-tuning.md#ball-height-limit-measure-it-per-rig).

---

## Where to go next

- Something looks wrong with the camera movement: the **symptom table** in the
  [tuning guide](../ai-panner-tuning.md#fix-a-symptom).
- Looking for any other setting: the [settings reference](settings-reference.md).
