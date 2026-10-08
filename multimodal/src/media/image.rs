// SPDX-FileCopyrightText: Copyright (c) 2024-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use backends::{ImageDecodeOutcome, ImageDecodeRequest, image_reader_backend, turbojpeg_backend};

mod backends;
pub use backends::{BackendDecline, DecodedImage, PixelFormat};

const DEFAULT_MAX_ALLOC: u64 = 128 * 1024 * 1024;

/// Decoder allocation limits, including the final channel-preserving output.
/// `None` removes a limit. The default allocation cap is 128 MiB.
#[derive(Clone, Debug)]
pub struct ImageDecoderLimits {
    pub max_image_width: Option<u32>,
    pub max_image_height: Option<u32>,
    pub max_alloc: Option<u64>,
}

impl Default for ImageDecoderLimits {
    fn default() -> Self {
        Self {
            max_image_width: None,
            max_image_height: None,
            max_alloc: Some(DEFAULT_MAX_ALLOC),
        }
    }
}

impl ImageDecoderLimits {
    fn validate_output(&self, width: u32, height: u32, channels: usize) -> Result<usize> {
        if self.max_image_width.is_some_and(|limit| width > limit)
            || self.max_image_height.is_some_and(|limit| height > limit)
        {
            anyhow::bail!("Image dimensions exceed configured limits: {width}x{height}");
        }

        let nbytes = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|pixels| pixels.checked_mul(channels as u64))
            .ok_or_else(|| {
                anyhow::anyhow!("Image allocation size overflow for dimensions: {width}x{height}")
            })?;
        if let Some(limit) = self.max_alloc
            && nbytes > limit
        {
            anyhow::bail!("Image allocation {nbytes} bytes exceeds configured limit {limit} bytes");
        }
        usize::try_from(nbytes)
            .map_err(|_| anyhow::anyhow!("Image allocation does not fit in usize: {nbytes} bytes"))
    }
}

/// Explicit JPEG selection; limits are never bypassed by fallback.
#[derive(Clone, Debug)]
pub struct ImageOptions {
    pub limits: ImageDecoderLimits,
    pub enable_libjpeg: bool,
    /// Reject a declined TurboJPEG input before trying ImageReader.
    pub require_libjpeg: bool,
}

impl Default for ImageOptions {
    fn default() -> Self {
        Self {
            limits: ImageDecoderLimits::default(),
            enable_libjpeg: true,
            require_libjpeg: false,
        }
    }
}

/// A caller-required TurboJPEG backend declined the input.
#[derive(Debug)]
pub struct JpegFallbackRequired(pub BackendDecline);

impl std::fmt::Display for JpegFallbackRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "libjpeg_turbo was required, but the input would have fallen back to image::ImageReader: {}",
            self.0
        )
    }
}
impl std::error::Error for JpegFallbackRequired {}

/// Whether the optional system TurboJPEG library can be loaded.
pub fn turbojpeg_available() -> bool {
    super::jpeg_turbo::available()
}

/// Decode one image to owned HWC u8 pixels, preserving L/LA/RGB/RGBA channels.
/// Higher-depth samples use the same image-crate conversion as Dynamo.
pub fn decode_image(bytes: &[u8], options: &ImageOptions) -> Result<DecodedImage> {
    let format = image::guess_format(bytes)?;
    let request = ImageDecodeRequest {
        bytes,
        format,
        limits: &options.limits,
    };
    let turbojpeg = turbojpeg_backend();
    if options.enable_libjpeg && turbojpeg.supports(format) {
        match turbojpeg.try_decode(request)? {
            ImageDecodeOutcome::Decoded(image) => return Ok(image),
            ImageDecodeOutcome::NotHandled(reason) if options.require_libjpeg => {
                return Err(JpegFallbackRequired(reason).into());
            }
            ImageDecodeOutcome::NotHandled(_) => {}
        }
    }
    let image_reader = image_reader_backend();
    match image_reader.try_decode(request)? {
        ImageDecodeOutcome::Decoded(image) => Ok(image),
        ImageDecodeOutcome::NotHandled(reason) => {
            anyhow::bail!("{} did not handle the input: {reason}", image_reader.name())
        }
    }
}

/// Decode an ordered batch using the crate's explicitly configured execution.
/// With the pool unarmed this runs on the caller. On failure, the selected
/// error is unspecified and some other items may already have completed.
pub fn decode_images<T: AsRef<[u8]> + Send + Sync>(
    inputs: &[T],
    options: &ImageOptions,
) -> Result<Vec<DecodedImage>> {
    crate::execution::try_map(inputs, |bytes| decode_image(bytes.as_ref(), options))
}
