# F336/F337 设计定案:任务栏可见性(窗口标题 + 响铃待办)

> 2026-10-29 /grill-me 逐条定案。切片一(共两片;切片二「隧道跨实例可见 + 状态栏入口」
> F338/F339 另立 spec、另发版)。
> 实现顺序:F337-a 后台标签照常泵(独立第一个提交)→ F337 数据层(bell 采集 → 待办判据)
> → F336 窗口标题 → F337 UI 标记。
> 2026-10-29 经 /review 复核修订(C1/C2/R1~R10,见文末「复核修订记录」)。

## 背景

用户在 Windows 11 上把 Mullion 最小化/被遮挡后,远端 Claude Code 跑完、等输入,
从任务栏上看不出来;多开时任务栏缩略图全叫 `mullion`,分不清谁是谁。

现状(已核实):
- 窗口标题写死 `"mullion"`(`app.rs` 的 `Window::default_attributes().with_title`),此后从不改。
- `emulator.rs` 的 `EventSink` 把 `Event::Bell` 落进 `_ => {}` 丢弃。
- 全仓无 `request_user_attention` / `FlashWindowEx` / overlay icon。

## 已核实的平台事实

| 事实 | 依据 |
|---|---|
| alacritty 0.26 遇 BEL 发 `Event::Bell` | `alacritty_terminal-0.26.0/src/term/mod.rs` `bell()` |
| vte 0.15 `osc_dispatch` 不处理 OSC 9 / 777(落 `unhandled`) | `vte-0.15.0/src/ansi.rs` |
| **tmux 3.7b 默认配置下,当前 window 与后台 window 的 BEL 都转发给客户端;OSC 9 / OSC 777 被整段吞掉** | 本机实测:`script` 录 tmux **客户端**流,`-f /dev/null` |
| winit 0.30.13 Windows 的 `request_user_attention(Some(Informational))` = `FlashWindowEx(FLASHW_TRAY \| FLASHW_TIMERNOFG)`:任务栏按钮**持续**橙色直到窗口被激活;窗口已是 `GetActiveWindow()` 时直接 return | `winit-0.30.13/src/platform_impl/windows/window.rs` |
| Claude Code 默认 `preferredNotifChannel = "auto"`:只对 iTerm2/Ghostty/Kitty 发通知,其他终端**不发任何东西**;设 `"terminal_bell"` 才发 BEL | code.claude.com/docs/en/settings-reference.md、terminal-config.md |

推论:**只认 BEL**。OSC 9 在主场景(tmux)下到不了客户端,不写嗅探器。

## 定案决策

1. 「看出来」= **有事要我处理(A)+ 区分是哪个窗口(B)**。overlay 角标(`ITaskbarList3`)不做。
2. 不做走马灯标题:任务栏默认合并不显标签、定时改标题破坏空载零唤醒(T3/N3)、悬停瞬间只见一段。
3. 待办触发源只认 BEL。断线、隧道失败**不进**待办(留给状态栏/toast;进了任务栏就天天闪、被习惯性无视)。不做输出静默检测(启发式误报)。
4. 前台可见时任务栏不闪;BEL 来自焦点 pane 且窗口有焦点 → 丢弃(用户正看着)。
5. 待办清除 = **该 pane 被看到**:窗口有焦点且它是活动标签的焦点 pane。不是「窗口一激活全清」(3 pane 只看了一块,另两块白提醒),也不要求键盘输入(只看结果不打字时标记赖着不走)。
6. 后台标签页里的 pane 待办 → 标签栏对应标签也挂标记。**前提是 F337-a**(见下)。
7. 焦点在抽屉(F290)上时,其父 pane 也算「被看到」——两者上下贴着同屏。

## ⓪ F337-a 后台标签照常泵(前置,独立提交)

现状:`pump_io`(`app.rs`)只对 `self.active_ws_mut()` 调 `ws.pump` 与
`ws.flush_expired_vt_sync`,全仓生产代码仅此一处。后台标签 rx 不排空、emulator 不前进:
- BEL 压在 rx 里,切回该标签时才喂进 vte,此刻它恰是「活动+焦点+窗口有焦点」,
  `should_mark` 判丢弃——**待办蒸发**,决策 6 不可能成立;
- 后台标签的光标查询/同步探测无应答,有界 rx(256)灌满后堵住 io_task(T1 同族,
  spec/docs 无任何「只泵活动标签」的刻意理由记录;对照 F128 `drive_reconnects` 明确要求遍历全部标签)。

改法:`pump_io` 遍历**全部**标签的 Workspace,`pump` + `flush_expired_vt_sync` 都做;
「是否需要重绘」只由活动标签的结果决定(后台标签不出帧,不标 `ui_dirty`)。
- 代价:后台流式输出实时做 VT 解析。这些字节切回时本来也要解析,总工作量不变,只是不再延迟。
- 不写 ADR:不涉及框架/库/依赖方向,属事件循环行为修正;理由记在本 spec 与提交正文。
- 改完前后各跑一遍 T1/T3/T7 守护:`emulator::tests::pty_write_is_collected`、
  `app::tests::redraw_is_frame_capped`、`frame::tests`。
- 守护:后台标签 pane 的 `PtyWrite` 被回写(行为式);后台标签有入站字节不触发重绘(T3)。

## ① 数据流

```
远端 BEL ─▶ alacritty Event::Bell
         ─▶ EventSink.bell: Arc<AtomicBool> 置 true   (emulator.rs,单列一臂,不落 `_ =>`)
         ─▶ Emulator::take_bell() -> bool             (读即清,形态同 take_remote_state)
         ─▶ Workspace 收 bell → PaneState.bell_seen   (Workspace 只记事实,不判待办)
         ─▶ App 层 should_mark → PaneState.attention
```

- **收 bell 的时机(C2)**:同步块(BSU/ESU)里的 BEL 要到 `flush_expired_vt_sync` 收口时
  才喂进 `Term` 才发 `Event::Bell`,而 flush 排在 `pump` 之后;最小化态只有 `UserEvent::Wake`
  才跑 `pump_io`,没有下一个入站字节就没有下一次 pump。所以 bell 的收集是 Workspace 上一个
  独立的 `collect_bells()`(纯取 `AtomicBool`,零分配),在 `pump_io` 里排在
  `flush_expired_vt_sync` **之后**调,覆盖全部标签。
- **落层(R2)**:`Workspace` 不知道 `window_focused` 和「我是不是活动标签」,只产出
  `bell_seen`;`should_mark` 与清除不变量在 App 层判,一处判一次。
- **置位**(纯函数):`should_mark(bell, pane_is_seen, window_focused) = bell && !(pane_is_seen && window_focused)`。
  `pane_is_seen` = 是活动标签的焦点 pane,**或**焦点在它的抽屉上(决策 7)——后台标签的焦点 pane 不算。
- **清除**(纯函数,按不变量每次 pump 后重算,不挂在入口上):
  `window_focused && pane_is_seen ⇒ attention = false`。
  理由:清除入口有切 pane 焦点 / 切标签 / `Focused(true)` 三处,逐个挂是列举式门控
  (本项目已踩四次);不变量式一处收口。
- **任务栏闪(R1)**:判据是**事件**不是状态跃迁——一次 `should_mark` 为真的 BEL 到达时,
  若 `!window_focused && !flashed_since_blur`,调
  `window.request_user_attention(Some(UserAttentionType::Informational))` 并置
  `flashed_since_blur = true`;`WindowEvent::Focused(true)` 臂复位。
  (按 `attention` 跃迁判会漏:pane 在窗口有焦点时已被标、随后失焦、同一块再响 → 无跃迁 → 不闪。)
  Informational 的橙色本身持续到激活,重复调用无意义。
- **焦点初值(R3)**:`window_focused` 构造时写死 `true`;多实例恢复时从未拿到焦点的窗口
  winit 不补发 `Focused(false)`。窗口创建后用 `window.has_focus()` 校准一次。
- `attention` 是 pane 的瞬态,**不进** F37/F148 快照;pane 关闭即随之消失。重连(`reattach_pane`)保留。

## ② 窗口标题(F336)

纯函数 `window_title(view: TitleInputs) -> String`:

- 主体:活动标签焦点 pane 的**项目名**,无项目时用 **host 标签**(会话名或 user@host)——
  与 `TitleView.project` / `.host` 同源推导,不另记。
- `(+N)`:整个窗口其余**真 pane** 数(抽屉不计,口径同 `pane_count`);N=0 不显示。
- 有待办:前缀 `[!] <首个待办 pane 的名字> · `;「首个」按标签顺序 → 几何序号,判据确定。
  **首个待办 pane 就是主体那块时只加 `[!] `、不重复名字(R4)**——窗口失焦时焦点 pane 也能被标。
- 无任何 pane(launcher):`mullion`。
- 例:`[!] 项目B · 项目A (+2) — mullion`、`[!] 项目A (+2) — mullion`、`项目A — mullion`。
- 调用点 `about_to_wait`:现算 → 与 `last_title: String` 比,**不同才 `set_title`**(跃迁触发,无定时器)。
  「算」与「`set_title`」分开;项目名复用构造 `TitleView.project` 的同一份推导(R8)。
- **不得挪进 `RedrawRequested` 臂**:最小化态不出帧,而标题恰在那时最需要更新(R8)。
- 标题走系统字体,不受 T9 约束;`[!]` 用 ASCII 只为与应用内风格一致。

## ③ UI 标记

- pane 标题条:`TitleView.attention: bool`,序号前画 `ui::icon` 自绘实心圆点,`warn` 色(T9:不用字符)。
- 标签栏:`TabView.attention: bool`,由「该标签内任一 pane 待办」**推导**,同款圆点。
- 两处过 F100 `annotate::mark` 登记。

## ④ 测试

单测:
- `emulator`:喂 `\x07` → `take_bell()` 为 true,再取为 false;配 VT fixture(tmux 客户端流里的 BEL 片段,录制见 `docs/vt-fixtures.md`)。
- `should_mark` 四象限;清除不变量(后台标签焦点 pane 不清、窗口失焦时焦点 pane 不清);`window_title` 各形态(无 pane / 无项目 / +N / 抽屉不计 / 待办前缀与「首个」判据)。
- 失焦期间连续 BEL 只请求一次 attention;`Focused(true)` 后再失焦可再次请求;
  已标的 pane 失焦后再响**仍会**请求(R1 的反例)。
- 抽屉有焦点时父 pane 的 BEL 不标、已有标记被清(决策 7)。

行为式守护(R9,源码切片属恒绿高发族,关键路径必须有行为式):
- 最小化路径:只走 `UserEvent::Wake → pump_io`,BEL 位于一个超时收口的同步块内 → bell 被收、
  flash 判据被触发(同时覆盖 C2)。
- 后台标签的 pane 收到 BEL → 该 pane `attention` 置位、标签推导为有待办(覆盖 F337-a + 决策 6)。

源码切片守护(接线断了纯函数照绿):
- `collect_bells()` 在 `pump_io` 中位于 `flush_expired_vt_sync` 之后。
- `Focused` 臂复位 `flashed_since_blur`。
- `set_title` 前有与 `last_title` 的不等比较(T3)。
- `EventSink` 有 `Event::Bell` 独立一臂。

## 无法自动验证(人工验收清单)

- 最小化 / 被遮挡时远端 `printf '\a'`:任务栏按钮变橙并保持;激活窗口后停。
- 前台时 BEL 不闪;非焦点 pane / 后台标签出现圆点,看过去即消失。
- 任务栏悬停缩略图与 Alt+Tab 中的标题文字。
- Claude Code 完成任务后真实触发(需远端前置)。

## 远端前置(用户侧,一次性)

- 每台远端机 `~/.claude/settings.json`:`{ "preferredNotifChannel": "terminal_bell" }`。
- 远端 tmux 若被配成 `bell-action none` / `monitor-bell off`,BEL 到不了客户端。

## 已知限制(行为如此,验收时别当 bug 报)

- 高延迟链路上,焦点 pane 里按 Tab/退格引出的 readline BEL 若在你 Alt+Tab 走开**之后**才到达,会闪一次(R6)。
- tmux `bell-action any` 会把**同会话其它 window** 的 BEL 也转发进这块 pane,切过去可能看到内容没变(R7)。
- 原生文件对话框(`rfd`)开着时主窗口失焦,此时 BEL 会让任务栏闪自己(R10)。
- 最小化态只靠入站字节(`Wake`)驱动泵:同步块若既无 ESU、之后也再无字节,超时收口要等窗口恢复,块里的 BEL 随之延后(PumpOnly 分支本就 `Wait`、不排 `next_timer_wake`,切片前已如此;Claude Code 的块都正常 ESU,风险低,改它要动 T7 排期,不值)。
- 标题条关掉(F83,`title_px.h == 0`)时 pane 级圆点不画;窗口标题 `[!]` 与标签圆点照常。

## 明确不做

OSC 9 / 777;声音;输出静默检测;overlay 角标;走马灯标题;断线/隧道失败进待办;
跨实例跳转窗口(属切片二范围,也已否掉)。

## 复核修订记录(2026-10-29 /review)

- **C1** 后台标签不被泵 → 新增 F337-a(用户定案:遍历全部标签,独立第一个提交)。
- **C2** 同步块里的 BEL 晚于 `take_bell` → `collect_bells()` 排在 `flush_expired_vt_sync` 之后。
- **R1** 闪烁判据由状态跃迁改为事件 + `flashed_since_blur`。
- **R2** Workspace 只记 `bell_seen`,待办判据在 App 层。
- **R3** `has_focus()` 校准焦点初值。
- **R4** 标题前缀不自指重复。
- **R5** 抽屉焦点算父 pane 被看到(决策 7)。
- **R6/R7/R10** 列入已知限制。
- **R8** 标题只在 `about_to_wait` 算,写明理由。
- **R9** 补两条行为式守护。
