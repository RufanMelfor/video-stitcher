use std::env;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-check-cfg=cfg(native)");
    println!("cargo:rustc-check-cfg=cfg(send_sync)");
    println!("cargo:rustc-check-cfg=cfg(webgl)");
    println!("cargo:rustc-check-cfg=cfg(Emscripten)");
    println!("cargo:rustc-check-cfg=cfg(dx12)");
    println!("cargo:rustc-check-cfg=cfg(gles)");
    println!("cargo:rustc-check-cfg=cfg(gles_with_std)");
    println!("cargo:rustc-check-cfg=cfg(metal)");
    println!("cargo:rustc-check-cfg=cfg(vulkan)");
    println!("cargo:rustc-check-cfg=cfg(any_backend)");
    println!("cargo:rustc-check-cfg=cfg(static_dxc)");
    println!("cargo:rustc-check-cfg=cfg(supports_64bit_atomics)");
    println!("cargo:rustc-check-cfg=cfg(supports_ptr_atomics)");

    let arch    = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let os      = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let vendor  = env::var("CARGO_CFG_TARGET_VENDOR").unwrap_or_default();
    let tenv    = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    // CARGO_CFG_TARGET_HAS_ATOMIC is a comma-separated list of sizes, e.g. "8,16,32,64,ptr"
    let has_atomic = env::var("CARGO_CFG_TARGET_HAS_ATOMIC").unwrap_or_default();

    let feat_dx12        = env::var("CARGO_FEATURE_DX12").is_ok();
    let feat_gles        = env::var("CARGO_FEATURE_GLES").is_ok();
    let feat_metal       = env::var("CARGO_FEATURE_METAL").is_ok();
    let feat_vulkan      = env::var("CARGO_FEATURE_VULKAN").is_ok();
    let feat_static_dxc  = env::var("CARGO_FEATURE_STATIC_DXC").is_ok();
    let feat_fragile     = env::var("CARGO_FEATURE_FRAGILE_SEND_SYNC_NON_ATOMIC_WASM").is_ok();

    let wasm32   = arch == "wasm32";
    let native   = !wasm32;
    let gles     = feat_gles;
    let dx12     = os == "windows" && feat_dx12;
    let metal    = vendor == "apple" && feat_metal;
    let vulkan   = !wasm32 && feat_vulkan;
    // send_sync: true for all non-wasm32 targets; for wasm32, approximate as true
    // when the fragile-send-sync feature is enabled (atomics = false assumption).
    let send_sync = !wasm32 || feat_fragile;
    let webgl    = wasm32 && os != "emscripten" && gles;
    let emscripten_cfg = os == "emscripten" && gles;
    let gles_with_std = gles
        && (!wasm32 || (vendor == "unknown" && os == "unknown") || os == "emscripten");
    let any_backend = dx12 || metal || vulkan || gles;
    let static_dxc  = os == "windows"
        && feat_static_dxc
        && arch != "aarch64"
        && tenv == "msvc";
    let supports_64bit_atomics = has_atomic.split(',').any(|s| s.trim() == "64");
    let supports_ptr_atomics   = has_atomic.split(',').any(|s| s.trim() == "ptr");

    if native               { println!("cargo:rustc-cfg=native"); }
    if send_sync            { println!("cargo:rustc-cfg=send_sync"); }
    if webgl                { println!("cargo:rustc-cfg=webgl"); }
    if emscripten_cfg       { println!("cargo:rustc-cfg=Emscripten"); }
    if dx12                 { println!("cargo:rustc-cfg=dx12"); }
    if gles                 { println!("cargo:rustc-cfg=gles"); }
    if gles_with_std        { println!("cargo:rustc-cfg=gles_with_std"); }
    if metal                { println!("cargo:rustc-cfg=metal"); }
    if vulkan               { println!("cargo:rustc-cfg=vulkan"); }
    if any_backend          { println!("cargo:rustc-cfg=any_backend"); }
    if static_dxc           { println!("cargo:rustc-cfg=static_dxc"); }
    if supports_64bit_atomics { println!("cargo:rustc-cfg=supports_64bit_atomics"); }
    if supports_ptr_atomics { println!("cargo:rustc-cfg=supports_ptr_atomics"); }
}
