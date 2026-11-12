//! F346-a:手机导入 `.mullionpack` —— 元数据整份,密文只留**项目可达**的那部分,
//! 用手机自己的数据密钥重封。零 UI、零 async、纯同步 IO(架构不变量)。
//!
//! 与桌面 [`crate::portable::install`] 的区别:
//! - 不备份:手机是只读消费者,真源在桌面,重导一次即恢复(设计 D4);
//! - 不写 `settings.toml`、`layouts/`(设计 D2);
//! - 未裁的全量密文**从不落盘**:裁剪在内存里做(设计 §3)。

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use base64::Engine as _;

use crate::credential::Auth;
use crate::error::StoreError;
use crate::group::GroupRecord;
use crate::master_key::MasterKeySource;
use crate::model::{SecretEntry, SessionId, SessionRecord};
use crate::project::ProjectRecord;
use crate::vault::{cred_key, write_atomic, Vault};

/// 项目可达的密文键集合。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Reachable {
    /// `secrets.enc` 里要保留的键:会话 id 的十进制串,以及 `cred:<id>`。
    pub keys: BTreeSet<String>,
    /// 跳板链展开失败的节点数(按节点去重)。**非零必须报给用户**(设计 D8):
    /// 「桌面能连、手机说跳板悬空」要事先看得见。
    pub broken_nodes: u32,
}

/// 对**每个项目(含归档,设计 D7)**的每个节点,收集「节点自身 + 整条跳板链」
/// 的会话,取它们的密文键和它们 `Auth::Ref` 引用的 `cred:` 键。
///
/// 展开失败的节点只留它自己(设计 D8):`expand_chain` 全有或全无,
/// 不能让一条坏配置拖垮整次导入。
pub fn project_reachable_secret_keys(
    sessions: &[SessionRecord],
    groups: &[GroupRecord],
    projects: &[ProjectRecord],
) -> Reachable {
    let by_id: BTreeMap<SessionId, SessionRecord> =
        sessions.iter().map(|s| (s.id, s.clone())).collect();
    let groups: BTreeMap<_, _> = groups.iter().map(|g| (g.id, g.clone())).collect();

    let mut ids = BTreeSet::new();
    let mut broken = BTreeSet::new();
    for node in projects.iter().flat_map(|p| &p.nodes) {
        ids.insert(*node);
        match crate::jump::expand_chain(*node, &by_id, &groups) {
            Ok(chain) => ids.extend(chain),
            Err(_) => {
                broken.insert(*node);
            }
        }
    }

    let mut keys = BTreeSet::new();
    for id in ids {
        // 项目引用了不存在的会话:没有密文可留(展开那步已计入 broken)。
        let Some(rec) = by_id.get(&id) else { continue };
        keys.insert(id.0.to_string());
        if let Auth::Ref(c) = &rec.auth {
            keys.insert(cred_key(*c));
        }
    }
    Reachable {
        keys,
        broken_nodes: broken.len() as u32,
    }
}

/// 暂存目录名。以 `.` 开头,与任何白名单文件都撞不上。
pub const STAGING_DIR: &str = ".import-staging";

/// 手机上落盘的明文文件。`settings.toml`、`layouts/` 按设计不写(D2)。
const PHONE_FILES: &[&str] = &["sessions.toml", "known_hosts.toml"];
const SECRETS_FILE: &str = "secrets.enc";

/// 一次导入的结果,给手机界面报给用户。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneImport {
    /// 导入后的项目名(含归档),按 `sessions.toml` 里的顺序。
    pub projects: Vec<String>,
    /// 保留 / 丢弃的密文条目数。
    pub kept: u32,
    pub dropped: u32,
    /// 见 [`Reachable::broken_nodes`]。
    pub broken_nodes: u32,
    /// **只计**白名单外 / base64 坏掉的条目。`settings.toml`、`layouts/*` 是
    /// 「按设计不写」,不计 —— 否则每份桌面包都报「跳过 N 条」,用户会以为丢了会话。
    pub unrecognized: u32,
}

/// 整份替换 `dir`:元数据整份、密文只留项目可达条目,用 `key_source` 重封。
///
/// 失败(口令错 / 包损坏 / 版本过新)时 `dir` 原样不动:一切先落在
/// `dir/.import-staging/`,全部成功后才逐个 rename 进 `dir`。
pub fn install_projects_only(
    dir: &Path,
    pack_text: &str,
    password: &str,
    key_source: &dyn MasterKeySource,
) -> Result<PhoneImport, StoreError> {
    let staging = dir.join(STAGING_DIR);
    let result = stage(&staging, pack_text, password, key_source).and_then(|summary| {
        commit(dir, &staging)?;
        Ok(summary)
    });
    let _ = std::fs::remove_dir_all(&staging);
    result
}

/// F346-b:解开云端对象的外层(备份口令),得到 `.mullionpack` 文本,交给
/// [`install_projects_only`](内层再用同一口令解)。
///
/// 云端对象与 SAF 选来的文件同等不可信(设计 D6):外层头里的 Argon2 参数先夹紧再派生。
pub fn open_cloud_object(blob: &[u8], passphrase: &str) -> Result<String, StoreError> {
    check_kdf_params(blob)?;
    let plain = crate::portable::open_secrets(blob, passphrase)?;
    // 不回显内容:解出来的若是别的东西,里面可能是任何明文。
    String::from_utf8(plain).map_err(|_| StoreError::CorruptPack("云端对象解开后不是文本".into()))
}

/// 包头里 Argon2 代价参数的上限。参数由**包自己声明**,恶意包可以写个 4 TiB
/// 让手机 OOM / 卡死,所以派生之前先夹紧。
///
/// 依据:桌面导出只写 `KdfParams::default()`(19 MiB / t=2 / p=1,见 `kdf.rs`)。
/// 上限取默认的宽松倍数:内存 64 MiB(约 3.4 倍,远低于 256 MiB 的硬顶,
/// 中低端手机也扛得住)、迭代 10(5 倍)、并行 4(4 倍)。将来桌面调高默认值时,
/// 手机端须同步放宽这里,否则新包会被拒收。
const MAX_KDF_M_COST_KIB: u32 = 64 * 1024;
const MAX_KDF_T_COST: u32 = 10;
const MAX_KDF_P_COST: u32 = 4;

/// 手机端能接的 `sessions.toml` 最低 schema。v<5 的迁移会读文件里记的本机私钥路径
/// (`Vault::open` → `legacy_key_paths`),在手机上无意义且可被拿来读任意文件。
const MIN_PHONE_SCHEMA: u32 = 5;

/// 只看密文头,不派生:Argon2 参数越界 → `CorruptPack`。
fn check_kdf_params(blob: &[u8]) -> Result<(), StoreError> {
    if let (crate::secrets_file::Scheme::Argon2id { params, .. }, _) =
        crate::secrets_file::parse(blob)?
    {
        if params.m_cost > MAX_KDF_M_COST_KIB
            || params.t_cost > MAX_KDF_T_COST
            || params.p_cost > MAX_KDF_P_COST
        {
            return Err(StoreError::CorruptPack(
                "包里声明的口令派生参数超出手机端上限,拒绝导入".into(),
            ));
        }
    }
    Ok(())
}

/// 在内存里探包内 `sessions.toml` 的 schema,< [`MIN_PHONE_SCHEMA`] → `PackTooOld`。
/// 探的必须是**最终落进 staging 的那份字节**:与 staging 循环同一规则 —— 逐条解码,
/// 解码失败的跳过,后写覆盖先写。只看最后一条会被「前旧后坏」绕过(坏的那条被跳过,
/// 旧的那条留在 staging 里照样触发迁移)。探不出版本一律拒收,不放给 `Vault::open`。
fn check_schema_floor(pack: &crate::portable::Pack) -> Result<(), StoreError> {
    let Some(bytes) = pack
        .file
        .iter()
        .filter(|e| e.path == "sessions.toml")
        .filter_map(|e| crate::portable::b64().decode(&e.body).ok())
        .next_back()
    else {
        return Ok(());
    };
    let probe = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|t| toml::from_str::<crate::migrate::SchemaProbe>(t).ok())
        .ok_or_else(|| StoreError::CorruptPack("sessions.toml 探不出 schema 版本".into()))?;
    if probe.schema_version < MIN_PHONE_SCHEMA {
        return Err(StoreError::PackTooOld(probe.schema_version));
    }
    Ok(())
}

/// 第 1~4 步:一切只落在 staging。
fn stage(
    staging: &Path,
    pack_text: &str,
    password: &str,
    key_source: &dyn MasterKeySource,
) -> Result<PhoneImport, StoreError> {
    // 纯内存步骤(解析包 / 验口令 / 解密密文)全部排在碰文件系统之前:
    // 这些步骤失败时 `dir` 连目录本身都不该被创建(设计 §3 第 6 条)。
    let pack = crate::portable::read_pack(pack_text)?;
    check_schema_floor(&pack)?;
    // 空 blob 不能交给 open_secrets:它会判成「没有口令头」(设计 C1)。
    let blob = crate::portable::secrets_blob(&pack)?;
    let mut secrets: BTreeMap<String, SecretEntry> = if blob.is_empty() {
        BTreeMap::new()
    } else {
        check_kdf_params(&blob)?;
        let plain = crate::portable::open_secrets(&blob, password)?;
        // 不能原样上抛 toml 错误:它的 Display 带出错行,那行可能就是 `password = "..."`。
        toml::from_str(&String::from_utf8(plain)?).map_err(|_| {
            StoreError::CorruptSecrets("解密后的内容不是合法 TOML(为防泄露不附原文)".into())
        })?
    };

    // 上次崩溃的残留整个清掉,里面的东西一概不信。
    // 忽略 NotFound 等错误,但删完仍在就不能往下走:残留会被当成本次结果 commit。
    let _ = std::fs::remove_dir_all(staging);
    if staging.exists() {
        return Err(StoreError::Io(format!(
            "上次导入的暂存目录清不掉:{}",
            staging.display()
        )));
    }
    std::fs::create_dir_all(staging)?;

    let mut unrecognized = 0u32;
    for entry in &pack.file {
        if crate::portable::entry_target(staging, &entry.path).is_none() {
            unrecognized += 1;
            continue;
        }
        if !PHONE_FILES.contains(&entry.path.as_str()) {
            continue; // settings.toml / layouts/*:按设计不写,也不计
        }
        match crate::portable::b64().decode(&entry.body) {
            Ok(bytes) => write_atomic(&staging.join(&entry.path), &bytes)?,
            Err(_) => unrecognized += 1,
        }
    }

    // 只读元数据:staging 里还没有 secrets.enc,Vault 拿到的是空密文表。
    // schema 偏旧会在这里迁移并写回 staging(连同 .bak),提交时只搬白名单。
    let (reach, projects) = {
        let vault = Vault::open(staging.to_path_buf(), key_source)?;
        let reach = project_reachable_secret_keys(vault.list(), vault.groups(), vault.projects());
        let projects = vault.projects().iter().map(|p| p.name.clone()).collect();
        (reach, projects)
    };

    let total = secrets.len();
    secrets.retain(|k, _| reach.keys.contains(k));
    let kept = secrets.len();

    let plain = toml::to_string_pretty(&secrets)?;
    let sealed = crate::portable::reseal_for_target(plain.as_bytes(), key_source)?;
    write_atomic(&staging.join(SECRETS_FILE), &sealed)?;

    Ok(PhoneImport {
        projects,
        kept: kept as u32,
        dropped: (total - kept) as u32,
        broken_nodes: reach.broken_nodes,
        unrecognized,
    })
}

/// 第 5 步:逐个 rename 进 `dir`;包里没带的在 `dir` 里删掉(整份替换)。
///
/// 顺序 sessions 在前、secrets 在后。中途崩溃留下「新 sessions + 旧 secrets」,
/// 二者同一把数据钥,`Vault::open` 的孤儿裁剪会容忍;重导即恢复(设计 §3)。
fn commit(dir: &Path, staging: &Path) -> Result<(), StoreError> {
    for name in PHONE_FILES.iter().chain(std::iter::once(&SECRETS_FILE)) {
        let src = staging.join(name);
        if src.exists() {
            std::fs::rename(&src, dir.join(name))?;
        } else {
            let _ = std::fs::remove_file(dir.join(name));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::CredentialId;
    use crate::model::{
        AppearancePrefs, AuthKind, Connection, GroupId, Identity, Protocol, TerminalPrefs,
    };
    use crate::network::{JumpRef, NetworkPrefs};
    use crate::project::ProjectId;

    /// `jump`:`None` = 未设置(走分组继承);`Some(&[])` = 显式不走跳板。
    fn session(id: u64, jump: Option<&[u64]>, auth: Auth) -> SessionRecord {
        SessionRecord {
            id: SessionId(id),
            modified_at: "2026-11-12T00:00:00Z".into(),
            identity: Identity {
                name: format!("s{id}"),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: format!("node{id}.example"),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth,
            terminal: TerminalPrefs::default(),
            appearance: AppearancePrefs::default(),
            network: NetworkPrefs {
                proxy: None,
                jump: jump.map(|j| j.iter().map(|i| JumpRef(SessionId(*i))).collect()),
            },
            automation: crate::automation::AutomationPrefs::default(),
            sftp: crate::sftp::SftpPrefs::default(),
            last_connected_at: None,
        }
    }

    fn inline() -> Auth {
        Auth::inline("u", AuthKind::Password)
    }

    fn group(id: u64, jump: &[u64]) -> GroupRecord {
        let mut g = GroupRecord {
            id: GroupId(id),
            name: format!("g{id}"),
            tags: Vec::new(),
            terminal: TerminalPrefs::default(),
            appearance: AppearancePrefs::default(),
            network: NetworkPrefs::default(),
            automation: crate::automation::AutomationPrefs::default(),
        };
        g.network.jump = Some(jump.iter().map(|i| JumpRef(SessionId(*i))).collect());
        g
    }

    fn project(id: u64, name: &str, nodes: &[u64]) -> ProjectRecord {
        ProjectRecord {
            id: ProjectId(id),
            name: name.into(),
            note: String::new(),
            nodes: nodes.iter().map(|n| SessionId(*n)).collect(),
            preferred: None,
            dir: "/srv/app".into(),
            tmux_name: None,
            created_at: "2026-11-12T00:00:00Z".into(),
            last_accessed_at: None,
            archived_at: None,
            memos: Vec::new(),
            icon: None,
        }
    }

    fn keys(r: &Reachable) -> Vec<&str> {
        r.keys.iter().map(String::as_str).collect()
    }

    #[test]
    fn a_project_node_keeps_its_own_secret_and_nothing_else() {
        let s = [
            session(1, Some(&[]), inline()),
            session(2, Some(&[]), inline()),
        ];
        let r = project_reachable_secret_keys(&s, &[], &[project(1, "p", &[1])]);
        assert_eq!(keys(&r), ["1"]);
        assert_eq!(r.broken_nodes, 0);
    }

    #[test]
    fn a_jump_inherited_from_the_group_is_kept() {
        let mut node = session(1, None, inline());
        node.identity.group_id = Some(GroupId(7));
        let s = [node, session(3, Some(&[]), inline())];
        let r = project_reachable_secret_keys(&s, &[group(7, &[3])], &[project(1, "p", &[1])]);
        assert_eq!(keys(&r), ["1", "3"], "分组继承来的跳板 3 必须可达");
    }

    #[test]
    fn the_credential_referenced_by_a_jump_session_is_kept() {
        let s = [
            session(1, Some(&[3]), inline()),
            session(3, Some(&[]), Auth::Ref(CredentialId(5))),
        ];
        let r = project_reachable_secret_keys(&s, &[], &[project(1, "p", &[1])]);
        assert_eq!(keys(&r), ["1", "3", "cred:5"]);
    }

    #[test]
    fn nodes_of_an_archived_project_are_kept() {
        let mut p = project(1, "p", &[1]);
        p.archived_at = Some("2026-11-01T00:00:00Z".into());
        let r = project_reachable_secret_keys(&[session(1, Some(&[]), inline())], &[], &[p]);
        assert_eq!(keys(&r), ["1"], "设计 D7:手机能打开归档项目,其凭据必须在");
    }

    #[test]
    fn unrelated_sessions_and_unused_credentials_are_dropped() {
        let s = [
            session(1, Some(&[]), inline()),
            session(4, Some(&[]), Auth::Ref(CredentialId(6))),
        ];
        let r = project_reachable_secret_keys(&s, &[], &[project(1, "p", &[1])]);
        assert_eq!(keys(&r), ["1"], "会话 4 与它引用的 cred:6 都不在任何项目里");
    }

    #[test]
    fn a_jump_used_only_by_non_project_sessions_is_dropped() {
        let s = [
            session(1, Some(&[]), inline()),
            session(3, Some(&[]), inline()),
            session(4, Some(&[3]), inline()),
        ];
        let r = project_reachable_secret_keys(&s, &[], &[project(1, "p", &[1])]);
        assert_eq!(keys(&r), ["1"]);
    }

    #[test]
    fn a_node_whose_chain_cannot_expand_keeps_only_itself_and_is_counted_once() {
        // 跳板 99 不存在 → expand_chain 失败。同一节点出现在两个项目里只计一次。
        let s = [session(1, Some(&[99]), inline())];
        let ps = [project(1, "p", &[1]), project(2, "q", &[1])];
        let r = project_reachable_secret_keys(&s, &[], &ps);
        assert_eq!(keys(&r), ["1"]);
        assert_eq!(r.broken_nodes, 1);
    }

    use crate::master_key::InMemoryKey;
    use crate::model::{SecretEntry, SessionsFile, CURRENT_SCHEMA};
    use crate::portable::PackFile;

    const PW: &str = "pack-pw";
    const KEY: InMemoryKey = InMemoryKey([7u8; 32]);

    /// 与 `portable` 测试同一手法:低代价 Argon2 参数,免得每条测试跑几百毫秒。
    /// 参数写在文件头里,`open_secrets` 按头里的解,与生产参数无关。
    fn seal_fast(plain: &[u8], password: &str) -> Vec<u8> {
        let params = crate::kdf::KdfParams {
            m_cost: 8,
            t_cost: 1,
            p_cost: 1,
        };
        let salt = [9u8; crate::kdf::SALT_LEN];
        let key = crate::kdf::derive_key(password, &salt, params).unwrap();
        let payload = crate::crypto::encrypt(&key, plain).unwrap();
        crate::secrets_file::encode(
            &crate::secrets_file::Scheme::Argon2id { params, salt },
            &payload,
        )
    }

    fn secrets_toml(keys: &[&str]) -> String {
        let m: BTreeMap<String, SecretEntry> = keys
            .iter()
            .map(|k| {
                let e = SecretEntry {
                    password: Some(format!("fake-{k}")),
                    ..Default::default()
                };
                (k.to_string(), e)
            })
            .collect();
        toml::to_string_pretty(&m).unwrap()
    }

    fn file(sessions: Vec<SessionRecord>, projects: Vec<ProjectRecord>) -> SessionsFile {
        SessionsFile {
            schema_version: CURRENT_SCHEMA,
            group: Vec::new(),
            session: sessions,
            tunnel: Vec::new(),
            credential: Vec::new(),
            project: projects,
        }
    }

    fn pack_with(files: Vec<(String, Vec<u8>)>, secrets_blob: &[u8]) -> String {
        let files = files
            .into_iter()
            .map(|(path, bytes)| PackFile {
                path,
                body: crate::portable::b64().encode(bytes),
            })
            .collect();
        crate::portable::write_pack(files, secrets_blob, "test", "2026-11-12T00:00:00Z").unwrap()
    }

    /// 一份「桌面导出」的包:三个顶层文件 + 一条布局记录 + 密文。
    fn desktop_pack(f: &SessionsFile, secret_keys: &[&str], known_hosts: bool) -> String {
        let mut files = vec![
            (
                "sessions.toml".to_string(),
                toml::to_string_pretty(f).unwrap().into_bytes(),
            ),
            (
                crate::settings::SETTINGS_FILE.to_string(),
                b"# settings\n".to_vec(),
            ),
            (
                format!("{}/1700000000000-42.toml", crate::history::HISTORY_DIR),
                b"# layout\n".to_vec(),
            ),
        ];
        if known_hosts {
            files.push(("known_hosts.toml".to_string(), b"# kh\n".to_vec()));
        }
        pack_with(files, &seal_fast(secrets_toml(secret_keys).as_bytes(), PW))
    }

    /// 项目 1 = 节点 1;会话 4 无关。密文 1、4 → 留 1 丢 4。
    fn demo() -> SessionsFile {
        file(
            vec![
                session(1, Some(&[]), inline()),
                session(4, Some(&[]), inline()),
            ],
            vec![project(1, "演示", &[1])],
        )
    }

    fn names(dir: &Path) -> BTreeSet<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect()
    }

    fn three_files() -> BTreeSet<String> {
        ["known_hosts.toml", "secrets.enc", "sessions.toml"]
            .map(String::from)
            .into()
    }

    fn phone_secret_keys(dir: &Path) -> BTreeSet<String> {
        let v = Vault::open(dir.to_path_buf(), &KEY).unwrap();
        let m: BTreeMap<String, SecretEntry> =
            toml::from_str(&v.secrets_plaintext().unwrap()).unwrap();
        m.into_keys().collect()
    }

    #[test]
    fn imports_only_reachable_secrets_and_writes_only_the_three_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        let r = install_projects_only(&dir, &desktop_pack(&demo(), &["1", "4"], true), PW, &KEY)
            .unwrap();
        assert_eq!(
            r,
            PhoneImport {
                projects: vec!["演示".into()],
                kept: 1,
                dropped: 1,
                broken_nodes: 0,
                unrecognized: 0,
            },
            "settings.toml 与 layouts/ 按设计不写,不计入 unrecognized"
        );
        assert_eq!(
            names(&dir),
            three_files(),
            "无 settings.toml / layouts/ / staging"
        );
        assert_eq!(phone_secret_keys(&dir), ["1".to_string()].into());
        let v = Vault::open(dir.clone(), &KEY).unwrap();
        assert_eq!(v.list().len(), 2, "元数据整份导入(D2):无关会话的元数据也在");
    }

    #[test]
    fn a_pack_without_secrets_imports_with_an_empty_password() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        let text = pack_with(
            vec![(
                "sessions.toml".into(),
                toml::to_string_pretty(&demo()).unwrap().into_bytes(),
            )],
            &[],
        );
        let r = install_projects_only(&dir, &text, "", &KEY).unwrap();
        assert_eq!((r.kept, r.dropped), (0, 0));
        assert!(phone_secret_keys(&dir).is_empty());
    }

    #[test]
    fn a_wrong_password_leaves_the_existing_import_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        let text = desktop_pack(&demo(), &["1", "4"], true);
        install_projects_only(&dir, &text, PW, &KEY).unwrap();
        let before = std::fs::read(dir.join("sessions.toml")).unwrap();

        let err = install_projects_only(&dir, &text, "wrong", &KEY).unwrap_err();
        assert!(matches!(err, StoreError::WrongPassword), "{err:?}");
        assert_eq!(std::fs::read(dir.join("sessions.toml")).unwrap(), before);
        assert_eq!(names(&dir), three_files(), "失败后 staging 也得清掉");
    }

    #[test]
    fn a_newer_pack_format_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let err = install_projects_only(tmp.path(), "format_version = 2\n", PW, &KEY).unwrap_err();
        assert!(matches!(err, StoreError::UnsupportedPack(2)), "{err:?}");
    }

    #[test]
    fn a_newer_schema_is_rejected_and_the_existing_import_is_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        install_projects_only(&dir, &desktop_pack(&demo(), &["1"], true), PW, &KEY).unwrap();
        let before = std::fs::read(dir.join("sessions.toml")).unwrap();

        let text = pack_with(
            vec![("sessions.toml".into(), b"schema_version = 99\n".to_vec())],
            &[],
        );
        let err = install_projects_only(&dir, &text, "", &KEY).unwrap_err();
        assert!(matches!(err, StoreError::UnsupportedSchema(99)), "{err:?}");
        assert_eq!(std::fs::read(dir.join("sessions.toml")).unwrap(), before);
        assert_eq!(names(&dir), three_files());
    }

    #[test]
    fn an_older_schema_pack_is_migrated_without_leaking_the_migration_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        let mut f = demo();
        f.schema_version = CURRENT_SCHEMA - 1;
        let r =
            install_projects_only(&dir, &desktop_pack(&f, &["1", "4"], true), PW, &KEY).unwrap();
        assert_eq!((r.kept, r.dropped), (1, 1));
        assert_eq!(
            names(&dir),
            three_files(),
            "迁移写出的 sessions.toml.bak 不能跟进 dir"
        );
        let text = std::fs::read_to_string(dir.join("sessions.toml")).unwrap();
        assert!(
            text.contains(&format!("schema_version = {CURRENT_SCHEMA}")),
            "手机上落的是迁移后的文件"
        );
    }

    #[test]
    fn only_entries_outside_the_whitelist_are_counted_as_unrecognized() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        let mut pack = crate::portable::read_pack(&desktop_pack(&demo(), &["1"], true)).unwrap();
        pack.file.push(PackFile {
            path: "../evil".into(),
            body: crate::portable::b64().encode(b"x"),
        });
        let text = toml::to_string_pretty(&pack).unwrap();
        let r = install_projects_only(&dir, &text, PW, &KEY).unwrap();
        assert_eq!(r.unrecognized, 1);
        assert!(!tmp.path().join("evil").exists());
    }

    #[test]
    fn a_reimport_replaces_the_whole_set_including_files_the_new_pack_lacks() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        install_projects_only(&dir, &desktop_pack(&demo(), &["1"], true), PW, &KEY).unwrap();
        assert!(dir.join("known_hosts.toml").exists());

        install_projects_only(&dir, &desktop_pack(&demo(), &["1"], false), PW, &KEY).unwrap();
        assert!(
            !dir.join("known_hosts.toml").exists(),
            "第二份包没带 known_hosts.toml,手机上那份必须删(整份替换,D4)"
        );
    }

    #[test]
    fn a_failed_parse_does_not_create_the_data_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        let text = desktop_pack(&demo(), &["1"], true);
        let err = install_projects_only(&dir, &text, "wrong", &KEY).unwrap_err();
        assert!(matches!(err, StoreError::WrongPassword), "{err:?}");
        assert!(!dir.exists(), "口令错属纯内存失败,不得留下空数据目录");

        let err = install_projects_only(&dir, "format_version = 2\n", PW, &KEY).unwrap_err();
        assert!(matches!(err, StoreError::UnsupportedPack(2)), "{err:?}");
        assert!(!dir.exists(), "版本过新同理");
    }

    #[test]
    fn a_stale_staging_file_does_not_leak_into_the_import() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        let staging = dir.join(STAGING_DIR);
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("known_hosts.toml"), "# 残留\n").unwrap();

        // 新包不带 known_hosts.toml:残留若没清掉,会被 commit 搬进 dir。
        install_projects_only(&dir, &desktop_pack(&demo(), &["1"], false), PW, &KEY).unwrap();
        assert!(!dir.join("known_hosts.toml").exists());
    }

    #[test]
    fn a_whitelisted_entry_with_bad_base64_is_counted_as_unrecognized() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        let mut pack = crate::portable::read_pack(&desktop_pack(&demo(), &["1"], false)).unwrap();
        pack.file.push(PackFile {
            path: "known_hosts.toml".into(),
            body: "!!!不是 base64".into(),
        });
        let text = toml::to_string_pretty(&pack).unwrap();
        let r = install_projects_only(&dir, &text, PW, &KEY).unwrap();
        assert_eq!(r.unrecognized, 1);
        assert!(!dir.join("known_hosts.toml").exists(), "坏条目不落盘");
    }

    #[test]
    fn a_stale_staging_dir_from_a_crash_is_discarded() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        let staging = dir.join(STAGING_DIR);
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("sessions.toml"), "这不是 TOML [[[").unwrap();
        std::fs::write(staging.join("junk"), "x").unwrap();

        install_projects_only(&dir, &desktop_pack(&demo(), &["1"], true), PW, &KEY).unwrap();
        assert_eq!(names(&dir), three_files());
    }

    #[test]
    fn new_sessions_with_old_secrets_still_open_after_a_crash_between_renames() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        install_projects_only(&dir, &desktop_pack(&demo(), &["1", "4"], true), PW, &KEY).unwrap();
        // 模拟第 5 步只 rename 了 sessions.toml 就崩溃:新会话表 + 旧密文。
        let newer = file(
            vec![
                session(1, Some(&[]), inline()),
                session(9, Some(&[]), inline()),
            ],
            vec![project(1, "演示", &[9])],
        );
        std::fs::write(
            dir.join("sessions.toml"),
            toml::to_string_pretty(&newer).unwrap(),
        )
        .unwrap();

        let v = Vault::open(dir.clone(), &KEY).expect("同一把数据钥,孤儿裁剪应容忍");
        assert_eq!(v.list().len(), 2);
    }

    // ---- 终审修复:I1 KDF 上限 / I2 旧包 / I4 错误不带原文 / Minor-4 暂存残留 ----

    /// 手工拼一个口令头里 m_cost 任意的密文段(载荷随便,反正不该走到解密)。
    fn blob_with_kdf(m_cost: u32, t_cost: u32, p_cost: u32) -> Vec<u8> {
        crate::secrets_file::encode(
            &crate::secrets_file::Scheme::Argon2id {
                params: crate::kdf::KdfParams {
                    m_cost,
                    t_cost,
                    p_cost,
                },
                salt: [1u8; crate::kdf::SALT_LEN],
            },
            &[0u8; 64],
        )
    }

    #[test]
    fn a_pack_header_demanding_a_huge_argon2_memory_is_rejected_before_deriving() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        // 1 GiB:不夹紧的话会真去派生,最后报 WrongPassword。
        let text = pack_with(
            vec![(
                "sessions.toml".into(),
                toml::to_string_pretty(&demo()).unwrap().into_bytes(),
            )],
            &blob_with_kdf(1 << 20, 2, 1),
        );
        let err = install_projects_only(&dir, &text, PW, &KEY).unwrap_err();
        assert!(matches!(err, StoreError::CorruptPack(_)), "{err:?}");
        assert!(!dir.exists());
    }

    #[test]
    fn the_kdf_ceiling_is_inclusive_and_covers_each_parameter() {
        let ok = |m, t, p| check_kdf_params(&blob_with_kdf(m, t, p)).is_ok();
        assert!(ok(MAX_KDF_M_COST_KIB, MAX_KDF_T_COST, MAX_KDF_P_COST));
        assert!(!ok(MAX_KDF_M_COST_KIB + 1, 1, 1));
        assert!(!ok(8, MAX_KDF_T_COST + 1, 1));
        assert!(!ok(8, 1, MAX_KDF_P_COST + 1));
        let d = crate::kdf::KdfParams::default();
        assert!(ok(d.m_cost, d.t_cost, d.p_cost), "桌面默认参数必须放行");
    }

    #[test]
    fn a_pre_v5_pack_is_refused_without_touching_the_data_dir_or_local_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        // auth.path 指向一个不存在的本机路径:迁移若读它也只是静默跳过,
        // 所以靠结果是 PackTooOld(而非走到后面别的结果)来证明没进 Vault::open。
        let toml = format!(
            "schema_version = 4\n[[session]]\nid = 1\n[session.auth]\nuser = \"u\"\nkind = \"key\"\npath = \"{}\"\n",
            tmp.path().join("no-such-key").display()
        );
        let text = pack_with(vec![("sessions.toml".into(), toml.into_bytes())], &[]);
        let err = install_projects_only(&dir, &text, "", &KEY).unwrap_err();
        assert!(matches!(err, StoreError::PackTooOld(4)), "{err:?}");
        assert!(!dir.exists(), "拒收发生在碰文件系统之前");

        // 已有导入也不受影响。
        install_projects_only(&dir, &desktop_pack(&demo(), &["1"], true), PW, &KEY).unwrap();
        let before = std::fs::read(dir.join("sessions.toml")).unwrap();
        let err = install_projects_only(&dir, &text, "", &KEY).unwrap_err();
        assert!(matches!(err, StoreError::PackTooOld(4)), "{err:?}");
        assert_eq!(std::fs::read(dir.join("sessions.toml")).unwrap(), before);
        assert_eq!(names(&dir), three_files());
    }

    #[test]
    fn the_schema_probe_reads_the_last_sessions_entry_like_staging_does() {
        // 包里 sessions.toml 出现两次:staging 里后写的覆盖先写的,
        // 探测必须看后一条,否则可用「前新后旧」绕过拒收。
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        let text = pack_with(
            vec![
                (
                    "sessions.toml".into(),
                    toml::to_string_pretty(&demo()).unwrap().into_bytes(),
                ),
                ("sessions.toml".into(), b"schema_version = 3\n".to_vec()),
            ],
            &[],
        );
        let err = install_projects_only(&dir, &text, "", &KEY).unwrap_err();
        assert!(matches!(err, StoreError::PackTooOld(3)), "{err:?}");
    }

    #[test]
    fn an_old_sessions_entry_followed_by_an_undecodable_one_is_still_refused() {
        // 「前旧后坏」:后一条 base64 解不开,staging 循环跳过它,留下的是前一条 v3。
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        let text = pack_with(
            vec![
                ("sessions.toml".into(), b"schema_version = 3\n".to_vec()),
                ("sessions.toml".into(), b"X".to_vec()),
            ],
            &[],
        );
        assert_eq!(
            text.matches("\"WA==\"").count(),
            1,
            "占位条目的 base64 应恰好一处"
        );
        let text = text.replace("\"WA==\"", "\"!!!notb64\"");
        let err = install_projects_only(&dir, &text, "", &KEY).unwrap_err();
        assert!(matches!(err, StoreError::PackTooOld(3)), "{err:?}");
        assert!(!dir.exists());
    }

    #[test]
    fn a_sessions_entry_without_a_readable_schema_is_refused_up_front() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        let text = pack_with(vec![("sessions.toml".into(), b"not = [toml".to_vec())], &[]);
        let err = install_projects_only(&dir, &text, "", &KEY).unwrap_err();
        assert!(matches!(err, StoreError::CorruptPack(_)), "{err:?}");
        assert!(!dir.exists());
    }

    #[test]
    fn a_corrupt_decrypted_secrets_error_never_echoes_the_plaintext() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        let plain = b"[1]\npassword = \"s3cr3t-MARK\" oops-not-toml\n";
        let text = pack_with(
            vec![(
                "sessions.toml".into(),
                toml::to_string_pretty(&demo()).unwrap().into_bytes(),
            )],
            &seal_fast(plain, PW),
        );
        let err = install_projects_only(&dir, &text, PW, &KEY).unwrap_err();
        assert!(matches!(err, StoreError::CorruptSecrets(_)), "{err:?}");
        let shown = format!("{err} | {err:?}");
        assert!(
            !shown.contains("s3cr3t-MARK"),
            "错误文案泄露了口令: {shown}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_staging_dir_that_cannot_be_cleared_fails_instead_of_being_committed() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        let staging = dir.join(STAGING_DIR);
        let sub = staging.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("known_hosts.toml"), "# 残留\n").unwrap();
        // 子目录只读:里面的文件删不掉,remove_dir_all 失败。
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();
        let can_still_delete = std::fs::File::create(sub.join("probe")).is_ok(); // root 无视权限
        let r = install_projects_only(&dir, &desktop_pack(&demo(), &["1"], false), PW, &KEY);
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
        if can_still_delete {
            return;
        }
        assert!(matches!(r, Err(StoreError::Io(_))), "{r:?}");
    }

    #[test]
    fn a_cloud_object_opens_back_to_the_pack_text() {
        let blob = crate::portable::seal_secrets("pack-text-中文".as_bytes(), "pw").unwrap();
        assert_eq!(open_cloud_object(&blob, "pw").unwrap(), "pack-text-中文");
    }

    #[test]
    fn a_cloud_object_with_the_wrong_passphrase_is_wrong_password() {
        let blob = crate::portable::seal_secrets(b"x", "pw").unwrap();
        assert!(matches!(
            open_cloud_object(&blob, "nope"),
            Err(StoreError::WrongPassword)
        ));
    }

    /// 外层头里的 Argon2 参数同样由不可信方声明,派生之前就要拒(同 F346-a 的内层)。
    #[test]
    fn a_cloud_object_with_absurd_kdf_params_is_refused_before_deriving() {
        let blob = blob_with_kdf(MAX_KDF_M_COST_KIB + 1, 1, 1);
        assert!(matches!(
            open_cloud_object(&blob, "pw"),
            Err(StoreError::CorruptPack(_))
        ));
    }

    #[test]
    fn a_cloud_object_that_is_not_text_is_a_corrupt_pack_without_echoing_it() {
        let blob = crate::portable::seal_secrets(&[0xff, 0xfe, 0xfd], "pw").unwrap();
        match open_cloud_object(&blob, "pw") {
            Err(StoreError::CorruptPack(msg)) => assert!(!msg.contains('\u{fffd}'), "{msg}"),
            other => panic!("期望 CorruptPack,拿到 {other:?}"),
        }
    }
}
