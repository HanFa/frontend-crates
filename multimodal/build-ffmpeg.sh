#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Native test/development build, matching Dynamo's wheel_builder codec surface.
# Usage: bash multimodal/build-ffmpeg.sh /absolute/install/prefix
set -euo pipefail
prefix=${1:?provide an absolute installation prefix}
[[ "$prefix" = /* ]] || { echo 'prefix must be absolute' >&2; exit 1; }
jobs=${CMAKE_BUILD_PARALLEL_LEVEL:-8}
mkdir -p "$prefix/src" "$prefix/lib/pkgconfig"
cd "$prefix/src"
curl --fail --location --retry 3 -o libvpx-1.14.1.tar.gz \
    https://github.com/webmproject/libvpx/archive/refs/tags/v1.14.1.tar.gz
curl --fail --location --retry 3 -o ffmpeg-9.0.1.tar.xz \
    https://ffmpeg.org/releases/ffmpeg-9.0.1.tar.xz
sha256sum --check <<'CHECKSUMS'
901747254d80a7937c933d03bd7c5d41e8e6c883e0665fadcb172542167c7977  libvpx-1.14.1.tar.gz
cf38e0e28c7e5605942c4a77755349b0145804a397af37eb1fb4c77cb237f635  ffmpeg-9.0.1.tar.xz
CHECKSUMS
tar xf libvpx-1.14.1.tar.gz
tar xf ffmpeg-9.0.1.tar.xz
cd libvpx-1.14.1
./configure --prefix="$prefix" --enable-shared --disable-static \
    --disable-examples --disable-unit-tests --disable-tools --disable-docs
make -j"$jobs"
make install
cd ../ffmpeg-9.0.1
export PKG_CONFIG_PATH="$prefix/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
export LD_LIBRARY_PATH="$prefix/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
./configure --prefix="$prefix" --build-suffix=_dynamo \
    --disable-gpl --disable-nonfree --disable-doc --disable-static \
    --disable-x86asm --disable-network \
    --disable-bsfs --enable-bsf=h264_mp4toannexb,hevc_mp4toannexb \
    --disable-devices --disable-libdrm --enable-shared --enable-libvpx \
    --disable-encoders --enable-encoder=libvpx_vp9 \
    --disable-decoders --enable-decoder=vp8,vp9,rawvideo \
    --disable-muxers --enable-muxer=mov,mp4,matroska,webm \
    --disable-demuxers --enable-demuxer=mov,matroska,rawvideo \
    --disable-parsers --enable-parser=vp8,vp9 \
    --disable-protocols --enable-protocol=file,pipe,fd
make -j"$jobs"
make install
for pc in "$prefix"/lib/pkgconfig/*_dynamo.pc; do
    ln -sf "$(basename "$pc")" "${pc%_dynamo.pc}.pc"
done
# A positive check prevents a broken loader from passing the absence checks.
"$prefix/bin/ffmpeg" -hide_banner -encoders > "$prefix/encoders.txt" 2>&1
grep -qiE 'libvpx[-_]vp9' "$prefix/encoders.txt"
for surface in encoders decoders parsers; do
    "$prefix/bin/ffmpeg" -hide_banner "-$surface" > "$prefix/$surface.txt" 2>&1
    if grep -qiE 'h\.?264|h\.?265|hevc|(^| )aac|nvenc|cuvid|nvdec' "$prefix/$surface.txt"; then
        echo "Disallowed codec in $surface" >&2
        exit 1
    fi
done
# Keep both source archives, extracted sources and license texts in prefix/src.
