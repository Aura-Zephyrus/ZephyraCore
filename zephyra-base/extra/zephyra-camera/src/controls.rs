//! 摄像头参数控制的自有类型（驱动侧能力，不做软件图像处理）。
//!
//! 这里只定义数据模型与设置意图；实际读写由 `capture` feature 下的
//! `CameraManager` 转换成 nokhwa 的 `KnownCameraControl` / `ControlValueSetter`。
//! 是否真正生效完全取决于摄像头驱动，因此"设备不支持"是一等公民：
//! [`SettingsApplyReport`] 逐项记录结果，绝不因个别控制失败而中断其余设置。

/// 摄像头控制项。与 nokhwa 的 `KnownCameraControl` 一一对应但由本 crate 自有。
#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq, Ord, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CameraControlId {
    Brightness,
    Contrast,
    Hue,
    Saturation,
    Sharpness,
    Gamma,
    WhiteBalance,
    BacklightComp,
    Gain,
    Pan,
    Tilt,
    Zoom,
    Exposure,
    Iris,
    Focus,
    /// 防闪烁（电源频率）。nokhwa 0.10 不暴露该控制，仅用于报告"不支持"。
    AntiFlicker,
    /// 平台相关扩展控制（nokhwa 用 u128 属性 ID 表示）。
    Other(u128),
}

impl CameraControlId {
    pub const ALL: [Self; 15] = [
        Self::Brightness,
        Self::Contrast,
        Self::Hue,
        Self::Saturation,
        Self::Sharpness,
        Self::Gamma,
        Self::WhiteBalance,
        Self::BacklightComp,
        Self::Gain,
        Self::Pan,
        Self::Tilt,
        Self::Zoom,
        Self::Exposure,
        Self::Iris,
        Self::Focus,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Brightness => "brightness",
            Self::Contrast => "contrast",
            Self::Hue => "hue",
            Self::Saturation => "saturation",
            Self::Sharpness => "sharpness",
            Self::Gamma => "gamma",
            Self::WhiteBalance => "white-balance",
            Self::BacklightComp => "backlight-compensation",
            Self::Gain => "gain",
            Self::Pan => "pan",
            Self::Tilt => "tilt",
            Self::Zoom => "zoom",
            Self::Exposure => "exposure",
            Self::Iris => "iris",
            Self::Focus => "focus",
            Self::AntiFlicker => "anti-flicker",
            Self::Other(_) => "other",
        }
    }
}

impl std::fmt::Display for CameraControlId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Other(id) => write!(f, "other({id})"),
            _ => f.write_str(self.label()),
        }
    }
}

/// 控制值。
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ControlValue {
    /// 自动模式（对应 `ControlValueSetter::None` + Automatic 标志）。
    Auto,
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Enum(i64),
    Raw(String),
}

/// 设备声明的取值范围与当前值。
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ControlRange {
    Integer {
        value: i64,
        min: Option<i64>,
        max: Option<i64>,
        step: Option<i64>,
        default: Option<i64>,
    },
    Float {
        value: f64,
        min: Option<f64>,
        max: Option<f64>,
        step: Option<f64>,
        default: Option<f64>,
    },
    Boolean {
        value: bool,
        default: Option<bool>,
    },
    Enum {
        value: i64,
        possible: Vec<i64>,
        default: Option<i64>,
    },
    None,
}

impl ControlRange {
    #[must_use]
    pub fn value(&self) -> ControlValue {
        match self {
            Self::Integer { value, .. } => ControlValue::Integer(*value),
            Self::Float { value, .. } => ControlValue::Float(*value),
            Self::Boolean { value, .. } => ControlValue::Boolean(*value),
            Self::Enum { value, .. } => ControlValue::Enum(*value),
            Self::None => ControlValue::Auto,
        }
    }

    /// 把设置值钳制到设备声明范围；类型不匹配时原样返回（由后端决定拒绝）。
    #[must_use]
    pub fn clamp(&self, value: ControlValue) -> ControlValue {
        match (self, value) {
            (Self::Integer { min, max, .. }, ControlValue::Integer(v)) => {
                ControlValue::Integer(v.clamp(min.unwrap_or(i64::MIN), max.unwrap_or(i64::MAX)))
            }
            (Self::Float { min, max, .. }, ControlValue::Float(v)) => {
                let low = min.unwrap_or(f64::NEG_INFINITY);
                let high = max.unwrap_or(f64::INFINITY);
                ControlValue::Float(v.clamp(low, high))
            }
            (Self::Enum { possible, .. }, ControlValue::Enum(v)) if !possible.is_empty() => {
                let nearest = possible
                    .iter()
                    .copied()
                    .min_by_key(|candidate| (candidate - v).abs())
                    .expect("possible 非空");
                ControlValue::Enum(nearest)
            }
            (_, other) => other,
        }
    }

    #[must_use]
    pub fn is_auto(&self) -> bool {
        matches!(self, Self::None)
    }
}

/// 设备声明的控制标志（对齐 nokhwa `KnownCameraControlFlag`）。
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ControlFlags {
    pub automatic: bool,
    pub manual: bool,
    pub continuous: bool,
    pub read_only: bool,
    pub write_only: bool,
    pub volatile: bool,
    pub disabled: bool,
}

impl ControlFlags {
    #[must_use]
    pub const fn can_write(&self) -> bool {
        !self.read_only && !self.disabled && !self.write_only
    }

    #[must_use]
    pub const fn supports_auto(&self) -> bool {
        self.automatic && !self.disabled
    }
}

/// 一项控制的完整能力描述。
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ControlCapabilities {
    pub id: CameraControlId,
    pub name: String,
    pub range: ControlRange,
    pub flags: ControlFlags,
    pub active: bool,
}

impl ControlCapabilities {
    #[must_use]
    pub fn supports_auto(&self) -> bool {
        self.flags.supports_auto()
    }

    #[must_use]
    pub fn is_writable(&self) -> bool {
        self.flags.can_write()
    }
}

/// 防闪烁的电源频率（荧光灯下消除条纹）。
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PowerLineFrequency {
    Hz50,
    Hz60,
}

/// 画质设置意图。`None` 表示"不改这一项"，而不是"重置为默认值"。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct QualitySettings {
    pub auto_white_balance: Option<bool>,
    pub auto_exposure: Option<bool>,
    pub auto_focus: Option<bool>,
    pub brightness: Option<i64>,
    pub contrast: Option<i64>,
    pub saturation: Option<i64>,
    pub sharpness: Option<i64>,
    pub gain: Option<i64>,
    /// 曝光值（设备单位，非 EV；范围请查 [`ControlCapabilities`]）。
    pub exposure: Option<i64>,
    /// 防闪烁意图。注意：nokhwa 0.10.11 **不暴露**电源频率控制，
    /// 因此这项只会记为"不支持"，保留字段供未来后端使用。
    pub anti_flicker: Option<PowerLineFrequency>,
}

impl QualitySettings {
    /// 让设备自动处理 3A（白平衡/曝光/对焦）——最常见的"画质一键优化"。
    #[must_use]
    pub fn auto_3a() -> Self {
        Self {
            auto_white_balance: Some(true),
            auto_exposure: Some(true),
            auto_focus: Some(true),
            ..Self::default()
        }
    }

    /// 逐项应用结果。
    #[must_use]
    pub fn entries(&self) -> Vec<(CameraControlId, SettingIntent)> {
        let mut entries = Vec::new();
        let auto_intent = |enabled: bool| {
            if enabled {
                SettingIntent::Auto
            } else {
                SettingIntent::Manual
            }
        };
        if let Some(value) = self.auto_white_balance {
            entries.push((CameraControlId::WhiteBalance, auto_intent(value)));
        }
        if let Some(value) = self.auto_exposure {
            entries.push((CameraControlId::Exposure, auto_intent(value)));
        }
        if let Some(value) = self.auto_focus {
            entries.push((CameraControlId::Focus, auto_intent(value)));
        }
        for (id, value) in [
            (CameraControlId::Brightness, self.brightness),
            (CameraControlId::Contrast, self.contrast),
            (CameraControlId::Saturation, self.saturation),
            (CameraControlId::Sharpness, self.sharpness),
            (CameraControlId::Gain, self.gain),
            (CameraControlId::Exposure, self.exposure),
        ] {
            if let Some(value) = value {
                entries.push((id, SettingIntent::Value(ControlValue::Integer(value))));
            }
        }
        entries
    }
}

/// 单项设置意图。
#[derive(Clone, Debug, PartialEq)]
pub enum SettingIntent {
    Auto,
    Manual,
    Value(ControlValue),
}

/// 扩展摄像头控制（Windows KS 扩展属性，超出 nokhwa 的 `KnownCameraControl` 范围）。
///
/// 对应 Chromium 的：
/// - `KSPROPERTY_CAMERACONTROL_EXTENDED_BACKGROUNDSEGMENTATION`
/// - `KSPROPERTY_CAMERACONTROL_EXTENDED_EYEGAZECORRECTION`
/// - `KSPROPERTY_CAMERACONTROL_EXTENDED_DIGITALWINDOW`
#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ExtendedControl {
    /// 背景虚化（Background Segmentation）。
    BackgroundBlur,
    /// 眼神矫正（Eye Gaze Correction）。
    EyeGazeCorrection,
    /// 人脸取景 / 数字窗口（Digital Window / Auto Face Framing）。
    FaceFraming,
    /// HDR（部分设备支持）。
    Hdr,
}

impl ExtendedControl {
    pub const ALL: [Self; 4] = [
        Self::BackgroundBlur,
        Self::EyeGazeCorrection,
        Self::FaceFraming,
        Self::Hdr,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::BackgroundBlur => "background-blur",
            Self::EyeGazeCorrection => "eye-gaze-correction",
            Self::FaceFraming => "face-framing",
            Self::Hdr => "hdr",
        }
    }

    /// Windows KS 扩展属性 GUID（以 u128 表示，便于序列化）。
    #[must_use]
    pub const fn ks_property_id(self) -> u128 {
        match self {
            // KSPROPERTY_CAMERACONTROL_EXTENDED_BACKGROUNDSEGMENTATION = 1
            Self::BackgroundBlur => 1,
            // KSPROPERTY_CAMERACONTROL_EXTENDED_EYEGAZECORRECTION = 4
            Self::EyeGazeCorrection => 4,
            // KSPROPERTY_CAMERACONTROL_EXTENDED_DIGITALWINDOW = 19
            Self::FaceFraming => 19,
            // KSPROPERTY_CAMERACONTROL_EXTENDED_HDR = 17
            Self::Hdr => 17,
        }
    }
}

impl std::fmt::Display for ExtendedControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// 扩展控制的目标状态。
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ExtendedControlSetting {
    Off,
    On,
    /// 设备自动模式（如人脸取景的 AUTOFACEFRAMING）。
    Auto,
}

/// 拍照设置意图（高分辨率静帧，独立于视频流）。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PhotoSettings {
    /// 目标静帧分辨率；`None` 表示用设备能力表中的最大分辨率。
    pub resolution: Option<crate::device::Resolution>,
    /// JPEG 编码质量（1..=100）；部分后端忽略。
    pub jpeg_quality: Option<u8>,
    /// 是否镜像（前置摄像头常见）。
    pub mirror: bool,
    /// 旋转角度（0/90/180/270）；Chromium 把旋转烘焙进像素。
    pub rotation_degrees: u16,
}

/// 一次性应用多项目设置的结果报告。不抛错——逐项记录。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SettingsApplyReport {
    /// 成功设置的控制项。
    pub applied: Vec<CameraControlId>,
    /// 设备不支持（未枚举到 / 只读 / 后端不暴露）。
    pub unsupported: Vec<CameraControlId>,
    /// 设置被设备拒绝，附错误文本。
    pub failed: Vec<(CameraControlId, String)>,
}

impl SettingsApplyReport {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.applied.is_empty() && self.unsupported.is_empty() && self.failed.is_empty()
    }

    #[must_use]
    pub fn applied_count(&self) -> usize {
        self.applied.len()
    }
}

#[cfg(test)]
#[path = "tests/controls_tests.rs"]
mod tests;
