// 本文件由 node.rs 的 #[cfg(test)] mod node_prop_tests 挂载 —— 测试代码与生产代码分离。
// 模块是 node 的**子模块**，所以 use super::* 既拿到公开 API，也拿到 pub(crate) / 私有字段
// （dirty / moved / key / measure_cache / set_rect / cache_measure / source_arc 等）。

use super::*;
use crate::core::{Color, Size};
use crate::element::Element;
use crate::property::ValueSource;
use crate::props;
use std::sync::Arc;

// ============================== 辅助 ==============================

/// 一棵干净的单节点：`dirty = None`，没有测量缓存。
fn bare_node() -> Node {
    let mut node = Node::from_element(&Element::new(NodeTag::Box));
    node.dirty = Dirty::None;
    node
}

/// 塞一份**可辨认**的测量缓存，返回 (约束, 视口, 缓存尺寸) 供断言命中/失效。
fn cache(node: &mut Node) -> (Constraints, crate::core::Size, crate::core::Size) {
    let constraints = Constraints::fixed(Size::new(100.0, 50.0));
    let viewport = Size::new(800.0, 600.0);
    let result = MeasureResult {
        size: Size::new(30.0, 20.0),
        content: Size::new(40.0, 25.0),
    };
    node.cache_measure(constraints, viewport, result.clone());
    (constraints, viewport, result.size)
}

/// 一棵带 key 且**已摆好矩形**的树：
/// root("root") → [a("a"), mid("mid") → child(Id(7))]。
fn keyed_tree() -> Node {
    let element = Element::new(NodeTag::Box).with_key("root").with_children([
        Element::new(NodeTag::Control).with_key("a"),
        Element::new(NodeTag::Box)
            .with_key("mid")
            .with_children([Element::new(NodeTag::Control).with_key(7u64)]),
    ]);
    let mut root = Node::from_element(&element);
    root.set_rect(Rect::new(0.0, 0.0, 200.0, 100.0));
    root.children_mut()[0].set_rect(Rect::new(10.0, 20.0, 40.0, 60.0));
    root.children_mut()[1].set_rect(Rect::new(60.0, 5.0, 80.0, 30.0));
    root.children_mut()[1].children_mut()[0].set_rect(Rect::new(70.0, 10.0, 20.0, 10.0));
    root
}

fn color_of(value: &PropertyValue) -> Option<Color> {
    value.get::<Color>()
}

// ============================== set_styled_prop ==============================

/// 写同值不标脏、写异值标 Paint；且必须写在 **Styled** 层而不是 Local 层。
///
/// 钉的变异：忽略 `set_styled_if_changed` 的返回值（静止界面永远重绘）、
/// 把判定反过来（写同值才标脏）、写到错误的层（`lookup` 来源错）。
#[test]
fn set_styled_prop_same_value_is_noop_and_different_value_marks_paint() {
    let mut node = bare_node();
    let first = Color::rgb(0.1, 0.2, 0.3);
    node.set_styled_prop(&props::BACKGROUND, PropertyValue::color(first));
    assert_eq!(node.dirty(), Dirty::Paint, "首次写入必须标 Paint");
    let (value, source) = node.props().lookup(&props::BACKGROUND).unwrap();
    assert_eq!(source, ValueSource::Styled, "样式值必须落在 Styled 层");
    assert_eq!(color_of(value), Some(first));

    node.dirty = Dirty::None;
    node.set_styled_prop(&props::BACKGROUND, PropertyValue::color(first));
    assert_eq!(node.dirty(), Dirty::None, "同一值再写一遍必须完全安静");

    let second = Color::rgb(0.9, 0.8, 0.7);
    node.set_styled_prop(&props::BACKGROUND, PropertyValue::color(second));
    assert_eq!(node.dirty(), Dirty::Paint, "异值才标脏");
    assert_eq!(
        color_of(node.props().lookup(&props::BACKGROUND).unwrap().0),
        Some(second),
        "表里必须留下最后写入的值"
    );
}

/// `affects_layout == true` 的属性标 Layout，并清掉测量缓存。
///
/// 钉的变异：把 Layout 降级成 Paint（间距变了却不重排）、
/// 写了布局属性却忘记清缓存（排列趟拿旧尺寸摆子节点）。
#[test]
fn set_styled_prop_layout_key_marks_layout_and_clears_measure_cache() {
    let mut node = bare_node();
    let (constraints, viewport, _) = cache(&mut node);
    assert!(
        node.cached_measure(constraints, viewport).is_some(),
        "前置：缓存已写入"
    );

    node.set_styled_prop(&props::WIDTH, PropertyValue::float(42.0));
    assert_eq!(node.dirty(), Dirty::Layout, "Width 是布局属性");
    assert_eq!(
        node.cached_measure(constraints, viewport),
        None,
        "布局变化必须清测量缓存"
    );
    assert!(node.cached_size().is_none());
}

/// `affects_layout == false` 的属性标 Paint，**不**清测量缓存。
///
/// 钉的变异：无条件清缓存——颜色一改就把跨帧复用的测量结果扔掉，静止界面每帧重测整棵树。
#[test]
fn set_styled_prop_paint_key_keeps_measure_cache() {
    let mut node = bare_node();
    let (constraints, viewport, size) = cache(&mut node);

    node.set_styled_prop(
        &props::BACKGROUND,
        PropertyValue::color(Color::rgb(0.4, 0.5, 0.6)),
    );
    assert_eq!(node.dirty(), Dirty::Paint);
    assert_eq!(
        node.cached_measure(constraints, viewport),
        Some(size),
        "只重绘的属性不该让缓存失效"
    );
}

/// 四个绘制变换键都触发 `moved`；同值重写不触发。
///
/// `moved` 记的是「变了」而不是「变成非恒等」——回到 0 的那一帧同样要移走。
/// 钉的变异：`is_transform_key` 漏掉某一个键、无条件 `mark_moved`、
/// 只对值非零时 `mark_moved`。
#[test]
fn set_styled_prop_transform_keys_mark_moved_only_on_change() {
    for (key, value) in [
        (&props::TRANSLATE_X, PropertyValue::float(5.0)),
        (&props::TRANSLATE_Y, PropertyValue::float(5.0)),
        (&props::SCALE_X, PropertyValue::float(2.0)),
        (&props::SCALE_Y, PropertyValue::float(2.0)),
    ] {
        let mut node = bare_node();
        node.set_styled_prop(key, value.clone());
        assert!(node.moved(), "变换键 {:?} 变化必须标 moved", key);
        assert_eq!(node.dirty(), Dirty::Paint, "变换键是纯绘制属性");

        node.dirty = Dirty::None;
        node.moved = false;
        node.set_styled_prop(key, value);
        assert!(!node.moved(), "同值重写不该标 moved");
        assert_eq!(node.dirty(), Dirty::None);
    }
}

/// 借用版 `set_styled_prop_ref` 与按值版行为必须完全一致。
///
/// 两条路径的标脏逻辑是复制的；任何一处写偏（同值标脏 / 布局属性降级 /
/// 漏 `moved` / 漏清缓存）都会让「同一种属性在样式表里的效果和在源码里不同」。
#[test]
fn set_styled_prop_ref_mirrors_value_version() {
    let mut node = bare_node();
    let value = PropertyValue::color(Color::rgb(0.4, 0.5, 0.6));
    node.set_styled_prop_ref(&props::BACKGROUND, &value);
    assert_eq!(node.dirty(), Dirty::Paint);
    assert_eq!(
        color_of(node.props().lookup(&props::BACKGROUND).unwrap().0),
        Some(Color::rgb(0.4, 0.5, 0.6))
    );

    node.dirty = Dirty::None;
    node.set_styled_prop_ref(&props::BACKGROUND, &value);
    assert_eq!(node.dirty(), Dirty::None, "借用版写同值也必须安静");

    let (constraints, viewport, _) = cache(&mut node);
    node.set_styled_prop_ref(&props::WIDTH, &PropertyValue::float(77.0));
    assert_eq!(node.dirty(), Dirty::Layout);
    assert_eq!(node.cached_measure(constraints, viewport), None);

    node.set_styled_prop_ref(&props::SCALE_X, &PropertyValue::float(1.5));
    assert!(node.moved(), "借用版写变换键也必须标 moved");
}

/// 回到恒等值的那一帧仍然标 `moved`（判据是「变了」而非「当前非恒等」）。
/// 若写成后者，把手滑回 0 的落点帧会把旧位置的像素留在屏幕上（两个把手残影）。
#[test]
fn back_to_identity_transform_still_marks_moved() {
    let mut node = bare_node();
    node.set_styled_prop(&props::TRANSLATE_X, PropertyValue::float(20.0));
    assert!(node.moved());
    clear_dirty(&mut node);
    assert!(!node.moved());
    assert_eq!(node.dirty(), Dirty::None);

    node.set_styled_prop(&props::TRANSLATE_X, PropertyValue::float(0.0));
    assert!(node.moved(), "20 → 0 同样是移走");
}

// ============================== set_prop（本地写入，与样式层同源） ==============================

/// 本地写入的标脏分级、缓存失效与 `moved` 同样要成立。
/// 注意 `set_prop` 走 `set_local`（无条件返回变更记录），与样式层的「同值静默」不同。
#[test]
fn set_prop_local_write_marks_dirty_and_moved() {
    let mut node = bare_node();
    let (constraints, viewport, _) = cache(&mut node);
    node.set_prop(&props::WIDTH, PropertyValue::float(12.0));
    assert_eq!(node.dirty(), Dirty::Layout);
    assert_eq!(node.cached_measure(constraints, viewport), None);

    node.dirty = Dirty::None;
    node.set_prop(&props::WIDTH, PropertyValue::float(12.0));
    assert_eq!(
        node.dirty(),
        Dirty::Layout,
        "set_prop 走 PropertyStore::set_local（无条件返回变更记录），所以每次写都标脏；\
         稳态去重在 diff 的 set_local_if_changed_ref 那一侧"
    );

    let mut other = bare_node();
    other.set_prop(&props::TRANSLATE_Y, PropertyValue::float(3.0));
    assert!(other.moved());
    assert_eq!(other.dirty(), Dirty::Paint);
}

// ============================== retain_styled_props ==============================

/// 只保留指定样式键，其余被拆除；返回值是真正拆下的键；
/// 拆到布局键 ⇒ Layout + 清缓存；拆除不影响 Local 层。
#[test]
fn retain_styled_props_keeps_only_requested_and_marks_dirty() {
    let mut node = bare_node();
    node.props_mut().set_styled(
        &props::BACKGROUND,
        PropertyValue::color(Color::rgb(0.1, 0.2, 0.3)),
    );
    node.props_mut()
        .set_styled(&props::WIDTH, PropertyValue::float(10.0));
    let (constraints, viewport, _) = cache(&mut node);

    let removed = node.retain_styled_props(|k| std::ptr::eq(k, &props::BACKGROUND));
    assert_eq!(removed.len(), 1);
    assert!(
        std::ptr::eq(removed[0], &props::WIDTH),
        "返回的应是被拆的键"
    );
    assert_eq!(node.dirty(), Dirty::Layout, "拆掉 Width 必须重排");
    assert_eq!(node.cached_measure(constraints, viewport), None);
    assert_eq!(
        node.props().lookup(&props::BACKGROUND).unwrap().1,
        ValueSource::Styled,
        "被 keep 的样式值应保留"
    );
    assert!(
        node.props().lookup(&props::WIDTH).is_none(),
        "未 keep 的样式值应被拆掉"
    );
}

/// 没有任何键被拆时：不标脏、不标 moved、缓存不动、返回空表。
/// 钉的变异：每帧无条件标脏（apply 每帧都调 retain，静止界面永不静止）。
#[test]
fn retain_styled_props_no_removal_is_silent() {
    let mut node = bare_node();
    node.props_mut()
        .set_styled(&props::TRANSLATE_X, PropertyValue::float(8.0));
    let (constraints, viewport, size) = cache(&mut node);

    let removed = node.retain_styled_props(|_| true);
    assert!(removed.is_empty());
    assert_eq!(node.dirty(), Dirty::None);
    assert!(!node.moved());
    assert_eq!(node.cached_measure(constraints, viewport), Some(size));
}

/// 拆掉样式层的绘制变换键 = 变换变回默认 ⇒ 标 `moved`；变换键只重绘 ⇒ 缓存保留。
#[test]
fn retain_styled_props_removed_transform_marks_moved() {
    let mut node = bare_node();
    node.props_mut()
        .set_styled(&props::TRANSLATE_X, PropertyValue::float(8.0));
    let (constraints, viewport, size) = cache(&mut node);

    let removed = node.retain_styled_props(|_| false);
    assert_eq!(removed.len(), 1);
    assert!(std::ptr::eq(removed[0], &props::TRANSLATE_X));
    assert!(node.moved(), "拆掉变换值也必须标 moved");
    assert_eq!(node.dirty(), Dirty::Paint);
    assert_eq!(
        node.cached_measure(constraints, viewport),
        Some(size),
        "只重绘不清缓存"
    );
}

// ============================== retain_local_props ==============================

/// 本地属性同样「只保留声明过的」；拆到布局键 ⇒ Layout + 清缓存；样式层不受影响。
#[test]
fn retain_local_props_keeps_only_requested_and_marks_dirty() {
    let mut node = bare_node();
    node.set_prop(
        &props::BACKGROUND,
        PropertyValue::color(Color::rgb(0.2, 0.2, 0.2)),
    );
    node.set_prop(&props::WIDTH, PropertyValue::float(30.0));
    node.props_mut().set_styled(
        &props::BACKGROUND,
        PropertyValue::color(Color::rgb(0.7, 0.7, 0.7)),
    );
    node.dirty = Dirty::None;
    let (constraints, viewport, _) = cache(&mut node);

    let removed = node.retain_local_props(|k| std::ptr::eq(k, &props::BACKGROUND));
    assert_eq!(removed.len(), 1);
    assert!(std::ptr::eq(removed[0], &props::WIDTH));
    assert_eq!(node.dirty(), Dirty::Layout);
    assert_eq!(node.cached_measure(constraints, viewport), None);
    assert_eq!(
        node.props().lookup(&props::BACKGROUND).unwrap().1,
        ValueSource::Local,
        "本地 BACKGROUND 保留，且仍压过同键样式值"
    );
    assert!(node.props().lookup(&props::WIDTH).is_none());
}

/// 本地层没有键被拆时同样完全安静。
#[test]
fn retain_local_props_no_removal_is_silent() {
    let mut node = bare_node();
    node.set_prop(&props::WIDTH, PropertyValue::float(30.0));
    node.dirty = Dirty::None;
    let (constraints, viewport, size) = cache(&mut node);

    let removed = node.retain_local_props(|_| true);
    assert!(removed.is_empty());
    assert_eq!(node.dirty(), Dirty::None);
    assert_eq!(node.cached_measure(constraints, viewport), Some(size));
}

/// 拆掉本地绘制属性 ⇒ Paint，缓存保留。
#[test]
fn retain_local_props_removed_paint_keeps_cache() {
    let mut node = bare_node();
    node.set_prop(
        &props::BACKGROUND,
        PropertyValue::color(Color::rgb(0.5, 0.5, 0.5)),
    );
    node.dirty = Dirty::None;
    let (constraints, viewport, size) = cache(&mut node);

    let removed = node.retain_local_props(|_| false);
    assert_eq!(removed.len(), 1);
    assert!(std::ptr::eq(removed[0], &props::BACKGROUND));
    assert_eq!(node.dirty(), Dirty::Paint);
    assert_eq!(node.cached_measure(constraints, viewport), Some(size));
}

/// 本地层拆键**不**在本方法里标 `moved`：diff 的调用点才负责标（见 diff.rs）。
/// 钉的变异：把样式层的 `mark_moved` 复制进本地层，令 diff 的语义重复/错位。
#[test]
fn retain_local_props_does_not_mark_moved_for_transform() {
    let mut node = bare_node();
    node.set_prop(&props::TRANSLATE_X, PropertyValue::float(8.0));
    // set_prop 写变换键本身就会标 moved，先把本帧状态清干净再测 retain 的行为。
    clear_dirty(&mut node);

    let removed = node.retain_local_props(|_| false);
    assert_eq!(removed.len(), 1);
    assert!(std::ptr::eq(removed[0], &props::TRANSLATE_X));
    assert_eq!(node.dirty(), Dirty::Paint);
    assert!(!node.moved(), "本地拆变换的 moved 由 diff 负责，不是本方法");
}

// ============================== 节点查询 ==============================

/// 按名字 / 整数 id 深搜命中；找不到返回 None；命中根自身也算。
#[test]
fn find_by_name_and_id_and_missing_keys() {
    let root = keyed_tree();

    let a = root.find(&ElementKey::Name("a")).expect("名字 key 应命中");
    assert_eq!(a.key(), Some(&ElementKey::Name("a")));
    assert_eq!(a.rect(), Rect::new(10.0, 20.0, 40.0, 60.0));

    let nested = root
        .find(&ElementKey::Id(7))
        .expect("整数 id 应能深搜到孙节点");
    assert_eq!(nested.key(), Some(&ElementKey::Id(7)));
    assert_eq!(nested.rect(), Rect::new(70.0, 10.0, 20.0, 10.0));

    assert!(root.find(&ElementKey::Name("missing")).is_none());
    assert!(root.find(&ElementKey::Id(99)).is_none());
    assert_eq!(
        root.find(&ElementKey::Name("root")).unwrap().rect(),
        Rect::new(0.0, 0.0, 200.0, 100.0),
        "find 先检查自己"
    );
}

/// 重复 key：先检查自身，再按深度优先文档序取第一个。
/// 钉的变异：用 `rfind` / 逆序遍历 / 先搜孩子再搜自己。
#[test]
fn find_prefers_root_then_document_order_on_duplicate_keys() {
    let mut root =
        Node::from_element(
            &Element::new(NodeTag::Box).with_children([
                Element::new(NodeTag::Control).with_key("dup"),
                Element::new(NodeTag::Box)
                    .with_children([Element::new(NodeTag::Control).with_key("dup")]),
            ]),
        );
    root.children_mut()[0].set_rect(Rect::new(1.0, 0.0, 1.0, 1.0));
    root.children_mut()[1].children_mut()[0].set_rect(Rect::new(5.0, 0.0, 1.0, 1.0));

    assert_eq!(
        root.find(&ElementKey::Name("dup")).unwrap().rect(),
        Rect::new(1.0, 0.0, 1.0, 1.0),
        "同层兄弟在前的先命中，不应取嵌套的那个"
    );

    root.key = Some(ElementKey::Name("dup"));
    root.set_rect(Rect::new(0.0, 0.0, 100.0, 100.0));
    assert_eq!(
        root.find(&ElementKey::Name("dup")).unwrap().rect(),
        Rect::new(0.0, 0.0, 100.0, 100.0),
        "自身优先于任何后代"
    );
}

/// `find_rect` 就是命中节点的已摆放矩形，缺失返回 None。
#[test]
fn find_rect_returns_arranged_rect_or_none() {
    let root = keyed_tree();
    assert_eq!(
        root.find_rect(&ElementKey::Name("mid")),
        Some(Rect::new(60.0, 5.0, 80.0, 30.0))
    );
    assert_eq!(
        root.find_rect(&ElementKey::Name("a")),
        Some(Rect::new(10.0, 20.0, 40.0, 60.0))
    );
    assert_eq!(root.find_rect(&ElementKey::Name("nope")), None);
}

/// 按文档序收集全部带 key 节点的矩形，并且**追加**到调用方的 vec（不清空）。
/// 钉的变异：漏掉根、逆序、清空入参、只收集有 INTERACTIVE 属性的节点。
#[test]
fn collect_keyed_rects_in_document_order_and_appends() {
    let root = keyed_tree();
    let mut out = vec![(
        ElementKey::Name("sentinel"),
        Rect::new(-1.0, -1.0, 1.0, 1.0),
    )];

    root.collect_keyed_rects(&mut out);

    assert_eq!(out.len(), 5, "1 个哨兵 + 4 个带 key 节点");
    assert_eq!(out[0].0, ElementKey::Name("sentinel"));
    assert_eq!(out[1].0, ElementKey::Name("root"));
    assert_eq!(out[1].1, Rect::new(0.0, 0.0, 200.0, 100.0));
    assert_eq!(out[2].0, ElementKey::Name("a"));
    assert_eq!(out[2].1, Rect::new(10.0, 20.0, 40.0, 60.0));
    assert_eq!(out[3].0, ElementKey::Name("mid"));
    assert_eq!(out[3].1, Rect::new(60.0, 5.0, 80.0, 30.0));
    assert_eq!(out[4].0, ElementKey::Id(7));
    assert_eq!(out[4].1, Rect::new(70.0, 10.0, 20.0, 10.0));
}

/// 中心点 = 矩形几何中心（不是左上角、不是相对坐标）。
#[test]
fn center_is_geometric_center() {
    let root = keyed_tree();

    let center = root.center();
    assert_eq!((center.x, center.y), (100.0, 50.0));

    let a = root.find(&ElementKey::Name("a")).unwrap();
    let center = a.center();
    assert_eq!((center.x, center.y), (30.0, 50.0));

    let nested = root.find(&ElementKey::Id(7)).unwrap();
    let center = nested.center();
    assert_eq!((center.x, center.y), (80.0, 15.0));
}

// ============================== take_only_child ==============================

#[test]
fn take_only_child_returns_the_single_child() {
    let mut root = Node::from_element(
        &Element::new(NodeTag::Box)
            .with_children([Element::new(NodeTag::Control).with_key("only")]),
    );
    root.children_mut()[0].set_rect(Rect::new(3.0, 4.0, 5.0, 6.0));

    let child = root.take_only_child();
    assert_eq!(child.key(), Some(&ElementKey::Name("only")));
    assert_eq!(child.rect(), Rect::new(3.0, 4.0, 5.0, 6.0));
    assert!(root.children().is_empty(), "取走后父节点应无子节点");
}

#[test]
#[should_panic(expected = "take_only_child 要求恰好一个子节点")]
fn take_only_child_panics_when_empty() {
    let mut root = Node::from_element(&Element::new(NodeTag::Box));
    let _ = root.take_only_child();
}

#[test]
#[should_panic(expected = "take_only_child 要求恰好一个子节点")]
fn take_only_child_panics_when_multiple() {
    let mut root = Node::from_element(&Element::new(NodeTag::Box).with_children([
        Element::new(NodeTag::Control),
        Element::new(NodeTag::Control),
    ]));
    let _ = root.take_only_child();
}

// ============================== moved / P2c 共享 Arc ==============================

/// `mark_moved` 置位，`clear_dirty` 连子树一起清掉（下一帧重新判「变了」）。
#[test]
fn mark_moved_and_clear_dirty_toggle_flag() {
    let mut root = Node::from_element(
        &Element::new(NodeTag::Box).with_children([Element::new(NodeTag::Control)]),
    );
    assert!(!root.moved());

    root.mark_moved();
    assert!(root.moved());
    clear_dirty(&mut root);
    assert!(!root.moved());

    root.children_mut()[0].mark_moved();
    assert!(root.children()[0].moved());
    clear_dirty(&mut root);
    assert!(!root.children()[0].moved());
}

/// `from_element_arc` 保留源 Arc 身份；`source_arc` / `set_source_arc` 读写同一份。
/// P2c 的「子树输入没变就跳过 diff」完全靠 `Arc::ptr_eq` 判这个身份。
#[test]
fn from_element_arc_preserves_source_identity() {
    let child = Arc::new(Element::new(NodeTag::Control).with_key("child"));
    let parent = Arc::new(Element::new(NodeTag::Box).with_shared_children([child.clone()]));
    let mut node = Node::from_element_arc(parent.clone());

    assert!(
        Arc::ptr_eq(node.source_arc().unwrap(), &parent),
        "根应保留源 Arc"
    );
    let child_node = &node.children()[0];
    assert!(
        Arc::ptr_eq(child_node.source_arc().unwrap(), &child),
        "子节点也应各自保留源 Arc"
    );

    let twin = Arc::new(Element::new(NodeTag::Control).with_key("child"));
    assert!(
        !Arc::ptr_eq(child_node.source_arc().unwrap(), &twin),
        "内容相同但不是同一个 Arc ⇒ 不可跳过"
    );

    node.set_source_arc(Some(twin.clone()));
    assert!(Arc::ptr_eq(node.source_arc().unwrap(), &twin));
    node.set_source_arc(None);
    assert!(node.source_arc().is_none());

    assert!(
        Node::from_element(&Element::new(NodeTag::Box))
            .source_arc()
            .is_none(),
        "非 Arc 入口没有源身份"
    );
}

/// 真正走一次 P2c 跳过：子节点的源 Arc 与声明树里的 Arc 相同 ⇒ diff 不下探，
/// 节点上「人为注入的、与声明不同的本地值」必须原样留下、脏标记不变。
///
/// 钉的变异：diff 里删掉 `Arc::ptr_eq` 判据（或 node 的 source_arc 读错来源）——
/// 那样子节点会被重新对账，注入值被覆盖且标脏。
#[test]
fn shared_arc_child_is_skipped_by_diff() {
    let original = Color::rgb(0.1, 0.2, 0.3);
    let injected = Color::rgb(0.9, 0.8, 0.7);
    let shared_child: Arc<Element> = Arc::new(
        Element::new(NodeTag::Control)
            .with_key("child")
            .with(&props::BACKGROUND, PropertyValue::color(original)),
    );
    let element = Arc::new(Element::new(NodeTag::Box).with_shared_children([shared_child.clone()]));
    let mut root = Node::from_element_arc(element.clone());
    assert_eq!(root.children().len(), 1);

    root.children_mut()[0].set_prop(&props::BACKGROUND, PropertyValue::color(injected));
    clear_dirty(&mut root);
    assert_eq!(
        color_of(
            root.children()[0]
                .props()
                .lookup(&props::BACKGROUND)
                .unwrap()
                .0
        ),
        Some(injected),
        "前置：注入了与声明不同的本地值"
    );

    let _stats = crate::diff::diff(&mut root, element.as_ref());

    let child = &root.children()[0];
    assert_eq!(child.key(), Some(&ElementKey::Name("child")));
    assert_eq!(
        color_of(child.props().lookup(&props::BACKGROUND).unwrap().0),
        Some(injected),
        "同 Arc ⇒ 跳过 diff，注入值不应被声明值覆盖"
    );
    assert_eq!(child.dirty(), Dirty::None, "跳过路径不应给子节点标脏");
}
