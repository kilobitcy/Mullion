# mullion

原生 GPU 加速的 SSH 客户端骨架：自研分屏 + 集成 SFTP，为「Claude Code 跑在远端 tmux 里」这个场景优化。

---

## 分层

```
mullion-core     布局树（自研分屏）。零 UI、零 IO，可纯单测。
mullion-term     alacritty_terminal 封装 + 按键/鼠标编码。
mullion-ssh      russh：PTY channel actor，后续加 SFTP。
mullion-app      winit + wgpu + glyphon：窗口、渲染、输入分发。
```

依赖方向严格单向：`app → {core, term, ssh}`，其余互不依赖。
这条约束的价值：布局 bug 和键码 bug 都能在**没有窗口**的情况下写测试复现。
这两类 bug 是终端项目里最费时间的，值得为它牺牲一点便利。

## 为什么是这几个库

| 选择 | 理由 | 不选什么 |
|---|---|---|
| `alacritty_terminal` 0.26 | **Apache-2.0**，可闭源。只做 VT 状态机不含渲染，正好。**自带 `damage()` 逐行脏区** | `wezterm-term`（能用，但 API 稳定性和文档差一截） |
| `glyphon` | cosmic-text 整形 + wgpu 字形图集。**CJK 字体回退它帮你搞定** | 自己写 glyph atlas（能省 2 个月） |
| `russh` | 纯 Rust，无 libssh2 C 依赖，Windows 交叉编译无痛 | `ssh2`（要链 C 库，Windows 上很烦） |
| `winit` + `wgpu` 裸用 | 分屏要自己实现，套 iced/egui 反而是负担 | `iced`（它的 `pane_grid` 就是分屏，用了等于没自己写） |

> Alacritty 本体用的是 OpenGL，不是 wgpu。你用 `alacritty_terminal` 只拿状态机，
> 渲染完全自己写，这点不冲突。

---

## 六个必须做对的点

按「做错了会有多痛」排序。前三个直接决定 Claude Code 能不能用。

### 1. `Event::PtyWrite` 必须回写 SSH channel

`Term` 通过 `EventListener` 发出需要回给对端的字节。漏了会导致：

- Claude Code 用 `CSI ? 2026 $ p` 探测同步输出 → 收不到应答 → **退回逐帧刷新 → 闪**
- 鼠标事件永远上不去 → `/tui fullscreen` 的鼠标交互全废
- 应用查询光标位置时**永久卡住**

代码在 `mullion-term/src/emulator.rs`，找 `Event::PtyWrite`。

### 2. 同步输出（DEC 2026）要真正实现

不是「解析了就行」——你的渲染器必须在 `CSI ? 2026 h` 和 `CSI ? 2026 l` 之间
**攒住不画**，收到 `l` 才提交一帧。这是消灭闪烁的唯一根治手段，
Anthropic 自己也在往 tmux / VS Code 终端推这个补丁。

`alacritty_terminal` 已经实现了 DECRPM 应答，你要补的是渲染侧的「攒帧」逻辑。

### 3. 喂数据和重绘必须解耦

流式输出时官方 issue 实测过 tmux 下每秒 4000~6700 次滚动事件。
「收到数据就重绘」= 抖成一团 + GPU 空转。

正确节奏（见 `mullion-app/src/main.rs` 的 `about_to_wait`）：

```
SSH 数据到达 → 立刻 feed 进仿真器（不节流，VT 状态机很快）
             → 只标记 dirty
固定 16ms   → 消费 dirty，画一帧
完全空闲     → ControlFlow::Wait，让出 CPU
```

### 4. 分屏后必须发 `window_change`

分屏改变了每个 pane 的列/行数。不通知对端，tmux 里的 Claude Code
还按老宽度排版，全屏 TUI 直接错行。见 `App::reflow()`。

### 5. Shift 必须能逃出鼠标捕获

`/tui fullscreen` 开鼠标捕获后，如果无条件上报，用户就永远复制不了终端里的文字。
行业惯例：**按住 Shift 强制走本地划选**。见 `keymap::mouse_should_report`。

### 6. Shift+Enter 的两套编码

Claude Code 用它插入换行而非提交。

- 对端启用 Kitty 键盘协议 → `CSI 13 ; 2 u`
- 否则 → `ESC CR`（`\x1b\r`），这是 `/terminal-setup` 写入的约定
- `Ctrl+J` 永远发 `\n`，官方保证的保底方案

已在 `keymap.rs` 实现并带测试。

---

## 高延迟链路调优（你走代理，这三条值钱）

都在 `mullion-ssh/src/pty.rs`：

1. **keepalive 30s**。机场 / NAT 常在 60~120s 无流量后静默丢连接。
   Claude Code 思考时上行可以几分钟没数据，正好踩坑。
2. **SSH 窗口开到 8MB**。默认窗口在 200ms+ RTT 下是吞吐瓶颈，
   表现为刷大段 diff 时「一顿一顿地吐」。
3. **出站队列有界（256）**。粘贴大段文本时靠背压保护内存，别用 unbounded。

> 更狠的做法：网络层换 UDP。参考 Go 写的 `trzsz-ssh (tssh)` + `tsshd`，
> 它提供 mosh 式的 UDP 模式。这条路能根治「代理抖一下就断线重连」，
> 但工作量大，建议放到 v0.4 之后。

---

## 里程碑建议

| 版本 | 范围 | 大概工作量 |
|---|---|---|
| **v0.1** | 单 pane 能连上、能跑 `claude`、TUI 不错行 | 2–3 周 |
| **v0.2** | 分屏（`mullion-core` 已给全）+ 焦点切换 + 拖拽 resize | 1 周 |
| **v0.3** | 同步输出攒帧 + damage 差分渲染 | 1–2 周 |
| **v0.4** | SFTP 侧栏：跟随 shell cwd、拖拽上传、就地编辑 | 2–3 周 |
| **v0.5** | 凭据 vault、`~/.ssh/config` 导入、跳板机 | 2 周 |

**v0.1 就去跑真实的 Claude Code**，别等功能齐了再验证。
终端仿真的坑只会在真实 TUI 下暴露，自己写 demo 测不出来。

---

## 上手

```bash
cargo test -p mullion-core    # 布局逻辑，无需 GPU
cargo test -p mullion-term    # 键码编码
cargo run  -p mullion-app
```

## 待补

骨架里这些是空的 / 占位，需要你填：

- `mullion-app/src/render.rs` — glyphon 单元格渲染器（**最大的一块**）
- `mullion-app/src/pane.rs` — Pane 结构：串起 Emulator + PtySession
- `mullion-ssh/src/sftp.rs` — russh-sftp
- `pty.rs` 里的 `KnownHosts` 实现 — **别图省事直接返回 `Ok(true)`**，
  那等于关掉了 SSH 的全部身份保证

## 版本核对

代码按下列版本写的，动手前 `cargo add` 一遍确认签名没变
（`russh` 发版较快，`client::Handler` 的签名历史上改过几次）：

```
alacritty_terminal 0.26   vte 0.15
russh 0.54                winit 0.30
wgpu 23                   glyphon 0.7
```
