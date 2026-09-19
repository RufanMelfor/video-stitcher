//! The detector model's input dimensions, and the letterbox transform
//! that maps a source frame into them.
//!
//! Models were assumed square until 2026-09-19, when `input_size` was a
//! single `u32` read from the ONNX shape's height and the width was
//! discarded. That silently mis-reads any non-square export: a
//! 2560x1920 model was treated as 1920x1920, scaling the ball *down*
//! rather than up.
//!
//! Square stays the common case, so [`InputSize::square`] is a one-liner
//! and every existing call site keeps its meaning. What changes is that
//! width and height can now differ, which matters because the cameras
//! are 4:3: a square model spends 25% of its tensor on grey padding, and
//! a 4:3 model at the same pixel budget hands the detector a
//! proportionally larger ball.

/// Width and height, in pixels, of a detector model's input tensor.
///
/// Construct with [`square`](Self::square) for the usual NxN model, or
/// [`new`](Self::new) when width and height differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputSize {
    /// Tensor width in pixels (ONNX BCHW `shape[3]`).
    pub width: u32,
    /// Tensor height in pixels (ONNX BCHW `shape[2]`).
    pub height: u32,
}

impl InputSize {
    /// A model whose input is `size x size`.
    pub const fn square(size: u32) -> Self {
        Self {
            width: size,
            height: size,
        }
    }

    /// A model with independent width and height.
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// Whether both dimensions are equal.
    ///
    /// Backends that have not been ported to non-square input (Metal,
    /// NCNN) reject anything else rather than silently using one
    /// dimension for both.
    pub const fn is_square(&self) -> bool {
        self.width == self.height
    }

    /// Element count of one CHW plane (`width * height`).
    pub const fn plane(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// Element count of a full 3-channel CHW tensor.
    pub const fn tensor_len(&self) -> usize {
        3 * self.plane()
    }

    /// The letterbox transform that fits `frame_w x frame_h` into this
    /// input: uniform scale plus centred padding.
    pub fn letterbox(&self, frame_w: u32, frame_h: u32) -> Letterbox {
        let (fw, fh) = (frame_w as f32, frame_h as f32);
        let scale = (self.width as f32 / fw).min(self.height as f32 / fh);
        let new_w = (fw * scale).round();
        let new_h = (fh * scale).round();
        Letterbox {
            scale,
            pad_x: (self.width as f32 - new_w) / 2.0,
            pad_y: (self.height as f32 - new_h) / 2.0,
            content_w: new_w as u32,
            content_h: new_h as u32,
        }
    }
}

impl std::fmt::Display for InputSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{}", self.width, self.height)
    }
}

/// How a source frame maps into an [`InputSize`]: scaled by `scale`,
/// then offset by the padding. Detection coordinates come back through
/// `(coord - pad) / scale`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Letterbox {
    /// Uniform scale applied to the source frame.
    pub scale: f32,
    /// Horizontal padding on each side, in input pixels.
    pub pad_x: f32,
    /// Vertical padding on each side, in input pixels.
    pub pad_y: f32,
    /// Width of the scaled frame inside the input tensor.
    pub content_w: u32,
    /// Height of the scaled frame inside the input tensor.
    pub content_h: u32,
}

impl Letterbox {
    /// Fraction of the input tensor that carries image rather than
    /// padding. `1.0` means the model's aspect ratio matches the
    /// source's and nothing is wasted.
    pub fn coverage(&self) -> f32 {
        let used = self.content_w as f32 * self.content_h as f32;
        let total =
            (self.content_w as f32 + 2.0 * self.pad_x) * (self.content_h as f32 + 2.0 * self.pad_y);
        if total > 0.0 { used / total } else { 0.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square_letterboxes_a_4_3_frame_with_side_padding() {
        // The shipping case: 3840x2880 into 1920x1920.
        let lb = InputSize::square(1920).letterbox(3840, 2880);
        assert!((lb.scale - 0.5).abs() < 1e-6);
        assert_eq!((lb.content_w, lb.content_h), (1920, 1440));
        assert!((lb.pad_x - 0.0).abs() < 1e-6);
        assert!((lb.pad_y - 240.0).abs() < 1e-6);
        // A quarter of the tensor is grey padding.
        assert!((lb.coverage() - 0.75).abs() < 0.01);
    }

    #[test]
    fn matching_aspect_ratio_wastes_nothing() {
        let lb = InputSize::new(2560, 1920).letterbox(3840, 2880);
        assert!((lb.scale - 2560.0 / 3840.0).abs() < 1e-6);
        assert_eq!((lb.content_w, lb.content_h), (2560, 1920));
        assert!((lb.pad_x - 0.0).abs() < 1e-6);
        assert!((lb.pad_y - 0.0).abs() < 1e-6);
        assert!((lb.coverage() - 1.0).abs() < 1e-6);
    }

    /// The reason this type exists: 2560x1440 is 16:9 against a 4:3
    /// source, so height binds and the extra width buys nothing. It
    /// lands on the *same* scale as the 1920 square model - the padding
    /// merely moves from top/bottom to left/right.
    #[test]
    fn wrong_aspect_ratio_gains_no_scale_over_a_square_model() {
        let wide = InputSize::new(2560, 1440).letterbox(3840, 2880);
        let square = InputSize::square(1920).letterbox(3840, 2880);
        assert!((wide.scale - square.scale).abs() < 1e-6);
        assert!(wide.pad_x > 0.0, "padding moved to the sides");
        assert!((wide.pad_y - 0.0).abs() < 1e-6);
    }

    #[test]
    fn tensor_len_follows_both_dimensions() {
        assert_eq!(InputSize::square(4).tensor_len(), 48);
        assert_eq!(InputSize::new(4, 2).tensor_len(), 24);
        assert_eq!(InputSize::new(4, 2).plane(), 8);
    }

    #[test]
    fn is_square_distinguishes_the_two() {
        assert!(InputSize::square(1920).is_square());
        assert!(!InputSize::new(2560, 1920).is_square());
    }

    /// Round-tripping a detection through the letterbox must land back
    /// on the source pixel it came from, for both aspect ratios.
    #[test]
    fn coordinates_round_trip_through_the_letterbox() {
        for size in [InputSize::square(1920), InputSize::new(2560, 1920)] {
            let lb = size.letterbox(3840, 2880);
            for (sx, sy) in [(0.0_f32, 0.0_f32), (1920.0, 1440.0), (3839.0, 2879.0)] {
                let px = sx * lb.scale + lb.pad_x;
                let py = sy * lb.scale + lb.pad_y;
                let bx = (px - lb.pad_x) / lb.scale;
                let by = (py - lb.pad_y) / lb.scale;
                assert!((bx - sx).abs() < 0.01, "{size} x {sx} -> {bx}");
                assert!((by - sy).abs() < 0.01, "{size} y {sy} -> {by}");
            }
        }
    }
}
