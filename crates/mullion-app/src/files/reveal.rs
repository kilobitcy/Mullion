//! F218:把终端选区里的一条路径变成「文件面板跳到那儿」。**纯逻辑** ——
//! 零 IO、零 async、零 egui,可脱离窗口单测。
//!
//! 三段判定各自成函数,理由与 `app::sync_plan_of` 那条注释一样:埋在
//! `&mut self` 方法体里的话,判据写反、顺序换掉都不会有任何测试变红。
//!
//! - [`parse`][] 选区串 → 目标(剥壳 + 判哪一栏)。
//! - [`arrived`][] 面板是不是**已经**停在这个目标上(决定「再按一次关栏」)。
//! - [`plan`][] 结合侧栏开合状态,这一下到底做什么。
//! - [`consume`][] 异机重开 sftp 之后,回来的这条 channel 是不是当初那台。
//!
//! **规整不在这里做**:剥完壳之后交给 F131 的
//! `path_input::resolve_remote_input` / `resolve_local_input`。自己再拼一份
//! 的话,「划 `../foo` 跳转」和「路径条敲 `../foo` 回车」会去到两个不同的
//! 地方 —— 那正是 `path_input` 模块头警告过的问题。

use super::PanelColumn;

/// 从选区里认出来的东西:去哪一栏、以及交给 `path_input` 的那一串。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub column: PanelColumn,
    /// 已经剥完壳的原文(仍可能是相对路径 / `~` 开头)。
    pub raw: String,
}

/// 目标路径的三个面。`arrived` 要用,而 POSIX 和本机两套路径的
/// 父目录/末段算法不同(`RemotePath::parent` vs `local::parent_local`),
/// 所以由调用方算好再传进来,这里只做比较。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Where<'a> {
    pub target: &'a [u8],
    pub parent: &'a [u8],
    pub base: &'a [u8],
}

/// 这一下按键要做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// 开栏,不跳(选区里没有路径)。
    Open,
    /// 开栏并跳过去。
    OpenAndReveal,
    /// 栏已经开着,跳过去。
    Reveal,
    /// 关栏。
    Close,
}

/// 尾部可以剥掉的标点。**不含 `.`** —— 剥它会把 `..` 吃成空串,而
/// 「划 `..` 跳上级」是合理用法;末段以 `.` 结尾的文件也是合法的。
const TRAILING_PUNCT: &[char] = &[
    ',', ';', ':', ')', ']', '}', '>', '，', '。', '；', '、', '）', '】', '」', '』',
];

/// 头部可以剥掉的标点。与 [`TRAILING_PUNCT`] 成对 —— 划中 `(crates/foo)`
/// 时只剥右半边的话,剩下的 `(crates/foo` 照样解析不出东西。
const LEADING_PUNCT: &[char] = &['(', '[', '{', '<', '（', '【', '「', '『'];

/// 成对引号。反引号也算 —— Markdown 里的路径几乎都裹在里面,而 Claude Code
/// 的回答正是本项目最主要的「路径来源」。
const QUOTES: &[(char, char)] = &[('"', '"'), ('\'', '\''), ('`', '`'), ('「', '」')];

/// 最多剥两级行列号:`app.rs:120` 和 `app.rs:120:9` 都是常见形态
/// (`cargo` 报错、`grep -n`、`rg`)。三级就不是行列号了。
const MAX_LINE_SUFFIXES: usize = 2;

/// 选区串 → 目标。`None` = 这一下不该跳(退化成纯开关)。
///
/// 判据全是**语法**的,不发任何请求 —— 按下去要立刻有反应,而这个键在
/// 高延迟代理链路上(本项目主场景)最忌讳「按了半秒才知道白按」。
///
/// 四道门,顺序不能换:
/// 1. **多行直接拒**。多行选区是 `ls`/`git status` 的一片输出,里面有 N 条
///    路径,挑哪条都是猜。
/// 2. trim 之后**整体**必须是一条路径 —— 不从句子里挖。挖的话
///    `error: xx in a/b.rs` 这种要靠启发式切词,切错就是跳到你没想去的地方,
///    而且静默。
/// 3. 剥壳:引号 →(标点 / 行列号)反复剥。
/// 4. **必须带路径分隔符或以 `~` 开头**。少了这道,划一个普通单词
///    (`cargo`、`error`)按下去就会解析成 `<cwd>/cargo` 然后弹一条「不存在」
///    —— 用户想要的只是开关侧栏。
pub fn parse(sel: &str) -> Option<Target> {
    if sel.contains('\n') || sel.contains('\r') {
        return None;
    }
    let mut s = sel.trim();
    s = strip_quotes(s);
    let mut peeled = 0;
    loop {
        let before = s;
        s = s
            .trim_start_matches(LEADING_PUNCT)
            .trim_end_matches(TRAILING_PUNCT)
            .trim();
        if peeled < MAX_LINE_SUFFIXES {
            if let Some(rest) = strip_line_suffix(s) {
                s = rest;
                peeled += 1;
            }
        }
        if s == before {
            break;
        }
    }
    if s.is_empty() {
        return None;
    }
    if !looks_like_path(s) {
        return None;
    }
    Some(Target {
        column: column_of(s),
        raw: s.to_string(),
    })
}

/// 剥一层成对引号。只剥一层 —— 两层引号的路径不存在于真实输出里,
/// 循环剥反而会把 `''` 这种空串吃出问题。
fn strip_quotes(s: &str) -> &str {
    for (open, close) in QUOTES {
        if let Some(rest) = s.strip_prefix(*open) {
            if let Some(inner) = rest.strip_suffix(*close) {
                return inner.trim();
            }
        }
    }
    s
}

/// 尾部是不是 `:<纯数字>`?是就返回剥掉之后的那一段。
///
/// **要求冒号左边还有东西**:`:12` 整串是数字端口之类,剥了就成空串。
fn strip_line_suffix(s: &str) -> Option<&str> {
    let (head, tail) = s.rsplit_once(':')?;
    if head.is_empty() || tail.is_empty() {
        return None;
    }
    if !tail.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(head)
}

/// 像不像一条路径:带分隔符,或以 `~` 开头(`~`、`~/x`)。
///
/// 光秃秃一个文件名(`app.rs`)**不算** —— 见 [`parse`] 第 4 道门。
fn looks_like_path(s: &str) -> bool {
    s.contains('/') || s.contains('\\') || s == "~" || s.starts_with("~/") || s.starts_with("~\\")
}

/// 哪一栏。**只有确凿的 Windows 绝对路径才判给本地栏**:盘符
/// (`C:\` / `C:/`)与 UNC(`\\server\share`)。
///
/// 其余一律远端 —— 包括一切相对路径。`docs/foo.md` 在两个平台上都合法,
/// 语法上判不出平台;而选区来自远端终端的输出,远端是压倒性的先验。
/// 反斜杠相对路径(`docs\foo.md`)同样归远端:反斜杠在 Linux 上是合法的
/// 文件名字符,为它特判会误伤真实存在的远端文件。
fn column_of(s: &str) -> PanelColumn {
    if s.starts_with("\\\\") {
        return PanelColumn::Local;
    }
    let b = s.as_bytes();
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
    {
        return PanelColumn::Local;
    }
    PanelColumn::Remote
}

/// 路径的末段。两栏的分隔符不同:远端是 POSIX(只认 `/`),本地在 Windows
/// 上 `/` 和 `\` 都算(`resolve_local_input` 会把 `D:\work/sub` 这种混写
/// 原样留下)。
pub fn base_name(path: &[u8], column: PanelColumn) -> Vec<u8> {
    let is_sep = |b: &u8| match column {
        PanelColumn::Remote => *b == b'/',
        PanelColumn::Local => *b == b'/' || *b == b'\\',
    };
    match path.iter().rposition(is_sep) {
        Some(i) => path[i + 1..].to_vec(),
        None => path.to_vec(),
    }
}

/// 面板是不是已经停在这个目标上了 —— 是的话再按一次就该关栏。
///
/// 两种到达形态:
/// - 目标是**目录**:当前目录就是它。
/// - 目标是**文件**:当前目录是它的父目录,**且**唯一选中项就是它。
///
/// 「且唯一选中项就是它」这半句不能省:只比目录的话,在 `src/` 里划
/// `app.rs` 跳过去、再划隔壁 `pane.rs` 按键,目录相同就直接关栏了 ——
/// 而用户要的是换选到 `pane.rs`。
///
/// 这里**判不出**目标究竟是文件还是目录(那要一次 `stat` 往返),所以两种
/// 形态取并集。已知代价:目标是目录 `/a/b`、而用户此刻正停在 `/a` 且选中了
/// `b`,会被判成「已到达」→ 关栏而不是进去。误判方向是「少跳一次」,
/// 不会跳到错的地方。
pub fn arrived(cwd: &[u8], only_selected: Option<&[u8]>, w: &Where) -> bool {
    if cwd == w.target {
        return true;
    }
    cwd == w.parent && only_selected == Some(w.base)
}

/// 侧栏开合 + 有没有目标 → 这一下做什么。
///
/// `target`:`None` = 选区里没认出路径;`Some(arrived)` = 认出来了,以及
/// 面板是不是已经在那儿。
pub fn plan(open: bool, target: Option<bool>) -> Plan {
    match (open, target) {
        (false, None) => Plan::Open,
        (false, Some(_)) => Plan::OpenAndReveal,
        (true, None) => Plan::Close,
        (true, Some(false)) => Plan::Reveal,
        (true, Some(true)) => Plan::Close,
    }
}

/// 异机跳转:重开好的这条 sftp channel,是不是当初发起时那台机器?
///
/// **不校验就是一次看不出错的误连**:等待重开的那半秒里用户可能又把焦点
/// 切到了第三台,回来的 client 是第三台的;而 `/home/ubuntu/...` 在几台机器
/// 上大概率都存在,面板会正常显示一个目录,没有任何迹象表明这是别人的文件。
pub fn consume(pending_host: Option<usize>, opened_host: Option<usize>) -> bool {
    pending_host == opened_host
}

/// F242:状态栏「当前选中的路径」那一格。
///
/// 存在的理由是 Ctrl+Shift+B **会把选区弄没**:开侧栏改变了终端的列数,
/// alacritty 在换列时无条件 `self.selection = None`(陷阱 T13 同源)。用户
/// 划完一条路径、按下键、选区消失 —— 在此之前他没有任何途径确认「我划中的
/// 到底是哪一串、按下去会去哪台机」。这一格就是那个确认。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusPath {
    /// 跨节点时的**起点**:文件面板此刻停在哪台。同节点 / 本地栏为 `None`。
    pub from: Option<String>,
    /// 目的地。`None` = 认出了一条路径,但落不到具体位置(面板还没开出
    /// sftp、拿不到基准目录…),显示成 `?`。
    pub to: Option<String>,
    /// 显示用的路径。能解析就是绝对路径,否则是剥完壳的选区原文。
    pub path: String,
}

/// 状态栏这一格的最小字符预算。低于这个数,省略号两边剩不下能认人的东西,
/// 那就不如让它溢出。
const MIN_BUDGET: usize = 16;

/// F242:单行闸门 + [`parse`]。
///
/// `text` **是惰性的**,而这正是这个函数存在的全部理由:`single_line` 为假
/// 时一次都不调用它。多行选区本来就会被 `parse` 的第一道门判死,但那道门
/// 开在「已经拼成 `String`」之后 —— F242 要每帧问一次结果,用户按住左键往回
/// 拖几百行回溯时,每帧先把几百 KB 拼出来再免费扔掉(陷阱 T3)。
///
/// `single_line` 的判据是 `Emulator::selection_is_single_line`,只读
/// `Selection::to_range` 的起止行,成本与选区大小无关。
pub fn parse_single_line(
    single_line: bool,
    text: impl FnOnce() -> Option<String>,
) -> Option<Target> {
    if !single_line {
        return None;
    }
    parse(&text()?)
}

/// F242:跨节点时那一格要写的**起点**。
///
/// 同机(`from == to`)给 `None`:写成 `a → a` 是纯噪音,而这一格的宽度要跟
/// 右侧四格抢。起点未知(`from` 为 `None`,面板还没开出 sftp)时也给 `None`
/// —— `? → b` 说不出任何东西。
pub fn crossing_from(from: Option<usize>, to: Option<usize>) -> Option<usize> {
    if from == to {
        None
    } else {
        from
    }
}

/// `(状态栏那一格, 悬停全文)`。
///
/// 两串**同源**:悬停要给的正是被中段省略吃掉的那部分,各拼各的就会出现
/// 「格子里写着 A、悬停显示 B」——而这种错只有把鼠标停上去才看得见。
pub fn status_cell(v: &StatusPath, budget: usize) -> (String, String) {
    let node = match (&v.from, &v.to) {
        // 跨节点:按下去面板会**换机器**,这是这一格最该说的事。
        (Some(f), Some(t)) => format!("{f} → {t}"),
        (_, Some(t)) => t.clone(),
        (_, None) => "?".to_string(),
    };
    (
        format!("· {node} · {}", elide_middle(&v.path, budget)),
        format!("{node} · {}", v.path),
    )
}

/// 中段省略,保头保尾。
///
/// **不能掐尾**(egui `Label::truncate` 那种):路径两头都是信息 —— 头上是
/// 根(哪个盘 / 家目录 / 仓库),尾上是文件名,而文件名正是用户唯一想确认的
/// 那半截。掐尾等于把这一格的用途删掉。
///
/// 尾巴分到 2/3:`…/mullion-app/src/app.rs` 比 `/data/Mullion/crates/…` 更能
/// 回答「我划中的是不是那一条」。
pub fn elide_middle(s: &str, budget: usize) -> String {
    let budget = budget.max(MIN_BUDGET);
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= budget {
        return s.to_string();
    }
    let keep = budget - 1; // 让出 `…` 一个字位
    let tail = keep * 2 / 3;
    let head = keep - tail;
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(&chars[chars.len() - tail..]);
    out
}

/// 这一格分到多少个**字符**。
///
/// 按字符而不是按像素排:状态栏用的不是等宽字体,量像素要走
/// `Fonts::layout`,那要 `&egui::Context` —— 这条判据就再也没法在无头容器里
/// 测了。估宽偏差的后果只是多截 / 少截几个字,不破版。
///
/// 一半留给右侧:错误、自动化、隧道、编码四格都在那边,它们比「我划中了
/// 什么」更该被看见。
pub fn char_budget(available_px: f32) -> usize {
    /// UI 字体(14px 非等宽)ASCII 的平均字宽估计。
    const APPROX_CHAR_PX: f32 = 7.0;
    const MAX_BUDGET: usize = 120;
    let n = (available_px * 0.5 / APPROX_CHAR_PX).max(0.0) as usize;
    n.clamp(MIN_BUDGET, MAX_BUDGET)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(s: &str) -> String {
        parse(s).expect("这一串应当认得出是路径").raw
    }

    /// 最常见的形态:`cargo`/`rg` 报错里的 `路径:行:列`。
    ///
    /// 自证会变红:把 `strip_line_suffix` 的调用从 `parse` 的循环里删掉。
    #[test]
    fn a_compiler_style_line_and_column_suffix_is_peeled_off() {
        assert_eq!(
            raw("crates/mullion-app/src/app.rs:3525:9"),
            "crates/mullion-app/src/app.rs"
        );
        assert_eq!(
            raw("crates/mullion-app/src/app.rs:3525"),
            "crates/mullion-app/src/app.rs"
        );
    }

    /// 剥到两级为止 —— 第三段数字不是行列号(那更像是路径本身的一部分)。
    ///
    /// 自证会变红:把 `MAX_LINE_SUFFIXES` 改成 3。
    #[test]
    fn only_two_numeric_suffixes_are_treated_as_line_and_column() {
        assert_eq!(raw("/var/log/2024:1:2:3"), "/var/log/2024:1");
    }

    /// 引号 / 反引号裹着的路径(Markdown、含空格时 shell 的输出)。
    ///
    /// 自证会变红:把 `strip_quotes` 的调用删掉。
    #[test]
    fn a_quoted_path_loses_its_quotes() {
        assert_eq!(raw("\"docs/gui render.md\""), "docs/gui render.md");
        assert_eq!(raw("`docs/adr-001.md`"), "docs/adr-001.md");
        assert_eq!(raw("'/var/log/nginx'"), "/var/log/nginx");
    }

    /// 成对的包裹标点两边都要剥 —— 只剥一边的话 `(crates/foo` 照样解析不出
    /// 东西。
    ///
    /// 自证会变红:把 `trim_end_matches(TRAILING_PUNCT)` 删掉(第一条红);
    /// 或把 `trim_start_matches(LEADING_PUNCT)` 删掉(第二条红)。
    #[test]
    fn wrapping_punctuation_is_peeled_off_on_both_sides() {
        assert_eq!(raw("/etc/hosts。"), "/etc/hosts");
        assert_eq!(raw("(crates/mullion-core)"), "crates/mullion-core");
    }

    /// **`.` 不在剥除表里**:剥了的话 `..` 会被吃成空串,而「划 `..` 跳上级」
    /// 是合理用法。
    ///
    /// 自证会变红:把 `'.'` 加进 `TRAILING_PUNCT`。
    #[test]
    fn a_dot_is_never_peeled_so_dotdot_survives() {
        assert_eq!(raw("../sibling"), "../sibling");
        assert_eq!(raw("docs/x."), "docs/x.");
    }

    /// 多行选区一概不受理 —— 里面有 N 条路径,挑哪条都是猜。
    ///
    /// 自证会变红:把 `parse` 开头那两行换行判断删掉。
    #[test]
    fn a_multi_line_selection_is_never_a_path() {
        assert!(parse("crates/a.rs\ncrates/b.rs").is_none());
        assert!(parse("crates/a.rs\r\n").is_none());
    }

    /// 没有分隔符的裸词不算路径 —— 否则划一个普通单词按键就会弹「不存在」,
    /// 而用户想要的只是开关侧栏。
    ///
    /// 自证会变红:让 `looks_like_path` 恒返回 `true`。
    #[test]
    fn a_bare_word_without_a_separator_is_not_a_path() {
        assert!(parse("cargo").is_none());
        assert!(parse("app.rs").is_none());
        assert!(parse("~").is_some());
        assert!(parse("~/Mullion").is_some());
    }

    /// 盘符与 UNC → 本地栏;其余(含一切相对路径)→ 远端栏。
    ///
    /// 自证会变红:把 `column_of` 里盘符那条判断删掉(第一条断言红);
    /// 或把反斜杠相对路径也判给本地(最后一条红)。
    #[test]
    fn only_a_drive_letter_or_unc_goes_to_the_local_column() {
        let col = |s: &str| parse(s).expect("认得出").column;
        assert_eq!(col("C:\\Users\\me\\x.txt"), PanelColumn::Local);
        assert_eq!(col("D:/work/x"), PanelColumn::Local);
        assert_eq!(col("\\\\nas\\share\\x"), PanelColumn::Local);
        assert_eq!(col("/home/ubuntu/x"), PanelColumn::Remote);
        assert_eq!(col("crates/mullion-app"), PanelColumn::Remote);
        assert_eq!(col("docs\\foo.md"), PanelColumn::Remote);
    }

    /// 目标是目录:当前目录就是它 = 已到达。
    #[test]
    fn being_in_the_target_directory_counts_as_arrived() {
        let w = Where {
            target: b"/a/b",
            parent: b"/a",
            base: b"b",
        };
        assert!(arrived(b"/a/b", None, &w));
    }

    /// 目标是文件:光停在父目录**不够**,选中的还得是它。
    ///
    /// 自证会变红:把 `arrived` 里 `&& only_selected == Some(w.base)` 删掉
    /// —— 同目录下换选另一个文件会变成「关栏」而不是「换选」。
    #[test]
    fn standing_in_the_parent_is_not_enough_the_file_must_be_the_selected_one() {
        let w = Where {
            target: b"/a/b/app.rs",
            parent: b"/a/b",
            base: b"app.rs",
        };
        assert!(arrived(b"/a/b", Some(b"app.rs"), &w));
        assert!(!arrived(b"/a/b", Some(b"pane.rs"), &w));
        assert!(!arrived(b"/a/b", None, &w));
    }

    /// 判定表(问题 2 那张)逐格钉住。
    ///
    /// 自证会变红:把 `(true, Some(true))` 那格改成 `Plan::Reveal`
    /// —— 「已到达再按一次」就永远关不掉栏了。
    #[test]
    fn the_key_opens_reveals_and_closes_exactly_as_specified() {
        assert_eq!(plan(false, None), Plan::Open);
        assert_eq!(plan(false, Some(false)), Plan::OpenAndReveal);
        assert_eq!(plan(false, Some(true)), Plan::OpenAndReveal);
        assert_eq!(plan(true, None), Plan::Close);
        assert_eq!(plan(true, Some(false)), Plan::Reveal);
        assert_eq!(plan(true, Some(true)), Plan::Close);
    }

    /// 异机意图的节点校验。
    ///
    /// 自证会变红:让 `consume` 恒返回 `true`。
    #[test]
    fn an_intent_is_only_consumed_on_the_host_it_was_made_for() {
        assert!(consume(Some(1), Some(1)));
        assert!(!consume(Some(1), Some(2)));
        assert!(!consume(Some(0), None));
        assert!(consume(None, None));
    }

    fn sp(from: Option<&str>, to: Option<&str>, path: &str) -> StatusPath {
        StatusPath {
            from: from.map(str::to_string),
            to: to.map(str::to_string),
            path: path.to_string(),
        }
    }

    /// F242:多行选区**根本不去取那个串**。这是 T3 那类每帧成本的闸门 ——
    /// 判据在拼串之前还是之后,画面上完全一样,只有拖着几百行时风扇会响。
    ///
    /// 自证会变红:把 `parse_single_line` 里的 `if !single_line` 那三行删掉
    /// (闭包会 panic)。
    #[test]
    fn a_multi_line_selection_is_never_even_turned_into_a_string() {
        let got = parse_single_line(false, || panic!("多行选区不该被拼成串"));
        assert_eq!(got, None);
    }

    /// 反面:单行时该照常走 `parse`,否则这个闸门就把功能一起关掉了。
    #[test]
    fn a_single_line_selection_still_goes_through_parse() {
        let got = parse_single_line(true, || Some("src/app.rs".to_string()));
        assert_eq!(got.map(|t| t.raw), Some("src/app.rs".to_string()));
    }

    /// F242:同机不写箭头,异机才写。
    ///
    /// 自证会变红:把 `crossing_from` 的两个分支对调。
    #[test]
    fn only_a_real_machine_change_gets_an_arrow() {
        assert_eq!(crossing_from(Some(0), Some(0)), None, "同机没有起点");
        assert_eq!(crossing_from(None, None), None, "两边都不知道也没有起点");
        assert_eq!(crossing_from(Some(0), Some(1)), Some(0), "异机写起点");
        assert_eq!(
            crossing_from(None, Some(1)),
            None,
            "起点不知道时不写 `? → b`"
        );
    }

    /// F242:三种前缀各自的样子。**逐格钉**——这一格是「按下去会发生什么」的
    /// 唯一预告,写错一格就是预告错了。
    ///
    /// 自证会变红:把 `(Some(f), Some(t))` 那条臂删掉(第一条红);把
    /// `(_, None)` 的 `"?"` 改成空串(第三条红)。
    #[test]
    fn the_cell_says_which_machine_the_key_would_take_you_to() {
        let (cell, _) = status_cell(&sp(Some("node-a"), Some("node-b"), "/x/y.rs"), 64);
        assert_eq!(cell, "· node-a → node-b · /x/y.rs");

        let (cell, _) = status_cell(&sp(None, Some("node-b"), "/x/y.rs"), 64);
        assert_eq!(cell, "· node-b · /x/y.rs");

        let (cell, _) = status_cell(&sp(None, None, "crates/y.rs"), 64);
        assert_eq!(cell, "· ? · crates/y.rs", "落不到具体位置时写 `?`");

        let (cell, _) = status_cell(&sp(None, Some("本机"), r"C:\a\b.txt"), 64);
        assert_eq!(cell, r"· 本机 · C:\a\b.txt");
    }

    /// F242:格子里省略了什么,悬停就得能找回什么 —— 两串必须同源。
    ///
    /// 自证会变红:把 `status_cell` 里 tooltip 那一行的 `v.path` 换成
    /// `elide_middle(&v.path, budget)`。
    #[test]
    fn hovering_gives_back_exactly_what_the_ellipsis_ate() {
        let long = "/data/Mullion/crates/mullion-app/src/shell/workspace/preset.rs";
        let v = sp(None, Some("node-b"), long);
        let (cell, full) = status_cell(&v, 32);
        assert!(cell.contains('…'), "32 个字位装不下这条,应该省略");
        assert!(!full.contains('…'), "悬停给的是全文");
        assert!(full.ends_with(long));
    }

    /// F242:省略保**两头**。掐尾(egui `Label::truncate` 那种)会把文件名切
    /// 掉,而文件名正是用户要确认的那半截 —— 这一格的用途就没了。
    ///
    /// 自证会变红:把 `elide_middle` 的 `out.extend(&chars[chars.len()-tail..])`
    /// 删掉(尾巴没了);或把 `let tail = keep * 2 / 3` 改成 `keep / 3`
    /// (`preset.rs` 这个末段就装不下了)。
    #[test]
    fn eliding_keeps_both_the_root_and_the_file_name() {
        let long = "/data/Mullion/crates/mullion-app/src/shell/workspace/preset.rs";
        let got = elide_middle(long, 32);
        assert_eq!(got.chars().count(), 32, "省略之后正好用满预算");
        assert!(got.starts_with('/'), "根还在");
        assert!(got.ends_with("preset.rs"), "文件名还在");
        assert!(got.contains('…'));
    }

    /// 装得下就一个字都不动 —— 省略号本身也是要占位的。
    #[test]
    fn a_path_that_fits_is_left_alone() {
        assert_eq!(elide_middle("/x/y.rs", 32), "/x/y.rs");
        // 正好等于预算:仍然不省略。
        let exact = "a".repeat(32);
        assert_eq!(elide_middle(&exact, 32), exact);
    }

    /// 预算低于下限时不省略成一堆点 —— 让它溢出比让它变成 `/…s` 有用。
    ///
    /// 自证会变红:把 `elide_middle` 里的 `budget.max(MIN_BUDGET)` 改成
    /// `budget`。
    #[test]
    fn a_tiny_budget_is_lifted_to_the_floor_instead_of_shredding_the_path() {
        let s = "/data/Mullion/src/app.rs";
        assert_eq!(elide_middle(s, 0).chars().count(), MIN_BUDGET);
        assert_eq!(elide_middle(s, 3), elide_middle(s, MIN_BUDGET));
    }

    /// F242:预算跟着可用宽度走,并且有上下界 —— 没有下界的话窄窗口下这一格
    /// 会缩成一个省略号,没有上界的话超宽屏上它会把右侧四格挤出可视区。
    ///
    /// 自证会变红:把 `char_budget` 的 `clamp(MIN_BUDGET, MAX_BUDGET)` 去掉。
    #[test]
    fn the_budget_follows_the_width_but_stays_inside_its_bounds() {
        assert_eq!(char_budget(0.0), MIN_BUDGET);
        assert_eq!(char_budget(-1000.0), MIN_BUDGET, "宽度算成负数也不崩");
        assert_eq!(char_budget(100_000.0), 120, "超宽屏封顶");
        let mid = char_budget(1000.0);
        assert!(
            mid > MIN_BUDGET && mid < 120,
            "中间那档确实跟着宽度走:{mid}"
        );
        assert!(char_budget(2000.0) > mid, "更宽就更长");
    }
}
