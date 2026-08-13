# 切片 H:凭据实体(F74)实现计划

设计:`docs/superpowers/specs/2026-08-13-credentials-design.md`(决策 D1~D11 + 8 条失效模式)。

分两段:**H-a 数据层**(store + 连接链路接线,不发版)、**H-b UI**(管理档 + 编辑器选择器,发版)。
每个任务 TDD:先写测试看它变红,再实现,再 `cargo test -p <crate>`,最后单独提交。
每个守护测试都要做一次**变异验收**(改坏生产代码 → 该测试变红 → `cp` 还原)。

---

## H-a 数据层

### Task 1 — `Auth` 枚举 + TOML 编码(D1/D2)
- 新建 `crates/mullion-store/src/credential.rs`:`CredentialId`、`CredentialRecord`、
  `InlineAuth`、`Auth` 枚举、私有 `AuthRepr` + 双向 `TryFrom`。
- `model.rs` 删掉旧 `Auth` 结构体,改 `pub use crate::credential::{Auth, InlineAuth}`。
- 测试:①inline round-trip 字节与 v8 一致(`[session.auth]` 里没有 `source`);
  ②ref round-trip;③两个真值并存 → 解析失败(失效模式 5);
  ④`source = "ref"` 但缺 `credential_id` → 解析失败。
- 提交 `feat(store): Auth 改为「本会话独有 / 引用共享凭据」二选一 (F74)`。

### Task 2 — `[[credential]]` 表 + schema v9(D11)
- `SessionsFile` 加 `#[serde(default)] pub credential: Vec<CredentialRecord>`;
  `CURRENT_SCHEMA = 9` + 文档注释说明「零迁移代码但仍升号」的理由。
- 测试:v8 文件(手写常量,带 password 与 public_key 两条会话)经 `Vault::open`
  逐字段等价映射成 `Inline`、`credentials()` 为空、`.bak` 存在(失效模式 7)。
- 提交 `feat(store): sessions.toml 新增 [[credential]] 表,schema v8→v9 (F74)`。

### Task 3 — Vault 凭据 CRUD + 密文键空间 + 引用完整性(D3/D7)
- `credentials()` / `credential(id)` / `credential_secret(id)` /
  `add_credential(draft)` / `update_credential(id, draft)` / `delete_credential(id)`。
- 密文键 `cred:<id>`,`Vault::open` 的 `secrets.retain` 集合扩成
  「会话 id ∪ `cred:<凭据 id>`」。
- `StoreError::CredentialInUse(Vec<SessionId>)` / `CredentialNotFound(CredentialId)`。
- 测试:①`credential_secrets_survive_reopen`(失效模式 1);②删被引用的凭据被拒
  且错误里带引用者 id(失效模式 3);③`cred:` 前缀与 `SessionId` 十进制不可能撞车
  (失效模式 8);④删凭据成功时连带清掉它的密文。
- 提交 `feat(store): 凭据 CRUD + cred: 密文键空间 + 被引用不可删 (F74)`。

### Task 4 — `resolve_auth` / `resolve_auth_of`(D5/D6/D4)
- `ResolvedAuth { user, kind, secret }`;`StoreError::DanglingCredential(CredentialId)`。
- 测试:①inline 解析出会话自己的 user/kind/secret;②ref 解析出凭据的;
  ③悬空引用 → `DanglingCredential`,**不降级**(失效模式 2);
  ④`the_proxy_password_always_comes_from_the_session_not_the_credential`(失效模式 4)。
- 提交 `feat(store): resolve_auth —— 引用解析与悬空硬失败 (F74)`。

### Task 5 — app 连接链路接线
- `session_map::to_dial_plan(rec, &ResolvedAuth)`;`dial_plan::build_hops_*` 的
  `secret_of` 换成 `auth_of: &dyn Fn(SessionId) -> ResolvedAuth`(非 Option)。
- `shell/store.rs`:`ssh_config_for` / `ssh_config_for_draft` 先解析目标与每一跳的
  auth(`?` 硬失败),再物化。
- 测试:`a_draft_referencing_a_credential_dials_with_it`(失效模式 6)、
  引用凭据的跳板拿到凭据的私钥正文、悬空引用让整条 `ssh_config_for` 报错。
- 提交 `feat(app): 拨号链路走凭据解析,悬空引用硬失败 (F74)`。

---

## H-b UI

### Task 6 — 编辑器缓冲 + 必填校验(D9/D10)
- `EditorBuffer` 加 `cred_source: CredSourceUi`(`Own` / `Shared`)与
  `credential_id: Option<CredentialId>`;`buffer.rs` 的 `from_record` / `build_draft`
  两头都要认。
- `validate.rs`:共享模式下「用户名」不参与校验,改判「必须选中凭据」,
  缺项仍映射到「认证」Tab。
- 测试:①共享模式下 draft 产出 `Auth::Ref`;②切回独有模式产出 `Auth::Inline`
  且带回原来的 user;③共享模式没选凭据 → 保存禁用且缺项指向「认证」Tab。
- 提交 `feat(app): 会话编辑器缓冲支持引用共享凭据 (F74/F91)`。

### Task 7 — 「认证」页来源选择器(D9)
- `fields::auth` 顶部加「凭据来源」两档;共享模式下画下拉 + 只读摘要 +
  「在「凭据」页修改」,身份/凭据两分节整体不画。
- 凭据库为空 → 下拉禁用 + `on_disabled_hover_text`。
- 测试:①共享模式下画面里没有「密码」输入行(严格二选一在 UI 上的样子);
  ②空库下拉禁用且给出去处;③点下拉里的某个凭据 → `buf.credential_id` 变。
- 提交 `feat(app): 认证页凭据来源二选一 (F74)`。

### Task 8 — 凭据管理档(D8)
- `ManagerMode::Credentials` 插在 SFTP 与隧道之间;`credential_list.rs`(左栏,
  行显示 名称 / user@kind / 「N 个会话在用」)+ `credential_editor.rs`(右栏表单:
  名称、用户名、认证方式、密码或私钥+口令、删除)。
- 删除被引用 → 内联红字列出引用者会话名,要求先解绑。
- 测试:①模式条有四档且顺序固定;②被引用的凭据点删除 → 报错文案里有引用者名字;
  ③凭据档下 ↑↓ / Ctrl+数字不去动会话编辑器(比照隧道档既有的门控)。
- 提交 `feat(app): 会话管理器新增「凭据」档 (F74)`。

### Task 9 — 接线落库
- `mod.rs` 分发新档;`app.rs` 把凭据的增删改落到 `SessionStore`;
  `has_real_action` 加新字段(否则 egui 丢弃帧里被静默吃掉)。
- 测试:凭据保存后 `store.credentials()` 里有它;删除被拒时状态栏报出引用者。
- 提交 `feat(app): 凭据档接线落库 (F74)`。

### Task 10 — 收口
- `spec.md`:F74 验收列改「已实现(v0.1.41 …)」,F75 标注仍未做;
  §6 里程碑加一行。
- 版本 0.1.41(单独 `chore:` 提交)→ 绿门 → 交叉编译 + objdump → Release → 报告。
