fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-cfg=if_docsrs_then_no_serde_core");
    println!("cargo:rustc-check-cfg=cfg(feature, values(\"result\"))");
    println!("cargo:rustc-check-cfg=cfg(if_docsrs_then_no_serde_core)");
    println!("cargo:rustc-check-cfg=cfg(no_core_cstr)");
    println!("cargo:rustc-check-cfg=cfg(no_core_error)");
    println!("cargo:rustc-check-cfg=cfg(no_core_net)");
    println!("cargo:rustc-check-cfg=cfg(no_core_num_saturating)");
    println!("cargo:rustc-check-cfg=cfg(no_diagnostic_namespace)");
    println!("cargo:rustc-check-cfg=cfg(no_serde_derive)");
    println!("cargo:rustc-check-cfg=cfg(no_std_atomic)");
    println!("cargo:rustc-check-cfg=cfg(no_std_atomic64)");
    println!("cargo:rustc-check-cfg=cfg(no_target_has_atomic)");
    // private.rs is inlined in src/lib.rs — no OUT_DIR write needed.
}
