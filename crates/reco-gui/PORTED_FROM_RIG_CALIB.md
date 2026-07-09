# Features ported from rig-calib

Tracks calibration-panel improvements built in `crates/rig-calib/` (a
focused standalone lens + rig calibration tool) that have been ported
back into `crates/reco-gui/`'s calibration panel, for eventual upstream
contribution. Update this list whenever more rig-calib work is ported.

Not everything in rig-calib belongs here: its settings schema, status
line, and general app scope are deliberately simpler than reco-gui's
(single-workflow tool vs. full production app) - see "Explicitly not
ported" below.

## Status

- [x] **Enhanced `LabeledSlider`** - double-click-to-edit exact value,
      up/down stepper arrows (`step` property). Applied to every
      existing calibration/lens slider.
- [x] **`Tip` tooltip component** - hover tooltips, used throughout the
      calibration panel.
- [x] **`ground_tilt_x` / `ground_tilt_z` sliders** - near-field
      ground-plane tilt correction (field already existed in
      `reco-core`'s `PlaneLayout`, just had no UI in reco-gui).
- [x] **`blend_flip_direction`** - checkbox + `ViewportConfig` wiring for
      which camera fades over the other at the seam.
- [x] **Variable playback speed** - `Playback::set_speed()`/`speed()`
      (0.05x-8x, drift-free pacing) + click-to-edit speed field in the
      transport bar.
- [x] **Auto-calibrate tuning knobs** - `force_x_rx` / `force_z_rz`
      (force-fit independent rotation angles) and full-resolution
      feature detection toggle.
- [x] **Sync-offset auto-detection** - "Detect Sync Offset" button
      (IMU telemetry, falls back to audio cross-correlation).
- [x] **Audio Sync waveform panel** - collapsible section with overlaid
      left/right envelope display, adjustable window/height, for
      visually verifying `sync_offset` is correct.
- [x] **Sync-offset playhead-jump fix** - changing the sync offset no
      longer resets playback to frame 0.
- [x] **`SectionHeader` expand indicator** - filled yellow square instead
      of a ▾/▸ text character, consistent across every collapsible
      section.
- [x] **Full transport bar** - 6-button NLE-style row (jump-to-start /
      step-back / play-pause / step-forward / stop / jump-to-end, each
      with a tooltip) plus a click-to-edit frame-number field.
- [x] **Audio Sync panel placed in the right controls panel** - initially
      landed in the left files panel (next to the existing Sync offset
      field); moved to the right panel as its own collapsible section,
      next to Calibration/Lens correction, matching rig-calib's layout.
- [x] **Reopen last-used files on startup** - `GuiSettings::last_left()`/
      `last_right()`/`last_calibration()` (existence-filtered) + startup
      wiring that preloads them before the first `try_init_and_update`
      call, mirroring rig-calib's behavior. Verified end-to-end: a real
      previous session's video pair auto-loaded and rendered on launch.
- [x] **Tooltip hover delay shortened** - `Tip`'s popup timer was ported
      verbatim from rig-calib at `3000ms`, which reads as "no tooltip at
      all" during normal quick mouse movement. Dropped to `600ms` in
      reco-gui. (Not changed in rig-calib - untouched there.)
- [x] **Right panel widened** - `right-panel-width` 280px -> 320px; at
      280px the ScrollView's scrollbar overlapped `LabeledSlider`'s
      up/down stepper arrows at the right edge, making them unclickable.
- [x] **Auto-calibrate tuning ("Advanced") moved to the right panel** -
      was in the left files panel; moved next to Calibration/Lens
      correction on the right, matching rig-calib's single-column layout.
      Required relaxing the right panel's open-toggle from `files-loaded`
      to `has-both-videos`, since these tuning knobs need to be reachable
      *before* the first calibration exists (the toggle previously
      required a pipeline to already be running).
- [x] **"Audio Sync" renamed to "Audio Sync Waveform"** - reco-gui-only
      naming tweak, not present in rig-calib.
- [x] **Transport bar: current-frame field moved under current-time,
      time format changed to `HH:MM:SS`** - the frame-number field used
      to sit far to the right (after total-time-text); now stacked
      directly under `current-time-text` since both describe the current
      playhead position. `format_time()` was `M:SS` (no leading zero on
      minutes, no hours); now zero-padded `HH:MM:SS`. reco-gui-only -
      rig-calib's own transport bar layout is untouched.
- [x] **Fixed: seeking via the frame-number field / step buttons / `[`/`]`
      keys didn't update the time display** - `on_seek` (debounced),
      `on_step_forward`, `on_step_backward`, and `on_seek_relative` all
      called `set_current_frame` directly instead of `sync_frame_display`,
      so `current_time_text` (and `total_time_text`) went stale after any
      seek that didn't go through the scrubber's own release handler.
      Real pre-existing bug, unrelated to rig-calib; found while wiring
      the frame-number field under `current-time-text`.
- [x] **Playback speed control labeled** - was a bare "1.0" box with a
      tiny "x" suffix, easy to mistake for a stray/duplicate input. Added
      a "Speed:" label plus a tooltip.
- [x] **Transport bar redesigned (v2)** - the first centered-buttons pass
      left an unbalanced tall bar (74px) with the button row floating in
      dead space. Replaced with: a full-width scrubber row (current time /
      timeline / total time), then a 24px control row split into three
      zones separated by thin translucent-white dividers (`#ffffff22`,
      same convention as the audio-waveform grid lines) - playhead info
      (frame field + speed) on the left, transport buttons *true-centered*
      via explicit `x: (parent.width - self.width) / 2` (not just
      stretch-alignment, so it stays centered regardless of how wide the
      side zones are) in the middle, recording + format controls on the
      right. Bar height back down to 52px. Mocked up as an HTML preview
      and approved before implementing. reco-gui-only.
- [x] **Transport bar inset to match the preview column width** - it
      previously spanned the full window (under the side panels too);
      now `preview-column` (the middle Rectangle between the left files
      panel and right controls panel) is a named id, and the bar's actual
      visual box is `x: preview-column.x; width: preview-column.width`
      inside a full-width wrapper row (direct layout children can't take
      an explicit `x`, so the wrapper absorbs the layout's positioning
      and the inner box is free to align itself). Both wrapper and inner
      box carry `root.bg-bar` so the color is seamless across the full
      width - only the *content* (buttons, timeline, fields) is inset to
      the preview's bounds, not the background. Status bar below it
      stays full-width. reco-gui-only.
- [x] **Transport buttons restyled to match the approved mockup** - the
      6 jump/step/play/stop buttons used the default std-widgets `Button`
      (Material dark), noticeably bulkier than the flat 26x22px buttons
      shown in the HTML mockup. Added a small `TransportButton` component
      (flat `#292929` fill, `#363636` border, `#2e2e2e` hover, 3px
      radius) and swapped all 6 in. ComboBoxes left as native widgets -
      restyling those means reimplementing dropdown behavior from
      scratch, not worth it for a look-only match. reco-gui-only.
- [x] **Fixed: transport bar overflowed into the status bar** - the
      outer bar `Rectangle` was fixed at `height: 52px`, but its actual
      content (padding 4 + scrubber row 20 + spacing 4 + control row 24 +
      padding 4) needs 56px. A plain `Rectangle` doesn't clip overflowing
      children by default, so the control row (frame field, transport
      buttons, Rec/format controls) spilled a few px down into the status
      bar row below it - visible as the status text sitting behind the
      Rec/balanced/auto cluster. Bumped to 58px. reco-gui-only.
- [x] **Full-window visual restyle (one flat control language)** - mocked
      up in HTML, approved, then applied to the Slint UI:
    - New `FlatButton` component (28px, 7px radius, flat `#24282c` fill,
      `#2e333a` hover, blue `#2f7bd6` primary variant) replaces all 44
      std-widgets `Button` usages across toolbar + panels + dialogs, so
      the whole app shares one button style instead of Material's bulky
      rounded look.
    - Color tokens re-based to cool-biased dark neutrals (a hint of blue
      toward the cyan accent) and the accent green brightened to `#74d69a`.
    - `SectionHeader` rebuilt as a 34px full-width row: hover fill, open
      state fill+border, inline yellow indicator, and a right-side chevron
      (`›` collapsed / `⌄` open) - matches the right-panel section rows in
      the mockup.
    - `SegmentList` file rows: taller (32px), bordered wells, 7px radius,
      brighter text, red hover on the remove ×.
    - `TransportButton` recolored to the FlatButton family; new
      `emphasized` variant (accent-tinted, wider) on the play/pause button.
    - New FOV pill overlaid top-center on the preview (`FOV 75° ·
      Constrained`), non-interactive so pan/zoom drag underneath still
      works.
    - Fixed the toolbar's floating panel-toggle: it was a bare `Rectangle`
      with no `x`, which Slint centers - so the ◀/▶ glyph floated in the
      middle of the toolbar. Moved into the toolbar row flow at the far
      right as a proper icon button (`⇤`/`⇥`).
    - reco-gui-only; rig-calib's own chrome is untouched.

## Explicitly not ported (intentional scope divergence)

- rig-calib's simplified settings schema and single-line status display
  - reco-gui's richer `GuiSettings` (export/telemetry/window-state) and
    multi-item toast stack are correct for a full app, not a gap.
