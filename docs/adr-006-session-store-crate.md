# ADR-006: 新增 mullion-store crate(会话/凭据持久化)

- 状态: 已接受
- 日期: 2026-07-25
- 关联: spec.md F70/F71、ADR-002(TOML)、切片 A spec

## 背景

切片 A 要「无参启动 + 会话增删改查 + 一次配置认证一直使用」,需要一个持久化层:
非敏感字段可 diff 的 TOML(承 ADR-002),密码/私钥口令加密。这段逻辑要能无头单测(F70)。

## 决策

新增第 5 个 crate `mullion-store`,承载 `SessionRecord` + TOML 读写 + 敏感字段加密。
依赖方向 `app → {core, term, ssh, store}`,store 不依赖其余任何 crate。app 做整合者
(SessionRecord → SshConfig)。

## 备选与否决理由

- **塞进 mullion-app**:app 是最难测的 crate(winit/wgpu),把可测的持久化/加密逻辑埋进去,
  F70「磁盘搜不到明文」这类验收就只能带 GUI 测。否掉。
- **塞进 mullion-ssh**:违反「ssh 只认字节流,不认会话/窗口」的架构不变量。否掉。

## 关键实现取舍

- 存储沿用 ADR-002:`sessions.toml` 明文非敏感 + `secrets.enc` 加密 blob(XChaCha20-Poly1305)。
- 主密钥走 OS keyring(满足「一次配置一直使用」不再追问),来源抽成 `MasterKeySource` trait,
  测试用内存实现 → 加密逻辑无头可测。
- **F70 的 Argon2id 推迟到 F71**:Argon2id 是「从口令派生密钥」,无主密码时无输入;切片 A 用
  keyring 高熵随机主密钥即满足 F70 的 P0。Argon2id 待 F71 主密码层引入。
- 两文件各自 tmp+rename 原子写;删除会话连带清密文,open() 裁剪孤儿密文(id 完整性,§3.1/§3.2)。
- 错误手写(匹配项目既有 `ConnectError` 风格),不引 thiserror。
- keyring 后端用纯 Rust 的 async-secret-service(zbus),避开 C 版 libdbus,交叉编译/无系统库依赖更干净。

## 后果

- crate 数 4 → 5;CLAUDE.md 架构表同步更新。
- egui/状态机/会话 UI 属切片 A 的 Plan A2,其架构决策(egui 做外壳)另记 ADR 或在 A2 落地时补。
