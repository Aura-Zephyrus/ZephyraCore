#![deny(rustdoc::broken_intra_doc_links)]
//! `zephyra-camera`：摄像头采集与像素格式处理。
//!
//! # 为什么是**可选子包**
//!
//! 摄像头采集不是 UI 词汇。GPUI、egui、iced、Qt、Flutter 没有一个把摄像头采集
//! 放进自己的框架 —— 因为那会让每个使用者都被迫装上 `nokhwa`，而绝大多数 UI 根本
//! 不用摄像头。放进 `zephyra-core` 核心还会破坏它零依赖这条不变量，而那正是「迁移缝
//! 能被验证」的前提。
//!
//! 所以它是独立的 crate，并且**默认 feature 为空**：`cargo build` 与 `cargo test`
//! 既不编译设备代码，也不引入 `nokhwa`。要不要摄像头是应用的决定。
//!
//! # feature 与选择性编译
//!
//! | feature | 依赖 | 内容 |
//! |---|---|---|
//! | 无（默认） | `bytes` | [`pixels`]：像素格式处理；[`device`]：设备识别/分辨率×帧率矩阵/候选列表/自适应；[`quality`]：帧率估计/丢帧诊断/节拍/自适应控制器；[`controls`]：参数控制类型 |
//! | `capture` | `nokhwa` | `capture`：设备枚举、能力查询与帧采集（仅该 feature 下可用） |
//! | `serde` | `serde` | 为上述纯数据模型派生 `Serialize` / `Deserialize` |
//! | `dx` | `windows` | 原生 Media Foundation → D3D11/D3D12 零拷贝 GPU 路径（仅 Windows，含受控 unsafe） |
//!
//! # 典型用法（启用 `capture`）
//!
//! ```
//! # #[cfg(feature = "capture")] {
//! use zephyra_camera::capture::CameraManager;
//! use zephyra_camera::device::{DeviceInfo, FormatPreference, Resolution};
//!
//! let mut manager = CameraManager::new();
//! for device in manager.list_devices().unwrap() {
//!     println!("{} — {}", device.display_name(), device.unique_id());
//! }
//!
//! // 按稳定 ID 找到设备，查能力，再按 720p / 优先 NV12 打开
//! # let devices = manager.list_devices().unwrap();
//! # let device: Option<DeviceInfo> = devices.into_iter().next();
//! if let Some(device) = device {
//!     let caps = manager.capabilities(&device).unwrap();
//!     println!("支持 {} 种媒体类型，最大 {}", caps.len(), caps.max_resolution().unwrap());
//!     let preference = FormatPreference::new(Resolution::new(1280, 720));
//!     manager.open_with(&device, &preference).unwrap();
//! }
//! # }
//! ```
//!
//! [`device`] 里的类型（[`DeviceInfo`]、[`Resolution`]、[`FormatInfo`]、[`CameraError`]
//! 等）在默认构建下也可用——它们是纯数据模型与纯逻辑，不依赖任何采集后端。
//!
//! # 哪些**没有**搬过来，以及为什么
//!
//! 这一节比上面的表格更重要——它记录的是被刻意排除的部分与理由，避免后来者
//! 以为"只是还没搬"。
//!
//! - **补光灯控制（USB HID）。** 它是 Windows 专有、且针对特定厂商硬件的私有协议，
//!   既不是框架词汇也不是通用能力。放进框架等于让每个使用者都背上 `hidapi`
//!   （且非 Windows 上根本没有实现）。留在应用侧。
//!
//! - **摄像头渲染管线。** 它与笔迹渲染共用同一份每帧状态（`Globals`、
//!   `ViewTransform`、`ColorPalette`）。搬走它就得同时搬走那套共享状态，或者让
//!   本 crate 反向依赖应用仓——两个方向都比留在原地更糟。
//!
//! 真正跨包可复用的那一半（NV12 平面几何、RGB→RGBA 降采样、拍照转换）已经在
//! [`pixels`] 里，且它零依赖。所以框架侧拿到的是可复用的部分，而不是一个
//! 把硬件、渲染和应用状态捆在一起的大包。

// 绝大多数模块零 unsafe；COM/D3D11 互操作集中在 `dx` 模块内，
// 以模块级 #[allow(unsafe_code)] 显式开口，便于审计。
#![deny(unsafe_code)]

#[cfg(all(feature = "dx", not(target_os = "windows")))]
compile_error!("`dx` feature (native DX11/DX12 zero-copy upload) requires Windows");

// 像素格式处理始终可用：任何要显示图像的界面都用得上，且零依赖。
pub mod pixels;

// 设备识别 / 分辨率能力 / 选择策略 / 错误类型：纯数据模型，始终可用且零依赖。
pub mod device;
// 画质诊断（帧率估计、丢帧、节拍、背压）：纯算法，始终可用且零依赖。
pub mod quality;
// 摄像头参数控制的自有类型；实际 I/O 在 capture 模块。
pub mod controls;
// 按错误类别分流的恢复策略状态机（零依赖）。
pub mod recovery;

#[cfg(feature = "capture")]
pub mod capture;

/// 原生 Media Foundation → D3D11/D3D12 零拷贝 GPU 路径。
#[cfg(all(feature = "dx", target_os = "windows"))]
#[allow(unsafe_code)]
pub mod dx;

/// 高分辨率静帧拍照（`IMFCaptureEngine`，仅 Windows，含受控 unsafe）。
#[cfg(all(feature = "dx", target_os = "windows"))]
#[allow(unsafe_code)]
pub mod photo;

pub use controls::{
    CameraControlId, ControlCapabilities, ControlFlags, ControlRange, ControlValue,
    ExtendedControl, ExtendedControlSetting, PhotoSettings, PowerLineFrequency, QualitySettings,
    SettingsApplyReport,
};
pub use device::{
    select_adaptive_format, select_exact_format, select_format, select_format_detailed,
    select_from_candidates, select_from_preferences, AdaptivePolicy, Backend, CameraError,
    CandidateSelection, DeviceCapabilities, DeviceChanges, DeviceIndex, DeviceInfo,
    FormatCandidate, FormatInfo, FormatPreference, PixelFormat, RejectReason, Resolution,
    ResolutionFps, Result, SelectionReport,
};
pub use pixels::{ArcNv12Planes, CpuFrameProcessor, Nv12Planes};
pub use recovery::{
    action_for_error, classify_error, FailureKind, RecoveryAction, RecoveryController,
    RecoveryPolicy,
};

#[cfg(all(feature = "dx", target_os = "windows"))]
pub use dx::{DxCapture, DxDevice, DxFrame};
#[cfg(all(feature = "dx", target_os = "windows"))]
pub use photo::DxPhotoCapture;
pub use quality::{
    AdaptiveSelector, BufferOwnership, CaptureFeedback, ConsumerLease, DropLog, DropReason,
    EwmaFrameRate, FeedbackTracker, FrameBufferPool, FrameLease, FrameRateEstimator, FrameStats,
    FrameStatsSnapshot, FrameTimestamps, PacingController, PacingDecision, ProducerLease,
    StreamHealth, TimestampTracker, TokenBucket,
};
