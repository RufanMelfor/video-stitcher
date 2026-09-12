//! GPU -> CPU NV12 plane readback.
//!
//! Copies a multi-planar NV12 wgpu texture's two planes into
//! tightly-packed CPU buffers: a `width * height` Y plane and a
//! `width * (height / 2)` interleaved UV plane, stripping the 256-byte
//! per-row padding wgpu requires for `copy_texture_to_buffer`.
//!
//! # Why this exists
//!
//! The stitch pipeline never needs this: it samples decoded NV12
//! textures in a render pass and the pixels stay on the GPU all the way
//! to the encoder. The raw-camera AI debug export
//! (`reco_io::raw_camera_debug`) is the opposite case - it wants the
//! *decode* on the GPU (a full-resolution raw camera feed, e.g.
//! 3840x2880 HEVC, is expensive to decode on the CPU) but must still
//! draw its detection boxes on the CPU, directly onto the decoded YUV
//! planes. Drawing on the GPU instead would mean rendering into an RGBA
//! target and converting back, putting every pixel of every frame
//! through an NV12 -> RGB -> NV12 roundtrip - a lossy chroma conversion
//! across the whole image, including the vast majority of pixels
//! nowhere near a box. For a diagnostic tool whose entire purpose is
//! showing *exactly* what the model saw, that tradeoff is backwards, so
//! the frame comes back to the CPU unconverted instead.
//!
//! # Per-plane copies
//!
//! An NV12 `wgpu::Texture` cannot be copied in one call: its planes have
//! different formats (`R8Unorm` Y, `Rg8Unorm` UV) and different sizes
//! (UV is half resolution in both axes). Each plane is copied separately
//! with the matching [`wgpu::TextureAspect`] - the same `Plane0`/`Plane1`
//! selectors [`crate::interop::d3d11::D3d11PlaneSource`] documents for
//! same-format copies.
//!
//! # Blocking, not pipelined
//!
//! Unlike [`RgbaReadback`](crate::gpu::rgba_readback::RgbaReadback) and
//! [`Nv12Converter`](crate::gpu::nv12_converter::Nv12Converter), this
//! reads back synchronously rather than triple-buffering two frames
//! behind. Those exist inside a render loop that always has more GPU
//! work queued to overlap the wait with; this runs in a decode loop that
//! needs *this* frame's pixels before it can draw on them and encode
//! them, so there is nothing to overlap against and a deferred result
//! would only add latency and buffering complexity. The copy itself is
//! GPU-to-GPU-buffer plus a map, not a format conversion.

use super::GpuContext;

/// Errors from [`Nv12Readback`].
#[derive(Debug, thiserror::Error)]
pub enum Nv12ReadbackError {
    /// Zero or odd dimensions - NV12 chroma is 2x2 subsampled, so both
    /// axes must be even and non-zero.
    #[error("invalid dimensions: {0}")]
    InvalidDimensions(String),
    /// Mapping a staging buffer for CPU read failed.
    #[error("buffer map failed: {0}")]
    Map(String),
}

/// GPU -> CPU NV12 plane readback with reusable staging buffers.
///
/// One instance is bound to a single frame size; [`read`](Self::read)
/// may be called once per frame and reuses the same buffers throughout
/// (no per-frame allocation).
pub struct Nv12Readback {
    y_staging: wgpu::Buffer,
    uv_staging: wgpu::Buffer,
    /// Tightly-packed output: `width * height` bytes.
    y_out: Vec<u8>,
    /// Tightly-packed output: `width * (height / 2)` bytes (interleaved).
    uv_out: Vec<u8>,
    width: u32,
    height: u32,
    y_bytes_per_row: u32,
    y_padded_bytes_per_row: u32,
    uv_bytes_per_row: u32,
    uv_padded_bytes_per_row: u32,
}

impl Nv12Readback {
    /// Allocate staging and output buffers for `width` x `height` NV12
    /// frames. Both dimensions must be even and non-zero.
    pub fn new(gpu: &GpuContext, width: u32, height: u32) -> Result<Self, Nv12ReadbackError> {
        if width == 0 || height == 0 {
            return Err(Nv12ReadbackError::InvalidDimensions(format!(
                "must be > 0, got {width}x{height}"
            )));
        }
        if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            // NV12 chroma is 2x2 subsampled; an odd axis has no
            // well-defined half-resolution plane.
            return Err(Nv12ReadbackError::InvalidDimensions(format!(
                "must be even, got {width}x{height}"
            )));
        }

        let y_bytes_per_row = width;
        let uv_bytes_per_row = width; // half width, 2 bytes per sample
        let uv_height = height / 2;
        // wgpu requires copy_texture_to_buffer rows aligned to 256 bytes
        // (COPY_BYTES_PER_ROW_ALIGNMENT).
        let y_padded_bytes_per_row = y_bytes_per_row.div_ceil(256) * 256;
        let uv_padded_bytes_per_row = uv_bytes_per_row.div_ceil(256) * 256;

        let device = &gpu.device;
        let y_staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("nv12_readback_y"),
            size: (y_padded_bytes_per_row as u64) * (height as u64),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let uv_staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("nv12_readback_uv"),
            size: (uv_padded_bytes_per_row as u64) * (uv_height as u64),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        Ok(Self {
            y_staging,
            uv_staging,
            y_out: vec![0u8; (width as usize) * (height as usize)],
            uv_out: vec![0u8; (width as usize) * (uv_height as usize)],
            width,
            height,
            y_bytes_per_row,
            y_padded_bytes_per_row,
            uv_bytes_per_row,
            uv_padded_bytes_per_row,
        })
    }

    /// The frame size this instance was built for.
    pub fn frame_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Copy `texture`'s two planes to the CPU, returning
    /// `(y_plane, uv_plane)` as tightly-packed slices borrowed from this
    /// instance's reusable buffers - valid until the next [`read`](Self::read).
    ///
    /// `texture` must be an NV12-format texture created with
    /// [`wgpu::TextureUsages::COPY_SRC`] and matching this instance's
    /// dimensions.
    pub fn read(
        &mut self,
        gpu: &GpuContext,
        texture: &wgpu::Texture,
    ) -> Result<(&[u8], &[u8]), Nv12ReadbackError> {
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("nv12_readback_encoder"),
            });

        // Y plane: full resolution, R8Unorm via Plane0.
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::Plane0,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.y_staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.y_padded_bytes_per_row),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );

        // UV plane: half resolution in both axes, Rg8Unorm via Plane1.
        // The copy extent is given in *plane* texels (half width/height),
        // each 2 bytes wide.
        let uv_height = self.height / 2;
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::Plane1,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.uv_staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.uv_padded_bytes_per_row),
                    rows_per_image: Some(uv_height),
                },
            },
            wgpu::Extent3d {
                width: self.width / 2,
                height: uv_height,
                depth_or_array_layers: 1,
            },
        );

        let submission = gpu.queue.submit(std::iter::once(encoder.finish()));

        map_and_strip(
            gpu,
            &submission,
            &self.y_staging,
            &mut self.y_out,
            self.y_bytes_per_row,
            self.y_padded_bytes_per_row,
            self.height,
        )?;
        map_and_strip(
            gpu,
            &submission,
            &self.uv_staging,
            &mut self.uv_out,
            self.uv_bytes_per_row,
            self.uv_padded_bytes_per_row,
            uv_height,
        )?;

        Ok((&self.y_out, &self.uv_out))
    }
}

/// Map one staging buffer, copy its rows into `out` with wgpu's 256-byte
/// row padding removed, then unmap. Blocking - see this module's doc
/// comment for why this path does not defer the wait.
fn map_and_strip(
    gpu: &GpuContext,
    submission: &wgpu::SubmissionIndex,
    staging: &wgpu::Buffer,
    out: &mut [u8],
    bytes_per_row: u32,
    padded_bytes_per_row: u32,
    rows: u32,
) -> Result<(), Nv12ReadbackError> {
    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    // Wait only for this frame's own copy, not for everything queued on
    // the device - the detector's preprocessing dispatches share this
    // queue, and an unscoped wait would block behind them too (the same
    // trap `reco_detect::wgpu_preprocess` documents costing 52ms/call).
    gpu.device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission.clone()),
            timeout: None,
        })
        .map_err(|e| Nv12ReadbackError::Map(format!("poll failed: {e:?}")))?;
    rx.recv()
        .map_err(|_| Nv12ReadbackError::Map("map callback dropped".into()))?
        .map_err(|e| Nv12ReadbackError::Map(e.to_string()))?;

    {
        let data = slice.get_mapped_range();
        let row = bytes_per_row as usize;
        let padded = padded_bytes_per_row as usize;
        if row == padded {
            out.copy_from_slice(&data[..out.len()]);
        } else {
            for y in 0..rows as usize {
                out[y * row..(y + 1) * row].copy_from_slice(&data[y * padded..y * padded + row]);
            }
        }
    }
    staging.unmap();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero_and_odd_dimensions() {
        let Ok(gpu) = GpuContext::new_blocking() else {
            eprintln!("Skipping GPU test: no adapter available");
            return;
        };
        assert!(Nv12Readback::new(&gpu, 0, 480).is_err());
        assert!(Nv12Readback::new(&gpu, 640, 0).is_err());
        // NV12 chroma is 2x2 subsampled - odd axes have no half-res plane.
        assert!(Nv12Readback::new(&gpu, 641, 480).is_err());
        assert!(Nv12Readback::new(&gpu, 640, 481).is_err());
        assert!(Nv12Readback::new(&gpu, 640, 480).is_ok());
    }

    #[test]
    fn allocates_correctly_sized_output_planes() {
        let Ok(gpu) = GpuContext::new_blocking() else {
            eprintln!("Skipping GPU test: no adapter available");
            return;
        };
        let r = Nv12Readback::new(&gpu, 64, 32).expect("valid dimensions");
        assert_eq!(r.frame_size(), (64, 32));
        assert_eq!(r.y_out.len(), 64 * 32, "Y is full resolution");
        assert_eq!(
            r.uv_out.len(),
            64 * 16,
            "UV is half height, full width (2 bytes per half-width sample)"
        );
    }

    /// The 256-byte alignment wgpu demands is the whole reason the strip
    /// step exists - a width that is already a multiple of 256 must take
    /// the fast path, one that is not must be padded.
    #[test]
    fn row_padding_matches_wgpu_alignment_rule() {
        let Ok(gpu) = GpuContext::new_blocking() else {
            eprintln!("Skipping GPU test: no adapter available");
            return;
        };
        // 3840 is a multiple of 256 -> no padding needed.
        let aligned = Nv12Readback::new(&gpu, 3840, 2880).expect("valid");
        assert_eq!(aligned.y_bytes_per_row, aligned.y_padded_bytes_per_row);
        // 1920 is also a multiple of 256; use a width that is not.
        let unaligned = Nv12Readback::new(&gpu, 300, 200).expect("valid");
        assert_eq!(unaligned.y_bytes_per_row, 300);
        assert_eq!(unaligned.y_padded_bytes_per_row, 512);
    }
}
