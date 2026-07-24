# ADR-003: 不支持 tmux `-CC` control mode

- 状态: 已接受
- 日期: 2026-07-23
- 关联: spec.md Q3、G2、F30–F38、F35、N-G1;风险 R5

## 背景

tmux `-CC` control mode 能把 tmux 原生的窗口 / pane 结构映射成客户端的原生标签页
(iTerm2 的招牌功能)。问题:Mullion 是否支持它。spec Q3 的初始倾向是「不做」。

## 决策

**不做**(至少 v0.5 之前)。将 spec Q3 的「倾向不做」升级为明确否决。

## 理由与备选

支持 `-CC` 的收益是拿到 tmux 原生窗口结构。但代价是**定位冲突**,不只是复杂度:

- 分屏是本项目的**自研核心**(F30–F38 / `mullion-core`),窗口管理权威在客户端布局树。
  上 `-CC` 会把权威交给 tmux,布局树退化成「反映 tmux 的镜子」,与 G2「分屏是一等公民」打架。
- 项目里 tmux 的角色被定得很清楚:**只负责会话保活**(S3 断线重连后 tmux 还活着),
  不负责窗口管理。`-CC` 把它从「保活后端」提拔成「窗口权威」,是定位漂移,违反 N-G1。
- 复杂度陡增(需实现 tmux control protocol 解析 + 状态同步),不匹配单人维护(R5)。
- F35 规定每个 pane 是 **Mullion 自己开的一条独立 SSH channel**,不是 attach 到 tmux 的某个 pane。
  分屏是 Mullion 开的,本就不需要 `-CC`。

## 后果 / 权衡

- 会话保活仍完全依赖远端 tmux。典型用法:每个 pane 各 attach 一个 tmux session / window,
  这样断线重连后每格都能独立恢复(S3)。

## 重新考虑的触发条件

出现强需求「Mullion 要与其他 tmux 客户端(如 iTerm2 / 别的机器)共享同一套 tmux 窗口」时,
再评估。当前主场景不涉及此需求。
