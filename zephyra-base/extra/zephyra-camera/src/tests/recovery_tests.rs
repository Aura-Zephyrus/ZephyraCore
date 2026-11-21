// 本文件由 recovery.rs 的 #[cfg(test)] mod tests 抽出。

use super::*;

#[test]
fn classify_mf_error_messages() {
    assert_eq!(
        classify_error("MF_E_HW_MFT_FAILED_START_STREAMING: camera in use"),
        FailureKind::CameraBusy
    );
    assert_eq!(
        classify_error("DXGI_ERROR_DEVICE_REMOVED"),
        FailureKind::D3dDeviceRemoved
    );
    assert_eq!(
        classify_error("MF_E_INVALIDREQUEST"),
        FailureKind::Transient
    );
    assert_eq!(classify_error("device lost"), FailureKind::DeviceLost);
    assert_eq!(
        classify_error("media type changed"),
        FailureKind::FormatChanged
    );
    assert_eq!(classify_error("something odd"), FailureKind::Unknown);
}

#[test]
fn transient_errors_retry_within_budget() {
    let policy = RecoveryPolicy {
        transient_retries: 2,
        restart_backoff: Duration::ZERO,
        ..RecoveryPolicy::default()
    };
    let mut controller = RecoveryController::new(policy);
    assert_eq!(
        controller.on_failure(FailureKind::Transient),
        RecoveryAction::RetryTransient {
            delay: Duration::from_millis(50)
        }
    );
    assert!(matches!(
        controller.on_failure(FailureKind::Transient),
        RecoveryAction::RetryTransient { .. }
    ));
    assert_eq!(
        controller.on_failure(FailureKind::Transient),
        RecoveryAction::GiveUp
    );
}

#[test]
fn d3d_loss_falls_back_after_bounded_gpu_restarts() {
    let policy = RecoveryPolicy {
        gpu_restarts_before_fallback: 2,
        restart_backoff: Duration::ZERO,
        ..RecoveryPolicy::default()
    };
    let mut controller = RecoveryController::new(policy);
    assert_eq!(
        controller.on_failure(FailureKind::D3dDeviceRemoved),
        RecoveryAction::RestartGpu
    );
    assert!(!controller.permanent_fallback());
    // 第 2 次仍是 GPU 重启（共 2 次，与 Chromium kMaxD3DRestarts 一致）。
    assert_eq!(
        controller.on_failure(FailureKind::D3dDeviceRemoved),
        RecoveryAction::RestartGpu
    );
    assert!(!controller.permanent_fallback());
    // 第 3 次才降级到软件路径。
    assert_eq!(
        controller.on_failure(FailureKind::D3dDeviceRemoved),
        RecoveryAction::FallbackToCpu
    );
    assert!(controller.permanent_fallback());
    // 降级后再次 GPU 丢失直接重启流，不再尝试 GPU。
    assert_eq!(
        controller.on_failure(FailureKind::D3dDeviceRemoved),
        RecoveryAction::RestartStream
    );
}

#[test]
fn first_frame_confirmation_gates_started_state() {
    let mut controller = RecoveryController::default();
    assert!(!controller.is_started());
    controller.confirm_first_frame();
    assert!(controller.is_started());
    controller.reset_started();
    assert!(!controller.is_started());
}

#[test]
fn restart_budget_and_success_reset() {
    let policy = RecoveryPolicy {
        max_restarts: 2,
        restart_backoff: Duration::ZERO,
        ..RecoveryPolicy::default()
    };
    let mut controller = RecoveryController::new(policy);
    assert_eq!(
        controller.on_failure(FailureKind::DeviceLost),
        RecoveryAction::RestartStream
    );
    assert_eq!(
        controller.on_failure(FailureKind::DeviceLost),
        RecoveryAction::RestartStream
    );
    assert_eq!(
        controller.on_failure(FailureKind::DeviceLost),
        RecoveryAction::GiveUp
    );
    controller.note_success();
    assert_eq!(controller.restart_count(), 0);
    assert_eq!(
        controller.on_failure(FailureKind::DeviceLost),
        RecoveryAction::RestartStream
    );
}

#[test]
fn backoff_suppresses_restart_storm() {
    let policy = RecoveryPolicy {
        restart_backoff: Duration::from_secs(60),
        ..RecoveryPolicy::default()
    };
    let mut controller = RecoveryController::new(policy);
    assert_eq!(
        controller.on_failure(FailureKind::DeviceLost),
        RecoveryAction::RestartStream
    );
    assert_eq!(
        controller.on_failure(FailureKind::DeviceLost),
        RecoveryAction::None
    );
}

#[test]
fn drop_reason_maps_to_failure_kind() {
    assert_eq!(
        FailureKind::from(DropReason::CameraBusy),
        FailureKind::CameraBusy
    );
    assert_eq!(
        FailureKind::from(DropReason::D3dDeviceRemoved),
        FailureKind::D3dDeviceRemoved
    );
    assert_eq!(
        FailureKind::from(DropReason::RateTooHigh),
        FailureKind::Unknown
    );
}

#[test]
fn action_for_error_classifies_camera_error() {
    let mut controller = RecoveryController::new(RecoveryPolicy {
        restart_backoff: Duration::ZERO,
        ..RecoveryPolicy::default()
    });
    let error = CameraError::Open("MF_E_HW_MFT_FAILED_START_STREAMING".to_owned());
    assert_eq!(
        action_for_error(&mut controller, &error),
        RecoveryAction::RestartStream
    );
    assert_eq!(controller.last_kind(), Some(FailureKind::CameraBusy));
}
