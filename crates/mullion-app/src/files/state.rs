//! 一个文件栏的运行态。**零 egui**:导航语义(进目录/回上级/刷新)在这里
//! 写成纯状态机,于是「双击链接跟不跟随」「Backspace 会不会走出根」
//! 这类 bug 不需要窗口就能复现。

use mullion_ssh::sftp::{Entry, EntryKind, RemotePath};

use super::{SortDir, SortKey};

/// 这一栏当前在干什么。
#[derive(Debug, Clone, PartialEq)]
pub enum Load {
    /// 还没连上 / 还没发过第一次请求。
    Idle,
    Loading,
    Ready,
    /// 出错了,字符串是已经格式化好的可读原因。
    Failed(String),
}

pub struct PaneState {
    pub cwd: RemotePath,
    pub entries: Vec<Entry>,
    pub load: Load,
    pub sort_key: SortKey,
    pub sort_dir: SortDir,
    pub show_hidden: bool,
    /// 选中的是**哪一条**,不是「第几行」。
    ///
    /// 存下标会错行:点一次列头 `entries` 就重排了,`show_hidden` 一切
    /// 过滤结果也变了,下标却纹丝不动 —— 高亮会跳到另一个文件上。本切片
    /// 只是看着别扭,等操作接到选中项上就是**对错文件下手**。
    /// 存身份则天然自愈:那一条没了(被过滤掉、或刷新后不在了),
    /// 找不到匹配即为「没选中」。
    pub selected: Option<RemotePath>,
    /// 每发一次请求 +1。异步结果回来时对不上就丢弃 ——
    /// 用户点得比网络快时,后发先至的旧结果会把新目录顶掉。
    pub request_seq: u64,
}

impl PaneState {
    pub fn new(cwd: RemotePath) -> Self {
        Self {
            cwd,
            entries: Vec::new(),
            load: Load::Idle,
            sort_key: SortKey::Name,
            sort_dir: SortDir::Asc,
            show_hidden: false,
            selected: None,
            request_seq: 0,
        }
    }

    /// 开始一次加载,返回本次的序号。调用方把它随异步任务带走,
    /// 结果回来时用 `accept` 校验。
    pub fn begin_load(&mut self, cwd: RemotePath) -> u64 {
        self.cwd = cwd;
        self.load = Load::Loading;
        self.selected = None;
        self.request_seq += 1;
        self.request_seq
    }

    /// 收下一次加载结果。序号对不上返回 `false`(结果被丢弃)。
    pub fn accept(&mut self, seq: u64, result: Result<Vec<Entry>, String>) -> bool {
        if seq != self.request_seq {
            return false;
        }
        match result {
            Ok(mut v) => {
                super::sort(&mut v, self.sort_key, self.sort_dir);
                self.entries = v;
                self.load = Load::Ready;
            }
            Err(msg) => {
                self.entries.clear();
                self.load = Load::Failed(msg);
            }
        }
        true
    }

    /// 点列头:同一列再点一次翻方向,换列则回到升序。
    pub fn click_header(&mut self, key: SortKey) {
        if self.sort_key == key {
            self.sort_dir = self.sort_dir.flipped();
        } else {
            self.sort_key = key;
            self.sort_dir = SortDir::Asc;
        }
        super::sort(&mut self.entries, self.sort_key, self.sort_dir);
    }

    /// 这一帧要画的行(过滤 + 已排序)。
    pub fn rows(&self) -> Vec<&Entry> {
        super::visible(&self.entries, self.show_hidden)
    }

    /// 「进去」的目标。目录 → 它自己;**指向目录的链接 → 跟随**
    /// (设计 D21:双击跟随,删除才不跟随);普通文件 → `None`。
    ///
    /// 名字**发不出去** wire 请求的一律 `None`(判据是 `is_operable`,不是
    /// `is_utf8`——`russh-sftp 2.4.0` 收包时就把非 UTF-8 字节 lossy 成了
    /// 合法 UTF-8 + `U+FFFD`,`is_utf8()` 对这类条目恒为真,拿它当判据会
    /// 让「标注不可操作」整条路径变成死代码,见 `mullion_ssh::sftp::RemotePath`
    /// 文档)。与其发一个必然失败的请求,不如在这里就不动。
    pub fn enter_target(&self, e: &Entry) -> Option<RemotePath> {
        if !e.name.is_operable() {
            return None;
        }
        match e.kind {
            EntryKind::Dir => Some(self.cwd.join(e.name.as_bytes())),
            // 链接目标可能是绝对路径,也可能是相对的。绝对的直接用,
            // 相对的按当前目录拼 —— 这是 POSIX 的语义。
            EntryKind::Symlink => e.link_target.as_ref().map(|t| {
                if t.as_bytes().starts_with(b"/") {
                    t.clone()
                } else {
                    self.cwd.join(t.as_bytes())
                }
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, kind: EntryKind) -> Entry {
        Entry {
            name: RemotePath::from_bytes(name.as_bytes().to_vec()),
            kind,
            size: 0,
            mtime: 0,
            mode: 0o644,
            uid: 0,
            gid: 0,
            link_target: None,
        }
    }

    fn state() -> PaneState {
        PaneState::new(RemotePath::from_bytes(b"/home/u".to_vec()))
    }

    /// 用户点得比网络快时,**后发先至的旧结果必须被丢掉** ——
    /// 否则界面会莫名其妙跳回上一个目录的内容。
    #[test]
    fn a_stale_listing_that_arrives_late_is_discarded() {
        let mut s = state();
        let first = s.begin_load(RemotePath::from_bytes(b"/a".to_vec()));
        let second = s.begin_load(RemotePath::from_bytes(b"/b".to_vec()));
        assert!(
            !s.accept(first, Ok(vec![e("stale", EntryKind::File)])),
            "旧结果该被丢弃"
        );
        assert!(s.entries.is_empty());
        assert!(s.accept(second, Ok(vec![e("fresh", EntryKind::File)])));
        assert_eq!(s.entries.len(), 1);
        assert_eq!(s.entries[0].name.display(), "fresh");
    }

    #[test]
    fn clicking_the_same_header_twice_flips_the_direction() {
        let mut s = state();
        assert_eq!((s.sort_key, s.sort_dir), (SortKey::Name, SortDir::Asc));
        s.click_header(SortKey::Name);
        assert_eq!(s.sort_dir, SortDir::Desc);
        s.click_header(SortKey::Size);
        assert_eq!(
            (s.sort_key, s.sort_dir),
            (SortKey::Size, SortDir::Asc),
            "换列回升序"
        );
    }

    /// D21:双击指向目录的链接**跟随进入**。
    #[test]
    fn a_symlink_to_a_directory_is_followed_on_enter() {
        let s = state();
        let mut link = e("l", EntryKind::Symlink);
        link.link_target = Some(RemotePath::from_bytes(b"/etc/nginx".to_vec()));
        assert_eq!(s.enter_target(&link).unwrap().as_bytes(), b"/etc/nginx");

        let mut rel = e("r", EntryKind::Symlink);
        rel.link_target = Some(RemotePath::from_bytes(b"sub".to_vec()));
        assert_eq!(s.enter_target(&rel).unwrap().as_bytes(), b"/home/u/sub");
    }

    /// D16 修订:名字发不出去 wire 请求的条目**连「进去」都不试** ——
    /// 发一个必然失败的请求只会给用户一条看不懂的错误。
    #[test]
    fn a_non_operable_entry_cannot_be_entered() {
        let s = state();
        let mut bad = e("x", EntryKind::Dir);
        bad.name = RemotePath::from_bytes(vec![0xff, 0xfe]);
        assert!(s.enter_target(&bad).is_none());
    }

    /// 实施期补丁的核心场景:名字是 `is_utf8() == true`(线上 lossy 过的
    /// `U+FFFD` 串),但 `is_operable() == false` —— 判据必须是后者,否则
    /// 这条会被当成好路径去「进入」,换回一条用户看不懂的 `NoSuchFile`。
    #[test]
    fn a_lossy_utf8_entry_with_replacement_chars_cannot_be_entered() {
        let s = state();
        let mut bad = e("x", EntryKind::Dir);
        bad.name = RemotePath::from_bytes("\u{fffd}\u{fffd}".as_bytes().to_vec());
        assert!(bad.name.is_utf8(), "前提:这一步的串本身是合法 UTF-8");
        assert!(s.enter_target(&bad).is_none());
    }

    #[test]
    fn a_plain_file_is_not_an_enter_target() {
        let s = state();
        assert!(s.enter_target(&e("a.txt", EntryKind::File)).is_none());
    }

    /// 点列头重排之后,选中的还得是**同一个文件**。
    ///
    /// 这条守的是 `selected` 存的是身份(`RemotePath`)而不是下标:存下标
    /// 的话重排后同一个下标指向的是另一条,高亮凭空跳走。本切片只是看着
    /// 别扭,等删除/改名接到选中项上,就是**对错文件下手**。
    #[test]
    fn re_sorting_keeps_the_selection_on_the_same_file() {
        let mut s = state();
        s.entries = vec![e("a.txt", EntryKind::File), e("z.txt", EntryKind::File)];
        s.load = Load::Ready;
        let picked = s.entries[0].name.clone(); // a.txt,此刻在第 0 行
        s.selected = Some(picked.clone());
        assert_eq!(s.rows()[0].name, picked);

        s.click_header(SortKey::Name); // 同一列再点 → 翻成降序

        assert_eq!(
            s.rows()[1].name,
            picked,
            "前提:重排确实把它挪到了另一行,否则这条测试什么也没守到"
        );
        assert_eq!(
            s.selected.as_ref(),
            Some(&picked),
            "选中跟着文件走,不跟着行号走"
        );
    }

    #[test]
    fn a_failed_load_clears_the_rows_and_keeps_the_reason() {
        let mut s = state();
        let seq = s.begin_load(RemotePath::from_bytes(b"/nope".to_vec()));
        assert!(s.accept(seq, Err("没有那个文件".into())));
        assert!(s.entries.is_empty());
        assert_eq!(s.load, Load::Failed("没有那个文件".into()));
    }
}
