//! 拖出到资源管理器(F59)。
//!
//! 分三层:
//!
//! - 本模块 —— **判据**:哪几条能拖、什么时候把手势交给操作系统。零平台代码。
//! - [`name`] / [`descriptor`] —— 落地名净化去重、`FILEGROUPDESCRIPTORW` 字节
//!   布局。同样零平台代码,这两块是这一片里**唯二能在无头环境验证**的东西。
//! - [`win`] —— Windows COM(`IDataObject`/`IStream`/`IDropSource` + 专用 STA
//!   线程)。`#[cfg(windows)]`,无头一行都验不了。
//!
//! 非 Windows 上 [`start`] 是个只记 warn 的空实现:macOS/Linux 的拖出协议
//! (`NSPasteboard` / XDND)完全不同,而它们在本项目里只要求「能编过」。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use mullion_ssh::sftp::{Entry, EntryKind, RemotePath};

use crate::files::state::PaneState;
use crate::files::PanelColumn;

pub mod descriptor;
pub mod name;
#[cfg(windows)]
pub mod win;

/// D12:全流程日志的 target。`MULLION_LOG=mullion::sftp::drag_out=debug` 打开。
/// 拖到某个程序里没反应时,这是**唯一**的诊断手段 —— 失败发生在别人的进程里。
pub const LOG: &str = "mullion::sftp::drag_out";

/// 一个要拖出去的文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DragOutItem {
    /// 远端**绝对**路径。
    pub remote: RemotePath,
    /// 落地名 —— **已经过 [`name::sanitize`] 与 [`name::unique`]**。
    pub name: String,
    pub size: u64,
}

/// 这一批选中项里,哪几条拖得出去;以及**跳过了几个目录**。
///
/// 只收普通文件(设计 N2):目录要另造一层描述符,而子项要在起拖那一刻就
/// 递归列完远端目录才知道有几项 —— 那是一次可能几十秒的网络往返,卡在手势里。
/// 跳过不是静默的,调用方要把跳过数说出来。
///
/// 名字送不上线的(`is_operable()` 为假)一并跳过,与删除/传输同一条判据 ——
/// 拼出来的路径请求发不出去,收进来只是一条必然失败的流。
pub fn items_for(state: &PaneState) -> (Vec<DragOutItem>, usize) {
    let picked: Vec<&Entry> = state.picked_entries();
    let mut skipped_dirs = 0usize;
    let mut usable: Vec<&Entry> = Vec::new();
    for e in picked {
        if e.kind == EntryKind::Dir {
            skipped_dirs += 1;
            continue;
        }
        if e.kind != EntryKind::File || !e.name.is_operable() {
            continue;
        }
        usable.push(e);
    }
    // 净化 → 去重必须**整批一起做**:去重是批内的语义,逐条处理看不见撞名。
    let landed = name::unique(
        &usable
            .iter()
            .map(|e| name::sanitize(&e.name.display()))
            .collect::<Vec<String>>(),
    );
    let items = usable
        .iter()
        .zip(landed)
        .map(|(e, name)| DragOutItem {
            remote: state.cwd.join(e.name.as_bytes()),
            name,
            size: e.size,
        })
        .collect();
    (items, skipped_dirs)
}

/// 什么时候把这一拖交给操作系统(设计 N1)。
///
/// - `from`:egui 拖拽载荷说这一拖是从哪一栏起的。`None` = 现在没在拖。
/// - `pointer_inside_window`:指针还在窗口里没有。
/// - `already_running`:已经有一条拖出线程在跑了。
///
/// **`!pointer_inside_window` 这一条是 F58 的命根子**:远端栏起拖的手势
/// 同时属于 F58(拖到本地栏 = 下载)。窗口内就交给 OS 的话,`DoDragDrop`
/// 会接管鼠标捕获,F58 的远端→本地方向当场失效。窗口内 = 内部传输,
/// 窗口外 = 交给系统。
pub fn should_hand_off(
    from: Option<PanelColumn>,
    pointer_inside_window: bool,
    already_running: bool,
) -> bool {
    from == Some(PanelColumn::Remote) && !pointer_inside_window && !already_running
}

/// 现在有没有一条拖出在跑。
///
/// 闸门放在这里而不是调用方,是因为**放闸的时机只有拖出线程知道** ——
/// `DoDragDrop` 是模态的,UI 线程 spawn 完就回去画帧了,它看不到什么时候结束。
static RUNNING: AtomicBool = AtomicBool::new(false);

/// 喂给 [`should_hand_off`] 的 `already_running`。
pub fn is_running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

/// 拖出线程退出时放闸。**正常退出和 panic 都要走到**,不然用户「拖过一次
/// 之后再也拖不动了」,而且没有任何提示。
pub(crate) fn finished() {
    RUNNING.store(false, Ordering::SeqCst);
}

/// 起一条拖出。**立刻返回** —— 真正的 `DoDragDrop` 在另一条线程里跑
/// (设计 D10:winit 的回调栈里起嵌套模态消息循环必 panic)。
///
/// 非 Windows 上只记一条 warn:macOS/Linux 的拖出协议完全不同,而本项目
/// 对它们只要求「能编过」。
#[cfg_attr(not(windows), allow(unused_variables))]
pub fn start(
    runtime: tokio::runtime::Handle,
    sftp: Arc<mullion_ssh::sftp::SftpClient>,
    items: Vec<DragOutItem>,
) {
    if items.is_empty() {
        return;
    }
    // 上闸和查闸必须是**同一个原子操作**:两次 `HoveredFile` 之间只隔几毫秒,
    // 先查后置的话两条 STA 线程会同时抢鼠标捕获。
    if RUNNING.swap(true, Ordering::SeqCst) {
        log::warn!(target: LOG, "已有一条拖出在跑,忽略这次({} 项)", items.len());
        return;
    }
    #[cfg(windows)]
    {
        win::start(runtime, sftp, items);
    }
    #[cfg(not(windows))]
    {
        log::warn!(
            target: LOG,
            "拖出只在 Windows 上实现(F59),本平台忽略 {} 项",
            items.len()
        );
        finished();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, kind: EntryKind, size: u64) -> Entry {
        Entry {
            name: RemotePath::from_bytes(name.as_bytes().to_vec()),
            kind,
            size,
            mtime: 0,
            mode: 0o644,
            uid: 0,
            gid: 0,
            link_target: None,
        }
    }

    fn state(entries: Vec<Entry>, pick: &[&str]) -> PaneState {
        let mut s = PaneState::new(RemotePath::from_bytes(b"/srv".to_vec()));
        s.entries = entries;
        for p in pick {
            s.selected
                .insert(RemotePath::from_bytes(p.as_bytes().to_vec()));
        }
        s
    }

    #[test]
    fn a_directory_in_the_selection_is_skipped_and_counted_rather_than_dragged() {
        // 目录要另造一层描述符,而子项要在起拖那一刻递归列完远端目录 ——
        // 那是一次可能几十秒的网络往返,卡在手势里。跳过,但**要说出来**。
        let s = state(
            vec![
                e("logs", EntryKind::Dir, 4096),
                e("a.txt", EntryKind::File, 7),
            ],
            &["logs", "a.txt"],
        );
        let (items, skipped) = items_for(&s);
        assert_eq!(skipped, 1, "跳过的目录数必须报出来,不能静默少一个");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "a.txt");
    }

    #[test]
    fn the_remote_path_is_absolute_so_the_stream_can_actually_open_it() {
        let s = state(vec![e("a.txt", EntryKind::File, 7)], &["a.txt"]);
        let (items, _) = items_for(&s);
        assert_eq!(items[0].remote.display(), "/srv/a.txt");
        assert_eq!(items[0].size, 7);
    }

    #[test]
    fn landing_names_are_sanitized_and_deduplicated_as_one_batch() {
        // `a:b` 与 `a?b` 净化后都是 `a_b`。逐条净化看不见撞名,资源管理器
        // 会拿后一个盖掉前一个 —— 用户「拖了 2 个下来只剩 1 个」。
        let s = state(
            vec![e("a:b", EntryKind::File, 1), e("a?b", EntryKind::File, 2)],
            &["a:b", "a?b"],
        );
        let (items, _) = items_for(&s);
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["a_b", "a_b (2)"]);
    }

    #[test]
    fn an_entry_whose_name_cannot_go_on_the_wire_is_not_dragged() {
        // 同删除/传输那条判据:拼出来的路径请求发不出去。
        let s = state(
            vec![
                Entry {
                    name: RemotePath::from_bytes(vec![0xff, 0xfe]),
                    ..e("x", EntryKind::File, 1)
                },
                e("ok.txt", EntryKind::File, 1),
            ],
            &[],
        );
        let mut s = s;
        s.selected.insert(RemotePath::from_bytes(vec![0xff, 0xfe]));
        s.selected
            .insert(RemotePath::from_bytes(b"ok.txt".to_vec()));
        let (items, _) = items_for(&s);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "ok.txt");
    }

    #[test]
    fn a_drag_that_is_still_inside_the_window_is_not_handed_to_the_os() {
        // **F58 的命根子**:远端栏起拖的手势同时属于「拖到本地栏 = 下载」。
        // 窗口内就交给 OS 的话,`DoDragDrop` 接管鼠标捕获,F58 的远端→本地
        // 方向当场失效。
        assert!(!should_hand_off(Some(PanelColumn::Remote), true, false));
        assert!(should_hand_off(Some(PanelColumn::Remote), false, false));
    }

    #[test]
    fn a_drag_that_started_in_the_local_column_is_never_handed_to_the_os() {
        // 本地文件拖到别处是资源管理器自己的事(设计 D5)。
        assert!(!should_hand_off(Some(PanelColumn::Local), false, false));
        assert!(!should_hand_off(None, false, false));
    }

    #[test]
    fn a_second_hand_off_is_refused_while_one_is_still_running() {
        // `DoDragDrop` 是模态的。重入会开出第二条 STA 线程,两条同时抢
        // 鼠标捕获。
        assert!(!should_hand_off(Some(PanelColumn::Remote), false, true));
    }
}
