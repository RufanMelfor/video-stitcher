//! RF-DETR output decoding, tiling and preprocessing.
//!
//! Model contract `rf-detr-imagenet-v1` (from the Reco Trainer `runtime.json`):
//! input `[1,3,512,512]` RGB, outputs `dets [1,300,4]` + `labels [1,300,4]`.
//! DETR style: no NMS, one box per query, sigmoid scores, classes
//! `[ball, player, referee, ignore]`.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Class {
    Ball,
    Player,
    Referee,
}

/// A detection in *normalized panorama* coordinates.
#[derive(Clone, Copy, Debug)]
pub struct Det {
    pub class: Class,
    pub conf: f32,
    pub cx: f32,
    pub cy: f32,
    pub w: f32,
    pub h: f32,
    /// Mean torso colour (RGB) if the caller sampled it; used for team assignment.
    pub color: Option<[u8; 3]>,
}

impl Det {
    pub fn new(class: Class, conf: f32, cx: f32, cy: f32, w: f32, h: f32) -> Self {
        Self {
            class,
            conf,
            cx,
            cy,
            w,
            h,
            color: None,
        }
    }
    /// Foot point (bottom centre of the box), used for ground projection.
    pub fn foot(&self) -> (f32, f32) {
        (self.cx, self.cy + self.h * 0.5)
    }
    pub fn iou(&self, o: &Det) -> f32 {
        let (ax0, ay0, ax1, ay1) = (
            self.cx - self.w / 2.0,
            self.cy - self.h / 2.0,
            self.cx + self.w / 2.0,
            self.cy + self.h / 2.0,
        );
        let (bx0, by0, bx1, by1) = (
            o.cx - o.w / 2.0,
            o.cy - o.h / 2.0,
            o.cx + o.w / 2.0,
            o.cy + o.h / 2.0,
        );
        let iw = (ax1.min(bx1) - ax0.max(bx0)).max(0.0);
        let ih = (ay1.min(by1) - ay0.max(by0)).max(0.0);
        let inter = iw * ih;
        let union = self.w * self.h + o.w * o.h - inter;
        if union <= 0.0 { 0.0 } else { inter / union }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxFormat {
    CxCyWh,
    Xyxy,
}

#[derive(Clone, Debug)]
pub struct DecodeCfg {
    /// Number of logits per query (4 for the basketball model).
    pub n_classes: usize,
    /// Logit index -> class. `None` = ignored (e.g. the `ignore` class).
    pub class_map: Vec<Option<Class>>,
    pub thr_ball: f32,
    pub thr_player: f32,
    pub thr_referee: f32,
    pub box_format: BoxFormat,
}

impl DecodeCfg {
    /// Matches `reco-basketball-small` (`classes: [ball, player, referee, ignore]`).
    /// The ball threshold is deliberately lower than the model's 0.7 default:
    /// the ball is tiny on a panorama.
    pub fn basketball_v1() -> Self {
        Self {
            n_classes: 4,
            class_map: vec![
                Some(Class::Ball),
                Some(Class::Player),
                Some(Class::Referee),
                None,
            ],
            thr_ball: 0.30,
            thr_player: 0.60,
            thr_referee: 0.50,
            box_format: BoxFormat::CxCyWh,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    BadShape {
        boxes: usize,
        logits: usize,
        n_classes: usize,
    },
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Decode one inference result. Boxes are returned normalized to the
/// *inference input* (tile); map to panorama with [`tile_to_pano`].
pub fn decode_rfdetr(
    boxes: &[f32],
    logits: &[f32],
    cfg: &DecodeCfg,
) -> Result<Vec<Det>, DecodeError> {
    let nc = cfg.n_classes;
    if nc == 0 || !boxes.len().is_multiple_of(4) || logits.len() != boxes.len() / 4 * nc {
        return Err(DecodeError::BadShape {
            boxes: boxes.len(),
            logits: logits.len(),
            n_classes: nc,
        });
    }
    let nq = boxes.len() / 4;
    let mut out = Vec::new();
    for q in 0..nq {
        let l = &logits[q * nc..(q + 1) * nc];
        let (mut ci, mut best) = (0usize, f32::NEG_INFINITY);
        for (i, &v) in l.iter().enumerate() {
            if v > best {
                best = v;
                ci = i;
            }
        }
        if !best.is_finite() {
            continue;
        }
        let class = match cfg.class_map.get(ci).copied().flatten() {
            Some(c) => c,
            None => continue,
        };
        let score = sigmoid(best);
        let thr = match class {
            Class::Ball => cfg.thr_ball,
            Class::Player => cfg.thr_player,
            Class::Referee => cfg.thr_referee,
        };
        if score < thr {
            continue;
        }
        let b = &boxes[q * 4..q * 4 + 4];
        if !b.iter().all(|v| v.is_finite()) {
            continue;
        }
        let (cx, cy, w, h) = match cfg.box_format {
            BoxFormat::CxCyWh => (b[0], b[1], b[2], b[3]),
            BoxFormat::Xyxy => (
                (b[0] + b[2]) / 2.0,
                (b[1] + b[3]) / 2.0,
                b[2] - b[0],
                b[3] - b[1],
            ),
        };
        if w <= 0.0 || h <= 0.0 {
            continue;
        }
        out.push(Det::new(class, score, cx, cy, w, h));
    }
    Ok(out)
}

/// Greedy per-class NMS (needed after merging overlapping tiles).
pub fn nms(mut dets: Vec<Det>, iou_thr: f32) -> Vec<Det> {
    dets.sort_by(|a, b| {
        b.conf
            .partial_cmp(&a.conf)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut keep: Vec<Det> = Vec::new();
    for d in dets {
        if keep
            .iter()
            .all(|k| k.class != d.class || k.iou(&d) < iou_thr)
        {
            keep.push(d);
        }
    }
    keep
}

/// Pixel rectangle inside the panorama.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tile {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Cover `roi` with square-ish tiles of `tile` px and fractional `overlap`
/// (e.g. 0.25). Tiles are meant to be fed to the 512x512 model (resize if
/// `tile != 512`). A ball needs native-ish resolution, so prefer tiles
/// close to 512 px over downscaling the whole panorama.
pub fn plan_tiles(roi: Tile, tile: u32, overlap: f32) -> Vec<Tile> {
    let tw = tile.min(roi.w);
    let th = tile.min(roi.h);
    let stride = |t: u32| ((t as f32 * (1.0 - overlap.clamp(0.0, 0.9))).round() as u32).max(1);
    let (sx, sy) = (stride(tw), stride(th));
    let positions = |start: u32, total: u32, t: u32, s: u32| -> Vec<u32> {
        let mut v = Vec::new();
        let mut p = 0;
        while p + t < total {
            v.push(start + p);
            p += s;
        }
        v.push(start + total - t);
        v.dedup();
        v
    };
    let xs = positions(roi.x, roi.w, tw, sx);
    let ys = positions(roi.y, roi.h, th, sy);
    let mut out = Vec::new();
    for &y in &ys {
        for &x in &xs {
            out.push(Tile { x, y, w: tw, h: th });
        }
    }
    out
}

/// Map a detection from model-input-normalized to panorama-normalized coordinates.
/// Non-square tiles are letterboxed (centered, aspect preserved) into the
/// square model input, so the padding is undone here.
pub fn tile_to_pano(d: &Det, t: &Tile, pano_w: u32, pano_h: u32) -> Det {
    let (fw, fh) = (pano_w as f32, pano_h as f32);
    let m = t.w.max(t.h) as f32;
    let (fx, fy) = (t.w as f32 / m, t.h as f32 / m);
    let cx = (d.cx - (1.0 - fx) / 2.0) / fx;
    let cy = (d.cy - (1.0 - fy) / 2.0) / fy;
    Det {
        cx: (t.x as f32 + cx * t.w as f32) / fw,
        cy: (t.y as f32 + cy * t.h as f32) / fh,
        w: d.w / fx * t.w as f32 / fw,
        h: d.h / fy * t.h as f32 / fh,
        ..*d
    }
}

/// True if the (panorama-normalized) box `d` ends at an edge of `t` that is not also an edge of the
/// panorama: the object most likely continues outside the tile and the box is only a part of it.
fn cut_by_tile(d: &Det, t: &Tile, pano_w: u32, pano_h: u32) -> bool {
    let (fw, fh) = (pano_w as f32, pano_h as f32);
    let (x0, x1) = ((d.cx - d.w / 2.0) * fw, (d.cx + d.w / 2.0) * fw);
    let (y0, y1) = ((d.cy - d.h / 2.0) * fh, (d.cy + d.h / 2.0) * fh);
    let eps = (t.w.max(t.h) as f32 * 0.006).max(1.5); // ~3 px of the 512 px model input
    let (tx0, ty0, tx1, ty1) = (
        t.x as f32,
        t.y as f32,
        (t.x + t.w) as f32,
        (t.y + t.h) as f32,
    );
    (t.x > 0 && x0 <= tx0 + eps)
        || (t.x + t.w < pano_w && x1 >= tx1 - eps)
        || (t.y > 0 && y0 <= ty0 + eps)
        || (t.y + t.h < pano_h && y1 >= ty1 - eps)
}

/// Merge the detections of overlapping tiles (tile-normalized, one list per tile) into panorama
/// detections. A player cut by a tile edge yields a partial box whose IoU with the full box from
/// the neighbouring tile is too low for NMS, and whose bottom edge is not the foot point. Such a
/// cut box is dropped when a complete box of the same class covers most of it (the complete box
/// then carries the higher confidence of the two); if no tile saw the
/// object whole, the cut box is kept (better a rough position than none). A box found as both
/// player and referee keeps the more confident class.
pub fn merge_tiles(
    per_tile: &[(Tile, Vec<Det>)],
    pano_w: u32,
    pano_h: u32,
    iou_thr: f32,
) -> Vec<Det> {
    let mut all: Vec<(Det, bool)> = Vec::new();
    for (t, dets) in per_tile {
        for d in dets {
            let m = tile_to_pano(d, t, pano_w, pano_h);
            all.push((m, cut_by_tile(&m, t, pano_w, pano_h)));
        }
    }
    // share of `a`'s area that lies inside `b`
    let covered = |a: &Det, b: &Det| {
        let iw = ((a.cx + a.w / 2.0).min(b.cx + b.w / 2.0)
            - (a.cx - a.w / 2.0).max(b.cx - b.w / 2.0))
        .max(0.0);
        let ih = ((a.cy + a.h / 2.0).min(b.cy + b.h / 2.0)
            - (a.cy - a.h / 2.0).max(b.cy - b.h / 2.0))
        .max(0.0);
        if a.w * a.h <= 0.0 {
            0.0
        } else {
            iw * ih / (a.w * a.h)
        }
    };
    // the complete box replaces the cut one but keeps the better of the two confidences:
    // both are the same object, and the cut view is often the clearer one
    let mut whole: Vec<Det> = all.iter().filter(|x| !x.1).map(|x| x.0).collect();
    let mut kept: Vec<Det> = Vec::new();
    for (d, _) in all.iter().filter(|x| x.1) {
        let best = whole
            .iter_mut()
            .filter(|k| k.class == d.class && covered(d, k) >= 0.6)
            .max_by(|a, b| {
                covered(d, a)
                    .partial_cmp(&covered(d, b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        match best {
            Some(k) => k.conf = k.conf.max(d.conf),
            None => kept.push(*d),
        }
    }
    kept.extend(whole);
    let mut out: Vec<Det> = Vec::new();
    for d in nms(kept, iou_thr) {
        let person = |c: Class| c != Class::Ball;
        if person(d.class)
            && out
                .iter()
                .any(|k| person(k.class) && k.class != d.class && k.iou(&d) >= 0.7)
        {
            continue; // `nms` returns by descending confidence, so the kept one is the stronger class
        }
        out.push(d);
    }
    out
}

/// `rf-detr-imagenet-v1` preprocessing: RGB `u8` (already resized to the
/// model input) -> planar CHW `f32`, `(x/255 - mean) / std` (ImageNet stats).
/// ASSUMPTION: verify against the trainer's export before trusting results.
pub fn preprocess_imagenet(rgb: &[u8], w: usize, h: usize) -> Vec<f32> {
    const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
    const STD: [f32; 3] = [0.229, 0.224, 0.225];
    let mut out = vec![0f32; 3 * w * h];
    for i in 0..w * h {
        for c in 0..3 {
            out[c * w * h + i] = (rgb[i * 3 + c] as f32 / 255.0 - MEAN[c]) / STD[c];
        }
    }
    out
}
