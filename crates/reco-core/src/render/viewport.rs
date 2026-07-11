//! Viewport cropping from the panoramic render.
//!
//! The viewport defines the 16:9 (or user-chosen) rectangle that is
//! extracted from the full panoramic view. The
//! [`crate::detect::panner::Panner`] emits the per-frame yaw/pitch that
//! positions this rectangle.

use crate::detect::director::ViewportPosition;

/// Configuration for the output viewport.
#[derive(Debug, Clone)]
pub struct ViewportConfig {
    /// Output width in pixels.
    pub width: u32,
    /// Output height in pixels.
    pub height: u32,
    /// Vertical field of view in degrees.
    ///
    /// Controls how "zoomed in" the output is. Larger values show more
    /// of the panorama. Default: 75.0 (matches v1 Three.js camera FOV).
    /// Note: this is vertical FOV per nalgebra's `Perspective3` convention.
    pub fov_degrees: f32,
    /// Seam blend width in UV space (0.0–1.0).
    ///
    /// Controls how much of the right plane's left edge fades in over the
    /// left plane using a smoothstep alpha gradient. `0.0` = hard seam,
    /// `0.15` = blend over 15% of the plane width. Default: 0.05 (tight
    /// crossfade — the small value matches the live-production tests on
    /// Jetson CSI IMX477, larger blends wash out ball tracking in the
    /// overlap region).
    pub blend_width: f32,
    /// Flip which camera's content fades over the other at the blend seam.
    ///
    /// `false` (default) = right camera fades in over a fixed left
    /// (original behavior). `true` = left camera fades in over a fixed
    /// right. Purely a rendering choice - doesn't move the seam or affect
    /// calibration geometry (unlike `PlaneLayout::intersect`).
    pub blend_flip_direction: bool,
    /// Rig tilt in radians (forward lean from vertical).
    ///
    /// Rotates the entire scene (both planes) to compensate for a
    /// physically tilted camera rig. When panning, this creates a
    /// natural roll correction that straightens vertical lines at the
    /// edges. `0.0` = no correction. Default: 0.0.
    pub rig_tilt: f32,
    /// Rig roll in radians (lateral lean).
    ///
    /// Rotates the scene around the forward axis to compensate for a
    /// laterally tilted camera rig. `0.0` = no correction. Default: 0.0.
    pub rig_roll: f32,
    /// Lens distortion correction amount (0.0 to 1.0).
    ///
    /// Controls how much KB4 correction the shader applies. `1.0`
    /// (default) is full correction. `0.0` is pinhole projection.
    /// Values between smoothly interpolate. This is a rendering
    /// parameter - it does NOT affect calibration accuracy.
    pub lens_correction_amount: f32,
    /// Automatically nudge each camera's color toward a shared mean,
    /// measured periodically from the seam-adjacent band of each camera's
    /// raw frame (see [`super::color_match`]). Corrects an exposure/white-
    /// balance mismatch between the two cameras that would otherwise show
    /// up as a color/brightness step at the seam, independent of geometric
    /// alignment. Only takes effect on the YUV420P/NV12 CPU-upload render
    /// paths - the BGRA and GPU zero-copy paths have no CPU pixel access
    /// and always render with identity color correction regardless of this
    /// flag. Default: `true`.
    pub color_match_enabled: bool,
    /// How wide a band (in plane UV space, from the seam-adjacent edge) the
    /// color-match measurement samples. Independent of `blend_width` - the
    /// physical camera overlap doesn't change with the crossfade width.
    /// Default: `0.15`.
    pub color_match_band_width: f32,
    /// Color-match sample grid columns. More points = a more stable
    /// measurement, at a small linear cost. Default: `8`.
    pub color_match_grid_cols: u32,
    /// Color-match sample grid rows. Default: `16`.
    pub color_match_grid_rows: u32,
    /// Re-measure color match every N rendered frames. Default: `15`.
    pub color_match_interval_frames: u32,
    /// Exponential-moving-average smoothing factor for the color-match
    /// correction (0.0–1.0). Higher reacts faster but is noisier; lower is
    /// smoother but slower to settle. Default: `0.15`.
    pub color_match_ema_alpha: f32,
    /// Safety clamp on the color-match luma (Y) offset magnitude. Default:
    /// `0.06`.
    pub color_match_max_y_offset: f32,
    /// Safety clamp on the color-match chroma (U/V) offset magnitude.
    /// Default: `0.04`.
    pub color_match_max_chroma_offset: f32,
    /// Use a 2-band spatial blend at the seam (blur low frequencies over a
    /// wide band, keep high frequencies over a narrow band) instead of a
    /// single alpha crossfade. Lets `blend_width` read as visually wider
    /// without doubling fine detail (ball, player edges, field lines) the
    /// way widening a single-band crossfade does - see
    /// [`super::renderer::Renderer::encode_multiband_stitch_pass`] and
    /// `reco-core/FRICTION.md`. Meaningfully more expensive (10 render
    /// passes instead of 1) - opt-in. Unlike `color_match_enabled` this is
    /// a pure GPU technique (no CPU pixel access needed), so it applies
    /// uniformly across every render path including BGRA and GPU
    /// zero-copy. Default: `false`.
    pub multiband_blend_enabled: bool,
}

impl Default for ViewportConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fov_degrees: 75.0,
            blend_width: 0.05,
            blend_flip_direction: false,
            rig_tilt: 0.0,
            rig_roll: 0.0,
            lens_correction_amount: 1.0,
            color_match_enabled: true,
            color_match_band_width: 0.15,
            color_match_grid_cols: 8,
            color_match_grid_rows: 16,
            color_match_interval_frames: 15,
            color_match_ema_alpha: 0.15,
            color_match_max_y_offset: 0.06,
            color_match_max_chroma_offset: 0.04,
            multiband_blend_enabled: false,
        }
    }
}

impl ViewportConfig {
    /// Aspect ratio of the output (width / height).
    ///
    /// Returns 1.0 if height is zero (degenerate viewport).
    pub fn aspect_ratio(&self) -> f32 {
        if self.height == 0 {
            return 1.0;
        }
        self.width as f32 / self.height as f32
    }

    /// Validate the viewport configuration.
    ///
    /// Returns an error description if any field is invalid.
    pub fn validate(&self) -> Result<(), String> {
        if self.width == 0 || self.height == 0 {
            return Err(format!(
                "viewport dimensions must be non-zero, got {}x{}",
                self.width, self.height
            ));
        }
        if !(1.0..179.0).contains(&self.fov_degrees) {
            return Err(format!(
                "fov_degrees must be in (1, 179), got {}",
                self.fov_degrees
            ));
        }
        if !(0.0..=1.0).contains(&self.blend_width) {
            return Err(format!(
                "blend_width must be in [0, 1], got {}",
                self.blend_width
            ));
        }
        Ok(())
    }
}

/// Resolved viewport state for a single frame.
///
/// Combines the viewport configuration with the director's pan position
/// to produce the final camera parameters for rendering.
#[derive(Debug, Clone)]
pub struct ResolvedViewport {
    /// The viewport configuration.
    pub config: ViewportConfig,
    /// The pan position for this frame.
    pub position: ViewportPosition,
}
