//! Photometric seam-alignment refinement: fine-tune a setup by comparing
//! what both cameras actually render at the seam, instead of matched
//! feature points.
//!
//! AKAZE points land almost only on the background, so the point-based
//! optimizer can leave visible steps in pitch lines and doubled players
//! near the camera. Here every candidate setup is rendered per camera
//! (`SingleCameraRenderer`, same shader as the export) and scored by
//! ZNCC between the two renders where both cover the view (1.0 =
//! identical content).
//!
//! Two score modes, run in this order by [`refine_alignment`]:
//!
//! - [`ScoreMode::Overlap`]: luma ZNCC over the whole overlap, averaged
//!   per far / mid / near third so the large, easy background cannot hide
//!   a misaligned near field. Smooth, wide basin: brings an AKAZE result
//!   close to aligned.
//! - [`ScoreMode::Seam`]: only within `seam_strip_px` of where the export
//!   actually cuts (`seam_offset`), on the vertical luma gradient, plus a
//!   zoomed view of the far field across the seam. Parallax means no
//!   setup aligns every depth everywhere in the overlap; this scores what
//!   the viewer sees at the cut, and the gradient makes horizontal edges
//!   (rails, boards, lines - what a vertical step breaks) dominate over
//!   tree texture. Narrow basin: starting it straight from an AKAZE
//!   result leaves a near-field step, so it runs second.
//!
//! Searched: `x_ty`, `x_rz`, `z_rx`, `x_rx`, `z_rz`, `ground_tilt_x/z`,
//! `top_tilt_x/z` (Nelder-Mead). `intersect`, `axis_offset`, the lenses
//! and the framing stay fixed.

use std::sync::atomic::{AtomicBool, Ordering};

use argmin::core::{CostFunction, Error as ArgminError, Executor, State};
use argmin::solver::neldermead::NelderMead;
use reco_core::calibration::Calibration;
use reco_core::gpu::GpuContext;
use reco_core::render::renderer::{GroundTilt, TopTilt};
use reco_core::render::scene::SceneGeometry;
use reco_core::render::single_camera::SingleCameraRenderer;
use reco_core::source::YuvFrame;

use crate::photometric::{self, OverlapMask};
use crate::row_profile;

const ALPHA_THRESHOLD: u8 = 127;
const ZNCC_PATCH: u32 = 16;
const MIN_VARIANCE: f32 = 1e-5;
/// Row-band disparity search for the `dy` report.
const BAND_HEIGHT: u32 = 16;
const MAX_DY: i32 = 15;
const MIN_CONFIDENCE: f32 = 0.3;

/// How many frame pairs (evenly spread) the refinement should score: each
/// costs two renders per evaluation, and on real footage 4 kept held-out
/// frames close to the fit frames' score.
pub const REFINE_FRAMES: usize = 4;

/// Parameter names searched by the refinement, in vector order.
pub const REFINE_PARAMS: [&str; 9] = [
    "x_ty",
    "x_rz",
    "z_rx",
    "x_rx",
    "z_rz",
    "ground_tilt_x",
    "ground_tilt_z",
    "top_tilt_x",
    "top_tilt_z",
];
/// Initial Nelder-Mead simplex step per parameter (same order).
const REFINE_STEPS: [f64; 9] = [0.002, 0.004, 0.008, 0.008, 0.004, 0.01, 0.01, 0.005, 0.005];

/// How the two camera renders are compared.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScoreMode {
    /// Luma ZNCC over the whole overlap, mean of the far / mid / near
    /// thirds. No far-zoom view.
    Overlap,
    /// Vertical-gradient ZNCC within `seam_strip_px` of the cut at
    /// `seam_offset` (`Topology::seam_offset`), thirds plus the far-zoom
    /// view weighted by `far_weight`.
    Seam {
        /// Seam position the export cuts at.
        seam_offset: f32,
    },
}

/// Evaluation view and search settings. Every field is used as given;
/// [`Default`] holds the values validated on real footage (SCG18,
/// 2026-10-30: matched or beat a hand-tuned setup on held-out frames).
#[derive(Debug, Clone)]
pub struct SeamRefineOptions {
    /// Evaluation render width in pixels.
    pub eval_width: u32,
    /// Evaluation render height in pixels.
    pub eval_height: u32,
    /// Vertical FOV (degrees) of the wide view, looking straight at the seam.
    pub fov_degrees: f32,
    /// Far-zoom view: radians above straight at the seam. 0.46 (~26 deg)
    /// lands on the far touchline and background for a mast-mounted rig.
    pub far_pitch: f32,
    /// Far-zoom view vertical FOV in degrees; 0 disables the view.
    pub far_fov: f32,
    /// Weight of the far-zoom ZNCC next to the three thirds
    /// ([`ScoreMode::Seam`] only).
    pub far_weight: f32,
    /// Half-width in evaluation pixels of the strip around the seam
    /// ([`ScoreMode::Seam`] only).
    pub seam_strip_px: f32,
    /// Nelder-Mead iteration cap per stage.
    pub max_iters: u64,
}

impl Default for SeamRefineOptions {
    fn default() -> Self {
        Self {
            eval_width: 1280,
            eval_height: 720,
            fov_degrees: 100.0,
            far_pitch: 0.46,
            far_fov: 25.0,
            far_weight: 2.0,
            seam_strip_px: 24.0,
            max_iters: 400,
        }
    }
}

/// Alignment score of one setup over a set of frame pairs.
#[derive(Debug, Clone, Default)]
pub struct SeamScore {
    /// Mean ZNCC per overlap third (far, mid, near); -1 when a third had
    /// no scoreable patch.
    pub zncc: [f32; 3],
    /// Mean ZNCC of the far-zoom view (`None` when not scored).
    pub far_zoom: Option<f32>,
    /// |dy| (view pixels) per measured row band, per third - only filled
    /// when requested from [`SeamEvaluator::score`].
    pub dy: [Vec<f32>; 3],
    /// |dy| per measured row band in the far-zoom view.
    pub far_dy: Vec<f32>,
    /// The single number the refinement maximizes.
    pub objective: f32,
}

/// Errors from [`refine_alignment`] / [`SeamEvaluator::refine`].
#[derive(Debug, Clone, thiserror::Error)]
pub enum SeamRefineError {
    /// Fewer than one frame pair, or frames of different sizes.
    #[error("seam refine needs at least one frame pair of equal size")]
    NoFrames,
    /// The cancel flag was set.
    #[error("seam refine cancelled")]
    Cancelled,
    /// The optimizer itself failed.
    #[error("seam refine optimizer: {0}")]
    Optimizer(String),
}

/// Result of [`refine_alignment`].
#[derive(Debug, Clone)]
pub struct SeamRefineOutcome {
    /// The refined setup (input setup with the searched parameters replaced).
    pub calibration: Calibration,
    /// Score of the input setup in the last stage's mode.
    pub before: SeamScore,
    /// Score of the refined setup in the last stage's mode.
    pub after: SeamScore,
}

/// Renderers for one input resolution plus the evaluation settings.
pub struct SeamEvaluator<'a> {
    gpu: &'a GpuContext,
    left: SingleCameraRenderer,
    right: SingleCameraRenderer,
    aspect: f32,
    opts: SeamRefineOptions,
}

/// One rendered view: both cameras plus the scored region.
struct ViewRender {
    left: Vec<u8>,
    right: Vec<u8>,
    mask: OverlapMask,
}

impl<'a> SeamEvaluator<'a> {
    /// Renderers for input frames of `input_width` x `input_height`.
    pub fn new(
        gpu: &'a GpuContext,
        input_width: u32,
        input_height: u32,
        opts: SeamRefineOptions,
    ) -> Self {
        let aspect = input_width as f32 / input_height as f32;
        let (w, h) = (opts.eval_width, opts.eval_height);
        Self {
            gpu,
            left: SingleCameraRenderer::new(gpu, input_width, input_height, w, h, aspect),
            right: SingleCameraRenderer::new(gpu, input_width, input_height, w, h, aspect),
            aspect,
            opts,
        }
    }

    /// The settings this evaluator renders and searches with.
    pub fn options(&self) -> &SeamRefineOptions {
        &self.opts
    }

    /// Score `cal` on `pairs`. `measure_dy` also fills the per-band
    /// vertical disparity (slower; for reports, not for the search).
    pub fn score(
        &self,
        cal: &Calibration,
        pairs: &[(YuvFrame, YuvFrame)],
        mode: ScoreMode,
        measure_dy: bool,
    ) -> SeamScore {
        let scene = SceneGeometry::new(&cal.topology, &cal.framing, self.aspect);
        let mut score = SeamScore::default();
        let mut sums = [(0.0f32, 0usize); 3];
        let mut far = (0.0f32, 0usize);
        let gradient = matches!(mode, ScoreMode::Seam { .. });

        for (l, r) in pairs {
            let wide = self.render(cal, &scene, l, r, mode, false);
            let (lz, rz) = (
                self.zncc_input(&wide.left, gradient),
                self.zncc_input(&wide.right, gradient),
            );
            if let Some((min_row, max_row)) = overlap_rows(&wide.mask) {
                let third = ((max_row - min_row) / 3).max(1);
                for (ti, sum) in sums.iter_mut().enumerate() {
                    let start = min_row + ti as u32 * third;
                    let end = if ti == 2 { max_row + 1 } else { start + third };
                    let sub = rows_only(&wide.mask, start, end);
                    let report =
                        photometric::windowed_zncc(&lz, &rz, &sub, ZNCC_PATCH, MIN_VARIANCE);
                    if report.valid_patches > 0 {
                        sum.0 += report.mean;
                        sum.1 += 1;
                    }
                }
                if measure_dy {
                    let third_of =
                        |row: f32| (((row as u32).saturating_sub(min_row)) / third).min(2) as usize;
                    for b in self.row_bands(&wide).iter().flatten() {
                        score.dy[third_of(b.row_center)].push(b.dy.abs());
                    }
                }
            }

            if self.far_view_scored(mode) {
                let zoom = self.render(cal, &scene, l, r, mode, true);
                let (lz, rz) = (
                    self.zncc_input(&zoom.left, gradient),
                    self.zncc_input(&zoom.right, gradient),
                );
                let report =
                    photometric::windowed_zncc(&lz, &rz, &zoom.mask, ZNCC_PATCH, MIN_VARIANCE);
                if report.valid_patches > 0 {
                    far.0 += report.mean;
                    far.1 += 1;
                }
                if measure_dy {
                    score
                        .far_dy
                        .extend(self.row_bands(&zoom).iter().flatten().map(|b| b.dy.abs()));
                }
            }
        }

        for (ti, (sum, n)) in sums.iter().enumerate() {
            score.zncc[ti] = if *n > 0 { sum / *n as f32 } else { -1.0 };
        }
        if far.1 > 0 {
            score.far_zoom = Some(far.0 / far.1 as f32);
        }
        let thirds: f32 = score.zncc.iter().sum();
        score.objective = match score.far_zoom {
            Some(z) => (thirds + self.opts.far_weight * z) / (3.0 + self.opts.far_weight),
            None => thirds / 3.0,
        };
        score
    }

    /// Composite of the first frame pair (left, right alpha-blended over
    /// it, scored region outlined red), as RGBA of
    /// `eval_width` x `eval_height`: the wide view, then the far-zoom view
    /// when `mode` scores it.
    pub fn composites(
        &self,
        cal: &Calibration,
        pair: &(YuvFrame, YuvFrame),
        mode: ScoreMode,
    ) -> Vec<Vec<u8>> {
        let scene = SceneGeometry::new(&cal.topology, &cal.framing, self.aspect);
        let mut views = vec![self.render(cal, &scene, &pair.0, &pair.1, mode, false)];
        if self.far_view_scored(mode) {
            views.push(self.render(cal, &scene, &pair.0, &pair.1, mode, true));
        }
        views.iter().map(composite).collect()
    }

    /// Nelder-Mead search from `start` maximizing [`Self::score`]'s
    /// objective in `mode`. `on_eval` gets the number of setups scored
    /// so far (roughly 1-2 per iteration, up to `max_iters` iterations).
    /// Returns the best setup found (never worse than `start`).
    pub fn refine(
        &self,
        start: &Calibration,
        pairs: &[(YuvFrame, YuvFrame)],
        mode: ScoreMode,
        on_eval: &mut dyn FnMut(usize),
        interrupted: &AtomicBool,
    ) -> Result<Calibration, SeamRefineError> {
        let x0 = get_params(start);
        let mut simplex = vec![x0.clone()];
        for (d, step) in REFINE_STEPS.iter().enumerate() {
            let mut v = x0.clone();
            v[d] += step;
            simplex.push(v);
        }
        let solver: NelderMead<Vec<f64>, f64> = NelderMead::new(simplex)
            .with_sd_tolerance(1e-6)
            .map_err(|e| SeamRefineError::Optimizer(e.to_string()))?;
        let cost = RefineCost {
            eval: self,
            base: start,
            pairs,
            mode,
            evals: std::cell::Cell::new(0),
            on_eval: std::cell::RefCell::new(on_eval),
            interrupted,
        };
        let res = Executor::new(cost, solver)
            .configure(|s| s.max_iters(self.opts.max_iters))
            .run();
        if interrupted.load(Ordering::Relaxed) {
            return Err(SeamRefineError::Cancelled);
        }
        let res = res.map_err(|e| SeamRefineError::Optimizer(e.to_string()))?;
        let best = res
            .state()
            .get_best_param()
            .ok_or_else(|| SeamRefineError::Optimizer("no best parameters".into()))?;
        Ok(with_params(start, best))
    }

    fn far_view_scored(&self, mode: ScoreMode) -> bool {
        matches!(mode, ScoreMode::Seam { .. }) && self.opts.far_fov > 0.0
    }

    fn render(
        &self,
        cal: &Calibration,
        scene: &SceneGeometry,
        l: &YuvFrame,
        r: &YuvFrame,
        mode: ScoreMode,
        far: bool,
    ) -> ViewRender {
        let (fov, pitch) = if far {
            (self.opts.far_fov, self.opts.far_pitch)
        } else {
            (self.opts.fov_degrees, 0.0)
        };
        let (lg, lt) = tilts(cal, false);
        let (rg, rt) = tilts(cal, true);
        let left = self.left.render_and_readback_pitched(
            self.gpu,
            scene,
            &cal.lenses[0],
            false,
            fov,
            pitch,
            &l.y,
            &l.u,
            &l.v,
            lg,
            lt,
        );
        let right = self.right.render_and_readback_pitched(
            self.gpu,
            scene,
            &cal.lenses[1],
            true,
            fov,
            pitch,
            &r.y,
            &r.u,
            &r.v,
            rg,
            rt,
        );
        let (w, h) = (self.opts.eval_width, self.opts.eval_height);
        let mut mask = photometric::overlap_mask_from_alpha(&left, &right, w, h, ALPHA_THRESHOLD);
        if let ScoreMode::Seam { seam_offset } = mode {
            self.restrict_to_seam(scene, fov, pitch, seam_offset, &mut mask);
        }
        ViewRender { left, right, mask }
    }

    /// Keep only the overlap pixels within `seam_strip_px` of the cut (the
    /// right plane's `uv.x = seam_offset` line, where the export cuts with
    /// the default blend direction). Empties the mask when the seam is not
    /// in view.
    fn restrict_to_seam(
        &self,
        scene: &SceneGeometry,
        fov: f32,
        pitch: f32,
        seam_offset: f32,
        mask: &mut OverlapMask,
    ) {
        let ends = [-0.5, 1.5].map(|v| {
            self.right
                .plane_point_to_pixel(scene, true, fov, pitch, [seam_offset, v])
        });
        let [Some(a), Some(b)] = ends else {
            mask.mask.fill(false);
            return;
        };
        let slope = (b[0] - a[0]) / (b[1] - a[1]);
        let strip = self.opts.seam_strip_px;
        let width = mask.width as usize;
        for (y, row) in mask.mask.chunks_exact_mut(width).enumerate() {
            let seam_x = a[0] + (y as f32 + 0.5 - a[1]) * slope;
            for (x, m) in row.iter_mut().enumerate() {
                *m &= (x as f32 + 0.5 - seam_x).abs() <= strip;
            }
        }
    }

    /// Luma, or its vertical central difference when `gradient`: a
    /// vertical misalignment shows only at horizontal edges.
    fn zncc_input(&self, rgba: &[u8], gradient: bool) -> Vec<f32> {
        let (w, h) = (self.opts.eval_width, self.opts.eval_height);
        let luma = photometric::to_luma(rgba, w, h);
        if !gradient {
            return luma;
        }
        let (w, h) = (w as usize, h as usize);
        let mut g = vec![0.0f32; w * h];
        for y in 1..h.saturating_sub(1) {
            for x in 0..w {
                g[y * w + x] = luma[(y + 1) * w + x] - luma[(y - 1) * w + x];
            }
        }
        g
    }

    fn row_bands(&self, view: &ViewRender) -> Vec<Option<row_profile::RowBandDisparity>> {
        let (w, h) = (self.opts.eval_width, self.opts.eval_height);
        row_profile::measure_row_bands(
            &photometric::to_luma(&view.left, w, h),
            &photometric::to_luma(&view.right, w, h),
            &view.mask,
            BAND_HEIGHT,
            MAX_DY,
            MIN_VARIANCE,
            MIN_CONFIDENCE,
        )
    }
}

/// Refine `cal` on `pairs`: [`ScoreMode::Overlap`] first (skipped when
/// `seam_only`), then [`ScoreMode::Seam`] at `seam_offset`.
/// `seam_only` suits a setup that is already close (e.g. after the user
/// moved the seam); a fresh AKAZE result needs both stages.
/// `on_progress(stage, stage_count, evals)` reports search progress.
#[allow(clippy::too_many_arguments)]
pub fn refine_alignment(
    gpu: &GpuContext,
    cal: &Calibration,
    pairs: &[(YuvFrame, YuvFrame)],
    seam_offset: f32,
    seam_only: bool,
    opts: SeamRefineOptions,
    on_progress: &mut dyn FnMut(usize, usize, usize),
    interrupted: &AtomicBool,
) -> Result<SeamRefineOutcome, SeamRefineError> {
    let Some((first, _)) = pairs.first() else {
        return Err(SeamRefineError::NoFrames);
    };
    let (w, h) = (first.width, first.height);
    if pairs
        .iter()
        .any(|(l, r)| l.width != w || l.height != h || r.width != w || r.height != h)
    {
        return Err(SeamRefineError::NoFrames);
    }
    let eval = SeamEvaluator::new(gpu, w, h, opts);
    let seam = ScoreMode::Seam { seam_offset };
    let stages: &[ScoreMode] = if seam_only {
        &[seam]
    } else {
        &[ScoreMode::Overlap, seam]
    };
    let before = eval.score(cal, pairs, seam, false);
    let mut current = cal.clone();
    for (i, &mode) in stages.iter().enumerate() {
        let n = stages.len();
        current = eval.refine(
            &current,
            pairs,
            mode,
            &mut |evals| on_progress(i, n, evals),
            interrupted,
        )?;
    }
    let after = eval.score(&current, pairs, seam, false);
    // The seam-mode search never ends worse than its own start, but the
    // overlap stage may trade seam alignment away; keep the input then.
    if after.objective < before.objective {
        log::warn!(
            "seam refine: result scored {:.4} < input {:.4}; keeping the input",
            after.objective,
            before.objective
        );
        return Ok(SeamRefineOutcome {
            calibration: cal.clone(),
            after: before.clone(),
            before,
        });
    }
    Ok(SeamRefineOutcome {
        calibration: current,
        before,
        after,
    })
}

struct RefineCost<'a, 'b> {
    eval: &'a SeamEvaluator<'a>,
    base: &'a Calibration,
    pairs: &'a [(YuvFrame, YuvFrame)],
    mode: ScoreMode,
    evals: std::cell::Cell<usize>,
    on_eval: std::cell::RefCell<&'b mut dyn FnMut(usize)>,
    interrupted: &'a AtomicBool,
}

impl CostFunction for RefineCost<'_, '_> {
    type Param = Vec<f64>;
    type Output = f64;

    fn cost(&self, p: &Self::Param) -> Result<Self::Output, ArgminError> {
        if self.interrupted.load(Ordering::Relaxed) {
            return Err(ArgminError::msg("cancelled"));
        }
        let cal = with_params(self.base, p);
        let score = self.eval.score(&cal, self.pairs, self.mode, false);
        let n = self.evals.get() + 1;
        self.evals.set(n);
        (self.on_eval.borrow_mut())(n);
        Ok(-(score.objective as f64))
    }
}

/// Up to `n` elements of `items`, evenly spread from first to last (all
/// of them when there are no more than `n`) - e.g. the calibration frames
/// to refine on.
pub fn spread_evenly<T: Clone>(items: &[T], n: usize) -> Vec<T> {
    if items.len() <= n {
        return items.to_vec();
    }
    if n <= 1 {
        return items.iter().take(n).cloned().collect();
    }
    (0..n)
        .map(|i| items[i * (items.len() - 1) / (n - 1)].clone())
        .collect()
}

/// Current values of [`REFINE_PARAMS`].
pub fn get_params(cal: &Calibration) -> Vec<f64> {
    let t = &cal.topology;
    vec![
        t.x_ty,
        t.x_rz,
        t.z_rx,
        t.x_rx,
        t.z_rz,
        t.ground_tilt_x,
        t.ground_tilt_z,
        t.top_tilt_x,
        t.top_tilt_z,
    ]
}

/// `base` with [`REFINE_PARAMS`] replaced by `p`.
pub fn with_params(base: &Calibration, p: &[f64]) -> Calibration {
    let mut cal = base.clone();
    let t = &mut cal.topology;
    t.x_ty = p[0];
    t.x_rz = p[1];
    t.z_rx = p[2];
    t.x_rx = p[3];
    t.z_rz = p[4];
    t.ground_tilt_x = p[5];
    t.ground_tilt_z = p[6];
    t.top_tilt_x = p[7];
    t.top_tilt_z = p[8];
    cal
}

fn tilts(cal: &Calibration, is_right: bool) -> (GroundTilt, TopTilt) {
    let t = &cal.topology;
    let (lens, ground, top) = if is_right {
        (&cal.lenses[1], t.ground_tilt_x, t.top_tilt_x)
    } else {
        (&cal.lenses[0], t.ground_tilt_z, t.top_tilt_z)
    };
    let k = lens.ground_tilt_k() as f32;
    (
        GroundTilt {
            tilt: ground as f32,
            k,
            band_full: t.ground_tilt_band_width as f32,
        },
        TopTilt {
            tilt: top as f32,
            k,
            band_full: t.top_tilt_band_width as f32,
        },
    )
}

/// First and last row with any coverage.
fn overlap_rows(mask: &OverlapMask) -> Option<(u32, u32)> {
    let covered = |y: u32| {
        let row = (y * mask.width) as usize..((y + 1) * mask.width) as usize;
        mask.mask[row].iter().any(|&m| m)
    };
    let first = (0..mask.height).find(|&y| covered(y))?;
    let last = (0..mask.height).rev().find(|&y| covered(y))?;
    Some((first, last))
}

/// `mask` limited to rows `start..end`.
fn rows_only(mask: &OverlapMask, start: u32, end: u32) -> OverlapMask {
    let w = mask.width as usize;
    OverlapMask {
        width: mask.width,
        height: mask.height,
        mask: mask
            .mask
            .iter()
            .enumerate()
            .map(|(p, &m)| {
                let y = (p / w) as u32;
                m && y >= start && y < end
            })
            .collect(),
    }
}

/// Left first, right alpha-blended over it (production draw order), with
/// the scored region's left/right edges drawn red.
fn composite(view: &ViewRender) -> Vec<u8> {
    let mut out = vec![0u8; view.left.len()];
    for (o, (l, r)) in out
        .chunks_exact_mut(4)
        .zip(view.left.chunks_exact(4).zip(view.right.chunks_exact(4)))
    {
        let ra = r[3] as f32 / 255.0;
        for c in 0..3 {
            o[c] = (r[c] as f32 * ra + l[c] as f32 * (1.0 - ra)).round() as u8;
        }
        o[3] = 255;
    }
    let m = &view.mask.mask;
    let w = view.mask.width as usize;
    for (p, &inside) in m.iter().enumerate() {
        let x = p % w;
        if inside && (x == 0 || x == w - 1 || !m[p - 1] || !m[p + 1]) {
            out[p * 4..p * 4 + 3].copy_from_slice(&[255, 0, 0]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spread_evenly_picks_ends_and_middle() {
        let v: Vec<u32> = (0..10).collect();
        assert_eq!(spread_evenly(&v, 4), vec![0, 3, 6, 9]);
        assert_eq!(spread_evenly(&v, 1), vec![0]);
        assert_eq!(spread_evenly(&v, 0), Vec::<u32>::new());
        assert_eq!(spread_evenly(&v[..3], 4), vec![0, 1, 2]);
    }

    #[test]
    fn param_tables_line_up() {
        assert_eq!(REFINE_PARAMS.len(), REFINE_STEPS.len());
    }

    #[test]
    fn rows_only_keeps_band() {
        let mask = OverlapMask {
            width: 2,
            height: 4,
            mask: vec![true; 8],
        };
        let sub = rows_only(&mask, 1, 3);
        assert_eq!(
            sub.mask,
            vec![false, false, true, true, true, true, false, false]
        );
        assert_eq!(overlap_rows(&sub), Some((1, 2)));
    }

    #[test]
    fn overlap_rows_empty_mask() {
        let mask = OverlapMask {
            width: 3,
            height: 2,
            mask: vec![false; 6],
        };
        assert_eq!(overlap_rows(&mask), None);
    }
}
