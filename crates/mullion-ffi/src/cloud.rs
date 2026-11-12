//! F346-b:手机从云端拉包。判据在 store(`cloud::latest_key` / `open_cloud_object` /
//! `install_projects_only`),这里只做 IO 编排与类型转换。
//! **只读**:mullion-cloud 以 `default-features = false` 依赖,没有 PUT(设计 D4)。

use std::path::Path;

use mullion_cloud::s3::{Credentials, S3Client};
use mullion_cloud::url::Endpoint;
use mullion_store::cloud::{
    self, is_corrupt, latest_key, passphrase_with, secret_key_with, set_passphrase_with,
    set_secret_key_with,
};
use mullion_store::{install_projects_only, open_cloud_object, InMemoryKey};

use crate::error::FfiError;
use crate::import::ImportSummary;

/// 云端对象的上限,与 SAF 导入同(`ImportViewModel.MAX_PACK_BYTES`)。
const MAX_OBJECT_BYTES: u64 = 16 * 1024 * 1024;

/// 手机表单。`secret_key` / `passphrase`:写入时空串 = 沿用已存;读出时**恒为空**
/// (不回传明文),是否已设置看 `has_*`。
#[derive(Clone, uniffi::Record)]
pub struct CloudForm {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub prefix: String,
    pub path_style: bool,
    pub socks5: String,
    pub access_key_id: String,
    pub secret_key: String,
    pub passphrase: String,
    pub has_secret: bool,
    pub has_passphrase: bool,
    pub is_corrupt: bool,
}

// 手写 Debug:SK/口令明文绝不进日志,只显示是否非空。
impl std::fmt::Debug for CloudForm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudForm")
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .field("path_style", &self.path_style)
            .field("socks5", &"<redacted>")
            .field("access_key_id", &"<redacted>")
            .field("secret_key", &"<redacted>")
            .field("passphrase", &"<redacted>")
            .field("has_secret", &self.has_secret)
            .field("has_passphrase", &self.has_passphrase)
            .field("is_corrupt", &self.is_corrupt)
            .finish()
    }
}

#[derive(Debug, uniffi::Record)]
pub struct PullSummary {
    /// 拉到的那份的对象键(界面显示序号与时间戳)。
    pub key: String,
    pub import: ImportSummary,
}

fn data_key(v: &[u8]) -> Result<InMemoryKey, FfiError> {
    let k: [u8; 32] = v
        .try_into()
        .map_err(|_| FfiError::Internal(format!("数据密钥应为 32 字节,实为 {}", v.len())))?;
    Ok(InMemoryKey(k))
}

#[uniffi::export]
pub fn load_cloud_form(cloud_dir: String) -> CloudForm {
    let dir = Path::new(&cloud_dir);
    let corrupt = is_corrupt(dir);
    let cfg = cloud::load(dir);
    CloudForm {
        endpoint: cfg.endpoint.clone(),
        region: cfg.region.clone(),
        bucket: cfg.bucket.clone(),
        prefix: cfg.prefix.clone(),
        path_style: cfg.path_style,
        socks5: cfg.socks5.clone(),
        access_key_id: cfg.access_key_id.clone(),
        secret_key: String::new(),
        passphrase: String::new(),
        has_secret: !cfg.secret_sealed.is_empty(),
        has_passphrase: cloud::has_passphrase(&cfg),
        is_corrupt: corrupt,
    }
}

/// 读-改-写:只改手机管的字段,桌面专用字段原样保留。
/// 调用方须串行化(Kotlin 侧 busy 闸):这里是无锁的读-改-写。
#[uniffi::export]
pub fn save_cloud_form(
    cloud_dir: String,
    data_key: Vec<u8>,
    form: CloudForm,
) -> Result<(), FfiError> {
    let dir = Path::new(&cloud_dir);
    if is_corrupt(dir) {
        return Err(FfiError::CloudConfigCorrupt);
    }
    // SOCKS5 地址里的 `user:pass@` 会被明文存进配置并进日志:一律拒存。
    if form.socks5.contains('@') {
        return Err(FfiError::Cloud("SOCKS5 地址不能带用户名口令".into()));
    }
    let key = self::data_key(&data_key)?;
    let mut cfg = cloud::load(dir);
    let new_ak = form.access_key_id.trim();
    // 换了 AK 却没给 SK:沿用旧 SK 必然错配,拒绝且不写盘(首次填写不算「换」)。
    if new_ak != cfg.access_key_id
        && form.secret_key.trim().is_empty()
        && !cfg.secret_sealed.is_empty()
    {
        return Err(FfiError::CloudIncomplete("SK(换了 AK 须重填)".into()));
    }
    cfg.endpoint = form.endpoint.trim().to_string();
    cfg.region = form.region.trim().to_string();
    cfg.bucket = form.bucket.trim().to_string();
    cfg.prefix = form.prefix.trim().to_string();
    cfg.path_style = form.path_style;
    cfg.socks5 = form.socks5.trim().to_string();
    cfg.access_key_id = form.access_key_id.trim().to_string();
    let sk = form.secret_key.trim();
    if !sk.is_empty() {
        set_secret_key_with(&mut cfg, &key, sk)?;
    }
    // 口令不 trim:空格可能就是口令的一部分。
    if !form.passphrase.is_empty() {
        set_passphrase_with(&mut cfg, &key, &form.passphrase)?;
    }
    std::fs::create_dir_all(dir).map_err(|e| FfiError::Storage(e.to_string()))?;
    cloud::save(dir, &cfg)?;
    Ok(())
}

#[uniffi::export]
pub fn reset_cloud_config(cloud_dir: String) -> Result<(), FfiError> {
    match std::fs::remove_file(Path::new(&cloud_dir).join(mullion_store::CLOUD_FILE)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(FfiError::Storage(e.to_string())),
    }
}

/// 拉最新一份并整份替换 `vault_dir`。**全程不打开 `vault_dir`**(只在
/// `install_projects_only` 的 staging 里开),坏库也能被修好(设计 D3)。
/// `now_amz_date` = Kotlin 侧按 UTC 格式化的 `YYYYMMDDTHHMMSSZ`(设计 D7)。
/// 注意:下面 `map_err(|_| CloudConfigUnreadable)` 的前提是 key source 为
/// `InMemoryKey`(恒 Ok,失败只可能是密文用别的钥封的);换成会自己失败的
/// key source 时这里会误报。
/// 阻塞(网络 + Argon2 + IO),Kotlin 侧放 IO 线程。
#[uniffi::export]
pub fn pull_latest(
    vault_dir: String,
    cloud_dir: String,
    data_key: Vec<u8>,
    now_amz_date: String,
) -> Result<PullSummary, FfiError> {
    let dir = Path::new(&cloud_dir);
    if is_corrupt(dir) {
        return Err(FfiError::CloudConfigCorrupt);
    }
    let key = self::data_key(&data_key)?;
    let cfg = cloud::load(dir);
    // 解不开 ≠ 包坏:不走 `?`,否则落到 `From<StoreError>` 的 BadPack(设计 R2)。
    let sk = secret_key_with(&cfg, &key).map_err(|_| FfiError::CloudConfigUnreadable)?;
    let pass = passphrase_with(&cfg, &key).map_err(|_| FfiError::CloudConfigUnreadable)?;

    // Region 为空时 SigV4 的签名范围里 region 段为空,任何真实 S3 都会 403,
    // 且会被误报成「AK/SK 不对」,所以这里当必填(桌面无此检查也无默认值)。
    let missing: Vec<&str> = [
        ("Endpoint", cfg.endpoint.is_empty()),
        ("Region", cfg.region.is_empty()),
        ("Bucket", cfg.bucket.is_empty()),
        ("AK", cfg.access_key_id.is_empty()),
        ("SK", sk.is_empty()),
        ("备份口令", pass.is_empty()),
    ]
    .into_iter()
    .filter_map(|(name, empty)| empty.then_some(name))
    .collect();
    if !missing.is_empty() {
        return Err(FfiError::CloudIncomplete(missing.join("、")));
    }

    let socks5 = (!cfg.socks5.is_empty()).then_some(cfg.socks5.as_str());
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
    )?;
    let keys = client.list_keys(&cfg.prefix, &now_amz_date)?;
    let Some(latest) = latest_key(&cfg.prefix, &keys) else {
        return Err(if keys.is_empty() {
            FfiError::CloudNoBackup(cfg.prefix.clone())
        } else {
            FfiError::CloudNoRecognizedBackup {
                count: keys.len() as u32,
                prefix: cfg.prefix.clone(),
            }
        });
    };
    let blob = match client.get(&latest, &now_amz_date, MAX_OBJECT_BYTES) {
        Err(mullion_cloud::CloudError::Status { code: 404, .. }) => {
            return Err(FfiError::CloudBackupVanished)
        }
        r => r?,
    };
    let text = open_cloud_object(&blob, &pass)?;
    let r = install_projects_only(Path::new(&vault_dir), &text, &pass, &key)?;
    Ok(PullSummary {
        key: latest,
        import: r.into(),
    })
}
