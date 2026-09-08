//! F224:**本机跨实例**的「哪些项目正开着」。零 UI、零 async、纯同步 IO。
//!
//! 一个 exe 实例 = `<config_dir>/projects/<实例id>.alive` 一个文件,
//! **每个进程只写自己那一个,从不改别人的** —— 理由与 F148 的历史文件完全
//! 相同(见 `history.rs` 模块文档:共享一个文件就得上文件锁,而那是新依赖
//! 加上我们在无头容器里验证不了的 Windows 行为)。
//!
//! 活性判定直接复用 F148 的 [`crate::history::is_alive`] 与那两个常量,
//! **不在这里重新定一套阈值** —— 两套阈值必然会漂移,而漂移的症状是
//! 「灯偶尔亮偶尔不亮」,查起来极贵。
//!
//! 多开是本项目的主场景:只看自己这个窗口的 pane 的话,另一个窗口里正跑着的
//! 项目在这边显示为「灭」,用户会去开第二份 —— 同一个目录两个 Claude Code。

use std::path::{Path, PathBuf};

use crate::error::StoreError;

/// 存放目录名(在 `config_dir` 下)。
pub const PRESENCE_DIR: &str = "projects";

/// 文件扩展名。与 F148 的心跳同名,但在**另一个目录**下,互不干扰。
pub const PRESENCE_EXT: &str = "alive";

/// `<dir>/projects`。
pub fn presence_dir(dir: &Path) -> PathBuf {
    dir.join(PRESENCE_DIR)
}

/// 某个实例的在场文件。
pub fn presence_path(dir: &Path, id: &str) -> PathBuf {
    presence_dir(dir).join(format!("{id}.{PRESENCE_EXT}"))
}

/// 文件内容:**第一行是 Unix 秒,其余每行一个 tmux 名**。
///
/// 名字里不可能出现换行:能走到这里的名字都过了
/// [`crate::automation::sanitize_tmux_name`],控制字符已被滤掉。这条前提
/// 若哪天不成立,症状是一个名字被拆成两行(多点亮一盏灯),有界。
///
/// `None` = 空文件 / 第一行不是数字。一律当**没有这条在场记录**处置。
pub fn parse(text: &str) -> Option<(i64, Vec<String>)> {
    let mut lines = text.lines();
    let at = lines.next()?.trim().parse::<i64>().ok()?;
    let names = lines
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    Some((at, names))
}

/// 写自己这一份。
///
/// **不是 `write_atomic`**,理由同 F148 的心跳:写一半的后果是下一次读回
/// `None`(少亮一盏灯),而这个文件每 15 秒就再写一次。
pub fn publish(dir: &Path, id: &str, now_secs: i64, names: &[String]) -> Result<(), StoreError> {
    std::fs::create_dir_all(presence_dir(dir))?;
    let mut text = now_secs.to_string();
    for n in names {
        text.push('\n');
        text.push_str(n);
    }
    std::fs::write(presence_path(dir, id), text.as_bytes())?;
    Ok(())
}

/// **别的**实例此刻报出来的 tmux 名(去重后)。
///
/// `self_id` 那份被跳过 —— 把自己的也算进来的话,「灯亮」会退化成「我们自己
/// 刚写过一次」,与 pane 上报那条路重复,且在自己的 pane 已经断开、文件还没
/// 到期的 45 秒里给出假亮。
///
/// 过期的文件在这里**只是跳过、不删** —— 删除是启动时 [`sweep`] 的事。
/// 每帧都去删别人的文件,会与那个实例自己的写入撞上。
pub fn read_others(dir: &Path, self_id: &str, now_secs: i64) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(presence_dir(dir)) else {
        // 目录不存在 = 还没有任何实例发布过,正常情况。
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for e in rd.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some(PRESENCE_EXT) {
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
        let Some((at, names)) = parse(&text) else {
            continue;
        };
        if !crate::history::is_alive(now_secs, at) {
            continue;
        }
        for n in names {
            if !out.contains(&n) {
                out.push(n);
            }
        }
    }
    out
}

/// 启动时清掉过期的在场文件。
///
/// 误判的后果两边都有界:多删 = 那个实例下次心跳自己写回来;少删 = 目录里
/// 多几个文件,下次启动再删。**自己那份不清** —— 我们马上就要写它。
pub fn sweep(dir: &Path, self_id: &str, now_secs: i64) {
    let Ok(rd) = std::fs::read_dir(presence_dir(dir)) else {
        return;
    };
    for e in rd.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some(PRESENCE_EXT) {
            continue;
        }
        if path.file_stem().and_then(|x| x.to_str()) == Some(self_id) {
            continue;
        }
        let expired = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| parse(&t))
            .is_none_or(|(at, _)| !crate::history::is_alive(now_secs, at));
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
        publish(d.path(), "a", 1000, &["proj-x".into(), "proj-y".into()]).unwrap();
        assert_eq!(
            read_others(d.path(), "b", 1000),
            vec!["proj-x".to_string(), "proj-y".to_string()]
        );
    }

    /// **自己那份不算数。** 算进来的话,「灯亮」会退化成「我们自己刚写过」,
    /// 并且在自己的 pane 已经断开、文件还没到期的那 45 秒里给出假亮。
    #[test]
    fn our_own_file_is_not_evidence_that_someone_else_is_running_it() {
        let d = dir();
        publish(d.path(), "a", 1000, &["proj-x".into()]).unwrap();
        assert!(read_others(d.path(), "a", 1000).is_empty());
    }

    /// 活性判定**复用 F148 的那条**,不另定阈值。
    ///
    /// 自证会变红:把 `read_others` 里的 `is_alive` 判断删掉。
    #[test]
    fn a_stale_file_stops_lighting_the_lamp_once_the_grace_period_is_over() {
        let d = dir();
        publish(d.path(), "a", 1000, &["proj-x".into()]).unwrap();
        let grace = crate::history::ALIVE_GRACE_SECS;
        assert_eq!(
            read_others(d.path(), "b", 1000 + grace),
            vec!["proj-x".to_string()],
            "刚好在宽限期内还得算活着"
        );
        assert!(
            read_others(d.path(), "b", 1000 + grace + 1).is_empty(),
            "过了宽限期就不该再点灯"
        );
    }

    /// 两个实例都开着同一个项目时只报一次 —— 调用方拿它做 `contains`,
    /// 重复项不会出错,但会让日志和将来的「几个人在用」计数难读。
    #[test]
    fn the_same_project_reported_by_two_instances_appears_once() {
        let d = dir();
        publish(d.path(), "a", 1000, &["proj-x".into()]).unwrap();
        publish(d.path(), "b", 1000, &["proj-x".into()]).unwrap();
        assert_eq!(read_others(d.path(), "c", 1000), vec!["proj-x".to_string()]);
    }

    /// 坏文件当「没有这条记录」,不是 panic —— 用户手改过、或上次写到一半。
    #[test]
    fn a_corrupt_file_is_skipped_instead_of_taking_the_client_down() {
        let d = dir();
        std::fs::create_dir_all(presence_dir(d.path())).unwrap();
        std::fs::write(presence_path(d.path(), "a"), b"\xff\xfe not text").unwrap();
        std::fs::write(presence_path(d.path(), "b"), "不是数字\nproj-x").unwrap();
        publish(d.path(), "c", 1000, &["proj-z".into()]).unwrap();
        assert_eq!(read_others(d.path(), "self", 1000), vec!["proj-z"]);
    }

    /// 一个项目都没开着时也要**照常写文件** —— 不写的话,上一轮那份带名字的
    /// 旧文件会在宽限期内继续点灯,用户看着一盏灯亮了 45 秒才灭。
    #[test]
    fn publishing_an_empty_list_actively_clears_what_we_reported_before() {
        let d = dir();
        publish(d.path(), "a", 1000, &["proj-x".into()]).unwrap();
        publish(d.path(), "a", 1010, &[]).unwrap();
        assert!(read_others(d.path(), "b", 1010).is_empty());
    }

    /// 启动清扫:过期的删掉,活着的和自己那份留着。
    #[test]
    fn startup_sweeps_expired_files_but_keeps_live_ones_and_our_own() {
        let d = dir();
        let grace = crate::history::ALIVE_GRACE_SECS;
        publish(d.path(), "dead", 1000 - grace - 1, &["x".into()]).unwrap();
        publish(d.path(), "live", 1000, &["y".into()]).unwrap();
        publish(d.path(), "me", 1000 - grace - 1, &["z".into()]).unwrap();

        sweep(d.path(), "me", 1000);

        assert!(!presence_path(d.path(), "dead").exists(), "过期的该删");
        assert!(presence_path(d.path(), "live").exists(), "活着的不许删");
        assert!(
            presence_path(d.path(), "me").exists(),
            "自己那份不许删 —— 马上就要往里写"
        );
    }

    #[test]
    fn parsing_rejects_a_first_line_that_is_not_a_timestamp() {
        assert_eq!(parse("1000\nproj-x"), Some((1000, vec!["proj-x".into()])));
        assert_eq!(parse("1000"), Some((1000, Vec::new())));
        assert_eq!(parse(""), None);
        assert_eq!(parse("proj-x"), None);
    }
}
