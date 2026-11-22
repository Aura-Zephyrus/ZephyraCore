//! theme 模块的集成测试：补齐 `tests/unit/tests.rs` 未覆盖的角色与分支。
//!
//! 断言一律用**字面量**（确切角色名字符串、确切色值），不拿被测函数的返回值当
//! 判据——否则角色互换 / 条件反转的变异会显示绿色。

use std::borrow::Cow;

use zephyra_core::core::Color;
use zephyra_core::property::PropertyValue;
use zephyra_core::theme::{
    resolve_resource, resource_value, roles, Theme, ThemeContext, ThemeVariant,
};

// ============================== control_fill_role 真值表 ==============================

/// 完整 8 行真值表：disabled > selected > hovered > rest。
/// 期望值写成字面量字符串，任一条优先级被调换 / 条件被反转都会红。
#[test]
fn control_fill_role_full_priority_truth_table() {
    // 元组顺序：(selected, disabled, hovered, 期望角色字面量)
    let cases: [(bool, bool, bool, &str); 8] = [
        (false, false, false, "control_fill"),
        (false, false, true, "control_fill_hover"),
        (true, false, false, "control_fill_pressed"),
        (true, false, true, "control_fill_pressed"),
        (false, true, false, "surface_alt"),
        (false, true, true, "surface_alt"),
        (true, true, false, "surface_alt"),
        (true, true, true, "surface_alt"),
    ];
    for (selected, disabled, hovered, expected) in cases {
        assert_eq!(
            roles::control_fill_role(selected, disabled, hovered),
            expected,
            "selected={selected} disabled={disabled} hovered={hovered}"
        );
    }
}

#[test]
fn control_fill_role_rest_is_plain_fill() {
    // 全 false 必须落到基础填充；若 hovered 分支被反转成 !hovered，这里先红。
    assert_eq!(
        roles::control_fill_role(false, false, false),
        "control_fill"
    );
}

#[test]
fn control_fill_role_hovered_beats_rest() {
    assert_eq!(
        roles::control_fill_role(false, false, true),
        "control_fill_hover"
    );
}

#[test]
fn control_fill_role_selected_beats_hovered() {
    // selected + hovered 同真时选中优先；对调两个 if 的顺序会立刻被这条捕获。
    assert_eq!(
        roles::control_fill_role(true, false, true),
        "control_fill_pressed"
    );
}

#[test]
fn control_fill_role_disabled_beats_selected_and_hovered() {
    // disabled 必须压过其余全部；把 disabled 检查挪到 selected / hovered 之后会红。
    assert_eq!(roles::control_fill_role(true, true, true), "surface_alt");
    assert_eq!(roles::control_fill_role(true, true, false), "surface_alt");
    assert_eq!(roles::control_fill_role(false, true, true), "surface_alt");
}

#[test]
fn control_fill_role_results_are_resolvable_in_both_themes() {
    // 真值表产出的键必须真的是可解析资源键，而不是一个拼错的字符串。
    for (selected, disabled, hovered, _) in [
        (false, false, false, ""),
        (false, false, true, ""),
        (false, true, false, ""),
        (true, false, false, ""),
        (true, true, true, ""),
    ] {
        let key = roles::control_fill_role(selected, disabled, hovered);
        assert!(
            resource_value(&Theme::dark(), key).is_some(),
            "深色主题无法解析 {key}"
        );
        assert!(
            resource_value(&Theme::light(), key).is_some(),
            "浅色主题无法解析 {key}"
        );
    }
}

// ============================== 全部语义角色 × 两个 variant ==============================

/// (角色键, 深色期望色, 浅色期望色)。色值全部照 `Theme::dark()` / `Theme::light()`
/// 的契约写成字面量；不做"从 Theme 字段读出来再比"的恒真断言。
const ALL_ROLES: &[(&str, Color, Color)] = &[
    (
        roles::SURFACE,
        Color::rgb(0.063, 0.071, 0.078),
        // 权威定义在 theme.rs::light() 的 palette.neutral_0：颜色是线性空间，
        // 宿主上屏编码到 sRGB，线性 0.888 ≈ sRGB #F2F2F7（Apple 分组背景）。
        // 纯白或 0.949（上屏 #FCFCFD）都会让白 alpha 卡片与背景几乎同色。
        Color::rgb(0.888, 0.888, 0.930),
    ),
    (
        roles::SURFACE_ALT,
        Color::rgb(0.102, 0.110, 0.118),
        Color::rgb(0.973, 0.976, 0.980),
    ),
    (
        roles::SURFACE_ALT_TRANSLUCENT,
        Color::rgba(0.102, 0.110, 0.118, 0.85),
        Color::rgba(0.973, 0.976, 0.980, 0.85),
    ),
    (
        roles::CONTENT,
        Color::rgb(0.898, 0.898, 0.898),
        Color::rgb(0.118, 0.133, 0.149),
    ),
    (
        roles::CONTENT_SECONDARY,
        Color::rgb(0.541, 0.573, 0.604),
        Color::rgb(0.400, 0.427, 0.455),
    ),
    (
        roles::CONTENT_DISABLED,
        Color::rgb(0.400, 0.427, 0.455),
        Color::rgb(0.541, 0.573, 0.604),
    ),
    (
        roles::STROKE,
        Color::rgb(0.239, 0.255, 0.275),
        // 权威定义在 light() 的 palette.neutral_300。
        Color::rgb(0.820, 0.820, 0.839),
    ),
    (
        roles::CONTROL_FILL,
        Color::rgb(0.137, 0.149, 0.161),
        Color::rgb(0.973, 0.976, 0.980),
    ),
    (
        roles::CONTROL_FILL_HOVER,
        Color::rgb(0.184, 0.196, 0.212),
        // 权威定义在 light() 的 palette.neutral_200。
        Color::rgb(0.898, 0.898, 0.918),
    ),
    (
        roles::CONTROL_FILL_PRESSED,
        Color::rgb(0.239, 0.255, 0.275),
        // 权威定义在 light() 的 palette.neutral_300。
        Color::rgb(0.820, 0.820, 0.839),
    ),
    (
        roles::ACCENT_FILL,
        Color::rgb(0.231, 0.510, 0.965),
        Color::rgb(0.157, 0.388, 0.921),
    ),
    (
        roles::ON_ACCENT_FILL,
        Color::rgb(0.882, 0.941, 1.0),
        Color::rgb(1.0, 1.0, 1.0),
    ),
    (
        roles::FOCUS_RING,
        Color::rgb(0.231, 0.510, 0.965),
        Color::rgb(0.157, 0.388, 0.921),
    ),
    (
        roles::FOCUS_INNER,
        Color::rgba(0.0, 0.0, 0.0, 0.70),
        Color::rgb(1.0, 1.0, 1.0),
    ),
    (
        roles::CARD,
        Color::rgba(1.0, 1.0, 1.0, 0.0512),
        Color::rgba(1.0, 1.0, 1.0, 0.7),
    ),
    (
        roles::CARD_SECONDARY,
        Color::rgba(1.0, 1.0, 1.0, 0.0326),
        Color::rgba(0.976, 0.976, 0.976, 0.5),
    ),
    (
        roles::CARD_BORDER,
        Color::rgba(0.0, 0.0, 0.0, 0.10),
        Color::rgba(0.0, 0.0, 0.0, 0.12),
    ),
    (
        roles::CONTROL_SECONDARY,
        Color::rgba(1.0, 1.0, 1.0, 0.0837),
        Color::rgba(0.976, 0.976, 0.976, 0.5),
    ),
    (
        roles::CONTROL_TERTIARY,
        Color::rgba(1.0, 1.0, 1.0, 0.0419),
        Color::rgba(0.976, 0.976, 0.976, 0.3),
    ),
    (
        roles::CONTROL_STRONG,
        Color::rgba(1.0, 1.0, 1.0, 0.5442),
        Color::rgba(0.0, 0.0, 0.0, 0.4458),
    ),
    (
        roles::SWITCH_ACCENT,
        Color::rgb(
            0x30 as f32 / 255.0,
            0xd1 as f32 / 255.0,
            0x58 as f32 / 255.0,
        ),
        Color::rgb(
            0x34 as f32 / 255.0,
            0xc7 as f32 / 255.0,
            0x59 as f32 / 255.0,
        ),
    ),
    (
        roles::SWITCH_TRACK,
        Color::rgba(120.0 / 255.0, 120.0 / 255.0, 128.0 / 255.0, 0.36),
        Color::rgba(120.0 / 255.0, 120.0 / 255.0, 120.0 / 255.0, 0.2),
    ),
    (
        roles::CONTROL_BORDER,
        Color::rgba(0.0, 0.0, 0.0, 0.09),
        Color::rgba(0.0, 0.0, 0.0, 0.10),
    ),
    (
        roles::CONTROL_BORDER_STRONG,
        Color::rgba(1.0, 1.0, 1.0, 0.6047),
        Color::rgba(0.0, 0.0, 0.0, 0.6063),
    ),
    (
        roles::DIVIDER,
        Color::rgba(1.0, 1.0, 1.0, 0.0837),
        Color::rgba(0.0, 0.0, 0.0, 0.16),
    ),
    (
        roles::LAYER,
        Color::rgba(0.227, 0.227, 0.227, 0.30),
        Color::rgba(1.0, 1.0, 1.0, 0.5),
    ),
    (
        roles::SUCCESS,
        Color::rgb(
            0x6c as f32 / 255.0,
            0xcb as f32 / 255.0,
            0x5f as f32 / 255.0,
        ),
        Color::rgb(
            0x0f as f32 / 255.0,
            0x7b as f32 / 255.0,
            0x0f as f32 / 255.0,
        ),
    ),
    (
        roles::CAUTION,
        Color::rgb(0xfc as f32 / 255.0, 0xe1 as f32 / 255.0, 0.00_f32 / 255.0),
        Color::rgb(
            0x9d as f32 / 255.0,
            0x5d as f32 / 255.0,
            0x00 as f32 / 255.0,
        ),
    ),
    (
        roles::CRITICAL,
        Color::rgb(
            0xff as f32 / 255.0,
            0x99 as f32 / 255.0,
            0xa4 as f32 / 255.0,
        ),
        Color::rgb(
            0xc4 as f32 / 255.0,
            0x2b as f32 / 255.0,
            0x1c as f32 / 255.0,
        ),
    ),
    (
        roles::SUCCESS_BG,
        Color::rgb(
            0x39 as f32 / 255.0,
            0x3d as f32 / 255.0,
            0x1b as f32 / 255.0,
        ),
        Color::rgb(
            0xdf as f32 / 255.0,
            0xf6 as f32 / 255.0,
            0xdd as f32 / 255.0,
        ),
    ),
    (
        roles::CAUTION_BG,
        Color::rgb(
            0x43 as f32 / 255.0,
            0x35 as f32 / 255.0,
            0x19 as f32 / 255.0,
        ),
        Color::rgb(
            0xff as f32 / 255.0,
            0xf4 as f32 / 255.0,
            0xce as f32 / 255.0,
        ),
    ),
    (
        roles::CRITICAL_BG,
        Color::rgb(
            0x44 as f32 / 255.0,
            0x27 as f32 / 255.0,
            0x26 as f32 / 255.0,
        ),
        Color::rgb(
            0xfd as f32 / 255.0,
            0xe7 as f32 / 255.0,
            0xe9 as f32 / 255.0,
        ),
    ),
];

/// 取角色颜色并确认"可用"：alpha 必须 > 0（不是全透明），四通道不得全零。
/// 注意有些角色是 rgba(0,0,0,0.09) 这类"rgb 为零但 alpha 非零"的叠层色，
/// 所以判据必须看 alpha，不能只要求 rgb 非零。
fn resolved(theme: &Theme, role: &str) -> Color {
    let c = theme
        .role(role)
        .unwrap_or_else(|| panic!("{:?} 缺少角色 {role}", theme.variant));
    assert!(c.a > 0.0, "角色 {role} 返回了全透明色：{c:?}");
    assert!(
        !(c.r == 0.0 && c.g == 0.0 && c.b == 0.0 && c.a == 0.0),
        "角色 {role} 返回了全零色：{c:?}"
    );
    c
}

#[test]
fn every_role_resolves_to_exact_token_in_both_variants() {
    // 用 builtin 取主题，顺带把 variant → 主题映射也钉住。
    let dark = Theme::builtin(ThemeVariant::Dark);
    let light = Theme::builtin(ThemeVariant::Light);
    for &(role, dark_expected, light_expected) in ALL_ROLES {
        let dark_color = resolved(&dark, role);
        let light_color = resolved(&light, role);
        assert_eq!(dark_color, dark_expected, "深色角色 {role} 色值不符");
        assert_eq!(light_color, light_expected, "浅色角色 {role} 色值不符");
        // 任意两个角色的 match 臂互换、或 variant 取值对调，这条都会红。
        assert_ne!(dark_color, light_color, "角色 {role} 在两变体下必须不同");
    }
}

#[test]
fn builtin_variants_match_named_constructors() {
    assert_eq!(Theme::builtin(ThemeVariant::Light), Theme::light());
    assert_eq!(Theme::builtin(ThemeVariant::Dark), Theme::dark());
    assert_ne!(
        Theme::builtin(ThemeVariant::Light),
        Theme::builtin(ThemeVariant::Dark)
    );
    assert_eq!(Theme::light().variant, ThemeVariant::Light);
    assert_eq!(Theme::dark().variant, ThemeVariant::Dark);
}

#[test]
fn unknown_role_still_returns_none() {
    assert_eq!(Theme::dark().role("surfcae"), None);
    assert_eq!(Theme::light().role(""), None);
}

// ============================== 变体翻转 / ThemeContext ==============================

#[test]
fn theme_variant_toggled_flips_and_is_an_involution() {
    assert_eq!(ThemeVariant::Light.toggled(), ThemeVariant::Dark);
    assert_eq!(ThemeVariant::Dark.toggled(), ThemeVariant::Light);
    // 翻转不能是恒等，也不能翻过头。
    assert_ne!(ThemeVariant::Light.toggled(), ThemeVariant::Light);
    assert_ne!(ThemeVariant::Dark.toggled(), ThemeVariant::Dark);
    assert_eq!(ThemeVariant::Light.toggled().toggled(), ThemeVariant::Light);
    assert_eq!(ThemeVariant::Dark.toggled().toggled(), ThemeVariant::Dark);
}

#[test]
fn theme_context_toggle_swaps_variant_and_role_colors() {
    // 期望色取自命名构造器，不从 ctx 自己读，避免自证。
    let light_surface = Theme::light().role(roles::SURFACE);
    let dark_surface = Theme::dark().role(roles::SURFACE);
    assert_ne!(light_surface, dark_surface);

    let mut ctx = ThemeContext::new(ThemeVariant::Light);
    assert_eq!(ctx.theme().variant, ThemeVariant::Light);
    assert_eq!(ctx.theme().role(roles::SURFACE), light_surface);

    assert!(ctx.toggle(), "toggle 必须报告发生了变化");
    assert_eq!(ctx.theme().variant, ThemeVariant::Dark);
    assert_eq!(ctx.theme().role(roles::SURFACE), dark_surface);

    assert!(ctx.toggle());
    assert_eq!(ctx.theme().variant, ThemeVariant::Light);
    assert_eq!(ctx.theme().role(roles::SURFACE), light_surface);

    // 从 Dark 起步也要能翻回 Light。
    let mut ctx = ThemeContext::new(ThemeVariant::Dark);
    assert_eq!(ctx.theme().variant, ThemeVariant::Dark);
    assert!(ctx.toggle());
    assert_eq!(ctx.theme().variant, ThemeVariant::Light);
    assert_eq!(ctx.theme().role(roles::SURFACE), light_surface);
}

// ============================== resource_value / resolve_resource ==============================

#[test]
fn resource_value_resolves_every_role_to_its_exact_color() {
    for (variant, expected_offset) in [(ThemeVariant::Dark, 1usize), (ThemeVariant::Light, 2usize)]
    {
        let theme = Theme::builtin(variant);
        for row in ALL_ROLES {
            let (role, _, _) = *row;
            let expected = if expected_offset == 1 { row.1 } else { row.2 };
            let value = resource_value(&theme, role)
                .unwrap_or_else(|| panic!("{variant:?} 无法解析资源键 {role}"));
            assert!(
                matches!(value, PropertyValue::Color(_)),
                "{role} 必须解析成 Color，实得 {value:?}"
            );
            assert_eq!(
                value,
                PropertyValue::color(expected),
                "{variant:?} 角色 {role}"
            );
        }
    }
}

#[test]
fn resource_value_resolves_all_spacing_keys_to_exact_dips() {
    let theme = Theme::light();
    for (key, dip) in [
        ("spacing.none", 0.0),
        ("spacing.xxsmall", 2.0),
        ("spacing.xsmall", 4.0),
        ("spacing.small", 8.0),
        ("spacing.medium", 12.0),
        ("spacing.large", 16.0),
        ("spacing.xlarge", 20.0),
        ("spacing.xxlarge", 24.0),
    ] {
        let value = resource_value(&theme, key).unwrap_or_else(|| panic!("间距键 {key} 应可解析"));
        assert_eq!(value, PropertyValue::float(dip), "间距键 {key} 的 dip 值");
    }
}

#[test]
fn resource_value_unknown_key_returns_none() {
    let theme = Theme::dark();
    assert_eq!(resource_value(&theme, "surfcae"), None);
    assert_eq!(resource_value(&theme, "spacing.huge"), None);
    assert_eq!(resource_value(&theme, ""), None);
    // spacing 分支与角色分支互不串台。
    assert_eq!(resource_value(&theme, "surface.medium"), None);
}

#[test]
fn resolve_resource_returns_owned_color_for_a_valid_key() {
    let resource = PropertyValue::Resource(roles::SURFACE.into());
    let resolved = resolve_resource(&Theme::dark(), &resource).expect("surface 必须可解析");
    // 必须返回 Owned：Cow::Borrowed 会把资源键当成已解析值，切主题时永远拿到旧键。
    assert!(matches!(resolved, Cow::Owned(_)), "解析结果必须是拥有者");
    assert_eq!(
        resolved.as_ref(),
        &PropertyValue::color(Color::rgb(0.063, 0.071, 0.078))
    );
}

#[test]
fn resolve_resource_re_resolves_the_same_key_for_each_theme() {
    let resource = PropertyValue::Resource(roles::ACCENT_FILL.into());
    let dark = resolve_resource(&Theme::dark(), &resource).unwrap();
    let light = resolve_resource(&Theme::light(), &resource).unwrap();
    assert_eq!(
        dark.as_ref(),
        &PropertyValue::color(Color::rgb(0.231, 0.510, 0.965))
    );
    assert_eq!(
        light.as_ref(),
        &PropertyValue::color(Color::rgb(0.157, 0.388, 0.921))
    );
    assert_ne!(dark, light, "同一个资源键必须随主题重解析");
    // 解析不能写回原属性：键必须留着，否则第二次切主题失效。
    assert!(resource.is_resource());
    assert_eq!(resource.resource_key(), Some(roles::ACCENT_FILL));
}

#[test]
fn resolve_resource_resolves_spacing_keys_too() {
    let resource = PropertyValue::Resource("spacing.medium".into());
    let resolved = resolve_resource(&Theme::dark(), &resource).unwrap();
    assert_eq!(resolved.as_ref(), &PropertyValue::float(12.0));
}

#[test]
fn resolve_resource_non_resource_value_returns_none() {
    let theme = Theme::dark();
    assert!(resolve_resource(&theme, &PropertyValue::color(Color::rgb(1.0, 0.0, 0.0))).is_none());
    assert!(resolve_resource(&theme, &PropertyValue::float(12.0)).is_none());
    // 字符串字面量长得像键也不是 Resource 变体，不得被误解析。
    assert!(resolve_resource(&theme, &PropertyValue::static_str("surface")).is_none());
}

#[test]
fn resolve_resource_unknown_key_returns_none_instead_of_black() {
    let resource = PropertyValue::Resource("not_a_role".into());
    assert!(resolve_resource(&Theme::dark(), &resource).is_none());
    assert!(resolve_resource(&Theme::light(), &resource).is_none());
}
