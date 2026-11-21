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

## 发版

tag `v0.3.0` = core + 编译器 + fx + 全部后端 + camera（当前）。
CLI 脚手架与主仓的控件层都固定到同一 tag，保证单一来源。
