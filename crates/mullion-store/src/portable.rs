//! F46-a:**整机迁移包** —— 把这台机器上的全部配置打成一个自包含文件,
//! 拿到新电脑上解开。零 UI、零 async、纯同步 IO(架构不变量)。
//!
//! ```text
//! 导出:配置目录 ──collect──> 文件表 ─┐
//!       secrets 明文 ──seal_secrets──┴─write_pack──> 一份 TOML 文本
//! 导入:TOML ──read_pack──> Pack ──open_secrets──> secrets 明文
//!                            │        └──reseal_for_target(目标机密钥)──┐
//!                            └────────────install(备份 + 写入)──────────┘
//! ```
//!
//! # 为什么密文要拆开重封,而不是把 `secrets.enc` 原样拷过去
//!
//! `secrets.enc` 的密钥在 `Keyring` 方案下来自**这台机器的** OS 钥匙串
//! (见 [`crate::master_key`])。原样拷到新电脑上,那把密钥不在新机器的钥匙串
//! 里 —— 表现是「会话都在,但每一条都要重新输密码」,而且零报错。所以导出时
//! 用一次性口令(Argon2id,复用 `secrets.enc` 自己那套文件头)重新封一遍,
//! 导入时解开、再用**目标机自己的**密钥封回去。
//!
//! 代价:源机如果设了主密码(F71),导入之后新机器上是钥匙串方案 —— 主密码
//! 要在新机器上重新设一次。这是刻意的:我们手上只有迁移包口令,派生源机主
//! 密码的那把密钥无从谈起,而「悄悄换了方案还不说」比「说清楚」糟得多。
//!
//! # 为什么整个包是一份 TOML 而不是 zip
//!
//! zip 是一个新依赖,而这里要装的东西一共就四类文本文件。base64 内嵌之后包
//! 变大约 1/3 —— 配置目录本来就只有几十 KB。换来的是:用户拿记事本就能看见
//! 里头有什么(除了密文),出问题时能自己判断包是不是完整的。
//!
//! # 路径白名单不是洁癖
//!
//! [`entry_target`] 只认三个顶层文件名和 `layouts/<id>.toml`。包是**用户从
//! 别处拿来的文件**,`path = "../../../.bashrc"` 写在里头完全合法;不校验的话
//! 一个迁移包能往任意位置写任意字节。

use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::error::StoreError;
use crate::master_key::MasterKeySource;

/// 本版本写出的包格式版本。
///
/// 读到**更高**的版本一律拒绝([`read_pack`]),不猜着解:包里装的是凭据和
/// 会话,猜错的后果是把用户的整份配置替换成一份残缺的东西,而备份是在替换
/// **之前**做的 —— 猜错的那一刻已经晚了。
pub const CURRENT_PACK_FORMAT: u32 = 1;

/// 建议的扩展名(app 侧的文件对话框用)。
pub const PACK_EXT: &str = "mullionpack";

/// 顶层要带走的文件。**整份替换的单位也是这张表** —— 包里没带的,目标机上
/// 那一份会被删掉(导入的语义是「让新电脑等于旧电脑」,不是「合并」)。
///
/// 日志不在表里:那是这台机器的运行痕迹,拿到新机器上没有意义,而且它是全
/// 目录里唯一会长到几十 MB 的东西。
pub const TOP_LEVEL_FILES: &[&str] = &[
    "sessions.toml",
    crate::settings::SETTINGS_FILE,
    "known_hosts.toml",
];

/// 包里的一个文件条目。正文一律 base64:TOML 的多行字符串对内容有要求
/// (`"""` 出现、控制字符),而 `layouts/*.toml` 里存的是用户输入过的标题。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackFile {
    /// 相对配置目录的路径,正斜杠分隔。**导入时要过 [`entry_target`]**。
    pub path: String,
    /// 文件正文的 base64。
    pub body: String,
}

/// 一个迁移包。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pack {
    pub format_version: u32,
    /// 导出这份包的客户端版本。只给人看,不参与任何判断。
    #[serde(default)]
    pub app_version: String,
    /// 导出时刻(RFC3339)。同上,只给人看 —— store 不持时钟,由调用方注入。
    #[serde(default)]
    pub exported_at: String,
    /// 重新封装过的 `secrets.enc` 字节的 base64。空 = 源机没有密文
    /// (一条密码都没存过)。
    #[serde(default)]
    pub secrets: String,
    /// 明文文件。`#[serde(default)]` 让「一个文件都没带」也能读。
    #[serde(default)]
    pub file: Vec<PackFile>,
}

/// 从配置目录读出要带走的明文文件。**不含 `secrets.enc`** —— 那条走
/// [`seal_secrets`],理由见模块文档。
///
/// 不存在的文件直接跳过(新装的机器可能一个 `known_hosts.toml` 都还没有),
/// 读不出来的也跳过:导出是尽力而为,为一个坏掉的布局记录把整次导出弄失败,
/// 用户拿不到的是全部会话和凭据。
///
/// `layouts/` 只带 `.toml`,**不带 `.alive`**:心跳文件的含义是「这个实例此刻
/// 正开着」。跟着包走到新电脑上,那几条现场会被判成「别人正在用」而永远不
/// 出现在恢复列表里 —— 带着走反而等于没带。
pub fn collect(dir: &Path) -> Vec<PackFile> {
    let mut out = Vec::new();
    for name in TOP_LEVEL_FILES {
        if let Ok(bytes) = std::fs::read(dir.join(name)) {
            out.push(PackFile {
                path: (*name).to_string(),
                body: b64().encode(bytes),
            });
        }
    }
    let mut records: Vec<PathBuf> = match std::fs::read_dir(crate::history::history_dir(dir)) {
        Ok(rd) => rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "toml"))
            .collect(),
        Err(_) => Vec::new(),
    };
    // 读目录的顺序是文件系统说了算。排一下,同一个目录导出两次字节相同 ——
    // 用户想 diff 两份包的时候这一点值钱。
    records.sort();
    for p in records {
        let (Some(name), Ok(bytes)) = (p.file_name().and_then(|n| n.to_str()), std::fs::read(&p))
        else {
            continue;
        };
        out.push(PackFile {
            path: format!("{}/{name}", crate::history::HISTORY_DIR),
            body: b64().encode(bytes),
        });
    }
    out
}

/// 把 `secrets.enc` 的**明文载荷**用一次性口令重新封装成一个 `secrets.enc`
/// 格式的字节串。
///
/// 复用 [`crate::secrets_file`] 的文件头而不是另发明一种封装:那套头已经把
/// 盐和 KDF 参数随密文存了(见 `kdf.rs` 顶上那句 —— 参数写死在代码里的话,
/// 哪天调参所有老文件都解不开,而症状是「口令突然不对了」)。包会在用户的
/// U 盘里躺很久,这条性质对它比对本机文件更要紧。
pub fn seal_secrets(plain: &[u8], password: &str) -> Result<Vec<u8>, StoreError> {
    let scheme = crate::secrets_file::Scheme::Argon2id {
        params: crate::kdf::KdfParams::default(),
        salt: crate::kdf::random_salt(),
    };
    let key = scheme_key(&scheme, password)?;
    let payload = crate::crypto::encrypt(&key, plain)?;
    Ok(crate::secrets_file::encode(&scheme, &payload))
}

/// 拿口令解开 [`seal_secrets`] 封出来的字节串,得回明文载荷。
///
/// **`Keyring` 方案一律拒绝**:那意味着包里这段是没加密的(`encode` 在
/// `Keyring` 下是恒等)。硬失败而不是照解 —— 「包里的凭据是明文的」这件事
/// 用户必须知道,静默解开等于替他接受了。
pub fn open_secrets(blob: &[u8], password: &str) -> Result<Vec<u8>, StoreError> {
    let (scheme, payload) = crate::secrets_file::parse(blob)?;
    if !scheme.has_password() {
        return Err(StoreError::CorruptPack(
            "包里的密文没有口令头 —— 这不是 Mullion 导出的包".into(),
        ));
    }
    let key = scheme_key(&scheme, password)?;
    // 解不开**优先解释成口令错**(与 `Vault::open_with` 同一姿态):用户的
    // 下一步动作完全不同(重打一遍 vs 换一份包)。
    crate::crypto::decrypt(&key, payload).map_err(|_| StoreError::WrongPassword)
}

/// 把明文载荷用**目标机自己的**密钥封成这台机器的 `secrets.enc` 字节。
///
/// 写出来的是 `Keyring` 方案(无文件头,与不设主密码时逐字节同构)。源机的
/// 主密码不会跟着过来,理由见模块文档。
pub fn reseal_for_target(
    plain: &[u8],
    key_source: &dyn MasterKeySource,
) -> Result<Vec<u8>, StoreError> {
    let key = key_source.load_or_create()?;
    let payload = crate::crypto::encrypt(&key, plain)?;
    Ok(crate::secrets_file::encode(
        &crate::secrets_file::Scheme::Keyring,
        &payload,
    ))
}

/// 序列化成包文本。`secrets_blob` 来自 [`seal_secrets`];空切片 = 源机没密文。
pub fn write_pack(
    files: Vec<PackFile>,
    secrets_blob: &[u8],
    app_version: &str,
    now_rfc3339: &str,
) -> Result<String, StoreError> {
    let pack = Pack {
        format_version: CURRENT_PACK_FORMAT,
        app_version: app_version.to_string(),
        exported_at: now_rfc3339.to_string(),
        secrets: b64().encode(secrets_blob),
        file: files,
    };
    Ok(toml::to_string_pretty(&pack)?)
}

/// 解析包文本。版本比本客户端新 → [`StoreError::UnsupportedPack`]。
pub fn read_pack(text: &str) -> Result<Pack, StoreError> {
    let pack: Pack = toml::from_str(text)?;
    if pack.format_version > CURRENT_PACK_FORMAT {
        return Err(StoreError::UnsupportedPack(pack.format_version));
    }
    Ok(pack)
}

/// 取出包里的密文字节。空 = 源机一条密码都没存过(**不是错误**)。
pub fn secrets_blob(pack: &Pack) -> Result<Vec<u8>, StoreError> {
    if pack.secrets.is_empty() {
        return Ok(Vec::new());
    }
    b64()
        .decode(&pack.secrets)
        .map_err(|e| StoreError::CorruptPack(format!("密文段不是合法 base64:{e}")))
}

/// 一个包条目该写到哪儿。`None` = 这条不认,**跳过它**。
///
/// 只认两种形状:
/// - [`TOP_LEVEL_FILES`] 里那三个名字;
/// - `layouts/<文件名>.toml`,且文件名只含 [`crate::history::new_instance_id`]
///   能产出的字符(数字和 `-`)。
///
/// 判据是**白名单**而不是「排掉 `..` 和绝对路径」:后者是一张黑名单,而
/// Windows 上的路径花样(`C:foo`、`\\?\`、UNC、反斜杠、尾随空格、大小写)
/// 多到没人列得全。白名单外的东西一律不写,新增文件类型时来这里加一笔。
pub fn entry_target(dir: &Path, path: &str) -> Option<PathBuf> {
    if TOP_LEVEL_FILES.contains(&path) {
        return Some(dir.join(path));
    }
    let name = path.strip_prefix(crate::history::HISTORY_DIR)?;
    let name = name.strip_prefix('/')?;
    let stem = name.strip_suffix(".toml")?;
    if stem.is_empty() || !stem.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        return None;
    }
    Some(crate::history::history_dir(dir).join(name))
}

/// 导入的结果,给 app 报给用户。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// 备份落在哪儿。
    pub backup: PathBuf,
    /// 写进去几个文件。
    pub written: usize,
    /// 跳过了几条(路径不在白名单 / base64 解不开)。**不是零就该说** ——
    /// 「导入成功」但少了几条会话,用户下次开机才发现。
    pub skipped: usize,
}

/// 备份 + 整份替换。
///
/// 顺序是硬的:**先备份、备份失败就整个不动**。备份是用户唯一的退路,而这
/// 一步之后我们会删掉他现在的 `sessions.toml`。
///
/// 备份是**复制**不是移动:配置目录里还躺着本进程正打开着的日志文件
/// (`logx.rs`),Windows 上移动或删除一个打开着的文件会失败 —— 整份 rename
/// 会在那一步挂掉,而此时新文件已经写了一半。复制只碰我们自己列出来的那几个
/// 文件,日志不在其中。
///
/// `layouts/` 是**合并**不是替换(与顶层三个文件不同):目标机上可能正开着
/// 别的 Mullion 窗口,它们的记录文件和心跳就在那个目录里。删掉活着的实例的
/// 记录,它退出时还会再写回来 —— 无害但混乱;删掉它的 `.alive` 则更糟,那个
/// 窗口的现场会当场被判死、出现在别人的恢复列表里。源机的实例 id 是
/// 「毫秒时间戳-pid」,与目标机的撞不上,合并不会互相覆盖。
pub fn install(
    dir: &Path,
    pack: &Pack,
    secrets_blob: &[u8],
    stamp: &str,
) -> Result<Installed, StoreError> {
    let backup = backup(dir, stamp)?;

    let mut written = 0usize;
    let mut skipped = 0usize;
    let mut seen_top: Vec<&str> = Vec::new();
    for entry in &pack.file {
        let (Some(target), Ok(bytes)) = (entry_target(dir, &entry.path), b64().decode(&entry.body))
        else {
            skipped += 1;
            continue;
        };
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::vault::write_atomic(&target, &bytes)?;
        written += 1;
        if let Some(name) = TOP_LEVEL_FILES.iter().find(|n| **n == entry.path) {
            seen_top.push(name);
        }
    }

    // 整份替换的另一半:包里**没带**的顶层文件,目标机上那一份要删掉。
    // 少了这一步,导入的语义就从「让新电脑等于旧电脑」退化成「合并」——
    // 用户以为自己搬过来了一份干净配置,实际上目标机的旧 `known_hosts.toml`
    // 还在里头,而 TOFU 的判据正来自它。
    for name in TOP_LEVEL_FILES {
        if !seen_top.contains(name) {
            let _ = std::fs::remove_file(dir.join(name));
        }
    }

    if secrets_blob.is_empty() {
        // 源机没有密文 —— 目标机那份也得走,否则留下的是一堆与新会话 id
        // 对不上的旧密文(`Vault::open_with` 会把它们当孤儿裁掉,但在那之前
        // 「解得开吗」这个问题是拿目标机的密钥问的,而我们刚把它换掉)。
        let _ = std::fs::remove_file(dir.join("secrets.enc"));
    } else {
        crate::vault::write_atomic(&dir.join("secrets.enc"), secrets_blob)?;
        written += 1;
    }

    Ok(Installed {
        backup,
        written,
        skipped,
    })
}

/// 把当前配置**复制**一份到 `<dir>/backup-<stamp>/`,返回备份目录。
///
/// 范围与 [`collect`] 一致再加上 `secrets.enc`:备份要能还原出导入前的状态,
/// 而密文正是被替换掉的东西里最要命的那份。
pub fn backup(dir: &Path, stamp: &str) -> Result<PathBuf, StoreError> {
    let out = dir.join(format!("backup-{stamp}"));
    std::fs::create_dir_all(&out)?;
    for name in TOP_LEVEL_FILES
        .iter()
        .chain(std::iter::once(&"secrets.enc"))
    {
        if let Ok(bytes) = std::fs::read(dir.join(name)) {
            std::fs::write(out.join(name), bytes)?;
        }
    }
    let src = crate::history::history_dir(dir);
    if let Ok(rd) = std::fs::read_dir(&src) {
        let dst = out.join(crate::history::HISTORY_DIR);
        std::fs::create_dir_all(&dst)?;
        for p in rd.flatten().map(|e| e.path()) {
            let (Some(name), Ok(bytes)) = (p.file_name(), std::fs::read(&p)) else {
                continue;
            };
            std::fs::write(dst.join(name), bytes)?;
        }
    }
    Ok(out)
}

fn b64() -> base64::engine::general_purpose::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// 从方案里的盐和参数派生密钥。`Keyring` 方案到不了这里(两个调用点各自先判)。
fn scheme_key(
    scheme: &crate::secrets_file::Scheme,
    password: &str,
) -> Result<[u8; 32], StoreError> {
    match scheme {
        crate::secrets_file::Scheme::Argon2id { params, salt } => {
            crate::kdf::derive_key(password, salt, *params)
        }
        crate::secrets_file::Scheme::Keyring => Err(StoreError::CorruptPack(
            "密文段没有口令头,派生不出密钥".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn decoded(files: &[PackFile], path: &str) -> Option<String> {
        let f = files.iter().find(|f| f.path == path)?;
        Some(String::from_utf8(b64().decode(&f.body).unwrap()).unwrap())
    }

    /// 口令强度与本模块无关,派生代价却会乘进每一条测试。
    fn sealed_fast(plain: &[u8], password: &str) -> Vec<u8> {
        let scheme = crate::secrets_file::Scheme::Argon2id {
            params: crate::kdf::KdfParams {
                m_cost: 8,
                t_cost: 1,
                p_cost: 1,
            },
            salt: [9u8; crate::kdf::SALT_LEN],
        };
        let key = scheme_key(&scheme, password).unwrap();
        let payload = crate::crypto::encrypt(&key, plain).unwrap();
        crate::secrets_file::encode(&scheme, &payload)
    }

    #[test]
    fn collect_takes_the_four_kinds_and_leaves_the_log_behind() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        write(d, "sessions.toml", "sessions");
        write(d, "settings.toml", "settings");
        write(d, "known_hosts.toml", "hosts");
        write(d, "layouts/1700-9.toml", "layout");
        write(d, "mullion-1700-9.log", "log");
        write(d, "secrets.enc", "cipher");

        let files = collect(d);
        assert_eq!(
            decoded(&files, "sessions.toml").as_deref(),
            Some("sessions")
        );
        assert_eq!(
            decoded(&files, "settings.toml").as_deref(),
            Some("settings")
        );
        assert_eq!(
            decoded(&files, "known_hosts.toml").as_deref(),
            Some("hosts")
        );
        assert_eq!(
            decoded(&files, "layouts/1700-9.toml").as_deref(),
            Some("layout")
        );
        assert!(
            !files.iter().any(|f| f.path.ends_with(".log")),
            "日志跟着包走没有意义,而它是全目录唯一会长到几十 MB 的东西"
        );
        assert!(
            !files.iter().any(|f| f.path == "secrets.enc"),
            "密文必须走重新封装那条路 —— 原样带走的话新机器上一条都解不开"
        );
    }

    /// `.alive` 的含义是「这个实例此刻正开着」。跟到新电脑上,那几条现场会被
    /// 判成「别人正在用」而永远不出现在恢复列表里 —— 带着走等于没带。
    #[test]
    fn the_heartbeat_files_do_not_travel() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "layouts/1700-9.toml", "layout");
        write(dir.path(), "layouts/1700-9.alive", "1700000000");
        let files = collect(dir.path());
        assert!(
            !files.iter().any(|f| f.path.ends_with(".alive")),
            "带上心跳 = 到了新机器上那条现场永远判活、永远不出现在恢复列表里"
        );
    }

    #[test]
    fn a_missing_file_is_skipped_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "sessions.toml", "s");
        let files = collect(dir.path());
        assert_eq!(files.len(), 1, "新装的机器上没有 known_hosts.toml 很正常");
    }

    #[test]
    fn secrets_round_trip_through_the_passphrase() {
        let plain = b"[42]\npassword = \"hunter2\"\n";
        let blob = sealed_fast(plain, "pack-pass");
        assert_eq!(open_secrets(&blob, "pack-pass").unwrap(), plain);
    }

    #[test]
    fn a_wrong_passphrase_says_so_instead_of_saying_the_file_is_broken() {
        let blob = sealed_fast(b"x", "right");
        assert!(
            matches!(open_secrets(&blob, "wrong"), Err(StoreError::WrongPassword)),
            "报「密文损坏」的话用户会去找备份,而他只是打错了一个字"
        );
    }

    /// `encode` 在 `Keyring` 方案下是恒等 —— 也就是说这段字节是**明文**。
    /// 静默解开等于替用户接受了「包里的凭据没加密」。
    #[test]
    fn a_pack_whose_secrets_have_no_password_header_is_rejected() {
        let blob = crate::secrets_file::encode(&crate::secrets_file::Scheme::Keyring, b"plain");
        assert!(matches!(
            open_secrets(&blob, "any"),
            Err(StoreError::CorruptPack(_))
        ));
    }

    /// 真走一遍 `seal_secrets`(默认 19 MiB 参数,只跑这一条):它是导出路径上
    /// 唯一的加密动作,而上面几条为了快用的是自己拼的头。盐每次随机 ——
    /// 同一份明文封两次字节必须不同,否则盐没进去。
    #[test]
    fn seal_secrets_really_encrypts_and_salts() {
        let a = seal_secrets(b"same", "pw").unwrap();
        let b = seal_secrets(b"same", "pw").unwrap();
        assert_ne!(a, b, "两次封出同样的字节 = 盐没参与");
        assert_eq!(open_secrets(&a, "pw").unwrap(), b"same");
    }

    #[test]
    fn the_pack_round_trips() {
        let files = vec![PackFile {
            path: "sessions.toml".into(),
            body: b64().encode("hello"),
        }];
        let text = write_pack(files.clone(), b"cipher", "0.1.108", "2026-09-14T00:00:00Z").unwrap();
        let pack = read_pack(&text).unwrap();
        assert_eq!(pack.format_version, CURRENT_PACK_FORMAT);
        assert_eq!(pack.app_version, "0.1.108");
        assert_eq!(pack.file, files);
        assert_eq!(secrets_blob(&pack).unwrap(), b"cipher");
    }

    /// 包里装的是凭据和会话,而备份是在替换**之前**做的 —— 猜错格式的那一刻
    /// 已经晚了。
    #[test]
    fn a_pack_from_a_newer_version_is_refused_not_guessed_at() {
        let text = format!("format_version = {}\n", CURRENT_PACK_FORMAT + 1);
        assert!(matches!(
            read_pack(&text),
            Err(StoreError::UnsupportedPack(_))
        ));
    }

    #[test]
    fn a_pack_that_is_not_toml_is_an_error_not_a_panic() {
        assert!(read_pack("这不是 toml {{{").is_err());
    }

    /// 包是用户从别处拿来的文件。不校验路径的话,一个包能往任意位置写任意字节。
    #[test]
    fn only_whitelisted_paths_get_a_target() {
        let dir = Path::new("/cfg");
        assert_eq!(
            entry_target(dir, "sessions.toml"),
            Some(PathBuf::from("/cfg/sessions.toml"))
        );
        assert_eq!(
            entry_target(dir, "layouts/1700-9.toml"),
            Some(PathBuf::from("/cfg/layouts/1700-9.toml"))
        );
        for bad in [
            "../../../.bashrc",
            "layouts/../../evil.toml",
            "/etc/passwd",
            "layouts/evil.exe",
            "layouts/../sessions.toml",
            "layouts/",
            "layouts/.toml",
            "mullion.log",
            "secrets.enc",
            "Sessions.toml",
            r"layouts\1700-9.toml",
            "layouts/1700-9.toml.exe",
        ] {
            assert_eq!(entry_target(dir, bad), None, "{bad} 不该有落点");
        }
    }

    #[test]
    fn install_writes_the_files_and_the_resealed_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let pack = Pack {
            format_version: 1,
            app_version: "0.1.108".into(),
            exported_at: String::new(),
            secrets: String::new(),
            file: vec![
                PackFile {
                    path: "sessions.toml".into(),
                    body: b64().encode("new sessions"),
                },
                PackFile {
                    path: "layouts/1700-9.toml".into(),
                    body: b64().encode("new layout"),
                },
            ],
        };
        let r = install(d, &pack, b"resealed", "20260914-120000").unwrap();
        assert_eq!(r.skipped, 0);
        assert_eq!(
            std::fs::read_to_string(d.join("sessions.toml")).unwrap(),
            "new sessions"
        );
        assert_eq!(
            std::fs::read_to_string(d.join("layouts/1700-9.toml")).unwrap(),
            "new layout"
        );
        assert_eq!(std::fs::read(d.join("secrets.enc")).unwrap(), b"resealed");
    }

    /// 备份是用户唯一的退路,而 `install` 的下一步就是删掉他现在的
    /// `sessions.toml`。
    #[test]
    fn what_was_there_before_is_copied_out_before_anything_is_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        write(d, "sessions.toml", "old sessions");
        write(d, "known_hosts.toml", "old hosts");
        write(d, "secrets.enc", "old cipher");
        write(d, "layouts/1600-1.toml", "old layout");

        let pack = Pack {
            format_version: 1,
            app_version: String::new(),
            exported_at: String::new(),
            secrets: String::new(),
            file: vec![PackFile {
                path: "sessions.toml".into(),
                body: b64().encode("new sessions"),
            }],
        };
        let r = install(d, &pack, b"new cipher", "stamp").unwrap();

        assert_eq!(
            std::fs::read_to_string(r.backup.join("sessions.toml")).unwrap(),
            "old sessions"
        );
        assert_eq!(
            std::fs::read_to_string(r.backup.join("known_hosts.toml")).unwrap(),
            "old hosts"
        );
        assert_eq!(
            std::fs::read_to_string(r.backup.join("secrets.enc")).unwrap(),
            "old cipher",
            "密文是被替换掉的东西里最要命的那份,备份漏了它等于没有退路"
        );
        assert_eq!(
            std::fs::read_to_string(r.backup.join("layouts/1600-1.toml")).unwrap(),
            "old layout"
        );
    }

    /// 整份替换的另一半。少了这一步,目标机的旧 `known_hosts.toml` 会留在
    /// 里头 —— 而 TOFU 的判据正来自它。
    #[test]
    fn a_top_level_file_the_pack_did_not_bring_is_removed_not_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        write(d, "known_hosts.toml", "old hosts");
        let pack = Pack {
            format_version: 1,
            app_version: String::new(),
            exported_at: String::new(),
            secrets: String::new(),
            file: vec![PackFile {
                path: "sessions.toml".into(),
                body: b64().encode("s"),
            }],
        };
        install(d, &pack, b"c", "stamp").unwrap();
        assert!(
            !d.join("known_hosts.toml").exists(),
            "包里没带就该删 —— 留着的话导入的语义从「等于旧电脑」退化成「合并」"
        );
    }

    /// `layouts/` 反过来:目标机上可能正开着别的窗口,它们的记录和心跳就在
    /// 那个目录里。删掉活着的实例的 `.alive`,那个窗口的现场会当场被判死。
    #[test]
    fn the_history_of_other_windows_survives_the_import() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        write(d, "layouts/1600-1.toml", "another window");
        write(d, "layouts/1600-1.alive", "1700000000");
        let pack = Pack {
            format_version: 1,
            app_version: String::new(),
            exported_at: String::new(),
            secrets: String::new(),
            file: vec![PackFile {
                path: "layouts/1700-9.toml".into(),
                body: b64().encode("mine"),
            }],
        };
        install(d, &pack, b"c", "stamp").unwrap();
        assert_eq!(
            std::fs::read_to_string(d.join("layouts/1600-1.toml")).unwrap(),
            "another window"
        );
        assert!(
            d.join("layouts/1600-1.alive").exists(),
            "删掉别人的心跳 = 那个正开着的窗口当场被判死"
        );
    }

    /// 源机一条密码都没存过 → 目标机那份旧密文也得走。留着的话,里头的键是
    /// 旧会话的 id,而 `sessions.toml` 已经换成新的了。
    #[test]
    fn an_empty_secrets_blob_removes_the_targets_old_cipher() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        write(d, "secrets.enc", "old cipher");
        let pack = Pack {
            format_version: 1,
            app_version: String::new(),
            exported_at: String::new(),
            secrets: String::new(),
            file: Vec::new(),
        };
        install(d, &pack, &[], "stamp").unwrap();
        assert!(!d.join("secrets.enc").exists());
        assert_eq!(
            std::fs::read_to_string(d.join("backup-stamp/secrets.enc")).unwrap(),
            "old cipher",
            "删之前必须已经在备份里"
        );
    }

    /// 「导入成功」但少了几条会话,用户下次开机才发现。
    #[test]
    fn entries_that_cannot_be_placed_are_counted_not_swallowed() {
        let dir = tempfile::tempdir().unwrap();
        let pack = Pack {
            format_version: 1,
            app_version: String::new(),
            exported_at: String::new(),
            secrets: String::new(),
            file: vec![
                PackFile {
                    path: "../evil.toml".into(),
                    body: b64().encode("x"),
                },
                PackFile {
                    path: "sessions.toml".into(),
                    body: "这不是 base64 ***".into(),
                },
            ],
        };
        let r = install(dir.path(), &pack, b"c", "stamp").unwrap();
        assert_eq!(r.skipped, 2);
        assert_eq!(r.written, 1, "只有密文那一份写进去了");
        assert!(!dir.path().parent().unwrap().join("evil.toml").exists());
    }

    /// 端到端:导出一份、在另一个目录里导入,明文文件逐字节相同,密文经
    /// 「口令封 → 口令解 → 目标机密钥封」之后还能用目标机的密钥解回来。
    #[test]
    fn a_pack_travels_from_one_machine_to_another() {
        let src = tempfile::tempdir().unwrap();
        write(src.path(), "sessions.toml", "会话库正文");
        write(src.path(), "layouts/1700-9.toml", "现场");

        let plain = b"[7]\npassword = \"hunter2\"\n";
        let text = write_pack(
            collect(src.path()),
            &sealed_fast(plain, "pack-pass"),
            "0.1.108",
            "2026-09-14T00:00:00Z",
        )
        .unwrap();

        let dst = tempfile::tempdir().unwrap();
        let pack = read_pack(&text).unwrap();
        let got = open_secrets(&secrets_blob(&pack).unwrap(), "pack-pass").unwrap();
        assert_eq!(got, plain);

        let target = crate::master_key::InMemoryKey([5u8; 32]);
        let resealed = reseal_for_target(&got, &target).unwrap();
        install(dst.path(), &pack, &resealed, "stamp").unwrap();

        assert_eq!(
            std::fs::read_to_string(dst.path().join("sessions.toml")).unwrap(),
            "会话库正文"
        );
        assert_eq!(
            std::fs::read_to_string(dst.path().join("layouts/1700-9.toml")).unwrap(),
            "现场"
        );
        let on_disk = std::fs::read(dst.path().join("secrets.enc")).unwrap();
        let (scheme, payload) = crate::secrets_file::parse(&on_disk).unwrap();
        assert_eq!(
            scheme,
            crate::secrets_file::Scheme::Keyring,
            "落到目标机上的必须是这台机器的方案 —— 带着源机的口令头过去,\
             下次开机要的是一个用户根本不知道自己设过的密码"
        );
        assert_eq!(
            crate::crypto::decrypt(&target.load_or_create().unwrap(), payload).unwrap(),
            plain
        );
    }
}
