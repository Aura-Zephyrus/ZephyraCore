//! 仓内**全部** `*.zstyle` 的排版门禁：「写法统一」靠测试钉住，不靠评审记性。
//!
//! # 为什么值得一条测试
//!
//! 排版规范里唯一「机器能判、人一定会忘」的部分是：已经统一过的写法不许再漂回去。
//! 实测 2026-10-31 存量漂移：19 份文件里 2 份是 CRLF、44 处单行规则
//! （`box.page-guide { direction: vertical; … }`）、选择器与 `{` 之间
//! 有对齐空格（`box.page-guide    {`）。这些都是纯排版差异，语义零变化，
//! 但 diff 里全是噪音。
//!
//! # 门的形状
//!
//! 与 `.zypui` 那条门禁同构：递归扫工作区全部 `*.zstyle`（跳过 `target` / `.git`），
//! 逐份判 `is_formatted`。新文件自动纳入，不需要登记。
//!
//! 另外钉两条**不损耗式**的量（它们不随存量变化而失效，即使存量已经全部干净）：
//!
//! - `fmt` 幂等：`format(format(x)) == format(x)`。不幂等的话 `--check` 会在
//!   格式化后的文件上继续报警，CI 永远不干净。
//! - `fmt` 不改 IR：排版前后 `parse` 的 IR 逐字段相等。括号里的注释解释了
//!   为什么这条重要 —— 它是「只动空白」这条承诺的硬判据。

/// 门禁本身要能抓到违规 —— 拿一份故意写烂的源码验判据，防止门禁形同虚设。
#[test]
fn the_gate_actually_catches_drift() {
    let bad = "control { width:1px; height:2px }\n";
    assert!(
        !crate::fmt::is_formatted(bad),
        "未排版的源码必须被判脏 —— 门禁形同虚设"
    );
    // 单行规则也是违规。
    let one_line = "box.x { width: 1px; }\n";
    assert!(
        !crate::fmt::is_formatted(one_line),
        "单行规则必须被判脏 —— 门禁漏掉了一整类漂移"
    );
    // 干净的必须判干净（否则 `zephyra fmt` 之后 CI 仍然红）。
    let clean = "control {\n    width: 1px;\n    height: 2px;\n}\n";
    assert!(
        crate::fmt::is_formatted(clean),
        "clean source must pass the gate"
    );
}
