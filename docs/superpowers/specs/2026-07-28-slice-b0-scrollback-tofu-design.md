# 切片 B0 — 滚动回溯(F17)+ TOFU 主机密钥持久化(F3) 设计

> 状态：已定稿（2026-07-28，brainstorming 通过）
> 关联 spec 编号：**F17**（滚动回溯）、**F3**（主机密钥 TOFU + 变更告警）
> 触碰领域陷阱：T5（Shift 逃生门）、T8（键盘先判后喂）、T4（尺寸变更）
> 里程碑：补齐 v0.1 需求清单里两个真实缺口，把「能连、能打字」升级为「能翻历史、能信任管理」。

切片 A 收口后 v0.1 的**判定标准**（真机不闪）已签字，但**需求项没齐**。B0 收口两项：
用户翻不了历史（F17），以及主机密钥指纹只活在内存、重启即忘、变更时只有一行通用错误（F3）。

---

## 1. 范围

**做**
- F17：终端 scrollback 可视化 + 三档滚轮分流 + `Shift+PageUp/PageDown` 翻页 + 行数可配（默认 10000）。
- F3：TOFU 指纹持久化到 `known_hosts.toml`；首次连接弹窗确认；指纹变更弹红色告警窗，可覆盖。

**不做（超出先问）**
- 划选复制 / 搜索 scrollback（F14/F15 另计）。
- 兼容 OpenSSH `~/.ssh/known_hosts` 格式的读写（见 §4.1 决策）。
- 主密码 / Argon2id（F71，ADR-006 已决延后）。
- `~/.ssh/config` 导入（F2）、分屏（F30–F35）。

---

## 2. 现状与真实缺口

盘点时的报告说「F17 未实现，因为 `Emulator` 无 scrollback」。**这个判断是错的**，已核实：

- `Term::new(Config, &Dimensions, listener)` 只读 `columns()` / `screen_lines()`，**从不读 `total_lines()`**。
  history 全部来自 `Config::scrolling_history`，`Config::default()` 已经是 **10000**。
- 所以 **scrollback 一直在跑**，`emulator.rs:40` 那句「骨架无 scrollback」是过时注释（本切片顺手修正）。

真实缺口只有两条：

1. **没有任何入口调 `Term::scroll_display()`** —— 滚轮/PageUp 事件根本没接。
2. **`snapshot()` 不含 `display_offset` 偏移** —— `grid[Line(i)]` 的 `Storage::compute_index` 是
   `-(requested - visible_lines).0 as usize - 1`，纯按 visible_lines 算，**不考虑 display_offset**。
   不改这里，滚动会「数据滚了、画面不动」，完全没有视觉效果。

viewport 第 i 行的正确索引：`Line(i as i32 - display_offset as i32)`。

---

## 3. F17 — 滚动回溯

### 3.1 为什么必须三档分流

**tmux 一启动就进 alternate screen。** 而 alacritty 的 `inactive_grid`（alt screen 那块）恒为
`Grid::new(num_lines, num_cols, 0)` —— **alternate screen 没有 scrollback，且改不了**。

也就是说：在本项目的**核心场景**（远端 tmux 跑 Claude Code TUI）里，本地 scrollback
**一行都拿不到**。只对裸 shell 有效。这不是缺陷，是终端协议的事实——历史归远端 tmux 管。

所以滚轮必须按终端状态分流，否则用户在 tmux 里滚轮毫无反应，会认为「这客户端坏了」。

### 3.2 三档决策（纯函数，可无窗口单测）

新增 `mullion_term::keymap::wheel_action`：

```rust
pub enum WheelAction {
    /// 本地回溯：Term::scroll_display(Scroll::Delta(lines))，正数向上。
    LocalScroll { lines: i32 },
    /// 鼠标滚轮上报：button 64=上/65=下，col/row 为 1-based 单元格坐标，
    /// sgr=false 时退化成 X10 编码。重复 count 次。
    Report { button: u8, col: u16, row: u16, sgr: bool, count: u16 },
    /// count 次 ESC[A(up=true) / ESC[B。
    ArrowKeys { up: bool, count: u16 },
    /// 远端明确关闭了滚轮相关模式，不臆造输入。
    None,
}
pub fn wheel_action(
    mode: TermMode, shift: bool, lines: i32, cell: (u16, u16),
) -> WheelAction
```

`TermMode` 由 `mullion-term` **重导出**（`pub use alacritty_terminal::term::TermMode`），
app 不直接依赖 `alacritty_terminal`——这条依赖只允许存在于 term crate 里。

| 条件 | 动作 | 用户看到 |
|---|---|---|
| 按住 Shift（**恒优先**） | `LocalScroll` | 本地回溯，任何情况下都能用 —— T5 逃生门同源 |
| 非 alt screen | `LocalScroll` | 裸 shell 翻本地历史 |
| alt + `MOUSE_MODE` 任一位 | `Report`（SGR 需 `SGR_MOUSE`） | tmux 自动进 copy-mode，翻远端历史 |
| alt + 无上报 + `ALTERNATE_SCROLL` | `ArrowKeys` | less / man 里滚轮变上下键 |
| alt + 无上报 + 无 `ALTERNATE_SCROLL` | `None` | 远端明确关了，不臆造输入 |

`ALTERNATE_SCROLL` 在 alacritty 里**默认开**（`TermMode::default()` 含它），远端可 DECRST 1007 关掉。

**Shift 恒走本地**是有意为之：和 T5「Shift 屏蔽鼠标上报以便划选」同一条逃生门，
用户只要记住「按住 Shift 就归本地」这一条规则，就永远不会被远端的模式设置困住。

### 3.3 键盘

- `Shift+PageUp` / `Shift+PageDown` → `Scroll::PageUp` / `PageDown`（本地）。
- 裸 `PageUp` / `PageDown` → 照旧编码转发远端（不改现有行为）。
- 用户按下任何普通键 → `scroll_to_bottom()`（`Scroll::Bottom`），标准终端行为。

### 3.4 term crate 的 API 增量

```rust
impl Emulator {
    pub fn with_history(cols: u16, rows: u16, history: usize) -> Self;  // F17 行数可配
    pub fn scroll(&mut self, scroll: Scroll);
    pub fn scroll_to_bottom(&mut self);
    pub fn mode(&self) -> TermMode;   // app 据此做 wheel_action 分流
}
```

`snapshot()` 改为按 `display_offset` 偏移取行。

### 3.5 四个坑（写进代码注释）

1. `snapshot()` 不偏移 `display_offset` → 滚动**完全没有视觉效果**（本切片的主要工作面）。
2. 光标滚出可视区时 `Cursor::visible` 必须置 false，否则光标画在错误的行上。
3. alt screen 下 `scroll_display` 静默无效（`max_scroll_limit == 0`）—— 这是三档分流存在的理由，
   不是「先这么写着」的地方。
4. **新输出到达时保持滚动位置**是 alacritty 内建的（`grid/mod.rs:267`：`display_offset != 0`
   时自动加偏移），**我们什么都不用做**，也**不要**自作聪明去补。

顺带：`shell/window_state.rs` 那套「最小化时 `Resized(0,0)` 不许 resize，否则带 scrollback 的
primary grid 按 1 列 reflow 后被 truncate、历史永久碾平」的防护，**到这个切片才真正有东西可防**
（此前无可见 scrollback，是空防护）。改 F17 时不得放松它。

---

## 4. F3 — TOFU 主机密钥持久化

### 4.1 存储：自有 `known_hosts.toml`

不复用 OpenSSH 的 `~/.ssh/known_hosts`。理由：那个格式有 hashed hostname、`@revoked` /
`@cert-authority` 标记、多密钥类型并列等一堆语义，**读得对、写不坏**的成本远超收益，
而我们只需要「host → 指纹」这一张表。用自有 TOML，和 `sessions.toml` 同目录同风格。

代价：与 OpenSSH 的信任库不互通。可接受——本项目是独立客户端，不是 ssh 的前端。

### 4.2 关键约束：`Fingerprint` 是 ssh 的类型，store 不能引用它

`mullion-store` 与 `mullion-ssh` **必须互不依赖**（架构不变量）。所以：

- store 侧只存**文本形式**：`{ host, algo: String, fingerprint: String }`，
  `fingerprint` 是 `SHA256:<base64>`，即 `ssh-keygen -lf` 的输出格式（人可核对）。
- `Fingerprint ⇄ String` 的转换在 **app 层**做（app 是唯一知道两者的地方）。

这是保住依赖方向的代价，也是唯一可接受的做法。若哪天觉得「让 store 引一下 ssh 就好了」——
那是设计错了，停下来问。

store 侧复用 `vault.rs` 既有的 `write_atomic`（tmp + rename），明文 TOML，不加密
（公钥指纹本就是公开信息，加密没有意义，反而妨碍用户手工核对）。

### 4.3 异步边界：`decide` 改 async

弹窗必须在 GUI 线程画，而 `check_server_key` 在 tokio 线程上跑。三种做法里选**握手就地挂起**：

- ❌ 同步 `decide` + 内部 `blocking_recv` 等 oneshot —— **tokio 在 runtime 线程上调
  `blocking_recv` 会 panic**。硬性排除。
- ❌ 先拒绝、弹窗后重连 —— 两次握手，且「重连」路径要复制一遍全部连接参数，易漂移。
- ✅ `decide` 返回 future，握手在 `.await` 处挂起，GUI 那边慢慢弹。`check_server_key`
  **本身已经是 `async fn`**，改动面极小。

trait 已是 `Arc<dyn HostKeyPolicy>`（trait object），**不能**直接写 `async fn`（AFIT 不 dyn-safe），
也不引 `async_trait`（多一个宏依赖不值）。手写返回类型：

```rust
pub trait HostKeyPolicy: Send + Sync {
    fn decide<'a>(&'a self, host: &'a str, fp: &'a Fingerprint)
        -> Pin<Box<dyn Future<Output = HostKeyDecision> + Send + 'a>>;
}
```

`TofuAccept`（测试用）返回 ready future，改动仅一层包装。

### 4.4 流程

```
启动    读 known_hosts.toml → KnownHosts → 注入 PromptingPolicy
握手    check_server_key → policy.decide(host, fp).await
        ├ 已记录且一致 → Accept                         （无弹窗，日常路径）
        ├ 未记录  → UserEvent::HostKeyPrompt{Unknown} → await oneshot
        │           确认 → record + 落盘 + Accept ／ 拒绝 → Reject(Unknown)
        └ 不一致  → UserEvent::HostKeyPrompt{Changed} → 红底告警，新旧指纹并列
                    「服务器重装过，信任新密钥」→ 覆盖 + 落盘 + Accept
                    默认按钮「取消连接」        → Reject(Changed)
```

弹窗必须显示：host、算法、`SHA256:...` 指纹全文（可复制），并提示用户在服务器上跑
`ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub` 核对。变更告警窗额外并列旧指纹，
措辞明确说出「这可能是中间人攻击」，且默认按钮是**取消**。

### 4.5 边界情况

| 情况 | 处理 |
|---|---|
| 落盘失败 | **不阻断连接**：记 `ui.last_error`，本次连得上，下次再问一次 |
| 弹窗期间关窗 | oneshot sender drop → `decide` 收 `Err` → 一律 **Reject**（fail-closed） |
| sshd `LoginGraceTime`（默认 120s） | 弹窗显示倒计时；超时后服务器断开，走正常 `ConnectErr` 路径 |
| CLI 直连（`cli_direct`） | 弹窗照弹（窗口已建好）；拒绝 → `ConnectErr` → `exit(1)`，保住可脚本化语义 |
| `known_hosts.toml` 损坏 | 记 warn，当空表处理（会重新问一次），**不崩、不静默清空原文件** |

新弹窗**必须**加进 `app.rs` 的 `modal` 表达式（`session_manager_open || about_open || editor_open`），
否则它开着时键盘还往终端漏（T8 同源）。

---

## 5. 各 crate 改动一览

| crate | 改动 | 为什么在这 |
|---|---|---|
| `mullion-term` | `Emulator::{with_history, scroll, scroll_to_bottom, mode}`；`snapshot()` 按 `display_offset` 偏移 | VT 状态，零 UI |
| `mullion-term::keymap` | `wheel_action()` 纯决策函数 | 键码 bug 要能无窗口复现 |
| `mullion-store` | 新模块 `known_hosts`：明文 TOML + 复用 `write_atomic` | 唯一碰文件系统的地方 |
| `mullion-ssh` | `HostKeyPolicy::decide` 返回 future；`TofuAccept` 包 ready | 握手边界，不认识文件 |
| `mullion-app` | `PromptingPolicy`、两个 egui 弹窗、滚轮/翻页接线、指纹文本 ⇄ `Fingerprint` | 唯一知道其余四者的地方 |

依赖方向不变：`app → {core, term, ssh, store}`，其余互不依赖。

---

## 6. 测试

**守护测试（新增）**

- `emulator::snapshot_follows_display_offset` —— 滚上去必须看见历史行（F17 主工作面的守护）
- `emulator::alt_screen_has_no_scrollback` —— 守住三档分流的**前提**；这条塌了整个滚轮设计就错了
- `emulator::scrollback_holds_configured_lines` —— 默认 10000 + `with_history` 可配
- `emulator::cursor_hidden_when_scrolled_out_of_viewport`
- `keymap::wheel_local_when_not_alt_screen` / `wheel_reports_when_mouse_mode` /
  `wheel_sends_arrows_when_alternate_scroll` / `wheel_none_when_alternate_scroll_off`
- `keymap::shift_forces_local_scroll_so_user_can_read_history`（T5 同源）
- `known_hosts::{round_trip, atomic_write_survives_partial, corrupt_file_is_treated_as_empty}`
- `session::{policy_accept_completes_handshake, policy_reject_aborts_handshake}`（async policy 两条路径）
- 现有 5 个 TOFU 测试改 async 后仍绿

**回归**：`pty_write_is_collected`（T1）、`snapshot_*`（含 CJK 双宽）、`resize_changes_dims`、
`input_route::*`（T8）全部必须仍绿。「绿」= `cargo test --workspace` 全过**且**
`clippy --workspace --all-targets -- -D warnings` 无输出。基线 148 个测试。

**人工验收清单**（无头验不了，写进 Release notes）

1. tmux 里滚轮 → 自动进 copy-mode，能翻远端历史
2. `less`/`man` 里滚轮 → 内容上下滚（走 ArrowKeys 档）
3. 裸 shell（不进 tmux）里滚轮 → 翻本地 scrollback，输出刷屏时滚动位置不被顶走
4. 任何状态下 `Shift+滚轮` → 都翻本地历史
5. 滚上去后按任意键 → 立即跳回底部
6. 首次连接弹窗的指纹与服务器 `ssh-keygen -lf` 输出**逐字符一致**
7. 确认后重启客户端 → 不再弹
8. 手工改坏 `known_hosts.toml` 里的指纹 → 弹红色变更告警，默认按钮是取消

---

## 7. 交付

按 CLAUDE.md「交付约定」执行：bump patch → 跑绿 → 交叉编译 + objdump 依赖验收 →
发 Release（标题纯 `v0.1.N`）→ 报链接 + sha256 + 上面第 6 节的人工验收清单。
