//! 本文件由 `zephyra::zephyra-core::src::visual.rs` 的 `#[cfg(test)] mod tests` 抽出 —— 测试代码与生产代码分离。
//!
//! 模块路径不变（生产文件里仍用 `#[path = ...]` 声明），所以 `use super::*`
//! 与对父模块私有项的访问照旧。

use super::*;
use std::cell::RefCell;

use crate::context::{dark_context, Context};
use crate::decoration::Decorations;
use crate::element::Element;
use crate::filter::{Filter, FilterChain};
use crate::id::NodeTag;
use crate::layout::layout;
use crate::node::GlyphQuadCache;
use crate::paint::{GlyphQuad, TextureRef};
use crate::property::PropertyValue;
use crate::props;
use crate::text::{NullEngine, ShapedText, TextEngine, TextRender, TextStyle};
use crate::theme::Theme;

#[test]
fn root_position_clears_ancestor_translate_for_visual_bounds() {
    let root = Element::new(NodeTag::Box)
        .with(&props::TRANSLATE_X, PropertyValue::float(50.0))
        .with_children([Element::new(NodeTag::Box)
            .with(&props::WIDTH, PropertyValue::float(10.0))
            .with(&props::HEIGHT, PropertyValue::float(10.0))
            .with(
                &props::POSITION,
                PropertyValue::position(crate::layout::Position::Root),
            )
            .with(&props::LEFT, PropertyValue::float(5.0))
            .with(&props::TOP, PropertyValue::float(7.0))]);
    let mut node = Node::from_element(&root);
    layout(&mut node, Rect::new(0.0, 0.0, 200.0, 100.0), dark_context());
    let child = &node.children()[0];
    // Parent translate (50) must not affect root-positioned child (D11).
    let (state, tr) = enter(&node, dark_context(), VisualState::ROOT);
    let (child_state, ctr) = enter(child, dark_context(), state);
    let _ = child_state;
    let vr = visual_rect(child, dark_context(), ctr);
    assert!((vr.x - 5.0).abs() < 0.001, "x={}", vr.x);
    assert!((vr.y - 7.0).abs() < 0.001, "y={}", vr.y);
    assert!((tr.x - 50.0).abs() < 0.001);
}

// ==================================================================
// sanitize / sanitized_scale —— 非有限值降级
// ==================================================================

/// `translate` 的非有限值必须归零；有限值（含负数与 0）原样保留。
///
/// 变异点：把 `v.is_finite()` 改成恒真 / 恒假，或把 `0.0` 改成 `1.0`。
#[test]
fn sanitize_maps_non_finite_to_zero_and_keeps_finite() {
    assert_eq!(sanitize(f32::NAN), 0.0);
    assert_eq!(sanitize(f32::INFINITY), 0.0);
    assert_eq!(sanitize(f32::NEG_INFINITY), 0.0);
    assert_eq!(sanitize(0.0), 0.0);
    assert_eq!(sanitize(-12.5), -12.5);
    assert_eq!(sanitize(37.25), 37.25);
}

/// 缩放的非有限值与 <=0 必须回落到 1.0；正有限值原样保留。
///
/// 变异点：`v > 0.0` 漏判、`is_finite()` 漏判、回落值写成 0.0。
#[test]
fn sanitized_scale_falls_back_to_one_for_invalid_values() {
    assert_eq!(sanitized_scale(0.0), 1.0);
    assert_eq!(sanitized_scale(-2.0), 1.0);
    assert_eq!(sanitized_scale(f32::NAN), 1.0);
    assert_eq!(sanitized_scale(f32::INFINITY), 1.0);
    assert_eq!(sanitized_scale(f32::NEG_INFINITY), 1.0);
    assert_eq!(sanitized_scale(0.5), 0.5);
    assert_eq!(sanitized_scale(2.0), 2.0);
}

// ==================================================================
// union_rects —— 空 / 单 / 多矩形
// ==================================================================

/// 两个矩形的并集：落在内部的零面积矩形不污染结果，相同矩形幂等，不同矩形
/// 取外接框。
///
/// 变异点：`min`/`max` 写反、只取 `a` 或只取 `b`、漏算 `right()/bottom()`。
#[test]
fn union_rects_handles_degenerate_single_and_multiple_rects() {
    // 「空」：零面积矩形落在真实矩形内部 ⇒ 仍是真实矩形（并集按角点算，
    // 所以这个退化点必须放在 b 内部，放在原点会把它自己的角点并进来）。
    assert_eq!(
        union_rects(
            Rect::new(7.0, 8.0, 0.0, 0.0),
            Rect::new(5.0, 6.0, 10.0, 20.0)
        ),
        Rect::new(5.0, 6.0, 10.0, 20.0)
    );
    // 「单」：两个相同矩形 ⇒ 不变。
    assert_eq!(
        union_rects(Rect::new(1.0, 2.0, 3.0, 4.0), Rect::new(1.0, 2.0, 3.0, 4.0)),
        Rect::new(1.0, 2.0, 3.0, 4.0)
    );
    // 「多」：两个分离矩形 ⇒ 外接框；顺序不能影响结果。
    let a = Rect::new(0.0, 0.0, 10.0, 10.0);
    let b = Rect::new(20.0, 5.0, 10.0, 10.0);
    assert_eq!(union_rects(a, b), Rect::new(0.0, 0.0, 30.0, 15.0));
    assert_eq!(union_rects(b, a), Rect::new(0.0, 0.0, 30.0, 15.0));
}

// ==================================================================
// VisualState / enter —— 根边界与滚动子偏移
// ==================================================================

/// `VisualState::ROOT` 是恒等基准：零位移、Content 层、序号 0。
///
/// 变异点：把 `Point::ZERO` 改成非零点，或把 `preorder` 改成 1。
#[test]
fn visual_state_root_is_identity() {
    assert_eq!(VisualState::ROOT.translate, Point::ZERO);
    assert_eq!(VisualState::ROOT.layer, crate::paint::Layer::Content);
    assert_eq!(VisualState::ROOT.preorder, 0);
}

/// `position: root` 清掉祖先累计位移，但节点自己的 translate 仍生效。
///
/// 变异点：`base_translate` 恒取 `parent.translate`（丢掉 root 分支）。
#[test]
fn enter_root_position_discards_ancestor_translate_but_keeps_own() {
    let parent = VisualState {
        translate: Point::new(50.0, 60.0),
        ..VisualState::ROOT
    };
    let el = Element::new(NodeTag::Box)
        .with(
            &props::POSITION,
            PropertyValue::position(crate::layout::Position::Root),
        )
        .with(&props::TRANSLATE_X, PropertyValue::float(7.0))
        .with(&props::TRANSLATE_Y, PropertyValue::float(8.0));
    let node = Node::from_element(&el);

    let (state, own) = enter(&node, dark_context(), parent);
    assert_eq!(own, Point::new(7.0, 8.0), "自己的位移不能丢");
    assert_eq!(
        state.translate,
        Point::new(7.0, 8.0),
        "root 边界必须清掉祖先的 (50,60)"
    );
}

/// 非 root 节点：子节点基准 = 父位移 + 自身 translate − 滚动偏移。
///
/// `own_translate` 是**本节点**视觉矩形用的（不含滚动），传给子节点的那份才扣
/// 掉 scroll。变异点：把 scroll 加回去、或把 scroll 也扣到 `own_translate`。
#[test]
fn enter_subtracts_scroll_offset_from_child_translate_only() {
    let parent = VisualState {
        translate: Point::new(1.0, 2.0),
        ..VisualState::ROOT
    };
    let el = Element::new(NodeTag::Box)
        .with(&props::TRANSLATE_X, PropertyValue::float(3.0))
        .with(&props::TRANSLATE_Y, PropertyValue::float(4.0))
        .with(&props::SCROLL_OFFSET_X, PropertyValue::float(10.0))
        .with(&props::SCROLL_OFFSET_Y, PropertyValue::float(20.0));
    let node = Node::from_element(&el);

    let (state, own) = enter(&node, dark_context(), parent);
    assert_eq!(own, Point::new(4.0, 6.0), "容器自身留在 (1+3, 2+4)");
    assert_eq!(
        state.translate,
        Point::new(-6.0, -14.0),
        "子内容按 -scroll 平移：(4-10, 6-20)"
    );
}

/// 非 root 且无滚动时，`own` 与子节点基准一致（父位移照常累加）。
#[test]
fn enter_accumulates_parent_translate_for_non_root_node() {
    let parent = VisualState {
        translate: Point::new(50.0, 60.0),
        ..VisualState::ROOT
    };
    let node = Node::from_element(&Element::new(NodeTag::Box));
    let (state, own) = enter(&node, dark_context(), parent);
    assert_eq!(own, Point::new(50.0, 60.0));
    assert_eq!(state.translate, Point::new(50.0, 60.0));
}

// ==================================================================
// damage_bounds —— 滤镜 / 装饰外扩
// ==================================================================

/// 没有任何滤镜 / 装饰时，脏区就等于视觉矩形。
#[test]
fn damage_bounds_equals_visual_rect_without_outsets() {
    let mut node = Node::from_element(&Element::new(NodeTag::Box));
    node.arranged_rect = Rect::new(10.0, 20.0, 100.0, 50.0);
    let ctx = dark_context();
    let visual = visual_rect(&node, ctx, Point::ZERO);
    assert_eq!(visual, Rect::new(10.0, 20.0, 100.0, 50.0));
    assert_eq!(damage_bounds(&node, ctx, Point::ZERO), visual);
}

/// 滤镜 outset 必须外扩脏区：σ=(2,3) × 3 ⇒ 左右 +6、上下 +9。
///
/// 变异点：漏掉 `filter.bounds_for`，或把它写成 `filter.outsets()` 的加法而漏
/// 掉左上角位移。具体字面量在 scissor 裁掉模糊边缘时会变成可见的硬切边。
#[test]
fn damage_bounds_expands_for_filter() {
    let mut node = Node::from_element(&Element::new(NodeTag::Box).with(
        &props::FILTER,
        PropertyValue::filter(FilterChain::single(Filter::Blur {
            sigma_x: 2.0,
            sigma_y: 3.0,
        })),
    ));
    node.arranged_rect = Rect::new(10.0, 20.0, 100.0, 50.0);
    let ctx = dark_context();
    // 视觉矩形本身不带滤镜外扩。
    assert_eq!(
        visual_rect(&node, ctx, Point::ZERO),
        Rect::new(10.0, 20.0, 100.0, 50.0)
    );
    assert_eq!(
        damage_bounds(&node, ctx, Point::ZERO),
        Rect::new(4.0, 11.0, 112.0, 68.0)
    );
}

/// 玻璃装饰的外投影必须外扩脏区：radius=24 ⇒ r=60、offset=(0,4) ⇒
/// left=60 / top=56 / right=60 / bottom=64。
#[test]
fn damage_bounds_expands_for_decoration() {
    let mut node = Node::from_element(&Element::new(NodeTag::Box).with(
        &props::DECORATIONS,
        PropertyValue::decorations(Decorations::glass()),
    ));
    node.arranged_rect = Rect::new(10.0, 20.0, 100.0, 50.0);
    let ctx = dark_context();
    assert_eq!(
        damage_bounds(&node, ctx, Point::ZERO),
        Rect::new(-50.0, -36.0, 220.0, 170.0)
    );
}

/// 滤镜与装饰同时存在时两者**串联**外扩（先滤镜、再装饰），且 translate 平移
/// 整个结果。断言具体矩形，钉住"只算其中一个 outset"的变异。
#[test]
fn damage_bounds_composes_filter_and_decoration_outsets() {
    let mut node = Node::from_element(
        &Element::new(NodeTag::Box)
            .with(
                &props::FILTER,
                PropertyValue::filter(FilterChain::single(Filter::Blur {
                    sigma_x: 2.0,
                    sigma_y: 3.0,
                })),
            )
            .with(
                &props::DECORATIONS,
                PropertyValue::decorations(Decorations::glass()),
            ),
    );
    node.arranged_rect = Rect::new(10.0, 20.0, 100.0, 50.0);
    let ctx = dark_context();
    assert_eq!(
        damage_bounds(&node, ctx, Point::new(5.0, 7.0)),
        Rect::new(-51.0, -38.0, 232.0, 188.0)
    );
}

// ==================================================================
// text_visual_bounds —— 缓存命中 / 整形回退
// ==================================================================

/// 计数替身：缓存命中时 `shape` 一次都不该被调用。
struct CountingEngine {
    inner: NullEngine,
    shapes: usize,
}

impl Default for CountingEngine {
    fn default() -> Self {
        // `char_width=0` 会让替身产出零宽字形、被 `quads_from_shaped` 丢掉，
        // 那样缓存里的 `glyphs` 是空的，命中路径根本走不到。
        Self {
            inner: NullEngine { char_width: 8.0 },
            shapes: 0,
        }
    }
}

impl TextEngine for CountingEngine {
    fn shape(&mut self, text: &str, style: &TextStyle, max_width: Option<f32>) -> ShapedText {
        self.shapes += 1;
        self.inner.shape(text, style, max_width)
    }

    fn advance(&mut self, ch: char, style: &TextStyle) -> f32 {
        self.inner.advance(ch, style)
    }

    fn line_height(&mut self, style: &TextStyle) -> f32 {
        self.inner.line_height(style)
    }
}

impl TextRender for CountingEngine {
    fn glyph_quads(
        &mut self,
        shaped: &ShapedText,
        origin: Point,
        style: &TextStyle,
    ) -> Option<(TextureRef, Vec<GlyphQuad>)> {
        self.inner.glyph_quads(shaped, origin, style)
    }
}

/// 造一个带文本的盒子，并预填一份与当前指纹完全一致的 `glyph_quads` 缓存。
fn text_node_with_valid_cache(
    engine: &RefCell<CountingEngine>,
    ctx: Context<'_>,
) -> (Node, Vec<GlyphQuad>) {
    let mut node = Node::from_element(
        &Element::new(NodeTag::Text)
            .with(&props::TEXT, PropertyValue::static_str("Hello"))
            .with(&props::TEXT_SIZE, PropertyValue::float(12.0)),
    );
    node.arranged_rect = Rect::new(10.0, 20.0, 100.0, 20.0);

    let text = node.text().expect("前置：节点必须有文本").to_string();
    let style = node.text_style(ctx);
    let max_width = node.text_max_width(ctx);
    let revision = engine.borrow().atlas_revision();
    let shaped = engine.borrow_mut().shape(&text, &style, max_width);
    let (atlas, quads) = engine
        .borrow_mut()
        .glyph_quads(&shaped, Point::ZERO, &style)
        .expect("替身应给出非空字形");
    assert!(!quads.is_empty(), "前置：缓存里的字形不能为空");

    *node.glyph_quads.borrow_mut() = Some(Box::new(GlyphQuadCache {
        text: text.into(),
        style,
        style_fingerprint: 0,
        max_width,
        revision,
        atlas,
        glyphs: quads.clone().into(),
    }));
    (node, quads)
}

/// 缓存命中：直接返回字形四边形的并集（含 origin 平移），且不重复整形。
///
/// 变异点：漏掉 `!c.glyphs.is_empty()` 判据、把 `revision` 比较写反、或命中后
/// 仍调一次 `shape`。计数替身让"重复整形"从不可观察变成字面断言。
#[test]
fn text_visual_bounds_cache_hit_returns_cached_quads_without_reshaping() {
    let theme = Theme::dark();
    let engine = RefCell::new(CountingEngine::default());
    let ctx = Context::with_engine(&theme, &engine);
    let (node, quads) = text_node_with_valid_cache(&engine, ctx);
    let translate = Point::new(10.0, 20.0);
    // text_visual_bounds 的 origin 是「节点矩形 + translate」。
    let origin = Point::new(node.rect().x + translate.x, node.rect().y + translate.y);

    let before = engine.borrow().shapes;
    let bounds = text_visual_bounds(&node, ctx, translate).expect("命中应返回矩形");
    assert_eq!(engine.borrow().shapes, before, "缓存命中不该再整形");
    assert_eq!(bounds, union_of_quads(&quads, origin));
}

/// 缓存未命中（缓存被清掉）时走整形回退：矩形来自 `shaped.size`，且正好整形
/// 一次。变异点：`shaped.size.width > 0 || height > 0` 改成 `&&`，或掉进 None。
#[test]
fn text_visual_bounds_falls_back_to_shaping_when_cache_is_cleared() {
    let theme = Theme::dark();
    let engine = RefCell::new(CountingEngine::default());
    let ctx = Context::with_engine(&theme, &engine);
    let (node, _) = text_node_with_valid_cache(&engine, ctx);
    node.glyph_quads.borrow_mut().take();
    let translate = Point::new(10.0, 20.0);
    let origin = Point::new(node.rect().x + translate.x, node.rect().y + translate.y);

    let shaped = engine
        .borrow_mut()
        .shape("Hello", &node.text_style(ctx), None);
    let before = engine.borrow().shapes;
    let bounds = text_visual_bounds(&node, ctx, translate).expect("整形回退应返回矩形");
    assert_eq!(engine.borrow().shapes, before + 1, "未命中应正好整形一次");
    assert_eq!(
        bounds,
        Rect::new(origin.x, origin.y, shaped.size.width, shaped.size.height)
    );
}

/// 没有文本：即便挂了引擎也应返回 `None`（`node.text()?` 早退）。
#[test]
fn text_visual_bounds_returns_none_without_text() {
    let theme = Theme::dark();
    let engine = RefCell::new(CountingEngine::default());
    let ctx = Context::with_engine(&theme, &engine);
    let node = Node::from_element(&Element::new(NodeTag::Box));
    assert_eq!(text_visual_bounds(&node, ctx, Point::ZERO), None);
    assert_eq!(engine.borrow().shapes, 0, "无文本不该触发整形");
}
