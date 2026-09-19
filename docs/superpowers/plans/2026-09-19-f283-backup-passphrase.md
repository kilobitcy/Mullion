# F283 备份口令与主密码解耦 —— 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 云端备份改用**独立备份口令**（Argon2id 派生，盐随密文走）加密，口令本身 `seal_local` 封进 `cloud.toml`；「必须先设主密码」与「清主密码自动关云备份」整套拆除。

**Architecture:** 载荷从「vault 主密钥整体加密 + 原样塞 `secrets.enc`」改成「口令封内层 `secrets.enc` + 口令封整包」——与 F46-a 的 `.mullionpack` **同形**，将来 F274 恢复可直接复用 `install_pack`。口令明文只在事件循环线程上从 `cloud.toml` 解出，两次 Argon2id 派生搬进阻塞半（避免卡帧）。

**Tech Stack:** `mullion-store`（`cloud.rs` / `vault.rs` / `portable.rs` / `kdf.rs`）、`mullion-app`（`cloudsync.rs` / `app.rs` / `ui/settings.rs`）。

---

## 设计约束（**每条都对应一种「静默坏掉」**，动手前读完）

**C1 —— 内层 `secrets.enc` 必须用口令重封，不能原样塞。**
`cloudsync.rs:178-194` 的注释自己写了这条预言：今天原样塞是成立的，因为设计 D6 保证
「有主密码 ⇒ `secrets.enc` 的头是 Argon2id、盐随文件走」。F283 拆掉主密码前提之后，
钥匙串方案的 `secrets.enc` **只有源机解得开**。原样上传的症状是：恢复那天会话全在、
**每条都要重新输密码**，且全链路零报错（F46-a 上已经踩过一次）。所以 prepare 必须
`vault.secrets_plaintext()` → `portable::seal_secrets(plain, passphrase)`，
和 `app.rs:11109-11121` 的导出路径逐字同形（包括「一条密码都没存过就不封空密文」）。

**C2 —— 口令是 `seal_local` 封的，改主密码时必须跟 SK 一起重封。**
`vault.rs:538-557` 的 `reseal_cloud_secret` 今天只认 `secret_sealed` 一个字段。
漏掉口令的症状：用户设/清一次主密码 → `cloud.toml` 里的口令密文用旧密钥封着、
解不开 → 云备份从此每轮失败。这是本项目登记过的**「列举式门控加档必然漏」**，
所以 Task 2 除了改逻辑，还要加一条**机械守护**：`CloudConfig` 里每多一个
`*_sealed` 字段，就必须在重封的两个函数体里各出现一次，否则测试红。

**C3 —— 两次 Argon2id 不许跑在事件循环线程上。**
`KdfParams::default()` 是 19 MiB / t=2，一次 30~60 ms，内外两层就是 60~120 ms。
`prepare()` 跑在事件循环线程（`cloudsync.rs:163-176` 的模块设计），在那儿派生
= 每次内容变更卡一帧 100 ms 左右。Task 4 把**打包与封装整体**搬进阻塞半：
主线程只做「要 vault 的事」（读明文、解 SK、解口令），搬进线程的仍然只有字节与字符串。
`upload_blocking` 的签名里**依然不许出现 `Vault`**（守护测试
`the_blocking_half_does_not_know_about_the_vault` 钉着，别动它）。

**C4 —— 指纹的算法不动。**
仍然是 `cloud::fingerprint(&files, &secrets_raw)`，其中 `secrets_raw` 是
**盘上 `secrets.enc` 的原始字节**（不是明文、不是重封后的字节）。
重封每次都换随机盐 + 随机 nonce，拿重封结果算指纹会让「内容没变」永远不成立 ——
后果是每个 interval 都上传一次，打的是用户的付费桶，还会把 `keep` 份历史刷光。
（`cloudsync.rs:208-213` 那段注释提过「将来可以改成对明文取指纹」，
**本切片不做**：那会改变 `last_fingerprint` 的语义，且引入「本地存一份凭据明文的哈希」
这个需要单独讨论的安全问题。）

**C5 —— 「没设口令」要说出来，不要静默停。**
不要把「有没有口令」加进 `cloud::should_upload` 的完整性闸（`cloud.rs:304-319`）——
那样定时器会安静地什么都不做，用户看到开关开着、却永远没有备份。
正确落点是 `prepare()` 返回 `Prepared::Failed("…")`，与今天 `NoMasterPassword` 那条
完全同形：走既有的 `report_cloud_failure` → 错误卡去重（`a_repeated_cloud_failure_does_not_keep_popping_the_same_card`）+ 退避，一次都不碰网络。

**C6 —— 不提供「用主密码当备份口令」的快捷项**（设计 D7）。加了等于把耦合请回来一半。

**C7 —— 新 UI 字符串必须在 GBK 内**（T9）。只用常见汉字与 ASCII，别用 `✓`／`→`／
`⚠` 之类符号；`tests/glyph_whitelist.rs` 会机械拦。

---

## 文件清单

| 文件 | 责任 |
|---|---|
| `crates/mullion-store/src/cloud.rs` | 加 `passphrase_sealed` 字段 + 三个存取函数（Task 1） |
| `crates/mullion-store/src/vault.rs` | 重封扩到口令（Task 2） |
| `crates/mullion-app/src/cloudsync.rs` | 改用口令封包 + 内层重封（Task 3）+ 封装搬进阻塞半（Task 4） |
| `crates/mullion-app/src/app.rs` | 拆 D12 接线与旧守护（Task 5） |
| `crates/mullion-app/src/ui/settings.rs` | 口令两行 + 门控改写 + 文案（Task 6） |
| `spec.md` | F283 登记为已实现、F272 的「硬前置 F71」订正（Task 7） |

---

### Task 1: `cloud.toml` 存一份备份口令

**Files:**
- Modify: `crates/mullion-store/src/cloud.rs`（结构体 `CloudConfig` 在 `cloud.rs:69-138`；
  `set_secret_key`/`secret_key` 在 `cloud.rs:198-218`）

- [ ] **Step 1: 先写失败测试**（追加进 `cloud.rs` 的 `mod tests`）

```rust
    /// F283:备份口令与 SK 一样,**明文一个字都不许落盘**。
    ///
    /// 判据不是「字段名对不对」而是「文件字节里搜不到口令本身」——
    /// 比照 `the_secret_key_never_hits_the_disk_in_the_clear` 的姿态。
    #[test]
    fn the_backup_passphrase_never_hits_the_disk_in_the_clear() {
        let dir = tempfile::tempdir().unwrap();
        let vault = test_vault(dir.path());
        let mut cfg = CloudConfig::default();
        set_passphrase(&mut cfg, &vault, "correct horse battery staple").unwrap();
        save(dir.path(), &cfg).unwrap();
        let raw = std::fs::read(dir.path().join(CLOUD_FILE)).unwrap();
        assert!(
            !String::from_utf8_lossy(&raw).contains("correct horse battery staple"),
            "口令明文进了 cloud.toml"
        );
        let back = load(dir.path());
        assert_eq!(passphrase(&back, &vault).unwrap(), "correct horse battery staple");
    }

    /// 老文件没有这个键 —— 按「没设过口令」读,**不判损坏**。
    /// `cloud.toml` 没有版本号,兼容全靠「缺键=默认值」这条契约
    /// (`a_file_written_by_an_older_version_still_loads_with_defaults` 同源)。
    #[test]
    fn an_old_cloud_toml_without_the_passphrase_reads_as_not_set() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(CLOUD_FILE),
            "enabled = true\nbucket = \"b\"\n",
        )
        .unwrap();
        let cfg = load(dir.path());
        assert!(!cfg.corrupt_for_test(), "缺键被误判成损坏");
        assert!(!has_passphrase(&cfg));
        assert!(cfg.enabled, "别的字段要照读");
    }
```

  `test_vault(..)` 与 `corrupt_for_test()`：本文件既有测试里已有等价手法
  （`a_corrupt_file_is_not_silently_replaced_by_defaults` 里怎么建 vault / 怎么看
  `corrupt` 就照抄，**不要新加 pub 方法**——`corrupt` 私有是 Task 8 的设计，
  `cloud.rs:1637` 有注释钉着）。

- [ ] **Step 2: 跑，确认红**

```bash
cargo test -p mullion-store cloud:: 2>&1 | tail -20
```
预期：`set_passphrase`/`passphrase`/`has_passphrase` 未定义，编译失败。

- [ ] **Step 3: 实现**

`CloudConfig` 加字段（放在 `secret_sealed` 之后，保持"同类相邻"）：

```rust
    /// F283:备份口令的密文(base64)。**与主密码无关**,也与 `secret_sealed`
    /// 不是一把钥匙的两用:前者封的是"解云端备份的那句口令",后者封的是
    /// "访问桶的那把 SK"。两者都走 `seal_local`(本机钥匙串/主密码当前那把),
    /// 于是**主密码一变,两者都要重封**——见 `Vault::reseal_cloud_secret`。
    ///
    /// 空串 = 没设过口令 = 云备份跑不起来(会在 `cloudsync::prepare` 那里
    /// 报出原因,不是静默不跑)。
    #[serde(default)]
    pub passphrase_sealed: String,
```

三个函数（紧跟在 `secret_key` 之后）：

```rust
/// F283:把备份口令用**本机当前密钥**封进配置(不落盘,调用方负责 `save`)。
///
/// 空口令不算口令:那会让"已设置"这个状态对应一段人人都能解开的密文。
pub fn set_passphrase(cfg: &mut CloudConfig, vault: &Vault, pass: &str) -> Result<(), StoreError> {
    if pass.is_empty() {
        return Err(StoreError::Kdf("备份口令不能为空".into()));
    }
    use base64::Engine as _;
    let sealed = vault.seal_local(pass.as_bytes())?;
    cfg.passphrase_sealed = base64::engine::general_purpose::STANDARD.encode(sealed);
    Ok(())
}

/// [`set_passphrase`] 的逆。没设过时返回空串(不是错误)——
/// "没设过"和"解不开"是两件事,调用方要分开报。
pub fn passphrase(cfg: &CloudConfig, vault: &Vault) -> Result<String, StoreError> {
    if cfg.passphrase_sealed.is_empty() {
        return Ok(String::new());
    }
    use base64::Engine as _;
    let blob = base64::engine::general_purpose::STANDARD
        .decode(&cfg.passphrase_sealed)
        .map_err(|e| StoreError::CorruptSecrets(e.to_string()))?;
    let plain = vault.open_local(&blob)?;
    String::from_utf8(plain).map_err(|e| StoreError::CorruptSecrets(e.to_string()))
}

/// 设置页要显示"已设置 / 未设置",而那句话**不该为了显示去解一次密文**
/// (解不开的时候显示"未设置"是错的,会引导用户去覆盖一份其实存在的口令)。
pub fn has_passphrase(cfg: &CloudConfig) -> bool {
    !cfg.passphrase_sealed.is_empty()
}
```

`StoreError` 的变体名以 `crates/mullion-store/src/error.rs` 实际定义为准
（`Kdf` / `CorruptSecrets` 若不叫这个名字，用最贴近的；**别新增变体**）。

- [ ] **Step 4: 跑，确认绿**

```bash
cargo test -p mullion-store cloud:: 2>&1 | grep "test result"
```

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-store/src/cloud.rs
git commit -m "feat(store): cloud.toml 存一份独立备份口令 (F283)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: 改主密码时把口令一起重封（+ 机械守护）

**Files:**
- Modify: `crates/mullion-store/src/vault.rs:538-571`（`reseal_cloud_secret` / `take_cloud_secret_plain`）

**为什么必须做**：见上文 **C2**。`seal_local` 用的是 vault 当前的 `self.key`，
`set_master_password`/`clear_master_password` 会换掉它。

- [ ] **Step 1: 先写失败测试**（追加进 `cloud.rs` 的 `mod tests`，与既有两条
  `changing_the_master_password_reseals_the_cloud_secret` 并排）

```rust
    /// F283/C2:改主密码之后,备份口令**还要解得开**。
    ///
    /// 漏了重封的症状:用户设一次主密码 → 云备份从此每轮失败,
    /// 而失败信息指向"口令解不开",没人会把它跟改主密码联系起来。
    #[test]
    fn changing_the_master_password_also_reseals_the_backup_passphrase() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = test_vault(dir.path());
        let mut cfg = CloudConfig::default();
        set_passphrase(&mut cfg, &vault, "the-backup-pass").unwrap();
        save(dir.path(), &cfg).unwrap();

        vault.set_master_password("a-master-password").unwrap();

        let back = load(dir.path());
        assert_eq!(
            passphrase(&back, &vault).unwrap(),
            "the-backup-pass",
            "改主密码之后备份口令解不开了"
        );
    }

    /// 撤销主密码那条路同样要重封 —— 两个入口共用一份实现,
    /// 但"共用"这件事本身要有测试钉着(既有的 SK 两条就是这么钉的)。
    #[test]
    fn clearing_the_master_password_also_reseals_the_backup_passphrase() {
        // 照抄 `clearing_the_master_password_also_reseals_the_cloud_secret` 的
        // 建库/建 key_source 手法,把断言换成 passphrase。
    }

    /// F283/C2 的**机械守护**:`CloudConfig` 里每多一个 `*_sealed` 字段,
    /// 就必须在重封的两个函数体里各露一次面。
    ///
    /// 这条是给**将来**写的:今天两个字段都在,但本项目已经三次栽在
    /// 「列举式门控加档必然漏」上,而漏掉的症状是静默的(密文用旧密钥封着,
    /// 只在下一次用到它的时候才炸,且报错指向别处)。
    ///
    /// 自证会变红:把 `reseal_cloud_secret` 里 `passphrase_sealed` 那几行删掉。
    #[test]
    fn every_sealed_field_in_the_cloud_config_gets_resealed() {
        let cloud_src = include_str!("cloud.rs");
        let vault_src = include_str!("vault.rs");
        let fields: Vec<&str> = cloud_src
            .lines()
            .filter_map(|l| l.trim().strip_prefix("pub "))
            .filter_map(|l| l.split(':').next())
            .filter(|n| n.ends_with("_sealed"))
            .collect();
        assert!(fields.len() >= 2, "没抓到 *_sealed 字段,判据本身失效了:{fields:?}");
        for f in fields {
            for func in ["fn reseal_cloud_secret(", "fn take_cloud_secret_plain("] {
                let body = body_of(vault_src, func);
                assert!(
                    body.contains(f),
                    "`{f}` 是 seal_local 封的,改主密码时没人重封它 —— \
                     症状是改完密码之后这段密文永久解不开,且报错指向别处。\
                     请在 `{func}` 里一并处理。"
                );
            }
        }
    }
```

`body_of(src, "fn xxx(")`：本仓库多处已有同名辅助（`app.rs` 测试模块里那个花括号
配平版本）。**在 `cloud.rs` 的 `mod tests` 里写一份本地 helper**（不要跨 crate 引），
并放在使用它的测试**之前**——本项目登记过「辅助函数定义在被锚定函数之后会被
`body_of` 一起吞掉」这个坑。实现要花括号配平（不要用 `split("\npub fn ")`，
那条切法在本仓库造过两次假绿）。

- [ ] **Step 2: 跑，确认红**（`cargo test -p mullion-store cloud::`）

- [ ] **Step 3: 实现**

`take_cloud_secret_plain` 改成同时带走两份，`reseal_cloud_secret` 同时封回两份。
建议把载体写成一个私有小结构，别用 `(Option<Vec<u8>>, Option<Vec<u8>>)`
（两个同型 `Option` 挨着，调换顺序编译器一个字都不会说）：

```rust
/// 换密钥**之前**从 `cloud.toml` 里取出来、换完再封回去的那几段。
///
/// 一个字段一个 `Option`:`None` = 没存过/解不开,那种情况下没东西要重封
/// (解不开的那份**不要清空**——用户重填一次就能恢复,清了就真没了)。
#[derive(Default)]
struct CarriedCloudSecrets {
    secret: Option<Vec<u8>>,
    passphrase: Option<Vec<u8>>,
}
```

两个函数按字段逐一处理，错误仍一律包成 `StoreError::CloudReseal`
（`vault.rs:546-548` 那段注释解释了为什么：主密码已经落盘了，抛裸错会被
调用方报成「主密码没能改成」，而那句话是错的）。**只 `load`/`save` 一次**
（`cloud.rs` 的 `save` 见 `corrupt` 会拒绝回写，别绕过它）。

- [ ] **Step 4: 跑绿 + 变异自证**

```bash
cargo test -p mullion-store 2>&1 | grep "test result"
```
变异（**先 commit 再做**）：把 `reseal_cloud_secret` 里处理 `passphrase_sealed`
的那几行删掉 → 上面三条测试应至少红两条。`git checkout crates/mullion-store/src/vault.rs` 还原。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-store/src/vault.rs crates/mullion-store/src/cloud.rs
git commit -m "fix(store): 改主密码时把备份口令与 SK 一起重封 (F283)

漏掉的症状是静默的:密文仍用旧密钥封着,下一次云备份才炸且报错指向别处。
加机械守护:CloudConfig 里每个 *_sealed 字段都必须出现在重封的两个函数体里。
守护:every_sealed_field_in_the_cloud_config_gets_resealed

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: 载荷改用备份口令（内层 + 外层）

**Files:**
- Modify: `crates/mullion-app/src/cloudsync.rs:177-244`（`prepare`）

**这是本切片的要害。** 见 **C1**（内层必须重封）与 **C4**（指纹算法不动）。

- [ ] **Step 1: 先写失败测试**（追加进 `cloudsync.rs` 的 `mod tests`）

```rust
    /// F283:**没有主密码也能备份**了 —— 这正是本切片的目的。
    ///
    /// 这条取代了旧的 `a_vault_without_a_master_password_is_refused_with_the_real_reason`
    /// (那条钉的是已经拆掉的设计 D6)。
    #[test]
    fn a_keyring_vault_with_a_passphrase_can_back_up() {
        // 建一个**钥匙串方案**的 vault(不设主密码)+ 一份带口令、带 AK/SK 的 cfg,
        // 断言 prepare 返回 Prepared::Ready(..)。
    }

    /// 没设口令 → **说出原因**,不是静默不跑(C5)。
    #[test]
    fn a_config_without_a_passphrase_is_refused_with_the_real_reason() {
        // 同上但 cfg.passphrase_sealed 为空,
        // 断言 matches!(got, Prepared::Failed(m) if m.contains("备份口令"))。
    }

    /// C1:包里的 `secrets.enc` 必须是**用备份口令重封过**的,不是盘上那份原样。
    ///
    /// 判据扎在字节上:拿备份口令能 `open_secrets` 出明文,而且那段字节
    /// **不等于**盘上 `secrets.enc` 的原始内容。
    ///
    /// 只断言"不等于"是不够的(随机 nonce 让任何一次重新加密都不等于),
    /// 所以必须同时断言"用口令解得开"。
    #[test]
    fn the_secrets_inside_the_payload_are_resealed_with_the_backup_passphrase() {
        // prepare → 拿 Payload → 用口令解开外层 → read_pack → 取 secrets blob
        // → mullion_store::open_secrets(blob, "口令") 应成功,
        //   且 blob != std::fs::read(dir.join("secrets.enc")).unwrap()
    }

    /// C4:指纹仍对**盘上 secrets.enc 的原始字节**取。
    ///
    /// 反例会红:若改成对重封结果取,同一份内容连算两次指纹都不相等 ——
    /// 后果是每个 interval 都上传一次,打的是用户的付费桶。
    #[test]
    fn the_fingerprint_does_not_change_just_because_we_resealed() {
        // 同一个 dir/vault/cfg 连调两次 prepare(第二次前把 cfg.last_fingerprint
        // 清空,避免走 Unchanged 早退),断言两次 Ready 的 fingerprint 相等。
    }
```

  建 vault/cfg 的手法照抄本文件既有的
  `a_first_run_with_a_master_password_produces_something_to_upload`。

- [ ] **Step 2: 跑，确认红**

```bash
cargo test -p mullion-app cloudsync:: 2>&1 | tail -20
```

- [ ] **Step 3: 实现**

把 `prepare` 的 ②③ 两步改成：

```rust
    // ② 取备份口令。**最先取**:没口令就没必要打包,而且这句话要说得出口(C5)。
    let pass = match cloud::passphrase(cfg, vault) {
        Ok(p) if !p.is_empty() => p,
        Ok(_) => {
            return Prepared::Failed(
                "云端备份还没设置备份口令 —— 请在设置的「云端备份」里设一个。\
                 它与主密码无关,忘了则云端已有的备份无法恢复。"
                    .into(),
            )
        }
        Err(e) => return Prepared::Failed(format!("读不出备份口令:{e}")),
    };

    // ③ 内层:`secrets.enc` **不能原样塞**(设计 C1)。拿明文用备份口令重封,
    //    与 F46-a 的导出路径逐字同形 —— 于是这份云端载荷跟 `.mullionpack`
    //    同形,将来恢复那头可以直接走 `install_pack`。
    //
    //    **原样塞会静默坏**:钥匙串方案封出来的密文只有源机解得开,
    //    恢复那天的症状是"会话全在、每条都要重新输密码"且零报错。
    let plain = match vault.secrets_plaintext() {
        Ok(p) => p,
        Err(e) => return Prepared::Failed(format!("读不出凭据:{e}")),
    };
    // 一条密码都没存过 → 不封空密文(照 `app.rs` 导出那条):
    // 封了的话恢复那头会照着一段空字节管用户要口令。
    let inner = if plain.is_empty() {
        Vec::new()
    } else {
        match portable::seal_secrets(plain.as_bytes(), &pass) {
            Ok(b) => b,
            Err(e) => return Prepared::Failed(format!("重封凭据失败:{e}")),
        }
    };

    // ④ 打包 + 外层整体加密,仍然用**同一句口令**(不同的盐,见 `secrets_file` 的头)。
    let text = match portable::write_pack(files, &inner, env!("CARGO_PKG_VERSION"), stamp_rfc3339) {
        Ok(t) => t,
        Err(e) => return Prepared::Failed(format!("打包失败:{e}")),
    };
    let sealed = match portable::seal_secrets(text.as_bytes(), &pass) {
        Ok(b) => b,
        Err(e) => return Prepared::Failed(format!("加密失败:{e}")),
    };
```

**注意**：`files` 在 ① 已经 `collect_top_level` 过了，`fp` 也已在 ① 用
**盘上原始 `secrets` 字节**算好（C4，那一段不要动）。
`portable::seal_secrets` 这个名字读起来像"只封凭据"，但它就是"用口令封一段字节"
（`portable.rs:158-166`）——在调用点用一行注释说明为什么两层都用它，别新造函数。

同时更新 `prepare` 头上那段模块注释：把「成立的前提只有一条：设计 D6 要求
云备份必须先设主密码」整段换成 C1 的新理由（**这段注释现在是错的，留着会害下一个人**）。

- [ ] **Step 4: 跑绿**

```bash
cargo test -p mullion-app cloudsync:: 2>&1 | grep "test result"
```
旧测试 `a_vault_without_a_master_password_is_refused_with_the_real_reason` 会红 —— 
**删掉它**（它钉的是已拆除的设计 D6），Step 1 的两条新测试是它的替代。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/cloudsync.rs
git commit -m "feat(app): 云端载荷改用独立备份口令加密,内层凭据一并重封 (F283)

C1:原样塞 secrets.enc 的前提是"必须有主密码",本切片拆掉了那条前提 ——
钥匙串方案的密文只有源机解得开,恢复那天零报错地全要重输密码。
守护:the_secrets_inside_the_payload_are_resealed_with_the_backup_passphrase
     the_fingerprint_does_not_change_just_because_we_resealed

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: 两次 Argon2id 搬进阻塞半

**Files:**
- Modify: `crates/mullion-app/src/cloudsync.rs`（`Payload` 结构、`prepare`、`upload_blocking`）

**为什么**：见 **C3**。两次派生 60~120 ms，跑在事件循环线程上就是每次内容变更卡一帧。

- [ ] **Step 1: 先写失败测试**

```rust
    /// C3:派生密钥是 60~120 ms 的活,**不许在事件循环线程上做**。
    ///
    /// 源码切片守护:`prepare` 体内不许出现任何封装调用,
    /// 它们必须都在 `upload_blocking` 体内。
    ///
    /// 自证会变红:把 `seal_secrets(` 挪回 `prepare`。
    #[test]
    fn the_key_derivation_happens_off_the_event_loop_thread() {
        let src = prod_src();
        let prep = strip_comments(&body_of(&src, "pub fn prepare("));
        assert!(
            !prep.contains("seal_secrets("),
            "封装留在 prepare 里 = 每次内容变更卡一帧(两次 Argon2id,60~120ms)"
        );
        let up = strip_comments(&body_of(&src, "pub fn upload_blocking("));
        assert_eq!(
            up.matches("seal_secrets(").count(),
            2,
            "内外两层都该在阻塞半里封"
        );
    }

    /// 搬家不许把 `Vault` 也搬过去 —— 既有守护
    /// `the_blocking_half_does_not_know_about_the_vault` 仍必须绿(别改它)。
    #[test]
    fn the_payload_carries_bytes_and_strings_only() {
        let src = prod_src();
        let decl = body_of(&src, "pub struct Payload");
        assert!(!decl.contains("Vault"), "Payload 不许带 Vault");
    }
```

  `prod_src()` / `strip_comments` / `body_of`：本文件测试模块里已有同族手法
  （`the_blocking_half_does_not_know_about_the_vault`，`cloudsync.rs:783-803`），
  照抄；缺什么就在测试模块里补本地 helper，**定义在使用它的测试之前**。

- [ ] **Step 2: 跑，确认红**

- [ ] **Step 3: 实现**

`Payload` 改成携带"还没封的料"：

```rust
/// 交给阻塞半的全部输入。**一个 `Vault` 都不带**(见 [`prepare`])。
///
/// 这里躺着两段明文(凭据正文与备份口令),和已经在这儿躺着的 SK 一样:
/// 同一个进程内的内存传递,不落盘、不进日志。**封装本身放在阻塞半**,
/// 因为 Argon2id 一次 30~60 ms,两层就是 60~120 ms —— 放在事件循环
/// 线程上就是每次内容变更卡一帧。
pub struct Payload {
    pub fingerprint: String,
    pub files: Vec<mullion_store::PackFile>,
    pub secrets_plain: String,
    pub passphrase: String,
    pub secret_access_key: String,
}
```

`prepare` 只留"要 vault 的事"：算指纹、取口令、取凭据明文、解 SK，然后原样装进 `Payload`。
`upload_blocking` 开头做三件事：内层 `seal_secrets` → `write_pack` → 外层 `seal_secrets`，
任一步失败返回 `UploadOutcome::Failed(..)`。`write_pack` 需要的时间戳用已有的
`stamp_rfc3339` 形参。

Task 3 写的四条测试要跟着搬到新的边界上（"包里 secrets 被重封""指纹不变"
现在要在 `upload_blocking` 的产出上验，或者把封装那段抽成一个可单测的私有函数
`fn seal_payload(p: &Payload, version: &str, stamp: &str) -> Result<Vec<u8>, String>`
——**推荐后者**：本项目登记过「纯函数测得扎实、接线没人看着」这种恒绿，
抽出来之后再配上面那条源码切片守护接线，两头都有人看着）。

- [ ] **Step 4: 跑绿 + 变异自证**（先 commit）

变异：把 `seal_payload(..)` 调用从 `upload_blocking` 挪回 `prepare` →
`the_key_derivation_happens_off_the_event_loop_thread` 应红。还原。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/cloudsync.rs
git commit -m "perf(app): 备份的两次 Argon2id 搬进阻塞半,不卡事件循环 (F283)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: 拆掉「清主密码自动关云备份」

**Files:**
- Modify: `crates/mullion-app/src/app.rs`
  - 删 `cloud_config_after_clearing_master_password`（`app.rs:15777-15785`）
  - 删 `disable_cloud_backup_after_clearing_master_password`（`app.rs:3594-3621`）
  - 删调用点（`O::ClearPassword`，`app.rs:3484-3496` 里那一句）
  - 删旧守护 `clearing_the_master_password_turns_an_enabled_cloud_backup_off`（`app.rs:25498`）

- [ ] **Step 1: 先写替代测试**

```rust
    /// F283:主密码与云备份解耦之后,**清主密码不许再去碰云配置**。
    ///
    /// 旧行为(设计 D12)是"清主密码 → 自动把云备份开关翻成 false",
    /// 理由是"钥匙串派生的密钥换台机器解不开"。F283 之后载荷根本不用
    /// vault 密钥封,那条理由不成立了,而副作用很实:用户清一次主密码,
    /// 云备份被悄悄关掉,下次想起来看的时候已经几周没有备份了。
    ///
    /// 自证会变红:把 `disable_cloud_backup_after_clearing_master_password`
    /// 的调用加回 `O::ClearPassword` 那条分支。
    #[test]
    fn clearing_the_master_password_leaves_the_cloud_backup_alone() {
        let src = prod_src();
        let arm = /* 取 `O::ClearPassword` 那条 arm,手法照抄被删掉的那条旧测试 */;
        assert!(
            !arm.contains("cloud"),
            "清主密码那条路又去碰云配置了 —— F283 已经把这两件事解耦"
        );
        assert!(
            !src.contains("fn disable_cloud_backup_after_clearing_master_password"),
            "接线函数应随设计 D12 一起删掉,留着迟早被接回去"
        );
    }
```

- [ ] **Step 2: 跑，确认红**（现在源码里还有那些字样）

- [ ] **Step 3: 删代码**

**只删这三处 + 旧测试**。`Vault::reseal_cloud_secret` 那一整套**保留**
（设计 D7 明确：AK/SK 随 vault 密钥重封的逻辑保留，Task 2 刚给它加了第二个字段）。
删完编译若报"未使用的 import/函数"，按 Scope Discipline 只删**本次改动导致变得无用**的那些。

- [ ] **Step 4: 跑绿**（`cargo test -p mullion-app 2>&1 | grep "test result"`）

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "refactor(app): 拆掉「清主密码自动关云备份」(F283)

设计 D12 的理由(vault 密钥换机解不开)在载荷改用独立口令之后不成立;
留着的副作用是用户清一次主密码、云备份被悄悄关掉。
守护:clearing_the_master_password_leaves_the_cloud_backup_alone

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: 设置页的备份口令

**Files:**
- Modify: `crates/mullion-app/src/ui/settings.rs`
  （`cloud()` 在 `632-851`；`SettingsDraft` 字段在 `102/153` 一带；
  守护表 `CLOUD_ROWS` 在 `1656-1670`；掩码测试在 `1792`）
- Modify: `crates/mullion-app/src/app.rs`（草稿的读入与写回：
  `the_settings_dialog_starts_from_the_cloud_config_on_disk` / 
  `the_cloud_draft_is_written_back_over_the_config_on_disk` 两条守护指着的地方）

- [ ] **Step 1: 先写失败测试**

```rust
    /// F283:口令与确认两格都必须打码,别的格一格都不许。
    ///
    /// 这条取代 `only_the_secret_key_field_is_masked`(判据从"只有 SK"
    /// 扩成"SK 与两个口令格"),**两头都钉**:漏钉后半句的话,
    /// "把 password(true) 挂到 Endpoint 上"这种复制粘贴 bug 逃得掉。
    #[test]
    fn only_the_secret_fields_are_masked() {
        let rows = cloud_rows();
        for (label, field) in CLOUD_ROWS {
            let row = row_of(&rows, label, field);
            assert_eq!(
                row.contains(".password(true)"),
                matches!(*field, "cloud_secret_new" | "cloud_pass_new" | "cloud_pass_confirm"),
                "「{label}」的打码状态不对"
            );
        }
    }

    /// 两次输入不一致时,**「确定」必须按不动**。
    ///
    /// 只给一行红字是不够的:用户点了确定、对话框关掉、红字消失,
    /// 他会以为口令设好了 —— 而实际上没写进去。忘记口令 = 云端备份作废,
    /// 这个误会的代价太大。
    #[test]
    fn a_mismatched_passphrase_blocks_the_ok_button() { /* 见 Step 3 的做法 */ }

    /// 设置页必须写明"忘了没有找回途径"(设计 D7 的硬要求)。
    #[test]
    fn the_cloud_section_says_a_forgotten_passphrase_cannot_be_recovered() {
        let body = body_of(prod_src(), "fn cloud(");
        assert!(body.contains("忘") && body.contains("找回"));
    }
```

- [ ] **Step 2: 跑，确认红**

- [ ] **Step 3: 实现**

1. `SettingsDraft` 加 `cloud_pass_new: String` 与 `cloud_pass_confirm: String`
   （两处：结构体定义 + `new`/`Default` 初始化），并加一个判据方法：

```rust
    /// 两次输入的备份口令是否不一致(空着 = 不改,不算不一致)。
    pub fn cloud_pass_mismatch(&self) -> bool {
        !self.cloud_pass_new.is_empty() && self.cloud_pass_new != self.cloud_pass_confirm
    }
```

2. `cloud()` 的门控改成 **`let ready = env.store_available;`**，
   把 `!ready` 那段「云端备份需要先设置主密码…」的说明整段删掉
   （它现在是错的）。**别把「有没有口令」做成门控**——那会造出
   F270 踩过的「没有出口的陷阱」（要填口令的那一格自己被灰掉）。

3. 在 `Access Key Secret` 那一行之后插入两行（严格照抄同一块的写法：
   `ui.add_enabled(ready, TextEdit::singleline(..).password(true).desired_width(w))`
   + `.changed()` 里 `*out = SettingsOut::Preview;` + `ui.end_row();`）：

```rust
        ui.label("备份口令");
        // …TextEdit(&mut draft.cloud_pass_new).password(true).hint_text("留空 = 不改")…
        ui.end_row();

        ui.label("确认口令");
        // …TextEdit(&mut draft.cloud_pass_confirm).password(true)…
        ui.end_row();
```

   `CLOUD_ROWS` 同步加两条（顺序与源码一致）。

4. 两行之后加状态与警示（用 `.size(11.0)` + `theme::c32(t.fg_muted)`，
   与本分节另外两处说明同形；**不要用 `theme::hint_text`**，见 `settings.rs:788` 一带的注释）：
   - 当前状态：`env.cloud_has_passphrase` 为真 → 「当前:已设置备份口令」，否则
     「当前:还没设置备份口令,云端备份不会运行」。
   - 恒显示一句：「备份口令与主密码无关。**忘了没有找回途径**,云端已有的备份将无法恢复。」
     （照抄 `pack_dialog.rs:141-145` 的措辞姿态，那里有测试钉着同类说明。）
   - `draft.cloud_pass_mismatch()` 为真 → 一行红字「两次输入的备份口令不一致」
     （颜色用本文件既有的错误色，照抄 `settings.rs:586` 一带主密码那条）。

   `SettingsEnv` 加 `cloud_has_passphrase: bool`（构造点在 `app.rs`，
   用 `mullion_store::cloud::has_passphrase(&cfg)` 填）。

5. 「确定」按钮门控：在设置对话框的底部按钮处（F280 刚改成 bottom_up 的那段）
   加上 `!draft.cloud_pass_mismatch()` 这个条件。若那里今天没有任何 enabled 门控，
   用 `ui.add_enabled(can_ok, ..)` 加一个，并在按钮旁给出原因文字（别做成
   "按钮灰着但不说为什么"）。

6. 写回路径（`app.rs`）：`cloud_pass_new` 非空且不 mismatch 时调
   `mullion_store::cloud::set_passphrase(&mut cfg, vault, &draft.cloud_pass_new)`，
   与 SK 的写回逐字同形（找 `set_secret_key` 的调用点照抄），
   写完把草稿里两个口令字段清空（**别把明文留在草稿里跨次打开**）。

- [ ] **Step 4: 跑绿**

```bash
cargo test -p mullion-app 2>&1 | grep "test result"
cargo test -p mullion-app --test glyph_whitelist 2>&1 | grep "test result"
```

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/settings.rs crates/mullion-app/src/app.rs
git commit -m "feat(app): 设置页加备份口令(两遍确认 + 无找回警示) (F283)

门控从「要有主密码」改成「库打开着」——「有没有口令」不做门控,
否则要填口令的那一格自己被灰掉(F270 踩过的无出口陷阱)。
守护:only_the_secret_fields_are_masked / a_mismatched_passphrase_blocks_the_ok_button

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 7: spec 登记

**Files:**
- Modify: `spec.md`（F283 在 `365` 行；F272 在 `354` 行）

- [ ] **Step 1: 改 F283 那一格**

把「**片二，未实现。**」改成「已实现（v0.1.113）」，并把落点写进去：
`cloud::{set_passphrase,passphrase,has_passphrase}` + `cloudsync::prepare` 内外两层重封
+ `Vault::reseal_cloud_secret` 带上口令 + 机械守护
`every_sealed_field_in_the_cloud_config_gets_resealed`。
保留「忘口令 = 云上历史备份作废，无找回」那句。

- [ ] **Step 2: 订正 F272 那一格**

F272 今天写着「**硬前置 F71**：未设主密码时设置页那一节整节灰掉」——
这条已被 F283 取代。改成：「硬前置 F71 已由 F283 取代：载荷改用独立备份口令派生，
未设主密码也能备份；设置页的门控只剩「库打开着」」。**别删原文的理由**，
在后面接一句「F283 起不再成立」——半年后回头看，理由比结论值钱。

- [ ] **Step 3: 提交**

```bash
git add spec.md
git commit -m "docs(spec): F283 登记为已实现,订正 F272 的主密码硬前置 (F283)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## 收尾（控制器执行，不在任务内）

1. 全量绿：`cargo test --workspace`、`clippy --workspace --all-targets -- -D warnings`、`cargo fmt --check`
2. 全分支终审（跨任务视角 + 计划逐条核对）
3. 发版 0.1.113（走 `release-windows` skill 一条龙）

**人工验收清单（无头环境验不了的）**：
- 没设主密码的机器上开云备份 → 设口令 → 手动「立刻备份到云」应成功（今天会被拒）
- 设过主密码的机器上改一次主密码 → 云备份仍能跑（口令被重封了）
- 两次口令输不一致 → 「确定」按不动且说明原因
- 升级后老用户：口令为空 → 状态栏应弹一次「还没设置备份口令」，且**不碰网络**
- 备份时不卡帧（Argon2id 在阻塞半）

## 本切片**不做**的事

- **恢复/下载路径**（F274/F275）：本切片只改"封"的那一半。做完之后云端载荷与
  F46-a 的 `.mullionpack` 同形，恢复那头可以直接 `install_pack` + 口令，但那是下一片。
- **老备份的兼容**：升级前上传的对象是 vault 主密钥封的，本版不去读它们（也没有读的路径）。
  发版说明里要写清楚：升级后请设置备份口令，此前的云端备份包需要源机主密码才能解开。
- **指纹改对明文取**（C4 里解释了为什么不做）。
