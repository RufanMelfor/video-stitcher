use std::env;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-check-cfg=cfg(native)");
    println!("cargo:rustc-check-cfg=cfg(Emscripten)");
    println!("cargo:rustc-check-cfg=cfg(web)");
    println!("cargo:rustc-check-cfg=cfg(send_sync)");
    println!("cargo:rustc-check-cfg=cfg(webgpu)");
    println!("cargo:rustc-check-cfg=cfg(webgl)");
    println!("cargo:rustc-check-cfg=cfg(dx12)");
    println!("cargo:rustc-check-cfg=cfg(metal)");
    println!("cargo:rustc-check-cfg=cfg(vulkan)");
    println!("cargo:rustc-check-cfg=cfg(gles)");
    println!("cargo:rustc-check-cfg=cfg(noop)");
    println!("cargo:rustc-check-cfg=cfg(wgpu_core)");
    println!("cargo:rustc-check-cfg=cfg(naga)");
    println!("cargo:rustc-check-cfg=cfg(static_dxc)");
    println!("cargo:rustc-check-cfg=cfg(supports_64bit_atomics)");
    println!("cargo:rustc-check-cfg=cfg(custom)");
    println!("cargo:rustc-check-cfg=cfg(std)");
    println!("cargo:rustc-check-cfg=cfg(no_std)");

    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let vendor = env::var("CARGO_CFG_TARGET_VENDOR").unwrap_or_default();
    let tenv = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let panic = env::var("CARGO_CFG_PANIC").unwrap_or_default();
    // CARGO_CFG_TARGET_HAS_ATOMIC is a comma-separated list of sizes, e.g. "8,16,32,64,ptr"
    let has_atomic = env::var("CARGO_CFG_TARGET_HAS_ATOMIC").unwrap_or_default();

    let feat_std = env::var("CARGO_FEATURE_STD").is_ok();
    let feat_web = env::var("CARGO_FEATURE_WEB").is_ok();
    let feat_webgpu = env::var("CARGO_FEATURE_WEBGPU").is_ok();
    let feat_webgl = env::var("CARGO_FEATURE_WEBGL").is_ok();
    let feat_dx12 = env::var("CARGO_FEATURE_DX12").is_ok();
    let feat_metal = env::var("CARGO_FEATURE_METAL").is_ok();
    let feat_vulkan = env::var("CARGO_FEATURE_VULKAN").is_ok();
    let feat_vulkan_portability = env::var("CARGO_FEATURE_VULKAN_PORTABILITY").is_ok();
    let feat_gles = env::var("CARGO_FEATURE_GLES").is_ok();
    let feat_angle = env::var("CARGO_FEATURE_ANGLE").is_ok();
    let feat_noop = env::var("CARGO_FEATURE_NOOP").is_ok();
    let feat_naga_ir = env::var("CARGO_FEATURE_NAGA_IR").is_ok();
    let feat_spirv = env::var("CARGO_FEATURE_SPIRV").is_ok();
    let feat_glsl = env::var("CARGO_FEATURE_GLSL").is_ok();
    let feat_static_dxc = env::var("CARGO_FEATURE_STATIC_DXC").is_ok();
    let feat_custom = env::var("CARGO_FEATURE_CUSTOM").is_ok();
    let feat_fragile = env::var("CARGO_FEATURE_FRAGILE_SEND_SYNC_NON_ATOMIC_WASM").is_ok();

    let wasm32 = arch == "wasm32";
    let native = !wasm32;
    let emscripten = wasm32 && os == "emscripten";
    let web = wasm32 && !emscripten && feat_web;

    // send_sync: for wasm32, approximate as true only when the fragile-send-sync
    // feature is enabled (assumes the atomics target-feature is off), matching the
    // simplification already used in vendor/wgpu-hal/build.rs for the same alias.
    let send_sync = native || feat_fragile;

    let webgpu = wasm32 && !emscripten && feat_webgpu;
    let webgl = wasm32 && !emscripten && feat_webgl;
    let dx12 = os == "windows" && feat_dx12;
    let metal = vendor == "apple" && feat_metal;
    let windows_linux_android = os == "windows" || os == "linux" || os == "android" || os == "freebsd";
    let vulkan = (windows_linux_android && feat_vulkan) || (vendor == "apple" && feat_vulkan_portability);
    let gles = ((windows_linux_android || emscripten) && feat_gles) || (vendor == "apple" && feat_angle);
    let noop = feat_noop;
    let wgpu_core = native || webgl || dx12 || metal || vulkan || gles || noop;
    let naga = feat_naga_ir || feat_spirv || feat_glsl;
    let static_dxc = os == "windows" && feat_static_dxc && arch != "aarch64" && tenv == "msvc";
    let supports_64bit_atomics = has_atomic.split(',').any(|s| s.trim() == "64");
    let custom = feat_custom;
    let std_cfg = feat_std || send_sync || panic == "unwind";
    let no_std = !std_cfg;

    if native {
        println!("cargo:rustc-cfg=native");
    }
    if emscripten {
        println!("cargo:rustc-cfg=Emscripten");
    }
    if web {
        println!("cargo:rustc-cfg=web");
    }
    if send_sync {
        println!("cargo:rustc-cfg=send_sync");
    }
    if webgpu {
        println!("cargo:rustc-cfg=webgpu");
    }
    if webgl {
        println!("cargo:rustc-cfg=webgl");
    }
    if dx12 {
        println!("cargo:rustc-cfg=dx12");
    }
    if metal {
        println!("cargo:rustc-cfg=metal");
    }
    if vulkan {
        println!("cargo:rustc-cfg=vulkan");
    }
    if gles {
        println!("cargo:rustc-cfg=gles");
    }
    if noop {
        println!("cargo:rustc-cfg=noop");
    }
    if wgpu_core {
        println!("cargo:rustc-cfg=wgpu_core");
    }
    if naga {
        println!("cargo:rustc-cfg=naga");
    }
    if static_dxc {
        println!("cargo:rustc-cfg=static_dxc");
    }
    if supports_64bit_atomics {
        println!("cargo:rustc-cfg=supports_64bit_atomics");
    }
    if custom {
        println!("cargo:rustc-cfg=custom");
    }
    if std_cfg {
        println!("cargo:rustc-cfg=std");
    }
    if no_std {
        println!("cargo:rustc-cfg=no_std");
    }
}
