# reco-core friction log

## wgpu `gles` feature disabled by default

**Symptom:** Build fails on Windows with `khronos_api` build-script error
(`webgl_exts.rs` not generated, os error 2 or 3).

**Root cause:** `khronos_api 3.1.0` (transitive via `wgpu/gles → wgpu-hal/gles →
glow → gl_generator → khronos_api`) has a build script that uses
`env::current_dir()` instead of `env::var("CARGO_MANIFEST_DIR")` to locate its
bundled `api_webgl/extensions/` XML tree. On Windows, Cargo does not guarantee
the working directory is the package root during build-script execution, so the
path lookup fails silently and `webgl_exts.rs` is never generated.

**Impact:** The GL/OpenGL ES backend is compiled out. This breaks the Raspberry
Pi 5 (V3D GPU) path in `gpu/mod.rs` which falls back to `wgpu::Backends::GL`.
All other platforms (Windows DX12, Linux Vulkan, macOS Metal) are unaffected.

**Workaround (RPi5):** Add `wgpu` as a direct dependency of any binary crate
targeting RPi5 and enable `wgpu/gles`. Or wait for a `khronos_api` update that
uses `CARGO_MANIFEST_DIR` in its build script.

**Proposed fix:** Replace the `rpi` runtime check in `gpu/mod.rs` with a
compile-time feature gate (`#[cfg(feature = "gles")]`), and add a `gles` /
`rpi` feature to reco-core that re-enables `wgpu/gles`.
