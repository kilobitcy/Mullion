# ADR-012:云端备份单开一个 crate,HTTP 用阻塞式 ureq

## 状态
已采纳(2026-09-15)

## 背景
F270 要把配置推到用户自己的 S3 兼容对象存储。工作区原本没有任何 HTTP/TLS 客户端
(`Cargo.lock` 里没有 reqwest/ureq/hyper/rustls,只有 russh 带进来的 `ring`)。

## 决策
1. 新建 `mullion-cloud` crate,架构不变量表从五条变六条。
2. HTTP 客户端用 `ureq 3`,`default-features = false`,显式挑 `rustls` + `socks-proxy`。

## 备选与为什么否掉

**塞进 `mullion-store`**:破坏「零 async、仅同步 IO、可纯单测」不变量里最值钱的
那半句——store 今天的每条测试都不需要起服务器,塞进去之后这条性质没了。

**塞进 `mullion-ssh`**:品类错误。那个 crate 的定义是「russh,只认字节流」。

**塞进 `mullion-app`**:依赖方向合法、不用改架构表,但正好落在本项目刚登记过的
「App 的方法测不了 → 交付判据整体恒绿」那一层(slice-f253-f256)。SigV4 签名算错了
靠什么发现,会变成一个开放问题。

**reqwest(async)**:能直接融进 app 现有的 tokio 运行时,但依赖树大一个量级
(hyper/h2/tower/http),当前 605 个 crate 会涨到 680+,且 `mullion-cloud` 就不再是
零 async 的纯 crate。对一个「每 30 分钟 PUT 几十 KB」的功能,这个价钱不值。

**自己手写 HTTP over rustls**:依赖最少,但要自己处理 chunked、重定向、连接复用、
超时、代理 CONNECT。在一个备份功能上自建 HTTP 栈,性价比极差。

## 代价
- exe 体积增加(N6 盯着 25MB 上限,片一发版时必须重新量)。
- **`base64` 两个版本并存**:`sha2`/`hmac` 核实过「工作区已有、不引入新版本」,
  但没对 `ureq` 拉进来的依赖做同样的核对——`ureq` 自己的 manifest 钉了
  `base64 = "0.23.1"`,而工作区（`mullion-store`/`mullion-app`)已经直接依赖
  `base64 0.22`。两者今天在 `Cargo.lock` 里并存（`grep -A1 'name = "base64"'
  Cargo.lock` 可见 0.22.1 与 0.23.1 两条)。这不是能修的:除非不用 `ureq`,
  否则拿不掉这第二个版本,只能接受它算进 exe 体积里。
- **TLS provider 必须在 `Cargo.toml` 里钉死**:ureq 官方 README 原话是
  "does not guarantee defaulting to it indefinitely"。默认哪天切到 aws-lc-rs,
  `x86_64-pc-windows-gnu` 交叉编译会在 aws-lc-sys 的 C/NASM 构建上炸——正是
  ADR-005 把 russh 切到 ring 要躲开的那件事,而且**只有交叉编译时才暴露**,
  本机 `cargo test` 全绿。

## F363 修订:SOCKS5 改为 mullion-cloud 自带连接器(2026-11)

决策第 2 条里的 `socks-proxy` 特性已去掉(原文保留不改)。

**起因**:ureq 3.4.2 的 `SocksConnector` 把握手丢进 `thread::scope`,用
`recv_timeout` 限时,但超时后 scope 退出仍要 join 那个卡在 `socks` crate
**无超时读**上的子线程。代理接受 TCP 却不应答时,整次请求无限挂起
(华为 P50 真机 13 分钟以上),`timeout_global` 形同虚设。桌面端云备份同样受影响。

**当时的备选**:
- **继续用 ureq `socks-proxy`、等上游修**:否。挂起会把 `spawn_blocking` 线程永久占住,
  而定时器还在往里塞新任务;上游无修复时间表,我们没有可依赖的节点。
- **在外层再包一层线程 + 超时**:否。放弃的线程仍卡在读上,泄漏线程与套接字,
  只是把挂起藏起来。
- **自带 SOCKS5 连接器(采纳)**:`mullion-cloud/src/socks.rs`,只做 RFC 1928
  无认证 + CONNECT,每步 IO 设读写超时、受同一截止时刻约束,不开线程;通过
  `Agent::with_parts` 接入,连接器链 SOCKS5 → HTTP CONNECT → TCP → rustls
  (HTTP CONNECT 留着,为保持环境变量代理与 NO_PROXY 的原行为)。

**行为差异**:目标域名改由代理解析(ATYP 0x03,`socks5h` 语义),不再本机先解析发 IP。
环境变量里的 SOCKS4 代理不再支持,会报错而不是静默直连。
