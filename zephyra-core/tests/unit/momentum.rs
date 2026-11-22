// 本文件由 momentum.rs 的 #[cfg(test)] mod tests 抽出 —— 测试代码与生产代码分离。
// 模块路径不变（src 里的声明用 #[path] 指过来），所以 use super::* 与私有可见性都照旧。

use super::*;

#[test]
fn inactive_tick_is_none() {
    let mut m = Momentum::new();
    assert!(m.tick(1.0 / 60.0).is_none());
    assert!(!m.is_active());
}

#[test]
fn decays_and_settles() {
    let mut m = Momentum::new();
    m.start(10.0, 0.0);
    let mut total = 0.0f32;
    let mut frames = 0;
    for _ in 0..2000 {
        match m.tick(1.0 / 60.0) {
            Some((dx, _)) => {
                total += dx;
                frames += 1;
            }
            None => break,
        }
    }
    assert!(!m.is_active(), "惯性应收敛停止");
    assert!(frames > 5, "应有实际滚动帧");
    assert!(total > 10.0, "总位移应超过初速（摩擦渐进），实际 {total}");
    assert!(total < 300.0, "总位移应有界，实际 {total}");
}

#[test]
fn frame_rate_independent_total_distance() {
    // 同一初速度：60Hz 与 120Hz 的累计位移应接近（离散积分误差 < 10%）
    let run = |steps: usize, dt: f32| {
        let mut m = Momentum::new();
        m.start(20.0, 0.0);
        let mut total = 0.0f32;
        for _ in 0..steps {
            match m.tick(dt) {
                Some((dx, _)) => total += dx,
                None => break,
            }
        }
        total
    };
    let t60 = run(600, 1.0 / 60.0);
    let t120 = run(1200, 1.0 / 120.0);
    let rel = (t60 - t120).abs() / t60;
    assert!(rel < 0.10, "帧率无关性破坏：60Hz {t60} vs 120Hz {t120}");
}

#[test]
fn stop_is_immediate() {
    let mut m = Momentum::new();
    m.start(50.0, 50.0);
    assert!(m.tick(1.0 / 60.0).is_some());
    m.stop();
    assert!(!m.is_active());
    assert!(m.tick(1.0 / 60.0).is_none());
}

// ─────────── dt_units.clamp(0.1, 8.0) 两端 + 摩擦常数敏感性 ───────────

/// dt=0（以及任何 ≤0 的值）必须被下限钳到 0.1 个基准帧，而不是变成 0
/// （那样位移会是 0、摩擦指数会是 1，惯性会"卡死"在原地）。
#[test]
fn zero_dt_clamps_to_lower_bound_and_yields_literal_displacement() {
    let mut m = Momentum::new();
    m.start(8.0, 0.0);
    let (dx, dy) = m.tick(0.0).expect("速度远高于停止阈值，本帧必须仍在滚动");
    // dt_units = 0.1；friction = 0.85 - 0.2*exp(-8/8) = 0.7764241；
    // frame_friction = 0.7764241^0.1 = 0.9750119；vx' = 7.800095；
    // dx = vx' * 0.1 = 0.7800095。
    assert!(
        (dx - 0.7800095).abs() < 1e-5,
        "dt=0 应钳到 0.1，实得 dx={dx}"
    );
    assert_eq!(dy, 0.0);
    assert!(m.is_active(), "钳位后速度仍 > 阈值，惯性必须继续");
    // 若下限被改成 0，dx 会是 0；改成 1.0，dx 会是 ~6.21 —— 都落不到 0.78。
}

/// 极大 dt 必须被上限钳到 8.0 个基准帧：既不能按原值积分出巨大位移，
/// 也不能因掉帧过长而直接归零。
#[test]
fn huge_dt_clamps_to_upper_bound_and_yields_literal_displacement() {
    let mut m = Momentum::new();
    m.start(8.0, 0.0);
    let (dx, dy) = m
        .tick(100.0)
        .expect("钳到 8 帧后速度 1.0565 仍 > 0.5，必须继续滚动");
    // dt_units = 8.0；frame_friction = 0.7764241^8 = 0.13206632；
    // vx' = 1.0565306；dx = 1.0565306 * 8 = 8.452245。
    assert!(
        (dx - 8.452245).abs() < 1e-5,
        "极大 dt 应钳到 8.0，实得 dx={dx}"
    );
    assert_eq!(dy, 0.0);
    assert!(m.is_active());
    // 上限被改成 16 时 vx' 会跌破 0.5 ⇒ tick 返回 None，上面的 expect 会失败。
}

/// 以恰好一个基准帧（dt_units=1）跑：frame_friction == friction，
/// 于是位移直接编码了 FRICTION_BASE / FRICTION_MAX / SPEED_SMOOTH 三个常数。
#[test]
fn one_base_frame_encodes_the_friction_formula_constants() {
    let mut m = Momentum::new();
    m.start(8.0, 0.0);
    let (dx, _) = m.tick(16.0 / 1000.0).expect("一帧后仍应活跃");
    // speed = 8 = SPEED_SMOOTH ⇒ exp(-1) = 0.36787944；
    // friction = 0.85 - 0.20*0.36787944 = 0.7764241；
    // vx' = 8 * 0.7764241 = 6.211393；dt_units = 1 ⇒ dx = 6.211393。
    assert!(
        (dx - 6.211393).abs() < 1e-5,
        "摩擦公式常数被改动，实得 dx={dx}"
    );

    // 低速端（speed=1）的摩擦应接近 FRICTION_BASE=0.65 而不是 FRICTION_MAX：
    // friction = 0.85 - 0.2*exp(-1/8) = 0.6735006。
    let mut low = Momentum::new();
    low.start(1.0, 0.0);
    let (ldx, _) = low.tick(16.0 / 1000.0).expect("一帧后仍应活跃");
    assert!(
        (ldx - 0.6735006).abs() < 1e-5,
        "低速摩擦应贴近 0.65，实得 {ldx}"
    );
}
