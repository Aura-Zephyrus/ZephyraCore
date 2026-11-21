// 本文件由 quality.rs 的 #[cfg(test)] mod tests 抽出。零硬件依赖的纯算法测试。

use super::*;
use crate::device::{AdaptivePolicy, FormatInfo, PixelFormat, Resolution};

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

#[test]
fn frame_rate_estimator_requires_three_samples() {
    let mut estimator = FrameRateEstimator::new();
    assert_eq!(estimator.add_sample(ms(33)), None);
    assert_eq!(estimator.add_sample(ms(33)), None);
    assert_eq!(estimator.add_sample(ms(33)), Some(30));
    assert_eq!(estimator.fps(), Some(30));
}

#[test]
fn frame_rate_estimator_rejects_unstable_window() {
    let mut estimator = FrameRateEstimator::new();
    // 30fps 与 15fps 落在不同桶 → 估计器复位，不给数。
    assert_eq!(estimator.add_sample(ms(33)), None);
    assert_eq!(estimator.add_sample(ms(33)), None);
    assert_eq!(estimator.add_sample(ms(66)), None);
    assert_eq!(estimator.fps(), None);
    // 窗口滑过不稳定样本后恢复稳定。
    for _ in 0..15 {
        estimator.add_sample(ms(33));
    }
    assert_eq!(estimator.fps(), Some(30));
    estimator.reset();
    assert_eq!(estimator.fps(), None);
}

#[test]
fn ewma_converges_and_uses_hysteresis() {
    let mut estimator = EwmaFrameRate::new();
    let mut timestamp = Duration::ZERO;
    // 稳定 30fps：首样本只锚定，之后 5s 强制刷新出数。
    for _ in 0..200 {
        timestamp += ms(33);
        let _ = estimator.update(timestamp);
    }
    assert!(estimator.raw_fps() > 25.0 && estimator.raw_fps() < 35.0);
    assert!(estimator.published_fps() > 0.0);

    // 短促的单帧抖动不应立刻改写已发布值（1s 迟滞）。
    let before = estimator.published_fps();
    estimator.update(timestamp + Duration::from_millis(1));
    assert_eq!(estimator.published_fps(), before);
}

#[test]
fn frame_stats_reports_percentiles_and_jitter() {
    let mut stats = FrameStats::new();
    let mut timestamp = Duration::ZERO;
    let mut observed = Vec::new();
    for period in [10, 20, 30, 40, 50] {
        timestamp += ms(period);
        observed.push(stats.note_timestamp(timestamp));
    }
    // 首样本只锚定基准，之后 4 个才是有效间隔。
    assert!(observed[0].is_none());
    assert!(observed[1..].iter().all(Option::is_some));
    let snapshot = stats.snapshot();
    // 时间戳为累计值 10/30/60/100/150ms，故间隔为 20/30/40/50ms。
    assert_eq!(snapshot.sample_count, 4);
    assert_eq!(snapshot.min_period_ms, 20.0);
    assert_eq!(snapshot.median_period_ms, 35.0);
    assert_eq!(snapshot.p95_period_ms, 50.0);
    assert_eq!(snapshot.max_period_ms, 50.0);
    assert!(snapshot.jitter_ms > 0.0);
    assert_eq!(stats.note_timestamp(timestamp - ms(1)), None);
    assert_eq!(stats.snapshot().resets, 1);
}

#[test]
fn frame_stats_filters_implausible_intervals() {
    let mut stats = FrameStats::new();
    assert!(stats.note_timestamp(ms(100)).is_none());
    assert!(stats.note_timestamp(ms(100)).is_none()); // 重复时间戳
    assert!(stats.note_timestamp(ms(2_000)).is_none()); // >1s
    assert!(stats.note_timestamp(ms(2_016)).is_some()); // 16ms
    assert_eq!(stats.snapshot().sample_count, 1);
}

#[test]
fn drop_log_counts_and_keeps_recent() {
    let mut log = DropLog::new();
    assert!(log.total() == 0);
    log.note(ms(10), DropReason::InvalidFormat);
    log.note(ms(20), DropReason::InvalidFormat);
    log.note(ms(30), DropReason::RateTooHigh);
    assert_eq!(log.count(DropReason::InvalidFormat), 2);
    assert_eq!(log.count(DropReason::RateTooHigh), 1);
    assert_eq!(log.total(), 3);
    let recent: Vec<_> = log.recent().collect();
    assert_eq!(recent.last(), Some(&(20, DropReason::RateTooHigh)));
    log.clear();
    assert_eq!(log.total(), 0);
}

#[test]
fn pacing_drops_frames_that_arrive_too_fast() {
    let mut pacing = PacingController::new(ms(33));
    // 首帧直接投递
    assert_eq!(pacing.should_deliver(ms(0)), PacingDecision::Deliver);
    // 间隔 10ms < 33*0.8 ≈ 26ms → 丢
    assert_eq!(
        pacing.should_deliver(ms(10)),
        PacingDecision::Drop(DropReason::RateTooHigh)
    );
    // 30ms 在正常范围 → 投递
    assert_eq!(pacing.should_deliver(ms(40)), PacingDecision::Deliver);
}

#[test]
fn pacing_accumulates_drift_and_resets_on_long_gap() {
    let mut pacing = PacingController::new(ms(33));
    pacing.should_deliver(ms(0));
    // 连续 35ms 略慢于目标：漂移累积后，一个 25ms 的帧可能被容忍
    pacing.should_deliver(ms(35));
    pacing.should_deliver(ms(70));
    // 漂移 +4ms 放宽了阈值：25ms 的帧仍可投递。
    assert_eq!(pacing.should_deliver(ms(95)), PacingDecision::Deliver);
    // 超过 1s 的间隔应复位为投递
    pacing.should_deliver(ms(5_000));
    assert_eq!(pacing.should_deliver(ms(5_030)), PacingDecision::Deliver);
}

#[test]
fn token_bucket_enforces_average_rate() {
    let mut bucket = TokenBucket::new(ms(33));
    // 容量 49.5ms：首帧消耗 33ms 后剩 16.5ms，再等 17ms 才够第二帧。
    assert!(bucket.allow(ms(0)));
    assert!(bucket.allow(ms(17)));
    assert!(!bucket.allow(ms(32)));
    // 等到累积足够令牌
    assert!(bucket.allow(ms(80)));
    bucket.reset();
    assert!(bucket.allow(ms(100)));
}

#[test]
fn frame_rate_estimator_rejects_zero_duration() {
    let mut estimator = FrameRateEstimator::new();
    estimator.add_sample(Duration::ZERO);
    estimator.add_sample(Duration::ZERO);
    estimator.add_sample(Duration::ZERO);
    // 0 秒 → fps_bucket = 0；三个样本一致，给出 0 而不是崩溃/NaN。
    assert_eq!(estimator.fps(), Some(0));
}

#[test]
fn frame_stats_percentile_clamps_to_sample_range() {
    let mut stats = FrameStats::new();
    let mut timestamp = Duration::ZERO;
    for period in [10, 20, 30] {
        timestamp += ms(period);
        stats.note_timestamp(timestamp);
    }
    assert_eq!(stats.percentile(-1.0), stats.periods.front().copied());
    assert_eq!(stats.percentile(2.0), stats.periods.back().copied());
    // 时间戳 10/30/60ms → 间隔 20/30ms → 中位数 25ms。
    assert_eq!(stats.median_period(), Some(ms(25)));
}

#[test]
fn frame_stats_delivered_and_dropped_counters() {
    let mut stats = FrameStats::new();
    assert_eq!(stats.snapshot().delivered, 0);
    stats.note_delivered();
    stats.note_delivered();
    stats.note_dropped();
    let snapshot = stats.snapshot();
    assert_eq!(snapshot.delivered, 2);
    assert_eq!(snapshot.dropped, 1);
}

#[test]
fn ewma_ignores_non_monotonic_timestamps() {
    let mut estimator = EwmaFrameRate::new();
    estimator.update(ms(100));
    estimator.update(ms(50)); // 回退，应忽略
    assert_eq!(estimator.raw_fps(), 0.0);
    estimator.update(ms(133));
    assert!(estimator.raw_fps() > 0.0);
}

#[test]
fn pacing_respects_configured_target_changes() {
    let mut pacing = PacingController::new(ms(100));
    pacing.should_deliver(ms(0));
    // 20ms << 100ms*0.8 → 丢帧
    assert_eq!(
        pacing.should_deliver(ms(20)),
        PacingDecision::Drop(DropReason::RateTooHigh)
    );
    pacing.set_target(ms(10));
    // 新目标 10ms，20ms 间隔即可投递
    assert_eq!(pacing.should_deliver(ms(40)), PacingDecision::Deliver);
    assert_eq!(pacing.target(), ms(10));
}

#[test]
fn pacing_accepts_first_frame_and_duplicate_timestamp() {
    let mut pacing = PacingController::new(ms(33));
    assert_eq!(pacing.should_deliver(ms(100)), PacingDecision::Deliver);
    // 重复时间戳：作为新基准，投递
    assert_eq!(pacing.should_deliver(ms(100)), PacingDecision::Deliver);
}

#[test]
fn token_bucket_refills_only_when_time_advances() {
    let mut bucket = TokenBucket::new(ms(10));
    assert!(bucket.allow(ms(0)));
    assert!(!bucket.allow(ms(0))); // 同一时刻，无新令牌
    assert!(bucket.allow(ms(20))); // 累积 20ms 令牌
}

#[test]
fn drop_reason_labels_are_unique_and_stable() {
    let labels: std::collections::HashSet<_> = DropReason::ALL.iter().map(|r| r.label()).collect();
    assert_eq!(labels.len(), DropReason::ALL.len());
    assert_eq!(
        DropReason::ALL[DropReason::DeviceLost.index()],
        DropReason::DeviceLost
    );
    assert_eq!(DropReason::RateTooHigh.to_string(), "rate-too-high");
}

#[test]
fn stream_health_reports_recovery_state() {
    let mut health = StreamHealth::default();
    assert!(health.is_healthy());
    health.last_error = Some("device lost".into());
    assert!(!health.is_healthy());
    health.restart_count = 1;
    assert!(!health.is_healthy());
    health.clear_error();
    health.restart_count = 0;
    assert!(health.is_healthy());
    health.permanent_fallback = true;
    health.last_error = None;
    assert!(health.is_healthy()); // is_healthy 只看错误/重启，不看下沉状态
}

#[test]
fn timestamp_tracker_monotonicizes_device_clock() {
    let mut tracker = TimestampTracker::new();
    // capture_begin = device(40) + offset(100-30=70) = 110ms。
    let first = tracker.track(ms(100), ms(50), Some(ms(40)), Some(ms(30)));
    assert_eq!(first.capture_begin_time, Some(ms(110)));
    assert!(!first.was_monotonicized);
    assert_eq!(first.clock_offset, Some(ms(70)));
    // 设备时钟回退 → 强制 last + 1µs。
    let second = tracker.track(ms(133), ms(83), Some(ms(10)), Some(ms(63)));
    assert!(second.was_monotonicized);
    assert_eq!(
        second.capture_begin_time,
        Some(ms(110) + Duration::from_micros(1))
    );
    assert_eq!(tracker.monotonicization_count(), 1);
    // 再次回退仍单调。
    let third = tracker.track(ms(166), ms(116), Some(ms(5)), Some(ms(96)));
    assert!(third.was_monotonicized);
    assert_eq!(tracker.monotonicization_count(), 2);
    tracker.reset();
    assert_eq!(tracker.monotonicization_count(), 0);
}

#[test]
fn frame_timestamps_expose_stream_to_reference_delta() {
    let timestamps = FrameTimestamps {
        reference_time: ms(100),
        stream_timestamp: ms(80),
        ..FrameTimestamps::default()
    };
    assert_eq!(timestamps.stream_to_reference_delta(), ms(20));
}

#[test]
fn expanded_drop_reason_taxonomy_is_stable() {
    assert_eq!(DropReason::ALL.len(), 23);
    assert_eq!(DropReason::CameraBusy.index(), 4);
    assert_eq!(DropReason::NotStarted.index(), 21);
    assert_eq!(DropReason::D3dDeviceRemoved.label(), "d3d-device-removed");
    assert!(DropReason::D3dDeviceRemoved.is_fatal());
    assert!(!DropReason::RateTooHigh.is_fatal());
    let labels: std::collections::HashSet<_> = DropReason::ALL.iter().map(|r| r.label()).collect();
    assert_eq!(labels.len(), DropReason::ALL.len());
}

#[test]
fn feedback_tracker_maintains_premapped_window() {
    let mut tracker = FeedbackTracker::new();
    tracker.set_premapped_window(Duration::from_millis(10));
    assert!(!tracker.needs_premapped());
    tracker.apply(CaptureFeedback {
        require_mapped_frame: true,
        frame_id: Some(7),
        ..Default::default()
    });
    assert!(tracker.needs_premapped());
    assert_eq!(tracker.feedback().frame_id, Some(7));
    // 窗口过期后不再要求预映射。
    std::thread::sleep(Duration::from_millis(15));
    assert!(!tracker.needs_premapped());
    tracker.reset();
    assert_eq!(tracker.feedback().frame_id, None);
}

#[test]
fn frame_buffer_pool_tracks_ownership_and_utilization() {
    let pool = FrameBufferPool::new(2);
    assert_eq!(pool.utilization(), 0.0);
    let producer = pool.reserve().expect("slot 0");
    let id = producer.buffer_id();
    assert_eq!(pool.slot_count(), 1);
    assert_eq!(pool.utilization(), 0.5);

    let released = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let released_clone = std::sync::Arc::clone(&released);
    let consumer = pool
        .hold_for_consumers(
            producer,
            1,
            Some(Box::new(move |buffer_id| {
                released_clone.store(buffer_id, std::sync::atomic::Ordering::SeqCst)
            })),
        )
        .expect("handoff");
    assert_eq!(consumer.buffer_id(), id);
    drop(consumer);
    assert_eq!(released.load(std::sync::atomic::Ordering::SeqCst), id);
    assert_eq!(pool.utilization(), 0.5);
    pool.invalidate();
    assert_eq!(pool.slot_count(), 0);
    assert_eq!(pool.utilization(), 0.0);
}

#[test]
fn frame_buffer_pool_respects_capacity() {
    let pool = FrameBufferPool::new(2);
    let a = pool.reserve().expect("a");
    let b = pool.reserve().expect("b");
    assert!(pool.reserve().is_none(), "pool full");
    let ca = pool.hold_for_consumers(a, 1, None).expect("ca");
    // 两个槽分别被 ca（消费者）和 b（生产者）持有 → 池满。
    assert!(pool.reserve().is_none());
    drop(ca);
    // ca 释放后 a 的槽回到 Free，可以再次预留。
    assert!(pool.reserve().is_some());
    drop(b);
}

#[test]
fn adaptive_selector_exploration_period_shortens_cooldown() {
    let policy = AdaptivePolicy {
        min_fps: 15,
        max_fps: 30,
        min_pixels: 0,
        max_pixels: 0,
        scale_up_below: 0.5,
        scale_down_above: 1.0,
        cooldown_frames: 20,
        exploration_frames: 30,
        scale_up_proving_frames: 5,
        prefer_nv12: true,
    };
    let formats = vec![
        FormatInfo::new(Resolution::new(640, 480), PixelFormat::Nv12, 30),
        FormatInfo::new(Resolution::new(1920, 1080), PixelFormat::Nv12, 30),
    ];
    let mut selector = AdaptiveSelector::new(policy);
    selector.set_formats(formats);
    // 先在 1080p，再切到 640x480 → 分辨率变化触发探索期。
    selector.set_current(FormatInfo::new(
        Resolution::new(1920, 1080),
        PixelFormat::Nv12,
        30,
    ));
    selector.set_current(FormatInfo::new(
        Resolution::new(640, 480),
        PixelFormat::Nv12,
        30,
    ));
    assert!(selector.in_exploration());
}

#[test]
fn adaptive_selector_smooths_load_and_respects_cooldown() {
    let formats = vec![
        FormatInfo::new(Resolution::new(640, 480), PixelFormat::Nv12, 30),
        FormatInfo::new(Resolution::new(1280, 720), PixelFormat::Nv12, 30),
        FormatInfo::new(Resolution::new(1920, 1080), PixelFormat::Nv12, 30),
    ];
    let policy = AdaptivePolicy {
        min_fps: 15,
        max_fps: 30,
        min_pixels: 0,
        max_pixels: 0,
        scale_up_below: 0.5,
        scale_down_above: 1.0,
        cooldown_frames: 3,
        exploration_frames: 2,
        scale_up_proving_frames: 5,
        prefer_nv12: true,
    };
    let mut selector = AdaptiveSelector::new(policy);
    selector.set_formats(formats);
    assert!(selector.formats().len() == 3);
    assert!(selector.current().is_none());

    // 首帧过载：平滑负载从 1.0 开始，仍为 1.0；冷却期未到 → 不推荐。
    assert_eq!(selector.update(1.5), None);
    assert_eq!(selector.update(1.5), None);
    // 第 3 帧冷却期满，平滑值仍 >= 1.0 → 推荐降档。
    let recommended = selector.update(1.5).expect("recommendation");
    assert_eq!(recommended.resolution, Resolution::new(640, 480));
    assert_eq!(selector.current(), Some(recommended));
    // 立刻再次喂数：冷却期内不重复推荐。
    assert_eq!(selector.update(0.1), None);
}

#[test]
fn adaptive_selector_set_current_syncs_state() {
    let mut selector = AdaptiveSelector::default();
    let format = FormatInfo::new(Resolution::new(1280, 720), PixelFormat::Nv12, 30);
    selector.set_current(format);
    assert_eq!(selector.current(), Some(format));
    selector.reset();
    assert_eq!(selector.current(), Some(format)); // reset 不清 current
    assert_eq!(selector.smoothed_load(), 1.0);
}

#[test]
fn capture_feedback_combines_by_intersection() {
    let a = CaptureFeedback {
        resource_utilization: 0.8,
        max_fps: Some(30),
        max_pixels: Some(1_000),
        frame_id: Some(1),
        require_mapped_frame: false,
    };
    let b = CaptureFeedback {
        resource_utilization: 1.4,
        max_fps: Some(60),
        max_pixels: Some(500),
        frame_id: Some(2),
        require_mapped_frame: true,
    };
    let combined = a.combine(&b);
    assert_eq!(combined.frame_id, Some(2));
    assert!(combined.require_mapped_frame);
    assert_eq!(combined.resource_utilization, 1.4);
    assert_eq!(combined.max_fps, Some(30));
    assert_eq!(combined.max_pixels, Some(500));
    let suggested = combined.suggested_period(ms(33)).as_secs_f64();
    assert!((suggested - 0.033 * 1.4).abs() < 1e-9);
    assert_eq!(CaptureFeedback::default().suggested_period(ms(33)), ms(33));
}
