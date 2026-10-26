# reco-minimap friction log

## Shirt colours only from the wgpu detector path

**Symptom:** with a detector backend other than `WgpuPreprocessingDetector`
(CPU decode, native CUDA/TensorRT, NCNN, Metal), `teams: auto` finds no
colour samples and draws every player in one colour.

**Cause:** `Detection::color` (added for the minimap) is filled only by
`reco_autocam::WgpuPreprocessingDetector`. Only there does the session hand
the detector a CPU-readable RGB tensor; the other paths keep pixels on the
GPU or inside the backend.

**What would fix it:** each backend sampling the same torso band from the
frame it already holds.

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
