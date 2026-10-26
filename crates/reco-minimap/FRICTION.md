# reco-minimap friction log

## Detections carry no pixel colour, so players cannot be split into teams

**Symptom:** the minimap draws every player in one colour.

**Cause:** the vendored tracker assigns teams from a torso colour sample per
detection (`detect::Det::color`). `reco_core::detect::detector::Detection` and
`MappedDetection` have no colour field. In an export, the detector sees GPU
frames (`WgpuNv12`, `Cuda`, `PreprocessedChw`), so a consumer cannot sample
pixels after the fact either.

**Current handling:** `reco::MinimapEngine` gives every player the same
placeholder colour and one tracker group of `2 * players_per_team` slots.
There is no consumer-side pixel readback.

**What would fix it:** an optional per-detection colour sample (e.g. mean
torso RGB) filled in by the detector backends from the frame they already
hold.

## The session does not expose the model's class labels

**Symptom:** a consumer that interprets raw `class_id`s next to an autocam
session (the minimap needs player / ball / referee) cannot ask the session
which labels its detector resolved.

**Current handling:** `reco_autocam::read_model_class_names(model_path)`
reads them the same way `setup_autocam` does. This loads the ONNX metadata
a second time (once per export).

## No GUI editor for the field points

**Symptom:** `minimap.json` has to be written by hand, or copied from the
calibration's `field_roi` when the ROI was drawn exactly on the corners.

**What would fix it:** a "minimap corners" mode in the lens-preview window
that already edits the field ROI and goal geometry, saving the points (and
field size) with the calibration.
