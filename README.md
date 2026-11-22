# ZephyraCore

ZephyraUI 生态的**库层单仓**：core、编译器栈、效果器与全部渲染后端。
CLI 与主仓都单向依赖这里；本仓不依赖它们中的任何一个。

## 包含什么

| 类别 | crate |
|---|---|
| 核心 | `zephyra-core`（属性系统、元素树、布局协议、DisplayList）、`zephyra-core/extra/zephyra-bezier` |
| 声明层 / 编译器 | `zephyra-style`、`zstyle-compile`、`zypui`、`zypui-compile` |
| 效果 / 动画 | `zephyra-fx`、`zephyra-svg-anim` |
| 系统适配 | `zephyra-base/*`：winit、winit-d3d12、vulkan、dcomp、platform*、window、ext*、app-config、uia |
| 可选桥接 | `zephyra-base/extra/zephyra-media`（依赖独立仓 zephyra-video） |
| 可选采集 | `zephyra-base/extra/zephyra-camera`（默认零设备依赖，`capture`/`dx` feature 按需） |

## 依赖方向

```
zephyra-vector / zephyra-video (独立仓)
                  ↓
            ZephyraCore (本仓)
              ↑          ↑
        ZephyraCLI   ZephyraUI 主仓（控件层）
```

本仓唯一的外部依赖是 `zephyra-vector`（git）、`zephyra-video`（git）与
`cssparser` / `naga` 等 crates.io 包；不含任何主仓依赖。

## 门禁

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

`default-members` 只含核心与编译器（纯计算、跨平台）；后端按需 `-p` 选择，
避免默认构建拉入 Windows COM / D3D / Vulkan 的平台依赖树。

## 变异测试

全仓 CORE + BASE 共 **24 个 crate** 的测试质量由 `cargo-mutants` 验证：把源码注入 bug，
看测试是否失败。每个 crate 一份配置，`-p` 与 `--config` 必须成对给出。

| 集合 | crate | 配置 |
|---|---|---|
| core | `zephyra-core` | `.cargo/mutants.toml`（master） |
| CORE（7） | `zephyra-bezier`、`zephyra-style`、`zstyle-compile`、`zypui`、`zypui-compile`、`zephyra-fx`、`zephyra-svg-anim` | `.cargo/mutants/<crate>.toml` |
| BASE（13） | `zephyra-render-ext`、`zephyra-ext`、`zephyra-ext-wgpu`、`zephyra-platform`、`zephyra-platform-linux`、`zephyra-platform-win`、`zephyra-app-config`、`zephyra-window`、`zephyra-winit-input`、`zephyra-winit`、`zephyra-winit-d3d12`、`zephyra-vulkan`、`zephyra-dcomp` | `.cargo/mutants/<crate>.toml` |
| BASE/extra（3） | `zephyra-media`、`zephyra-uia`、`zephyra-camera` | `.cargo/mutants/<crate>.toml` |

本地运行（workspace root）：

```bash
cargo install cargo-mutants --locked   # 已装可跳过

# Windows：master 配置 copy_target = true，会把约 8.8 GB 的 target 复制进临时目录换增量编译；
# C 盘空间不足时必须把 TEMP/TMP 指到大容量盘。
$env:TEMP = 'E:\tmp'; $env:TMP = 'E:\tmp'
$env:CARGO_TARGET_DIR = 'E:\tmp\zc-a11-target'

# per-crate 配置均为 copy_target = false：可原地跑（不复制 target）
cargo mutants -p zephyra-style --config .cargo/mutants/zephyra-style.toml --in-place

# 或并行跑（--in-place 与 --jobs 互斥，二选一）
cargo mutants -p zephyra-style --config .cargo/mutants/zephyra-style.toml --jobs 8

# core 沿用 master 配置（copy_target = true），并行时不要加 --in-place
cargo mutants -p zephyra-core --jobs 8
```

`--in-place` 只能由命令行给出（见 cargo-mutants options.rs），不写进配置；配置中的
`copy_target` 决定是否复制 `target` 以获取增量构建。`exclude_re` 是等价变异的唯一豁免口：
字符串数组，正则匹配 `cargo mutants --list` 输出的名称，必须逐条写明理由。

**平台/设备 crate 的作用域（有意折中）**：`zephyra-winit` / `zephyra-winit-d3d12`
只变异 `renderer.rs` / `shader.rs` / `swapchain.rs`，`zephyra-vulkan` 只变异 `ext.rs`，
`zephyra-dcomp` 只变异 `lib.rs`，`zephyra-platform-win` 只变异 `version.rs` / `locale.rs` /
`live_resize.rs`，`zephyra-uia` 只变异 `model.rs` / `rules.rs`，`zephyra-camera` 只变异
`pixels.rs` / `quality.rs` / `recovery.rs` / `device.rs` / `controls.rs`，`zephyra-media`
只变异 `output.rs` / `demux.rs` / `decode_thread.rs` / `video.rs` / `clock.rs` / `error.rs`。
真实 GPU / DComp / Vulkan ICD / 摄像头 / 音频设备的路径其测试都带 `#[ignore]`，headless
环境根本跑不到，无法杀死变异体；把这些文件圈进来只会制造永久存活的假红。这是"覆盖全部
crate"与"每个存活变异都可裁决"之间的明确取舍，详见 `zephyra-core/MUTATION_TESTING.md`。

CI 的 `mutants` job 是 24 路矩阵，每个 PR 对每 crate 跑全量，并上传独立报告。它**不设分数
硬线**，但任何未被测试杀死的变异都会让 job 失败（`cargo-mutants` 退出码 2）。存活变异按
4 步裁决：① 补字面量断言的测试杀死它；② 证明等价后加入该 crate 配置的 `exclude_re` 并写
理由；③ 不可编译变异（unviable）无需处理；④ 超时先查真死循环，确认阈值过低才调高倍数。

等价变异的裁决规则、已补测试清单与全量运行记录见 `zephyra-core/MUTATION_TESTING.md`。

## 发版

- tag `v0.2.0` = core + 编译器 + fx + 全部后端。CLI 脚手架与主仓的控件层固定于此。
- `main` 额外含 `zephyra-camera`（可选采集 crate），未打新 tag——需要它的应用可直接依赖 `main` 或后续 tag。
