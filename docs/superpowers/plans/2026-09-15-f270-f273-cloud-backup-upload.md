# F270~F273 云端备份（片一：上传） Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 Mullion 能把「整机迁移包（去掉 layouts）」用主密码派生的密钥整体加密后，推到用户自己的 S3 兼容对象存储上——手动一键 + 定时兜底，内容没变就不推。

**Architecture:** 新建第六个 crate `mullion-cloud`（只认「字节 + 键名」，零 async、零 UI，用阻塞式 ureq）。打包/加密/指纹留在 `mullion-store`（零 async、仅同步 IO 的不变量不破）。云配置住独立的 `cloud.toml`，**不在迁移包白名单里**，AK/SK 用 vault key 加密。`mullion-app` 负责设置分节、菜单入口、状态栏和定时驱动，网络调用一律 `spawn_blocking`。

**Tech Stack:** Rust 2021 / ureq 3（rustls + ring provider，显式钉死）/ sha2（已在 Cargo.lock，来自 russh）/ chacha20poly1305 + argon2（已有）/ egui 0.30。

**设计文档：** `docs/superpowers/specs/2026-09-15-f270-f275-cloud-backup-design.md`（决策表 D1~D15，实现中每条都要能对上）。

---

## 关键约束（每个任务都受它约束，别在中途忘掉）

1. **依赖方向**：`app → {core, term, ssh, store, cloud}`。`cloud` **不依赖任何一个兄弟 crate**，也不认识 `Pack`/`Session`/UI。
2. **`cloud` 零 async**：ureq 是阻塞式的，app 侧用 `tokio::task::spawn_blocking` 挖出去。绝不能在事件循环里同步跑网络（T3/T7 红线）。
3. **ureq 的 TLS provider 必须钉死**（D4）：官方文档原话不保证永远默认 ring，静默切到 aws-lc-rs 会让 `x86_64-pc-windows-gnu` 交叉编译炸，且只有交叉编译时才暴露。
4. **UI 字符串只许用 GBK 内的字形**（T9）：`tests/glyph_whitelist.rs` 机械守护着。不要在文案里写 `→`、`…` 之外的符号；`…` 已在白名单里，先跑一遍测试再说。
5. **测试不许恒绿**：每写完一个守护测试，**手动改坏被测代码验证它变红**，再改回来。改坏之前**先 `git commit`**（本项目已有五次「变异验证时被 `git checkout` 吞掉未提交编辑」的记录）。

---

## File Structure

**新建 crate `crates/mullion-cloud/`：**

| 文件 | 职责 |
|---|---|
| `Cargo.toml` | 只依赖 ureq（钉死 features）、sha2、hmac、log。**不依赖任何 mullion-\*** |
| `src/lib.rs` | 模块导出 + 架构不变量说明 |
| `src/error.rs` | `CloudError`（含 `AlreadyExists` 这个必须能被调用方分辨出来的变体） |
| `src/sigv4.rs` | SigV4 签名。**纯函数，零 IO**。canonical request → string to sign → signing key → Authorization 头 |
| `src/url.rs` | endpoint + bucket + key → URL 与 Host 头。virtual-hosted / path-style 两种。**纯函数** |
| `src/xml.rs` | ListObjectsV2 响应里抠 `<Key>` 与 `<NextContinuationToken>`。**纯函数，不引 XML crate** |
| `src/s3.rs` | `S3Client`：`put_no_overwrite` / `list_keys`。唯一碰网络的地方 |
| `tests/fake_s3.rs` | 假 HTTP server（std::net::TcpListener）+ 协议流程测试 |
| `tests/live.rs` | `#[ignore]` 的真 bucket 端到端，AK/SK 从 env 传 |

**改 `crates/mullion-store/`：**

| 文件 | 改什么 |
|---|---|
| `src/cloud.rs`（新建） | `CloudConfig` + `cloud.toml` 读-改-写 + AK/SK 封解 + 指纹 + 对象键命名 + 上传决策 |
| `src/portable.rs` | 抽出 `collect_top_level`（`collect` 复用它），新增 `cloud_payload` / `open_cloud_payload` |
| `src/vault.rs` | 新增 `seal_with_master` / `open_with_master` / `scheme()`；`set_master_password` / `clear_master_password` 连带重封 `cloud.toml`（D12） |
| `src/lib.rs` | 导出新模块 |

**改 `crates/mullion-app/`：**

| 文件 | 改什么 |
|---|---|
| `src/cloudsync.rs`（新建） | 把 store 的打包结果 + cloud 的客户端接起来的编排函数；`spawn_blocking` 的调用点 |
| `src/ui/settings.rs` | 新增「云端备份」分节（未设主密码时整节灰掉） |
| `src/ui/chrome.rs` | 菜单「立刻备份到云」+ 状态栏云指示器 |
| `src/app.rs` | `UserEvent::CloudBackupDone` + `drive_cloud_backup` + 设置提交接线 |

---

## Task 1: 新建 `mullion-cloud` crate 骨架 + ADR

**Files:**
- Create: `crates/mullion-cloud/Cargo.toml`
- Create: `crates/mullion-cloud/src/lib.rs`
- Create: `crates/mullion-cloud/src/error.rs`
- Modify: `Cargo.toml`（workspace members + workspace.dependencies）
- Create: `docs/adr-0NN-cloud-crate-and-http-client.md`（NN = 现有最大 ADR 编号 + 1，先 `ls docs/adr-*.md` 查）
- Modify: `CLAUDE.md`（架构不变量表加一行）

- [ ] **Step 1: 查现有 ADR 编号**

```bash
ls docs/adr-*.md | sort | tail -3
```

记下最大编号，下面的 `0NN` 全部替换成 最大编号+1。

- [ ] **Step 2: 建 crate 的 Cargo.toml**

```toml
[package]
name = "mullion-cloud"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
# ⚠️ TLS provider 必须显式钉死(设计 D4)。ureq 3 今天默认 rustls+ring,但官方
# README 原话是 "does not guarantee defaulting to it indefinitely" —— 哪天默认
# 切到 aws-lc-rs,`x86_64-pc-windows-gnu` 的交叉编译会在 aws-lc-sys 的 C/NASM
# 构建上当场炸(正是 ADR-005 当初把 russh 切到 ring 要躲开的那件事),而本机
# `cargo test` 一切正常 —— 只有交叉编译时才暴露。
#
# 不开 `gzip`:它会再拉一条 `flate2` 的路径,而 RUSTSEC-2026-0153 对本项目
# 是**真可达**的(见 memory「cargo audit 可达性登记」),不主动加第二条。
# 不开 `cookies`/`json`/`charset`:对象存储一条都用不上(N6 exe 体积)。
ureq = { workspace = true }
# SigV4 要 HMAC-SHA256。两者本来就在 Cargo.lock 里(russh 拉的),加直接依赖
# 不引入新版本 —— 同 `base64`/`argon2` 那两条的理由。
sha2 = { workspace = true }
hmac = { workspace = true }
log = { workspace = true }

[dev-dependencies]
# 假 S3 server 只用 std::net,不需要 tokio。
```

- [ ] **Step 3: 在 workspace 根 Cargo.toml 里登记**

`members` 数组里加 `"crates/mullion-cloud"`，`[workspace.dependencies]` 末尾加：

```toml
# F270:云端备份的 HTTP 客户端。**阻塞式**,所以 `mullion-cloud` 与 `mullion-store`
# 同样是零 async 的纯 crate;app 侧用 spawn_blocking 挖出去(T3/T7:绝不在事件
# 循环里同步跑网络)。features 见 crates/mullion-cloud/Cargo.toml 顶上的长注释 ——
# `rustls` 这一条是**交叉编译能不能过**的开关,不是口味问题。
ureq = { version = "3", default-features = false, features = ["rustls", "socks-proxy"] }
# SigV4 的 HMAC-SHA256 与内容指纹。两者已在 Cargo.lock(russh 传递依赖),
# 加直接依赖不引入新版本。
sha2 = "0.10"
hmac = "0.12"
```

- [ ] **Step 4: 写 `src/error.rs`**

```rust
//! `mullion-cloud` 的错误类型。**不用 thiserror** —— 本 crate 的依赖清单越短
//! 越好(它是 exe 体积与交叉编译风险的直接来源),手写一个 Display 够用。

use std::fmt;

#[derive(Debug)]
pub enum CloudError {
    /// 目标对象已经存在(`ForbidOverwrite` 生效)。**必须与别的错分开** ——
    /// 调用方要靠它决定「换个序号重试」而不是「报失败给用户」。
    AlreadyExists,
    /// 服务端回了非 2xx。带上状态码与响应正文的前若干字节:对象存储的报错
    /// 几乎全在正文的 `<Code>` 里(`SignatureDoesNotMatch` / `AccessDenied` /
    /// `NoSuchBucket`),只报状态码等于把唯一有用的那条信息扔了。
    Status { code: u16, body: String },
    /// 网络层(连不上 / 超时 / TLS 握手失败)。
    Transport(String),
    /// 响应解析不出来。
    Malformed(String),
    /// 配置本身不合法(endpoint 为空、bucket 名非法…),还没发请求就停住。
    Config(String),
}

impl fmt::Display for CloudError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CloudError::AlreadyExists => write!(f, "目标对象已存在"),
            CloudError::Status { code, body } => write!(f, "服务端返回 {code}:{body}"),
            CloudError::Transport(e) => write!(f, "网络错误:{e}"),
            CloudError::Malformed(e) => write!(f, "响应解析失败:{e}"),
            CloudError::Config(e) => write!(f, "配置不合法:{e}"),
        }
    }
}

impl std::error::Error for CloudError {}
```

- [ ] **Step 5: 写 `src/lib.rs`**

```rust
//! mullion-cloud —— S3 兼容对象存储客户端(F270)。
//!
//! # 架构不变量
//!
//! **只认「字节 + 键名」。** 不认识 `Pack`、`SessionRecord`、egui,也**不依赖
//! 任何 mullion-\* crate**。打包、加密、指纹全在 `mullion-store` 那边。
//!
//! 这条约束的价值:违反了之后,「SigV4 签名算错了」和「包组装错了」就再也
//! 分不开 —— 而前者只能靠官方测试向量证明,后者只能靠 round-trip 证明,
//! 两类证据没法互相替代。
//!
//! **零 async、零 UI、零 GPU。** ureq 是阻塞式的;app 侧用 `spawn_blocking`
//! 挖出去(绝不能在事件循环里同步跑网络 —— T3/T7)。

pub mod error;
pub mod s3;
pub mod sigv4;
pub mod url;
pub mod xml;

pub use error::CloudError;
```

暂时让 `s3`/`sigv4`/`url`/`xml` 是空文件（`// 见后续任务`），先让骨架编过。

- [ ] **Step 6: 验证编译**

Run: `cargo build -p mullion-cloud 2>&1 | tail -20`
Expected: 编译通过（可能有 unused 警告，先不管）。

- [ ] **Step 7: 写 ADR**

Create `docs/adr-0NN-cloud-crate-and-http-client.md`：

```markdown
# ADR-0NN:云端备份单开一个 crate,HTTP 用阻塞式 ureq

## 状态
已采纳（2026-09-15）

## 背景
F270 要把配置推到用户自己的 S3 兼容对象存储。工作区原本没有任何 HTTP/TLS 客户端
（`Cargo.lock` 里没有 reqwest/ureq/hyper/rustls，只有 russh 带进来的 `ring`）。

## 决策
1. 新建 `mullion-cloud` crate，架构不变量表从五条变六条。
2. HTTP 客户端用 `ureq 3`，`default-features = false`，显式挑 `rustls` + `socks-proxy`。

## 备选与为什么否掉

**塞进 `mullion-store`**：破坏「零 async、仅同步 IO、可纯单测」不变量里最值钱的
那半句——store 今天的每条测试都不需要起服务器，塞进去之后这条性质没了。

**塞进 `mullion-ssh`**：品类错误。那个 crate 的定义是「russh，只认字节流」。

**塞进 `mullion-app`**：依赖方向合法、不用改架构表，但正好落在本项目刚登记过的
「App 的方法测不了 → 交付判据整体恒绿」那一层（slice-f253-f256）。SigV4 签名算错了
靠什么发现，会变成一个开放问题。

**reqwest（async）**：能直接融进 app 现有的 tokio 运行时，但依赖树大一个量级
（hyper/h2/tower/http），当前 605 个 crate 会涨到 680+，且 `mullion-cloud` 就不再是
零 async 的纯 crate。对一个「每 30 分钟 PUT 几十 KB」的功能，这个价钱不值。

**自己手写 HTTP over rustls**：依赖最少，但要自己处理 chunked、重定向、连接复用、
超时、代理 CONNECT。在一个备份功能上自建 HTTP 栈，性价比极差。

## 代价
- exe 体积增加（N6 盯着 25MB 上限，片一发版时必须重新量）。
- **TLS provider 必须在 `Cargo.toml` 里钉死**：ureq 官方 README 原话是
  "does not guarantee defaulting to it indefinitely"。默认哪天切到 aws-lc-rs，
  `x86_64-pc-windows-gnu` 交叉编译会在 aws-lc-sys 的 C/NASM 构建上炸——正是
  ADR-005 把 russh 切到 ring 要躲开的那件事，而且**只有交叉编译时才暴露**，
  本机 `cargo test` 全绿。
```

- [ ] **Step 8: 在 CLAUDE.md 架构不变量表里加一行**

在 `mullion-store` 那行下面、`mullion-app` 那行上面插入：

```
mullion-cloud    S3 兼容对象存储客户端。只认字节与键名,不认识 Pack/Session/UI。零 async。可纯单测。
```

并把下一行的依赖方向改成：

```
**依赖方向严格单向**：`app → {core, term, ssh, store, cloud}`，其余互不依赖。
```

- [ ] **Step 9: 提交**

```bash
git add Cargo.toml Cargo.lock crates/mullion-cloud CLAUDE.md docs/adr-0NN-cloud-crate-and-http-client.md
git commit -m "feat(cloud): 新建 mullion-cloud crate 骨架与 ADR (F270)

架构不变量表五条变六条。ureq 的 TLS provider 在 Cargo.toml 里显式钉死
rustls(ring)——官方文档不保证永远默认 ring,静默切 aws-lc-rs 会让
x86_64-pc-windows-gnu 交叉编译炸且只有交叉编译时才暴露(ADR-005 同族)。"
```

---

## Task 2: SigV4 签名（纯函数 + 官方测试向量）

**Files:**
- Modify: `crates/mullion-cloud/src/sigv4.rs`

**⚠️ 向量来源（已核对，直接用，不要再改期望值）：**

算法规范逐字取自 AWS 官方页 `IAM/latest/UserGuide/reference_sigv-create-signed-request`
（派生链四步、canonical request 六段、string-to-sign 四行、UriEncode 规则、hex 大小写）。
AWS 已把带真实 hex 的完整算例从文档里撤掉，所以下面两条向量的**期望值是用 openssl
独立复算验证过的**（`openssl dgst -sha256 -mac HMAC`），不是凭记忆写的：

- `signing_key(+版密钥, 20150830, us-east-1, iam)` → `c4afb1cc…` ✅ 已复算一致
- S3 GET Object 的 `Signature=f0e8bdb8…` ✅ 已复算一致（canonical request hash
  `7344ae5b7ee6c3e7e6b0fe0640412a37625d1fbfff95c48bbb2dc43964946972`）

**如果你实现完测试没过，先怀疑实现，不要改期望值。** 两个常量都验过了。

- [ ] **Step 1: 写签名密钥派生的失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// `aws-sig-v4-test-suite` 那对示例凭据(AKIDEXAMPLE)的 secret。
    ///
    /// **注意结尾是 `ENG+bPx` 不是 `ENG/bPx`。** AWS 在不同文档里用了两对长得
    /// 极像的示例凭据:`AKIDEXAMPLE` 配 `...MDENG+bPx...`(本条与下面那条
    /// 20150830/iam 的向量用它),`AKIAIOSFODNN7EXAMPLE` 配 `...MDENG/bPx...`
    /// (S3 GET Object 那条向量用它)。配错一个字符,派生出的密钥完全不同,
    /// 而症状只是一条对不上的 hex —— 很容易被误判成「实现写错了」而去改
    /// 正确的实现。两条向量的期望值都已用 openssl 独立复算核对过。
    const EX_SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";

    /// 派生链必须是 kDate → kRegion → kService → kSigning,**顺序不能换**。
    /// 顺序错了照样能算出一个 32 字节的东西,服务端只会回一个 403
    /// `SignatureDoesNotMatch` —— 与「AK 权限不够」长得一模一样。
    #[test]
    fn the_signing_key_matches_the_published_vector() {
        let got = signing_key(EX_SECRET, "20150830", "us-east-1", "iam");
        assert_eq!(
            hex(&got),
            "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9",
            "派生链与 AWS 公布的向量不符"
        );
    }
}
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-cloud sigv4 2>&1 | tail -20`
Expected: FAIL，`cannot find function signing_key`。

- [ ] **Step 3: 实现 `signing_key` 与 `hex`**

```rust
//! AWS Signature Version 4(F270)。**纯函数,零 IO、零网络。**
//!
//! 单独成模块的唯一理由:这是整个云端备份里**唯一能被官方测试向量证明对错**
//! 的部分。签名算错的症状是服务端回 403 `SignatureDoesNotMatch`,而它与
//! 「AK 权限不够」「时钟偏差太大」长得完全一样 —— 靠真机试错去区分,一次
//! 往返几秒钟,而且要真的有一个 bucket。向量测试是零成本的替代品。

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// 十六进制小写。SigV4 全篇用的都是小写 hex,大写会静默算出另一个签名。
pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// SHA-256 的十六进制摘要。空载荷也要算(SigV4 要求 `x-amz-content-sha256`
/// 永远有值,不能省略)。
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hmac(key: &[u8], msg: &[u8]) -> Vec<u8> {
    // `new_from_slice` 对 HMAC 只在密钥长度为 0 时才可能出错,而下面四次调用
    // 的密钥都非空;真出错了也不该 panic 掉整个 app,退回一个不可能匹配的
    // 空签名让服务端拒掉 —— 拿到的是一条 403,比进程没了强。
    match HmacSha256::new_from_slice(key) {
        Ok(mut m) => {
            m.update(msg);
            m.finalize().into_bytes().to_vec()
        }
        Err(_) => Vec::new(),
    }
}

/// 派生签名密钥。**四步的顺序是规范定死的**,见模块文档里那条症状。
pub fn signing_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = hmac(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k_region = hmac(&k_date, region.as_bytes());
    let k_service = hmac(&k_region, service.as_bytes());
    hmac(&k_service, b"aws4_request")
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-cloud sigv4 2>&1 | tail -20`
Expected: PASS。若 FAIL，**先核对官方文档的向量**再怀疑实现。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-cloud/src/sigv4.rs
git commit -m "feat(cloud): SigV4 签名密钥派生 + 官方向量守护 (F270)"
```

- [ ] **Step 6: 写完整签名的失败测试**

追加到 `mod tests`：

```rust
/// AWS 官方 "Example: GET Object" 的完整端到端向量。
///
/// 这一条同时钉住五件事:canonical request 的拼法、signed headers 的**排序与
/// 小写**、string to sign 的三行结构、scope 的四段、Authorization 头的格式。
/// 其中任何一件写错,服务端都只会回同一条 403。
#[test]
fn the_authorization_header_matches_the_published_get_object_vector() {
    let req = Request {
        method: "GET",
        host: "examplebucket.s3.amazonaws.com",
        // **已 URI-encode 过的**路径。SigV4 对 S3 要求「不要二次编码」,
        // 所以编码在 `url.rs` 那边一次做完,这里只管拼。
        path: "/test.txt",
        query: "",
        payload_sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        amz_date: "20130524T000000Z",
        extra_headers: &[("range", "bytes=0-9")],
    };
    let got = authorization(
        &req,
        "AKIAIOSFODNN7EXAMPLE",
        "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        "us-east-1",
        "s3",
    );
    assert_eq!(
        got,
        "AWS4-HMAC-SHA256 \
         Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
         SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
         Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
    );
}

/// signed headers 必须**按名字排序**,不是按调用方给的顺序。给的顺序反过来
/// 也得算出同一个签名 —— 否则「加一个自定义头」就会随机地让签名失效。
#[test]
fn header_order_from_the_caller_does_not_change_the_signature() {
    let mk = |extra: &'static [(&'static str, &'static str)]| Request {
        method: "PUT",
        host: "b.example.com",
        path: "/k",
        query: "",
        payload_sha256: "abc",
        amz_date: "20260915T101500Z",
        extra_headers: extra,
    };
    let a = authorization(&mk(&[("x-a", "1"), ("x-b", "2")]), "AK", "SK", "cn-hangzhou", "s3");
    let b = authorization(&mk(&[("x-b", "2"), ("x-a", "1")]), "AK", "SK", "cn-hangzhou", "s3");
    assert_eq!(a, b, "signed headers 没排序 —— 调用方换个顺序签名就变了");
}
```

- [ ] **Step 7: 跑测试确认失败**

Run: `cargo test -p mullion-cloud sigv4 2>&1 | tail -20`
Expected: FAIL，`cannot find type Request` / `cannot find function authorization`。

- [ ] **Step 8: 实现 `Request` 与 `authorization`**

追加到 `sigv4.rs`（放在 `mod tests` 之前）：

```rust
/// 要签的这一次请求。**字段全是已经定型的字符串** —— 本模块不做任何
/// URI 编码/规范化,那是 `url.rs` 的事。两处都做的话,S3 的「不要二次编码」
/// 规则迟早被违反,而症状还是那条 403。
pub struct Request<'a> {
    pub method: &'a str,
    pub host: &'a str,
    /// 已编码的路径,以 `/` 开头。
    pub path: &'a str,
    /// 已编码、**已按名字排序**的 query string,不含 `?`。空串 = 没有 query。
    pub query: &'a str,
    /// 载荷的 SHA-256 十六进制。
    pub payload_sha256: &'a str,
    /// `YYYYMMDD'T'HHMMSS'Z'`。
    pub amz_date: &'a str,
    /// 除 `host` / `x-amz-date` / `x-amz-content-sha256` 之外还要签的头。
    /// 名字**必须已经是小写**(调用方给大写的话这里不替他改,因为改了之后
    /// 实际发出去的头与签进去的头就可能不是同一个)。
    pub extra_headers: &'a [(&'a str, &'a str)],
}

/// 算出 `Authorization` 头的完整值。
pub fn authorization(req: &Request<'_>, ak: &str, sk: &str, region: &str, service: &str) -> String {
    let date = &req.amz_date[..8];

    // 三个必签头 + 调用方给的。排序在这里做一次,签名与 SignedHeaders 用的
    // 是**同一个已排序列表** —— 分两处各排一次,迟早漂开。
    let mut headers: Vec<(String, String)> = vec![
        ("host".to_string(), req.host.to_string()),
        (
            "x-amz-content-sha256".to_string(),
            req.payload_sha256.to_string(),
        ),
        ("x-amz-date".to_string(), req.amz_date.to_string()),
    ];
    for (k, v) in req.extra_headers {
        headers.push((k.to_string(), v.trim().to_string()));
    }
    headers.sort_by(|a, b| a.0.cmp(&b.0));

    let canonical_headers: String = headers
        .iter()
        .map(|(k, v)| format!("{k}:{v}\n"))
        .collect();
    let signed_headers: Vec<&str> = headers.iter().map(|(k, _)| k.as_str()).collect();
    let signed_headers = signed_headers.join(";");

    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        req.method, req.path, req.query, canonical_headers, signed_headers, req.payload_sha256
    );

    let scope = format!("{date}/{region}/{service}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{}",
        req.amz_date,
        scope,
        sha256_hex(canonical_request.as_bytes())
    );

    let key = signing_key(sk, date, region, service);
    let signature = hex(&hmac(&key, string_to_sign.as_bytes()));

    format!(
        "AWS4-HMAC-SHA256 Credential={ak}/{scope}, SignedHeaders={signed_headers}, Signature={signature}"
    )
}
```

- [ ] **Step 9: 跑测试确认通过**

Run: `cargo test -p mullion-cloud sigv4 2>&1 | tail -20`
Expected: 3 passed。

- [ ] **Step 10: 变异验证（先提交，再改坏）**

```bash
git add crates/mullion-cloud/src/sigv4.rs
git commit -m "feat(cloud): SigV4 Authorization 头 + 两条官方向量守护 (F270)"
```

然后逐条改坏并确认变红，每条验完 `git checkout crates/mullion-cloud/src/sigv4.rs`：

| 变异 | 应该变红的测试 |
|---|---|
| `headers.sort_by(...)` 整行删掉 | `header_order_from_the_caller_does_not_change_the_signature` |
| `signing_key` 里 `k_region` 与 `k_service` 两行对调 | `the_signing_key_matches_the_published_vector` |
| `string_to_sign` 里把 `scope` 与 `req.amz_date` 对调 | `the_authorization_header_matches_the_published_get_object_vector` |
| `hex` 里 `{b:02x}` 改成 `{b:02X}` | 两条向量测试红；`header_order_...` **不会红**（它比的是两次 `authorization()` 输出相等，大小写对两边同样生效——它守的是排序，不是 hex 大小写） |

Run: `cargo test -p mullion-cloud sigv4 2>&1 | grep -E "test result|FAILED"`
Expected: 每次变异都有测试 FAILED；一条都没红就说明那条守护是恒绿的，停下来问。

---

## Task 3: URL 与 Host 构造（virtual-hosted / path-style）

**Files:**
- Modify: `crates/mullion-cloud/src/url.rs`

- [ ] **Step 1: 写失败测试**

```rust
//! endpoint + bucket + key → 请求 URL 与 `Host` 头(F270)。**纯函数。**
//!
//! 两种寻址方式都得支持:阿里云 OSS / AWS S3 默认是 virtual-hosted
//! (`<bucket>.<endpoint>`),而自建 MinIO 通常只能走 path-style
//! (`<endpoint>/<bucket>`)—— 选了「通吃」就不能只做一种。
//!
//! **签名用的 path 与实际请求的 path 必须逐字节一致**,所以两者从同一个函数
//! 出来。分两处各拼一次的话,path-style 下签名里少个 `/bucket` 前缀,服务端
//! 回 403 `SignatureDoesNotMatch` —— 又是那条什么都看不出来的错。

#[cfg(test)]
mod tests {
    use super::*;

    fn oss() -> Endpoint {
        Endpoint {
            base: "https://oss-cn-hangzhou.aliyuncs.com".into(),
            bucket: "my-bucket".into(),
            path_style: false,
        }
    }

    #[test]
    fn virtual_hosted_puts_the_bucket_in_the_host() {
        let t = oss().target("mullion/000001-20260915T101500Z.mpk");
        assert_eq!(t.host, "my-bucket.oss-cn-hangzhou.aliyuncs.com");
        assert_eq!(t.path, "/mullion/000001-20260915T101500Z.mpk");
        assert_eq!(
            t.url,
            "https://my-bucket.oss-cn-hangzhou.aliyuncs.com/mullion/000001-20260915T101500Z.mpk"
        );
    }

    #[test]
    fn path_style_puts_the_bucket_in_the_path() {
        let mut e = oss();
        e.path_style = true;
        let t = e.target("mullion/a.mpk");
        assert_eq!(t.host, "oss-cn-hangzhou.aliyuncs.com");
        assert_eq!(
            t.path, "/my-bucket/mullion/a.mpk",
            "path-style 下 bucket 必须进 path —— 而签名签的就是这个 path"
        );
    }

    /// 签名里的 path 与 URL 里的 path 是**同一个字符串**。这条钉住的是
    /// 「有人以后为了省事在 s3.rs 里另拼一次 URL」。
    #[test]
    fn the_signed_path_is_the_tail_of_the_url() {
        for path_style in [false, true] {
            let mut e = oss();
            e.path_style = path_style;
            let t = e.target("mullion/x.mpk");
            assert!(
                t.url.ends_with(&t.path),
                "path_style={path_style} 时 URL 尾巴不是签名用的 path:{} vs {}",
                t.url,
                t.path
            );
        }
    }

    /// 末尾斜杠是用户手打 endpoint 时最常见的多余字符。不吃掉的话
    /// URL 里会出现 `//`,而**签名里也会**,于是签名对得上但 OSS 把
    /// 它当成一个名字以 `/` 开头的对象 —— 静默存到一个谁都找不到的键上。
    #[test]
    fn a_trailing_slash_on_the_endpoint_is_absorbed() {
        let e = Endpoint {
            base: "https://oss-cn-hangzhou.aliyuncs.com/".into(),
            bucket: "b".into(),
            path_style: false,
        };
        assert_eq!(e.target("k").url, "https://b.oss-cn-hangzhou.aliyuncs.com/k");
    }

    /// query 参数必须**按名字排序**后拼 —— SigV4 的 canonical query string
    /// 要求如此,而我们把同一个字符串既拿去签名又拿去发请求。
    #[test]
    fn query_parameters_are_sorted_by_name() {
        let q = query_string(&[("prefix", "mullion/"), ("list-type", "2")]);
        assert_eq!(q, "list-type=2&prefix=mullion%2F");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-cloud url 2>&1 | tail -20`
Expected: FAIL，找不到 `Endpoint`。

- [ ] **Step 3: 实现**

在 `mod tests` 之前写：

```rust
/// 一台对象存储 + 一个 bucket。
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// 形如 `https://oss-cn-hangzhou.aliyuncs.com`,末尾斜杠会被吃掉。
    pub base: String,
    pub bucket: String,
    /// `true` = `<base>/<bucket>/<key>`(MinIO 等自建服务多半只支持这种);
    /// `false` = `<bucket>.<host>/<key>`(OSS / S3 / R2 的默认)。
    pub path_style: bool,
}

/// 一次请求要用到的三件套。**三者同源**,见模块文档。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub url: String,
    pub host: String,
    /// 签名用的 canonical path(已编码,以 `/` 开头)。
    pub path: String,
}

/// RFC 3986 的 unreserved 之外一律转义。**`/` 也转义** —— 本函数只用来编
/// query 的值,那里的 `/` 必须是 `%2F`(canonical query string 的规则)。
fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// 对象键的编码:与 [`encode_component`] 同规则,**但保留 `/`** —— 键里的
/// 斜杠是路径分隔符,转义掉之后对象会存到一个名字里带 `%2F` 的键上,而且
/// 一切正常、零报错,只有在控制台里看才发现层级没了。
fn encode_key(s: &str) -> String {
    s.split('/').map(encode_component).collect::<Vec<_>>().join("/")
}

/// 按名字排序后拼成 canonical query string。
pub fn query_string(params: &[(&str, &str)]) -> String {
    let mut v: Vec<(&str, &str)> = params.to_vec();
    v.sort_by(|a, b| a.0.cmp(b.0));
    v.iter()
        .map(|(k, val)| format!("{}={}", encode_component(k), encode_component(val)))
        .collect::<Vec<_>>()
        .join("&")
}

impl Endpoint {
    /// 算出访问 `key` 要用的 URL / Host / 签名 path。
    pub fn target(&self, key: &str) -> Target {
        let base = self.base.trim_end_matches('/');
        // 用户没写 scheme 时补 https。**不要写成 `.map(|(s, h)| (s, h))`** ——
        // 那是 clippy 的 `map_identity`,`-D warnings` 下直接编不过。
        let (scheme, host_only) = base.split_once("://").unwrap_or(("https", base));
        let enc = encode_key(key);
        if self.path_style {
            let path = format!("/{}/{}", encode_component(&self.bucket), enc);
            Target {
                url: format!("{scheme}://{host_only}{path}"),
                host: host_only.to_string(),
                path,
            }
        } else {
            let host = format!("{}.{}", self.bucket, host_only);
            let path = format!("/{enc}");
            Target {
                url: format!("{scheme}://{host}{path}"),
                host,
                path,
            }
        }
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-cloud url 2>&1 | tail -20`
Expected: 5 passed。

- [ ] **Step 5: 变异验证（先提交）**

```bash
git add crates/mullion-cloud/src/url.rs
git commit -m "feat(cloud): endpoint/bucket/key 到 URL 与签名 path (F270)"
```

| 变异 | 应该变红的测试 |
|---|---|
| `encode_key` 改成直接 `encode_component(s)` | `virtual_hosted_puts_the_bucket_in_the_host`（`/` 被转义成 `%2F`） |
| `trim_end_matches('/')` 删掉 | `a_trailing_slash_on_the_endpoint_is_absorbed` |
| `query_string` 里 `v.sort_by(..)` 删掉 | `query_parameters_are_sorted_by_name` |
| `path_style` 分支里 path 去掉 bucket 段 | **只有** `path_style_puts_the_bucket_in_the_path` |

最后一条**只杀掉一条测试，这是对的，不要以为漏了**：`url` 是拿 `path` 拼出来的，
去掉 bucket 段之后两边一起少，`the_signed_path_is_the_tail_of_the_url` 照样成立。
那条测试守的是「有人在别处另拼一次 URL」，不是「path 拼错」——两条测试各守各的。

---

## Task 4: ListObjectsV2 响应解析（不引 XML crate）

**Files:**
- Modify: `crates/mullion-cloud/src/xml.rs`

- [ ] **Step 1: 写失败测试**

```rust
//! ListObjectsV2 的响应里抠出 `<Key>` 与 `<NextContinuationToken>`(F270)。
//!
//! **刻意不引 XML crate**:我们只需要两个标签的文本,而每多一个依赖就多一份
//! exe 体积与交叉编译风险(理由同根 Cargo.toml 里不用 `image` 那几条)。
//!
//! 代价写清楚:这是**标签扫描,不是 XML 解析**。键里如果出现 `&lt;` 这类
//! 实体,我们要反转义;CDATA、命名空间前缀、注释一律不认。对象键由我们自己
//! 生成(纯数字与短横),这些情况不会出现 —— 但**别拿这个函数去解别的 XML**。

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult>
  <Name>my-bucket</Name>
  <Prefix>mullion/</Prefix>
  <IsTruncated>false</IsTruncated>
  <Contents><Key>mullion/000001-20260915T101500Z.mpk</Key><Size>4096</Size></Contents>
  <Contents><Key>mullion/000002-20260915T111500Z.mpk</Key><Size>4100</Size></Contents>
</ListBucketResult>"#;

    #[test]
    fn every_key_is_extracted_in_order() {
        let r = parse_list(SAMPLE).expect("解析失败");
        assert_eq!(
            r.keys,
            vec![
                "mullion/000001-20260915T101500Z.mpk".to_string(),
                "mullion/000002-20260915T111500Z.mpk".to_string(),
            ]
        );
        assert_eq!(r.next_token, None);
    }

    /// 截断标记与续页令牌必须一起读出来。**只读 keys 不读令牌**的后果:
    /// 超过 1000 个对象时我们只看得见第一页,而 ListObjectsV2 按字典序返回,
    /// 第一页里的最大序号**不是**真正的最大序号 —— 下一次上传会撞上一个
    /// 已存在的键,ForbidOverwrite 把它挡掉,表现是「备份莫名其妙失败」。
    #[test]
    fn a_truncated_response_yields_its_continuation_token() {
        let xml = "<ListBucketResult><IsTruncated>true</IsTruncated>\
                   <NextContinuationToken>abc123</NextContinuationToken>\
                   <Contents><Key>k1</Key></Contents></ListBucketResult>";
        let r = parse_list(xml).expect("解析失败");
        assert_eq!(r.next_token.as_deref(), Some("abc123"));
    }

    /// `<Name>` 与 `<Prefix>` 也是文本标签,但它们不是 `<Key>`。这条钉住的是
    /// 「有人把匹配写成 `contains("Key")`」——那会把 `<NextContinuationToken>`
    /// 也匹配进来(它的名字里就有 "Token" 没有 "Key",但 `<ETag>` 之类将来加的
    /// 标签就不一定了)。
    ///
    /// **这是构造式覆盖,不是门控,变异表里没有它是有意的。** 在当前实现
    /// (`<Key>` 带尖括号精确匹配)下它必然通过 —— 它防的是**将来有人换一种
    /// 匹配方式**,那时它才第一次有机会变红。不要为了「让它能被杀掉」去改
    /// 实现或删掉它。
    #[test]
    fn only_key_elements_are_taken_not_every_text_node() {
        let r = parse_list(SAMPLE).expect("解析失败");
        assert!(
            !r.keys.iter().any(|k| k == "my-bucket" || k == "mullion/"),
            "把 Name/Prefix 也当成 Key 了:{:?}",
            r.keys
        );
    }

    /// 第二条断言是**替换顺序**的守护,不是凑数:输入 `a&amp;amp;lt;b` 的正解是
    /// `a&amp;lt;b`(只解一层)。若把 `&amp;amp;` 那次替换挪到最前面,会先得到
    /// `a&amp;lt;b` 再被下一次替换解成 `a<b` —— 多解了一层。**只用 `a&amp;amp;b`
    /// 做输入的话这个变异杀不掉**(两种顺序都得 `a&b`),那条守护就是恒绿的。
    #[test]
    fn xml_entities_in_a_key_are_unescaped() {
        let xml = "<ListBucketResult><Contents><Key>a&amp;b</Key></Contents></ListBucketResult>";
        assert_eq!(parse_list(xml).unwrap().keys, vec!["a&b".to_string()]);

        let nested =
            "<ListBucketResult><Contents><Key>a&amp;lt;b</Key></Contents></ListBucketResult>";
        assert_eq!(
            parse_list(nested).unwrap().keys,
            vec!["a&lt;b".to_string()],
            "实体只该解一层 —— `&amp;amp;` 必须最后换"
        );
    }

    /// 服务端回了一段我们读不懂的东西时,**报错而不是返回空列表**。
    /// 返回空的话调用方会把序号从 1 重新开始,`ForbidOverwrite` 挡住之后
    /// 表现成「备份失败」,而真实原因(响应格式不对)被彻底吃掉。
    #[test]
    fn a_response_that_is_not_a_list_result_is_an_error_not_an_empty_list() {
        let r = parse_list("<Error><Code>AccessDenied</Code></Error>");
        assert!(r.is_err(), "非 ListBucketResult 必须报错,不能静默返回空列表");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-cloud xml 2>&1 | tail -20`
Expected: FAIL，找不到 `parse_list`。

- [ ] **Step 3: 实现**

```rust
use crate::error::CloudError;

/// 一次 ListObjectsV2 的结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ListResult {
    pub keys: Vec<String>,
    /// `Some` = 还有下一页,把它当 `continuation-token` 再发一次。
    pub next_token: Option<String>,
}

/// 抠出 `<tag>` 的全部文本。标签名精确匹配(带上尖括号),避免前缀撞名。
fn take_all(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        let after = &rest[i + open.len()..];
        let Some(j) = after.find(&close) else { break };
        out.push(unescape(&after[..j]));
        rest = &after[j + close.len()..];
    }
    out
}

/// XML 的五个预定义实体。顺序要紧:`&amp;` **必须最后换**,否则
/// `&amp;lt;` 会先变成 `&lt;` 再变成 `<` —— 静默多解一层。
fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// 解析 ListObjectsV2 的响应体。
pub fn parse_list(xml: &str) -> Result<ListResult, CloudError> {
    if !xml.contains("<ListBucketResult") {
        return Err(CloudError::Malformed(format!(
            "不是 ListBucketResult:{}",
            xml.chars().take(200).collect::<String>()
        )));
    }
    Ok(ListResult {
        keys: take_all(xml, "Key"),
        next_token: take_all(xml, "NextContinuationToken").into_iter().next(),
    })
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-cloud xml 2>&1 | tail -20`
Expected: 5 passed。

- [ ] **Step 5: 提交并变异验证**

```bash
git add crates/mullion-cloud/src/xml.rs
git commit -m "feat(cloud): ListObjectsV2 响应的标签扫描解析 (F270)"
```

| 变异 | 应该变红的测试 |
|---|---|
| `parse_list` 里去掉 `<ListBucketResult` 的检查、直接返回 | `a_response_that_is_not_a_list_result_is_an_error_not_an_empty_list` |
| `next_token` 那行改成恒 `None` | `a_truncated_response_yields_its_continuation_token` |
| `unescape` 里把 `&amp;` 那行挪到最前面 | `xml_entities_in_a_key_are_unescaped`（靠它的**第二条**断言杀，第一条杀不掉） |

`only_key_elements_are_taken_not_every_text_node` 不在表里是有意的，理由写在那条测试的注释里。

---

## Task 5: `S3Client` —— `put_no_overwrite` 与 `list_keys`

**Files:**
- Modify: `crates/mullion-cloud/src/s3.rs`
- Create: `crates/mullion-cloud/tests/fake_s3.rs`

- [ ] **Step 1: 写假 S3 server 与失败测试**

Create `crates/mullion-cloud/tests/fake_s3.rs`：

```rust
//! 假 S3 服务端 + 协议流程测试(F270)。
//!
//! 手法照抄 `mullion-ssh` 的「假 sshd + 拿自家客户端打自家服务端」——
//! 真 bucket 的端到端在 `tests/live.rs`(`#[ignore]`,要 AK/SK)。
//!
//! **这一层证不了签名对不对**(假 server 不验签),那是 `sigv4.rs` 里官方
//! 向量的活。这里证的是:方法/路径/头拼对了没、409 有没有被认成
//! `AlreadyExists`、分页有没有跟着续页令牌走完。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;

use mullion_cloud::error::CloudError;
use mullion_cloud::s3::{Credentials, S3Client};
use mullion_cloud::url::Endpoint;

/// 服务端收到的一次请求(测试要断言的部分)。
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    authorization: String,
    body: Vec<u8>,
    forbid_overwrite: Option<String>,
    if_none_match: Option<String>,
}

/// 起一个只回放固定响应的 HTTP server。返回 (端口, 收到的请求的接收端)。
///
/// `replies` 按顺序回放,用完之后一律回 500 —— **不循环回放最后一条**:
/// 循环的话「多发了一次请求」这种 bug 会被静默吸收掉。
fn serve(replies: Vec<(u16, String)>) -> (u16, mpsc::Receiver<Seen>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut replies = replies.into_iter();
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { break };
            let Some(seen) = read_request(&mut s) else { break };
            let _ = tx.send(seen);
            let (code, body) = replies.next().unwrap_or((500, String::new()));
            let resp = format!(
                "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = s.write_all(resp.as_bytes());
            let _ = s.flush();
        }
    });
    (port, rx)
}

fn read_request(s: &mut TcpStream) -> Option<Seen> {
    let mut r = BufReader::new(s.try_clone().ok()?);
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut authorization = String::new();
    let mut forbid_overwrite = None;
    let mut if_none_match = None;
    let mut len = 0usize;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).ok()? == 0 || h.trim().is_empty() {
            break;
        }
        let lower = h.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("authorization:") {
            authorization = v.trim().to_string();
        }
        if let Some(v) = lower.strip_prefix("x-oss-forbid-overwrite:") {
            forbid_overwrite = Some(v.trim().to_string());
        }
        if let Some(v) = lower.strip_prefix("if-none-match:") {
            if_none_match = Some(v.trim().to_string());
        }
        if let Some(v) = lower.strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; len];
    if len > 0 {
        r.read_exact(&mut body).ok()?;
    }
    Some(Seen { method, path, authorization, body, forbid_overwrite, if_none_match })
}

fn client(port: u16) -> S3Client {
    S3Client::new(
        Endpoint {
            base: format!("http://127.0.0.1:{port}"),
            bucket: "b".into(),
            // 假 server 只有一个 IP,virtual-hosted 的 `b.127.0.0.1` 解析不了
            // —— path-style 是本地测试唯一走得通的寻址方式。
            path_style: true,
        },
        Credentials { access_key_id: "AK".into(), secret_access_key: "SK".into() },
        "cn-hangzhou".into(),
        None,
    )
}

#[test]
fn a_put_sends_the_bytes_and_signs_the_request() {
    let (port, rx) = serve(vec![(200, String::new())]);
    client(port)
        .put_no_overwrite("mullion/000001-x.mpk", b"hello", "20260915T101500Z")
        .expect("PUT 应该成功");
    let seen = rx.recv().expect("服务端没收到请求");
    assert_eq!(seen.method, "PUT");
    assert_eq!(seen.path, "/b/mullion/000001-x.mpk");
    assert_eq!(seen.body, b"hello");
    assert!(
        seen.authorization.starts_with("aws4-hmac-sha256 credential=ak/20260915/cn-hangzhou/s3/"),
        "Authorization 头不对:{}",
        seen.authorization
    );
}

/// **不带 ForbidOverwrite 的 PUT 会静默覆盖别的机器刚推上去的那一份。**
/// 而追加式序号布局的全部安全性就建立在这个头上(设计 D8:OSS 没有
/// PUT If-Match,这是唯一能用的并发保护)。
#[test]
fn a_put_always_asks_the_server_to_refuse_overwriting() {
    let (port, rx) = serve(vec![(200, String::new())]);
    let _ = client(port).put_no_overwrite("k", b"x", "20260915T101500Z");
    let seen = rx.recv().expect("没收到请求");
    assert_eq!(
        seen.forbid_overwrite.as_deref(),
        Some("true"),
        "PUT 没带 x-oss-forbid-overwrite —— 阿里云 OSS 上并发时会静默覆盖别人的备份"
    );
    // **两个头都要断言。** 只守一个的话,漏发另一个在对应的那一族服务端上
    // 就是「完全没有并发保护」,而客户端侧看到的是一次成功的 PUT ——
    // 零报错、测试全绿。这正是项目里「列举式门控在加档时必然漏」的形状。
    assert_eq!(
        seen.if_none_match.as_deref(),
        Some("*"),
        "PUT 没带 If-None-Match: * —— S3/R2/MinIO 上并发时会静默覆盖别人的备份"
    );
}

/// 409 必须被认成 `AlreadyExists`,而不是一条普通的 `Status`。
/// 调用方要靠这个分辨「换个序号重试」和「报失败给用户」。
#[test]
fn a_409_becomes_already_exists_not_a_generic_status_error() {
    let (port, _rx) = serve(vec![(409, "<Error><Code>FileAlreadyExists</Code></Error>".into())]);
    let e = client(port)
        .put_no_overwrite("k", b"x", "20260915T101500Z")
        .expect_err("409 应该报错");
    assert!(
        matches!(e, CloudError::AlreadyExists),
        "409 没被认成 AlreadyExists,拿到的是 {e:?}"
    );
}

/// 非 2xx 的响应正文必须带进错误里:对象存储的真实原因全在正文的
/// `<Code>` 里(SignatureDoesNotMatch / AccessDenied / NoSuchBucket),
/// 只报状态码等于把唯一有用的信息扔了。
#[test]
fn an_error_response_carries_the_server_message() {
    let (port, _rx) = serve(vec![(403, "<Error><Code>SignatureDoesNotMatch</Code></Error>".into())]);
    let e = client(port)
        .put_no_overwrite("k", b"x", "20260915T101500Z")
        .expect_err("403 应该报错");
    match e {
        CloudError::Status { code, body } => {
            assert_eq!(code, 403);
            assert!(body.contains("SignatureDoesNotMatch"), "正文丢了:{body}");
        }
        other => panic!("期望 Status,拿到 {other:?}"),
    }
}

/// 分页必须跟着续页令牌走完。只取第一页的话,超过 1000 个对象之后
/// 「最大序号」就是错的,下一次上传会撞上已存在的键。
#[test]
fn listing_follows_the_continuation_token_until_it_is_gone() {
    let page1 = "<ListBucketResult><IsTruncated>true</IsTruncated>\
                 <NextContinuationToken>t2</NextContinuationToken>\
                 <Contents><Key>mullion/000001-a.mpk</Key></Contents></ListBucketResult>";
    let page2 = "<ListBucketResult><IsTruncated>false</IsTruncated>\
                 <Contents><Key>mullion/000002-b.mpk</Key></Contents></ListBucketResult>";
    let (port, rx) = serve(vec![(200, page1.into()), (200, page2.into())]);
    let keys = client(port).list_keys("mullion/", "20260915T101500Z").expect("LIST 失败");
    assert_eq!(
        keys,
        vec!["mullion/000001-a.mpk".to_string(), "mullion/000002-b.mpk".to_string()]
    );
    let first = rx.recv().expect("第一页请求");
    assert!(first.path.contains("list-type=2"), "不是 ListObjectsV2:{}", first.path);
    let second = rx.recv().expect("没发第二页请求 —— 续页令牌被忽略了");
    assert!(
        second.path.contains("continuation-token=t2"),
        "第二页没带令牌:{}",
        second.path
    );
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-cloud --test fake_s3 2>&1 | tail -20`
Expected: FAIL，找不到 `S3Client`。

- [ ] **Step 3: 实现 `s3.rs`**

```rust
//! S3 兼容对象存储的两个操作:条件写与列举(F270)。**本 crate 唯一碰网络的地方。**
//!
//! 阻塞式(ureq)。调用方必须在 `spawn_blocking` 里用它 —— 在事件循环里同步
//! 跑网络会把帧率打到零(T3/T7)。

use std::time::Duration;

use crate::error::CloudError;
use crate::sigv4::{self, sha256_hex};
use crate::url::{query_string, Endpoint};
use crate::xml;

/// 单次请求的超时。高延迟代理链路是本项目的主场景,给得比"感觉够用"宽一档;
/// 但必须有限 —— 没有超时的话一次卡住的上传会把 `spawn_blocking` 的线程
/// 永久占住,而定时器还在往里塞新的。
const TIMEOUT: Duration = Duration::from_secs(60);

/// 一次能列多少个。取服务端上限,减少往返。
const MAX_KEYS: &str = "1000";

/// 续页的次数上限。**不是性能考虑,是防失控** —— 服务端若因某种原因永远
/// 回同一个令牌,没有上限就是一个不会结束的循环,而它跑在后台线程里,
/// 用户只看得见「备份一直在转」。
const MAX_PAGES: usize = 64;

#[derive(Debug, Clone)]
pub struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
}

pub struct S3Client {
    endpoint: Endpoint,
    creds: Credentials,
    region: String,
    agent: ureq::Agent,
}

impl S3Client {
    /// `socks5` = `Some("host:port")` 时全部请求走 SOCKS5 代理。
    pub fn new(
        endpoint: Endpoint,
        creds: Credentials,
        region: String,
        socks5: Option<&str>,
    ) -> Self {
        let mut b = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            // **必须关掉**:409(ForbidOverwrite 命中)与 403(签名/权限)都要
            // 我们自己看状态码分类。留着默认的话它们全变成同一个
            // `Error::StatusCode`,`AlreadyExists` 就认不出来了。
            .http_status_as_error(false);
        if let Some(p) = socks5 {
            if let Ok(proxy) = ureq::Proxy::new(&format!("socks5://{p}")) {
                b = b.proxy(Some(proxy));
            }
        }
        Self {
            endpoint,
            creds,
            region,
            agent: b.build().into(),
        }
    }

    /// 写一个对象,**要求服务端在对象已存在时拒绝**。
    ///
    /// `amz_date` 由调用方注入(本 crate 不持时钟,同 `mullion-store` 的姿态:
    /// 持了时钟就没法写确定性测试)。
    pub fn put_no_overwrite(
        &self,
        key: &str,
        body: &[u8],
        amz_date: &str,
    ) -> Result<(), CloudError> {
        let t = self.endpoint.target(key);
        let payload = sha256_hex(body);
        // 阿里云 OSS 认 `x-oss-forbid-overwrite`;S3/R2/MinIO 认
        // `If-None-Match: *`。**两个都发** —— 不认识的那个会被忽略,而漏发
        // 任一个都意味着在对应的服务端上完全没有并发保护(且零报错)。
        let extra = [
            ("if-none-match", "*"),
            ("x-oss-forbid-overwrite", "true"),
        ];
        let auth = sigv4::authorization(
            &sigv4::Request {
                method: "PUT",
                host: &t.host,
                path: &t.path,
                query: "",
                payload_sha256: &payload,
                amz_date,
                extra_headers: &extra,
            },
            &self.creds.access_key_id,
            &self.creds.secret_access_key,
            &self.region,
            "s3",
        );
        let mut req = self
            .agent
            .put(&t.url)
            .header("authorization", &auth)
            .header("x-amz-date", amz_date)
            .header("x-amz-content-sha256", &payload);
        for (k, v) in extra {
            req = req.header(k, v);
        }
        let resp = req.send(body).map_err(transport)?;
        let code = resp.status().as_u16();
        if (200..300).contains(&code) {
            return Ok(());
        }
        // 409 = OSS 的 FileAlreadyExists;412 = S3/R2 对 If-None-Match 的
        // Precondition Failed。**两个都要认** —— 只认一个的话,在另一族服务端
        // 上「有人抢先推了」会被报成普通失败,重试逻辑不会启动。
        if code == 409 || code == 412 {
            return Err(CloudError::AlreadyExists);
        }
        Err(status_err(code, resp))
    }

    /// 列出 `prefix` 下的全部键(跟完续页令牌)。
    pub fn list_keys(&self, prefix: &str, amz_date: &str) -> Result<Vec<String>, CloudError> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut params: Vec<(&str, &str)> =
                vec![("list-type", "2"), ("prefix", prefix), ("max-keys", MAX_KEYS)];
            if let Some(t) = &token {
                params.push(("continuation-token", t));
            }
            let query = query_string(&params);
            // 列举的目标是 bucket 根,不是某个对象 —— key 传空串。
            let t = self.endpoint.target("");
            let payload = sha256_hex(b"");
            let auth = sigv4::authorization(
                &sigv4::Request {
                    method: "GET",
                    host: &t.host,
                    path: &t.path,
                    query: &query,
                    payload_sha256: &payload,
                    amz_date,
                    extra_headers: &[],
                },
                &self.creds.access_key_id,
                &self.creds.secret_access_key,
                &self.region,
                "s3",
            );
            let resp = self
                .agent
                .get(&format!("{}?{}", t.url, query))
                .header("authorization", &auth)
                .header("x-amz-date", amz_date)
                .header("x-amz-content-sha256", &payload)
                .call()
                .map_err(transport)?;
            let code = resp.status().as_u16();
            if !(200..300).contains(&code) {
                return Err(status_err(code, resp));
            }
            let text = body_text(resp);
            let page = xml::parse_list(&text)?;
            out.extend(page.keys);
            match page.next_token {
                Some(t) => token = Some(t),
                None => return Ok(out),
            }
        }
        Err(CloudError::Malformed(format!(
            "续页超过 {MAX_PAGES} 次仍未结束 —— 服务端可能在重复同一个令牌"
        )))
    }
}

fn transport(e: ureq::Error) -> CloudError {
    CloudError::Transport(e.to_string())
}

fn body_text(mut resp: http::Response<ureq::Body>) -> String {
    resp.body_mut().read_to_string().unwrap_or_default()
}

fn status_err(code: u16, resp: http::Response<ureq::Body>) -> CloudError {
    let body = body_text(resp);
    CloudError::Status {
        code,
        // 截断:错误正文会进日志,而对象存储偶尔会回几 KB 的 HTML 错误页。
        body: body.chars().take(400).collect(),
    }
}
```

**注意**：上面两处 `http::Response<ureq::Body>` 请**一律写成 `ureq::http::Response<ureq::Body>`**
（ureq 3 re-export 了 `http`）。**不要 `cargo add http`** —— 那会给本 crate 加一条
ADR-012 没登记过的直接依赖，而 `ureq` 本来就把它拉进来了，白多一笔。

`ureq::Agent::config_builder()` / `.timeout_global()` / `.http_status_as_error()` /
`Config` 到 `Agent` 的转换，**以 `cargo build` 的实际报错为准，不要猜**（本项目的
API 漂移纪律）。同一处连续改两次没过就停下来报告。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-cloud --test fake_s3 2>&1 | tail -30`
Expected: 5 passed。

- [ ] **Step 5: 提交并变异验证**

```bash
git add crates/mullion-cloud
git commit -m "feat(cloud): put_no_overwrite 与分页 list_keys + 假 S3 server (F270)"
```

| 变异 | 应该变红的测试 |
|---|---|
| `extra` 里删掉 `x-oss-forbid-overwrite` | `a_put_always_asks_the_server_to_refuse_overwriting` |
| `extra` 里删掉 `if-none-match` | `a_put_always_asks_the_server_to_refuse_overwriting`（第二条断言） |
| `code == 409 \|\| code == 412` 改成 `code == 412` | `a_409_becomes_already_exists_not_a_generic_status_error` |
| `status_err` 里 body 改成 `String::new()` | `an_error_response_carries_the_server_message` |
| `list_keys` 里 `match page.next_token` 改成直接 `return Ok(out)` | `listing_follows_the_continuation_token_until_it_is_gone` |
| `http_status_as_error(false)` 改成 `true` | `a_409_...` 与 `an_error_response_...` 都红 |

---

## Task 6: `Vault` 暴露主密钥封装能力

**Files:**
- Modify: `crates/mullion-store/src/vault.rs`

- [ ] **Step 1: 写失败测试**

追加到 `vault.rs` 的 `mod tests`：

```rust
/// 云端备份要把整包用**当前 vault 的密钥**封起来(设计 D5)。
///
/// 封出来的字节必须带 `secrets_file` 的文件头(salt + KDF 参数),因为另一台
/// 机器只有主密码,盐得从包里读回来 —— 盐不随包走的话,对面拿主密码派生出
/// 的是另一把钥匙,症状是「主密码明明没错却解不开」。
#[test]
fn a_blob_sealed_with_the_master_key_carries_the_salt_so_another_machine_can_open_it() {
    let dir = tempfile::tempdir().expect("临时目录");
    let mut v = Vault::open(dir.path().to_path_buf(), &key()).expect("开库");
    v.set_master_password("hunter2").expect("设主密码");

    let blob = v.seal_with_master(b"payload").expect("封装");
    let (scheme, _) = crate::secrets_file::parse(&blob).expect("解析头");
    assert!(
        scheme.has_password(),
        "封出来的东西没有口令头 —— 换台机器就解不开了"
    );

    // 「另一台机器」= 只有主密码,没有本机钥匙串。
    let crate::secrets_file::Scheme::Argon2id { params, salt } = scheme else {
        unreachable!()
    };
    let derived = crate::kdf::derive_key("hunter2", &salt, params).expect("派生");
    let payload = crate::secrets_file::parse(&blob).unwrap().1;
    assert_eq!(
        crate::crypto::decrypt(&derived, payload).expect("解密"),
        b"payload"
    );
}

/// 没设主密码时**拒绝封装**,不是照封(设计 D6)。
///
/// 钥匙串方案下 `key` 来自本机钥匙串,封出来的东西换台机器一个字都解不开 ——
/// 而云备份的主场景恰恰是「换新电脑」。照封的后果是用户以为自己有备份,
/// 直到真的换机那天才发现,且那时候旧机器可能已经不在了。
#[test]
fn sealing_is_refused_without_a_master_password_because_the_keyring_key_cannot_travel() {
    let dir = tempfile::tempdir().expect("临时目录");
    let v = Vault::open(dir.path().to_path_buf(), &key()).expect("开库");
    assert!(
        matches!(v.seal_with_master(b"x"), Err(StoreError::NoMasterPassword)),
        "钥匙串方案下必须拒绝封装"
    );
}

#[test]
fn a_blob_sealed_with_the_master_key_round_trips_locally() {
    let dir = tempfile::tempdir().expect("临时目录");
    let mut v = Vault::open(dir.path().to_path_buf(), &key()).expect("开库");
    v.set_master_password("hunter2").expect("设主密码");
    let blob = v.seal_with_master(b"round trip").expect("封装");
    assert_eq!(v.open_with_master(&blob).expect("解开"), b"round trip");
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-store vault:: 2>&1 | tail -20`
Expected: FAIL，找不到 `seal_with_master` / `StoreError::NoMasterPassword`。

- [ ] **Step 3: 加错误变体**

在 `crates/mullion-store/src/error.rs` 的 `StoreError` 里加：

```rust
    /// F270:这个操作要求库是主密码方案,而当前是钥匙串方案。
    ///
    /// **不是「加密失败」** —— 加密本身完全能做,只是封出来的东西离不开这台
    /// 机器。把它跟 `Crypto` 混成一条的话,UI 只能说「加密失败」,而用户需要
    /// 知道的是「去设一个主密码」。
    NoMasterPassword,
```

并在它的 `Display` impl 里加一条：

```rust
            StoreError::NoMasterPassword => write!(f, "这个操作需要先设置主密码"),
```

（已核实：`error.rs` 是**手写** `Display` + `std::error::Error`，不引 `thiserror`，
`#[derive(Debug)]` 只有 Debug。所以上面两段照抄即可，**不要**加 `#[error(..)]`。
加变体后若 `mullion-app` 里某处穷尽 `match` 编不过，补一条分支，**不要**改成 `_ =>`
把别的变体一起吞掉。）

- [ ] **Step 4: 实现 `Vault` 的两个方法**

在 `vault.rs` 里 `has_master_password` 附近加：

```rust
    /// F270:用**当前库的密钥**封一段字节,产出带 `secrets_file` 文件头的 blob。
    ///
    /// 文件头里的盐与 KDF 参数是**另一台机器能解开它的全部前提**:对面只有
    /// 主密码,盐得从 blob 里读回来。
    ///
    /// **钥匙串方案一律拒绝**(设计 D6):那把密钥躺在本机钥匙串里,封出来的
    /// 东西换台机器一个字都解不开,而云备份的主场景正是「换新电脑」。
    pub fn seal_with_master(&self, plain: &[u8]) -> Result<Vec<u8>, StoreError> {
        if !self.scheme.has_password() {
            return Err(StoreError::NoMasterPassword);
        }
        let payload = crypto::encrypt(&self.key, plain)?;
        Ok(crate::secrets_file::encode(&self.scheme, &payload))
    }

    /// [`Self::seal_with_master`] 的逆。**只解本机自己封的那一份** ——
    /// 用别的盐封的(换过主密码之前那些)在这里会失败,由调用方去解释成
    /// 「需要旧主密码」而不是「文件坏了」(设计 D13)。
    pub fn open_with_master(&self, blob: &[u8]) -> Result<Vec<u8>, StoreError> {
        let (_, payload) = crate::secrets_file::parse(blob)?;
        crypto::decrypt(&self.key, payload)
    }

    /// F270:当前的密钥方案。云端备份要拿它的 salt 判断「这份是不是本机
    /// 当前主密码封的」(设计 D14 的清理判据)。
    pub fn scheme(&self) -> crate::secrets_file::Scheme {
        self.scheme
    }
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p mullion-store vault:: 2>&1 | grep -E "test result|FAILED"`
Expected: 全绿。

- [ ] **Step 6: 提交并变异验证**

```bash
git add crates/mullion-store/src/vault.rs crates/mullion-store/src/error.rs
git commit -m "feat(store): Vault 暴露主密钥封装能力,钥匙串方案下拒绝 (F270)

钥匙串方案下封出来的东西换台机器解不开,而云备份的主场景正是换新电脑 ——
照封的后果是用户以为有备份,换机那天才发现(设计 D6)。"
```

| 变异 | 应该变红的测试 |
|---|---|
| `seal_with_master` 里去掉 `has_password` 检查 | `sealing_is_refused_without_a_master_password_because_the_keyring_key_cannot_travel` |
| `encode(&self.scheme, ..)` 改成 `encode(&Scheme::Keyring, ..)` | `a_blob_sealed_with_the_master_key_carries_the_salt_so_another_machine_can_open_it` |

---

## Task 7: 云端载荷的组装（去 layouts + 整体加密 + 指纹）

**Files:**
- Modify: `crates/mullion-store/src/portable.rs`
- Create: `crates/mullion-store/src/cloud.rs`
- Modify: `crates/mullion-store/src/lib.rs`
- Modify: `crates/mullion-store/Cargo.toml`

- [ ] **Step 1: 给 store 加 sha2 依赖**

`crates/mullion-store/Cargo.toml` 的 `[dependencies]` 里加：

```toml
# F270:云端载荷的内容指纹。**判据必须是内容指纹而不是列举式脏标记** ——
# 「列举式门控在加档时必然漏」本项目已经踩过三次,漏一次的后果是
# 「用户以为备份了其实没有」。0.10 已在 Cargo.lock(russh 拉的),不引新版本。
sha2.workspace = true
```

- [ ] **Step 2: 写失败测试（新建 `src/cloud.rs`，先只写测试与模块头）**

```rust
//! F270~F273:云端备份的数据层。**零网络** —— 真正的 PUT/LIST 在
//! `mullion-cloud`,这里只负责「装什么、封成什么、什么时候该推」。
//!
//! # 为什么云配置不住 settings.toml
//!
//! `settings.toml` **在同步包里**。AK/SK 进去的话,拉一份云端配置下来会把本机
//! 的 AK/SK 覆盖成云端那份的 —— 两台机用不同 RAM 子账号就串了;更糟的是云端
//! 那份若是几个月前推的、AK 已经轮换,拉下来本机拿到一把过期密钥,之后永远
//! 推不上去,而报的是 `403`,跟「配置被覆盖」毫无关联。
//!
//! endpoint / bucket / 游标同理:它们是「本机对云的看法」,不该被云端的内容
//! 覆盖。`cloud.toml` 不在 [`crate::portable::TOP_LEVEL_FILES`] 白名单里,
//! 于是 `install` 天然碰不到它。**加新配置文件时记得回来看这一条。**

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cloud_config_file_is_not_in_the_pack_whitelist() {
        assert!(
            !crate::portable::TOP_LEVEL_FILES.contains(&CLOUD_FILE),
            "cloud.toml 进了迁移包白名单 —— 导入一份包会把本机的 AK/SK 与游标覆盖掉"
        );
        assert!(
            crate::portable::entry_target(std::path::Path::new("/tmp/x"), CLOUD_FILE).is_none(),
            "entry_target 认了 cloud.toml —— 一个包就能改掉本机的云配置"
        );
    }

    /// 云端载荷**不带 layouts**(设计 D7):窗口几何与分屏树是本机属性,
    /// 且它是配置目录里变动最频繁的东西 —— 带上等于让「内容变了」近似恒真。
    #[test]
    fn the_cloud_payload_carries_no_layout_records() {
        let dir = tempfile::tempdir().expect("临时目录");
        std::fs::write(dir.path().join("sessions.toml"), b"x = 1").expect("写会话");
        let hd = crate::history::history_dir(dir.path());
        std::fs::create_dir_all(&hd).expect("建现场目录");
        std::fs::write(hd.join("123-4.toml"), b"y = 2").expect("写现场");

        let files = crate::portable::collect_top_level(dir.path());
        assert!(
            files.iter().all(|f| !f.path.contains(crate::history::HISTORY_DIR)),
            "云端载荷带上了现场记录:{:?}",
            files.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
        // 反过来:本地迁移包**仍然要带**(F46-a 的语义没变)。
        let local = crate::portable::collect(dir.path());
        assert!(
            local.iter().any(|f| f.path.contains(crate::history::HISTORY_DIR)),
            "本地迁移包不带现场了 —— 这是 F46-a 的回归,不是本切片该改的东西"
        );
    }

    /// 指纹必须**只由内容决定**:同样的内容算两次必须相等,否则定时器每一轮
    /// 都会认为「变了」,于是每 30 分钟推一份一模一样的包,N 份历史窗口当场
    /// 被自己刷光。
    #[test]
    fn the_fingerprint_is_stable_for_identical_content() {
        let a = vec![
            crate::portable::PackFile { path: "sessions.toml".into(), body: "AAA".into() },
            crate::portable::PackFile { path: "settings.toml".into(), body: "BBB".into() },
        ];
        assert_eq!(fingerprint(&a, b"secret"), fingerprint(&a, b"secret"));
    }

    /// 改任何一个文件的内容,指纹必须变 —— 否则那个文件的改动永远推不上去,
    /// 而且没有任何报错。
    #[test]
    fn the_fingerprint_changes_when_any_part_changes() {
        let base = vec![crate::portable::PackFile {
            path: "sessions.toml".into(),
            body: "AAA".into(),
        }];
        let base_fp = fingerprint(&base, b"secret");

        let mut body_changed = base.clone();
        body_changed[0].body = "AAB".into();
        assert_ne!(fingerprint(&body_changed, b"secret"), base_fp, "正文变了指纹没变");

        let mut path_changed = base.clone();
        path_changed[0].path = "settings.toml".into();
        assert_ne!(fingerprint(&path_changed, b"secret"), base_fp, "文件名变了指纹没变");

        assert_ne!(fingerprint(&base, b"other"), base_fp, "密文变了指纹没变");
    }

    /// 长度前缀**单独守一条**。
    ///
    /// 上一条测试杀不掉「把三处长度前缀删了」这个变异:`sessions.toml`+`AAA`
    /// 与 `settings.toml`+`AAA` 直接拼起来本来就不同,改动照样被看见。真正
    /// 的漏洞是**边界歧义** —— 不加前缀时 `("a","bc")` 与 `("ab","c")` 喂进
    /// 哈希的字节完全一样。症状:把一段内容从一个文件挪到另一个文件,指纹
    /// 不变,这次改动永远推不上去且零报错。
    #[test]
    fn the_fingerprint_separates_the_name_from_the_body() {
        let a = vec![crate::portable::PackFile { path: "a".into(), body: "bc".into() }];
        let b = vec![crate::portable::PackFile { path: "ab".into(), body: "c".into() }];
        assert_ne!(
            fingerprint(&a, b"s"),
            fingerprint(&b, b"s"),
            "名字与正文的边界没进哈希 —— 内容在文件之间搬家会被漏掉"
        );
    }

    /// 文件顺序不该影响指纹。`collect_top_level` 今天是定序的,但指纹是
    /// 「内容一样吗」的判据,让它依赖一个可能被重构掉的顺序,等于埋一颗
    /// 「某次无关重构之后每轮都重推」的雷。
    #[test]
    fn the_fingerprint_does_not_depend_on_file_order() {
        let a = vec![
            crate::portable::PackFile { path: "sessions.toml".into(), body: "AAA".into() },
            crate::portable::PackFile { path: "settings.toml".into(), body: "BBB".into() },
        ];
        let b = vec![a[1].clone(), a[0].clone()];
        assert_eq!(fingerprint(&a, b"s"), fingerprint(&b, b"s"));
    }
}
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p mullion-store cloud:: 2>&1 | tail -20`
Expected: FAIL（模块还没注册 / 找不到 `fingerprint` / `collect_top_level`）。

- [ ] **Step 4: 在 `portable.rs` 里抽出 `collect_top_level`**

把现有 `collect` 拆成两半（`collect` 复用新函数，行为不变）：

```rust
/// 只读顶层那三个文件。**云端载荷用的就是这个**(设计 D7:不带 `layouts/`)。
///
/// 抽出来而不是给 `collect` 加一个布尔参数:调用点读起来是
/// `collect_top_level(dir)` 而不是 `collect(dir, false)` —— 后者在调用点
/// 完全看不出那个 `false` 是什么意思。
pub fn collect_top_level(dir: &Path) -> Vec<PackFile> {
    let mut out = Vec::new();
    for name in TOP_LEVEL_FILES {
        if let Ok(bytes) = std::fs::read(dir.join(name)) {
            out.push(PackFile {
                path: (*name).to_string(),
                body: b64().encode(bytes),
            });
        }
    }
    out
}
```

并把 `collect` 的开头改成：

```rust
pub fn collect(dir: &Path) -> Vec<PackFile> {
    let mut out = collect_top_level(dir);
    // …以下 layouts 那一段原样保留…
```

- [ ] **Step 5: 实现 `cloud.rs` 的 `CLOUD_FILE` 与 `fingerprint`**

在 `cloud.rs` 的 `mod tests` 之前加：

```rust
use sha2::{Digest, Sha256};

/// 云配置文件名。**刻意不进 [`crate::portable::TOP_LEVEL_FILES`]**,见模块文档。
pub const CLOUD_FILE: &str = "cloud.toml";

/// 云端载荷的内容指纹(十六进制 SHA-256)。
///
/// **内容寻址,不是列举式脏标记**:后者在加新配置文件时必然漏一笔,而漏掉的
/// 后果是那个文件的改动永远推不上去且零报错(本项目「列举式门控」已踩三次)。
///
/// 排序之后再喂:指纹是「内容一样吗」的判据,让它依赖文件的枚举顺序,等于埋
/// 一颗「某次无关重构之后每一轮都重推」的雷 —— 而那会把 N 份历史窗口刷光。
pub fn fingerprint(files: &[crate::portable::PackFile], secrets: &[u8]) -> String {
    let mut sorted: Vec<&crate::portable::PackFile> = files.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut h = Sha256::new();
    for f in sorted {
        // 长度前缀:不加的话 ("ab","c") 与 ("a","bc") 算出同一个指纹,
        // 于是「把一段内容从一个文件挪到另一个文件」这种改动会被漏掉。
        h.update((f.path.len() as u64).to_le_bytes());
        h.update(f.path.as_bytes());
        h.update((f.body.len() as u64).to_le_bytes());
        h.update(f.body.as_bytes());
    }
    h.update((secrets.len() as u64).to_le_bytes());
    h.update(secrets);
    let mut s = String::with_capacity(64);
    for b in h.finalize() {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}
```

- [ ] **Step 6: 注册模块**

`crates/mullion-store/src/lib.rs`：`pub mod cloud;`（按字母序插在 `credential` 之前），
并在 `pub use` 区加：

```rust
pub use cloud::{fingerprint as cloud_fingerprint, CloudConfig, CLOUD_FILE};
```

（`CloudConfig` 在 Task 8 才有，先只导出 `fingerprint` 与 `CLOUD_FILE`，Task 8 再补。）

`crates/mullion-store/src/portable.rs` 的 `TOP_LEVEL_FILES` 上方加一句注释：

```rust
/// **`cloud.toml` 刻意不在这张表里**(F271):它是「本机对云的看法」,被云端
/// 内容覆盖会造出「拉一次就拿到过期 AK、之后永远推不上去且只报 403」。
```

- [ ] **Step 7: 跑测试确认通过**

Run: `cargo test -p mullion-store cloud:: 2>&1 | grep -E "test result|FAILED"`
Expected: 6 passed。

- [ ] **Step 8: 跑全量确认没碰坏 F46-a**

Run: `cargo test -p mullion-store > /tmp/store.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/store.log`
Expected: 全绿（尤其 `portable::tests` 与 `a_pack_travels_from_one_machine_to_another`）。

- [ ] **Step 9: 提交并变异验证**

```bash
git add crates/mullion-store
git commit -m "feat(store): 云端载荷的范围与内容指纹 (F272)

云端包不带 layouts(本机属性 + 变动最频繁),本地迁移包不变。
指纹走内容寻址而不是列举式脏标记 —— 后者加档必漏,漏掉的后果是
那个文件的改动永远推不上去且零报错。"
```

| 变异 | 应该变红的测试 |
|---|---|
| `fingerprint` 里 `sorted.sort_by(..)` 删掉 | `the_fingerprint_does_not_depend_on_file_order` |
| 只删 `h.update((f.path.len() ..))` 与 `h.update((f.body.len() ..))` 两处 | `the_fingerprint_separates_the_name_from_the_body` |
| `h.update(secrets)` 删掉 | `the_fingerprint_changes_when_any_part_changes` |
| `collect_top_level` 里补上 layouts 那一段 | `the_cloud_payload_carries_no_layout_records` |
| `TOP_LEVEL_FILES` 里加上 `CLOUD_FILE` | `the_cloud_config_file_is_not_in_the_pack_whitelist` |

**`h.update((secrets.len() ..))` 那一处是等价变异,不要为它编守护。** 密文是喂进
哈希的最后一段,前面每个文件的 path/body 都已带长度前缀,把它的长度删掉产不出
任何一对可区分的输入 —— 留着它是为了「以后在密文后面再追加字段」时不必回头
重想边界,不是因为今天有哪条输入靠它分开。跑这条变异会全绿,**那是正确的**,
别改测试去凑红。

---

## Task 8: `CloudConfig` 与 `cloud.toml` 的读-改-写

**Files:**
- Modify: `crates/mullion-store/src/cloud.rs`

- [ ] **Step 1: 写失败测试**

追加到 `cloud.rs` 的 `mod tests`：

```rust
    fn cfg() -> CloudConfig {
        CloudConfig {
            enabled: true,
            endpoint: "https://oss-cn-hangzhou.aliyuncs.com".into(),
            region: "cn-hangzhou".into(),
            bucket: "my-bucket".into(),
            prefix: "mullion/".into(),
            path_style: false,
            keep: 20,
            interval_min: 30,
            access_key_id: String::new(),
            secret_sealed: String::new(),
            last_fingerprint: String::new(),
            last_seq: 0,
            last_ok_at: String::new(),
            corrupt: false,
        }
    }

    #[test]
    fn a_config_round_trips_through_the_file() {
        let dir = tempfile::tempdir().expect("临时目录");
        save(dir.path(), &cfg()).expect("写");
        assert_eq!(load(dir.path()), cfg());
    }

    /// 没有文件时给一份**关着的**默认配置 —— 不是「开着但字段是空的」。
    /// 后者会让定时器每一轮都尝试连一个空 endpoint,状态栏一直报错。
    #[test]
    fn a_missing_file_yields_a_disabled_default() {
        let dir = tempfile::tempdir().expect("临时目录");
        let c = load(dir.path());
        assert!(!c.enabled, "默认必须是关着的");
        assert_eq!(c.prefix, DEFAULT_PREFIX);
        assert_eq!(c.keep, DEFAULT_KEEP);
    }

    /// 坏文件**不许当成默认值**(F247/F248 的「整份覆盖」缺陷族):
    /// 照 default 兜底的话,下一次 `save` 会把用户手打的 endpoint/bucket
    /// 连同 AK/SK 一起抹掉,而这一切零报错。
    #[test]
    fn a_corrupt_file_is_not_silently_replaced_by_defaults() {
        let dir = tempfile::tempdir().expect("临时目录");
        std::fs::write(dir.path().join(CLOUD_FILE), "这不是 toml { [").expect("写坏文件");
        let c = load(dir.path());
        assert!(!c.enabled, "读不懂的配置必须当成关着的");
        assert!(
            save(dir.path(), &c).is_err(),
            "读坏之后还允许回写 —— 那会把用户的 AK/SK 静默抹掉"
        );
        // 标记**不许寄生在数据字段上**:设置弹窗把 `endpoint` 直接绑到文本框,
        // 用户改一下就把标记冲掉,`save` 当场放行、`secret_sealed` 被抹。
        assert!(
            c.endpoint.is_empty(),
            "损坏标记污染了 endpoint:{:?} —— 它会出现在设置弹窗的输入框里,\
             而用户改掉它就等于把守护关掉了",
            c.endpoint
        );
    }

    /// 坏标记**不能落盘**。写出去的话下次 `load` 会把一份好文件读成坏的,
    /// 于是云备份从此永久拒绝回写,且没有任何办法自愈。
    #[test]
    fn the_corrupt_mark_never_reaches_the_file() {
        let dir = tempfile::tempdir().expect("临时目录");
        save(dir.path(), &cfg()).expect("写");
        let text = std::fs::read_to_string(dir.path().join(CLOUD_FILE)).expect("读");
        assert!(!text.contains("corrupt"), "损坏标记落盘了:{text}");
    }

    /// 游标(`last_seq` / `last_fingerprint`)是写在这个文件里的,而这个文件
    /// 不进包 —— 这条钉住的是「有人以后为了省事把游标挪进 settings.toml」。
    #[test]
    fn the_cursor_lives_in_the_file_that_the_pack_cannot_touch() {
        let dir = tempfile::tempdir().expect("临时目录");
        let mut c = cfg();
        c.last_seq = 42;
        c.last_fingerprint = "deadbeef".into();
        save(dir.path(), &c).expect("写");
        let text = std::fs::read_to_string(dir.path().join(CLOUD_FILE)).expect("读");
        assert!(text.contains("last_seq"), "游标没落在 cloud.toml 里");
        let settings = dir.path().join(crate::settings::SETTINGS_FILE);
        assert!(
            !settings.exists() || !std::fs::read_to_string(&settings).unwrap().contains("last_seq"),
            "游标漏进了 settings.toml —— 那个文件会被导入的包整份替换掉"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-store cloud:: 2>&1 | tail -20`
Expected: FAIL，找不到 `CloudConfig`。

- [ ] **Step 3: 实现**

在 `cloud.rs` 里加（`mod tests` 之前）：

```rust
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::StoreError;

/// 对象键的默认前缀。
pub const DEFAULT_PREFIX: &str = "mullion/";
/// 默认保留几份。
pub const DEFAULT_KEEP: u32 = 20;
/// 默认多久算一次指纹(分钟)。
pub const DEFAULT_INTERVAL_MIN: u32 = 30;

/// 本机对云端的全部看法。**整份住 `cloud.toml`,不进迁移包**,见模块文档。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub bucket: String,
    #[serde(default = "default_prefix")]
    pub prefix: String,
    /// `true` = `<endpoint>/<bucket>/<key>`。自建 MinIO 多半要打开。
    #[serde(default)]
    pub path_style: bool,
    #[serde(default = "default_keep")]
    pub keep: u32,
    #[serde(default = "default_interval")]
    pub interval_min: u32,
    /// AK 是标识不是秘密,明文存。
    #[serde(default)]
    pub access_key_id: String,
    /// SK **用 vault key 封过再 base64**(F271)。空 = 还没填。
    #[serde(default)]
    pub secret_sealed: String,
    /// 上次成功推上去的那一份的内容指纹。
    #[serde(default)]
    pub last_fingerprint: String,
    /// 上次成功推上去的序号。
    #[serde(default)]
    pub last_seq: u64,
    /// 上次成功的时刻(RFC3339)。只给状态栏看。
    #[serde(default)]
    pub last_ok_at: String,
    /// 这份是从**读不懂的文件**上来的。`save` 见到它就拒绝写。
    ///
    /// **不落盘**(`serde(skip)`),**私有**(只有 [`load`] 能置位)。
    ///
    /// 为什么标记住在结构体里而不是让 `load` 返回 `Result`:守护必须待在
    /// `save` 内部。挪到调用方就成了「每个调用点都要记得判一下」,而这正是
    /// 本项目已经踩过三次的「列举式门控在加档时必然漏」。
    ///
    /// 为什么**不**把标记塞进 `endpoint` 之类的数据字段:设置弹窗直接把
    /// `endpoint` 绑到文本框(Task 12)。塞进去的话用户会在输入框里看见那串
    /// 哨兵,而他只要改一下 endpoint 就把标记冲掉了 —— `save` 当场放行,
    /// `secret_sealed` 连同别的字段一起被默认值抹掉。守护在它最该生效的
    /// 那条路上恰好失效。
    #[serde(skip)]
    corrupt: bool,
}

fn default_prefix() -> String {
    DEFAULT_PREFIX.to_string()
}
fn default_keep() -> u32 {
    DEFAULT_KEEP
}
fn default_interval() -> u32 {
    DEFAULT_INTERVAL_MIN
}

impl Default for CloudConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: String::new(),
            region: String::new(),
            bucket: String::new(),
            prefix: default_prefix(),
            path_style: false,
            keep: default_keep(),
            interval_min: default_interval(),
            access_key_id: String::new(),
            secret_sealed: String::new(),
            last_fingerprint: String::new(),
            last_seq: 0,
            last_ok_at: String::new(),
            corrupt: false,
        }
    }
}

/// 读 `cloud.toml`。读不懂时返回一份**关着且不可回写**的配置。
pub fn load(dir: &Path) -> CloudConfig {
    let path = dir.join(CLOUD_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return CloudConfig::default();
    };
    match toml::from_str::<CloudConfig>(&text) {
        Ok(c) => c,
        Err(_) => CloudConfig {
            corrupt: true,
            ..CloudConfig::default()
        },
    }
}

/// 写 `cloud.toml`。
pub fn save(dir: &Path, cfg: &CloudConfig) -> Result<(), StoreError> {
    if cfg.corrupt {
        return Err(StoreError::CorruptSecrets(
            "cloud.toml 读不懂,拒绝回写 —— 先把它改好或删掉".into(),
        ));
    }
    let text = toml::to_string_pretty(cfg)?;
    crate::vault::write_atomic(&dir.join(CLOUD_FILE), text.as_bytes())
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-store cloud:: 2>&1 | grep -E "test result|FAILED"`
Expected: 11 passed（Task 7 的 6 条 + 本任务的 5 条）。

- [ ] **Step 5: 补导出**

`lib.rs` 的 `pub use cloud::{...}` 补全成：

```rust
pub use cloud::{
    fingerprint as cloud_fingerprint, load as load_cloud_config, save as save_cloud_config,
    CloudConfig, CLOUD_FILE, DEFAULT_INTERVAL_MIN, DEFAULT_KEEP, DEFAULT_PREFIX,
};
```

- [ ] **Step 6: 提交并变异验证**

```bash
git add crates/mullion-store
git commit -m "feat(store): cloud.toml 的读-改-写,坏文件拒绝回写 (F271)"
```

| 变异 | 应该变红的测试 |
|---|---|
| `load` 的 `Err` 分支改成 `CloudConfig::default()` | `a_corrupt_file_is_not_silently_replaced_by_defaults` |
| `save` 里去掉 `if cfg.corrupt` 检查 | 同上 |
| `corrupt` 字段上的 `#[serde(skip)]` 去掉 | `the_corrupt_mark_never_reaches_the_file` |
| `load` 的 `Err` 分支改成 `endpoint: "\u{0}corrupt".into(), ..` 式的哨兵（即原设计） | `a_corrupt_file_is_not_silently_replaced_by_defaults`（endpoint 那条新断言） |
| `default()` 里 `enabled: true` | `a_missing_file_yields_a_disabled_default` |

---

## Task 9: AK/SK 的封与解 + 主密码变更时连带重封

**Files:**
- Modify: `crates/mullion-store/src/cloud.rs`
- Modify: `crates/mullion-store/src/vault.rs`

- [ ] **Step 1: 写失败测试**

追加到 `cloud.rs` 的 `mod tests`：

```rust
    /// SK 落盘必须是密文。这条同 `tests/f70_no_plaintext.rs` 的姿态:
    /// 在**文件字节**里搜明文,而不是相信调用链。
    #[test]
    fn the_secret_key_never_hits_the_disk_in_the_clear() {
        let dir = tempfile::tempdir().expect("临时目录");
        let mut v = crate::vault::Vault::open(
            dir.path().to_path_buf(),
            &crate::master_key::InMemoryKey([5u8; 32]),
        )
        .expect("开库");
        v.set_master_password("hunter2").expect("设主密码");

        let mut c = cfg();
        set_secret_key(&mut c, &v, "TOP-SECRET-SK-VALUE").expect("封 SK");
        save(dir.path(), &c).expect("写");

        let bytes = std::fs::read(dir.path().join(CLOUD_FILE)).expect("读");
        assert!(
            !String::from_utf8_lossy(&bytes).contains("TOP-SECRET-SK-VALUE"),
            "SK 明文落到了 cloud.toml 里"
        );
        assert_eq!(secret_key(&c, &v).expect("解 SK"), "TOP-SECRET-SK-VALUE");
    }

    /// **改主密码必须连带重封 `cloud.toml`**(设计 D12)。
    ///
    /// 不重封的症状:AK/SK 当场解不开,云备份静默失效,而错误要等到几十分钟后
    /// 的一次定时上传才冒出来 —— 那时候用户早就不记得自己改过主密码了。
    #[test]
    fn changing_the_master_password_reseals_the_cloud_secret() {
        let dir = tempfile::tempdir().expect("临时目录");
        let mut v = crate::vault::Vault::open(
            dir.path().to_path_buf(),
            &crate::master_key::InMemoryKey([5u8; 32]),
        )
        .expect("开库");
        v.set_master_password("old").expect("设旧密码");

        let mut c = cfg();
        set_secret_key(&mut c, &v, "SK-VALUE").expect("封");
        save(dir.path(), &c).expect("写");

        v.set_master_password("new").expect("改密码");

        let after = load(dir.path());
        assert_eq!(
            secret_key(&after, &v).expect("改完密码之后应该还解得开"),
            "SK-VALUE",
            "改主密码没有重封 cloud.toml —— AK/SK 从此解不开且零报错"
        );
    }

    /// `clear_master_password` 走同一条路:退回钥匙串方案之后,SK 必须仍然
    /// 解得开(它改用钥匙串密钥封),否则用户只是「取消了主密码」,云配置
    /// 却连带坏掉。
    #[test]
    fn clearing_the_master_password_also_reseals_the_cloud_secret() {
        let dir = tempfile::tempdir().expect("临时目录");
        let ks = crate::master_key::InMemoryKey([5u8; 32]);
        let mut v = crate::vault::Vault::open(dir.path().to_path_buf(), &ks).expect("开库");
        v.set_master_password("old").expect("设密码");
        let mut c = cfg();
        set_secret_key(&mut c, &v, "SK-VALUE").expect("封");
        save(dir.path(), &c).expect("写");

        v.clear_master_password(&ks).expect("取消主密码");

        let after = load(dir.path());
        assert_eq!(secret_key(&after, &v).expect("仍应解得开"), "SK-VALUE");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-store cloud:: 2>&1 | tail -20`
Expected: FAIL，找不到 `set_secret_key`。

- [ ] **Step 3: 实现 `set_secret_key` / `secret_key`**

在 `cloud.rs` 里加：

```rust
use base64::Engine as _;

fn b64() -> base64::engine::general_purpose::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// 把 SK 用 vault 的密钥封进 `cfg.secret_sealed`。
///
/// **用 `seal_local` 而不是 `seal_with_master`**:`cloud.toml` 是本机文件,
/// 不跟着任何包走,所以钥匙串方案下也该能存 —— 用户可以先填好云配置,
/// 再去设主密码。云备份**本身**要求主密码(设计 D6),但那道闸在上传那一步,
/// 不该把「填配置」也一起挡掉。
pub fn set_secret_key(
    cfg: &mut CloudConfig,
    vault: &crate::vault::Vault,
    sk: &str,
) -> Result<(), StoreError> {
    cfg.secret_sealed = b64().encode(vault.seal_local(sk.as_bytes())?);
    Ok(())
}

/// 取回 SK 明文。空 = 还没填过(**不是错误**)。
pub fn secret_key(
    cfg: &CloudConfig,
    vault: &crate::vault::Vault,
) -> Result<String, StoreError> {
    if cfg.secret_sealed.is_empty() {
        return Ok(String::new());
    }
    let blob = b64()
        .decode(&cfg.secret_sealed)
        .map_err(|e| StoreError::CorruptSecrets(format!("cloud.toml 的密文不是合法 base64:{e}")))?;
    let plain = vault.open_local(&blob)?;
    String::from_utf8(plain).map_err(StoreError::from)
}
```

- [ ] **Step 4: 在 `Vault` 上加 `seal_local` / `open_local`**

`vault.rs`，紧挨着 `seal_with_master`：

```rust
    /// F271:用本库当前的密钥封一段**本机文件**用的字节。
    ///
    /// 与 [`Self::seal_with_master`] 的区别只有一个:**不要求主密码方案**。
    /// 封出来的东西只给本机的 `cloud.toml` 用,不跟着任何包走,所以钥匙串
    /// 方案下也成立。
    pub fn seal_local(&self, plain: &[u8]) -> Result<Vec<u8>, StoreError> {
        let payload = crypto::encrypt(&self.key, plain)?;
        Ok(crate::secrets_file::encode(&self.scheme, &payload))
    }

    /// [`Self::seal_local`] 的逆。
    pub fn open_local(&self, blob: &[u8]) -> Result<Vec<u8>, StoreError> {
        let (_, payload) = crate::secrets_file::parse(blob)?;
        crypto::decrypt(&self.key, payload)
    }
```

- [ ] **Step 5: 在 `set_master_password` / `clear_master_password` 里连带重封**

在 `vault.rs` 两个方法**换掉 `self.key` 与 `self.scheme` 之前**，先把 SK 解出来；
换完之后再封回去、落盘。抽一个私有辅助函数，两处共用（别各写一遍 —— 各写一遍
的话 `clear` 那条迟早被漏掉，而那正是这条测试要挡的东西）：

```rust
    /// F271 / 设计 D12:密钥换了之后,把 `cloud.toml` 里的 SK 用新密钥重封。
    ///
    /// **两个改密码的入口共用这一份。** 各写一遍的话 `clear_master_password`
    /// 那条迟早被漏掉,而漏掉的症状是:用户取消主密码之后云备份静默失效,
    /// 报错要等几十分钟后的一次定时上传才冒出来。
    ///
    /// 失败只报 `Err` **不回滚主密码**:主密码已经换好并落盘了,回滚意味着
    /// 再写一次文件,失败链条更长。云配置坏了是可修的(重填一次 AK/SK),
    /// 主密码写到一半不是。
    fn reseal_cloud_secret(&mut self, old_plain: Option<Vec<u8>>) -> Result<(), StoreError> {
        let Some(plain) = old_plain else { return Ok(()) };
        let mut cfg = crate::cloud::load(&self.dir);
        if cfg.secret_sealed.is_empty() {
            return Ok(());
        }
        cfg.secret_sealed = {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.encode(self.seal_local(&plain)?)
        };
        crate::cloud::save(&self.dir, &cfg)
    }

    /// 换密钥**之前**把 SK 解出来。解不开(从没填过/文件坏了)一律 `None` ——
    /// 那种情况下没有东西需要重封。
    fn take_cloud_secret_plain(&self) -> Option<Vec<u8>> {
        let cfg = crate::cloud::load(&self.dir);
        if cfg.secret_sealed.is_empty() {
            return None;
        }
        use base64::Engine as _;
        let blob = base64::engine::general_purpose::STANDARD
            .decode(&cfg.secret_sealed)
            .ok()?;
        self.open_local(&blob).ok()
    }
```

两个方法今天**都以 `self.save()` 收尾**（已核实），所以收尾那一行要改成先 `?`
再重封。完整形状：

```rust
    pub fn set_master_password(&mut self, password: &str) -> Result<(), StoreError> {
        self.sync_from_disk_if_untouched();
        if password.is_empty() {
            return Err(StoreError::Kdf("主密码不能为空".into()));
        }
        // **必须在换 key 之前解**:换完就再也解不开了。放在空密码检查之后,
        // 省掉一次注定作废的文件读。
        let carried = self.take_cloud_secret_plain();
        let params = crate::kdf::KdfParams::default();
        let salt = crate::kdf::random_salt();
        let key = crate::kdf::derive_key(password, &salt, params)?;
        self.key = key;
        self.scheme = crate::secrets_file::Scheme::Argon2id { params, salt };
        self.save()?;
        self.reseal_cloud_secret(carried)
    }

    pub fn clear_master_password(
        &mut self,
        key_source: &dyn MasterKeySource,
    ) -> Result<(), StoreError> {
        self.sync_from_disk_if_untouched();
        // 同上:`key_source.load_or_create()` 失败时会带着 `?` 提前返回,
        // 那条路上 `carried` 直接被丢掉,本来也没东西要重封。
        let carried = self.take_cloud_secret_plain();
        let key = key_source.load_or_create()?;
        self.key = key;
        self.scheme = crate::secrets_file::Scheme::Keyring;
        self.save()?;
        self.reseal_cloud_secret(carried)
    }
```

**`self.save()?` 那个 `?` 是新加的**（原来是 `self.save()` 直接当返回值）。漏掉它
的症状：`save` 失败时重封仍然照跑，`cloud.toml` 被新密钥重封而 `secrets.enc`
还是老的——两个文件从此对不上。

`sync_from_disk_if_untouched()` 保持在最前，位置别动——它碰的是 `sessions.toml`
与 `secrets.enc`，与 `cloud.toml` 无关，两者独立；不动它单纯是 Scope Discipline
（本任务没有理由改它的时机）。

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test -p mullion-store 2>&1 | grep -E "test result|FAILED"`
Expected: 全绿。

- [ ] **Step 7: 提交并变异验证**

```bash
git add crates/mullion-store
git commit -m "feat(store): AK/SK 用 vault key 封存,改主密码时连带重封 (F271)

不重封的症状是 AK/SK 当场解不开、云备份静默失效,而错误要等几十分钟后
的一次定时上传才冒出来(设计 D12)。两个改密码入口共用一份重封代码 ——
各写一遍的话 clear 那条迟早被漏掉。"
```

| 变异 | 应该变红的测试 |
|---|---|
| `set_master_password` 里去掉 `reseal_cloud_secret` 那句 | `changing_the_master_password_reseals_the_cloud_secret` |
| `clear_master_password` 里去掉那句 | `clearing_the_master_password_also_reseals_the_cloud_secret` |
| `set_secret_key` 改成直接 `cfg.secret_sealed = sk.to_string()` | `the_secret_key_never_hits_the_disk_in_the_clear` |

---

## Task 10: 对象键命名、序号推进、上传决策

**Files:**
- Modify: `crates/mullion-store/src/cloud.rs`

- [ ] **Step 1: 写失败测试**

追加到 `cloud.rs` 的 `mod tests`：

```rust
    /// 序号必须**零填充定宽**。不填充的话字典序是 `1, 10, 2` ——
    /// 而「最新那份」= List 结果里序号最大的那条,靠的正是字典序。
    #[test]
    fn the_sequence_number_is_zero_padded_so_lexical_order_equals_numeric_order() {
        let a = object_key("mullion/", 2, "20260915T101500Z");
        let b = object_key("mullion/", 10, "20260915T101500Z");
        assert!(a < b, "字典序与数值序不一致:{a} 应该排在 {b} 前面");
    }

    #[test]
    fn a_key_round_trips_through_parse() {
        let k = object_key("mullion/", 42, "20260915T101500Z");
        assert_eq!(parse_seq("mullion/", &k), Some(42));
    }

    /// 别人往同一个前缀下丢了别的文件时,不认识的键**跳过**而不是
    /// 让整次上传失败 —— bucket 是用户自己的,里头有什么我们管不着。
    #[test]
    fn a_key_we_do_not_recognise_is_skipped_not_fatal() {
        assert_eq!(parse_seq("mullion/", "mullion/readme.txt"), None);
        assert_eq!(parse_seq("mullion/", "other/000001-x.mpk"), None);
    }

    /// 下一个序号 = 已有的最大值 + 1。**空列表从 1 起**,不是 0 ——
    /// 0 与「没推过」的游标初值撞在一起,分不出「从没推过」和「推过第 0 份」。
    #[test]
    fn the_next_sequence_is_one_past_the_largest_existing() {
        assert_eq!(next_seq("mullion/", &[]), 1);
        assert_eq!(
            next_seq(
                "mullion/",
                &[
                    "mullion/000001-a.mpk".to_string(),
                    "mullion/000007-b.mpk".to_string(),
                    "mullion/000003-c.mpk".to_string(),
                ]
            ),
            8
        );
    }

    /// 一份**填完整了**的配置。
    ///
    /// **`should_upload` 的测试一律用这个,不要用 `cfg()`。** `cfg()` 的
    /// `access_key_id` / `secret_sealed` 是空的(Task 8 那几条测的是读写往返,
    /// 不需要填),而 `should_upload` 的第一道闸就是「配置完不完整」——
    /// 拿 `cfg()` 去测的话,`a_changed_fingerprint_...` 会直接红,而
    /// `an_unchanged_fingerprint_...` 会**恒绿**:它返回 false 是因为「没填完」,
    /// 跟指纹判据一点关系都没有,把指纹那一条整个删掉它照样绿。
    fn ready_cfg() -> CloudConfig {
        CloudConfig {
            access_key_id: "AK".into(),
            secret_sealed: "sealed".into(),
            ..cfg()
        }
    }

    /// 先钉住 `ready_cfg` 真的是「会推」的那一档 —— 否则下面每一条
    /// `assert!(!should_upload(..))` 都可能是因为别的原因恒假。
    #[test]
    fn the_ready_config_is_actually_uploadable() {
        assert!(
            should_upload(&ready_cfg(), "brand-new", 999),
            "ready_cfg 本身就推不动 —— 下面那几条「不推」的断言全都测不到自己想测的东西"
        );
    }

    /// 关着的时候永远不推 —— 哪怕内容变了。
    #[test]
    fn a_disabled_config_never_uploads() {
        let mut c = ready_cfg();
        c.enabled = false;
        assert!(!should_upload(&c, "new-fp", 999));
    }

    /// 指纹没变就不推。**这是保住 N 份历史窗口的全部** —— 不判的话每 30 分钟
    /// 推一份一模一样的包,20 份历史会在 10 小时内被自己刷光。
    #[test]
    fn an_unchanged_fingerprint_does_not_upload() {
        let mut c = ready_cfg();
        c.last_fingerprint = "same".into();
        assert!(!should_upload(&c, "same", 999));
    }

    #[test]
    fn a_changed_fingerprint_uploads_once_the_interval_has_passed() {
        let mut c = ready_cfg();
        c.last_fingerprint = "old".into();
        c.interval_min = 30;
        assert!(!should_upload(&c, "new", 29), "还没到点就推了");
        assert!(should_upload(&c, "new", 30), "到点了却不推");
    }

    /// 从没推过(指纹为空)时,**到点就推第一份**。
    /// 若写成「指纹为空 → 不推」,开了开关的用户永远等不到第一份备份。
    #[test]
    fn a_config_that_never_uploaded_still_gets_its_first_push() {
        let mut c = ready_cfg();
        c.last_fingerprint = String::new();
        assert!(should_upload(&c, "first", 30));
    }

    /// 配置不全(endpoint/bucket/AK/SK 任一为空)时不推 —— 推了也只会拿到一条
    /// 网络错误,而状态栏会把它报成「备份失败」,掩盖真正的原因是「没填完」。
    ///
    /// **四个字段逐个试**,不是只试一个:少判任一个的症状都一样(开着开关、
    /// 每轮都发一次注定 403 的请求),而只试一个的话漏掉的那几个零报错。
    #[test]
    fn an_incomplete_config_does_not_upload() {
        for spoil in ["endpoint", "bucket", "ak", "sk"] {
            let mut c = ready_cfg();
            match spoil {
                "endpoint" => c.endpoint = String::new(),
                "bucket" => c.bucket = String::new(),
                "ak" => c.access_key_id = String::new(),
                _ => c.secret_sealed = String::new(),
            }
            assert!(!should_upload(&c, "fp", 999), "{spoil} 为空时不该推");
        }
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-store cloud:: 2>&1 | tail -20`
Expected: FAIL，找不到 `object_key`。

- [ ] **Step 3: 实现**

```rust
/// 序号在键里占几位。6 位 = 一百万份,以每 30 分钟一份算够用 57 年。
///
/// **定宽零填充是硬要求**:「最新那份」= List 结果里序号最大的那条,而
/// ListObjectsV2 按**字典序**返回 —— 不填充的话 `10` 会排在 `2` 前面。
const SEQ_WIDTH: usize = 6;

/// 云端对象的扩展名。
const OBJ_EXT: &str = ".mpk";

/// 一份备份的对象键。
pub fn object_key(prefix: &str, seq: u64, stamp: &str) -> String {
    format!("{prefix}{seq:0SEQ_WIDTH$}-{stamp}{OBJ_EXT}")
}

/// 从对象键里抠出序号。不是我们生成的键 → `None`(跳过,不是错误:
/// bucket 是用户自己的,里头有什么我们管不着)。
pub fn parse_seq(prefix: &str, key: &str) -> Option<u64> {
    let rest = key.strip_prefix(prefix)?;
    let rest = rest.strip_suffix(OBJ_EXT)?;
    let (seq, _stamp) = rest.split_once('-')?;
    if seq.len() != SEQ_WIDTH || !seq.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    seq.parse().ok()
}

/// 下一个该用的序号。**空列表从 1 起** —— 0 与「从没推过」的游标初值撞在
/// 一起,那两种状态就再也分不开了。
pub fn next_seq(prefix: &str, keys: &[String]) -> u64 {
    keys.iter()
        .filter_map(|k| parse_seq(prefix, k))
        .max()
        .map_or(1, |m| m + 1)
}

/// 这一轮该不该推。**纯函数** —— 时钟由调用方折算成
/// `minutes_since_last_ok` 传进来(store 不持时钟)。
///
/// 判据顺序是有意的:先看开关、再看配置完不完整、再看指纹、最后才看时间。
/// 「配置没填完」与「指纹没变」在 UI 上要说不同的话,混成一条的话状态栏只能
/// 报「备份失败」,把真正的原因吃掉。
pub fn should_upload(cfg: &CloudConfig, now_fingerprint: &str, minutes_since_last_ok: u64) -> bool {
    if !cfg.enabled {
        return false;
    }
    if cfg.endpoint.is_empty()
        || cfg.bucket.is_empty()
        || cfg.access_key_id.is_empty()
        || cfg.secret_sealed.is_empty()
    {
        return false;
    }
    if cfg.last_fingerprint == now_fingerprint {
        return false;
    }
    minutes_since_last_ok >= u64::from(cfg.interval_min)
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-store cloud:: 2>&1 | grep -E "test result|FAILED"`
Expected: 全绿。

- [ ] **Step 5: 提交并变异验证**

```bash
git add crates/mullion-store/src/cloud.rs
git commit -m "feat(store): 对象键命名、序号推进与上传决策 (F273)"
```

| 变异 | 应该变红的测试 |
|---|---|
| `{seq:0SEQ_WIDTH$}` 改成 `{seq}` | `the_sequence_number_is_zero_padded_so_lexical_order_equals_numeric_order` |
| `next_seq` 的 `map_or(1, ..)` 改成 `map_or(0, ..)` | `the_next_sequence_is_one_past_the_largest_existing` |
| `should_upload` 里删掉指纹那条 | `an_unchanged_fingerprint_does_not_upload` |
| `should_upload` 里删掉配置完整性那一段 | `an_incomplete_config_does_not_upload` |
| 完整性那段里**只删 `secret_sealed.is_empty()` 一项** | `an_incomplete_config_does_not_upload`（`sk` 那一轮） |
| `>=` 改成 `>` | `a_changed_fingerprint_uploads_once_the_interval_has_passed` |
| `!cfg.enabled` 那条改成 `false`（即永不因开关拦下） | `a_disabled_config_never_uploads` |

**`ready_cfg` 是这一组的前提，不是装饰。** 跑变异前先确认
`the_ready_config_is_actually_uploadable` 是绿的；它一红，下面每条
「不推」的断言都失去意义（会因为别的原因恒假），此时任何变异结果都不算数。

---

## Task 11: app —— 上传编排（`cloudsync.rs`）

**Files:**
- Create: `crates/mullion-app/src/cloudsync.rs`
- Modify: `crates/mullion-app/src/lib.rs`
- Modify: `crates/mullion-app/Cargo.toml`

- [ ] **Step 1: 给 app 加 cloud 依赖**

`crates/mullion-app/Cargo.toml` 的 `[dependencies]` 里加：

```toml
# F270:云端备份。`app` 是唯一允许同时知道 store 与 cloud 的地方 ——
# cloud 只认字节与键名,不认识 Pack(架构不变量,见 mullion-cloud 的 lib.rs)。
mullion-cloud = { path = "../mullion-cloud" }
```

- [ ] **Step 2: 写失败测试**

Create `crates/mullion-app/src/cloudsync.rs`：

```rust
//! F273:云端备份的上传编排。
//!
//! **`mullion-store` 负责「装什么、封成什么」,`mullion-cloud` 负责「送出去」,
//! 这里是唯一同时知道两者的地方**(架构不变量:app 是唯一允许知道其余
//! 几个 crate 的地方)。
//!
//! # 阻塞
//!
//! `mullion-cloud` 是阻塞式的(ureq)。[`upload_blocking`] 必须在
//! `tokio::task::spawn_blocking` 里调用 —— 在事件循环里同步跑网络会把帧率
//! 打到零(T3/T7 红线)。这个约束靠 `app.rs` 那边的调用点守着。

#[cfg(test)]
mod tests {
    use super::*;

    /// 撞上「序号已被占用」时必须**重新 List、换一个序号再试**,而不是
    /// 直接报失败。这正是追加式布局在没有 CAS 的服务端上的全部并发保护
    /// (设计 D8:OSS 的 PutObject 没有 If-Match)。
    #[test]
    fn a_taken_sequence_number_is_retried_with_a_fresh_one() {
        let plan = plan_after_collision(3);
        assert_eq!(plan, Some(4), "撞号之后没有往前挪");
    }

    /// 重试必须有上限。没有上限的话,一个总是回 409 的服务端会让这个
    /// 后台线程永远转下去,而用户只看得见「备份一直在转」。
    #[test]
    fn retries_are_bounded_so_a_always_409_server_cannot_spin_forever() {
        assert!(MAX_PUT_ATTEMPTS >= 2, "至少要能重试一次");
        assert!(MAX_PUT_ATTEMPTS <= 8, "上限太高,等于没有上限");
    }

    /// 成功之后游标必须**同时**推进指纹与序号。
    ///
    /// 只推指纹的话:下一次会算出同一个序号,`ForbidOverwrite` 把它挡掉,
    /// 表现成「备份莫名其妙失败」。
    /// 只推序号的话:指纹永远对不上,每一轮都重推一份内容相同的包,
    /// N 份历史窗口在几小时内被自己刷光。
    #[test]
    fn a_successful_upload_advances_both_the_fingerprint_and_the_sequence() {
        let mut cfg = mullion_store::CloudConfig::default();
        cfg.last_fingerprint = "old".into();
        cfg.last_seq = 3;
        record_success(&mut cfg, "new-fp", 4, "2026-09-15T10:15:00Z");
        assert_eq!(cfg.last_fingerprint, "new-fp");
        assert_eq!(cfg.last_seq, 4);
        assert_eq!(cfg.last_ok_at, "2026-09-15T10:15:00Z");
    }

    /// 失败**不许推进游标**。推了的话下一轮 `should_upload` 会认为
    /// 「内容没变」,于是这次没推上去的改动永远推不上去了,且零报错。
    #[test]
    fn a_failed_upload_leaves_the_cursor_alone() {
        let mut cfg = mullion_store::CloudConfig::default();
        cfg.last_fingerprint = "old".into();
        cfg.last_seq = 3;
        let before = cfg.clone();
        record_failure(&mut cfg);
        assert_eq!(cfg, before, "失败之后游标被动过了 —— 这次的改动会永远推不上去");
    }
}
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p mullion-app cloudsync 2>&1 | tail -20`
Expected: FAIL，模块没注册 / 找不到函数。

- [ ] **Step 4: 实现**

在 `cloudsync.rs` 的 `mod tests` 之前加：

```rust
use std::path::Path;

use mullion_cloud::error::CloudError;
use mullion_cloud::s3::{Credentials, S3Client};
use mullion_cloud::url::Endpoint;
use mullion_store::{cloud, portable, CloudConfig, Vault};

/// 撞号之后最多再试几次。**必须有限**,见上面那条测试的理由。
pub const MAX_PUT_ATTEMPTS: u32 = 4;

/// 一次上传的结果,回送给事件循环。
#[derive(Debug, Clone)]
pub enum UploadOutcome {
    /// 推上去了。带上新的指纹与序号,由 `app.rs` 写回 `cloud.toml`。
    Ok { fingerprint: String, seq: u64, at: String },
    /// 内容没变,什么都没做。**不是失败** —— 状态栏不该因此报红。
    Unchanged,
    /// 失败。已格式化的、可给用户看的一句话。
    Failed(String),
}

/// 撞号之后该用哪个序号。抽成纯函数只为可测 —— 真实的下一个序号要
/// 重新 List 才知道,这里给的是「至少要比刚才那个大」这条不变量。
fn plan_after_collision(taken: u64) -> Option<u64> {
    taken.checked_add(1)
}

/// 成功之后推进游标。**指纹与序号必须一起推**,见测试里的两条症状。
pub fn record_success(cfg: &mut CloudConfig, fingerprint: &str, seq: u64, at: &str) {
    cfg.last_fingerprint = fingerprint.to_string();
    cfg.last_seq = seq;
    cfg.last_ok_at = at.to_string();
}

/// 失败之后**什么都不改**。单独写成一个函数而不是「在调用点什么都不写」:
/// 有名字的空操作挡得住「顺手在这里记一下免得下次重试」那种改动,
/// 而那种改动会让这次没推上去的改动永远推不上去。
pub fn record_failure(_cfg: &mut CloudConfig) {}

/// 组装载荷并推上去。**阻塞。必须在 `spawn_blocking` 里调用。**
///
/// `stamp_compact` = `YYYYMMDD'T'HHMMSS'Z'`(既当 SigV4 的 `x-amz-date`,
/// 也当对象键里那一段 —— 两者同源,省得出现「键上写着 10 点、签名说 11 点」)。
pub fn upload_blocking(
    dir: &Path,
    vault: &Vault,
    cfg: &CloudConfig,
    stamp_compact: &str,
    stamp_rfc3339: &str,
    socks5: Option<&str>,
) -> UploadOutcome {
    // ① 装:顶层三文件 + 密文。**不带 layouts**(设计 D7)。
    let files = portable::collect_top_level(dir);
    let secrets = std::fs::read(dir.join("secrets.enc")).unwrap_or_default();
    let fp = cloud::fingerprint(&files, &secrets);
    if fp == cfg.last_fingerprint {
        return UploadOutcome::Unchanged;
    }

    // ② 封:先拼成 F46-a 的包文本,再**整体**用 vault key 加密(设计 D5)。
    //    整体加密之后,sessions.toml 里的真机 IP / 用户名 / 跳板拓扑不落云端明文。
    let text = match portable::write_pack(files, &secrets, env!("CARGO_PKG_VERSION"), stamp_rfc3339)
    {
        Ok(t) => t,
        Err(e) => return UploadOutcome::Failed(format!("打包失败:{e}")),
    };
    let sealed = match vault.seal_with_master(text.as_bytes()) {
        Ok(b) => b,
        Err(mullion_store::StoreError::NoMasterPassword) => {
            return UploadOutcome::Failed(
                "云端备份需要先设置主密码 —— 钥匙串里的密钥换台机器解不开".into(),
            )
        }
        Err(e) => return UploadOutcome::Failed(format!("加密失败:{e}")),
    };

    // ③ 送。
    let sk = match cloud::secret_key(cfg, vault) {
        Ok(s) if !s.is_empty() => s,
        Ok(_) => return UploadOutcome::Failed("还没填 Access Key Secret".into()),
        Err(e) => return UploadOutcome::Failed(format!("读不出 Access Key Secret:{e}")),
    };
    let client = S3Client::new(
        Endpoint {
            base: cfg.endpoint.clone(),
            bucket: cfg.bucket.clone(),
            path_style: cfg.path_style,
        },
        Credentials {
            access_key_id: cfg.access_key_id.clone(),
            secret_access_key: sk,
        },
        cfg.region.clone(),
        socks5,
    );

    let mut seq = match client.list_keys(&cfg.prefix, stamp_compact) {
        Ok(keys) => cloud::next_seq(&cfg.prefix, &keys),
        Err(e) => return UploadOutcome::Failed(format!("列举云端对象失败:{e}")),
    };
    for _ in 0..MAX_PUT_ATTEMPTS {
        let key = cloud::object_key(&cfg.prefix, seq, stamp_compact);
        match client.put_no_overwrite(&key, &sealed, stamp_compact) {
            Ok(()) => {
                return UploadOutcome::Ok {
                    fingerprint: fp,
                    seq,
                    at: stamp_rfc3339.to_string(),
                }
            }
            // 别的机器抢先用掉了这个序号。**往前挪再试** —— 这是没有 CAS
            // 的服务端上唯一的并发保护(设计 D8)。
            Err(CloudError::AlreadyExists) => match plan_after_collision(seq) {
                Some(next) => seq = next,
                None => return UploadOutcome::Failed("序号用尽".into()),
            },
            Err(e) => return UploadOutcome::Failed(format!("上传失败:{e}")),
        }
    }
    UploadOutcome::Failed(format!(
        "连试 {MAX_PUT_ATTEMPTS} 个序号都被占用 —— 可能有别的机器正在频繁上传"
    ))
}
```

- [ ] **Step 5: 注册模块**

`crates/mullion-app/src/lib.rs` 里加 `pub mod cloudsync;`（按既有顺序插入）。

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test -p mullion-app cloudsync 2>&1 | grep -E "test result|FAILED"`
Expected: 4 passed。

- [ ] **Step 7: 提交并变异验证**

```bash
git add crates/mullion-app/src/cloudsync.rs crates/mullion-app/src/lib.rs crates/mullion-app/Cargo.toml Cargo.lock
git commit -m "feat(app): 云端备份的上传编排 (F273)

装(不带 layouts)→ 封(整体加密,真机 IP 不落云端明文)→ 送(撞号往前挪)。
成功时指纹与序号必须一起推进:只推指纹会让下一次撞号,只推序号会让每轮
都重推一份相同的包、把 N 份历史窗口刷光。"
```

| 变异 | 应该变红的测试 |
|---|---|
| `record_success` 里删掉 `cfg.last_seq = seq;` | `a_successful_upload_advances_both_the_fingerprint_and_the_sequence` |
| `record_failure` 里加 `cfg.last_fingerprint = "x".into();` | `a_failed_upload_leaves_the_cursor_alone` |
| `MAX_PUT_ATTEMPTS` 改成 `1` | `retries_are_bounded_so_a_always_409_server_cannot_spin_forever` |
| `plan_after_collision` 改成 `Some(taken)` | `a_taken_sequence_number_is_retried_with_a_fresh_one` |

---

## Task 12: 设置弹窗的「云端备份」分节

**Files:**
- Modify: `crates/mullion-app/src/ui/settings.rs`

- [ ] **Step 1: 写失败测试**

追加到 `settings.rs` 的 `mod tests`（沿用该文件已有的 `run_env` / `interact_env` 辅助）：

```rust
    /// 未设主密码时,整节必须**灰掉并说明原因**(设计 D6)。
    ///
    /// 钥匙串方案下封出来的包换台机器解不开 —— 让用户填完一整屏 AK/SK、
    /// 开了开关、等了半小时,才在状态栏看见一句「需要主密码」,是最糟的路径。
    #[test]
    fn the_cloud_section_is_disabled_and_explains_itself_without_a_master_password() {
        let mut d = draft();
        let (texts, _) = run_env(&mut d, false, /* has_master_password */ false);
        assert!(
            texts.iter().any(|t| t.contains("云端备份")),
            "没有云端备份分节:{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("需要先设置主密码")),
            "没说清楚为什么用不了:{texts:?}"
        );
    }

    /// 设了主密码之后,开关必须真的可点,且点一下报 Preview(草稿变了,
    /// 要等「确定」才落盘)。
    #[test]
    fn toggling_the_cloud_switch_reports_a_preview() {
        let mut d = draft();
        let out = interact_env(&mut d, CLOUD_ENABLED_LABEL, true);
        assert_eq!(out, SettingsOut::Preview);
        assert!(d.cloud_enabled, "开关没被点开");
    }

    /// 草稿必须从**落盘的那份**起,不是硬编码默认值。
    /// 从默认值起的症状:打开设置弹窗、什么都没动、点「确定」,
    /// 用户配好的云端备份被关掉了。
    #[test]
    fn the_cloud_draft_starts_from_the_stored_config_not_a_hardcoded_default() {
        let mut stored = mullion_store::CloudConfig::default();
        stored.enabled = true;
        stored.bucket = "my-bucket".into();
        stored.keep = 7;
        let d = SettingsDraft::from_settings_and_cloud(&mullion_store::Settings::default(), &stored);
        assert!(d.cloud_enabled);
        assert_eq!(d.cloud_bucket, "my-bucket");
        assert_eq!(d.cloud_keep, 7);
    }

    /// AK Secret 框必须是密码框。**不是洁癖**:这个弹窗会被截图发出来
    /// (本项目的排查流程里「发个截图」是常规动作),明文摆在那儿就跟着走了。
    #[test]
    fn the_secret_key_field_is_masked() {
        let src = include_str!("settings.rs");
        let body = src
            .split("fn cloud(")
            .nth(1)
            .expect("没有 cloud 分节函数");
        let head = body.split("\nfn ").next().unwrap_or(body);
        let idx = head
            .find("cloud_secret")
            .expect("cloud 分节里没有 SK 输入框");
        assert!(
            head[idx..idx + 300].contains(".password(true)"),
            "SK 输入框不是密码框 —— 截图发出去就跟着走了"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app settings 2>&1 | tail -20`
Expected: FAIL。

- [ ] **Step 3: 扩 `SettingsDraft`**

在 `SettingsDraft` 里加（字段注释要写「为什么」，照该文件既有密度）：

```rust
    /// F271:云端备份的草稿。**与 `Settings` 分开** —— 它们落在两个文件里
    /// (`cloud.toml` 不进迁移包,见 `mullion_store::cloud` 的模块文档),
    /// 合成一份的话「确定」那一步会分不清该写哪个文件。
    pub cloud_enabled: bool,
    pub cloud_endpoint: String,
    pub cloud_region: String,
    pub cloud_bucket: String,
    pub cloud_prefix: String,
    pub cloud_path_style: bool,
    pub cloud_keep: u32,
    pub cloud_interval_min: u32,
    pub cloud_access_key_id: String,
    /// 新填的 SK。**空 = 不改**(不是「清空」):每次打开设置都要用户重打一遍
    /// 一串 30 位的密钥,是在逼人把它记在别处。
    pub cloud_secret_new: String,
```

并加一个构造器（保留原 `from_settings` 给别处用，新的是它的超集）：

```rust
    /// F271:从落盘的设置 + 落盘的云配置起一份草稿。
    ///
    /// **两个文件各读各的** —— 云配置在 `cloud.toml`,它不进迁移包。
    pub fn from_settings_and_cloud(
        s: &mullion_store::Settings,
        c: &mullion_store::CloudConfig,
    ) -> Self {
        Self {
            cloud_enabled: c.enabled,
            cloud_endpoint: c.endpoint.clone(),
            cloud_region: c.region.clone(),
            cloud_bucket: c.bucket.clone(),
            cloud_prefix: c.prefix.clone(),
            cloud_path_style: c.path_style,
            cloud_keep: c.keep,
            cloud_interval_min: c.interval_min,
            cloud_access_key_id: c.access_key_id.clone(),
            cloud_secret_new: String::new(),
            ..Self::from_settings(s)
        }
    }
```

（`from_settings` 里对应的十个字段补上默认值，让它仍能单独编过。）

- [ ] **Step 4: 加常量与分节函数**

在文件顶部常量区加：

```rust
/// 云端备份开关的标签。实现与测试**共用这一份**(同 `BOOTSTRAP_LABEL` 的理由)。
const CLOUD_ENABLED_LABEL: &str = "开启云端备份";
/// path-style 开关的标签。
const CLOUD_PATH_STYLE_LABEL: &str = "用 path-style 寻址（自建 MinIO 多半要打开）";
```

在 `show()` 的 `form::section(ui, t, "设置", "安全", &mut first);` **之后**、
「快捷键」之前插入：

```rust
            form::section(ui, t, "设置", "云端备份", &mut first);
            cloud(ui, t, draft, env, &mut out);
```

新增分节函数（放在 `security` 之后）：

```rust
/// 云端备份分节(F271)。
///
/// **未设主密码时整节置灰**(设计 D6):钥匙串方案下封出来的包换台机器解不开,
/// 而云备份的主场景正是「换新电脑」。让用户填完一整屏 AK/SK、开了开关、
/// 等半小时才在状态栏看见「需要主密码」,是最糟的那条路径。
fn cloud(
    ui: &mut egui::Ui,
    t: &Theme,
    draft: &mut SettingsDraft,
    env: SettingsEnv<'_>,
    out: &mut SettingsOut,
) {
    let ready = env.store_available && env.has_master_password;
    let avail = ui.available_width();
    let w = field_w(avail, FIELD_W_M, 0.0);

    if !ready {
        ui.label(
            egui::RichText::new(
                "云端备份需要先设置主密码 —— 钥匙串里的那把钥匙只在这台机器上有效，\
                 用它封出来的备份换台电脑一个字也解不开。请先在上面的「安全」里设一个。",
            )
            .size(11.0)
            .color(theme::c32(t.fg_muted)),
        );
        ui.add_space(SP_S);
    }

    ui.add_enabled_ui(ready, |ui| {
        form::grid(ui, "settings_cloud", |ui| {
            ui.label("");
            if ui
                .checkbox(&mut draft.cloud_enabled, CLOUD_ENABLED_LABEL)
                .changed()
            {
                *out = SettingsOut::Preview;
            }
            ui.end_row();

            ui.label("Endpoint");
            if ui
                .add(egui::TextEdit::singleline(&mut draft.cloud_endpoint).desired_width(w))
                .changed()
            {
                *out = SettingsOut::Preview;
            }
            ui.end_row();

            ui.label("Region");
            if ui
                .add(egui::TextEdit::singleline(&mut draft.cloud_region).desired_width(w))
                .changed()
            {
                *out = SettingsOut::Preview;
            }
            ui.end_row();

            ui.label("Bucket");
            if ui
                .add(egui::TextEdit::singleline(&mut draft.cloud_bucket).desired_width(w))
                .changed()
            {
                *out = SettingsOut::Preview;
            }
            ui.end_row();

            ui.label("前缀");
            if ui
                .add(egui::TextEdit::singleline(&mut draft.cloud_prefix).desired_width(w))
                .changed()
            {
                *out = SettingsOut::Preview;
            }
            ui.end_row();

            ui.label("Access Key ID");
            if ui
                .add(egui::TextEdit::singleline(&mut draft.cloud_access_key_id).desired_width(w))
                .changed()
            {
                *out = SettingsOut::Preview;
            }
            ui.end_row();

            ui.label("Access Key Secret");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut draft.cloud_secret_new)
                        .password(true)
                        .desired_width(w)
                        .hint_text("留空 = 不改"),
                )
                .changed()
            {
                *out = SettingsOut::Preview;
            }
            ui.end_row();

            ui.label("");
            if ui
                .checkbox(&mut draft.cloud_path_style, CLOUD_PATH_STYLE_LABEL)
                .changed()
            {
                *out = SettingsOut::Preview;
            }
            ui.end_row();

            ui.label("保留份数");
            if ui
                .add(egui::DragValue::new(&mut draft.cloud_keep).range(1..=200))
                .changed()
            {
                *out = SettingsOut::Preview;
            }
            ui.end_row();

            ui.label("检查间隔");
            if ui
                .add(
                    egui::DragValue::new(&mut draft.cloud_interval_min)
                        .range(5..=1440)
                        .suffix(" 分钟"),
                )
                .changed()
            {
                *out = SettingsOut::Preview;
            }
            ui.end_row();

            ui.label("");
            ui.label(
                egui::RichText::new(
                    "整份配置会用主密码派生的密钥加密之后再上传，云上那份是不可读的二进制；\
                     内容没变就不上传。窗口布局与现场记录不上云（它们是这台机器的属性）。\
                     建议用 RAM 子账号、只授权这一个 bucket 的这一个前缀。",
                )
                .size(11.0)
                .color(theme::c32(t.fg_muted)),
            );
            ui.end_row();
        });
    });
    ui.add_space(SP_M);
}
```

- [ ] **Step 5: 扩测试辅助 `run_env` / `interact_env`**

该文件已有的辅助函数签名里多半只有 `not_monospace`。按测试里的调用形态补一个
`has_master_password` 参数，并让 `SettingsEnv` 照着填。**以文件实际签名为准**
（若已有该参数就不用改）。

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test -p mullion-app settings 2>&1 | grep -E "test result|FAILED"`
Expected: 全绿。

- [ ] **Step 7: 跑字形白名单与表单规范守护**

Run: `cargo test -p mullion-app --test glyph_whitelist --test form_guidelines --test dialog_contrast --test strong_text_color 2>&1 | grep -E "test result|FAILED"`
Expected: 全绿。若 `glyph_whitelist` 报红，说明文案里混进了 GBK 外的字形 —— 改文案，
**不要**去改白名单。

- [ ] **Step 8: 提交并变异验证**

```bash
git add crates/mullion-app/src/ui/settings.rs
git commit -m "feat(app): 设置弹窗加云端备份分节,未设主密码时整节置灰 (F271)"
```

| 变异 | 应该变红的测试 |
|---|---|
| `let ready = ...` 改成 `true` | `the_cloud_section_is_disabled_and_explains_itself_without_a_master_password` |
| `.password(true)` 删掉 | `the_secret_key_field_is_masked` |
| `from_settings_and_cloud` 里 `cloud_enabled` 改成 `false` | `the_cloud_draft_starts_from_the_stored_config_not_a_hardcoded_default` |

---

## Task 13: 菜单入口与状态栏指示器

**Files:**
- Modify: `crates/mullion-app/src/ui/chrome.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs`

- [ ] **Step 1: 写失败测试**

追加到 `chrome.rs` 的 `mod tests`：

```rust
    /// F273:「配置」菜单里必须有一个**常驻的手动备份入口**。
    ///
    /// 只有定时的话,用户在「我刚改完一堆会话，现在要重装系统」这个语境下
    /// 没有任何办法让它立刻推一份 —— 而那正是最需要备份的一刻。
    ///
    /// **扎的是源码结构**(菜单项要展开 `menu_button` 才画得出来,跑帧测不到)。
    /// 判据串带上行首缩进,避免匹配到这条测试自己(第五类恒绿模式)。
    #[test]
    fn the_config_menu_has_a_permanent_entry_to_back_up_now() {
        let src = include_str!("chrome.rs");
        assert!(
            src.contains("\n                    if ui.button(\"立刻备份到云\").clicked() {"),
            "「配置」菜单里没有手动备份入口"
        );
    }

    /// 状态栏的云指示器在**没配置时不占格**(同隧道指示器那条理由:
    /// 每多一格常驻信息,别的信息就少一分被看见的机会)。
    #[test]
    fn the_cloud_cell_is_absent_when_cloud_backup_is_off() {
        let texts = status_texts_with_cloud(None);
        assert!(
            !texts.iter().any(|t| t.contains("云")),
            "关着的时候还占了一格:{texts:?}"
        );
    }

    /// 配了就必须画出来,而且**失败要看得见**。
    /// 「静默失败」是备份功能唯一致命的失败模式 —— 用户以为有备份,直到需要
    /// 它的那天才发现没有。
    #[test]
    fn a_failing_cloud_backup_is_shown_in_the_status_bar() {
        let texts = status_texts_with_cloud(Some(&CloudCell {
            text: "云 备份失败".into(),
            severity: crate::tunnels::Severity::Danger,
        }));
        assert!(
            texts.iter().any(|t| t.contains("备份失败")),
            "备份失败没出现在状态栏:{texts:?}"
        );
    }
```

并照该文件已有的 `status_texts` 辅助，加一个 `status_texts_with_cloud`。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app chrome 2>&1 | tail -20`
Expected: FAIL。

- [ ] **Step 3: 加菜单项**

在 `chrome.rs` 的「配置」菜单里，`导出脱敏日志…` 之后、`设置…` 之前插入：

```rust
                    // F273:手动备份的常驻入口。只有定时的话,用户在
                    // 「刚改完一堆会话、现在要重装系统」这个语境下没有任何
                    // 办法让它立刻推一份 —— 而那正是最需要备份的一刻。
                    if ui.button("立刻备份到云").clicked() {
                        ui_state.cloud_backup_request = true;
                        ui.close_menu();
                    }
```

在 `ui/mod.rs` 的 `UiState`（`pack_pick_request` 附近）加：

```rust
    /// F273:菜单里点了「立刻备份到云」→ `app.rs` 事后起一次 spawn_blocking 上传。
    pub cloud_backup_request: bool,
```

（该结构体若有 `has_real_action` 之类的完备性方法，**必须同步加一笔** ——
F199 的注释里写过，漏了的话这次点击会被 egui 的 discard 趟静默吃掉。）

- [ ] **Step 4: 加状态栏格**

在 `chrome.rs` 加：

```rust
/// F273:状态栏上的云端备份格。`None` = 没配置,**不占格**
/// (同隧道指示器:每多一格常驻信息,别的信息就少一分被看见的机会)。
pub struct CloudCell {
    pub text: String,
    pub severity: crate::tunnels::Severity,
}
```

给 `status_bar` 加一个 `cloud: Option<&CloudCell>` 参数，画在隧道格之后、
`right` 之前，颜色按 `severity` 取（照隧道那段的写法，复用同一个 match）：

```rust
                    // F273:云端备份排在隧道之后。**失败必须看得见** ——
                    // 「静默失败」是备份功能唯一致命的失败模式:用户以为有
                    // 备份,直到需要它的那天才发现没有。
                    if let Some(c) = cloud {
                        let color = match c.severity {
                            crate::tunnels::Severity::Calm => t.fg_muted,
                            crate::tunnels::Severity::Warn => t.warn,
                            crate::tunnels::Severity::Danger => t.danger,
                        };
                        let r = ui.colored_label(theme::c32(color), &c.text);
                        annotate::mark(ui.ctx(), "状态栏/云端备份", r.rect);
                        ui.separator();
                    }
```

`status_bar` 的全部调用点（`app.rs` 里）补上新参数。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p mullion-app chrome 2>&1 | grep -E "test result|FAILED"`
Expected: 全绿。

- [ ] **Step 6: 提交并变异验证**

```bash
git add crates/mullion-app/src/ui/chrome.rs crates/mullion-app/src/ui/mod.rs crates/mullion-app/src/app.rs
git commit -m "feat(app): 菜单「立刻备份到云」+ 状态栏云指示器 (F273)

失败必须看得见 —— 静默失败是备份功能唯一致命的失败模式。"
```

| 变异 | 应该变红的测试 |
|---|---|
| 菜单项文案改成「备份到云」 | `the_config_menu_has_a_permanent_entry_to_back_up_now` |
| 状态栏那段 `if let Some(c)` 改成 `if false` | `a_failing_cloud_backup_is_shown_in_the_status_bar` |
| 那段改成无条件画（`None` 时画空串） | `the_cloud_cell_is_absent_when_cloud_backup_is_off` |

---

## Task 14: `app.rs` 接线 —— 定时驱动 + spawn_blocking + 结果回收

**Files:**
- Modify: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: 写失败测试**

追加到 `app.rs` 的 `mod tests`（这个文件里已有大量「源码切片」式守护，照它的形态写）：

```rust
    /// F273:上传**必须**在 `spawn_blocking` 里跑。
    ///
    /// `mullion-cloud` 是阻塞式的(ureq)。在事件循环里同步调用它,一次
    /// 高延迟往返就能把帧率打到零 —— 而本项目的存在理由就是「不卡」
    /// (T3/T7 红线)。
    ///
    /// **扎的是源码结构**:这条约束没有运行期的表现可以断言(测试环境里
    /// 网络调用本来就不会发生),漏了也不报错,只有真机上卡给用户看。
    #[test]
    fn the_cloud_upload_runs_off_the_event_loop_thread() {
        let src = prod_src();
        let body = body_of(src, "fn spawn_cloud_backup(");
        assert!(
            body.contains("spawn_blocking"),
            "云端上传没走 spawn_blocking —— 一次高延迟往返就会把帧率打到零"
        );
        assert!(
            !body.contains("upload_blocking(") || body.contains("spawn_blocking"),
            "在事件循环里直接调用了 upload_blocking"
        );
    }

    /// 同一时刻只许有一次上传在途。没有这道闸的话:定时器每 30 分钟塞一个,
    /// 而一次高延迟上传可能跑几分钟 —— 手动点几下就能攒出一串并发的
    /// `spawn_blocking`,它们会互相撞号(ForbidOverwrite),表现成
    /// 「备份时好时坏」。
    #[test]
    fn only_one_cloud_upload_is_in_flight_at_a_time() {
        let src = prod_src();
        let body = body_of(src, "fn spawn_cloud_backup(");
        assert!(
            body.contains("self.cloud_in_flight"),
            "没有在途标记 —— 定时与手动会攒出一串并发上传并互相撞号"
        );
    }

    /// 结果回来时必须把在途标记**归还**。每条出口都要还。
    ///
    /// 这是本项目的常客形状(见 T13 的「hold 每条出口都要归还」):漏一条
    /// 出口的后果是那之后**永远**不再备份,且没有任何报错。
    #[test]
    fn every_path_that_ends_a_cloud_upload_hands_the_in_flight_flag_back() {
        let src = prod_src();
        let body = body_of(src, "UserEvent::CloudBackupDone(");
        let returns = body.matches("self.cloud_in_flight = false").count();
        assert!(
            returns >= 1,
            "CloudBackupDone 的处理里没有归还在途标记 —— 之后永远不再备份"
        );
        // 三个分支(Ok/Unchanged/Failed)不许有任何一条提前 return 绕过归还。
        assert!(
            !body.contains("return"),
            "CloudBackupDone 的处理里有提前 return,可能绕过在途标记的归还"
        );
    }

    /// 定时驱动必须**每帧都被调到**,而不是挂在某个偶尔才走的分支上。
    #[test]
    fn the_cloud_backup_is_driven_every_frame() {
        let src = prod_src();
        assert!(
            src.contains("self.drive_cloud_backup("),
            "drive_cloud_backup 没有被调用 —— 定时备份从来不会发生"
        );
    }
```

（`prod_src()` / `body_of()` 是 `app.rs` 测试区已有的辅助，直接用。
**注意本项目已登记的坑**：源码切片守护不剥注释，判据串里的关键词若同时出现在
注释里会造成假绿 —— 写完这四条后按下面的变异表逐条验一遍。）

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app cloud 2>&1 | tail -20`
Expected: FAIL。

- [ ] **Step 3: 加 `App` 字段**

```rust
    /// F273:有一次云端备份在途。**必须有这道闸**:定时器每 30 分钟塞一个,
    /// 而一次高延迟上传可能跑几分钟 —— 没闸的话手动点几下就能攒出一串
    /// 并发上传,它们互相撞号(ForbidOverwrite),表现成「备份时好时坏」。
    cloud_in_flight: bool,
    /// F273:上次算指纹的时刻(单调毫秒)。定时的起点。
    cloud_last_check_ms: u64,
    /// F273:最近一次备份的结论,状态栏那一格的数据源。
    cloud_status: Option<crate::ui::chrome::CloudCell>,
```

- [ ] **Step 4: 加 `UserEvent` 变体**

```rust
    /// F273:一次云端备份跑完了(成功/没变/失败都走这一条)。
    ///
    /// **三种结局共用一个变体**:在途标记的归还只能有一处,分成三个变体
    /// 就变成三处各归还一次 —— 而「漏一条出口」是本项目的常客形状,
    /// 漏掉之后**永远**不再备份且零报错。
    CloudBackupDone(crate::cloudsync::UploadOutcome),
```

- [ ] **Step 5: 实现 `spawn_cloud_backup` 与 `drive_cloud_backup`**

```rust
    /// F273:起一次云端备份。已有在途的就**直接回**(不排队:排队等于把
    /// 「已经过时的那一份」推上去,而下一轮会立刻再推一份新的)。
    fn spawn_cloud_backup(&mut self) {
        if self.cloud_in_flight {
            return;
        }
        let Some(dir) = self.config_dir.clone() else { return };
        self.cloud_in_flight = true;
        let proxy = self.proxy.clone();
        let stamp_compact = crate::localtime::utc_compact();
        let stamp_rfc3339 = crate::localtime::utc_rfc3339();
        let socks5 = self.cloud_socks5.clone();
        // **必须 spawn_blocking**:mullion-cloud 是阻塞式的,在事件循环里
        // 同步跑一次高延迟往返就能把帧率打到零(T3/T7)。
        tokio::task::spawn_blocking(move || {
            // vault 不能跨线程搬,所以在这条线程上重新打开一份**只读**的。
            // 具体怎么拿到 vault,依 `App` 当前持有的形态定 —— 若 `Vault`
            // 是 `Arc<Mutex<..>>`,直接 clone 那个 Arc 进来即可。
            let outcome = crate::cloudsync::upload_blocking(
                &dir,
                &vault,
                &cfg,
                &stamp_compact,
                &stamp_rfc3339,
                socks5.as_deref(),
            );
            let _ = proxy.send_event(UserEvent::CloudBackupDone(outcome));
        });
    }

    /// F273:每帧看一眼该不该起一次定时备份。
    ///
    /// **判据是内容指纹,不是脏标记**(设计 D9)。算指纹要读四个文件,
    /// 所以按 `interval_min` 限流 —— 每帧都算的话是每秒几十次读盘。
    fn drive_cloud_backup(&mut self, now_ms: u64) {
        if self.cloud_in_flight {
            return;
        }
        let Some(dir) = self.config_dir.clone() else { return };
        let cfg = mullion_store::cloud::load(&dir);
        if !cfg.enabled {
            return;
        }
        let interval_ms = u64::from(cfg.interval_min) * 60_000;
        if now_ms.saturating_sub(self.cloud_last_check_ms) < interval_ms {
            return;
        }
        self.cloud_last_check_ms = now_ms;
        let files = mullion_store::portable::collect_top_level(&dir);
        let secrets = std::fs::read(dir.join("secrets.enc")).unwrap_or_default();
        let fp = mullion_store::cloud::fingerprint(&files, &secrets);
        if fp == cfg.last_fingerprint {
            return;
        }
        self.spawn_cloud_backup();
    }
```

在事件循环里每帧调用 `self.drive_cloud_backup(now_ms);`（放在 `drive_reconnects`
之类的同伴旁边），并在菜单动作处理里接上 `if ui_state.cloud_backup_request { self.spawn_cloud_backup(); }`。

- [ ] **Step 6: 实现 `CloudBackupDone` 的处理**

```rust
            UserEvent::CloudBackupDone(outcome) => {
                // 在途标记**只在这一处归还**,且这个分支里没有任何提前 return
                // —— 漏一条出口的后果是之后永远不再备份,且零报错(T13 同族)。
                self.cloud_in_flight = false;
                let dir = self.config_dir.clone();
                match outcome {
                    crate::cloudsync::UploadOutcome::Ok { fingerprint, seq, at } => {
                        if let Some(d) = dir {
                            let mut cfg = mullion_store::cloud::load(&d);
                            crate::cloudsync::record_success(&mut cfg, &fingerprint, seq, &at);
                            if let Err(e) = mullion_store::cloud::save(&d, &cfg) {
                                log::warn!("云端备份游标写回失败:{e}");
                            }
                        }
                        self.cloud_status = Some(crate::ui::chrome::CloudCell {
                            text: format!("云 已备份 #{seq}"),
                            severity: crate::tunnels::Severity::Calm,
                        });
                    }
                    crate::cloudsync::UploadOutcome::Unchanged => {
                        // 没变不是失败,不动状态栏那一格。
                    }
                    crate::cloudsync::UploadOutcome::Failed(msg) => {
                        log::warn!("云端备份失败:{msg}");
                        self.cloud_status = Some(crate::ui::chrome::CloudCell {
                            text: "云 备份失败".into(),
                            severity: crate::tunnels::Severity::Danger,
                        });
                        self.ui.set_error(format!("云端备份失败:{msg}"));
                    }
                }
            }
```

- [ ] **Step 7: 接「确定」时把云草稿落盘**

在设置弹窗 `SettingsOut::Commit` 的处理里，除了现有的 `settings::save`，加：

```rust
                // F271:云配置落**另一个文件**(`cloud.toml` 不进迁移包)。
                // SK 空 = 不改 —— 每次打开设置都要重打一遍 30 位密钥,
                // 是在逼用户把它记在别处。
                if let Some(d) = self.config_dir.clone() {
                    let mut cfg = mullion_store::cloud::load(&d);
                    cfg.enabled = draft.cloud_enabled;
                    cfg.endpoint = draft.cloud_endpoint.clone();
                    cfg.region = draft.cloud_region.clone();
                    cfg.bucket = draft.cloud_bucket.clone();
                    cfg.prefix = draft.cloud_prefix.clone();
                    cfg.path_style = draft.cloud_path_style;
                    cfg.keep = draft.cloud_keep;
                    cfg.interval_min = draft.cloud_interval_min;
                    cfg.access_key_id = draft.cloud_access_key_id.clone();
                    if !draft.cloud_secret_new.is_empty() {
                        if let Some(v) = self.vault.as_ref() {
                            if let Err(e) =
                                mullion_store::cloud::set_secret_key(&mut cfg, v, &draft.cloud_secret_new)
                            {
                                self.ui.set_error(format!("保存 Access Key Secret 失败:{e}"));
                            }
                        }
                        draft.cloud_secret_new.clear();
                    }
                    if let Err(e) = mullion_store::cloud::save(&d, &cfg) {
                        self.ui.set_error(format!("保存云端备份配置失败:{e}"));
                    }
                }
```

**`self.vault` / `self.config_dir` / `self.proxy` 的实际名字以 `app.rs` 当前形态
为准** —— 先 grep 一遍，不要照抄这里的字段名。

- [ ] **Step 8: 跑全量**

Run: `cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log`
Expected: 全绿。

Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20`
Expected: 无输出。

Run: `cargo fmt --check`
Expected: 无输出。

- [ ] **Step 9: 提交并变异验证**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 云端备份的定时驱动与结果回收 (F273)

在途标记只在 CloudBackupDone 一处归还,三种结局共用一个事件变体 ——
分成三个变体就是三处各归还一次,而「漏一条出口」的后果是之后永远
不再备份且零报错(T13 同族)。"
```

| 变异 | 应该变红的测试 |
|---|---|
| `spawn_cloud_backup` 里去掉 `spawn_blocking`，直接同步调 | `the_cloud_upload_runs_off_the_event_loop_thread` |
| 去掉 `if self.cloud_in_flight { return; }` 那句 | `only_one_cloud_upload_is_in_flight_at_a_time` |
| `CloudBackupDone` 分支里删掉 `self.cloud_in_flight = false;` | `every_path_that_ends_a_cloud_upload_hands_the_in_flight_flag_back` |
| 事件循环里注释掉 `self.drive_cloud_backup(now_ms);` | `the_cloud_backup_is_driven_every_frame` |

**⚠️ 若某条变异杀不掉**：多半是源码切片守护匹配到了注释里的关键词（本项目已登记
的坑）。把判据串改成带行首缩进的精确形态，或把它扎到一个注释里不会出现的形状上。
**杀不掉就是恒绿，停下来重写守护，不要跳过。**

---

## Task 15: live 测试、体积核对、spec 登记、发版

**Files:**
- Create: `crates/mullion-cloud/tests/live.rs`
- Modify: `spec.md`
- Modify: `README.md`（若它列了架构图 / crate 清单）

- [ ] **Step 1: 写 live 测试**

```rust
//! 对**真实** S3 兼容存储的端到端验证(F270)。默认 `#[ignore]`。
//!
//! 真机信息一律从环境变量传,**绝不写死进库**(同 `mullion-ssh --test live`):
//!
//! ```bash
//! MULLION_CLOUD_LIVE=1 \
//! MULLION_CLOUD_ENDPOINT=https://oss-cn-hangzhou.aliyuncs.com \
//! MULLION_CLOUD_REGION=cn-hangzhou \
//! MULLION_CLOUD_BUCKET=<你的 bucket> \
//! MULLION_CLOUD_AK=<AK> MULLION_CLOUD_SK=<SK> \
//!   cargo test -p mullion-cloud --test live -- --ignored --nocapture
//! ```
//!
//! **这是片一唯一能证明 SigV4 被真实服务端接受的证据。** 假 server 不验签,
//! 官方向量只证明我们与 AWS 的规范一致 —— 阿里云对 V4 的兼容细节没有一手
//! 文档,只能靠这条测试。

use mullion_cloud::s3::{Credentials, S3Client};
use mullion_cloud::url::Endpoint;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
#[ignore = "要真实 bucket 与 AK/SK,见模块文档"]
fn a_real_bucket_accepts_our_signature_and_refuses_an_overwrite() {
    if env("MULLION_CLOUD_LIVE").is_none() {
        eprintln!("跳过:未设 MULLION_CLOUD_LIVE");
        return;
    }
    let client = S3Client::new(
        Endpoint {
            base: env("MULLION_CLOUD_ENDPOINT").expect("MULLION_CLOUD_ENDPOINT"),
            bucket: env("MULLION_CLOUD_BUCKET").expect("MULLION_CLOUD_BUCKET"),
            path_style: env("MULLION_CLOUD_PATH_STYLE").is_some(),
        },
        Credentials {
            access_key_id: env("MULLION_CLOUD_AK").expect("MULLION_CLOUD_AK"),
            secret_access_key: env("MULLION_CLOUD_SK").expect("MULLION_CLOUD_SK"),
        },
        env("MULLION_CLOUD_REGION").expect("MULLION_CLOUD_REGION"),
        env("MULLION_CLOUD_SOCKS5").as_deref(),
    );
    // 时间戳从 env 传 —— 本 crate 不持时钟。跑之前用 `date -u +%Y%m%dT%H%M%SZ`。
    let stamp = env("MULLION_CLOUD_STAMP").expect("MULLION_CLOUD_STAMP：date -u +%Y%m%dT%H%M%SZ");
    let key = format!("mullion-live-test/{stamp}.bin");

    client
        .put_no_overwrite(&key, b"mullion live probe", &stamp)
        .expect("第一次 PUT 应该成功 —— 失败多半是签名或权限");

    // 第二次必须被拒。**这一条是追加式布局全部并发保护的真机证明** ——
    // 若真实服务端忽略了那两个头,本地假 server 是测不出来的。
    let again = client.put_no_overwrite(&key, b"x", &stamp);
    assert!(
        matches!(again, Err(mullion_cloud::CloudError::AlreadyExists)),
        "服务端没有拒绝覆盖,拿到的是 {again:?} —— 并发保护在这台服务端上不成立"
    );

    let keys = client
        .list_keys("mullion-live-test/", &stamp)
        .expect("LIST 应该成功");
    assert!(keys.contains(&key), "刚写的对象没出现在列表里:{keys:?}");

    eprintln!("live 验证通过。请手动删掉测试对象:{key}");
}
```

Run: `cargo test -p mullion-cloud --test live 2>&1 | grep -E "test result"`
Expected: `1 ignored`（不设 env 时不跑）。

- [ ] **Step 2: 登记 spec 条目**

在 `spec.md` 的功能表末尾（F269 之后）加 F270~F275 六行，内容照设计文档 §4 的表格，
每行补上「验收」列（指向本计划里的守护测试名）。

同时把非目标 N-G4 改写成设计文档 §1 给的那段。

- [ ] **Step 3: 跑完整绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Expected: 三条全部干净。**只跑了单个 crate 不叫绿。**

- [ ] **Step 4: 量 exe 体积**

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app 2>&1 | tail -5
ls -l target/x86_64-pc-windows-gnu/release/mullion-app.exe
```

与 v0.1.110 的 17.2 MiB 对比，把增量记进 PR 描述。**超过 N6 的 25MB 上限就停下来问。**

- [ ] **Step 5: 交叉编译依赖验收**

```bash
x86_64-w64-mingw32-objdump -p target/x86_64-pc-windows-gnu/release/mullion-app.exe \
  | grep "DLL Name" | sort -u
```

Expected: 与上一版**完全一致**。多出任何一个 DLL 名字都要查清楚 —— rustls + ring
应当是静态链接的，冒出新 DLL 说明 TLS provider 没走 ring（ADR-005 的坑）。

- [ ] **Step 6: 提交**

```bash
git add spec.md crates/mullion-cloud/tests/live.rs
git commit -m "docs(spec): 登记 F270~F275,改写 N-G4 (F270)

N-G4 从「云同步、账号体系」收窄成「账号体系」——云备份用的是用户自己的
对象存储与 AK/SK,我们不运营服务端、不持有用户数据,本地文件仍是唯一真值源。"
```

- [ ] **Step 7: 发版**

按 `.claude/skills/release-windows/SKILL.md` 一条龙：升 patch 版本号 → 跑绿 →
交叉编译 + objdump 验收 → 签名 → 发 GitHub Release（走 socks 代理，本机 DNS 解析
不了 github）→ 报链接。

**Release notes 里的人工验收清单（这些无头容器里验不了）：**

```
## F270~F273 云端备份（上传）人工验收

前置：设置 → 安全 → 设一个主密码；设置 → 云端备份 → 填 endpoint/region/bucket/
AK/SK，勾上「开启云端备份」，点确定。

- [ ] 「配置 → 立刻备份到云」点一下，状态栏出现「云 已备份 #1」
- [ ] 去 OSS 控制台看，`mullion/000001-<时间>.mpk` 在那儿，**用记事本打开是乱码**
      （整体加密生效；能看见 `[[file]]` 或主机名就是加密没接上，立刻报）
- [ ] 什么都不改，再点一次「立刻备份到云」→ 云上**不应该**多出第二个对象
      （指纹判据生效）
- [ ] 改一条会话的名字，再点一次 → 云上多出 `000002-...`
- [ ] 把 AK 故意改错一位 → 状态栏出现红色「云 备份失败」，且错误里能看出是
      签名/权限问题（不是一句干巴巴的「失败」）
- [ ] 拔网线 → 点「立刻备份到云」→ 报网络错误，**不卡界面**（这一条最要紧：
      上传若没走 spawn_blocking，整个窗口会僵住几十秒）
- [ ] 代理链路下重复上面两条
- [ ] 改一次主密码，然后再点「立刻备份到云」→ 仍然成功
      （AK/SK 重封生效；报「读不出 Access Key Secret」就是 D12 的接线漏了）
- [ ] 开着备份跑半天，观察是否有异常 CPU 占用（定时算指纹每 30 分钟一次，
      不该在 profile 行里看得见）
```

---

## Self-Review 记录

**Spec 覆盖检查（对照设计文档 D1~D15）：**

- D1 整包 LWW → 片一只做上传，LWW 的「显式仲裁」在片二。片一的并发保护 = D8 的
  `ForbidOverwrite` + 撞号重试（Task 5 / Task 11）。✅
- D2 S3 兼容 → Task 3 的 path_style + Task 5。✅
- D3 新 crate → Task 1。✅
- D4 ureq + 钉死 provider → Task 1 Step 2/3，Task 15 Step 5 的 objdump 验收。✅
- D5 整体加密 → Task 6 + Task 11 Step 4。✅
- D6 要求主密码 → Task 6（store 层拒绝）+ Task 12（UI 层置灰）。✅
- D7 不带 layouts → Task 7。✅
- D8 追加式序号 → Task 10 + Task 5。✅
- D9 触发时机 → Task 10 的 `should_upload` + Task 14 的 `drive_cloud_backup`。✅
- D10 仲裁 → **片二**，片一不做。状态栏那一格（Task 13）是它的地基。
- D11 cloud.toml 不进包 → Task 7 Step 2 的守护 + Task 8。✅
- D12 改密码连带重封 → Task 9。✅
- D13 旧包保留标注 → **片二**（片一不删任何东西，所以不冲突）。
- D14 清理跳过异密码份 → **片二**。片一完全不删对象，云上只增不减。
- D15 切片 → 本计划就是片一。✅

**片一刻意不做的（写进 Release notes，别让用户以为坏了）：**
云上对象只增不减（保留份数那个设置项在片一**不生效**，它是片二的配置）；
发现别的机器推过了不会提示；不能从云端拉回来（恢复仍走「导入配置…」+ 手动下载）。
→ **设置弹窗里「保留份数」那一项在片一要加一句 hint 说明「下一个版本生效」**，
否则就是一个假的开关。这一条补进 Task 12 Step 4 的分节文案。
