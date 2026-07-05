use std::env;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-check-cfg=cfg(dot_out)");
    println!("cargo:rustc-check-cfg=cfg(glsl_out)");
    println!("cargo:rustc-check-cfg=cfg(hlsl_out)");
    println!("cargo:rustc-check-cfg=cfg(msl_out)");
    println!("cargo:rustc-check-cfg=cfg(spv_out)");
    println!("cargo:rustc-check-cfg=cfg(wgsl_out)");
    println!("cargo:rustc-check-cfg=cfg(std)");
    println!("cargo:rustc-check-cfg=cfg(no_std)");

    let dot_out = env::var("CARGO_FEATURE_DOT_OUT").is_ok();
    let glsl_out = env::var("CARGO_FEATURE_GLSL_OUT").is_ok();
    let hlsl_out = env::var("CARGO_FEATURE_HLSL_OUT").is_ok()
        || (cfg!(target_os = "windows")
            && env::var("CARGO_FEATURE_HLSL_OUT_IF_TARGET_WINDOWS").is_ok());
    let msl_out = env::var("CARGO_FEATURE_MSL_OUT").is_ok()
        || (cfg!(target_vendor = "apple")
            && env::var("CARGO_FEATURE_MSL_OUT_IF_TARGET_APPLE").is_ok());
    let spv_out = env::var("CARGO_FEATURE_SPV_OUT").is_ok();
    let wgsl_out = env::var("CARGO_FEATURE_WGSL_OUT").is_ok();
    let std = env::var("CARGO_FEATURE_WGSL_IN").is_ok()
        || env::var("CARGO_FEATURE_STDERR").is_ok()
        || env::var("CARGO_FEATURE_FS").is_ok();

    if dot_out  { println!("cargo:rustc-cfg=dot_out"); }
    if glsl_out { println!("cargo:rustc-cfg=glsl_out"); }
    if hlsl_out { println!("cargo:rustc-cfg=hlsl_out"); }
    if msl_out  { println!("cargo:rustc-cfg=msl_out"); }
    if spv_out  { println!("cargo:rustc-cfg=spv_out"); }
    if wgsl_out { println!("cargo:rustc-cfg=wgsl_out"); }
    if std      { println!("cargo:rustc-cfg=std"); }
    else        { println!("cargo:rustc-cfg=no_std"); }
}
