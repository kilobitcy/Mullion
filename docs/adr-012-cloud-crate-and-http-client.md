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
- **TLS provider 必须在 `Cargo.toml` 里钉死**:ureq 官方 README 原话是
  "does not guarantee defaulting to it indefinitely"。默认哪天切到 aws-lc-rs,
  `x86_64-pc-windows-gnu` 交叉编译会在 aws-lc-sys 的 C/NASM 构建上炸——正是
  ADR-005 把 russh 切到 ring 要躲开的那件事,而且**只有交叉编译时才暴露**,
  本机 `cargo test` 全绿。
