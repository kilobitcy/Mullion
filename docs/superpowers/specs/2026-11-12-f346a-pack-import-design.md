# F346-a 手机导入 `.mullionpack`(只导项目可达凭据 + Keystore 重封)

日期:2026-11-12。上位设计:`2026-11-12-android-client-design.md` §3 F346;ADR-013 决策 6「数据单向流入手机」。

F346 拆两片:**F346-a**(本文,文件导入)与 **F346-b**(云端拉包:cloud crate 补 GET、手机录云配置、解外层)。

修订:初稿经 `/review` 复核后改为「内存裁剪」方案(§3),并补齐空密文包、schema 过新、
Keystore 不可改参、备份还原、守护判据等(复核意见 C1~C3、R1~R10)。

## 1. 已定决策

| # | 决策 | 理由 |
|---|---|---|
| D1 | 拆片:a 文件导入,b 云拉 | 云配置怎么进手机是独立问题,不该卡住主干 |
| D2 | **只裁密文,元数据整份**:`sessions.toml`、`known_hosts.toml` 整份导入;`secrets.enc` 只留项目可达条目 | 分组继承/跳板解析沿用桌面同一份代码,不会裁出悬空引用;F359 用得上非项目会话元数据。主机名/用户名不是凭据 |
| D3 | **信封**:AndroidKeyStore 里不可导出的 AES-256-GCM 包装钥,包一把随机 32 字节数据密钥;Rust 侧拿数据密钥当 `InMemoryKey` | 手机上的 `secrets.enc` 与桌面同格式、同代码。**Keystore 密钥生成后参数不可改**:F347 加认证要求时要新 alias 解旧包、重包,所以 `vault-key.bin` 首字节留版本号(§5) |
| D4 | 导入 = 整份替换,**不做备份** | 手机是只读消费者,真源在桌面;重导一次即恢复 |
| D5 | **「不回写」的主判据是结构性的**:`mullion-ffi` 不依赖 `mullion-cloud`;辅以源码守护 | 列举式黑名单必漏(H 切片教训);F346-b 引入 cloud 时再换判据 |
| D6 | 手机本地 TOFU 确认(F350)将来另存一个文件 | 重导不会冲掉手机上点过的「信任」 |
| D7 | **归档项目也算可达** | F348「搜索穿透归档」意味着手机能打开归档项目 |
| D8 | 跳板链展开失败的节点:只留节点自身密文,**计数报给用户** | `expand_chain` 全有或全无;不让一条坏配置拖垮整次导入,但「桌面能连、手机说悬空」必须事先看得见 |

## 2. 数据流

```
SAF 选文件 → Kotlin 读成文本
  → ffi::inspect_pack(text) -> PackInfo { has_secrets }      只读,决定要不要出口令框
  → (has_secrets 时)用户输包口令
  → VaultKey.dataKey()(Keystore 解包;首次或解包失败则重新生成,见 §5)
  → ffi::import_pack(dir, text, password, data_key)          [Dispatchers.IO]
      → store::portable::install_projects_only(dir, text, password, &InMemoryKey(k))
  → ImportSummary
```

## 3. store(判据,内存裁剪)

**不开带密文的 Vault,裁剪全在内存里做** —— 未裁的全量密文从不落盘,也不需要给 `Vault` 加 mutator。

- `project_reachable_secret_keys(sessions, groups, projects) -> Reachable { keys: BTreeSet<String>, broken_nodes: u32 }`:
  对**每个项目(含归档)**的每个节点,收集「节点自身 + `jump::expand_chain` 展开的整条跳板链」的会话 id;
  这些会话的密文键 `<id>`,以及其中 `Auth::Ref(c)` 的 `cred:<c>`。展开失败 → 只留节点自身,`broken_nodes += 1`。
- `install_projects_only(dir, pack_text, password, key_source) -> Result<PhoneImport, StoreError>`:
  1. `remove_dir_all(dir/.import-staging)`(清上次崩溃残留),再建。
  2. `read_pack`;包里的 `sessions.toml`、`known_hosts.toml` 写进 staging(**不写 `secrets.enc`**)。
  3. `Vault::open(staging, key_source)`:没有密文文件 → 空密文表;**迁移照常**,旧 schema 包按迁移后模型算;
     schema 过新 → `UnsupportedSchema` 原样上抛。用 `list()` / `groups()` / `projects()` 算 `Reachable`。
  4. 密文:`secrets_blob` 为空 → 空表(**不调 `open_secrets`**,它会把空 blob 判成「没有口令头」);
     否则 `open_secrets`(口令错 → `WrongPassword`)→ `toml::from_str::<BTreeMap<String, SecretEntry>>` →
     `retain(可达键)` → `toml` → `crypto::encrypt` → `secrets_file::encode(Keyring)` → 写进 staging。
     与 `Vault::save` 写出的字节同构。
  5. staging 里的文件逐个 `rename` 进 `dir`;包里没带的顶层文件在 `dir` 里删掉;删 staging。
     `settings.toml`、`layouts/` 一律不写。
     **三次 rename 之间崩溃不另做事务**:新 `sessions.toml` + 旧 `secrets.enc` 同一把数据钥,
     `Vault::open` 的孤儿裁剪会容忍(有测试钉住);重导即恢复。
  6. 任何一步失败:删 staging,`dir` 不动(第 5 步之前)。
- `PhoneImport { projects: Vec<String>, kept: u32, dropped: u32, broken_nodes: u32, unrecognized: u32 }`。
  `unrecognized` **只计**白名单外 / base64 坏的条目;`settings.toml`、`layouts/*` 是「按设计不写」,**不计**
  —— 否则每份桌面包都报「跳过 N 条」,用户会以为丢了会话。
- `InMemoryKey` 文档从「测试用」改为「测试与 Android 生产路径」,防止被当测试辅助 `#[cfg(test)]` 掉。

## 4. ffi

- `inspect_pack(pack_text: String) -> Result<PackInfo, FfiError>`:`PackInfo { has_secrets: bool }`。只读。
- `import_pack(dir: String, pack_text: String, password: String, data_key: Vec<u8>) -> Result<ImportSummary, FfiError>`。
  `data_key` 长度不为 32 → 报错,**不碰 `dir`**。
- `FfiError` 增变体,文案各指一条出路:
  - `WrongPassword` —— 「口令不对,再输一次」;
  - `NeedsNewerApp(u32)` —— 包的 schema / format 版本比手机端新 →「请升级手机端」
    (桌面与手机版本号独立,手机落后一版是**常态**);
  - `BadPack(String)` —— 损坏 →「换一份包」。
- 「不回写」守护:
  1. **结构性**:扩 `tests/dependency_direction.rs`,断言 `mullion-ffi` 不依赖 `mullion-cloud`;
  2. 源码守护:`src/` **剥注释、切掉 `#[cfg(test)]` 模块后**不出现 `write_pack`、`seal_secrets`、`put_no_overwrite`、`.save(`。

## 5. Kotlin

- `VaultKey`:alias `mullion-vault-wrap-v1`,`AES/GCM/NoPadding` 256;
  `filesDir/vault-key.bin` = `版本(1 字节,=1) ‖ iv(12) ‖ 密文`。
  - 不存在 → `SecureRandom` 生成 32 字节数据密钥、包好、落盘;
  - **解包失败**(alias 丢了 / 系统还原 / 清除凭据存储)→ 视为无钥:删 `vault-key.bin` 与 `filesDir/vault/`、重新生成,
    提示「本机密钥已失效,请重新导入」(D4:重导即恢复)。
  - 本片**不设**用户认证要求(F347)。
- `AndroidManifest.xml`:`android:allowBackup="false"` —— 否则系统备份带走 `vault-key.bin` 而带不走 Keystore 钥。
- 最小界面加「导入 .mullionpack」:`ACTION_OPEN_DOCUMENT` → `inspect_pack` →(需要时)口令框 → 摘要文本
  「已导入 N 个项目:名1、名2…;保留 M 份凭据,丢弃 K 份」;`broken_nodes`、`unrecognized` 非零时各加一句。
  项目列表界面是 F348。
- 导入数据目录:`filesDir/vault/`。
- 已知代价(F347 前):数据密钥经 uniffi `Vec<u8>` 过 JNA,Kotlin 堆与 JNA 缓冲各留一份,不零化。

## 6. 测试

| 验收 | 手段 |
|---|---|
| 只导项目可达凭据 | store 单测:节点密文留;分组继承来的跳板留;跳板会话引用的 `cred:` 留;**归档项目的节点留**;无关会话丢;无人引用的凭据丢;只被非项目会话用的跳板丢;展开失败的节点只留自身且 `broken_nodes == 1` |
| 不回写 | ① store:导入后 `dir` 内只有三文件白名单(无 `cloud.toml`、`layouts/`、`settings.toml`、staging);② ffi 不依赖 `mullion-cloud`;③ ffi 源码守护(§4) |
| 无密文包 | store 单测:`secrets` 为空的包,空口令导入成功,`kept == 0` |
| 口令错 / 格式或 schema 过新 / 旧 schema 包 | store 单测;ffi 单测:过新 → `NeedsNewerApp` |
| 计数 | store 单测:带 `settings.toml` + `layouts/` 的桌面包 `unrecognized == 0`;塞一条 `../evil` → 1 |
| 重导整份替换 | store 单测:第二份包没带 `known_hosts.toml` → 手机上那份被删 |
| 崩溃残留 / 中间态 | store 单测:预置脏 staging 仍导入成功;新 `sessions.toml` + 旧 `secrets.enc` 的目录 `Vault::open` 成功 |
| `data_key` 长度 | ffi 单测:31 / 33 字节 → 报错且 `dir` 未被创建 |
| Keystore 包解 + 端到端 | 模拟器插桩:`VaultKey` 两次取得同一密钥;删 alias 后再取 → 重新生成且 `vault/` 被清;导入入库的测试包(Rust example 生成、假数据)后摘要的项目名与计数正确 |

## 7. 不做

云拉包(F346-b)、生物识别门控(F347)、项目列表界面(F348)、TOFU 三态(F350)。
包元数据认证(F362):终审 I5 —— `sessions.toml` / `known_hosts.toml` 明文无认证,篡改主机地址可骗走凭据;本片只在导入界面警示。
