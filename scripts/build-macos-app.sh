#!/usr/bin/env bash
# Build an ad-hoc-signed "RECO Panopitch.app" bundle on macOS.
#
# Builds the GUI (reco-panopitch) and CLI (reco) in release mode and
# assembles:
#
#   RECO Panopitch.app/Contents/
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
# Requirements: Xcode command line tools (codesign, plutil), Rust, and
# FFmpeg 7 via Homebrew (current Homebrew `ffmpeg` is 9.x, which
# ffmpeg-next 8.1 does not compile against):
#   brew install ffmpeg@7 pkg-config
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

# Point pkg-config at Homebrew's ffmpeg@7 unless the caller already did.
if [[ "${PKG_CONFIG_PATH:-}" != *ffmpeg@7* ]]; then
    if command -v brew >/dev/null && brew --prefix ffmpeg@7 >/dev/null 2>&1; then
        ff7="$(brew --prefix ffmpeg@7)"
        export PKG_CONFIG_PATH="$ff7/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
        echo "Using FFmpeg 7 from $ff7"
    else
        echo "warning: ffmpeg@7 not found via Homebrew; build may fail against FFmpeg 9.x" >&2
    fi
fi

target_args=()
if [[ -n "${CARGO_TARGET:-}" ]]; then
    target_args=(--target "$CARGO_TARGET")
    bin_dir="$repo_root/target/$CARGO_TARGET/release"
else
    bin_dir="$repo_root/target/release"
fi

if [[ $skip_build -eq 0 ]]; then
    echo "Building $GUI_BIN and $CLI_BIN (release)..."
    cargo build --release ${target_args[@]+"${target_args[@]}"} \
        -p reco-gui --bin "$GUI_BIN" \
        -p reco-cli --bin "$CLI_BIN" \
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
    <string>12.0</string>
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
for f in "$app/Contents/MacOS/"*; do
    codesign --force --sign - "$f"
done
codesign --force --sign - "$app"
codesign --verify --verbose=2 "$app"

echo
echo "Done: $app"
echo "Not notarized: on first launch, right-click the app and choose Open,"
echo "or run: xattr -dr com.apple.quarantine \"$app\""
