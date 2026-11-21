# ZephyraCore

ZephyraUI 生态的**核心与编译器栈**独立仓：唯一真相源，CLI 与主仓都单向依赖这里。

## 包含什么

| crate | 作用 |
|---|---|
| `zephyra-core` | 宿主无关的纯计算核心：属性系统、元素树、布局协议、DisplayList 契约 |
| `zephyra-bezier` | 零依赖的三次贝塞尔缓动（`zephyra-core/extra/`） |
| `zephyra-style` | 声明层：`.zstyle` 样式表 → 属性值，含动画补间 |
| `zstyle-compile` | `.zstyle` 解析 / 校验 / 排版 |
| `zypui` | `.zypui` 运行时 IR：解码 / 展开 / 求值 / 实例化 |
| `zypui-compile` | `.zypui` 解析 / 校验 / 编译 |

## 依赖方向

```
zephyra-vector (独立仓)  →  zephyra-core  →  编译器栈
                                  ↑               ↑
                           ZephyraUI 主仓    ZephyraCLI
```

本仓**不依赖** ZephyraUI 主仓，也不依赖任何 GPU / 窗口 / 平台库。
宿主适配（winit / D3D / Vulkan）全部留在主仓的 `zephyra-base/*`。

## 门禁

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## 注意

样式资产门禁（扫全仓 `.zstyle` / `.zypui`）住在**主仓**，因为资产在那里；
本仓只测编译器逻辑本身（解析、排版、IR 不变性）。
