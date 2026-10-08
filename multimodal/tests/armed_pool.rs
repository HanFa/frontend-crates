// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Arming the pool is process-global state, so everything about it lives in
//! this one test binary (its counterpart, the unarmed guard, has its own).

#![cfg(all(feature = "parallel", target_os = "linux"))]

use dynamo_multimodal::{MmError, execution};

#[test]
fn init_pool_arms_fanout_and_pins_the_thread_count() {
    execution::init_pool(2).unwrap();

    let mut buf = vec![0usize; 8];
    execution::for_chunks_mut(&mut buf, 2, |i, chunk| chunk.fill(i));
    assert_eq!(buf, [0, 0, 1, 1, 2, 2, 3, 3]);

    #[cfg(feature = "media-decode")]
    {
        use dynamo_multimodal::media::image::{ImageOptions, decode_images};
        let inputs: Vec<_> = (1..8)
            .map(|width| {
                let mut bytes = std::io::Cursor::new(Vec::new());
                image::DynamicImage::new_rgba8(width, 2)
                    .write_to(&mut bytes, image::ImageFormat::Png)
                    .unwrap();
                bytes.into_inner()
            })
            .collect();
        let decoded = decode_images(&inputs, &ImageOptions::default()).unwrap();
        for (index, image) in decoded.iter().enumerate() {
            assert_eq!(image.width as usize, index + 1);
            assert_eq!(image.pixels.len(), (index + 1) * 2 * 4);
        }
    }

    let named: Vec<String> = std::fs::read_dir("/proc/self/task")
        .expect("procfs")
        .filter_map(|entry| {
            let comm = entry.ok()?.path().join("comm");
            std::fs::read_to_string(comm).ok()
        })
        .map(|name| name.trim().to_string())
        .filter(|name| name.starts_with("dyn-mm"))
        .collect();
    assert_eq!(
        named.len(),
        2,
        "expected the armed pool's threads: {named:?}"
    );

    execution::init_pool(2).unwrap();
    assert!(matches!(
        execution::init_pool(3),
        Err(MmError::InvalidInput { .. })
    ));
}
