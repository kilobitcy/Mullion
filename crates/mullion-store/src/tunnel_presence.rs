//! F338:**本机跨实例**的「哪些隧道正在哪个实例里跑」。零 UI、零 async、纯同步 IO。
//!
//! 隧道的运行态(句柄、状态)只活在各自进程的内存里,另一个 exe 实例
//! 只能从配置里读到「有这条隧道」,读不到「它正在别处跑」—— 于是第二个窗口里
//! 同一条隧道显示「启动」,点下去去抢一个已经被占着的端口。
//!
//! 做法照 F224 的 [`crate::presence`]:一个实例 = `<config_dir>/tunnel-state/<实例id>.state`
//! 一个文件,**每个进程只写自己那一个,从不改别人的**;活性判定复用
//! F148 的 [`crate::history::is_alive`]。
//!
//! **不并进 F224 的在场文件**:那个文件「第一行是秒,其余每行一个 tmux 名」,
//! 版本不一致的实例(多开时很常见)读到追加的隧道行,会把它们当 tmux 名点亮
//! 一盏凭空的灯。新开一个目录,旧版根本不读。
//!
//! 文件内容(纯文本):
//! ```text
//! <写入时刻 Unix 秒>
//! <pid> <本实例启动时刻 Unix 秒>
//! <隧道 id> <状态标签>      ← 每条在跑的隧道一行
//! ```

use std::path::{Path, PathBuf};

use crate::error::StoreError;
use crate::tunnel::TunnelId;

/// 存放目录名(在 `config_dir` 下)。
pub const TUNNEL_STATE_DIR: &str = "tunnel-state";

/// 文件扩展名。
pub const TUNNEL_STATE_EXT: &str = "state";

/// 别的实例报出来的隧道状态。比 `mullion_ssh::tunnel::TunnelState` 粗:
/// store 不认识 ssh crate,而对「另一个窗口」来说,重连第几次、还有几秒
/// 都不是本窗口能处置的信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteTunnelState {
    Connecting,
    Running,
    Reconnecting,
    /// 停下了等人工。端口多半已放开,**不挡**本实例启动。
    Failed,
}

impl RemoteTunnelState {
    fn tag(self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Running => "running",
            Self::Reconnecting => "reconnecting",
            Self::Failed => "failed",
        }
    }

    fn from_tag(s: &str) -> Option<Self> {
        Some(match s {
            "connecting" => Self::Connecting,
            "running" => Self::Running,
            "reconnecting" => Self::Reconnecting,
            "failed" => Self::Failed,
            _ => return None,
        })
    }

    /// 这个状态是否应当挡住本实例对同一条隧道的「启动」。
    ///
    /// 「连接中」也挡:两个实例几乎同时点启动,就是去抢同一个端口。
    /// 「失败」不挡:挡了的话用户在本实例里就无法重试。
    pub fn blocks_start(self) -> bool {
        !matches!(self, Self::Failed)
    }
}

/// 别的实例里的一条隧道。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtherTunnel {
    pub tunnel: TunnelId,
    pub state: RemoteTunnelState,
    /// 对方进程号,供用户在任务管理器里对上号。
    pub pid: u32,
    /// 对方实例启动时刻(Unix 秒)。
    pub started_at: i64,
    pub instance: String,
}

/// `<dir>/tunnel-state`。
pub fn tunnel_state_dir(dir: &Path) -> PathBuf {
    dir.join(TUNNEL_STATE_DIR)
}

/// 某个实例的隧道状态文件。
pub fn tunnel_state_path(dir: &Path, id: &str) -> PathBuf {
    tunnel_state_dir(dir).join(format!("{id}.{TUNNEL_STATE_EXT}"))
}

/// 解析结果:`(写入时刻, pid, 启动时刻, 各隧道)`。
type Parsed = (i64, u32, i64, Vec<(TunnelId, RemoteTunnelState)>);

/// `None` = 头两行有一行读不出来。一律当**没有这条记录**处置。
/// 单个隧道行读不出来只跳过那一行 —— 别的行是好的。
pub fn parse(text: &str) -> Option<Parsed> {
    let mut lines = text.lines();
    let at = lines.next()?.trim().parse::<i64>().ok()?;
    let mut head = lines.next()?.split_whitespace();
    let pid = head.next()?.parse::<u32>().ok()?;
    let started = head.next()?.parse::<i64>().ok()?;
    let items = lines
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let id = it.next()?.parse::<u64>().ok()?;
            let st = RemoteTunnelState::from_tag(it.next()?)?;
            Some((TunnelId(id), st))
        })
        .collect();
    Some((at, pid, started, items))
}

/// 写自己这一份。**不是 `write_atomic`**,理由同 F224:写一半的后果是下一次
/// 读回 `None`(这个实例的隧道暂时看不见),而它每次状态变化、每 15 秒都会重写。
pub fn publish(
    dir: &Path,
    id: &str,
    now_secs: i64,
    pid: u32,
    started_at: i64,
    items: &[(TunnelId, RemoteTunnelState)],
) -> Result<(), StoreError> {
    std::fs::create_dir_all(tunnel_state_dir(dir))?;
    let mut text = format!("{now_secs}\n{pid} {started_at}");
    for (t, s) in items {
        text.push_str(&format!("\n{} {}", t.0, s.tag()));
    }
    std::fs::write(tunnel_state_path(dir, id), text.as_bytes())?;
    Ok(())
}

/// 撤掉自己那份(优雅退出时)。不撤的话,隧道已经随进程死了,别的实例还要
/// 再显示 45 秒「在另一个窗口运行中」。
pub fn remove_own(dir: &Path, id: &str) {
    let _ = std::fs::remove_file(tunnel_state_path(dir, id));
}

/// **别的**实例此刻报出来的隧道。`self_id` 那份被跳过。
///
/// 过期的文件只跳过、不删 —— 删除是启动时 [`sweep`] 的事(同 F224)。
pub fn read_others(dir: &Path, self_id: &str, now_secs: i64) -> Vec<OtherTunnel> {
    let Ok(rd) = std::fs::read_dir(tunnel_state_dir(dir)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some(TUNNEL_STATE_EXT) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|x| x.to_str()) else {
            continue;
        };
        if stem == self_id {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some((at, pid, started_at, items)) = parse(&text) else {
            continue;
        };
        if !crate::history::is_alive(now_secs, at) {
            continue;
        }
        for (tunnel, state) in items {
            out.push(OtherTunnel {
                tunnel,
                state,
                pid,
                started_at,
                instance: stem.to_string(),
            });
        }
    }
    out
}

/// 启动时清掉过期的状态文件。自己那份不清。
pub fn sweep(dir: &Path, self_id: &str, now_secs: i64) {
    let Ok(rd) = std::fs::read_dir(tunnel_state_dir(dir)) else {
        return;
    };
    for e in rd.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some(TUNNEL_STATE_EXT) {
            continue;
        }
        if path.file_stem().and_then(|x| x.to_str()) == Some(self_id) {
            continue;
        }
        let expired = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| parse(&t))
            .is_none_or(|(at, ..)| !crate::history::is_alive(now_secs, at));
        if expired {
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn what_we_publish_is_what_another_instance_reads_back() {
        let d = dir();
        publish(
            d.path(),
            "a",
            1000,
            4242,
            900,
            &[
                (TunnelId(3), RemoteTunnelState::Running),
                (TunnelId(5), RemoteTunnelState::Reconnecting),
            ],
        )
        .unwrap();
        let got = read_others(d.path(), "b", 1000);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].pid, 4242);
        assert_eq!(got[0].started_at, 900);
        assert_eq!(got[0].instance, "a");
        let mut by_id: Vec<_> = got.iter().map(|o| (o.tunnel, o.state)).collect();
        by_id.sort_by_key(|(t, _)| *t);
        assert_eq!(
            by_id,
            vec![
                (TunnelId(3), RemoteTunnelState::Running),
                (TunnelId(5), RemoteTunnelState::Reconnecting)
            ]
        );
    }

    /// 自己那份不算数:否则本实例的隧道会被当成「在另一个窗口跑」,
    /// 自己停掉之后「启动」还要灰上 45 秒。
    #[test]
    fn our_own_file_is_not_another_window() {
        let d = dir();
        publish(
            d.path(),
            "a",
            1000,
            1,
            1,
            &[(TunnelId(1), RemoteTunnelState::Running)],
        )
        .unwrap();
        assert!(read_others(d.path(), "a", 1000).is_empty());
    }

    /// 活性判定复用 F148 那条,不另定阈值。自证会变红:删掉 `read_others` 里的 `is_alive`。
    #[test]
    fn a_stale_file_stops_counting_once_the_grace_period_is_over() {
        let d = dir();
        publish(
            d.path(),
            "a",
            1000,
            1,
            1,
            &[(TunnelId(1), RemoteTunnelState::Running)],
        )
        .unwrap();
        let grace = crate::history::ALIVE_GRACE_SECS;
        assert_eq!(read_others(d.path(), "b", 1000 + grace).len(), 1);
        assert!(read_others(d.path(), "b", 1000 + grace + 1).is_empty());
    }

    /// 优雅退出撤掉自己那份之后,别的实例立刻不再看到它,不用等宽限期。
    #[test]
    fn removing_our_file_hides_our_tunnels_at_once() {
        let d = dir();
        publish(
            d.path(),
            "a",
            1000,
            1,
            1,
            &[(TunnelId(1), RemoteTunnelState::Running)],
        )
        .unwrap();
        remove_own(d.path(), "a");
        assert!(read_others(d.path(), "b", 1000).is_empty());
    }

    /// 只有「失败」不挡启动。自证会变红:把 `blocks_start` 改成恒 true / 恒 false。
    #[test]
    fn only_a_failed_tunnel_leaves_the_start_button_free() {
        assert!(RemoteTunnelState::Connecting.blocks_start());
        assert!(RemoteTunnelState::Running.blocks_start());
        assert!(RemoteTunnelState::Reconnecting.blocks_start());
        assert!(!RemoteTunnelState::Failed.blocks_start());
    }

    /// 坏文件 / 坏行只跳过它自己,不 panic,也不拖累别的实例。
    #[test]
    fn corrupt_content_is_skipped_per_file_and_per_line() {
        let d = dir();
        std::fs::create_dir_all(tunnel_state_dir(d.path())).unwrap();
        std::fs::write(tunnel_state_path(d.path(), "a"), b"\xff\xfe not text").unwrap();
        std::fs::write(tunnel_state_path(d.path(), "b"), "不是数字\n1 1").unwrap();
        std::fs::write(
            tunnel_state_path(d.path(), "c"),
            "1000\n7 3\n9 nonsense\nx running\n4 running",
        )
        .unwrap();
        let got = read_others(d.path(), "me", 1000);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].tunnel, TunnelId(4));
    }

    #[test]
    fn sweep_removes_only_expired_files_and_never_our_own() {
        let d = dir();
        let grace = crate::history::ALIVE_GRACE_SECS;
        publish(d.path(), "old", 1000, 1, 1, &[]).unwrap();
        publish(d.path(), "fresh", 1000 + grace + 5, 1, 1, &[]).unwrap();
        publish(d.path(), "me", 1, 1, 1, &[]).unwrap();
        sweep(d.path(), "me", 1000 + grace + 5);
        assert!(!tunnel_state_path(d.path(), "old").exists());
        assert!(tunnel_state_path(d.path(), "fresh").exists());
        assert!(tunnel_state_path(d.path(), "me").exists());
    }
}
