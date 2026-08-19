# Mullion — 项目上下文

> 全局偏好（语言、精简度、Scope Discipline、防注入、写文件行数上限）见 `~/.claude/CLAUDE.md`
> 与 `~/.claude/rules/`。**此处不重复，只写这个项目特有的东西。**


## 背景：有多台ubuntu 服务器，已安装了claude code，进行开发，原先在Windows11中，用powershell+ssh，连接到ubuntu。tmux只用来保持claude code连接（ssh离线后保活）

## 项目是什么
原生 GPU 加速的 SSH 客户端。核心场景：**在 Windows 上，通过高延迟代理链路，
操作跑在远端 tmux 里的 Claude Code TUI**。

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
| T3 | 喂数据和重绘没解耦 | 每秒几千次重绘，GPU 空转、风扇起飞 | `app::tests::redraw_is_frame_capped` |
| T4 | 分屏后没发 `window_change` | tmux 里的 TUI 按旧列数排版，全屏直接错行 | `app::tests::reflow_emits_resize` |
| T5 | 鼠标上报没有 Shift 逃生门 | `/tui fullscreen` 下用户永远无法划选复制 | `keymap::tests::shift_blocks_mouse_report_so_user_can_copy` |
| T6 | Shift+Enter 编码错 | Claude Code 里无法插入换行，一按就提交 | `keymap::tests::shift_enter_without_kitty_is_esc_cr` |
| T7 | 帧率节流后 `ControlFlow::WaitUntil` 不复位 | 首次节流后永久 100% CPU 忙转（T3/N3 红线） | `frame::tests`（`plan` 决策 4 条）；事件循环三分支须显式复位 control_flow |
| T8 | 判给终端的键盘事件仍先喂 `egui_state.on_window_event` | egui 焦点系统吞掉 Tab → 焦点跳到菜单栏 → `wants_keyboard_input()` 恒 true → 终端**永久**收不到任何键（Tab 补全后回车/退格全废，鼠标仍灵） | `input_route::tests::terminal_keyboard_is_never_fed_to_egui_so_tab_cannot_steal_focus`；键盘先判后喂，指针先喂后判 |

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

终端仿真的正确性没法靠眼睛看。做法是：

1. 在真实环境里录一段字节流：`ssh host 'tmux new -d …'` 后用 `script -q` 抓原始输出，
   存到 `crates/mullion-term/tests/fixtures/*.bin`
2. 测试里把字节喂进 `Emulator`，把 grid 渲染成纯文本 + 属性摘要
3. 跟 `*.snap` 比对

已有 fixture 见目录。**新增 VT 相关功能时必须配一个 fixture**，
否则这个功能在真实 TUI 下是什么样，没人知道。

录制脚本：`scripts/record-fixture.sh`（还没写，需要时告诉我）。

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
**一条龙做完，别停下来问「要不要 bump / 要不要发版」**：

1. **升 patch 版本号** —— `workspace.package.version` 第三位 +1，单独一个 `chore:` 提交。
2. **跑绿** —— `cargo test --workspace` + `clippy -D warnings` + `fmt --check`。不绿不发。
3. **交叉编译** Windows exe，并做 objdump 依赖验收 —— 出现 `libgcc_s_seh-1.dll` /
   `libwinpthread-1.dll` **即为不合格，必须修**。
4. **签名** —— `scripts/sign-windows.sh`（自签名证书在 `~/.mullion-signing/`，私钥不进仓库）。
   **必须在算 sha256 之前**，签名会改文件内容。**这是唯一漏了也不报错的一步**：不签照样
   算得出 hash、发得掉 Release，只有我在 Windows 上看到「发布者：未知」才发现。
5. **发 GitHub Release** —— **标题只能是纯版本号 `v0.1.N`**，不带破折号、不带摘要、不带 emoji。
   先 push 再发版（`gh release create` 会把 tag 建在远端当前 HEAD 上）。
6. **报给我** —— Release 链接 + sha256 + 人工验收清单。

具体命令、代理设置、notes 模板见 `.claude/skills/release-windows/SKILL.md`（说「发版」时自动加载）。
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
- `ui-form-guidelines.md` —— 表单布局规范（分节/宽度三档/间距五档/危险措辞/空态文案），
  写任何 egui 表单前先扫一眼；机械守护在 `crates/mullion-app/tests/form_guidelines.rs`
- 最新 ADR：`adr-010`（隧道是与会话平级的一等对象，**独占**自己的 SSH 连接——不复用会话那条；
  刻意偏离 PuTTY 的「隧道属于会话」与 adr-009 的「一条连接承载多单元」，两处理由都在里面；
  硬约束是 `russh` 的 `tcpip_forward` 要 `&mut self`，`Arc<Handle>` 给不出，`-R` 复用连接编译不过）；
  `adr-009`（一条 SSH 连接开多 channel 承载多分屏；含它引入的四条新失效模式：
  channel 泄漏、T1 升级为 per-pane、迟到的 `PaneOpened` 要查树成员 + Workspace 世代）；
  `adr-008`（自诊断日志：接 `log` facade 白拿 wgpu/winit/russh 内部诊断 + 阶段打点 +
  看门狗；级别用 `MULLION_LOG` / `MULLION_LOG_DEPS`，默认 info/warn）；
  `adr-007`（用 egui 做外壳：菜单/状态栏/会话弹窗；含与 wgpu23/winit0.30 同帧集成的坑）

架构级决策（换 GUI 框架、换 SSH 库、改依赖方向）写进 `docs/adr-NNN-*.md`，
写清「当时的备选是什么、为什么否掉」。半年后回头看，理由比结论值钱。
