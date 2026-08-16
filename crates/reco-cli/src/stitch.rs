//! Stitch subcommand: encode two video files into a panoramic output.
//!
//! Uses `StitchJob` (Layer 3 API) for all cases, including autocam.
//! The `on_session` callback wires up detection and direction when a
//! YOLO model is provided.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Arguments for the stitch subcommand, collected from CLI parsing.
///
/// `detection_interval`, `lookahead`, and `tracking_mode` are only
/// consumed inside `#[cfg(feature = "autocam")]` blocks below, so
/// `--no-default-features` builds see them as dead. Silence the lint
/// here instead of per-field gating to keep the struct shape stable
/// across features.
#[allow(dead_code)]
pub struct StitchArgs<'a> {
    pub left: &'a str,
    pub right: &'a str,
    pub calibration: &'a str,
    pub output: &'a str,
    pub width: u32,
    pub height: u32,
    pub blend: Option<f32>,
    /// Flip which camera fades in over the other at the blend seam. See
    /// `reco_core::calibration::Topology::blend_flip_direction`.
    pub blend_flip_direction: bool,
    /// Use a 2-band spatial blend at the seam. See
    /// `reco_core::calibration::Topology::multiband_blend_enabled`.
    pub multiband: bool,
    /// Draw a debug line at the exact geometric seam position. See
    /// `reco_core::render::pipeline::StitchPipeline::set_show_seam_line`.
    pub show_seam_line: bool,
    /// Manual seam nudge. See `reco_core::calibration::Topology::seam_offset`.
    pub seam_offset: f32,
    /// Disable auto exposure/color matching at the seam. See
    /// `reco_core::calibration::Topology::color_match_enabled`.
    pub no_color_match: bool,
    pub start_time: Option<f64>,
    pub end_time: Option<f64>,
    pub max_frames: Option<u64>,
    pub encoder_name: Option<String>,
    pub codec: &'a str,
    pub quality: &'a str,
    pub sync_offset: i64,
    pub model_path: Option<&'a str>,
    pub detection_interval: u64,
    pub lookahead: f64,
    pub lookahead_reduced_bit_depth: bool,
    /// EXPERIMENTAL: see `reco-cli`'s `--async-detect` flag help text.
    pub async_detect: bool,
    /// EXPERIMENTAL: see `reco-cli`'s `--async-detect-dual` flag help
    /// text. No effect unless `async_detect` is also set.
    pub async_detect_dual: bool,
    pub tracking_mode: &'a str,
    pub quality_value: Option<u8>,
    pub preset: Option<String>,
    /// Output container selector (`mp4` / `fmp4` / `mkv`). None
    /// means default (plain MP4, finalized at close). `mkv` or
    /// `fmp4` for streamable tee use.
    pub container: Option<&'a str>,
    /// Optional replay-recording output path. When `Some`, the
    /// stitch job writes a stacked-video copy of the source frames
    /// alongside the stitched output (M6.5 feature, `replay`
    /// feature flag on reco-cli).
    pub replay_path: Option<&'a str>,
    /// Optional replay-tile downscale `(width, height)`. When
    /// `Some`, the GPU pack shader produces smaller replay tiles
    /// (FRICTION reco-obs A19). Has no effect without
    /// [`Self::replay_path`]. GPU path only — CPU-resident
    /// sources log a warn and record at source dims.
    pub replay_scale: Option<(u32, u32)>,
    /// When true, silently continue without tracking if detection
    /// cannot run (e.g. zero-copy mode without TensorRT). Default
    /// false: error out so the user knows tracking was requested but
    /// not delivered.
    pub allow_no_tracking: bool,
    /// Force CPU decode to enable ORT CPU detection without TensorRT.
    pub no_zero_copy: bool,
    /// Path for pipeline event JSONL output.
    pub events_path: Option<&'a str>,
    /// Precomputed trajectory CSV (overrides AI panner).
    pub trajectory_path: Option<&'a str>,
    /// FieldPanner tuning JSON (field mode); only present keys override.
    pub panner_config_path: Option<&'a str>,
    /// Named panner preset (base config); JSON overlays on top.
    pub panner_preset: Option<&'a str>,
    /// Ball tracker's player-anchor gate override (radians). See
    /// `reco_autocam::AutocamConfig::player_anchor_max_rad`.
    pub player_anchor_rad: Option<f32>,
    /// Ball tracker's coast-budget override (seconds). See
    /// `reco_autocam::AutocamConfig::ball_coast_secs`.
    pub ball_coast_secs: Option<f32>,
    /// Zoom-target smoothing rate override. See
    /// `reco_autocam::panners::FieldPannerConfig::fov_alpha`.
    pub fov_alpha: Option<f32>,
    /// Cluster-position smoothing rate override. See
    /// `reco_autocam::panners::FieldPannerConfig::cluster_alpha`.
    pub cluster_alpha: Option<f32>,
}

/// Run the stitch subcommand.
pub fn run_stitch(args: StitchArgs<'_>, interrupted: &Arc<AtomicBool>) -> anyhow::Result<()> {
    const MAX_DIM: u32 = 8192;
    anyhow::ensure!(
        args.width > 0 && args.width <= MAX_DIM && args.height > 0 && args.height <= MAX_DIM,
        "Output dimensions {}x{} out of range: width and height must be 1..={MAX_DIM}",
        args.width,
        args.height,
    );

    let progress = crate::helpers::ProgressReporter::new(30);

    // Load calibration up front so we can extract field_roi for autocam
    // and pass the pre-loaded calibration to StitchJob. `field_roi` is
    // only consumed under the autocam feature; a leading underscore
    // silences the unused-var lint on `--no-default-features` builds.
    let cal = reco_core::calibration::Calibration::from_file(Path::new(args.calibration))?;
    // Densified so RoiFilteredDetector's point-in-polygon test follows
    // the true (curved, in raw-distorted space) field boundary instead
    // of straight-lining between the calibration's few stored vertices -
    // see `FieldRoi::densified`'s doc comment.
    #[cfg_attr(not(feature = "autocam"), allow(unused_variables))]
    let field_roi = cal
        .field_roi
        .as_ref()
        .map(|roi| roi.densified(&cal.lenses[0], &cal.lenses[1]));

    // Accept `a.mp4;b.mp4;c.mp4` to chain segments via the concat demuxer
    // (mirrors the GUI's multi-segment selection). A single path stays Single.
    let to_input = |s: &str| -> reco_io::stitch_job::InputPath {
        let parts: Vec<std::path::PathBuf> = s
            .split(';')
            .filter(|p| !p.is_empty())
            .map(std::path::PathBuf::from)
            .collect();
        if parts.len() > 1 {
            log::info!(
                "CLI input: {} segments, chaining via concat demuxer",
                parts.len()
            );
            reco_io::stitch_job::InputPath::Chained(parts)
        } else {
            reco_io::stitch_job::InputPath::Single(std::path::PathBuf::from(s))
        }
    };
    // Snapshot the settings actually used for this export into the
    // output container's "comment" tag, so a batch of test exports
    // with varying AI/blend settings stays self-describing without a
    // separate sidecar file (`ffprobe -show_entries format_tags`).
    let metadata_comment = serde_json::json!({
        "reco_export": {
            "codec": args.codec,
            "quality": args.quality,
            "quality_value": args.quality_value,
            "resolution": format!("{}x{}", args.width, args.height),
            "blend_width": args.blend,
            "blend_flip_direction": args.blend_flip_direction,
            "multiband_blend": args.multiband,
            "show_seam_line": args.show_seam_line,
            "seam_offset": args.seam_offset,
            "color_match": !args.no_color_match,
            "autocam": {
                "model_path": args.model_path,
                "tracking_mode": args.tracking_mode,
                "detection_interval": args.detection_interval,
                "lookahead_secs": args.lookahead,
                "lookahead_reduced_bit_depth": args.lookahead_reduced_bit_depth,
                "panner_preset": args.panner_preset,
                "panner_config_path": args.panner_config_path,
                "player_anchor_rad": args.player_anchor_rad,
                "ball_coast_secs": args.ball_coast_secs,
            }
        }
    })
    .to_string();

    let mut job = reco_io::StitchJob::with_calibration(
        to_input(args.left),
        to_input(args.right),
        cal,
        args.output,
    )
    .codec(parse_codec(args.codec))
    .quality(parse_quality(args.quality))
    .resolution(args.width, args.height)
    .blend_flip_direction(args.blend_flip_direction)
    .multiband_blend_enabled(args.multiband)
    .show_seam_line(args.show_seam_line)
    .seam_offset(args.seam_offset)
    .color_match_enabled(!args.no_color_match)
    .metadata_comment(metadata_comment)
    .on_progress(move |p: &reco_core::session::types::FrameProgress| {
        // Use the session's own elapsed clock so the reported
        // rate excludes one-time GPU / encoder / ORT init and
        // reflects only the decode → stitch → encode loop.
        progress.report_with_elapsed(p.frames_completed, p.elapsed);
    });

    if let Some(b) = args.blend {
        job = job.blend_width(b);
    }
    if let Some(t) = args.start_time {
        job = job.start_time(t);
    }
    if let Some(t) = args.end_time {
        job = job.end_time(t);
    }
    if let Some(n) = args.max_frames {
        job = job.max_frames(n);
    }
    if args.sync_offset != 0 {
        job = job.sync_offset(args.sync_offset);
    }
    if args.no_zero_copy {
        job = job.force_cpu_decode();
    }
    // Lookahead only helps when an AI panner drives the camera: it buffers
    // future frames so the panner can lead and the loop can centered-smooth.
    // For a plain stitch (no model) or sweep mode it would only add latency
    // and VRAM, so skip it and say why.
    let tracking_active = args.model_path.is_some() && args.tracking_mode != "sweep";
    if args.lookahead > 0.0 {
        if tracking_active {
            job = job.lookahead(args.lookahead);
            log::info!(
                "Lookahead: {:.1}s buffer enabled (AI tracking active)",
                args.lookahead
            );
            if args.lookahead_reduced_bit_depth {
                job = job.lookahead_reduced_bit_depth(true);
                log::info!(
                    "Lookahead bit depth: reduced to 8-bit (halved VRAM, 10-bit sources only)"
                );
            }
        } else {
            log::debug!(
                "Lookahead {:.1}s ignored: no AI tracking (needs --model, non-sweep); \
                 a plain stitch needs none",
                args.lookahead
            );
        }
    }
    if let Some(path) = args.events_path {
        job = job.events(path);
    }
    if let Some(ref enc) = args.encoder_name {
        job = job.encoder_name(enc);
    }
    if let Some(qv) = args.quality_value {
        job = job.quality_value(qv);
    }
    if let Some(ref preset) = args.preset {
        job = job.preset(preset);
    }
    if let Some(container) = args.container {
        let fmt: reco_io::output::Format = container
            .parse()
            .map_err(|e: String| anyhow::anyhow!("{e} (expected mp4, fmp4, mkv, mov, or flv)"))?;
        job = job.format(fmt);
    }

    // Opt-in replay recording. The builder call is all the
    // consumer needs - StitchJob owns the encoder lifecycle, the
    // per-frame tap, and the finalize.
    #[cfg(feature = "replay")]
    if let Some(replay_path) = args.replay_path {
        job = job.with_replay_recording(replay_path);
        if let Some((w, h)) = args.replay_scale {
            job = job.with_replay_scale(w, h);
            println!("Replay recording: {replay_path} (scaled to {w}x{h} per tile)");
        } else {
            println!("Replay recording: {replay_path}");
        }
    }
    #[cfg(feature = "replay")]
    if args.replay_scale.is_some() && args.replay_path.is_none() {
        log::warn!("--replay-scale specified without --replay; ignoring.");
    }
    #[cfg(not(feature = "replay"))]
    if args.replay_path.is_some() {
        log::warn!(
            "--replay specified but `replay` feature is disabled. \
             Build with --features replay to enable."
        );
    }

    // Precomputed trajectory file overrides all AI tracking.
    #[cfg(feature = "autocam")]
    if let Some(traj_path) = args.trajectory_path {
        let traj_path = traj_path.to_owned();
        job =
            job.on_session(
                move |session, _source| match reco_autocam::panners::FilePanner::from_csv(
                    std::path::Path::new(&traj_path),
                ) {
                    Ok(panner) => {
                        session.set_panner(Box::new(panner));
                        log::info!("Tracking mode: precomputed trajectory from {traj_path}");
                    }
                    Err(e) => {
                        log::error!("Failed to load trajectory {traj_path}: {e}");
                    }
                },
            );
    }

    // Sweep panner needs no model - attach it directly.
    #[cfg(feature = "autocam")]
    if args.trajectory_path.is_none() && args.tracking_mode == "sweep" {
        job = job.on_session(|session, _source| {
            // Use 80% of coverage max FOV so the viewport fits comfortably.
            let max_fov = session.coverage().map_or(50.0, |c| c.max_fov_degrees());
            let sweep_fov = (max_fov * 0.8).clamp(5.0, 50.0);
            let panner =
                Box::new(reco_autocam::panners::SweepPanner::new(0.8, 10.0).with_fov(sweep_fov));
            session.set_panner(panner);
            log::info!("Tracking mode: sweep (debug, FOV={sweep_fov:.1} deg)");
        });
    }

    // Flag set inside the on_session callback when tracking was requested
    // but couldn't be initialized. Checked after job.run() to produce a
    // clean error exit without segfaulting (process::exit inside a GPU
    // callback crashes NVDEC/Vulkan teardown).
    #[cfg(feature = "autocam")]
    let tracking_failed = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Wire up autocam via the on_session callback if a model is provided.
    #[cfg(feature = "autocam")]
    if args.trajectory_path.is_none()
        && args.tracking_mode != "sweep"
        && let Some(model_path) = args.model_path
    {
        let model_path = model_path.to_owned();
        let interval = args.detection_interval;
        let mode_str = args.tracking_mode.to_owned();
        let allow_fallback = args.allow_no_tracking;
        let player_anchor_rad = args.player_anchor_rad;
        let ball_coast_secs = args.ball_coast_secs;
        let async_detect = args.async_detect;
        let async_detect_dual = args.async_detect_dual;
        let lookahead_secs = args.lookahead;
        let tracking_failed = Arc::clone(&tracking_failed);
        // Resolve FieldPanner tuning up front so a bad preset/file fails
        // before rendering. Preset is the base; --panner-config overlays.
        let panner_cfg: Option<reco_autocam::panners::FieldPannerConfig> = {
            use reco_autocam::panners::{FieldPannerConfig, PRESET_NAMES};
            let base = match args.panner_preset {
                Some(name) => {
                    let c = FieldPannerConfig::from_preset_name(name).ok_or_else(|| {
                        anyhow::anyhow!(
                            "unknown --panner-preset '{name}' (expected: {})",
                            PRESET_NAMES.join(", ")
                        )
                    })?;
                    log::info!("FieldPanner preset: {name}");
                    Some(c)
                }
                None => None,
            };
            match args.panner_config_path {
                Some(p) => {
                    let contents = std::fs::read_to_string(p)
                        .map_err(|e| anyhow::anyhow!("reading panner config {p}: {e}"))?;
                    let cfg = if let Some(b) = base {
                        let mut v = serde_json::to_value(&b).expect("config serializes");
                        let over: serde_json::Value = serde_json::from_str(&contents)
                            .map_err(|e| anyhow::anyhow!("parsing panner config {p}: {e}"))?;
                        if let (Some(bm), serde_json::Value::Object(om)) = (v.as_object_mut(), over)
                        {
                            bm.extend(om);
                        }
                        serde_json::from_value(v)
                            .map_err(|e| anyhow::anyhow!("applying panner config {p}: {e}"))?
                    } else {
                        serde_json::from_str(&contents)
                            .map_err(|e| anyhow::anyhow!("parsing panner config {p}: {e}"))?
                    };
                    log::info!("FieldPanner config loaded from {p}");
                    Some(cfg)
                }
                None => base,
            }
        };

        // --fov-alpha/--cluster-alpha win over --panner-preset/--panner-config,
        // applied last. Neither is part of any named preset's own values,
        // so if no preset/config file was given but one of these flags
        // was, start from FieldPannerConfig::default() so it has a base
        // to land on (matches setup_autocam's own fallback base).
        let panner_cfg = if args.fov_alpha.is_some() || args.cluster_alpha.is_some() {
            let mut c = panner_cfg.unwrap_or_default();
            if let Some(a) = args.fov_alpha {
                c.fov_alpha = a;
            }
            if let Some(a) = args.cluster_alpha {
                c.cluster_alpha = a;
            }
            Some(c)
        } else {
            panner_cfg
        };

        // Snapshot the resolved AI/panner settings for the events JSONL
        // header (see StitchJob::ai_run_config) - built here, before
        // panner_cfg moves into the on_session closure below.
        if args.events_path.is_some() {
            use reco_autocam::panners::{ClusterMode, FramingMode};
            let fp = panner_cfg.clone().unwrap_or_default();
            job = job.ai_run_config(
                model_path.clone(),
                reco_core::calibration::AutocamDefaults {
                    tracking_mode: mode_str.clone(),
                    detection_interval: interval as u32,
                    player_anchor_rad: player_anchor_rad
                        .unwrap_or(reco_autocam::trackers::ball::DEFAULT_PLAYER_ANCHOR_RAD),
                    ball_coast_secs: ball_coast_secs.unwrap_or(
                        reco_autocam::trackers::ball::DEFAULT_COAST_FRAMES as f32 / 30.0,
                    ),
                    lookahead_secs: args.lookahead,
                    lookahead_reduced_bit_depth: args.lookahead_reduced_bit_depth,
                    preset: args.panner_preset.unwrap_or("").to_string(),
                    framing: if fp.framing == FramingMode::FrameAll {
                        "frame_all"
                    } else {
                        "action"
                    }
                    .to_string(),
                    lock_pitch: fp.lock_pitch,
                    cluster_mode: if fp.cluster_mode == ClusterMode::TrimmedMean {
                        "trimmed_mean"
                    } else {
                        "density"
                    }
                    .to_string(),
                    cluster_bandwidth_rad: fp.cluster_bandwidth_rad,
                    dead_zone_rad: fp.dead_zone_rad,
                    ball_weight: fp.ball_weight,
                    ball_max_dist_from_cluster: fp.ball_max_dist_from_cluster,
                    fov_tight: fp.fov_tight,
                    fov_wide: fp.fov_wide,
                    fov_default: fp.fov_default,
                    fov_alpha: fp.fov_alpha,
                    cluster_alpha: fp.cluster_alpha,
                },
            );
        }

        job = job.on_session(move |session, source| {
            let info = source.info();
            let mode = match mode_str.as_str() {
                "sweep" => reco_autocam::TrackingMode::Sweep,
                "ball" => reco_autocam::TrackingMode::Ball,
                _ => reco_autocam::TrackingMode::Field,
            };
            let is_10bit =
                source.gpu_pixel_format() == reco_core::render::renderer::GpuPixelFormat::P010;
            let mut autocam_config = reco_autocam::AutocamConfig::new(&model_path)
                .with_tracking_mode(mode)
                .with_detection_interval(interval)
                .with_10bit(is_10bit);
            if mode == reco_autocam::TrackingMode::Ball {
                autocam_config.confidence_threshold = Some(0.25);
            }
            if let Some(ref cfg) = panner_cfg {
                autocam_config.field_panner_config = Some(cfg.clone());
            }
            autocam_config.player_anchor_max_rad = player_anchor_rad;
            autocam_config.ball_coast_secs = ball_coast_secs;
            let autocam_config = if let Some(roi) = field_roi {
                autocam_config.with_field_roi(roi)
            } else {
                autocam_config
            };
            match reco_autocam::setup_autocam(
                session,
                &autocam_config,
                info.fps as f32,
                source.is_gpu_resident(),
            ) {
                Ok(true) => {
                    println!("Autocam: tracking enabled (model: {model_path})");
                    // EXPERIMENTAL: --async-detect. Builds a SEPARATE
                    // detector instance (doubles the model's VRAM/
                    // session footprint) moved onto a dedicated worker
                    // thread - see reco-core's `async_detect` module.
                    // Only affects the buffered/export loop
                    // (lookahead > 0); harmless but pointless to enable
                    // otherwise, so skip it rather than pay the extra
                    // detector-construction cost for nothing.
                    #[cfg(feature = "ort")]
                    if async_detect && lookahead_secs > 0.0 && async_detect_dual {
                        let conf = autocam_config.confidence_threshold.unwrap_or(0.10);
                        match (
                            reco_autocam::CpuYoloDetector::with_config(
                                &model_path,
                                conf,
                                Vec::new(),
                            ),
                            reco_autocam::CpuYoloDetector::with_config(
                                &model_path,
                                conf,
                                Vec::new(),
                            ),
                        ) {
                            (Ok(left), Ok(right)) => {
                                let queue_depth =
                                    ((lookahead_secs * info.fps).ceil() as usize).max(2);
                                session.enable_async_detect_dual(
                                    Box::new(left),
                                    Box::new(right),
                                    queue_depth,
                                );
                                println!(
                                    "Autocam: EXPERIMENTAL async detect thread active, dual \
                                     (queue depth {queue_depth})"
                                );
                            }
                            (Err(e), _) | (_, Err(e)) => log::warn!(
                                "--async-detect-dual: could not load both detector \
                                 instances ({e}), continuing with synchronous detection"
                            ),
                        }
                    } else if async_detect && lookahead_secs > 0.0 {
                        match reco_autocam::CpuYoloDetector::with_config(
                            &model_path,
                            autocam_config.confidence_threshold.unwrap_or(0.10),
                            Vec::new(),
                        ) {
                            Ok(inference_detector) => {
                                let queue_depth =
                                    ((lookahead_secs * info.fps).ceil() as usize).max(2);
                                session
                                    .enable_async_detect(Box::new(inference_detector), queue_depth);
                                println!(
                                    "Autocam: EXPERIMENTAL async detect thread active \
                                     (queue depth {queue_depth})"
                                );
                            }
                            Err(e) => log::warn!(
                                "--async-detect: could not load a second detector instance \
                                 ({e}), continuing with synchronous detection"
                            ),
                        }
                    }
                    #[cfg(not(feature = "ort"))]
                    if async_detect {
                        let _ = async_detect_dual;
                        log::warn!(
                            "--async-detect requires --features ort; ignoring (synchronous \
                             detection unaffected)"
                        );
                    }
                }
                Ok(false) => {
                    let msg = "Tracking requested but detection cannot run in zero-copy mode. \
                               Build with --features tensorrt for GPU detection, \
                               or use CPU decode (--no-zero-copy). \
                               Pass --allow-no-tracking to continue without tracking.";
                    if allow_fallback {
                        log::warn!("{msg}");
                    } else {
                        log::error!("{msg}");
                        tracking_failed.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                Err(e) => {
                    let msg = format!(
                        "Autocam setup failed: {e}. \
                                       Pass --allow-no-tracking to continue without tracking."
                    );
                    if allow_fallback {
                        log::warn!("{msg}");
                    } else {
                        log::error!("{msg}");
                        tracking_failed.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        });
    }
    #[cfg(not(feature = "autocam"))]
    if args.model_path.is_some() {
        log::warn!(
            "--model specified but autocam feature is disabled. Build with --features autocam to enable AI tracking."
        );
    }

    let result = job.run(interrupted)?;

    #[cfg(feature = "autocam")]
    if tracking_failed.load(std::sync::atomic::Ordering::Relaxed) {
        anyhow::bail!(
            "Tracking was requested but could not run. \
             Pass --allow-no-tracking to continue without tracking."
        );
    }

    println!(
        "\nDone: {} frames in {:.1}s ({:.1} fps) -> {}",
        result.frames_processed,
        result.elapsed.as_secs_f64(),
        result.fps(),
        args.output
    );

    if let Some(snap) = &result.telemetry {
        let summary = reco_core::telemetry::SessionSummary {
            snapshot: snap.clone(),
        };
        println!("\n{summary}");
    }

    Ok(())
}

fn parse_codec(s: &str) -> reco_io::output::Codec {
    s.parse().unwrap_or_else(|_| {
        log::warn!("Unknown codec '{s}', defaulting to H.264");
        reco_io::output::Codec::H264
    })
}

fn parse_quality(s: &str) -> reco_io::output::Quality {
    s.parse().unwrap_or_else(|_| {
        log::warn!("Unknown quality '{s}', defaulting to balanced");
        reco_io::output::Quality::Balanced
    })
}
