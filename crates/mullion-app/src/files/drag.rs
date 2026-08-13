//! 拖拽的**判据**(F58 栏间拖 / F52 从资源管理器拖入)。
//!
//! 零 egui、零 winit —— egui 那层只负责「把 payload 挂到行上」和「松手时把
//! 落点读回来」,「这一拖成不成立 / 落到哪个目录 / 拖进来的一批路径怎么摊成
//! 上传批次」全在这里。拖拽是**手上没有第二次确认机会**的动作(松手即传),
//! 判据留在渲染闭包里就只能靠人眼在真机上试。

use std::path::PathBuf;

use mullion_ssh::sftp::RemotePath;

use crate::files::PanelColumn;

/// 一次栏间拖拽的载荷。**只带「从哪栏拖的」**。
///
/// 拖的是哪几条**不进** payload:源栏的选中集是 `PaneState` 里的真值,松手
/// 那一刻现取即可。抄一份进 payload 会让「拖动途中选中集变了」出现两个互相
/// 矛盾的答案,而用户看到的是屏幕上的高亮(即真值那一份)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DragFrom(pub PanelColumn);

/// 松手落在哪儿。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Landing {
    /// 目标栏的**当前目录**(落在空白处、列头、路径条上)。
    Cwd,
    /// 当前目录下的**子目录**(松手那一刻指针正压在这个目录行上)。
    Sub(Vec<u8>),
}

/// 收到「有人拖过来了」的那一栏决定方向:远端栏收到 = 上传(东西是从本地
/// 拖过来的),本地栏收到 = 下载。
///
/// 判据单独立在这里、**全项目只有一处**:它跟 `FileAction::Transfer` 的方向
/// 规则(「把我这栏选中的送出去」)长得像但含义相反,两条极容易在维护里被
/// 抹平成一条。抹平的后果是拖一次就把远端那份覆盖成本地的(或反过来),
/// 而拖拽没有二次确认。
pub fn direction_for_drop(onto: PanelColumn) -> crate::files::queue::Direction {
    use crate::files::queue::Direction;
    match onto {
        PanelColumn::Remote => Direction::Upload,
        PanelColumn::Local => Direction::Download,
    }
}

impl Landing {
    /// 目标目录相对目标栏当前目录的子目录名。`None` = 就落在当前目录。
    pub fn sub(self) -> Option<Vec<u8>> {
        match self {
            Landing::Cwd => None,
            Landing::Sub(name) => Some(name),
        }
    }
}

/// 从 `from` 栏拖到 `onto` 栏、松手时指针底下是 `over_dir`,该传到哪儿。
///
/// `None` = 这一拖不成立。
///
/// `over_dir`:松手那一刻指针底下那一行的名字,**且那一行是目录**。文件行与
/// 空白都给 `None` —— 落在文件上不解释成「覆盖那个文件」:拖拽没有二次确认,
/// 一个手抖就能把远端文件盖掉,而「覆盖某个具体文件」这个需求本来就罕见。
pub fn drop_target(
    from: PanelColumn,
    onto: PanelColumn,
    over_dir: Option<Vec<u8>>,
) -> Option<Landing> {
    // 同栏内拖 = 移动/改名,本切片不做。**必须在这里挡住**:放过去的话,
    // 远端栏内把 a 拖到 b 会变成「把 a 上传到远端 b」,即自己传给自己 ——
    // 源和目标同一个路径,`open_write` 截断在前、读在后,文件直接清零。
    if from == onto {
        return None;
    }
    Some(match over_dir {
        Some(name) => Landing::Sub(name),
        None => Landing::Cwd,
    })
}

/// 把拖进窗口的一批**绝对路径**按父目录分组,每组给 `(父目录, 名字列表)`。
///
/// 为什么要分组:既有的 `plan_transfer` 收的是「相对于某个本地目录的名字」
/// (右键「上传到远端」那条路径本来就只可能来自本地栏那一个目录)。拖进来的
/// 一批却可能横跨好几个目录。分组之后逐组喂给同一个 `plan_transfer`,
/// **上传那条通路一个字不用改** —— 改它等于同时动右键上传那条已经验过的路。
///
/// 顺序稳定(按父目录首次出现的先后):拖 5 个文件进来,队列里的顺序要跟
/// 用户框选的顺序对得上。用 `HashMap` 收集再迭代的话,每次拖同一批文件
/// 队列顺序都不一样,像是程序自己在乱动。
///
/// 没有文件名的路径(盘符根 `C:\`、Unix 的 `/`)直接跳过 —— 它没有「名字」
/// 可以拼到远端目录下面,收进来只会变成一条必然失败的 job。
pub fn group_by_parent(paths: &[PathBuf]) -> Vec<(PathBuf, Vec<Vec<u8>>)> {
    let mut out: Vec<(PathBuf, Vec<Vec<u8>>)> = Vec::new();
    for p in paths {
        let (Some(parent), Some(name)) = (p.parent(), p.file_name()) else {
            continue;
        };
        let name = super::local::os_bytes(name);
        match out.iter_mut().find(|(d, _)| d == parent) {
            Some((_, names)) => names.push(name),
            None => out.push((parent.to_path_buf(), vec![name])),
        }
    }
    out
}

/// 拖入悬停时贴在远端栏顶上的那句话(设计 D9:**规则先于动作可见**)。
///
/// winit 0.30 的 `DroppedFile` 不带坐标、Windows 在 OLE 拖放期间不发
/// `CursorMoved`,落点判不了,所以规则是「扔在窗口哪儿都一样」。既然规则
/// 反直觉,它就必须在用户**松手之前**写在屏幕上,而不是松手之后用一条
/// toast 通知他东西传到别处去了。
pub fn drop_in_hint(remote_cwd: &RemotePath, n: usize) -> String {
    format!("松开上传 {n} 项到 {}", remote_cwd.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dragging_within_the_same_column_is_not_a_transfer() {
        // 远端栏内部把一条拖到另一条 —— 那是移动/改名,本切片不做。放过去
        // 就是「把远端文件上传到它自己」,截断在读之前,文件会被清零。
        assert_eq!(
            drop_target(PanelColumn::Remote, PanelColumn::Remote, None),
            None
        );
        assert_eq!(
            drop_target(
                PanelColumn::Local,
                PanelColumn::Local,
                Some(b"sub".to_vec())
            ),
            None
        );
    }

    #[test]
    fn dropping_on_a_directory_row_targets_that_subdirectory() {
        assert_eq!(
            drop_target(
                PanelColumn::Local,
                PanelColumn::Remote,
                Some(b"logs".to_vec())
            ),
            Some(Landing::Sub(b"logs".to_vec()))
        );
    }

    #[test]
    fn dropping_on_blank_space_targets_the_current_directory() {
        assert_eq!(
            drop_target(PanelColumn::Remote, PanelColumn::Local, None),
            Some(Landing::Cwd)
        );
    }

    #[test]
    fn dropped_paths_are_grouped_by_their_parent_directory() {
        let g = group_by_parent(&[
            PathBuf::from("/a/x.txt"),
            PathBuf::from("/b/y.txt"),
            PathBuf::from("/a/z.txt"),
        ]);
        assert_eq!(g.len(), 2, "两个父目录 → 两组,不是三组");
        assert_eq!(g[0].0, PathBuf::from("/a"), "顺序按父目录首次出现");
        assert_eq!(
            g[0].1,
            vec![b"x.txt".to_vec(), b"z.txt".to_vec()],
            "同一个父目录下的两条并进一组,组内保持拖入顺序"
        );
        assert_eq!(g[1].0, PathBuf::from("/b"));
    }

    #[test]
    fn a_dropped_path_without_a_file_name_is_skipped_rather_than_uploaded_to_nowhere() {
        // 盘符根 / 文件系统根:有父目录没名字(或两者都没有)。拼不出远端
        // 落点,收进来就是一条必然失败的 job。
        let g = group_by_parent(&[PathBuf::from("/"), PathBuf::from("/a/ok.txt")]);
        assert_eq!(g.len(), 1, "只剩下有名字的那一条");
        assert_eq!(g[0].1, vec![b"ok.txt".to_vec()]);
    }

    #[test]
    fn a_drop_onto_the_remote_column_uploads_rather_than_downloads() {
        // 方向弄反 = 拖一次就把远端那份覆盖成本地的(或反过来),而拖拽
        // 没有二次确认。`app.rs` 的两条接线都从这里取方向,不各自硬编码。
        use crate::files::queue::Direction;
        assert_eq!(direction_for_drop(PanelColumn::Remote), Direction::Upload);
        assert_eq!(direction_for_drop(PanelColumn::Local), Direction::Download);
    }

    #[test]
    fn the_hover_hint_names_the_directory_that_will_receive_the_files() {
        // 落点判不了这条规则要在松手**之前**说出来,而且要带上具体目录 ——
        // 只说「松开上传」的话,用户没法知道传到哪去了。
        let h = drop_in_hint(&RemotePath::from_bytes(b"/srv/www".to_vec()), 3);
        assert!(h.contains("/srv/www"), "必须写出目标目录:{h}");
        assert!(h.contains('3'), "必须写出条数:{h}");
    }
}
