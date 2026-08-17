# 远端状态上报(标题条目录名 / tmux 名 / SFTP 目录继承)

> 需求编号 **F123**(`spec.md` §4.4)。它是被移出范围的 **F51**「跟随 cwd 自动切目录」
> 的**缩小版承接者**,不是复活:不依赖 tmux 转发内层 OSC 7(那条正是 F51 被否的理由
> 之一),改走 tmux 自己发的窗口标题;而且只在侧栏「关→开」跃迁那一次继承,不做持续跟随。

分屏标题条右边那一小段「tmux 名 · 目录名」,以及 `Ctrl+Shift+B` 打开文件面板时
远端栏落在哪个目录,数据**全部来自远端自己发过来的 OSC 转义序列**。
远端不发,这两个功能就静默降级(标题条不显示那一段、文件面板落回登录目录)——
不是 bug,是拿不到。

## 为什么不能旁路问一句

adr-009:一条 SSH 连接承载所有分屏。旁路开一条 exec channel 跑
`tmux display-message -p '#{pane_current_path}'` 拿回来的是**某个** pane 的路径,
而 `$SSH_CONNECTION` 四元组在所有分屏之间完全相同,没有任何办法把它对上是哪一块。
`channel.set_env` 注入自己的标识也不行:sshd 的 `AcceptEnv` 默认只放 `LANG`/`LC_*`。

所以只剩「远端主动报、按 channel 收」这一条路。

## 远端要怎么配

### tmux(推荐,一行,**tmux 场景下唯一可靠的一条**)

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

### shell 报 cwd(OSC 7)—— **只在不经过 tmux 时有用**

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

| 远端配置 | 标题条右区 | 文件面板起始目录 |
|---|---|---|
| 都没配 | 不显示 | F120 配置的默认远端目录 → 登录目录(`.`) |
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

- 解析:`crates/mullion-term/src/remote_state.rs`(纯函数,14 条测试)
- 采集:`Emulator::feed` 里跑 `Osc7Sniffer`,`Emulator::take_remote_state` 取走
- 落地:`Workspace::pump`(`crates/mullion-app/src/shell/workspace/mod.rs`)→
  `PaneState.cwd` / `.tmux`
- 显示:`crates/mullion-app/src/ui/pane_title.rs` 的 `dir_leaf` / `side_text`
- 继承:`crates/mullion-app/src/app.rs` 的 `files_start_dir`(首次打开走
  `trigger_sftp_open`)/ `sync_files_to_focused_pane`(已开着时的关→开跃迁同步)/
  `files_hotkey_event`(`Ctrl+Shift+B`)
