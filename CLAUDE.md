# Mullion — 项目上下文

> 全局偏好（语言、精简度、Scope Discipline、防注入）见 `~/.claude/CLAUDE.md`
> 与 `~/.claude/rules/`。**此处不重复，只写这个项目特有的东西。**


## 背景：有多台ubuntu 服务器，已安装了claude code，进行开发，原先在Windows11中，用powershell+ssh，连接到ubuntu。tmux只用来保持claude code连接（ssh离线后保活）

## 项目是什么
原生 GPU 加速的 SSH 客户端。核心场景：**在 Windows 上，通过高延迟代理链路，操作跑在远端 tmux 里的 Claude Code TUI**，支持多开。

需求细节见 `spec.md`。功能要有 spec 里的编号（F1/N3…），提交信息和测试名都引用它。

**边界**（超出范围的需求先问，别自己开工）：
- 不做终端复用器。会话保活是远端 tmux 的事，我们只做客户端。
- 不做 AI 侧栏、不做数据库客户端、不做 RDP/VNC。
- macOS/Linux 能编过就行，**Windows 11 是唯一的一等公民**。

---

## 架构不变量

改动不得违反。违反了就是设计错误，不是「先跑起来再说」。

```
mullion-core     布局树。零 UI、零 IO、零 async。可纯单测。
mullion-term     VT 仿真封装 + 输入编码。只依赖 alacritty_terminal / vte。
mullion-ssh      russh。不认识「pane」「窗口」这些概念，只认字节流。
mullion-store    会话/凭据持久化。TOML + keyring 加密。零 UI、零 async、仅同步 IO。可纯单测。
mullion-app      winit + wgpu + glyphon(终端自绘)+ egui(外壳:菜单/状态栏/会话弹窗)。唯一允许知道其余四者的地方。
```

**依赖方向严格单向**：`app → {core, term, ssh, store}`，其余互不依赖。

这条约束的全部价值在于：**布局 bug 和键码 bug 能在没有窗口的情况下写测试复现**。
这两类是本项目最费时间的 bug。任何「为了方便」把 UI 类型漏进 core/term 的改动，
等于把项目的可测试性拆掉，直接拒绝。

如果你觉得某个功能非得打破这个方向才能实现——**停下来问我**，多半是抽象没找对。

---

## 领域陷阱

动到相关代码前必读。每条都配了对应测试，改完要能指出跑的是哪个测试。

| # | 陷阱 | 症状 | 守护测试 |
|---|---|---|---|
| T1 | `Event::PtyWrite` 没回写 SSH channel | 同步输出探测无应答 → 全屏 TUI 闪；鼠标全废；光标查询永久卡死 | `emulator::tests::pty_write_is_collected` |
| T2 | 收到 `CSI ? 2026 h` 后没攒帧 | 流式输出时撕裂、抖动 | `render::tests::sync_update_defers_present` |
| T2' | 以为「实现了攒帧」就等于攒帧生效了 | **tmux 只在外层终端登记了 `sync` 特性时才把内层的 BSU/ESU 往外发，否则整个吞掉**。我们报 `xterm-256color`，不在 tmux 内置特性表里 —— T2 的攒帧在主场景（tmux）下**从来没生效过**，而且客户端侧一切正常、零报错，只有拿 `script` 抓字节流数 BSU 才看得见。登记必须在 attach **之前**（运行期改对已 attach 的 client 无效），必须是独立 shell 语句（串进同一次 tmux 调用的话，老 tmux 上 set 失败会中止后面全部命令），必须写死数组下标（`set -a` 不去重，每次重连长一条） | `automation::tests` 四条（`the_sync_feature_is_registered_before_the_client_attaches` 等）；`session_map::tests::the_term_we_ask_the_pty_for_is_the_one_we_register_tmux_sync_against` |
| T3 | 喂数据和重绘没解耦 | 每秒几千次重绘，GPU 空转、风扇起飞 | `app::tests::redraw_is_frame_capped` |
| T4 | 分屏后没发 `window_change` | tmux 里的 TUI 按旧列数排版，全屏直接错行 | `app::tests::reflow_emits_resize` |
| T5 | 鼠标上报没有 Shift 逃生门 | `/tui fullscreen` 下用户永远无法划选复制 | `keymap::tests::shift_blocks_mouse_report_so_user_can_copy` |
| T6 | Shift+Enter 编码错 | Claude Code 里无法插入换行，一按就提交 | `keymap::tests::shift_enter_without_kitty_is_esc_cr` |
| T7 | 帧率节流后 `ControlFlow::WaitUntil` 不复位 | 首次节流后永久 100% CPU 忙转（T3/N3 红线） | `frame::tests`（`plan` 决策 4 条）；事件循环三分支须显式复位 control_flow |
| T8 | 判给终端的键盘事件仍先喂 `egui_state.on_window_event` | egui 焦点系统吞掉 Tab → 焦点跳到菜单栏 → `wants_keyboard_input()` 恒 true → 终端**永久**收不到任何键（Tab 补全后回车/退格全废，鼠标仍灵） | `input_route::tests::terminal_keyboard_is_never_fed_to_egui_so_tab_cannot_steal_focus`；键盘先判后喂，指针先喂后判 |
| T9 | 往 egui 的 UI 字符串里直接写非 ASCII 符号 | egui 字体链只有两级（内置 + 微软雅黑），链外字形画成豆腐块 `□`；**编译/测试/日志全静默，只有人眼能看见**，且 Linux 开发机上多半是正常的 | `tests/glyph_whitelist.rs::no_ui_string_contains_a_glyph_the_font_cannot_draw`；要么登记进 `ui::glyphs::VERIFIED`（判据是 **GBK** 内，不是 GB18030），要么走 `ui::icon` 自绘 |
| T10 | 以为窗口的 IME 开关是自己说了算 | `egui-winit` 每帧按「egui 里有没有文本框在组字」调 `set_ime_allowed`，**关掉的是整个窗口的 IME**。终端不是 egui 部件，egui 永远不知道它也需要 IME——用户点过一次任意输入框再回终端，中文**永久**打不出来（按 Windows 中英文切换键毫无反应），且没有自愈路径，只能重启 exe。同族的还有「`WindowEvent::Ime` 绕过输入分流」：egui 输入框里打的中文会**同时**上屏和发到远端 shell | `input::tests::the_ime_ledger_is_clamped_to_false_so_egui_never_disables_the_window_ime`；`app::tests::the_ime_ledger_is_clamped_before_egui_gets_a_chance_to_disable_it`（顺序错了完全失效且静默）；`app::tests::ime_reaches_the_terminal_only_when_the_keyboard_would` |
| T11 | 「发完之后再做 X」的计时从**调用点**起算 | `automation::run` 第一段是等 pane 收到远端首字节，最长等 `ready_timeout_ms`（默认 **15 秒**），之后才轮到 `write_scheduled` 发字节。而 `on_pane_ready` 里的 "ready" 指的是**通道就绪**，不是首字节。任何在那里算好 deadline 的逻辑，在高延迟代理链路（本项目主场景）上都会在字节还没发出去时就判定失败——**测试全绿、本机全绿，只有真机慢链路才炸**。唯一可靠的起算点是 `UserEvent::AutomationDone(.., Outcome::Completed(_))`；非 `Completed`（等首字节超时／用户接管／断线）意味着压根没发出去，该撤掉判定而不是判失败 | `app::tests::the_grace_period_starts_when_the_bytes_are_out_not_when_we_decide_to_send`；`app::tests::an_attach_that_never_went_out_is_not_judged`。配套的：凡是把生命周期挂在 `AutomationDone` 上的队列，都要在驱动处按世代号兜底回收——`wind_down` 关整标签时对在途 task 直接 `abort()`，那个事件永远不会抵达（`a_check_whose_tab_is_gone_is_dropped_instead_of_piling_up`） |
| T12 | 把 `GetCurrentThread()` 的返回值存进结构体给别的线程用 | 它是**伪句柄**（常量 `-2`），含义是「调用它的那个线程」。存给看门狗线程之后，`GetThreadTimes` 量的是看门狗自己 —— 主线程 CPU% 恒等于零点几，而事件循环正忙转。**静默错值，没有任何报错**，且本机 Linux 上这段代码根本不编译 | `sysprobe::tests::this_platform_reports_cpu_time_that_actually_grows_when_we_burn_cpu`；必须在主线程上 `DuplicateHandle` 拿自有句柄 |
| T13 | 以为选区是我们自己的状态 | 选区归 alacritty 管，而它把「这行被擦过」等同于「整段选区作废」：`clear_line`/`clear_screen` 一律 `take().filter(|s| !s.intersects_range(..))` —— **沾边就整段丢**，连没被碰过的行一起。全屏 TUI 每轮重绘都擦几行，用户按住左键拖，高亮出现又被冲掉；而 `selection_update` 在 `None` 上是**静默 no-op**，拖再远也回不来。补偿只能补「被丢成 `None`」这一种：滚动走的是 `rotate`、结果仍是 `Some`，那是正确的跟随行为，盖回旧坐标就变成滚一行选中别的文本。且 hold **每条出口都要归还**，挂住之后这个 pane 的选区再也擦不掉 | `vt_fixtures::a_repaint_that_erases_one_line_must_not_take_the_whole_held_selection_with_it`；`emulator::tests::holding_the_button_does_not_freeze_the_selection_against_scrolling`；`app::tests::every_path_that_ends_the_drag_also_hands_the_selection_hold_back` |

**T1 和 T3/T7 是最容易在重构中被悄悄破坏的。** 事件循环在 `app.rs`（`main.rs` 只做接线），
动 `emulator.rs` 或 `app.rs` 事件循环时，先跑这几个测试，改完再跑一遍。

**GUI/渲染层还有一批「编译过、跑起来才崩」的坑**（glyphon `atlas.trim`、winit
`NamedKey`/`WaitUntil`、wgpu 尺寸 NaN、alacritty `colors()` 全 None / 两种宽字 spacer、
agent 平台差异、字体 DPI…）——动 `app.rs`/`text.rs`/`gpu.rs`/`input.rs`/`keymap.rs`
前读 **`docs/gui-render-gotchas.md`**（每条给「症状/规则/守护」）。

---

## 构建与测试

```bash
cargo test -p mullion-core     # 布局，不需要 GPU，最快，改布局先跑这个
cargo test -p mullion-term     # 键码 + VT 快照
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo run -p mullion-app -- user@host -p 22 -i /path/key   # 跑真实连接（GUI 需显示器）

# Linux→Windows 交叉编译出可实测的 exe（详见 docs/cross-compile-windows.md）：
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app
# 真机 SSH 验证（验加密后端/协商，本机就能跑；真机信息用 env 传，不写死在库）：
MULLION_LIVE=1 MULLION_LIVE_HOST=<真机> MULLION_LIVE_USER=<用户> MULLION_LIVE_KEY=<私钥> \
  cargo test -p mullion-ssh --test live -- --ignored
```

**「绿」的定义**：`cargo test --workspace` 全过 **且** `clippy -D warnings` 无输出。
只跑了单个 crate 的测试不叫绿，不许据此说「测试通过」。

大输出先落盘再 grep，别整片倒进上下文：
```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
```

### VT 快照测试（本项目最重要的测试手段）

终端仿真的正确性没法靠眼睛看，只能拿真实录下来的字节流喂 `Emulator` 再比对快照。
**新增 VT 相关功能时必须配一个 fixture**，否则这个功能在真实 TUI 下是什么样，没人知道。

fixture 在 `crates/mullion-term/tests/fixtures/`；录制步骤见 `docs/vt-fixtures.md`。

---

## 你无法验证的东西

这个项目有一大块是**你在无头容器里根本验证不了**的。碰到这些，
写代码 + 写测试脚手架，然后**明确标注「未验证，需人工确认」**，不要说「已完成」。

无法自动验证的清单：

- **GPU 渲染是否正确**（字形位置、颜色、光标形状、CJK 宽字符占两格）
- **是否真的不闪**（这是本项目存在的理由，但它只有人眼能判定）
- **真实 SSH 链路行为**（认证、代理、断线重连、高延迟下的吞吐）
- **输入法**（第三方中文输入法在 winit 下的候选框行为）
- **任何「感觉」类指标**（跟手、顺滑）

对这些，你能做的是：
- 把逻辑抽到能单测的纯函数里（例如「攒帧」逻辑本身可以脱离 GPU 测）
- 写一个能让我手动跑的验证清单，写进 PR 描述
- **不要**自己编一个「看起来像通过了」的结论

**现在能做到**：GUI 可在本机交叉编译成 Windows exe 交人工实测
（`docs/cross-compile-windows.md`）；SSH 认证/协商可在本机对内网真机跑 live 验证
（`MULLION_LIVE=1`）。单 pane 已端到端在 Windows 11 实机验收通过
（窗口显示 + 远端登录 + 键盘输入 + Google Sans Code 字体，2026-07-24）。
仍纯人眼的只剩「是否不闪 / 字形/CJK 对齐 / 输入法 / 手感」。

---

## API 漂移

本项目依赖的几个 crate 发版快、破坏性改动多。**写涉及它们的代码前，
先查当前版本的实际签名，不要凭记忆写。**

| crate | 风险 |
|---|---|
| `russh` | `client::Handler` 的签名历史上改过多次（async_trait → 原生 async fn） |
| `winit` | 0.30 换成了 `ApplicationHandler`，网上大量教程还是旧 API |
| `wgpu` | 每个大版本都动 API |
| `glyphon` | 跟随 wgpu 版本，容易版本打架 |
| `alacritty_terminal` | `Term::new` / `damage()` 签名相对稳，但 `vte` 的 `ansi::Handler` 在加方法 |

查法（优先本地，离线可用、跟锁定版本一致）：
```bash
cargo doc -p russh --open   # 或直接读 ~/.cargo/registry/src/**/russh-*/src/client/mod.rs
```

编译失败**先看错误提示的实际签名**，别猜着改。同一处连续改两次没过，
停下来问我。

已按锁定版本核实过一批具体签名 + 踩过的坑（winit/wgpu/glyphon/cosmic-text/alacritty），
汇总在 **`docs/gui-render-gotchas.md`**——写渲染/输入代码前先扫一眼,省得重踩。

---

## 交付约定（**不用每次再问我，默认执行**）

只要本轮改动落到了 `mullion-app`（或任何影响 Windows 端行为的地方）并且我要拿去实机验，
**一条龙做完，别停下来问「要不要 bump / 要不要发版」**：升 patch 版本号 → 跑绿 →
交叉编译 + objdump 验收 → 签名 → 发 GitHub Release → 报链接和人工验收清单。

完整步骤、命令、代理设置、notes 模板见 `.claude/skills/release-windows/SKILL.md`
（说「发版」时自动加载）。**别凭记忆做**——每一步都有漏了也不报错的坑。
**本机 DNS 解析不了 github**，所有 `gh`/curl 都要走代理。私密信息（真机 IP / 用户名 /
私钥路径 / 凭据）**永不进被跟踪文件、永不推送**，库里只留占位。

## 提交约定

- 中文，一行摘要 + 必要时正文
- 摘要带 spec 编号：`feat(core): 分屏拖拽 resize 夹紧最小尺寸 (F4)`
- 触到领域陷阱的改动，正文写明跑了哪个守护测试
- 一次提交只做一件事。「顺手改了格式」是 Scope Discipline 违规

## 目录约定

ADR 放 `docs/adr-NNN-*.md`，一个决策一个文件；brainstorm/writing-plans 的产物
（各切片的设计 spec 与实现 plan）放 `docs/superpowers/`。

`docs/` 关键非 ADR 文件：
- `cross-compile-windows.md` —— Linux 交叉编译 Windows exe 的运行手册（代理/mingw/objdump/live 验证/发布 Release）
- `gui-render-gotchas.md` —— GUI/渲染/输入层「编译过跑起来才崩」的坑（动那几个文件前必读）
- `field-capture.md` —— 实机采集与验收运行手册（性能切片留下的「待人工验」怎么一次性回收：
  静置 A/B、Debug 档目视表、N1/N5 判据、日志各段怎么读）
- `ui-form-guidelines.md` —— 表单布局规范（分节/宽度三档/间距五档/危险措辞/空态文案），
  写任何 egui 表单前先扫一眼；机械守护在 `crates/mullion-app/tests/form_guidelines.rs`
- ADR 全在 `docs/adr-NNN-*.md`，按需读原文；碰架构决策前先查有没有对应 ADR。

架构级决策（换 GUI 框架、换 SSH 库、改依赖方向）写进 `docs/adr-NNN-*.md`，
写清「当时的备选是什么、为什么否掉」。半年后回头看，理由比结论值钱。
