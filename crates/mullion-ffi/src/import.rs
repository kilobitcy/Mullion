//! F346-a:手机导入 `.mullionpack`。判据全在 `mullion_store::phone_import`,
//! 这里只做类型转换。**本模块(以及整个 ffi)不得出现任何写出包 / 推云的调用**,
//! 守护见 `tests/no_write_back.rs` 与 `tests/dependency_direction.rs`。

use std::path::Path;

use mullion_store::{install_projects_only, read_pack, InMemoryKey};

use crate::error::FfiError;

/// 选完文件先看一眼:要不要弹口令框。
#[derive(Debug, uniffi::Record)]
pub struct PackInfo {
    pub has_secrets: bool,
}

/// 见 `mullion_store::PhoneImport`。
#[derive(Debug, uniffi::Record)]
pub struct ImportSummary {
    pub projects: Vec<String>,
    pub kept: u32,
    pub dropped: u32,
    pub broken_nodes: u32,
    pub unrecognized: u32,
}

/// 只读:解析包头,不碰任何文件。
#[uniffi::export]
pub fn inspect_pack(pack_text: String) -> Result<PackInfo, FfiError> {
    let pack = read_pack(&pack_text)?;
    Ok(PackInfo {
        has_secrets: !pack.secrets.is_empty(),
    })
}

/// 整份替换 `dir`。`data_key` = Keystore 信封解出的 32 字节数据密钥;
/// 长度不对直接报错,**不碰 `dir`**。耗时(Argon2 + IO),Kotlin 侧放 IO 线程。
#[uniffi::export]
pub fn import_pack(
    dir: String,
    pack_text: String,
    password: String,
    data_key: Vec<u8>,
) -> Result<ImportSummary, FfiError> {
    let key: [u8; 32] = data_key
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Internal(format!("数据密钥应为 32 字节,实为 {}", data_key.len())))?;
    let r = install_projects_only(Path::new(&dir), &pack_text, &password, &InMemoryKey(key))?;
    Ok(ImportSummary {
        projects: r.projects,
        kept: r.kept,
        dropped: r.dropped,
        broken_nodes: r.broken_nodes,
        unrecognized: r.unrecognized,
    })
}
