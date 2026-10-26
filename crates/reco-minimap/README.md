# reco-minimap

Draws a top-down pitch minimap (players, referee, ball) in a corner of an
export, from the same AI detections that drive the camera. It is off unless
you switch it on.

The tracking and drawing core is vendored from
[wendibus/reco-minimap](https://github.com/wendibus/reco-minimap) (AGPL-3.0,
commit `5e96fa28`), without its `tools` layer (browser UI, ONNX inference,
ffmpeg I/O, jersey OCR). `src/reco.rs` connects that core to a stitch session.

## Turning it on

- **GUI:** Export dialog -> "Pitch minimap (needs AI Tracking + minimap.json)".
  The export needs AI Tracking in Field or Ball mode with a model, and a
  `minimap.json` in the same folder as the calibration file. If either is
  missing, the export does not start and the dialog says why.
- **CLI:** `reco stitch ... --model <model> --minimap minimap.json`.

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
  "placement": {
    "corner": "bottom_right",
    "width_fraction": 0.25,
    "margin_fraction": 0.02,
    "background_opacity": 0.75
  }
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
- `placement.corner`: `top_left`, `top_right`, `bottom_left` or
  `bottom_right`. `width_fraction` is the minimap width as a share of the
  output width.

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
4. The vendored tracker (Kalman + Hungarian matching, fixed slots, ball
   state machine) smooths the positions. The renderer draws an RGBA image
   at exactly the size it is shown at. `MinimapOverlay` hands that image to
   the overlay compositor.

The minimap is redrawn only when new detections arrive, not on every frame.

## Limits

- **No teams.** reco's detections carry no shirt colour, so every player is
  one white dot (see `FRICTION.md`).
- **The referee** is yellow only when the model detects the `referee`
  class. Otherwise referees are drawn as players.
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
