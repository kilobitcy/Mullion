# F156:恢复弹窗关闭入口 · 换节点后焦点跟随 · 非 tmux 的 shell OSC 7 自举

> 2026-08-24。用户实机（Windows 11 + v0.1.64）报的三条。
> 三条互相独立，共用一个切片只是因为都落在 `mullion-app`，一次发版一起验。
> 编号从 F156 起：`spec.md` 里最大是 F152（F153~F155 当时没回填，本片也不补，
> 那超出 scope）。

---

## a. 「恢复上次的现场」弹窗加关闭入口(F156-a)

### 现状与问题

`ui/history.rs` 的弹窗底部已经有「不恢复」按钮，但窗口标题栏右上角没有 ×。
用户的直觉是「弹窗右上角就该有个叉」，找不到时会以为这个弹窗关不掉。

### 设计

`egui::Window` 挂 `.open(&mut open)`：

```rust
let mut open = true;
egui::Window::new("恢复上次的现场")
    .open(&mut open)
    ...
```

画完之后 `open == false` 就回报**既有的** `HistoryOut::Dismiss`——不新增出口变体，
`app.rs` 那侧的处置一行不动。

同时接 Esc，走 `session_manager/keys.rs` 已有的惯例（`i.key_pressed(egui::Key::Escape)`）。
这个弹窗里没有文本框，不需要那边的 `typing` 让位逻辑。

### 关键取舍

- **用 egui 自带的 close button，不自绘 `×`**：egui 0.30 的 close button 是
  `line_segment` 画的，不是文字，不碰 T9 的字形白名单（`tests/glyph_whitelist.rs`）。
  自绘的话得往 `ui::glyphs::VERIFIED` 里登记，为一个系统本来就提供的控件不值得。
- **× 与「不恢复」并存**：底部按钮是键盘路径的出口（Tab 能够到），× 是鼠标路径的
  直觉位置。删掉任一个都会让某一类用户找不到出口。
- **`out` 用 `get_or_insert` 而不是直接赋值**：同一帧里既点了行又关了窗在物理上
  不可能，但让「先发生的结论优先」是显式的，比依赖不可能性更稳。

### 守护测试

| 测试 | 钉住什么 | 自证会变红 |
|---|---|---|
| `closing_the_window_with_the_title_bar_x_reports_dismiss` | × 回报 `Dismiss` | 去掉 `.open(&mut open)` |
| `pressing_escape_closes_the_dialog` | Esc 回报 `Dismiss` | 去掉 Esc 分支 |
| 既有的 `dismissing_reports_dismiss` / `clicking_a_row_restores_that_record_right_away` | 原路径没被 × 挤坏 | —— |

× 是线段画的，现有的 `click(label)` 辅助（靠找 `Shape::Text`）找不到它。
点击测试改用「按 title bar 右上角的坐标点」：从 `Window` 的 response rect 推出
右上角，往左内缩一个 close button 的宽度。**这个坐标推法本身是脆的**（egui 换版本
可能挪位置），所以测试断言失败时的信息里要带上实际点到的坐标，别只给一句
`assert_eq` 失败。

---

## b. 换节点成功后焦点跟到那块 pane(F156-b)

### 现状与问题

分屏标题条上点「⇆ 换节点」→ 选一台 → 连上之后，键盘焦点仍在原来那块 pane。
用户刚指定了新节点，下一步必然是往它里面敲东西。

### 设计

改在**自由函数 `rehost_pane`** 里，成功挂上之后：

```rust
ws.set_focus(pane);
```

**不是**改在 `UserEvent::PaneRehosted` 的事件分支里。理由：`rehost_pane` 已经是
能脱离事件循环单测的自由函数（现有测试
`rehosting_a_pane_repoints_it_and_wipes_the_old_hosts_screen` 就是直接调它），
放这里测试能直接断言 `ws.focus()`；放事件分支只能写「读 `app.rs` 源码找字符串」
式的断言，那是本项目反复踩到的恒绿模式。

`rehost_pane` 开头那个 `pane_still_wanted` 早退不动：pane 在拨号途中没了就不设焦点
（`set_focus` 本身也有成员校验，但让早退挡在前面语义更清楚）。

### 关键取舍

- **`reattach_pane`（F128 断线自动重连）刻意不跟着改**。两个函数长得很像，但语义
  相反：换节点是用户**刚刚**主动发起的，焦点跟过去是他的预期；断线重连是后台自愈，
  可能发生在用户正在另一块 pane 里打字的任意时刻，抢焦点等于把用户的按键打到别处去。
  这条差异必须由一个**对照测试**钉住，而不是只写注释——注释拦不住下一次「顺手统一
  一下这两个函数」的重构。
- **只动分屏焦点，不动 egui 输入焦点**（用户在选项里选的就是这一档）。若当时输入
  焦点在文件侧栏，本片不把它抢回终端。

### 守护测试

| 测试 | 钉住什么 | 自证会变红 |
|---|---|---|
| `rehosting_a_pane_moves_the_focus_to_it` | 换节点成功后 `ws.focus() == pane` | 删掉 `set_focus` 那行 |
| `reattach_does_not_steal_the_focus_but_rehost_does` | 两个函数的相反语义 | 往 `reattach_pane` 里也加 `set_focus` |
| 既有的 `rehosting_a_pane_that_is_gone_is_refused` | 失败路径不设焦点 | 去掉 `pane_still_wanted` 早退 |

---

## c. 非 tmux 场景的 shell OSC 7 自举(F156-c)

### 现状与问题

用户报：`Ctrl+Shift+B` 打开 SFTP 侧栏时远端目录跟随「经常留在 `~`」，
且明确是**非 tmux 的情况下**没跟住。

根因不是接线 bug，是**这个场景根本没有数据源**。`PaneState.cwd` 只有两条腿
（`docs/remote-state-setup.md`）：

| 腿 | 非 tmux 下的实际情况 |
|---|---|
| OSC 7 | Ubuntu 的 bash **默认不发**，要手工往 `.bashrc` 加 `PROMPT_COMMAND` |
| OSC 0/2 窗口标题 | Ubuntu 默认 `.bashrc` 在 `TERM=xterm*` 时会发 `user@host: ~/dir`，但只要 PS1 被 starship / oh-my-bash / 自定义 rc 接管，那段 `case` 分支就不生效 |

tmux 场景能跟住，是因为 F124 把 `#{pane_current_path}` 塞进了 **tmux 自己**发的
标题，绕开了 shell。非 tmux 没有等价机制。

### 设计

F124 的 shell 版：pane 的 shell channel 一建立就往 PTY 写一次注入串，让远端 shell
从此每个提示符都发一次 OSC 7。

**注入串**（前导一个空格是设计的一部分）：

```sh
 __mullion_osc7() { printf '\033]7;file://%s\033\\' "$PWD"; }; if [ -n "$BASH_VERSION" ]; then PROMPT_COMMAND="__mullion_osc7${PROMPT_COMMAND:+;$PROMPT_COMMAND}"; elif [ -n "$ZSH_VERSION" ]; then precmd_functions+=(__mullion_osc7); fi; clear
```

放在新模块 `crates/mullion-app/src/shell_bootstrap.rs`，与 `remote_bootstrap.rs`
同构：纯逻辑、零 IO、零 async，命令串是常量 + 一个生成函数，真正的写在 `app.rs`。

**注入时机**：`ConnectOk` / `PaneOpened` / `PaneRehosted` 三处「pane 挂上了、拿到
写口」的地方，**在 `start_automation` 之前**。

三处各写一遍正是「列举式门控在加档时必然漏」的陷阱（本项目已踩中三次），所以收成
一个方法：

```rust
fn on_pane_ready(
    &mut self,
    generation: u64,
    pane: PaneId,
    sink: Arc<SshSession>,
    plan: Option<PendingAutomation>,
)
```

内部先注入、再 `if let Some(plan) = plan { self.start_automation(..) }`。
三个调用点全部改成调它。加第四种 pane 建立方式时不会再漏。

`ByteSink::write` 是**同步**的（`try_send` 语义），注入不需要起 task。

**开关**：`mullion_store::Settings` 新增 `shell_osc7_bootstrap: bool`，
`#[serde(default = ...)]` 默认 `true`（老的 `settings.toml` 里没有这个字段）。
设置弹窗「远端」分节第二个勾选框，紧挨 F124 那个。

**独立开关而不是复用 F124 的**：两者副作用完全不同——F124 改的是远端 tmux server
的内存态选项，本片往用户**当前这条 shell** 里写命令并清屏。想只关掉其中一件事是
合理诉求，一个开关做不到。

### 关键取舍

- **主机名段留空**（`file:///path`）：`parse_osc7` 本来就忽略主机名段（它在
  tmux/容器里经常是错的）。留空省掉 `$HOSTNAME`(bash) / `$HOST`(zsh) 的差异，
  注入串短一截、少一处能出错的地方。
- **`printf '...%s' "$PWD"` 而不是把 `$PWD` 拼进格式串**：目录名含 `%` 会被
  printf 当格式符吃掉，拼出来的路径是错的——而错的绝对路径**骗得过**下游
  「是不是绝对路径」的校验，会把 SFTP 面板带到一个不存在的目录去。
- **前导一个空格**：Ubuntu 默认 `HISTCONTROL=ignoreboth`（含 `ignorespace`），
  这条不进 shell history。不是所有发行版都这样，所以它是**尽力而为**，不是保证。
- **末尾 `clear`**：用户拍板要清屏。代价是 motd / 登录横幅一起被清掉。
- **函数名带 `__mullion_` 前缀**：双下划线开头 + 项目名，撞用户已有函数的概率
  可以忽略；真撞了也是覆盖我们自己的，不会破坏用户的 shell。
- **不写远端任何文件**。这条命令只活在这条 shell 的内存里，断开即消失。这是它与
  「往 `~/.bashrc` 追加」方案的关键区别，也是能默认开启的前提。
- **注入必须在 `start_automation` 之前**：注入串自带 `clear`，跑在自动化之后会把
  用户登录后命令的输出清掉一半。

### 已知限制（写进文档与人工验收清单）

- **fish / csh 下这一行是语法错误**，屏幕上会打一行报错。fish 3.x 起本来就默认发
  OSC 7，不做兼容。用户可以关掉开关。
- **tmux 场景无效但无害**：tmux 会吃掉内层 OSC 7 不转发（F51 被否的理由之一），
  注入进去也传不出来；那个场景走 F124 那条腿，两者不冲突。
- **注入发生在 pane 建立那一刻**。用户之后在 pane 里 `ssh` 到第三台机器，
  PROMPT_COMMAND 不会跟过去。
- **远端 sshd 配了 `ForceCommand`、或用户 shell 直接是 tmux** 时，写进去的字节会
  变成那个程序的输入。这是注入方案的固有代价。

### 为什么不在 `Ctrl+Shift+B` 那一刻现写

那时 pane 里可能正跑着 Claude Code 之类的全屏 TUI，写进去的字节会变成 TUI 的按键
输入——轻则乱敲，重则触发一个不该触发的操作。**pane 刚建立、shell 还没跑任何程序**
是唯一安全的注入窗口。

### 能验证 / 不能验证

**能真跑**（无头 Linux 开发机上）：新增 live 测试，拿真 bash `eval` 这条注入串、
再调一次 `__mullion_osc7`，把它吐出的字节喂给 `mullion_term::remote_state::Osc7Sniffer`
+ `parse_osc7`，断言解出来的路径等于那个 `$PWD`。**这条测试验的是整条链路最容易
错的一环**（转义在 shell 和 Rust 之间来回穿三层引号）。参照 `tests/tmux_bootstrap_live.rs`
的写法，同样带 `#[ignore]` + 环境变量门控还是直接跑，取决于开发机上有没有 bash
（有，所以直接跑）。

**验不了，进人工验收清单**：
- 注入时机是否真的落在 pty 缓冲窗口里（会不会被 shell 漏读）
- `clear` 之后的观感（motd 被清掉是否可接受）
- 真实高延迟代理链路下注入与登录后自动化的先后顺序
- 非 bash/zsh 远端上那一行报错的实际样子

---

## 不做的事

- 不回填 F153~F155 进 `spec.md`（超出 scope，本片只加 F156 三条）。
- 不做「侧栏已开着时持续跟随终端 cd」——那是 `docs/remote-state-setup.md` 里
  明确记录过的、故意不做的行为，理由（浏览被反复打断）没有变化。
- 不动 egui 输入焦点在终端与文件侧栏之间的归属（F156-b 只动分屏焦点）。
- 不提供「往远端 `~/.bashrc` 写入」的选项。

## 顺带修正

`docs/remote-state-setup.md` 末尾那句「`files_start_dir` 只接受以 `/` 开头的绝对
路径，`~/...` 一律落回配置的默认远端目录」**已经过时**——F123 后来补了
`expand_tilde` 那条腿，`home` 已知时 `~/x` 会被展开成绝对路径。这句话会把排查方向
带偏（本次定位时就差点被它误导），随本片一起改掉，并补上非 tmux 场景现在由
F156-c 覆盖这件事。
