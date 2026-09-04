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
    /// B3:连接/通道没了。跟 `Failed` 分开是因为**界面动作不同** ——
    /// 这个状态要给一个重连入口,而 `Failed` 只是报一句、停在原地。
    Disconnected,
}

/// F200:一次就地改名的编辑态。
#[derive(Debug, Clone, PartialEq)]
pub struct RenameEdit {
    /// 正在改的那一行的**原名**(不含目录)。
    ///
    /// 存名字而不是行下标:点一次列头 `entries` 就重排了,下标会让输入框
    /// 跳到另一个文件上 —— 与 `PaneState::selected` 存身份是同一条理由,
    /// 而改名同样是「对错文件下手」那一类不可逆操作。
    pub from: RemotePath,
    /// 输入框内容。
    pub buf: String,
    /// 刚进编辑态、**还没把键盘焦点要过来**。渲染那侧要一次就清掉。
    ///
    /// 用一次性标志而不是「每帧发现没焦点就抢回来」:F131 时实测过,
    /// 无条件每帧 `request_focus()` 会让两栏互抢,先进编辑态的那栏永远
    /// `lost_focus()` 不了、退不出来(见 `PaneState::path_edit` 的文档)。
    pub focus_pending: bool,
}

/// F219:一次就地**新建**的编辑态。
///
/// 与 `RenameEdit` 分开而不是加个 `kind` 字段:改名绑着一条**已存在的行**
/// (`from` 是它的原名,`accept` 里那一行没了就得清掉),新建谁都不绑 ——
/// 塞进同一个结构体的话,`from` 对新建就是个必须编出来的假值,而编译器
/// 对这种「一半字段没意义」的类型一声不吭。
#[derive(Debug, Clone, PartialEq)]
pub struct NewEdit {
    /// 输入框内容。进来时是空的。
    pub buf: String,
    /// 刚进新建态、**还没把键盘焦点要过来**。渲染那侧要一次就清掉
    /// (每帧无条件 `request_focus()` 会让两栏互抢,见 `RenameEdit` 的文档)。
    pub focus_pending: bool,
}

pub struct PaneState {
    pub cwd: RemotePath,
    pub entries: Vec<Entry>,
    pub load: Load,
    pub sort_key: SortKey,
    pub sort_dir: SortDir,
    pub show_hidden: bool,
    /// **选中集**(F54 多选)。存的是身份(`RemotePath`)不是下标。
    ///
    /// 存下标会错行:点一次列头 `entries` 就重排了,`show_hidden` 一切
    /// 过滤结果也变了,下标却纹丝不动 —— 高亮会跳到另一个文件上。D1 时
    /// 只是看着别扭,D2 起操作真的接到选中项上,错行就是**对错文件下手**,
    /// 而删除不可逆。存身份则天然自愈:那一条没了,匹配不上即为没选中。
    ///
    /// 用 `BTreeSet` 不是 `Vec`:去重是硬需求(Ctrl 点两次同一条),
    /// 而 `RemotePath` 的 `Ord` 是字节序、正好够当集合键。
    /// **集合里的顺序无意义** —— 要按可见行序拿,用 [`PaneState::selected_paths`]。
    pub selected: std::collections::BTreeSet<RemotePath>,
    /// 光标行:`↑`/`↓` 移动的那一条,也是**单目标**操作(重命名、改权限)
    /// 的对象。
    ///
    /// 与 `selected` 分开是必须的 —— 多选了 5 条时「重命名」该改哪一条
    /// 没有答案,界面上就得有一条明确的「当前行」。
    pub cursor: Option<RemotePath>,
    /// `Shift` 范围选择的起点。平点 / Ctrl 点会把它挪到点中那条;
    /// Shift 点**不挪**(否则连续 Shift 点会变成一段一段接龙)。
    pub anchor: Option<RemotePath>,
    /// 每发一次请求 +1。异步结果回来时对不上就丢弃 ——
    /// 用户点得比网络快时,后发先至的旧结果会把新目录顶掉。
    pub request_seq: u64,
    /// F131:路径条正在被编辑时的缓冲。`None` = 只读态(默认)。
    ///
    /// 放每栏一份而不是每面板一份:两栏各有自己的路径条。两栏互斥靠
    /// 「egui 键盘焦点唯一」——但这条**只在没人每帧无条件 `request_focus()`
    /// 时成立**:质量复核实测过,渲染那侧如果每帧都发现自己没焦点就抢
    /// 回去,两栏会互抢,先进编辑态的那栏永远抢不到失焦、退不出来。所以
    /// `ui/files_panel.rs` 只在「进入编辑态那一刻」请求一次焦点,不在每帧
    /// 重复请求——这个前提不成立的话,这里就不是「不需要额外互斥」,而是
    /// 「谁都退不出去」。
    pub path_edit: Option<String>,
    /// F200:就地改名的编辑缓冲。`None` = 没在改名(默认)。
    ///
    /// **只有远端栏用得上**(设计 D5:本地文件管理外包给资源管理器),
    /// 但字段放在 `PaneState` 上而不是 `PanelFrame` 上 —— 与 `path_edit`
    /// 一致,「哪一栏在编辑」这件事本来就是每栏各自的事。
    pub rename_edit: Option<RenameEdit>,
    /// F219:就地新建文件的输入缓冲。`None` = 没在新建(默认)。
    ///
    /// **与 `rename_edit` 互斥**:两个 `TextEdit` 同时活着会互抢键盘焦点,
    /// 先进编辑态那个永远退不出来(F131 实测过的同款事故)。互斥在
    /// `begin_new_file` / `begin_rename` 两个入口各清一次对方。
    pub new_edit: Option<NewEdit>,
    /// F142:这条连接上的 uid/gid → 名字缓存。**本地栏那份永远是空的**
    /// (本地栏属主列恒画 `—`,见 `ui::files_panel::owner_text`)。
    ///
    /// 挂在栏上而不是标签上:一栏对应一条连接,换连接时只需清这一份
    /// (`OwnerNames::clear`,调用点在 `app.rs` 拿到新 SFTP client 那一刻)。
    pub owners: super::owners::OwnerNames,
    /// F218:这次加载完之后要**选中并滚动到**的那一条(末段名字,不含目录)。
    ///
    /// 由 `app.rs` 在发出 `Goto(父目录)` **之后**写入 —— `begin_load` 会
    /// `clear_selection`,写在它之前的话选中会被自己清掉。
    ///
    /// 只在 `accept` 里消费一次,不留:留着的话下一次刷新会把用户手点的
    /// 选中再改回来。
    pub reveal_pick: Option<RemotePath>,
    /// F218:下一帧要把视口滚到哪一条上。由 [`PaneState::accept`] 消费
    /// `reveal_pick` 时置上,画面那侧 `take()` 掉。
    ///
    /// 存名字不存行号:行号要等 `rows()`(排序 + 隐藏过滤)算完才知道,
    /// 而那是画面那一侧的事。
    pub scroll_to: Option<RemotePath>,
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
            selected: std::collections::BTreeSet::new(),
            cursor: None,
            anchor: None,
            request_seq: 0,
            path_edit: None,
            rename_edit: None,
            new_edit: None,
            owners: super::owners::OwnerNames::default(),
            reveal_pick: None,
            scroll_to: None,
        }
    }

    /// 开始一次加载,返回本次的序号。调用方把它随异步任务带走,
    /// 结果回来时用 `accept` 校验。
    pub fn begin_load(&mut self, cwd: RemotePath) -> u64 {
        self.cwd = cwd;
        self.load = Load::Loading;
        self.clear_selection();
        // F200:换目录了 —— 那个输入框会浮在另一个目录的某一行上,而回车
        // 拼出来的路径用的是新 cwd,改的是另一个文件。
        self.rename_edit = None;
        // F219:同上 —— 那个输入框回车拼出来的路径用的是**新** cwd,
        // 建出来的文件会落在用户根本没在看的目录里。
        self.new_edit = None;
        self.request_seq += 1;
        self.request_seq
    }

    /// F132:这一栏连的机器要换了 —— 把它作废,等新连接重新加载。
    ///
    /// 三件事一件都不能少:
    /// - `request_seq += 1`:在途的列目录结果是**另一台**机器上的内容,回来时
    ///   必须被 `accept` 的序号校验丢掉,否则新连接刚开好就被旧机器的目录覆盖。
    /// - `load` 退回 `Idle`:留在 `Loading` 的话,`App::trigger_sftp_open` 的
    ///   `already_loading` 早退会让新的 open 一个字节都发不出去,面板永久转圈
    ///   —— 而那之后 `has_client` 是 `false`,判定在第一行就短路,**没有任何
    ///   自愈路径**,只能重启。
    /// - 清选中:选的是另一台上的文件,留着只会让下一次操作打到不存在的路径。
    ///
    /// 跟 `begin_load` 分开是因为这里**没有**下一个 cwd 可填:去哪个目录要等
    /// 新连接 `canonicalize(".")` 回来才知道(F123)。
    pub fn invalidate(&mut self) {
        self.load = Load::Idle;
        self.clear_selection();
        // F200:同 `begin_load` —— 换的还是另一台机器,更不能留。
        self.rename_edit = None;
        // F219:同上 —— 换的还是另一台机器,更不能留。
        self.new_edit = None;
        // F218:同理 —— 那一条是**另一台**机器上的文件名。留着的话,新机器上
        // 恰好有个同名文件就会被莫名其妙地选中并滚到跟前。
        self.reveal_pick = None;
        self.scroll_to = None;
        self.request_seq += 1;
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
                // 刷新后已经不存在的条目要从选择集里剔掉 —— 留着的话,
                // 删除确认框会列出远端已经没有的路径,用户点确认后收到
                // 一条 NoSuchFile,完全不知道自己删的是什么。
                self.prune_selection();
                // F200:改名途中那一行没了(别人删了 / 传输完自动刷新),
                // 编辑态跟着消失。留着的话回车会拿一个已经不存在的原名去拼
                // 请求,而那个输入框还浮在某一行上、指着的已经是别的文件。
                if self
                    .rename_edit
                    .as_ref()
                    .is_some_and(|r| !self.entries.iter().any(|e| e.name == r.from))
                {
                    self.rename_edit = None;
                }
                // F219:`new_edit` 故意不跟着上面那段一起判 —— 它不绑定任何
                // 一行(见 `NewEdit` 的文档),没有「那一行没了」这回事,清掉
                // 只会把用户正在打的字吞掉。
                self.take_reveal_pick();
            }
            Err(msg) => {
                self.entries.clear();
                self.load = Load::Failed(msg);
            }
        }
        true
    }

    /// F218:目录刚列完 —— 把「要亮给用户看的那一条」落到选中 + 光标 +
    /// 待滚动上。没有待办、或那一条不在这批 entries 里就什么都不做。
    ///
    /// **隐藏文件要顺手打开开关**:`.gitignore`/`.env` 这类是常划的路径,
    /// 而 `rows()` 会把它们过滤掉 —— 不打开的话,选中和滚动都落在一条
    /// **画不出来**的行上,用户看到的是「按了没反应」。
    fn take_reveal_pick(&mut self) {
        let Some(pick) = self.reveal_pick.take() else {
            return;
        };
        if !self.entries.iter().any(|e| e.name == pick) {
            return;
        }
        if pick.as_bytes().starts_with(b".") {
            self.show_hidden = true;
        }
        self.select_only(&pick);
        self.scroll_to = Some(pick);
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

    /// 清空选择集、光标与锚点。换目录时必须**整套**清 —— 只清一部分的话,
    /// 新目录里同名的文件会凭空带着上一个目录的选中态,而删除不可逆。
    pub fn clear_selection(&mut self) {
        self.selected.clear();
        self.cursor = None;
        self.anchor = None;
    }

    /// 把已经不在 `entries` 里的选中项 / 光标 / 锚点剔掉。
    fn prune_selection(&mut self) {
        let alive: std::collections::BTreeSet<&RemotePath> =
            self.entries.iter().map(|e| &e.name).collect();
        self.selected.retain(|p| alive.contains(p));
        if self.cursor.as_ref().is_some_and(|p| !alive.contains(p)) {
            self.cursor = None;
        }
        if self.anchor.as_ref().is_some_and(|p| !alive.contains(p)) {
            self.anchor = None;
        }
    }

    /// 某一行是不是选中的。渲染每行都要问一次,所以是集合查找不是线性扫。
    pub fn is_selected(&self, name: &RemotePath) -> bool {
        self.selected.contains(name)
    }

    /// 选中项,**按当前可见行序**给出。
    ///
    /// 删除确认框列路径、将来的批量下载排队都用它 —— 用户看到的顺序和
    /// 我们处理的顺序一致,对账才对得上。
    pub fn selected_paths(&self) -> Vec<RemotePath> {
        self.rows()
            .into_iter()
            .filter(|e| self.selected.contains(&e.name))
            .map(|e| e.name.clone())
            .collect()
    }

    /// 操作目标:**选中集非空就是选中集,否则退化成光标那一条**(都按可见
    /// 行序)。F52 的传输入口用它。
    ///
    /// 这条「没选中就当选了光标行」的退化语义与 `FileAsk::Delete` 一致
    /// (见 `App::open_files_dialog`)——用户右键一条没选中的行、或者用键盘
    /// 移到某行直接发起操作时,想要的就是那一条;弹一个「没有选中任何条目」
    /// 只会让人以为程序坏了。
    ///
    /// 返回的是 `Entry` 而不是路径:传输要的不只是名字,还要 `kind`(目录得
    /// 递归展开)和 `size`(进度条的分母)。
    pub fn picked_entries(&self) -> Vec<&Entry> {
        let picked: Vec<RemotePath> = if self.selected.is_empty() {
            self.cursor.iter().cloned().collect()
        } else {
            self.selected_paths()
        };
        self.rows()
            .into_iter()
            .filter(|e| picked.contains(&e.name))
            .collect()
    }

    /// F200:开始就地改名**光标那一行**。返回是否真的进了编辑态。
    ///
    /// 单目标(认光标行而不是选择集)与 `FileAsk::Chmod` 同一条既有约定 ——
    /// 多选了 5 条时「改哪一条的名字」没有答案。
    ///
    /// 名字发不出去(`is_operable` 为假)的行直接不让开始:`rename` 请求打
    /// 不中那个文件,让用户敲半天再报一句 `NoSuchFile` 只是把失败推后。
    pub fn begin_rename(&mut self) -> bool {
        // F219:两个就地输入框互斥,见 `PaneState::new_edit` 的文档。
        self.new_edit = None;
        let Some(cur) = self.cursor.clone() else {
            return false;
        };
        if !cur.is_operable() || !self.entries.iter().any(|e| e.name == cur) {
            return false;
        }
        self.rename_edit = Some(RenameEdit {
            buf: cur.display().to_string(),
            from: cur,
            focus_pending: true,
        });
        true
    }

    /// F219:开始在**当前目录**里就地新建一个文件。返回是否真的进了新建态。
    ///
    /// 不像 `begin_rename` 那样需要光标行 —— 新建不针对任何一条已有的行,
    /// 空目录里同样成立(那正是用户最需要它的时候)。
    pub fn begin_new_file(&mut self) -> bool {
        // 互斥,见 `new_edit` 的文档。
        self.rename_edit = None;
        self.new_edit = Some(NewEdit {
            buf: String::new(),
            focus_pending: true,
        });
        true
    }

    /// F202:这一栏按下 Delete 要删的东西 —— 绝对路径配一个「是不是目录」。
    ///
    /// 抽成一个函数是因为它有**两个调用方、判据必须逐字一致**:确认框
    /// (裸 Delete)和免确认的 Shift+Del。各算一遍的话,免确认那条路上没有
    /// 任何东西会让用户发现两边算得不一样 —— 而这一片不可逆。
    ///
    /// 发不出去的名字(收包时已被 lossy 成 `U+FFFD`,见 `RemotePath::as_wire`)
    /// 直接剔掉:请求打不中那个文件,留着只会让计数多出一条。
    pub fn delete_targets(&self) -> Vec<(RemotePath, bool)> {
        self.picked_entries()
            .into_iter()
            .filter(|e| e.name.is_operable())
            .map(|e| (self.cwd.join(e.name.as_bytes()), e.kind == EntryKind::Dir))
            .collect()
    }

    /// 点了一行。`ctrl` = 切换单条,`shift` = 从锚点到这里的闭区间,
    /// 都不按 = 只选这一条。
    ///
    /// **范围按可见行序算**(`rows()`),不是 `entries` 的存储序:点过列头
    /// 重排、或关着隐藏文件时,用户说的「从这儿到那儿」指的是他眼里看到
    /// 的那一段。按存储序算会把一条他从没见过的 `.env` 选进删除列表。
    pub fn click_row(&mut self, name: &RemotePath, ctrl: bool, shift: bool) {
        if shift {
            let Some(anchor) = self.anchor.clone() else {
                self.select_only(name);
                return;
            };
            let order: Vec<RemotePath> = self.rows().into_iter().map(|e| e.name.clone()).collect();
            let (Some(a), Some(b)) = (
                order.iter().position(|p| *p == anchor),
                order.iter().position(|p| p == name),
            ) else {
                // 锚点已经不在可见行里(被过滤掉 / 刷新后没了)——
                // 退化成平点,不猜用户想选哪一段。
                self.select_only(name);
                return;
            };
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            self.selected = order[lo..=hi].iter().cloned().collect();
            self.cursor = Some(name.clone());
            // 锚点**不动** —— 动了的话连续 Shift 点会变成一段接一段。
            return;
        }
        if ctrl {
            if !self.selected.remove(name) {
                self.selected.insert(name.clone());
            }
            self.cursor = Some(name.clone());
            self.anchor = Some(name.clone());
            return;
        }
        self.select_only(name);
    }

    /// 只选这一条,光标与锚点都落到它上面。
    pub fn select_only(&mut self, name: &RemotePath) {
        self.selected.clear();
        self.selected.insert(name.clone());
        self.cursor = Some(name.clone());
        self.anchor = Some(name.clone());
    }

    /// F150:栏底状态行的文案。
    ///
    /// 这一行同时是**用户唯一能看见的选中证据** —— 用户报过「按 Ctrl 点,
    /// 屏幕上完全没变化」,当时高亮画得比背景还暗,除了这行字之外没有任何
    /// 途径能分辨「没选上」和「选上了但看不见」。
    ///
    /// 两条口径:
    /// - 计数按**可见行**(`rows()`),不是 `entries.len()` —— 关着隐藏文件时
    ///   两者不一样,报存储数跟用户眼睛看到的对不上。
    /// - 体积只算文件。目录的 `size` 在 SFTP 里是元数据大小(常见 4096),
    ///   加进去给出的是个没有意义的数;而全选目录时干脆不拼体积,拼出来
    ///   「· 0 B」是在撒谎。
    pub fn status_text(&self) -> String {
        let rows = self.rows();
        let picked: Vec<&Entry> = rows
            .iter()
            .copied()
            .filter(|e| self.selected.contains(&e.name))
            .collect();
        if picked.is_empty() {
            return format!("{} 项", rows.len());
        }
        let bytes: u64 = picked
            .iter()
            .filter(|e| e.kind != EntryKind::Dir)
            .map(|e| e.size)
            .sum();
        let has_file = picked.iter().any(|e| e.kind != EntryKind::Dir);
        if has_file {
            format!("已选 {} 项 · {}", picked.len(), super::human_size(bytes))
        } else {
            format!("已选 {} 项", picked.len())
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

    /// F218:目录列完之后,待亮的那一条要成为**唯一选中项 + 光标**,并留下
    /// 待滚动标记。
    ///
    /// 自证会变红:把 `accept` 里的 `self.take_reveal_pick()` 删掉。
    #[test]
    fn a_revealed_file_becomes_the_only_selection_and_asks_to_be_scrolled_to() {
        let mut s = state();
        s.reveal_pick = Some(RemotePath::from_bytes(b"b.txt".to_vec()));
        s.accept(
            s.request_seq,
            Ok(vec![
                e("a.txt", EntryKind::File),
                e("b.txt", EntryKind::File),
            ]),
        );
        assert_eq!(s.selected_paths().len(), 1, "该只选中一条:{:?}", s.selected);
        assert!(s.is_selected(&RemotePath::from_bytes(b"b.txt".to_vec())));
        assert_eq!(
            s.cursor.as_ref().map(|c| c.display().to_string()),
            Some("b.txt".into())
        );
        assert_eq!(
            s.scroll_to.as_ref().map(|c| c.display().to_string()),
            Some("b.txt".into()),
            "没留下待滚动标记 —— 大目录里那一条会被选中在视口外,用户看不见"
        );
        assert!(
            s.reveal_pick.is_none(),
            "待办该被消费掉,留着下次刷新会再改一次选中"
        );
    }

    /// F218:待亮的是隐藏文件时,**顺手打开隐藏文件开关** —— `rows()` 会把
    /// 它过滤掉,选中和滚动都落在一条画不出来的行上,用户看到的是「按了没反应」。
    ///
    /// 自证会变红:把 `take_reveal_pick` 里那句 `self.show_hidden = true` 删掉。
    #[test]
    fn revealing_a_dotfile_turns_the_hidden_switch_on_so_it_is_actually_visible() {
        let mut s = state();
        assert!(!s.show_hidden, "前提:默认不显示隐藏文件");
        s.reveal_pick = Some(RemotePath::from_bytes(b".gitignore".to_vec()));
        s.accept(
            s.request_seq,
            Ok(vec![
                e(".gitignore", EntryKind::File),
                e("a.txt", EntryKind::File),
            ]),
        );
        assert!(s.show_hidden);
        assert!(
            s.rows().iter().any(|r| r.name.display() == ".gitignore"),
            "那一条仍然被过滤在可见行之外"
        );
    }

    /// F218:待亮的那一条这批里没有(被别人删了 / 传输途中刷新)——
    /// 什么都不动,不去乱选一条。
    #[test]
    fn a_reveal_target_that_is_not_in_the_listing_changes_nothing() {
        let mut s = state();
        s.reveal_pick = Some(RemotePath::from_bytes(b"gone.txt".to_vec()));
        s.accept(s.request_seq, Ok(vec![e("a.txt", EntryKind::File)]));
        assert!(s.selected.is_empty());
        assert!(s.scroll_to.is_none());
    }

    /// F52:没选中任何东西时,操作目标退化成光标那一条 —— 右键一条没选中的
    /// 行就发起传输是最常见的用法,这里空手而归的话菜单点了等于没点。
    #[test]
    fn nothing_selected_means_the_cursor_row_is_the_target() {
        let mut s = state();
        s.accept(
            s.request_seq,
            Ok(vec![
                e("a.txt", EntryKind::File),
                e("b.txt", EntryKind::File),
            ]),
        );
        s.cursor = Some(RemotePath::from_bytes(b"b.txt".to_vec()));
        let picked = s.picked_entries();
        assert_eq!(picked.len(), 1, "该退化成光标那一条:{picked:?}");
        assert_eq!(picked[0].name.display(), "b.txt");
    }

    /// F200:F2 就地改名 —— 进编辑态时缓冲要**预填当前名字**。
    /// 空着的话用户得从零打一遍,而改名十有八九只改末尾几个字。
    ///
    /// 自证会变红:把 `buf` 初值改成 `String::new()`。
    #[test]
    fn f2_starts_renaming_the_cursor_row_seeded_with_its_current_name() {
        let mut s = state();
        s.accept(s.request_seq, Ok(vec![e("notes.txt", EntryKind::File)]));
        s.cursor = Some(RemotePath::from_bytes(b"notes.txt".to_vec()));
        assert!(s.begin_rename(), "光标行在,该进得了编辑态");
        let r = s.rename_edit.as_ref().expect("没进编辑态");
        assert_eq!(r.from.display(), "notes.txt");
        assert_eq!(r.buf, "notes.txt", "缓冲没预填原名");
    }

    /// 发不出去的名字改不了名:`rename` 请求打不中那个文件,让用户进编辑态
    /// 敲半天再报一句 `NoSuchFile`,不如一开始就不让开始(与 `row()` 把这类
    /// 行画成 dim 是同一条判据)。
    ///
    /// 自证会变红:去掉 `begin_rename` 里的 `is_operable` 判断。
    #[test]
    fn a_name_we_cannot_send_cannot_be_renamed_in_place() {
        let mut s = state();
        let bad = Entry {
            name: RemotePath::from_bytes(vec![0xff, 0xfe]),
            ..e("x", EntryKind::File)
        };
        let name = bad.name.clone();
        s.accept(s.request_seq, Ok(vec![bad]));
        s.cursor = Some(name);
        assert!(!s.begin_rename(), "发不出去的名字不该能进编辑态");
        assert!(s.rename_edit.is_none());
    }

    /// 改名途中目录刷新、那一行没了(别人删了 / 传输完自动刷新)——
    /// 编辑态必须自己消失。留着的话回车会拿一个**已经不存在的原名**去
    /// 拼请求,而界面上那个输入框还浮在某一行上,指着的已经是另一个文件。
    /// 与 `selected`/`anchor` 存身份而不是下标是同一套自愈思路。
    ///
    /// 自证会变红:把 `accept` 里清 `rename_edit` 那一句删掉。
    #[test]
    fn a_refresh_that_loses_the_row_drops_the_rename_so_it_cannot_hit_another_file() {
        let mut s = state();
        s.accept(s.request_seq, Ok(vec![e("a.txt", EntryKind::File)]));
        s.cursor = Some(RemotePath::from_bytes(b"a.txt".to_vec()));
        assert!(s.begin_rename());
        s.accept(s.request_seq, Ok(vec![e("b.txt", EntryKind::File)]));
        assert!(s.rename_edit.is_none(), "那一行没了,编辑态还赖着");
    }

    /// 换目录 / 换机器时改名编辑态也要没。`begin_load` 之后列表是**另一个
    /// 目录**的,那个输入框会浮在一条毫不相干的行上,而回车拼出来的路径
    /// 用的是新 cwd —— 改的是另一台机器上的另一个文件。
    ///
    /// 自证会变红:把 `begin_load`/`invalidate` 里清 `rename_edit` 的那句删掉。
    #[test]
    fn leaving_the_directory_drops_the_rename_edit() {
        for nav in [0, 1] {
            let mut s = state();
            s.accept(s.request_seq, Ok(vec![e("a.txt", EntryKind::File)]));
            s.cursor = Some(RemotePath::from_bytes(b"a.txt".to_vec()));
            assert!(s.begin_rename());
            if nav == 0 {
                s.begin_load(RemotePath::from_bytes(b"/tmp".to_vec()));
            } else {
                s.invalidate();
            }
            assert!(s.rename_edit.is_none(), "换目录/换机器后编辑态还赖着");
        }
    }

    /// F219:进新建态之后,缓冲是空的、焦点待办是真。
    #[test]
    fn beginning_a_new_file_opens_an_empty_buffer_asking_for_focus() {
        let mut s = state();
        assert!(s.begin_new_file(), "该进得了新建态");
        let n = s.new_edit.as_ref().expect("没进新建态");
        assert_eq!(n.buf, "", "新建的输入框该是空的");
        assert!(n.focus_pending, "没要焦点 —— 用户得先拿鼠标点一下才能打字");
    }

    /// F219:两个就地输入框**不能同时活着** —— egui 里两个 `TextEdit` 会互抢
    /// 键盘焦点,先进编辑态那个永远 `lost_focus()` 不了、退不出来(F131 实测)。
    ///
    /// 自证会变红:把 `begin_new_file` 里清 `rename_edit` 的那句删掉。
    #[test]
    fn starting_a_new_file_cancels_an_in_flight_rename_and_the_other_way_round() {
        let mut s = state();
        s.accept(s.request_seq, Ok(vec![e("a.txt", EntryKind::File)]));
        s.cursor = Some(RemotePath::from_bytes(b"a.txt".to_vec()));
        assert!(s.begin_rename(), "前提:进得了改名态");
        assert!(s.begin_new_file());
        assert!(
            s.rename_edit.is_none(),
            "改名态还赖着 —— 两个输入框会互抢焦点"
        );

        let mut s = state();
        s.accept(s.request_seq, Ok(vec![e("a.txt", EntryKind::File)]));
        s.cursor = Some(RemotePath::from_bytes(b"a.txt".to_vec()));
        assert!(s.begin_new_file());
        assert!(s.begin_rename(), "前提:进得了改名态");
        assert!(s.new_edit.is_none(), "新建态还赖着 —— 同上");
    }

    /// F219:换目录 / 换机器之后,那个输入框回车拼出来的是**另一个目录**里的
    /// 路径 —— 必须清掉。
    ///
    /// 自证会变红:把 `begin_load`/`invalidate` 里清 `new_edit` 的那句删掉。
    #[test]
    fn leaving_the_directory_drops_the_new_file_edit() {
        for leave in [0u8, 1] {
            let mut s = state();
            assert!(s.begin_new_file());
            if leave == 0 {
                s.begin_load(RemotePath::from_bytes(b"/tmp".to_vec()));
            } else {
                s.invalidate();
            }
            assert!(s.new_edit.is_none(), "换目录/换机器后新建态还赖着");
        }
    }

    /// F219:`begin_rename` 清 `new_edit` 那句在**函数体第一句**,在
    /// 「有没有光标行」那道闸门之前 —— 空目录里打了一半新文件名,误按
    /// 或连按 F2(此时没有光标行,`begin_rename` 会返回 `false`),新建框
    /// 也该跟着让位,不能因为改名没进得去就赖着不走。
    ///
    /// 自证会变红:把 `begin_rename` 里 `self.new_edit = None;` 挪到
    /// `let Some(cur) = self.cursor.clone() else { return false; }` 之后。
    #[test]
    fn begin_rename_clears_the_new_file_edit_even_when_it_fails_to_start() {
        let mut s = state();
        assert!(s.begin_new_file(), "前提:进得了新建态");
        assert!(
            !s.begin_rename(),
            "前提:没有光标行,改名该失败 —— 否则这条测的是成功路径,缺口原样存在"
        );
        assert!(s.new_edit.is_none(), "改名失败也不该让新建框继续赖着");
    }

    /// F219:**刷新不清新建态** —— 它不绑任何已有行,清掉会把用户正在打的字
    /// 吞掉(与 `rename_edit` 的处置故意不同:那个绑着一条具体的行)。
    ///
    /// 自证会变红:在 `accept` 里加一句 `self.new_edit = None;`。
    #[test]
    fn a_refresh_keeps_the_new_file_edit_because_it_is_not_tied_to_any_row() {
        let mut s = state();
        assert!(s.begin_new_file());
        s.new_edit.as_mut().unwrap().buf = "half-typed".into();
        let seq = s.request_seq;
        assert!(s.accept(seq, Ok(vec![e("a.txt", EntryKind::File)])));
        assert_eq!(
            s.new_edit.as_ref().map(|n| n.buf.as_str()),
            Some("half-typed"),
            "刷新把用户正在打的字吞了"
        );
    }

    /// F202:删除目标是**绝对路径 + 是不是目录**。抽成一个函数是因为它有
    /// 两个调用方(确认框走的裸 Delete、免确认的 Shift+Del),两边算得
    /// 不一样的话,免确认那条路上没有任何东西会让用户发现 —— 而删除不可逆。
    ///
    /// 自证会变红:把 `cwd.join` 换成只给名字(路径就打到当前工作目录去了),
    /// 或者把 `EntryKind::Dir` 那个判断写反(递归删会落到普通文件上)。
    #[test]
    fn delete_targets_are_absolute_paths_paired_with_dir_flags() {
        let mut s = state();
        s.accept(
            s.request_seq,
            Ok(vec![e("a.txt", EntryKind::File), e("d", EntryKind::Dir)]),
        );
        s.selected = ["a.txt", "d"]
            .iter()
            .map(|n| RemotePath::from_bytes(n.as_bytes().to_vec()))
            .collect();
        let got: Vec<(String, bool)> = s
            .delete_targets()
            .into_iter()
            .map(|(p, d)| (p.display().to_string(), d))
            .collect();
        assert_eq!(
            got,
            // 顺序 = 可见行序(目录排在文件前),与确认框上逐条列出的
            // 顺序同源 —— 对账要对得上。
            vec![
                ("/home/u/d".to_string(), true),
                ("/home/u/a.txt".to_string(), false),
            ],
            "删除目标算错了"
        );
    }

    /// 发不出去的名字(收包时已被 lossy,`as_wire` 挡下)不许进删除列表:
    /// 请求打不中那个文件,留着只会让确认框上的计数多一条 —— 用户以为
    /// 删了 2 个,实际 1 个。
    ///
    /// 自证会变红:去掉 `is_operable` 那道过滤。
    #[test]
    fn a_name_we_cannot_send_never_becomes_a_delete_target() {
        let mut s = state();
        let bad = Entry {
            name: RemotePath::from_bytes(vec![0xff, 0xfe, b'.', b't', b'x', b't']),
            ..e("x", EntryKind::File)
        };
        assert!(!bad.name.is_operable(), "这个名字本该是发不出去的");
        s.accept(s.request_seq, Ok(vec![e("ok.txt", EntryKind::File), bad]));
        s.selected = s.entries.iter().map(|e| e.name.clone()).collect();
        let got = s.delete_targets();
        assert_eq!(got.len(), 1, "发不出去的名字混进来了:{got:?}");
        assert_eq!(got[0].0.display(), "/home/u/ok.txt");
    }

    /// 有选中集时**光标不额外算一条** —— 否则用户选了 3 个、光标停在第 4 个
    /// 上,会莫名其妙传 4 个文件。
    #[test]
    fn a_selection_wins_over_the_cursor_so_nothing_extra_is_transferred() {
        let mut s = state();
        s.accept(
            s.request_seq,
            Ok(vec![
                e("a.txt", EntryKind::File),
                e("b.txt", EntryKind::File),
                e("c.txt", EntryKind::File),
            ]),
        );
        s.selected = [RemotePath::from_bytes(b"a.txt".to_vec())]
            .into_iter()
            .collect();
        s.cursor = Some(RemotePath::from_bytes(b"c.txt".to_vec()));
        let names: Vec<String> = s
            .picked_entries()
            .iter()
            .map(|e| e.name.display().into_owned())
            .collect();
        assert_eq!(names, vec!["a.txt".to_string()], "光标那条被多算了进来");
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
        s.select_only(&picked);
        assert_eq!(s.rows()[0].name, picked);

        s.click_header(SortKey::Name); // 同一列再点 → 翻成降序

        assert_eq!(
            s.rows()[1].name,
            picked,
            "前提:重排确实把它挪到了另一行,否则这条测试什么也没守到"
        );
        assert!(s.is_selected(&picked), "选中跟着文件走,不跟着行号走");
    }

    fn ready(names: &[&str]) -> PaneState {
        let mut s = state();
        s.entries = names.iter().map(|n| e(n, EntryKind::File)).collect();
        s.load = Load::Ready;
        s
    }

    fn rp(name: &str) -> RemotePath {
        RemotePath::from_bytes(name.as_bytes().to_vec())
    }

    /// 平点一行:清空原有选择,只留这一条,光标与锚点都落到它上面。
    #[test]
    fn a_plain_click_selects_exactly_one_row() {
        let mut s = ready(&["a", "b", "c"]);
        s.click_row(&rp("a"), false, false);
        s.click_row(&rp("c"), false, false);
        assert_eq!(s.selected_paths(), vec![rp("c")], "平点该只剩最后点的那条");
        assert_eq!(s.cursor.as_ref(), Some(&rp("c")));
        assert_eq!(s.anchor.as_ref(), Some(&rp("c")));
    }

    /// Ctrl 点:切换那一条的选中态,其余不动。
    #[test]
    fn a_ctrl_click_toggles_one_row_without_clearing_the_rest() {
        let mut s = ready(&["a", "b", "c"]);
        s.click_row(&rp("a"), false, false);
        s.click_row(&rp("c"), true, false);
        assert_eq!(s.selected_paths(), vec![rp("a"), rp("c")]);
        // 再 Ctrl 点一次 c 应当取消它
        s.click_row(&rp("c"), true, false);
        assert_eq!(s.selected_paths(), vec![rp("a")]);
    }

    /// Shift 点:从锚点到这一条**闭区间**全选,按当前**可见行序**算 ——
    /// 不是按 `entries` 的存储顺序,也不是按字节序。
    #[test]
    fn a_shift_click_selects_the_inclusive_visible_range() {
        let mut s = ready(&["a", "b", "c", "d"]);
        s.click_row(&rp("b"), false, false); // 锚点 = b
        s.click_row(&rp("d"), false, true);
        assert_eq!(s.selected_paths(), vec![rp("b"), rp("c"), rp("d")]);
        assert_eq!(s.anchor.as_ref(), Some(&rp("b")), "Shift 不该挪动锚点");
        assert_eq!(s.cursor.as_ref(), Some(&rp("d")), "光标跟着走");
    }

    /// 反向 Shift(从下往上点)同样是闭区间。
    #[test]
    fn a_backwards_shift_click_selects_the_same_range() {
        let mut s = ready(&["a", "b", "c", "d"]);
        s.click_row(&rp("d"), false, false);
        s.click_row(&rp("b"), false, true);
        assert_eq!(s.selected_paths(), vec![rp("b"), rp("c"), rp("d")]);
    }

    /// 隐藏文件被过滤掉时,Shift 范围**不该把看不见的那条也选上** ——
    /// 用户选的是他看得见的那一段,删除确认框里冒出一条他从没见过的
    /// `.env` 是最坏的一种意外。
    #[test]
    fn a_shift_range_never_picks_up_rows_that_are_filtered_out() {
        let mut s = state();
        // 存储序里 `.secret` 夹在 a 与 c 中间 —— 按存储序算范围就会选中它。
        s.entries = vec![
            e("a", EntryKind::File),
            e(".secret", EntryKind::File),
            e("c", EntryKind::File),
        ];
        s.load = Load::Ready;
        s.show_hidden = false;
        assert_eq!(s.rows().len(), 2, "前提:隐藏项确实被过滤掉了");

        s.click_row(&rp("a"), false, false);
        s.click_row(&rp("c"), false, true);
        // 断言打在**原始选择集**上,不是 `selected_paths()` —— 后者自己又按
        // `rows()` 过滤了一遍,会把偷偷选进来的隐藏项遮住,断言就恒绿了。
        assert!(
            !s.is_selected(&rp(".secret")),
            "被过滤掉的隐藏项不该混进范围选择: {:?}",
            s.selected
        );
        assert_eq!(s.selected_paths(), vec![rp("a"), rp("c")]);
    }

    /// 换目录必须把选择集、光标、锚点一起清干净 —— 留着的话,新目录里
    /// 恰好同名的文件会凭空「已选中」,而删除是不可逆的。
    #[test]
    fn navigating_away_clears_the_whole_selection() {
        let mut s = ready(&["a", "b"]);
        s.click_row(&rp("a"), false, false);
        s.click_row(&rp("b"), true, false);
        s.begin_load(rp("/elsewhere"));
        assert!(s.selected.is_empty(), "换目录该清空选择集");
        assert!(s.cursor.is_none());
        assert!(s.anchor.is_none());
    }

    /// 刷新之后,**已经不在列表里的选中项要被丢掉**。留着的话,删除确认框
    /// 会列出一个远端已经没有的路径,用户点确认后收到一条 NoSuchFile,
    /// 完全不知道自己删的是什么。
    #[test]
    fn a_refresh_drops_selections_that_no_longer_exist() {
        let mut s = ready(&["a", "b"]);
        let seq = s.begin_load(rp("/home/u"));
        // `begin_load` 已经清了,这里手工放回去 —— 模拟「刷新当前目录时
        // 选中态还在」这个真实场景(刷新走的是同一条 accept 路径)。
        s.selected.insert(rp("a"));
        s.selected.insert(rp("b"));
        s.cursor = Some(rp("b"));
        s.anchor = Some(rp("b"));

        assert!(s.accept(seq, Ok(vec![e("a", EntryKind::File)])));

        assert_eq!(s.selected_paths(), vec![rp("a")], "没了的那条该被丢掉");
        assert!(s.cursor.is_none(), "光标指着的那条没了,光标也该清掉");
        assert!(s.anchor.is_none(), "锚点同理");
    }

    /// F132:换机器前作废这一栏 —— 三件事各自都能单独出事,逐条钉。
    ///
    /// 尤其**递增序号**那条:少了它,在途的那次列目录(内容来自**上一台**
    /// 机器)回来时序号照样对得上,会被当成新连接的结果收下,用户于是在
    /// B 机的面板里看着 A 机的目录 —— 而这正是 F132 要修的那类错位。
    ///
    /// 自证会变红:把 `invalidate` 里的 `request_seq += 1` / `load = Idle` /
    /// `clear_selection()` 分别删掉,对应断言各红一条。
    #[test]
    fn invalidating_a_pane_makes_the_in_flight_listing_of_the_old_host_stale() {
        let mut s = state();
        let stale = s.begin_load(rp("/srv"));
        s.selected.insert(rp("a"));
        s.cursor = Some(rp("a"));

        s.invalidate();

        assert!(
            !s.accept(stale, Ok(vec![e("from-old-host", EntryKind::File)])),
            "旧机器那次列目录还能被收下 —— 面板会显示另一台上的内容"
        );
        assert_eq!(
            s.load,
            Load::Idle,
            "留在 Loading 的话 trigger_sftp_open 会撞 already_loading 早退,面板永久转圈"
        );
        assert!(
            s.selected_paths().is_empty() && s.cursor.is_none(),
            "选中/光标指的是另一台上的文件,留着会让下一次操作打到不存在的路径"
        );
    }

    /// F218:换机器时那条「待亮的文件」也要作废 —— 它是**另一台**上的文件名。
    ///
    /// `invalidate` 递增了序号,所以旧那次列目录进不来;但新连接的**下一次**
    /// 列目录序号是对的,待办留着就会在新机器上找同名文件,恰好有一个就被
    /// 莫名其妙地选中并滚到跟前,而用户压根没在这台机器上按过那个键。
    ///
    /// 自证会变红:把 `invalidate` 里的 `self.reveal_pick = None;` 删掉。
    #[test]
    fn a_reveal_pick_does_not_survive_a_host_switch() {
        let mut s = state();
        s.reveal_pick = Some(rp("same-name.txt"));

        s.invalidate();
        let seq = s.begin_load(rp("/srv"));
        assert!(s.accept(seq, Ok(vec![e("same-name.txt", EntryKind::File)])));

        assert!(
            s.selected_paths().is_empty() && s.scroll_to.is_none(),
            "新机器上的同名文件被当成上一台的跳转目标亮起来了"
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

    /// 没选中时状态行报的是**可见行数**,不是 `entries.len()` ——
    /// 关着隐藏文件时两者不一样,报存储数就跟用户眼睛看到的对不上。
    #[test]
    fn the_status_line_counts_visible_rows_when_nothing_is_selected() {
        let mut s = PaneState::new(rp("/"));
        s.entries = vec![
            e("a", EntryKind::File),
            e("b", EntryKind::File),
            e(".hidden", EntryKind::File),
        ];
        s.load = Load::Ready;
        assert_eq!(s.status_text(), "2 项", "隐藏文件不该计进去");
        s.show_hidden = true;
        assert_eq!(s.status_text(), "3 项");
    }

    /// 选中时报条数 + 体积。体积只算文件 —— 目录的 `size` 在 SFTP 里是元数据
    /// 大小(常见 4096),加进去给出的是一个没有意义的数。
    #[test]
    fn the_status_line_reports_the_selection_size_counting_files_only() {
        let mut s = PaneState::new(rp("/"));
        let mut big = e("big.bin", EntryKind::File);
        big.size = 2048;
        let mut small = e("small.txt", EntryKind::File);
        small.size = 1024;
        let mut dir = e("logs", EntryKind::Dir);
        dir.size = 4096;
        s.entries = vec![big, small, dir];
        s.load = Load::Ready;
        s.selected.insert(rp("big.bin"));
        s.selected.insert(rp("small.txt"));
        s.selected.insert(rp("logs"));
        assert_eq!(
            s.status_text(),
            "已选 3 项 · 3.0 KB",
            "3 条(含一个目录),体积只算两个文件的 2048+1024"
        );
    }

    /// 只选了目录 → **不拼体积**。拼出来是「已选 1 项 · 0 B」,而目录当然
    /// 不是 0 字节,那行字是在撒谎。
    #[test]
    fn a_directory_only_selection_omits_the_size_instead_of_claiming_zero_bytes() {
        let mut s = PaneState::new(rp("/"));
        let mut dir = e("logs", EntryKind::Dir);
        dir.size = 4096;
        s.entries = vec![dir];
        s.load = Load::Ready;
        s.selected.insert(rp("logs"));
        assert_eq!(s.status_text(), "已选 1 项");
    }
}
