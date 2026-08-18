# 光标形态 + 输入法内联 + 文件图标 + 断线重连 —— 设计

> 需求编号 **F125**(光标形状与闪烁)、**F126**(输入法 preedit 内联)、
> **F127**(SFTP 文件类型图标)、**F128**(断线自动重连)、**F129**(断连 pane 的
> `Ctrl+D`)——五条均为新增,待写进 `spec.md` §4。日期 2026-08-18。

## 为什么把这五项放在一片里

它们是同一次实机使用暴露出来的五个「日常一直硌着」的点,彼此没有依赖,但落点
高度重叠:F125/F126 都在终端自绘层(`gpu.rs` / `text.rs` / `snapshot.rs`),
F128/F129 都在 pane 生命周期(`workspace/mod.rs::pump` / `app.rs` 事件循环)。
分五片发五个版本没有收益,合成一片一次发版。

## 先说一个根因发现:重连之所以「不快」,是因为压根检测不到

`crates/mullion-ssh/src/session.rs:301` 用的是 `client::Config::default()`。
russh 0.54.5 的默认值(`src/client/mod.rs:1695`)是:

```rust
inactivity_timeout: None,
keepalive_interval: None,
keepalive_max: 3,
```

**没有 keepalive**,意味着代理链路中断造成 TCP 半开时,客户端不会收到任何 EOF:
`ChannelMsg::Eof/Close` 不来 → `rx` 不关闭 → `Workspace::pump` 那条
`TryRecvError::Disconnected` 分支永远不进 → pane 连 `Disconnected` 都标不上,
只是静静地不出字。**任何重连逻辑都不会被触发**。

所以 F128 的第一步不是写重连,是开 keepalive。这也是「用户体感上断线之后要
很久才反应过来」的机制解释。

---

## ① F125 光标形状与闪烁

### 现状

`gpu.rs::CursorStyle` 只有两个变体:焦点 pane 画 `Block`(实心方块)、非焦点画
`Hollow`(空心框),**都不闪**,且完全不看远端要什么形状。

### 做什么

**跟随远端 DECSCUSR(`CSI Ps SP q`),默认竖线闪烁。**

`alacritty_terminal 0.26` 已经解析了这条序列(`term/mod.rs:2204 set_cursor_style`),
`Term::cursor_style()` 是 public 的,返回 `CursorStyle { shape, blinking }` ——
**我们一行解析代码都不用写**,只需要把它接出来。

- `mullion-term`:
  - `snapshot::Cursor` 增加 `shape` 与 `blinking` 两个字段。
    `shape` 用**本 crate 自己的枚举**(`Block` / `Beam` / `Underline` /
    `HollowBlock` / `Hidden`),不把 `alacritty_terminal::vte::ansi::CursorShape`
    漏进公开 API —— 依赖方向不变,且 alacritty 加变体时不会直接冲击 `mullion-app`。
    映射写成一个 `match`,新变体编译期报错(不用 `_ =>` 兜底)。
  - `Term` 的 `Config::default_cursor_style` 设成 `Beam` + `blinking: true`。
    这一处同时满足两件事:远端没说话时是竖线闪烁(用户要的默认),远端说了话
    (vim 普通模式请求方块)就跟着变。
  - `Emulator::cursor()`(轻量同源版)与 `snapshot()` 两处必须给出**同一份**
    shape/blinking —— 既有测试 `cursor_agrees_with_the_full_snapshot_even_after_scrolling`
    已经在钉这件事,加字段后它自动覆盖。

- `mullion-app`:
  - `gpu.rs::CursorStyle` 重新定义为「怎么画」:`Bar` / `Block` / `Underline` /
    `Hollow` / `None`。焦点 pane 由 `snap.cursor.shape` 决定,**非焦点 pane 恒
    `Hollow` 且恒不闪** —— 4 块分屏一起闪会让人看不出焦点在哪,这条优先级高于
    「忠实呈现远端形状」。
  - 竖线宽 2px、底线高 2px(1px 在高 DPI 下几乎看不见;不做 DPI 缩放,与既有
    `HOLLOW_PX` 同一档处理)。

### 闪烁与帧率的接线(T3/T7 红线)

闪烁 = 周期性重绘,这正是 T3(每秒几千次重绘)和 T7(节流后 `WaitUntil` 不复位
导致 100% CPU 忙转)的雷区。做法:

- 相位是**纯函数**:`blink_visible(now_ms, last_input_ms) -> bool`,半周期
  **530ms**(Windows 系统默认光标闪烁周期),放在 `frame.rs` 旁边,可脱离 GPU 单测。
- **打字重置相位**:`last_input_ms` 在每次终端收到键盘输入时更新,保证「刚敲完
  一个字符,光标一定是亮的」。不重置的话连续打字时光标会随机隐没,观感像丢帧。
- **窗口失焦不闪**:失焦时焦点 pane 的光标按 `Hollow` 画(与非焦点 pane 同款),
  和 Windows 上其它终端的惯例一致,也顺带省掉后台窗口的周期重绘。
- 定时走 `about_to_wait` 里**既有的** `next_frame_at` 机制排 `WaitUntil`,不新开
  第二条定时路径;下一次翻转点由上面那个纯函数算。

### 判据

- shape/blinking 从 `Term::cursor_style()` 取,DECSCUSR 各 Ps 值 → 形状的映射有
  单测(喂 `CSI 2 SP q` 等字节进 `Emulator`,断言快照里的 shape)。
- 默认(远端一言不发)是 `Beam` + 闪,有单测。
- `blink_visible` 的相位、打字重置、失焦不闪,三条各有纯函数单测。
- 非焦点 pane 恒 `Hollow` 恒不闪,有单测(拿两个 pane 的 `PaneRender` 走
  `quads_for_panes`)。
- 闪烁**不得**退化成每帧重绘:`redraw_is_frame_capped`(T3)与 `frame::tests`(T7)
  必须仍绿;另加一条守护,断言闪烁排的是 `WaitUntil` 而不是无条件 `request_redraw`。
- 观感(2px 够不够粗、530ms 快不快)只有人眼能判 → 人工验收清单。

---

## ② F126 输入法:拼音内联显示

### 现状

`input.rs::ImeState` 只保留一个 `preediting: bool`,用途是**吞键**(组字期间不把
键送去终端)。`Ime::Preedit(text, _)` 里的 `text` 被丢掉,所以屏幕上什么都看不见 ——
用户在打「gang'jin」时,终端是空的,只有系统候选框浮着。

### 做什么

按参考图(Windows cmd)的做法:**拼音串画在光标位置,带下划线表示「尚未提交」**。

- `ImeState` 保存 preedit 文本。winit 的 `Ime::Preedit(text, cursor_range)` 里那个
  range 忽略不用 —— 已拍板光标停在拼音串**末尾**。
- 渲染(只画焦点 pane):
  1. 从光标格起,按终端网格逐字符摆放,**CJK 宽字符占两格**(复用 `text.rs` 既有
     的宽度判定,不另起一套)。
  2. 先铺一层默认背景色 `Quad` 盖住底下原有字符,再画 preedit 文字,再画 1px
     下划线 `Quad`(glyphon 不画下划线,必须自己画)。
  3. **超出行尾直接截断**,不折行 —— 折行要动到行内容布局,而 preedit 是纯覆盖层,
     不该有那么大的权力。
  4. 光标画在 preedit 末尾。
- `set_ime_cursor_area` 跟着 preedit **末尾**走(现在跟的是光标格)。不跟的话
  拼音一长,系统候选框就压在刚打出来的拼音上。既有的「只在位置变了才调」那条
  优化保留(跨进程系统调用,每帧无脑调会掉帧)。

### 判据

- 布局算式抽成纯函数 `preedit_layout(cols, cursor_col, text) -> Vec<PreeditCell>`
  (每项含列号、字符、占几格),脱离 GPU 单测:ASCII 拼音、含 CJK 的已转换串、
  行尾截断、光标列已在最后一格,四种情形各一条。
- preedit 非空时光标落在串末尾,有单测。
- `ImeState` 三条结束边(`Commit` / 空 `Preedit` / `Disabled`)都要**清空文本**,
  不只是清 `preediting` 标志 —— 漏一条就会在屏幕上留一串永不消失的幽灵拼音。
  三条各有单测(既有测试已覆盖标志位,这次扩到文本)。
- 真实第三方输入法(搜狗/微信/小狼毫)的观感与候选框位置 → 人工验收(spec §7 R2
  一直挂着的风险项)。

---

## ③ F127 SFTP 文件类型图标

### 现状

`ui/file_icon.rs` 有四种 painter 自绘线框(目录 / 文件 / 链接 / 其他),1px stroke,
**颜色由调用方按「名字能不能操作」传入**,所有类型同色。一屏文件全是同一个页角
图标,扫视时区分不出类型。

### 做什么

细分到 8 类,每类一个形状 + 一个语义色:

| 类 | 形状 | 判据 |
|---|---|---|
| 目录 | 实心文件夹(带页签) | `EntryKind::Dir` |
| 归档 | 盒子 + 捆带 | `.zip .tar .gz .bz2 .xz .7z .rar .tgz` |
| 图片 | 相框 + 山形 | `.png .jpg .jpeg .gif .bmp .svg .webp .ico` |
| 代码 | `< >` 尖括号 | `.rs .py .sh .js .ts .c .h .cpp .go .java .rb .lua .toml .yaml .yml` |
| 文档 | 页 + 文字线 | `.md .txt .log .json .csv .pdf .doc .docx` |
| 可执行 | 齿轮/箭头 | `.exe .msi .bat .cmd`,或权限位含 `+x`(且不是目录) |
| 链接 | 页 + 箭头 | `EntryKind::Symlink` |
| 其他 | 菱形 | 其余(含 `EntryKind::Other`、无扩展名且不可执行的普通文件) |

- 判类是**纯函数** `classify(kind, name, perm) -> IconKind`,与形状、颜色分开。
- 扩展名**小写归一**后比对,表驱动(一张 `&[(&str, IconKind)]`),加类型只改一处。
- 颜色进 `theme.rs`,与 F62/F80 的色板体系同源;**不硬编 RGB 在 `file_icon.rs` 里**
  (主题一换就失配,和 F80「终端背景色三处必须同源」同一个教训)。
- **保留既有的可操作性闸门**:名字不可操作(`!is_operable()`)的行,图标仍按
  `fg_dimmer` 整体变灰,不上类型色 —— 否则会出现「文字灰了图标还亮着」这种
  自相矛盾的行,而这正是当初把颜色决定权交给调用方的理由。
- 仍然 painter 自绘,**不用 emoji/字体字形**(Windows 上会变豆腐块,且字形宽度
  不可控会让整列名称起点漂移 —— 这条是 D1 定的,不动)。

### 判据

- `classify` 表驱动单测:每类至少两个样本 + 大小写混合 + 无扩展名 + 双扩展名
  (`a.tar.gz`)+ 点开头的隐藏文件(`.bashrc` 判文档而不是「扩展名 bashrc」)。
- 可执行位优先于扩展名(一个 `+x` 的 `run.sh` 判「可执行」还是「代码」——**判代码**,
  扩展名优先,因为脚本本来就常带 `+x`,否则半屏脚本全变齿轮)。这条要有单测钉住。
- 既有两条形状测试扩到 8 类:每个图标画在格子内、8 类两两不同形。
- 语义色与面板底色的对比度 ≥ 3:1 实算单测(沿用 F62 的阈值与写法)。
- 16px 下 8 种形状是否真的一眼可辨 → 人工验收。

---

## ④ F128 断线自动重连

### 第 0 步:开 keepalive

`session.rs` 建 `client::Config` 时设:

```rust
keepalive_interval: Some(Duration::from_secs(10)),
keepalive_max: 3,        // 默认值就是 3,显式写出来是为了让判据可读
```

10s × 3 = **最坏 30s 检测到死链**。russh 的实现(`client/mod.rs:1195` 一段)是
「收到任何数据就把计数清零」,所以正常收发时不会误判;超过 `keepalive_max` 次无
应答就关连接 → `Handle` 关闭 → 该连接上所有 channel 的 `rx` 关闭 → `pump` 那条
`TryRecvError::Disconnected` 分支被触发。链条到此打通。

**不设 `inactivity_timeout`** —— 那是「一段时间没数据就回收连接」,会把空闲但
健康的连接(挂着 tmux 过夜,正是本项目的主场景)直接杀掉。

### 第 1 步:区分两种「rx 关闭」——本项最容易做错的地方

`rx` 关闭有两种截然不同的原因:

| 原因 | 现象 | 该怎么办 |
|---|---|---|
| 传输层死了(网络/代理断) | `handle.is_closed() == true` | **重连** |
| 用户在远端敲了 `exit` | channel EOF,但连接还活着,`is_closed() == false` | **绝不重连** |

判据用 `SshConnection::is_closed()`(`session.rs:162`,包的是 russh `Handle::is_closed`)。
**漏掉第二种的后果**:用户 `exit` 之后 pane 立刻又活过来,他永远退不出去,而且
看不出为什么。这条必须有一个能自证变红的守护测试。

第二种情形下 pane 停在 `Disconnected` 等用户处置 —— 正好交给 F129 的 `Ctrl+D`。

### 第 2 步:按 host 分组重连

一条 SSH 连接承载多个 pane(adr-009),断的时候是**整条连接**上的 pane 一起死。
所以重连按 `ws.hosts[i]` 分组:建**一条**新连接,再为该 host 上每个断掉的 pane
开 channel。4 分屏只握手一次,不是四次。

- 退避复用 `mullion-ssh/src/tunnel.rs::backoff_delay`(1s→2s→…封顶 30s,8 次后停),
  不另写一套 —— F114 已经为它写过序列单测。
- 凭据取 `TerminalTab::last_cfg`(那里存着完整的 `SshConfig`,含解密后的凭据,
  连接建立时就在内存里了),**不回库重查**:重连时用户完全可能已经改过或删了
  那条会话记录(与 `PendingRehost` / `automation_template` 同一个理由)。
- 主机密钥用 `host_key.rs` 已有的后台重连策略:**只信已记录且一致**;指纹变了
  **立即停止且绝不重试**(F114 同款红线,`host_key.rs:140` 那段注释就是为这个写的)。
- 8 次退避全失败后放弃,屏内写一行「重连失败,已停止」,pane 停在 `Disconnected`。

### 第 3 步:换挂时保留内容

新增 `reattach_pane`,与既有 `rehost_pane`(`app.rs:7152`)同族,但有一处关键差别:

| | `rehost_pane`(换节点) | `reattach_pane`(重连) |
|---|---|---|
| `emulator` | **重建**(换了台机器,旧内容没意义) | **保留**(断线前的输出可滚可复制) |
| `pty` / `rx` | 换新 | 换新 |
| `pacer` | 重置 | 重置 |
| `saw_first_byte` | `false` | `false` |
| `last_grid` | `(0,0)` 逼发 `window_change`(T4) | 同左 |
| `cwd` / `tmux` | 清空 | 清空(新 shell 会重报) |
| `status` | `Live` | `Live` |

两个函数共用一个私有 helper 处理相同的那几项,避免「改了一边忘了另一边」——
T4 那条 `last_grid = (0,0)` 尤其不能漏,漏了远端就按 80x24 排版。

### 第 4 步:重跑登录后自动化

用 `TerminalTab::automation_template` 走 `automation::pending_for_extra_pane`
(与分屏新 pane、换节点后的 pane 同一条路径)。这样重连后 tmux attach 会自动跑,
Claude Code 会话直接回到眼前 —— 这正是 spec §1 场景 S3 描述的东西。

### 第 5 步:可见反馈

- **屏内一行**:本地字节喂进该 pane 的 `emulator`(不经过网络),
  `\r\n[Mullion] 连接已断开,3 秒后重试(第 2 次)\r\n` / `\r\n[Mullion] 已重连\r\n`。
  走 emulator 意味着它进滚动回溯、可复制、可被 `clear` 清掉,不需要额外图层。
  **不做实时倒计时**:每次重试**之前**写一行,写完就不动了(倒计时要把一个
  deadline 引进帧循环,正是 spec §1 修订一要避免的东西,`automation_status`
  当初不做定时淡出也是同一个理由)。
  已知代价:断线时远端若停在 alternate screen(全屏 TUI),这行会写在 alt screen 上,
  远端重画时被覆盖。接受 —— 相反的做法(单开一层 overlay)要在渲染层引入第二套
  文本来源,代价大得多。
- **标题条**:`PaneStatus` 增加 `Reconnecting`,标题条圆点第三态(`theme.rs:73`
  那条注释里预留的槽位正是这个)。`preset.rs::plan_for_count` 的
  `by_status` 分档要跟着改成**穷尽 match**(「减屏优先关已断开的」这类判据在加档
  时必然漏 —— 这是本项目已经踩中过三次的坑)。

### 第 6 步:收尾 sftp

连接换了,`TerminalTab::sftp`(蹭 `ws.hosts[0].handle` 开的)就是死的。重连成功后
把 `sftp` 与 `sftp_home` 置 `None`,下次用户点侧栏时重开。在途的 `sftp_tasks`
一并 abort(收口纪律见该字段文档)。

### 判据

- keepalive 参数写在 `session.rs` 且值可断言(源码级守护 + 一条断言 `Config` 字段
  的单测);**守护锚点必须带行首缩进**(第五类恒绿模式:不带缩进会匹配到测试
  自己那一行,永远绿)。
- 「`exit` 不重连 / 断网才重连」的分档是纯函数(输入:rx 状态 + `is_closed()`),
  两条单测,且能自证变红。
- 按 host 分组:一条 host 上 3 个断掉的 pane 只产生 1 次 `establish`,有单测
  (拿假 sshd 数握手次数)。
- `reattach_pane` 保留 emulator 内容、且 `last_grid` 被重置,两条单测。
- 退避序列直接复用 F114 已有单测,不重写。
- 指纹变更不进重试队列,断言能自证变红(F114 同款要求)。
- 重连后自动化被起了一次,有单测(与 `PaneOpened` 那条同款的源码级守护 + 行为测试)。
- 真实断网/代理抖动下的恢复时间 → 人工验收(拔网线、掐代理各一次)。

---

## ⑤ F129 断连 pane 的 `Ctrl+D`

判据是纯函数:

```rust
fn ctrl_d_action(status: PaneStatus, is_last_pane: bool) -> CtrlD
// Live                        -> Send   (原样发 0x04,EOF 语义不能动)
// Disconnected / Reconnecting -> ClosePane,若 is_last_pane 则 CloseTab
```

- `Live` 时必须原样发 `0x04`。这是 shell 的 EOF,劫持它等于把一个天天用的键废掉。
- 非 `Live` 时不发字节(对端已经没有了),直接关 pane;是标签里最后一个 pane 就
  关掉整个标签(已拍板)。关标签走**既有的关标签路径**(`Ctrl+W` 那条),
  「关掉最后一个标签之后剩什么」沿用现状,不在这里另立一套。
- 键盘路由不变:仍然**先判后喂**,不把这个事件喂给 egui(T8 红线)。

### 判据

- `ctrl_d_action` 三个分支 + `is_last_pane` 组合的纯函数单测,`PaneStatus` 用
  **穷尽 match**(加了 `Reconnecting` 之后编译器会逼这里跟着改,这是刻意的)。
- `Live` 下 `Ctrl+D` 仍编码成 `0x04`,单测(拿既有 `keymap` 测试同款写法)。
- 关到最后一个 pane 时标签被关掉,有单测。

---

## 不做什么

- **不改 `nodelay`**。`client::Config::nodelay` 默认 `false`(Nagle 开着),高延迟
  链路下逐字输入会被额外攒包 —— 这是一个真实的手感问题,但不在这次请求范围内,
  已单独报给用户,由他决定要不要另开一片。
- **不做光标闪烁/形状的用户设置项**。远端 DECSCUSR + 一个合理默认已经覆盖需求,
  加设置项是没人要的功能(YAGNI)。
- **不做 preedit 折行**、不做候选框自绘(那是系统输入法的事)。
- **不做重连时的凭据交互**。缺凭据(`last_cfg` 不在)就不自动重连,停在
  `Disconnected` 等人 —— 与 F114 同一条纪律。
- **不改 SFTP 面板的列/排序/密度**,只动图标。

## 落点一览

```
mullion-term/src/snapshot.rs      Cursor 加 shape/blinking(+ 自己的 CursorShape 枚举)
mullion-term/src/emulator.rs      从 Term::cursor_style() 取;default_cursor_style = Beam+blink
mullion-ssh/src/session.rs        client::Config 开 keepalive(10s × 3)
mullion-app/src/gpu.rs            CursorStyle 扩成 5 种画法 + preedit 底色/下划线 Quad
mullion-app/src/frame.rs          blink_visible 纯函数 + 下一次翻转点
mullion-app/src/text.rs           preedit 文本层 + preedit_layout 纯函数
mullion-app/src/input.rs          ImeState 保存 preedit 文本
mullion-app/src/ui/file_icon.rs   classify + 8 类形状
mullion-app/src/theme.rs          8 类图标语义色 + 重连中的圆点色
mullion-app/src/shell/workspace/  PaneStatus::Reconnecting;pump 分档;reattach_pane
mullion-app/src/shell/input_route.rs  ctrl_d_action
mullion-app/src/app.rs            重连调度(按 host 分组 / 退避 / 事件)+ 闪烁排 WaitUntil
                                  + set_ime_cursor_area 跟 preedit 末尾
spec.md                           新增 F125~F129 五行
```

## 人工验收清单(交叉编译出 exe 后在 Windows 11 上跑)

1. 空闲时光标是竖线且在闪;打字过程中光标不随机隐没。
2. `vim` 里普通模式光标变方块、`i` 进插入模式变竖线;`:q` 退出后回竖线。
3. 分屏 2 块:只有焦点那块的光标在闪,另一块是不闪的空心框;窗口失焦后两块都不闪。
4. 搜狗/微信输入法打「gangjin」:拼音出现在光标处、带下划线,候选框在拼音下方
   不遮挡;按 Esc 取消后屏幕干净不留字;选词后拼音消失、汉字进终端。
5. SFTP 面板浏览一个混合目录:8 类图标一眼可辨,颜色不刺眼;名字乱码的行图标仍然是灰的。
6. 掐掉代理(或拔网线)::30s 内 pane 出现「连接已断开,N 秒后重试」;恢复网络后
   自动重连、tmux attach 自动跑、断线前的输出还在上方。
7. 在远端敲 `exit`:pane 变断开态且**不会**自动重连;按 `Ctrl+D` 关掉这块分屏;
   对最后一块分屏按 `Ctrl+D`,整个标签关闭。
8. 活着的 pane 里按 `Ctrl+D`:行为与以前一致(shell 收到 EOF)。
