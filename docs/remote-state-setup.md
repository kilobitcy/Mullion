# 远端状态上报(标题条目录名 / tmux 名 / SFTP 目录继承)

> 需求编号 **F123**(`spec.md` §4.4)。它是被移出范围的 **F51**「跟随 cwd 自动切目录」
> 的**缩小版承接者**,不是复活:不依赖 tmux 转发内层 OSC 7(那条正是 F51 被否的理由
> 之一),改走 tmux 自己发的窗口标题;而且只在侧栏「关→开」跃迁那一次继承,不做持续跟随。

分屏标题条上「目录名 · tmux 名」那两段(自 2026-08-18 起并进左边那一整串
`序号 · 节点名 · 目录名 · tmux 名`,不再是右侧角标),以及 `Ctrl+Shift+B` 打开文件面板时
远端栏落在哪个目录,数据**全部来自远端自己发过来的 OSC 转义序列**。**自 2026-08-18
起,这些转义序列默认由 F124 自动配置好**(连上就在一条旁路 channel 里把远端 tmux 的
`set-titles`/`set-titles-string` 开好),正常情况下你不用做任何事。只有**远端没有
tmux**、或者你在设置弹窗里**关掉了这个自举开关**,这两个功能才会静默降级(标题条
不显示那一段、文件面板落回登录目录)——不是 bug,是拿不到。

## 为什么不能旁路问一句

adr-009:一条 SSH 连接承载所有分屏。旁路开一条 exec channel 跑
`tmux display-message -p '#{pane_current_path}'` 拿回来的是**某个** pane 的路径,
而 `$SSH_CONNECTION` 四元组在所有分屏之间完全相同,没有任何办法把它对上是哪一块。
`channel.set_env` 注入自己的标识也不行:sshd 的 `AcceptEnv` 默认只放 `LANG`/`LC_*`。

所以只剩「远端主动报、按 channel 收」这一条路。

**F124 的旁路 channel 没有绕开这条限制,恰恰是这条限制的一个例外证明**:F124 不问
「当前 pane 路径是什么」,只改 tmux 服务器的**全局**选项(`set-titles`/
`set-titles-string`)——`set-titles` 本来就与 pane 无关,不存在「问的是哪一块」这个
问题,所以旁路 exec 在这里可行。之后每个 pane 各自的路径,靠的还是上面说的
「远端主动报、按 channel 收」:tmux 用 `#{pane_current_path}` 把每个 pane 自己的
路径填进它自己发出的窗口标题里,mullion 照旧从各 pane 的字节流里嗅标题。

## 远端要怎么配

### 默认:F124 自动配置好了,通常不用做任何事

连上 SSH 之后,mullion 立刻在一条旁路 exec channel 里跑:

```
tmux set -g set-titles on && tmux set -g set-titles-string '#S:#I:#W #{pane_current_path}'
```

- **什么时候跑**:连上立刻跑一次;没成功(比如 tmux 服务器还没起)就每 **30 秒**
  重试一次,直到成功为止——成功之后这条连接就**永不再试**。
  重试**由帧驱动**(判据挂在事件循环的空闲回调上),不是独立的后台定时器:
  窗口完全没有任何输入/输出、一帧都不画的时候不会凭空触发。这不影响实际使用——
  用户真要去开 tmux 就必然在敲键盘,敲键盘就有帧。
- **成功判据是 exec 的退出码**:两条 `tmux set` 用 `&&` 串联,退出码 0 当且仅当
  两条都成功。用 `;` 串会把「没配上」误记成「已配上」然后不再重试,所以刻意不用。
- **影响面**:改的是 tmux **服务器全局**选项,不是某个 pane 私有的。同一台机器上
  如果还有别的终端 `attach` 了同一个 tmux server,它们的窗口标题也会被一起改成
  `会话名:窗口号:窗口名 目录` 这个格式。这个选项活在 tmux 服务器**内存**里,不写
  `~/.tmux.conf`,server 退出(`tmux kill-server`)即失效——下次连接 F124 会重新
  配一遍。
- **怎么关**:设置弹窗 →「远端」分节 →「自动配置远端 tmux 的状态上报」勾选框,
  关掉之后这条连接不会再跑这条命令(已经配上的不会被撤销)。
- **已知时延**:tmux 只在**下一次客户端重绘**时才会重新计算并发送标题,不是
  `cd` 一敲就立刻更新——实测是等到下一次提示符刷新(敲下一条命令、按回车)才看到
  标题条跟上。

### 关掉自举之后,怎么自己配

关掉「自动配置远端 tmux 的状态上报」开关,或者远端根本没有 tmux(比如裸 shell、
或用的是 screen),F124 的自举就不会跑,想要 F123 的效果得自己配。

#### tmux(推荐,一行,**tmux 场景下唯一可靠的一条**)

`~/.tmux.conf`:

```
set -g set-titles on
```

tmux 会按默认的 `set-titles-string`(`#S:#I:#W`,如 `main:0:bash`)发窗口标题
(OSC 0/2)。我们从标题里认会话名的判据是:第一个冒号前的一段作为候选名,
**且第二段必须是纯数字**(对应 tmux 的 `#I` 窗口序号)——否则 Ubuntu 默认 bash
的 `user@host: ~/dir` 会被当成会话名 `user@host`,用户没开 tmux 却看到一个假会话名。

想连目录一起报:

```
set -g set-titles-string '#S:#I:#W #{pane_current_path}'
```

cwd 的判据是标题里第一个以 `/`、`~/` 开头或恰好是 `~` 的空白分隔 token。
改完 `tmux kill-server` 或 `tmux source-file ~/.tmux.conf`。

#### shell 报 cwd(OSC 7)—— **只在不经过 tmux 时有用**

Ubuntu 的 bash 默认**不发** OSC 7。可以加到 `~/.bashrc`:

```bash
osc7_cwd() { printf '\033]7;file://%s%s\033\\' "$HOSTNAME" "$PWD"; }
PROMPT_COMMAND="osc7_cwd${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
```

（已核实这段代码发出的字节确实是 `ESC ] 7 ; file://host/path ESC \`,能被
`Osc7Sniffer` + `parse_osc7` 正确认出、也能正确取到 `/path` 那一段。）

**但本项目的主场景是「远端 tmux 里的 Claude Code」,而这个场景下这条腿基本没用**:
实测(`tmux 3.7b`)tmux 会自己吃掉内层 shell 发出的 OSC 7 去更新它自己的
`pane_current_path`,**默认不会把原始序列转发给外层终端**——挂了这段 `.bashrc`
之后在 tmux 里 `cd`,mullion 这边完全收不到 `ESC ] 7`。这与 `spec.md` §4.4
里记录的、F51(自动跟随 cwd)被移出范围的理由是同一个事实。

所以:
- **在 tmux 里**(即本项目的主场景),cwd 只能靠上面「tmux」小节的
  `set-titles-string` 带路径这条路,OSC 7 这段 `.bashrc` 配置**发挥不了作用**。
- **不经过 tmux 时**(比如直接 SSH 到一个裸 shell,或者中间的多路复用器确实会
  转发 OSC 7),这段 `.bashrc` 配置有用,且优先级更高、更准(见下)。

zsh 用户装了 `oh-my-zsh` 的话通常已经在发了;fish 从 3.x 起默认发。同样受
「tmux 默认不转发」这条限制。

**OSC 7 收到时压过标题里的路径**:它是路径本身,标题里那个是给人看的(带 `~` 缩写、
可能被 shell 截断到第一个空格)。这个优先级发生在拿到 OSC 7 的那一批里(`take_remote_state`);
拿不到 OSC 7 的批次(tmux 场景的常态)自然回落到标题里解析出的路径。

## 降级行为

| 远端配置 | 标题条里的目录/tmux 两段 | 文件面板起始目录 |
|---|---|---|
| 开着 F124(默认) | `会话名 · 目录名` | 该目录 |
| 没配(关了 F124 或远端没 tmux) | 不显示 | F120 配置的默认远端目录 → 登录目录(`.`) |
| 只开 `set-titles on` | `会话名` | 同上(标题里没有路径) |
| `set-titles-string` 带路径 | `会话名 · 目录名` | 该目录(若是绝对路径) |
| tmux 之外还发了 OSC 7 | `会话名 · 目录名`(以 OSC 7 为准) | 该目录 |

标题里的路径常常是 `~/Mullion` 这种缩写形式。**`~` 只用来在标题条上显示目录名**,
不拿去当 SFTP 起始目录 —— openssh 的 `sftp-server` 不展开 `~`,直接拿 `~/Mullion`
去 `canonicalize` 会失败,面板会停在「取不到登录目录」,比不继承更糟
(`files_start_dir` 只接受以 `/` 开头的绝对路径,`~/...` 一律落回配置的默认远端目录)。

## 文件面板已经开着时,不会跟着终端 cd 跑

目录继承(②)只发生在文件侧栏**关→开**的那一次跃迁:`Ctrl+Shift+B` 把侧栏从关
打开时,才会去读焦点 pane 当前报出的目录当起点。侧栏已经开着的情况下,用户在终端里
`cd` 到别处,面板**不会**跟着跳走;之后用户在面板里点开别的目录,也不会被终端的
`cd` 拽回去。

这是故意的:侧栏已开着时如果每次终端 cd 都同步一次,用户在文件面板里刚点开一个
目录浏览,终端那边一个 `cd` 就把面板拽走,浏览体验会被反复打断,而且拽走的时机
完全不可控(远端何时发 OSC 7/标题是异步的)。想让面板重新跟上终端当前目录,
关掉侧栏再用 `Ctrl+Shift+B` 开一次即可。

## 相关代码

- 自举:`crates/mullion-app/src/remote_bootstrap.rs`(命令串 + 重试判据)+
  `App::tick_tmux_bootstrap`(`about_to_wait` 里跑)
- 解析:`crates/mullion-term/src/remote_state.rs`(纯函数,14 条测试)
- 采集:`Emulator::feed` 里跑 `Osc7Sniffer`,`Emulator::take_remote_state` 取走
- 落地:`Workspace::pump`(`crates/mullion-app/src/shell/workspace/mod.rs`)→
  `PaneState.cwd` / `.tmux`
- 显示:`crates/mullion-app/src/ui/pane_title.rs` 的 `dir_leaf` / `title_text`
  (`title_text` 把「序号 · 节点 · 目录 · tmux」拼成左边一整串;右侧角标那套
  `side_text` 已随该改动删除)
- 继承:`crates/mullion-app/src/app.rs` 的 `files_start_dir`(首次打开走
  `trigger_sftp_open`)/ `sync_files_to_focused_pane`(已开着时的关→开跃迁同步)/
  `files_hotkey_event`(`Ctrl+Shift+B`)
- `~` 展开:`crates/mullion-app/src/app.rs` 的 `expand_tilde` / `files_start_dir`
