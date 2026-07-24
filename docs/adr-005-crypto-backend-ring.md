# ADR-005: SSH 加密后端用 ring（替 aws-lc-rs）

- 状态: 已接受
- 日期: 2026-07-24
- 关联: F1（SSH 认证）；[cross-compile-windows](cross-compile-windows.md)；R1（依赖 API 漂移）

## 背景

`russh` 0.54 默认加密后端是 `aws-lc-rs`，它拉入 `aws-lc-sys`——AWS-LC（BoringSSL 分支）
的 C 库，构建期要 **cmake + C 编译器 + NASM**。两个痛点：

- **交叉编译到 Windows 卡死**：aws-lc-sys 要在构建期编 C + 汇编，同时依赖 cmake、汇编器、
  以及能正确识别目标三元组的 CMake toolchain，任一错位就炸。这是 Linux→Windows 交叉编译
  最难的一环（wgpu/winit/glyphon 那堆反而是纯 Rust 绑定，不构成 C 构建）。
- **原生 Windows 编译要装 NASM**：给用户增加环境负担。

`russh` 的 feature 表提供 `ring` 作为一等替代后端（`ring = ["dep:ring"]`）。

## 决策

**workspace 依赖改为：**
```toml
russh = { version = "0.54", default-features = false, features = ["ring", "flate2", "rsa"] }
```
- `ring` 替 `aws-lc-rs`：依赖树里基本只剩纯 Rust + ring（ring 交叉编译到 windows-gnu 干净）。
- 保留 `rsa`：RSA 密钥走独立的 `rsa` crate，**与后端无关**，`rsa-sha2-512` 行为不变。
- 保留 `flate2`：其默认 `rust_backend`（miniz_oxide，纯 Rust），**不**引入 `zlib-sys`（已核 Cargo.lock）。

## 备选与否决

- **保持 aws-lc-rs + cargo-xwin（MSVC ABI 交叉）**：最难的 aws-lc-sys C 构建仍原样保留，
  cargo-xwin 只缓解 CMake toolchain 一环，还要装 clang/llvm/ninja + 下 ~1GB MS SDK。投产比最差。否。
- **保持 aws-lc-rs，只在原生 Windows 编**：可行但要用户装 NASM，且放弃「本机交叉出 exe 快速迭代」。否。

## 后果 / 权衡

- **这是运行时行为变更，不只是 build 选项**：ring 与 aws-lc-rs 暴露给 russh 的
  KEX / cipher / host-key 算法集合不完全一致——连老服务器或特定厂商设备时协商结果可能变。
  → **必须对真实目标主机跑 live 验证**，hermetic 测不出协商差异。
  已验证：`MULLION_LIVE=1` 的 `pubkey_live` 打 `user@192.0.2.10` 成功（`echo MULLION_OK` 回显）。
- **FIPS**：aws-lc-rs 是 FIPS 那条路，ring 不是。个人工具无所谓；若将来有 FIPS 要求需回切。
- **收益**：交叉编译干净、原生 Windows 免装 NASM、构建更快、依赖更小。
- **revert 一行**：改回 `russh = "0.54"` 即恢复 aws-lc-rs。

## 重新考虑的触发条件

- 出现「某算法只有 aws-lc-rs 有、ring 没有」导致连不上某台机器 → 先查 aws-lc-sys 的
  `AWS_LC_SYS_PREBUILT_NASM` 等环境开关能否绕开 NASM（版本间行为不一，用前查 crates.io 文档），
  再决定是否整体回切。
- 出现 FIPS 合规要求。
