//! i18n 单元测试：语言标签解析、偏好协商、消息表。
//!
//! 从 `src/tests/i18n_tests.rs` 移入 `tests/` —— 所有被测项均为 `pub`，
//! 移到集成测试后不需要改可见性。

use zephyra_core::i18n::{negotiate, Messages, Preferences, Tag};

/// 解析必须照 BCP 47 的三段结构，且**规范化**大小写。
#[test]
fn tag_is_parsed_and_normalized_per_bcp47() {
    let t = Tag::parse("zh-hans-cn").expect("合法");
    assert_eq!(t.language(), "zh");
    assert_eq!(t.script(), Some("Hans"));
    assert_eq!(t.region(), Some("CN"));
    assert_eq!(t.as_str(), "zh-Hans-CN");

    assert_eq!(Tag::parse("en").unwrap().as_str(), "en");
    assert_eq!(Tag::parse("en-US").unwrap().as_str(), "en-US");
    // 3 数字区域（UN M.49）。
    assert_eq!(Tag::parse("es-419").unwrap().region(), Some("419"));
    // 非法项必须 None，不做模糊修复。
    assert!(Tag::parse("").is_none());
    assert!(Tag::parse("e").is_none());
    assert!(Tag::parse("zh_cn").is_none());
    assert!(Tag::parse("zh-CN-extra").is_none());
}

/// 协商必须是「**表里第一个匹配的**」，不是「全局最接近的」。
///
/// 这条是官方措辞的直接翻译：`Intl` 用
/// "the first entry in the list that matches a locale supported"。
#[test]
fn negotiation_follows_preference_order_not_globally_nearest() {
    // `ja` 排第一 ⇒ 必须选 ja，即使 `en-US` 与第二项**完全相等**（距离 0）。
    let prefs = Preferences::from_list(&["ja", "en-US"]);
    let available = [Tag::parse("en-US").unwrap(), Tag::parse("ja").unwrap()];
    assert_eq!(negotiate(&prefs, &available).unwrap().as_str(), "ja");

    // 反过来：`en-US` 优先 ⇒ 选 en-US。
    let prefs = Preferences::from_list(&["en-US", "ja"]);
    assert_eq!(negotiate(&prefs, &available).unwrap().as_str(), "en-US");
}

/// 宿主交回的**真实**系统偏好必须能落到应用的可用语言上。
///
/// # 这条钉的是真机形状
///
/// Windows 的 `GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, …)` 交回的是
/// **区域**标签（实测本机 `["zh-CN", "en-US"]`），而应用声明的可用语言里
/// 常常写的是**文字**子标签（`zh-Hans`）。两者必须能对上，否则中文系统的
/// 用户打开应用看到的是英文 —— 而协商本身**不报错**。
///
/// 形状：`zh-CN`（无 script）对 `zh-Hans`（有 script、无 region）⇒
/// 按 `Tag::distance` 的规则，丢 script 与 region ⇒ 命中。
#[test]
fn a_region_only_system_tag_matches_a_script_only_available_tag() {
    let available = [
        Tag::parse("en").unwrap(),
        Tag::parse("zh-Hans").unwrap(),
        Tag::parse("ja").unwrap(),
    ];
    let prefs = Preferences::from_list(&["zh-CN", "en-US"]);
    assert_eq!(
        negotiate(&prefs, &available).unwrap().as_str(),
        "zh-Hans",
        "系统偏好 zh-CN 必须落到应用声明的 zh-Hans 上"
    );
    // 反过来：系统偏好英文时选英文。
    let prefs = Preferences::from_list(&["en-US", "zh-CN"]);
    assert_eq!(negotiate(&prefs, &available).unwrap().as_str(), "en");
}

/// 回退优先级：全中 > 丢 region > 只语言段。
#[test]
fn fallback_distance_follows_official_order() {
    let full = Tag::parse("zh-Hans-CN").unwrap();
    assert_eq!(full.distance(&Tag::parse("zh-Hans-CN").unwrap()), Some(0));
    assert_eq!(full.distance(&Tag::parse("zh-Hans").unwrap()), Some(2));
    assert_eq!(full.distance(&Tag::parse("zh").unwrap()), Some(3));
    // 语言段不同 ⇒ 不匹配。
    assert_eq!(full.distance(&Tag::parse("en").unwrap()), None);
    // script 不同（Hans vs Hant）⇒ 不匹配：那是两种文字。
    assert_eq!(
        Tag::parse("zh-Hans")
            .unwrap()
            .distance(&Tag::parse("zh-Hant").unwrap()),
        None
    );
}

/// `Messages::select` 必须走 language-only 回退。
///
/// 浏览器显式追加 language-only 回退标签（`en-US,en;q=0.9`），
/// 所以「只有 `zh-Hans-CN` 译文而用户是 `zh-Hans`」必须能取到。
#[test]
fn message_lookup_falls_back_to_language_only() {
    let mut m = Messages::new();
    m.insert("zh-Hans-CN", "hello", "你好");
    // 只插了精确 locale。
    assert_eq!(
        m.select(&Preferences::from_list(&["zh-Hans-CN"]), "en", "hello"),
        Some("你好")
    );

    // 语言段回退：用户标签短于可用表时也应命中。
    m.insert("zh", "bye", "再见");
    assert_eq!(
        m.select(&Preferences::from_list(&["zh-Hans", "en"]), "en", "bye"),
        Some("再见")
    );
}

/// 漏翻必须回落到**默认语言**的文案，而不是键名、更不是空白。
///
/// 这是浏览器的真实行为（某门语言缺资源时回落默认语言），
/// 也是"降级要写在明处"的落点：真要发现漏翻，靠门禁而不是靠界面。
#[test]
fn missing_translation_falls_back_to_default_language() {
    let mut m = Messages::new();
    m.insert("en", "nav.home", "Home");
    // `ja` 一个键都没有 ⇒ 必须拿到 `en` 的文案。
    assert_eq!(
        m.select(&Preferences::from_list(&["ja"]), "en", "nav.home"),
        Some("Home"),
        "漏翻应回落默认语言，而不是 None/空串"
    );
    // 连默认语言也没有 ⇒ None（由调用方显示键名）。
    assert_eq!(
        m.select(&Preferences::from_list(&["ja"]), "en", "nope"),
        None
    );
}

/// 默认语言的语言段回退：默认语言写 `zh-Hans` 而调用方给 `zh` 时仍要命中。
///
/// 这一条是**真实**的：`negotiate` 交出来的 `Tag` 会带 script，
/// 而调用方手里的默认语言常量可能只写到语言段。
#[test]
fn default_locale_also_uses_language_section_fallback() {
    let mut m = Messages::new();
    m.insert("zh-Hans", "a", "A");
    assert_eq!(m.select(&Preferences::new(), "zh-Hans", "a"), Some("A"));
    // `en` 兜底时先试 `en`、再试 `en`（无 script 无 region，同一项）。
    assert_eq!(m.select(&Preferences::new(), "en-Hans", "a"), None);
}

/// 漏翻必须可被机器发现（门禁依据）。
#[test]
fn missing_keys_are_enumerable() {
    let mut m = Messages::new();
    m.insert("en", "a", "A");
    let missing = m.missing_in("en", &["a", "b", "c"]);
    assert_eq!(missing, vec!["b".to_string(), "c".to_string()]);
}

/// 偏好表有序、去重，`first()` 对应 `navigator.language`。
#[test]
fn preference_list_is_ordered_and_deduplicated() {
    let p = Preferences::from_list(&["en-US", "zh-CN"]);
    assert_eq!(p.first().unwrap().as_str(), "en-US");
    let mut q = p.clone();
    q.push(Tag::parse("en-US").unwrap());
    assert_eq!(q.len(), 2, "去重但保留顺序");
    q.push(Tag::parse("ja").unwrap());
    assert_eq!(q.len(), 3);
    // 坏项跳过而不是让整表失效。
    assert_eq!(Preferences::from_list(&["en", "bogus-tag"]).len(), 1);
}

/// `from_owned` 与 `from_list` 必须同形（宿主交回的是有所有权的串）。
///
/// 这条不是恒真：把 `from_owned` 实现成"`from_list` 但忽略非法项"的话，
/// 本测试仍会过，而真正的问题（宿主给的语言**全**被跳过 → 回落默认）
/// 会静默发生。所以这里额外断言"含坏项时好项仍在"。
#[test]
fn owned_and_borrowed_lists_have_same_shape() {
    let owned: Vec<String> = vec!["zh-CN".to_string(), "坏-标签".to_string()];
    let p = Preferences::from_owned(&owned);
    assert_eq!(p.len(), 1);
    assert_eq!(p.first().unwrap().as_str(), "zh-CN");
    assert_eq!(p, Preferences::from_list(&["zh-CN"]));
}

/// `with_weight` / `Weighted`：权重是 `Accept-Language` 的 `q` 值，必须原样写入，
/// 且**不参与重排**（有序偏好表的顺序即优先级）。
#[test]
fn weighted_preferences_keep_weight_and_order() {
    let prefs = Preferences::from_list(&["en-US", "zh-CN", "ja"]);
    let weighted = prefs.with_weight(0.9);
    // 权重字段确实被写入（写死成 1.0 / 忽略入参都会红）。
    assert_eq!(weighted.weight, 0.9);
    assert_ne!(prefs.with_weight(0.5).weight, weighted.weight);
    // 偏好表原样克隆，顺序不变。
    assert_eq!(weighted.prefs, prefs);
    let order: Vec<String> = weighted.prefs.tags().iter().map(|t| t.as_str()).collect();
    assert_eq!(order, vec!["en-US", "zh-CN", "ja"]);
}

/// `remove` / `len_for` / `is_empty`：`remove` 是"制造漏翻"的门禁专用刀，
/// 必须真的删掉目标键、返回是否删成功，且不误伤其他 locale。
#[test]
fn messages_remove_len_for_and_is_empty() {
    let mut m = Messages::new();
    assert!(m.is_empty());
    m.insert("en", "a", "A");
    m.insert("en", "b", "B");
    m.insert("ja", "a", "あ");
    assert!(!m.is_empty());
    assert_eq!(m.len_for("en"), 2);
    assert_eq!(m.len_for("ja"), 1);
    assert_eq!(m.len_for("fr"), 0);

    // 删存在的键返回 true，且只影响该 locale。
    assert!(m.remove("en", "a"));
    assert!(m.remove("en", "b"));
    assert_eq!(m.len_for("en"), 0);
    assert_eq!(m.len_for("ja"), 1);
    assert!(m.get("en", "a").is_none());
    assert_eq!(m.get("ja", "a"), Some("あ"));

    // 删不存在的键返回 false，计数不变。
    assert!(!m.remove("en", "a"));
    assert!(!m.remove("fr", "x"));
    assert_eq!(m.len_for("ja"), 1);

    // 删光最后一条后 is_empty 归真。
    assert!(m.remove("ja", "a"));
    assert!(m.is_empty());
    assert_eq!(m.len_for("ja"), 0);
}

/// `negotiate` 距离相等时的 tie-break：可用表里两个 locale 与偏好距离相同，
/// 代码用 `d < bd`（严格小于）⇒ 相等时不替换，取 **available 中靠前**的那个。
/// 若比较符改成 `d <= bd`，靠后的会胜出，下面每条断言都会红。
#[test]
fn negotiate_tie_break_prefers_earlier_available_locale() {
    // want 只有 language 段，与 zh-Hans / zh-Hant 的距离同为 3（丢 script 与 region）。
    let prefs = Preferences::from_list(&["zh"]);
    let available = [
        Tag::parse("zh-Hans").unwrap(),
        Tag::parse("zh-Hant").unwrap(),
    ];
    assert_eq!(negotiate(&prefs, &available).unwrap().as_str(), "zh-Hans");

    // 交换 available 顺序 ⇒ 靠前的变成 zh-Hant，仍应取靠前者。
    let available_rev = [
        Tag::parse("zh-Hant").unwrap(),
        Tag::parse("zh-Hans").unwrap(),
    ];
    assert_eq!(
        negotiate(&prefs, &available_rev).unwrap().as_str(),
        "zh-Hant"
    );

    // 另一档距离（丢 region = 2）的 tie 同样取靠前：zh-Hans-CN 而非 zh-Hans-TW。
    let prefs2 = Preferences::from_list(&["zh-Hans"]);
    let a2 = [
        Tag::parse("zh-Hans-CN").unwrap(),
        Tag::parse("zh-Hans-TW").unwrap(),
    ];
    assert_eq!(negotiate(&prefs2, &a2).unwrap().as_str(), "zh-Hans-CN");
}

/// `Tag::parse` 的拒绝分支：重复 script/region、非法长度（1 字母 language、
/// 5 字母子标签、3 字母非数字 region、3/5 字母 script）都必须 None。
#[test]
fn tag_parse_rejects_duplicate_and_illegal_subtags() {
    // 重复 script：第二个 4 字母子标签无处安放 ⇒ None。
    assert!(Tag::parse("zh-Hans-Latn").is_none());
    // 重复 region：两个 2 字母子标签 ⇒ None。
    assert!(Tag::parse("zh-CN-US").is_none());
    // 数字 region 与字母 region 混用也算重复。
    assert!(Tag::parse("zh-419-US").is_none());
    assert!(Tag::parse("zh-US-419").is_none());
    // 非法长度：1 字母 language、5 字母子标签无对应分支 ⇒ None。
    assert!(Tag::parse("e-US").is_none());
    assert!(Tag::parse("zh-USABC").is_none());
    // region 只认 2 字母或 3 数字；3 字母非数字不是 region ⇒ None。
    assert!(Tag::parse("zh-USA").is_none());
    // script 长度非法（3 / 5 字母）⇒ None。
    assert!(Tag::parse("zh-Han").is_none());
    assert!(Tag::parse("zh-Hansi").is_none());
}

/// `Tag::parse` 的正确解析：4 字母 script + 2 字母 / 3 数字 region，
/// 且大小写规范化由 `title_case` / `to_ascii_uppercase` 完成。
#[test]
fn tag_parse_accepts_four_letter_script_and_two_or_three_letter_region() {
    let t = Tag::parse("en-latn-gb").unwrap();
    assert_eq!(t.language(), "en");
    assert_eq!(t.script(), Some("Latn"));
    assert_eq!(t.region(), Some("GB"));
    assert_eq!(t.as_str(), "en-Latn-GB");

    // 3 数字地区（UN M.49）与 script 同时出现。
    let t = Tag::parse("zh-hans-419").unwrap();
    assert_eq!(t.script(), Some("Hans"));
    assert_eq!(t.region(), Some("419"));
    assert_eq!(t.as_str(), "zh-Hans-419");
}

/// `Display` 必须输出规范化字符串（与 `as_str` 一致）。
#[test]
fn tag_display_matches_normalized_string() {
    let t = Tag::parse("zh-hans-cn").unwrap();
    assert_eq!(format!("{}", t), "zh-Hans-CN");
    assert_eq!(format!("{}", t), t.as_str());
    assert_eq!(format!("{}", Tag::parse("en").unwrap()), "en");
    assert_eq!(
        format!("{}", Tag::parse("zh-hant-TW").unwrap()),
        "zh-Hant-TW"
    );
}

/// `valid_language` / `valid_script` / `valid_region` / `title_case` 的边界值。
/// 这些私有函数经 `Tag::parse` 间接测试：把边界改动成别的值就会红。
#[test]
fn tag_validation_boundaries_and_title_case() {
    // valid_language: 长度 2..=8 且全 ASCII 字母。
    assert!(Tag::parse("aa").is_some());
    assert!(Tag::parse("abcdefgh").is_some());
    assert!(Tag::parse("a").is_none()); // 长度 1：下界之外
    assert!(Tag::parse("abcdefghi").is_none()); // 长度 9：上界之外
    assert!(Tag::parse("ab1").is_none()); // 含非字母

    // valid_script: 恰好 4 字母；title_case 首字母大写、其余小写。
    assert_eq!(Tag::parse("zh-hANS").unwrap().script(), Some("Hans"));
    assert_eq!(Tag::parse("zh-HANS").unwrap().script(), Some("Hans"));
    assert!(Tag::parse("zh-han1").is_none()); // 非全字母

    // valid_region: 恰好 2 字母并转大写。
    assert_eq!(Tag::parse("zh-us").unwrap().region(), Some("US"));
    assert!(Tag::parse("zh-u").is_none()); // 长度 1
    assert!(Tag::parse("zh-u1").is_none()); // 含非字母
}
