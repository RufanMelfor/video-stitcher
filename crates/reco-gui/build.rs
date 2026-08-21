fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent-dark".to_string());
    slint_build::compile_with_config("ui/main.slint", config).unwrap();

    if let Ok(output) = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
    {
        let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !hash.is_empty() {
            println!("cargo:rustc-env=GIT_HASH={hash}");
        }
    }
    println!("cargo:rerun-if-changed=../../.git/HEAD");

    bundle_scoreboards();
}

/// Copy `scoreboards/` next to the compiled binary (`target/{debug,release}/
/// scoreboards`), for both build profiles.
///
/// `reco_scoreboard::discover_installed` looks for an install-adjacent
/// `<exe_dir>/scoreboards` first - the layout a real packaged distribution
/// would ship - plus a `CARGO_MANIFEST_DIR`-relative dev fallback, but that
/// fallback is deliberately `debug_assertions`-only (a release binary
/// shouldn't leak the build machine's source path). Without this copy step,
/// a locally-built *release* reco-gui.exe finds no scoreboard packages at
/// all - reproduces the packaged-install layout locally instead of
/// special-casing debug vs. release discovery.
fn bundle_scoreboards() {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source = manifest_dir.join("../../scoreboards");
    println!("cargo:rerun-if-changed={}", source.display());

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo for build scripts");
    // OUT_DIR is target/{profile}/build/reco-gui-<hash>/out; the binary
    // itself lands three levels up, at target/{profile}.
    let Some(profile_dir) = std::path::Path::new(&out_dir).ancestors().nth(3) else {
        println!("cargo:warning=cannot locate target/<profile> from OUT_DIR={out_dir}");
        return;
    };
    let dest = profile_dir.join("scoreboards");

    if let Err(error) = copy_dir_all(&source, &dest) {
        println!(
            "cargo:warning=cannot bundle scoreboards/ next to the binary ({} -> {}): {error}",
            source.display(),
            dest.display()
        );
    }
}

fn copy_dir_all(source: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let dest_path = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &dest_path)?;
        } else {
            std::fs::copy(entry.path(), dest_path)?;
        }
    }
    Ok(())
}
