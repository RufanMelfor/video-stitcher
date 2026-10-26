//! Wiring of the minimap core into a reco stitch session.
//!
//! ```text
//! StitchSession --PipelineEvent::DetectionsRaw--> MinimapSink --+
//!                                                               | Arc<Mutex<MinimapEngine>>
//! StitchSession <--OverlayFrameSource::try_frame-- MinimapOverlay+
//! ```
//!
//! Both halves run on the session thread: the session emits a frame's
//! detections right before it renders that frame, so the overlay frame
//! the compositor pulls next always shows the frame being rendered (also
//! with lookahead, where `DetectionsRaw` is emitted at render time with
//! the buffered frame's detections).
//!
//! # Projection
//!
//! A detection's foot point (bottom centre of its box, in its own
//! camera's raw normalized frame) is mapped to panorama yaw/pitch with
//! [`reco_core::projection::camera_to_panorama`], then onto the plane
//! below the camera (`r = 1 / tan(depression)` along the yaw direction).
//! Every ground point is a central projection of that plane, so a single
//! homography maps it to field metres; it is fitted to the
//! [`FieldPoint`]s of the setup file (at least 4, e.g. the corners).
//!
//! # Teams
//!
//! Detections carry a torso colour sample ([`MappedDetection::color`],
//! filled by the wgpu detector wrapper). With [`TeamSetup::Auto`] the
//! first batches are held back while their colours are split into two
//! teams (2-means in Lab), then replayed; each player is then assigned to
//! the nearer shirt colour and drawn in it. Referees (when the model has a
//! `referee` class) and the ball are separate. Without colour samples
//! (other detector backends) every player is drawn in one colour.

use std::path::Path;
use std::sync::{Arc, Mutex};

use reco_core::calibration::Calibration;
use reco_core::detect::director::MappedDetection;
use reco_core::detect::pipeline_event::{PipelineEvent, PipelineEventSink};
use reco_core::geometry::{CameraId, VirtualCamera, rig_frame};
use reco_core::render::overlay::{OverlayFrame, OverlayFrameSource, OverlayPlacement};
use reco_core::render::scene::SceneGeometry;
use serde::{Deserialize, Serialize};

use crate::detect::{Class, Det};
use crate::geom::{Homography, Projector, Vec2};
use crate::render::{Renderer, Theme};
use crate::sport::SportProfile;
use crate::{Config, FrameState, Minimap};

/// Current [`MinimapSetup::version`].
pub const SETUP_VERSION: u32 = 1;

/// Below this depression angle (radians, ~0.3 deg) a ray is treated as
/// not reaching the ground: the plane coordinate would explode.
const MIN_DEPRESSION_RAD: f32 = 0.005;

/// A fit residual (RMS, metres) above this is logged as a likely
/// mis-clicked or mislabelled field point.
const RMS_WARN_M: f32 = 2.0;

/// Shirt colour every player is given without a team split; any colour
/// works as long as it is far from [`AWAY_PLACEHOLDER_RGB`].
const PLAYER_RGB: [u8; 3] = [245, 245, 245];
/// The unused second team colour without a team split, as far from
/// [`PLAYER_RGB`] as possible so the team classifier never hesitates.
const AWAY_PLACEHOLDER_RGB: [u8; 3] = [0, 0, 0];

/// Opacity of the minimap's field background (lines, players and ball are
/// always opaque).
pub const BACKGROUND_OPACITY: f32 = 0.75;

/// Team learning ([`TeamSetup::Auto`]): only confident player boxes whose
/// foot is on the field give colour samples (no bench, no spectators).
const LEARN_MIN_CONFIDENCE: f32 = 0.35;
/// Samples and detection batches needed before the colours are split.
const LEARN_MIN_SAMPLES: usize = 150;
const LEARN_MIN_BATCHES: usize = 10;
/// Give up learning after this many batches (too few samples: e.g. a
/// detector backend that does not sample colours).
const LEARN_MAX_BATCHES: usize = 150;
/// Two shirt colours closer than this (Lab distance) are not two teams.
const MIN_TEAM_SEPARATION: f32 = 20.0;
/// A colour about equally far from both shirts (Lab distance difference
/// below this) is not assigned to a team; such a box can still continue an
/// existing track but cannot start one.
const TEAM_MARGIN: f32 = 6.0;
/// A colour farther than this (Lab distance) from both shirts is neither
/// team's: passed on unchanged, so the core treats the box as a referee
/// (a coach or referee in plain clothes the model calls `person`). Same
/// value as the core's `TeamClassifier::max_dist`.
const TEAM_OUTLIER_DIST: f32 = 45.0;

/// Errors from loading or applying a minimap setup.
#[derive(Debug, thiserror::Error)]
pub enum MinimapError {
    /// The setup file could not be read.
    #[error("cannot read minimap setup {path}: {source}")]
    Io {
        /// The file that failed.
        path: String,
        /// The underlying error.
        source: std::io::Error,
    },
    /// The setup file is not valid JSON for [`MinimapSetup`].
    #[error("invalid minimap setup {path}: {source}")]
    Parse {
        /// The file that failed.
        path: String,
        /// The underlying error.
        source: serde_json::Error,
    },
    /// The setup is well-formed but unusable.
    #[error("invalid minimap setup: {0}")]
    Invalid(String),
}

/// Which camera a [`FieldPoint`] was picked in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SetupCamera {
    /// The left camera.
    Left,
    /// The right camera.
    Right,
}

impl From<SetupCamera> for CameraId {
    fn from(c: SetupCamera) -> Self {
        match c {
            SetupCamera::Left => CameraId::Left,
            SetupCamera::Right => CameraId::Right,
        }
    }
}

/// One known pitch point: where it is in a camera frame, and where it is
/// on the field.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FieldPoint {
    /// Camera the point was picked in.
    pub camera: SetupCamera,
    /// Horizontal position in that camera's raw (distorted) frame,
    /// normalized `[0, 1]` - the same space as the calibration's field ROI.
    pub x: f64,
    /// Vertical position in that camera's raw frame, normalized `[0, 1]`.
    pub y: f64,
    /// Field coordinate along the length, metres: `0` = left goal line
    /// (as seen in the panorama), `field_length_m` = right goal line.
    pub field_x: f32,
    /// Field coordinate across, metres: `0` = far sideline,
    /// `field_width_m` = near sideline (the camera side).
    pub field_y: f32,
}

/// Screen corner the minimap is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Corner {
    /// Top left.
    TopLeft,
    /// Top right.
    TopRight,
    /// Bottom left.
    BottomLeft,
    /// Bottom right.
    BottomRight,
}

impl Corner {
    /// Every corner, in the order a picker lists them.
    pub const ALL: [Corner; 4] = [
        Corner::TopLeft,
        Corner::TopRight,
        Corner::BottomLeft,
        Corner::BottomRight,
    ];

    /// Stable name (`top_left` ...), as used in CLI flags and settings.
    pub fn name(self) -> &'static str {
        match self {
            Corner::TopLeft => "top_left",
            Corner::TopRight => "top_right",
            Corner::BottomLeft => "bottom_left",
            Corner::BottomRight => "bottom_right",
        }
    }

    /// Inverse of [`Self::name`].
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.name() == name)
    }
}

/// Where and how large the minimap appears in the output video - an
/// export setting, independent of the match's `minimap.json`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MinimapPlacement {
    /// Screen corner.
    pub corner: Corner,
    /// Minimap width as a fraction of the output width, `[0.05, 0.6]`.
    pub width_fraction: f32,
    /// Gap to the frame edges as a fraction of the output height, `[0, 0.2]`.
    pub margin_fraction: f32,
}

impl MinimapPlacement {
    /// Accepted [`Self::width_fraction`] range.
    pub const WIDTH_RANGE: std::ops::RangeInclusive<f32> = 0.05..=0.6;
    /// Accepted [`Self::margin_fraction`] range.
    pub const MARGIN_RANGE: std::ops::RangeInclusive<f32> = 0.0..=0.2;

    /// Check ranges.
    pub fn validate(&self) -> Result<(), MinimapError> {
        if !Self::WIDTH_RANGE.contains(&self.width_fraction) {
            return Err(MinimapError::Invalid(format!(
                "minimap width {} is outside {:?} of the output width",
                self.width_fraction,
                Self::WIDTH_RANGE
            )));
        }
        if !Self::MARGIN_RANGE.contains(&self.margin_fraction) {
            return Err(MinimapError::Invalid(format!(
                "minimap margin {} is outside {:?} of the output height",
                self.margin_fraction,
                Self::MARGIN_RANGE
            )));
        }
        Ok(())
    }
}

/// The `minimap.json` setup file: field size, the projection's field
/// points and how teams are told apart - everything that belongs to the
/// match and the camera position. Where the minimap is drawn is an export
/// setting ([`MinimapPlacement`]). Every field is required - there are no
/// hidden defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MinimapSetup {
    /// Format version, must be [`SETUP_VERSION`].
    pub version: u32,
    /// Field length (goal line to goal line), metres.
    pub field_length_m: f32,
    /// Field width (sideline to sideline), metres.
    pub field_width_m: f32,
    /// Players per team on the field (11, 9, 8, 6 ...).
    pub players_per_team: usize,
    /// Referee slots (only used when the model has a `referee` class).
    pub referees: usize,
    /// Known field points, at least 4, not all on one line.
    pub points: Vec<FieldPoint>,
    /// How players are split into teams.
    pub teams: TeamSetup,
}

/// How players are split into teams (`"teams"` in `minimap.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum TeamSetup {
    /// Learn the two shirt colours from the first seconds of the export.
    Auto,
    /// Fixed shirt colours as `#rrggbb`.
    Colors {
        /// One team's shirt colour.
        home: String,
        /// The other team's shirt colour.
        away: String,
    },
    /// No team split: every player in one colour.
    Off,
}

impl MinimapSetup {
    /// Load and validate a setup file.
    pub fn from_file(path: &Path) -> Result<Self, MinimapError> {
        let text = std::fs::read_to_string(path).map_err(|source| MinimapError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let setup: Self = serde_json::from_str(&text).map_err(|source| MinimapError::Parse {
            path: path.display().to_string(),
            source,
        })?;
        setup.validate()?;
        Ok(setup)
    }

    /// Check ranges; [`Self::from_file`] calls this.
    pub fn validate(&self) -> Result<(), MinimapError> {
        let bad = |msg: String| Err(MinimapError::Invalid(msg));
        if self.version != SETUP_VERSION {
            return bad(format!(
                "version {} is not supported (expected {SETUP_VERSION})",
                self.version
            ));
        }
        let size_ok = |v: f32| v.is_finite() && (5.0..=150.0).contains(&v);
        if !size_ok(self.field_length_m) || !size_ok(self.field_width_m) {
            return bad(format!(
                "field size {}x{} m is outside 5..150 m",
                self.field_length_m, self.field_width_m
            ));
        }
        if !(1..=15).contains(&self.players_per_team) {
            return bad(format!(
                "players_per_team {} is outside 1..15",
                self.players_per_team
            ));
        }
        if self.referees > 6 {
            return bad(format!("referees {} is above 6", self.referees));
        }
        if self.points.len() < 4 {
            return bad(format!(
                "{} field points given, at least 4 are needed",
                self.points.len()
            ));
        }
        for (i, p) in self.points.iter().enumerate() {
            let in_frame = |v: f64| v.is_finite() && (0.0..=1.0).contains(&v);
            let margin = 5.0;
            let on_field = |v: f32, max: f32| v.is_finite() && v >= -margin && v <= max + margin;
            if !in_frame(p.x) || !in_frame(p.y) {
                return bad(format!(
                    "point {i}: camera position ({}, {}) is outside 0..1",
                    p.x, p.y
                ));
            }
            if !on_field(p.field_x, self.field_length_m) || !on_field(p.field_y, self.field_width_m)
            {
                return bad(format!(
                    "point {i}: field position ({}, {}) is not on the {}x{} m field",
                    p.field_x, p.field_y, self.field_length_m, self.field_width_m
                ));
            }
        }
        if let TeamSetup::Colors { home, away } = &self.teams {
            for (name, hex) in [("home", home), ("away", away)] {
                if crate::colors::parse_hex(hex).is_none() {
                    return bad(format!("teams.{name} \"{hex}\" is not a #rrggbb colour"));
                }
            }
        }
        Ok(())
    }

    /// Football field markings scaled to this setup's field size, with
    /// the setup's team and referee counts.
    pub fn sport_profile(&self) -> SportProfile {
        let base = SportProfile::football();
        let sx = self.field_length_m / base.field.length;
        let sy = self.field_width_m / base.field.width;
        let scale = |p: Vec2| Vec2::new(p.x * sx, p.y * sy);
        SportProfile {
            field: crate::geom::Field::new(self.field_length_m, self.field_width_m),
            team_size: self.players_per_team,
            referees: self.referees,
            lines: base
                .lines
                .iter()
                .map(|l| l.iter().copied().map(scale).collect())
                .collect(),
            landmarks: base
                .landmarks
                .iter()
                .map(|l| crate::sport::Landmark {
                    key: l.key.clone(),
                    x: l.x * sx,
                    y: l.y * sy,
                })
                .collect(),
            goals: base.goals.iter().copied().map(scale).collect(),
            ..base
        }
    }
}

/// Model class ids the minimap consumes, resolved from the model's own
/// label list - never assumed (reco's football checkpoints use
/// `0=person, 1=ball, 2=referee`, COCO exports `0=person, 32=sports ball`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassMap {
    /// Player class (`person` / `player`).
    pub player: u16,
    /// Ball class (`ball` / `sports ball`), if the model has one.
    pub ball: Option<u16>,
    /// Referee class, if the model has one.
    pub referee: Option<u16>,
}

impl ClassMap {
    /// Resolve from model labels (case-insensitive). `None` when the
    /// model has no player class - a minimap without players is useless.
    pub fn from_names(names: &[String]) -> Option<Self> {
        let find = |candidates: &[&str]| {
            names
                .iter()
                .position(|n| candidates.iter().any(|c| n.eq_ignore_ascii_case(c)))
                .and_then(|i| u16::try_from(i).ok())
        };
        Some(Self {
            player: find(&["person", "player"])?,
            ball: find(&["ball", "sports ball"]),
            referee: find(&["referee"]),
        })
    }

    fn class_of(&self, id: u16) -> Option<Class> {
        if id == self.player {
            Some(Class::Player)
        } else if Some(id) == self.ball {
            Some(Class::Ball)
        } else if Some(id) == self.referee {
            Some(Class::Referee)
        } else {
            None
        }
    }
}

/// Camera frame -> field metres through the stitch geometry (see the
/// module doc's "Projection").
///
/// As a [`Projector`], `u` is a side-by-side camera coordinate: `[0, 1)`
/// is the left camera's normalized x, `[1, 2]` the right camera's plus 1.
pub struct RecoProjector {
    ground: GroundFrame,
    to_field: Homography,
    rms_error_m: f32,
}

impl RecoProjector {
    /// Fit the projection to `setup`'s field points.
    pub fn new(calibration: &Calibration, setup: &MinimapSetup) -> Result<Self, MinimapError> {
        let ground = GroundFrame::new(calibration);
        let mut plane = Vec::with_capacity(setup.points.len());
        let mut field = Vec::with_capacity(setup.points.len());
        for (i, p) in setup.points.iter().enumerate() {
            let Some(g) = ground.project(p.camera.into(), p.x as f32, p.y as f32) else {
                return Err(MinimapError::Invalid(format!(
                    "point {i} ({:?} camera at {:.3}, {:.3}) is not below the horizon",
                    p.camera, p.x, p.y
                )));
            };
            plane.push(g);
            field.push(Vec2::new(p.field_x, p.field_y));
        }
        let to_field = Homography::from_points(&plane, &field).ok_or_else(|| {
            MinimapError::Invalid("field points are degenerate (e.g. all on one line)".into())
        })?;
        let mut sq = 0.0f32;
        for (i, (g, f)) in plane.iter().zip(&field).enumerate() {
            let err = to_field.apply(*g).map_or(f32::INFINITY, |m| m.dist(*f));
            log::debug!("minimap field point {i}: residual {err:.2} m");
            sq += err * err;
        }
        let rms_error_m = (sq / plane.len() as f32).sqrt();
        if rms_error_m > RMS_WARN_M {
            log::warn!(
                "Minimap: field points fit with {rms_error_m:.1} m RMS error - a point is                  probably mis-clicked or has the wrong field coordinates"
            );
        }
        Ok(Self {
            ground,
            to_field,
            rms_error_m,
        })
    }

    /// RMS residual of the field points after the fit, metres. Exactly
    /// 0 with 4 points (a homography has 8 degrees of freedom); only more
    /// points make this a real check.
    pub fn rms_error_m(&self) -> f32 {
        self.rms_error_m
    }

    /// Field position of a point in a camera's raw normalized frame.
    pub fn camera_to_field(&self, camera: CameraId, x: f32, y: f32) -> Option<Vec2> {
        self.to_field.apply(self.ground.project(camera, x, y)?)
    }
}

impl Projector for RecoProjector {
    fn to_court(&self, u: f32, v: f32) -> Option<Vec2> {
        if u < 1.0 {
            self.camera_to_field(CameraId::Left, u, v)
        } else {
            self.camera_to_field(CameraId::Right, u - 1.0, v)
        }
    }
}

/// Camera point -> coordinate on the level plane below the camera.
///
/// Panorama yaw/pitch are measured in the tilted rig frame, so the true
/// up direction comes from [`reco_core::geometry::rig_frame`] (the same
/// frame the renderer levels the horizon with).
struct GroundFrame {
    calibration: Calibration,
    scene: SceneGeometry,
    camera: VirtualCamera,
    up: [f32; 3],
    e1: [f32; 3],
    e2: [f32; 3],
}

impl GroundFrame {
    fn new(calibration: &Calibration) -> Self {
        let lens = &calibration.lenses[0];
        let aspect = lens.width as f32 / lens.height as f32;
        let scene = SceneGeometry::new(&calibration.topology, &calibration.framing, aspect);
        let camera = VirtualCamera::new(&scene.camera_position);
        let (up, forward) = rig_frame(
            &camera,
            calibration.framing.tilt as f32,
            calibration.framing.roll as f32,
        );
        let up = up.normalize();
        // Any orthonormal pair perpendicular to `up` works: the field
        // homography absorbs the in-plane rotation.
        let e1 = (forward - up * forward.dot(&up)).normalize();
        let e2 = up.cross(&e1);
        Self {
            calibration: calibration.clone(),
            scene,
            camera,
            up: [up.x, up.y, up.z],
            e1: [e1.x, e1.y, e1.z],
            e2: [e2.x, e2.y, e2.z],
        }
    }

    fn project(&self, camera: CameraId, x: f32, y: f32) -> Option<Vec2> {
        let pos = reco_core::projection::camera_to_panorama(
            camera,
            x,
            y,
            &self.calibration,
            &self.scene,
        )?;
        let d = self.camera.yaw_pitch_to_direction(pos.yaw, pos.pitch);
        let dot = |a: [f32; 3]| d.x * a[0] + d.y * a[1] + d.z * a[2];
        let down = -dot(self.up);
        if down < MIN_DEPRESSION_RAD.sin() {
            return None;
        }
        Some(Vec2::new(dot(self.e1) / down, dot(self.e2) / down))
    }
}

/// Side-by-side camera x (see [`RecoProjector`]).
fn side_by_side_x(camera: CameraId, x: f32) -> f32 {
    match camera {
        CameraId::Left => x,
        CameraId::Right => x + 1.0,
    }
}

/// Team split state of a [`MinimapEngine`].
enum Teams {
    /// One colour for every player.
    Single,
    /// Collecting shirt colours; the batches are replayed once decided.
    Learning {
        samples: Vec<[u8; 3]>,
        held: Vec<(u64, Vec<MappedDetection>)>,
    },
    /// Two shirt colours.
    Split { home: [u8; 3], away: [u8; 3] },
}

/// Tracking + rendering state shared by [`MinimapSink`] and [`MinimapOverlay`].
pub struct MinimapEngine {
    setup: MinimapSetup,
    /// Built once the team colours are known (immediately unless
    /// [`TeamSetup::Auto`]); holds the projector from then on.
    minimap: Option<Minimap>,
    /// The projector until `minimap` takes it over.
    projector: Option<RecoProjector>,
    teams: Teams,
    renderer: Renderer,
    classes: ClassMap,
    /// Source frame rate; `None` until [`MinimapHandle::set_fps`].
    fps: Option<f64>,
    fps_missing_logged: bool,
    placement: OverlayPlacement,
    /// The last batch fed to the tracker, to skip the repeats the session
    /// emits between detection runs (`--detection-interval`).
    last_batch: Vec<MappedDetection>,
    pending: Option<OverlayFrame>,
}

impl MinimapEngine {
    /// Build for one export.
    ///
    /// `output_size` is the encoded video's resolution (the minimap is
    /// rendered at exactly the pixel size it is shown at). The source
    /// frame rate is set later through [`MinimapHandle::set_fps`], once
    /// the session's source is open; detections before that are skipped.
    pub fn new(
        setup: &MinimapSetup,
        placement: MinimapPlacement,
        calibration: &Calibration,
        classes: ClassMap,
        output_size: (u32, u32),
    ) -> Result<Self, MinimapError> {
        setup.validate()?;
        placement.validate()?;
        if output_size.0 == 0 || output_size.1 == 0 {
            return Err(MinimapError::Invalid("output size must be non-zero".into()));
        }
        let projector = RecoProjector::new(calibration, setup)?;
        log::info!(
            "Minimap: {}x{} m field, {} field points, fit RMS {:.2} m, classes {:?}, teams {:?}",
            setup.field_length_m,
            setup.field_width_m,
            setup.points.len(),
            projector.rms_error_m(),
            classes,
            setup.teams
        );

        let profile = setup.sport_profile();
        let target_w = (output_size.0 as f32 * placement.width_fraction).round();
        let mut theme = Theme {
            px_per_m: 1.0,
            floor: [24, 92, 48],
            lines: [235, 240, 235],
            home: PLAYER_RGB,
            away: PLAYER_RGB,
            referee: [250, 205, 0],
            ball: [255, 120, 0],
            background_alpha: (BACKGROUND_OPACITY * 255.0).round() as u8,
            min_alpha: 0.5,
            // Upstream sizes target a full-window basketball court; a
            // quarter-width football overlay needs bigger markers.
            marker_scale: 1.6,
            ..Theme::default()
        };
        // Renderer::size is linear in px_per_m: measure at 1 px/m, scale.
        let unit_w = Renderer::with_profile(theme.clone(), profile.clone())
            .size()
            .0 as f32;
        theme.px_per_m = (target_w / unit_w.max(1.0)).max(0.5);
        let renderer = Renderer::with_profile(theme, profile);
        let (w, h) = renderer.size();
        let overlay_placement = corner_placement(
            (w as u32, h as u32),
            output_size,
            placement.corner,
            placement.margin_fraction,
        );

        let mut engine = Self {
            setup: setup.clone(),
            minimap: None,
            projector: Some(projector),
            teams: Teams::Single,
            renderer,
            classes,
            fps: None,
            fps_missing_logged: false,
            placement: overlay_placement,
            last_batch: Vec::new(),
            pending: None,
        };
        match &setup.teams {
            TeamSetup::Off => engine.start_tracking(Teams::Single),
            TeamSetup::Colors { home, away } => {
                // validate() checked both parse.
                let parse = |h: &str| crate::colors::parse_hex(h).unwrap_or(PLAYER_RGB);
                engine.start_tracking(Teams::Split {
                    home: parse(home),
                    away: parse(away),
                });
            }
            TeamSetup::Auto => {
                engine.teams = Teams::Learning {
                    samples: Vec::new(),
                    held: Vec::new(),
                }
            }
        }
        // Show the empty field from the first frame on, not only once the
        // first detections arrive.
        let empty = FrameState {
            t: 0.0,
            players: Vec::new(),
            referees: Vec::new(),
            ball: crate::ball::BallOut {
                state: crate::ball::BallState::Unknown,
                pos: None,
                vel: None,
            },
            events: Vec::new(),
        };
        engine.pending = Some(engine.render(&empty));
        Ok(engine)
    }

    /// Build the tracker for `teams` (`Single` or `Split`).
    fn start_tracking(&mut self, teams: Teams) {
        let Some(projector) = self.projector.take() else {
            return;
        };
        let profile = self.setup.sport_profile();
        let mut cfg = match teams {
            Teams::Split { home, away } => {
                log::info!(
                    "Minimap: team colours {} and {}",
                    crate::colors::hex(home),
                    crate::colors::hex(away)
                );
                self.renderer.theme.home = display_color(home);
                self.renderer.theme.away = display_color(away);
                let mut cfg = Config::for_sport(&profile, home, away);
                // A box whose colour is neither shirt (see `to_dets`)
                // becomes a referee.
                cfg.referee_from_color = true;
                cfg
            }
            _ => {
                let mut cfg = Config::for_sport(&profile, PLAYER_RGB, AWAY_PLACEHOLDER_RGB);
                // Both teams share one group of slots.
                cfg.tracker.max_per_team = 2 * self.setup.players_per_team;
                cfg.referee_from_color = false;
                cfg
            }
        };
        // No goalkeeper kits are known; a keeper in a third colour is
        // treated like any colour that is neither shirt (see `to_dets`).
        cfg.keepers_enabled = false;
        self.minimap = Some(Minimap::new(cfg, Box::new(projector)));
        self.teams = teams;
    }

    fn set_fps(&mut self, fps: f64) -> Result<(), MinimapError> {
        if !(fps.is_finite() && fps > 0.0) {
            return Err(MinimapError::Invalid(format!("fps {fps} must be positive")));
        }
        self.fps = Some(fps);
        Ok(())
    }

    /// Placement to register the overlay layer with.
    pub fn placement(&self) -> OverlayPlacement {
        self.placement
    }

    /// Feed one frame's detections. Repeats of the previous batch (frames
    /// between detection runs) and empty batches are skipped: the tracker
    /// then keeps its last state instead of seeing everyone stand still.
    pub fn on_detections(&mut self, frame_index: u64, detections: &[MappedDetection]) {
        if self.fps.is_none() {
            if !self.fps_missing_logged {
                log::warn!("Minimap: frame rate not set yet, detections skipped");
                self.fps_missing_logged = true;
            }
            return;
        }
        if detections.is_empty() || same_batch(&self.last_batch, detections) {
            return;
        }
        self.last_batch = detections.to_vec();
        if matches!(self.teams, Teams::Learning { .. }) {
            self.learn(frame_index, detections);
            return;
        }
        self.track(frame_index, detections);
    }

    /// Collect shirt colours from one batch; once enough are in, split
    /// them into two teams and replay the held batches.
    fn learn(&mut self, frame_index: u64, detections: &[MappedDetection]) {
        let field = crate::geom::Field::new(self.setup.field_length_m, self.setup.field_width_m);
        let player = self.classes.player;
        let Teams::Learning { samples, held } = &mut self.teams else {
            return;
        };
        if let Some(projector) = &self.projector {
            for d in detections {
                let Some(color) = d.color else { continue };
                if d.class_id != player || d.confidence < LEARN_MIN_CONFIDENCE {
                    continue;
                }
                let foot = (d.camera_center.0, d.camera_center.1 + d.camera_size.1 / 2.0);
                if projector
                    .camera_to_field(d.camera, foot.0, foot.1)
                    .is_some_and(|p| field.contains(p, 0.0))
                {
                    samples.push(color);
                }
            }
        }
        held.push((frame_index, detections.to_vec()));
        let enough = samples.len() >= LEARN_MIN_SAMPLES && held.len() >= LEARN_MIN_BATCHES;
        if !enough && held.len() < LEARN_MAX_BATCHES {
            return;
        }
        let split = if enough {
            crate::colors::pick_teams(samples)
        } else {
            None
        };
        let teams = match split {
            Some((a, b, dist)) if dist >= MIN_TEAM_SEPARATION => Teams::Split {
                home: a.rgb,
                away: b.rgb,
            },
            Some((_, _, dist)) => {
                log::warn!(
                    "Minimap: shirt colours too similar to tell teams apart \
                     (Lab distance {dist:.0}), drawing all players in one colour"
                );
                Teams::Single
            }
            None => {
                log::warn!(
                    "Minimap: only {} shirt colour samples in {} detection batches \
                     (does this detector backend sample colours?), drawing all players \
                     in one colour",
                    samples.len(),
                    held.len()
                );
                Teams::Single
            }
        };
        let held = std::mem::take(held);
        self.start_tracking(teams);
        for (index, batch) in &held {
            self.track(*index, batch);
        }
    }

    fn track(&mut self, frame_index: u64, detections: &[MappedDetection]) {
        let (Some(fps), Some(_)) = (self.fps, self.minimap.as_ref()) else {
            return;
        };
        let dets = self.to_dets(detections);
        let t = frame_index as f64 / fps;
        let state = self
            .minimap
            .as_mut()
            .expect("checked above")
            .process(t, &dets);
        self.pending = Some(self.render(&state));
    }

    /// Convert to the core's detections. A player whose colour is clearly
    /// nearer one shirt gets that shirt's exact colour, so the core's
    /// classifier never hesitates over a shaded shirt; a colour about
    /// equally far from both gets none (it can only continue a track); a
    /// colour far from both is passed on as is, which the core treats as
    /// a referee.
    fn to_dets(&self, detections: &[MappedDetection]) -> Vec<Det> {
        detections
            .iter()
            .filter_map(|d| {
                let class = self.classes.class_of(d.class_id)?;
                let mut det = Det::new(
                    class,
                    d.confidence,
                    side_by_side_x(d.camera, d.camera_center.0),
                    d.camera_center.1,
                    d.camera_size.0,
                    d.camera_size.1,
                );
                if class == Class::Player {
                    det.color = match self.teams {
                        Teams::Split { home, away } => d.color.and_then(|c| {
                            let (dh, da) = (
                                crate::colors::lab_dist(c, home),
                                crate::colors::lab_dist(c, away),
                            );
                            if dh.min(da) > TEAM_OUTLIER_DIST {
                                Some(c)
                            } else if (dh - da).abs() < TEAM_MARGIN {
                                None
                            } else if dh < da {
                                Some(home)
                            } else {
                                Some(away)
                            }
                        }),
                        _ => Some(PLAYER_RGB),
                    };
                }
                Some(det)
            })
            .collect()
    }

    fn render(&self, state: &FrameState) -> OverlayFrame {
        let canvas = self.renderer.render(state);
        let (w, h) = (canvas.w as u32, canvas.h as u32);
        OverlayFrame {
            width: w,
            height: h,
            design_size: (w, h),
            rgba: canvas.rgba,
        }
    }

    /// The newest rendered minimap, once.
    pub fn take_frame(&mut self) -> Option<OverlayFrame> {
        self.pending.take()
    }
}

/// A measured shirt colour made readable on the minimap: shirts are
/// measured in shade or backlight, so they come out darker and greyer
/// than they look; lift brightness and saturation, keep the hue.
fn display_color(rgb: [u8; 3]) -> [u8; 3] {
    let [r, g, b] = rgb.map(|v| v as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let v = max;
    let s = if max > 0.0 { (max - min) / max } else { 0.0 };
    let h = if max == min {
        0.0
    } else if max == r {
        ((g - b) / (max - min)).rem_euclid(6.0)
    } else if max == g {
        (b - r) / (max - min) + 2.0
    } else {
        (r - g) / (max - min) + 4.0
    };
    let s = (s * 1.6).min(1.0);
    let v = v.max(0.85);
    let c = v * s;
    let x = c * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [r1, g1, b1].map(|u| ((u + m) * 255.0).round().clamp(0.0, 255.0) as u8)
}

fn same_batch(a: &[MappedDetection], b: &[MappedDetection]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.camera == y.camera
                && x.class_id == y.class_id
                && x.confidence == y.confidence
                && x.camera_center == y.camera_center
                && x.camera_size == y.camera_size
        })
}

/// Placement that puts a `design`-sized overlay, shown 1:1, in `corner`.
fn corner_placement(
    design: (u32, u32),
    output: (u32, u32),
    corner: Corner,
    margin_fraction: f32,
) -> OverlayPlacement {
    let (dw, dh) = (design.0.max(1) as f32, design.1.max(1) as f32);
    let (ow, oh) = (output.0.max(1) as f32, output.1.max(1) as f32);
    // The compositor contain-fits the design size, then multiplies by
    // `scale`; undo the fit so the minimap lands at its own pixel size,
    // shrunk only if it would not fit.
    let fit = (ow / dw).min(oh / dh);
    let shown = (dw.min(ow) / dw).min(dh.min(oh) / dh);
    let scale = shown / fit;
    let (sw, sh) = (dw * shown, dh * shown);
    let m = margin_fraction * oh;
    let cx = match corner {
        Corner::TopLeft | Corner::BottomLeft => m + sw / 2.0,
        Corner::TopRight | Corner::BottomRight => ow - m - sw / 2.0,
    };
    let cy = match corner {
        Corner::TopLeft | Corner::TopRight => m + sh / 2.0,
        Corner::BottomLeft | Corner::BottomRight => oh - m - sh / 2.0,
    };
    OverlayPlacement {
        offset: ((cx - ow / 2.0) / ow, (cy - oh / 2.0) / oh),
        scale,
    }
}

/// Pipeline-event sink half: feeds `DetectionsRaw` to the engine. To keep
/// the `--events` JSONL writer as well, combine both with
/// [`reco_core::detect::pipeline_event::TeeSink`] (`StitchJob::event_sink`
/// does this).
pub struct MinimapSink {
    engine: Arc<Mutex<MinimapEngine>>,
}

impl PipelineEventSink for MinimapSink {
    fn emit(&mut self, event: PipelineEvent) {
        if let PipelineEvent::DetectionsRaw {
            frame_index,
            ref detections,
        } = event
        {
            match self.engine.lock() {
                Ok(mut engine) => engine.on_detections(frame_index, detections),
                Err(_) => log::warn!("Minimap: engine lock poisoned, frame {frame_index} skipped"),
            }
        }
    }
}

/// Overlay half: hands the compositor each new minimap frame.
pub struct MinimapOverlay {
    engine: Arc<Mutex<MinimapEngine>>,
}

impl OverlayFrameSource for MinimapOverlay {
    fn try_frame(&mut self) -> Result<Option<OverlayFrame>, String> {
        self.engine
            .lock()
            .map(|mut e| e.take_frame())
            .map_err(|_| "minimap engine lock poisoned".to_string())
    }
}

/// Control half: settings only known once the session is open.
#[derive(Clone)]
pub struct MinimapHandle {
    engine: Arc<Mutex<MinimapEngine>>,
}

impl MinimapHandle {
    /// Set the source frame rate (frame index -> seconds). Detections
    /// are skipped until this is called.
    pub fn set_fps(&self, fps: f64) -> Result<(), MinimapError> {
        self.engine
            .lock()
            .map_err(|_| MinimapError::Invalid("minimap engine lock poisoned".into()))?
            .set_fps(fps)
    }
}

/// Split an engine into its session-facing halves. Register the sink with
/// `StitchSession::set_event_sink` (or `StitchJob::event_sink`), the
/// overlay as an overlay layer at [`MinimapEngine::placement`], and call
/// [`MinimapHandle::set_fps`] once the source is open.
pub fn attach(engine: MinimapEngine) -> (MinimapSink, MinimapOverlay, MinimapHandle) {
    let engine = Arc::new(Mutex::new(engine));
    (
        MinimapSink {
            engine: engine.clone(),
        },
        MinimapOverlay {
            engine: engine.clone(),
        },
        MinimapHandle { engine },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn class_map_reads_labels_not_positions() {
        let reco = ClassMap::from_names(&names(&["person", "ball", "referee"])).unwrap();
        assert_eq!(
            reco,
            ClassMap {
                player: 0,
                ball: Some(1),
                referee: Some(2)
            }
        );
        let mut coco = vec!["x".to_string(); 40];
        coco[0] = "person".into();
        coco[32] = "sports ball".into();
        let coco = ClassMap::from_names(&coco).unwrap();
        assert_eq!((coco.player, coco.ball, coco.referee), (0, Some(32), None));
        assert!(ClassMap::from_names(&names(&["ball"])).is_none());
    }

    #[test]
    fn corner_placement_lands_in_the_corner_at_pixel_size() {
        let p = corner_placement((640, 400), (2560, 1440), Corner::BottomRight, 0.0);
        // contain-fit of 640x400 into 2560x1440 is 3.6x; 1:1 needs scale 1/3.6
        assert!((p.scale - 1.0 / 3.6).abs() < 1e-4);
        // centre 320 px from the right edge, 200 px from the bottom
        assert!((p.offset.0 - (2560.0 - 320.0 - 1280.0) / 2560.0).abs() < 1e-5);
        assert!((p.offset.1 - (1440.0 - 200.0 - 720.0) / 1440.0).abs() < 1e-5);
        let tl = corner_placement((640, 400), (2560, 1440), Corner::TopLeft, 0.0);
        assert!(tl.offset.0 < 0.0 && tl.offset.1 < 0.0);
    }

    fn setup() -> MinimapSetup {
        MinimapSetup {
            version: SETUP_VERSION,
            field_length_m: 64.0,
            field_width_m: 42.5,
            players_per_team: 8,
            referees: 1,
            points: vec![
                FieldPoint {
                    camera: SetupCamera::Left,
                    x: 0.5,
                    y: 0.5,
                    field_x: 0.0,
                    field_y: 0.0,
                };
                4
            ],
            teams: TeamSetup::Auto,
        }
    }

    #[test]
    fn setup_validation_rejects_bad_values() {
        assert!(setup().validate().is_ok());
        let mut s = setup();
        s.points.pop();
        assert!(s.validate().is_err(), "3 points");
        let mut s = setup();
        s.points[0].x = 1.5;
        assert!(s.validate().is_err(), "camera x outside the frame");
        let mut s = setup();
        s.points[0].field_x = 80.0;
        assert!(s.validate().is_err(), "point off the field");
        let mut s = setup();
        s.version = 2;
        assert!(s.validate().is_err(), "unknown version");
        let mut s = setup();
        s.teams = TeamSetup::Colors {
            home: "#ff8000".into(),
            away: "blue".into(),
        };
        assert!(s.validate().is_err(), "colour that is not #rrggbb");
    }

    #[test]
    fn setup_requires_every_field() {
        let json = r#"{"version":1,"field_length_m":64,"field_width_m":42.5,"points":[]}"#;
        assert!(serde_json::from_str::<MinimapSetup>(json).is_err());
    }

    #[test]
    fn display_color_keeps_hue_and_lifts_dark_shirts() {
        let orange = display_color([168, 77, 64]);
        assert!(orange[0] > 200 && orange[0] > orange[1] && orange[1] > orange[2]);
        let navy = display_color([40, 50, 90]);
        assert!(navy[2] > 200 && navy[2] > navy[0], "{navy:?}");
        let grey = display_color([87, 81, 79]);
        assert!(grey.iter().all(|&v| v > 180), "{grey:?}");
    }

    #[test]
    fn team_modes_parse() {
        let base = r#""version":1,"field_length_m":64,"field_width_m":42.5,"players_per_team":8,"referees":1,"points":[]"#;
        let parse = |teams: &str| {
            serde_json::from_str::<MinimapSetup>(&format!("{{{base},\"teams\":{teams}}}"))
                .map(|s| s.teams)
        };
        assert_eq!(parse(r#"{"mode":"auto"}"#).unwrap(), TeamSetup::Auto);
        assert_eq!(parse(r#"{"mode":"off"}"#).unwrap(), TeamSetup::Off);
        assert_eq!(
            parse(r##"{"mode":"colors","home":"#ff8000","away":"#2040c0"}"##).unwrap(),
            TeamSetup::Colors {
                home: "#ff8000".into(),
                away: "#2040c0".into()
            }
        );
        assert!(parse(r#"{"mode":"colors"}"#).is_err(), "colours missing");
    }

    #[test]
    fn placement_validation_and_corner_names() {
        let ok = MinimapPlacement {
            corner: Corner::BottomRight,
            width_fraction: 0.25,
            margin_fraction: 0.02,
        };
        assert!(ok.validate().is_ok());
        assert!(
            MinimapPlacement {
                width_fraction: 0.9,
                ..ok
            }
            .validate()
            .is_err()
        );
        assert!(
            MinimapPlacement {
                margin_fraction: -0.1,
                ..ok
            }
            .validate()
            .is_err()
        );
        for c in Corner::ALL {
            assert_eq!(Corner::from_name(c.name()), Some(c));
        }
        assert_eq!(Corner::from_name("middle"), None);
    }

    #[test]
    fn profile_scales_football_markings_to_the_field() {
        let p = setup().sport_profile();
        assert_eq!((p.field.length, p.field.width), (64.0, 42.5));
        assert_eq!(p.team_size, 8);
        let max_x = p
            .lines
            .iter()
            .flatten()
            .map(|v| v.x)
            .fold(f32::MIN, f32::max);
        assert!((max_x - 64.0).abs() < 1e-3);
    }

    #[test]
    fn repeated_batches_are_recognised() {
        let d = MappedDetection {
            camera: CameraId::Left,
            class_id: 0,
            confidence: 0.5,
            camera_center: (0.2, 0.3),
            camera_size: (0.01, 0.03),
            position: None,
            color: None,
        };
        let mut e = d;
        assert!(same_batch(&[d], &[e]));
        e.confidence = 0.6;
        assert!(!same_batch(&[d], &[e]));
        assert!(!same_batch(&[d], &[d, d]));
    }
}
