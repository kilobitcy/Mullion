# F346-b 手机从云端拉包(只读,不回写)

日期:2026-11-12。上位设计:`2026-11-12-android-client-design.md` §3 F346;ADR-013 决策 6「数据单向流入手机」。
前片:`2026-11-12-f346a-pack-import-design.md`(文件导入,`install_projects_only` 与 Keystore 信封都在那里)。

修订:初稿经 `/review` 复核后改了 D4 的判据措辞(feature 统一)、D5 的理由、SK/口令解封不再依赖打开 `vault/`,
并补齐损坏探测、错误串线、前缀不符、列举后被清理等(复核意见 C1~C3、R1~R11)。

## 1. 已定决策

| # | 决策 | 理由 |
|---|---|---|
| D1 | 云配置**手机手填**,字段与桌面设置页一一对应(SK 等长串靠粘贴) | 不改桌面、不发桌面版;二维码/包内带配置都要动桌面导出格式 |
| D2 | **手动拉**:只在点「从云端更新」时拉最新一份 | 不做启动自动拉,省掉离线/失败提示/流量的整套状态 |
| D3 | AK/SK/备份口令**存本机**,SK 与口令用 Keystore 数据密钥封(与 `seal_local` 字节同构,但**不打开 `vault/`**,见 §4 store) | 每次输口令太烦;F347 之后自然受生物识别门控。不依赖 `Vault::open`:`vault/` 坏了时拉取是唯一的自愈手段,不能被它堵死 |
| D4 | **「不回写」判据换成 feature 门**:`mullion-cloud` 新增默认开的 `write` feature,`put_no_overwrite` 在门后;ffi 以 `default-features = false` 依赖 | 手机产物(孤立构建 `cargo ndk -p mullion-ffi`)里没有 PUT 代码。**注意 feature 统一**:`resolver = "2"` 下 `cargo test/clippy --workspace` 同时选中 app,`write` 会被打开,ffi 的测试二进制照样能链接 `put_no_overwrite` —— 所以判据是三条机械检查(§6),不是「写了编不过」。否掉:只读类型 `S3Reader`(只是约定,可绕);Kotlin 自发 GET(SigV4 两份实现必漂移) |
| D5 | 云配置在 `filesDir/cloud/cloud.toml`,**不在 `vault/` 里**;复用 store `cloud::load/save` 格式 | `vault/` 是「恰好三个文件」的整份替换区:插桩测试断言它只有三个文件(`PackImportTest`),Keystore 重置整目录删。云配置与「包里来的数据」生命周期不同,分开放。(`commit` 只碰三个固定文件名,放进去不会被抹 —— 不是理由) |
| D6 | 云端对象与 SAF 文件**同等不可信** | 完整性只靠对象存储账号权限;I5 仍归 F362 |
| D7 | 签名时间由 **Kotlin 侧按 UTC 格式化成 `YYYYMMDDTHHMMSSZ` 传入**,不自动校时 | ffi 没有 `time` 依赖,不自己写历法;测试可注入固定时间。偏差时 S3 回 `RequestTimeTooSkewed`,转成中文提示 |

## 2. 对象格式(桌面既有,不改)

`crates/mullion-app/src/cloudsync.rs::seal_payload`:

```
inner  = portable::seal_secrets(secrets_plain, 口令)
text   = portable::write_pack(files, &inner, app_version, stamp)
object = portable::seal_secrets(text.as_bytes(), 口令)
键     = {prefix}{seq:06}-{stamp}.mpk
```

内外两层同一口令(备份口令,F283 与主密码解耦)。

## 3. 数据流

```
「从云端更新」→ 先保存表单 → ffi::pull_latest(vault_dir, cloud_dir, data_key, now_amz_date)
  → 读 cloud.toml:读不懂 → CloudConfigCorrupt(先于必填项检查)
  → 解出 SK / 口令(cloud::*_with(&InMemoryKey),不打开 vault/);解不开 → CloudConfigUnreadable
  → 必填项检查
  → S3Client(default-features=false)::list_keys(prefix)
  → store::cloud::latest_key(prefix, keys)         None → 空列表 / 「列到 N 个但形状不对」分开报
  → S3Client::get(key, max = 16 MiB)
  → store::phone_import::open_cloud_object(blob, 口令)  先夹紧外层 Argon2 参数再派生
  → store::phone_import::install_projects_only(vault_dir, text, 口令, &InMemoryKey)
  → PullSummary { key, import: ImportSummary }
```

`Vault::open` 只出现在 `install_projects_only` 的 staging 里;`pull_latest` 自己全程不打开 `vault/`。

## 4. 各层改动

**mullion-cloud**
- `[features] default = ["write"]; write = []`;`put_no_overwrite` 及其独占的私有辅助挂 `#[cfg(feature = "write")]`。
- 新增 `S3Client::get(&self, key, amz_date, max_bytes) -> Result<Vec<u8>, CloudError>`:签名 GET;超上限报错(读到 `max_bytes + 1` 即止,不信 `Content-Length`);
  非 2xx 一律走既有 `status_err` → `CloudError::Status { code, body }`(不新增变体;ffi 按 code/body 里的 S3 错误码出文案);3xx 走既有 `redirect_err`;沿用 `max_redirects(0)` / `http_status_as_error(false)` / 60s 超时。
- `mullion-app` 依赖保持默认 feature,行为不变。
- 已知限制:`TIMEOUT` 是含正文的全局 60s,16 MiB 在高延迟手机链路上可能撞超时 —— 不改,列入人工待验。

**mullion-store**
- `cloud::latest_key(prefix, keys) -> Option<String>`:`parse_seq` 取最大;认不出的键忽略。与 `next_seq` 同口径。
- `cloud::set_secret_key_with` / `secret_key_with` / `set_passphrase_with` / `passphrase_with(cfg, &dyn MasterKeySource, ..)`:
  `crypto::encrypt` + `secrets_file::encode(Keyring)`,与 `Vault::seal_local` 在 Keyring 方案下字节同构(有测试钉住)。
  既有的 `Vault` 版本不动(桌面在用)。
- `cloud::is_corrupt(dir) -> bool`:文件存在但 `toml::from_str` 失败。`load` 本身不变。
- `phone_import::open_cloud_object(blob, passphrase) -> Result<String, StoreError>`:`check_kdf_params` → `open_secrets` → UTF-8(非 UTF-8 → `CorruptPack`,不回显内容)。

**mullion-ffi**
- 依赖 `mullion-cloud = { path, default-features = false }`;`Cargo.toml` 里「不得依赖 mullion-cloud」的注释与
  `dependency_direction.rs` 的文档一并改写成新判据(否则后人按旧注释把依赖删回去)。
- 导出:`load_cloud_form(cloud_dir) -> CloudForm`(SK/口令只给「已设置」布尔,不回传明文)、
  `save_cloud_form(cloud_dir, data_key, form)`(SK/口令空串 = 沿用已存)、
  `reset_cloud_config(cloud_dir)`、`pull_latest(vault_dir, cloud_dir, data_key, now_amz_date) -> PullSummary`。
- `CloudForm`:endpoint、region、bucket、prefix、path_style、socks5、access_key_id、secret_key、passphrase、has_secret、has_passphrase、is_corrupt。
- 桌面专用字段(enabled / keep / interval_min / last_* / no_if_none_match)**手机不读不写**:保存时从磁盘读回再改手机管的字段(读-改-写,别整份覆盖)。
- `FfiError` 增变体 `CloudConfigCorrupt` / `CloudConfigUnreadable` / `Cloud(文案)`,`From<CloudError>` 显式穷举。
  **cloud.toml 解封失败不得走 `?` 落到 `From<StoreError>`**(那会映射成 `BadPack`「包已损坏」,文案串线),显式映射到 `CloudConfigUnreadable`。
- S3 错误文案只提取 `Status.body` 里的 `<Code>`;没有 `<Code>`(HTML 错误页)就只显示状态码 —— 正文里可能带 `AWSAccessKeyId`/`StringToSign`。

**Android**
- `CloudSection`(`ImportSection` 下方)+ `CloudViewModel`;调用在 `Dispatchers.IO`。
- Keystore 永久失效路径(`PackImport` 的重置)同时清空 `filesDir/cloud/`,提示「云配置需重新填写」。
- I5 警示文案覆盖两条来源。

## 5. 错误与 UI

表单:Endpoint、Region、Bucket、前缀(默认 `mullion/`)、path-style 开关、SOCKS5(可空)、AK、SK、备份口令。
SK/口令为密码框,留空 = 沿用已存,旁边显示「已设置/未设置」(只看字段非空,不为显示去解密)。
按钮:「保存」「从云端更新」(后者先保存)。

| 情形 | 提示 | 本地 |
|---|---|---|
| `cloud.toml` 读不懂(先于必填项判) | 「云配置损坏」+「重置云配置」按钮 | 不动 |
| SK/口令密文解不开 | 「云配置里的 SK/口令解不开,请重填」 | 不动 |
| 必填项空(Endpoint/Region/Bucket/AK/SK/口令;Region 必填因 SigV4 签名需要) | 「先填 …」 | 不发请求 |
| 列表空 | 「云端还没有备份(前缀 xxx)」 | 不动 |
| 列到 N 个对象但无可识别键 | 「列到 N 个对象,都不是备份文件 —— 检查前缀(桌面默认 `mullion/`,结尾斜杠也算)」 | 不动 |
| 403 `InvalidAccessKeyId` / `SignatureDoesNotMatch` | 「AK/SK 不对(已保存,请重填)」 | 不动 |
| 403 `RequestTimeTooSkewed` | 「手机时间偏差过大,检查系统时间」 | 不动 |
| GET 404(列举后被桌面 keep 清理) | 「最新备份刚被替换,请重试」 | 不动 |
| 其余非 2xx | 「云端返回 {code} {Code}」 | 不动 |
| 对象 > 16 MiB | 「云端对象过大」 | 不动 |
| 口令错(外层或内层) | 「备份口令不对」 | 不动,口令框可重输 |
| KDF 越界 / 包坏 / schema 过旧 | 沿用 F346-a 文案 | 不动 |
| 成功 | 「已从云端 #000123(stamp)更新:N 个项目 …」 | 整份替换 |

**任何失败 `vault/` 原样不动**(`install_projects_only` 的 staging 保证)。

## 6. 测试

- **cloud**(`tests/fake_s3.rs`):`get` 的方法/路径/签名头;200 原字节;超上限报错;404/403 → `Status{code}`。
- **store**:`open_cloud_object` 往返(两层 `seal_secrets`,与 `seal_payload` 同形)、外层口令错、外层 KDF 越界在派生前被拒、非 UTF-8;
  `latest_key` 取最大 / 忽略杂键 / 空 → None;`*_with` 与 `Vault::seal_local` 互解(同一把钥,两个方向);`is_corrupt` 缺文件 false / 坏文件 true。
- **ffi**:
  - 端到端:假 S3 回放演示包(`tests/common/demo_pack.rs` 外再封一层)→ `pull_latest` → 计数与 F346-a 演示包一致;
  - 失败路径(口令错 / 404 / 过大)`vault/` 字节不变;
  - **`vault/` 损坏时照样能拉**(写坏 `sessions.toml` / `secrets.enc` → `pull_latest` 成功并修复);
  - 云配置坏文件 → `CloudConfigCorrupt`,而不是「先填 Endpoint」;SK 用别的钥封 → `CloudConfigUnreadable`,而不是 `BadPack`;
  - 云表单保存→读回,SK/口令解得开;空串沿用;桌面专用字段保存后原样保留;
  - **「不回写」验收(三条机械检查,D4)**:
    1. `dependency_direction.rs`:ffi 的 cloud 依赖声明 `default-features = false`,且
       `cargo tree -p mullion-ffi -e features --target all` 不出现 `mullion-cloud feature "write"`
       (`-e features` 的行形状不同,现有 `has()` 前缀匹配要改;**两头变异都跑**:打开 `write` 必红、关着必绿,防恒绿);
    2. `no_write_back.rs` 字面守护保留;
    3. 孤立构建:`cargo check -p mullion-cloud --no-default-features` 与 `cargo clippy -p mullion-ffi --all-targets -- -D warnings`
       写进 `android/scripts/e2e.sh` 开头(即「Android 绿」的一部分)—— 关掉 `write` 后的配置在 `--workspace` 下从来不被编到,
       dead code / unused import 只会在这里冒出来。
- **Android**:`examples/fake_s3_pack.rs` 宿主机起在 `127.0.0.1:9000` 回放演示包,`e2e.sh` 一并启动;
  插桩测试填 `http://10.0.2.2:9000`、`path_style = true`(单 IP 无法虚拟主机寻址)、固定 bucket;
  流程:填表单 → 从云端更新 → 成功摘要;口令错可重输。证明 ureq/rustls/ring 栈在 Android 上真跑得通。
- **计划第一步**:`cargo ndk` 交叉编 ring/rustls 的探针。编不过就停下来问。

## 7. 人工待验

- 真机连真实 bucket(R2 / MinIO 等)拉取桌面上传的备份;
- 手机时钟偏差时的提示;
- SOCKS5 字段在手机上的实际可用性;
- 高延迟链路 + 大包(图标内嵌在 `sessions.toml`)下 60s 全局超时够不够。

已知取舍:「从云端更新」先保存表单,填错的 SK 会被存下并显示「已设置」—— 由 403 文案提示「已保存,请重填」兜住。

## 8. 不做

- 自动拉、增量/去重(同一份重拉照样整份替换)、选历史版本、任何写云端操作、桌面改动。
