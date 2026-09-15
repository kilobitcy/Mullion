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

        // **两份密文必须等长**。写成 `b"secret"`(6) vs `b"other"`(5) 的话,
        // 长度一不同,光靠长度前缀就把它们分开了 —— 于是「把 `h.update(secrets)`
        // 整句删掉」这个真缺陷照样全绿(实测过)。症状:密文改了但字节数没变
        // (vault 换个 nonce 重写就是这样),指纹认为「没变」,这次改动永远
        // 推不上去且零报错。
        assert_ne!(fingerprint(&base, b"secreT"), base_fp, "密文变了指纹没变");
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
            socks5: String::new(),
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
    /// SOCKS5 代理,形如 `127.0.0.1:1080`。空 = 直连。
    ///
    /// **不带 `socks5://` 前缀**:`S3Client::new` 收到的是 `host:port`,
    /// 自己 `format!("socks5://{p}")` 补前缀(已核实 `s3.rs:69`)。带着前缀
    /// 传进去会拼成 `socks5://socks5://…`,`ureq::Proxy::new` 直接报
    /// `Config` 错。那条错误文案是清楚的,所以不在这里做容错剥前缀 ——
    /// 加一段没有守护测试的容错,比让用户看见一条准确的报错更糟。
    ///
    /// **这个字段不补的话 `socks5` 参数就是条死线**:`mullion-cloud` 为它
    /// 开了 ureq 的 `socks-proxy` 特性、`S3Client::new` 专门收了这个参数,
    /// 而设计 D15 把「SOCKS 代理链路通不通」列进了片一的真机验收项 ——
    /// 没有配置入口的话那条永远传 `None`,验收项验的是一条从没走过的路
    /// (本项目登记过同一形状:「量具存在≠接在那条路上」)。
    #[serde(default)]
    pub socks5: String,
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
            socks5: String::new(),
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

> **已完成**（`46214ca` `d4a912c` `b637967` `8f5d141` `10ec3fa`）。15 条测试，
> 五条变异全部杀掉。两轮质量复核挖出的东西值得记下来：
>
> 1. **「读不出来」被当成「没有」**（原 `fs::read(..).unwrap_or_default()`，两处）。
>    复核实跑证实非空 `secrets.enc` 读失败时算出的指纹与「文件不存在」逐字节相同 ——
>    一次瞬时 IO 失败会让一份**密文段是空的**备份被正常加密、正常上传、正常推进游标，
>    恢复那天 `portable` 还把空密文段解释成「源机没有密码」这个合法状态。
>    修法是 `read_secrets` 把 `NotFound` 与其余错误分开。
> 2. **守护只推到一半**。第一版只守住 `read_secrets` / `fingerprint_now` 这一层，
>    把 `prepare` 里那一行单独退回 `unwrap_or_default()` 仍然 14/14 全绿 ——
>    而 `prepare` 才是真正走到「打包 → 加密 → 上传」的那条路。守护必须
>    **顺着调用链推到真正产出上传内容的那一层**。
> 3. **测「读不出来」用同名目录，不用 `chmod 0o000`**（Windows 上 chmod 是 no-op）。
>    但注意 `Vault::open_with` 自己就是 `if secrets_path.exists() { fs::read(..)? }`
>    （`vault.rs:170`），同名目录会让开库先失败 —— 所以测试要**两个目录**：
>    vault 开在干净的那个，`prepare` 指向放了同名目录的那个。
> 4. **`msg.contains("主密码")` 挡不住分支合并**：`StoreError::NoMasterPassword`
>    的 `Display` 本身就是「这个操作需要先设置主密码」，并进通用
>    `Err(e) => Failed(format!("加密失败:{e}"))` 之后消息仍然含「主密码」。
>    靠补一条 `!msg.starts_with("加密失败")` 才分得开。
>
> **遗留债（登记，不在本切片修）**：那条守护现在是两条字符串断言，判据搭在
> `StoreError::NoMasterPassword` 的 `Display` 文案上。更稳的做法是在 `prepare`
> 里先 `matches!(err, StoreError::NoMasterPassword)` 再格式化，但那要改
> `Prepared::Failed(String)` 的错误传递结构。`mullion-cloud/src/error.rs` 的注释里
> 已经为**控制流**画过同一条红线（「不要靠 match 错误消息文本做控制流」），
> 这里是测试判据、性质轻一档，但同根。

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
//! # 一次上传拆成两半
//!
//! [`prepare`](主线程) → [`upload_blocking`](`spawn_blocking` 线程)。
//!
//! 拆的理由是 **`Vault` 搬不进线程**:它住在 `App.store` 里、没有 `Clone`,
//! 而给它加 `Clone` 等于允许「两份 Vault 各自 `save()` 互相覆盖」——
//! 正是 F247/F248 刚修完的「整份覆盖」缺陷族。于是凡是要 vault 的活
//! (封载荷、解 SK)留在事件循环线程上,搬进线程的只有字节。
//!
//! 主线程那一半全是纯 CPU(读几十 KB、一次 sha256、一次 XChaCha20),微秒级。
//! 内容没变时它直接回 `Unchanged`,连 `spawn_blocking` 都不起。
//!
//! # 阻塞
//!
//! `mullion-cloud` 是阻塞式的(ureq)。[`upload_blocking`] 必须挖进
//! `spawn_blocking` —— 在事件循环里同步跑网络会把帧率打到零(T3/T7 红线)。
//!
//! **调用点要走 `Runtime` 句柄上的 `spawn_blocking`,不是自由函数
//! `tokio::task::spawn_blocking`**:GUI 线程不在 runtime 上下文里,自由函数
//! 形态会在运行期直接 panic(编译得过、测试全绿,只有真机才炸)。
//! 这个约束靠 `app.rs` 那边的调用点守着。

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
    ///
    /// **判据是「真的只打了这么多次」,不是「常量落在某个区间」。**
    /// 后者是对一个字面量做断言:循环写成 `loop {}` 忘了用这个常量,
    /// 它照样绿。
    #[test]
    fn retries_are_bounded_so_a_always_409_server_cannot_spin_forever() {
        let mut calls = 0;
        let r = put_with_retry("p/", 1, "20260915T101500Z", |_| {
            calls += 1;
            Err(CloudError::AlreadyExists)
        });
        assert!(r.is_err(), "全程 409 却报成功");
        assert_eq!(
            calls, MAX_PUT_ATTEMPTS as usize,
            "实际打了 {calls} 次,与上限对不上 —— 循环没用这个常量"
        );
    }

    /// 撞号之后返回的必须是**真正写成功的那个序号**,不是一开始那个。
    ///
    /// 返回错的话,游标会被推到一个并不存在的序号上,下一轮从那儿 +1,
    /// 中间空出来的号永远不会被用 —— 而 `keep` 份的清理是按序号算的。
    #[test]
    fn the_sequence_that_comes_back_is_the_one_that_actually_landed() {
        let mut left = 2;
        let seq = put_with_retry("p/", 5, "20260915T101500Z", |_| {
            if left > 0 {
                left -= 1;
                Err(CloudError::AlreadyExists)
            } else {
                Ok(())
            }
        })
        .expect("第三次该成功");
        assert_eq!(seq, 7, "撞了两次之后落在 7,返回的却是 {seq}");
    }

    /// 不是撞号的错误**立刻停**,不要拿它去消耗重试次数 —— 403(签名/权限)
    /// 重试四次还是 403,只是把用户等待的时间乘以四。
    #[test]
    fn a_non_collision_error_stops_immediately() {
        let mut calls = 0;
        let r = put_with_retry("p/", 1, "20260915T101500Z", |_| {
            calls += 1;
            Err(CloudError::Status { code: 403, body: "SignatureDoesNotMatch".into() })
        });
        assert!(r.is_err());
        assert_eq!(calls, 1, "非撞号的错误也在重试 —— 打了 {calls} 次");
    }

    /// 成功之后游标必须**同时**推进指纹与序号。
    ///
    /// 只推指纹的话:下一次会算出同一个序号,`ForbidOverwrite` 把它挡掉,
    /// 表现成「备份莫名其妙失败」。
    /// 只推序号的话:指纹永远对不上,每一轮都重推一份内容相同的包,
    /// N 份历史窗口在几小时内被自己刷光。
    ///
    // `CloudConfig` 的 `corrupt` 字段是私有的(Task 8 的设计),于是
    // `CloudConfig { .., ..Default::default() }` 在 **mullion-store 之外**
    // 编不过(E0451:field `corrupt` is private)。只能 default 完再逐字段赋,
    // 而那正好是 `field_reassign_with_default` 要抓的形状 —— 这里没有别的写法,
    // 不是懒。**别把 `corrupt` 改成 pub 来迎合这条 lint**:它私有的理由
    // (守护必须待在 `save` 内部)比这条 style lint 重要得多。
    #[allow(clippy::field_reassign_with_default)]
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
    // `#[allow]` 的理由同上一条:`corrupt` 私有,FRU 在本 crate 里编不过。
    #[allow(clippy::field_reassign_with_default)]
    #[test]
    fn a_failed_upload_leaves_the_cursor_alone() {
        let mut cfg = mullion_store::CloudConfig::default();
        cfg.last_fingerprint = "old".into();
        cfg.last_seq = 3;
        let before = cfg.clone();
        record_failure(&mut cfg);
        assert_eq!(cfg, before, "失败之后游标被动过了 —— 这次的改动会永远推不上去");
    }

    /// 「从没成功推过」必须折算成**已经过了任意久**,不是 0。
    ///
    /// 折算成 0 的话,刚开启云备份的用户要等满一个 `interval_min` 才有
    /// 第一份 —— 默认 30 分钟,而他刚点完「确定」正盯着状态栏看。
    /// `should_upload` 的文档把这个契约写死成 `u64::MAX`,这里钉住它。
    #[test]
    fn a_config_that_never_succeeded_reads_as_overdue_not_as_just_now() {
        assert_eq!(minutes_since_last_ok("", some_time()), u64::MAX);
        assert_eq!(
            minutes_since_last_ok("不是时间戳", some_time()),
            u64::MAX,
            "读不懂的时间戳被当成「刚刚推过」—— 那会永远推不出去且零报错"
        );
    }

    /// **未来的 `last_ok_at` 要当成「到点了」,不是「刚刚才推过」。**
    ///
    /// 本项目在 F253~F256 上踩过同一形状:`is_alive` 把未来的心跳算成
    /// 「永远活着」。这里若照那样写,一次时钟回拨(或者换台时区/时钟不准的
    /// 机器推过一份)就会让这台机器**在那个未来时刻到来之前永不备份** ——
    /// 可能是几个月,期间状态栏一片安静,零报错。
    #[test]
    fn a_timestamp_from_the_future_counts_as_overdue_not_as_fresh() {
        let now = some_time();
        let ahead = now + time::Duration::days(400);
        let ahead_s = ahead
            .format(&time::format_description::well_known::Rfc3339)
            .expect("格式化");
        assert_eq!(
            minutes_since_last_ok(&ahead_s, now),
            u64::MAX,
            "未来的时间戳被当成「刚推过」—— 这台机器要等到那一刻才会再备份"
        );
    }

    /// 正常情况按分钟折算,且**向下取整**(59 秒不算一分钟)。
    #[test]
    fn a_normal_gap_converts_to_whole_minutes() {
        let now = some_time();
        for (secs, want) in [(0_i64, 0_u64), (59, 0), (60, 1), (5400, 90)] {
            let then = now - time::Duration::seconds(secs);
            let s = then
                .format(&time::format_description::well_known::Rfc3339)
                .expect("格式化");
            assert_eq!(minutes_since_last_ok(&s, now), want, "差 {secs} 秒");
        }
    }

    /// 测试用的固定时刻。**不取 `now_utc()`** —— 拿真实时钟的测试会在
    /// 某些时刻偶发地红,而那种红没人查得动。
    fn some_time() -> time::OffsetDateTime {
        time::OffsetDateTime::from_unix_timestamp(1_789_000_000).expect("固定时刻")
    }

    /// `upload_blocking` **不许认识 `Vault`**。
    ///
    /// 它跑在 `spawn_blocking` 线程上,而 `Vault` 住在 `App.store` 里、
    /// 没有 `Clone` —— 今天靠借用检查挡着。但只要有人哪天给 `Vault` 加一个
    /// `Clone`,「顺手把 vault 传进去」就编得过了,而那等于允许两份 Vault
    /// 各自 `save()` 互相覆盖(F247/F248 刚修完的「整份覆盖」缺陷族)。
    ///
    /// 扎在**签名**上而不是整个函数体:函数体里出现 `Vault` 这个词的地方
    /// 还有文档注释,而签名是唯一说明「什么东西跨了线程」的那一行。
    ///
    /// 自证会变红:把 `vault: &Vault` 加回 `upload_blocking` 的参数表。
    #[test]
    fn the_blocking_half_does_not_know_about_the_vault() {
        let src = include_str!("cloudsync.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .expect("测试模块分界变了,这条测试的锚点失效了");
        assert!(
            prod.len() < src.len(),
            "没能切掉测试模块 —— 下面这条断言会恒真"
        );
        let at = prod
            .find("pub fn upload_blocking(")
            .expect("找不到 upload_blocking");
        let tail = &prod[at..];
        let end = tail.find(") -> ").expect("签名没闭合");
        let sig = &tail[..end];
        assert!(
            !sig.contains("Vault"),
            "upload_blocking 的签名里出现了 Vault —— 它跑在别的线程上:{sig}"
        );
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

/// 把 `cloud.toml` 里的 `last_ok_at` 折算成 [`cloud::should_upload`] 要的
/// 「距上次成功过了几分钟」。**折算住在 app 这一侧** —— store 不持时钟,
/// 也没有 `time` 依赖(别为这一个函数给它加一个)。
///
/// 两种输入都折算成 `u64::MAX`(「已经过了任意久」,该推):
///
/// - **空 / 读不懂** —— 从没成功推过。折算成 0 的话,刚开启云备份的用户
///   要等满一个 `interval_min` 才见到第一份(默认 30 分钟),而他正盯着
///   状态栏看。
/// - **在未来** —— 时钟回拨,或者另一台时钟不准的机器推过一份。
///   **本项目在 F253~F256 上踩过同一形状**(`is_alive` 把未来的心跳算成
///   「永远活着」):照那样写,这台机器要等到那个未来时刻才会再备份,
///   可能是几个月,期间一片安静、零报错。
pub fn minutes_since_last_ok(last_ok_at: &str, now: time::OffsetDateTime) -> u64 {
    let Ok(then) = time::OffsetDateTime::parse(
        last_ok_at,
        &time::format_description::well_known::Rfc3339,
    ) else {
        return u64::MAX;
    };
    let secs = (now - then).whole_seconds();
    if secs < 0 {
        return u64::MAX;
    }
    (secs as u64) / 60
}

/// 这一轮内容的指纹。[`prepare`] 里也要算一次 —— **这是有意的重复**。
///
/// 定时那一路必须先拿到指纹才问得了 [`cloud::should_upload`](「内容变没变」
/// 是它四道闸里的一道),而 `prepare` 那次顺带还把载荷封好了、只在真要推的
/// 时候才跑。省掉这里这次的唯一办法是把四道闸拆散塞进 `prepare`,那样
/// `should_upload` 就没人调用了 —— 而配置完整性那道闸也就永远不会跑,
/// 开着开关但没填完的用户每轮发一次注定 403 的请求,状态栏报「备份失败」,
/// 把真正的原因吃掉。
///
/// 成本:读四个几十 KB 的文件 + 一次 sha256,每个轮询 tick 一次。微秒级。
///
/// 返回 `Err` 的唯一原因是 `secrets.enc` **在但读不出来**(见 `read_secrets`)。
/// 那种情况必须往上报而不是按空字节继续 —— 否则会算出「secrets 为空」那一版
/// 指纹,让一份密文段是空的备份被正常加密、正常上传、正常推进游标。
pub fn fingerprint_now(dir: &Path) -> Result<String, String> {
    let files = portable::collect_top_level(dir);
    let secrets = read_secrets(dir)?;
    Ok(cloud::fingerprint(&files, &secrets))
}

/// 读 `secrets.enc`。**「不存在」与「读不出来」必须分开。**
///
/// 不存在是合法状态(这台机器从没存过密码),按空字节继续。
///
/// 读不出来(权限 / IO / 被杀软短暂锁住 —— Windows 上这几样都不罕见)
/// **必须报错**。静默当成空字节的话:指纹算成「secrets 为空」那一版、
/// 包里密文段是空的,而这一份会被正常加密、正常上传、正常推进游标 ——
/// 全程零报错。指纹推进之后,除非内容再变一次,这个空洞不会被下一轮覆盖
/// 修掉。等到真要拿它恢复的那天,`portable` 把空密文段解释成「源机没有
/// 密码」(那是**合法**状态,见 `portable.rs:229`),恢复流程也不报错,
/// 只是恢复完发现所有会话都要重新输凭据。
///
/// 一路都在报成功 —— 这是备份功能最致命的失败模式。
fn read_secrets(dir: &Path) -> Result<Vec<u8>, String> {
    match std::fs::read(dir.join("secrets.enc")) {
        Ok(b) => Ok(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("读不出 secrets.enc:{e}")),
    }
}

/// 一次上传的**主线程那一半**的产物。
pub struct Payload {
    /// 这一份的内容指纹。上传成功后由 `app.rs` 写回游标。
    pub fingerprint: String,
    /// 已经用 vault key 整体封好的字节。**云上那份就是它。**
    pub sealed: Vec<u8>,
    /// 解出来的 Access Key Secret。
    pub secret_access_key: String,
}

/// [`prepare`] 的三种结局。
pub enum Prepared {
    /// 内容没变。**连线程都不用起** —— 更不用建 TCP。
    Unchanged,
    Ready(Payload),
    /// 还没送出去就失败了(没设主密码、SK 读不出、打包失败)。
    Failed(String),
}

/// 上传的**主线程那一半**:装 + 封 + 解 SK。
///
/// # 为什么拆成两半
///
/// `Vault` **搬不进 `spawn_blocking`**:它住在 `App.store` 里、没有 `Clone`,
/// 而给它加一个 `Clone` 等于允许「两份 Vault 各自 `save()` 互相覆盖」——
/// 那正是 F247/F248 刚修完的「整份覆盖」缺陷族。于是凡是要 vault 的活
/// (封载荷、解 SK)全留在事件循环线程上,搬进线程的只有**字节**。
///
/// 这一半**全是纯 CPU**:读四个几十 KB 的文件、一次 sha256、一次
/// XChaCha20 —— 微秒级,不会卡帧。真正会卡的是网络,那一半在
/// [`upload_blocking`] 里。
///
/// 顺带的好处:指纹比对也在这儿做,内容没变时连 `spawn_blocking` 都不起。
pub fn prepare(dir: &Path, vault: &Vault, cfg: &CloudConfig, stamp_rfc3339: &str) -> Prepared {
    // ① 装:顶层三文件 + 密文。**不带 layouts**(设计 D7)。
    //
    // 这里把 `secrets.enc` **原样**放进包,而不是像 F46-a 的本地迁移包那样
    // 用一次性口令重封(`portable::seal_secrets`)。成立的前提**只有一条**:
    // 设计 D6 要求云备份必须先设主密码,于是 `secrets.enc` 的文件头一定是
    // Argon2id、盐随文件走,另一台机器拿主密码就解得开。
    //
    // **这条前提一旦松动(比如哪天允许钥匙串方案也上传),这里必须同步改成
    // 重封** —— 否则拉回来的密文用的是源机钥匙串里的密钥,换台机器一个字
    // 都解不开,而症状是「每条会话都要重新输密码」且零报错(本项目在 F46-a
    // 上已经踩过一次)。`seal_with_master` 在钥匙串方案下会返回
    // `NoMasterPassword`,那是今天挡住这条路的东西,别把它绕过去。
    let files = portable::collect_top_level(dir);
    // 「读不出来」不能当成「没有」,见 `read_secrets` 的文档。
    let secrets = match read_secrets(dir) {
        Ok(b) => b,
        Err(e) => return Prepared::Failed(e),
    };
    let fp = cloud::fingerprint(&files, &secrets);
    // 定时那一路已经在 `drive_cloud_backup` 里问过 `should_upload` 了(其中
    // 一道闸就是指纹)。这里**还要再判一次**,因为**手动**那一路是绕过
    // `should_upload` 的 —— 用户点「立刻备份到云」时,「开关关着」「还没到点」
    // 都不该拦他,但「内容没变」要拦(并且要说出这句话,见 `spawn_cloud_backup`)。
    if fp == cfg.last_fingerprint {
        return Prepared::Unchanged;
    }
    // 注意 `secrets.enc` 的字节**不是内容的函数**:`crypto::encrypt` 每次换
    // 一个随机 nonce,所以 vault 存一次盘、密文整个变一遍,哪怕里头一个字段
    // 都没改。后果是「任何一次 vault save 都会触发一次上传」。今天可以接受
    // (vault save 本来就对应一次真实改动),但若以后出现「定时重写 secrets.enc」
    // 之类的路径,这一条会把 keep 份历史窗口刷光 —— 那时候要做的是把指纹的
    // 密文分量换成对**明文载荷**取,不是去调大 interval。

    // ② 封:先拼成 F46-a 的包文本,再**整体**用 vault key 加密(设计 D5)。
    //    整体加密之后,sessions.toml 里的真机 IP / 用户名 / 跳板拓扑不落云端明文。
    let text = match portable::write_pack(files, &secrets, env!("CARGO_PKG_VERSION"), stamp_rfc3339)
    {
        Ok(t) => t,
        Err(e) => return Prepared::Failed(format!("打包失败:{e}")),
    };
    let sealed = match vault.seal_with_master(text.as_bytes()) {
        Ok(b) => b,
        Err(mullion_store::StoreError::NoMasterPassword) => {
            return Prepared::Failed(
                "云端备份需要先设置主密码 —— 钥匙串里的密钥换台机器解不开".into(),
            )
        }
        Err(e) => return Prepared::Failed(format!("加密失败:{e}")),
    };

    // ③ 解 SK。**这是最后一件需要 vault 的事**,做完之后线程那一半就只剩字节了。
    let sk = match cloud::secret_key(cfg, vault) {
        Ok(s) if !s.is_empty() => s,
        Ok(_) => return Prepared::Failed("还没填 Access Key Secret".into()),
        Err(e) => return Prepared::Failed(format!("读不出 Access Key Secret:{e}")),
    };

    Prepared::Ready(Payload {
        fingerprint: fp,
        sealed,
        secret_access_key: sk,
    })
}

/// 上传的**阻塞那一半**:只碰网络。**必须挖进 `spawn_blocking` 调用**
/// (走 `Runtime` 句柄,见模块文档)。
///
/// **签名里不许出现 `Vault`**,见 [`prepare`] 的那段理由(有守护测试钉着)。
///
/// `stamp_compact` = `YYYYMMDD'T'HHMMSS'Z'`(既当 SigV4 的 `x-amz-date`,
/// 也当对象键里那一段 —— 两者同源,省得出现「键上写着 10 点、签名说 11 点」)。
pub fn upload_blocking(
    payload: Payload,
    cfg: &CloudConfig,
    stamp_compact: &str,
    stamp_rfc3339: &str,
) -> UploadOutcome {
    // `S3Client::new` 返回 `Result`(代理串解析不了时报 `Config`)——
    // **不要写成 `.unwrap()`**:那条路上用户填错代理地址就是当场 panic。
    let socks5 = (!cfg.socks5.is_empty()).then_some(cfg.socks5.as_str());
    let client = match S3Client::new(
        Endpoint {
            base: cfg.endpoint.clone(),
            bucket: cfg.bucket.clone(),
            path_style: cfg.path_style,
        },
        Credentials {
            access_key_id: cfg.access_key_id.clone(),
            secret_access_key: payload.secret_access_key,
        },
        cfg.region.clone(),
        socks5,
    ) {
        Ok(c) => c,
        Err(e) => return UploadOutcome::Failed(format!("云端客户端建不起来:{e}")),
    };

    let start = match client.list_keys(&cfg.prefix, stamp_compact) {
        Ok(keys) => cloud::next_seq(&cfg.prefix, &keys),
        Err(e) => return UploadOutcome::Failed(format!("列举云端对象失败:{e}")),
    };
    match put_with_retry(&cfg.prefix, start, stamp_compact, |key| {
        client.put_no_overwrite(key, &payload.sealed, stamp_compact)
    }) {
        Ok(seq) => UploadOutcome::Ok {
            fingerprint: payload.fingerprint,
            seq,
            at: stamp_rfc3339.to_string(),
        },
        Err(msg) => UploadOutcome::Failed(msg),
    }
}

/// 撞号重试的循环本体。**把网络那一步收进闭包,是为了让这段逻辑测得到。**
///
/// 本项目登记过一种恒绿:「纯函数测得扎实、接线没人看着」。`upload_blocking`
/// 整体要真网络才跑得起来,于是最容易出错的那一段(撞号往前挪、上限、
/// 成功时返回的到底是哪个序号)就变成没人守。抽出来之后,假的 `put`
/// 闭包就能把三种形状全测到。
fn put_with_retry(
    prefix: &str,
    mut seq: u64,
    stamp: &str,
    mut put: impl FnMut(&str) -> Result<(), CloudError>,
) -> Result<u64, String> {
    for _ in 0..MAX_PUT_ATTEMPTS {
        let key = cloud::object_key(prefix, seq, stamp);
        match put(&key) {
            Ok(()) => return Ok(seq),
            // 别的机器抢先用掉了这个序号。**往前挪再试** —— 这是没有 CAS
            // 的服务端上唯一的并发保护(设计 D8)。
            Err(CloudError::AlreadyExists) => match plan_after_collision(seq) {
                Some(next) => seq = next,
                None => return Err("序号用尽".into()),
            },
            Err(e) => return Err(format!("上传失败:{e}")),
        }
    }
    Err(format!(
        "连试 {MAX_PUT_ATTEMPTS} 个序号都被占用 —— 可能有别的机器正在频繁上传"
    ))
}
```

- [ ] **Step 5: 注册模块**

`crates/mullion-app/src/lib.rs` 里加 `pub mod cloudsync;`（按既有顺序插入）。

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test -p mullion-app cloudsync 2>&1 | grep -E "test result|FAILED"`
Expected: 7 passed。

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
| `plan_after_collision` 改成 `Some(taken)` | `a_taken_sequence_number_is_retried_with_a_fresh_one`；`the_sequence_that_comes_back_is_the_one_that_actually_landed` |
| `put_with_retry` 成功时 `return Ok(seq)` 改成 `return Ok(0)` | `the_sequence_that_comes_back_is_the_one_that_actually_landed` |
| `Err(e) => return Err(..)` 那条改成跟 `AlreadyExists` 一样往前挪 | `a_non_collision_error_stops_immediately` |
| 给 `upload_blocking` 的参数表加回 `vault: &Vault` | `the_blocking_half_does_not_know_about_the_vault` |
| `for _ in 0..MAX_PUT_ATTEMPTS` 改成 `for _ in 0..8` | `retries_are_bounded_so_a_always_409_server_cannot_spin_forever`（次数断言） |

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
        // **判据是精确相等,不是 `contains`。** 下面那句提示文案里也带着
        // 「云端备份」四个字,用 `contains` 的话把 `form::section(.., "云端备份", ..)`
        // 整行删掉这条照样绿 —— 而那时候云端那一堆字段会挂在「安全」分节底下,
        // 看起来像是主密码设置的一部分。
        // (已核实:`form::section` 把 title 原样画成一个独立的 `Shape::Text`,
        // 而 `run_env` 收的是每个 `Shape::Text` 的 `galley.text()` 全文,
        // 所以标题那一条就是「云端备份」这四个字本身。)
        assert!(
            texts.iter().any(|t| t == "云端备份"),
            "没有「云端备份」分节标题 —— 那些字段会挂在「安全」底下:{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("需要先设置主密码")),
            "没说清楚为什么用不了:{texts:?}"
        );
    }

    /// 设了主密码之后,开关必须真的可点,且点一下报 Preview(草稿变了,
    /// 要等「确定」才落盘)。
    ///
    /// **最后两个 `true` 是 `store_available` / `has_master_password`** ——
    /// 任一为 `false` 的话整节是 `add_enabled_ui(false)`,点不动,这条会红在
    /// 一个跟它想测的东西无关的原因上。
    #[test]
    fn toggling_the_cloud_switch_reports_a_preview() {
        let mut d = draft();
        let out = interact_env(
            &mut d,
            CLOUD_ENABLED_LABEL,
            egui::Vec2::ZERO,
            true,
            true,
            true,
        );
        assert_eq!(out, SettingsOut::Preview);
        assert!(d.cloud_enabled, "开关没被点开");
    }

    /// 草稿必须从**落盘的那份**起,不是硬编码默认值。
    /// 从默认值起的症状:打开设置弹窗、什么都没动、点「确定」,
    /// 用户配好的云端备份被关掉了。
    ///
    // `CloudConfig::corrupt` 是私有的,`..Default::default()` 在 mullion-store
    // 之外编不过(E0451)。只能 default 完再逐字段赋 —— 没有别的写法。
    // **别为了这条 lint 把 `corrupt` 改成 pub**,它私有是 Task 8 的设计。
    #[allow(clippy::field_reassign_with_default)]
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
        // 先切掉测试模块:这条测试自己的正文里就字面写着 `fn cloud(`
        // 与 `.password(true)`,不切的话锚点和判据都可能落在测试自己身上。
        // (已核实:本文件只有 `settings.rs:573` 一处 `#[cfg(test)]`。)
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .expect("测试模块分界变了,这条测试的锚点失效了");
        assert!(prod.len() < src.len(), "没能切掉测试模块 —— 下面会考到测试自己");
        let body = prod.split("fn cloud(").nth(1).expect("没有 cloud 分节函数");
        // 分节函数都在顶格,下一个 `\nfn ` 就是本节的结束。
        let head = body.split("\nfn ").next().unwrap_or(body);
        let idx = head.find("cloud_secret").expect("cloud 分节里没有 SK 输入框");
        // **按行取窗口,不要按字节切。** `head[idx..idx + 300]` 在这个满是中文
        // 注释的文件里几乎必然切在 UTF-8 字符中间 —— 那是 panic,不是红,
        // 报出来的信息跟「SK 没打码」毫无关系。`head[idx..]` 是安全的:
        // `idx` 来自 `find`,一定在字符边界上,切到结尾永远合法。
        let window: String = head[idx..].lines().take(12).collect::<Vec<_>>().join("\n");
        assert!(
            window.contains(".password(true)"),
            "SK 输入框不是密码框 —— 截图发出去就跟着走了:{window}"
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
    /// SOCKS5 代理,空 = 直连。见 `CloudConfig::socks5` 上那段理由。
    pub cloud_socks5: String,
    pub cloud_access_key_id: String,
    /// 新填的 SK。**空 = 不改**(不是「清空」):每次打开设置都要用户重打一遍
    /// 一串 30 位的密钥,是在逼人把它记在别处。
    pub cloud_secret_new: String,
```

并**把现有的 `from_settings`（`settings.rs:84-97`）改成一行转发**，让新的
`from_settings_and_cloud` 成为唯一那个穷尽字面量。今天它长这样：

```rust
    /// 从落盘的设置起一份草稿。
    pub fn from_settings(s: &mullion_store::Settings) -> Self {
        Self {
            family: s.font_family.clone(),
            font_pt: s.font_pt,
            typed: s.font_family.clone().unwrap_or_default(),
            new_password: String::new(),
            confirm_password: String::new(),
            tmux_bootstrap: s.tmux_bootstrap,
            shell_osc7_bootstrap: s.shell_osc7_bootstrap,
            show_hidden_files: s.show_hidden_files,
            log_level: s.log_level,
        }
    }
```

改成这两个（**注意方向**：新的是原始构造器，旧的转发给它）：

```rust
    /// 从落盘的设置起一份草稿。云端那一节按 `CloudConfig::default()` 起手。
    ///
    /// **生产路径上没人该调它** —— 设置弹窗走
    /// [`Self::from_settings_and_cloud`],因为这一个读不到 `cloud.toml`,
    /// 拿它起的草稿云端字段全是默认值,而「确定」是会把草稿写回
    /// `cloud.toml` 的:endpoint / bucket / AK 会被一次「打开设置再点确定」
    /// 悄悄清空(本项目登记过的「整份覆盖」缺陷族,已经踩过五处)。
    /// 留着它只为那些跟云端毫无关系的单测能少写一个参数;
    /// `app.rs` 里有一条守护钉着生产代码不许出现它。
    pub fn from_settings(s: &mullion_store::Settings) -> Self {
        Self::from_settings_and_cloud(s, &mullion_store::CloudConfig::default())
    }

    /// F271:从落盘的设置 + 落盘的云配置起一份草稿。
    ///
    /// **两个文件各读各的** —— 设置在 `settings.toml`,云配置在 `cloud.toml`,
    /// 后者不进迁移包(见 `mullion_store::cloud` 的模块文档)。
    ///
    /// 这里是 `SettingsDraft` **唯一**的穷尽字面量。加字段时只有这一处要改,
    /// 漏了当场编译不过 —— 而不是「有两处、改了一处、另一处悄悄给了错值」。
    pub fn from_settings_and_cloud(
        s: &mullion_store::Settings,
        c: &mullion_store::CloudConfig,
    ) -> Self {
        Self {
            family: s.font_family.clone(),
            font_pt: s.font_pt,
            typed: s.font_family.clone().unwrap_or_default(),
            new_password: String::new(),
            confirm_password: String::new(),
            tmux_bootstrap: s.tmux_bootstrap,
            shell_osc7_bootstrap: s.shell_osc7_bootstrap,
            show_hidden_files: s.show_hidden_files,
            log_level: s.log_level,
            cloud_enabled: c.enabled,
            cloud_endpoint: c.endpoint.clone(),
            cloud_region: c.region.clone(),
            cloud_bucket: c.bucket.clone(),
            cloud_prefix: c.prefix.clone(),
            cloud_path_style: c.path_style,
            cloud_keep: c.keep,
            cloud_interval_min: c.interval_min,
            cloud_socks5: c.socks5.clone(),
            cloud_access_key_id: c.access_key_id.clone(),
            // **空 = 不改**,不是「清空」。见字段上那段理由。
            cloud_secret_new: String::new(),
        }
    }
```

**别反过来写**（`from_settings_and_cloud` 里用 `..Self::from_settings(s)`）——
那是无限递归，而且会留下两处字面量。

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

            // **这一行不能省。** 片一完全不删云端对象(清理是片二的活),
            // 这个数字存得下、也回得来,但没有任何代码会用它 —— 不说明的话
            // 它就是一个假开关:用户设成 5,以为云上只会留 5 份,实际一直在涨,
            // 直到有天发现 bucket 里几百个对象。这类「看得见摸不着的开关」
            // 本项目在 F265 上刚吃过一次(「灯早就有了,用户根本没注意到」的
            // 反面:控件早就有了,用户以为它在起作用)。
            //
            // 小字用 `.size(11.0)` + `c32(t.fg_muted)`,跟本分节另外两处说明
            // 以及 `settings.rs` 里其余六处同形。**别改成 `theme::hint_text`**:
            // 那一层是给 `TextEdit` 的 hint 用的(egui 派生的 weak 色达不到 AA),
            // 它给的是 `fg_dimmer`,跟并排的两段说明会深浅不一。这个文件里两套
            // 写法确实并存(6 处 vs 2 处),新写的一律跟多数那套走,
            // 至少别在同一个分节里混用。
            ui.label("");
            ui.label(
                egui::RichText::new("下一个版本生效：当前版本只往上传，不清理旧份")
                    .size(11.0)
                    .color(theme::c32(t.fg_muted)),
            );
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

            // SOCKS5 代理。**不补这一格的话 `socks5` 参数就是条死线** ——
            // `mullion-cloud` 为它开了 ureq 的 `socks-proxy` 特性、
            // `S3Client::new` 专门收了这个参数,而设计 D15 把「SOCKS 代理
            // 链路通不通」列进了片一的真机验收项。没有入口就永远传 `None`,
            // 那条验收项验的是一条从没走过的路。
            ui.label("SOCKS5 代理");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut draft.cloud_socks5)
                        .desired_width(w)
                        // **hint 里写清不带 `socks5://`**:`S3Client::new` 自己
                        // 补前缀,用户照直觉填全 URL 的话会拼成
                        // `socks5://socks5://…` 而当场报「配置不合法」。
                        .hint_text("127.0.0.1:1080,留空 = 直连"),
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

- [ ] **Step 5: 修 `settings.rs` 自己那个穷尽的 `draft()` 辅助**

**测试辅助的签名不用动，已核实过（别按印象改）：**

```rust
fn run_env(d: &mut SettingsDraft, not_monospace: bool, has_master_password: bool)
    -> (Vec<String>, SettingsOut)          // settings.rs:618，内部写死 store_available: true

fn interact_env(d: &mut SettingsDraft, label: &str, offset: egui::Vec2, release: bool,
                store_available: bool, has_master_password: bool) -> SettingsOut   // settings.rs:680
```

**要动的是 `fn draft()`（`settings.rs:590`）** —— 它是**穷尽结构体字面量**，
今天列了 9 个字段、没有 `..`：

```rust
    fn draft() -> SettingsDraft {
        SettingsDraft {
            family: Some("Cascadia Mono".into()),
            font_pt: 10.0,
            typed: "Cascadia Mono".into(),
            new_password: String::new(),
            confirm_password: String::new(),
            tmux_bootstrap: true,
            shell_osc7_bootstrap: true,
            show_hidden_files: true,
            log_level: mullion_store::LogLevel::Info,
        }
    }
```

Step 3 加了 11 个字段之后**这里当场编译不过**。改成：

```rust
    fn draft() -> SettingsDraft {
        // 只覆盖这一组测试真正在意的那几项,其余从构造器起手。
        // **不要**把 11 个云端字段一个个补进来 —— 那样每加一个字段都要回来
        // 改一次,而补错值的表现是这一整组测试悄悄测了别的东西。
        SettingsDraft {
            family: Some("Cascadia Mono".into()),
            font_pt: 10.0,
            typed: "Cascadia Mono".into(),
            ..SettingsDraft::from_settings(&mullion_store::Settings::default())
        }
    }
```

**全库一共有三处穷尽的 `SettingsDraft` 字面量，三处都要改，漏一处就编不过**
（已核实；其余五处都是 `..draft()`，不用动）：

1. `settings.rs:590` `fn draft()` —— 就是上面这一处。
2. `settings.rs:815`（测试 `a_font_family_that_is_not_installed_says_so`）—— 见下。
3. `app.rs:18252`（测试 `a_password_change_always_clears_the_two_boxes`）—— Step 6。

`settings.rs:815` 今天长这样：

```rust
        let mut d = SettingsDraft {
            family: Some("Comic Sans MS".into()),
            font_pt: 10.0,
            typed: "Comic Sans MS".into(),
            new_password: String::new(),
            confirm_password: String::new(),
            tmux_bootstrap: true,
            shell_osc7_bootstrap: true,
            show_hidden_files: true,
            log_level: mullion_store::LogLevel::Info,
        };
```

改成只覆盖它真正在意的那三项（这条测的是「选了没装的字体要提示」，跟别的字段
一点关系都没有）：

```rust
        let mut d = SettingsDraft {
            family: Some("Comic Sans MS".into()),
            font_pt: 10.0,
            typed: "Comic Sans MS".into(),
            ..draft()
        };
```

（`draft()` 就在同一个 `mod tests` 里，同文件另外四处 `SettingsDraft { .. }`
用的都是它。）

**注意语义变化**：`from_settings(&Settings::default())` 给的 `tmux_bootstrap` /
`shell_osc7_bootstrap` / `show_hidden_files` / `log_level` 是 `Settings::default()`
的值，未必都等于原来写死的 `true`。**改完先跑整组 `settings` 测试**；若有测试因此
变红，说明它依赖的是那几个写死的 `true`，把那一项在**该条测试内部**显式设回去，
**不要**改回穷尽字面量。

- [ ] **Step 6: 修 `app.rs` 里那个穷尽的 `SettingsDraft` 字面量**

`crates/mullion-app/src/app.rs:18252`（测试 `a_password_change_always_clears_the_two_boxes`）
用**穷尽结构体字面量**构造 `SettingsDraft`，今天列了 9 个字段、**没有 `..Default`**：

```rust
            let mut d = crate::ui::settings::SettingsDraft {
                family: None,
                font_pt: 10.0,
                typed: String::new(),
                new_password: "hunter2".into(),
                confirm_password: "hunter2".into(),
                tmux_bootstrap: true,
                shell_osc7_bootstrap: true,
                show_hidden_files: true,
                log_level: mullion_store::LogLevel::Info,
            };
```

Step 3 加了 11 个字段之后**这里当场编译不过**。把它改成从构造器起手、只覆盖这条
测试真正关心的两个字段：

```rust
            // 这条测的是「改完密码两个框要清空」,跟别的字段一点关系都没有。
            // **不要**在这里把新字段一个个补齐 —— 那样每加一个字段都要回来改一次,
            // 而漏改的表现是编译失败(还好),补错值的表现是这条测试悄悄测了别的东西。
            let mut d = crate::ui::settings::SettingsDraft {
                new_password: "hunter2".into(),
                confirm_password: "hunter2".into(),
                ..crate::ui::settings::SettingsDraft::from_settings(
                    &mullion_store::Settings::default(),
                )
            };
```

**别顺手给 `SettingsDraft` 加 `#[derive(Default)]`** —— `font_pt: 10.0` 之类的值
不是 `Default` 该给的，`from_settings` 才是这个结构体唯一的正经起点。

- [ ] **Step 7: 跑测试确认通过**

Run: `cargo test -p mullion-app settings 2>&1 | grep -E "test result|FAILED"`
Expected: 全绿。

- [ ] **Step 8: 跑字形白名单与表单规范守护**

Run: `cargo test -p mullion-app --test glyph_whitelist --test form_guidelines --test dialog_contrast --test strong_text_color 2>&1 | grep -E "test result|FAILED"`
Expected: 全绿。若 `glyph_whitelist` 报红，说明文案里混进了 GBK 外的字形 —— 改文案，
**不要**去改白名单。

- [ ] **Step 9: 提交并变异验证**

```bash
git add crates/mullion-app/src/ui/settings.rs
git commit -m "feat(app): 设置弹窗加云端备份分节,未设主密码时整节置灰 (F271)"
```

| 变异 | 应该变红的测试 |
|---|---|
| `let ready = ...` 改成 `true` | `the_cloud_section_is_disabled_and_explains_itself_without_a_master_password` |
| `show()` 里删掉 `form::section(ui, t, "设置", "云端备份", &mut first);` 那一行 | 同上（**判据必须是精确相等**：提示文案里也带着「云端备份」四个字，用 `contains` 的话这条杀不掉） |
| `.password(true)` 删掉 | `the_secret_key_field_is_masked` |
| `from_settings_and_cloud` 里 `cloud_enabled` 改成 `false` | `the_cloud_draft_starts_from_the_stored_config_not_a_hardcoded_default` |
| `from_settings` 改回穷尽字面量，云端字段填死值 | **杀不掉**（只要填的值恰好等于 `CloudConfig::default()`，就是等价变异）。**如实记下来，别硬编守护** —— 真正挡住漂移的是「`from_settings_and_cloud` 是唯一的穷尽字面量」这个结构，加字段时漏改当场编译不过 |
| `checkbox` 的 `.changed()` 分支里不写 `*out = SettingsOut::Preview;` | `toggling_the_cloud_switch_reports_a_preview` |

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
    /// **扎的是源码结构**(菜单项要展开 `menu_button` 才画得出来,跑帧测不到),
    /// 且**先切掉测试模块**再找 needle —— `include_str!` 拿到的是含这条测试
    /// 自己的全文。`str::split` 找不到分隔符时会把整串原样还回来,所以额外
    /// 钉一条「切完确实变短了」的兜底。
    ///
    /// 照抄同文件 `the_settings_menu_has_a_permanent_entry_to_export_the_redacted_log`
    /// 的形态。**刻意不用同文件另一条(F156 那条)的「靠行首缩进躲开自己」写法**:
    /// 那招能成立只是因为测试体里的 needle 带反斜杠转义、字节恰好与生产代码不同,
    /// 一旦有人把它抽成常量或改写成 raw string 就当场恒真。
    ///
    /// 自证会变红:把 `chrome.rs` 里「立刻备份到云」那个菜单项删掉。
    #[test]
    fn the_config_menu_has_a_permanent_entry_to_back_up_now() {
        let src = include_str!("chrome.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .expect("测试模块分界变了,这条测试的锚点失效了");
        assert!(
            prod.len() < src.len(),
            "没能切掉测试模块 —— 下面这条断言会恒真"
        );
        assert!(
            prod.contains("if ui.button(\"立刻备份到云\").clicked() {"),
            "「配置」菜单里没有手动备份入口"
        );
    }

    /// 状态栏的云指示器在**没配置时不占格**(同隧道指示器那条理由:
    /// 每多一格常驻信息,别的信息就少一分被看见的机会)。
    ///
    /// **判据是两态的条数差,不只是「没有『云』字」。** 只判没有「云」字的话,
    /// 「`None` 时画一个空标签」这条变异逃得掉 —— 空串里当然没有「云」,
    /// 而格子实实在在占掉了。条数差把「根本没画」(差 0)、「画了」(差 1)、
    /// 「跟着多画了别的」(差 ≥2)三种分开。
    #[test]
    fn the_cloud_cell_is_absent_when_cloud_backup_is_off() {
        let off = status_texts(None, None, None, None);
        let cell = CloudCell {
            text: "云 已备份 #1".into(),
            severity: crate::tunnels::Severity::Calm,
        };
        let on = status_texts(None, None, None, Some(&cell));
        assert!(
            !off.iter().any(|t| t.contains("云")),
            "关着的时候还占了一格:{off:?}"
        );
        assert_eq!(
            on.len(),
            off.len() + 1,
            "开关两态画出来的文字条数应该正好差一条。差 0 = 那一格根本没画;\
             差 ≥2 = 有别的东西跟着变了。off={off:?} on={on:?}"
        );
    }

    /// 配了就必须画出来,而且**失败要看得见**。
    /// 「静默失败」是备份功能唯一致命的失败模式 —— 用户以为有备份,直到需要
    /// 它的那天才发现没有。
    #[test]
    fn a_failing_cloud_backup_is_shown_in_the_status_bar() {
        let cell = CloudCell {
            text: "云 备份失败".into(),
            severity: crate::tunnels::Severity::Danger,
        };
        let texts = status_texts(None, None, None, Some(&cell));
        assert!(
            texts.iter().any(|t| t.contains("备份失败")),
            "备份失败没出现在状态栏:{texts:?}"
        );
    }
```

**已核实的接线**（写代码前不必再 grep，但要按这份改）：

- 测试辅助 `status_texts` 已存在于 `chrome.rs:685`，签名是
  `fn status_texts(automation, tunnel, selection_path) -> Vec<String>`。
  **给它加第四个参数 `cloud: Option<&CloudCell>`**，不要新建一个
  `status_texts_with_cloud` —— 两个辅助会有九成重复。既有的四处调用点
  （`chrome.rs` 的 728 / 746 / 767 / 786 行）补一个 `None`。
- 同文件还有 `run_status`（649 行）里两处 `status_bar(..)` 调用、
  以及 `annotate` 那条测试（815 行）一处，同样补 `None`。

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

**已核实**：`UiState`（`ui/mod.rs`）没有 `has_real_action` 之类的完备性方法，
所以这里不需要额外补一笔。但它的同族字段各有各的消费点，形态有两种：
`export_log_request` 走具名的 `drain_export_log_request()` 并配了一条守护
`drain_export_log_request_is_both_defined_and_called`；`pack_pick_request`
走 `app.rs:14020` 的内联 `std::mem::take(&mut self.ui.pack_pick_request)`。

`cloud_backup_request` 的**消费点在 Task 14**（本任务只置位）。Task 14 里
**必须**用 `std::mem::take` 消费 —— 只读不清的话这个 bool 永远是 `true`，
在途标记一还回来下一帧就再起一次，变成无限重传。

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

**已核实**：`status_bar`（`chrome.rs:475`）现在是 9 个参数，头上已经挂了
`#[allow(clippy::too_many_arguments)]`，加第十个不会撞 clippy。

**数据源放 `UiState`，不放 `App`。** 在 `ui/mod.rs` 的 `UiState` 里
（`last_error` 附近）加：

```rust
    /// F273:最近一次云端备份的结论,状态栏那一格的数据源。
    /// `None` = 从没备份过 → 不占格。
    ///
    /// **住在 `UiState` 而不是 `App`**:状态栏是从 `ui_state` 和 `UiFrame`
    /// 两处取料画出来的,而 `ui/mod.rs:961` 那个调用点根本够不着 `App` 的字段。
    /// 放 `App` 的话这一格只能先传 `None` 占位、等下一个任务再回来接 ——
    /// 而「占位忘了接」正是本项目登记过的「量具存在≠接在那条路上」。
    /// 它跟 `last_error` 同性质:一次性的、不落盘的、纯给人看的结论。
    pub cloud_status: Option<chrome::CloudCell>,
```

调用点一共五处，**`app.rs` 里一处都没有**（计划早先写错了）：
- 生产：`crates/mullion-app/src/ui/mod.rs:961` 的 `chrome::status_bar(..)` ——
  **这一处本任务就要真的接上**，传 `ui_state.cloud_status.as_ref()`，不留占位。
- 测试：`chrome.rs` 的 `run_status` 里两处、`status_texts` 里一处、
  `annotate` 那条测试里一处 —— 一律补 `None`。

配套守护（加进 Step 1 那一批）：

```rust
    /// F273:状态栏那一格必须**真的接在** `ui_state.cloud_status` 上。
    ///
    /// `CloudCell` 画得再对,只要生产调用点传的是字面 `None`,用户就永远
    /// 看不见备份结论 —— 而编译、测试、clippy 全干净。本项目已登记同一形状
    /// (「量具存在≠接在那条路上」)。
    ///
    /// 扎在 `ui/mod.rs` 上而不是 `chrome.rs`:调用点在那边。
    ///
    /// 自证会变红:把那个实参改回 `None`。
    #[test]
    fn the_status_bar_is_actually_fed_the_cloud_cell() {
        let src = include_str!("mod.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .expect("测试模块分界变了,这条测试的锚点失效了");
        assert!(prod.len() < src.len(), "没能切掉测试模块 —— 下面那条会恒真");
        assert!(
            prod.contains("ui_state.cloud_status.as_ref()"),
            "状态栏没接上云备份结论 —— 那一格会永远空着"
        );
    }
```

**这条放 `ui/mod.rs` 自己的 `mod tests` 里**（已核实：`ui/mod.rs:1268` 有
`#[cfg(test)] mod tests {`，且全文件只有这一处 `#[cfg(test)]`，所以
`split(..).next()` 切得干净）。别放 `chrome.rs` —— 那样 `include_str!` 要写成
`../mod.rs`，绕一圈没好处。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p mullion-app chrome 2>&1 | grep -E "test result|FAILED"`
Expected: 全绿。

- [ ] **Step 6: 提交并变异验证**

```bash
# **本任务不动 `app.rs`**。状态栏那一格的数据源住在 `UiState::cloud_status`
# (见 Step 4 的理由),生产调用点 `ui/mod.rs:961` **在本任务就真的接上**,
# 不留占位。Task 14 只负责往 `cloud_status` 里写结论。
# 把 app.rs 一起 add 进来只会夹带别的在途改动。
git add crates/mullion-app/src/ui/chrome.rs crates/mullion-app/src/ui/mod.rs
git commit -m "feat(app): 菜单「立刻备份到云」+ 状态栏云指示器 (F273)

失败必须看得见 —— 静默失败是备份功能唯一致命的失败模式。"
```

| 变异 | 应该变红的测试 |
|---|---|
| 菜单项文案改成「备份到云」 | `the_config_menu_has_a_permanent_entry_to_back_up_now` |
| 状态栏那段 `if let Some(c)` 改成 `if false` | `a_failing_cloud_backup_is_shown_in_the_status_bar`；`the_cloud_cell_is_absent_when_cloud_backup_is_off`（条数差变 0） |
| 那段改成无条件画（`None` 时画空串） | `the_cloud_cell_is_absent_when_cloud_backup_is_off`（条数差变 0；只判「没有『云』字」的话这条逃得掉，所以判据是条数差） |
| `ui/mod.rs:961` 的实参改回字面 `None` | `the_status_bar_is_actually_fed_the_cloud_cell` |

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
    ///
    /// **必须先 `strip_comments`**:Step 6 的生产代码注释里就写着
    /// 「必须 spawn_blocking」,不剥的话删掉真正那句调用测试照绿
    /// (本项目已登记的坑,`app.rs` 里几十处共享这个手法)。
    ///
    /// **判据钉的是 `self._runtime.spawn_blocking(move ||`,不是
    /// `tokio::task::spawn_blocking`。** 后者会在运行期 panic:GUI 线程
    /// **不在** tokio runtime 上下文里(`app.rs:11017` 那句注释的原话是
    /// 「GUI 线程不在 runtime 里,得显式进去一趟」),而自由函数形态的
    /// `tokio::task::spawn_blocking` 要求调用处有 runtime 上下文。
    /// 这条错**编译得过、全部测试照绿**,只有真机上第一次备份时当场崩。
    ///
    /// 自证会变红:把 `self._runtime.spawn_blocking(move || { .. })` 拆掉,
    /// 直接同步调 `upload_blocking`。
    #[test]
    fn the_cloud_upload_runs_off_the_event_loop_thread() {
        let body = strip_comments(body_of(prod_src(), "fn spawn_cloud_backup("));
        assert!(
            body.contains("self._runtime.spawn_blocking(move ||"),
            "云端上传没走 runtime 句柄上的 spawn_blocking —— 要么把帧率打到零,\
             要么(用自由函数形态时)因为 GUI 线程不在 runtime 上下文里当场 panic"
        );
    }

    /// 同一时刻只许有一次上传在途。没有这道闸的话:定时器每 30 分钟塞一个,
    /// 而一次高延迟上传可能跑几分钟 —— 手动点几下就能攒出一串并发的
    /// `spawn_blocking`,它们会互相撞号(ForbidOverwrite),表现成
    /// 「备份时好时坏」。
    ///
    /// 判据钉的是**那道闸**(`if self.cloud_in_flight {`),不是裸字段名:
    /// 函数体里还有 `self.cloud_in_flight = true;` 那句,只搜字段名的话
    /// 「把闸删掉」这条变异照样全绿。同样要先 `strip_comments`。
    ///
    /// 自证会变红:删掉 `if self.cloud_in_flight { return; }` 那三行。
    #[test]
    fn only_one_cloud_upload_is_in_flight_at_a_time() {
        let body = strip_comments(body_of(prod_src(), "fn spawn_cloud_backup("));
        assert!(
            body.contains("if self.cloud_in_flight {"),
            "没有在途闸 —— 定时与手动会攒出一串并发上传并互相撞号"
        );
    }

    /// 结果回来时必须把在途标记**归还**。每条出口都要还。
    ///
    /// 这是本项目的常客形状(见 T13 的「hold 每条出口都要归还」):漏一条
    /// 出口的后果是那之后**永远**不再备份,且没有任何报错。
    ///
    /// **锚点必须是 match 分支那一行**,不能用裸的 `UserEvent::CloudBackupDone(`:
    /// `body_of` 取的是 `find` 的**第一次出现**,而 `UserEvent` 枚举定义在
    /// `app.rs:57`、远在 `fn user_event`(11265 行)之前 —— 锚到变体定义上的话
    /// 那一行连 `{` 都没有,`body_of` 会一路截到后面某个不相干的块,断言变成
    /// 在考一段随机代码。
    ///
    /// 归还只允许有**一处**:多写一处就意味着有人在别的分支里补了个兜底,
    /// 而那正是「三种结局共用一个变体」要避免的形状。
    ///
    /// 自证会变红:删掉 `self.cloud_in_flight = false;` 那句(第一条红),
    /// 或者在某个分支里再补一句(第二条红)。
    #[test]
    fn every_path_that_ends_a_cloud_upload_hands_the_in_flight_flag_back() {
        let body = strip_comments(body_of(
            prod_src(),
            "UserEvent::CloudBackupDone(outcome) => {",
        ));
        assert_eq!(
            body.matches("self.cloud_in_flight = false").count(),
            1,
            "在途标记的归还不是恰好一处 —— 漏了之后永远不再备份,多了说明有分支在自己兜底"
        );
    }

    /// 定时驱动必须**每帧都被调到**,而不是挂在某个偶尔才走的分支上。
    ///
    /// **判据扎在 `pump_io` 的函数体里,不是扫全篇找「出现过」。** 扫全篇只
    /// 答得出「有人调过它」,答不出「每帧都调」—— 而把这句挪进任何一个偶尔
    /// 才走的分支(比如某个 `if let Some(ws)` 里),扫全篇那条照样全绿,
    /// 定时备份却变成了「碰运气才跑一次」。判据要放在**两种情形分得开**的
    /// 那一层,这是本项目登记过的形状。
    ///
    /// 选 `pump_io` 是因为它自己的文档就写着「**每帧**调」,而且
    /// `drive_automation` / `drive_attach_checks` / `drive_project_visits`
    /// 三个同族驱动都住在那里 —— 跟它们做邻居,以后谁搬家也会一起搬。
    ///
    /// 同样先 `strip_comments` —— 注释里写一句「这里调 `self.drive_cloud_backup(..)`」
    /// 就能让这条恒绿,而函数体里那句其实被注释掉了。
    ///
    /// 自证会变红:把 `pump_io` 里 `self.drive_cloud_backup(now);` 那句注释掉,
    /// 或者把它挪出 `pump_io`(哪怕挪到另一个每帧都走的地方,这条也会红 ——
    /// 那时候要连同这条守护的锚点一起改,并在注释里说清新宿主为什么每帧走)。
    #[test]
    fn the_cloud_backup_is_driven_every_frame() {
        let body = strip_comments(body_of(prod_src(), "fn pump_io("));
        assert!(
            body.contains("self.drive_cloud_backup("),
            "drive_cloud_backup 不在 pump_io 里 —— 它要么没人调(定时备份从来不发生),\
             要么挂在某个偶尔才走的分支上(变成碰运气才跑一次):{body}"
        );
    }

    /// 「该不该推」这个判断必须**真的走 `cloud::should_upload`**,不能在
    /// 这里自己写闸。
    ///
    /// 自己写必然只写到「开着 + 到点了」这两道。它四道闸里最要紧的是
    /// **配置完整性**:开着开关但 endpoint / AK 没填完的用户,每一轮都发一次
    /// 注定 403 的请求,状态栏报一句「云端备份失败」,把真正的原因
    /// (「还没填完」)吃掉。少调它还有第二重后果:`should_upload` 的六条
    /// 测试全部变成没人调用的死代码 —— 本项目登记过的
    /// **「量具存在≠接在那条路上」**(F264 那次是埋点没接,这次是判据没接)。
    ///
    /// 同时钉住**间隔从 `cfg.last_ok_at` 起算**。改成从内存里那个
    /// `cloud_last_check_ms` 折算的话,起算点就变成「本进程上次看盘的时刻」
    /// 而不是「上次真的推成的时刻」—— 一个开开关关的用户每次启动都会立刻
    /// 推一份,把 keep 份历史窗口按开机次数刷光。这是 **T11**:计时要从
    /// 「事情真的成了」起算,不是从调用点起算。
    ///
    /// 照例先 `strip_comments` —— 上面那段 `drive_cloud_backup` 的文档注释里
    /// 就写着 `should_upload` 这个词,不剥的话把调用整段删掉测试照绿。
    ///
    /// 自证会变红:把 `should_upload(..)` 那一段换成
    /// `if since < u64::from(cfg.interval_min) { return; }`(第一条红),
    /// 或者把 `&cfg.last_ok_at` 换成别的来源(第二条红)。
    #[test]
    fn whether_to_back_up_is_decided_by_should_upload_not_by_a_hand_rolled_gate() {
        let body = strip_comments(body_of(prod_src(), "fn drive_cloud_backup("));
        assert!(
            body.contains("cloud::should_upload(&cfg, &fp, since)"),
            "定时那一路没走 should_upload —— 配置完整性那道闸永远不会跑,\
             没填完的用户会每轮发一次注定 403 的请求"
        );
        // 拆成两条,**不要**写成一条带换行与缩进的整串:那样判据就绑死在
        // rustfmt 当下的折行决定上 —— 谁加一句 `use` 把路径缩短,这一行就
        // 折不起来了,守护当场假红。
        assert!(
            body.contains("minutes_since_last_ok("),
            "没做「距上次成功过了几分钟」的折算"
        );
        assert!(
            body.contains("&cfg.last_ok_at"),
            "间隔不是从持久化的 last_ok_at 起算 —— 开开关关的用户每次启动都会推一份"
        );
    }
```

**已核实的测试辅助**（`app.rs` 的 `mod tests` 里都有，直接用，别自己再造）：
- `prod_src() -> &'static str`（23932 行）：`include_str!("app.rs")` 再切掉
  `\n#[cfg(test)]\nmod tests {` 之后的部分。
- `body_of(production, sig) -> &str`（23903 行）：从 `sig` **第一次出现**处起，
  取到第一个 `{` 之后的大括号配平块。注意「第一次出现」——锚点串必须是那段
  代码独有的形状。
- `strip_comments(body) -> String`（23923 行）：剥掉**整行**注释（行尾注释不剥）。
  它的文档注释里原话是「源码切片断言几乎都得先过这一道…已实证过好几次
  『只删代码、注释原样，测试照绿』」。上面四条守护全部过了这一道。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app cloud 2>&1 | tail -20`
Expected: FAIL。

- [ ] **Step 3: 加 `App` 字段**

```rust
    /// F273:有一次云端备份在途。**必须有这道闸**:定时器每 30 分钟塞一个,
    /// 而一次高延迟上传可能跑几分钟 —— 没闸的话手动点几下就能攒出一串
    /// 并发上传,它们互相撞号(ForbidOverwrite),表现成「备份时好时坏」。
    cloud_in_flight: bool,
    /// F273:上次**看盘**的时刻。**是 `self.now_ms()` 的时基**(自 `self.start`
    /// 起算的相对毫秒),不是 unix 时间。
    ///
    /// 它节流的只是 [`CLOUD_POLL_MS`] 这一层「多久读一次那四个文件算指纹」,
    /// **不是用户配的备份间隔** —— 后者归 `cloud::should_upload` 管,
    /// 从持久化的 `cfg.last_ok_at` 起算。
    ///
    /// 两者不能合并成一个:合并之后间隔的起算点就成了「本进程上次看盘」
    /// 而不是「上次真的推成」,一个开开关关的用户每次启动都会立刻推一份,
    /// 把 keep 份历史窗口按开机次数刷光(T11:计时从「事情真的成了」起算)。
    cloud_last_check_ms: u64,
```

**只加这两个字段。** 状态栏那一格的数据源是 `self.ui.cloud_status`
（`UiState` 上，Task 13 已加并已接到 `ui/mod.rs:961`）——**别在 `App` 上再开一个**，
两份状态必然有一天对不上（影子状态，本项目已踩过若干次）。

- [ ] **Step 4: 加 `UserEvent` 变体**

```rust
    /// F273:一次云端备份跑完了(成功/没变/失败都走这一条)。
    ///
    /// **三种结局共用一个变体**:在途标记的归还只能有一处,分成三个变体
    /// 就变成三处各归还一次 —— 而「漏一条出口」是本项目的常客形状,
    /// 漏掉之后**永远**不再备份且零报错。
    CloudBackupDone(crate::cloudsync::UploadOutcome),
```

**加完会有一处编译不过**：`user_event_marks_dirty`（`app.rs:15202`）是穷尽
`match`。把 `CloudBackupDone(_)` 加进底下「其余一律标脏」那一组（它会改状态栏
那一格，不标脏的话结论要等下一次别的事件才显示出来）。**不要**为了省事改成
`_ =>` —— 那会把以后新加的变体一起吞掉。

- [ ] **Step 5: 给 `SessionStore` 开一个 vault 访问器**

`crates/mullion-app/src/shell/store.rs`（`SessionStore` 是 `Vault` 的薄封装，
`vault` 字段私有）：

```rust
    /// F270:借出底下的 `Vault`,给云端备份封载荷用。
    ///
    /// **只读借用**(`&self`)。云备份那条路上要 vault 做两件纯 CPU 的事
    /// (封整包、解 SK),都不写盘;开成 `&mut` 的话调用点会需要一个可变
    /// 借用,而它跟同一帧里读 `store.list()` 的地方冲突。
    pub fn vault(&self) -> &mullion_store::Vault {
        &self.vault
    }
```

- [ ] **Step 6: 实现 `spawn_cloud_backup` 与 `drive_cloud_backup`**

```rust
    /// F273:起一次云端备份。
    ///
    /// **主线程只做纯 CPU 的那一半**(`cloudsync::prepare`:读四个文件、
    /// 一次 sha256、一次 XChaCha20,微秒级),网络那一半挖进 `spawn_blocking`。
    /// 拆两半的根由是 `Vault` 搬不进线程 —— 见 `cloudsync` 的模块文档。
    ///
    /// 已有在途的就**直接回**(不排队:排队等于把「已经过时的那一份」推上去,
    /// 而下一轮会立刻再推一份新的)。
    ///
    /// `manual` = 用户从菜单点的。**只影响「没变」时说不说话**:定时那条
    /// 每半小时静悄悄地不做事是对的,而用户主动点了「立刻备份到云」却什么
    /// 都不发生,就是「点了没反应」(F265 记过同一形状:功能在那儿,
    /// 用户不知道它起没起作用)。
    fn spawn_cloud_backup(&mut self, manual: bool) {
        if self.cloud_in_flight {
            return;
        }
        let Some(dir) = crate::shell::store::config_dir() else {
            return;
        };
        let cfg = mullion_store::cloud::load(&dir);
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let stamp_rfc3339 = now_rfc3339();
        let stamp_compact = now_compact();
        let payload = match crate::cloudsync::prepare(&dir, store.vault(), &cfg, &stamp_rfc3339) {
            crate::cloudsync::Prepared::Unchanged => {
                if manual {
                    self.ui.set_error("云端备份:内容没变,没有需要上传的改动".into());
                }
                return;
            }
            crate::cloudsync::Prepared::Failed(msg) => {
                self.ui.set_error(format!("云端备份失败:{msg}"));
                return;
            }
            crate::cloudsync::Prepared::Ready(p) => p,
        };
        self.cloud_in_flight = true;
        let proxy = self.proxy.clone();
        // **必须 spawn_blocking**:mullion-cloud 是阻塞式的,在事件循环里
        // 同步跑一次高延迟往返就能把帧率打到零(T3/T7)。
        //
        // **走 `self._runtime` 这个句柄,不要用自由函数 `tokio::task::spawn_blocking`**:
        // GUI 线程不在 runtime 上下文里(同 `app.rs:11017` 那处 `_runtime.enter()`
        // 的理由),自由函数形态会在运行期直接 panic —— 而且编译得过、
        // 测试全绿,只有真机上第一次备份才炸。
        self._runtime.spawn_blocking(move || {
            let outcome =
                crate::cloudsync::upload_blocking(payload, &cfg, &stamp_compact, &stamp_rfc3339);
            let _ = proxy.send_event(UserEvent::CloudBackupDone(outcome));
        });
    }

    /// F273:每帧看一眼该不该起一次定时备份。
    ///
    /// **该不该推这个判断整个交给 [`mullion_store::cloud::should_upload`]**,
    /// 这里不自己写闸。自己写的话必然只写到「开着 + 到点了」这两道 ——
    /// 而它四道闸里最要紧的是**配置完整性**:开着开关但 endpoint 或 AK 没填完
    /// 的用户,每一轮都会发一次注定 403 的请求,状态栏报一句「云端备份失败」,
    /// 把真正的原因(「还没填完」)吃掉。少调它还有第二重后果:那个函数的
    /// 六条测试会全部变成没人调用的死代码 —— 本项目登记过的
    /// 「量具存在≠接在那条路上」。
    ///
    /// 两层节流是**两回事,别合并**:
    ///
    /// - [`CLOUD_POLL_MS`] 是「多久看一眼盘」,固定值,只为不每帧去读那四个
    ///   文件算指纹。
    /// - `cfg.interval_min` 是**用户配的备份间隔**,归 `should_upload` 管。
    ///
    /// 合成一个(照 `interval_min` 来轮询)的话,间隔的起算点就变成了
    /// 「本进程上次看盘的时刻」,而不是「上次成功推上去的时刻」—— 一个开开
    /// 关关的用户每次启动都会立刻推一份,把 keep 份历史窗口刷光。这正是
    /// **T11 那条陷阱**:计时要从「事情真的成了」起算,不是从调用点起算。
    fn drive_cloud_backup(&mut self, now_ms: u64) {
        if self.cloud_in_flight {
            return;
        }
        if now_ms.saturating_sub(self.cloud_last_check_ms) < CLOUD_POLL_MS {
            return;
        }
        self.cloud_last_check_ms = now_ms;
        let Some(dir) = crate::shell::store::config_dir() else {
            return;
        };
        let cfg = mullion_store::cloud::load(&dir);
        // 开关那一道也在 `should_upload` 里,但这里先挡一下:关着的时候
        // 连指纹都不必算(读四个文件),而绝大多数用户是关着的。
        if !cfg.enabled {
            return;
        }
        // 算不出指纹 = `secrets.enc` 在但读不出来。**这一轮跳过**,不能
        // 按「secrets 为空」那一版指纹继续 —— 那会推一份密文段是空的备份
        // 上去,还把游标推进了(见 `cloudsync::read_secrets`)。只记日志
        // 不弹状态栏:多半是杀软/IO 的瞬时问题,下一轮就好了。
        let fp = match crate::cloudsync::fingerprint_now(&dir) {
            Ok(fp) => fp,
            Err(e) => {
                log::warn!("云端备份:算不出内容指纹,这一轮跳过:{e}");
                return;
            }
        };
        let since = crate::cloudsync::minutes_since_last_ok(
            &cfg.last_ok_at,
            time::OffsetDateTime::now_utc(),
        );
        if !mullion_store::cloud::should_upload(&cfg, &fp, since) {
            return;
        }
        self.spawn_cloud_backup(false);
    }
```

轮询间隔常量，放在 `app.rs` 的常量区：

```rust
/// F273:定时备份**多久看一眼盘**。与用户配的 `interval_min` 是两回事 ——
/// 那个归 `cloud::should_upload` 管,这个只为不每帧去读那四个文件算指纹。
///
/// 取一分钟:算一次指纹是读几十 KB + 一次 sha256(微秒级),一分钟一次在
/// profile 行里看不见;而它决定的是「到点之后最晚多久会真的推出去」,
/// 再长就会让用户点完设置等半天看不到动静。
const CLOUD_POLL_MS: u64 = 60_000;
```

两个时间戳辅助（`app.rs` 里**没有**现成的，要新写；`localtime.rs` 里也没有
`utc_compact` / `utc_rfc3339` 这种东西，别去找）。放在 `app.rs` 的自由函数区：

```rust
/// F273:`YYYY-MM-DD'T'HH:MM:SS'Z'`。写进 `cloud.toml` 的 `last_ok_at`,
/// 也进包头 —— 与 `app.rs` 里另外两处 `now_utc().format(&Rfc3339)` 同形。
fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// F273:`YYYYMMDD'T'HHMMSS'Z'`。**SigV4 的 `x-amz-date` 与对象键里那一段
/// 共用这一个** —— 两者同源,省得出现「键上写着 10 点、签名说 11 点」。
fn now_compact() -> String {
    let t = time::OffsetDateTime::now_utc();
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}
```

调用点在 **`fn pump_io(&mut self)`** 里，加在
`self.drive_automation();` / `self.drive_attach_checks();` /
`self.drive_project_visits();` 那一串**末尾**（已核实：`app.rs:8596`~`8598`）：

```rust
        self.drive_cloud_backup(now);
```

**参数是 `now`，不是 `now_ms`** —— 那个局部量在 `pump_io` 开头就有
（`app.rs:8581` 的 `let now = self.now_ms();`），直接用，别再取一次时间。

**必须放在 `pump_io` 里，不是别处。** 它的文档写着「每帧调」，而那三个同族
驱动都住在这儿；守护 `the_cloud_backup_is_driven_every_frame` 的锚点就钉在
这个函数体上。挪到任何一个偶尔才走的分支里，定时备份就变成碰运气才跑一次，
而扫全篇式的判据看不出这个差别。

菜单动作处理里接上手动入口 —— **必须 `take`，不能只读**：

```rust
                // F273:菜单里点的「立刻备份到云」。`take` 而不是只读:
                // 只读的话这个 bool 永远是 `true`,在途标记一还回来下一帧
                // 就再起一次,变成无限重传。同 `pack_pick_request` 的写法。
                if std::mem::take(&mut self.ui.cloud_backup_request) {
                    self.spawn_cloud_backup(true);
                }
```

- [ ] **Step 7: 实现 `CloudBackupDone` 的处理**

```rust
            UserEvent::CloudBackupDone(outcome) => {
                // 在途标记**只在这一处归还**,且这个分支里没有任何提前 return
                // —— 漏一条出口的后果是之后永远不再备份,且零报错(T13 同族)。
                self.cloud_in_flight = false;
                // **`App` 没有 `config_dir` 字段**,走自由函数(见本任务末尾
                // 「已核实的字段来源」)。
                let dir = crate::shell::store::config_dir();
                match outcome {
                    crate::cloudsync::UploadOutcome::Ok { fingerprint, seq, at } => {
                        if let Some(d) = dir {
                            let mut cfg = mullion_store::cloud::load(&d);
                            crate::cloudsync::record_success(&mut cfg, &fingerprint, seq, &at);
                            if let Err(e) = mullion_store::cloud::save(&d, &cfg) {
                                log::warn!("云端备份游标写回失败:{e}");
                            }
                        }
                        // 结论写进 `self.ui`,不是 `App` —— 状态栏那一格
                        // (`ui/mod.rs:961`)读的就是这里,Task 13 已经接死。
                        self.ui.cloud_status = Some(crate::ui::chrome::CloudCell {
                            text: format!("云 已备份 #{seq}"),
                            severity: crate::tunnels::Severity::Calm,
                        });
                    }
                    crate::cloudsync::UploadOutcome::Unchanged => {
                        // 没变不是失败,不动状态栏那一格。
                    }
                    crate::cloudsync::UploadOutcome::Failed(msg) => {
                        log::warn!("云端备份失败:{msg}");
                        self.ui.cloud_status = Some(crate::ui::chrome::CloudCell {
                            text: "云 备份失败".into(),
                            severity: crate::tunnels::Severity::Danger,
                        });
                        self.ui.set_error(format!("云端备份失败:{msg}"));
                    }
                }
            }
```

- [ ] **Step 8a: 把草稿的起点换成 `from_settings_and_cloud`**

**这一步漏了的话，Task 12 的整个云端分节永远显示空白**，而且编译、测试、clippy
全都干净——本项目登记过同一形状：「量具存在≠接在那条路上」。

`crates/mullion-app/src/app.rs:3285`（`fn sync_settings_dialog`）是 `SettingsDraft`
**唯一的生产构造点**，今天写的是：

```rust
                self.ui.settings_draft = Some(crate::ui::settings::SettingsDraft::from_settings(
                    &self.settings,
                ));
```

换成：

```rust
                // F271:云配置在**另一个文件**里(`cloud.toml` 不进迁移包),
                // 所以草稿要从两个来源起手。读不到配置目录时退回 `Default`——
                // 那种情况下云端分节全空,而「确定」那一步同样拿不到目录、
                // 不会写出任何东西,两端一致。
                let cloud = crate::shell::store::config_dir()
                    .map(|d| mullion_store::cloud::load(&d))
                    .unwrap_or_default();
                self.ui.settings_draft =
                    Some(crate::ui::settings::SettingsDraft::from_settings_and_cloud(
                        &self.settings,
                        &cloud,
                    ));
```

配套守护（加进 Step 1 那一批测试里；**判据不能是「`from_settings_and_cloud` 存在」**，
那是恒绿的——它必须扎在「弹窗真的显示了盘上那份配置」上）：

```rust
    /// F271:设置弹窗里的云端分节必须显示**盘上那份** `cloud.toml`。
    ///
    /// 这条守的是接线,不是构造器本身。`from_settings_and_cloud` 写得再对,
    /// 只要 `sync_settings_dialog` 还在调 `from_settings`,用户看到的就是一张
    /// 空表 —— 而且编译、测试、clippy 全干净,只有人眼能发现。
    ///
    /// 自证会变红:把 `sync_settings_dialog` 里那两行改回
    /// `SettingsDraft::from_settings(&self.settings)`。
    #[test]
    fn the_settings_dialog_starts_from_the_cloud_config_on_disk() {
        let production = prod_src();
        let body = body_of(&production, "fn sync_settings_dialog(");
        let body = strip_comments(&body);
        assert!(
            body.contains("from_settings_and_cloud"),
            "设置弹窗的草稿没接上云配置 —— 云端分节会永远显示空白:{body}"
        );
        // 这一条**故意扫全篇生产代码,不只是这个函数体**:
        // `from_settings` 起的草稿云端字段全是默认值,而「确定」会把草稿写回
        // `cloud.toml` —— 任何一个新冒出来的调用点都意味着 endpoint / bucket /
        // AK 会被一次「打开设置再点确定」悄悄清空(「整份覆盖」缺陷族,
        // 本项目已踩过五处)。只守着这一个函数体的话,新加的调用点照样溜过去。
        //
        // 注意 `from_settings_and_cloud(` **不含**子串 `from_settings(`
        // (中间隔着 `_and_cloud`),所以这条不会误伤上面那句。
        assert_eq!(
            strip_comments(&production)
                .matches("SettingsDraft::from_settings(")
                .count(),
            0,
            "生产代码里还有人在调只读 settings 的那个构造器 —— \
             用它起的草稿云端字段全是默认值,一次「确定」就会把 AK/endpoint 清空"
        );
    }
```

（`prod_src` / `body_of` / `strip_comments` 是 `app.rs` 测试模块里已有的源码切片
辅助，见 `app.rs:23903`(`body_of`)/`23923`(`strip_comments`)/`23932`(`prod_src`)。**必须过 `strip_comments`** —— 不然上面那段新写的
注释里就字面带着 `from_settings_and_cloud`，断言当场恒真。）

- [ ] **Step 8b: 接「确定」时把云草稿落盘**

**已核实的现场**（`app.rs:3372` 的 `O::Commit` 分支）：那里**没有 `draft` 这个
绑定**，只有 `self.take_settings_draft();` + 一段 `graft_changed` 三方合并 +
`self.ui.settings_open = false;`。`take_settings_draft` 走的是 `as_ref()`，
**不会清掉** `self.ui.settings_draft`，所以草稿在这一步仍然在。

在 `O::Commit` 分支里、`self.ui.settings_open = false;` **之前**加一行：

```rust
                self.save_cloud_draft();
```

然后新写这个方法（放在 `apply_settings_action` 附近）：

```rust
    /// F271:把设置弹窗里的云端分节写进 `cloud.toml`。
    ///
    /// **单独一个方法,不是揉进 `O::Commit`**:这段要同时碰
    /// `self.ui.settings_draft`(**可变** —— SK 存完必须清掉)、`self.store`
    /// (借 vault)和 `self.ui.set_error`(又一次 `&mut self.ui`)。揉在一起
    /// 会撞借用检查。先把要用的几项**克隆成局部量**,借用就都断干净了。
    ///
    /// 云配置落的是**另一个文件** —— `cloud.toml` 刻意不在
    /// `portable::TOP_LEVEL_FILES` 里(设计 D11),所以它不跟着 `settings.toml`
    /// 那条三方合并的路走。
    fn save_cloud_draft(&mut self) {
        let Some(d) = self.ui.settings_draft.as_ref() else {
            return;
        };
        let enabled = d.cloud_enabled;
        let endpoint = d.cloud_endpoint.clone();
        let region = d.cloud_region.clone();
        let bucket = d.cloud_bucket.clone();
        let prefix = d.cloud_prefix.clone();
        let path_style = d.cloud_path_style;
        let keep = d.cloud_keep;
        let interval_min = d.cloud_interval_min;
        let socks5 = d.cloud_socks5.clone();
        let access_key_id = d.cloud_access_key_id.clone();
        let secret_new = d.cloud_secret_new.clone();

        let Some(dir) = crate::shell::store::config_dir() else {
            return;
        };
        // 读-改-写:盘上那份打底,只盖弹窗管的那几项。游标
        // (`last_seq` / `last_fingerprint` / `last_ok_at`)与已封好的
        // `secret_sealed` 必须原样留着 —— 整份覆盖是 F247/F248 的缺陷族。
        let mut cfg = mullion_store::cloud::load(&dir);
        cfg.enabled = enabled;
        cfg.endpoint = endpoint;
        cfg.region = region;
        cfg.bucket = bucket;
        cfg.prefix = prefix;
        cfg.path_style = path_style;
        cfg.keep = keep;
        cfg.interval_min = interval_min;
        cfg.socks5 = socks5;
        cfg.access_key_id = access_key_id;

        // SK 空 = 不改 —— 每次打开设置都要重打一遍 30 位密钥,
        // 等于在逼用户把它记在别处。
        let mut sk_err = None;
        if !secret_new.is_empty() {
            match self.store.as_ref() {
                Some(s) => {
                    if let Err(e) =
                        mullion_store::cloud::set_secret_key(&mut cfg, s.vault(), &secret_new)
                    {
                        sk_err = Some(format!("保存 Access Key Secret 失败:{e}"));
                    }
                }
                None => sk_err = Some("会话库还没打开,Access Key Secret 没能保存".into()),
            }
            // **不管成没成都清掉**:失败时留着的话,下一次点确定会拿同一个
            // 明文再试一遍,而用户以为自己早就改过了;而且那串明文会一直
            // 躺在草稿里等着被截图。
            if let Some(d) = self.ui.settings_draft.as_mut() {
                d.cloud_secret_new.clear();
            }
        }

        let save_err = mullion_store::cloud::save(&dir, &cfg)
            .err()
            .map(|e| format!("保存云端备份配置失败:{e}"));
        // `set_error` 只留最后一条 —— 两条分别发的话第一条会被静默吃掉。
        let msgs: Vec<String> = [sk_err, save_err].into_iter().flatten().collect();
        if !msgs.is_empty() {
            self.ui.set_error(msgs.join(" / "));
        }
    }
```

配套守护（加进 Step 1 那一批）：

```rust
    /// F271:云配置写回必须是**读-改-写**,不是整份覆盖。
    ///
    /// 整份覆盖的症状:用户打开设置点一下确定,`last_seq` / `last_fingerprint`
    /// 连同已封好的 `secret_sealed` 一起被草稿里的空值抹掉 —— 下一轮备份
    /// 从序号 1 重来、SK 没了要重填,而这一切零报错(F247/F248 缺陷族)。
    ///
    /// 自证会变红:把 `let mut cfg = mullion_store::cloud::load(&dir);`
    /// 改成 `let mut cfg = mullion_store::CloudConfig::default();`。
    #[test]
    fn the_cloud_draft_is_written_back_over_the_config_on_disk() {
        let body = strip_comments(body_of(prod_src(), "fn save_cloud_draft("));
        assert!(
            body.contains("mullion_store::cloud::load(&dir)"),
            "云配置写回没有先读盘 —— 游标与已封好的 SK 会被一次「确定」抹掉"
        );
        assert!(
            !body.contains("CloudConfig::default()"),
            "写回的底是 default() —— 那就是整份覆盖:{body}"
        );
    }

    /// F271:SK 输入框**不管存成没存成都要清掉**。
    ///
    /// 留着的症状有两个,都静默:下次点确定会拿同一串明文再试一遍
    /// (用户以为早就改过了),以及那串明文一直躺在草稿里等着被截图 ——
    /// 本项目的排查流程里「发个截图」是常规动作。
    ///
    /// 判据有两半:**恰好一处** `clear()`,且它挂在 `match` **外面**的那个
    /// `if let` 上 —— 塞进 `Some`/`None` 某条分支里的话另一条路上就不清了。
    ///
    /// 第二半钉的是**缩进**(12 格,即直接在 `if !secret_new.is_empty()` 里),
    /// 不是先后顺序。「排在错误分支之后」是句废话:把 `clear()` 搬进错误分支,
    /// 它照样排在后面 —— 那条断言杀不掉自己要挡的变异。`strip_comments` 只删
    /// 整行注释、不动代码行的缩进,所以这个判据是稳的。
    ///
    /// 自证会变红:删掉那句 `clear()`;或者把那个 `if let` 整块搬进
    /// `match` 的 `Some(s) =>` 分支里(缩进变 16 格)。
    #[test]
    fn the_secret_key_box_is_cleared_whether_or_not_it_saved() {
        let body = strip_comments(body_of(prod_src(), "fn save_cloud_draft("));
        assert_eq!(
            body.matches(".cloud_secret_new.clear()").count(),
            1,
            "SK 框的清空不是恰好一处 —— 漏了会让明文留在草稿里,多了说明有分支在兜底"
        );
        assert!(
            body.contains("\n            if let Some(d) = self.ui.settings_draft.as_mut() {"),
            "清空没挂在 match 外层那个 if let 上(缩进对不上)—— 它得在存成/存砸\
             两条路之后都执行:{body}"
        );
    }
```

**`cloud.toml` 读坏时这条路是个死胡同,片一有意不修 —— 但要知道它长什么样。**
`load` 读不懂时返回的那份 `CloudConfig` 带着私有的 `corrupt` 标记,设置弹窗于是
显示一张**空表单**(用户会以为自己从没配过),填完点确定 → `save` 拒绝 → 每次
都报同一条错。用户唯一的自救办法是去删那个文件。

片一给到的程度:`save` 的错误文案里**带完整路径**(Task 8 已实现并有守护测试
钉着),所以 `set_error` 出来的那句话里能看见该删哪个文件。**「重置云配置」
按钮留给片二** —— 触发条件是手改 `%APPDATA%` 底下的 TOML 并改坏,能干这事的
人也能把文件删掉,为它现在加一条 UI 路径不划算。**别在本任务里顺手加。**

**已核实的字段来源**（`app.rs` 当前形态，别再去猜）：
- 配置目录：**`App` 没有 `config_dir` 字段**，走自由函数
  `crate::shell::store::config_dir() -> Option<PathBuf>`。
- vault：**`App` 没有 `vault` 字段**。它在 `self.store: Option<SessionStore>`
  里，且 `SessionStore.vault` 是私有的 —— Step 5 新开的 `vault()` 访问器就是
  为这两个调用点开的。
- `self.proxy: EventLoopProxy<UserEvent>`（`app.rs:2213`）确实存在，`Clone` 可用。

- [ ] **Step 9: 跑全量**

Run: `cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log`
Expected: 全绿。

Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20`
Expected: 无输出。

Run: `cargo fmt --check`
Expected: 无输出。

- [ ] **Step 10: 提交并变异验证**

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
| `self._runtime.spawn_blocking(` 改成 `tokio::task::spawn_blocking(` | 同上（这条变异**编译得过**，正是它要挡的那种：真机首次备份 panic） |
| 去掉 `if self.cloud_in_flight { return; }` 那句 | `only_one_cloud_upload_is_in_flight_at_a_time` |
| `CloudBackupDone` 分支里删掉 `self.cloud_in_flight = false;` | `every_path_that_ends_a_cloud_upload_hands_the_in_flight_flag_back` |
| `pump_io` 里注释掉 `self.drive_cloud_backup(now);` | `the_cloud_backup_is_driven_every_frame` |
| 把那句从 `pump_io` 挪进一个偶尔才走的分支（例如 `if flushed { .. }` 里） | 同上。**这条是那条守护真正要挡的东西** —— 判据若写成「扫全篇找 `self.drive_cloud_backup(`」，这条逃得掉，而定时备份会变成碰运气才跑一次 |
| `drive_cloud_backup` 里把 `should_upload(..)` 换成手写的 `if since < u64::from(cfg.interval_min) { return; }` | `whether_to_back_up_is_decided_by_should_upload_not_by_a_hand_rolled_gate`（第一条断言） |
| `minutes_since_last_ok` 的实参从 `&cfg.last_ok_at` 改成从 `cloud_last_check_ms` 折算 | 同上（第二条断言）——T11：起算点从「真的推成了」退回「本进程上次看盘」 |
| `CLOUD_POLL_MS` 改成 `u64::from(cfg.interval_min) * 60_000` | **杀不掉**（两层节流合并之后行为差异只在"开开关关"的真机场景里才看得见）。**这条如实记下来，别硬编一条守护** —— 上面那两条断言钉的是「判据走 `should_upload`、起算点用 `last_ok_at`」，合并后两者仍然成立，只是轮询变懒。后果有限（最晚推迟一个 interval），不值得为它把常量结构复杂化 |
| 菜单分支里 `std::mem::take(&mut self.ui.cloud_backup_request)` 改成只读 `self.ui.cloud_backup_request` | 见 Task 13 的表（无限重传） |

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
        // `MULLION_CLOUD_SOCKS5` 是 `host:port`,**不带 `socks5://`**。
        env("MULLION_CLOUD_SOCKS5").as_deref(),
    )
    // `new` 返回 `Result`(代理地址解析不了时报 `Config`)—— 这里不能直接
    // 当成 `S3Client` 用,那是编译不过的。
    .expect("建不起客户端 —— 多半是 MULLION_CLOUD_SOCKS5 填错了");
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

**要改三处，不是两处。** 已核实过的行号（写的时候再 grep 确认一遍，别照抄行号）：

**(a) 加六行。** 在 `spec.md:351`（F269 那行）之后追加 F270~F275，内容照设计文档 §4
的表格，每行补上「验收标准」列（指向本计划里的守护测试名）。表是 4 列
`| ID | 需求 | 优先级 | 验收标准 |`。

注意落点是 **§4.7「会话组织与集合操作」的表**（`spec.md:296`~`354`）。这个小节
早就成了新条目的兜底桶（F264~F269 都在里面），而且 **F48「配置跨机可用」也在
这张表里**（`spec.md:310`）—— 云备份正是它的延续，放这儿是对的。

**(b) 改写非目标 N-G4**（`spec.md:48`）。现在是：

```
- N-G4 云同步、账号体系。配置就是本地文件。
```

改成设计文档 §1 给的那段（收窄成「账号体系」，说清我们不运营服务端、
不持有用户数据、本地文件仍是唯一真值源）。

**(c) 改 F48 的正文**（`spec.md:310`）。**这一处计划原先漏了，不改就自相矛盾。**
F48 的「需求」列里现在写着：

> **Mullion 自己不做同步传输**——文件搬运交给用户或外部工具（Syncthing / Git / 网盘），
> 与 N-G4「不做云同步、账号体系」一致

这两句话在 F270 落地之后**都不再成立**：我们做了传输，而 N-G4 也已经不再说
「不做云同步」了。改成「搬运可以交给外部工具，也可以用 F270 的云端备份；
两条路推的都是同一份可被外部接管的目录」，并把那句对 N-G4 的引用同步成新措辞。

留着不改的后果不是排版问题：F48 那一列是**将来有人提需求时的拒绝理由**
（F97 的备注里写明了这种用法）。一条已经被推翻的拒绝理由留在那儿，下一次
就会被当成有效依据引用。

改完自查：`grep -n "N-G4\|不做云同步\|不做同步传输" spec.md`，
剩下的每一处都要与新措辞一致。

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
git commit -m "docs(spec): 登记 F270~F275,改写 N-G4 与 F48 (F270)

N-G4 从「云同步、账号体系」收窄成「账号体系」——云备份用的是用户自己的
对象存储与 AK/SK,我们不运营服务端、不持有用户数据,本地文件仍是唯一真值源。

F48 的正文一起改:它原先写着「Mullion 自己不做同步传输」并直接引用了
N-G4 的旧措辞,两句在 F270 落地后都不成立。那一列是将来拒绝需求时要引用的
理由,留一条已被推翻的在那儿,下次就会被当成有效依据。"
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
否则就是一个假的开关。**已补进 Task 12 Step 4 的分节代码**（「保留份数」那一行
后面紧跟一条 `theme::hint_text` 说明），不是待办。
