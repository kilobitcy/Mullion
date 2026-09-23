//! F278:文件面板的**递归模糊搜索**。纯逻辑 —— 零 egui、零 IO、零 async。
//!
//! 遍历本身是一台状态机([`Walk`]),形状照抄 `files/queue.rs`:每帧
//! [`Walk::take_runnable`] 吐出最多 n 个待列目录,app 侧各起一条 `list_dir`,
//! 结果回来喂 [`Walk::accept`]。这么切的理由和 `queue.rs` 逐字相同 ——
//! 「BFS 顺序对不对」「封顶卡没卡住」「最后一批还在飞的时候会不会提前报
//! 搜完」全是状态机 bug,必须能在没有网络的情况下复现。
//!
//! **为什么不发一句远端 `find`**(spec F278 已定):SFTP-only 的节点压根
//! 没有 exec 通道;有 exec 的也可能是 busybox,`-iname`/`-maxdepth` 参数
//! 不齐;再加上文件名里的引号与空格转义,是一摊比逐目录遍历更难对的事。

use mullion_ssh::sftp::{Entry, EntryKind, RemotePath};

/// 名字里出现的这几个字符按**子序列**匹配。大小写不敏感。
///
/// **是子序列不是子串**(fzf 式):用户记得住的往往是 `apprs` 这种骨架,
/// 而不是完整的 `app.rs`。`crate::search::matches` 那一套(会话/项目列表用的)
/// 刻意**不是**子序列 —— 那边的字段里有多行长文本(F237 的项目说明),子序列
/// 在长文本上几乎命中一切。这边匹配的是**单个文件名**,短,子序列正合适。
/// 两处判据不同是有意的,不要合并。
///
/// 空查询恒不匹配 —— 调用方(`Walk::new`)在起搜前就该挡掉空串,
/// 这里返回 `false` 只是兜底:恒 `true` 会让一次空搜索把整棵树当成 500 个
/// 命中吐出来。
pub fn matches(query: &str, name: &str) -> bool {
    if query.is_empty() {
        return false;
    }
    let mut want = query.chars().flat_map(char::to_lowercase).peekable();
    for c in name.chars().flat_map(char::to_lowercase) {
        match want.peek() {
            Some(&w) if w == c => {
                want.next();
            }
            Some(_) => {}
            None => return true,
        }
    }
    want.peek().is_none()
}

/// F291:查询串里有没有通配符。有 → [`glob_matches`],没有 → [`matches`]。
pub fn is_glob(query: &str) -> bool {
    query.contains(['*', '?'])
}

/// F291:按查询串的形状分派。**这是 `Walk` 唯一该调的入口。**
pub fn query_matches(query: &str, name: &str) -> bool {
    if is_glob(query) {
        glob_matches(query, name)
    } else {
        matches(query, name)
    }
}

/// F291:glob 整名匹配。`*` 任意段(含空串、含 `.`),`?` 恰一个字符,其余
/// 字面量;大小写不敏感;**整名锚定**(`*.docx` 不命中 `a.docx.bak`)。
/// 不支持 `[...]`,方括号是字面量。
///
/// 手写而不引 `globset`:判据就这两个元字符,一个依赖换 30 行不值。
/// 经典双指针 + 单回溯点:遇到 `*` 记下位置,失配时回到上一个 `*` 多吃一个字符。
pub fn glob_matches(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().flat_map(char::to_lowercase).collect();
    let n: Vec<char> = name.chars().flat_map(char::to_lowercase).collect();
    let (mut pi, mut ni) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None; // (pattern 里 `*` 之后的位置, 当时的 name 位置)
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi + 1, ni));
            pi += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp;
            ni = sn + 1;
            star = Some((sp, ni));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// 结果行显示的那一串:`root` 下的相对路径。
///
/// **显示相对路径而不是绝对路径**(spec F278 已定):搜索的心智是「在**这个
/// 目录下面**找」,绝对路径每行都顶着同一段前缀,把唯一有信息量的后半截
/// 挤出可视区。
///
/// `path` 不在 `root` 下时**原样返回绝对路径** —— 那说明调用方把两棵树的
/// 东西混一起了,硬切前缀会切出一串没头没尾的乱码,不如把真相摆出来。
pub fn relative(root: &RemotePath, path: &RemotePath) -> String {
    let r = root.as_bytes();
    let p = path.as_bytes();
    // 根是 `/` 时没有「前缀 + 分隔符」可言,直接剥掉那一个字节。
    let cut = if r == b"/" {
        // 根是 `/` 时前缀本身就是那个分隔符,只剥一个字节 —— 但仍要确认
        // `path` 真的以它开头且不止这一个字节。空路径会让 `p[1..]` 直接
        // 越界 panic(GUI 里就是整个窗口崩掉),不带前导 `/` 的相对路径
        // 则会被静默吃掉首字符(`foo` 切成 `oo`)。两种都落进下面那条
        // 「不在根下」的出口:原样把绝对路径摆出来,看得见、查得着。
        if p.starts_with(b"/") && p.len() > 1 {
            1
        } else {
            return path.display().to_string();
        }
    } else if p.starts_with(r) && p.get(r.len()) == Some(&b'/') {
        r.len() + 1
    } else {
        return path.display().to_string();
    };
    RemotePath::from_bytes(p[cut..].to_vec())
        .display()
        .to_string()
}

/// 命中封顶。**500 条之后停**:再多用户也不会往下翻,而每多一条就多一次
/// 无意义的往返。到顶时状态里写明「结果可能不全」,不装作搜完了。
pub const MAX_HITS: usize = 500;

/// 目录封顶。**2000 个之后停**:家目录下随便一个 `node_modules` 就是几万个
/// 目录,而本项目的主场景是高延迟代理链路,一次往返几百毫秒。
pub const MAX_DIRS: usize = 2000;

/// 同时在飞的 `list_dir` 条数。
///
/// **不是 1**:串行的话 2000 个目录 × 300ms RTT ≈ 10 分钟,这个功能等于不存在。
/// **也不是几十**:russh-sftp 在同一条 channel 上复用请求 id,开太多只会把
/// 窗口撑满、把同一条连接上的交互式操作(列目录、编辑保存)挤到后面去。
pub const CONCURRENCY: usize = 4;

/// 停下来的原因。**四档分开**,因为界面上要说的话完全不同 ——
/// 「搜完了,没找到」和「翻了 2000 个目录还没翻完」是两件事,混成一句
/// 「没有结果」会让用户以为文件不存在,而它可能就在第 2001 个目录里。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// 整棵树翻完了。
    Exhausted,
    /// 命中到 [`MAX_HITS`] 了。
    HitCap,
    /// 目录翻到 [`MAX_DIRS`] 了。
    DirCap,
    /// 用户按了取消。
    Canceled,
}

/// 一条命中。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// **绝对路径**。跳转要用它,显示时才现算相对路径(走 [`relative`])——
    /// 只存相对路径的话,跳转那一步要把根拼回去,而根可能已经变了。
    pub path: RemotePath,
    /// 是目录还是文件。决定跳过去之后是「进去」还是「在父目录里亮出来」,
    /// 也决定结果行画哪个图标。
    pub is_dir: bool,
}

/// 一次递归搜索的遍历状态。BFS。
///
/// **BFS 不是 DFS**:用户要找的东西绝大多数就在浅层,深度优先会一头扎进
/// 某个 `node_modules` 里,封顶用完了还没回到第二层。
pub struct Walk {
    root: RemotePath,
    query: String,
    show_hidden: bool,
    /// 还没发出去的目录。
    pending: std::collections::VecDeque<RemotePath>,
    /// 已经发出去、还没回来的条数。**收工判据要用它**,见 `status`。
    inflight: usize,
    /// 已经**列过**的目录数(成功失败都算)。封顶判据。
    visited: usize,
    hits: Vec<Hit>,
    stop: Option<Stop>,
    /// 列不出来的目录数(多半是权限不足)。界面上顺带报一句 ——
    /// 静默跳过的话,用户会以为那几棵子树里真的没有他要的东西。
    skipped: usize,
}

impl Walk {
    /// 起一次搜索。`query` 由调用方保证非空(空串会让 [`matches`] 恒不命中,
    /// 白跑一整棵树)。
    pub fn new(root: RemotePath, query: String, show_hidden: bool) -> Self {
        let mut pending = std::collections::VecDeque::new();
        pending.push_back(root.clone());
        Self {
            root,
            query,
            show_hidden,
            pending,
            inflight: 0,
            visited: 0,
            hits: Vec::new(),
            stop: None,
            skipped: 0,
        }
    }

    /// 这一轮可以发出去几个目录。照 `queue::Queue::take_runnable` 的形状。
    ///
    /// **已经停了就一个都不吐**:到顶之后再发请求,结果只会被 `accept` 原样
    /// 丢掉,白白占着链路。
    pub fn take_runnable(&mut self) -> Vec<RemotePath> {
        if self.stop.is_some() {
            return Vec::new();
        }
        let room = CONCURRENCY.saturating_sub(self.inflight);
        let mut out = Vec::new();
        while out.len() < room {
            // 封顶判在**发出去之前**:判在回来的时候的话,最后一轮会超发
            // 到 2003 个,而且那三个的结果照样被收下。
            if self.visited + self.inflight >= MAX_DIRS {
                // pending 还有东西 = 真的被封顶截断了;空了 = 正好翻完,
                // 那由 `status` 判成 `Exhausted`。
                if !self.pending.is_empty() {
                    self.stop = Some(Stop::DirCap);
                }
                break;
            }
            match self.pending.pop_front() {
                Some(d) => {
                    self.inflight += 1;
                    out.push(d);
                }
                None => break,
            }
        }
        out
    }

    /// 一次列目录回来了。
    ///
    /// **无论成败都要调**:不调的话 `inflight` 永远减不回去,`status` 就
    /// 永远判不出收工 —— 界面上是一个转到天荒地老的「正在搜索」。
    ///
    /// 单条失败**不中止整次搜索**:权限不足的目录在真实机器上遍地都是
    /// (`/proc`、别人的家目录),一条就把整次搜索判死的话,这个功能在
    /// 任何一台真机上都用不了。
    pub fn accept(&mut self, dir: &RemotePath, result: Result<Vec<Entry>, String>) {
        self.inflight = self.inflight.saturating_sub(1);
        self.visited += 1;
        if self.stop.is_some() {
            return;
        }
        let entries = match result {
            Ok(v) => v,
            Err(_) => {
                self.skipped += 1;
                return;
            }
        };
        for e in entries {
            let name = e.name.as_bytes();
            // 隐藏项随 `show_hidden`:**既不匹配也不递归**。只挡匹配不挡
            // 递归的话,关着开关搜一次家目录照样要爬完整个 `.cache`。
            if !self.show_hidden && name.starts_with(b".") {
                continue;
            }
            let full = dir.join(name);
            if query_matches(&self.query, &e.name.display()) {
                self.hits.push(Hit {
                    path: full.clone(),
                    is_dir: e.kind == EntryKind::Dir,
                });
                if self.hits.len() >= MAX_HITS {
                    self.stop = Some(Stop::HitCap);
                    return;
                }
            }
            // **只有 `EntryKind::Dir` 往下走**。`list_dir` 是 lstat 语义
            // (见 `SftpClient::stat` 的文档),指向目录的软链接是
            // `EntryKind::Symlink`,自然被挡在外面 —— 这正是 spec 要的
            // 「不跟符号链接」。跟了的话 `a -> ..` 这种环会让遍历永不收敛,
            // 只有 2000 的封顶兜着,而那 2000 次往返全是白跑的。
            if e.kind == EntryKind::Dir {
                self.pending.push_back(full);
            }
        }
    }

    /// 用户按了取消。
    pub fn cancel(&mut self) {
        if self.stop.is_none() {
            self.stop = Some(Stop::Canceled);
        }
    }

    /// 还在跑吗。`None` = 还在跑;`Some(stop)` = 停了,原因在里面。
    ///
    /// **收工判据是「待列空 **且** 在飞为零」**。只看 `pending.is_empty()`
    /// 的话,最后一批还在路上的时候就会报「搜完了,0 个结果」,而半秒后
    /// 结果才回来 —— 用户已经看过那句「没找到」并关掉了。
    pub fn status(&self) -> Option<Stop> {
        if let Some(s) = self.stop {
            return Some(s);
        }
        if self.pending.is_empty() && self.inflight == 0 {
            return Some(Stop::Exhausted);
        }
        None
    }

    pub fn hits(&self) -> &[Hit] {
        &self.hits
    }
    pub fn visited(&self) -> usize {
        self.visited
    }
    pub fn skipped(&self) -> usize {
        self.skipped
    }
    pub fn root(&self) -> &RemotePath {
        &self.root
    }
    pub fn query(&self) -> &str {
        &self.query
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rp(s: &str) -> RemotePath {
        RemotePath::from_bytes(s.as_bytes().to_vec())
    }

    /// 子序列,不是子串 —— 用户记得住的是骨架。
    ///
    /// 自证会变红:把 `matches` 改成 `name.to_lowercase().contains(&query.to_lowercase())`
    /// (第一条断言红)。
    #[test]
    fn the_query_matches_as_a_subsequence_not_a_substring() {
        assert!(matches("apprs", "app.rs"));
        assert!(matches("app.rs", "app.rs"));
        assert!(matches("mn", "main.rs"));
        assert!(!matches("rsapp", "app.rs"), "顺序不对不算命中");
        assert!(!matches("appx", "app.rs"));
    }

    /// 大小写不敏感 —— 远端多半是 Linux,用户按 Windows 习惯打小写。
    ///
    /// 自证会变红:把两处 `to_lowercase` 删掉。
    #[test]
    fn matching_ignores_case_on_both_sides() {
        assert!(matches("APP", "app.rs"));
        assert!(matches("app", "APP.RS"));
        assert!(matches("Gm", "GAMMA"));
    }

    /// 空查询恒不匹配。恒 `true` 的话一次空搜索会把整棵树当成 500 个命中吐
    /// 出来 —— 而用户什么都没输。
    ///
    /// 自证会变红:把 `matches` 开头那三行 `if query.is_empty()` 删掉。
    #[test]
    fn an_empty_query_matches_nothing_rather_than_everything() {
        assert!(!matches("", "app.rs"));
        assert!(!matches("", ""));
    }

    /// 相对路径剥掉根 + 那一个分隔符。
    ///
    /// 自证会变红:把 `r.len() + 1` 改成 `r.len()`(会多出个前导 `/`)。
    #[test]
    fn a_hit_is_shown_relative_to_the_directory_the_search_started_in() {
        assert_eq!(
            relative(&rp("/home/u/proj"), &rp("/home/u/proj/src/app.rs")),
            "src/app.rs"
        );
        assert_eq!(
            relative(&rp("/home/u/proj"), &rp("/home/u/proj/a.txt")),
            "a.txt"
        );
    }

    /// 根是 `/` 时只剥那一个字节 —— 按「根 + 分隔符」算会多剥一个字符,
    /// `/etc/hosts` 会显示成 `tc/hosts`。
    ///
    /// 自证会变红:把 `if r == b"/"` 那一支删掉。
    #[test]
    fn searching_from_the_filesystem_root_does_not_eat_a_character() {
        assert_eq!(relative(&rp("/"), &rp("/etc/hosts")), "etc/hosts");
    }

    /// 不在根下面的路径原样给绝对路径 —— 硬切前缀会切出没头没尾的乱码。
    ///
    /// 自证会变红:把最后那条 `return path.display().to_string()` 改成
    /// 无条件切 `r.len()`。
    #[test]
    fn a_path_outside_the_root_keeps_its_absolute_form() {
        assert_eq!(
            relative(&rp("/home/u/proj"), &rp("/var/log/x")),
            "/var/log/x"
        );
        // 前缀撞上但不是目录边界(`/home/u/project` vs `/home/u/proj`)。
        assert_eq!(
            relative(&rp("/home/u/proj"), &rp("/home/u/project/x")),
            "/home/u/project/x"
        );
    }

    /// 根是 `/` 那条快路径同样要验前缀。不验的话:空路径让 `p[1..]` **越界
    /// panic**(GUI 里是整个窗口崩掉),不带前导 `/` 的路径被静默吃掉首字符
    /// (`foo` 切成 `oo`)—— 而这两种都不会有任何报错。
    ///
    /// 自证会变红:把根分支里的 `if p.starts_with(b"/") && p.len() > 1` 那道门
    /// 去掉、改回无条件 `1`(第一条断言当场 panic)。
    #[test]
    fn the_root_shortcut_still_checks_the_prefix_instead_of_blindly_cutting() {
        assert_eq!(relative(&rp("/"), &rp("")), "");
        assert_eq!(relative(&rp("/"), &rp("foo")), "foo");
        assert_eq!(relative(&rp("/"), &rp("/")), "/");
    }

    /// `path` 恰好等于 `root` 时两条分支**给同一种结果**(原样绝对路径)。
    /// 不对称的话,同一种「搜到目录自己」的情形在根目录下显示成空行、
    /// 在别处显示成完整路径,而列表里空行看着像渲染坏了。
    ///
    /// 自证会变红:把根分支的 `p.len() > 1` 删掉(第一条会变成 `""`)。
    #[test]
    fn a_path_that_is_exactly_the_root_behaves_the_same_at_either_depth() {
        assert_eq!(relative(&rp("/"), &rp("/")), "/");
        assert_eq!(relative(&rp("/home/u"), &rp("/home/u")), "/home/u");
    }

    fn e(name: &str, kind: EntryKind) -> Entry {
        Entry {
            name: rp(name),
            kind,
            size: 0,
            mtime: 0,
            mode: 0o644,
            uid: 0,
            gid: 0,
            link_target: None,
        }
    }

    /// 把一次搜索跑到底:反复 `take_runnable` → 从 `tree` 里取目录内容
    /// → `accept`。返回命中的相对路径,按发现顺序,以及 `visited`/`skipped`。
    fn run(
        root: &str,
        query: &str,
        show_hidden: bool,
        tree: &[(&str, Vec<Entry>)],
    ) -> (Vec<String>, Stop, usize, usize) {
        let mut w = Walk::new(rp(root), query.to_string(), show_hidden);
        // 上限兜底:状态机写错成不收敛时,让测试**失败**而不是挂死。
        for _ in 0..100_000 {
            let batch = w.take_runnable();
            if batch.is_empty() {
                break;
            }
            for d in batch {
                let found = tree
                    .iter()
                    .find(|(p, _)| p.as_bytes() == d.as_bytes())
                    .map(|(_, v)| v.clone());
                w.accept(&d, found.ok_or_else(|| "没有权限".to_string()));
            }
        }
        let stop = w.status().expect("跑到底之后必须有结论");
        let rels = w
            .hits()
            .iter()
            .map(|h| relative(w.root(), &h.path))
            .collect();
        (rels, stop, w.visited(), w.skipped())
    }

    /// 递归真的往下走,而且结果是相对路径。
    ///
    /// 自证会变红:把 `accept` 里 `self.pending.push_back(full)` 那一句删掉
    /// —— 只剩根目录那一层的命中。
    #[test]
    fn the_search_descends_into_subdirectories() {
        let tree = vec![
            (
                "/r",
                vec![e("src", EntryKind::Dir), e("a.txt", EntryKind::File)],
            ),
            ("/r/src", vec![e("app.rs", EntryKind::File)]),
        ];
        let (hits, stop, _, _) = run("/r", "ap", false, &tree);
        assert_eq!(hits, vec!["src/app.rs"]);
        assert_eq!(stop, Stop::Exhausted);
    }

    /// **不跟符号链接**:指向目录的软链接是 `EntryKind::Symlink`,不入队。
    ///
    /// 自证会变红:把 `if e.kind == EntryKind::Dir` 改成
    /// `if matches!(e.kind, EntryKind::Dir | EntryKind::Symlink)` —— `/r/link`
    /// 会被列,`visited` 从 1 变成 2。
    #[test]
    fn a_symlink_is_never_followed_even_when_it_points_at_a_directory() {
        let tree = vec![
            ("/r", vec![e("link", EntryKind::Symlink)]),
            // 真去列它就会命中这一条 —— 不跟链接的话永远看不见。
            ("/r/link", vec![e("inside.txt", EntryKind::File)]),
        ];
        let (hits, _, visited, _) = run("/r", "inside", false, &tree);
        assert!(hits.is_empty(), "跟着软链接走了:{hits:?}");
        assert_eq!(visited, 1, "只该列根目录一个");
    }

    /// 隐藏项随 `show_hidden`:关着的时候**既不匹配也不递归**。
    ///
    /// 自证会变红:把 `accept` 里 `if !self.show_hidden && ...` 那三行删掉
    /// (第一组断言红);或只改成「过滤命中、照样入队」(`visited` 那条红)。
    #[test]
    fn hidden_entries_are_neither_matched_nor_descended_into() {
        let tree = vec![
            (
                "/r",
                vec![e(".git", EntryKind::Dir), e(".env", EntryKind::File)],
            ),
            ("/r/.git", vec![e("config", EntryKind::File)]),
        ];
        let (off, _, visited_off, _) = run("/r", "cfg", false, &tree);
        assert!(off.is_empty());
        assert_eq!(visited_off, 1, "关着开关时不该进 .git");

        let (on, _, visited_on, _) = run("/r", "config", true, &tree);
        assert_eq!(on, vec![".git/config"]);
        assert_eq!(visited_on, 2);

        let (dot, _, _, _) = run("/r", ".env", true, &tree);
        assert_eq!(dot, vec![".env"], "开着开关时隐藏文件本身也该被匹配");
    }

    /// 命中封顶:到 500 就停,而且状态说得出是「封顶」不是「搜完了」。
    /// 混成一句的话用户会以为结果就这些 —— 而它可能还差得远。
    ///
    /// 自证会变红:把 `if self.hits.len() >= MAX_HITS` 那一段删掉。
    #[test]
    fn hitting_the_result_cap_stops_and_says_so() {
        let names: Vec<String> = (0..MAX_HITS + 50).map(|i| format!("f{i}.txt")).collect();
        let many: Vec<Entry> = names.iter().map(|n| e(n, EntryKind::File)).collect();
        let tree = vec![("/r", many)];
        let (hits, stop, _, _) = run("/r", "f", false, &tree);
        assert_eq!(hits.len(), MAX_HITS);
        assert_eq!(stop, Stop::HitCap);
    }

    /// 目录封顶:判在**发出去之前**。判在回来时的话最后一轮会超发。
    ///
    /// 自证会变红:把 `take_runnable` 里 `self.visited + self.inflight >= MAX_DIRS`
    /// 改成 `self.visited >= MAX_DIRS`(单链树场景一在这个变异下仍然全绿 ——
    /// `pending` 里从头到尾只有 1 个元素、`inflight` 每轮归零,「预占名额」
    /// 这道防线在这棵树上根本没有起作用的机会;分支因子 ≥2 的场景二会把
    /// `visited` 冲到 2000 以上,实测冲到 2003)。
    #[test]
    fn the_directory_cap_is_enforced_before_the_requests_go_out() {
        // 场景一:单链树,每个目录只生一个子目录。
        let mut w = Walk::new(rp("/r"), "zzz".to_string(), false);
        for _ in 0..100_000 {
            let batch = w.take_runnable();
            if batch.is_empty() {
                break;
            }
            for d in batch {
                w.accept(&d, Ok(vec![e("sub", EntryKind::Dir)]));
            }
        }
        assert_eq!(w.status(), Some(Stop::DirCap));
        assert!(
            w.visited() <= MAX_DIRS,
            "超发了:列了 {} 个目录,上限 {MAX_DIRS}",
            w.visited()
        );

        // 场景二:分支因子 2 的树。单链树里 `pending` 在临界点附近永远只剩
        // 1 个元素,`self.visited + self.inflight` 里的 `+ self.inflight`
        // 在这棵树上从来没有被真正踩到过;分支因子 ≥2 之后 `pending` 会
        // 攒出不止一个待发目录,批次边界才会跟 `MAX_DIRS` 错开,让「发出去
        // 之前预占名额」这道防线真正生效。
        let mut w2 = Walk::new(rp("/r2"), "zzz".to_string(), false);
        for _ in 0..100_000 {
            let batch = w2.take_runnable();
            if batch.is_empty() {
                break;
            }
            for d in batch {
                w2.accept(&d, Ok(vec![e("a", EntryKind::Dir), e("b", EntryKind::Dir)]));
            }
        }
        assert_eq!(w2.status(), Some(Stop::DirCap));
        assert!(
            w2.visited() <= MAX_DIRS,
            "分支树超发了:列了 {} 个目录,上限 {MAX_DIRS}",
            w2.visited()
        );
    }

    /// 正好翻完不该被报成「封顶」—— 那两句话对用户的含义相反。
    ///
    /// 自证会变红:把 `take_runnable` 里 `if !self.pending.is_empty()` 那道
    /// 门去掉,改成无条件 `self.stop = Some(Stop::DirCap)`。
    #[test]
    fn a_tree_that_ends_exactly_at_the_cap_is_exhausted_not_capped() {
        let mut w = Walk::new(rp("/r"), "zzz".to_string(), false);
        let mut left = MAX_DIRS;
        for _ in 0..100_000 {
            let batch = w.take_runnable();
            if batch.is_empty() {
                break;
            }
            for d in batch {
                left -= 1;
                // 最后一个目录不再生子目录 —— 树到此为止,正好 MAX_DIRS 个。
                let kids = if left > 0 {
                    vec![e("sub", EntryKind::Dir)]
                } else {
                    vec![]
                };
                w.accept(&d, Ok(kids));
            }
        }
        assert_eq!(w.visited(), MAX_DIRS);
        assert_eq!(w.status(), Some(Stop::Exhausted));
    }

    /// **收工判据是「待列空 且 在飞为零」**。只看 `pending` 的话,最后一批
    /// 还在路上时就会报「搜完了、没找到」,而半秒后结果才回来。
    ///
    /// 自证会变红:把 `status` 里的 `&& self.inflight == 0` 删掉。
    #[test]
    fn a_search_with_requests_still_in_flight_is_not_finished() {
        let mut w = Walk::new(rp("/r"), "a".to_string(), false);
        let batch = w.take_runnable();
        assert_eq!(batch.len(), 1);
        assert_eq!(w.status(), None, "发出去了还没回来,不算搜完");
        w.accept(&batch[0], Ok(vec![e("a.txt", EntryKind::File)]));
        assert_eq!(w.status(), Some(Stop::Exhausted));
    }

    /// 单条目录列不出来(权限不足)**不中止整次搜索**,但要留痕 —— `skipped()`
    /// 已经接进 `files_panel.rs` 的「N 个目录没权限读」文案,不能只测「没死」。
    ///
    /// 自证会变红:把 `accept` 里 `self.skipped += 1;` 删掉(`skipped` 那条
    /// 断言红,`hits`/`stop` 两条看不出区别 —— 这正是要补的覆盖缺口)。
    #[test]
    fn a_directory_we_cannot_read_is_skipped_rather_than_killing_the_search() {
        let tree = vec![
            (
                "/r",
                vec![e("locked", EntryKind::Dir), e("ok", EntryKind::Dir)],
            ),
            // `/r/locked` 故意不在树里 —— `run` 会给它一个 Err。
            ("/r/ok", vec![e("apple.txt", EntryKind::File)]),
        ];
        let (hits, stop, _, skipped) = run("/r", "apple", false, &tree);
        assert_eq!(hits, vec!["ok/apple.txt"]);
        assert_eq!(stop, Stop::Exhausted);
        assert_eq!(skipped, 1, "只有 /r/locked 一个目录读失败");
    }

    /// 取消之后不再吐任何请求 —— 吐了的话取消只是界面上的假象,链路还在跑。
    ///
    /// 自证会变红:把 `take_runnable` 开头 `if self.stop.is_some()` 那三行删掉。
    #[test]
    fn canceling_stops_new_requests_from_going_out() {
        let mut w = Walk::new(rp("/r"), "a".to_string(), false);
        let batch = w.take_runnable();
        w.accept(
            &batch[0],
            Ok(vec![e("d1", EntryKind::Dir), e("d2", EntryKind::Dir)]),
        );
        w.cancel();
        assert!(w.take_runnable().is_empty());
        assert_eq!(w.status(), Some(Stop::Canceled));
    }

    /// BFS:浅的先出。深度优先会一头扎进某个 node_modules,封顶用完了还没
    /// 回到第二层。
    ///
    /// 自证会变红:把 `pending` 换成 `Vec` + `pop()`(后进先出)。**光靠上面
    /// 那棵「一层只有一个目录」的树抓不住这条变异**——`pending` 里从来
    /// 只有单个元素,先进先出还是后进先出看不出区别。第二棵树同一层放
    /// 两个目录,`take_runnable` 会在**同一次调用**里把两个都取出来,
    /// FIFO/LIFO 在“取出顺序”上才第一次出现分歧。
    #[test]
    fn the_walk_goes_breadth_first_so_shallow_hits_come_out_first() {
        let tree = vec![
            (
                "/r",
                vec![e("deep", EntryKind::Dir), e("a-shallow", EntryKind::File)],
            ),
            ("/r/deep", vec![e("a-deep", EntryKind::File)]),
        ];
        let (hits, _, _, _) = run("/r", "a", false, &tree);
        assert_eq!(hits, vec!["a-shallow", "deep/a-deep"]);

        // 同一层两个目录:先列到的先出。
        let siblings = vec![
            ("/s", vec![e("d2", EntryKind::Dir), e("d1", EntryKind::Dir)]),
            ("/s/d2", vec![e("x2", EntryKind::File)]),
            ("/s/d1", vec![e("x1", EntryKind::File)]),
        ];
        let (hits, _, _, _) = run("/s", "x", false, &siblings);
        assert_eq!(
            hits,
            vec!["d2/x2", "d1/x1"],
            "`d2` 比 `d1` 先列到,该先出结果"
        );
    }

    // ---- F291 通配 ----

    /// `*` 任意段(含 `.` 与空串)、`?` 恰一个字符、整名锚定、大小写不敏感。
    ///
    /// 自证会变红:把 `glob_matches` 的整名锚定去掉(`*.docx` 会命中
    /// `a.docx.bak`);或把小写化去掉(`*.DOCX` 那条红)。
    #[test]
    fn a_glob_matches_the_whole_name_case_insensitively() {
        assert!(glob_matches("*.docx", "报告.docx"));
        assert!(glob_matches("*.docx", "a.DOCX"));
        assert!(glob_matches("*.DOCX", "a.docx"));
        assert!(!glob_matches("*.docx", "a.docx.bak"), "整名锚定");
        assert!(glob_matches("a?c", "abc"));
        assert!(!glob_matches("a?c", "abbc"));
        assert!(glob_matches("*", "anything"));
        assert!(glob_matches("*", ""));
        assert!(glob_matches("a*b*c", "aXXbYYc"));
        assert!(!glob_matches("a*b*c", "aXXcYYb"));
        assert!(glob_matches("*.docx", ".docx"), "`*` 可以是空串");
        assert!(glob_matches("[a]", "[a]"), "方括号是字面量");
        assert!(!glob_matches("[a]", "a"));
    }

    /// 分派:查询串里**有** `*`/`?` 才走 glob,否则维持 F278 子序列。
    /// `.docx`(不带星)仍是子序列 —— 用户分得清哪种在生效的唯一办法。
    ///
    /// 自证会变红:把 `query_matches` 改成恒走 `matches`(`*.docx` 全灭);
    /// 或恒走 glob(`.docx` 命中 `x.docx-notes` 那条红)。
    #[test]
    fn a_query_with_a_wildcard_is_a_glob_and_without_one_stays_a_subsequence() {
        assert!(query_matches("*.docx", "a.docx"));
        assert!(!query_matches("*.docx", "a.docx.bak"));
        assert!(
            !query_matches("*.docx", "docx"),
            "glob 下 `.` 是字面量,必须出现"
        );
        assert!(
            query_matches(".docx", "x.docx-notes"),
            "无通配 = 子序列,行为不变"
        );
        assert!(query_matches("appr", "app.rs"));
    }

    /// `Walk` 用的是分派函数,不是裸 `matches` —— 否则上面两条纯函数测试
    /// 全绿、真搜索照旧不认 `*`。
    ///
    /// 自证会变红:把 `accept` 里的 `query_matches(` 改回 `matches(`。
    #[test]
    fn the_walk_dispatches_through_query_matches() {
        let mut w = Walk::new(rp("/root"), "*.docx".to_string(), true);
        let dir = w.take_runnable().pop().unwrap();
        w.accept(
            &dir,
            Ok(vec![
                e("a.docx", EntryKind::File),
                e("a.docx.bak", EntryKind::File),
            ]),
        );
        let hits: Vec<String> = w
            .hits()
            .iter()
            .map(|h| h.path.display().to_string())
            .collect();
        assert_eq!(hits, vec!["/root/a.docx".to_string()]);
    }
}
