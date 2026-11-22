# zephyra-core 变异测试

## 目的

`zephyra-core` 的测试质量用变异测试（`cargo-mutants`）验证：**把源码注入 bug，看测试是否失败**。
测试杀不死的变异叫"存活变异"（survivor），它要么暴露测试缺口，要么是被证明无害的等价变异。
本轮要求"保证测试准确性"，因此不允许对存活变异置之不理——每一条都必须被裁决。

## 运行

```bash
# 安装（已装可跳过）
cargo install cargo-mutants --locked

# 全量（约 2332 个变异体）
$env:TEMP = 'E:\tmp'; $env:TMP = 'E:\tmp'   # Windows：必须指向大容量盘，C 盘空间不足
cargo mutants -p zephyra-core --jobs 8
```

> `copy_target = true` 会复制约 8.8 GB 的 `target` 到临时目录以换增量编译，
> 因此 `TEMP`/`TMP` 必须指向有足够剩余空间的磁盘。`--in-place` 与 `--jobs` 互斥，
> 并行时必须用默认的副本模式。

## 配置

`.cargo/mutants.toml`（workspace root）：

- `examine_globs = ["zephyra-core/src/**/*.rs"]`：只变异 core 的 30 个自有逻辑文件。
- `exclude_globs`：排除 5 个纯 `pub use` 空壳（`core.rs`、`decoration.rs`、`filter.rs`、
  `icon/mod.rs`、`text/mod.rs`）——它们的算法在 git 依赖 `zephyra-vector`，本仓无可变异逻辑。
- `test_package = ["zephyra-core"]`：每个变异只跑 core 的测试。
- `timeout_multiplier = 4.0`、`build_timeout_multiplier = 4.0`：core 测试基线 < 5s，宽松倍数避免误杀。

## CI 门禁语义

`.github/workflows/ci.yml` 的 `mutants` job 每次 PR 跑全量：

- **不设分数硬线**。
- `cargo-mutants` 退出码 `2` 表示存在存活变异 → job 失败。
- 确认无害的等价变异必须在 `.cargo/mutants.toml` 的 `exclude_re` 中显式列出，
  并写明为何等价；不允许靠调高超时或删除测试来"刷绿"。

## 裁决流程

对每个存活变异按以下顺序处理：

1. **测试缺口**：在公开 API 层补一条断言字面量的测试（不复用被测字段自证，避免"恒绿"），重跑该变异确认被杀死。
2. **等价变异**（改与不改行为一致，如 rank 元组中永不重复的第三分量比较符）：加入 `exclude_re` 并写明理由。
3. **不可编译变异**（unviable）：无需处理，`cargo-mutants` 不计入失败。
4. **超时**：先排查是否真有死循环；确认是超时阈值过低则调高倍数，不允许直接忽略。

## 当前状态

**已完成的补测试**（本轮新增约 157 条，均用字面量断言，逐条对应一个具体变异）：

| 模块 | 文件 | 覆盖重点 |
|---|---|---|
| theme | `tests/theme_tests.rs`（新建） | `control_fill_role` 8 行真值表；32 个角色 × 双变体精确色值；资源解析 |
| node | `tests/unit/node_prop_tests.rs`（新建） | styled/local 写入与拆除、脏标记分级、缓存清理、节点查询、P2c 共享子树 |
| style | `tests/unit/style_value_tests.rs`（新建） | 四个 clamp 边界、NaN/inf、`clip_rect` 双来源优先级 |
| visual | `tests/unit/visual.rs` | `sanitize`/`sanitized_scale`、`damage_bounds` outset、文本视觉缓存与回退 |
| frame | `tests/unit/frame_tests.rs` | `sum_us`、`Damage::Display`、`timings()`、styler/post-diff 注入时机 |
| i18n | `tests/i18n_tests.rs` | 权重、`remove`/`len_for`、等距 tie-break、重复子标签拒绝 |
| layout | `tests/unit/layout_tests.rs` | shrink 冻结循环、7 态 AlignContent、WrapReverse、n==1 特例、pick_axis 夹逼 |
| input | `tests/unit/{momentum,recognizer,manipulation,pointer,text}.rs`、`tests/unit/input/router.rs` | clamp 两端、摩擦常数、除零退化、指针表满、revision 时机、Space roundtrip |
| property/hit/context | `tests/unit/property_store_tests.rs`、`tests/unit/hit.rs`、`tests/unit/context_id_tests.rs`（新建） | Custom eq 指针、Debug 各变体、交集钳位、MAX_DEPTH、Context/ElementKey 分支 |
| paint | `tests/unit/paint_tests.rs`、`tests/atlas_eviction_paint_tests.rs` | viewBox 守卫、指纹嵌套子字段、scalar 取 min 轴、`render_stable` 收敛趟数 |

**顺带修复的生产 bug**：`src/property.rs` 的 `PartialEq::Custom` 中 `ai.type_id()` 解析到
`Arc<dyn Any + Send + Sync>` 包装类型（`Any::type_id` for `'static` 具体类型，恒相同），导致
内部类型不同的两个 `Custom` 可能判等；已改为 `(**ai).type_id()`，并在
`property_store_tests.rs::custom_eq_rejects_different_inner_types_even_with_same_eq_pointer`
钉死。

**当前门禁**（本机最终实测）：`cargo fmt --all -- --check`、`cargo clippy -p zephyra-core --all-targets -- -D warnings`、
`cargo test --workspace` 全部通过（workspace 共 1284 passed）；core 本身 538 passed / 1 ignored。

期间修正了 `theme_tests.rs` 中 7 处按旧版本猜测的浅色期望色值（surface/stroke/control_fill_hover/
control_fill_pressed/card_border/control_border/divider），使其逐条对应 `theme.rs::light()` 的权威
palette 定义——这类"测试值与实现漂移"正是变异测试要防的假绿来源。

**全量变异测试**：本轮因单次运行耗时很长，按用户要求暂缓，待后续执行；执行后把
变异分数、存活变异逐条裁决结果回填到本文件。
