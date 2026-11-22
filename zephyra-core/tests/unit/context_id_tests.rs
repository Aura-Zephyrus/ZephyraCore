// 本文件由 context.rs 末尾的 #[cfg(test)] mod context_id_tests 抽出。
// 同时钉住 context.rs 的 Context 构造子和 id.rs 的 ElementKey ——
// 两者都不值得各开一个挂载点，统一挂在 context.rs 这一个文件里。

use super::*;

use std::cell::RefCell;
use std::sync::OnceLock;

use crate::id::ElementKey;

static TEST_ICON_SET: OnceLock<crate::icon::IconSet> = OnceLock::new();

fn test_icons() -> &'static crate::icon::IconSet {
    TEST_ICON_SET.get_or_init(crate::icon::IconSet::new)
}

/// `with_all` 必须把三样能力**同时**装上：少装任何一样，`Some(...)` 的分支
/// 就永远走不到，症状是"图标显示不出来但没有任何报错"。
#[test]
fn with_all_installs_theme_engine_and_icon_set() {
    let theme = crate::theme::Theme::builtin(crate::theme::ThemeVariant::Light);
    let engine = RefCell::new(crate::text::NullEngine::default());
    let icons = test_icons();

    let ctx = Context::with_all(&theme, &engine, icons);

    assert_eq!(ctx.theme.variant, crate::theme::ThemeVariant::Light);
    assert!(ctx.engine.is_some(), "with_all 必须挂上文本引擎");
    assert_eq!(
        ctx.icon_set().map(|s| std::ptr::eq(s, icons)),
        Some(true),
        "with_all 必须挂上传进来的那个图标集（同一份，不是副本）"
    );
}

/// `icon_set()` 只反映当前上下文里有没有图标能力：
/// `new` / `with_engine` 都没有，`with_all` 和有 `with_icons` 的才有。
#[test]
fn icon_set_reflects_whether_icons_were_installed() {
    let theme = crate::theme::Theme::dark();
    let engine = RefCell::new(crate::text::NullEngine::default());

    assert!(Context::new(&theme).icon_set().is_none());
    assert!(Context::with_engine(&theme, &engine).icon_set().is_none());

    let icons = test_icons();
    let all = Context::with_all(&theme, &engine, icons);
    assert_eq!(
        all.icon_set().map(|s| std::ptr::eq(s, icons)),
        Some(true),
        "with_all 挂上的必须是传入的图标集本身"
    );

    let with_icons = Context::new(&theme).with_icons(icons);
    assert_eq!(
        with_icons.icon_set().map(|s| std::ptr::eq(s, icons)),
        Some(true)
    );
}

/// `light_context()` 必须给出浅色主题，且与 `dark_context()` 用不同的共享实例。
#[test]
fn light_context_uses_the_shared_light_theme() {
    let light = light_context();
    assert_eq!(light.theme.variant, crate::theme::ThemeVariant::Light);
    assert!(!light.engine.is_some(), "便捷上下文不带文本引擎");
    assert!(light.icon_set().is_none(), "便捷上下文不带图标集");
    assert!(
        std::ptr::eq(light.theme, light_context().theme),
        "必须复用同一份 OnceLock 主题，而不是每次新建"
    );
    assert!(
        !std::ptr::eq(light.theme, dark_context().theme),
        "浅色与深色不能共用同一个 Theme 实例"
    );
}

/// `ElementKey::Id` 没有名字形式；名字形式才返回 `Some`。
#[test]
fn element_key_id_has_no_name() {
    assert_eq!(ElementKey::Id(42).as_name(), None);
    assert_eq!(ElementKey::Name("button").as_name(), Some("button"));
}

/// `Display`：`Id` 是 `#{整数}`，`Name` 是裸名字。
#[test]
fn element_key_display_uses_hash_prefix_for_ids() {
    assert_eq!(format!("{}", ElementKey::Id(42)), "#42");
    assert_eq!(format!("{}", ElementKey::Id(0)), "#0");
    assert_eq!(format!("{}", ElementKey::Name("button")), "button");
}

/// `From<u64>` 只能产出 `Id` 分支；`From<&str>` 只能产出 `Name` 分支。
#[test]
fn from_u64_creates_an_id_key_and_from_str_creates_a_name_key() {
    let id: ElementKey = 42u64.into();
    assert_eq!(id, ElementKey::Id(42));
    assert_eq!(id.as_name(), None, "u64 不能变成 Name");

    let name: ElementKey = "button".into();
    assert_eq!(name, ElementKey::Name("button"));
    assert_eq!(name.as_name(), Some("button"));
}
