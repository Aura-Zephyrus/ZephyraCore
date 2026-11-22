//! 回归：图集淘汰重排发生时，一帧 render 产出的所有文本 quad 必须属于
//! **同一份**图集布局（同一 revision）。
//!
//! 这是 ViewStage「设置页部分文字错乱」的根因测试。formerly：paint 逐节点调
//! `glyph_quads`，中途图集满触发淘汰重排 → 先算节点的 uv 失效 → 与最终上传的
//! 图集错位 → 前段文字串到别的字形槽位。修复后 render 会在 revision 变化时
//! 重跑到收敛。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use zephyra_core::context::Context;
use zephyra_core::core::Point;
use zephyra_core::element::Element;
use zephyra_core::id::NodeTag;
use zephyra_core::paint::{Command, DisplayList, GlyphQuad, TextureRef};
use zephyra_core::property::PropertyValue;
use zephyra_core::props;
use zephyra_core::text::atlas::AtlasRaster;
use zephyra_core::text::sdf::{from_coverage, SDF_BASE_PX, SDF_PADDING, SDF_SPREAD};
use zephyra_core::text::{
    GlyphSource, NullEngine, PlacedGlyph, ShapedText, TextEngine, TextRender, TextStack, TextStyle,
};
use zephyra_core::theme::{Theme, ThemeVariant};

/// 每个不同字符 → 不同字形 id，逼图集淘汰重排。
#[derive(Debug)]
struct DistinctSource;

impl GlyphSource for DistinctSource {
    fn glyph_id(&mut self, ch: char) -> u32 {
        ch as u32
    }
    fn advance(&mut self, _glyph_id: u32, _px_size: f32) -> f32 {
        10.0
    }
    fn rasterize(&mut self, _glyph_id: u32, px_size: u32) -> Option<AtlasRaster> {
        let n = (px_size / 4).max(8);
        let cov = vec![255u8; (n * n) as usize];
        Some(AtlasRaster {
            offset: Point::new(0.0, 0.0),
            width: n,
            height: n,
            sdf: from_coverage(&cov, n, n, SDF_PADDING),
            spread: SDF_SPREAD,
            base_px: SDF_BASE_PX as f32,
            is_bitmap: false,
        })
    }
    fn font_id(&self) -> u32 {
        0
    }
}

/// 包装真实 TextStack，记录每个文本最后一次 `glyph_quads` 时的图集 revision。
struct TrackingEngine {
    inner: TextStack<DistinctSource>,
    /// 文本标识 → 最后一次算出其 quad 时的 revision（调用后读）。
    last_rev: RefCell<HashMap<String, u64>>,
    /// 本趟观察到的最大 revision。
    max_rev: RefCell<u64>,
}

impl TrackingEngine {
    fn reset(&self) {
        self.last_rev.borrow_mut().clear();
        *self.max_rev.borrow_mut() = 0;
    }
}

impl TextEngine for TrackingEngine {
    fn shape(
        &mut self,
        text: &str,
        style: &TextStyle,
        max_width: Option<f32>,
    ) -> zephyra_core::text::ShapedText {
        self.inner.shape(text, style, max_width)
    }
    fn advance(&mut self, ch: char, style: &TextStyle) -> f32 {
        self.inner.advance(ch, style)
    }
    fn line_height(&mut self, style: &TextStyle) -> f32 {
        self.inner.line_height(style)
    }
}

impl TextRender for TrackingEngine {
    fn glyph_quads(
        &mut self,
        shaped: &zephyra_core::text::ShapedText,
        origin: Point,
        style: &TextStyle,
    ) -> Option<(TextureRef, Vec<GlyphQuad>)> {
        let key: String = shaped
            .lines
            .iter()
            .flat_map(|l| l.glyphs.iter())
            .map(|g: &PlacedGlyph| char::from_u32(g.glyph_id).unwrap_or('?'))
            .collect();
        let out = self.inner.glyph_quads(shaped, origin, style);
        if out.is_some() {
            let rev = self.inner.atlas_revision();
            self.last_rev.borrow_mut().insert(key, rev);
            let mut m = self.max_rev.borrow_mut();
            if rev > *m {
                *m = rev;
            }
        }
        out
    }
    fn atlas_revision(&self) -> u64 {
        self.inner.atlas_revision()
    }
}

/// 一组每个节点文本都不同的树；`base` 决定用哪段 Unicode 区间的字符。
fn many_texts(n: usize, base: u32) -> Element {
    let children: Vec<Element> = (0..n)
        .map(|i| {
            let ch = char::from_u32(base + i as u32).unwrap();
            Element::new(NodeTag::Box).with(
                &props::TEXT,
                PropertyValue::static_str(Box::leak(ch.to_string().into_boxed_str())),
            )
        })
        .collect();
    Element::new(NodeTag::Box)
        .with_key("root")
        .with_children(children)
}

fn render_tree(element: &Element, theme: &Theme, engine: &RefCell<TrackingEngine>) {
    let node = zephyra_core::node::Node::from_element(element);
    let ctx = Context::with_engine(theme, engine);
    let mut out = DisplayList::new();
    zephyra_core::paint::render(&node, ctx, &mut out);
    assert!(out
        .iter()
        .any(|i| matches!(i.command, Command::Text { .. })));
}

#[test]
fn all_text_quads_share_final_revision_after_eviction() {
    // 图集容量：px=14 ⇒ tier 64 ⇒ padded 32×32=1024px²/字形。
    // 224² ≈ 50K ⇒ 约 49 个字形。每帧 40 个能装下，但两帧累计 80 个会淘汰。
    let inner = TextStack::with_atlas_size(DistinctSource, 224);
    let engine = RefCell::new(TrackingEngine {
        inner,
        last_rev: RefCell::new(HashMap::new()),
        max_rev: RefCell::new(0),
    });
    let theme = Theme::builtin(ThemeVariant::Dark);

    // 第一帧：用 0x4E00 段填满图集（制造历史残留）。
    render_tree(&many_texts(40, 0x4E00), &theme, &engine);

    // 第二帧：换 0x5000 段 —— 新字形挤掉历史残留，触发淘汰重排。
    engine.borrow().reset();
    render_tree(&many_texts(40, 0x5000), &theme, &engine);

    let final_rev = engine.borrow().inner.atlas_revision();
    let last_rev = engine.borrow().last_rev.borrow().clone();
    let max_rev = *engine.borrow().max_rev.borrow();

    assert!(max_rev > 0, "第二帧应触发图集淘汰重排");

    // 核心回归判据：第二帧所有文本节点最后一次算 quad 时的 revision 必须
    // 等于最终值。若有节点停留在旧 revision，它的 uv 就属于旧布局 —— 正是
    // 截图里串位的文字。
    let bad: Vec<_> = last_rev
        .iter()
        .filter(|(_, rev)| **rev != final_rev)
        .collect();
    assert!(
        bad.is_empty(),
        "有 {} 个文本节点的 quad 停留在旧图集 revision（最终 {}）: {:?}",
        bad.len(),
        final_rev,
        bad
    );
}

// ============================ render_stable 的趟数门禁 ============================
//
// `render` 在 revision 变化时会重跑到收敛。行为正确时"跑几趟"没有可见差别 ——
// 但一旦趟数被改小（守卫反转 / 循环上界改动 / 前值读晚），症状正是本文件第一条
// 测试抓的 uv 串位。所以这里用一只 revision 可控的替身，把"趟数"变成可字面断言的量。

/// revision 可控的替身：`atlas_revision()` 返回 `min(glyph_quads 次数, cap)`。
///
/// 于是 `cap = N` 意味着 revision 会变化 N 次，render_stable 必须跑 N+1 趟才收敛
/// （第一趟 + N 次重跑）。真实图集的淘汰时序没法精确控制，而这只替身可以。
struct RevisionProbe {
    inner: NullEngine,
    cap: u64,
    /// 已发生的 `glyph_quads` 次数。
    quads: Cell<u64>,
    /// `shape` 次数。每个文本节点每趟恰好一次 ⇒ 等于"文本趟数"。
    shapes: Cell<usize>,
    /// `atlas_revision()` 的读取次数（用来钉无文本时的一趟早退）。
    revision_reads: Cell<usize>,
    /// 每次 `glyph_quads` 时 text_command 看到的 revision（即写入缓存的 revision）。
    revisions_at_quads: RefCell<Vec<u64>>,
    /// 最近一次 `atlas_revision()` 的返回值。
    last_revision: Cell<u64>,
}

impl RevisionProbe {
    fn new(cap: u64) -> Self {
        Self {
            inner: NullEngine { char_width: 8.0 },
            cap,
            quads: Cell::new(0),
            shapes: Cell::new(0),
            revision_reads: Cell::new(0),
            revisions_at_quads: RefCell::new(Vec::new()),
            last_revision: Cell::new(0),
        }
    }

    /// 不计数地读当前 revision（断言用；不污染 `revision_reads`）。
    fn current_revision(&self) -> u64 {
        self.quads.get().min(self.cap)
    }
}

impl TextEngine for RevisionProbe {
    fn shape(&mut self, text: &str, style: &TextStyle, max_width: Option<f32>) -> ShapedText {
        self.shapes.set(self.shapes.get() + 1);
        self.inner.shape(text, style, max_width)
    }
    fn advance(&mut self, ch: char, style: &TextStyle) -> f32 {
        self.inner.advance(ch, style)
    }
    fn line_height(&mut self, style: &TextStyle) -> f32 {
        self.inner.line_height(style)
    }
}

impl TextRender for RevisionProbe {
    fn glyph_quads(
        &mut self,
        shaped: &ShapedText,
        origin: Point,
        style: &TextStyle,
    ) -> Option<(TextureRef, Vec<GlyphQuad>)> {
        self.quads.set(self.quads.get() + 1);
        self.revisions_at_quads
            .borrow_mut()
            .push(self.last_revision.get());
        self.inner.glyph_quads(shaped, origin, style)
    }
    fn atlas_revision(&self) -> u64 {
        self.revision_reads.set(self.revision_reads.get() + 1);
        let rev = self.current_revision();
        self.last_revision.set(rev);
        rev
    }
}

/// 单个文本节点；不跑 layout（与 `render_tree` 同形），这样 `shape` 次数只来自绘制趟。
fn one_text(text: &'static str) -> Element {
    Element::new(NodeTag::Box)
        .with_key("root")
        .with(&props::TEXT, PropertyValue::static_str(text))
}

fn paint_probe(element: &Element, theme: &Theme, engine: &RefCell<RevisionProbe>) -> DisplayList {
    let node = zephyra_core::node::Node::from_element(element);
    let ctx = Context::with_engine(theme, engine);
    let mut out = DisplayList::new();
    zephyra_core::paint::render(&node, ctx, &mut out);
    out
}

fn probe_theme() -> Theme {
    Theme::builtin(ThemeVariant::Dark)
}

/// 无 Text 命令 ⇒ 一趟完成：`atlas_revision()` 只读一次（前值），随后早退。
///
/// 钉的变异：`if !out.any(Text)` 被删掉或反转 —— 那会多读一次 revision（终值），
/// 于是这里从 1 变成 2。
#[test]
fn render_without_text_exits_after_one_pass() {
    let engine = RefCell::new(RevisionProbe::new(3));
    let no_text = Element::new(NodeTag::Box)
        .with_key("root")
        .with(&props::WIDTH, PropertyValue::float(20.0))
        .with(&props::HEIGHT, PropertyValue::float(20.0))
        .with(
            &props::BACKGROUND,
            PropertyValue::color(zephyra_core::core::Color::rgb(1.0, 1.0, 1.0)),
        );

    let out = paint_probe(&no_text, &probe_theme(), &engine);
    assert!(
        !out.iter()
            .any(|i| matches!(i.command, Command::Text { .. })),
        "前提：这棵树不该有 Text 命令"
    );
    assert_eq!(
        engine.borrow().revision_reads.get(),
        1,
        "无文本时只读一次 revision（前值）就早退；读到 2 次说明早退守卫被删/反转"
    );
    assert_eq!(engine.borrow().shapes.get(), 0, "无文本时不该发生任何整形");
}

/// 有 Text 但 revision 不变 ⇒ 不重跑：恰好一趟，`shape` 一次。
///
/// 钉的变异：`if rev_after != rev_before` 被删掉 —— 那会无条件进入重跑循环，
/// 即使 revision 稳定也多跑一趟。
#[test]
fn render_with_text_and_stable_revision_runs_once() {
    let engine = RefCell::new(RevisionProbe::new(0));
    paint_probe(&one_text("stable"), &probe_theme(), &engine);

    assert_eq!(
        engine.borrow().shapes.get(),
        1,
        "revision 稳定时不该重跑：`shape` 应当恰好一次"
    );
    assert_eq!(
        engine.borrow().revision_reads.get(),
        3,
        "revision 稳定时：render_stable 的前值 + text_command 的一次 + 终值 = 3"
    );
}

/// 有 Text 且 revision 变化 ⇒ 重跑到收敛；`cap = 3` 要求恰好 4 趟。
///
/// 钉的变异：`for _ in 0..4` 的上界被改小（≤2 时第 3 次重跑不会发生，debug_assert
/// 直接 panic），以及 `rev_before` 被挪到 `render_rec` 之后（那样前值 == 终值，
/// 根本不会进重跑循环，`shape` 会停在 1）。
#[test]
fn render_reruns_until_revision_converges() {
    let engine = RefCell::new(RevisionProbe::new(3));
    paint_probe(&one_text("evict me"), &probe_theme(), &engine);

    let e = engine.borrow();
    assert_eq!(
        e.shapes.get(),
        4,
        "cap=3 ⇒ revision 变化 3 次 ⇒ 第一趟 + 3 次重跑 = 4 趟"
    );
    assert_eq!(e.current_revision(), 3, "终值应停在 cap=3");
    assert_eq!(
        e.revisions_at_quads.borrow().last().copied(),
        Some(3),
        "最后一次写入缓存的 revision 必须等于终值 —— 否则整份列表的 uv 不属于同一份图集"
    );
    assert_eq!(
        e.revision_reads.get(),
        12,
        "4 趟 ⇒ 前值(1) + 每趟 text_command(4) + 终值(1) + 3 次重跑的前后值(6) = 12"
    );
}

/// revision 永不收敛 ⇒ 跑满 4 次重跑后 debug_assert 暴露配置错误。
#[test]
#[should_panic(expected = "图集 revision 在 4 趟内未收敛")]
fn render_panics_when_revision_never_converges() {
    let engine = RefCell::new(RevisionProbe::new(u64::MAX));
    paint_probe(&one_text("never"), &probe_theme(), &engine);
}
