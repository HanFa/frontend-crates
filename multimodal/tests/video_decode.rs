// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

#![cfg(all(feature = "video", target_os = "linux"))]

use dynamo_multimodal::media::video::{
    VideoDecoderLimits, VideoOptions, decode_video, decode_videos,
};

const VIDEO: &[u8] = include_bytes!("fixtures/media/240p_10.mp4");

#[test]
fn sampling_preserves_dimensions_metadata_and_allocation_limits() {
    let mut options = VideoOptions {
        num_frames: Some(5),
        strict: true,
        ..Default::default()
    };
    let video = decode_video(VIDEO.to_vec(), &options).unwrap();
    assert_eq!((video.num_frames, video.height, video.width), (5, 240, 320));
    assert_eq!(video.pixels.len(), 5 * 240 * 320 * 3);
    assert_eq!(video.metadata.source_fps, 1.0);
    assert_eq!(video.metadata.source_duration, 10.0);
    assert_eq!(video.metadata.sampled_timestamps, [0., 3., 5., 7., 9.]);
    options.limits = VideoDecoderLimits {
        max_alloc: Some(video.pixels.len() as u64),
    };
    assert_eq!(
        decode_video(VIDEO.to_vec(), &options).unwrap().pixels,
        video.pixels
    );
    options.limits.max_alloc = Some(video.pixels.len() as u64 - 1);
    assert!(
        decode_video(VIDEO.to_vec(), &options)
            .unwrap_err()
            .to_string()
            .contains("exceed max alloc")
    );
}

#[test]
fn fps_sampling_and_max_frames_keep_dynamo_precedence() {
    let options = VideoOptions {
        fps: Some(0.5),
        max_frames: Some(3),
        ..Default::default()
    };
    let video = decode_video(VIDEO.to_vec(), &options).unwrap();
    assert_eq!(video.num_frames, 3);
    assert_eq!(video.metadata.sampled_timestamps, [0., 5., 9.]);
    let options = VideoOptions {
        max_frames: Some(32),
        ..Default::default()
    };
    assert_eq!(
        decode_video(VIDEO.to_vec(), &options).unwrap().num_frames,
        10
    );
    let options = VideoOptions {
        num_frames: Some(1),
        ..Default::default()
    };
    assert_eq!(
        decode_video(VIDEO.to_vec(), &options)
            .unwrap()
            .metadata
            .sampled_timestamps,
        [5.]
    );
}

#[test]
fn incompatible_sampling_options_fail_before_opening_media() {
    for options in [
        VideoOptions {
            fps: Some(1.),
            num_frames: Some(1),
            ..Default::default()
        },
        VideoOptions {
            max_frames: Some(1),
            num_frames: Some(1),
            ..Default::default()
        },
    ] {
        assert!(
            decode_video(vec![], &options)
                .unwrap_err()
                .to_string()
                .contains("cannot be specified at the same time")
        );
    }
}

#[test]
fn strict_and_lenient_conversion_errors_match_dynamo() {
    let bytes = include_bytes!("fixtures/media/dynamic_resolution_4.mp4");
    let options = VideoOptions {
        num_frames: Some(4),
        ..Default::default()
    };
    let video = decode_video(bytes.to_vec(), &options).unwrap();
    assert_eq!((video.num_frames, video.height, video.width), (2, 16, 16));
    assert_eq!(video.metadata.sampled_timestamps, [0., 0.5]);
    let options = VideoOptions {
        strict: true,
        ..options
    };
    assert!(
        decode_video(bytes.to_vec(), &options)
            .unwrap_err()
            .to_string()
            .contains("FFmpeg RGB conversion error")
    );
}

#[test]
fn video_batches_own_ordered_results() {
    let inputs = [include_bytes!("fixtures/media/2p_10.mp4").as_slice(), VIDEO];
    let options = VideoOptions {
        num_frames: Some(2),
        ..Default::default()
    };
    let videos = decode_videos(&inputs, &options).unwrap();
    assert_eq!(videos.iter().map(|v| v.width).collect::<Vec<_>>(), [2, 320]);
    for (bytes, video) in inputs.iter().zip(videos) {
        let reference = decode_video(bytes.to_vec(), &options).unwrap();
        assert_eq!(video.pixels, reference.pixels);
        assert_eq!(video.metadata, reference.metadata);
    }
}
