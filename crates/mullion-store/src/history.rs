//! F148:**多实例**现场历史的磁盘布局。零 UI、零 async、纯同步 IO。
//!
//! 一个 exe 实例 = `<config_dir>/layouts/<实例id>.toml` 一个文件 +
//! `<config_dir>/layouts/<实例id>.alive` 一个心跳文件。**每个进程只写自己那
//! 两个文件,从不改别人的**。
//!
//! 为什么不是「一个文件里存 10 条数组」:那样每个实例退出时都要读-改-写同一份
//! 文件,两个窗口同时关闭时,后写的那个会拿着旧的 9 条 + 自己,把先写的那条
//! **整个抹掉** —— 静默且不可恢复。要修就得上文件锁,而那是新依赖 + 我们
//! 验证不了的 Windows 平台行为。
//!
//! 一实例一文件之后,并发问题从结构上消失:写路径永不共享,唯一的共享操作是
//! 启动时的删除,而**删除的误判后果有界** —— 多删 = 少一条历史,少删 = 目录里
//! 多几个文件,下次启动再删。
//!
//! 本模块**不认识 `mullion-core`**,理由同 `layout.rs`(架构不变量)。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::StoreError;
use crate::layout::SavedLayout;

/// 记录目录名(在 `config_dir` 下)。
pub const HISTORY_DIR: &str = "layouts";

/// 心跳文件的扩展名。
pub const ALIVE_EXT: &str = "alive";

/// 心跳的写入间隔(秒)。见 [`is_alive`] 的宽限说明。
pub const HEARTBEAT_INTERVAL_SECS: u64 = 15;

/// 心跳的宽限期(秒)= 写入间隔的 3 倍。
pub const ALIVE_GRACE_SECS: i64 = 45;

/// 已关闭的记录最多留几条。**活着的实例不计入这个名额** ——
/// 否则「同时开着 10 个窗口」会把全部历史删光,而用户说的「保留最后 10 条
/// 记录」指的是**已经关掉的现场**。
pub const MAX_RECORDS: usize = 10;

/// 这个实例的身份。**毫秒时间戳 + pid**,零新依赖。
///
/// 同一毫秒同一 pid 不可能撞(pid 在一毫秒内不会被回收再分配给新进程)。
/// 时间戳由调用方传进来而不是这里取 —— 取时刻是 IO 边界上的事,传进来才
/// 测得了「同毫秒不同 pid 不撞」。
///
/// 只含数字和 `-`,直接当文件名安全(不需要转义,也不可能撞上 `..`)。
pub fn new_instance_id(now_ms: u128, pid: u32) -> String {
    format!("{now_ms}-{pid}")
}

/// 记录目录:`<dir>/layouts`。
pub fn history_dir(dir: &Path) -> PathBuf {
    dir.join(HISTORY_DIR)
}

/// 某个实例的记录文件。
pub fn record_path(dir: &Path, id: &str) -> PathBuf {
    history_dir(dir).join(format!("{id}.toml"))
}

/// 某个实例的心跳文件。
pub fn alive_path(dir: &Path, id: &str) -> PathBuf {
    history_dir(dir).join(format!("{id}.{ALIVE_EXT}"))
}

/// 心跳停在 `heartbeat_secs` 的那个实例,在 `now_secs` 这一刻还算活着吗。
///
/// **纯函数**,这是刻意的:活性判定的另外两条路(文件锁 / PID + 进程启动时间)
/// 都要平台特定代码,而 Windows 那一半在无头容器里验证不了。换成一段算术之后,
/// 两种误判的后果都有界且**会自愈**:
/// - 判活为死 → 列表里多一条(恢复它 = 克隆一份,无害);
/// - 判死为活 → 隐藏它,但心跳必然过期,**最多隐藏 45 秒**。
///
/// 宽限期取写入间隔的 3 倍:一次写盘失败、一次调度延迟都不该让一个活着的
/// 窗口被判死。
///
/// **`saturating_sub` 不是装饰**:`heartbeat_secs` 来自 `.alive` 文件里
/// `parse::<i64>()` 的结果,用户手改成 `i64::MIN` 是合法输入,
/// 裸减法在 debug 构建下会溢出 panic —— 一个被手改过的心跳文件就能让客户端
/// 起不来。饱和之后,未来的心跳自然算活着,而那正是时钟往回跳(NTP 校时)时
/// 我们要的:判死等于把一个正开着的窗口摆进恢复列表。
pub fn is_alive(now_secs: i64, heartbeat_secs: i64) -> bool {
    now_secs.saturating_sub(heartbeat_secs) <= ALIVE_GRACE_SECS
}

/// 当前的 Unix 秒(UTC)。系统时钟早于 1970 时返回 0。
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 当前的 Unix 毫秒(UTC)。给 [`new_instance_id`] 用。
pub fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// 写一次心跳:把 `now` 这个 Unix 秒写进 `<dir>/layouts/<id>.alive`。
///
/// **不是 `write_atomic`**:心跳只有一行数字,写一半的后果是下一次读回 `None`
/// (判死),而心跳每 15 秒就再写一次 —— 为它多付一次 rename 不值得。记录文件
/// 那边才需要原子写(那里写一半会丢掉整个现场)。
///
/// **必须无条件调用**,不能搭布局落盘的顺风车:`App::flush_layout_if_due` 是
/// 「布局没变就不写盘」,心跳跟着它走的话,一个开着不动的窗口会永远不写心跳、
/// 被别的实例判成死的。
pub fn touch_alive(dir: &Path, id: &str, now: i64) -> Result<(), StoreError> {
    std::fs::create_dir_all(history_dir(dir))?;
    std::fs::write(alive_path(dir, id), now.to_string().as_bytes())?;
    Ok(())
}

/// 读某个实例最后一次心跳的时刻。`None` = 没写过 / 读不出来 / 内容不是数字,
/// 三者一律当**死了**处置(见模块文档:误判的后果有界且会自愈)。
pub fn read_heartbeat(dir: &Path, id: &str) -> Option<i64> {
    let text = std::fs::read_to_string(alive_path(dir, id)).ok()?;
    text.trim().parse::<i64>().ok()
}

/// 列表里的一条。
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    /// 实例 id(= 文件名去掉 `.toml`)。恢复时用它认领槽位。
    pub id: String,
    pub layout: SavedLayout,
    /// 它的主人此刻还活着吗(判据见 [`is_alive`])。**只是给调用方过滤用**,
    /// 判错不影响能不能恢复。
    pub alive: bool,
}

/// 原子写一条记录。
///
/// **这里要 `write_atomic`**(与心跳相反):写一半的记录会让下次启动少一条
/// 现场,而这个文件每 2 秒才写一次、承载的是用户拖出来的分屏比例。
pub fn save_record(dir: &Path, id: &str, layout: &SavedLayout) -> Result<(), StoreError> {
    let hdir = history_dir(dir);
    std::fs::create_dir_all(&hdir)?;
    let mut out = layout.clone();
    out.schema_version = crate::layout::CURRENT_LAYOUT_SCHEMA;
    let text = toml::to_string_pretty(&out)?;
    crate::vault::write_atomic(&record_path(dir, id), text.as_bytes())
}

/// 删一条记录(连心跳一起)。找不到文件**不算错** —— 调用方多半正在清理一个
/// 本来就不存在的槽位。
pub fn remove_record(dir: &Path, id: &str) {
    let _ = std::fs::remove_file(record_path(dir, id));
    let _ = std::fs::remove_file(alive_path(dir, id));
}

/// 列举目录里全部记录,**按 `updated_at` 倒序**(最近的在前)。
///
/// **没有 `Result`,这是刻意的**(同 `layout::Loaded` 的理由):历史不是用户
/// 资产,读不出来的正确表现是「这条不在列表里」,不是「打不开客户端」。
/// 单条记录解析失败只跳过那一条 —— 另外几条现场是好的,用户要的是它们照常出现。
///
/// `now_secs` 由调用方传:活性判定要它,而取时刻是 IO 边界上的事,传进来才
/// 测得了「心跳早停的那条被判死」。
pub fn list_records(dir: &Path, now_secs: i64) -> Vec<HistoryEntry> {
    let hdir = history_dir(dir);
    let Ok(rd) = std::fs::read_dir(&hdir) else {
        // 目录不存在 = 首次运行,正常情况,不记日志。
        return Vec::new();
    };
    let mut out = Vec::new();
    for ent in rd.flatten() {
        let path = ent.path();
        // `.alive` 心跳、`write_atomic` 崩在 rename 之前留下的 `.tmp` 残骸,
        // 都不是记录。
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(layout) = toml::from_str::<SavedLayout>(&text) else {
            continue;
        };
        // 更新版本写出来的记录:**不猜**(同 `layout::load`)。字段可能改了
        // 含义,按现版本读出来的现场是错的,而错的现场比没有现场更难排查。
        if layout.schema_version > crate::layout::CURRENT_LAYOUT_SCHEMA {
            continue;
        }
        let alive = read_heartbeat(dir, id).is_some_and(|h| is_alive(now_secs, h));
        out.push(HistoryEntry {
            id: id.to_string(),
            layout,
            alive,
        });
    }
    // 倒序:最近的现场在最上面。`updated_at` 相同时按 id 倒序,让顺序**确定**
    // —— 不定的顺序会让列表在两次启动之间莫名其妙地换位置。
    out.sort_by(|a, b| {
        b.layout
            .updated_at
            .cmp(&a.layout.updated_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    out
}

/// 哪几条该删。输入必须是 [`list_records`] 的输出(已按 `updated_at` 倒序)。
///
/// **纯函数**:「该删哪几条」是这一片唯一一处会**永久丢用户数据**的判断,
/// 把它跟「怎么删文件」分开,才能对着一堆构造出来的条目把边界钉死。
///
/// 规则(设计 D5):活着的一概不删、且**不占名额**;死的里面留最新
/// [`MAX_RECORDS`] 条,其余按从旧到新的顺序删。
pub fn plan_prune(entries: &[HistoryEntry]) -> Vec<String> {
    entries
        .iter()
        .filter(|e| !e.alive)
        .skip(MAX_RECORDS)
        .map(|e| e.id.clone())
        .collect()
}

/// 裁剪一次,返回删了几条。**只在启动时调**(设计 X6):关窗口时也裁的话,
/// 「读整个目录」就从一次性动作变成了每个实例退出都要做的共享操作。
pub fn prune(dir: &Path, now_secs: i64) -> usize {
    let doomed = plan_prune(&list_records(dir, now_secs));
    for id in &doomed {
        remove_record(dir, id);
    }
    doomed.len()
}

/// 把 v1 时代的单份 `layout.toml` 迁成一条记录,然后**删掉老文件**(设计 D14)。
///
/// 返回 `Some(id)` = 真的迁了一条;`None` = 没有老文件 / 老文件是空的 / 老文件
/// 坏了。**这三种情况都会把老文件删掉** —— 留着的话每次启动都要来读一遍,
/// 而它永远不会再被写。
///
/// 这是**单向**升级:exe 降回旧版本会一条标签都不恢复。可接受 —— 布局不是
/// 用户资产,丢了只是回到空标签栏(`layout.rs` 开篇就是这么写的)。
pub fn migrate_legacy(dir: &Path, id: &str, now: i64) -> Option<String> {
    let legacy = dir.join(crate::layout::LAYOUT_FILE);
    if !legacy.exists() {
        return None;
    }
    // `layout::load` 永不失败:坏文件读回来是空布局 + 一句 note。
    let mut layout = crate::layout::load(dir).layout;
    let _ = std::fs::remove_file(&legacy);
    if layout.tabs.is_empty() {
        return None;
    }
    // v1 没有这个字段,读回来是 0。留 0 的话这条记录会永远排在列表最底下,
    // 而且是第一个被裁掉的 —— 而它恰恰是用户升级前最后一次的现场。
    layout.updated_at = now;
    match save_record(dir, id, &layout) {
        Ok(()) => Some(id.to_string()),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{SavedNodeEntry, SavedTab, SavedTabKind};
    use crate::model::SessionId;

    /// 一份最小的可用布局:一个终端标签、一个叶子。
    fn layout_at(updated_at: i64, title: &str) -> SavedLayout {
        SavedLayout {
            schema_version: crate::layout::CURRENT_LAYOUT_SCHEMA,
            active_tab: 0,
            updated_at,
            window: None,
            tabs: vec![SavedTab {
                kind: SavedTabKind::Terminal,
                session_id: SessionId(1),
                title: title.into(),
                focus_leaf: 0,
                tree: vec![SavedNodeEntry::leaf()],
            }],
        }
    }

    #[test]
    fn two_instances_started_in_the_same_millisecond_get_different_ids() {
        assert_ne!(
            new_instance_id(1_755_000_000_123, 4242),
            new_instance_id(1_755_000_000_123, 4243)
        );
    }

    #[test]
    fn the_same_process_started_later_gets_a_different_id() {
        assert_ne!(
            new_instance_id(1_755_000_000_123, 4242),
            new_instance_id(1_755_000_000_124, 4242)
        );
    }

    /// 实例 id 直接当文件名用,所以里面**不能**出现路径分隔符或 `..` ——
    /// 否则一个被手改过的 id 就能让 `record_path` 指到目录外面去。
    #[test]
    fn an_instance_id_is_safe_to_use_as_a_file_name() {
        let id = new_instance_id(1_755_000_000_123, 4242);
        assert!(
            id.chars().all(|c| c.is_ascii_digit() || c == '-'),
            "实例 id 里出现了非数字非连字符:{id}"
        );
    }

    #[test]
    fn the_record_and_heartbeat_live_side_by_side_under_the_history_dir() {
        let dir = Path::new("/cfg");
        assert_eq!(record_path(dir, "7-1"), Path::new("/cfg/layouts/7-1.toml"));
        assert_eq!(alive_path(dir, "7-1"), Path::new("/cfg/layouts/7-1.alive"));
    }

    /// 刚跳过一次心跳的实例还活着 —— 宽限期就是给它的。
    #[test]
    fn an_instance_that_missed_one_heartbeat_is_still_alive() {
        assert!(
            is_alive(1000, 1000 - 20),
            "跳了一次心跳就被判死,宽限期形同虚设"
        );
    }

    /// 过了宽限期就算死了。
    ///
    /// **判据写死成秒数,不用 `ALIVE_GRACE_SECS`** —— 拿常量去断言常量是
    /// 重言式:常量改成 4500,测试会跟着改判据、永远自洽,「宽限期到底是多久」
    /// 根本没被锁住。
    #[test]
    fn an_instance_silent_past_the_grace_period_is_considered_gone() {
        assert!(is_alive(1000, 955), "刚好 45 秒没心跳,该算活着");
        assert!(
            !is_alive(1000, 954),
            "46 秒没心跳还判活 —— 那条记录会被永久隐藏"
        );
    }

    /// 宽限期必须是心跳间隔的 3 倍:一次写盘失败、一次调度延迟都不该让一个
    /// 活着的窗口被判死。两个常量哪一个被单独调了,这里就变红。
    #[test]
    fn the_grace_period_is_three_heartbeats_wide() {
        assert_eq!(ALIVE_GRACE_SECS, 3 * HEARTBEAT_INTERVAL_SECS as i64);
    }

    /// 时钟往回跳(NTP 校时)时,未来的心跳必须算活着 —— 判死等于把一个正
    /// 开着的窗口摆进恢复列表,用户恢复它就克隆出一个重复的现场。
    #[test]
    fn a_heartbeat_from_the_future_still_counts_as_alive() {
        assert!(is_alive(1000, 5000));
        assert!(is_alive(1000, i64::MAX), "极端未来的心跳也该算活着");
    }

    /// `.alive` 里的心跳是 `parse::<i64>()` 出来的,手改过的文件能塞进**任何**
    /// `i64`,包括 `i64::MIN`。`now - i64::MIN` 在 debug 构建下溢出 panic ——
    /// 一个被手改过的心跳文件就能让客户端起不来,所以 `is_alive` 里的
    /// `saturating_sub` 不是装饰。
    ///
    /// 自证会变红:把 `saturating_sub` 换回裸减法,第一条断言 panic(那也是红)。
    /// 注意 `i64::MAX` 那个方向**不会**溢出(`1000 - i64::MAX` 仍在范围内),
    /// 钉住溢出的只有 `i64::MIN` 这一条。
    #[test]
    fn a_hand_edited_heartbeat_at_the_low_extreme_does_not_crash_the_client() {
        assert!(!is_alive(1000, i64::MIN), "远古到溢出的心跳该算死了");
        assert!(!is_alive(i64::MAX, 0), "远古心跳该算死了");
    }

    /// 心跳写下去、读回来,时刻对得上。
    #[test]
    fn a_heartbeat_can_be_written_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        touch_alive(dir.path(), "7-1", 1_755_000_000).unwrap();
        assert_eq!(read_heartbeat(dir.path(), "7-1"), Some(1_755_000_000));
    }

    /// 没写过心跳的实例读回 `None` —— 调用方据此把它当成死的。
    #[test]
    fn a_missing_heartbeat_reads_back_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_heartbeat(dir.path(), "nobody"), None);
    }

    /// 心跳文件被手改成垃圾 → `None`(判死),**不是报错**。心跳不是用户资产,
    /// 它坏了的正确表现是「那条记录出现在列表里」,不是「启动失败」。
    #[test]
    fn a_corrupt_heartbeat_reads_back_as_none_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(history_dir(dir.path())).unwrap();
        std::fs::write(alive_path(dir.path(), "7-1"), b"\x00 not a number").unwrap();
        assert_eq!(read_heartbeat(dir.path(), "7-1"), None);
    }

    /// 重复 touch 就是覆盖,不是追加 —— 追加的话文件会随运行时长无限增长,
    /// 而它每 15 秒写一次。
    #[test]
    fn touching_twice_overwrites_rather_than_appends() {
        let dir = tempfile::tempdir().unwrap();
        touch_alive(dir.path(), "7-1", 1000).unwrap();
        touch_alive(dir.path(), "7-1", 2000).unwrap();
        assert_eq!(read_heartbeat(dir.path(), "7-1"), Some(2000));
        let len = std::fs::metadata(alive_path(dir.path(), "7-1"))
            .unwrap()
            .len();
        assert!(len < 32, "心跳文件在增长({len} 字节)—— 它每 15 秒写一次");
    }

    #[test]
    fn a_record_can_be_saved_and_listed_back() {
        let dir = tempfile::tempdir().unwrap();
        save_record(dir.path(), "7-1", &layout_at(1000, "prod")).unwrap();
        let got = list_records(dir.path(), 9999);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "7-1");
        assert_eq!(got[0].layout.tabs[0].title, "prod");
    }

    /// 目录不存在 = 空列表,**不报错**。首次运行走的就是这条。
    #[test]
    fn a_missing_history_dir_lists_nothing_without_complaining() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list_records(dir.path(), 9999).is_empty());
    }

    /// 坏文件**不能**拖垮整份列表 —— 另外那几条现场是好的,用户要的是它们
    /// 照常出现。这是 `layout::load` 那条「布局不是用户资产」的同一姿态,
    /// 只是从「整份」下沉到了「每条」。
    ///
    /// **好坏交替、各 8 条**,不是各 1 条:`read_dir` 的顺序没有保证,一好
    /// 一坏时「先读好的、再撞上坏的」会让「撞上就整份返回」的写法照样交出
    /// 那一条好记录,测试恒绿。交替之后,除非 8 条好的全排在 8 条坏的前面
    /// (字典序文件系统下不可能,hash 序下也只有 1/12870),必然丢掉好记录。
    ///
    /// 自证会变红:把 `list_records` 里 `read_to_string` 或 `toml::from_str`
    /// **任意一条**失败分支的 `continue` 改成 `return out;`。
    #[test]
    fn corrupt_records_do_not_take_the_whole_list_down() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..8i64 {
            save_record(
                dir.path(),
                &format!("{i}a-good"),
                &layout_at(1000 + i, "good"),
            )
            .unwrap();
            // **两种坏法各来一半**:非 UTF-8 的死在 `read_to_string`,合法
            // UTF-8 但非法 TOML 的死在 `toml::from_str` —— 那是两条独立的
            // `continue`,只喂一种坏法就只压得住其中一道,另一道恒绿。
            let junk: &[u8] = if i % 2 == 0 {
                b"\x00\xff not utf-8"
            } else {
                b"not toml [[["
            };
            std::fs::write(record_path(dir.path(), &format!("{i}b-bad")), junk).unwrap();
        }
        let got = list_records(dir.path(), 9999);
        assert_eq!(got.len(), 8, "坏记录把好记录也带走了");
        assert!(got.iter().all(|e| e.id.ends_with("a-good")));
    }

    /// 列表按 `updated_at` **倒序** —— 最近的现场在最上面。
    #[test]
    fn records_come_back_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        save_record(dir.path(), "a", &layout_at(1000, "old")).unwrap();
        save_record(dir.path(), "b", &layout_at(3000, "new")).unwrap();
        save_record(dir.path(), "c", &layout_at(2000, "mid")).unwrap();
        let ids: Vec<_> = list_records(dir.path(), 9999)
            .into_iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(ids, vec!["b", "c", "a"]);
    }

    /// 心跳新鲜的记录标成 `alive` —— 调用方据此把它排除在恢复列表之外
    /// (正在用的现场没必要出现在别人的列表里)。
    #[test]
    fn a_record_with_a_fresh_heartbeat_is_marked_alive() {
        let dir = tempfile::tempdir().unwrap();
        save_record(dir.path(), "live", &layout_at(1000, "x")).unwrap();
        save_record(dir.path(), "dead", &layout_at(1000, "y")).unwrap();
        touch_alive(dir.path(), "live", 10_000).unwrap();
        touch_alive(dir.path(), "dead", 10_000 - ALIVE_GRACE_SECS - 1).unwrap();
        let got = list_records(dir.path(), 10_000);
        let live = got.iter().find(|e| e.id == "live").unwrap();
        let dead = got.iter().find(|e| e.id == "dead").unwrap();
        assert!(
            live.alive,
            "心跳新鲜却被判死 —— 那个窗口的现场会被别人克隆走"
        );
        assert!(!dead.alive, "心跳早停了还判活 —— 那条记录会被永久隐藏");
    }

    /// `.alive` 心跳、`write_atomic` 崩在 rename 之前留下的 `.tmp` 残骸,
    /// 都不是记录。
    ///
    /// **那个 `.tmp` 里放的是合法 TOML**,不是垃圾:放垃圾的话,扩展名过滤
    /// 被删掉之后它照样会栽在下游 `toml::from_str` 上被跳过 —— 两道防御叠着,
    /// 测试一道也压不住(「冗余防御让变异恒绿」)。
    #[test]
    fn only_toml_files_count_as_records() {
        let dir = tempfile::tempdir().unwrap();
        save_record(dir.path(), "7-1", &layout_at(1000, "real")).unwrap();
        touch_alive(dir.path(), "7-1", 1000).unwrap();
        std::fs::write(
            history_dir(dir.path()).join("7-9.tmp"),
            toml::to_string_pretty(&layout_at(2000, "残骸")).unwrap(),
        )
        .unwrap();
        assert_eq!(list_records(dir.path(), 9999).len(), 1);
    }

    /// 更新版本写出来的记录**不猜**:字段可能改了含义,按现版本读出来的现场
    /// 是错的,而错的现场比没有现场更难排查(同 `layout::load` 的姿态)。跳过
    /// 它,别的记录照常出现。
    ///
    /// 绕开 `save_record` 直接写文件 —— `save_record` 会把 `schema_version`
    /// 强行盖回当前版本,造不出「未来版本的记录」。
    #[test]
    fn a_record_from_a_newer_schema_is_skipped_instead_of_guessed() {
        let dir = tempfile::tempdir().unwrap();
        save_record(dir.path(), "ok", &layout_at(1000, "now")).unwrap();
        let mut future = layout_at(2000, "future");
        future.schema_version = crate::layout::CURRENT_LAYOUT_SCHEMA + 1;
        std::fs::write(
            record_path(dir.path(), "future"),
            toml::to_string_pretty(&future).unwrap(),
        )
        .unwrap();
        let ids: Vec<_> = list_records(dir.path(), 9999)
            .into_iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(ids, vec!["ok"], "未来版本的记录被当成现版本读进来了");
    }

    /// 造 n 条条目,`alive` 按 `alive_ids` 决定,`updated_at` 递增(所以列表
    /// 倒序后 `id` 越大越靠前)。
    fn entries(n: usize, alive_ids: &[usize]) -> Vec<HistoryEntry> {
        let mut v: Vec<_> = (0..n)
            .map(|i| HistoryEntry {
                id: format!("{i}"),
                layout: layout_at(1000 + i as i64, "x"),
                alive: alive_ids.contains(&i),
            })
            .collect();
        v.sort_by_key(|e| std::cmp::Reverse(e.layout.updated_at));
        v
    }

    /// 不到上限就一个都不删。
    #[test]
    fn nothing_is_dropped_while_under_the_cap() {
        assert!(plan_prune(&entries(MAX_RECORDS, &[])).is_empty());
    }

    /// 超了就删**最老的那几条**,不是最新的。
    ///
    /// 自证会变红:把 `plan_prune` 里的 `.skip(MAX_RECORDS)` 改成 `.take(..)`。
    #[test]
    fn the_oldest_records_are_the_ones_dropped() {
        let doomed = plan_prune(&entries(MAX_RECORDS + 3, &[]));
        assert_eq!(
            doomed,
            vec!["2".to_string(), "1".to_string(), "0".to_string()]
        );
    }

    /// **活着的一条都不删**,而且它们**不占名额** —— 否则「同时开着 10 个
    /// 窗口」会把全部历史删光,而用户要的「最后 10 条记录」指的是已经关掉的现场。
    ///
    /// 自证会变红:把 `plan_prune` 里的 `.filter(|e| !e.alive)` 删掉。
    #[test]
    fn live_instances_are_never_pruned_and_do_not_eat_the_quota() {
        // 12 条,其中最新的 5 条活着 → 死的只有 7 条,一条都不该删。
        let all_alive_newest: Vec<usize> = (7..12).collect();
        let doomed = plan_prune(&entries(12, &all_alive_newest));
        assert!(
            doomed.is_empty(),
            "活着的占了名额,把好好的历史删了:{doomed:?}"
        );
    }

    /// 活着的不占名额,但死的仍然按 10 条封顶。
    #[test]
    fn dead_records_are_still_capped_when_live_ones_are_around() {
        // 14 条,最新的 2 条活着 → 死的 12 条,该删掉最老的 2 条。
        let doomed = plan_prune(&entries(14, &[12, 13]));
        assert_eq!(doomed, vec!["1".to_string(), "0".to_string()]);
    }

    /// 端到端:裁剪真的把文件(连心跳一起)从盘上删掉。
    #[test]
    fn pruning_actually_removes_the_files_and_their_heartbeats() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(MAX_RECORDS + 2) {
            save_record(
                dir.path(),
                &format!("{i}"),
                &layout_at(1000 + i as i64, "x"),
            )
            .unwrap();
            touch_alive(dir.path(), &format!("{i}"), 0).unwrap(); // 全是老心跳 = 全死
        }
        let dropped = prune(dir.path(), 9999);
        assert_eq!(dropped, 2);
        assert!(!record_path(dir.path(), "0").exists(), "记录文件没删掉");
        assert!(
            !alive_path(dir.path(), "0").exists(),
            "心跳文件没跟着删 —— 目录会越攒越多"
        );
        assert!(record_path(dir.path(), "11").exists(), "最新的那条被误删了");
        assert_eq!(list_records(dir.path(), 9999).len(), MAX_RECORDS);
    }

    /// 老的 `layout.toml` 变成一条记录,原文件被删掉(设计 D14)。
    #[test]
    fn the_legacy_layout_file_becomes_a_record_and_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        crate::layout::save(dir.path(), &layout_at(0, "从前的现场")).unwrap();
        let id = migrate_legacy(dir.path(), "legacy-1", 1_755_000_000);
        assert_eq!(id, Some("legacy-1".to_string()));
        assert!(
            !dir.path().join(crate::layout::LAYOUT_FILE).exists(),
            "老文件没删 —— 每次启动都会再迁移一遍,历史里会长出一堆重复"
        );
        let got = list_records(dir.path(), 9999);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].layout.tabs[0].title, "从前的现场");
    }

    /// 老文件没有 `updated_at`(v1 格式),迁移时补上调用方给的时刻 ——
    /// 留 0 的话它会永远排在列表最底下、并且第一个被裁掉。
    ///
    /// 自证会变红:把 `migrate_legacy` 里 `layout.updated_at = now;` 删掉。
    #[test]
    fn a_migrated_record_gets_a_timestamp_instead_of_staying_at_zero() {
        let dir = tempfile::tempdir().unwrap();
        crate::layout::save(dir.path(), &layout_at(0, "x")).unwrap();
        migrate_legacy(dir.path(), "legacy-1", 1_755_000_000);
        assert_eq!(
            list_records(dir.path(), 9999)[0].layout.updated_at,
            1_755_000_000
        );
    }

    /// 没有老文件 = 什么都不做,**不报错**。升级之后的每一次启动走的都是这条。
    #[test]
    fn migrating_without_a_legacy_file_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(migrate_legacy(dir.path(), "legacy-1", 1000), None);
        assert!(list_records(dir.path(), 9999).is_empty());
    }

    /// 老文件是空布局(一个标签都没有)→ 不值得迁,但**照样删掉**它,
    /// 免得每次启动都来读一遍。
    #[test]
    fn an_empty_legacy_file_is_deleted_without_creating_a_record() {
        let dir = tempfile::tempdir().unwrap();
        crate::layout::save(dir.path(), &SavedLayout::empty()).unwrap();
        assert_eq!(migrate_legacy(dir.path(), "legacy-1", 1000), None);
        assert!(!dir.path().join(crate::layout::LAYOUT_FILE).exists());
        assert!(list_records(dir.path(), 9999).is_empty());
    }

    /// 老文件坏了 → 删掉、不迁,**不阻断启动**(同 `layout::load` 的姿态)。
    #[test]
    fn a_corrupt_legacy_file_is_dropped_without_blocking_startup() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(
            dir.path().join(crate::layout::LAYOUT_FILE),
            b"\x00 not toml [[[",
        )
        .unwrap();
        assert_eq!(migrate_legacy(dir.path(), "legacy-1", 1000), None);
        assert!(!dir.path().join(crate::layout::LAYOUT_FILE).exists());
    }
}
