# reco-minimap

Draws a top-down pitch minimap (players in their team colour, referee,
ball) in a corner of an export, from the same AI detections that drive the
camera. It is off unless you switch it on.

The tracking and drawing core is vendored from
[wendibus/reco-minimap](https://github.com/wendibus/reco-minimap) (AGPL-3.0,
commit `5e96fa28`), without its `tools` layer (browser UI, ONNX inference,
ffmpeg I/O, jersey OCR). `src/reco.rs` connects that core to a stitch session.

## Turning it on

- **GUI:** Export dialog -> "Pitch minimap (needs AI Tracking + minimap.json)".
  The export needs AI Tracking in Field or Ball mode with a model, and a
  `minimap.json` in the same folder as the calibration file. If either is
  missing, the export does not start and the dialog says why. Below the
  checkbox: corner, size and gap to the edge. These are remembered between
  sessions.
- **CLI:** `reco stitch ... --model <model> --minimap minimap.json`, with
  `--minimap-corner top_left|top_right|bottom_left|bottom_right` (default
  `bottom_right`), `--minimap-width 0.25` (share of the video width,
  0.05-0.6) and `--minimap-margin 0.02` (share of the video height, 0-0.2).

Where the minimap goes is an export setting. Everything about the match
itself (field, field points, teams) is in `minimap.json`.

## `minimap.json`

Every field is required.

```json
{
  "version": 1,
  "field_length_m": 64.0,
  "field_width_m": 42.5,
  "players_per_team": 8,
  "referees": 1,
  "points": [
    { "camera": "left",  "x": 0.5097, "y": 0.3344, "field_x": 0.0,  "field_y": 0.0  },
    { "camera": "left",  "x": 0.2045, "y": 0.5569, "field_x": 0.0,  "field_y": 42.5 },
    { "camera": "right", "x": 0.4795, "y": 0.3222, "field_x": 64.0, "field_y": 0.0  },
    { "camera": "right", "x": 0.7855, "y": 0.5432, "field_x": 64.0, "field_y": 42.5 }
  ],
  "teams": { "mode": "auto" }
}
```

- `points`: at least 4 known field points that are not all on one line,
  usually the four corners. `x`/`y` are positions in that camera's raw
  frame, normalized 0..1. This is the same space as the calibration's
  `field_roi`, so a field ROI drawn exactly on the corners gives you these
  numbers directly. `field_x` runs from the left goal line (0) to the right
  one (`field_length_m`). `field_y` runs from the far sideline (0) to the
  near sideline on the camera side (`field_width_m`). With more than 4
  points, the fit also reports how well they agree.
- `players_per_team` / `referees`: slot counts for the tracker.
- `teams` sets how players are split into teams:
  - `{ "mode": "auto" }` learns the two shirt colours from the first
    seconds of the export.
  - `{ "mode": "colors", "home": "#f06020", "away": "#2040c0" }` uses fixed
    colours. The dots are then drawn in exactly these colours.
  - `{ "mode": "off" }` draws every player in one colour.
- `referee_color` / `ball_color` (optional, `#rrggbb`) set the referee and
  ball dot colours. Without them, the defaults are `#facd00` (yellow) and
  `#ff7800` (orange).
- `show_referees` (optional, default `true`): `false` hides the referee
  dots. That covers the model's `referee` class and anyone whose colour is
  neither team's, such as a coach. They are still tracked, so they never
  take a player's place.

To check a setup without exporting, run:

```bash
cargo run -p reco-minimap --example minimap_fit -- Calibration.json minimap.json out_dir
```

It prints each point's residual and writes `out_dir/fit.ppm`, which shows
where the fit puts every point on the field.

## How it works

1. The session emits each frame's detections (`PipelineEvent::DetectionsRaw`)
   right before it renders that frame. `MinimapSink` receives them.
2. Each detection's foot point (bottom centre of its box) goes through
   `reco_core::projection::camera_to_panorama`. The ray is then made level
   with the rig tilt/roll (`reco_core::geometry::rig_frame`) and
   intersected with the plane below the camera.
3. A homography fitted to `points` maps that plane to field metres.
4. Teams: the wgpu detector wrapper (`reco-autocam`) stores each box's
   torso colour in `Detection::color`. It takes the median of the shirt
   band, read from the same RGB tensor the model saw, so there is no extra
   frame readback. In `auto` mode the first ~10 detection batches with at
   least 150 on-field samples are held back. Their colours are split into
   two teams (2-means in Lab), and the held batches are then replayed.
   After that, each player gets the nearer shirt colour. A colour about
   equally far from both shirts gets no team. A colour far from both
   (e.g. a coach or referee in plain clothes) counts as a referee.
5. The vendored tracker (Kalman + Hungarian matching, fixed slots, ball
   state machine) smooths the positions. The renderer draws an RGBA image
   at exactly the size it is shown at. `MinimapOverlay` hands that image to
   the overlay compositor.

The minimap is redrawn only when new detections arrive, not on every frame.

## Limits

- **Team colours are measured colours.** Shirts in shade or backlight are
  measured darker and greyer than they look. The minimap brightens them,
  but a dark navy kit can still come out light grey. Set
  `"mode": "colors"` for exact colours.
- **Colour samples come only from the wgpu detector path**, which is what
  the Windows TensorRT/DirectML exports use. With other backends (CPU
  decode, native CUDA/TensorRT, NCNN, Metal) `auto` finds no samples and
  falls back to one colour, with a warning in the log.
- **Goalkeepers** in a third colour are drawn as referees: no keeper kit
  colours are learned.
- **The referee** is yellow when the model detects the `referee` class or
  the colour is neither team's.
- **The ball** shows only while the tracker considers it in play. A ball in
  the air is projected onto the ground, so it appears further away than it
  is.
- **Accuracy** depends on the field points. On the test match (youth,
  4-corner fit), sideline points left out of the fit landed 0.7-1.9 m from
  the line.
- **Field markings** are the standard football markings scaled to the
  field size, not the real lines of a youth field.
- **Scoreboard combined:** with both the scoreboard and the minimap on,
  layers are composited on the CPU at full resolution whenever one of them
  changes. That can lower export speed (see
  `reco_core::render::overlay_layers`).
