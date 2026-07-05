fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-check-cfg=cfg(error_generic_member_access)");
    println!("cargo:rustc-check-cfg=cfg(thiserror_nightly_testing)");
    println!("cargo:rustc-check-cfg=cfg(thiserror_no_backtrace_type)");
    // private.rs is inlined in src/lib.rs — no OUT_DIR write needed.
}
