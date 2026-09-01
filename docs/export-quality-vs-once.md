# Export quality vs Once Autocam

Research log from 2026-08-30: the user suspected Once Autocam's exports
(a competitor product processing the same match) looked sharper than
reco's, and wanted to know why - and whether reco can beat it. Grounded
in real measurements on a real match (`05 BMC20 -Berghem Sport`,
Berghem Sport J011-1), not guessed. All exports below used the same
source cameras, calibration, and AI model unless noted.

## Why VMAF wasn't the right tool

VMAF compares a reference frame to a distorted frame of the *same*
composition. reco and Once each run their own independent AI panner
over the same match - different pan/tilt/zoom decisions at every
instant, and different clip durations - so frame N in one file is not
the same moment as frame N in the other. A direct VMAF diff would
produce numbers that look scientific but measure nothing real. See
also the general VMAF findings from 2026-08-29 (self-comparison only
valid for same-content encoder/regression checks).

Instead: [`ffmpeg blurdetect`](https://ffmpeg.org/ffmpeg-filters.html#blurdetect)
is a **no-reference** per-frame sharpness estimate (higher = blurrier).
It needs no alignment between two videos - each is scored independently
- so it works even when the two systems frame the action differently.
Comparisons below sample both videos at 1fps over the *same* real-world
time window (paired by second) so they're at least looking at the same
moments of play, even though the crop differs.

## Root cause: two separate levers, FOV dominates

All measurements below are on the t=185-260s window of the match (the
window with the largest reco-vs-Once gap in an initial 400s-wide scan),
75 one-second samples per video, 2560x1440 output both sides.

| Export | Settings | Blur (lower = sharper) | vs Once |
|---|---|---|---|
| raw LEFT camera | no processing at all | 3.146 | ceiling reference |
| reco | quality=**high** + FOV 50-70deg (wide) | 3.333 | -4.4% (sharper) |
| **Once Autocam** | (competitor, unknown settings) | **3.486** | - |
| reco | quality=**high** + FOV 35-55deg (production default) | 3.648 | +4.6% (blurrier) |
| reco | quality=**fast** + FOV 35-55deg (the original export) | 3.711 | +6.5% (blurrier) |

Two independent things were tested:

1. **Encoder quality preset** (`fast` -> `high`) alone: ~1.7% improvement.
   Small, because `fast` isn't starved for bits - reco's original export
   was already at a *higher* bitrate than Once's (15.4 vs 12.2 Mbps) -
   `fast` just uses NVENC's cheapest algorithm (preset `p3`, CQ 28)
   regardless of how many bits it's given.
2. **FOV range** (`fov_tight`/`fov_wide`/`fov_default`, tunable today via
   the GUI's AI Tracking sliders): widening from the production default
   (35-55deg, avg ~50deg) to 50-70deg (avg ~65deg) bought ~9% - the
   dominant lever. A tighter FOV crops a smaller slice of the same
   3840x2880-per-lens source and has to digitally upscale more to fill
   2560x1440, which reads as softness independent of the encoder.

Combined (`high` + wide FOV), reco ends up **sharper than Once** and
close to the raw-source ceiling. Raw source itself has almost no blur
variance across the 75 samples (3.136-3.160) - confirming the gap is
entirely a processing choice on both products' parts, not a limit of
the cameras.

**No FOV code change was needed** - `fov_tight`/`fov_wide`/`fov_default`
are already GUI-adjustable sliders (see
[`docs/ai-panner-tuning.md`](ai-panner-tuning.md)). The user tunes
these themselves per match.

## Does quality/bitrate alone (no FOV change) beat Once?

Tested at Once's own bitrate (12 Mbps, matched, `--max-bitrate 12`) to
isolate whether the *encoder* alone (not more bits, not wider FOV) is
the deciding factor:

| Export | Blur | vs Once (3.486) |
|---|---|---|
| reco: high + wide FOV, 12 Mbps (matched) | 3.361 | **-3.6% (beats Once)** |
| reco: high + production FOV (35-55deg), 12 Mbps (matched) | 3.688 | +5.8% (loses to Once) |

**FOV is the deciding factor, not the encoder.** At Once's own bitrate
budget, reco only wins if the FOV is also widened; quality preset alone
is not enough to overcome a narrow FOV's upscale penalty.

## The "high" tier's hidden bitrate ceiling (and the fix)

Every quality preset bundles an NVENC preset + CQ + a bitrate *ceiling*
(`maxrate`), not just a CQ value:

| Preset | NVENC preset | CQ | Target (1080p base) | Ceiling (1080p base) | Ceiling at 2560x1440 |
|---|---|---|---|---|---|
| fast | p3 (fastest) | 28 | 6 Mbps | 10 Mbps | ~17.8 Mbps |
| balanced | p4 | 23 | 9 Mbps | 14 Mbps | ~24.9 Mbps |
| high | p5 | 19 | 15 Mbps | 22 Mbps | ~39.1 Mbps |

(Base values scale by resolution: `factor = pixel_count / (1920x1080)`,
clamped to `[0.5, 8.0]` - 1.778x at 2560x1440. See
[`build_encoder_opts`](../crates/reco-io/src/ffmpeg/encoder.rs) for the
`hevc_nvenc` branch these come from; other codecs/encoders have their
own tuples in the same function.)

`--quality-value` (0-100) can push CQ below "high"'s 19 (e.g. 95 -> CQ
~13.4), but **the bitrate ceiling doesn't move with it** - it's still
whatever the nearest tier (`>=75` -> "high") set. Measured: `--quality-value
95 --preset p7` (NVENC's slowest/best preset) produced *byte-identical*
blur to plain `--quality high` (3.3332 vs 3.3332) - the encoder wanted
to spend more bits for the lower CQ but the ceiling wouldn't let it, and
`p7` just cost ~40% more encode time for nothing.

**Fixed 2026-08-30**: added `--max-bitrate <MBPS>` to `reco stitch`,
overriding both the target and peak bitrate outright, independent of
quality preset/`--quality-value`. Verified it actually lifts the ceiling:
`--quality-value 95 --preset p7 --max-bitrate 60` landed at a real 60.4
Mbps (previously capped ~39.5M) with blur improving further to 3.3237 -
small (diminishing returns past "high"+wide-FOV - most of the gain was
already FOV+preset, not bitrate), but confirms the cap is really gone.
GUI-only exposes the `fast`/`balanced`/`high` dropdown today - no
`--quality-value` or `--max-bitrate` equivalent there yet (CLI-only).

## How Once's FOV compares (estimated)

Once has no exposed calibration/telemetry, but both products film the
same physical pitch from (as far as could be told from matching
background landmarks - floodlights, trees, a comms mast) the same
vantage point. Photogrammetric estimate: measured a fixed real-world
object (the goal frame; cross-checked against a scoreboard panel, both
visible in both videos at the same timestamp) in pixels, in a reco
frame with a *known* FOV, and used the ratio to back out Once's implied
angle (`pixel_width ~ 1/tan(half_fov)` at matched output resolution).

| | Vertical FOV | Horizontal FOV |
|---|---|---|
| Once (estimated, ~10% uncertainty) | ~70-73deg | ~104-105deg |
| reco - wide test config used above | 65deg | 97deg |
| reco - production default (`fov_default`) | 50deg | 79deg |
| reco - production `fov_wide` ceiling | 55deg | ~87deg |

Once appears to run **wider than even the "wide" test config above**,
and notably wider than reco's current `fov_wide` ceiling - consistent
with the near-flat blur variance seen in an earlier Once contact-sheet
scan (it rarely zooms in tight). Two independent landmark measurements
(goal frame, scoreboard panel) agreed within ~4% of each other.

## YouTube upload compliance (2026-08-30)

Checked reco's exports against YouTube's
[recommended upload encoding settings](https://support.google.com/youtube/answer/1722171).

| YouTube requirement | reco export | Status |
|---|---|---|
| Container: MP4 | MP4 | OK |
| Chroma: 4:2:0 | yuv420p, confirmed via ffprobe | OK |
| Frame rate: standard (24-30fps) | 30000/1001 (~29.97fps) | OK |
| Bitrate 1440p @ standard fps: 16 Mbps | fast ~15.4-15.8M (matches), balanced ~16-25M, high ~27-39M (above - not a problem, just more than the recommendation asks for) | OK |
| Audio: AAC-LC, 48kHz | AAC-LC, 48000Hz, confirmed | OK |
| Audio: stereo (384 kbps) | **mono**, ~285 kbps | Deviation - see below, not a reco bug |
| MP4 "Fast Start" (`moov` before `mdat`) | was NOT set - `moov` landed at the very end of the file (confirmed via `ffprobe -v trace` on a real 797MB export: `mdat` at offset 44, `moov` at the last ~65KB) | **Fixed 2026-08-30** |

**Fast Start fix**: plain-MP4 output (`Container::Mp4`, the default) called
`write_header()` with no `movflags` at all -
[`encoder.rs`](../crates/reco-io/src/ffmpeg/encoder.rs) now passes
`movflags=faststart` for that path, so ffmpeg's muxer rewrites the file
at `write_trailer` to put `moov` first. Verified on a real export:
`ftyp -> moov -> free -> mdat`, in that order. Zero quality impact (it's
purely an atom-order rewrite of the already-encoded data), zero change
to fragmented-MP4/Matroska's own existing streaming behavior. Built,
clippy (`-D warnings`) and `cargo fmt --check` clean.

**Mono audio is NOT a reco bug** - checked the *raw, unprocessed* DJI
source files directly (before reco ever touches them):
`LEFT/DJI_..._L01.MP4` and `RIGHT/DJI_..._R01.MP4` both report
`channels=1` (mono), 48kHz AAC-LC, 285375 bps - byte-for-byte the same
bitrate that ends up in reco's export, confirming reco's audio handling
is pure stream-copy passthrough with no re-encoding or downmixing. The
DJI Osmo Action 4 cameras are recording mono at the source; cause
unconfirmed (could be a camera setting, could be a hardware/mic
limitation) - not something reco's pipeline can fix without a real
audio re-encode path (see the 2026-08-29 stereo-upmix research, deferred
by the user).

## Keyframe interval (GOP): the real "give YouTube less work to do" lever

Prompted by a different framing of the goal: not just "match YouTube's
recommended settings" but "minimize how much YouTube's own transcode
pipeline has to transform our upload." Checked the actual keyframe
spacing (`ffprobe -skip_frame nokey`) on real files:

| | Keyframe interval |
|---|---|
| reco (before fix) | 8.34s (250 frames - NVENC's unconfigured default) |
| Once Autocam | 2.00s (60 frames @ 30fps) |

A long GOP forces the encoder to predict from an increasingly stale
reference frame for many seconds - worse under this app's constantly-
panning autocam than for mostly-static content - and gives any
downstream transcoder (YouTube's included) far coarser points to
re-segment or switch adaptive-bitrate quality at; 2 seconds is the
de-facto standard keyframe interval across VOD/streaming platforms
(HLS/DASH segmenting, etc.), which Once's export already matches and
reco's didn't.

**Fixed 2026-08-30**: `StitchJob` (crates/reco-io/src/stitch_job.rs) now
sets `gop_size: Some((fps * 2.0).round() as u32)` instead of leaving it
`None`. Verified on a real export: keyframes now land at exactly 2.002s
intervals (60 frames @ 29.97fps), matching Once. Build/clippy/fmt clean.

## Recommendations

- **FOV**: the user tunes `fov_tight`/`fov_wide`/`fov_default` wider
  themselves per match via the existing GUI sliders - no code change.
  Once's estimated ~70deg vertical / ~104deg horizontal is a reasonable
  reference point for how far "wide" can go while still reading as a
  broadcast shot (see the sample frame from the wide test, visually
  sane, not degenerate).
- **Quality preset**: `high` over `fast` for final exports is a free
  ~1.7% win with no real downside beyond file size/encode time; the GUI
  dropdown already supports this today, default was left at `fast`
  (deliberately - not changed, since the control already lets the user
  choose).
- **`--max-bitrate`**: available on `reco stitch` today (CLI only). Real
  but small benefit beyond `high` + wide FOV - most useful if the user
  wants a specific bitrate budget (e.g. matching a target platform's
  ceiling) rather than as a quality lever on its own.
