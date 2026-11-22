// 本文件由 renderer.rs 的 #[cfg(test)] mod tests 抽出 —— 测试代码与生产代码分离。

use super::*;

/// P0.1：`HookContext` 的具体上下文必须能向下转型，否则外部钩子（笔迹层）
/// 拿不到立即上下文，画不出任何像素。
#[test]
fn hook_ctx_is_downcastable() {
    use zephyra_render_ext::registration::HookContextExt;

    // 空指针只用于验证类型可转型；不调用访问器就不会解引用。
    let mut ctx = HookCtx {
        context: std::ptr::null(),
        device: std::ptr::null(),
        rtv: std::ptr::null(),
        size: (100.0, 80.0),
        frame_index: 7,
        ext_pool: None,
    };
    let dyn_ctx: &mut dyn zephyra_render_ext::registration::HookContext = &mut ctx;
    assert!(
        dyn_ctx.downcast_mut::<HookCtx>().is_some(),
        "downcast_mut::<HookCtx>() 必须成功 —— 钩子要据此拿立即上下文绘制"
    );
    assert_eq!(dyn_ctx.frame_index(), 7);
    assert_eq!(dyn_ctx.size(), (100.0, 80.0));
}

// ---- 纯函数表驱动测试（无需 GPU / 窗口） ----
//
// 下面这些 helper 都是纯计算：表驱动 + 字面量期望值，目的是让抄错一条映射/算错一位
// 的变异立刻变红，而不是让它躲在"某个混合模式静默画错"里。

/// `d3d11_factor` 的穷尽 10 个 `Coeff` 分支，逐项钉死字面量 `D3D11_BLEND`。
///
/// 漏一个变体会被穷尽 match 挡在编译期，但**抄错一条**只会让某个模式静默画错 ——
/// 所以这里按 Skia `kCoeffs` 把每一项返回值写死。
#[test]
fn d3d11_factor_maps_all_ten_coeffs_to_literal_blends() {
    let cases: [(Coeff, D3D11_BLEND); 10] = [
        (Coeff::Zero, D3D11_BLEND_ZERO),
        (Coeff::One, D3D11_BLEND_ONE),
        (Coeff::SrcColor, D3D11_BLEND_SRC_COLOR),
        (Coeff::OneMinusSrcColor, D3D11_BLEND_INV_SRC_COLOR),
        (Coeff::DstColor, D3D11_BLEND_DEST_COLOR),
        (Coeff::OneMinusDstColor, D3D11_BLEND_INV_DEST_COLOR),
        (Coeff::SrcAlpha, D3D11_BLEND_SRC_ALPHA),
        (Coeff::OneMinusSrcAlpha, D3D11_BLEND_INV_SRC_ALPHA),
        (Coeff::DstAlpha, D3D11_BLEND_DEST_ALPHA),
        (Coeff::OneMinusDstAlpha, D3D11_BLEND_INV_DEST_ALPHA),
    ];
    for (c, expected) in cases {
        assert_eq!(d3d11_factor(c), expected, "Coeff::{c:?} 的颜色因子映射错了");
    }
}

/// `d3d11_alpha_factor` 的穷尽 10 个分支，逐项钉死字面量。
///
/// 颜色系数走的是 alpha **对偶**（`Cs→as`、`1−Cs→1−as`、`Cd→ad`、`1−Cd→1−ad`），
/// 不是原样复用颜色因子 —— D3D11 的 alpha 槽不接受颜色因子。
#[test]
fn d3d11_alpha_factor_maps_all_ten_coeffs_to_literal_blends() {
    let cases: [(Coeff, D3D11_BLEND); 10] = [
        (Coeff::Zero, D3D11_BLEND_ZERO),
        (Coeff::One, D3D11_BLEND_ONE),
        // 非显然的四条对偶：颜色因子 → alpha 因子。
        (Coeff::SrcColor, D3D11_BLEND_SRC_ALPHA),
        (Coeff::OneMinusSrcColor, D3D11_BLEND_INV_SRC_ALPHA),
        (Coeff::DstColor, D3D11_BLEND_DEST_ALPHA),
        (Coeff::OneMinusDstColor, D3D11_BLEND_INV_DEST_ALPHA),
        // 这四条本来就合法，原样透传。
        (Coeff::SrcAlpha, D3D11_BLEND_SRC_ALPHA),
        (Coeff::OneMinusSrcAlpha, D3D11_BLEND_INV_SRC_ALPHA),
        (Coeff::DstAlpha, D3D11_BLEND_DEST_ALPHA),
        (Coeff::OneMinusDstAlpha, D3D11_BLEND_INV_DEST_ALPHA),
    ];
    for (c, expected) in cases {
        assert_eq!(
            d3d11_alpha_factor(c),
            expected,
            "Coeff::{c:?} 的 alpha 因子映射错了"
        );
    }
}

/// 四条颜色→alpha 对偶必须**与颜色因子不同**。
///
/// 若有人把 `d3d11_alpha_factor` 改成直接 `d3d11_factor(c)`，`Modulate` / `Screen`
/// 的 `CreateBlendState` 会回 `E_INVALIDARG`，症状是 `Renderer::new` 整个失败且
/// 只报"参数错误"——这里用 `assert_ne` 把它挡在纯计算层。
#[test]
fn d3d11_alpha_factor_pins_color_to_alpha_duals() {
    for (c, color, alpha) in [
        (
            Coeff::SrcColor,
            D3D11_BLEND_SRC_COLOR,
            D3D11_BLEND_SRC_ALPHA,
        ),
        (
            Coeff::OneMinusSrcColor,
            D3D11_BLEND_INV_SRC_COLOR,
            D3D11_BLEND_INV_SRC_ALPHA,
        ),
        (
            Coeff::DstColor,
            D3D11_BLEND_DEST_COLOR,
            D3D11_BLEND_DEST_ALPHA,
        ),
        (
            Coeff::OneMinusDstColor,
            D3D11_BLEND_INV_DEST_COLOR,
            D3D11_BLEND_INV_DEST_ALPHA,
        ),
    ] {
        assert_eq!(d3d11_factor(c), color, "Coeff::{c:?} 的颜色因子");
        assert_eq!(d3d11_alpha_factor(c), alpha, "Coeff::{c:?} 的 alpha 对偶");
        assert_ne!(
            d3d11_alpha_factor(c),
            d3d11_factor(c),
            "Coeff::{c:?} 的颜色系数不能原样塞进 alpha 槽"
        );
    }
}

/// `mode_by_index` 是按声明顺序取值的表：钉住首尾与几个已知下标。
#[test]
fn mode_by_index_pins_known_entries() {
    assert_eq!(mode_by_index(0), BlendMode::Clear);
    assert_eq!(mode_by_index(1), BlendMode::Src);
    assert_eq!(mode_by_index(3), BlendMode::SrcOver);
    assert_eq!(mode_by_index(13), BlendMode::Modulate);
    assert_eq!(mode_by_index(14), BlendMode::Screen);
    assert_eq!(mode_by_index(15), BlendMode::Overlay);
    assert_eq!(mode_by_index(BlendMode::COUNT - 1), BlendMode::Luminosity);
}

/// 所有合法下标都是身份映射：`mode_by_index(i) as usize == i`。
#[test]
fn mode_by_index_is_identity_for_all_valid_indices() {
    for i in 0..BlendMode::COUNT {
        assert_eq!(mode_by_index(i) as usize, i, "下标 {i} 不是身份映射");
    }
}

/// 返回的是值而非 `Option`：越界就是数组越界 panic，不能悄悄给个默认模式。
#[test]
#[should_panic(expected = "index out of bounds")]
fn mode_by_index_one_past_last_panics() {
    let _ = mode_by_index(BlendMode::COUNT);
}

#[test]
#[should_panic(expected = "index out of bounds")]
fn mode_by_index_absurd_index_panics() {
    let _ = mode_by_index(usize::MAX);
}

/// `content_translate` 必须原样带出 `pass.translate`：钉死零、正、负、分数 x/y。
///
/// 用 `pass_for_chain` 造一张真实 pass 做模板，只覆盖 `translate` 字段 —— 这样
/// 既覆盖了"影子整体平移"这条路径的载体，又不把期望值建立在离屏外扩的推导上。
#[test]
fn content_translate_pins_exact_offsets() {
    use zephyra_core::decoration::Decorations;
    use zephyra_core::filter::FilterChain;

    let base = offscreen::pass_for_chain(
        &FilterChain::none(),
        Rect::new(10.0, 20.0, 100.0, 60.0),
        &Decorations::none(),
        0.0,
    );
    for (tx, ty) in [(0.0, 0.0), (13.0, -7.0), (-100.25, 0.5), (1.0 / 3.0, -2.75)] {
        let pass = OffscreenPass {
            translate: (tx, ty),
            ..base.clone()
        };
        assert_eq!(content_translate(&pass), Point::new(tx, ty));
    }
}

/// 真实 pass 的 `translate = -origin + offset` 必须被 `content_translate` 带出。
///
/// 这是影子"先平移再模糊"那条式子在后端的落点；写成常量或漏掉 `offset` 都会让
/// 影子画回原位，而计数与日志一切正常。
#[test]
fn content_translate_reflects_neg_origin_plus_offset() {
    use zephyra_core::decoration::Decorations;
    use zephyra_core::filter::{Filter, FilterChain};

    let offset = Point::new(6.0, -4.0);
    let pass = offscreen::pass_for_chain(
        &FilterChain::single(Filter::DropShadow {
            color: Color::rgba(0.0, 0.0, 0.0, 0.5),
            offset,
            sigma: 2.0,
        }),
        Rect::new(40.0, 30.0, 120.0, 80.0),
        &Decorations::none(),
        0.0,
    );
    assert_eq!(
        content_translate(&pass),
        Point::new(-pass.origin.0 + offset.x, -pass.origin.1 + offset.y)
    );
    assert_eq!(pass.translate.0, -pass.origin.0 + 6.0);
    assert_eq!(pass.translate.1, -pass.origin.1 - 4.0);
}

/// `Damage::Full` 必须显式给出全屏矩形（D3D11 没有"关掉 scissor"的开关）。
#[test]
fn damage_scissor_full_is_whole_target() {
    for ((w, h), expected) in [
        ((100u32, 80u32), (0, 0, 100, 80)),
        ((1, 1), (0, 0, 1, 1)),
        ((1920, 1080), (0, 0, 1920, 1080)),
    ] {
        let s = damage_scissor((w, h), Damage::Full);
        assert_eq!(
            (s.left, s.top, s.right, s.bottom),
            expected,
            "Damage::Full 必须给全屏矩形"
        );
    }
}

/// 普通整数矩形：DIP=像素（当前宿主的比例），逐字段精确。
#[test]
fn damage_scissor_pins_integer_rect() {
    let s = damage_scissor((100, 80), Damage::Rect(Rect::new(10.0, 20.0, 30.0, 40.0)));
    assert_eq!((s.left, s.top, s.right, s.bottom), (10, 20, 40, 60));
}

/// 分数 DIP 坐标：左/上 `floor`、右/下 `ceil`，边界恰好为整数时不许多算一列。
#[test]
fn damage_scissor_pins_fractional_dip() {
    let s = damage_scissor((100, 80), Damage::Rect(Rect::new(10.3, 20.7, 5.4, 8.2)));
    assert_eq!((s.left, s.top, s.right, s.bottom), (10, 20, 16, 29));

    let s = damage_scissor((256, 256), Damage::Rect(Rect::new(0.25, 0.75, 7.75, 3.25)));
    assert_eq!((s.left, s.top, s.right, s.bottom), (0, 0, 8, 4));
}

/// 负的左/上坐标要夹到 0，但右/下边界仍按原点一起平移。
#[test]
fn damage_scissor_clamps_negative_left_top_but_keeps_extent() {
    let s = damage_scissor((100, 80), Damage::Rect(Rect::new(-5.0, -3.0, 20.0, 10.0)));
    assert_eq!((s.left, s.top, s.right, s.bottom), (0, 0, 15, 7));
}

/// 整个矩形都在负坐标时右/下边界也变负 —— 必须回退全屏而不是设空矩形。
#[test]
fn damage_scissor_fully_negative_rect_falls_back_to_full() {
    let s = damage_scissor((100, 80), Damage::Rect(Rect::new(-20.0, -10.0, 5.0, 5.0)));
    assert_eq!((s.left, s.top, s.right, s.bottom), (0, 0, 100, 80));
}

/// 退化矩形（零宽/零高/负宽/负高）一律回退全屏。
///
/// D3D11 的 scissor 是半开区间 `[left, right)`，`right <= left` 时什么都不画
/// 且零错误 —— 宁可多画也不要让元素凭空消失。
#[test]
fn damage_scissor_degenerate_rect_falls_back_to_full() {
    for rect in [
        Rect::new(10.0, 10.0, 0.0, 10.0),
        Rect::new(10.0, 10.0, 10.0, 0.0),
        Rect::new(30.0, 30.0, -5.0, 10.0),
        Rect::new(30.0, 30.0, 10.0, -5.0),
    ] {
        let s = damage_scissor((100, 80), Damage::Rect(rect));
        assert_eq!(
            (s.left, s.top, s.right, s.bottom),
            (0, 0, 100, 80),
            "退化矩形 {rect:?} 必须回退全屏"
        );
    }
}

/// 超大矩形**不夹到目标尺寸** —— 右/下边界直接由 DIP 算出。
///
/// 这条钉死当前语义：调用方给的脏区可以超出屏幕，scissor 只需覆盖整个目标。
#[test]
fn damage_scissor_oversized_rect_is_not_clamped() {
    let s = damage_scissor(
        (100, 80),
        Damage::Rect(Rect::new(-100.0, -100.0, 500.0, 500.0)),
    );
    assert_eq!((s.left, s.top, s.right, s.bottom), (0, 0, 400, 400));
}
