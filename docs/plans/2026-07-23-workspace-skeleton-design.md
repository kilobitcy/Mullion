# 设计文档:Mullion workspace 骨架

- 日期: 2026-07-23
- 状态: 已批准(brainstorming),待 plan 批准
- 关联: spec.md(架构/守护测试)、README.md、CLAUDE.md 陷阱表、ADR-001/002/003

## 问题陈述

spec / README / CLAUDE 已完整描述四-crate 架构、依赖方向、六个守护测试与库选型,
但仓库里**代码零行**(只有文档)。需要先立起一个可编译的 workspace 骨架,
把"能在无 GPU / 无真实 SSH 下单测"的守护测试作为红线基线立起来——
这正是本项目可测试性的核心资产(布局 bug、键码 bug 能脱离窗口复现)。

## 设计决策与理由

1. **四 crate + 严格单向依赖** `app → {core, term, ssh}`(架构不变量)。
   价值:布局/键码/攒帧/reflow 都能脱离窗口写测试。

2. **三个"挂在 app 上"的守护测试(T2/T3/T4)抽成无 wgpu/winit 的纯件**:
   - T2 攒帧 → 纯状态机 struct,present 用抽象回调
   - T3 帧率封顶 → 可注入时钟的节流器(不用真实时间)
   - T4 reflow → 布局变更算新行列,经抽象 resize sink 发出,测试注入 fake sink
   这样它们和布局/键码一样能无窗口测。

3. **守护测试的红/绿策略**:不依赖真实 GPU/SSH 的部分(core 布局、term keymap+emulator、
   app 三个纯逻辑)在骨架阶段**实现到绿**;GPU 渲染、真实 SSH 链路只留**可编译占位**
   并标注「未验证,需人工确认」。不假装绿。

4. **render 层按 ADR-001 留窄接口**(`draw(grid, damage, surface)`),glyphon 只是一个实现;
   骨架不实现真实渲染。

5. **KnownHosts 桩绝不返回 `Ok(true)`**(README/spec F3 TOFU 红线),留一个 `verify()` 返回 false 的骨架。

## 替代方案

- **先只搭 core + term**:作为离线回退——若 `cargo add` 拉不到 alacritty_terminal/russh/winit/wgpu/glyphon,
  则 core(纯 Rust 无外部依赖)仍可完整搭建 + 测试,其余 crate 先不引依赖、只留纯占位。

## 已解决的开放问题

- Q1/Q2/Q3 → 见 ADR-001/002/003(已决)。
- 分支名 → `main`,本机管理无远端。
- 配置格式 TOML(ADR-002)→ 骨架**不实现配置**,仅记录方向。

## 明确不做(YAGNI / Scope)

功能实现一律不做:SFTP、代理、跳板机、持久化、真实渲染、真实 SSH 认证、tmux -CC。
骨架只立结构 + 守护测试基线。
