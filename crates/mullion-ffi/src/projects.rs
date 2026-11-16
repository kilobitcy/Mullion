//! F348 项目列表 + F349 打开项目。判据全在 store(`project_view` / `overlay_project` /
//! `build_plan`),这里只做 IO 编排:开 vault、选节点(F304)、拨号、挂 attach 计划。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use mullion_ssh::dial::probe_first_hop;
use mullion_ssh::session::connect;
use mullion_store::project_view::{candidates, rows, Tab};
use mullion_store::{build_plan, overlay_project, ProjectId, Vault};
use time::{OffsetDateTime, UtcOffset};

use crate::cloud::data_key;
use crate::error::FfiError;
use crate::hostkey::{PendingHostKey, PhonePolicy};
use crate::runtime::runtime;
use crate::terminal::{Plan, ScreenListener, Terminal};

/// F304:非最后一条路线先探首跳,给它多久。与桌面 `PROJECT_PROBE_BUDGET` 一致。
const PROBE_BUDGET: Duration = Duration::from_secs(3);

/// 项目列表的一行。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ProjectRow {
    pub id: u64,
    pub name: String,
    pub dir: String,
    /// 「3 天前」/「从未打开」。
    pub last_opened: String,
    pub nodes: u32,
    pub archived: bool,
}

/// 打开项目的结局。没见过的主机不算错误:弹窗确认后重开即可。
#[derive(uniffi::Enum)]
pub enum OpenOutcome {
    Opened {
        terminal: Arc<Terminal>,
        node: String,
    },
    UnknownHostKey {
        key: PendingHostKey,
    },
}

fn open_vault(vault_dir: &str, key: &[u8]) -> Result<Vault, FfiError> {
    Ok(Vault::open(PathBuf::from(vault_dir), &data_key(key)?)?)
}

/// F348。浏览只列在用;有搜索词时穿透归档(F257),排序同桌面 `rows`。
/// `utc_offset_seconds` 由 Kotlin 给:Android 上 `time` 取本地偏移不可靠。
#[uniffi::export]
pub fn list_projects(
    vault_dir: String,
    data_key: Vec<u8>,
    query: String,
    utc_offset_seconds: i32,
) -> Result<Vec<ProjectRow>, FfiError> {
    let vault = open_vault(&vault_dir, &data_key)?;
    let offset = UtcOffset::from_whole_seconds(utc_offset_seconds).unwrap_or(UtcOffset::UTC);
    Ok(project_rows(
        &vault,
        &query,
        OffsetDateTime::now_utc(),
        offset,
    ))
}

fn project_rows(
    vault: &Vault,
    query: &str,
    now: OffsetDateTime,
    offset: UtcOffset,
) -> Vec<ProjectRow> {
    // 手机没有运行指示灯(一期):冻结灯表传空。
    rows(
        vault.projects(),
        Tab::Active,
        query,
        vault.list(),
        &BTreeMap::new(),
    )
    .into_iter()
    .map(|p| ProjectRow {
        id: p.id.0,
        name: p.name.clone(),
        dir: p.dir.clone(),
        last_opened: p.last_accessed_at.as_deref().map_or_else(
            || "从未打开".to_string(),
            |t| mullion_store::localtime::relative(t, now, offset),
        ),
        nodes: p.nodes.len() as u32,
        archived: p.archived_at.is_some(),
    })
    .collect()
}

/// F349。阻塞直到连上(或确定连不上);Kotlin 放 `Dispatchers.IO`。
#[uniffi::export]
pub fn open_project(
    vault_dir: String,
    data_key: Vec<u8>,
    project_id: u64,
    cols: u16,
    rows: u16,
    listener: Arc<dyn ScreenListener>,
) -> Result<OpenOutcome, FfiError> {
    let vault = open_vault(&vault_dir, &data_key)?;
    let p = vault
        .projects()
        .iter()
        .find(|p| p.id == ProjectId(project_id))
        .ok_or_else(|| FfiError::Connect("项目已不在本机数据里,请重新导入".into()))?
        .clone();
    let order = candidates(&p, |id| crate::dial::is_routed(&vault, id));
    if order.is_empty() {
        return Err(FfiError::Connect(format!("项目「{}」没有可用节点", p.name)));
    }

    if PhonePolicy::load(Path::new(&vault_dir)).is_corrupt() {
        return Err(FfiError::Storage(crate::hostkey::CORRUPT.into()));
    }
    let rt = runtime();
    let mut tried = Vec::new();
    for (i, id) in order.iter().enumerate() {
        let last = i + 1 == order.len();
        let node = vault
            .get(*id)
            .map_or_else(|| format!("节点 {}", id.0), |r| r.identity.name.clone());
        let cfg = match crate::dial::dial_config(&vault, *id, cols, rows) {
            Ok(c) => c,
            Err(why) => {
                tried.push(why);
                continue;
            }
        };
        // F304:最后一条不探 —— 探不探都得拨它,探只是白花 3 秒。
        if !last {
            if let Err(e) = rt.block_on(probe_first_hop(
                &cfg.hops,
                &cfg.host,
                cfg.port,
                PROBE_BUDGET,
            )) {
                tried.push(format!("「{node}」{e}"));
                continue;
            }
        }
        // 够得着之后的失败(认证、主机密钥)就地报错,不换下一条(桌面同口径)。
        let policy = Arc::new(PhonePolicy::load(Path::new(&vault_dir)));
        let connected = rt.block_on(connect(&cfg, policy.clone(), Arc::new(|| {})));
        let (session, rx) = match connected {
            Ok(v) => v,
            Err(e) => {
                if let Some(key) = policy.take_pending() {
                    return Ok(OpenOutcome::UnknownHostKey { key });
                }
                return Err(FfiError::Connect(format!("「{node}」{e}")));
            }
        };
        let resolved = vault
            .resolve_for(*id)
            .map_err(|e| FfiError::Connect(format!("「{node}」{e}")))?;
        let overlaid = overlay_project(&p, &resolved.automation);
        // fallback 传空串:项目 tmux 名恒由 `project_tmux_name` 给,不许回落会话名
        // (两个项目共用一个 Claude Code,见桌面 `pending_for_project`)。
        let steps = build_plan(&overlaid, "");
        let plan = (!steps.is_empty()).then_some(Plan {
            steps,
            ready_timeout_ms: overlaid.ready_timeout_ms,
        });
        let terminal = Terminal::start(session, rx, &cfg, listener, plan);
        return Ok(OpenOutcome::Opened { terminal, node });
    }
    Err(FfiError::Connect(format!(
        "项目「{}」的节点都连不上:{}",
        p.name,
        tried.join(";")
    )))
}
