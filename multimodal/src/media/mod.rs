// SPDX-FileCopyrightText: Copyright (c) 2024-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Owned media decoding with Dynamo's pixel and resource-limit semantics.
//!
//! These APIs are separate from the Pillow-compatible `image::decode` API.
//! Fetching, configuration merging, async scheduling, hashing and registration
//! belong to the caller. No environment variables are read here.

#[cfg(feature = "media-decode")]
pub mod image;
#[cfg(feature = "media-decode")]
mod jpeg_turbo;
#[cfg(feature = "video")]
pub mod video;

#[cfg(all(feature = "video", not(target_os = "linux")))]
compile_error!(
    "the video feature currently requires Linux memfd and procfs; image decoding does not require it"
);
