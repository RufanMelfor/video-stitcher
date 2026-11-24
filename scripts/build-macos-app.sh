#!/usr/bin/env bash
# Build an ad-hoc-signed "RECO Panopitch.app" bundle on macOS.
#
# Builds the GUI (reco-panopitch) and CLI (reco) in release mode and
# assembles:
#
#   RECO Panopitch.app/Contents/
#     Frameworks/                   bundled FFmpeg dylibs (self-built, see build-ffmpeg-macos.sh)
#     Info.plist
#     MacOS/reco-panopitch          GUI (bundle executable)
#     MacOS/reco                    CLI
#     MacOS/libonnxruntime.dylib    optional, see ORT_LIB below
#     Resources/scoreboards/        scoreboard packages
#     Resources/LICENSE
#
# Runtime lookup this layout relies on (no code changes needed):
#   - scoreboards: reco-scoreboard checks <exe>/../Resources/scoreboards
#   - ONNX Runtime (load-dynamic builds): checked next to the executable
#   - lens profiles (resources/profiles.cbor.gz) are embedded at compile
#     time, so resources/ is not copied.
#
# Requirements: Xcode command line tools (codesign, plutil), Rust, cmake and
# pkg-config (brew install cmake pkg-config). FFmpeg 7 is built from source
# on first run (scripts/build-ffmpeg-macos.sh, a few minutes) because
# Homebrew's ffmpeg is 9.x (too new for ffmpeg-next 8.1), and Homebrew's
# ffmpeg@7 is not relocatable.
#
# Usage:
#   scripts/build-macos-app.sh [--skip-build] [--out DIR] [-- <extra cargo args>]
#
# Environment:
#   ORT_LIB        path to libonnxruntime*.dylib to bundle (optional)
#   CARGO_TARGET   target triple, e.g. aarch64-apple-darwin (optional;
#                  default is the host target)
#
# Status: written from the steps reported in
# RufanMelfor/video-stitcher#1. NOT yet run on a Mac by the maintainers.

set -euo pipefail

APP_NAME="RECO Panopitch"
BUNDLE_ID="org.reco.panopitch"
GUI_BIN="reco-panopitch"
CLI_BIN="reco"

die() { echo "error: $*" >&2; exit 1; }

[[ "$(uname -s)" == "Darwin" ]] || die "this script only runs on macOS"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

skip_build=0
out_dir="$repo_root/target/macos-app"
cargo_extra=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-build) skip_build=1; shift ;;
        --out) [[ $# -ge 2 ]] || die "--out needs a directory"; out_dir="$2"; shift 2 ;;
        --) shift; cargo_extra=("$@"); break ;;
        -h|--help) sed -n '2,36p' "$0"; exit 0 ;;
        *) die "unknown argument: $1 (see --help)" ;;
    esac
done

for tool in cargo codesign plutil; do
    command -v "$tool" >/dev/null || die "'$tool' not found in PATH"
done

# FFmpeg: use our self-built, relocatable FFmpeg 7 (scripts/build-ffmpeg-macos.sh)
# so the app runs on a Mac without Homebrew and on macOS 12+. Set
# FFMPEG_PREFIX to use a different build, or HOMEBREW_FFMPEG=1 to link
# Homebrew's ffmpeg@7 for a quick local-only build (not relocatable).
if [[ "${HOMEBREW_FFMPEG:-0}" == "1" ]]; then
    ff7="$(brew --prefix ffmpeg@7)"
    export PKG_CONFIG_PATH="$ff7/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
    ffmpeg_prefix=""
    echo "Using Homebrew FFmpeg 7 (app will NOT run without Homebrew)"
else
    ffmpeg_prefix="${FFMPEG_PREFIX:-$repo_root/target/ffmpeg-macos}"
    if [[ ! -f "$ffmpeg_prefix/lib/pkgconfig/libavcodec.pc" ]]; then
        [[ -z "${FFMPEG_PREFIX:-}" ]] || die "FFMPEG_PREFIX=$FFMPEG_PREFIX has no libavcodec.pc"
        "$repo_root/scripts/build-ffmpeg-macos.sh"
    fi
    export PKG_CONFIG_LIBDIR="$ffmpeg_prefix/lib/pkgconfig"
    unset PKG_CONFIG_PATH
    echo "Using FFmpeg from $ffmpeg_prefix"
fi
# Keep every object (Rust and C) on the same minimum macOS as FFmpeg.
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-12.0}"

target_args=()
if [[ -n "${CARGO_TARGET:-}" ]]; then
    target_args=(--target "$CARGO_TARGET")
    bin_dir="$repo_root/target/$CARGO_TARGET/release"
else
    bin_dir="$repo_root/target/release"
fi

if [[ $skip_build -eq 0 ]]; then
    echo "Building $GUI_BIN and $CLI_BIN (release)..."
    # coreml: ONNX Runtime CoreML execution provider (GPU / Neural Engine)
    # for AI ball tracking. NO_COREML=1 falls back to CPU inference.
    coreml_features=(--features reco-gui/coreml,reco-cli/coreml)
    [[ "${NO_COREML:-0}" == "1" ]] && coreml_features=()
    cargo build --release ${target_args[@]+"${target_args[@]}"} \
        -p reco-gui --bin "$GUI_BIN" \
        -p reco-cli --bin "$CLI_BIN" \
        ${coreml_features[@]+"${coreml_features[@]}"} \
        ${cargo_extra[@]+"${cargo_extra[@]}"}
fi

[[ -x "$bin_dir/$GUI_BIN" ]] || die "missing $bin_dir/$GUI_BIN (build failed, or --skip-build without a prior build)"
[[ -x "$bin_dir/$CLI_BIN" ]] || die "missing $bin_dir/$CLI_BIN"

version="$(sed -n 's/^version *= *"\(.*\)"/\1/p' crates/reco-gui/Cargo.toml | head -1)"
[[ -n "$version" ]] || die "could not read version from crates/reco-gui/Cargo.toml"
# CFBundleVersion must be numeric dot-separated: strip a pre-release suffix.
short_version="${version%%-*}"

app="$out_dir/$APP_NAME.app"
echo "Assembling $app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

cp "$bin_dir/$GUI_BIN" "$app/Contents/MacOS/"
cp "$bin_dir/$CLI_BIN" "$app/Contents/MacOS/"
cp -R scoreboards "$app/Contents/Resources/scoreboards"
cp LICENSE "$app/Contents/Resources/"

if [[ -n "${ORT_LIB:-}" ]]; then
    [[ -f "$ORT_LIB" ]] || die "ORT_LIB=$ORT_LIB does not exist"
    cp "$ORT_LIB" "$app/Contents/MacOS/libonnxruntime.dylib"
    echo "Bundled ONNX Runtime from $ORT_LIB"
fi

if [[ -n "$ffmpeg_prefix" ]]; then
    python3 "$repo_root/scripts/bundle-macos-dylibs.py" "$app/Contents/Frameworks" "$ffmpeg_prefix" \
        "$app/Contents/MacOS/$GUI_BIN" "$app/Contents/MacOS/$CLI_BIN"
fi

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundleDisplayName</key>
    <string>$APP_NAME</string>
    <key>CFBundleIdentifier</key>
    <string>$BUNDLE_ID</string>
    <key>CFBundleExecutable</key>
    <string>$GUI_BIN</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$version</string>
    <key>CFBundleVersion</key>
    <string>$short_version</string>
    <key>LSMinimumSystemVersion</key>
    <string>$MACOSX_DEPLOYMENT_TARGET</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.video</string>
</dict>
</plist>
PLIST
plutil -lint "$app/Contents/Info.plist" >/dev/null || die "generated Info.plist is invalid"

# Ad-hoc signature (no Developer ID): enough to run locally on Apple
# Silicon. Sign nested code first, then the bundle.
echo "Ad-hoc signing..."
for f in "$app/Contents/Frameworks/"* "$app/Contents/MacOS/"*; do
    [[ -e "$f" ]] && codesign --force --sign - "$f"
done
codesign --force --sign - "$app"
codesign --verify --verbose=2 "$app"

# Release archive in the layout reco-update expects:
#   reco-panopitch-v<version>-macos-aarch64.zip (+ .sha256)
if [[ "${MAKE_ZIP:-1}" == "1" ]]; then
    arch="$(uname -m)"; [[ "$arch" == "arm64" ]] && arch="aarch64"
    zip_name="reco-panopitch-v$version-macos-$arch.zip"
    rm -f "$out_dir/$zip_name" "$out_dir/$zip_name.sha256"
    ditto -c -k --keepParent "$app" "$out_dir/$zip_name"
    (cd "$out_dir" && shasum -a 256 "$zip_name" > "$zip_name.sha256")
    echo "Release archive: $out_dir/$zip_name"
fi

echo
echo "Done: $app"
echo "Not notarized: on first launch, right-click the app and choose Open,"
echo "or run: xattr -dr com.apple.quarantine \"$app\""
