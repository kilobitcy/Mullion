# F1/F3/F6 SSH 连接 + PTY 收发 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 mullion-ssh 从骨架变成能连、能认证（密码/pubkey/ssh-agent）、能开 PTY、能双向收发字节的纯 async 库，并在 app 里把字节泵接进 `Emulator`，能打真机 `user@192.0.2.10`。

**Architecture:** 方案 B —— app 拥有唯一 tokio 运行时；mullion-ssh 是纯 async 库（零 UI、零运行时所有权，靠注入的 `wake`/`policy` 回调不碰 winit、不弹 UI）。一条 SSH channel 由单个 io task 用 `channel.split()` 的读写半 + `select!` 独占收发；远端字节经有界 mpsc 交给 app，app 每帧 `feed` 进仿真器并把 `take_pty_writes` 回写（T1）。

**Tech Stack:** Rust 2021 · russh 0.54 · tokio · alacritty_terminal（经 mullion-term）· winit（app，脚手架）

设计定稿见 `docs/superpowers/specs/2026-07-23-ssh-connection-pty-design.md`。

---

## 文件结构

**mullion-ssh（生产）**
- `crates/mullion-ssh/src/error.rs` — 新建。`ConnectError`（F6 每类一变体）+ Display/Error + `classify_tcp`。
- `crates/mullion-ssh/src/config.rs` — 新建。`SshConfig` / `AuthMethod`。
- `crates/mullion-ssh/src/known_hosts.rs` — 扩展。`Fingerprint::from_public_key`、`get`、`HostKeyPolicy`/`HostKeyDecision`/`HostKeyOutcome`、`TofuAccept`。
- `crates/mullion-ssh/src/pty.rs` — 重写占位为 `PtyParams`。
- `crates/mullion-ssh/src/session.rs` — 新建。`ClientHandler`(check_server_key)、`establish`、`connect`、`SshSession`、io task。
- `crates/mullion-ssh/src/lib.rs` — 增模块导出。

**mullion-ssh（测试）**
- `crates/mullion-ssh/tests/fixtures/{server_hostkey,client_key,other_key}` — 新建。ssh-keygen 生成的 ed25519 测试密钥。
- `crates/mullion-ssh/tests/common/mod.rs` — 新建。进程内 echo 测试 server。
- `crates/mullion-ssh/tests/auth.rs` — 新建。密码/pubkey/主机密钥变更集成测试。
- `crates/mullion-ssh/tests/pty.rs` — 新建。PTY echo 往返 + resize 集成测试。
- `crates/mullion-ssh/tests/live.rs` — 新建。真机 pubkey/agent 门控冒烟（`#[ignore]`）。

**mullion-app**
- `crates/mullion-app/Cargo.toml` — 加 `tokio` 依赖。
- `crates/mullion-app/src/session_pump.rs` — 新建。`pump` 纯件（T1，可无窗口单测）。
- `crates/mullion-app/src/lib.rs` — 声明 `session_pump`。
- `crates/mullion-app/src/main.rs` — 脚手架接线（tokio 运行时 + UserEvent，**不真跑事件循环，未验证**）。

**docs**
- `docs/adr-004-async-boundary.md` — 可选，记方案 B 决策。

---

## Task 1: 测试密钥 fixtures

生成后续测试用的 ed25519 密钥（不带 passphrase），提交进仓库。仅测试用。

**Files:**
- Create: `crates/mullion-ssh/tests/fixtures/server_hostkey`（+ `.pub`）
- Create: `crates/mullion-ssh/tests/fixtures/client_key`（+ `.pub`）
- Create: `crates/mullion-ssh/tests/fixtures/other_key`（+ `.pub`）

- [ ] **Step 1: 生成三把密钥**

Run:
```bash
cd /data/Mullion/crates/mullion-ssh/tests/fixtures 2>/dev/null || mkdir -p /data/Mullion/crates/mullion-ssh/tests/fixtures
cd /data/Mullion/crates/mullion-ssh/tests/fixtures
ssh-keygen -t ed25519 -N '' -C mullion-test-hostkey -f server_hostkey
ssh-keygen -t ed25519 -N '' -C mullion-test-client  -f client_key
ssh-keygen -t ed25519 -N '' -C mullion-test-other   -f other_key
ls -1
```
Expected: 生成 `server_hostkey`,`server_hostkey.pub`,`client_key`,`client_key.pub`,`other_key`,`other_key.pub`。

- [ ] **Step 2: 提交**

```bash
cd /data/Mullion
git add crates/mullion-ssh/tests/fixtures/
git commit -m "test(ssh): 测试密钥 fixtures(server/client/other,仅测试用)"
```

---

## Task 2: ConnectError 错误枚举与分类（F6）

F6 红线：区分 DNS 失败 / 拒绝连接 / 认证失败 / 主机密钥变更，不许统一 "connection failed"。纯逻辑，先测。

**Files:**
- Create: `crates/mullion-ssh/src/error.rs`
- Modify: `crates/mullion-ssh/src/lib.rs`

- [ ] **Step 1: 写失败测试**

创建 `crates/mullion-ssh/src/error.rs`：

```rust
//! F6:连接失败给可操作错误。每类一个变体,红线是不许统一 "connection failed"。

use std::fmt;

use crate::known_hosts::Fingerprint;

/// 连接期的可操作错误(F6)。每个变体对应一类可区分的失败。
#[derive(Debug)]
pub enum ConnectError {
    /// 域名解析失败(区别于「解析成功但连不上」)。
    DnsResolution(String),
    /// TCP 连接被拒绝(对端无监听 / 防火墙 RST)。
    ConnectionRefused(String),
    /// 认证失败(凭据不对,区别于连接失败)。
    AuthFailed,
    /// 主机密钥变更 —— 疑似 MITM,已拦截(F3)。
    HostKeyChanged {
        host: String,
        expected: Fingerprint,
        got: Fingerprint,
    },
    /// 首次连接此主机,指纹未记录,需 TOFU 确认(F3)。
    HostKeyUnknown { host: String, got: Fingerprint },
    /// 其余网络 IO 错误。
    Io(String),
    /// 开 channel / request_pty 失败。
    PtyRequest,
}

/// 把 TCP 连接阶段的 io 错误分类到精确变体(F6)。
pub fn classify_tcp(e: std::io::Error) -> ConnectError {
    match e.kind() {
        std::io::ErrorKind::ConnectionRefused => ConnectError::ConnectionRefused(e.to_string()),
        _ => ConnectError::Io(e.to_string()),
    }
}

fn hex(fp: &Fingerprint) -> String {
    fp.0.iter().map(|b| format!("{b:02x}")).collect()
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectError::DnsResolution(h) => write!(f, "域名解析失败:{h} —— 检查主机名/DNS"),
            ConnectError::ConnectionRefused(a) => write!(f, "连接被拒绝:{a} —— 检查端口/sshd 是否在跑"),
            ConnectError::AuthFailed => write!(f, "认证失败 —— 检查用户名/密钥/密码"),
            ConnectError::HostKeyChanged { host, expected, got } => write!(
                f,
                "主机 {host} 的密钥已变更(疑似中间人,已拦截):记录 {} → 收到 {}",
                hex(expected),
                hex(got)
            ),
            ConnectError::HostKeyUnknown { host, got } => {
                write!(f, "首次连接 {host},指纹 {} 未记录,需确认(TOFU)", hex(got))
            }
            ConnectError::Io(e) => write!(f, "网络 IO 错误:{e}"),
            ConnectError::PtyRequest => write!(f, "开 PTY 失败 —— 对端可能不允许 PTY"),
        }
    }
}

impl std::error::Error for ConnectError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refused_is_distinct_from_generic_io() {
        let refused = classify_tcp(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
        assert!(matches!(refused, ConnectError::ConnectionRefused(_)));
        let other = classify_tcp(std::io::Error::from(std::io::ErrorKind::TimedOut));
        assert!(matches!(other, ConnectError::Io(_)), "非 refused 应落 Io,不得混为一类");
    }

    #[test]
    fn every_variant_has_distinct_actionable_message() {
        // F6 红线:每类错误消息互不相同且非空,不许统一 "connection failed"。
        let variants = [
            ConnectError::DnsResolution("h".into()),
            ConnectError::ConnectionRefused("1.2.3.4:22".into()),
            ConnectError::AuthFailed,
            ConnectError::HostKeyChanged {
                host: "h".into(),
                expected: Fingerprint(vec![1]),
                got: Fingerprint(vec![2]),
            },
            ConnectError::HostKeyUnknown { host: "h".into(), got: Fingerprint(vec![3]) },
            ConnectError::Io("io".into()),
            ConnectError::PtyRequest,
        ];
        let msgs: Vec<String> = variants.iter().map(|e| e.to_string()).collect();
        for m in &msgs {
            assert!(!m.is_empty());
        }
        let mut uniq = msgs.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), msgs.len(), "错误消息必须两两不同(F6)");
    }
}
```

- [ ] **Step 2: 声明模块**

修改 `crates/mullion-ssh/src/lib.rs`，在 `pub mod known_hosts;` 上方加一行：

```rust
pub mod error;
```

- [ ] **Step 3: 跑测试确认失败→通过**

Run: `cargo test -p mullion-ssh error::`
Expected: 先因未声明模块编译错;补齐后 2 个测试 PASS。

- [ ] **Step 4: 提交**

```bash
git add crates/mullion-ssh/src/error.rs crates/mullion-ssh/src/lib.rs
git commit -m "feat(ssh): ConnectError 错误枚举与 TCP 分类 (F6)"
```

---

## Task 3: SshConfig / AuthMethod / PtyParams（F1）

连接参数与认证方式。纯数据结构。`pty.rs` 的占位 `PtySession` 无人使用（仅 pane.rs 注释提及），重写为 `PtyParams`。

**Files:**
- Create: `crates/mullion-ssh/src/config.rs`
- Rewrite: `crates/mullion-ssh/src/pty.rs`
- Modify: `crates/mullion-ssh/src/lib.rs`

- [ ] **Step 1: 写 config.rs**

```rust
//! 连接参数与认证方式(F1)。只是数据,不含 UI/pane 概念。

use std::path::PathBuf;

/// F1 三种认证。
#[derive(Debug, Clone)]
pub enum AuthMethod {
    /// 密码认证。
    Password(String),
    /// 公钥认证:本地私钥文件(如 /path/to/key.pem)+ 可选 passphrase。
    PublicKey {
        path: PathBuf,
        passphrase: Option<String>,
    },
    /// ssh-agent 认证(从 SSH_AUTH_SOCK 取身份)。
    Agent,
}

/// 一次连接所需的全部参数。app 构造后交给 `session::connect`。
#[derive(Debug, Clone)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: AuthMethod,
    /// 初始 PTY 尺寸;reflow 后由 `SshSession::resize` 同步(F34)。
    pub cols: u16,
    pub rows: u16,
    /// TERM 名,固定 "xterm-256color"。
    pub term: String,
}
```

- [ ] **Step 2: 重写 pty.rs**

把 `crates/mullion-ssh/src/pty.rs` 整个替换为：

```rust
//! PTY 请求参数(F1)。

use russh::Pty;

/// `request_pty` 的参数集。终端模式骨架先留空(用默认)。
#[derive(Debug, Clone)]
pub struct PtyParams {
    pub term: String,
    pub cols: u16,
    pub rows: u16,
    pub modes: Vec<(Pty, u32)>,
}

impl PtyParams {
    pub fn new(term: impl Into<String>, cols: u16, rows: u16) -> Self {
        Self {
            term: term.into(),
            cols,
            rows,
            modes: Vec::new(),
        }
    }
}
```

- [ ] **Step 3: 声明模块**

`crates/mullion-ssh/src/lib.rs` 增：

```rust
pub mod config;
```

（`pub mod pty;` 已存在，保留。）

- [ ] **Step 4: 编译**

Run: `cargo build -p mullion-ssh`
Expected: 通过（`russh::Pty` 存在，无编译错）。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-ssh/src/config.rs crates/mullion-ssh/src/pty.rs crates/mullion-ssh/src/lib.rs
git commit -m "feat(ssh): SshConfig/AuthMethod 与 PtyParams (F1)"
```

---

## Task 4: known_hosts 扩展 —— 指纹与 TOFU 策略（F3）

在既有 `KnownHosts`（verify/record + 3 个守护测试）之上，加：从公钥算指纹、peek、以及 `HostKeyPolicy` 决策接口 + `TofuAccept` 实现。

**Files:**
- Modify: `crates/mullion-ssh/src/known_hosts.rs`

- [ ] **Step 1: 写失败测试（加到文件末尾 `tests` mod 内）**

在 `crates/mullion-ssh/src/known_hosts.rs` 的 `mod tests` 中追加：

```rust
    #[test]
    fn fingerprint_from_key_is_deterministic_and_distinguishes_keys() {
        // 同一把私钥的公钥 → 指纹稳定;不同私钥 → 指纹不同。
        let k1 = russh::keys::load_secret_key("tests/fixtures/client_key", None).unwrap();
        let k2 = russh::keys::load_secret_key("tests/fixtures/other_key", None).unwrap();
        let fp1a = Fingerprint::from_public_key(&k1.public_key().clone());
        let fp1b = Fingerprint::from_public_key(&k1.public_key().clone());
        let fp2 = Fingerprint::from_public_key(&k2.public_key().clone());
        assert_eq!(fp1a, fp1b, "同一公钥指纹必须稳定");
        assert_ne!(fp1a, fp2, "不同公钥指纹必须不同");
        assert_eq!(fp1a.0.len(), 32, "SHA-256 应为 32 字节");
    }

    #[test]
    fn tofu_records_unknown_then_accepts_same_rejects_changed() {
        // F3:未知主机首次记录并放行;同指纹再来放行;指纹变更 → Reject(Changed)。
        let known = std::sync::Arc::new(std::sync::Mutex::new(KnownHosts::new()));
        let policy = TofuAccept::new(known);
        let a = fp(b"AAAA");
        let b = fp(b"BBBB");
        assert!(matches!(policy.decide("h", &a), HostKeyDecision::Accept), "首次应记录并放行");
        assert!(matches!(policy.decide("h", &a), HostKeyDecision::Accept), "同指纹应放行");
        match policy.decide("h", &b) {
            HostKeyDecision::Reject(HostKeyOutcome::Changed { expected, got, .. }) => {
                assert_eq!(expected, a);
                assert_eq!(got, b);
            }
            _ => panic!("指纹变更必须 Reject(Changed)(F3 红线)"),
        }
    }
```

- [ ] **Step 2: 加实现（在文件里 `KnownHosts` impl 与 `#[cfg(test)]` 之间插入）**

先给现有 `Fingerprint` 加 `from_public_key`，给 `KnownHosts` 加 `get`，再加策略类型：

```rust
impl Fingerprint {
    /// 从 SSH 公钥算 SHA-256 指纹(用 ssh_key 内置,不引 sha2)。
    pub fn from_public_key(key: &russh::keys::ssh_key::PublicKey) -> Self {
        let f = key.fingerprint(russh::keys::ssh_key::HashAlg::Sha256);
        Fingerprint(f.as_bytes().to_vec())
    }
}

impl KnownHosts {
    /// 查已记录的指纹(不改状态)。
    pub fn get(&self, host: &str) -> Option<&Fingerprint> {
        self.entries.get(host)
    }
}

/// 主机密钥被拒时的精确原因,供 connect 映射成 ConnectError。
#[derive(Debug, Clone)]
pub enum HostKeyOutcome {
    /// 记录过但指纹不一致(疑似 MITM)。
    Changed {
        host: String,
        expected: Fingerprint,
        got: Fingerprint,
    },
    /// 从未记录,策略不自动信任。
    Unknown { host: String, got: Fingerprint },
}

/// check_server_key 的决策结果。
#[derive(Debug)]
pub enum HostKeyDecision {
    Accept,
    Reject(HostKeyOutcome),
}

/// 主机密钥策略。ssh 不弹 UI —— app 注入实现(弹窗版),测试/首版注入 TofuAccept。
pub trait HostKeyPolicy: Send + Sync {
    fn decide(&self, host: &str, fp: &Fingerprint) -> HostKeyDecision;
}

/// TOFU 策略:未记录→记录并放行;一致→放行;不一致→拒(Changed)。
/// 冒烟/hermetic 测试/首版默认用它。app 弹窗版另做,未知时返回 Reject(Unknown)。
pub struct TofuAccept {
    known: std::sync::Arc<std::sync::Mutex<KnownHosts>>,
}

impl TofuAccept {
    pub fn new(known: std::sync::Arc<std::sync::Mutex<KnownHosts>>) -> Self {
        Self { known }
    }
}

impl HostKeyPolicy for TofuAccept {
    fn decide(&self, host: &str, fp: &Fingerprint) -> HostKeyDecision {
        let mut kh = self.known.lock().expect("known-hosts poisoned");
        match kh.get(host).cloned() {
            None => {
                kh.record(host, fp.clone());
                HostKeyDecision::Accept
            }
            Some(known) if &known == fp => HostKeyDecision::Accept,
            Some(known) => HostKeyDecision::Reject(HostKeyOutcome::Changed {
                host: host.to_owned(),
                expected: known,
                got: fp.clone(),
            }),
        }
    }
}
```

- [ ] **Step 3: 跑测试**

Run: `cargo test -p mullion-ssh known_hosts::`
Expected: 既有 3 个 + 新增 2 个共 5 个 PASS。（fingerprint 测试需在 crate 根跑，fixture 路径相对 `crates/mullion-ssh/`；cargo test 的工作目录即该 crate 根，路径成立。）

- [ ] **Step 4: 提交**

```bash
git add crates/mullion-ssh/src/known_hosts.rs
git commit -m "feat(ssh): 指纹与 TOFU 主机密钥策略 (F3)"
```

---

## Task 5: 进程内 echo 测试 server（测试支撑）

用 russh 的 server 端在 `127.0.0.1:0` 起一个测试 server：接受 `testuser` 的密码/pubkey，开 PTY，收到什么数据就回显什么。hermetic、无 root、无 docker，Windows/CI/无头全能跑。

**Files:**
- Create: `crates/mullion-ssh/tests/common/mod.rs`
- Create: `crates/mullion-ssh/tests/smoke_server.rs`（只为验证 server 自身可连，用 russh 原生 client）

- [ ] **Step 1: 写测试 server**

创建 `crates/mullion-ssh/tests/common/mod.rs`：

```rust
//! 进程内 echo 测试 server。收到数据原样回显,供客户端断言往返。
//! 注:server Handler 的方法签名以本机 russh 0.54 源码为准,如编译报签名不符按提示微调。

use std::sync::Arc;

use russh::keys::PublicKey;
use russh::server::{Auth, Handler, Msg, Session};
use russh::{Channel, ChannelId, CryptoVec};

pub const TEST_USER: &str = "testuser";
pub const TEST_PASSWORD: &str = "test-password";

pub struct EchoHandler;

impl Handler for EchoHandler {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        if user == TEST_USER && password == TEST_PASSWORD {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn auth_publickey(&mut self, user: &str, _key: &PublicKey) -> Result<Auth, Self::Error> {
        // 测试 server 接受任意 pubkey(客户端持 client_key);真机由 authorized_keys 把关。
        if user == TEST_USER {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _col: u32,
        _row: u32,
        _pw: u32,
        _ph: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        // 回显收到的字节,供客户端断言往返。
        session.data(channel, CryptoVec::from(data.to_vec()))?;
        Ok(())
    }
}

/// 在 127.0.0.1:0 起 echo server,返回实际监听地址。随进程/运行时结束回收。
pub async fn spawn_echo_server() -> std::net::SocketAddr {
    let host_key =
        russh::keys::load_secret_key("tests/fixtures/server_hostkey", None).expect("load hostkey");
    let mut config = russh::server::Config::default();
    config.keys.push(host_key);
    let config = Arc::new(config);

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind test server");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            let config = config.clone();
            tokio::spawn(async move {
                let _ = russh::server::run_stream(config, stream, EchoHandler).await;
            });
        }
    });
    addr
}
```

- [ ] **Step 2: 写一个用 russh 原生 client 的 smoke，确认 server 能连能认证**

创建 `crates/mullion-ssh/tests/smoke_server.rs`：

```rust
//! 只验证测试 server 自身:用 russh 原生 client 连上并密码认证成功。
//! (不依赖我们尚未写的 session::connect,避免循环依赖。)

mod common;

use std::sync::Arc;

struct AcceptAll;
impl russh::client::Handler for AcceptAll {
    type Error = russh::Error;
    async fn check_server_key(
        &mut self,
        _key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_server_accepts_password() {
    let addr = common::spawn_echo_server().await;
    let config = Arc::new(russh::client::Config::default());
    let mut handle = russh::client::connect(config, (addr.ip(), addr.port()), AcceptAll)
        .await
        .expect("connect");
    let res = handle
        .authenticate_password(common::TEST_USER, common::TEST_PASSWORD)
        .await
        .expect("auth call");
    assert!(matches!(res, russh::client::AuthResult::Success), "密码认证应成功");
}
```

- [ ] **Step 3: 跑 smoke（此步会暴露 server Handler 签名的任何不符，按编译器提示微调 common/mod.rs）**

Run: `cargo test -p mullion-ssh --test smoke_server -- --nocapture`
Expected: `test_server_accepts_password` PASS。若报 `channel_success` / `pty_request` 参数不符，按 `~/.cargo/registry/src/**/russh-0.54.5/src/server/mod.rs` 的实际签名调整（这是 API 漂移纪律要求的「看错误的实际签名再改」）。

- [ ] **Step 4: 提交**

```bash
git add crates/mullion-ssh/tests/common crates/mullion-ssh/tests/smoke_server.rs
git commit -m "test(ssh): 进程内 echo 测试 server + 自连 smoke"
```

---

## Task 6: establish() —— 连接 + 认证 + 主机校验（F1/F3/F6）

自管 DNS+TCP（精确分类 F6），`connect_stream` 握手触发 `check_server_key`（接 TOFU），再按 `AuthMethod` 认证。返回存活的 `Handle`（PTY 在 Task 7 接）。

**Files:**
- Create: `crates/mullion-ssh/src/session.rs`
- Modify: `crates/mullion-ssh/src/lib.rs`
- Create: `crates/mullion-ssh/tests/auth.rs`

- [ ] **Step 1: 写集成测试**

创建 `crates/mullion-ssh/tests/auth.rs`：

```rust
mod common;

use std::sync::{Arc, Mutex};

use mullion_ssh::config::{AuthMethod, SshConfig};
use mullion_ssh::error::ConnectError;
use mullion_ssh::known_hosts::{Fingerprint, KnownHosts, TofuAccept};
use mullion_ssh::session::establish;

fn cfg(addr: std::net::SocketAddr, auth: AuthMethod) -> SshConfig {
    SshConfig {
        host: addr.ip().to_string(),
        port: addr.port(),
        user: common::TEST_USER.to_string(),
        auth,
        cols: 80,
        rows: 24,
        term: "xterm-256color".to_string(),
    }
}

fn tofu() -> Arc<TofuAccept> {
    Arc::new(TofuAccept::new(Arc::new(Mutex::new(KnownHosts::new()))))
}

#[tokio::test(flavor = "multi_thread")]
async fn password_auth_succeeds() {
    let addr = common::spawn_echo_server().await;
    let c = cfg(addr, AuthMethod::Password(common::TEST_PASSWORD.into()));
    assert!(establish(&c, tofu()).await.is_ok(), "正确密码应连上");
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_password_is_auth_failed() {
    let addr = common::spawn_echo_server().await;
    let c = cfg(addr, AuthMethod::Password("nope".into()));
    match establish(&c, tofu()).await {
        Err(ConnectError::AuthFailed) => {}
        other => panic!("错密码应 AuthFailed,实际 {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pubkey_auth_succeeds() {
    let addr = common::spawn_echo_server().await;
    let c = cfg(
        addr,
        AuthMethod::PublicKey {
            path: "tests/fixtures/client_key".into(),
            passphrase: None,
        },
    );
    assert!(establish(&c, tofu()).await.is_ok(), "pubkey 应连上");
}

#[tokio::test(flavor = "multi_thread")]
async fn changed_host_key_is_rejected() {
    // F3:预置一个不同指纹 → 连接必须 HostKeyChanged。
    let addr = common::spawn_echo_server().await;
    let known = Arc::new(Mutex::new(KnownHosts::new()));
    known
        .lock()
        .unwrap()
        .record(&addr.ip().to_string(), Fingerprint(vec![0xde, 0xad]));
    let policy = Arc::new(TofuAccept::new(known));
    let c = cfg(addr, AuthMethod::Password(common::TEST_PASSWORD.into()));
    match establish(&c, policy).await {
        Err(ConnectError::HostKeyChanged { .. }) => {}
        other => panic!("主机密钥变更应 HostKeyChanged,实际 {other:?}"),
    }
}
```

- [ ] **Step 2: 写 session.rs 的连接+认证部分**

创建 `crates/mullion-ssh/src/session.rs`：

```rust
//! russh 连接 + 认证 + 主机校验 + PTY io task。
//! ssh 只认字节流:不认识 pane/窗口,不碰 winit(唤醒经注入的 wake 回调)。

use std::sync::{Arc, Mutex};

use russh::client::{self, AuthResult, Handle};
use russh::keys::ssh_key;
use tokio::net::TcpStream;

use crate::config::{AuthMethod, SshConfig};
use crate::error::{classify_tcp, ConnectError};
use crate::known_hosts::{Fingerprint, HostKeyDecision, HostKeyOutcome, HostKeyPolicy};

/// russh 客户端 Handler:只负责主机密钥校验(TOFU,F3)。
/// 被拒原因经共享 `outcome` 回传给 establish(Handler 会被 move 进 connect_stream)。
///
/// 必须 `pub`:`establish`/`connect` 是 pub 且签名里出现 `Handle<ClientHandler>`,
/// 私有类型出现在公开接口会 E0446。字段保持私有 → 对外不透明,无 pub 构造。
pub struct ClientHandler {
    host: String,
    policy: Arc<dyn HostKeyPolicy>,
    outcome: Arc<Mutex<Option<HostKeyOutcome>>>,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &ssh_key::PublicKey) -> Result<bool, Self::Error> {
        let fp = Fingerprint::from_public_key(key);
        match self.policy.decide(&self.host, &fp) {
            HostKeyDecision::Accept => Ok(true),
            HostKeyDecision::Reject(o) => {
                *self.outcome.lock().expect("outcome poisoned") = Some(o);
                Ok(false) // russh 据此中止握手
            }
        }
    }
}

/// 其余 russh 传输错误 → Io(兜底)。认证被拒走 AuthResult::Failure,不经此。
fn map_russh(e: russh::Error) -> ConnectError {
    ConnectError::Io(e.to_string())
}

/// 若握手期记录了主机密钥被拒原因,产出精确错误;否则按传输错误兜底。
fn host_key_or(outcome: &Arc<Mutex<Option<HostKeyOutcome>>>, e: russh::Error) -> ConnectError {
    match outcome.lock().expect("outcome poisoned").take() {
        Some(HostKeyOutcome::Changed { host, expected, got }) => {
            ConnectError::HostKeyChanged { host, expected, got }
        }
        Some(HostKeyOutcome::Unknown { host, got }) => ConnectError::HostKeyUnknown { host, got },
        None => map_russh(e),
    }
}

/// 连接 + 认证 + 主机校验。成功返回存活的 russh Handle(PTY 由 connect 接)。
pub async fn establish(
    cfg: &SshConfig,
    policy: Arc<dyn HostKeyPolicy>,
) -> Result<Handle<ClientHandler>, ConnectError> {
    // 1) DNS:此步任何失败都归 DnsResolution。
    let mut addrs = tokio::net::lookup_host((cfg.host.as_str(), cfg.port))
        .await
        .map_err(|e| ConnectError::DnsResolution(e.to_string()))?;
    let addr = addrs
        .next()
        .ok_or_else(|| ConnectError::DnsResolution(format!("{} 无解析结果", cfg.host)))?;

    // 2) TCP:分类 refused / 其他 io(F6)。
    let stream = TcpStream::connect(addr).await.map_err(classify_tcp)?;

    // 3) 握手:触发 check_server_key(TOFU)。
    let outcome = Arc::new(Mutex::new(None));
    let handler = ClientHandler {
        host: cfg.host.clone(),
        policy,
        outcome: outcome.clone(),
    };
    let config = Arc::new(client::Config::default());
    let mut handle = client::connect_stream(config, stream, handler)
        .await
        .map_err(|e| host_key_or(&outcome, e))?;

    // 4) 认证。
    let result = authenticate(&mut handle, cfg).await?;
    match result {
        AuthResult::Success => Ok(handle),
        AuthResult::Failure { .. } => Err(ConnectError::AuthFailed),
    }
}

async fn authenticate(
    handle: &mut Handle<ClientHandler>,
    cfg: &SshConfig,
) -> Result<AuthResult, ConnectError> {
    match &cfg.auth {
        AuthMethod::Password(pw) => handle
            .authenticate_password(&cfg.user, pw)
            .await
            .map_err(map_russh),
        AuthMethod::PublicKey { path, passphrase } => {
            let key = russh::keys::load_secret_key(path, passphrase.as_deref())
                .map_err(|e| ConnectError::Io(format!("读私钥失败: {e}")))?;
            let with = russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), None);
            handle
                .authenticate_publickey(&cfg.user, with)
                .await
                .map_err(map_russh)
        }
        AuthMethod::Agent => authenticate_agent(handle, &cfg.user).await,
    }
}

/// ssh-agent 认证:从 SSH_AUTH_SOCK 取身份逐个尝试。
/// 仅 live 测试覆盖(Task 9);exact Signer 接法参见 russh 示例 client_exec_interactive。
async fn authenticate_agent(
    handle: &mut Handle<ClientHandler>,
    user: &str,
) -> Result<AuthResult, ConnectError> {
    let mut agent = russh::keys::agent::client::AgentClient::connect_env()
        .await
        .map_err(|e| ConnectError::Io(format!("连 ssh-agent 失败: {e}")))?;
    let identities = agent
        .request_identities()
        .await
        .map_err(|e| ConnectError::Io(format!("取 agent 身份失败: {e}")))?;
    // 逐个身份试;成功即返回,否则记住最后一次结果交上层映射为 AuthFailed。
    // 不手工构造 AuthResult::Failure(字段名/MethodSet 易随版本漂),避开脆弱点。
    let mut last: Option<AuthResult> = None;
    for id in identities {
        let res = handle
            .authenticate_publickey_with(user, id, None, &mut agent)
            .await
            .map_err(map_russh)?;
        if matches!(res, AuthResult::Success) {
            return Ok(res);
        }
        last = Some(res);
    }
    last.ok_or(ConnectError::AuthFailed) // 无任何身份 → 直接 AuthFailed
}
```

> **impl 提示**：`authenticate_publickey_with(user, key, hash_alg, signer)` 的确切参数以本机
> russh 0.54 源码为准（`AgentClient` 是否直接作 `Signer`、`hash_alg` 类型）。agent 只靠 Task 9
> live 验证，hermetic 不覆盖 —— 但此函数仍须**编译通过**（否则整 crate 编不过、所有测试挂）；
> 若签名不符，照编译器提示与 russh 示例 `client_exec_interactive` 的 agent 分支修正。

- [ ] **Step 3: 声明模块**

`crates/mullion-ssh/src/lib.rs` 增：

```rust
pub mod session;
```

- [ ] **Step 4: 跑测试**

Run: `cargo test -p mullion-ssh --test auth -- --nocapture`
Expected: `password_auth_succeeds` / `wrong_password_is_auth_failed` / `pubkey_auth_succeeds` / `changed_host_key_is_rejected` 全 PASS。若 agent 相关代码编译报签名不符，按源码修正 `authenticate_agent`（不影响本 test 文件，agent 走 live）。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-ssh/src/session.rs crates/mullion-ssh/src/lib.rs crates/mullion-ssh/tests/auth.rs
git commit -m "feat(ssh): connect_stream 连接+三种认证+check_server_key 接 TOFU (F1/F3/F6)"
```

---

## Task 7: PTY 开通 + io task + SshSession（F1/F34）

`connect` = establish + 开 session channel + request_pty + request_shell + `split()` + spawn io task。`SshSession` 暴露非阻塞 `write`/`resize`；远端字节经 `mpsc::Receiver<Vec<u8>>` 交 app。

**Files:**
- Modify: `crates/mullion-ssh/src/session.rs`
- Create: `crates/mullion-ssh/tests/pty.rs`

- [ ] **Step 1: 写集成测试**

创建 `crates/mullion-ssh/tests/pty.rs`：

```rust
mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use mullion_ssh::config::{AuthMethod, SshConfig};
use mullion_ssh::known_hosts::{KnownHosts, TofuAccept};
use mullion_ssh::session::connect;

fn cfg(addr: std::net::SocketAddr) -> SshConfig {
    SshConfig {
        host: addr.ip().to_string(),
        port: addr.port(),
        user: common::TEST_USER.to_string(),
        auth: AuthMethod::Password(common::TEST_PASSWORD.into()),
        cols: 80,
        rows: 24,
        term: "xterm-256color".to_string(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pty_echo_roundtrip() {
    let addr = common::spawn_echo_server().await;
    let policy = Arc::new(TofuAccept::new(Arc::new(Mutex::new(KnownHosts::new()))));
    let wake = Arc::new(|| {});
    let (session, mut rx) = connect(&cfg(addr), policy, wake).await.expect("connect");

    session.write(b"ping".to_vec()).expect("write");
    let got = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("未超时")
        .expect("收到回显");
    assert_eq!(&got, b"ping", "PTY 往返:发 ping 应收 ping");
}

#[tokio::test(flavor = "multi_thread")]
async fn resize_does_not_error() {
    // F34:resize 命令应被 io task 接受(echo server 不回内容,只验不报错/不 panic)。
    let addr = common::spawn_echo_server().await;
    let policy = Arc::new(TofuAccept::new(Arc::new(Mutex::new(KnownHosts::new()))));
    let (session, _rx) = connect(&cfg(addr), policy, Arc::new(|| {}))
        .await
        .expect("connect");
    session.resize(100, 40).expect("resize 应入队成功");
    tokio::time::sleep(Duration::from_millis(50)).await; // 让 io task 处理
}
```

- [ ] **Step 2: 在 session.rs 追加 connect + SshSession + io task**

在 `crates/mullion-ssh/src/session.rs` 末尾（`authenticate_agent` 之后、`#[cfg(test)]` 之前如有）追加：

```rust
use tokio::sync::mpsc;

/// 交给 io task 的命令。
enum SshCmd {
    Write(Vec<u8>),
    Resize(u16, u16),
}

/// 句柄非阻塞发命令的失败原因。
#[derive(Debug)]
pub enum TrySendErr {
    /// 出站队列已满(粘贴大段 + 慢链路;本切片键鼠几乎不触发)。
    Full,
    /// 对端已关闭。
    Closed,
}

fn map_try_send<T>(e: mpsc::error::TrySendError<T>) -> TrySendErr {
    match e {
        mpsc::error::TrySendError::Full(_) => TrySendErr::Full,
        mpsc::error::TrySendError::Closed(_) => TrySendErr::Closed,
    }
}

/// 一条存活的 SSH PTY 会话句柄。winit 线程可直接调 write/resize(非阻塞)。
pub struct SshSession {
    cmd_tx: mpsc::Sender<SshCmd>,
}

impl SshSession {
    /// 把字节回写对端(键鼠编码 / take_pty_writes 的回写,T1)。非阻塞。
    pub fn write(&self, bytes: Vec<u8>) -> Result<(), TrySendErr> {
        self.cmd_tx.try_send(SshCmd::Write(bytes)).map_err(map_try_send)
    }

    /// reflow 后同步 PTY 尺寸(window_change,F34/T4)。非阻塞。
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), TrySendErr> {
        self.cmd_tx
            .try_send(SshCmd::Resize(cols, rows))
            .map_err(map_try_send)
    }
}

/// 连接 + 认证 + 开 PTY + 起 io task。返回句柄与「远端字节」接收端(app 每帧 drain)。
/// `wake` 由 app 注入(EventLoopProxy.send_event);ssh 不认识 winit。
pub async fn connect(
    cfg: &SshConfig,
    policy: Arc<dyn HostKeyPolicy>,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<(SshSession, mpsc::Receiver<Vec<u8>>), ConnectError> {
    let handle = establish(cfg, policy).await?;

    let channel = handle
        .channel_open_session()
        .await
        .map_err(|_| ConnectError::PtyRequest)?;
    channel
        .request_pty(true, &cfg.term, cfg.cols as u32, cfg.rows as u32, 0, 0, &[])
        .await
        .map_err(|_| ConnectError::PtyRequest)?;
    channel
        .request_shell(true)
        .await
        .map_err(|_| ConnectError::PtyRequest)?;

    // 拆读写半:read.wait()(&mut) 与 write.data()(&) 同任务 select! 不冲突。
    let (read, write) = channel.split();

    let (inbound_tx, inbound_rx) = mpsc::channel::<Vec<u8>>(256);
    let (cmd_tx, cmd_rx) = mpsc::channel::<SshCmd>(256);

    tokio::spawn(io_task(read, write, cmd_rx, inbound_tx, wake, handle));

    Ok((SshSession { cmd_tx }, inbound_rx))
}

async fn io_task(
    mut read: russh::ChannelReadHalf,
    write: russh::ChannelWriteHalf<client::Msg>,
    mut cmd_rx: mpsc::Receiver<SshCmd>,
    inbound_tx: mpsc::Sender<Vec<u8>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    _handle: Handle<ClientHandler>, // 持有以保活连接;drop 即断连
) {
    loop {
        tokio::select! {
            msg = read.wait() => match msg {
                Some(russh::ChannelMsg::Data { data }) => {
                    if inbound_tx.send(data.to_vec()).await.is_err() {
                        break; // app 侧接收端已丢
                    }
                    wake();
                }
                Some(russh::ChannelMsg::Eof) | Some(russh::ChannelMsg::Close) | None => break,
                _ => {} // ExitStatus / WindowAdjusted 等骨架先忽略
            },
            cmd = cmd_rx.recv() => match cmd {
                Some(SshCmd::Write(b)) => {
                    let _ = write.data(&b[..]).await;
                }
                Some(SshCmd::Resize(c, r)) => {
                    let _ = write.window_change(c as u32, r as u32, 0, 0).await;
                }
                None => {
                    let _ = write.eof().await;
                    break; // 所有句柄已 drop
                }
            },
        }
    }
}
```

> **impl 提示**：`russh::ChannelReadHalf` / `russh::ChannelWriteHalf` / `russh::ChannelMsg` /
> `russh::client::Msg` 的确切导出路径以源码为准（可能在 `russh::` 顶层或 `russh::channels::`）；
> 编译器会指明。`data.to_vec()`：`ChannelMsg::Data.data` 是 `CryptoVec`，`to_vec()` 得 `Vec<u8>`。

- [ ] **Step 3: 跑测试（含既有全部，确保没破坏 Task 6）**

Run: `cargo test -p mullion-ssh -- --nocapture`
Expected: 单测 + smoke + auth + pty 全 PASS（`pty_echo_roundtrip`、`resize_does_not_error` 新增 PASS）。

- [ ] **Step 4: clippy**

Run: `cargo clippy -p mullion-ssh --all-targets -- -D warnings`
Expected: 无输出。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-ssh/src/session.rs crates/mullion-ssh/tests/pty.rs
git commit -m "feat(ssh): PTY 开通 + io task 收发 + SshSession 句柄 (F1/F34)"
```

---

## Task 8: app —— tokio 依赖 + 字节泵纯件（T1）

app 接线里唯一能无头验证的是「泵」逻辑：喂远端字节 → `take_pty_writes` 回写（T1）。抽成纯件单测。winit/GPU 事件循环留作脚手架，明确未验证。

**Files:**
- Modify: `crates/mullion-app/Cargo.toml`
- Create: `crates/mullion-app/src/session_pump.rs`
- Modify: `crates/mullion-app/src/lib.rs`

- [ ] **Step 1: 加 tokio 依赖**

`crates/mullion-app/Cargo.toml` 的 `[dependencies]` 里，在 `glyphon.workspace = true` 下加：

```toml
tokio.workspace = true
```

- [ ] **Step 2: 写失败测试 + 纯件**

创建 `crates/mullion-app/src/session_pump.rs`：

```rust
//! 字节泵纯件:app 每帧把远端字节喂进仿真器,并取回需回写对端的字节(T1)。
//! 不碰网络/GPU,可无窗口单测。真实事件循环在 main.rs 接线(未验证,需人工确认)。

use mullion_term::emulator::Emulator;

/// 喂入若干段远端字节,推进仿真器,返回需回写 SSH channel 的出站字节(T1)。
/// app 每帧:先 drain SSH 接收端得到 `inbound`,调 `pump`,再把返回值交 `SshSession::write`。
pub fn pump(emu: &mut Emulator, inbound: &[Vec<u8>]) -> Vec<u8> {
    for chunk in inbound {
        emu.feed(chunk);
    }
    emu.take_pty_writes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pump_feeds_and_collects_pty_writes() {
        // T1:喂入光标位置查询(DSR 6),pump 必须回收 CPR 应答 —— 这些字节要回写 SSH channel。
        // 漏了 → 同步输出探测无应答 → 全屏 TUI 闪。
        let mut emu = Emulator::new(80, 24);
        let out = pump(&mut emu, &[b"\x1b[6n".to_vec()]);
        assert_eq!(out, b"\x1b[1;1R", "pump 未回收 PtyWrite(T1)");
    }

    #[test]
    fn pump_without_query_yields_nothing() {
        let mut emu = Emulator::new(80, 24);
        let out = pump(&mut emu, &[b"hello".to_vec()]);
        assert!(out.is_empty(), "普通输出不应产生回写");
    }
}
```

- [ ] **Step 3: 声明模块**

`crates/mullion-app/src/lib.rs` 里加（与既有 `frame`/`reflow`/`render` 等并列）：

```rust
pub mod session_pump;
```

- [ ] **Step 4: 跑测试**

Run: `cargo test -p mullion-app session_pump::`
Expected: `pump_feeds_and_collects_pty_writes` / `pump_without_query_yields_nothing` PASS。

- [ ] **Step 5: 确认既有守护测试仍绿（T3/T4）**

Run: `cargo test -p mullion-app`
Expected: 含 `redraw_is_frame_capped`(T3)、`reflow_emits_resize`(T4) 全 PASS。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/Cargo.toml crates/mullion-app/src/session_pump.rs crates/mullion-app/src/lib.rs
git commit -m "feat(app): tokio 依赖 + 字节泵纯件,回收 take_pty_writes (T1)"
```

> **脚手架说明（不在本任务落代码，写进 PR 描述）**：main.rs 真实接线 = 建 `tokio::runtime`
> → `runtime.block_on(connect(..))` 拿 `(session, rx)` → `EventLoop<UserEvent>` 用
> `proxy.send_event(UserEvent::RemoteData)` 作 `wake` → 每帧 drain `rx.try_recv()` 收集成
> `Vec<Vec<u8>>` → `pump(&mut emu, &inbound)` → 非空则 `session.write(out)`（T1）→ 键鼠经
> keymap → `session.write`；reflow → `session.resize`（T4）。GPU/winit/是否真不闪无法无头验证。

---

## Task 9: 真机 live 冒烟（门控）+ 手动验证清单

pubkey/agent 打真机 `user@192.0.2.10`。默认 `#[ignore]`，靠 `MULLION_LIVE=1` 手动跑，`cargo test --workspace` 不受影响。

**Files:**
- Create: `crates/mullion-ssh/tests/live.rs`

- [ ] **Step 1: 写门控 live 测试**

创建 `crates/mullion-ssh/tests/live.rs`：

```rust
//! 真机 live 冒烟。默认 ignore;设 MULLION_LIVE=1 且真机可达时手动跑:
//!   MULLION_LIVE=1 cargo test -p mullion-ssh --test live -- --ignored --nocapture
//! 目标: user@192.0.2.10:22,私钥 /path/to/key.pem。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use mullion_ssh::config::{AuthMethod, SshConfig};
use mullion_ssh::known_hosts::{KnownHosts, TofuAccept};
use mullion_ssh::session::connect;

fn live_enabled() -> bool {
    std::env::var("MULLION_LIVE").as_deref() == Ok("1")
}

fn base(auth: AuthMethod) -> SshConfig {
    SshConfig {
        host: "192.0.2.10".into(),
        port: 22,
        user: "testuser".into(),
        auth,
        cols: 80,
        rows: 24,
        term: "xterm-256color".into(),
    }
}

async fn run_echo(auth: AuthMethod) {
    let policy = Arc::new(TofuAccept::new(Arc::new(Mutex::new(KnownHosts::new()))));
    let (session, mut rx) = connect(&base(auth), policy, Arc::new(|| {}))
        .await
        .expect("真机连接");
    // 真机是真 shell:发一条命令,断言输出里出现标记。
    session.write(b"echo MULLION_OK\n".to_vec()).expect("write");
    let deadline = Duration::from_secs(10);
    let mut seen = Vec::new();
    let ok = tokio::time::timeout(deadline, async {
        while let Some(chunk) = rx.recv().await {
            seen.extend_from_slice(&chunk);
            if seen.windows(10).any(|w| w == b"MULLION_OK") {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(ok, "真机 shell 应回显 MULLION_OK;收到: {:?}", String::from_utf8_lossy(&seen));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "需真机 192.0.2.10 + MULLION_LIVE=1"]
async fn pubkey_live() {
    if !live_enabled() {
        eprintln!("跳过:未设 MULLION_LIVE=1");
        return;
    }
    run_echo(AuthMethod::PublicKey {
        path: "/path/to/key.pem".into(),
        passphrase: None,
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "需真机 + agent 已加载 /path/to/key.pem + MULLION_LIVE=1"]
async fn agent_live() {
    if !live_enabled() {
        eprintln!("跳过:未设 MULLION_LIVE=1");
        return;
    }
    run_echo(AuthMethod::Agent).await;
}
```

- [ ] **Step 2: 确认默认不跑（保持 workspace 绿）**

Run: `cargo test -p mullion-ssh --test live`
Expected: 2 个测试均 `ignored`，0 failed。

- [ ] **Step 3: 手动 live 冒烟（在本环境跑一次 pubkey；TCP 已确认可达 192.0.2.10:22）**

Run:
```bash
chmod 600 /path/to/key.pem
MULLION_LIVE=1 cargo test -p mullion-ssh --test live pubkey_live -- --ignored --nocapture
```
Expected: `pubkey_live` PASS（真机回显 MULLION_OK）。**若失败**：按 F6 错误信息定位（DNS/refused/auth/hostkey）；agent 版需先 `eval $(ssh-agent) && ssh-add /path/to/key.pem` 再单跑 `agent_live`。这一步属人工确认，结果写进 PR 描述，不谎报。

- [ ] **Step 4: 提交**

```bash
git add crates/mullion-ssh/tests/live.rs
git commit -m "test(ssh): 真机 pubkey/agent live 冒烟(MULLION_LIVE 门控)"
```

---

## Task 10（可选）: ADR-004 async 边界

**Files:**
- Create: `docs/adr-004-async-boundary.md`

- [ ] **Step 1: 写 ADR**

创建 `docs/adr-004-async-boundary.md`，记：决策 = 方案 B（app 拥有 tokio，ssh 纯 async 库）；备选 A（ssh 自带运行时线程）否于「库内隐藏运行时反模式」；备选 C（winit 循环 block_on）否于「高延迟冻 UI，违背零闪烁/N1」；ssh 靠注入 `wake`/`policy` 不依赖 winit/不弹 UI。

- [ ] **Step 2: 提交**

```bash
git add docs/adr-004-async-boundary.md
git commit -m "docs: adr-004 async 边界(方案 B)"
```

---

## 收尾验证（全部任务后）

- [ ] **workspace 全绿**

Run:
```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
```
Expected: 各 crate `test result: ok`，无 FAILED/panicked。

- [ ] **clippy 干净**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: 无输出。

- [ ] **fmt**

Run: `cargo fmt --check`
Expected: 无差异。

「绿」定义（项目 CLAUDE.md）：以上三条同时满足。live 冒烟属人工确认项，单列。

---

## 自查:spec 覆盖

- **F1 密码/pubkey/agent**：Task 6（establish 三分支）+ Task 7（PTY）+ 测试 auth.rs（密码/pubkey hermetic）+ live.rs（pubkey/agent 真机）。密码 hermetic、pubkey hermetic+live、agent live。✓
- **F3 TOFU**：Task 4（策略）+ Task 6（check_server_key 接线 + `changed_host_key_is_rejected`）。既有 3 守护测试保留。✓
- **F6 错误枚举**：Task 2（每变体独立 + 分类 + 消息互异测试）。✓
- **T1 回写**：Task 8（pump 回收 take_pty_writes 测试）;mullion-term 既有 `pty_write_is_collected` 不动。✓
- **T3/T4**：不改其逻辑，Task 8 Step 5 确认仍绿;resize 路径 Task 7 `resize_does_not_error`。✓
- **方案 B 架构不变量**：ssh 无 winit（`wake` 回调）、无 UI（`policy`）、无 pane；app 独占 tokio 与泵编排。✓
- **不引 thiserror**：Task 2 手写 Display/Error。✓
- **超范围留待**：F2 ssh_config、F4/F5 代理跳板、断线重连、known_hosts 磁盘完整格式 —— 均未在任务内，符合 spec §1。✓
