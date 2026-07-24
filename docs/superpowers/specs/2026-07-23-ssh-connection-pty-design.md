# F1/F3/F6 — SSH 连接 + PTY 收发 设计

> 状态：已定稿（2026-07-23，brainstorming 通过）
> 关联 spec 编号：F1（三种认证）、F3（TOFU 主机校验）、F6（可操作错误）、并触碰 T1/T3/T4/F34。
> 里程碑：v0.1「单 pane 能跑真实 Claude Code 全屏 TUI」的最短路径第一步。

这是让「远端 tmux 里的 Claude Code」在真实链路上跑起来的第一个真实能力：把 mullion-ssh
从骨架变成能连、能认证、能开 PTY、能收发字节的纯 async 库，并在 app 里把字节泵接进
`Pane`/`Emulator`。验证目标：真机 `user@192.0.2.10:22`，私钥 `/path/to/key.pem`。

---

## 1. 范围

**做**
- mullion-ssh 真实 russh 连接。
- F1 三种认证：密码 / 公钥（`/path/to/key.pem`）/ ssh-agent。
- F3 TOFU 主机校验接线（`check_server_key` → `KnownHosts`）。
- F6 可操作错误枚举（每类一个变体，不许统一 "connection failed"）。
- PTY 开通（`request_pty`）与字节双向收发。
- app 侧字节泵：远端字节 → `Emulator::feed`；`Emulator::take_pty_writes` → 回写（T1）。
- 能打真机 `user@192.0.2.10`。

**不做（留下一切片，超出先问）**
- F2 `~/.ssh/config` 解析。
- F4 SOCKS5/HTTP CONNECT 代理、F5 ProxyJump 跳板。
- 断线重连（S3）。
- known_hosts 完整 OpenSSH 磁盘格式（本切片只做极简 load/save）。
- GUI 的 TOFU 首次确认弹窗（本切片用 policy 回调占位，app 后续接真弹窗）。

---

## 2. 架构决策：async 边界（方案 B）

russh 是 async（tokio），winit 事件循环是同步单线程。三者接法选 **方案 B**：

> **app 拥有唯一 tokio 运行时；mullion-ssh 是纯 async 库（零 UI、零运行时所有权）。**

否掉的备选：
- **A（ssh 自带后台 tokio 线程 + 同步句柄）**：库内隐藏运行时是反模式，关闭/panic 传播难，测试要绕线程。
- **C（winit 循环里 block_on）**：高延迟下冻 UI，违背零闪烁/跟手与 N1 空闲 CPU。

方案 B 让 mullion-ssh 成为可 `#[tokio::test]` 直接打真机的纯 async 库，线程与唤醒复杂度
收在 app 这个本来就是集成层的地方。**建议补 `docs/adr-004-async-boundary.md` 记此决策。**

架构不变量守护：
- mullion-ssh **不认识 winit**：唤醒通过注入的 `wake: Arc<dyn Fn()>` 回调，不引 winit 类型。
- mullion-ssh **不弹 UI**：TOFU 决策通过注入的 `policy: Arc<dyn HostKeyPolicy>`。
- mullion-ssh **不认识 pane/窗口**：只吞吐字节 + 连接参数。
- 字节泵编排落在 app（唯一知道 term + ssh 的地方）。

---

## 3. mullion-ssh 模块与公共 API

```
config.rs       SshConfig{ host, port, user, auth, cols, rows, term }   —— 连接参数,非 UI
auth.rs         AuthMethod::{ Password(String), PublicKey{path,passphrase}, Agent }
error.rs        ConnectError（F6,每类一个变体）
session.rs      connect() + SshSession(句柄) + 内部 io task
known_hosts.rs  已有 verify/record + 新增 Fingerprint::from_public_key + 极简 load/save + HostKeyPolicy
pty.rs          PTY 请求参数（term="xterm-256color" + 终端模式）,替换现占位 PtySession
lib.rs          pub mod 导出
```

### 公共入口

```rust
pub async fn connect(
    cfg:    &SshConfig,
    policy: Arc<dyn HostKeyPolicy>,      // TOFU 决策:app 注入弹窗 / 测试注入 accept-and-record
    wake:   Arc<dyn Fn() + Send + Sync>, // app 注入 EventLoopProxy.send_event;ssh 不认识 winit
) -> Result<(SshSession, mpsc::Receiver<Vec<u8>>), ConnectError>;

impl SshSession {
    /// 非阻塞:把字节塞进内部命令 channel(try_send)。winit 线程可直接调,无需 block_on。
    /// 满(Full)/对端已关(Closed)由 app 处理;本切片键鼠写入几乎不触发 Full。
    pub fn write(&self, bytes: Vec<u8>) -> Result<(), TrySendErr>;
    /// reflow 后同步 PTY 尺寸(window_change,F34/T4)。
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), TrySendErr>;
}
```

- 返回值把「远端字节」以 `mpsc::Receiver<Vec<u8>>` 交给 app，app 每帧 drain。
- `write` / `resize` 走内部**有界** `mpsc::Sender<SshCmd>`（`try_send` 非阻塞）。本切片只有键鼠
  小写入，队列几乎不会满；满时的背压/合并策略随粘贴（F18）再定，`write` 现返回 `Full`/`Closed`
  交 app 处理（故句柄返回类型是 `Result<(), TrySendErr>`,非仅 `Closed`）。

### 内部 io task（`channel.split()` 成读写半，单任务 `select!` 收发）

> **务必 split**（russh `Channel::split`，源码 line 445）：`ChannelReadHalf::wait(&mut self)` 与
> `ChannelWriteHalf::data(&self)` 若同在一个未拆分的 `Channel` 上，`select!` 里 `wait()` 持 `&mut`、
> 写命令持 `&`，借用冲突编不过。拆成两个独立绑定后无冲突。
> 开 PTY 顺序：`channel_open_session()` → `request_pty` → `request_shell` →（以上在 `Channel` 上）
> → `split()` → 进 select 循环。

```
let (mut read, write) = channel.split();
loop select! {
    msg = read.wait() => match msg {                          // &mut read
        Some(ChannelMsg::Data{data})              => { inbound_tx.send(data.to_vec()).await?; wake(); }  // CryptoVec→Vec<u8>
        Some(ChannelMsg::Eof|ChannelMsg::Close) | None => break, // → app 侧断连(本切片不重连)
        _ => {}                                               // ExitStatus/WindowAdjusted 等先忽略
    }
    cmd = cmd_rx.recv() => match cmd {                         // write:&self,与 read 不冲突
        Some(SshCmd::Write(b))    => write.data(&b[..]).await?,
        Some(SshCmd::Resize(c,r)) => write.window_change(c as u32, r as u32, 0, 0).await?,
        Some(SshCmd::Close) | None => { write.eof().await.ok(); break }
    }
}
```

`wake()` 让 app 请求重绘（不 per-byte 喂，交给 T3 攒帧/帧率上限决定何时 present）。

---

## 4. 三种认证（F1）

`connect` 流程：
1. **DNS** `tokio::net::lookup_host` —— 失败 → `ConnectError::DnsResolution`。
2. **TCP** `TcpStream::connect` —— `io::ErrorKind::ConnectionRefused` → `ConnectionRefused`；
   其余 io 错 → `Io`。（自己先解析+连接,再交 `client::connect_stream`,以便精确分类 F6。）
3. **握手** 触发 `check_server_key`（见 §5）。
4. **认证** 按 `AuthMethod`：
   - `Password` → `authenticate_password`。
   - `PublicKey{path,passphrase}` → 读 `/path/to/key.pem`（russh keys 加载,支持 passphrase）
     → `authenticate_publickey`。
   - `Agent` → `AgentClient::connect_env` + `request_identities` + `authenticate_publickey_with`。
   - `AuthResult::Failure{..}` → `ConnectError::AuthFailed`。
5. **开 PTY** `channel_open_session()`
   → `request_pty(want_reply=true, term, cols, rows, 0, 0, &[] /* &[(Pty,u32)] 终端模式,骨架先空 */)`
   → `request_shell(true)` → `split()`。开 channel / `request_pty` 失败 → `ConnectError::PtyRequest`。

russh 0.54 已核实签名：`authenticate_password` / `authenticate_publickey` /
`authenticate_publickey_with(Signer)`；`AgentClient::connect_env` / `request_identities`；
`connect_stream(config, stream, handler)`（收已连好的 `TcpStream`）；
`Channel::split() -> (ChannelReadHalf, ChannelWriteHalf)`；
`request_pty(want_reply, term, col, row, pixw, pixh, &[(Pty,u32)])` / `request_shell(want_reply)` /
`window_change(col,row,pixw,pixh)` / `data(impl AsyncRead)` / `ChannelReadHalf::wait() -> Option<ChannelMsg>`。

---

## 5. TOFU 主机校验接线（F3）

`Handler::check_server_key(&mut self, pk: &ssh_key::PublicKey) -> Result<bool>`（russh 默认
`Ok(false)`，安全默认）里：

```
let fp = Fingerprint::from_public_key(pk);   // SHA-256,用 ssh_key 内置 key.fingerprint(Sha256),不引 sha2
match policy.decide(&self.host, &fp) {
    Accept            => Ok(true),
    RecordThenAccept  => { self.known.lock().record(host, fp); Ok(true) }   // TOFU 首次
    Reject(outcome)   => { *self.key_outcome.lock() = Some(outcome); Ok(false) } // russh 中止握手
}
```

> **状态共享**：Handler 被 move 进 `connect_stream` 后外部读不到其字段，故 `key_outcome`
> （与需回写的 `known`）用 `Arc<Mutex<..>>` 在 Handler 与 connect() 之间共享。`check_server_key`
> 返回 `Ok(false)` 令 russh 中止握手 → connect() 再读共享 `key_outcome`，映射成精确错误：
- 已记录但不一致 → `ConnectError::HostKeyChanged{ host, expected, got }`。
- 未记录且 policy 不自动记录 → `ConnectError::HostKeyUnknown{ host, got }`（app 后续接弹窗）。

`HostKeyPolicy` 实现：
- `TofuAccept`：未记录→记录并放行；不一致→拒（冒烟/hermetic 测试/首版默认）。
- 后续 app 弹窗版：未记录→抛 `HostKeyUnknown` 让 UI 决策。

**红线守护**：既有测试 `known_hosts::tests::mismatched_fingerprint_is_rejected`
（`verify` 绝不无条件 true）继续跑,新增指纹变更 → `HostKeyChanged` 的映射测试。

---

## 6. F6 错误枚举（红线：不许统一 "connection failed"）

```rust
pub enum ConnectError {
    DnsResolution(String),                         // 域名解析失败
    ConnectionRefused(String),                     // TCP 拒绝
    AuthFailed,                                    // 认证失败(区别于连接失败)
    HostKeyChanged { host, expected: Fingerprint, got: Fingerprint }, // 疑似 MITM,拦截
    HostKeyUnknown { host, got: Fingerprint },     // 首次连接,需 TOFU 确认
    Io(String),                                    // 其余网络 IO
    PtyRequest,                                    // 开 channel / request_pty 失败
}
```

每条配一句可操作中文（Display）。手写 `Display` + `std::error::Error`，**不引 thiserror**
（保持依赖精简）。

---

## 7. app 接线 —— 字节泵（T1/T3/T4 交汇）

- app 建唯一 `tokio::runtime::Runtime`（多线程）。**新依赖 tokio（方案 B 认可）。**
- 启动 `runtime.block_on(connect(..))` 拿 `(session, rx)`；一次性,可接受阻塞。
- `EventLoop<UserEvent>` 参数化，`wake` 注入 `proxy.send_event(UserEvent::RemoteData)`
  （触碰 `main.rs` 事件循环类型,小范围改动）。
- **每帧**（复用 a07b71d 的攒帧/帧率上限，**T3**）：
  1. drain `rx.try_recv()` → `pane.emulator.feed(&bytes)`；
  2. 喂完 `let out = pane.emulator.take_pty_writes(); if !out.is_empty() { session.write(out) }`
     （**T1 回写,同一 winit 线程内原子完成**）。
- 键鼠输入（既有 `keymap`）→ `session.write()`。
- reflow / 分屏 resize（**T4**）→ `session.resize(cols, rows)` → `window_change`。
- `emulator` 只在 winit 线程碰；io task 只搬原始字节 → 保持单线程仿真。

守护测试：动 `main.rs` 事件循环前后各跑一次 T1(`pty_write_is_collected`)、
T3(`redraw_is_frame_capped`)；resize 路径跑 T4(`reflow_emits_resize`)。

---

## 8. 测试方案

### 反转说明（原选「临时本地 sshd」→ 改「进程内 russh 测试 server」）
当前身份为普通用户 `ubuntu`（uid 1000，非 root），建带密码的一次性系统用户 + 跑 sshd 需
root/PAM，**本环境做不到**；且一等公民 **Windows 11 无本地 sshd/PAM**，本地 sshd 测试在
Windows/CI 跑不起来。故 password（及可 hermetic 化的认证）改用 **russh 的 server 端在进程内
起测试 server**（同一 russh crate，无新依赖、无 root、无 docker，Windows/CI/无头全能跑）。

### 分层
- **hermetic（默认进 `cargo test --workspace`，处处绿）**
  - 进程内 russh 测试 server：覆盖 password + pubkey 认证 → 开 PTY → echo 回读断言。
  - known_hosts：既有 3 测试 + `Fingerprint::from_public_key` + verify/record + 极简 load/save 往返。
  - F6 错误分类纯函数：给定 io/russh 错误 → 断言映射到正确变体（每变体各一例）。
- **live 门控（手动 / 真机，`#[ignore]` + 环境变量 `MULLION_LIVE=1`）**
  - `pubkey_live`：`user@192.0.2.10` + `/path/to/key.pem` → 开 PTY → `echo MULLION_OK` 断言收到。
  - `agent_live`：把 `/path/to/key.pem` 加载进 ssh-agent → `AuthMethod::Agent` → 同上。
- **agent hermetic**：需 agent socket，设为 Unix-only 可选，主要靠 live 覆盖。

「绿」定义（项目 CLAUDE.md）：`cargo test --workspace` 全过 **且** `clippy -D warnings` 无输出。

---

## 9. 新增依赖

- **mullion-app**：`tokio`（workspace，features 已配；方案 B 认可）。
- **mullion-ssh**：无生产新依赖（russh 含 server 端与 keys；`ssh_key` 经 russh 再导出）。
  - dev 可能需 `tempfile` 测 known_hosts load/save —— 可用 std 手写临时路径规避,尽量不加。

---

## 10. 交付物 & 无法自动验证项

**交付**
- §3 模块 + hermetic 测试全绿 + `cargo clippy --workspace -D warnings` 干净。
- 一份可手动跑的 live 验证清单（写进 PR 描述）。
- 建议附 `docs/adr-004-async-boundary.md`。

**你无法替我验证 / 需人工确认（写代码 + 脚手架，标「未验证」，不谎报完成）**
- 真机 pubkey/agent 冒烟：我会在本环境跑一次 live 冒烟（TCP 已确认可达 `192.0.2.10:22`），
  但 **GPU 下真实 Claude Code 全屏 TUI「不闪」只有你的眼睛能判**（v0.1 判定标准是人工目视）。
- 高延迟吞吐 / keepalive 30s / 空闲存活 ≥30min（N8/N9）：需真实高延迟链路人工验证。
- 第三方中文输入法（R2）：与本切片无关，但属同类不可自动验证项。

---

## 11. 提交切分建议（供 writing-plans 展开）

1. `feat(ssh): ConnectError 错误枚举与分类 (F6)` —— 纯枚举 + Display + 分类纯函数 + 单测。
2. `feat(ssh): SshConfig / AuthMethod 与 Fingerprint::from_public_key (F1/F3)`。
3. `feat(ssh): russh connect + 三种认证 + check_server_key 接 TOFU (F1/F3)` —— 含进程内测试 server。
4. `feat(ssh): PTY 开通与 io task 收发 + SshSession 句柄 (F1)`。
5. `feat(app): tokio 运行时 + 字节泵接入 Pane,回写 take_pty_writes (T1/T3/T4)`。
6. `test(ssh): 真机 pubkey/agent live 冒烟(门控) + 手动验证清单`。
7.（可选）`docs: adr-004 async 边界（方案 B）`。
