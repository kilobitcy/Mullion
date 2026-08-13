//! 文件面板的**纯逻辑**:排序、隐藏文件过滤、列宽档位。
//! 零 egui / 零 tokio / 零 IO —— 渲染在 `ui::files_panel`,协议在
//! `mullion_ssh::sftp`。这么切是为了让「点了列头之后顺序对不对」
//! 这类 bug 能在没有窗口的情况下写测试复现。

use mullion_ssh::sftp::{Entry, EntryKind};

pub mod drag;
pub mod local;
pub mod queue;
pub mod state;
pub mod transfer;

/// 面板里的两栏之一(F50)。**判据类型,不是显示用的标签** —— 「哪些操作
/// 可用」(D5:写操作只在远端栏)、「拖过来是上传还是下载」(F58)都按它分流。
///
/// 住在 `files` 而不是 `ui::files_panel`:`files` 是纯逻辑层,`ui` 依赖它、
/// 反过来不成立(同 `state.rs`/`queue.rs`)。`drag.rs` 的落点判据要拿它当
/// 参数,定义留在 ui 里的话,纯逻辑层就得反向依赖渲染层。
/// `ui::files_panel` 里有一条 `pub use` 重导出,老的引用路径照样能用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelColumn {
    /// 远端为默认——打开一个 SFTP 标签,用户第一件事多半是看远端目录。
    #[default]
    Remote,
    Local,
}

impl PanelColumn {
    /// `Tab` 在两栏之间来回(设计 D23:F6/Tab 换焦点)。
    pub fn flipped(self) -> Self {
        match self {
            PanelColumn::Remote => PanelColumn::Local,
            PanelColumn::Local => PanelColumn::Remote,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Size,
    Mtime,
    Perm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

impl SortDir {
    pub fn flipped(self) -> Self {
        match self {
            SortDir::Asc => SortDir::Desc,
            SortDir::Desc => SortDir::Asc,
        }
    }
}

/// 就地排序。**目录恒在前**,倒序只翻同组内部的顺序(设计 D21)——
/// 把目录一起翻到底下从来不是任何人想要的。
pub fn sort(entries: &mut [Entry], key: SortKey, dir: SortDir) {
    entries.sort_by(|a, b| {
        let group = is_dir(b).cmp(&is_dir(a)); // 目录(true)排前
        if group != std::cmp::Ordering::Equal {
            return group;
        }
        let ord = match key {
            // 不分大小写:分了的话 `Gamma` 会跑到 `alpha` 前面。
            SortKey::Name => a
                .name
                .display()
                .to_lowercase()
                .cmp(&b.name.display().to_lowercase()),
            SortKey::Size => a.size.cmp(&b.size),
            SortKey::Mtime => a.mtime.cmp(&b.mtime),
            // 与 `perm_string` 画出来的那 9 位对齐。按整个 mode 排的话,
            // 两行显示一模一样的 `rwxr-xr-x` 会排不到一起(其中一个带
            // setuid),用户找不出原因。
            SortKey::Perm => (a.mode & 0o777).cmp(&(b.mode & 0o777)),
        };
        match dir {
            SortDir::Asc => ord,
            SortDir::Desc => ord.reverse(),
        }
    });
}

fn is_dir(e: &Entry) -> bool {
    e.kind == EntryKind::Dir
}

/// 过滤隐藏项(`.` 开头)。`show_hidden` 为真时原样返回。
pub fn visible(entries: &[Entry], show_hidden: bool) -> Vec<&Entry> {
    entries
        .iter()
        .filter(|e| show_hidden || !e.name.as_bytes().starts_with(b"."))
        .collect()
}

/// `1.5 MB` 这种。1024 进制,一位小数;不足 1 KB 直接给字节数
/// (`0.9 KB` 比 `920 B` 难读)。
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64 / 1024.0;
    let mut unit = 0;
    // 进不进位要看**四舍五入之后**的值。直接比 `v >= 1024.0` 的话,
    // 1048575 B 除下来是 1023.999…、不进位,可 `{:.1}` 又把它印成
    // 「1024.0 KB」—— 一个该进没进的数,看着就是个 bug。1 MB 上下的
    // 日志和压缩包在真实目录里遍地都是,不是理论边界。
    while round_to_one_decimal(v) >= 1024.0 && unit + 1 < UNITS.len() {
        v /= 1024.0;
        unit += 1;
    }
    format!("{v:.1} {}", UNITS[unit])
}

/// `{:.1}` 会得到的那个值。判进位与印出来的必须是同一套标准。
fn round_to_one_decimal(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// `rwxr-xr-x`。**只看低 9 位**:类型位已经在 `EntryKind` 里了,
/// 再画一遍就成了 `drwx…` 那种把同一件事说两遍的写法。
pub fn perm_string(mode: u32) -> String {
    let bits = mode & 0o777;
    let mut s = String::with_capacity(9);
    for shift in [6, 3, 0] {
        let g = (bits >> shift) & 0o7;
        s.push(if g & 0o4 != 0 { 'r' } else { '-' });
        s.push(if g & 0o2 != 0 { 'w' } else { '-' });
        s.push(if g & 0o1 != 0 { 'x' } else { '-' });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_ssh::sftp::{Entry, EntryKind, RemotePath};

    fn e(name: &str, kind: EntryKind, size: u64, mtime: u32) -> Entry {
        Entry {
            name: RemotePath::from_bytes(name.as_bytes().to_vec()),
            kind,
            size,
            mtime,
            mode: 0o644,
            uid: 1000,
            gid: 1000,
            link_target: None,
        }
    }

    fn sample() -> Vec<Entry> {
        vec![
            e("zeta.txt", EntryKind::File, 10, 300),
            e("alpha", EntryKind::Dir, 4096, 100),
            e(".hidden", EntryKind::File, 1, 400),
            e("beta.txt", EntryKind::File, 5000, 200),
            e("Gamma", EntryKind::Dir, 4096, 500),
        ]
    }

    fn names(v: &[Entry]) -> Vec<String> {
        v.iter().map(|e| e.name.display().to_string()).collect()
    }

    /// 默认序:目录在前 + 名称升序(设计 D21)。
    #[test]
    fn the_default_order_puts_directories_first_then_names_ascending() {
        let mut v = sample();
        sort(&mut v, SortKey::Name, SortDir::Asc);
        assert_eq!(
            names(&v),
            vec!["alpha", "Gamma", ".hidden", "beta.txt", "zeta.txt"]
        );
    }

    /// 名称排序**不分大小写** —— 分了的话 `Gamma` 会跑到 `alpha` 前面,
    /// 用户眼里就是「排序坏了」。
    #[test]
    fn name_sorting_ignores_case_so_uppercase_does_not_jump_to_the_top() {
        let mut v = sample();
        sort(&mut v, SortKey::Name, SortDir::Asc);
        let dirs: Vec<String> = names(&v).into_iter().take(2).collect();
        assert_eq!(dirs, vec!["alpha", "Gamma"]);
    }

    /// 倒序**只翻名字,不翻「目录在前」** —— 目录跑到最底下不是任何人
    /// 想要的,那只是排序实现偷懒的副作用。
    #[test]
    fn reversing_the_order_keeps_directories_on_top() {
        let mut v = sample();
        sort(&mut v, SortKey::Name, SortDir::Desc);
        assert_eq!(
            names(&v),
            vec!["Gamma", "alpha", "zeta.txt", "beta.txt", ".hidden"]
        );
    }

    #[test]
    fn sorting_by_size_still_keeps_directories_on_top() {
        let mut v = sample();
        sort(&mut v, SortKey::Size, SortDir::Desc);
        let first_two: Vec<String> = names(&v).into_iter().take(2).collect();
        assert!(first_two.contains(&"alpha".to_string()));
        assert!(first_two.contains(&"Gamma".to_string()));
        assert_eq!(names(&v)[2], "beta.txt", "文件里最大的排最前");
    }

    #[test]
    fn hidden_entries_are_dropped_unless_asked_for() {
        let v = sample();
        assert_eq!(visible(&v, false).len(), 4);
        assert_eq!(visible(&v, true).len(), 5);
    }

    /// 按时间排序真的看 `mtime`,不是看名字。
    ///
    /// 这个位置原先放的是「同一输入排两次结果一致」—— 那条**恒绿**:
    /// 任何确定性函数都满足它,把 `key` 参数整个忽略掉照样通过,一点
    /// 区分力都没有。换成这条,忽略 key 就变红。
    #[test]
    fn sorting_by_mtime_orders_by_time_not_by_name() {
        let mut v = sample();
        sort(&mut v, SortKey::Mtime, SortDir::Asc);
        // 目录仍在前(alpha=100 早于 Gamma=500),文件按时间:
        // beta.txt=200 < zeta.txt=300 < .hidden=400。
        assert_eq!(
            names(&v),
            vec!["alpha", "Gamma", "beta.txt", "zeta.txt", ".hidden"]
        );
    }

    /// 权限排序只看低 9 位 —— 与 `perm_string` 画出来的那 9 位对齐。
    /// 不对齐的话,两行显示完全相同的 `rwxr-xr-x` 排不到一起,用户
    /// 只会觉得排序是坏的。
    #[test]
    fn permission_sorting_looks_at_the_same_nine_bits_the_column_shows() {
        let mut a = e("a.sh", EntryKind::File, 1, 1);
        a.mode = 0o4755; // setuid + rwxr-xr-x
        let mut b = e("b.sh", EntryKind::File, 1, 1);
        b.mode = 0o0755; // 同样画成 rwxr-xr-x
        assert_eq!(perm_string(a.mode), perm_string(b.mode));

        let mut v = vec![a, b];
        sort(&mut v, SortKey::Perm, SortDir::Asc);
        assert_eq!(
            names(&v),
            vec!["a.sh", "b.sh"],
            "低 9 位相同 → 稳定排序保持原序;不 mask 的话 setuid 那条会被排到后面"
        );
    }

    #[test]
    fn a_size_is_rendered_with_one_decimal_and_a_unit() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1024 * 1024 * 3 / 2), "1.5 MB");
        // 进位边界:1023.999… KB 印成一位小数就是「1024.0 KB」,
        // 一个该进没进的数。判进位与印出来必须用同一套舍入。
        assert_eq!(human_size(1024 * 1024 - 1), "1.0 MB");
        assert_eq!(human_size(1024 * 1024), "1.0 MB");
    }

    /// 权限画成 `rwxr-xr-x`。**只画低 9 位** —— 类型位在 kind 里,
    /// 混进来会变成 `drwx…` 那种把类型重复画两遍的写法。
    #[test]
    fn permissions_render_as_nine_characters() {
        assert_eq!(perm_string(0o755), "rwxr-xr-x");
        assert_eq!(perm_string(0o644), "rw-r--r--");
        assert_eq!(perm_string(0o40755 & 0o7777), "rwxr-xr-x");
    }
}
