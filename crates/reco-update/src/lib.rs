//! Self-update for RECO Panopitch (GUI and CLI).
//!
//! Flow: [`check`] asks GitHub for the newest release that ships an asset
//! for *this* platform, [`download`] fetches it and verifies the published
//! SHA-256, [`stage`] unpacks it next to the install, and
//! [`spawn_installer`] starts a tiny detached script that waits for this
//! process to exit, swaps the files in and relaunches the app. The caller
//! exits right after `spawn_installer` returns.
//!
//! Release asset convention (one zip per platform, plus its checksum):
//!
//! ```text
//! reco-panopitch-<tag>-<platform>[-<variant>].zip
//! reco-panopitch-<tag>-<platform>[-<variant>].zip.sha256
//!
//! <platform> = windows-x86_64 | macos-aarch64 | linux-x86_64
//! <variant>  = optional, set at build time via RECO_RELEASE_VARIANT
//!              (e.g. "cpu") for builds that must not replace each other
//! ```
//!
//! macOS zips contain `RECO Panopitch.app`; Windows/Linux zips contain the
//! install directory contents (optionally inside one top-level folder).
//!
//! Integrity: the checksum is downloaded from the same release over HTTPS
//! and protects against corrupt/truncated downloads and CDN tampering. It
//! does not defend against a compromised GitHub account; signing releases
//! would be the next step.

use anyhow::{Context, Result, anyhow, bail};
use semver::Version;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const REPO: &str = "RufanMelfor/video-stitcher";
const ASSET_PREFIX: &str = "reco-panopitch-";
/// Refuse absurd downloads (the Windows build is ~165 MB).
const MAX_DOWNLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Build-time variant, e.g. `RECO_RELEASE_VARIANT=cpu cargo build ...`.
const VARIANT: Option<&str> = option_env!("RECO_RELEASE_VARIANT");

/// A downloadable newer release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub version: Version,
    pub page_url: String,
    pub asset_name: String,
    pub asset_url: String,
    pub asset_size: u64,
    pub sha_url: String,
}

/// `windows-x86_64`, `macos-aarch64`, `linux-x86_64`, ... for this build.
pub fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// Suffix every asset of this build ends with (before `.zip`).
fn asset_suffix() -> String {
    match VARIANT {
        Some(v) if !v.is_empty() => format!("-{}-{}.zip", platform(), v),
        _ => format!("-{}.zip", platform()),
    }
}

/// Only talk to GitHub (API, release pages, release asset CDN).
fn is_trusted_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.split('@').next_back().unwrap_or("");
    let host = host.split(':').next().unwrap_or("");
    host == "github.com"
        || host == "api.github.com"
        || host.ends_with(".githubusercontent.com")
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(60 * 30)))
        .timeout_connect(Some(std::time::Duration::from_secs(15)))
        .build()
        .into()
}

fn get(url: &str) -> Result<ureq::http::Response<ureq::Body>> {
    if !is_trusted_url(url) {
        bail!("refusing to fetch from untrusted URL: {url}");
    }
    agent()
        .get(url)
        .header("User-Agent", "reco-panopitch-updater")
        .header("Accept", "application/vnd.github+json")
        .call()
        .with_context(|| format!("GET {url}"))
}

/// Pick the newest applicable release from the GitHub `/releases` JSON.
///
/// - drafts are ignored;
/// - pre-releases count only if `current` is itself a pre-release or
///   `include_prerelease` is set (`/releases/latest` never returns
///   pre-releases, which is why it is not used);
/// - a release without an asset *and* checksum for this platform is
///   skipped, so publishing the Windows zip first never offers a macOS
///   user an update that does not exist yet;
/// - only strictly newer versions are returned (no downgrade).
pub fn pick_release(
    releases: &serde_json::Value,
    current: &Version,
    include_prerelease: bool,
) -> Option<Release> {
    let suffix = asset_suffix();
    let allow_pre = include_prerelease || !current.pre.is_empty();
    let mut best: Option<Release> = None;
    for r in releases.as_array()? {
        if r["draft"].as_bool().unwrap_or(false) {
            continue;
        }
        if r["prerelease"].as_bool().unwrap_or(false) && !allow_pre {
            continue;
        }
        let Some(tag) = r["tag_name"].as_str() else { continue };
        let Ok(version) = Version::parse(tag.trim_start_matches('v')) else {
            continue;
        };
        if version <= *current || best.as_ref().is_some_and(|b| b.version >= version) {
            continue;
        }
        let assets = r["assets"].as_array().cloned().unwrap_or_default();
        let find = |name: &dyn Fn(&str) -> bool| {
            assets.iter().find(|a| a["name"].as_str().is_some_and(name))
        };
        let Some(zip) = find(&|n| n.starts_with(ASSET_PREFIX) && n.ends_with(&suffix)) else {
            continue;
        };
        let zip_name = zip["name"].as_str().unwrap_or_default().to_string();
        let sha_name = format!("{zip_name}.sha256");
        let Some(sha) = find(&|n| n == sha_name) else { continue };
        let (Some(zip_url), Some(sha_url)) = (
            zip["browser_download_url"].as_str(),
            sha["browser_download_url"].as_str(),
        ) else {
            continue;
        };
        if !is_trusted_url(zip_url) || !is_trusted_url(sha_url) {
            continue;
        }
        best = Some(Release {
            tag: tag.to_string(),
            version,
            page_url: r["html_url"]
                .as_str()
                .filter(|u| is_trusted_url(u))
                .map(String::from)
                .unwrap_or_else(|| format!("https://github.com/{REPO}/releases/tag/{tag}")),
            asset_name: zip_name,
            asset_url: zip_url.to_string(),
            asset_size: zip["size"].as_u64().unwrap_or(0),
            sha_url: sha_url.to_string(),
        });
    }
    best
}

/// Ask GitHub whether a newer release exists for this platform.
pub fn check(current_version: &str, include_prerelease: bool) -> Result<Option<Release>> {
    let current = Version::parse(current_version.trim_start_matches('v'))
        .with_context(|| format!("bad current version {current_version:?}"))?;
    let url = format!("https://api.github.com/repos/{REPO}/releases?per_page=30");
    let mut resp = get(&url)?;
    let body = resp.body_mut().read_to_string().context("read release list")?;
    let json: serde_json::Value = serde_json::from_str(&body).context("parse release list")?;
    Ok(pick_release(&json, &current, include_prerelease))
}

/// First 64-hex-digit token of a `.sha256` file (`<hash>  <name>` or bare).
pub fn parse_sha256(text: &str) -> Option<String> {
    let tok = text.split_whitespace().next()?.trim_start_matches('*');
    (tok.len() == 64 && tok.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| tok.to_ascii_lowercase())
}

/// Download `release` into `dir`, verify its SHA-256, return the zip path.
/// `progress(done, total)` is called periodically (`total` may be 0).
pub fn download(
    release: &Release,
    dir: &Path,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let mut sha_resp = get(&release.sha_url)?;
    let sha_text = sha_resp.body_mut().read_to_string().context("read checksum")?;
    let expected = parse_sha256(&sha_text).ok_or_else(|| anyhow!("malformed checksum file"))?;

    let resp = get(&release.asset_url)?;
    let total = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(release.asset_size);
    if total > MAX_DOWNLOAD_BYTES {
        bail!("download too large ({total} bytes)");
    }
    let part = dir.join(format!("{}.part", release.asset_name));
    let dest = dir.join(&release.asset_name);
    let mut out = std::fs::File::create(&part)?;
    let mut hasher = Sha256::new();
    let mut reader = resp.into_body().into_reader();
    let (mut buf, mut done, mut last_report) = (vec![0u8; 256 * 1024], 0u64, 0u64);
    loop {
        let n = reader.read(&mut buf).context("download interrupted")?;
        if n == 0 {
            break;
        }
        done += n as u64;
        if done > MAX_DOWNLOAD_BYTES {
            bail!("download exceeds size limit");
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n])?;
        if done - last_report >= 512 * 1024 {
            last_report = done;
            progress(done, total);
        }
    }
    out.sync_all()?;
    drop(out);
    progress(done, total);
    let actual = hex(&hasher.finalize());
    if actual != expected {
        let _ = std::fs::remove_file(&part);
        bail!("checksum mismatch (expected {expected}, got {actual}); download discarded");
    }
    std::fs::rename(&part, &dest)?;
    Ok(dest)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What gets replaced, and what to launch afterwards.
#[derive(Debug, Clone)]
pub struct InstallTarget {
    /// macOS: the `.app` bundle. Windows/Linux: the directory holding the exe.
    pub root: PathBuf,
    /// The executable that is running now (relaunched after the swap).
    pub exe: PathBuf,
    /// Start the app again after the swap. `true` for the GUI; the CLI
    /// sets it to `false` so `reco update` doesn't pop up a window.
    pub relaunch: bool,
}

/// Locate the current installation, or explain why it cannot self-update
/// (dev build, read-only location). The GUI then falls back to opening the
/// release page.
pub fn install_target() -> Result<InstallTarget> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let dir = exe.parent().ok_or_else(|| anyhow!("exe has no parent"))?.to_path_buf();
    if exe.components().any(|c| c.as_os_str() == "target")
        && exe.components().any(|c| c.as_os_str() == "release" || c.as_os_str() == "debug")
    {
        bail!("running from a cargo target directory (development build)");
    }
    let root = if cfg!(target_os = "macos") {
        // .../RECO Panopitch.app/Contents/MacOS/reco-panopitch
        let app = dir
            .parent()
            .and_then(Path::parent)
            .filter(|p| p.extension().is_some_and(|e| e == "app"))
            .ok_or_else(|| anyhow!("not running from an .app bundle"))?;
        app.to_path_buf()
    } else {
        dir
    };
    // Need write access to the parent (macOS: swap the bundle) or the dir.
    let probe_dir = if cfg!(target_os = "macos") {
        root.parent().unwrap_or(&root).to_path_buf()
    } else {
        root.clone()
    };
    let probe = probe_dir.join(format!(".reco-update-probe-{}", std::process::id()));
    std::fs::write(&probe, b"")
        .map_err(|e| anyhow!("{} is not writable ({e}); update manually", probe_dir.display()))?;
    let _ = std::fs::remove_file(&probe);
    Ok(InstallTarget { root, exe, relaunch: true })
}

/// Unpack `zip` next to the install and return the directory whose
/// contents/bundle will replace it.
pub fn stage(zip: &Path, target: &InstallTarget) -> Result<PathBuf> {
    let parent = target.root.parent().ok_or_else(|| anyhow!("root has no parent"))?;
    let staging = parent.join(".reco-update-staging");
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;
    extract(zip, &staging)?;

    let new_root = if cfg!(target_os = "macos") {
        let app = std::fs::read_dir(&staging)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "app"))
            .ok_or_else(|| anyhow!("update archive contains no .app bundle"))?;
        let dest = parent.join(".reco-update-new.app");
        if dest.exists() {
            std::fs::remove_dir_all(&dest)?;
        }
        std::fs::rename(&app, &dest)?;
        let _ = std::fs::remove_dir_all(&staging);
        dest
    } else {
        // Flatten a single top-level folder.
        let entries: Vec<_> = std::fs::read_dir(&staging)?.filter_map(|e| e.ok()).collect();
        if entries.len() == 1 && entries[0].path().is_dir() {
            entries[0].path()
        } else {
            staging
        }
    };
    let exe_name = target.exe.file_name().ok_or_else(|| anyhow!("exe has no name"))?;
    let expected = if cfg!(target_os = "macos") {
        new_root.join("Contents/MacOS").join(exe_name)
    } else {
        new_root.join(exe_name)
    };
    if !expected.is_file() {
        bail!("update archive is missing {}", expected.display());
    }
    Ok(new_root)
}

#[cfg(target_os = "macos")]
fn extract(zip: &Path, dest: &Path) -> Result<()> {
    let status = std::process::Command::new("/usr/bin/ditto")
        .args(["-x", "-k"])
        .arg(zip)
        .arg(dest)
        .status()
        .context("run ditto")?;
    if !status.success() {
        bail!("ditto failed to extract the update ({status})");
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn extract(zip: &Path, dest: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(zip)?)?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        // enclosed_name rejects absolute paths and `..` (zip-slip).
        let Some(rel) = file.enclosed_name() else {
            bail!("unsafe path in update archive: {}", file.name());
        };
        let out = dest.join(rel);
        if file.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(p) = out.parent() {
            std::fs::create_dir_all(p)?;
        }
        let mut f = std::fs::File::create(&out)?;
        std::io::copy(&mut file, &mut f)?;
        #[cfg(unix)]
        if let Some(mode) = file.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode & 0o777))?;
        }
    }
    Ok(())
}

#[cfg(unix)]
const UNIX_SCRIPT: &str = r#"#!/bin/sh
# Waits for the app to quit, swaps in the new version, relaunches.
# Inputs come via environment (never interpolated into the script).
i=0
while kill -0 "$RECO_PID" 2>/dev/null && [ "$i" -lt 240 ]; do sleep 0.5; i=$((i+1)); done
if [ "$RECO_MODE" = "bundle" ]; then
  BAK="$RECO_ROOT.old"
  rm -rf "$BAK"
  if mv "$RECO_ROOT" "$BAK"; then
    if mv "$RECO_NEW" "$RECO_ROOT"; then
      rm -rf "$BAK"
    else
      mv "$BAK" "$RECO_ROOT"   # roll back
    fi
  fi
  /usr/bin/xattr -dr com.apple.quarantine "$RECO_ROOT" 2>/dev/null
  [ -z "$RECO_NO_LAUNCH" ] && /usr/bin/open "$RECO_ROOT"
else
  cp -Rf "$RECO_NEW"/. "$RECO_ROOT"/ && rm -rf "$RECO_STAGING"
  [ -z "$RECO_NO_LAUNCH" ] && nohup "$RECO_EXE" >/dev/null 2>&1 &
fi
rm -f "$0"
"#;

#[cfg(windows)]
const WINDOWS_SCRIPT: &str = r#"@echo off
:wait
tasklist /FI "PID eq %RECO_PID%" 2>NUL | find "%RECO_PID%" >NUL
if not errorlevel 1 (
  ping -n 2 127.0.0.1 >NUL
  goto wait
)
robocopy "%RECO_NEW%" "%RECO_ROOT%" /E /IS /IT /R:60 /W:1 /NFL /NDL /NJH /NJS /NP >NUL
if errorlevel 8 exit /b 1
rmdir /S /Q "%RECO_STAGING%"
if not defined RECO_NO_LAUNCH start "" "%RECO_EXE%"
del "%~f0"
"#;

/// Start the detached installer. Call `std::process::exit`/quit the event
/// loop right after this returns `Ok`.
pub fn spawn_installer(target: &InstallTarget, new_root: &Path) -> Result<()> {
    // Directory to delete after the swap: the macOS staged bundle itself,
    // or on Windows/Linux the staging folder (the parent when a single
    // top-level folder was flattened).
    let staging = if !cfg!(target_os = "macos")
        && new_root.file_name().is_none_or(|n| n != ".reco-update-staging")
    {
        new_root.parent().unwrap_or(new_root).to_path_buf()
    } else {
        new_root.to_path_buf()
    };
    let script_dir = std::env::temp_dir();
    let pid = std::process::id().to_string();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::process::CommandExt;
        let script = script_dir.join(format!("reco-update-{pid}.sh"));
        std::fs::write(&script, UNIX_SCRIPT)?;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))?;
        let mut cmd = std::process::Command::new("/bin/sh");
        if !target.relaunch {
            cmd.env("RECO_NO_LAUNCH", "1");
        }
        cmd.arg(&script)
            .env("RECO_PID", &pid)
            .env("RECO_MODE", if cfg!(target_os = "macos") { "bundle" } else { "dir" })
            .env("RECO_ROOT", &target.root)
            .env("RECO_NEW", new_root)
            .env("RECO_STAGING", &staging)
            .env("RECO_EXE", &target.exe)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .context("start installer")?;
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let script = script_dir.join(format!("reco-update-{pid}.cmd"));
        std::fs::write(&script, WINDOWS_SCRIPT.replace('\n', "\r\n"))?;
        let mut cmd = std::process::Command::new("cmd");
        if !target.relaunch {
            cmd.env("RECO_NO_LAUNCH", "1");
        }
        cmd.arg("/C")
            .arg(&script)
            .env("RECO_PID", &pid)
            .env("RECO_ROOT", &target.root)
            .env("RECO_NEW", new_root)
            .env("RECO_STAGING", &staging)
            .env("RECO_EXE", &target.exe)
            .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
            .spawn()
            .context("start installer")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rel(tag: &str, pre: bool, assets: &[&str]) -> serde_json::Value {
        json!({
            "tag_name": tag, "prerelease": pre, "draft": false,
            "html_url": format!("https://github.com/{REPO}/releases/tag/{tag}"),
            "assets": assets.iter().map(|n| json!({
                "name": n, "size": 10,
                "browser_download_url": format!("https://github.com/{REPO}/releases/download/{tag}/{n}")
            })).collect::<Vec<_>>()
        })
    }
    fn names(tag: &str) -> Vec<String> {
        let z = format!("reco-panopitch-{tag}{}", asset_suffix());
        vec![z.clone(), format!("{z}.sha256")]
    }
    fn full(tag: &str, pre: bool) -> serde_json::Value {
        let n = names(tag);
        rel(tag, pre, &[&n[0], &n[1]])
    }

    #[test]
    fn prerelease_user_sees_newer_prerelease() {
        let all = json!([full("v1.0.0-beta.2", true), full("v1.0.0-beta.1", true)]);
        let cur = Version::parse("1.0.0-beta.1").unwrap();
        assert_eq!(pick_release(&all, &cur, false).unwrap().tag, "v1.0.0-beta.2");
    }

    #[test]
    fn stable_user_ignores_prerelease_unless_opted_in() {
        let all = json!([full("v1.1.0-rc.1", true), full("v1.0.0", false)]);
        let cur = Version::parse("1.0.0").unwrap();
        assert!(pick_release(&all, &cur, false).is_none());
        assert_eq!(pick_release(&all, &cur, true).unwrap().tag, "v1.1.0-rc.1");
    }

    #[test]
    fn beta_updates_to_final() {
        let all = json!([full("v1.0.0", false)]);
        let cur = Version::parse("1.0.0-beta.3").unwrap();
        assert_eq!(pick_release(&all, &cur, false).unwrap().tag, "v1.0.0");
    }

    #[test]
    fn never_downgrades_and_skips_drafts() {
        let mut draft = full("v2.0.0", false);
        draft["draft"] = json!(true);
        let all = json!([draft, full("v0.9.0", false)]);
        assert!(pick_release(&all, &Version::parse("1.0.0").unwrap(), false).is_none());
    }

    #[test]
    fn skips_release_without_asset_or_checksum_for_this_platform() {
        let other = rel("v2.0.0", false, &["reco-panopitch-v2.0.0-plan9-mips.zip"]);
        let n = names("v1.5.0");
        let no_sha = rel("v1.5.0", false, &[&n[0]]);
        let all = json!([other, no_sha, full("v1.2.0", false)]);
        let r = pick_release(&all, &Version::parse("1.0.0").unwrap(), false).unwrap();
        assert_eq!(r.tag, "v1.2.0");
    }

    #[test]
    fn rejects_untrusted_asset_hosts() {
        let n = names("v2.0.0");
        let mut r = rel("v2.0.0", false, &[&n[0], &n[1]]);
        r["assets"][0]["browser_download_url"] = json!("https://evil.example/x.zip");
        assert!(pick_release(&json!([r]), &Version::parse("1.0.0").unwrap(), false).is_none());
    }

    #[test]
    fn trusted_url_rules() {
        assert!(is_trusted_url("https://github.com/a/b"));
        assert!(is_trusted_url("https://objects.githubusercontent.com/x"));
        assert!(!is_trusted_url("http://github.com/a"));
        assert!(!is_trusted_url("https://github.com.evil.io/a"));
        assert!(!is_trusted_url("https://evil.io/@github.com/a"));
        assert!(!is_trusted_url("https://github.com@evil.io/a"));
    }

    #[test]
    fn sha256_parsing() {
        let h = "a".repeat(64);
        assert_eq!(parse_sha256(&format!("{h}  file.zip\n")), Some(h.clone()));
        assert_eq!(parse_sha256(&format!("{}\n", h.to_uppercase())), Some(h.clone()));
        assert_eq!(parse_sha256(&format!("{h} *file.zip")), Some(h));
        assert_eq!(parse_sha256("nothex"), None);
        assert_eq!(parse_sha256(""), None);
    }

    /// Real end-to-end swap with a release zip built by
    /// `scripts/build-macos-app.sh`. Run manually:
    ///   RECO_TEST_ZIP=<zip> RECO_TEST_APP=<installed .app> RECO_NO_LAUNCH=1 \
    ///     cargo test -p reco-update -- --ignored e2e_stage_and_spawn
    /// then check that the .app was replaced after the test process exits.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn e2e_stage_and_spawn() {
        let zip = PathBuf::from(std::env::var("RECO_TEST_ZIP").unwrap());
        let app = PathBuf::from(std::env::var("RECO_TEST_APP").unwrap());
        let target = InstallTarget {
            root: app.clone(),
            exe: app.join("Contents/MacOS/reco-panopitch"),
            relaunch: true,
        };
        let new_root = stage(&zip, &target).unwrap();
        assert!(new_root.join("Contents/MacOS/reco-panopitch").is_file());
        spawn_installer(&target, &new_root).unwrap();
    }

    #[cfg(unix)]
    fn run_script(mode: &str, root: &Path, new: &Path, staging: &Path) {
        let dir = root.parent().unwrap();
        let sh = dir.join("u.sh");
        std::fs::write(&sh, UNIX_SCRIPT).unwrap();
        let st = std::process::Command::new("/bin/sh")
            .arg(&sh)
            .env("RECO_PID", "999999999") // not running -> no waiting
            .env("RECO_MODE", mode)
            .env("RECO_ROOT", root)
            .env("RECO_NEW", new)
            .env("RECO_STAGING", staging)
            .env("RECO_EXE", root.join("exe"))
            .env("RECO_NO_LAUNCH", "1")
            .status()
            .unwrap();
        assert!(st.success());
        assert!(!sh.exists(), "script removes itself");
    }

    #[cfg(unix)]
    #[test]
    fn bundle_swap_replaces_and_cleans_up() {
        let d = tempfile::tempdir().unwrap();
        let (root, new) = (d.path().join("A.app"), d.path().join("new.app"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(root.join("v"), "old").unwrap();
        std::fs::write(new.join("v"), "new").unwrap();
        run_script("bundle", &root, &new, &new);
        assert_eq!(std::fs::read_to_string(root.join("v")).unwrap(), "new");
        assert!(!new.exists());
        assert!(!d.path().join("A.app.old").exists());
    }

    #[cfg(unix)]
    #[test]
    fn bundle_swap_rolls_back_when_new_is_missing() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("A.app");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("v"), "old").unwrap();
        run_script("bundle", &root, &d.path().join("missing.app"), &d.path().join("x"));
        assert_eq!(std::fs::read_to_string(root.join("v")).unwrap(), "old");
    }

    #[cfg(unix)]
    #[test]
    fn dir_mode_overlays_and_keeps_user_files() {
        let d = tempfile::tempdir().unwrap();
        let (root, stg) = (d.path().join("inst"), d.path().join("stg"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&stg).unwrap();
        std::fs::write(root.join("app"), "old").unwrap();
        std::fs::write(root.join("my-setup.json"), "mine").unwrap();
        std::fs::write(stg.join("app"), "new").unwrap();
        run_script("dir", &root, &stg, &stg);
        assert_eq!(std::fs::read_to_string(root.join("app")).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(root.join("my-setup.json")).unwrap(), "mine");
        assert!(!stg.exists());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn zip_slip_is_rejected() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let zp = dir.path().join("evil.zip");
        {
            let mut w = zip::ZipWriter::new(std::fs::File::create(&zp).unwrap());
            let o = zip::write::SimpleFileOptions::default();
            w.start_file("../escape.txt", o).unwrap();
            w.write_all(b"x").unwrap();
            w.finish().unwrap();
        }
        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();
        assert!(extract(&zp, &out).is_err());
        assert!(!dir.path().join("escape.txt").exists());
    }
}
