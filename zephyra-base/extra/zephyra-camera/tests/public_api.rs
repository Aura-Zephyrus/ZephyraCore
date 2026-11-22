//! 集成测试：只通过 crate 根的重导出访问 API，模拟 crate 外部的使用者。
//!
//! 默认 7 条；`serde` feature 下额外 3 条真实 JSON 往返测试。

use std::time::Duration;

use zephyra_camera::{
    action_for_error, classify_error, select_exact_format, select_format, select_format_detailed,
    select_from_candidates, Backend, CandidateSelection, ControlRange, ControlValue, DeviceIndex,
    DeviceInfo, DropLog, DropReason, EwmaFrameRate, FailureKind, FormatCandidate, FormatInfo,
    FormatPreference, FrameRateEstimator, Nv12Planes, PacingController, PacingDecision,
    PixelFormat, QualitySettings, RecoveryAction, RecoveryController, RecoveryPolicy, RejectReason,
    Resolution,
};
// 仅在 serde 往返测试里用到；默认 feature 下不能无条件导入（clippy -D warnings）。
#[cfg(feature = "serde")]
use zephyra_camera::PowerLineFrequency;

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

#[test]
fn public_api_nv12_planes_split_is_zero_copy() {
    let (w, h) = (4u32, 4u32);
    let data: Vec<u8> = (0..(w * h) as u8 * 3 / 2).collect();
    let base = data.as_ptr();
    let planes = Nv12Planes::split(bytes::Bytes::from(data), w, h).unwrap();
    assert_eq!((planes.y.len(), planes.uv.len()), (16, 8));
    assert_eq!(planes.y[15], 15);
    assert_eq!(planes.uv[0], 16);
    // 两个平面都是同一底层分配的零拷贝 slice，UV 紧接在 Y 之后。
    assert_eq!(planes.y.as_ptr(), base);
    assert_eq!(planes.uv.as_ptr(), base.wrapping_add(16));
    // 奇数尺寸无法定义 4:2:0 的 UV 平面。
    assert!(Nv12Planes::split(bytes::Bytes::from(vec![0u8; 24]), 3, 3).is_none());
}

#[test]
fn public_api_format_selection_prefers_strict_nv12() {
    let formats = [
        FormatInfo::new(Resolution::new(1280, 720), PixelFormat::Mjpeg, 30),
        FormatInfo::new(Resolution::new(1280, 720), PixelFormat::Nv12, 30),
        FormatInfo::new(Resolution::new(640, 480), PixelFormat::Nv12, 60),
    ];
    let preference = FormatPreference::new(Resolution::new(1280, 720)).strict_nv12();
    let chosen = select_format(&formats, &preference).unwrap();
    assert_eq!(chosen.format, PixelFormat::Nv12);
    assert_eq!(chosen.resolution, Resolution::new(1280, 720));

    let report = select_format_detailed(&formats, &preference).unwrap();
    assert_eq!(report.reason, "exact-nv12");
    assert!(report
        .rejected
        .iter()
        .any(|(_, why)| *why == RejectReason::FormatMismatch));

    // 严格精确分辨率：设备不支持就返回 None，而不是回退到别的分辨率。
    assert!(select_exact_format(&formats, Resolution::new(1920, 1080), 0).is_none());
}

#[test]
fn public_api_candidates_fall_back_to_supported_resolution() {
    let formats = [
        FormatInfo::new(Resolution::new(1280, 720), PixelFormat::Nv12, 30),
        FormatInfo::new(Resolution::new(640, 480), PixelFormat::Nv12, 60),
    ];
    let candidates = [
        FormatCandidate::new(Resolution::new(1920, 1080), Some(30)).strict_nv12(),
        FormatCandidate::new(Resolution::new(1280, 720), Some(30)),
        FormatCandidate::any(),
    ];
    let selection: CandidateSelection = select_from_candidates(&formats, &candidates).unwrap();
    assert_eq!(selection.index, 1);
    assert_eq!(selection.chosen.resolution, Resolution::new(1280, 720));
}

#[test]
fn public_api_cpu_frame_processor_converts_rgb_to_rgba() {
    let rgb = [255, 0, 0, 0, 255, 0, 0, 0, 255, 128, 128, 128];
    let mut processor = zephyra_camera::CpuFrameProcessor::new();
    let owned = processor.process(&rgb, 2, 2);
    assert_eq!(owned.len(), 16);
    assert_eq!(&owned[..4], &[255, 0, 0, 255]);
    assert_eq!(&owned[12..], &[128, 128, 128, 255]);
    assert_eq!((processor.last_width(), processor.last_height()), (2, 2));

    // 借出版复用同一缓冲，预热后结果与 owned 完全一致。
    assert_eq!(processor.process_borrowed(&rgb, 2, 2), owned.as_slice());
}

#[test]
fn public_api_quality_diagnostics_estimate_and_report_drops() {
    let mut estimator = FrameRateEstimator::new();
    assert_eq!(estimator.add_sample(ms(33)), None);
    assert_eq!(estimator.add_sample(ms(33)), None);
    assert_eq!(estimator.add_sample(ms(33)), Some(30));
    assert_eq!(estimator.fps(), Some(30));

    let mut ewma = EwmaFrameRate::new();
    let mut timestamp = Duration::ZERO;
    for _ in 0..100 {
        timestamp += ms(33);
        let _ = ewma.update(timestamp);
    }
    assert!((20.0..40.0).contains(&ewma.raw_fps()));
    assert!(ewma.published_fps() > 0.0);

    let mut drops = DropLog::new();
    drops.note(ms(10), DropReason::InvalidFormat);
    drops.note(ms(20), DropReason::RateTooHigh);
    assert_eq!(drops.total(), 2);
    assert_eq!(drops.count(DropReason::InvalidFormat), 1);

    let mut pacing = PacingController::new(ms(33));
    assert_eq!(
        pacing.should_deliver(Duration::ZERO),
        PacingDecision::Deliver
    );
    assert_eq!(
        pacing.should_deliver(ms(10)),
        PacingDecision::Drop(DropReason::RateTooHigh)
    );
}

#[test]
fn public_api_recovery_classifies_and_bounds_retries() {
    assert_eq!(
        classify_error("MF_E_HW_MFT_FAILED_START_STREAMING: camera in use"),
        FailureKind::CameraBusy
    );
    assert_eq!(
        classify_error("DXGI_ERROR_DEVICE_REMOVED"),
        FailureKind::D3dDeviceRemoved
    );

    let policy = RecoveryPolicy {
        transient_retries: 0,
        restart_backoff: Duration::ZERO,
        ..RecoveryPolicy::default()
    };
    let mut controller = RecoveryController::new(policy);
    assert_eq!(
        controller.on_failure(FailureKind::Transient),
        RecoveryAction::GiveUp
    );
    assert_eq!(controller.last_kind(), Some(FailureKind::Transient));

    assert_eq!(
        action_for_error(
            &mut RecoveryController::new(policy),
            &zephyra_camera::CameraError::DeviceNotFound("usb#vid".into())
        ),
        RecoveryAction::RestartStream
    );
}

#[test]
fn public_api_device_info_and_quality_settings() {
    let device = DeviceInfo::new(
        DeviceIndex::Index(2),
        "Integrated Camera",
        "USB Camera",
        r"\\?\USB#VID_1234",
        Backend::MediaFoundation,
    );
    assert_eq!(device.display_name(), "Integrated Camera");
    assert_eq!(device.index().as_u32(), Some(2));
    assert_eq!(device.backend(), Backend::MediaFoundation);

    let settings = QualitySettings::auto_3a();
    assert_eq!(settings.entries().len(), 3);

    let range = ControlRange::Integer {
        value: 50,
        min: Some(0),
        max: Some(100),
        step: Some(1),
        default: Some(50),
    };
    assert_eq!(
        range.clamp(ControlValue::Integer(200)),
        ControlValue::Integer(100)
    );
    assert_eq!(range.value(), ControlValue::Integer(50));
}

#[cfg(feature = "serde")]
#[test]
fn serde_roundtrip_device_info_and_format_info() {
    let device = DeviceInfo::new(
        DeviceIndex::String("rtsp://cam".into()),
        "IP Camera",
        "Network",
        "rtsp://cam",
        Backend::Network,
    );
    let json = serde_json::to_string(&device).unwrap();
    let back: DeviceInfo = serde_json::from_str(&json).unwrap();
    assert_eq!(device, back);

    let format = FormatInfo::new(Resolution::new(1280, 720), PixelFormat::Nv12, 30);
    let json = serde_json::to_string(&format).unwrap();
    let back: FormatInfo = serde_json::from_str(&json).unwrap();
    assert_eq!(format, back);
}

#[cfg(feature = "serde")]
#[test]
fn serde_roundtrip_quality_settings() {
    let settings = QualitySettings {
        brightness: Some(120),
        anti_flicker: Some(PowerLineFrequency::Hz50),
        ..QualitySettings::default()
    };
    let json = serde_json::to_string(&settings).unwrap();
    let back: QualitySettings = serde_json::from_str(&json).unwrap();
    assert_eq!(settings, back);
}

#[cfg(feature = "serde")]
#[test]
fn serde_roundtrip_selection_report() {
    let formats = [
        FormatInfo::new(Resolution::new(1280, 720), PixelFormat::Mjpeg, 30),
        FormatInfo::new(Resolution::new(1280, 720), PixelFormat::Nv12, 30),
    ];
    let report = select_format_detailed(
        &formats,
        &FormatPreference::new(Resolution::new(1280, 720)).strict_nv12(),
    )
    .unwrap();
    let json = serde_json::to_string(&report).unwrap();
    let back = serde_json::from_str::<zephyra_camera::SelectionReport>(&json).unwrap();
    assert_eq!(report, back);
}
