# reco-io friction log

## AI Debug raw-camera export decodes-and-discards every frame up to `start_secs` instead of seeking

**Symptom (found 2026-09-20):** the AI Debug tab's raw-camera export
(`raw_camera_debug::run`) shows no frame-count progress for a long,
apparently-stalled period right after "AI debug raw export starting...",
before the first `{camera} pass: frame N (...)` status update ever
appears - long enough to look like a hang, especially with a late
`start_secs`. GPU/CPU activity during that period confirms the process
is genuinely working, not stuck.

**Root cause:** `raw_camera_debug::run_one_camera`'s start-time skip
(`crates/reco-io/src/raw_camera_debug.rs`, `FrameSource::skip_one_frame`)
calls `decoder.next_frame()` / `next_frame_d3d11()` in a loop, once per
frame to skip - a real decode-and-discard of every single frame between
0 and `start_secs`, not a seek. The `on_progress` callback (which drives
the GUI's frame counter) is only invoked once the main per-frame loop
starts, *after* this skip loop finishes - so the entire skip duration is
silent in the UI.

This is a real, disproportionate cost: at `start_secs=500` and 30fps,
that is 15,000 frames individually decoded before anything visible
happens - on this crate's CPU decode fallback (the AI Debug tab
disables D3D11VA, see the "D3D11 GPU decode empty frames" friction
entry in `reco-detect`/related memory), each ~15-20ms, so ~4-5 minutes
of apparent stall per export, scaling linearly with `start_secs`.

**The normal stitch export does not have this problem.** Its own
start-time skip goes through `SmartSource::skip_frames`
(`crates/reco-io/src/smart_source.rs:758`), which does a real seek per
platform: `CpuFrameSource::seek` (an actual `av_seek_frame`-backed
seek, not a decode loop), the GPU-zero-copy paths drain an
already-decoding background pipeline via a channel `recv()` rather than
decoding synchronously, and the D3D11 path calls `seek_to_secs`
directly. `raw_camera_debug` predates or was built independent of
`SmartSource` and never picked up its seek mechanism - it has its own
`FrameSource` enum (`Cpu`/`D3d11`) with its own decode-and-discard skip
instead.

**Not a simple swap - checked before touching it.** The initial skip's
own doc comment (a few lines above the loop) says it is "frame-accurate
by construction... a lossy `-ss` seek lands on the nearest keyframe, not
the exact frame, which matters for a fast-moving ball", and explicitly
cross-references `dump_detection_frames.rs`'s identical convention. That
is a real constraint, not an oversight: `sync_offset_right` corrects a
left/right camera sync offset in *frames*, and a `seek_to_secs` seeking
each camera's two independent source files to their nearest (and
generally different) keyframes could reintroduce exactly the kind of
few-frame sync drift `sync_offset_right` exists to remove - silently,
since nothing before `start_secs` is drawn to catch it visually.

That said, `seek_to_secs` is *already* used one screen down in the same
function, for cut-range skips between keep-windows (see the `cut_ranges`
field doc: "a cut range is typically minutes long... decoding through
one just to throw the frames away would make this diagnostic
impractically slow for no benefit - nothing is drawn or encoded from
inside a cut range, so the small seek-accuracy loss at its boundary is
invisible in the output"). That justification is identical for the
*initial* `start_secs` skip - nothing is drawn or encoded before
`start_secs` either. The one difference is `sync_offset_right`: cut-range
seeks happen well after left/right sync has already been established
frame-accurately at the start, so a keyframe-snap there does not
reintroduce drift, while an initial seek would seek the two cameras
independently *before* that sync-offset correction has ever been
applied.

**Fix that preserves both properties:** seek only up to a keyframe near
`start_frame`, then finish the remaining handful of frames (at most one
GOP's worth, typically well under a second of video, not the full
`start_secs` span) with the existing frame-accurate `skip_one_frame`
loop - same pattern real editors use for "seek then step to frame".
Untried as of this entry; needs `VideoDecoder`/`D3d11FrameSource` to
expose whichever timestamp the underlying seek actually landed on (or
just re-derive it by decoding one frame after the seek and reading its
pts) so the remaining-frames count is computed correctly rather than
assumed.
