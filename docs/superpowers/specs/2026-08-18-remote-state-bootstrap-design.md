# 远端状态自举 + 标题条重排 + 终端区缩进 —— 设计

> 需求编号 **F124**(新增,`spec.md` §4.4)、**F123**(重排显示格式)、**F83**(标题条)、
> **F80**(终端区视觉)。日期 2026-08-18。

## 为什么要做这一片

F123(v0.1.49)把「远端状态 → 标题条 / SFTP 目录继承」整条链接通了,但**实机上一个
字节都收不到**。原因不在接线,在数据源:

- 本机(即用户的目标机型)实测 `tmux show -g set-titles` → **off**,`~/.tmux.conf`
  里也没开。tmux 不发窗口标题,`Emulator` 那条标题腿永远空转。
- tmux 会吃掉内层 shell 的 OSC 7 去更新自己的 `pane_current_path`,**默认不转发**
  给外层终端(这正是 F51 被移出范围的第③条理由)。OSC 7 那条腿在 tmux 场景同样空转。
- 非 tmux 场景(裸 shell)能拿到 Ubuntu 默认 bash 发的 `user@host: ~/dir`,但目录是
  `~/Mullion` 这种缩写,`files_start_dir` 只接受绝对路径 → SFTP 继承照样落空。

结论:F123 的**降级路径是常态,正常路径是例外**。要让它真的有用,必须由客户端主动
把远端配起来。

## 三条实测事实(本机 tmux 3.7b,已跑过)

1. **旁路开 `set-titles` 对已 attach 的客户端立刻生效。**
   在一个 pty 里跑 `tmux new`,再从**另一个进程**跑
   `tmux set -g set-titles on` + `set -g set-titles-string '#S:#I:#W #{pane_current_path}'`,
   那个 pty 当场收到 `ESC ] 0 ; demo:0:bash /tmp BEL`。
2. **`set-titles` 是 tmux 服务器的全局选项,与 pane 无关。**
   所以旁路 exec 不需要区分「是哪块分屏」——adr-009 判死 F51 的那条(`$SSH_CONNECTION`
   四元组在所有分屏间相同)在这里**不构成障碍**,因为我们要写的不是 per-pane 的东西。
3. **tmux 服务器不在时 `tmux set -g` 退出码 1,且不会顺手拉起一个空 server。**
   ```
   $ tmux -L nosrv kill-server; sh -c "tmux -L nosrv set -g set-titles on && ..."
   error connecting to /tmp/tmux-1000/nosrv (No such file or directory)
   退出码=1
   ```
   退出码可以直接当成功判据,不需要解析 stderr。

已知时延:`cd` 之后 tmux 不会立刻重发标题,要等下一次客户端重绘(实测是下一次提示符
刷新)。这是 tmux 自己的采样时机,不打算绕。

## 范围

### ① F124 远端状态自举(旁路 exec 自动开 tmux 上报)

连上之后开一条 exec channel 跑:

```sh
tmux set -g set-titles on && tmux set -g set-titles-string '#S:#I:#W #{pane_current_path}'
```

- 用 `&&` 串联而不是 `\;`:退出码 0 当且仅当两条都成功,不需要解析输出。
- **覆写全局 `set-titles-string`**(现值是 tmux 默认的 `#S:#I:#W - "#T" #{session_alerts}`,
  里面没有路径)。刻意不选「追加 `#{pane_current_path}` 到现值末尾」:默认值里的
  `#T`(pane 标题)可能先吐出一个像路径的 token,`parse_title`「取第一个 `/` 开头的
  空白分隔 token」会拿错。也刻意不选「只在现值等于 tmux 默认时才覆写」:那样在
  「用户改过 set-titles-string」的机器上目录继承会**静默**失效,而用户看不出为什么。
  代价明确写进文档:同一台机器上其它终端 attach 同一个 tmux server 时,窗口标题
  也会跟着变成这个格式。
- **调度**:连上立刻试一次;未成功则每 **30 s** 重试;**成功后永不再试**(tmux 全局
  选项在 server 生命期内一直有效)。断线重连会造一个全新的 `Workspace` 世代,状态
  随之重置,自动从头来。
- **开关**:`settings.toml` 新增布尔项,默认 **开**。关掉即整条不跑(连一次 exec 都不发)。
  这是往用户机器上写东西,必须给得出「不要」。

**为什么不写 `~/.tmux.conf`**:那是不可逆地动用户 dotfiles。内存里的全局选项随
tmux server 退出即失效,是可撤销的;真想持久化,文档里给一行让用户自己贴。

**为什么不做「检测不到就提示用户手配」**:用户有多台机器,每台手配一次是这个功能
迄今没生效的直接原因。

### ② `~` 展开(补 F123 在裸 shell 场景的缺口)

`spawn_sftp_open` 本来就会 `canonicalize(".")` 拿登录目录。把焦点 pane 的 cwd 一起
传进去,**在已知 home 的那一侧**展开:

```
expand_tilde(b"~",          b"/home/dev") -> b"/home/dev"
expand_tilde(b"~/Mullion",  b"/home/dev") -> b"/home/dev/Mullion"
expand_tilde(b"/srv/app",   b"/home/dev") -> None   // 已是绝对路径,不归它管
expand_tilde(b"~foo/x",     b"/home/dev") -> None   // `~user` 语义我们不知道,不猜
```

`files_start_dir` 多收一个 `home: Option<&[u8]>`。优先级不变、只是多了一档:

```
pane 报的绝对路径  >  展开后的 `~` 路径  >  F120 配置的默认远端目录  >  登录目录(".")
```

`sync_files_to_focused_pane`(侧栏「关→开」跃迁那条路)吃同一份 home——它从标签里
已经存着的登录目录取,不再发一次 `canonicalize`。

**边界**:`home` 拿不到(sftp 还没开)时 `~` 路径不展开,按原样被「只接受绝对路径」
挡掉,退回配置值。不猜 `/home/<user>`。

### ③ 标题条重排(F123 显示格式)

```
2 · build-01 · Mullion · main            ⇆ ✕     (tmux 里)
3 · build-01 · Mullion                   ⇆ ✕     (裸 shell)
4 · build-01                             ⇆ ✕     (什么都没报)
5 · build-01 (已断开)                    ⇆ ✕
```

- `title_text(index, host, dir_leaf, tmux, status)` 拼一整串放左区,缺的段**连分隔符
  一起消失**。顺序 = 序号 · 节点名 · 最后一级目录名 · tmux 名。
- 序号保留:分屏多了要靠它认哪块。
- **断开时只显示 `N · host (已断开)`**,不带目录和 tmux 名——那两个此刻是陈旧值,
  摆着是误导。
- 删掉 `side_text` 与 `SIDE_MAX_FRAC`:右区只剩 `⇆ ✕` 两个按钮,左区文字占满余下宽度
  并在按钮前截断。

### ④ 终端区左右缩进(F80)

`geom.rs` 新增 `TERM_PAD_PT: f32 = 8.0`(逻辑点,与标题条内边距 `shrink2(8, 4)` 同值,
标题文字与终端首列落在同一条竖线上)。`layout_geometry` 里:

```
term_px.x += pad
term_px.w  = term_px.w.saturating_sub(2 * pad)
```

极窄 pane 下 `w` 退化到 0,`grid_size_for` 已经保证夹到至少 `(1, 1)`,PTY 侧不会收到
0 列。上下不动——再吃半行高度不值。

分隔线仍画在 `px` 之间让出的那 1 px 缝里(`GAP_PX`),不受缩进影响。

## 架构落点

| 层 | 改什么 | 为什么在这层 |
|---|---|---|
| `mullion-app/src/remote_bootstrap.rs`(新) | 命令字节串常量 + `should_attempt` 调度判据 | 纯逻辑、零 IO,可单测 |
| `mullion-app/src/app.rs` | 每 `HostConn` 一份 `{last_try, done}`;帧循环跑判据 → spawn exec;`expand_tilde` / `files_start_dir` / `sync_target_of` | 唯一允许知道其余四者的地方 |
| `mullion-app/src/ui/pane_title.rs` | `title_text` 五参拼接;删 `side_text`/`SIDE_MAX_FRAC` | 显示层 |
| `mullion-app/src/shell/workspace/geom.rs` | `TERM_PAD_PT` + `term_px` 内缩 | 渲染/命中/`window_change` 共读同一份 `PaneGeom`(T4) |
| `mullion-store` | `Settings` 新增自举开关 | 设置持久化 |

依赖方向不变:`app → {core, term, ssh, store}`。`mullion-term` 这一片**不动**——
解析已经是对的,缺的只是数据源。

## 错误处理

- exec 失败(退出码非 0 / channel 开不出 / 超时):**静默**,只记 `log::debug`。
  用户没装 tmux、或用非默认 socket,都会走到这里,不该弹错误。
- exec 有超时包裹(与 `SftpClient::open` 同样的理由:裸 russh 调用不带超时,
  高延迟链路上会挂住)。
- 覆写的是**内存里**的 tmux 全局选项,server 退出即失效,没有需要回滚的持久状态。
- `~` 展开拿不到 home 时不展开、不猜;`~user` 形式不展开。

## 测试

**纯函数(自动)**

- `remote_bootstrap`:命令串字面量(含 `&&` 串联与单引号包裹的 format string);
  `should_attempt` 的四条——首次立刻试 / 未到 30 s 不重试 / 到点重试 / 已成功恒不再试。
- `expand_tilde`:`~`、`~/x`、已是绝对路径、`~user`、空输入、非 UTF-8 尾巴。
- `files_start_dir`:四档优先级 + home 缺失时 `~` 被挡掉。
- `pane_title::title_text`:四种缺段组合 + 断开时不带目录/tmux。
- `geom`:缩进在 100%/150%/200% 缩放下的像素值;极窄 pane 下 `w` 不下溢、`grid ≥ (1,1)`;
  最右 pane 的 `GAP_PX` 让位与缩进叠加正确。

**live(自动,`--ignored`)**

对**本机 tmux**(3.7b 已装)真跑一遍:起一个 `-L mullion-test` 的 server → 跑自举
命令 → 断言 `tmux -L mullion-test show -g set-titles` 变成 `on` 且
`set-titles-string` 是我们那串 → `kill-server`。这条是真验证,不是脚手架。

**人工验收(无头验不了)**

- Windows 上缩进的实际观感(是否还是顶着边界 / 是否吃掉了太多列)。
- 标题条在窄 pane 下的截断位置是否合理。
- 真机上 `tmux attach` / `tmux new` / `cd` 之后标题条三段是否按预期变化,以及
  `cd` 后「慢一次提示符刷新」的时延是否可接受。
- `Ctrl+Shift+B` 打开文件面板,远端栏是否直接落在终端所在的深目录。
- 关掉自举开关后,远端 tmux 的 `set-titles` 是否确实不再被改。

## 文档

- `docs/remote-state-setup.md` 重写:从「你得手配」改成「默认自动配、可关」,
  保留手配片段给关掉开关的人,并写明覆写全局 `set-titles-string` 的影响面。
- `spec.md` 新增 F124 一行,并在 F123 行里指向它。
