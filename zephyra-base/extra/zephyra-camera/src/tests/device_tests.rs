// 本文件由 device.rs 的 #[cfg(test)] mod tests 抽出 —— 测试代码与生产代码分离。
// 模块路径不变（src 里的声明用 #[path] 指过来），所以 use super::* 与私有可见性都照旧。

use super::*;

/// 构造能力表：(分辨率, 格式, 帧率) 列表。
fn formats(entries: &[(u32, u32, PixelFormat, u32)]) -> Vec<FormatInfo> {
    entries
        .iter()
        .map(|&(w, h, format, fps)| FormatInfo::new(Resolution::new(w, h), format, fps))
        .collect()
}

#[test]
fn resolution_geometry_and_display() {
    let hd = Resolution::new(1280, 720);
    assert_eq!(hd.to_string(), "1280x720");
    assert_eq!(hd.pixels(), 921_600);
    assert!(hd.is_even());
    assert!(hd.is_wide());
    assert!((hd.aspect_ratio() - 16.0 / 9.0).abs() < 0.01);

    let odd = Resolution::new(641, 481);
    assert!(!odd.is_even());
    assert!(!odd.is_wide());
    assert_eq!(Resolution::new(1, 0).aspect_ratio(), f32::INFINITY);
}

#[test]
fn pixel_format_labels_are_stable() {
    assert_eq!(PixelFormat::Nv12.to_string(), "NV12");
    assert_eq!(PixelFormat::Mjpeg.to_string(), "MJPEG");
    assert_eq!(PixelFormat::RawRgb.to_string(), "RGB24");
    assert!(PixelFormat::Nv12.is_planar_yuv());
    assert!(!PixelFormat::Mjpeg.is_planar_yuv());
    assert_eq!(
        FormatInfo::new(Resolution::new(640, 480), PixelFormat::Mjpeg, 30).to_string(),
        "640x480@30 MJPEG"
    );
}

#[test]
fn select_format_prefers_exact_resolution_nv12() {
    let list = formats(&[
        (1920, 1080, PixelFormat::Mjpeg, 30),
        (1280, 720, PixelFormat::Nv12, 30),
        (1280, 720, PixelFormat::Mjpeg, 30),
        (640, 480, PixelFormat::Nv12, 60),
    ]);
    let preference = FormatPreference::new(Resolution::new(1280, 720));
    let chosen = select_format(&list, &preference).expect("format");
    assert_eq!(chosen.resolution, Resolution::new(1280, 720));
    assert!(chosen.is_nv12());
    assert_eq!(chosen.fps, 30);
}

#[test]
fn select_format_falls_back_to_mjpeg_when_nv12_is_slower() {
    // NV12 只有 15fps，MJPEG 有 30fps：应保持原设备的协商启发式，回退 MJPEG。
    let list = formats(&[
        (1280, 720, PixelFormat::Nv12, 15),
        (1280, 720, PixelFormat::Mjpeg, 30),
    ]);
    let chosen =
        select_format(&list, &FormatPreference::new(Resolution::new(1280, 720))).expect("format");
    assert_eq!(chosen.format, PixelFormat::Mjpeg);
    assert_eq!(chosen.fps, 30);
}

#[test]
fn select_format_honors_min_fps_and_nv12_toggle() {
    let list = formats(&[
        (640, 480, PixelFormat::Nv12, 15),
        (640, 480, PixelFormat::Mjpeg, 30),
        (640, 480, PixelFormat::Yuyv, 24),
    ]);
    // 最低 30fps 淘汰 15fps 的 NV12 → 同为 30fps 时 MJPEG 优先级高于 YUYV。
    let preference = FormatPreference::new(Resolution::new(640, 480)).with_min_fps(30);
    let chosen = select_format(&list, &preference).expect("format");
    assert_eq!(chosen.format, PixelFormat::Mjpeg);
    assert_eq!(chosen.fps, 30);

    // 帧率门槛高于所有格式 → 无结果。
    assert!(select_format(&list, &FormatPreference::default().with_min_fps(120)).is_none());

    // 显式关闭 NV12 偏好后，即使 NV12 帧率最高也不选它。
    let fast_nv12 = formats(&[
        (640, 480, PixelFormat::Nv12, 60),
        (640, 480, PixelFormat::Mjpeg, 30),
        (640, 480, PixelFormat::Yuyv, 24),
    ]);
    let with_nv12 = select_format(
        &fast_nv12,
        &FormatPreference::new(Resolution::new(640, 480)),
    )
    .expect("format");
    assert_eq!(with_nv12.format, PixelFormat::Nv12);
    let without = select_format(
        &fast_nv12,
        &FormatPreference::new(Resolution::new(640, 480)).without_nv12(),
    )
    .expect("format");
    assert_eq!(without.format, PixelFormat::Mjpeg);
    assert_eq!(without.fps, 30);
}

#[test]
fn select_format_falls_back_to_largest_resolution() {
    let list = formats(&[
        (320, 240, PixelFormat::Mjpeg, 30),
        (1920, 1080, PixelFormat::Mjpeg, 15),
        (1280, 720, PixelFormat::Nv12, 24),
    ]);
    // 指定分辨率设备不支持：回退到像素数最大的 1920x1080（而非列表顺序）。
    let chosen =
        select_format(&list, &FormatPreference::new(Resolution::new(800, 600))).expect("format");
    assert_eq!(chosen.resolution, Resolution::new(1920, 1080));
    assert_eq!(chosen.fps, 15);
}

#[test]
fn strict_format_rejects_mjpeg_and_explains_selection() {
    let list = formats(&[
        (1280, 720, PixelFormat::Nv12, 30),
        (1280, 720, PixelFormat::Mjpeg, 60),
        (640, 480, PixelFormat::Nv12, 30),
    ]);
    let preference = FormatPreference::new(Resolution::new(1280, 720)).strict_nv12();
    let report = select_format_detailed(&list, &preference).expect("report");
    assert_eq!(report.chosen.format, PixelFormat::Nv12);
    assert_eq!(report.chosen.fps, 30);
    assert_eq!(report.reason, "exact-nv12");
    assert!(report
        .rejected
        .iter()
        .any(|(format, reason)| *reason == RejectReason::FormatMismatch
            && format.format == PixelFormat::Mjpeg));
    assert!(report.rejected.iter().any(|(format, reason)| *reason
        == RejectReason::ResolutionMismatch
        && format.resolution == Resolution::new(640, 480)));

    // 设备完全没有 NV12 → 严格模式下打开失败，而不是悄悄走 MJPEG。
    let mjpeg_only = formats(&[(1280, 720, PixelFormat::Mjpeg, 30)]);
    assert!(select_format(&mjpeg_only, &preference).is_none());
}

#[test]
fn select_exact_format_is_strict() {
    let list = formats(&[(1280, 720, PixelFormat::Nv12, 30)]);
    assert!(select_exact_format(&list, Resolution::new(1280, 720), 30).is_some());
    assert!(select_exact_format(&list, Resolution::new(1280, 720), 60).is_none());
    assert!(select_exact_format(&list, Resolution::new(640, 480), 0).is_none());
}

#[test]
fn capabilities_group_and_sort_resolutions() {
    let caps = DeviceCapabilities::new(formats(&[
        (640, 480, PixelFormat::Mjpeg, 30),
        (1920, 1080, PixelFormat::Mjpeg, 30),
        (1280, 720, PixelFormat::Nv12, 30),
        (1280, 720, PixelFormat::Mjpeg, 30),
        (640, 480, PixelFormat::Mjpeg, 30), // 重复项
    ]));
    assert_eq!(caps.len(), 4);
    assert_eq!(
        caps.resolutions(),
        vec![
            Resolution::new(1920, 1080),
            Resolution::new(1280, 720),
            Resolution::new(640, 480),
        ]
    );
    assert_eq!(caps.max_resolution(), Some(Resolution::new(1920, 1080)));
    assert_eq!(caps.formats_for(Resolution::new(1280, 720)).len(), 2);
    assert!(caps.supports_nv12(Resolution::new(1280, 720)));
    assert!(!caps.supports_nv12(Resolution::new(640, 480)));
    assert_eq!(caps.nv12_formats().len(), 1);
    assert!(DeviceCapabilities::default().is_empty());
}

#[test]
fn device_info_matching_by_name_and_id() {
    let device = DeviceInfo::new(
        DeviceIndex::Index(2),
        "USB Camera",
        "Integrated webcam",
        r"\\?\USB#VID_1234&PID_5678#6&abc#{e5323777}",
        Backend::MediaFoundation,
    );
    assert_eq!(device.display_name(), "USB Camera");
    assert_eq!(device.to_string(), "USB Camera [2] (Media Foundation)");
    assert!(device.matches_name("camera"));
    assert!(device.matches_name("WEBCAM"));
    assert!(!device.matches_name(""));
    assert!(device.matches_id(r"\\?\USB#VID_1234"));
    assert!(device.matches("usb camera"));

    let unnamed = DeviceInfo::new(
        DeviceIndex::String("rtsp://localhost/stream".into()),
        "  ",
        "",
        "rtsp://localhost/stream",
        Backend::Network,
    );
    assert_eq!(unnamed.display_name(), "Camera #rtsp://localhost/stream");
    assert!(unnamed.matches_id("rtsp://localhost"));
    assert!(!unnamed.matches_id(""));
}

#[test]
fn device_index_variants() {
    let numeric = DeviceIndex::from(7u32);
    assert_eq!(numeric.as_u32(), Some(7));
    assert!(numeric.is_numeric());
    assert_eq!(numeric.to_string(), "7");

    let textual = DeviceIndex::from("rtsp://cam");
    assert_eq!(textual.as_u32(), None);
    assert!(!textual.is_numeric());
    assert_eq!(textual.as_string(), "rtsp://cam");
}

#[test]
fn resolution_ordering_and_large_pixels_do_not_overflow() {
    // Resolution 派生 Ord 按 (width, height) 字典序；pixels 用 u64 避免 4K+ 溢出。
    let small = Resolution::new(640, 480);
    let big = Resolution::new(3840, 2160);
    assert!(small < big);
    assert_eq!(big.pixels(), 8_294_400);
    let huge = Resolution::new(u32::MAX, u32::MAX);
    assert_eq!(huge.pixels(), u32::MAX as u64 * u32::MAX as u64);
    assert_eq!(Resolution::default(), Resolution::new(0, 0));
    assert_eq!(Resolution::new(0, 0).aspect_ratio(), f32::INFINITY);
    assert!(Resolution::new(0, 0).is_even()); // 0 是偶数
}

#[test]
fn selection_prefers_mjpeg_over_yuyv_at_equal_fps() {
    let list = formats(&[
        (640, 480, PixelFormat::Yuyv, 30),
        (640, 480, PixelFormat::Mjpeg, 30),
        (640, 480, PixelFormat::Gray, 30),
    ]);
    // 格式优先级 MJPEG > YUYV > GRAY（无 NV12 时）。
    let chosen =
        select_format(&list, &FormatPreference::new(Resolution::new(640, 480))).expect("format");
    assert_eq!(chosen.format, PixelFormat::Mjpeg);
}

#[test]
fn min_fps_filter_can_force_higher_resolution() {
    // 720p 只有 15fps，1080p 有 30fps：min_fps=30 时只能选 1080p。
    let list = formats(&[
        (1280, 720, PixelFormat::Nv12, 15),
        (1920, 1080, PixelFormat::Mjpeg, 30),
    ]);
    let preference = FormatPreference::new(Resolution::new(1280, 720)).with_min_fps(30);
    let report = select_format_detailed(&list, &preference).expect("report");
    assert_eq!(report.chosen.resolution, Resolution::new(1920, 1080));
    assert_eq!(report.chosen.format, PixelFormat::Mjpeg);
    assert!(report
        .rejected
        .iter()
        .any(|(_, reason)| *reason == RejectReason::BelowMinFps));
}

#[test]
fn selection_with_empty_formats_returns_none() {
    let empty: Vec<FormatInfo> = Vec::new();
    assert!(select_format(&empty, &FormatPreference::default()).is_none());
    assert!(select_format_detailed(&empty, &FormatPreference::default()).is_none());
    assert!(select_exact_format(&empty, Resolution::new(640, 480), 0).is_none());
}

#[test]
fn capabilities_dedup_is_order_independent() {
    let entries = [
        (640, 480, PixelFormat::Mjpeg, 30),
        (1920, 1080, PixelFormat::Mjpeg, 30),
        (1280, 720, PixelFormat::Nv12, 30),
    ];
    let a = DeviceCapabilities::new(formats(&entries));
    let reversed: Vec<_> = entries.iter().rev().copied().collect();
    let b = DeviceCapabilities::new(formats(&reversed));
    assert_eq!(a, b);
    // 同一分辨率同一格式不同帧率：保留两条（dedup 只去掉完全相同的项）。
    let with_fps = DeviceCapabilities::new(formats(&[
        (640, 480, PixelFormat::Mjpeg, 30),
        (640, 480, PixelFormat::Mjpeg, 15),
    ]));
    assert_eq!(with_fps.len(), 2);
    assert_eq!(with_fps.formats()[0].fps, 30);
}

#[test]
fn capabilities_expose_full_resolution_fps_matrix() {
    let caps = DeviceCapabilities::new(formats(&[
        (640, 480, PixelFormat::Nv12, 30),
        (640, 480, PixelFormat::Nv12, 60),
        (640, 480, PixelFormat::Mjpeg, 30),
        (1280, 720, PixelFormat::Nv12, 30),
        (1280, 720, PixelFormat::Mjpeg, 60),
    ]));
    assert_eq!(caps.frame_rates(), vec![60, 30]);
    assert_eq!(caps.fps_for(Resolution::new(640, 480)), vec![60, 30]);
    assert_eq!(caps.fps_for(Resolution::new(1280, 720)), vec![60, 30]);
    assert_eq!(caps.fps_range(), Some((30, 60)));
    assert_eq!(
        caps.resolutions_for_fps(60),
        vec![Resolution::new(1280, 720), Resolution::new(640, 480)]
    );
    assert!(caps.supports(Resolution::new(640, 480), 60));
    assert!(!caps.supports(Resolution::new(640, 480), 120));

    let matrix = caps.matrix();
    assert_eq!(matrix.len(), 2);
    assert_eq!(matrix[0].resolution, Resolution::new(1280, 720));
    assert_eq!(matrix[0].fps, vec![60, 30]);
    assert_eq!(matrix[1].resolution, Resolution::new(640, 480));
    // 同分辨率同帧率有 NV12 与 MJPEG 两种：best_format_for_fps 应优先 NV12。
    let best = matrix[1].best_format_for_fps(30).expect("best");
    assert!(best.is_nv12());
    assert_eq!(
        caps.best_format_for(Resolution::new(640, 480), 60)
            .unwrap()
            .fps,
        60
    );
    assert!(caps
        .best_format_for(Resolution::new(640, 480), 120)
        .is_none());
}

#[test]
fn nearest_resolution_picks_closest_pixel_count() {
    let caps = DeviceCapabilities::new(formats(&[
        (640, 480, PixelFormat::Nv12, 30),
        (1280, 720, PixelFormat::Nv12, 30),
        (1920, 1080, PixelFormat::Nv12, 30),
    ]));
    assert_eq!(
        caps.nearest_resolution(Resolution::new(1300, 700)),
        Some(Resolution::new(1280, 720))
    );
    assert_eq!(
        caps.nearest_resolution(Resolution::new(100, 100)),
        Some(Resolution::new(640, 480))
    );
    assert!(DeviceCapabilities::default()
        .nearest_resolution(Resolution::new(1, 1))
        .is_none());
}

#[test]
fn select_from_candidates_tries_in_order_and_falls_back() {
    let list = formats(&[
        (640, 480, PixelFormat::Nv12, 30),
        (1280, 720, PixelFormat::Nv12, 30),
        (1920, 1080, PixelFormat::Mjpeg, 30),
    ]);
    let candidates = [
        FormatCandidate::new(Resolution::new(1920, 1080), Some(30)).strict_nv12(),
        FormatCandidate::new(Resolution::new(1280, 720), Some(30)).strict_nv12(),
        FormatCandidate::any(),
    ];
    let selection = select_from_candidates(&list, &candidates).expect("selection");
    // 1080p 只有 MJPEG，被 strict_nv12 跳过；720p NV12 命中。
    assert_eq!(selection.index, 1);
    assert_eq!(selection.chosen.resolution, Resolution::new(1280, 720));
    assert!(selection.chosen.is_nv12());

    // 严格 NV12 且没有任何候选命中时返回 None。
    let strict_only = [FormatCandidate::new(Resolution::new(1920, 1080), Some(30)).strict_nv12()];
    assert!(select_from_candidates(&list, &strict_only).is_none());
}

#[test]
fn select_from_candidates_honors_exact_fps() {
    let list = formats(&[
        (1280, 720, PixelFormat::Nv12, 30),
        (1280, 720, PixelFormat::Nv12, 60),
    ]);
    let candidates = [FormatCandidate::new(Resolution::new(1280, 720), Some(60))];
    let selection = select_from_candidates(&list, &candidates).expect("selection");
    assert_eq!(selection.chosen.fps, 60);
    assert_eq!(selection.chosen.format, PixelFormat::Nv12);
}

#[test]
fn select_from_preferences_tries_in_order() {
    let list = formats(&[
        (640, 480, PixelFormat::Nv12, 30),
        (1280, 720, PixelFormat::Nv12, 30),
    ]);
    let preferences = [
        FormatPreference::new(Resolution::new(1920, 1080)),
        FormatPreference::new(Resolution::new(1280, 720)),
    ];
    let selection = select_from_preferences(&list, &preferences).expect("selection");
    assert_eq!(selection.index, 1);
    assert_eq!(selection.chosen.resolution, Resolution::new(1280, 720));
}

#[test]
fn adaptive_selection_scales_with_utilization() {
    let list = formats(&[
        (640, 480, PixelFormat::Nv12, 15),
        (640, 480, PixelFormat::Nv12, 30),
        (1280, 720, PixelFormat::Nv12, 30),
        (1920, 1080, PixelFormat::Nv12, 30),
    ]);
    let policy = AdaptivePolicy {
        min_fps: 15,
        max_fps: 30,
        min_pixels: 0,
        max_pixels: 0,
        scale_up_below: 0.5,
        scale_down_above: 1.0,
        cooldown_frames: 10,
        exploration_frames: 5,
        scale_up_proving_frames: 10,
        prefer_nv12: true,
    };
    let idle = select_adaptive_format(&list, &policy, 0.1).expect("idle");
    let balanced = select_adaptive_format(&list, &policy, 0.75).expect("balanced");
    let overloaded = select_adaptive_format(&list, &policy, 1.5).expect("overloaded");
    assert!(idle.resolution.pixels() >= balanced.resolution.pixels());
    assert!(balanced.resolution.pixels() >= overloaded.resolution.pixels());
    assert_eq!(overloaded.resolution, Resolution::new(640, 480));
    assert_eq!(overloaded.fps, 15);

    // 约束生效：min_fps=30 淘汰 15fps 档位。
    let constrained = AdaptivePolicy {
        min_fps: 30,
        ..policy
    };
    let chosen = select_adaptive_format(&list, &constrained, 1.5).expect("constrained");
    assert_eq!(chosen.fps, 30);
}

#[test]
fn device_index_default_and_equality() {
    assert_eq!(DeviceIndex::default(), DeviceIndex::Index(0));
    assert_eq!(DeviceIndex::from(3u32), DeviceIndex::Index(3));
    assert_ne!(DeviceIndex::from(3u32), DeviceIndex::String("3".into()));
    assert_eq!(Backend::default(), Backend::Auto);
    assert_eq!(Backend::MediaFoundation.to_string(), "Media Foundation");
    assert_eq!(Backend::UsbVideoClass.label(), "USB Video Class");
}

#[test]
fn camera_error_display_is_actionable() {
    assert_eq!(
        CameraError::DeviceNotFound("cam".into()).to_string(),
        "camera not found: cam"
    );
    assert_eq!(CameraError::NotOpen.to_string(), "camera is not open");
    assert_eq!(
        CameraError::InvalidResolution(Resolution::new(641, 480)).to_string(),
        "invalid camera resolution: 641x480"
    );
    assert_eq!(
        CameraError::UnsupportedFormat(PixelFormat::Gray).to_string(),
        "unsupported camera pixel format: GRAY"
    );
    let error: Box<dyn std::error::Error> = Box::new(CameraError::Open("busy".into()));
    assert!(error.to_string().contains("busy"));
}

#[test]
fn device_changes_report_adds_and_removes() {
    let old = DeviceInfo::new(DeviceIndex::Index(0), "A", "", "a", Backend::Auto);
    let new = DeviceInfo::new(DeviceIndex::Index(1), "B", "", "b", Backend::Auto);
    let changes = DeviceChanges {
        added: vec![new],
        removed: vec![old],
    };
    assert!(!changes.is_empty());
    assert_eq!(changes.len(), 2);
    assert!(DeviceChanges::default().is_empty());
}
