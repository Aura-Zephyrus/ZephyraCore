//! `style.rs` 里数值夹逼与裁剪来源优先级的分支测试。
//!
//! # 为什么单独一个文件
//!
//! `text_style()` / `clip_rect()` 里的每一个 `clamp`、`round`、`max`、类型宽度
//! 与来源判定都是一条独立的契约：改错任何一处，画面症状都离病因很远（字号
//! 变成 0 被宿主回落、字重悄悄截断、裁剪区突然是零尺寸而整块不裁）。这里把
//! 每条契约拆成一个测试，断言**精确字面量**，不复用被测字段自证。
//!
//! 测试模块挂在 `src/style.rs` 末尾，于是可以用 `super::` 直接调用私有的
//! [`super::finite`] / [`super::sanitized_scale`] —— 这两个纯函数是夹逼逻辑的
//! 最小单元，直接测它们能把变异定位到具体一行。

use crate::context::{self, Context};
use crate::core::Rect;
use crate::element::Element;
use crate::id::NodeTag;
use crate::layout::Overflow;
use crate::node::Node;
use crate::property::PropertyValue;
use crate::props;

fn ctx() -> Context<'static> {
    context::dark_context()
}

/// 摆好一个带已知 border box 的节点，供 `clip_rect` 的 overflow 分支断言。
fn placed_node(e: &Element) -> Node {
    let mut node = Node::from_element(e);
    node.set_rect(Rect::new(0.0, 0.0, 200.0, 100.0));
    node
}

// ============================== text_style: size ==============================

/// `TEXT_SIZE` 小于 1 时被 `.max(1.0)` 夹到 1.0。
/// 变异：去掉 / 改小 `max(1.0)` 的下界 ⇒ 拿到 0.5。
#[test]
fn text_size_below_one_is_clamped_to_one() {
    let e = Element::new(NodeTag::Text).with(&props::TEXT_SIZE, PropertyValue::float(0.5));
    let ts = Node::from_element(&e).text_style(ctx());
    assert_eq!(ts.size, 1.0);
}

/// 未设置时回落到默认字号 14.0（而不是 0.0 —— 宿主会把 0 当非法）。
/// 变异：改掉 `number_or` 的 fallback。
#[test]
fn text_size_unset_uses_fourteen() {
    let ts = Node::from_element(&Element::new(NodeTag::Text)).text_style(ctx());
    assert_eq!(ts.size, 14.0);
}

// ============================== text_style: weight ==============================

/// weight = 0 经 `round().clamp(1,1000)` 后是 1。
/// 变异：下界改成 0 ⇒ 拿到 0。
#[test]
fn weight_zero_clamps_to_one() {
    let e = Element::new(NodeTag::Text).with(&props::FONT_WEIGHT, PropertyValue::float(0.0));
    let ts = Node::from_element(&e).text_style(ctx());
    assert_eq!(ts.weight, 1);
}

/// weight = 0.4 → round(0) → clamp 下界 → 1。
/// 变异：把 `round()` 换成 `trunc()` 或漏掉 clamp（0 as u16 = 0）。
#[test]
fn weight_fraction_below_half_clamps_to_one() {
    let e = Element::new(NodeTag::Text).with(&props::FONT_WEIGHT, PropertyValue::float(0.4));
    let ts = Node::from_element(&e).text_style(ctx());
    assert_eq!(ts.weight, 1);
}

/// weight = 1001 → round(1001) → clamp 上界 → 1000。
/// 变异：上界改成 1001 或 65535 ⇒ 拿到 1001。
#[test]
fn weight_above_1000_clamps_to_1000() {
    let e = Element::new(NodeTag::Text).with(&props::FONT_WEIGHT, PropertyValue::float(1001.0));
    let ts = Node::from_element(&e).text_style(ctx());
    assert_eq!(ts.weight, 1000);
}

/// 用 700.6 区分 `round()` 与 `trunc()`：round → 701，trunc → 700。
/// 下界 clamp 对小值有掩盖作用，所以判别点必须放在远离 1 的地方。
#[test]
fn weight_rounds_instead_of_truncating() {
    let e = Element::new(NodeTag::Text).with(&props::FONT_WEIGHT, PropertyValue::float(700.6));
    let ts = Node::from_element(&e).text_style(ctx());
    assert_eq!(ts.weight, 701);
}

/// weight = NaN → `number_or` 过滤 → `finite()` → None → 默认 400。
/// 变异：默认分支改成 0 / 去掉 finite 判断。
#[test]
fn weight_nan_falls_back_to_400() {
    let e = Element::new(NodeTag::Text).with(&props::FONT_WEIGHT, PropertyValue::float(f32::NAN));
    let ts = Node::from_element(&e).text_style(ctx());
    assert_eq!(ts.weight, 400);
}

// ============================== text_style: palette ==============================

/// palette = -1 → `clamp(0, u16::MAX)` 下界 → 0。
/// 变异：下界改成 1 ⇒ 拿到 1。
#[test]
fn palette_negative_clamps_to_zero() {
    let e = Element::new(NodeTag::Text).with(&props::TEXT_PALETTE, PropertyValue::int(-1));
    let ts = Node::from_element(&e).text_style(ctx());
    assert_eq!(ts.palette, 0);
}

/// palette = 65536 → 上界夹到 u16::MAX。
/// 变异：`u16::MAX` → `u8::MAX` ⇒ 拿到 255。
#[test]
fn palette_above_u16_clamps_to_65535() {
    let e = Element::new(NodeTag::Text).with(&props::TEXT_PALETTE, PropertyValue::int(65536));
    let ts = Node::from_element(&e).text_style(ctx());
    assert_eq!(ts.palette, 65535);
}

// ============================== text_style: max_lines ==============================

/// max_lines = -3 → `.max(0.0)` → `Some(0)`。
/// 变异：下界改成 1 / 去掉 clamp 后改走 abs ⇒ 拿到 1 或 3。
#[test]
fn negative_max_lines_clamps_to_zero() {
    let e = Element::new(NodeTag::Text).with(&props::TEXT_MAX_LINES, PropertyValue::float(-3.0));
    let ts = Node::from_element(&e).text_style(ctx());
    assert_eq!(ts.max_lines, Some(0));
}

/// 正值原样保留 —— 防止 `.max(0.0)` 被替换成常量 0.0。
#[test]
fn positive_max_lines_is_preserved() {
    let e = Element::new(NodeTag::Text).with(&props::TEXT_MAX_LINES, PropertyValue::float(3.0));
    let ts = Node::from_element(&e).text_style(ctx());
    assert_eq!(ts.max_lines, Some(3));
}

// ============================== text_style: 非有限值过滤 ==============================

/// line_height = NaN → `finite()` 吞掉 → None。
/// 变异：`finite()` 的 `is_finite()` 判定被去掉 ⇒ Some(NaN)。
#[test]
fn nan_line_height_is_dropped() {
    let e =
        Element::new(NodeTag::Text).with(&props::TEXT_LINE_HEIGHT, PropertyValue::float(f32::NAN));
    let ts = Node::from_element(&e).text_style(ctx());
    assert_eq!(ts.line_height, None);
}

/// line_height = inf 同样被吞掉（不是 clamp 成一个巨大值）。
#[test]
fn infinite_line_height_is_dropped() {
    let e = Element::new(NodeTag::Text).with(
        &props::TEXT_LINE_HEIGHT,
        PropertyValue::float(f32::INFINITY),
    );
    let ts = Node::from_element(&e).text_style(ctx());
    assert_eq!(ts.line_height, None);
}

/// letter_spacing = NaN → `num_or_default` 的 `is_finite` 过滤 → 默认 0.0。
/// 变异：过滤条件被删掉 ⇒ NaN 混进排版。
#[test]
fn nan_letter_spacing_falls_back_to_zero() {
    let e = Element::new(NodeTag::Text)
        .with(&props::TEXT_LETTER_SPACING, PropertyValue::float(f32::NAN));
    let ts = Node::from_element(&e).text_style(ctx());
    assert_eq!(ts.letter_spacing, 0.0);
}

/// letter_spacing = inf 同样回落到 0.0。
#[test]
fn infinite_letter_spacing_falls_back_to_zero() {
    let e = Element::new(NodeTag::Text).with(
        &props::TEXT_LETTER_SPACING,
        PropertyValue::float(f32::INFINITY),
    );
    let ts = Node::from_element(&e).text_style(ctx());
    assert_eq!(ts.letter_spacing, 0.0);
}

/// 间距允许负值：改用 `finite()` 会把 -5 吞成 0。
#[test]
fn negative_letter_spacing_is_preserved() {
    let e =
        Element::new(NodeTag::Text).with(&props::TEXT_LETTER_SPACING, PropertyValue::float(-5.0));
    let ts = Node::from_element(&e).text_style(ctx());
    assert_eq!(ts.letter_spacing, -5.0);
}

/// `finite()` 本体：非有限与负值 → None，0 与正值 → Some。
/// 变异：`v >= 0.0` → `true` 或 `v.is_finite()` → `true`。
#[test]
fn finite_swallows_non_finite_and_negative() {
    assert_eq!(super::finite(f32::NAN), None);
    assert_eq!(super::finite(f32::INFINITY), None);
    assert_eq!(super::finite(f32::NEG_INFINITY), None);
    assert_eq!(super::finite(-0.5), None);
    assert_eq!(super::finite(0.0), Some(0.0));
    assert_eq!(super::finite(3.5), Some(3.5));
}

// ============================== sanitized_scale ==============================

/// `sanitized_scale()` 本体：NaN / inf / 0 / 负值 → 1.0，正值原样返回。
/// 变异：`v > 0.0` → `v >= 0.0`（0 会通过）、`is_finite()` 被删（inf 会通过）。
#[test]
fn sanitized_scale_rejects_invalid_values() {
    assert_eq!(super::sanitized_scale(f32::NAN), 1.0);
    assert_eq!(super::sanitized_scale(f32::INFINITY), 1.0);
    assert_eq!(super::sanitized_scale(f32::NEG_INFINITY), 1.0);
    assert_eq!(super::sanitized_scale(0.0), 1.0);
    assert_eq!(super::sanitized_scale(-2.0), 1.0);
    assert_eq!(super::sanitized_scale(2.5), 2.5);
}

/// getter 真的调用了 `sanitized_scale`（0 与负值都要变 1.0）。
/// 变异：getter 里去掉 sanitize 包装 ⇒ 直接拿到 0.0 / -2.0。
#[test]
fn scale_getters_sanitize_zero_and_negative() {
    let e = Element::new(NodeTag::Box)
        .with(&props::SCALE_X, PropertyValue::float(0.0))
        .with(&props::SCALE_Y, PropertyValue::float(-2.0));
    let node = Node::from_element(&e);
    assert_eq!(node.scale_x(ctx()), 1.0);
    assert_eq!(node.scale_y(ctx()), 1.0);
}

/// 合法的正缩放不被“安全化”成 1.0。
#[test]
fn scale_getters_keep_valid_scale() {
    let e = Element::new(NodeTag::Box).with(&props::SCALE_X, PropertyValue::float(1.75));
    let node = Node::from_element(&e);
    assert_eq!(node.scale_x(ctx()), 1.75);
}

// ============================== clip_rect ==============================

/// 显式 `CLIP_RECT` 单独存在时被返回。
/// 变异：显式分支被忽略 ⇒ None。
#[test]
fn explicit_clip_rect_is_returned() {
    let e = Element::new(NodeTag::Box).with(
        &props::CLIP_RECT,
        PropertyValue::rect(Rect::new(5.0, 6.0, 7.0, 8.0)),
    );
    let node = Node::from_element(&e);
    assert_eq!(node.clip_rect(ctx()), Some(Rect::new(5.0, 6.0, 7.0, 8.0)));
}

/// 只有 overflow 时裁到 border box。
/// 变异：overflow 分支被删掉 ⇒ None。
#[test]
fn overflow_hidden_clips_to_border_box() {
    let e = Element::new(NodeTag::Box)
        .with(&props::OVERFLOW, PropertyValue::overflow(Overflow::Hidden));
    let node = placed_node(&e);
    assert_eq!(
        node.clip_rect(ctx()),
        Some(Rect::new(0.0, 0.0, 200.0, 100.0))
    );
}

/// 显式 `CLIP_RECT` 优先于 overflow —— 优先级反转时拿到 border box。
#[test]
fn explicit_clip_rect_wins_over_overflow() {
    let e = Element::new(NodeTag::Box)
        .with(
            &props::CLIP_RECT,
            PropertyValue::rect(Rect::new(5.0, 6.0, 7.0, 8.0)),
        )
        .with(&props::OVERFLOW, PropertyValue::overflow(Overflow::Hidden));
    let node = placed_node(&e);
    assert_eq!(node.clip_rect(ctx()), Some(Rect::new(5.0, 6.0, 7.0, 8.0)));
}

/// 零宽显式 clip 视为“不裁剪”（默认全零矩形走的就是这条路）。
/// 变异：`r.width > 0.0` 被删 ⇒ 返回一个零宽 Rect。
#[test]
fn zero_width_clip_is_ignored() {
    let e = Element::new(NodeTag::Box).with(
        &props::CLIP_RECT,
        PropertyValue::rect(Rect::new(1.0, 2.0, 0.0, 50.0)),
    );
    let node = Node::from_element(&e);
    assert_eq!(node.clip_rect(ctx()), None);
}

/// 零高显式 clip 被忽略，overflow 仍能兜底成 border box。
/// 变异：`r.height > 0.0` 被删 ⇒ 返回零高 Rect 而非 border box。
#[test]
fn zero_height_clip_falls_back_to_overflow() {
    let e = Element::new(NodeTag::Box)
        .with(
            &props::CLIP_RECT,
            PropertyValue::rect(Rect::new(1.0, 2.0, 50.0, 0.0)),
        )
        .with(&props::OVERFLOW, PropertyValue::overflow(Overflow::Hidden));
    let node = placed_node(&e);
    assert_eq!(
        node.clip_rect(ctx()),
        Some(Rect::new(0.0, 0.0, 200.0, 100.0))
    );
}

/// 默认（visible + 零 clip）不裁剪。
#[test]
fn visible_overflow_without_clip_returns_none() {
    let node = Node::from_element(&Element::new(NodeTag::Box));
    assert_eq!(node.clip_rect(ctx()), None);
}

/// scroll / auto 也走裁剪分支（`clips()` 只排除 Visible）。
/// 变异：`clips()` 改成 `== Hidden` ⇒ 这两档不再裁剪。
#[test]
fn scroll_and_auto_overflow_also_clip() {
    for overflow in [Overflow::Scroll, Overflow::Auto] {
        let e =
            Element::new(NodeTag::Box).with(&props::OVERFLOW, PropertyValue::overflow(overflow));
        let node = placed_node(&e);
        assert_eq!(
            node.clip_rect(ctx()),
            Some(Rect::new(0.0, 0.0, 200.0, 100.0)),
            "overflow = {:?} 必须裁剪",
            overflow
        );
    }
}
