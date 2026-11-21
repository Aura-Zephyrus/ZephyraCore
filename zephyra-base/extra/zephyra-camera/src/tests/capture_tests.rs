// 本文件由 capture.rs 的 #[cfg(test)] mod tests 抽出 —— 测试代码与生产代码分离。
// 仅 capture feature 启用时编译；构造 nokhwa 类型验证转换为自有类型时无损。

use super::*;

#[test]
fn camera_info_converts_to_owned_device_info() {
    let info = CameraInfo::new(
        "HD USB Camera",
        "Integrated webcam",
        r"\\?\USB#VID_1234&PID_5678#6&abc#{e5323777}",
        CameraIndex::Index(3),
    );
    let device = device_info_from_camera_info(&info);
    assert_eq!(device.index(), &DeviceIndex::Index(3));
    assert_eq!(device.name(), "HD USB Camera");
    assert_eq!(device.description(), "Integrated webcam");
    assert_eq!(
        device.unique_id(),
        r"\\?\USB#VID_1234&PID_5678#6&abc#{e5323777}"
    );
    assert_eq!(device.backend(), Backend::Auto);
    assert!(!device.matches_id("HD USB Camera"));
    assert!(device.matches_id(r"\\?\USB#VID_1234"));
}

#[test]
fn camera_info_with_string_index_converts() {
    let info = CameraInfo::new(
        "IP Cam",
        "",
        "rtsp://cam",
        CameraIndex::String("rtsp://cam".into()),
    );
    let device = device_info_from_camera_info(&info);
    assert_eq!(
        device.index(),
        &DeviceIndex::String("rtsp://cam".to_owned())
    );
    assert!(device.matches_id("rtsp://cam"));
}

#[test]
fn camera_format_converts_to_owned_format_info() {
    let format = CameraFormat::new(NokhwaResolution::new(1280, 720), FrameFormat::NV12, 30);
    let info = format_info_from_camera_format(&format);
    assert_eq!(info.resolution, Resolution::new(1280, 720));
    assert_eq!(info.format, PixelFormat::Nv12);
    assert_eq!(info.fps, 30);
    assert_eq!(info.to_string(), "1280x720@30 NV12");
}

#[test]
fn backends_roundtrip_through_api_backend() {
    for backend in [
        Backend::Auto,
        Backend::AvFoundation,
        Backend::Video4Linux,
        Backend::UsbVideoClass,
        Backend::MediaFoundation,
        Backend::OpenCv,
        Backend::GStreamer,
        Backend::Network,
        Backend::Browser,
    ] {
        assert_eq!(backend_from_api(api_from_backend(backend)), backend);
    }
}

#[test]
fn camera_control_converts_to_owned_capabilities() {
    let control = nokhwa::utils::CameraControl::new(
        KnownCameraControl::Brightness,
        "Brightness".to_owned(),
        ControlValueDescription::IntegerRange {
            min: 0,
            max: 255,
            value: 128,
            step: 1,
            default: 128,
        },
        vec![
            KnownCameraControlFlag::Manual,
            KnownCameraControlFlag::Continuous,
        ],
        true,
    );
    let caps = caps_from_camera_control(&control);
    assert_eq!(caps.id, CameraControlId::Brightness);
    assert_eq!(caps.name, "Brightness");
    assert!(caps.is_writable());
    assert!(caps.flags.continuous);
    assert!(caps.active);
    assert_eq!(
        caps.range.clamp(ControlValue::Integer(300)),
        ControlValue::Integer(255)
    );
}

#[test]
fn control_ids_roundtrip_except_unsupported_ones() {
    for id in CameraControlId::ALL {
        assert_eq!(
            control_id_from_known(known_from_control_id(id).expect("known")),
            id
        );
    }
    assert!(known_from_control_id(CameraControlId::AntiFlicker).is_none());
    assert!(known_from_control_id(CameraControlId::Other(1)).is_none());
}

#[test]
fn camera_manager_default_state_is_closed() {
    let manager = CameraManager::new();
    assert!(!manager.is_open());
    assert!(!manager.is_native_nv12());
    assert_eq!(manager.negotiated_fps(), 0);
    assert!(manager.observed_period().is_none());
    assert!(manager.current_device().is_none());
    assert!(manager.current_format().is_none());
    assert!(manager.devices().is_empty());
    assert!(!manager.devices_changed());
    assert!(manager.last_changes().is_empty());
    assert_eq!(manager.reliable_fps(), None);
    assert_eq!(manager.smoothed_fps(), 0.0);
    assert_eq!(manager.total_drops(), 0);
    assert_eq!(manager.drop_count(DropReason::DeviceLost), 0);
    assert!(manager.stream_health().is_healthy());
    assert_eq!(manager.frame_stats().sample_count, 0);
    assert_eq!(CameraManager::default().negotiated_fps(), 0);
}

#[test]
fn closed_camera_operations_return_not_open() {
    let mut manager = CameraManager::new();
    assert_eq!(
        manager.set_resolution(Resolution::new(640, 480)),
        Err(CameraError::NotOpen)
    );
    assert!(manager.supported_controls().is_err());
    assert!(manager.control(CameraControlId::Brightness).is_err());
    assert!(manager
        .set_control(CameraControlId::Brightness, ControlValue::Integer(50))
        .is_err());
    assert!(manager.set_auto(CameraControlId::Focus, true).is_err());
    assert!(manager.restart_stream().is_err());
}

#[test]
fn pacing_period_uses_requested_until_enough_samples() {
    let manager = CameraManager::new();
    let requested = Duration::from_millis(33);
    assert_eq!(manager.pacing_period(requested), requested);
}

#[test]
fn apply_settings_without_camera_reports_failures_not_panic() {
    let mut manager = CameraManager::new();
    let report = manager.apply_settings(&QualitySettings::auto_3a());
    // 未打开摄像头时每项都失败，但不 panic，且失败项被完整记录。
    assert_eq!(report.applied_count(), 0);
    assert_eq!(report.failed.len(), 3);
    assert!(report.unsupported.is_empty());
}

#[test]
fn device_index_roundtrip_to_camera_index() {
    let numeric = DeviceIndex::Index(5);
    assert_eq!(
        camera_index_from_device_index(&numeric),
        CameraIndex::Index(5)
    );
    let textual = DeviceIndex::String("rtsp://cam".into());
    assert_eq!(
        camera_index_from_device_index(&textual),
        CameraIndex::String("rtsp://cam".into())
    );
}
