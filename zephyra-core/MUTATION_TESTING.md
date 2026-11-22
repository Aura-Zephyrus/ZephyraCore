# ZephyraCore 变异测试

## 目的

测试质量用变异测试（`cargo-mutants`）验证：**把源码注入 bug，看测试是否失败**。
测试杀不死的变异叫"存活变异"（survivor），它要么暴露测试缺口，要么是被证明无害的等价变异。
本轮要求"保证测试准确性"，因此不允许对存活变异置之不理——每一条都必须被裁决。

## 覆盖范围（CORE + BASE 共 24 个 crate）

| 集合 | crate | 配置 |
|---|---|---|
| core | `zephyra-core` | `.cargo/mutants.toml`（master，30 个自有逻辑文件；5 个纯 `pub use` 空壳排除） |
| CORE（7） | `zephyra-bezier`、`zephyra-style`、`zstyle-compile`、`zypui`、`zypui-compile`、`zephyra-fx`、`zephyra-svg-anim` | `.cargo/mutants/<crate>.toml` |
| BASE（13） | `zephyra-render-ext`、`zephyra-ext`、`zephyra-ext-wgpu`、`zephyra-platform`、`zephyra-platform-linux`、`zephyra-platform-win`、`zephyra-app-config`、`zephyra-window`、`zephyra-winit-input`、`zephyra-winit`、`zephyra-winit-d3d12`、`zephyra-vulkan`、`zephyra-dcomp` | `.cargo/mutants/<crate>.toml` |
| BASE/extra（3） | `zephyra-media`、`zephyra-uia`、`zephyra-camera` | `.cargo/mutants/<crate>.toml` |

每个 per-crate 配置都设 `examine_globs` 为该 crate 的 `src/**/*.rs`（平台 crate 见下方收窄）、
`test_package = ["<crate>"]`、`output = "mutants.out/<crate>"`、
`timeout_multiplier = 4.0` / `build_timeout_multiplier = 4.0`、
`skip_calls_defaults = true`、`copy_target = false`。纯 `pub use` 文件（如各 `prelude.rs`）
用 `exclude_globs` 排除。

## 运行

```bash
# 安装（已装可跳过）
cargo install cargo-mutants --locked

# Windows：master 配置 copy_target = true，会复制约 8.8 GB 的 target 到临时目录换增量编译；
# C 盘空间不足时必须把 TEMP/TMP 指向大容量盘（本机 C 盘仅剩 3 GB）。
$env:TEMP = 'E:\tmp'; $env:TMP = 'E:\tmp'
$env:CARGO_TARGET_DIR = 'E:\tmp\zc-a11-target'

# 单个 crate（per-crate 配置 copy_target = false，可原地跑、不复制 target）
cargo mutants -p zephyra-style --config .cargo/mutants/zephyra-style.toml --in-place

# 或并行跑（--in-place 与 --jobs 互斥，二选一）
cargo mutants -p zephyra-style --config .cargo/mutants/zephyra-style.toml --jobs 8

# core 沿用 master 配置（copy_target = true），并行时不要加 --in-place
cargo mutants -p zephyra-core --jobs 8

# 只列变异体（不变异、不构建），用于校验配置
cargo mutants -p zephyra-camera --config .cargo/mutants/zephyra-camera.toml --list
```

> `--in-place` 只能由命令行给出（见 cargo-mutants options.rs），不写进配置；配置里的
> `copy_target` 决定是否复制 `target` 换取增量构建。master 的 `copy_target = true` 必须配
> 大容量临时盘；per-crate 的 `false` 让本地可直接 `--in-place`，CI 则走默认副本模式。

## 平台 / 设备 crate 的作用域（有意折中）

GPU / DComp / Vulkan ICD / 摄像头 / 音频设备的真实路径，其测试都标了 `#[ignore]`
（例如 `zephyra-winit/tests/command_validation.rs`、`zephyra-winit-d3d12/tests/swapchain_draw.rs`、
`zephyra-vulkan/tests/ext_contract_stubs.rs`、`zephyra-media/tests/audio_sink_leak.rs`）。
headless 环境跑不到这些测试，它们**无法杀死变异体**；把对应源文件圈进来只会产生永远
存活、又无法裁决的假红。因此这些 crate 只变异有可执行单元测试的纯 helper：

| crate | `examine_globs` 限定的文件 |
|---|---|
| `zephyra-winit` | `renderer.rs`、`shader.rs` |
| `zephyra-winit-d3d12` | `renderer.rs`、`shader.rs`、`swapchain.rs` |
| `zephyra-vulkan` | `ext.rs`（纹理 / Hook 资源记账，不碰设备） |
| `zephyra-dcomp` | `lib.rs`（`SwapchainDesc` / `base_swapchain_desc` / `create_factory`） |
| `zephyra-platform-win` | `version.rs`、`locale.rs`、`live_resize.rs` |
| `zephyra-uia` | `model.rs`、`rules.rs` |
| `zephyra-camera` | `pixels.rs`、`quality.rs`、`recovery.rs`、`device.rs`、`controls.rs`（默认 feature 零设备依赖） |
| `zephyra-media` | `output.rs`、`demux.rs`、`decode_thread.rs`、`video.rs`、`clock.rs`、`error.rs` |

这是"覆盖全部 crate"与"每条存活变异都可裁决"之间的明确取舍，不是遗漏；有真机 / 设备的
环境后续可放宽对应 globs 并开启相应 feature 重跑。

## CI 门禁语义

`.github/workflows/ci.yml` 的 `mutants` job 是 24 路矩阵（每 crate 一路），每次 PR 跑全量：

- **不设分数硬线**。
- `cargo-mutants` 退出码 `2` 表示存在存活变异 → 该 crate 的 job 失败。
- 每路上传独立报告 artifact `mutants-report-<crate>`，路径 `mutants.out`。
- CI 命令：`cargo mutants -p <crate> --config .cargo/mutants/<crate>.toml --jobs 4`
  （core 用 `.cargo/mutants.toml`）。

## 等价变异的裁决规则与 `exclude_re`

确认无害的等价变异必须在**对应 crate 的配置**的 `exclude_re` 中显式列出并写明理由：

```toml
# cargo-mutants schema: 字符串数组，正则匹配 `cargo mutants --list` 输出的名称。
# 必须逐条写明等价理由；不得用宽泛正则批量豁免，也不得调高超时/删除测试来"刷绿"。
exclude_re = ["replace <exact mutant name from --list> with its anchored regex"]
```

文档中目前只给出裁决规则，没有逐条确认并登记的等价变异，故各配置只保留上述格式说明注释
（注释掉 `exclude_re = []`）。一旦裁决出具体等价变异，取消注释并填入精确正则。

## 裁决流程

对每个存活变异按以下顺序处理：

1. **测试缺口**：在公开 API 层补一条断言字面量的测试（不复用被测字段自证，避免"恒绿"），重跑该变异确认被杀死。
2. **等价变异**（改与不改行为一致，如 rank 元组中永不重复的第三分量比较符）：加入该 crate 配置的 `exclude_re` 并写明理由。
3. **不可编译变异**（unviable）：无需处理，`cargo-mutants` 不计入失败。
4. **超时**：先排查是否真有死循环；确认是超时阈值过低则调高倍数，不允许直接忽略。

## 当前状态

**已完成的补测试**（core，本轮新增约 157 条，均用字面量断言，逐条对应一个具体变异）：

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

**全量变异测试**：全部 24 份配置已用 `cargo mutants --config <cfg> -p <crate> --list` 逐一校验通过
（只列变异体、不变异）；实际变异战役耗时很长，按用户要求暂缓执行，待运行后把各 crate 的
变异分数、存活变异逐条裁决结果回填到本文件。
