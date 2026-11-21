# zephyra-camera

摄像头采集、设备识别、画面质量控制与像素格式处理。ZephyraCore 库层的可选子包。

**依赖严格**：除 `bytes` 外默认零依赖，不拉入任何设备采集库；可选 feature 按需开启。

## 默认零设备依赖

`Cargo.toml` 的 `default = []`：默认构建只含 `bytes` 支撑的像素格式处理、设备数据模型、
帧率估计/丢帧诊断/节拍控制和参数控制类型，**不编译设备代码、不拉 nokhwa**。
要不要摄像头是应用的决定：

```bash
cargo build                        # 零设备依赖（像素 + 数据模型 + 诊断算法）
cargo build --features capture     # 设备枚举与帧采集（nokhwa / UVC / Media Foundation）
cargo build --features serde       # 为纯数据模型派生 Serialize/Deserialize
cargo build --features dx          # 原生 MF → D3D11/D3D12 零拷贝 GPU 路径（仅 Windows）
cargo build --all-features
```

## 分层

| 层 | 模块 | feature | 内容 |
|---|---|---|---|
| 像素处理 | `pixels` | 无 | NV12 平面切片、RGB→RGBA、最近邻降采样、拍照转换 |
| 设备模型 | `device` | 无 | `DeviceInfo` / `Resolution` / `FormatInfo` / `DeviceCapabilities` / `CameraError`，纯函数格式选择、全量分辨率×帧率矩阵、候选列表与自适应策略 |
| 画质诊断 | `quality` | 无 | `FrameRateEstimator` / `EwmaFrameRate` / `FrameStats` / `DropLog` / `PacingController` / `CaptureFeedback` / `FrameTimestamps` / `FrameBufferPool` / `FeedbackTracker` |
| 参数控制 | `controls` | 无 | `CameraControlId` / `ControlRange` / `QualitySettings` / `SettingsApplyReport` / `ExtendedControl` / `PhotoSettings` 等类型 |
| 流恢复 | `recovery` | 无 | `RecoveryController` / `RecoveryPolicy`：错误分类、瞬态重试、GPU 有界降级、首帧确认 |
| 设备 I/O | `capture` | `capture` | `CameraManager`：枚举/识别、能力查询、NV12 直传 / RGB 回退取帧、流恢复 |
| 原生 GPU 路径 | `dx` | `dx`（含 `capture`） | `DxCapture` / `DxFrame` / `DxDevice`：MF → D3D11 纹理 → D3D12 共享资源，零 CPU 拷贝 |
| 高分辨率拍照 | `photo` | `dx`（含 `capture`） | `DxPhotoCapture`：独立 `IMFCaptureEngine` photo 流，不打断视频 |

公共 API 不出现 nokhwa 类型：`capture` 把 `CameraInfo` / `CameraFormat` / `CameraControl`
转换成自有类型后交出。更换后端不会影响调用方。

## 画质设计（参考 Chromium）

Chrome 的"画质"主要不靠软件滤镜，而是四件事。本 crate 按同样思路实现：

| Chrome 机制 | 本 crate 对应 |
|---|---|
| `capability_list_win.cc` 的格式比较：NV12 精确匹配、按 \|Δ高\|→\|Δ宽\|→\|Δ帧率\| 排序、避免转换 | `FormatPreference::strict_nv12()` + `select_format_detailed()` 的选择报告 |
| `video_capture_device_mf_win.cc` 的驱动侧 3A（WB/曝光/对焦/亮度/对比度/锐度/增益/PTZ） | `CameraManager::apply_settings(&QualitySettings)`，逐项报告支持情况 |
| `frame_rate_estimator.cc` 的分桶 min/max 窗口 + `video_track_adapter.cc` 的 EWMA/迟滞 | `FrameRateEstimator::fps()`（不稳定返回 `None`）+ `smoothed_fps()`（1%/1s 迟滞、5s 强制刷新） |
| `MaybeDropFrame` 的 20% 容差 + 累积漂移、`VideoCaptureFrameDropReason` | `PacingController` + `DropReason` + `DropLog` |
| 有界重启后退回软件路径（`kMaxD3DRestarts`） | `StreamHealth` + `restart_stream()`，超限后永久退化到 RGB/CPU 路径 |

## 原生 DX11/DX12 零拷贝上传（`dx` feature，仅 Windows）

nokhwa 的 MSMF 后端用 `IMFMediaBuffer::Lock()` 把帧锁进 CPU 内存，无法提供 GPU 纹理。
`dx` feature 自建 Media Foundation 管线，照 Chromium 的方式取原生纹理：

```text
MF 摄像头 → IMFSample → IMFDXGIBuffer::GetResource → ID3D11Texture2D
                                                    ├─ D3D11 直接使用（Y=子资源0，UV=子资源1）
                                                    └─ IDXGIResource1::CreateSharedHandle
                                                         → ID3D12Device::OpenSharedHandle
                                                         → ID3D12Resource（跨 API 零拷贝）
```

```rust,no_run
# #[cfg(all(feature = "dx", target_os = "windows"))]
# fn main() -> Result<(), zephyra_camera::CameraError> {
use zephyra_camera::capture::CameraManager;
use zephyra_camera::device::{FormatPreference, Resolution};

let mut manager = CameraManager::new();
let device = manager.list_devices()?.into_iter().next().expect("camera");
let preference = FormatPreference::new(Resolution::new(1280, 720)).strict_nv12();

// 原生 GPU 路径：设备必须提供该分辨率的 NV12 媒体类型，否则报错（不静默降级）
manager.open_gpu(&device, &preference, true)?; // true = 同时创建 D3D12 设备
while let Some(frame) = manager.poll_frame_gpu()? {
    let _d3d11_texture = frame.texture();                 // 直接给 D3D11 渲染管线
    let _d3d12_resource = frame.import_to_dx12(
        manager.gpu_device().expect("D3D12 device"),
    )?;                                                    // 或导入 D3D12（无像素拷贝）
}
# Ok(())
# }
# #[cfg(not(all(feature = "dx", target_os = "windows")))]
# fn main() {}
```

要点与限制：

- 设备侧要求原生 NV12 媒体类型（会枚举 `MF_MT_SUBTYPE == MFVideoFormat_NV12` 并按分辨率/帧率挑选）。
  不支持时 `open_gpu` 返回 `CameraError::Dx`，不会偷偷退回 CPU 解码。
- D3D11 设备创建使用 `D3D11_CREATE_DEVICE_VIDEO_SUPPORT | BGRA_SUPPORT`，并通过
  `IMFDXGIDeviceManager::ResetDevice` 注册给 MF；开启 `ID3D11Multithread` 保护，与 Chromium 一致。
- D3D12 导入依赖共享 NT 句柄。若驱动创建的相机纹理没有 `D3D11_RESOURCE_MISC_SHARED_NTHANDLE`，
  `CreateSharedHandle` / `OpenSharedHandle` 会失败并明确报错——这是驱动限制，不是本库的降级路径。
- `dx` 与 `photo` 是 crate 内两处允许 `unsafe` 的模块（COM 互操作），均以模块级
  `#[allow(unsafe_code)]` 显式开口；crate 根仍是 `#![deny(unsafe_code)]`。
- 未连接摄像头时无法做运行时验证；已通过 `Windows` 目标下的编译、clippy 与全部单测。

刻意**不做**：软件降噪/美化/HDR/背景虚化/点击对焦——这些依赖设备 HAL 或重型推理，
不是通用 Rust 层能可靠提供的。防闪烁（50/60Hz）虽在 `QualitySettings` 中保留字段，
但 nokhwa 0.10 不暴露电源频率控制，应用时会记为"不支持"（API 不会因此报错）。

同样地，Chrome 的"假 NV12"（full-range MJPEG 伪装）检测依赖 MF 的 nominal range 属性，
nokhwa 不暴露。替代方案是 `strict_nv12()`：要求精确 NV12，若流中媒体类型静默切换则
计一次 `FormatChanged` 丢帧并尝试有界恢复。

## 用法

### 设备识别与热插拔

```rust,no_run
use zephyra_camera::capture::CameraManager;
use zephyra_camera::{Backend, CameraError};

fn main() -> Result<(), CameraError> {
    let mut manager = CameraManager::new();
    let devices = manager.list_devices_with_backend(Backend::Auto)?;
    for device in &devices {
        println!("{} [{}] id={}", device.display_name(), device.index(), device.unique_id());
    }

    // 唯一 ID（MSMF 上是设备符号链接）比数字索引稳定，适合存进配置
    if let Some(device) = manager.find_by_id(r"\\?\USB#VID_1234")? {
        println!("找到 {}", device.name());
    }

    let changes = manager.refresh_devices()?; // added / removed
    println!("变化: {}", changes.len());
    Ok(())
}
```

### 分辨率、格式选择与画质

```rust,no_run
use zephyra_camera::capture::CameraManager;
use zephyra_camera::device::{select_format_detailed, FormatPreference, Resolution};
use zephyra_camera::CameraError;

fn main() -> Result<(), CameraError> {
    let mut manager = CameraManager::new();
    let device = manager.list_devices()?.into_iter().next().expect("camera");

    // 必须临时打开设备才能读媒体类型表（nokhwa 限制），读完立即释放
    let caps = manager.capabilities(&device)?;
    let preference = FormatPreference::new(Resolution::new(1280, 720)).strict_nv12();
    let report = select_format_detailed(caps.formats(), &preference).expect("format");
    println!("选 {}，理由 {}，排除 {} 项", report.chosen, report.reason, report.rejected.len());

    // 打开后调整驱动侧画质（设备不支持的项会在报告里列出）
    manager.open_with(&device, &preference)?;
    let applied = manager.apply_settings(&zephyra_camera::QualitySettings::auto_3a());
    println!("成功 {} 项 / 不支持 {} 项 / 失败 {} 项",
        applied.applied_count(), applied.unsupported.len(), applied.failed.len());

    // 每帧取帧后可读统计
    manager.poll_frame_nv12();
    println!("可靠帧率 {:?}，展示帧率 {:.1}，总丢帧 {}",
        manager.reliable_fps(), manager.smoothed_fps(), manager.total_drops());
    println!("统计 {:#?}", manager.frame_stats());
    Ok(())
}
```

`select_format` 中的分辨率是**偏好**而非硬约束（不支持时回退到最大分辨率）；严格匹配用
`select_exact_format`。所有可能失败的操作返回 `CameraError`，不再用裸 `String`。

### 多分辨率与多帧率

```rust,no_run
# #[cfg(feature = "capture")]
# fn main() -> Result<(), zephyra_camera::CameraError> {
use zephyra_camera::capture::CameraManager;
use zephyra_camera::{
    AdaptivePolicy, FormatCandidate, FormatPreference, Resolution, select_from_candidates,
};

let mut manager = CameraManager::new();
let device = manager.list_devices()?.into_iter().next().expect("camera");

// 1) 全量矩阵：每个分辨率下支持哪些帧率/格式
for row in manager.format_matrix()? {
    println!("{} → {:?}", row.resolution, row.fps);
}
println!("帧率范围 {:?}", manager.supported_frame_rates()?);
println!("720p 可用帧率 {:?}", manager.frame_rates_for(Resolution::new(1280, 720))?);

// 2) 有序候选：分辨率是硬约束，不支持就尝试下一项
let candidates = [
    FormatCandidate::new(Resolution::new(1920, 1080), Some(30)).strict_nv12(),
    FormatCandidate::new(Resolution::new(1280, 720), Some(60)),
    FormatCandidate::any(),
];
manager.open_candidates(&device, &candidates)?;
println!("当前 {} @ {} fps", manager.current_resolution().unwrap(), manager.current_fps().unwrap());

// 3) 运行时切换：分辨率 / 帧率 / 精确格式，失败时自动记录 last_switch_error
manager.switch_to(Resolution::new(640, 480), 30)?;
manager.set_frame_rate(60)?;
if manager.last_switch_error.is_some() {
    manager.revert_format()?; // 回退到上一个成功格式
}

// 4) 自适应：按消费者负载自动升降档（set_feedback 时自动触发）
manager.enable_adaptive(AdaptivePolicy::default())?;
manager.set_feedback(zephyra_camera::CaptureFeedback {
    resource_utilization: 1.2, // 过载 → 降档
    ..Default::default()
});
# Ok(())
# }
# #[cfg(not(feature = "capture"))]
# fn main() {}
```

- `FormatCandidate` 列表里的分辨率是**硬约束**；单个 `FormatPreference` 的分辨率只是偏好。
- `set_format` / `set_frame_rate` / `switch_to` 会重启流以精确协商（与 MSMF 的限制一致）。
- GPU（`dx`）路径同样支持候选列表与运行时切换：`open_gpu_candidates` / `DxCapture::set_format`。

## 与 Chrome 对齐的健壮性 / 诊断能力

参考 Chromium 的 `media/capture/video/win/video_capture_device_mf_win.cc` 补齐了四类差距：

| Chrome 机制 | 本 crate 对应 |
|---|---|
| `ProcessEventError()` 按 HRESULT 分类：占用 / 设备丢失 / GPU 丢失 / 瞬态错误各有策略；`kMaxD3DRestarts` 后降级软件 | `RecoveryController` + `RecoveryPolicy`：瞬态紧重试、占用 grace 窗口、GPU 有界重启后 CPU 降级；首帧到达才 `is_stream_started()` |
| 三重时间戳：`reference_time` / `stream_timestamp` / `MFSampleExtension_DeviceReferenceSystemTime` + 单调化 | `FrameTimestamps` + `TimestampTracker`（设备时钟回退时 +1µs 强制单调） |
| 34 种 `VideoCaptureFrameDropReason` UMA 分类 | `DropReason` 扩到 23 种（设备/缓冲池/节拍/消费者/GPU/状态机六组），`is_fatal()` 标记需要触发恢复的类别 |
| 有界缓冲池 + `VideoFrame::AddDestructionObserver` 反馈闭环 + `require_mapped_frame` 时间窗口 | `FrameBufferPool`（归属状态 + `Arc` drop 回调）+ `FeedbackTracker`（500ms 预映射窗口）+ `CaptureFeedback::combine()` |
| D3D12 跨设备完成屏障 `IDXGIDevice2::EnqueueSetEvent` + `Wait`、共享纹理标志校验 | `DxFrame::wait_for_gpu_completion()` / `is_cross_api_shareable()`，`import_to_dx12()` 自动做屏障 |
| `IMFCaptureEngine` 的独立 photo stream 高分辨率静帧 | `DxPhotoCapture::capture_to_file()`，不打断视频流 |
| `IMFExtendedCameraController` 的背景虚化 / 眼神矫正 / 人脸取景 | `ExtendedControl` + `DxCapture::set_extended_control()` |
| `VideoCaptureOracle` 的探索期 + 持续欠载证明期（只借通用启发式，不移植屏幕捕获逻辑） | `AdaptivePolicy::exploration_frames` / `scale_up_proving_frames` |

明确**不移植**：ChromeOS HAL/Mojo IPC、UMA 遥测传输、WinRT AQS 枚举、
`VideoCaptureOracle` 整套屏幕捕获状态机、跨平台 V4L2/AVFoundation 后端、EXIF 写入/快门声。

**当前无消费者**：本 crate 已独立成仓，但主仓尚未接入——所有能力通过公共 API 提供，
没有桥接到渲染管线（`Nv12Planes` / `DxFrame::texture()` 的 NV12 格式与 D3D11 importer
兼容，但接线与真机验证仍待应用层完成；D3D12/Vulkan 没有 CPU 视频平面导入路径）。

## 门禁

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --all-features -- -D warnings
cargo test                         # 单元 90 + 集成 7 + doctest 1，全通过
cargo test --all-features          # 单元 104 + 集成 10 + doctest 1，全通过
cargo doc --no-deps
```

> 以上计数实测于 **2026-11-21**（Windows / 本机）：命令 `cargo test -- --list` 与
> `cargo test --all-features -- --list` 逐条列举，另用 `grep -c '#\[test\]'` 逐文件复核；
> 上述两套 `cargo test` 命令均 0 failed / 0 ignored。

测试分两类：

**单元测试**位于 `src/tests/`，由各模块的 `#[cfg(test)] #[path]` 挂入：

| 文件 | 条数 | 何时运行 |
|---|---|---|
| `pixels_tests.rs` | 15 | 两套命令都跑 |
| `pixels_bridge_tests.rs` | 4 | 两套命令都跑 |
| `device_tests.rs` | 25 | 两套命令都跑 |
| `quality_tests.rs` | 28 | 两套命令都跑 |
| `controls_tests.rs` | 10 | 两套命令都跑 |
| `recovery_tests.rs` | 8 | 两套命令都跑 |
| `capture_tests.rs` | 11 | 仅 `capture` / `dx` feature |
| `dx.rs` 内联测试 | 2 | 仅 `dx` feature（Windows） |
| `photo.rs` 内联测试 | 1 | 仅 `dx` feature（Windows） |

默认单元 90 条、all-features 单元 104 条（同上命令，2026-11-21 实测）。

**集成测试**位于 `tests/public_api.rs`，只通过 crate 根的重导出访问 API（模拟外部使用者），
7 条默认运行；`serde` feature 下额外 3 条真实 JSON 往返测试（用 dev-dependency `serde_json`），
合计 10 条。另有 1 条 doctest（`lib.rs` 示例）。数量少了说明搬运或挂载漏了文件。

## 像素数据出口与缓冲策略

- `Nv12Planes` 保持 `bytes::Bytes`（采集路径零拷贝切片）；跨 crate 交给主仓
  `CpuPlanes`（`Arc<[u8]>`）时调 `to_arc_planes()` / `into_arc_planes()`，
  每个平面至多一次拷贝，`Vec → Arc<[u8]>` 不另分配。camera 不因此引入任何依赖。
- `CpuFrameProcessor` 有三种出口：`process()` 交出 `Vec` 所有权（旧语义，每帧分配）；
  `process_into(..., &mut Vec<u8>)` 复用调用方缓冲容量；`process_borrowed(...)`
  借出内部缓冲，预热后零分配零拷贝，适合同步纹理上传。
  降采样索引表在尺寸不变时始终复用。

## 刻意排除的部分

- **补光灯控制（USB HID）**：Windows 专有、特定厂商私有协议，不是通用 UI 能力。
- **摄像头渲染管线**：与应用的每帧共享状态（`Globals` / `ViewTransform` /
  `ColorPalette`）耦合，搬走要么同时搬那套状态，要么反向依赖应用仓。

跨包可复用的部分（NV12 平面几何、降采样、拍照转换、设备识别、分辨率能力、画质诊断）
都在这里，默认零依赖。
