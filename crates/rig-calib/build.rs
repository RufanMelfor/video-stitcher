fn main() {
    // Run the Slint compiler on a thread with a larger stack. main.slint's
    // key-pressed handler has grown into a long sequential chain of
    // if-statements (free-fly camera controls); on Windows' small 1MB
    // default thread stack, slint-compiler's AST traversal overflows past
    // roughly a dozen statements in one handler. Not a bug in the .slint
    // source - just needs more headroom than the platform default gives it.
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let config =
                slint_build::CompilerConfiguration::new().with_style("fluent-dark".to_string());
            slint_build::compile_with_config("ui/main.slint", config).unwrap();
        })
        .expect("failed to spawn slint-compiler thread")
        .join()
        .expect("slint-compiler thread panicked");

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

    // Embed the app icon as a PE resource so Explorer/taskbar shortcuts show
    // it before the exe is even launched (separate from the Slint `icon`
    // property, which only sets the runtime window/taskbar icon).
    #[cfg(windows)]
    {
        winresource::WindowsResource::new()
            .set_icon("assets/icon.ico")
            .compile()
            .expect("failed to embed Windows icon resource");
    }
}
