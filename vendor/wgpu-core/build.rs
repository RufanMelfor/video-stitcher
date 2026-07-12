use std::env;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-check-cfg=cfg(windows_linux_android)");
    println!("cargo:rustc-check-cfg=cfg(send_sync)");
    println!("cargo:rustc-check-cfg=cfg(dx12)");
    println!("cargo:rustc-check-cfg=cfg(webgl)");
    println!("cargo:rustc-check-cfg=cfg(gles)");
    println!("cargo:rustc-check-cfg=cfg(vulkan)");
    println!("cargo:rustc-check-cfg=cfg(metal)");
    println!("cargo:rustc-check-cfg=cfg(supports_64bit_atomics)");

    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let vendor = env::var("CARGO_CFG_TARGET_VENDOR").unwrap_or_default();
    // CARGO_CFG_TARGET_HAS_ATOMIC is a comma-separated list of sizes, e.g. "8,16,32,64,ptr"
    let has_atomic = env::var("CARGO_CFG_TARGET_HAS_ATOMIC").unwrap_or_default();

    let feat_std = env::var("CARGO_FEATURE_STD").is_ok();
    let feat_dx12 = env::var("CARGO_FEATURE_DX12").is_ok();
    let feat_gles = env::var("CARGO_FEATURE_GLES").is_ok();
    let feat_metal = env::var("CARGO_FEATURE_METAL").is_ok();
    let feat_vulkan = env::var("CARGO_FEATURE_VULKAN").is_ok();
    let feat_vulkan_portability = env::var("CARGO_FEATURE_VULKAN_PORTABILITY").is_ok();
    let feat_webgl = env::var("CARGO_FEATURE_WEBGL").is_ok();
    let feat_angle = env::var("CARGO_FEATURE_ANGLE").is_ok();
    let feat_fragile = env::var("CARGO_FEATURE_FRAGILE_SEND_SYNC_NON_ATOMIC_WASM").is_ok();

    let wasm32 = arch == "wasm32";
    let windows_linux_android =
        os == "windows" || os == "linux" || os == "android" || os == "freebsd";
    // send_sync: for wasm32, approximate as true only when the fragile-send-sync
    // feature is enabled (assumes the atomics target-feature is off), matching the
    // simplification already used in vendor/wgpu-hal/build.rs for the same alias.
    let send_sync = feat_std && (!wasm32 || feat_fragile);
    let dx12 = os == "windows" && feat_dx12;
    let webgl = wasm32 && os != "emscripten" && feat_webgl;
    let gles = (windows_linux_android && feat_gles)
        || webgl
        || (os == "emscripten" && feat_gles)
        || (vendor == "apple" && feat_angle);
    let vulkan =
        (windows_linux_android && feat_vulkan) || (vendor == "apple" && feat_vulkan_portability);
    let metal = vendor == "apple" && feat_metal;
    let supports_64bit_atomics = has_atomic.split(',').any(|s| s.trim() == "64");

    if windows_linux_android {
        println!("cargo:rustc-cfg=windows_linux_android");
    }
    if send_sync {
        println!("cargo:rustc-cfg=send_sync");
    }
    if dx12 {
        println!("cargo:rustc-cfg=dx12");
    }
    if webgl {
        println!("cargo:rustc-cfg=webgl");
    }
    if gles {
        println!("cargo:rustc-cfg=gles");
    }
    if vulkan {
        println!("cargo:rustc-cfg=vulkan");
    }
    if metal {
        println!("cargo:rustc-cfg=metal");
    }
    if supports_64bit_atomics {
        println!("cargo:rustc-cfg=supports_64bit_atomics");
    }
}
