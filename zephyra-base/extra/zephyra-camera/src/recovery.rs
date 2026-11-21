//! 按错误类别分流的恢复策略状态机。
//!
//! Chromium 不会用统一的"重启 N 次"处理所有失败：`ProcessEventError()`
//! (`media/capture/video/win/video_capture_device_mf_win.cc:2574`) 按 HRESULT
//! 区分摄像头占用、D3D 设备丢失、未文档化的瞬态错误，并采用完全不同的策略：
//!
//! | 失败类别 | Chromium 策略 | 本模块对应 |
//! |---|---|---|
//! | `MF_E_INVALIDREQUEST`（瞬态） | 紧重试 200×50ms | [`RecoveryPolicy::transient_retries`] |
//! | `MF_E_HW_MFT_FAILED_START_STREAMING`（占用） | 查询活动监视器 + 500ms 默认兜底 | [`RecoveryPolicy::busy_grace`] + [`FailureKind::CameraBusy`] |
//! | `DXGI_ERROR_DEVICE_REMOVED`（GPU 丢失） | 2 次 D3D 重启后降级软件 | [`RecoveryPolicy::gpu_restarts_before_fallback`] |
//! | 设备断开 | 重建媒体源 | [`FailureKind::DeviceLost`] |
//!
//! 另外，Chromium 不信任 `MF_CAPTURE_ENGINE_PREVIEW_STARTED` 事件——摄像头被占用时
//! 它也可能触发；必须等**第一帧真正到达**才认为流已启动（`SendOnStartedIfNotYetSent()`，
//! mf_win.cc:2677）。[`RecoveryController::confirm_first_frame`] 实现这一点。

use std::time::{Duration, Instant};

use crate::device::CameraError;
use crate::quality::DropReason;

/// 失败类别（决定采用哪种恢复策略）。
#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum FailureKind {
    /// 摄像头被其他进程占用。
    CameraBusy,
    /// 设备断开 / 流丢失。
    DeviceLost,
    /// D3D/GPU 设备丢失（可先重启 GPU 路径，再降级 CPU）。
    D3dDeviceRemoved,
    /// 媒体类型被静默切换。
    FormatChanged,
    /// 未文档化的瞬态错误（如 `MF_E_INVALIDREQUEST`），适合紧重试。
    Transient,
    /// 无法分类的失败。
    Unknown,
}

/// 把 nokhwa / MF 的错误文本分类。
///
/// nokhwa 把 `NokhwaError` 转成字符串时保留了 HRESULT 名称，因此这里按名称匹配；
/// 匹配不到时归为 `Unknown`（保守，走常规重启）。
#[must_use]
pub fn classify_error(message: &str) -> FailureKind {
    let message = message.to_ascii_lowercase();
    if message.contains("hw_mft_failed_start_streaming")
        || message.contains("camera in use")
        || message.contains("busy")
    {
        FailureKind::CameraBusy
    } else if message.contains("dxgi_error_device_removed")
        || message.contains("device_removed")
        || message.contains("device removed")
    {
        FailureKind::D3dDeviceRemoved
    } else if message.contains("mf_e_invalidrequest") || message.contains("invalidrequest") {
        FailureKind::Transient
    } else if message.contains("device lost")
        || message.contains("disconnect")
        || message.contains("no device")
        || message.contains("not found")
    {
        FailureKind::DeviceLost
    } else if message.contains("format changed") || message.contains("media type") {
        FailureKind::FormatChanged
    } else {
        FailureKind::Unknown
    }
}

impl From<DropReason> for FailureKind {
    fn from(reason: DropReason) -> Self {
        match reason {
            DropReason::CameraBusy => Self::CameraBusy,
            DropReason::DeviceLost => Self::DeviceLost,
            DropReason::D3dDeviceRemoved => Self::D3dDeviceRemoved,
            DropReason::FormatChanged => Self::FormatChanged,
            DropReason::InvalidFormat | DropReason::BufferInvalidated => Self::Transient,
            _ => Self::Unknown,
        }
    }
}

/// 恢复策略参数。默认值对齐 Chromium 的量级。
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RecoveryPolicy {
    /// 单个类别允许的最大重启次数。
    pub max_restarts: u32,
    /// 瞬态错误紧重试次数（Chromium: 200）。
    pub transient_retries: u32,
    /// 瞬态重试间隔（Chromium: 50ms）。
    pub transient_delay: Duration,
    /// 摄像头占用时等待活动监视器的时间；超时按"未占用"处理（Chromium: 500ms）。
    pub busy_grace: Duration,
    /// GPU 路径重启多少次后降级到 CPU（Chromium `kMaxD3DRestarts`: 2）。
    pub gpu_restarts_before_fallback: u32,
    /// 两次重启之间的最小间隔（避免风暴）。
    pub restart_backoff: Duration,
}

impl Default for RecoveryPolicy {
    fn default() -> Self {
        Self {
            max_restarts: 3,
            transient_retries: 8, // 测试友好；生产可调到 Chromium 的 200
            transient_delay: Duration::from_millis(50),
            busy_grace: Duration::from_millis(500),
            gpu_restarts_before_fallback: 2,
            restart_backoff: Duration::from_millis(100),
        }
    }
}

/// 一次恢复决策。
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RecoveryAction {
    /// 不做任何事（冷却期内 / 无需恢复）。
    None,
    /// 重启整条采集流。
    RestartStream,
    /// 只重启 GPU 路径（保留设备源）。
    RestartGpu,
    /// 降级到 CPU/RGB 路径后重启。
    FallbackToCpu,
    /// 瞬态错误：稍后重试（调用方应 sleep `delay`）。
    RetryTransient { delay: Duration },
    /// 超过恢复次数，放弃。
    GiveUp,
}

/// 恢复状态机。
#[derive(Clone, Debug)]
pub struct RecoveryController {
    policy: RecoveryPolicy,
    restart_count: u32,
    gpu_restart_count: u32,
    transient_count: u32,
    last_attempt: Option<Instant>,
    /// 是否收到过首帧（未收到前 `is_started()` 始终为 false）。
    first_frame_confirmed: bool,
    /// 是否已永久降级到 CPU 路径。
    permanent_fallback: bool,
    /// 最近一次失败类别。
    last_kind: Option<FailureKind>,
}

impl RecoveryController {
    #[must_use]
    pub fn new(policy: RecoveryPolicy) -> Self {
        Self {
            policy,
            restart_count: 0,
            gpu_restart_count: 0,
            transient_count: 0,
            last_attempt: None,
            first_frame_confirmed: false,
            permanent_fallback: false,
            last_kind: None,
        }
    }

    #[must_use]
    pub const fn policy(&self) -> &RecoveryPolicy {
        &self.policy
    }

    #[must_use]
    pub const fn restart_count(&self) -> u32 {
        self.restart_count
    }

    #[must_use]
    pub const fn gpu_restart_count(&self) -> u32 {
        self.gpu_restart_count
    }

    #[must_use]
    pub const fn permanent_fallback(&self) -> bool {
        self.permanent_fallback
    }

    #[must_use]
    pub const fn last_kind(&self) -> Option<FailureKind> {
        self.last_kind
    }

    /// 是否真的"已启动"（Chromium：必须收到首帧）。
    #[must_use]
    pub const fn is_started(&self) -> bool {
        self.first_frame_confirmed
    }

    /// 首帧到达后调用；之后 `is_started()` 才返回 true。
    pub fn confirm_first_frame(&mut self) {
        self.first_frame_confirmed = true;
    }

    /// 重置"已启动"标志（重新打开流时）。
    pub fn reset_started(&mut self) {
        self.first_frame_confirmed = false;
    }

    /// 一次成功打开/重启后调用：清除连续失败计数，但保留累计 GPU 降级状态。
    pub fn note_success(&mut self) {
        self.restart_count = 0;
        self.transient_count = 0;
        self.last_kind = None;
        self.last_attempt = None;
    }

    /// 收到失败后决定下一步动作。
    pub fn on_failure(&mut self, kind: FailureKind) -> RecoveryAction {
        self.last_kind = Some(kind);
        // 重启风暴保护：冷却期内不重复动作。
        if let Some(last) = self.last_attempt {
            if last.elapsed() < self.policy.restart_backoff {
                return RecoveryAction::None;
            }
        }

        match kind {
            FailureKind::Transient => {
                self.transient_count += 1;
                if self.transient_count <= self.policy.transient_retries {
                    RecoveryAction::RetryTransient {
                        delay: self.policy.transient_delay,
                    }
                } else {
                    RecoveryAction::GiveUp
                }
            }
            FailureKind::CameraBusy => {
                // 占用通常是暂时的：给一个 grace 窗口，不累积永久降级。
                self.restart_count += 1;
                if self.restart_count > self.policy.max_restarts {
                    RecoveryAction::GiveUp
                } else {
                    RecoveryAction::RestartStream
                }
            }
            FailureKind::D3dDeviceRemoved => {
                self.gpu_restart_count += 1;
                self.last_attempt = Some(Instant::now());
                if self.permanent_fallback {
                    return RecoveryAction::RestartStream;
                }
                if self.gpu_restart_count > self.policy.gpu_restarts_before_fallback {
                    self.permanent_fallback = true;
                    RecoveryAction::FallbackToCpu
                } else {
                    RecoveryAction::RestartGpu
                }
            }
            FailureKind::DeviceLost | FailureKind::FormatChanged | FailureKind::Unknown => {
                if self.restart_count >= self.policy.max_restarts {
                    return RecoveryAction::GiveUp;
                }
                self.restart_count += 1;
                self.last_attempt = Some(Instant::now());
                RecoveryAction::RestartStream
            }
        }
    }

    /// 手动强制降级到 CPU（例如外部检测到 GPU 黑名单）。
    pub fn force_fallback(&mut self) {
        self.permanent_fallback = true;
    }

    /// 完全重置（换设备时）。
    pub fn reset(&mut self) {
        *self = Self::new(self.policy);
    }
}

impl Default for RecoveryController {
    fn default() -> Self {
        Self::new(RecoveryPolicy::default())
    }
}

/// 便捷函数：把 `CameraError` 转成恢复动作（不携带上下文时使用）。
#[must_use]
pub fn action_for_error(
    controller: &mut RecoveryController,
    error: &CameraError,
) -> RecoveryAction {
    let kind = match error {
        CameraError::Dx(message) => classify_error(message),
        CameraError::Open(message) | CameraError::Stream(message) => classify_error(message),
        CameraError::DeviceNotFound(_) => FailureKind::DeviceLost,
        _ => FailureKind::Unknown,
    };
    controller.on_failure(kind)
}

#[cfg(test)]
#[path = "tests/recovery_tests.rs"]
mod tests;
