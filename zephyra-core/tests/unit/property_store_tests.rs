// 本文件由 property.rs 的 #[cfg(test)] mod store_tests 抽出 —— 测试代码与生产代码分离。
// 模块路径不变（src 里的声明用 #[path] 指过来），所以 use super::* 与私有可见性都照旧。

use super::*;

use std::any::Any;
use std::sync::Arc;

use crate::core::{Color, CornerShape, Rect, UvRect};
use crate::decoration::Decorations;
use crate::filter::{BlendMode, FilterChain, Material, MaterialParams};
use crate::layout::{
    AlignContent, AlignItems, AlignSelf, Anchor, Direction, FlexDirection, FlexWrap, MainAxisAlign,
    Overflow, Position,
};
use crate::paint::Layer;

// ⚠ `static` 而不是 `const`：`&CONST` 每次引用都**创建一个临时值**，
// 于是 `resolve(&store, &A, None)` 返回的借用指向临时 —— 编译期就报
// "temporary value dropped while borrowed"。`static` 才给出 `'static` 引用。
static A: PropertyKey = PropertyKey::new("Layout.Width", PropertyValue::float(0.0));
static B: PropertyKey = PropertyKey::new("Paint.Opacity", PropertyValue::float(1.0));

/// 重复给**同一个** key 设值：表里必须留下**最后那个**。
///
/// # 这条在钉什么
///
/// 覆盖式的写入（upsert）有个"表里留哪一份"的隐含决定，而它没有任何
/// 可见症状 —— 表里的值错了，`resolve` 照样返回 `ValueSource::Local`，
/// 断言"来源"的那批测试全过，只有"值"不对。
///
/// 历史事实：本 crate 曾用三个 `HashMap`（值 / 样式值 / 名字）实现，
/// 换 `Vec` 时我在"用 `mem::replace` 交换以避免 clone"这一步写反了顺序，
/// 结果是**旧值留在表里**。现有 286 条测试**全部照过** ——
/// 因为没有一条测过"同一 key 设两次"。这条就是那次遗漏的补上。
#[test]
fn last_value_wins_when_setting_same_property_twice() {
    let mut store = PropertyStore::new();
    store.set_local(&A, PropertyValue::float(1.0));
    store.set_local(&A, PropertyValue::float(2.0));
    store.set_local(&A, PropertyValue::float(3.0));

    let (value, source) = resolve(&store, &A, None);
    assert_eq!(source, ValueSource::Local);
    assert_eq!(
        value.get::<f32>(),
        Some(3.0),
        "同一属性设了三次（1/2/3），表里应当是 3.0，实得 {:?}",
        value.get::<f32>()
    );
    assert_eq!(
        store
            .local_value(PropertyStore::id_of(&A))
            .unwrap()
            .get::<f32>(),
        Some(3.0)
    );
    assert_eq!(store.iter_local().count(), 1, "重复设不应增加条目数");
}

/// `PropertyChange` 的 `new` 必须与表里最终留的值一致。
///
/// 这两条要一起看：`new` 用来驱动"要不要重绘"，而表里的值用来实际画。
/// 两者不一致的症状是「界面停在旧值但重绘计数在涨」——
/// 有动画时表现为**永远追不上**，静止时看不出来。
#[test]
fn change_log_value_matches_table() {
    let mut store = PropertyStore::new();
    let first = store.set_local(&A, PropertyValue::float(10.0)).unwrap();
    assert_eq!(
        first.old.as_ref().and_then(|v| v.get::<f32>()),
        None,
        "首次设置没有旧值"
    );
    assert_eq!(first.new.as_ref().unwrap().get::<f32>(), Some(10.0));

    let second = store.set_local(&A, PropertyValue::float(20.0)).unwrap();
    assert_eq!(second.old.as_ref().unwrap().get::<f32>(), Some(10.0));
    assert_eq!(
        second.new.as_ref().unwrap().get::<f32>(),
        store
            .local_value(PropertyStore::id_of(&A))
            .unwrap()
            .get::<f32>(),
        "`new` 与表里的值不一致"
    );

    // 清掉之后再设：旧值应回到 None（而不是残留）。
    store.clear_local(&A);
    assert!(!store.is_locally_set(&A));
    let third = store.set_local(&A, PropertyValue::float(30.0)).unwrap();
    assert_eq!(third.old, None, "清除后再设，旧值应当是 None");
}

/// 本地值与样式值相互独立，且本地压过样式。
///
/// 两者曾分别是两张表；合成一个 `Vec` 类型后最容易出的错是**共用了同一条
/// 查找**（于是样式值被当成本地值、或本地设置把样式值覆盖掉）。
#[test]
fn local_and_styled_do_not_interfere() {
    let mut store = PropertyStore::new();
    store.set_styled(&A, PropertyValue::float(1.0));
    store.set_local(&A, PropertyValue::float(2.0));
    assert_eq!(resolve(&store, &A, None).1, ValueSource::Local);
    assert_eq!(resolve(&store, &A, None).0.get::<f32>(), Some(2.0));

    // 另一个 key 只有样式值。
    store.set_styled(&B, PropertyValue::float(0.5));
    assert_eq!(resolve(&store, &B, None).1, ValueSource::Styled);

    // 清除本地后应回落到样式值，而不是"什么都没有"。
    store.clear_local(&A);
    let (value, source) = resolve(&store, &A, None);
    assert_eq!(source, ValueSource::Styled);
    assert_eq!(value.get::<f32>(), Some(1.0));
}

/// `set_local_by_id` 与 `set_local` 走的是两条路径，但结果必须一致。
///
/// 两条路径的原因：`set_local` 拿得到 `&'static PropertyKey`（于是知道名字），
/// 而 diff 只有 id。这条钉的是"id 路径不会因为缺名字而丢掉值或写错地方"。
#[test]
fn write_by_id_matches_write_by_key() {
    let mut by_key = PropertyStore::new();
    by_key.set_local(&A, PropertyValue::float(7.0));
    let mut by_id = PropertyStore::new();
    // ⚠️ 按 id 写入一条**从未登记过**的键现在**写不进去**（返回 `false`）。
    // 旧实现会造一条名字叫 `Layout.Unknown` 的假记录凑合，那永远查不出
    // 「这条其实不影响布局」，只是一律多重排；B1 之后 `LocalEntry` 存的就是
    // 键本身，造不出假键，所以这里改成「不写」。
    assert!(
        !by_id.set_local_by_id(PropertyStore::id_of(&A), PropertyValue::float(7.0)),
        "只持 id 无法反推键，所以写不进去——返回 false 好过写一条分类靠猜的条目"
    );
    assert_eq!(by_id.iter_local().count(), 0, "写不进去就不该留下条目");
}

// ============================== PropertyValue::PartialEq / Debug / get ==============================

// 两个**不同**的函数指针：实现刻意写成语义等价（都是 i32 相等判断），
// 只有地址不同 —— 于是「同类型 + 同值」但 fn ptr 不同时，唯一能把它们分开的
// 就是 `std::ptr::eq(*eq, *eqi)` 那一行。
fn eq_i32(a: &dyn Any, b: &dyn Any) -> bool {
    a.downcast_ref::<i32>() == b.downcast_ref::<i32>()
}

fn eq_i32_other(a: &dyn Any, b: &dyn Any) -> bool {
    b.downcast_ref::<i32>() == a.downcast_ref::<i32>()
}

/// `Custom` 的相等性必须先看 **eq 函数指针**：同类型同值但两个不同的 eq 函数 ⇒ 不等。
#[test]
fn custom_eq_requires_the_same_eq_function_pointer() {
    let a = PropertyValue::Custom {
        inner: Arc::new(5i32),
        eq: eq_i32,
    };
    let b = PropertyValue::Custom {
        inner: Arc::new(5i32),
        eq: eq_i32_other,
    };
    assert!(a != b, "同类型同值但 eq 指针不同必须判不等");
    assert!(a == a.clone(), "同值且 eq 指针相同才判等");
}

/// 回归：`Custom` 的 type_id 必须取自 `Arc` 里的 trait object（内部具体类型），
/// 而不是 `Arc<dyn Any + Send + Sync>` 这个包装类型本身——后者是 `'static` 具体类型，
/// type_id 恒相同。修复前两个不同类型但共用同一 eq 函数的值会判等。
#[test]
fn custom_eq_rejects_different_inner_types_even_with_same_eq_pointer() {
    fn eq_always(_a: &dyn std::any::Any, _b: &dyn std::any::Any) -> bool {
        true
    }
    let a = PropertyValue::Custom {
        inner: Arc::new(5i32),
        eq: eq_always,
    };
    let b = PropertyValue::Custom {
        inner: Arc::new(5i64),
        eq: eq_always,
    };
    assert!(
        a != b,
        "内部具体类型不同必须判不等，即使 eq 函数指针相同且 eq 恒返回 true"
    );
}

/// `Custom` 的相等性必须**真的调用**携带的 eq 函数，而不是只比指针/类型。
#[test]
fn custom_eq_calls_the_carried_eq_function() {
    let a = PropertyValue::Custom {
        inner: Arc::new(5i32),
        eq: eq_i32,
    };
    let different = PropertyValue::Custom {
        inner: Arc::new(6i32),
        eq: eq_i32,
    };
    assert!(a != different, "同类型同 eq 指针但值不同 ⇒ 不等");
    assert!(
        a == PropertyValue::Custom {
            inner: Arc::new(5i32),
            eq: eq_i32,
        },
        "同类型同 eq 指针且值相同 ⇒ 等"
    );
}

/// 每个变体的 `Debug` 输出都是钉死的契约：属性 diff 失败时全靠它定位。
#[test]
fn debug_output_pins_scalar_string_and_enum_variants() {
    assert_eq!(format!("{:?}", PropertyValue::float(1.5)), "1.5");
    assert_eq!(format!("{:?}", PropertyValue::int(42)), "42");
    assert_eq!(format!("{:?}", PropertyValue::bool(true)), "true");
    assert_eq!(
        format!("{:?}", PropertyValue::Str(Arc::from("hi"))),
        "\"hi\""
    );
    assert_eq!(format!("{:?}", PropertyValue::static_str("hey")), "\"hey\"");
    assert_eq!(
        format!("{:?}", PropertyValue::color(Color::rgb(1.0, 0.0, 0.0))),
        "Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }"
    );
    assert_eq!(
        format!("{:?}", PropertyValue::direction(Direction::Vertical)),
        "Vertical"
    );
    assert_eq!(
        format!("{:?}", PropertyValue::cross_align(AlignItems::Center)),
        "Center"
    );
    assert_eq!(
        format!(
            "{:?}",
            PropertyValue::main_align(MainAxisAlign::SpaceBetween)
        ),
        "SpaceBetween"
    );
    assert_eq!(
        format!("{:?}", PropertyValue::align_content(AlignContent::Stretch)),
        "Stretch"
    );
    assert_eq!(
        format!("{:?}", PropertyValue::align_self(AlignSelf::Auto)),
        "Auto"
    );
    assert_eq!(
        format!("{:?}", PropertyValue::flex_wrap(FlexWrap::WrapReverse)),
        "WrapReverse"
    );
    assert_eq!(
        format!("{:?}", PropertyValue::flex_direction(FlexDirection::Column)),
        "Column"
    );
    assert_eq!(format!("{:?}", PropertyValue::layer(Layer::Popup)), "Popup");
    assert_eq!(
        format!("{:?}", PropertyValue::position(Position::Root)),
        "Root"
    );
    assert_eq!(
        format!("{:?}", PropertyValue::overflow(Overflow::Hidden)),
        "Hidden"
    );
    assert_eq!(
        format!("{:?}", PropertyValue::anchor(Anchor::BottomRight)),
        "BottomRight"
    );
    assert_eq!(
        format!("{:?}", PropertyValue::blend_mode(BlendMode::Plus)),
        "Plus"
    );
    assert_eq!(
        format!("{:?}", PropertyValue::material(Material::Frosted)),
        "Frosted"
    );
}

/// 复合变体的 `Debug` 输出：滤镜报项数、装饰报哪几件在场、矩形取整、资源带名字。
#[test]
fn debug_output_pins_composite_variants() {
    assert_eq!(
        format!("{:?}", PropertyValue::filter(FilterChain::none())),
        "Filter(0 项)"
    );
    assert_eq!(
        format!(
            "{:?}",
            PropertyValue::material_params(MaterialParams::none())
        ),
        "MaterialParams { amount: None, bezel: None, dispersion: None, ior: None }"
    );
    assert_eq!(
        format!("{:?}", PropertyValue::decorations(Decorations::none())),
        "Decorations(highlight=false shadow=false inner=false)"
    );
    assert_eq!(
        format!("{:?}", PropertyValue::decorations(Decorations::glass())),
        "Decorations(highlight=true shadow=true inner=true)"
    );
    assert_eq!(
        format!(
            "{:?}",
            PropertyValue::uv_rect(UvRect::new(0.0, 0.0, 1.0, 1.0))
        ),
        "UvRect { x: 0.0, y: 0.0, width: 1.0, height: 1.0 }"
    );
    assert_eq!(
        format!("{:?}", PropertyValue::corner_shape(CornerShape::ROUND)),
        "CornerShape(k=1)"
    );
    assert_eq!(
        format!("{:?}", PropertyValue::rect(Rect::new(1.0, 2.0, 30.0, 40.0))),
        "Rect(1,2,30x40)"
    );
    assert_eq!(
        format!("{:?}", PropertyValue::Resource(Arc::from("Brush.Accent"))),
        "Resource(Brush.Accent)"
    );
    assert_eq!(format!("{:?}", PropertyValue::custom(5i32)), "Custom");
}

/// `get::<T>()` 对每个变体都要向下转型成**具体**类型并返回克隆值。
#[test]
fn get_downcasts_every_variant_to_its_concrete_type() {
    assert_eq!(PropertyValue::float(1.5).get::<f32>(), Some(1.5));
    assert_eq!(PropertyValue::int(7).get::<i32>(), Some(7));
    assert_eq!(PropertyValue::bool(true).get::<bool>(), Some(true));
    assert_eq!(
        PropertyValue::Str(Arc::from("hello"))
            .get::<Arc<str>>()
            .as_deref(),
        Some("hello")
    );
    assert_eq!(
        PropertyValue::static_str("hi").get::<&'static str>(),
        Some("hi")
    );
    let red = Color::rgb(1.0, 0.0, 0.0);
    assert_eq!(PropertyValue::color(red).get::<Color>(), Some(red));
    assert_eq!(
        PropertyValue::direction(Direction::Horizontal).get::<Direction>(),
        Some(Direction::Horizontal)
    );
    assert_eq!(
        PropertyValue::cross_align(AlignItems::End).get::<AlignItems>(),
        Some(AlignItems::End)
    );
    assert_eq!(
        PropertyValue::main_align(MainAxisAlign::Center).get::<MainAxisAlign>(),
        Some(MainAxisAlign::Center)
    );
    assert_eq!(
        PropertyValue::align_content(AlignContent::End).get::<AlignContent>(),
        Some(AlignContent::End)
    );
    assert_eq!(
        PropertyValue::align_self(AlignSelf::Stretch).get::<AlignSelf>(),
        Some(AlignSelf::Stretch)
    );
    assert_eq!(
        PropertyValue::flex_wrap(FlexWrap::Wrap).get::<FlexWrap>(),
        Some(FlexWrap::Wrap)
    );
    assert_eq!(
        PropertyValue::flex_direction(FlexDirection::Row).get::<FlexDirection>(),
        Some(FlexDirection::Row)
    );
    assert_eq!(
        PropertyValue::layer(Layer::Overlay).get::<Layer>(),
        Some(Layer::Overlay)
    );
    assert_eq!(
        PropertyValue::position(Position::Parent).get::<Position>(),
        Some(Position::Parent)
    );
    assert_eq!(
        PropertyValue::overflow(Overflow::Auto).get::<Overflow>(),
        Some(Overflow::Auto)
    );
    assert_eq!(
        PropertyValue::anchor(Anchor::TopLeft).get::<Anchor>(),
        Some(Anchor::TopLeft)
    );
    assert_eq!(
        PropertyValue::filter(FilterChain::none()).get::<FilterChain>(),
        Some(FilterChain::none())
    );
    assert_eq!(
        PropertyValue::blend_mode(BlendMode::SrcOver).get::<BlendMode>(),
        Some(BlendMode::SrcOver)
    );
    assert_eq!(
        PropertyValue::material(Material::Solid).get::<Material>(),
        Some(Material::Solid)
    );
    assert_eq!(
        PropertyValue::material_params(MaterialParams::none()).get::<MaterialParams>(),
        Some(MaterialParams::none())
    );
    assert_eq!(
        PropertyValue::decorations(Decorations::none()).get::<Decorations>(),
        Some(Decorations::none())
    );
    assert_eq!(
        PropertyValue::uv_rect(UvRect::FULL).get::<UvRect>(),
        Some(UvRect::FULL)
    );
    assert_eq!(
        PropertyValue::corner_shape(CornerShape::SQUIRCLE).get::<CornerShape>(),
        Some(CornerShape::SQUIRCLE)
    );
    let r = Rect::new(1.0, 2.0, 3.0, 4.0);
    assert_eq!(PropertyValue::rect(r).get::<Rect>(), Some(r));
    assert_eq!(PropertyValue::custom(9i32).get::<i32>(), Some(9));
}

/// `get` 的两种 `None`：类型不符，以及资源键（尚未解析的中间态）。
#[test]
fn get_returns_none_for_wrong_type_and_for_resource_keys() {
    assert_eq!(PropertyValue::float(1.5).get::<i32>(), None);
    assert_eq!(PropertyValue::int(7).get::<f32>(), None);
    assert_eq!(PropertyValue::custom(9i32).get::<u32>(), None);
    assert_eq!(
        PropertyValue::Resource(Arc::from("Brush.Accent")).get::<Arc<str>>(),
        None,
        "资源键不是已解析的值，任何 T 都取不到"
    );
}

/// `name_affects_layout` 是 B0 时期的名字名单：只认精确名字，不再有 `Layout.` 前缀捷径。
#[test]
fn name_affects_layout_accepts_only_exact_listed_names() {
    for name in [
        "Padding",
        "Text.LineHeight",
        "Width",
        "Height",
        "Spacing",
        "Collapsed",
        "Direction",
        "CrossAlign",
        "MainAlign",
        "Text.Size",
        "Text.MaxWidth",
        "Text.LetterSpacing",
        "Text.Path",
        "FlexGrow",
        "FlexBasis",
        "Order",
        "Position",
        "Left",
        "Top",
        "Anchor",
    ] {
        assert!(name_affects_layout(name), "{name} 必须在名单里");
    }
}

#[test]
fn name_affects_layout_rejects_lookalikes_and_old_prefix() {
    for name in [
        "Layout.Width",
        "Layout.Spacing",
        "Layout.Padding",
        "padding",
        "WIDTH",
        "WidthX",
        "Text.SizeX",
        "Text.LineHeightExtra",
        "Opacity",
        "Background",
        "",
    ] {
        assert!(
            !name_affects_layout(name),
            "{name} 不在名单里，不能影响布局分类"
        );
    }
}
