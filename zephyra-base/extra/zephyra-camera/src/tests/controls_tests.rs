// 本文件由 controls.rs 的 #[cfg(test)] mod tests 抽出。

use super::*;

#[test]
fn control_range_clamps_integer_and_float() {
    let integer = ControlRange::Integer {
        value: 50,
        min: Some(0),
        max: Some(100),
        step: Some(1),
        default: Some(50),
    };
    assert_eq!(
        integer.clamp(ControlValue::Integer(200)),
        ControlValue::Integer(100)
    );
    assert_eq!(
        integer.clamp(ControlValue::Integer(-5)),
        ControlValue::Integer(0)
    );
    assert_eq!(integer.value(), ControlValue::Integer(50));
    assert!(!integer.is_auto());

    let float = ControlRange::Float {
        value: 0.5,
        min: Some(0.0),
        max: Some(1.0),
        step: Some(0.1),
        default: Some(0.5),
    };
    assert_eq!(
        float.clamp(ControlValue::Float(2.0)),
        ControlValue::Float(1.0)
    );
    assert!(ControlRange::None.is_auto());
}

#[test]
fn control_range_enum_picks_nearest_possible_value() {
    let range = ControlRange::Enum {
        value: 0,
        possible: vec![0, 10, 20],
        default: Some(0),
    };
    assert_eq!(range.clamp(ControlValue::Enum(17)), ControlValue::Enum(20));
}

#[test]
fn control_flags_express_capabilities() {
    let writable = ControlFlags {
        manual: true,
        automatic: true,
        ..ControlFlags::default()
    };
    assert!(writable.can_write());
    assert!(writable.supports_auto());
    let readonly = ControlFlags {
        read_only: true,
        automatic: true,
        ..ControlFlags::default()
    };
    assert!(!readonly.can_write());
    assert!(readonly.supports_auto());
    let disabled = ControlFlags {
        disabled: true,
        manual: true,
        ..ControlFlags::default()
    };
    assert!(!disabled.can_write());
    assert!(!disabled.supports_auto());
}

#[test]
fn quality_settings_entries_include_only_set_fields() {
    let settings = QualitySettings {
        auto_white_balance: Some(true),
        auto_exposure: Some(false),
        brightness: Some(120),
        anti_flicker: Some(PowerLineFrequency::Hz50),
        ..QualitySettings::default()
    };
    let entries = settings.entries();
    assert_eq!(entries.len(), 3);
    assert_eq!(
        entries[0],
        (CameraControlId::WhiteBalance, SettingIntent::Auto)
    );
    assert_eq!(
        entries[1],
        (CameraControlId::Exposure, SettingIntent::Manual)
    );
    assert_eq!(
        entries[2],
        (
            CameraControlId::Brightness,
            SettingIntent::Value(ControlValue::Integer(120))
        )
    );
    assert!(QualitySettings::default().entries().is_empty());
    assert_eq!(QualitySettings::auto_3a().entries().len(), 3);
}

#[test]
fn apply_report_tracks_per_control_outcomes() {
    let mut report = SettingsApplyReport::default();
    assert!(report.is_empty());
    report.applied.push(CameraControlId::Brightness);
    report.unsupported.push(CameraControlId::Focus);
    report
        .failed
        .push((CameraControlId::Gain, "busy".to_owned()));
    assert_eq!(report.applied_count(), 1);
    assert!(!report.is_empty());
}

#[test]
fn control_range_clamps_enum_to_nearest_and_preserves_other_types() {
    let range = ControlRange::Enum {
        value: 0,
        possible: vec![1, 2, 3],
        default: Some(1),
    };
    // 17 离 3 最近。
    assert_eq!(range.clamp(ControlValue::Enum(17)), ControlValue::Enum(3));
    // 类型不匹配原样返回。
    assert_eq!(
        range.clamp(ControlValue::Integer(42)),
        ControlValue::Integer(42)
    );
    let none = ControlRange::None;
    assert_eq!(
        none.clamp(ControlValue::Boolean(true)),
        ControlValue::Boolean(true)
    );
}

#[test]
fn integer_range_without_bounds_clamps_to_type_limits() {
    let range = ControlRange::Integer {
        value: 0,
        min: None,
        max: None,
        step: Some(1),
        default: None,
    };
    assert_eq!(
        range.clamp(ControlValue::Integer(i64::MAX)),
        ControlValue::Integer(i64::MAX)
    );
    assert_eq!(
        range.clamp(ControlValue::Integer(i64::MIN)),
        ControlValue::Integer(i64::MIN)
    );
}

#[test]
fn boolean_and_float_ranges_report_values() {
    let boolean = ControlRange::Boolean {
        value: true,
        default: Some(false),
    };
    assert_eq!(boolean.value(), ControlValue::Boolean(true));
    let float = ControlRange::Float {
        value: 0.25,
        min: None,
        max: None,
        step: None,
        default: None,
    };
    assert_eq!(float.value(), ControlValue::Float(0.25));
}

#[test]
fn all_settings_fields_are_reflected_in_entries() {
    let settings = QualitySettings {
        auto_white_balance: Some(false),
        auto_exposure: Some(true),
        auto_focus: Some(false),
        brightness: Some(10),
        contrast: Some(20),
        saturation: Some(30),
        sharpness: Some(40),
        gain: Some(50),
        exposure: Some(60),
        anti_flicker: Some(PowerLineFrequency::Hz60),
    };
    // 3 个自动项 + 6 个数值项 = 9（anti_flicker 不进 entries）。
    assert_eq!(settings.entries().len(), 9);
    assert!(settings
        .entries()
        .iter()
        .any(|(id, intent)| *id == CameraControlId::Exposure
            && matches!(intent, SettingIntent::Value(ControlValue::Integer(60)))));
}

#[test]
fn control_labels_are_stable() {
    assert_eq!(CameraControlId::WhiteBalance.to_string(), "white-balance");
    assert_eq!(CameraControlId::Other(42).to_string(), "other(42)");
    assert_eq!(CameraControlId::ALL.len(), 15);
}
