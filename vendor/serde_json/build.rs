fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-check-cfg=cfg(fast_arithmetic, values(\"32\", \"64\"))");
    // x86_64 / aarch64 / any 64-bit pointer-width target → 64-bit limbs.
    // Hard-coded for the Windows x86_64 build environment; matches what the
    // original build script would detect at runtime.
    println!("cargo:rustc-cfg=fast_arithmetic=\"64\"");
}
