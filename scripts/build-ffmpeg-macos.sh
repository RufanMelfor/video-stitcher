#!/usr/bin/env bash
# Build a self-contained FFmpeg 7.1 for the macOS app bundle.
#
# Why: Homebrew's ffmpeg@7 links ~60 Homebrew dylibs by absolute path and
# is built for the *current* macOS (a bottle built on macOS 27 refuses to
# load on older systems). An app bundle that ships those is neither
# relocatable nor portable. This script builds FFmpeg from source with:
#
#   - a fixed deployment target (default macOS 12.0),
#   - x264 and SVT-AV1 linked *statically* into libavcodec,
#   - Apple VideoToolbox / AudioToolbox (hardware decode + encode),
#   - --disable-autodetect, so nothing from /opt/homebrew leaks in.
#
# The result only depends on system libraries (libSystem, libz, libbz2
# and Apple frameworks) and can be copied into a .app.
#
# Output: target/ffmpeg-macos/{lib,include,lib/pkgconfig}
#
# Usage: scripts/build-ffmpeg-macos.sh [--force]
# Needs: Xcode command line tools, cmake, git, curl, make
#        (brew install cmake pkg-config)
#
# FFmpeg is configured with --enable-gpl (libx264). RECO Panopitch is
# AGPL-3.0, which is compatible; see THIRD_PARTY_NOTICES.md.

set -euo pipefail

FFMPEG_VERSION="${FFMPEG_VERSION:-7.1.1}"
X264_REF="${X264_REF:-stable}"
SVTAV1_TAG="${SVTAV1_TAG:-v2.3.0}"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-12.0}"

die() { echo "error: $*" >&2; exit 1; }
[[ "$(uname -s)" == "Darwin" ]] || die "macOS only"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
prefix="$repo_root/target/ffmpeg-macos"
work="$repo_root/target/ffmpeg-macos-build"
jobs="$(sysctl -n hw.ncpu)"

if [[ "${1:-}" == "--force" ]]; then rm -rf "$prefix" "$work"; fi
if [[ -f "$prefix/lib/pkgconfig/libavcodec.pc" ]]; then
    echo "FFmpeg already built in $prefix (use --force to rebuild)"
    exit 0
fi
for t in cmake git curl make clang; do command -v "$t" >/dev/null || die "'$t' not found"; done

mkdir -p "$prefix" "$work"
export PKG_CONFIG_LIBDIR="$prefix/lib/pkgconfig"; unset PKG_CONFIG_PATH
# Make sure Homebrew prefixes can't be picked up by accident.
export PATH="/usr/bin:/bin:/usr/sbin:/sbin:$(dirname "$(command -v cmake)"):$(dirname "$(command -v pkg-config 2>/dev/null || echo /usr/bin/pkg-config)")"
cflags="-mmacosx-version-min=$MACOSX_DEPLOYMENT_TARGET -O2"

echo "== x264 ($X264_REF) =="
if [[ ! -f "$prefix/lib/libx264.a" ]]; then
    [[ -d "$work/x264" ]] || git clone --depth 1 --branch "$X264_REF" \
        https://code.videolan.org/videolan/x264.git "$work/x264"
    (cd "$work/x264" &&
        ./configure --prefix="$prefix" --enable-static --enable-pic --disable-cli \
            --extra-cflags="$cflags" --extra-ldflags="-mmacosx-version-min=$MACOSX_DEPLOYMENT_TARGET" &&
        make -j"$jobs" && make install)
fi

echo "== SVT-AV1 ($SVTAV1_TAG) =="
if [[ ! -f "$prefix/lib/libSvtAv1Enc.a" ]]; then
    [[ -d "$work/svtav1" ]] || git clone --depth 1 --branch "$SVTAV1_TAG" \
        https://gitlab.com/AOMediaCodec/SVT-AV1.git "$work/svtav1"
    cmake -S "$work/svtav1" -B "$work/svtav1/build" -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_INSTALL_PREFIX="$prefix" -DBUILD_SHARED_LIBS=OFF -DBUILD_APPS=OFF \
        -DBUILD_DEC=OFF -DBUILD_TESTING=OFF -DCMAKE_OSX_DEPLOYMENT_TARGET="$MACOSX_DEPLOYMENT_TARGET" \
        -DCMAKE_POSITION_INDEPENDENT_CODE=ON
    cmake --build "$work/svtav1/build" -j"$jobs" && cmake --install "$work/svtav1/build"
fi

echo "== FFmpeg $FFMPEG_VERSION =="
src="$work/ffmpeg-$FFMPEG_VERSION"
if [[ ! -d "$src" ]]; then
    curl -fL "https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz" -o "$work/ffmpeg.tar.xz"
    tar -xf "$work/ffmpeg.tar.xz" -C "$work"
fi
(cd "$src" &&
    ./configure --prefix="$prefix" \
        --enable-shared --disable-static --disable-programs --disable-doc \
        --disable-autodetect --enable-gpl --enable-pic \
        --enable-libx264 --enable-libsvtav1 \
        --enable-videotoolbox --enable-audiotoolbox \
        --enable-zlib --enable-bzlib \
        --extra-cflags="$cflags -I$prefix/include" \
        --extra-ldflags="-mmacosx-version-min=$MACOSX_DEPLOYMENT_TARGET -L$prefix/lib" &&
    make -j"$jobs" && make install)

echo
echo "Done: $prefix"
otool -L "$prefix/lib/libavcodec.61.dylib" | sed -n '2,$p'
