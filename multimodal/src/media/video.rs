// SPDX-FileCopyrightText: Copyright (c) 2024-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::io::Write;
use std::os::fd::AsRawFd;

use anyhow::Result;
use ffmpeg_next::Rational;
use memfile::{CreateOptions, MemFile, Seal};
use video_rs::Time;

/// Small time buffer (seconds) to avoid edge cases when seeking near frame boundaries
const FRAME_TIME_BUFFER_SECS: f64 = 0.001;
const DEFAULT_MAX_ALLOC: u64 = 512 * 1024 * 1024; // 512 MB

#[derive(Clone, Debug)]
pub struct VideoDecoderLimits {
    /// Maximum allowed total allocation of decoded frames in bytes
    pub max_alloc: Option<u64>,
}

impl Default for VideoDecoderLimits {
    fn default() -> Self {
        Self {
            max_alloc: Some(DEFAULT_MAX_ALLOC),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct VideoOptions {
    pub limits: VideoDecoderLimits,

    /// sample N frames per second
    pub fps: Option<f64>,
    /// sample at most N frames
    pub max_frames: Option<u64>,
    /// sample N frames in total (linspace)
    pub num_frames: Option<u64>,
    /// fail if some frames fail to decode
    pub strict: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VideoMetadata {
    pub source_fps: f64,
    pub source_duration: f64,
    pub sampled_timestamps: Vec<f64>,
}

fn get_num_requested_frames(
    config: &VideoOptions,
    duration_secs: f64,
    frame_rate: f64,
    mut total_frames: u64,
) -> Result<u64> {
    if total_frames == 0 && duration_secs > 0.0 && frame_rate > 0.0 {
        total_frames = (duration_secs * frame_rate) as u64;
    }

    anyhow::ensure!(total_frames > 0, "Cannot determine the video frame count");

    let requested_frames = if let Some(target_fps) = config.fps {
        // fps based sampling
        anyhow::ensure!(duration_secs > 0.0, "Cannot determine the video duration");
        (duration_secs * target_fps) as u64
    } else {
        // frame count based sampling; the last fallback is to decode all frames
        config.num_frames.unwrap_or(total_frames)
    };

    let requested_frames = requested_frames
        .min(config.max_frames.unwrap_or(requested_frames))
        .max(1);

    anyhow::ensure!(
        requested_frames > 0 && requested_frames <= total_frames,
        "Cannot decode {requested_frames} frames from {total_frames} total frames",
    );

    Ok(requested_frames)
}

fn get_target_times(
    requested_frames: u64,
    duration_secs: f64,
    frame_rate: f64,
) -> Result<Vec<Time>> {
    anyhow::ensure!(
        requested_frames > 0,
        "Invalid requested frames {requested_frames}"
    );
    anyhow::ensure!(duration_secs > 0.0, "Invalid duration {duration_secs}");
    anyhow::ensure!(frame_rate > 0.0, "Invalid frame rate {frame_rate}");

    let frame_duration = 1.0 / frame_rate;
    // Add small buffer to avoid edge cases
    // Variable frame rate might not work well here
    let last_frame_time = (duration_secs - frame_duration - FRAME_TIME_BUFFER_SECS).max(0.0);

    if requested_frames == 1 {
        return Ok(vec![Time::from_secs(last_frame_time as f32 / 2.0)]);
    }

    Ok((0..requested_frames)
        .map(|i| {
            let time_secs = (i as f64 * last_frame_time) / (requested_frames as f64 - 1.0);
            Time::from_secs(time_secs.max(0.0) as f32)
        })
        .collect())
}

fn get_frame_timestamp(frame: &ffmpeg_next::frame::Video, time_base: Rational) -> Result<f64> {
    anyhow::ensure!(!frame.is_corrupt(), "Frame is corrupt");

    // get timestamp from frame metadata: best_effort_timestamp or pts from ffmpeg
    let best_effort_pts = frame.timestamp();
    let pts = frame.pts();

    match best_effort_pts.or(pts) {
        Some(ts) => Ok(Time::new(Some(ts), time_base).as_secs() as f64),
        None => anyhow::bail!("No timestamp found (both best_effort_pts and pts are None)"),
    }
}

fn get_sample_timestamp(
    config: &VideoOptions,
    frame: &ffmpeg_next::frame::Video,
    time_base: Rational,
) -> Result<Option<f64>> {
    match get_frame_timestamp(frame, time_base) {
        Ok(timestamp) => Ok(Some(timestamp)),
        Err(error) if config.strict => Err(error.context("FFmpeg frame timestamp error")),
        Err(error) => {
            tracing::debug!(%error, "Skipping video frame without a usable timestamp");
            Ok(None)
        }
    }
}

fn handle_sample_error(
    config: &VideoOptions,
    target_index: &mut usize,
    target_count: usize,
    error: anyhow::Error,
) -> Result<bool> {
    if config.strict {
        return Err(error);
    }

    tracing::debug!(%error, target_index = *target_index, "Skipping failed video sample");
    *target_index += 1;
    Ok(*target_index == target_count)
}

fn copy_rgb_frame(frame: &ffmpeg_next::frame::Video, output_buffer: &mut [u8]) -> Result<()> {
    let width = frame.width();
    let height = frame.height();
    let row_bytes = width as usize * 3;
    anyhow::ensure!(
        output_buffer.len() == row_bytes * height as usize,
        "Invalid RGB output buffer size"
    );

    let stride = frame.stride(0);
    anyhow::ensure!(stride >= row_bytes, "FFmpeg RGB frame stride is too small");
    let rgb_data = frame.data(0);
    anyhow::ensure!(
        rgb_data.len() >= stride * height as usize,
        "FFmpeg RGB frame data is truncated"
    );
    for row in 0..height as usize {
        let source_offset = row * stride;
        let output_offset = row * row_bytes;
        output_buffer[output_offset..output_offset + row_bytes]
            .copy_from_slice(&rgb_data[source_offset..source_offset + row_bytes]);
    }
    Ok(())
}

fn convert_ffmpeg_frame_to_rgb(
    scaler: &mut ffmpeg_next::software::scaling::Context,
    decoded_frame: &ffmpeg_next::frame::Video,
    rgb_frame: &mut ffmpeg_next::frame::Video,
    output_buffer: &mut [u8],
) -> Result<()> {
    scaler.run(decoded_frame, rgb_frame)?;
    copy_rgb_frame(rgb_frame, output_buffer)
}

fn video_open_error(error: ffmpeg_next::Error) -> anyhow::Error {
    anyhow::anyhow!(
        "failed to open the video for decoding: {error}. If the input uses a \
         codec other than VP8/VP9 (e.g. H.264 or H.265), note this \
         frontend decoder's in-tree FFmpeg decodes only VP8/VP9 -- \
         re-encode to VP9, e.g. `ffmpeg -i input.mp4 -c:v libvpx-vp9 -an \
         output.webm`, or send it to the backend, where H.264/H.265 \
         decode in hardware via NVDEC. Otherwise the input may be \
         malformed or not a video."
    )
}

fn decode_video_impl(config: &VideoOptions, bytes: Vec<u8>) -> Result<DecodedVideo> {
    use ffmpeg_next::codec::context::Context;
    use ffmpeg_next::software::scaling::{Context as ScalingContext, Flags};
    use ffmpeg_next::util::format::pixel::Pixel;

    let mut mem_file = MemFile::create("video", CreateOptions::new().allow_sealing(true))?;
    mem_file.write_all(&bytes)?;
    drop(bytes);
    mem_file.add_seals(Seal::Write | Seal::Shrink | Seal::Grow)?;
    let fd_path = format!("/proc/self/fd/{}", mem_file.as_raw_fd());
    let mut input = ffmpeg_next::format::input(&fd_path).map_err(video_open_error)?;

    let (stream_index, stream_time_base, source_duration, source_fps, total_frames, parameters) = {
        let input_stream = input
            .streams()
            .best(ffmpeg_next::media::Type::Video)
            .ok_or_else(|| anyhow::anyhow!("FFmpeg could not find a video stream"))?;
        let stream_time_base = input_stream.time_base();
        let frame_rate = input_stream.rate();
        anyhow::ensure!(
            frame_rate.denominator() > 0,
            "Cannot determine the video frame rate"
        );
        (
            input_stream.index(),
            stream_time_base,
            Time::new(Some(input_stream.duration()), stream_time_base).as_secs() as f64,
            (frame_rate.numerator() as f32 / frame_rate.denominator() as f32) as f64,
            input_stream.frames().max(0) as u64,
            input_stream.parameters(),
        )
    };

    // Duration and frame count come from file metadata and might be inaccurate.
    let requested_frames =
        get_num_requested_frames(config, source_duration, source_fps, total_frames)?;
    let target_times = get_target_times(requested_frames, source_duration, source_fps)?;

    let mut decoder_context = Context::new();
    decoder_context.set_time_base(stream_time_base);
    decoder_context.set_parameters(parameters)?;
    let mut decoder = decoder_context
        .decoder()
        .video()
        .map_err(video_open_error)?;
    let decoder_time_base = decoder.time_base();
    let (width, height) = (decoder.width(), decoder.height());
    anyhow::ensure!(
        width > 0 && height > 0,
        "Invalid video dimensions {width}x{height}"
    );

    let max_alloc = config.limits.max_alloc.unwrap_or(u64::MAX);
    anyhow::ensure!(
        (width as u64) * (height as u64) * requested_frames * 3 <= max_alloc,
        "Video dimensions {requested_frames}x{width}x{height}x3 exceed max alloc {max_alloc}"
    );

    let frame_size = width as usize * height as usize * 3;
    let mut all_frames = vec![0u8; requested_frames as usize * frame_size];
    let mut sampled_timestamps = Vec::with_capacity(requested_frames as usize);
    let mut target_index = 0usize;
    let mut decoded_frame = ffmpeg_next::frame::Video::empty();
    let mut rgb_frame = ffmpeg_next::frame::Video::empty();
    let mut scaler = ScalingContext::get(
        decoder.format(),
        width,
        height,
        Pixel::RGB24,
        width,
        height,
        Flags::AREA,
    )?;

    let mut receive_frames = |decoder: &mut ffmpeg_next::decoder::Video,
                              target_index: &mut usize,
                              sampled_timestamps: &mut Vec<f64>|
     -> Result<bool> {
        loop {
            match decoder.receive_frame(&mut decoded_frame) {
                Ok(()) => {
                    let timestamp =
                        match get_sample_timestamp(config, &decoded_frame, decoder_time_base)? {
                            Some(timestamp) => timestamp,
                            None => continue,
                        };
                    if timestamp < target_times[*target_index].as_secs() as f64 {
                        continue;
                    }

                    let offset = sampled_timestamps.len() * frame_size;
                    if let Err(error) = convert_ffmpeg_frame_to_rgb(
                        &mut scaler,
                        &decoded_frame,
                        &mut rgb_frame,
                        &mut all_frames[offset..offset + frame_size],
                    ) {
                        let error = anyhow::anyhow!(
                            "FFmpeg RGB conversion error at timestamp {timestamp:.3}s: {error:?}"
                        );
                        if handle_sample_error(config, target_index, target_times.len(), error)? {
                            return Ok(true);
                        }
                        continue;
                    }
                    sampled_timestamps.push(timestamp);
                    *target_index += 1;
                    if *target_index == target_times.len() {
                        return Ok(true);
                    }
                }
                Err(ffmpeg_next::Error::Other {
                    errno: ffmpeg_next::error::EAGAIN,
                })
                | Err(ffmpeg_next::Error::Eof) => return Ok(false),
                Err(error) => {
                    let error = anyhow::anyhow!("FFmpeg frame decode error: {error:?}");
                    if handle_sample_error(config, target_index, target_times.len(), error)? {
                        return Ok(true);
                    }
                    return Ok(false);
                }
            }
        }
    };

    let mut finished = false;
    for (stream, mut packet) in input.packets() {
        if stream.index() != stream_index {
            continue;
        }
        packet.rescale_ts(stream.time_base(), decoder_time_base);
        if let Err(error) = decoder.send_packet(&packet) {
            let error = anyhow::anyhow!("FFmpeg packet decode error: {error:?}");
            if handle_sample_error(config, &mut target_index, target_times.len(), error)? {
                finished = true;
                break;
            }
            continue;
        }
        if receive_frames(&mut decoder, &mut target_index, &mut sampled_timestamps)? {
            finished = true;
            break;
        }
    }

    if !finished {
        match decoder.send_eof() {
            Ok(()) => {
                finished =
                    receive_frames(&mut decoder, &mut target_index, &mut sampled_timestamps)?;
            }
            Err(error) => {
                let error = anyhow::anyhow!("FFmpeg decoder flush error: {error:?}");
                finished =
                    handle_sample_error(config, &mut target_index, target_times.len(), error)?;
            }
        }
    }
    anyhow::ensure!(
        !config.strict || finished,
        "FFmpeg reached end of video after decoding {} of {requested_frames} requested frames",
        sampled_timestamps.len()
    );

    let num_frames_decoded = sampled_timestamps.len();
    anyhow::ensure!(
        num_frames_decoded > 0,
        "Failed to decode any frames, check for video corruption"
    );
    all_frames.truncate(num_frames_decoded * frame_size);

    Ok(DecodedVideo {
        pixels: all_frames,
        width,
        height,
        num_frames: num_frames_decoded,
        metadata: VideoMetadata {
            source_fps,
            source_duration,
            sampled_timestamps,
        },
    })
}

/// Contiguous owned NHWC RGB u8 frames and sampling metadata.
#[derive(Debug)]
pub struct DecodedVideo {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub num_frames: usize,
    pub metadata: VideoMetadata,
}

/// Decode video bytes with Dynamo's sampling and allocation semantics.
/// Requires Linux memfd/procfs and a dynamically linked FFmpeg installation.
pub fn decode_video(bytes: Vec<u8>, options: &VideoOptions) -> Result<DecodedVideo> {
    anyhow::ensure!(
        options.fps.is_none() || options.num_frames.is_none(),
        "fps and num_frames cannot be specified at the same time"
    );
    anyhow::ensure!(
        options.max_frames.is_none() || options.num_frames.is_none(),
        "max_frames and num_frames cannot be specified at the same time"
    );
    decode_video_impl(options, bytes)
}

/// Decode an ordered batch through the crate's explicit execution facilities.
/// The encoded inputs remain owned by the caller; each decode takes a copy.
pub fn decode_videos<T: AsRef<[u8]> + Send + Sync>(
    inputs: &[T],
    options: &VideoOptions,
) -> Result<Vec<DecodedVideo>> {
    crate::execution::try_map(inputs, |bytes| {
        decode_video(bytes.as_ref().to_vec(), options)
    })
}
