// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

#![cfg(feature = "media-decode")]

use base64::{Engine as _, engine::general_purpose::STANDARD};
use dynamo_multimodal::image::decode::{DecodeLimits, decode_rgb};
use dynamo_multimodal::media::image::{
    ImageOptions, JpegFallbackRequired, PixelFormat, decode_image, decode_images,
    turbojpeg_available,
};
use image::{DynamicImage, ImageBuffer, ImageFormat};

fn encode(image: DynamicImage, format: ImageFormat) -> Vec<u8> {
    let mut output = std::io::Cursor::new(Vec::new());
    image.write_to(&mut output, format).unwrap();
    output.into_inner()
}

#[test]
fn preserves_channels_and_depth_conversion_without_changing_rgb_api() {
    let cases = [
        (
            DynamicImage::ImageLuma8(ImageBuffer::from_raw(2, 1, vec![5, 230]).unwrap()),
            PixelFormat::L8,
            vec![5, 230],
        ),
        (
            DynamicImage::ImageLumaA8(ImageBuffer::from_raw(1, 1, vec![5, 19]).unwrap()),
            PixelFormat::La8,
            vec![5, 19],
        ),
        (
            DynamicImage::ImageRgb8(ImageBuffer::from_raw(1, 1, vec![1, 2, 3]).unwrap()),
            PixelFormat::Rgb8,
            vec![1, 2, 3],
        ),
        (
            DynamicImage::ImageRgba8(ImageBuffer::from_raw(1, 1, vec![1, 2, 3, 4]).unwrap()),
            PixelFormat::Rgba8,
            vec![1, 2, 3, 4],
        ),
        (
            DynamicImage::ImageLuma16(ImageBuffer::from_raw(2, 1, vec![0, u16::MAX]).unwrap()),
            PixelFormat::L8,
            vec![0, 255],
        ),
    ];
    for (image, format, expected) in cases {
        let high_depth = image.color().bits_per_pixel() > 8 * image.color().channel_count() as u16;
        let bytes = encode(image, ImageFormat::Png);
        let decoded = decode_image(&bytes, &ImageOptions::default()).unwrap();
        assert_eq!(decoded.source_format, ImageFormat::Png);
        assert_eq!(decoded.pixel_format, format);
        assert_eq!(decoded.pixels, expected);
        let rgb = decode_rgb(&bytes, &DecodeLimits::default());
        if high_depth {
            assert!(rgb.is_err());
        } else {
            assert_eq!(
                rgb.unwrap().0.len(),
                (decoded.width * decoded.height * 3) as usize
            );
        }
    }
}

#[test]
fn dynamo_formats_do_not_expand_the_existing_rgb_contract() {
    let bytes = encode(DynamicImage::new_rgb8(2, 2), ImageFormat::Tiff);
    assert!(decode_image(&bytes, &ImageOptions::default()).is_ok());
    assert!(decode_rgb(&bytes, &DecodeLimits::default()).is_err());
    assert!(dynamo_multimodal::image::decode::dimensions(&bytes).is_err());
}

#[test]
fn limits_apply_to_decoder_and_channel_preserving_output() {
    let bytes = encode(DynamicImage::new_rgba8(8, 9), ImageFormat::Png);
    let mut options = ImageOptions::default();
    options.limits.max_image_width = Some(7);
    assert!(decode_image(&bytes, &options).is_err());
    options.limits.max_image_width = None;
    options.limits.max_alloc = Some(8 * 9 * 3);
    assert!(decode_image(&bytes, &options).is_err());
    options.limits.max_alloc = None;
    assert_eq!(
        decode_image(&bytes, &options).unwrap().pixels.len(),
        8 * 9 * 4
    );
}

#[test]
fn jpeg_selection_preserves_pixels_limits_and_cmyk_fallback() {
    if std::env::var_os("DYNAMO_REQUIRE_LIBJPEG_TURBO_TEST").is_some() {
        assert!(
            turbojpeg_available(),
            "system TurboJPEG is required by this test run"
        );
    }
    let jpeg = STANDARD
        .decode(include_str!("fixtures/media/pil_parity_17x11.jpg.b64").trim())
        .unwrap();
    let expected = STANDARD
        .decode(include_str!("fixtures/media/pil_parity_17x11.rgb.b64").trim())
        .unwrap();
    let mut options = ImageOptions {
        require_libjpeg: true,
        ..Default::default()
    };
    let decoded = decode_image(&jpeg, &options);
    if turbojpeg_available() {
        assert_eq!(decoded.unwrap().pixels, expected);
        options.limits.max_alloc = Some(0);
        let error = decode_image(&jpeg, &options).unwrap_err();
        assert!(error.to_string().contains("exceeds configured limit"));
        assert!(error.downcast_ref::<JpegFallbackRequired>().is_none());
    } else {
        assert!(decoded.unwrap_err().is::<JpegFallbackRequired>());
    }
    let cmyk = STANDARD
        .decode(include_str!("fixtures/media/cmyk_2x2.jpg.b64").trim())
        .unwrap();
    options.limits.max_alloc = None;
    assert!(
        decode_image(&cmyk, &options)
            .unwrap_err()
            .is::<JpegFallbackRequired>()
    );
    options.require_libjpeg = false;
    let fallback = decode_image(&cmyk, &options).unwrap();
    options.enable_libjpeg = false;
    assert_eq!(
        fallback.pixels,
        decode_image(&cmyk, &options).unwrap().pixels
    );
}

#[test]
fn batch_keeps_input_order_and_owns_pixels_after_inputs_are_dropped() {
    let inputs: Vec<_> = (1..8)
        .map(|width| encode(DynamicImage::new_rgb8(width, 2), ImageFormat::Png))
        .collect();
    let decoded = decode_images(&inputs, &ImageOptions::default()).unwrap();
    drop(inputs);
    for (index, image) in decoded.iter().enumerate() {
        assert_eq!(image.width as usize, index + 1);
        assert_eq!(image.pixels, vec![0; (index + 1) * 2 * 3]);
    }
    assert!(decode_images(&[b"invalid".as_slice()], &ImageOptions::default()).is_err());
}
