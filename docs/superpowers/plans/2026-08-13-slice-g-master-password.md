# 切片 G:主密码(F71)实现计划

> 设计定案:`docs/superpowers/specs/2026-08-13-master-password-design.md`
> 一次提交一件事;每个 Task 结束跑 `cargo test -p mullion-store`(前三个)或
> `cargo test --workspace`(后面的),并对**新加的每条守护测试**做变异验收
> (`cp <文件> /tmp/x.bak` 备份、改一处、跑测试确认变红、`cp` 还原 —— **不许用
> `git checkout`**,那会连带抹掉本 Task 其余未提交的改动)。

**目标:** 让 `secrets.enc` 可以由用户设定的主密码经 Argon2id 派生密钥,
盐随密文存放,从而使配置目录可以整体搬到另一台机器;不设主密码时行为不变。

**架构:** `mullion-store` 新增两个纯模块(`kdf` 派生、`secrets_file` 文件头),
`Vault` 增加 `open_with` / `probe_scheme` / 设改撤三个方法;`mullion-app` 增加
启动解锁弹窗与设置弹窗的「安全」分节。依赖方向不变。

**技术栈:** `argon2 0.5.3`(**已在 `Cargo.lock` 里**,keyring 的传递依赖 ——
加直接依赖不引入新版本,同 `base64`/`arboard` 的先例),
`default-features = false, features = ["alloc"]`。

---

### Task 1:Argon2id 派生纯函数(`kdf.rs`)

**Files:**
- 改:`Cargo.toml`(workspace deps 加 `argon2`)、`crates/mullion-store/Cargo.toml`
- 建:`crates/mullion-store/src/kdf.rs`
- 改:`crates/mullion-store/src/lib.rs`(`pub mod kdf;` + re-export)
- 改:`crates/mullion-store/src/error.rs`(新增 `Kdf(String)`)

- [ ] 写测试(全部落在 `kdf.rs` 的 `mod tests`):
  - `same_password_and_salt_derive_the_same_key`
  - `changing_the_salt_changes_the_key`(spec F71 点名)
  - `changing_the_password_changes_the_key`
  - `params_are_carried_not_hardcoded_so_old_files_still_open` —— 同口令同盐、
    只改 `t_cost`,派生结果必须不同(证明参数真的进了派生,而不是被忽略)
  - `an_empty_salt_is_rejected_not_silently_accepted`
- [ ] 实现:

```rust
pub const SALT_LEN: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfParams { pub m_cost: u32, pub t_cost: u32, pub p_cost: u32 }
impl Default for KdfParams { /* Params::DEFAULT 的三个值 */ }

pub fn derive_key(password: &str, salt: &[u8], p: KdfParams) -> Result<[u8; 32], StoreError>;
pub fn random_salt() -> [u8; SALT_LEN];   // 复用 XChaCha20Poly1305::generate_key 拿熵
```

- [ ] 变异验收:把 `derive_key` 里的 `salt` 换成固定常量 → 第 2 条必须红。
- [ ] 提交 `feat(store): Argon2id 派生纯函数,参数随调用方传入 (F71)`

---

### Task 2:`secrets.enc` 文件头(`secrets_file.rs`)

**Files:** 建 `crates/mullion-store/src/secrets_file.rs`;改 `lib.rs`

- [ ] 测试:
  - `a_legacy_blob_is_recognised_as_keyring`(随机 24 字节开头 → `Keyring`)
  - `header_round_trips_every_field`(m/t/p/salt 逐字段回来)
  - `a_truncated_header_is_an_error_not_a_panic`(0..35 每个长度都切一刀)
  - `an_unknown_kdf_byte_is_rejected_not_treated_as_keyring`
  - `an_unknown_header_version_is_rejected`
  - `the_payload_after_the_header_is_byte_identical_to_the_legacy_layout`
- [ ] 实现 `pub enum Scheme { Keyring, Argon2id { params: KdfParams, salt: [u8; SALT_LEN] } }`、
  `pub fn parse(blob: &[u8]) -> Result<(Scheme, &[u8]), StoreError>`、
  `pub fn encode(scheme: &Scheme, payload: &[u8]) -> Vec<u8>`。
- [ ] 变异验收:`parse` 里把 `salt_len` 读成常量 16(忽略头里的字节)→
  round-trip 测试仍绿,所以**额外**加一条用 `salt_len = 8` 的头去 parse 的断言。
- [ ] 提交 `feat(store): secrets.enc 文件头 —— 盐与 KDF 参数随密文走 (F71)`

---

### Task 3:`Vault` 接上两种方案

**Files:** 改 `crates/mullion-store/src/vault.rs`、`error.rs`、`lib.rs`

- [ ] 测试(`vault.rs` 的 `mod tests`):
  - `without_a_master_password_the_file_stays_in_the_legacy_format` ——
    存一条密码后 `save()`,读磁盘字节,断言 `crypto::decrypt(固定 key, bytes)` 直接能开
    (即「与今日逐字节等价」的可测形式)
  - `setting_a_master_password_rewrites_the_file_and_the_old_key_no_longer_opens_it`
  - `a_password_protected_vault_reopens_with_the_right_password`
  - `the_wrong_password_is_reported_as_wrong_password_not_corrupt`
  - `opening_a_password_protected_vault_the_old_way_asks_for_a_password`
    (`Vault::open` → `PasswordRequired`,不是 `Crypto`)
  - `clearing_the_master_password_returns_the_file_to_the_legacy_format`
  - `changing_the_master_password_invalidates_the_old_one`
  - `probe_scheme_on_a_missing_file_says_keyring`
  - `secrets_survive_setting_a_master_password`(设完密码重开,口令还在 —— GC 那条
    `retain` 与本片无关但必须不受影响)
- [ ] 实现 `pub enum Unlock<'a> { Keyring(&'a dyn MasterKeySource), Password(&'a str) }`、
  `Vault::open_with`、`Vault::probe_scheme(dir)`、`Vault::has_master_password()`、
  `set_master_password` / `clear_master_password`。
- [ ] `save()` 按 `self.scheme` 决定写不写头。
- [ ] 变异验收:`set_master_password` 里去掉 `self.save()` → 第 2 条必须红。
- [ ] 提交 `feat(store): 主密码 —— 设/改/撤三条路径与方案探测 (F71)`

---

### Task 4:启动解锁弹窗

**Files:** 建 `crates/mullion-app/src/ui/unlock.rs`;改 `ui/mod.rs`、`app.rs`、
`shell/store.rs`

- [ ] `unlock.rs` 的测试:
  - `merely_showing_the_dialog_changes_nothing`
  - `enter_in_the_password_box_submits`
  - `quit_reports_quit_so_the_caller_can_close_the_window`
  - `the_error_line_only_shows_after_a_failed_try`
- [ ] `app.rs` 的测试:
  - `an_unlock_dialog_counts_as_a_modal_so_the_terminal_gets_no_keys`(T8)
  - `a_wrong_password_keeps_the_dialog_open_and_does_not_disable_the_session_store`
- [ ] `has_real_action` 加 `a.unlock.is_some()`(D4b 的老坑:手工枚举)。
- [ ] 变异验收:`modal_open()` 去掉 unlock 那一项 → T8 那条必须红。
- [ ] 提交 `feat(app): 启动解锁弹窗 —— 主密码库先解锁再打开 (F71)`

---

### Task 5:设置弹窗「安全」分节

**Files:** 改 `crates/mullion-app/src/ui/settings.rs`、`ui/mod.rs`、`app.rs`

- [ ] 测试:
  - `the_security_section_says_whether_a_master_password_is_set`
  - `mismatched_confirmation_is_called_out_and_blocks_the_button`
  - `an_empty_password_cannot_be_set`
  - `the_irreversible_warning_is_always_visible_not_only_after_typing`
  - `clearing_is_only_offered_when_a_password_is_set`
- [ ] 变异验收:把「两次不一致」判据改成恒 `false` → 对应测试必须红。
- [ ] 提交 `feat(app): 设置弹窗「安全」分节 —— 设/改/撤主密码 (F71)`

---

### Task 6:文档与发布

- [ ] `spec.md` F71 行标注已实现;`CLAUDE.md` 若有 ADR 需要则不加(本片无架构级决策)
- [ ] `docs/ui-form-guidelines.md` 无需改(新分节按既有规范)
- [ ] `chore: 版本 0.1.40(主密码)`
- [ ] 绿门 → 交叉编译 → objdump → Release v0.1.40
