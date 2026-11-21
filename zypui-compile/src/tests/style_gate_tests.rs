//! 仓内**全部** `*.zypui` 的排版门禁：写法统一靠测试钉住，不靠评审记性。
//!
//! # 为什么值得一条测试
//!
//! 属性顺序**不是语义** —— zypuic 按名字取属性（`XEl::attr`），把 `class` 挪到
//! 行尾与把它放前面编出的字节码逐字节相同。既然编译器不管，「写法统一」就没有
//! 任何机器兜底：三份文件曾各写各的（class 有的在中、有的在尾；标签有的跨三行；
//! `<if>` 有的挤一行有的三行）。人看着难受，diff 也难读。
//!
//! 这几条规则**只影响人读**，所以门禁判的是排版而不是语义 —— 真正的语义正确性
//! 由 zypuic 编译期报错 + gallery 整树门禁承担（见 `docs/developer/07-declarative-ui.md`）。
//!
//! # 判据住在 `fmt` 模块里，这里只负责「扫全仓然后跑一遍」
//!
//! 上一轮（2026-10-31）判据与排版是两份实现，于是门禁自己少报了一半 class 违规
//! （拿归一化前的内容回跑才抓到，见 dev-log）。所以五条规则的判据现在住在
//! `crate::fmt::violations` —— 与本 crate 的 `zypuic fmt` 共用同一份扫描代码，
//! 结构上不可能再漂。本文件只剩「递归扫工作区 + 读文件 + 汇总」。

/// 门禁的判据与排版器对齐的**硬**检查：每条规则各给一个坏样例，`fmt` 必须
/// 修到与 `violations` 判据一致的干净形态。
///
/// 这一条防的是「跑完 `zypuic fmt`，门禁还红」—— 比没有 fmt 更糟，因为它会
/// 让人以为已经统一过了。
#[test]
fn fmt_fixes_exactly_what_the_gate_checks() {
    let cases: &[(&str, &str)] = &[
        // 规则 1：缩进不是 2 的倍数 / 行尾空白 / tab
        (
            "<component name=\"X\"> <box key=\"a\"/> </component>\n",
            "<component name=\"X\">\n  <box key=\"a\"/>\n</component>\n",
        ),
        // 规则 2+3：开始标签跨行
        (
            "<box key=\"a\"\n  class=\"c\"/>\n",
            "<box key=\"a\" class=\"c\"/>\n",
        ),
        // 规则 3：一行两个开始标签
        (
            "<if cond=\"a\"><B/></if>\n",
            "<if cond=\"a\">\n  <B/>\n</if>\n",
        ),
        // 规则 4：key 不在第一位
        (
            "<box class=\"c\" key=\"a\"/>\n",
            "<box key=\"a\" class=\"c\"/>\n",
        ),
        // 规则 5：class 不在最后
        (
            "<box key=\"a\" class=\"c\" icon=\"gear\"/>\n",
            "<box key=\"a\" icon=\"gear\" class=\"c\"/>\n",
        ),
        // 单引号属性同样要被看见（上一轮的判据只认 `"`，漏的正是这一类）。
        ("<box class='c' key='a'/>\n", "<box key='a' class='c'/>\n"),
    ];
    for (bad, want) in cases {
        let got =
            crate::fmt::format(bad).unwrap_or_else(|| panic!("fmt 必须认得这个漂移样例：{bad:?}"));
        assert_eq!(&got, want, "fmt 没有修好门禁会报的那条：{bad:?}");
        assert_eq!(
            crate::fmt::violations(want),
            Vec::<String>::new(),
            "修出来的形态仍然违反门禁：{want:?}"
        );
    }
}

/// 规则本身要能抓到违规 —— 拿几段故意写坏的源码验判据，防止门禁形同虚设。
#[test]
fn layout_violations_are_actually_detected() {
    let cases: &[(&str, &str)] = &[
        // 规则 5：class 不在末尾（含**带缩进**的 —— 那一版判据曾被缩进卡住）
        ("<box key=\"a\" class=\"x\" icon=\"gear\"/>", "规则5"),
        (
            "  <control class=\"button\" interactive=\"true\"/>",
            "规则5",
        ),
        // 规则 4：key 不是第一个
        ("<box class=\"x\" key=\"a\"/>", "规则4"),
        (
            "  <control interactive=\"true\" key=\"a\" class=\"x\"/>",
            "规则4",
        ),
        // 规则 3：标签跨行
        ("<box key=\"a\"\n  class=\"x\"/>", "规则3"),
        // 规则 2：裸文本
        ("hello", "规则2"),
        // 规则 1：缩进不是 2 的倍数
        ("   <box key=\"a\"/>", "规则1"),
    ];
    for (src, want) in cases {
        let got = crate::fmt::violations(src);
        assert!(
            got.iter().any(|x| x.contains(want)),
            "[{src:?}] 应当被判 [{want}]，得到 {got:?}"
        );
    }

    // 干净写法一条不报（注释里的 `<` 也不算标签）。
    let clean = "<!-- <not-a-tag class=\"in-comment\" key=\"x\"> -->\n<box key=\"a\" text=\"hi\" class=\"c\"/>\n";
    assert!(
        crate::fmt::violations(clean).is_empty(),
        "干净文件不该报，得到 {:?}",
        crate::fmt::violations(clean)
    );
}
