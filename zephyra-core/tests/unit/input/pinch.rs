// 本文件由 input/pinch.rs 的 #[cfg(test)] mod tests 抽出 —— 测试代码与生产代码分离。
// 模块路径不变（src 里的声明用 #[path] 指过来），所以 use super::* 与私有可见性都照旧。
// 这里直接测 PinchState 的纯几何与状态机，recognizer.rs 只做端到端间接覆盖。

use super::*;
use crate::input::pointer::PointerTable;

// ─────────────────────── start_pinch：几何与重置 ───────────────────────

/// p0=(40,50)、p1=(100,130) ⇒ dx=60、dy=80、距离 100，中点 (70,90)。
/// 用整数勾股数保证所有期望值都是字面量，不受浮点误差影响。
#[test]
fn start_pinch_records_distance_midpoint_and_defaults() {
    let mut s = PinchState::new();
    s.start_pinch((40.0, 50.0), (100.0, 130.0));

    assert!(s.is_pinching());
    assert_eq!(s.ids, [NO_ID, NO_ID], "ids 由 on_down/on_up 负责");
    assert_eq!(s.initial_distance, 100.0);
    assert_eq!(s.prev_distance, 100.0);
    assert_eq!(s.start_mid, (70.0, 90.0));
    assert_eq!(s.tolerance_sq, 64.0);
    assert!(!s.beyond_tolerance);
    assert_eq!(s.moved, [false, false]);
}

/// 第二次 start_pinch 必须把上一次的激活/批次门控状态全部清掉。
/// p0=(0,0)、p1=(30,40) ⇒ 距离 50，中点 (15,20)。
#[test]
fn start_pinch_resets_all_transient_state() {
    let mut s = PinchState::new();
    s.start_pinch((40.0, 50.0), (100.0, 130.0));
    s.ids = [1, 2];
    s.prev_distance = 321.0;
    s.start_mid = (1.0, 2.0);
    s.beyond_tolerance = true;
    s.moved = [true, true];

    s.start_pinch((0.0, 0.0), (30.0, 40.0));

    assert!(s.is_pinching());
    assert_eq!(s.ids, [1, 2], "start_pinch 不改 ids");
    assert_eq!(s.initial_distance, 50.0);
    assert_eq!(s.prev_distance, 50.0);
    assert_eq!(s.start_mid, (15.0, 20.0));
    assert_eq!(s.tolerance_sq, 64.0);
    assert!(!s.beyond_tolerance);
    assert_eq!(s.moved, [false, false]);
}

/// 两指同点是除零退化路径：参考距离必须为 0，且仍保持激活态。
#[test]
fn start_pinch_with_coincident_points_has_zero_reference() {
    let mut s = PinchState::new();
    s.start_pinch((100.0, 100.0), (100.0, 100.0));

    assert!(s.is_pinching());
    assert_eq!(s.initial_distance, 0.0);
    assert_eq!(s.prev_distance, 0.0);
    assert_eq!(s.start_mid, (100.0, 100.0));
    assert_eq!(s.tolerance_sq, 64.0);
    assert!(!s.beyond_tolerance);
    assert_eq!(s.moved, [false, false]);
}

// ─────────────────────── finish：结束会话 ───────────────────────

/// finish 清掉 is_pinching / ids / moved，几何状态保持原值（供调试与重入）。
#[test]
fn finish_clears_active_session_but_preserves_geometry() {
    let mut s = PinchState::new();
    s.ids = [7, 9];
    s.start_pinch((40.0, 50.0), (100.0, 130.0));
    s.moved = [true, false];
    s.beyond_tolerance = true;
    s.prev_distance = 250.0;

    s.finish();

    assert!(!s.is_pinching());
    assert_eq!(s.ids, [NO_ID, NO_ID]);
    assert_eq!(s.moved, [false, false]);
    assert_eq!(s.initial_distance, 100.0);
    assert_eq!(s.prev_distance, 250.0);
    assert_eq!(s.start_mid, (70.0, 90.0));
    assert_eq!(s.tolerance_sq, 64.0);
    assert!(s.beyond_tolerance);
}

/// 未激活时 finish 必须是 no-op；连续 finish 也不能改动任何字段。
#[test]
fn finish_is_a_noop_on_fresh_state_and_is_idempotent() {
    let mut s = PinchState::new();
    s.finish();

    assert!(!s.is_pinching());
    assert_eq!(s.ids, [NO_ID, NO_ID]);
    assert_eq!(s.initial_distance, 0.0);
    assert_eq!(s.prev_distance, 0.0);
    assert_eq!(s.start_mid, (0.0, 0.0));
    assert_eq!(s.tolerance_sq, 0.0);
    assert!(!s.beyond_tolerance);
    assert_eq!(s.moved, [false, false]);

    s.start_pinch((0.0, 0.0), (30.0, 40.0));
    s.finish();
    s.finish();
    assert!(!s.is_pinching());
    assert_eq!(s.ids, [NO_ID, NO_ID]);
    assert_eq!(s.moved, [false, false]);
    assert_eq!(s.initial_distance, 50.0);
    assert_eq!(s.start_mid, (15.0, 20.0));
}

// ─────────────────────── 与 PointerTable 交互时的直接状态 ───────────────────────

/// on_down 记录表中前两指的 id 与几何，并返回 scale=1 的锚点；重复 down 不改状态。
#[test]
fn on_down_records_ids_and_anchor_geometry() {
    let mut table = PointerTable::new();
    table.down(7, 10.0, 20.0);
    table.down(9, 70.0, 100.0); // dx=60, dy=80 ⇒ 距离 100，中点 (40,60)
    let mut s = PinchState::new();

    let d = s.on_down(&table).expect("两指按下应激活");
    assert_eq!((d.scale, d.center_x, d.center_y), (1.0, 40.0, 60.0));
    assert_eq!(s.ids, [7, 9]);
    assert_eq!(s.start_mid, (40.0, 60.0));
    assert_eq!(s.initial_distance, 100.0);
    assert_eq!(s.prev_distance, 100.0);
    assert!(s.is_pinching());

    assert!(s.on_down(&table).is_none(), "已捏合时不得重复激活");
    assert_eq!(s.ids, [7, 9]);
    assert_eq!(s.start_mid, (40.0, 60.0));
    assert_eq!(s.initial_distance, 100.0);
}

/// 激活后首帧超容差以该帧指距为增量参考；批次门控通过后才更新 prev_distance。
#[test]
fn on_move_updates_incremental_reference_and_keeps_activation_midpoint() {
    let mut table = PointerTable::new();
    table.down(7, 0.0, 0.0);
    table.down(9, 100.0, 0.0);
    let mut s = PinchState::new();
    let anchor = s.on_down(&table).expect("两指按下应激活");
    assert_eq!(
        (anchor.scale, anchor.center_x, anchor.center_y),
        (1.0, 50.0, 0.0)
    );

    // 指 7 移到 -20：指距 120、中点 (40,0)。只有一指动过 → 批次门控拦截，
    // 但激活与参考距离已经更新。
    table.move_pointer(7, -20.0, 0.0);
    assert!(s.on_move(&table, 7).is_none());
    assert_eq!(s.prev_distance, 120.0);
    assert!(s.beyond_tolerance);
    assert_eq!(s.moved, [true, false]);

    // 指 9 移到 110：指距 130、中点 (45,0)。两指均动过 ⇒ 产出 130/120。
    table.move_pointer(9, 110.0, 0.0);
    let d = s.on_move(&table, 9).expect("两指均动后应产出增量");
    assert!((d.scale - 130.0 / 120.0).abs() < 1e-6, "实得 {}", d.scale);
    assert_eq!((d.center_x, d.center_y), (45.0, 0.0));
    assert_eq!(s.prev_distance, 130.0);
    assert_eq!(s.start_mid, (50.0, 0.0), "激活中点不能被后续帧改写");
    assert_eq!(s.moved, [false, false]);
}

/// on_up 结束旧会话并用剩余两指重新激活，几何必须来自新的两指。
#[test]
fn on_up_regrabs_with_remaining_fingers_and_restarts_geometry() {
    let mut table = PointerTable::new();
    table.down(7, 0.0, 0.0);
    table.down(9, 100.0, 0.0);
    table.down(11, 220.0, 0.0);
    let mut s = PinchState::new();
    s.on_down(&table);

    table.up(7);
    assert!(s.on_up(&table, 7), "剩两指应 regrab");
    assert!(s.is_pinching());
    assert_eq!(s.ids, [9, 11]);
    assert_eq!(s.start_mid, (160.0, 0.0));
    assert_eq!(s.initial_distance, 120.0);
    assert_eq!(s.prev_distance, 120.0);
    assert_eq!(s.moved, [false, false]);

    table.up(9);
    assert!(!s.on_up(&table, 9), "只剩一指不再 regrab");
    assert!(!s.is_pinching());
    assert_eq!(s.ids, [NO_ID, NO_ID]);
    assert_eq!(s.moved, [false, false]);
}
