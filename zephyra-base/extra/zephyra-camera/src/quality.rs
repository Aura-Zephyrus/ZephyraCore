//! 帧统计、丢帧诊断与节拍控制。
//!
//! 算法取自 Chromium（见 README「参考 Chromium」）：
//! - 分桶 min/max 窗口帧率估计：`media/base/frame_rate_estimator.cc`
//! - EWMA + 迟滞发布：`third_party/blink/renderer/modules/mediastream/video_track_adapter.cc`
//! - 带累积漂移的丢帧节拍：同上 `MaybeDropFrame`
//! - 消费者背压信号：`media/capture/video/video_capture_feedback.h`
//!
//! 本模块零依赖且始终编译；所有算法都是纯逻辑，可无硬件单测。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::device::{select_adaptive_format, AdaptivePolicy, FormatInfo};

/// 丢帧原因。分类对齐 Chromium 的 `VideoCaptureFrameDropReason`（`video_capture_types.h:223`），
/// 按"发生在哪一层"分组：设备/缓冲池/节拍/消费者/GPU/状态机。
///
/// 索引是稳定的诊断契约（只增不改序），便于持久化统计或上报。
#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq, Ord, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DropReason {
    // ---- 设备 / 采样层 ----
    /// 帧格式无效（不是协商的像素格式）。
    InvalidFormat,
    /// 解码/转换失败。
    ConversionFailed,
    /// 媒体类型被静默切换（如 NV12 → MJPEG）。
    FormatChanged,
    /// 设备断开或流丢失。
    DeviceLost,
    /// 摄像头被其他进程占用（`MF_E_HW_MFT_FAILED_START_STREAMING`）。
    CameraBusy,
    /// 后端返回了空样本（MF `ReceivedSampleIsNull`）。
    SampleNull,
    /// 锁定媒体缓冲失败（MF `LockingBufferDeliveredNullptr`）。
    BufferLockFailed,
    /// `GetBufferByIndex` 失败。
    BufferIndexFailed,
    /// 设备时钟回退或时间戳重复。
    DuplicateTimestamp,
    /// 帧间隔异常（>1s 或 <1ms）。
    StaleTimestamp,
    // ---- 缓冲池层 ----
    /// 缓冲池耗尽（达到最大数量）。
    BufferPoolExhausted,
    /// 缓冲分配失败。
    BufferAllocationFailed,
    /// 缓冲被 `InvalidateBuffers()` 作废（换流/设备重置后旧帧失效）。
    BufferInvalidated,
    // ---- 节拍 / 分发层 ----
    /// 帧到得太密，超过目标节拍。
    RateTooHigh,
    /// 分辨率适配器拒绝该帧。
    ResolutionAdapterDeclined,
    /// 分发器丢弃（消费者未就绪）。
    SinkNotReady,
    /// 帧投递器丢弃。
    FrameDelivererDropped,
    // ---- 消费者 / GPU ----
    /// 消费者过载（背压信号）。
    ConsumerOverload,
    /// GPU 纹理映射失败。
    GpuMapFailed,
    /// D3D 设备丢失（`DXGI_ERROR_DEVICE_REMOVED`）。
    D3dDeviceRemoved,
    /// 零拷贝路径不可用，已退化到软件路径。
    ZeroCopyUnavailable,
    // ---- 状态机 ----
    /// 流尚未进入 started 状态（未收到首帧）。
    NotStarted,
    /// 后处理失败。
    PostProcessingFailed,
}

impl DropReason {
    pub const ALL: [Self; 23] = [
        Self::InvalidFormat,
        Self::ConversionFailed,
        Self::FormatChanged,
        Self::DeviceLost,
        Self::CameraBusy,
        Self::SampleNull,
        Self::BufferLockFailed,
        Self::BufferIndexFailed,
        Self::DuplicateTimestamp,
        Self::StaleTimestamp,
        Self::BufferPoolExhausted,
        Self::BufferAllocationFailed,
        Self::BufferInvalidated,
        Self::RateTooHigh,
        Self::ResolutionAdapterDeclined,
        Self::SinkNotReady,
        Self::FrameDelivererDropped,
        Self::ConsumerOverload,
        Self::GpuMapFailed,
        Self::D3dDeviceRemoved,
        Self::ZeroCopyUnavailable,
        Self::NotStarted,
        Self::PostProcessingFailed,
    ];

    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::InvalidFormat => "invalid-format",
            Self::ConversionFailed => "conversion-failed",
            Self::FormatChanged => "format-changed",
            Self::DeviceLost => "device-lost",
            Self::CameraBusy => "camera-busy",
            Self::SampleNull => "sample-null",
            Self::BufferLockFailed => "buffer-lock-failed",
            Self::BufferIndexFailed => "buffer-index-failed",
            Self::DuplicateTimestamp => "duplicate-timestamp",
            Self::StaleTimestamp => "stale-timestamp",
            Self::BufferPoolExhausted => "buffer-pool-exhausted",
            Self::BufferAllocationFailed => "buffer-allocation-failed",
            Self::BufferInvalidated => "buffer-invalidated",
            Self::RateTooHigh => "rate-too-high",
            Self::ResolutionAdapterDeclined => "resolution-adapter-declined",
            Self::SinkNotReady => "sink-not-ready",
            Self::FrameDelivererDropped => "frame-deliverer-dropped",
            Self::ConsumerOverload => "consumer-overload",
            Self::GpuMapFailed => "gpu-map-failed",
            Self::D3dDeviceRemoved => "d3d-device-removed",
            Self::ZeroCopyUnavailable => "zero-copy-unavailable",
            Self::NotStarted => "not-started",
            Self::PostProcessingFailed => "post-processing-failed",
        }
    }

    /// 该丢帧原因是否意味着流已不可用（需要触发恢复）。
    #[must_use]
    pub const fn is_fatal(self) -> bool {
        matches!(
            self,
            Self::DeviceLost
                | Self::CameraBusy
                | Self::D3dDeviceRemoved
                | Self::FormatChanged
                | Self::BufferAllocationFailed
        )
    }
}

impl std::fmt::Display for DropReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

// ---- 三重时间戳（对齐 Chromium `MFVideoCallback::OnSample`）----------------

/// 一帧的三个时间维度（对应 Chromium `media/capture/video/video_capture_device.h:186-192`）：
///
/// - `reference_time`：系统单调时钟上的**到达时刻**，用于音视频同步（有调度噪声）。
/// - `stream_timestamp`：驱动/流时钟上的时间戳（`IMFSample::GetSampleTime`，100ns 单位）。
///   可能相对系统时钟漂移，适合算帧间隔，不适合直接做 A/V 同步。
/// - `capture_begin_time`：设备**曝光开始**时刻（`MFSampleExtension_DeviceReferenceSystemTime`
///   经偏移校正后），已强制单调。用于精确测量端到端延迟。
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FrameTimestamps {
    pub reference_time: Duration,
    pub stream_timestamp: Duration,
    pub capture_begin_time: Option<Duration>,
    /// 是否发生了时间戳回退并被强制单调化。
    pub was_monotonicized: bool,
    /// 设备时钟与系统时钟之间估算的偏差（最近一次采样）。
    pub clock_offset: Option<Duration>,
}

impl FrameTimestamps {
    /// 流时钟与到达时刻之间的差（正值表示流时钟落后于系统时钟）。
    #[must_use]
    pub fn stream_to_reference_delta(&self) -> Duration {
        self.reference_time.saturating_sub(self.stream_timestamp)
    }
}

/// 时间戳跟踪器：把设备曝光时间对齐到系统时钟并强制单调。
///
/// Chromium 在 `video_capture_device_mf_win.cc:844-896` 的做法：
/// ```text
/// reference_time = TimeTicks::Now()                         // 系统到达时刻
/// mf_time_now = TimeTicks() + Microseconds(MFGetSystemTime()/10)
/// mf_time_offset = reference_time - mf_time_now             // 采样时刻偏差
/// capture_begin = Microseconds(device_reference_time/10) + mf_time_offset
/// if capture_begin <= last_capture_begin: capture_begin = last + 1µs
/// ```
#[derive(Clone, Debug, Default)]
pub struct TimestampTracker {
    last_capture_begin: Option<Duration>,
    last_stream_timestamp: Option<Duration>,
    monotonicization_count: u64,
}

impl TimestampTracker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 用三个原始时间样本生成校正后的 [`FrameTimestamps`]。
    ///
    /// `device_time` 是设备曝光开始的原始时钟值；若没有该扩展则传 `None`，
    /// 此时 `capture_begin_time` 留空。`clock_offset` 在本帧采样时计算：
    /// `reference_time - (system_clock_at_sample)`，这里用 `stream_timestamp` 附近的系统时间近似。
    pub fn track(
        &mut self,
        reference_time: Duration,
        stream_timestamp: Duration,
        device_time: Option<Duration>,
        system_clock_at_sample: Option<Duration>,
    ) -> FrameTimestamps {
        let clock_offset = system_clock_at_sample.map(|system| {
            // 系统时钟领先则 offset 为正；用 saturating_sub 避免时钟回退时 panic。
            reference_time.saturating_sub(system)
        });
        let mut capture_begin_time = device_time.map(|device| match clock_offset {
            Some(offset) => device.saturating_add(offset),
            None => device,
        });
        let mut was_monotonicized = false;
        if let (Some(current), Some(last)) = (capture_begin_time, self.last_capture_begin) {
            if current <= last {
                // 强制单调：+1µs（与 Chromium 一致），避免下游视频帧元数据时间倒流。
                capture_begin_time = Some(last + Duration::from_micros(1));
                was_monotonicized = true;
                self.monotonicization_count += 1;
            }
        }
        if let Some(current) = capture_begin_time {
            self.last_capture_begin = Some(current);
        }
        self.last_stream_timestamp = Some(stream_timestamp);
        FrameTimestamps {
            reference_time,
            stream_timestamp,
            capture_begin_time,
            was_monotonicized,
            clock_offset,
        }
    }

    #[must_use]
    pub const fn monotonicization_count(&self) -> u64 {
        self.monotonicization_count
    }

    #[must_use]
    pub const fn last_capture_begin(&self) -> Option<Duration> {
        self.last_capture_begin
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// 分桶 min/max 窗口帧率估计器（Chromium `FrameRateEstimator`）。
///
/// 最多 15 个样本，至少 3 个才出数；窗口内的最小/最大间隔若落到不同的 1fps 桶，
/// 说明源端不稳定，估计器会复位并返回 `None`——宁可不给数，也不给抖动数。
#[derive(Clone, Debug)]
pub struct FrameRateEstimator {
    samples: VecDeque<Duration>,
    bucket: Option<u32>,
}

const FRAME_RATE_MAX_SAMPLES: usize = 15;
const FRAME_RATE_MIN_SAMPLES: usize = 3;

fn fps_bucket(period: Duration) -> u32 {
    let seconds = period.as_secs_f64();
    if seconds <= 0.0 {
        return 0;
    }
    (1.0 / seconds + 0.5) as u32
}

impl FrameRateEstimator {
    #[must_use]
    pub fn new() -> Self {
        Self {
            samples: VecDeque::with_capacity(FRAME_RATE_MAX_SAMPLES),
            bucket: None,
        }
    }

    /// 记一个源端帧间隔样本，返回当前可靠帧率（不稳定时为 `None`）。
    pub fn add_sample(&mut self, period: Duration) -> Option<u32> {
        self.samples.push_back(period);
        while self.samples.len() > FRAME_RATE_MAX_SAMPLES {
            self.samples.pop_front();
        }
        if self.samples.len() < FRAME_RATE_MIN_SAMPLES {
            return None;
        }
        let min = self.samples.iter().min()?;
        let max = self.samples.iter().max()?;
        let min_bucket = fps_bucket(*min);
        let max_bucket = fps_bucket(*max);
        if min_bucket != max_bucket {
            self.bucket = None;
            return None;
        }
        self.bucket = Some(min_bucket);
        self.bucket
    }

    #[must_use]
    pub const fn fps(&self) -> Option<u32> {
        self.bucket
    }

    pub fn reset(&mut self) {
        self.samples.clear();
        self.bucket = None;
    }
}

impl Default for FrameRateEstimator {
    fn default() -> Self {
        Self::new()
    }
}

/// EWMA 帧率 + 迟滞发布（Chromium `ComputeFrameRate` / `MaybeUpdateFrameRate`）。
///
/// 每次帧时间戳更新内部估计 `fps = 0.2 × (1000/Δt) + 0.8 × fps`；只有相对已发布值
/// 偏离超过 1% **持续至少 1 秒**才发布新值，且每 5 秒强制刷新一次——避免把逐帧抖动
/// 暴露给 UI 造成数字乱跳。
#[derive(Clone, Debug)]
pub struct EwmaFrameRate {
    fps: f32,
    prev_timestamp: Option<Duration>,
    published: f32,
    published_at: Option<Duration>,
    candidate_since: Option<Duration>,
}

impl EwmaFrameRate {
    #[must_use]
    pub fn new() -> Self {
        Self {
            fps: 0.0,
            prev_timestamp: None,
            published: 0.0,
            published_at: None,
            candidate_since: None,
        }
    }

    /// 记一个源端时间戳，返回本次是否发布了新值（附最新已发布帧率）。
    pub fn update(&mut self, timestamp: Duration) -> Option<f32> {
        let Some(previous) = self.prev_timestamp else {
            self.prev_timestamp = Some(timestamp);
            return None;
        };
        self.prev_timestamp = Some(timestamp);
        if timestamp <= previous {
            return None;
        }
        let delta_ms = (timestamp - previous).as_secs_f64() * 1000.0;
        if delta_ms <= 0.0 {
            return None;
        }
        self.fps = 200.0 / delta_ms as f32 + 0.8 * self.fps;

        let deviation = if self.published <= 0.0 {
            f32::INFINITY
        } else {
            (self.fps - self.published).abs() / self.published
        };
        let forced = self
            .published_at
            .is_some_and(|at| timestamp.saturating_sub(at) >= Duration::from_secs(5));
        if deviation > 0.01 {
            self.candidate_since.get_or_insert(timestamp);
        } else {
            self.candidate_since = None;
        }
        let sustained = self
            .candidate_since
            .is_some_and(|since| timestamp.saturating_sub(since) >= Duration::from_secs(1));
        if forced || sustained {
            self.published = self.fps;
            self.published_at = Some(timestamp);
            self.candidate_since = None;
            Some(self.published)
        } else {
            None
        }
    }

    #[must_use]
    pub const fn published_fps(&self) -> f32 {
        self.published
    }

    #[must_use]
    pub const fn raw_fps(&self) -> f32 {
        self.fps
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

impl Default for EwmaFrameRate {
    fn default() -> Self {
        Self::new()
    }
}

/// 帧间隔统计快照（所有时间单位毫秒，便于序列化与日志输出）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FrameStatsSnapshot {
    pub min_period_ms: f64,
    pub median_period_ms: f64,
    pub p95_period_ms: f64,
    pub max_period_ms: f64,
    /// 平均绝对偏差（抖动，毫秒）。
    pub jitter_ms: f64,
    pub delivered: u64,
    pub dropped: u64,
    /// 源端时间戳回退/重置次数。
    pub resets: u64,
    pub sample_count: usize,
}

/// 帧间隔统计：升序滚动窗口 + 最近 16 帧时间戳环形缓冲（对齐 Chromium）。
#[derive(Clone, Debug)]
pub struct FrameStats {
    periods: VecDeque<Duration>,
    recent_timestamps: VecDeque<Duration>,
    delivered: u64,
    dropped: u64,
    resets: u64,
    last_timestamp: Option<Duration>,
}

const PERIOD_SAMPLE_LIMIT: usize = 128;
const RECENT_TIMESTAMP_LIMIT: usize = 16;

impl FrameStats {
    #[must_use]
    pub fn new() -> Self {
        Self {
            periods: VecDeque::with_capacity(PERIOD_SAMPLE_LIMIT),
            recent_timestamps: VecDeque::with_capacity(RECENT_TIMESTAMP_LIMIT),
            delivered: 0,
            dropped: 0,
            resets: 0,
            last_timestamp: None,
        }
    }

    /// 记一个源端时间戳。返回其间隔（无效样本返回 `None`）。
    pub fn note_timestamp(&mut self, timestamp: Duration) -> Option<Duration> {
        self.recent_timestamps.push_back(timestamp);
        while self.recent_timestamps.len() > RECENT_TIMESTAMP_LIMIT {
            self.recent_timestamps.pop_front();
        }
        let Some(previous) = self.last_timestamp else {
            self.last_timestamp = Some(timestamp);
            return None;
        };
        self.last_timestamp = Some(timestamp);
        if timestamp <= previous {
            // 时间戳回退（设备重置/换流）：丢弃该间隔样本，只记一次重置。
            self.resets += 1;
            return None;
        }
        let delta = timestamp - previous;
        if delta < Duration::from_millis(1) || delta > Duration::from_secs(1) {
            return None;
        }
        let at = self.periods.partition_point(|&period| period <= delta);
        self.periods.insert(at, delta);
        while self.periods.len() > PERIOD_SAMPLE_LIMIT {
            self.periods.pop_front();
        }
        Some(delta)
    }

    pub fn note_delivered(&mut self) {
        self.delivered += 1;
    }

    pub fn note_dropped(&mut self) {
        self.dropped += 1;
    }

    pub fn clear(&mut self) {
        self.periods.clear();
        self.recent_timestamps.clear();
        self.last_timestamp = None;
    }

    #[must_use]
    pub fn median_period(&self) -> Option<Duration> {
        let len = self.periods.len();
        if len == 0 {
            return None;
        }
        Some(if len.is_multiple_of(2) {
            (self.periods[len / 2 - 1] + self.periods[len / 2]) / 2
        } else {
            self.periods[len / 2]
        })
    }

    /// 分位数（0.0..=1.0），用最近秩法。
    #[must_use]
    pub fn percentile(&self, q: f64) -> Option<Duration> {
        if self.periods.is_empty() {
            return None;
        }
        let rank = ((self.periods.len() as f64 - 1.0) * q.clamp(0.0, 1.0)).round() as usize;
        self.periods.get(rank).copied()
    }

    #[must_use]
    pub fn snapshot(&self) -> FrameStatsSnapshot {
        let sample_count = self.periods.len();
        let median = self.median_period();
        let p95 = self.percentile(0.95);
        let jitter_ms = median.map_or(0.0, |median| {
            let sum: u128 = self
                .periods
                .iter()
                .map(|p| p.abs_diff(median).as_nanos())
                .sum();
            (sum as f64 / sample_count as f64) / 1_000_000.0
        });
        let ms = |period: Option<Duration>| period.map_or(0.0, |p| p.as_secs_f64() * 1000.0);
        FrameStatsSnapshot {
            min_period_ms: ms(self.periods.front().copied()),
            median_period_ms: ms(median),
            p95_period_ms: ms(p95),
            max_period_ms: ms(self.periods.back().copied()),
            jitter_ms,
            delivered: self.delivered,
            dropped: self.dropped,
            resets: self.resets,
            sample_count,
        }
    }
}

impl Default for FrameStats {
    fn default() -> Self {
        Self::new()
    }
}

/// 丢帧日志：按原因计数 + 最近 32 条事件（时间戳相对首帧，毫秒）。
#[derive(Clone, Debug)]
pub struct DropLog {
    counts: [u64; DropReason::ALL.len()],
    recent: VecDeque<(u64, DropReason)>,
    origin: Option<Duration>,
}

impl DropLog {
    #[must_use]
    pub fn new() -> Self {
        Self {
            counts: [0; DropReason::ALL.len()],
            recent: VecDeque::with_capacity(32),
            origin: None,
        }
    }

    pub fn note(&mut self, at: Duration, reason: DropReason) {
        self.origin.get_or_insert(at);
        let relative_ms = at
            .saturating_sub(self.origin.expect("origin just set"))
            .as_millis() as u64;
        self.counts[reason.index()] += 1;
        self.recent.push_back((relative_ms, reason));
        while self.recent.len() > 32 {
            self.recent.pop_front();
        }
    }

    #[must_use]
    pub fn count(&self, reason: DropReason) -> u64 {
        self.counts[reason.index()]
    }

    #[must_use]
    pub fn total(&self) -> u64 {
        self.counts.iter().sum()
    }

    pub fn recent(&self) -> impl Iterator<Item = (u64, DropReason)> + '_ {
        self.recent.iter().copied()
    }

    pub fn clear(&mut self) {
        self.counts = [0; DropReason::ALL.len()];
        self.recent.clear();
        self.origin = None;
    }
}

impl Default for DropLog {
    fn default() -> Self {
        Self::new()
    }
}

/// 视频流健康状态（供 UI 显示"摄像头已断开/已降级"）。
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StreamHealth {
    /// 最近一次错误的可读描述。
    pub last_error: Option<String>,
    /// 自动重启次数。
    pub restart_count: u32,
    /// 重启次数超限后永久退回 CPU/RGB 路径，不再尝试 NV12 直传。
    pub permanent_fallback: bool,
}

impl StreamHealth {
    #[must_use]
    pub const fn is_healthy(&self) -> bool {
        self.last_error.is_none() && self.restart_count == 0
    }

    pub fn clear_error(&mut self) {
        self.last_error = None;
    }
}

/// 节拍决策。
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PacingDecision {
    Deliver,
    Drop(DropReason),
}

/// 带累积漂移的节拍控制器（Chromium `MaybeDropFrame`）。
///
/// 容差为目标周期的 20%；`Δt < target − tolerance − drift` 时丢帧，
/// `drift` 钳制在 `target/2` 以内；负间隔或 >1s 时复位。这比"固定间隔取帧"
/// 稳定得多——不会因为设备略慢就每次都取到重复帧。
#[derive(Clone, Debug)]
pub struct PacingController {
    target: Duration,
    drift_nanos: i128,
    last_delivered: Option<Duration>,
}

impl PacingController {
    #[must_use]
    pub fn new(target: Duration) -> Self {
        Self {
            target,
            drift_nanos: 0,
            last_delivered: None,
        }
    }

    pub fn set_target(&mut self, target: Duration) {
        self.target = target;
        self.reset();
    }

    #[must_use]
    pub const fn target(&self) -> Duration {
        self.target
    }

    pub fn reset(&mut self) {
        self.drift_nanos = 0;
        self.last_delivered = None;
    }

    /// 判断一帧是否应投递。传入源端时间戳。
    pub fn should_deliver(&mut self, timestamp: Duration) -> PacingDecision {
        let Some(last) = self.last_delivered else {
            self.last_delivered = Some(timestamp);
            return PacingDecision::Deliver;
        };
        if timestamp <= last {
            // 时间戳回退：把本帧当作新基准。
            self.last_delivered = Some(timestamp);
            self.drift_nanos = 0;
            return PacingDecision::Deliver;
        }
        let delta = timestamp - last;
        if delta > Duration::from_secs(1) {
            self.last_delivered = Some(timestamp);
            self.drift_nanos = 0;
            return PacingDecision::Deliver;
        }
        let target_ns = self.target.as_nanos() as i128;
        let tolerance_ns = target_ns / 5;
        let delta_ns = delta.as_nanos() as i128;
        if delta_ns < target_ns - tolerance_ns - self.drift_nanos {
            return PacingDecision::Drop(DropReason::RateTooHigh);
        }
        self.last_delivered = Some(timestamp);
        self.drift_nanos += delta_ns - target_ns;
        let max_drift = target_ns / 2;
        if self.drift_nanos > max_drift {
            self.drift_nanos = max_drift;
        }
        PacingDecision::Deliver
    }
}

/// 令牌桶：容量 = 1.5 × 周期，用于消费者背压。
#[derive(Clone, Debug)]
pub struct TokenBucket {
    period: Duration,
    capacity_ns: u128,
    tokens_ns: u128,
    last: Option<Duration>,
}

impl TokenBucket {
    #[must_use]
    pub fn new(period: Duration) -> Self {
        let capacity = period.as_nanos().saturating_mul(3) / 2;
        Self {
            period,
            capacity_ns: capacity,
            tokens_ns: capacity,
            last: None,
        }
    }

    /// 在 `now` 时刻是否允许一帧通过。
    pub fn allow(&mut self, now: Duration) -> bool {
        if let Some(last) = self.last {
            if now > last {
                self.tokens_ns = (self.tokens_ns + (now - last).as_nanos()).min(self.capacity_ns);
            }
            self.last = Some(now);
        } else {
            self.last = Some(now);
        }
        let cost = self.period.as_nanos();
        if self.tokens_ns >= cost {
            self.tokens_ns -= cost;
            true
        } else {
            false
        }
    }

    pub fn reset(&mut self) {
        self.tokens_ns = self.capacity_ns;
        self.last = None;
    }
}

/// 消费者 → 生产者的背压信号（Chromium `VideoCaptureFeedback`）。
#[derive(Copy, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CaptureFeedback {
    /// 1.0 = 可持续的最大负载；>1 = 过载；<0 = 忽略该信号。
    pub resource_utilization: f32,
    /// 消费者要求的最大帧率。
    pub max_fps: Option<u32>,
    /// 消费者要求的最大像素数。
    pub max_pixels: Option<u64>,
    /// 关联的帧 ID（用于把反馈和具体某一帧对应起来）。
    pub frame_id: Option<u64>,
    /// 消费者要求预映射帧（如需要 CPU 访问的消费者），会开启一个时间窗口。
    pub require_mapped_frame: bool,
}

impl CaptureFeedback {
    pub fn combine(&self, other: &Self) -> Self {
        Self {
            resource_utilization: self.resource_utilization.max(other.resource_utilization),
            max_fps: match (self.max_fps, other.max_fps) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            },
            max_pixels: match (self.max_pixels, other.max_pixels) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            },
            frame_id: other.frame_id.or(self.frame_id),
            require_mapped_frame: self.require_mapped_frame || other.require_mapped_frame,
        }
    }

    /// 过载时给出建议节拍：利用率超过 1 则按比例放慢。
    #[must_use]
    pub fn suggested_period(&self, base: Duration) -> Duration {
        if self.resource_utilization <= 1.0 || self.resource_utilization < 0.0 {
            return base;
        }
        let scaled = base.as_secs_f64() * self.resource_utilization as f64;
        Duration::from_secs_f64(scaled)
    }
}

impl Default for CaptureFeedback {
    fn default() -> Self {
        Self {
            resource_utilization: 1.0,
            max_fps: None,
            max_pixels: None,
            frame_id: None,
            require_mapped_frame: false,
        }
    }
}

/// 背压反馈跟踪器：记录最新反馈，并维护 `require_mapped_frame` 的时间窗口。
///
/// Chromium 在 MF 后端收到 `require_mapped_frame` 反馈后，会在
/// `kMaxFeedbackPremappingEffectDuration`（500ms）内把缓冲标记为预映射，
/// 让消费者避免昂贵的重复映射（mf_win.cc:2317-2327）。
#[derive(Clone, Debug)]
pub struct FeedbackTracker {
    latest: CaptureFeedback,
    premapped_until: Option<Instant>,
    premapped_window: Duration,
}

impl FeedbackTracker {
    #[must_use]
    pub fn new() -> Self {
        Self {
            latest: CaptureFeedback::default(),
            premapped_until: None,
            premapped_window: Duration::from_millis(500),
        }
    }

    pub fn set_premapped_window(&mut self, window: Duration) {
        self.premapped_window = window;
    }

    /// 应用一份反馈（与已有反馈 `combine`），更新预映射窗口。
    pub fn apply(&mut self, feedback: CaptureFeedback) {
        self.latest = self.latest.combine(&feedback);
        if feedback.require_mapped_frame {
            self.premapped_until = Some(Instant::now() + self.premapped_window);
        }
    }

    #[must_use]
    pub const fn feedback(&self) -> &CaptureFeedback {
        &self.latest
    }

    /// 当前是否应提供预映射帧。
    #[must_use]
    pub fn needs_premapped(&self) -> bool {
        self.premapped_until
            .is_some_and(|until| Instant::now() < until)
    }

    pub fn reset(&mut self) {
        self.latest = CaptureFeedback::default();
        self.premapped_until = None;
    }
}

impl Default for FeedbackTracker {
    fn default() -> Self {
        Self::new()
    }
}

// ---- 有界缓冲池（对齐 Chromium `VideoCaptureBufferPool`）-------------------

/// 缓冲槽的归属状态（简化自 Chromium 的 producer-held / consumer-held / free）。
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BufferOwnership {
    Free,
    ProducerHeld,
    /// 被 N 个消费者持有（Chromium `HoldForConsumers(id, num_clients)`）。
    ConsumerHeld(u32),
}

#[derive(Clone, Debug)]
struct PoolSlot {
    id: u64,
    state: BufferOwnership,
    epoch: u64,
}

#[derive(Debug)]
struct PoolInner {
    slots: Vec<PoolSlot>,
    capacity: usize,
    next_id: u64,
    epoch: u64,
}

/// 有界帧缓冲池。只跟踪**逻辑缓冲槽的身份与归属**，不持有像素数据
/// （像素由 `bytes::Bytes` 引用计数持有）——目的是提供利用率指标、所有权生命周期
/// 与"缓冲作废"语义，对标 Chromium 的 `VideoCaptureBufferPool`。
#[derive(Clone, Debug)]
pub struct FrameBufferPool {
    inner: Arc<Mutex<PoolInner>>,
}

/// 生产者租约：drop 时自动把槽归还为 Free。
#[derive(Debug)]
pub struct ProducerLease {
    inner: Arc<Mutex<PoolInner>>,
    id: u64,
}

/// 消费者租约：drop 时减少持有计数，并触发可选的"帧释放"回调
/// （对应 Chromium `VideoFrame::AddDestructionObserver`）。
pub struct ConsumerLease {
    inner: Arc<Mutex<PoolInner>>,
    id: u64,
    on_release: Option<Box<dyn Fn(u64) + Send + Sync>>,
}

impl std::fmt::Debug for ConsumerLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsumerLease")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// 带消费者租约的帧封装：drop 时自动把"帧被消费完"的事件回报给采集端。
pub struct FrameLease<T> {
    pub payload: T,
    /// 租约字段刻意保留：其 `Drop` 实现负责在帧释放时回报消费者反馈。
    #[allow(dead_code)]
    lease: ConsumerLease,
    pub buffer_id: u64,
}

impl<T> FrameLease<T> {
    #[must_use]
    pub const fn new(payload: T, buffer_id: u64, lease: ConsumerLease) -> Self {
        Self {
            payload,
            buffer_id,
            lease,
        }
    }

    #[must_use]
    pub const fn buffer_id(&self) -> u64 {
        self.buffer_id
    }
}

impl FrameBufferPool {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PoolInner {
                slots: Vec::with_capacity(capacity),
                capacity: capacity.max(1),
                next_id: 0,
                epoch: 0,
            })),
        }
    }

    /// 为生产者保留一个槽；池满时返回 `None`（Chromium 的 `buffer_id_to_drop` 语义简化）。
    #[must_use]
    pub fn reserve(&self) -> Option<ProducerLease> {
        let mut inner = self.inner.lock().ok()?;
        let slot = inner
            .slots
            .iter_mut()
            .find(|slot| slot.state == BufferOwnership::Free);
        let id = if let Some(slot) = slot {
            slot.state = BufferOwnership::ProducerHeld;
            slot.id
        } else {
            if inner.slots.len() >= inner.capacity {
                return None;
            }
            let id = inner.next_id;
            inner.next_id += 1;
            let epoch = inner.epoch;
            inner.slots.push(PoolSlot {
                id,
                state: BufferOwnership::ProducerHeld,
                epoch,
            });
            id
        };
        Some(ProducerLease {
            inner: Arc::clone(&self.inner),
            id,
        })
    }

    /// 把生产者持有的槽转交给 N 个消费者（Chromium `HoldForConsumers`）。
    pub fn hold_for_consumers(
        &self,
        lease: ProducerLease,
        clients: u32,
        on_release: Option<Box<dyn Fn(u64) + Send + Sync>>,
    ) -> Option<ConsumerLease> {
        let id = lease.id;
        {
            let mut inner = self.inner.lock().ok()?;
            let epoch = inner.epoch;
            let slot = inner.slots.iter_mut().find(|slot| slot.id == id)?;
            if slot.state != BufferOwnership::ProducerHeld || slot.epoch != epoch {
                return None;
            }
            slot.state = BufferOwnership::ConsumerHeld(clients.max(1));
        }
        // ProducerLease 的 Drop 会尝试归还，但此时状态已是 ConsumerHeld，
        // Drop 逻辑只在 Free/ProducerHeld 时修改，故不会冲突。
        std::mem::forget(lease);
        Some(ConsumerLease {
            inner: Arc::clone(&self.inner),
            id,
            on_release,
        })
    }

    /// 当前缓冲池利用率：被持有槽数 / 容量（Chromium `GetBufferPoolUtilization`）。
    #[must_use]
    pub fn utilization(&self) -> f32 {
        let inner = self
            .inner
            .lock()
            .map(|guard| guard.slots.len())
            .unwrap_or(0);
        inner as f32 / self.capacity() as f32
    }

    #[must_use]
    pub fn capacity(&self) -> usize {
        self.inner.lock().map(|inner| inner.capacity).unwrap_or(1)
    }

    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.inner
            .lock()
            .map(|inner| inner.slots.len())
            .unwrap_or(0)
    }

    /// 作废所有缓冲（换流 / 设备重置后调用）。之后 `hold_for_consumers` 会拒绝旧槽。
    pub fn invalidate(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.epoch += 1;
            inner.slots.clear();
        }
    }
}

impl ProducerLease {
    #[must_use]
    pub const fn buffer_id(&self) -> u64 {
        self.id
    }
}

impl Drop for ProducerLease {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.inner.lock() {
            if let Some(slot) = inner.slots.iter_mut().find(|slot| slot.id == self.id) {
                if matches!(
                    slot.state,
                    BufferOwnership::ProducerHeld | BufferOwnership::Free
                ) {
                    slot.state = BufferOwnership::Free;
                }
            }
        }
    }
}

impl ConsumerLease {
    #[must_use]
    pub const fn buffer_id(&self) -> u64 {
        self.id
    }
}

impl Drop for ConsumerLease {
    fn drop(&mut self) {
        let mut should_report = false;
        if let Ok(mut inner) = self.inner.lock() {
            if let Some(slot) = inner.slots.iter_mut().find(|slot| slot.id == self.id) {
                if let BufferOwnership::ConsumerHeld(count) = slot.state {
                    match count.checked_sub(1) {
                        Some(0) => {
                            slot.state = BufferOwnership::Free;
                            should_report = true;
                        }
                        Some(remaining) => slot.state = BufferOwnership::ConsumerHeld(remaining),
                        None => slot.state = BufferOwnership::Free,
                    }
                }
            }
        }
        if should_report {
            if let Some(callback) = self.on_release.take() {
                callback(self.id);
            }
        }
    }
}

impl Default for FrameBufferPool {
    fn default() -> Self {
        Self::new(3)
    }
}

/// 自适应分辨率/帧率控制器：平滑负载 + 迟滞 + 档位选择。
///
/// 每帧喂入消费者负载（1.0 = 满负荷），累计到足够帧数且负载确实偏离时，
/// 才推荐切换到新的分辨率/帧率档位。切换本身由 `capture` 层执行。
#[derive(Clone, Debug)]
pub struct AdaptiveSelector {
    policy: AdaptivePolicy,
    /// 启用时缓存的能力表，避免每次喂负载都重新打开设备查询。
    formats: Vec<FormatInfo>,
    current: Option<FormatInfo>,
    smoothed_load: f32,
    frames: u64,
    last_change_frame: u64,
    /// 分辨率发生变化后的探索期截止帧。
    exploration_until: u64,
    /// 持续低负载的起始帧（升档证明期）。
    low_load_since: Option<u64>,
}

impl AdaptiveSelector {
    #[must_use]
    pub fn new(policy: AdaptivePolicy) -> Self {
        Self {
            policy,
            formats: Vec::new(),
            current: None,
            smoothed_load: 1.0,
            frames: 0,
            last_change_frame: 0,
            exploration_until: 0,
            low_load_since: None,
        }
    }

    #[must_use]
    pub const fn policy(&self) -> &AdaptivePolicy {
        &self.policy
    }

    #[must_use]
    pub const fn current(&self) -> Option<FormatInfo> {
        self.current
    }

    #[must_use]
    pub const fn smoothed_load(&self) -> f32 {
        self.smoothed_load
    }

    /// 设置可选档位表（启用时由设备能力表填充）。
    pub fn set_formats(&mut self, formats: Vec<FormatInfo>) {
        self.formats = formats;
    }

    #[must_use]
    pub fn formats(&self) -> &[FormatInfo] {
        &self.formats
    }

    /// 显式设置当前格式（外部切换后同步，避免下一次推荐重复切回）。
    pub fn set_current(&mut self, format: FormatInfo) {
        let resolution_changed = self
            .current
            .is_some_and(|current| current.resolution != format.resolution);
        self.current = Some(format);
        self.last_change_frame = self.frames;
        if resolution_changed {
            // 分辨率变了 → 进入探索期，期间缩短冷却以快速收敛到新档位。
            self.exploration_until = self.frames + self.policy.exploration_frames;
        }
    }

    pub fn reset(&mut self) {
        self.smoothed_load = 1.0;
        self.frames = 0;
        self.last_change_frame = 0;
        self.exploration_until = 0;
        self.low_load_since = None;
    }

    /// 是否处于分辨率变化后的探索期。
    #[must_use]
    pub const fn in_exploration(&self) -> bool {
        self.frames < self.exploration_until
    }

    /// 喂入本帧负载；返回 `Some(新格式)` 表示建议切换（调用方应执行并回调 [`Self::set_current`]）。
    pub fn update(&mut self, utilization: f32) -> Option<FormatInfo> {
        let formats = &self.formats;
        self.frames += 1;
        // 时间加权平滑：0.1 的系数约等于 10 帧窗口，抑制单帧抖动。
        self.smoothed_load = 0.9 * self.smoothed_load + 0.1 * utilization;

        // 升档需要持续低负载（Chromium 的"欠载证明期"），降档则立即响应过载。
        if self.smoothed_load <= self.policy.scale_up_below {
            self.low_load_since.get_or_insert(self.frames);
        } else {
            self.low_load_since = None;
        }

        // 探索期内冷却缩短为 1/4，允许更快地搜索新档位。
        let effective_cooldown = if self.in_exploration() {
            self.policy.cooldown_frames / 4
        } else {
            self.policy.cooldown_frames
        };
        if self.frames.saturating_sub(self.last_change_frame) < effective_cooldown {
            return None;
        }
        let recommended = select_adaptive_format(formats, &self.policy, self.smoothed_load)?;
        if self.current == Some(recommended) {
            return None;
        }
        // 升档（更高像素数）必须通过证明期；降档不受此限制。
        let is_upgrade = self
            .current
            .is_some_and(|current| recommended.resolution.pixels() > current.resolution.pixels());
        if is_upgrade {
            let sustained = self.low_load_since.is_some_and(|since| {
                self.frames.saturating_sub(since) >= self.policy.scale_up_proving_frames
            });
            if !sustained {
                return None;
            }
        }
        self.current = Some(recommended);
        self.last_change_frame = self.frames;
        self.low_load_since = None;
        Some(recommended)
    }
}

impl Default for AdaptiveSelector {
    fn default() -> Self {
        Self::new(AdaptivePolicy::default())
    }
}

#[cfg(test)]
#[path = "tests/quality_tests.rs"]
mod tests;
