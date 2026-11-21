//! 摄像头设备枚举、能力查询与帧采集。
//!
//! 本模块是唯一接触 nokhwa 的地方：所有对外返回值都转换成 [`crate::device`] 里的
//! 自有类型，调用方不需要知道采集后端的存在。

use std::time::Duration;

use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{
    ApiBackend, CameraFormat, CameraIndex, CameraInfo, ControlValueDescription, ControlValueSetter,
    FrameFormat, KnownCameraControl, KnownCameraControlFlag, RequestedFormat, RequestedFormatType,
    Resolution as NokhwaResolution,
};
use nokhwa::Camera;

use crate::controls::{
    CameraControlId, ControlCapabilities, ControlFlags, ControlRange, ControlValue,
    QualitySettings, SettingIntent, SettingsApplyReport,
};
use crate::device::{
    select_format, Backend, CameraError, DeviceCapabilities, DeviceChanges, DeviceIndex,
    DeviceInfo, FormatInfo, FormatPreference, PixelFormat, Resolution, Result,
};
use crate::quality::{
    CaptureFeedback, DropLog, DropReason, EwmaFrameRate, FrameRateEstimator, FrameStats,
    FrameStatsSnapshot, PacingController, StreamHealth,
};

/// nokhwa 后端 → 自有后端类型。
fn backend_from_api(api: ApiBackend) -> Backend {
    match api {
        ApiBackend::Auto => Backend::Auto,
        ApiBackend::AVFoundation => Backend::AvFoundation,
        ApiBackend::Video4Linux => Backend::Video4Linux,
        ApiBackend::UniversalVideoClass => Backend::UsbVideoClass,
        ApiBackend::MediaFoundation => Backend::MediaFoundation,
        ApiBackend::OpenCv => Backend::OpenCv,
        ApiBackend::GStreamer => Backend::GStreamer,
        ApiBackend::Network => Backend::Network,
        ApiBackend::Browser => Backend::Browser,
    }
}

/// 自有后端类型 → nokhwa 后端。
fn api_from_backend(backend: Backend) -> ApiBackend {
    match backend {
        Backend::Auto => ApiBackend::Auto,
        Backend::AvFoundation => ApiBackend::AVFoundation,
        Backend::Video4Linux => ApiBackend::Video4Linux,
        Backend::UsbVideoClass => ApiBackend::UniversalVideoClass,
        Backend::MediaFoundation => ApiBackend::MediaFoundation,
        Backend::OpenCv => ApiBackend::OpenCv,
        Backend::GStreamer => ApiBackend::GStreamer,
        Backend::Network => ApiBackend::Network,
        Backend::Browser => ApiBackend::Browser,
    }
}

fn pixel_from_frame(format: FrameFormat) -> PixelFormat {
    match format {
        FrameFormat::MJPEG => PixelFormat::Mjpeg,
        FrameFormat::YUYV => PixelFormat::Yuyv,
        FrameFormat::NV12 => PixelFormat::Nv12,
        FrameFormat::GRAY => PixelFormat::Gray,
        FrameFormat::RAWRGB => PixelFormat::RawRgb,
        FrameFormat::RAWBGR => PixelFormat::RawBgr,
    }
}

fn format_info_from_camera_format(format: &CameraFormat) -> FormatInfo {
    FormatInfo::new(
        Resolution::new(format.resolution().width(), format.resolution().height()),
        pixel_from_frame(format.format()),
        format.frame_rate(),
    )
}

fn frame_from_pixel(format: PixelFormat) -> FrameFormat {
    match format {
        PixelFormat::Mjpeg => FrameFormat::MJPEG,
        PixelFormat::Yuyv => FrameFormat::YUYV,
        PixelFormat::Nv12 => FrameFormat::NV12,
        PixelFormat::Gray => FrameFormat::GRAY,
        PixelFormat::RawRgb => FrameFormat::RAWRGB,
        PixelFormat::RawBgr => FrameFormat::RAWBGR,
    }
}

fn camera_index_from_device_index(index: &DeviceIndex) -> CameraIndex {
    match index {
        DeviceIndex::Index(i) => CameraIndex::Index(*i),
        DeviceIndex::String(s) => CameraIndex::String(s.clone()),
    }
}

/// `CameraInfo::misc()` 在 MSMF 后端是设备符号链接，是跨枚举稳定的唯一 ID。
fn device_info_from_camera_info(info: &CameraInfo) -> DeviceInfo {
    let index = match info.index() {
        CameraIndex::Index(i) => DeviceIndex::Index(*i),
        CameraIndex::String(s) => DeviceIndex::String(s.clone()),
    };
    DeviceInfo::new(
        index,
        info.human_name(),
        info.description(),
        info.misc(),
        backend_from_api(ApiBackend::Auto),
    )
}

// ---- 摄像头控制的自有类型 ↔ nokhwa ----------------------------------------

fn control_id_from_known(control: KnownCameraControl) -> CameraControlId {
    match control {
        KnownCameraControl::Brightness => CameraControlId::Brightness,
        KnownCameraControl::Contrast => CameraControlId::Contrast,
        KnownCameraControl::Hue => CameraControlId::Hue,
        KnownCameraControl::Saturation => CameraControlId::Saturation,
        KnownCameraControl::Sharpness => CameraControlId::Sharpness,
        KnownCameraControl::Gamma => CameraControlId::Gamma,
        KnownCameraControl::WhiteBalance => CameraControlId::WhiteBalance,
        KnownCameraControl::BacklightComp => CameraControlId::BacklightComp,
        KnownCameraControl::Gain => CameraControlId::Gain,
        KnownCameraControl::Pan => CameraControlId::Pan,
        KnownCameraControl::Tilt => CameraControlId::Tilt,
        KnownCameraControl::Zoom => CameraControlId::Zoom,
        KnownCameraControl::Exposure => CameraControlId::Exposure,
        KnownCameraControl::Iris => CameraControlId::Iris,
        KnownCameraControl::Focus => CameraControlId::Focus,
        KnownCameraControl::Other(id) => CameraControlId::Other(id),
    }
}

/// `Other` 在 nokhwa 0.10 的 MSMF 后端只接受 ColorEnable 属性 ID，
/// 电源频率等扩展控制无法表达，因此返回 `None` 表示"后端不支持"。
fn known_from_control_id(id: CameraControlId) -> Option<KnownCameraControl> {
    Some(match id {
        CameraControlId::Brightness => KnownCameraControl::Brightness,
        CameraControlId::Contrast => KnownCameraControl::Contrast,
        CameraControlId::Hue => KnownCameraControl::Hue,
        CameraControlId::Saturation => KnownCameraControl::Saturation,
        CameraControlId::Sharpness => KnownCameraControl::Sharpness,
        CameraControlId::Gamma => KnownCameraControl::Gamma,
        CameraControlId::WhiteBalance => KnownCameraControl::WhiteBalance,
        CameraControlId::BacklightComp => KnownCameraControl::BacklightComp,
        CameraControlId::Gain => KnownCameraControl::Gain,
        CameraControlId::Pan => KnownCameraControl::Pan,
        CameraControlId::Tilt => KnownCameraControl::Tilt,
        CameraControlId::Zoom => KnownCameraControl::Zoom,
        CameraControlId::Exposure => KnownCameraControl::Exposure,
        CameraControlId::Iris => KnownCameraControl::Iris,
        CameraControlId::Focus => KnownCameraControl::Focus,
        CameraControlId::AntiFlicker | CameraControlId::Other(_) => return None,
    })
}

fn range_from_description(description: &ControlValueDescription) -> ControlRange {
    match description {
        ControlValueDescription::None => ControlRange::None,
        ControlValueDescription::Integer {
            value,
            default,
            step,
        } => ControlRange::Integer {
            value: *value,
            min: None,
            max: None,
            step: Some(*step),
            default: Some(*default),
        },
        ControlValueDescription::IntegerRange {
            min,
            max,
            value,
            step,
            default,
        } => ControlRange::Integer {
            value: *value,
            min: Some(*min),
            max: Some(*max),
            step: Some(*step),
            default: Some(*default),
        },
        ControlValueDescription::Float {
            value,
            default,
            step,
        } => ControlRange::Float {
            value: *value,
            min: None,
            max: None,
            step: Some(*step),
            default: Some(*default),
        },
        ControlValueDescription::FloatRange {
            min,
            max,
            value,
            step,
            default,
        } => ControlRange::Float {
            value: *value,
            min: Some(*min),
            max: Some(*max),
            step: Some(*step),
            default: Some(*default),
        },
        ControlValueDescription::Boolean { value, default } => ControlRange::Boolean {
            value: *value,
            default: Some(*default),
        },
        ControlValueDescription::Enum {
            value,
            possible,
            default,
        } => ControlRange::Enum {
            value: *value,
            possible: possible.clone(),
            default: Some(*default),
        },
        // 字符串/字节/键值/点/RGB 等奇异类型统一以 Auto 表示，避免泄漏后端类型。
        _ => ControlRange::None,
    }
}

fn flags_from_known(flags: &[KnownCameraControlFlag]) -> ControlFlags {
    let mut result = ControlFlags::default();
    for flag in flags {
        match flag {
            KnownCameraControlFlag::Automatic => result.automatic = true,
            KnownCameraControlFlag::Manual => result.manual = true,
            KnownCameraControlFlag::Continuous => result.continuous = true,
            KnownCameraControlFlag::ReadOnly => result.read_only = true,
            KnownCameraControlFlag::WriteOnly => result.write_only = true,
            KnownCameraControlFlag::Volatile => result.volatile = true,
            KnownCameraControlFlag::Disabled => result.disabled = true,
        }
    }
    result
}

fn setter_from_value(value: &ControlValue) -> Option<ControlValueSetter> {
    Some(match value {
        ControlValue::Auto => ControlValueSetter::None,
        ControlValue::Integer(value) => ControlValueSetter::Integer(*value),
        ControlValue::Float(value) => ControlValueSetter::Float(*value),
        ControlValue::Boolean(value) => ControlValueSetter::Boolean(*value),
        ControlValue::Enum(value) => ControlValueSetter::EnumValue(*value),
        ControlValue::Raw(_) => return None,
    })
}

fn caps_from_camera_control(control: &nokhwa::utils::CameraControl) -> ControlCapabilities {
    ControlCapabilities {
        id: control_id_from_known(control.control()),
        name: control.name().to_owned(),
        range: range_from_description(control.description()),
        flags: flags_from_known(control.flag()),
        active: control.active(),
    }
}

/// 摄像头管理器。
///
/// 双路径取帧：
/// - NV12 直传（默认优先）：`frame()` 返回 Media Foundation 原生 NV12 采样
///   （后端 `MF_READWRITE_DISABLE_CONVERTERS=true`，无驱动/软件转换），
///   字节以 `bytes::Bytes` 引用计数持有，可零拷贝切片后跨线程上传 GPU。
///   CPU 每帧像素级工作为零（YUV→RGB 由渲染管线着色器完成）。
/// - RGB 回退：设备仅提供 MJPEG 等格式时，走 nokhwa 软件解码为 RGB，
///   再由上层 [`crate::CpuFrameProcessor`] 转换（旧行为）。
pub struct CameraManager {
    camera: Option<Camera>,
    /// `dx` feature 下的原生 GPU 采集器（与 nokhwa 路径互斥）。
    #[cfg(feature = "dx")]
    gpu: Option<crate::dx::DxCapture>,
    is_open: bool,
    /// 当前流是否输出原生 NV12
    native_nv12: bool,
    /// 可复用的 RGB 解码缓冲（回退路径，避免每帧分配）
    decode_buf: Vec<u8>,
    /// 协商到的帧率（设备**声称**的能力，取所选格式/分辨率下的最大值）。
    negotiated_fps: u32,
    /// 实际投递周期的滚动样本（升序维护，中位数即 [`Self::observed_period`]）。
    ///
    /// # 为什么必须实测，不能只信 `negotiated_fps`
    ///
    /// `compatible_camera_formats()` 报的是设备**声称**的最大帧率。本机实测：
    ///
    /// ```text
    /// 协商：NV12 1280x720@30
    /// MF 源端时间戳间隔：min 31.6  中位 47.6  p95 47.9  max 48.1 ms
    /// ```
    ///
    /// 设备以 **≈21fps** 稳定输出，而非声称的 30fps，而且**极其规律**（p95 与
    /// 中位仅差 0.3ms）。按声称值定节拍会一直比实际快：采集线程以 60Hz 空转
    /// poll（每次都阻塞等样本），显示侧按 33ms 网格取帧——于是"每 33ms 显示
    /// 一次、实际每 47.6ms 才有一张新画面"，观感是**隔一帧才动一下**。
    source_periods: std::collections::VecDeque<Duration>,
    /// 上一个源端时间戳（`VecDeque` 存的是间隔，不是时间戳本身）。
    last_source_ts: Option<Duration>,
    /// 最近一次枚举的设备快照（供名称/ID 查找与热插拔比较）。
    known_devices: Vec<DeviceInfo>,
    /// 最近一次 [`Self::refresh_devices`] 算出的差异。
    last_changes: DeviceChanges,
    /// 当前打开的设备与偏好，供 `set_resolution` / 流恢复重开使用。
    opened_device: Option<DeviceInfo>,
    opened_preference: FormatPreference,
    /// 当前精确媒体类型（成功后记录，用于切换失败时回退）。
    current_format_info: Option<crate::device::FormatInfo>,
    /// 切换失败时回退到的上一个成功格式。
    fallback_format: Option<crate::device::FormatInfo>,
    /// 最近一次分辨率/帧率切换失败的原因（供 UI 提示）。
    pub last_switch_error: Option<String>,
    /// 自适应分辨率/帧率控制器（启用后按背压自动切换）。
    adaptive: Option<crate::quality::AdaptiveSelector>,

    // ---- 画质诊断与节拍 ----
    /// 帧间隔统计（中位/p95/抖动/投递与丢弃计数）。
    frame_stats: FrameStats,
    /// 按原因分类的丢帧日志。
    drop_log: DropLog,
    /// Chrome 式分桶帧率估计（不稳定时不出数）。
    frame_rate_estimator: FrameRateEstimator,
    /// EWMA + 迟滞的展示帧率。
    ewma_fps: EwmaFrameRate,
    /// 带漂移累积的投递节拍。
    pacing: PacingController,
    /// 消费者背压信号。
    feedback: CaptureFeedback,
    /// 上一次生效的节拍目标；变化时重置控制器。
    pacing_target: Duration,
    /// 视频流健康状态与有限次恢复。
    stream_health: StreamHealth,
    max_restarts: u32,
    /// 按错误类别分流的恢复状态机（取代旧的"一刀切"重启计数）。
    recovery: crate::recovery::RecoveryController,
    /// 最近一帧的三重时间戳（系统到达 / 流时钟 / 曝光开始）。
    last_timestamps: Option<crate::quality::FrameTimestamps>,
    /// 曝光时间戳单调化跟踪器。
    timestamp_tracker: crate::quality::TimestampTracker,
    /// 系统单调时钟的基准点（用于把 `Instant` 转成可序列化的 `Duration`）。
    stream_epoch: std::time::Instant,
    /// 背压反馈跟踪器（含预映射时间窗口）。
    feedback_tracker: crate::quality::FeedbackTracker,
    /// 有界帧缓冲池（跟踪归属与利用率）。
    buffer_pool: crate::quality::FrameBufferPool,
}

/// 滚动样本数：约 3 秒 @20fps。够短到能跟上设备节拍变化，够长到不被单帧抖动带偏。
const SOURCE_PERIOD_SAMPLES: usize = 64;

impl CameraManager {
    pub fn new() -> Self {
        Self {
            camera: None,
            #[cfg(feature = "dx")]
            gpu: None,
            is_open: false,
            native_nv12: false,
            decode_buf: Vec::new(),
            negotiated_fps: 0,
            source_periods: std::collections::VecDeque::with_capacity(SOURCE_PERIOD_SAMPLES),
            last_source_ts: None,
            known_devices: Vec::new(),
            last_changes: DeviceChanges::default(),
            opened_device: None,
            opened_preference: FormatPreference::default(),
            current_format_info: None,
            fallback_format: None,
            last_switch_error: None,
            adaptive: None,
            frame_stats: FrameStats::new(),
            drop_log: DropLog::new(),
            frame_rate_estimator: FrameRateEstimator::new(),
            ewma_fps: EwmaFrameRate::new(),
            pacing: PacingController::new(Duration::from_millis(33)),
            feedback: CaptureFeedback::default(),
            pacing_target: Duration::from_millis(33),
            stream_health: StreamHealth::default(),
            max_restarts: 3,
            recovery: crate::recovery::RecoveryController::default(),
            last_timestamps: None,
            timestamp_tracker: crate::quality::TimestampTracker::new(),
            stream_epoch: std::time::Instant::now(),
            feedback_tracker: crate::quality::FeedbackTracker::new(),
            buffer_pool: crate::quality::FrameBufferPool::new(3),
        }
    }

    /// 设置流恢复的最大自动重启次数（默认 3 次，仿 Chromium `kMaxD3DRestarts`）。
    pub fn set_max_restarts(&mut self, max: u32) {
        self.max_restarts = max;
    }

    // ---- 帧率与节拍（保持原有逻辑不变） ------------------------------------

    /// 设备**声称**的最大帧率（协商值）。样本不足时作为帧率的唯一依据。
    pub fn negotiated_fps(&self) -> u32 {
        self.negotiated_fps
    }

    /// 设备**实际**的投递周期（中位数）。
    ///
    /// 样本不足 [`SOURCE_PERIOD_SAMPLES`] 的一半时返回 `None` —— 宁可让调用方
    /// 退回协商值，也不要用三五个样本的结论去改节拍（中位数在极少样本下会被
    /// 单帧抖动主导）。
    pub fn observed_period(&self) -> Option<Duration> {
        self.source_periods
            .get(self.source_periods.len() / 2)
            .copied()
            .filter(|_| self.source_periods.len() >= SOURCE_PERIOD_SAMPLES / 2)
    }

    /// 采集节拍：取"请求值"与"实测周期"中**较慢**的那个。
    ///
    /// 设备慢于请求 → 以实测为准，否则 `poll` 每轮都阻塞等样本，纯空转；
    /// 请求慢于设备 → 以请求为准，那是用户主动要的降帧（如"低帧率"省电）。
    pub fn pacing_period(&self, requested: Duration) -> Duration {
        match self.observed_period() {
            Some(observed) => observed.max(requested),
            None => requested,
        }
    }

    /// 记一个源端间隔样本（保持 `VecDeque` **升序**，便于取中位数）。
    ///
    /// 用**中位数**而不是均值：源端偶发的一两帧长间隔（设备瞬时卡顿）会把均值
    /// 拉高一截，而中位数几乎不受影响——节拍要的是"常态"，不是"平均"。
    fn note_source_period(&mut self, ts: Duration) {
        let Some(previous) = self.last_source_ts.replace(ts) else {
            // 首样本只锚定基准，不构成间隔
            return;
        };
        // 先比再减：`Duration` 的 `Sub` 在下溢时 panic，而源端时间戳回退
        // （设备重置 / 换流）是真实会发生的，直接 `checked_sub` 反而把一个
        // 应当"丢弃样本"的情况升级成 panic。
        if ts <= previous {
            return;
        }
        let delta = ts - previous;
        // 剔除明显不是"帧间隔"的值：<1ms（重复时间戳）与 >1s（停流/长停顿）。
        if delta < Duration::from_millis(1) || delta > Duration::from_secs(1) {
            return;
        }
        let at = self.source_periods.partition_point(|&d| d <= delta);
        self.source_periods.insert(at, delta);
        while self.source_periods.len() > SOURCE_PERIOD_SAMPLES {
            self.source_periods.pop_front();
        }
    }

    // ---- 画质诊断与恢复 -----------------------------------------------------

    /// 当前投递节拍：取观测周期（或协商值）再按消费者背压放慢。
    fn effective_pacing_target(&self) -> Duration {
        let base = self.observed_period().unwrap_or_else(|| {
            if self.negotiated_fps > 0 {
                Duration::from_secs(1) / self.negotiated_fps
            } else {
                Duration::from_millis(33)
            }
        });
        self.feedback.suggested_period(base)
    }

    /// 节拍目标变化时才重置控制器（避免每帧重置丢掉累积漂移）。
    fn sync_pacing(&mut self) {
        let target = self.effective_pacing_target();
        if target != self.pacing_target {
            self.pacing.set_target(target);
            self.pacing_target = target;
        }
    }

    /// 喂入一帧的源端时间戳，返回该帧是否应因节拍被丢弃。
    fn note_frame_timestamp(&mut self, timestamp: Duration) -> Option<DropReason> {
        self.sync_pacing();
        if let Some(delta) = self.frame_stats.note_timestamp(timestamp) {
            let _ = self.frame_rate_estimator.add_sample(delta);
        }
        let _ = self.ewma_fps.update(timestamp);
        match self.pacing.should_deliver(timestamp) {
            crate::quality::PacingDecision::Deliver => None,
            crate::quality::PacingDecision::Drop(reason) => {
                self.record_drop(timestamp, reason);
                Some(reason)
            }
        }
    }

    fn record_drop(&mut self, timestamp: Duration, reason: DropReason) {
        self.drop_log.note(timestamp, reason);
        self.frame_stats.note_dropped();
    }

    /// 按错误类别分流的恢复：瞬态错误重试、摄像头占用等待、GPU 丢失有界降级。
    fn recover_stream(&mut self, timestamp: Duration, reason: DropReason) -> bool {
        self.record_drop(timestamp, reason);
        self.stream_health.last_error = Some(reason.to_string());
        let kind = crate::recovery::FailureKind::from(reason);
        use crate::recovery::RecoveryAction;
        let action = self.recovery.on_failure(kind);
        let Some(device) = self.opened_device.clone() else {
            return false;
        };
        match action {
            RecoveryAction::None
            | RecoveryAction::RetryTransient { .. }
            | RecoveryAction::GiveUp => {
                if action == RecoveryAction::GiveUp {
                    self.stream_health.permanent_fallback = true;
                }
                false
            }
            RecoveryAction::FallbackToCpu => {
                // 与 Chromium 一致：GPU 重启次数用尽后降级到 CPU/RGB 路径。
                self.stream_health.permanent_fallback = true;
                let mut preference = self.opened_preference;
                preference.prefer_nv12 = false;
                match self.open_with(&device, &preference) {
                    Ok(()) => {
                        self.stream_health.clear_error();
                        true
                    }
                    Err(error) => {
                        self.stream_health.last_error = Some(error.to_string());
                        false
                    }
                }
            }
            RecoveryAction::RestartGpu | RecoveryAction::RestartStream => {
                let mut preference = self.opened_preference;
                if self.recovery.permanent_fallback() {
                    preference.prefer_nv12 = false;
                }
                match self.open_with(&device, &preference) {
                    Ok(()) => {
                        self.recovery.note_success();
                        self.stream_health.clear_error();
                        true
                    }
                    Err(error) => {
                        self.stream_health.last_error = Some(error.to_string());
                        false
                    }
                }
            }
        }
    }

    /// 基于实际错误文本的恢复（比按 DropReason 更精确）。
    fn recover_from_error(&mut self, error: &CameraError) -> bool {
        let reason = match error {
            CameraError::Dx(message) => {
                if crate::recovery::classify_error(message)
                    == crate::recovery::FailureKind::D3dDeviceRemoved
                {
                    DropReason::D3dDeviceRemoved
                } else {
                    DropReason::DeviceLost
                }
            }
            _ => DropReason::DeviceLost,
        };
        self.recover_stream(Duration::ZERO, reason)
    }

    /// 手动重启视频流（复用当前设备与偏好），遵循有限次恢复策略。
    pub fn restart_stream(&mut self) -> Result<()> {
        let device = self.opened_device.clone().ok_or(CameraError::NotOpen)?;
        let action = self
            .recovery
            .on_failure(crate::recovery::FailureKind::Unknown);
        if action == crate::recovery::RecoveryAction::GiveUp {
            self.stream_health.permanent_fallback = true;
            return Err(CameraError::Stream("restart limit exceeded".to_owned()));
        }
        let preference = self.opened_preference;
        let result = self.open_with(&device, &preference);
        if result.is_ok() {
            self.recovery.note_success();
            self.stream_health.clear_error();
        }
        result
    }

    /// 恢复策略控制器（只读，便于 UI 显示"被占用/设备丢失/GPU 降级"）。
    #[must_use]
    pub const fn recovery(&self) -> &crate::recovery::RecoveryController {
        &self.recovery
    }

    /// 调整恢复策略。
    pub fn set_recovery_policy(&mut self, policy: crate::recovery::RecoveryPolicy) {
        self.recovery = crate::recovery::RecoveryController::new(policy);
    }

    /// 流是否真的启动（必须收到过首帧——Chromium 不信任 started 事件）。
    #[must_use]
    pub fn is_stream_started(&self) -> bool {
        self.recovery.is_started()
    }

    /// 最近一帧的三重时间戳。
    #[must_use]
    pub const fn last_timestamps(&self) -> Option<&crate::quality::FrameTimestamps> {
        self.last_timestamps.as_ref()
    }

    /// 时间戳单调化次数（设备时钟异常指标）。
    #[must_use]
    pub const fn timestamp_monotonicizations(&self) -> u64 {
        self.timestamp_tracker.monotonicization_count()
    }

    /// 当前帧统计快照。
    #[must_use]
    pub fn frame_stats(&self) -> FrameStatsSnapshot {
        self.frame_stats.snapshot()
    }

    /// 按原因统计的丢帧次数。
    #[must_use]
    pub fn drop_count(&self, reason: DropReason) -> u64 {
        self.drop_log.count(reason)
    }

    /// 总丢帧次数。
    #[must_use]
    pub fn total_drops(&self) -> u64 {
        self.drop_log.total()
    }

    /// 最近丢帧事件（相对时间毫秒, 原因）。
    pub fn recent_drops(&self) -> impl Iterator<Item = (u64, DropReason)> + '_ {
        self.drop_log.recent()
    }

    /// 分桶估计的可靠帧率（源端不稳定时为 `None`，不猜测）。
    #[must_use]
    pub fn reliable_fps(&self) -> Option<u32> {
        self.frame_rate_estimator.fps()
    }

    /// EWMA 平滑并迟滞发布后的帧率（适合 UI 展示）。
    #[must_use]
    pub fn smoothed_fps(&self) -> f32 {
        self.ewma_fps.published_fps()
    }

    /// 视频流健康状态。
    pub fn stream_health(&self) -> &StreamHealth {
        &self.stream_health
    }

    /// 设置消费者背压信号；过载时会按比例放慢投递节拍。
    pub fn set_feedback(&mut self, feedback: CaptureFeedback) {
        self.feedback_tracker.apply(feedback);
        self.feedback = *self.feedback_tracker.feedback();
        self.sync_pacing();
        if self.adaptive.is_some() {
            self.update_adaptive(feedback.resource_utilization);
        }
    }

    #[must_use]
    pub const fn feedback(&self) -> &CaptureFeedback {
        &self.feedback
    }

    /// 背压反馈跟踪器（含 `require_mapped_frame` 时间窗口）。
    #[must_use]
    pub const fn feedback_tracker(&self) -> &crate::quality::FeedbackTracker {
        &self.feedback_tracker
    }

    /// 当前缓冲池利用率（0.0..=N；>1 表示池已满且仍被持有）。
    #[must_use]
    pub fn buffer_pool_utilization(&self) -> f32 {
        self.buffer_pool.utilization()
    }

    /// 预留一个帧缓冲槽；池满时返回 `None`（应记录 `DropReason::BufferPoolExhausted`）。
    #[must_use]
    pub fn reserve_frame_buffer(&self) -> Option<crate::quality::ProducerLease> {
        self.buffer_pool.reserve()
    }

    /// 缓冲池作废（换流 / 设备重置后调用）。
    pub fn invalidate_frame_buffers(&self) {
        self.buffer_pool.invalidate();
    }

    // ---- 设备识别 ----------------------------------------------------------

    /// 仅查询，不更新缓存（内部使用）。
    fn query_devices(&self, backend: Backend) -> Result<Vec<DeviceInfo>> {
        let devices = nokhwa::query(api_from_backend(backend))
            .map_err(|error| CameraError::Backend(error.to_string()))?;
        Ok(devices.iter().map(device_info_from_camera_info).collect())
    }

    /// 用指定后端枚举摄像头，并更新内部缓存。
    pub fn list_devices_with_backend(&mut self, backend: Backend) -> Result<Vec<DeviceInfo>> {
        let devices = self.query_devices(backend)?;
        self.known_devices = devices.clone();
        Ok(devices)
    }

    /// 枚举可用摄像头设备（自动后端，等价于 `list_devices_with_backend(Backend::Auto)`）。
    pub fn list_devices(&mut self) -> Result<Vec<DeviceInfo>> {
        self.list_devices_with_backend(Backend::Auto)
    }

    /// 最近一次枚举的缓存快照，不会触发设备查询。
    pub fn devices(&self) -> &[DeviceInfo] {
        &self.known_devices
    }

    /// 重新枚举并与上次快照比较，返回新增/移除的设备（热插拔检测）。
    pub fn refresh_devices(&mut self) -> Result<DeviceChanges> {
        let fresh = self.query_devices(Backend::Auto)?;
        let previous = &self.known_devices;
        let changes = DeviceChanges {
            added: fresh
                .iter()
                .filter(|device| !previous.contains(device))
                .cloned()
                .collect(),
            removed: previous
                .iter()
                .filter(|device| !fresh.contains(device))
                .cloned()
                .collect(),
        };
        self.known_devices = fresh;
        self.last_changes = changes.clone();
        Ok(changes)
    }

    /// 最近一次 [`Self::refresh_devices`] 是否检测到变化。
    #[must_use]
    pub fn devices_changed(&self) -> bool {
        !self.last_changes.is_empty()
    }

    /// 最近一次枚举差异的明细。
    pub fn last_changes(&self) -> &DeviceChanges {
        &self.last_changes
    }

    /// 在缓存中按名称/描述查找设备（忽略大小写，子串匹配）。
    /// 缓存为空时自动枚举一次；之后如需刷新请显式调用 [`Self::refresh_devices`]。
    pub fn find_by_name(&mut self, name: &str) -> Result<Option<DeviceInfo>> {
        if self.known_devices.is_empty() {
            self.list_devices()?;
        }
        Ok(self
            .known_devices
            .iter()
            .find(|device| device.matches_name(name))
            .cloned())
    }

    /// 按唯一 ID（设备符号链接或其末段）查找设备。缓存策略同 [`Self::find_by_name`]。
    pub fn find_by_id(&mut self, id: &str) -> Result<Option<DeviceInfo>> {
        if self.known_devices.is_empty() {
            self.list_devices()?;
        }
        Ok(self
            .known_devices
            .iter()
            .find(|device| device.matches_id(id))
            .cloned())
    }

    /// 按名称或唯一 ID 查找设备。缓存策略同 [`Self::find_by_name`]。
    pub fn find(&mut self, query: &str) -> Result<Option<DeviceInfo>> {
        if self.known_devices.is_empty() {
            self.list_devices()?;
        }
        Ok(self
            .known_devices
            .iter()
            .find(|device| device.matches(query))
            .cloned())
    }

    // ---- 分辨率与能力 ------------------------------------------------------

    /// 查询设备声明的所有媒体类型。
    ///
    /// 注意：nokhwa 必须**临时打开设备**才能读取原生媒体类型表，因此调用期间
    /// 设备会被短暂占用；读完立即释放，不会开启视频流。
    pub fn capabilities(&self, device: &DeviceInfo) -> Result<DeviceCapabilities> {
        let mut camera = Camera::new(
            camera_index_from_device_index(device.index()),
            RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate),
        )
        .map_err(|error| CameraError::CapabilityQuery(error.to_string()))?;
        let formats = camera
            .compatible_camera_formats()
            .map_err(|error| CameraError::CapabilityQuery(error.to_string()))?;
        Ok(DeviceCapabilities::new(
            formats.iter().map(format_info_from_camera_format).collect(),
        ))
    }

    /// 按偏好打开指定设备。
    ///
    /// 格式协商阶梯：先按默认请求打开并读取设备原生媒体类型表，用
    /// [`crate::device::select_format`] 按偏好选出媒体类型；若选中 NV12，则以精确格式
    /// 重开为原生 NV12（下游直传管线接管，免掉每帧软件解码）；重开失败时保持默认
    /// 协商结果走 RGB 回退路径。
    pub fn open_with(&mut self, device: &DeviceInfo, preference: &FormatPreference) -> Result<()> {
        if self.is_open {
            self.close();
        }

        let index = camera_index_from_device_index(device.index());
        let mut camera = Camera::new(
            index.clone(),
            RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate),
        )
        .map_err(|error| CameraError::Open(error.to_string()))?;

        if let Some(target) = preference.resolution {
            let _ = camera.set_resolution(NokhwaResolution::new(target.width, target.height));
        }

        // 设备原生媒体类型表（compatible_fourcc 只返回前 2 个格式，不能用于能力判断）
        let native = camera.compatible_camera_formats().unwrap_or_default();
        let capabilities =
            DeviceCapabilities::new(native.iter().map(format_info_from_camera_format).collect());
        let chosen = select_format(capabilities.formats(), preference).ok_or_else(|| {
            CameraError::CapabilityQuery(format!(
                "no compatible media type for preference {preference:?}"
            ))
        })?;
        // camera 在本函数内只用于协商能力表，随后由 open_selected 重新打开；
        // drop 时 stop_stream 还未调用，这里显式停流避免占用设备。
        let _ = camera.stop_stream();
        drop(camera);

        self.open_selected(device, chosen, preference)
    }

    /// 用已选定的精确媒体类型打开设备（不重新协商格式）。
    fn open_selected(
        &mut self,
        device: &DeviceInfo,
        chosen: FormatInfo,
        preference: &FormatPreference,
    ) -> Result<()> {
        if self.is_open {
            self.close();
        }
        let index = camera_index_from_device_index(device.index());
        let mut camera = Camera::new(
            index.clone(),
            RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate),
        )
        .map_err(|error| CameraError::Open(error.to_string()))?;
        let target = Resolution::new(chosen.width(), chosen.height());
        let _ = camera.set_resolution(NokhwaResolution::new(target.width, target.height));

        // 后置 set_frame_format / set_resolution 在此后端不可用（当前帧率被误读为 1），
        // 因此必须在打开时就以精确格式协商——这对 NV12 直传和多分辨率/帧率切换都适用。
        let frame_format = frame_from_pixel(chosen.format);
        let requested_formats = [frame_format];
        let request = RequestedFormat::with_formats(
            RequestedFormatType::Exact(CameraFormat::new(
                NokhwaResolution::new(chosen.width(), chosen.height()),
                frame_format,
                chosen.fps,
            )),
            &requested_formats,
        );
        match Camera::new(index, request) {
            Ok(exact_camera) => {
                log::info!("Negotiated exact format {}", chosen.label());
                camera = exact_camera;
            }
            Err(error) => {
                log::warn!("Exact format negotiation rejected ({error}), keeping default format")
            }
        }

        camera
            .open_stream()
            .map_err(|error| CameraError::Stream(error.to_string()))?;

        let actual = camera.frame_format();
        self.native_nv12 = actual == FrameFormat::NV12;
        log::info!(
            "Camera stream format: {:?} ({})",
            actual,
            if self.native_nv12 {
                "zero-copy direct upload path"
            } else {
                "RGB decode fallback path"
            }
        );

        self.camera = Some(camera);
        self.is_open = true;
        // 记下**声称**的帧率。它只作为实测前的初值，以及实测样本不足时的兜底。
        self.negotiated_fps = chosen.fps;
        self.source_periods.clear();
        self.last_source_ts = None;
        self.opened_device = Some(device.clone());
        self.opened_preference = *preference;
        self.fallback_format = self.current_format_info;
        self.current_format_info = Some(chosen);

        // 每次（重）开流都重置诊断状态：旧流的丢帧统计对新流没有意义。
        self.frame_stats.clear();
        self.drop_log.clear();
        self.frame_rate_estimator.reset();
        self.ewma_fps.reset();
        self.pacing_target = self.effective_pacing_target();
        self.pacing = PacingController::new(self.pacing_target);
        Ok(())
    }

    /// 按数字索引与目标分辨率打开摄像头（兼容旧调用方式）。
    ///
    /// 等价于用 `Backend::Auto` 枚举出的设备构造 `DeviceInfo`，配合
    /// `prefer_nv12 = true` 的 [`FormatPreference`] 调用 [`Self::open_with`]。
    pub fn open(&mut self, device_index: usize, width: u32, height: u32) -> Result<()> {
        let device = DeviceInfo::new(
            DeviceIndex::Index(device_index as u32),
            "",
            "",
            "",
            Backend::Auto,
        );
        self.open_with(
            &device,
            &FormatPreference::new(Resolution::new(width, height)),
        )
    }

    /// 按名称查找并打开摄像头。
    pub fn open_by_name(&mut self, name: &str, preference: &FormatPreference) -> Result<()> {
        let device = self
            .find_by_name(name)?
            .ok_or_else(|| CameraError::DeviceNotFound(name.to_owned()))?;
        self.open_with(&device, preference)
    }

    /// 按唯一 ID 查找并打开摄像头（比数字索引更稳定）。
    pub fn open_by_id(&mut self, id: &str, preference: &FormatPreference) -> Result<()> {
        let device = self
            .find_by_id(id)?
            .ok_or_else(|| CameraError::DeviceNotFound(id.to_owned()))?;
        self.open_with(&device, preference)
    }

    // ---- 多分辨率 / 多帧率 --------------------------------------------------

    /// 按有序候选列表打开摄像头：逐项尝试，第一个协商成功者胜出。
    ///
    /// 例如 `[1080p@30, 720p@60, 480p@30, any]`，设备不支持高分辨率时自动降级。
    /// 全部失败时返回最后一次失败的错误。
    pub fn open_candidates(
        &mut self,
        device: &DeviceInfo,
        candidates: &[crate::device::FormatCandidate],
    ) -> Result<()> {
        let caps = self.capabilities(device)?;
        let selection = crate::device::select_from_candidates(caps.formats(), candidates)
            .ok_or_else(|| {
                CameraError::CapabilityQuery(
                    "none of the format candidates is supported".to_owned(),
                )
            })?;
        let preference = FormatPreference {
            resolution: Some(selection.chosen.resolution),
            min_fps: selection.chosen.fps,
            prefer_nv12: selection.chosen.is_nv12(),
            strict_format: candidates[selection.index].strict_format,
        };
        self.open_selected(device, selection.chosen, &preference)
    }

    /// 切换到精确的媒体类型（分辨率 + 帧率 + 像素格式）。
    ///
    /// 失败时不改变当前流，错误记入 [`Self::last_switch_error`]，并可通过
    /// [`Self::revert_format`] 回退到上一个成功格式。
    pub fn set_format(&mut self, format: FormatInfo) -> Result<()> {
        self.last_switch_error = None;
        #[cfg(feature = "dx")]
        if self.gpu.is_some() {
            let previous = self.current_format_info;
            let result = self
                .gpu
                .as_mut()
                .ok_or(CameraError::NotOpen)?
                .set_format(&format);
            return match result {
                Ok(()) => {
                    self.current_format_info = Some(format);
                    self.fallback_format = previous;
                    Ok(())
                }
                Err(error) => {
                    self.last_switch_error = Some(error.to_string());
                    Err(error)
                }
            };
        }
        let device = self.opened_device.clone().ok_or(CameraError::NotOpen)?;
        let preference = FormatPreference {
            resolution: Some(format.resolution),
            min_fps: format.fps,
            prefer_nv12: format.is_nv12(),
            strict_format: true,
        };
        match self.open_selected(&device, format, &preference) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.last_switch_error = Some(error.to_string());
                Err(error)
            }
        }
    }

    /// 保持当前分辨率与像素格式，仅切换帧率。
    pub fn set_frame_rate(&mut self, fps: u32) -> Result<()> {
        let current = self.current_format_info.ok_or(CameraError::NotOpen)?;
        self.set_format(FormatInfo::new(current.resolution, current.format, fps))
    }

    /// 保持当前像素格式与帧率，仅切换分辨率。
    pub fn set_resolution_keep_fps(&mut self, resolution: Resolution) -> Result<()> {
        let current = self.current_format_info.ok_or(CameraError::NotOpen)?;
        self.set_format(FormatInfo::new(resolution, current.format, current.fps))
    }

    /// 一步切换分辨率与帧率（保持当前像素格式）。
    pub fn switch_to(&mut self, resolution: Resolution, fps: u32) -> Result<()> {
        let current = self.current_format_info.ok_or(CameraError::NotOpen)?;
        self.set_format(FormatInfo::new(resolution, current.format, fps))
    }

    /// 回退到上一次成功切换前的格式（切换失败后的恢复手段）。
    pub fn revert_format(&mut self) -> Result<()> {
        let format = self.fallback_format.ok_or_else(|| {
            CameraError::CapabilityQuery("no previous format to revert to".to_owned())
        })?;
        self.set_format(format)
    }

    /// 当前精确格式（分辨率 + 帧率 + 像素格式）。
    #[must_use]
    pub const fn current_format_info(&self) -> Option<FormatInfo> {
        self.current_format_info
    }

    #[must_use]
    pub fn current_resolution(&self) -> Option<Resolution> {
        self.current_format_info.map(|format| format.resolution)
    }

    #[must_use]
    pub fn current_fps(&self) -> Option<u32> {
        self.current_format_info.map(|format| format.fps)
    }

    /// 查询设备支持的全部分辨率（临时打开设备读能力表）。
    pub fn supported_resolutions(&self) -> Result<Vec<Resolution>> {
        Ok(self
            .capabilities(self.opened_device.as_ref().ok_or(CameraError::NotOpen)?)?
            .resolutions())
    }

    /// 查询设备支持的全部帧率（跨分辨率，从高到低）。
    pub fn supported_frame_rates(&self) -> Result<Vec<u32>> {
        Ok(self
            .capabilities(self.opened_device.as_ref().ok_or(CameraError::NotOpen)?)?
            .frame_rates())
    }

    /// 查询指定分辨率下支持的帧率。
    pub fn frame_rates_for(&self, resolution: Resolution) -> Result<Vec<u32>> {
        Ok(self
            .capabilities(self.opened_device.as_ref().ok_or(CameraError::NotOpen)?)?
            .fps_for(resolution))
    }

    /// 查询完整的分辨率 × 帧率能力矩阵。
    pub fn format_matrix(&self) -> Result<Vec<crate::device::ResolutionFps>> {
        Ok(self
            .capabilities(self.opened_device.as_ref().ok_or(CameraError::NotOpen)?)?
            .matrix())
    }

    /// 启用自适应分辨率/帧率：按消费者负载自动在能力范围内切换档位。
    pub fn enable_adaptive(&mut self, policy: crate::device::AdaptivePolicy) -> Result<()> {
        #[cfg(feature = "dx")]
        let caps = if self.gpu.is_some() {
            let device = self.opened_device.as_ref().ok_or(CameraError::NotOpen)?;
            crate::dx::DxCapture::capabilities(device)?
        } else {
            let device = self.opened_device.as_ref().ok_or(CameraError::NotOpen)?;
            self.capabilities(device)?
        };
        #[cfg(not(feature = "dx"))]
        let caps = {
            let device = self.opened_device.as_ref().ok_or(CameraError::NotOpen)?;
            self.capabilities(device)?
        };
        let mut selector = crate::quality::AdaptiveSelector::new(policy);
        selector.set_formats(caps.formats().to_vec());
        if let Some(format) = self.current_format_info {
            selector.set_current(format);
        }
        self.adaptive = Some(selector);
        Ok(())
    }

    pub fn disable_adaptive(&mut self) {
        self.adaptive = None;
    }

    #[must_use]
    pub const fn adaptive(&self) -> Option<&crate::quality::AdaptiveSelector> {
        self.adaptive.as_ref()
    }

    /// 根据当前背压负载向自适应控制器喂数，必要时切换格式。
    /// 被 `set_feedback` 与取帧路径自动调用，通常无需手动调用。
    fn update_adaptive(&mut self, utilization: f32) {
        let recommended = self
            .adaptive
            .as_mut()
            .and_then(|selector| selector.update(utilization));
        let Some(recommended) = recommended else {
            return;
        };
        if Some(recommended) == self.current_format_info {
            return;
        }
        // 统一走 set_format，自动适配 CPU / GPU 两条路径。
        let _ = self.set_format(recommended);
    }

    /// 用原生 Media Foundation + D3D11 路径打开摄像头（零 CPU 拷贝的 GPU 纹理）。
    ///
    /// 与普通 [`Self::open_with`] 互斥；设备必须提供目标分辨率的原生 NV12 媒体类型，
    /// 否则直接返回错误，不会静默退回软件解码。`with_dx12 = true` 时同时创建 D3D12
    /// 设备，纹理可通过 [`crate::dx::DxFrame::import_to_dx12`] 导入为 D3D12 资源。
    #[cfg(feature = "dx")]
    pub fn open_gpu(
        &mut self,
        device: &DeviceInfo,
        preference: &FormatPreference,
        with_dx12: bool,
    ) -> Result<()> {
        self.close();
        let target = preference.resolution.unwrap_or(Resolution::new(640, 480));
        let capture = crate::dx::DxCapture::open(
            device,
            target.width,
            target.height,
            preference.min_fps,
            with_dx12,
        )?;
        self.negotiated_fps = capture.fps();
        self.gpu = Some(capture);
        self.is_open = true;
        self.native_nv12 = true;
        self.opened_device = Some(device.clone());
        self.opened_preference = *preference;
        self.current_format_info = self.gpu.as_ref().and_then(|gpu| gpu.current_format());
        Ok(())
    }

    /// 用 GPU 路径按有序候选列表打开摄像头：逐个尝试 NV12 媒体类型，第一个成功即用。
    #[cfg(feature = "dx")]
    pub fn open_gpu_candidates(
        &mut self,
        device: &DeviceInfo,
        candidates: &[crate::device::FormatCandidate],
        with_dx12: bool,
    ) -> Result<()> {
        let caps = self.capabilities(device)?;
        let selection = crate::device::select_from_candidates(caps.formats(), candidates)
            .ok_or_else(|| {
                CameraError::Dx("none of the GPU format candidates is supported".to_owned())
            })?;
        let format = selection.chosen;
        self.close();
        let capture = crate::dx::DxCapture::open_format(device, &format, with_dx12)?;
        self.negotiated_fps = capture.fps();
        self.gpu = Some(capture);
        self.is_open = true;
        self.native_nv12 = true;
        self.opened_device = Some(device.clone());
        self.opened_preference = FormatPreference::new(format.resolution).with_min_fps(format.fps);
        self.current_format_info = Some(format);
        Ok(())
    }

    /// 高分辨率静帧拍照：用独立的 `IMFCaptureEngine` photo 流，不打断当前视频流。
    /// 保存在 `path`（通常是 `.jpg`）。需要 `dx` feature。
    #[cfg(feature = "dx")]
    pub fn take_photo(
        &self,
        settings: &crate::controls::PhotoSettings,
        path: &std::path::Path,
    ) -> Result<()> {
        let device = self.opened_device.as_ref().ok_or(CameraError::NotOpen)?;
        let photo = crate::photo::DxPhotoCapture::new(device)?;
        photo.capture_to_file(settings, path)
    }

    /// 设置扩展摄像头控制（仅 GPU 路径可用，需设备支持）。
    #[cfg(feature = "dx")]
    pub fn set_extended_control(
        &self,
        control: crate::controls::ExtendedControl,
        setting: crate::controls::ExtendedControlSetting,
    ) -> Result<()> {
        self.gpu
            .as_ref()
            .ok_or(CameraError::NotOpen)?
            .set_extended_control(control, setting)
    }

    /// 原生 GPU 路径下的 D3D11/D3D12 设备（D3D12 仅在 `open_gpu(..., true)` 时存在）。
    #[cfg(feature = "dx")]
    pub fn gpu_device(&self) -> Option<&crate::dx::DxDevice> {
        self.gpu.as_ref().map(crate::dx::DxCapture::device)
    }

    /// 取一帧 GPU 纹理（仅 `open_gpu` 路径可用）。
    #[cfg(feature = "dx")]
    pub fn poll_frame_gpu(&mut self) -> Result<Option<crate::dx::DxFrame>> {
        self.gpu.as_mut().ok_or(CameraError::NotOpen)?.poll()
    }

    /// 关闭摄像头
    pub fn close(&mut self) {
        if let Some(mut camera) = self.camera.take() {
            let _ = camera.stop_stream();
        }
        #[cfg(feature = "dx")]
        {
            self.gpu = None;
        }
        self.is_open = false;
        self.native_nv12 = false;
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        self.is_open
    }

    /// 当前流是否为原生 NV12（决定上层走直传还是回退路径）。
    pub fn is_native_nv12(&self) -> bool {
        self.native_nv12
    }

    /// 当前打开的设备（若由 [`Self::open_with`] 或 [`Self::open`] 打开）。
    pub fn current_device(&self) -> Option<&DeviceInfo> {
        self.opened_device.as_ref()
    }

    /// 当前生效的格式（设备实际协商结果 + 声称帧率）。
    pub fn current_format(&self) -> Option<FormatInfo> {
        let camera = self.camera.as_ref()?;
        let resolution = camera.resolution();
        Some(FormatInfo::new(
            Resolution::new(resolution.width(), resolution.height()),
            pixel_from_frame(camera.frame_format()),
            camera.frame_rate().max(self.negotiated_fps),
        ))
    }

    /// 切换分辨率。会按新偏好**重新协商并重启视频流**（MSMF 后端后置
    /// `set_resolution` 不可靠，精确格式只能在打开时协商）。
    pub fn set_resolution(&mut self, resolution: Resolution) -> Result<()> {
        if !self.is_open {
            return Err(CameraError::NotOpen);
        }
        if resolution.width == 0 || resolution.height == 0 || !resolution.is_even() {
            return Err(CameraError::InvalidResolution(resolution));
        }
        let device = self.opened_device.clone().ok_or(CameraError::NotOpen)?;
        let mut preference = self.opened_preference;
        preference.resolution = Some(resolution);
        self.open_with(&device, &preference)
    }

    // ---- 摄像头参数控制（驱动侧，非软件画质处理） ---------------------------

    fn require_camera(&self) -> Result<&Camera> {
        self.camera.as_ref().ok_or(CameraError::NotOpen)
    }

    /// 列出设备声明支持的所有控制项及其范围/标志。
    pub fn supported_controls(&self) -> Result<Vec<ControlCapabilities>> {
        let camera = self.require_camera()?;
        let controls = camera
            .camera_controls()
            .map_err(|error| CameraError::Control(error.to_string()))?;
        Ok(controls.iter().map(caps_from_camera_control).collect())
    }

    /// 查询单个控制项；设备不支持时返回 `None`。
    pub fn control(&self, id: CameraControlId) -> Result<Option<ControlCapabilities>> {
        let Some(known) = known_from_control_id(id) else {
            return Ok(None);
        };
        let camera = self.require_camera()?;
        match camera.camera_control(known) {
            Ok(control) => Ok(Some(caps_from_camera_control(&control))),
            Err(_) => Ok(None),
        }
    }

    /// 设置一个控制值。会先按设备声明范围钳制；设备不支持时报错。
    pub fn set_control(&mut self, id: CameraControlId, value: ControlValue) -> Result<()> {
        let known = known_from_control_id(id)
            .ok_or_else(|| CameraError::Control(format!("{id} not supported by backend")))?;
        let clamped = match self.control(id)? {
            Some(caps) => {
                if !caps.is_writable() {
                    return Err(CameraError::Control(format!("{id} is read-only")));
                }
                caps.range.clamp(value)
            }
            None => value,
        };
        let setter = setter_from_value(&clamped)
            .ok_or_else(|| CameraError::Control(format!("invalid value for {id}")))?;
        let camera = self.camera.as_mut().ok_or(CameraError::NotOpen)?;
        camera
            .set_camera_control(known, setter)
            .map_err(|error| CameraError::Control(error.to_string()))
    }

    /// 切换自动/手动模式。切到手动时会以当前值重新设置（nokhwa 用值类型区分自动与手动）。
    pub fn set_auto(&mut self, id: CameraControlId, auto: bool) -> Result<()> {
        if auto {
            return self.set_control(id, ControlValue::Auto);
        }
        let caps = self
            .control(id)?
            .ok_or_else(|| CameraError::Control(format!("{id} not supported by device")))?;
        let value = caps.range.value();
        if matches!(value, ControlValue::Auto) {
            return Err(CameraError::Control(format!(
                "{id} has no manual value to apply"
            )));
        }
        self.set_control(id, value)
    }

    /// 一次性应用多项画质设置。逐项记录成功/不支持/失败，绝不因单项失败中断。
    pub fn apply_settings(&mut self, settings: &QualitySettings) -> SettingsApplyReport {
        let mut report = SettingsApplyReport::default();
        // nokhwa 0.10 的 MSMF 后端不暴露电源频率控制，只能记为不支持。
        if settings.anti_flicker.is_some() {
            report.unsupported.push(CameraControlId::AntiFlicker);
        }
        for (id, intent) in settings.entries() {
            let result = match intent {
                SettingIntent::Auto => self.set_auto(id, true),
                SettingIntent::Manual => self.set_auto(id, false),
                SettingIntent::Value(value) => self.set_control(id, value),
            };
            match result {
                Ok(()) => report.applied.push(id),
                Err(CameraError::Control(message)) if message.contains("not supported") => {
                    report.unsupported.push(id);
                }
                Err(error) => report.failed.push((id, error.to_string())),
            }
        }
        report
    }

    // ---- 取帧（保持原有逻辑不变） ------------------------------------------

    /// 【NV12 路径】取最新原始帧（零转换、零拷贝转移所有权）。
    /// 返回 (NV12 字节, 宽, 高, 源端时间戳)；布局 = Y 平面(w*h) + 交错 UV 平面(w*h/2)。
    /// 长度不符（驱动行填充等）时返回 None 并自动降级到 RGB 路径。
    pub fn poll_frame_nv12(&mut self) -> Option<(bytes::Bytes, u32, u32, Option<Duration>)> {
        let camera = self.camera.as_mut()?;
        let frame = match camera.frame() {
            Ok(frame) => frame,
            Err(error) => {
                self.stream_health.last_error = Some(error.to_string());
                self.recover_from_error(&CameraError::Stream(error.to_string()));
                return None;
            }
        };
        let w = frame.resolution().width();
        let h = frame.resolution().height();
        // **源端时间戳**：MediaFoundation 给的采集时刻。`nokhwa` 把它从
        // `raw_bytes()` 的 `capture_ts` 透传到 `Buffer::capture_timestamp()`
        // （见 nokhwa-0.10.11 `backends/capture/msmf_backend.rs`）。
        let capture_ts = frame.capture_timestamp();
        if let Some(ts) = capture_ts {
            self.note_source_period(ts);
            if self.note_frame_timestamp(ts).is_some() {
                return None;
            }
        }
        // 三重时间戳：系统到达 / 流时钟 / 曝光开始（经偏移校正 + 单调化）。
        let reference_time = std::time::Instant::now();
        let reference_duration = reference_time.duration_since(self.stream_epoch);
        let device_time = frame.capture_timestamp(); // 驱动提供的最接近曝光开始的时刻
        let system_clock = device_time.and_then(|ts| {
            // 用流时钟 + 已观测偏差近似设备曝光时的系统时钟。
            self.last_timestamps
                .and_then(|last| last.stream_to_reference_delta().checked_sub(Duration::ZERO))
                .map(|delta| ts.saturating_add(delta))
        });
        let timestamps = self.timestamp_tracker.track(
            reference_duration,
            capture_ts.unwrap_or_default(),
            device_time,
            system_clock,
        );
        self.last_timestamps = Some(timestamps);
        // 真正拿到有效帧后才确认"已启动"（Chromium 不信任 started 事件）。
        self.recovery.confirm_first_frame();

        if frame.source_frame_format() != FrameFormat::NV12 {
            // 媒体类型被静默切换：计一次丢帧并尝试有界恢复；失败则让上层走 RGB
            self.native_nv12 = false;
            self.recover_stream(capture_ts.unwrap_or_default(), DropReason::FormatChanged);
            return None;
        }
        let expected = (w as usize) * (h as usize) * 3 / 2;
        let data = frame.buffer_bytes();
        if data.len() != expected || w % 2 != 0 || h % 2 != 0 {
            log::warn!(
                "NV12 frame size mismatch (got {}, expected {}); falling back to RGB path",
                data.len(),
                expected
            );
            self.record_drop(capture_ts.unwrap_or_default(), DropReason::InvalidFormat);
            self.native_nv12 = false;
            return None;
        }
        self.frame_stats.note_delivered();
        Some((data, w, h, capture_ts))
    }

    /// 【RGB 回退路径】获取最新帧并解码为 RGB，写入可复用缓冲（避免每帧分配）。
    /// 返回 (宽, 高)，像素数据通过 [`Self::frame_rgb`] 读取。
    pub fn poll_frame(&mut self) -> Option<(u32, u32)> {
        let camera = self.camera.as_mut()?;
        let frame = match camera.frame() {
            Ok(frame) => frame,
            Err(error) => {
                self.stream_health.last_error = Some(error.to_string());
                self.recover_from_error(&CameraError::Stream(error.to_string()));
                return None;
            }
        };
        let w = frame.resolution().width();
        let h = frame.resolution().height();
        let capture_ts = frame.capture_timestamp();
        if let Some(ts) = capture_ts {
            self.note_source_period(ts);
            if self.note_frame_timestamp(ts).is_some() {
                return None;
            }
        }
        // 三重时间戳（RGB 回退路径同样跟踪，便于统一诊断）。
        let reference_duration = std::time::Instant::now().duration_since(self.stream_epoch);
        let timestamps = self.timestamp_tracker.track(
            reference_duration,
            capture_ts.unwrap_or_default(),
            capture_ts,
            None,
        );
        self.last_timestamps = Some(timestamps);
        self.recovery.confirm_first_frame();
        let needed = (w as usize) * (h as usize) * 3;
        self.decode_buf.resize(needed, 0);
        if let Err(error) = frame.decode_image_to_buffer::<RgbFormat>(&mut self.decode_buf) {
            self.record_drop(capture_ts.unwrap_or_default(), DropReason::ConversionFailed);
            log::warn!("RGB decode failed: {error}");
            return None;
        }
        self.frame_stats.note_delivered();
        Some((w, h))
    }

    /// 最近一次 [`Self::poll_frame`] 解码出的 RGB 数据。
    pub fn frame_rgb(&self) -> &[u8] {
        &self.decode_buf
    }
}

impl Default for CameraManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CameraManager {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
#[path = "tests/capture_tests.rs"]
mod tests;
