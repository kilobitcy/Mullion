//! F215:内置编辑器的语法高亮。
//!
//! 三条约束决定了这个模块长什么样:
//!
//! **① egui 的 `layouter` 每帧都跑。** `TextEdit` 不缓存排版结果的构造过程,
//! 只缓存 galley 本身。所以「每帧重新高亮一遍全文」是默认后果 —— 一个
//! 三千行的文件配上 60fps,就是 T3/N3 那条红线(每秒几千次重算,风扇起飞)。
//! 这里挡两层:文本没变直接还上一帧的 `Arc<Galley>`(哈希 + 一次 Arc clone),
//! 文本变了只重算受影响的那几行。
//!
//! **② 按行增量的判据是「进入这一行时的解析状态」,不是「这一行变没变」。**
//! 只看行内容的话,在第 3 行敲一个 `"` 会让下面整片变成字符串,而缓存里
//! 那些行的内容一个字都没动 —— 颜色于是**静默**停在旧的上面。所以缓存命中
//! 要同时满足「行内容没变」**且**「进入状态与上一行算出来的一致」;状态一变,
//! 往下重算,直到某一行的进入状态又对上了为止(通常一两行就收敛)。
//!
//! **③ 行的身份是「内容 + 位置」,而回车会把整片位置挪掉。** 纯按下标比对
//! 的话,在文件开头按一次回车 = 全文重算。所以先求公共前缀/后缀,后缀那段
//! 按位移查旧表 —— 这一下把「按回车」从 O(全文) 变回 O(改动附近)。
//!
//! 主题不从 `.tmTheme` 读,是拿 `theme.rs` 的色板现拼的(见 `syntect_theme`):
//! 另配一套颜色的话,编辑器里的绿和文件面板里的绿会是两个绿,而且没人会记得
//! 同步改。

use crate::theme::Theme;
use mullion_term::snapshot::Rgb;
use std::sync::{Arc, OnceLock};
use syntect::highlighting::{
    Color, FontStyle, Highlighter, ScopeSelectors, StyleModifier, Theme as SynTheme, ThemeItem,
    ThemeSettings,
};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

/// 超过这个大小就不高亮,正文照旧能看能改。
///
/// 判据不是「多大算大文件」,是**一次全量高亮要多久**:syntect 在这台机器上
/// 大约每秒几百 KB,256 KiB 已经是一次卡顿的量级;再往上,一次粘贴就能让
/// 窗口停半秒以上。远端配置/代码文件几乎全在这条线以下,超过它的多半是日志
/// 和数据导出 —— 那些本来也没有语法可言。
pub const MAX_BYTES: usize = 256 * 1024;

/// 语法集。**懒加载**:反序列化那个 368 KiB 的 packdump 要几十毫秒,而绝大
/// 多数会话从头到尾没打开过编辑器,不该让每次启动都掏这笔钱。
///
/// 用 `nonewlines` 那一份:我们按行喂,行尾不带 `\n`。喂错的那一份不会报错,
/// 只是行尾的锚点(`$`)匹配不上,多行结构(块注释、跨行字符串)会**静默**
/// 在行尾断掉。
fn syntax_set() -> &'static SyntaxSet {
    static SS: OnceLock<SyntaxSet> = OnceLock::new();
    SS.get_or_init(SyntaxSet::load_defaults_nonewlines)
}

/// 扩展名 → 一个「够近」的语法名。
///
/// syntect 自带的语法表是 Sublime 的默认包,里面**没有** ini/conf/dockerfile
/// 这些我们天天在远端编辑的东西。与其让它们全掉进 Plain Text(一整片同色,
/// 用户看不出这个功能开着没有),不如映射到形状最接近的那一个:
/// `key = value` 一族全归 Java Properties,`#!` 一族全归 bash。
///
/// **只做形状近似,不猜语义**:`.ts` 归 JavaScript 是因为两者的字符串/注释/
/// 关键字画出来几乎一样;而把 `.log` 归到什么语言都只会画出一堆假高亮。
const NEAR_ENOUGH: &[(&str, &str)] = &[
    // key = value / key value:配置文件的绝大多数
    ("conf", "Java Properties"),
    ("cfg", "Java Properties"),
    ("ini", "Java Properties"),
    ("env", "Java Properties"),
    ("service", "Java Properties"),
    ("repo", "Java Properties"),
    ("desktop", "Java Properties"),
    // `#` 注释 + 井号引导的一族
    ("bashrc", "Bourne Again Shell (bash)"),
    ("profile", "Bourne Again Shell (bash)"),
    ("zsh", "Bourne Again Shell (bash)"),
    ("fish", "Bourne Again Shell (bash)"),
    // TypeScript 没进默认包,而它与 JavaScript 的注释/字符串/关键字基本重合
    ("ts", "JavaScript"),
    ("tsx", "JavaScript"),
    ("jsx", "JavaScript"),
    ("mjs", "JavaScript"),
    ("cjs", "JavaScript"),
];

/// 整个文件名(不是扩展名)对应的语法。Linux 上一大批配置文件压根没有后缀。
const BY_FILENAME: &[(&str, &str)] = &[
    ("sshd_config", "Java Properties"),
    ("ssh_config", "Java Properties"),
    ("nginx.conf", "Java Properties"),
    ("dockerfile", "Bourne Again Shell (bash)"),
    ("makefile", "Makefile"),
    ("gnumakefile", "Makefile"),
    ("fstab", "Java Properties"),
    ("hosts", "Java Properties"),
    ("crontab", "Bourne Again Shell (bash)"),
    ("bashrc", "Bourne Again Shell (bash)"),
    ("bash_profile", "Bourne Again Shell (bash)"),
    ("zshrc", "Bourne Again Shell (bash)"),
    ("profile", "Bourne Again Shell (bash)"),
    ("gitconfig", "Java Properties"),
];

/// 一条远端路径该用哪套语法。纯函数,单测得到。
///
/// 顺序是「整名 → 扩展名 → 近似映射 → Plain Text」。整名排最前是因为
/// `nginx.conf` 的扩展名是 `conf`,两条都命中,而整名那条更准。
pub fn syntax_for<'a>(path: &str, ss: &'a SyntaxSet) -> &'a SyntaxReference {
    let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let lower = file.to_ascii_lowercase();

    if let Some((_, name)) = BY_FILENAME.iter().find(|(k, _)| *k == lower) {
        if let Some(s) = ss.find_syntax_by_name(name) {
            return s;
        }
    }
    // 前导点的隐藏文件(`.bashrc`)按去掉点之后的整名再试一次。
    if let Some(stripped) = lower.strip_prefix('.') {
        if let Some((_, name)) = BY_FILENAME.iter().find(|(k, _)| *k == stripped) {
            if let Some(s) = ss.find_syntax_by_name(name) {
                return s;
            }
        }
    }
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or(&lower);
    if let Some(s) = ss.find_syntax_by_extension(ext) {
        return s;
    }
    if let Some((_, name)) = NEAR_ENOUGH.iter().find(|(k, _)| *k == ext) {
        if let Some(s) = ss.find_syntax_by_name(name) {
            return s;
        }
    }
    ss.find_syntax_plain_text()
}

fn syn(c: Rgb) -> Color {
    Color {
        r: c.r,
        g: c.g,
        b: c.b,
        a: 0xff,
    }
}

fn item(scope: &str, fg: Rgb) -> ThemeItem {
    ThemeItem {
        // 选择器串是编译期写死的常量,解析不了说明这一行本身写错了。
        scope: scope.parse::<ScopeSelectors>().expect("选择器写错了"),
        style: StyleModifier {
            foreground: Some(syn(fg)),
            background: None,
            font_style: Some(FontStyle::empty()),
        },
    }
}

/// 拿 `theme.rs` 的色板现拼一套 syntect 主题。
///
/// 六档,全部落在既有语义色上 —— 编辑器里的绿必须就是文件面板里的那个绿。
/// 每一档在 `term_bg` 上都过 4.5:1(`the_highlight_palette_is_readable_on_the_editor_background`
/// 盯着);注释用最暗的 `fg_dimmer`(6.53:1)是刻意的:它该退到后面去,
/// 但退到读不清就成了另一个 bug。
pub fn syntect_theme(t: &Theme) -> SynTheme {
    SynTheme {
        name: Some("Mullion".into()),
        author: None,
        settings: ThemeSettings {
            foreground: Some(syn(t.term_fg)),
            background: Some(syn(t.term_bg)),
            ..Default::default()
        },
        scopes: vec![
            item("comment", t.fg_dimmer),
            item("string, constant.character", t.ok),
            item("constant.numeric, constant.language", t.warn),
            item("keyword, storage.modifier", t.accent),
            item("entity.name.function, support.function", t.info),
            item("storage.type, support.type, entity.name", t.fg_muted),
        ],
    }
}

/// 一行的高亮结果。
#[derive(Clone)]
struct LineHl {
    /// 行内容的指纹。
    hash: u64,
    /// **进入**这一行之前的解析状态。缓存命中的第二个条件(见模块头 ②)。
    enter: (ParseState, ScopeStack),
    /// 着色段:`(这一段有多少字节, 前景色)`,顺序拼起来正好是整行。
    spans: Vec<(usize, Rgb)>,
}

fn hash_of(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// 新旧两份行指纹的公共前缀 / 公共后缀长度(不重叠)。
///
/// 回车/删行会把后面整片行的下标挪掉,只按下标比对的话它们全部落空。
/// 拿位移查旧表,这一类改动就退回「只重算改动附近」。
fn prefix_suffix(old: &[u64], new: &[u64]) -> (usize, usize) {
    let n = old.len().min(new.len());
    let mut p = 0;
    while p < n && old[p] == new[p] {
        p += 1;
    }
    let mut s = 0;
    while s < n - p && old[old.len() - 1 - s] == new[new.len() - 1 - s] {
        s += 1;
    }
    (p, s)
}

/// 新表下标 → 旧表下标。前缀原位,后缀按位移,中间那段没有对应关系。
fn old_index(
    i: usize,
    old_len: usize,
    new_len: usize,
    prefix: usize,
    suffix: usize,
) -> Option<usize> {
    if i < prefix {
        Some(i)
    } else if i + suffix >= new_len {
        // i - (new_len - suffix) 是它在后缀里的序号
        Some(old_len - (new_len - i))
    } else {
        None
    }
}

/// 一个打开着的文件的高亮缓存。挂在 `EditorState` 上,关窗即没。
pub struct Cache {
    syntax: &'static SyntaxReference,
    theme: SynTheme,
    lines: Vec<LineHl>,
    /// 上一帧交出去的排版结果,连同它是按什么算出来的。
    galley: Option<(u64, u32, u32, Arc<egui::Galley>)>,
    /// 上一次 `update` 真正重算了几行。守护测试的判据 —— 「增量生效了没有」
    /// 在画面上完全看不出来(颜色一模一样),只有这个数说得清。
    pub recomputed: usize,
    /// 这个文件超了 `MAX_BYTES`,只排版不上色。
    pub too_big: bool,
}

impl Cache {
    pub fn new(path: &str, t: &Theme, len: usize) -> Self {
        let ss = syntax_set();
        Self {
            syntax: syntax_for(path, ss),
            theme: syntect_theme(t),
            lines: Vec::new(),
            galley: None,
            recomputed: 0,
            too_big: len > MAX_BYTES,
        }
    }

    /// 这个文件认出来的语法叫什么(窗口里报给用户看)。
    pub fn syntax_name(&self) -> &str {
        &self.syntax.name
    }

    /// 重算受影响的行,返回全文的着色段。
    fn update(&mut self, text: &str) {
        let ss = syntax_set();
        let hl = Highlighter::new(&self.theme);
        let lines: Vec<&str> = text.split('\n').collect();
        let new_hashes: Vec<u64> = lines.iter().map(|l| hash_of(l)).collect();
        let old_hashes: Vec<u64> = self.lines.iter().map(|l| l.hash).collect();
        let (prefix, suffix) = prefix_suffix(&old_hashes, &new_hashes);

        let mut state = (ParseState::new(self.syntax), ScopeStack::new());
        let mut out: Vec<LineHl> = Vec::with_capacity(lines.len());
        self.recomputed = 0;

        for (i, line) in lines.iter().enumerate() {
            let oi = old_index(i, self.lines.len(), lines.len(), prefix, suffix);
            // 命中要三样都对上:行内容、进入状态、以及**下一行存在**
            // (离开状态就是下一行的进入状态,没有下一行就没得抄)。
            let hit = oi.and_then(|oi| {
                let cur = self.lines.get(oi)?;
                let next = self.lines.get(oi + 1)?;
                (cur.hash == new_hashes[i] && cur.enter == state)
                    .then(|| (cur.clone(), next.enter.clone()))
            });
            if let Some((cur, leave)) = hit {
                out.push(cur);
                state = leave;
                continue;
            }

            let enter = state.clone();
            let mut ps = enter.0.clone();
            let mut stack = enter.1.clone();
            let mut spans = Vec::new();
            match ps.parse_line(line, ss) {
                Ok(ops) => {
                    let mut pos = 0usize;
                    for (at, op) in &ops {
                        if *at > pos {
                            spans.push((at - pos, rgb_of(&hl, &stack)));
                            pos = *at;
                        }
                        // 语法表自相矛盾时(几乎只在自制语法里)就地停手,
                        // 这一行按当前栈上完色 —— 整窗白屏比少一段颜色糟得多。
                        if stack.apply(op).is_err() {
                            break;
                        }
                    }
                    if pos < line.len() {
                        spans.push((line.len() - pos, rgb_of(&hl, &stack)));
                    }
                }
                Err(_) => spans.push((line.len(), rgb_of(&hl, &stack))),
            }
            state = (ps, stack);
            self.recomputed += 1;
            out.push(LineHl {
                hash: new_hashes[i],
                enter,
                spans,
            });
        }
        self.lines = out;
    }

    /// egui 的 `layouter` 落到这里。返回这一帧要画的 galley。
    pub fn layout(&mut self, ui: &egui::Ui, text: &str, wrap_width: f32) -> Arc<egui::Galley> {
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let key = (hash_of(text), wrap_width.to_bits(), font.size.to_bits());
        // 文本/折行宽度/字号都没变 —— 连 `LayoutJob` 都不必再拼一遍。
        // 这一条才是「打字不掉帧」的主力:不加的话,每帧都要为几千行拼
        // 一份带几万个 section 的 job(T3/N3)。
        if let Some((h, w, f, g)) = &self.galley {
            if (*h, *w, *f) == key {
                return g.clone();
            }
        }

        let mut job = egui::text::LayoutJob {
            wrap: egui::text::TextWrapping {
                max_width: wrap_width,
                ..Default::default()
            },
            break_on_newline: true,
            ..Default::default()
        };
        if self.too_big {
            job.append(
                text,
                0.0,
                egui::TextFormat {
                    font_id: font,
                    color: crate::theme::c32(self.default_fg()),
                    ..Default::default()
                },
            );
        } else {
            self.update(text);
            let plain = crate::theme::c32(self.default_fg());
            let mut at = 0usize;
            for (li, line) in text.split('\n').enumerate() {
                let end_of_line = at + line.len();
                let mut pos = at;
                for (len, rgb) in &self.lines[li].spans {
                    let end = (pos + len).min(end_of_line);
                    job.append(
                        &text[pos..end],
                        0.0,
                        egui::TextFormat {
                            font_id: font.clone(),
                            color: crate::theme::c32(*rgb),
                            ..Default::default()
                        },
                    );
                    pos = end;
                }
                // 着色段拼不满整行时把尾巴补上 —— 宁可少一段颜色,
                // 也不能让排版出来的文本比原文短(光标会整体错位)。
                if pos < end_of_line {
                    job.append(
                        &text[pos..end_of_line],
                        0.0,
                        egui::TextFormat {
                            font_id: font.clone(),
                            color: plain,
                            ..Default::default()
                        },
                    );
                }
                at = end_of_line;
                if at < text.len() {
                    // 换行符自己也要进 job,理由同上。
                    job.append(
                        "\n",
                        0.0,
                        egui::TextFormat {
                            font_id: font.clone(),
                            color: plain,
                            ..Default::default()
                        },
                    );
                    at += 1;
                }
            }
        }
        let galley = ui.fonts(|f| f.layout_job(job));
        self.galley = Some((key.0, key.1, key.2, galley.clone()));
        galley
    }

    fn default_fg(&self) -> Rgb {
        let c = self.theme.settings.foreground.unwrap_or(Color::WHITE);
        Rgb::new(c.r, c.g, c.b)
    }
}

fn rgb_of(hl: &Highlighter<'_>, stack: &ScopeStack) -> Rgb {
    let c = hl.style_for_stack(&stack.scopes).foreground;
    Rgb::new(c.r, c.g, c.b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{contrast_ratio, MULLION_DARK};

    /// 六档颜色在编辑器正文底色上都得读得清。
    ///
    /// 高亮的整个意义是「一眼分得开」,而分得开的前提是每一档本身先看得见。
    /// 注释那一档最容易踩线 —— 它天生就该最暗。
    #[test]
    fn the_highlight_palette_is_readable_on_the_editor_background() {
        let t = MULLION_DARK;
        let th = syntect_theme(&t);
        let bg = t.term_bg;
        assert_eq!(th.scopes.len(), 6, "档数变了,这条测试要跟着改");
        for it in &th.scopes {
            let c = it.style.foreground.expect("每一档都得给前景色");
            let fg = Rgb::new(c.r, c.g, c.b);
            let r = contrast_ratio(fg, bg);
            assert!(
                r >= 4.5,
                "{:?} 在 term_bg 上只有 {r:.2}:1 —— 读不清的高亮不如不高亮",
                it.scope
            );
        }
    }

    /// 认语法:整名优先于扩展名,近似映射兜住默认包里没有的那些。
    ///
    /// 自证会变红:把 `BY_FILENAME` 那一轮查找删掉(`nginx.conf` 会掉回
    /// 扩展名那条路);或删掉 `NEAR_ENOUGH`(`.ts` 会掉进 Plain Text)。
    #[test]
    fn a_remote_path_picks_a_syntax_that_is_at_least_shaped_like_the_file() {
        let ss = syntax_set();
        let name = |p: &str| syntax_for(p, ss).name.clone();

        assert_eq!(name("/home/u/x.rs"), "Rust");
        assert_eq!(name("/etc/nginx/nginx.conf"), "Java Properties");
        assert_eq!(name("/etc/ssh/sshd_config"), "Java Properties");
        assert_eq!(name("/home/u/.bashrc"), "Bourne Again Shell (bash)");
        assert_eq!(name("/srv/app/main.ts"), "JavaScript");
        assert_eq!(name("/srv/app/Makefile"), "Makefile");
        // 认不出来的一律 Plain Text —— 而不是随便挑一个画出满屏假颜色。
        assert_eq!(name("/var/log/syslog.20260901"), "Plain Text");
    }

    fn cache(path: &str) -> Cache {
        Cache::new(path, &MULLION_DARK, 0)
    }

    /// 改一行只重算那一行(和收敛用的下一行)。
    ///
    /// 这是本切片「按行增量」的**唯一**判据:全量重算画出来的颜色跟增量
    /// 一模一样,画面上永远看不出差别,只有这个计数说得清。三千行的文件
    /// 每敲一个字符全量重算一次,就是 T3/N3 那条红线。
    ///
    /// 自证会变红:把 `hit` 那一段删掉(恒不命中)。
    #[test]
    fn typing_one_character_does_not_re_highlight_the_whole_file() {
        let mut c = cache("a.rs");
        let mut text = (0..500)
            .map(|i| format!("let x{i} = {i};"))
            .collect::<Vec<_>>()
            .join("\n");
        c.update(&text);
        assert_eq!(c.recomputed, 500, "首次必须全量");

        text = text.replace("let x250 = 250;", "let x250 = 251;");
        c.update(&text);
        assert!(
            c.recomputed <= 2,
            "改一行却重算了 {} 行 —— 增量没生效",
            c.recomputed
        );
    }

    /// 在开头按一次回车,底下几百行不该整片重算。
    ///
    /// 纯按下标比对的话它们的下标全挪了一位、一条都命中不了 —— 而这恰恰是
    /// 编辑文件时最常做的动作。
    ///
    /// 自证会变红:让 `prefix_suffix` 恒返回 `(0, 0)`。
    #[test]
    fn pressing_enter_at_the_top_does_not_shift_every_line_out_of_the_cache() {
        let mut c = cache("a.rs");
        let text = (0..300)
            .map(|i| format!("let x{i} = {i};"))
            .collect::<Vec<_>>()
            .join("\n");
        c.update(&text);
        let shifted = format!("\n{text}");
        c.update(&shifted);
        assert!(
            c.recomputed <= 3,
            "在开头插一行就重算了 {} 行 —— 后缀复用没生效",
            c.recomputed
        );
    }

    /// 缓存命中不能只看行内容:上游状态变了,下面那些**一个字都没动**的行
    /// 也得跟着改色。
    ///
    /// 在第一行补一个未闭合的 `/*`,后面整片就是注释。只比行内容的实现在
    /// 这里会**静默**留住旧颜色 —— 编译/测试/日志全不报,只有人眼看得见。
    ///
    /// 自证会变红:把命中条件里的 `&& cur.enter == state` 去掉。
    #[test]
    fn a_change_upstream_recolours_lines_whose_own_text_never_moved() {
        let mut c = cache("a.rs");
        let body = "let a = 1;\nlet b = 2;\nlet c = 3;";
        c.update(&format!("// x\n{body}"));
        let plain = c.lines[2].spans.clone();

        c.update(&format!("/* x\n{body}"));
        let commented = c.lines[2].spans.clone();
        assert_ne!(
            plain, commented,
            "第一行改成块注释之后,底下那些行还是旧颜色 —— 缓存只比了行内容"
        );
        let comment = MULLION_DARK.fg_dimmer;
        assert!(
            commented.iter().all(|(_, c)| *c == comment),
            "块注释里的行该整行是注释色:{commented:?}"
        );
    }

    /// 大文件只排版不上色 —— 而且**得真的不上色**,不是「上了色但很慢」。
    #[test]
    fn a_file_past_the_threshold_is_laid_out_but_not_highlighted() {
        let small = Cache::new("a.rs", &MULLION_DARK, MAX_BYTES);
        assert!(!small.too_big, "正好卡在线上的不算大");
        let big = Cache::new("a.rs", &MULLION_DARK, MAX_BYTES + 1);
        assert!(big.too_big);
    }

    /// `prefix_suffix` / `old_index` 是下标映射的唯一出口,拿具体数字锁死。
    #[test]
    fn the_index_map_reuses_the_tail_after_an_insertion() {
        // 旧:[a b c d],新:[a x b c d](在下标 1 插了一行)
        let old = [1u64, 2, 3, 4];
        let new = [1u64, 9, 2, 3, 4];
        assert_eq!(prefix_suffix(&old, &new), (1, 3));
        let m = |i| old_index(i, old.len(), new.len(), 1, 3);
        assert_eq!(m(0), Some(0)); // 前缀原位
        assert_eq!(m(1), None); // 新插进来的那一行没有对应
        assert_eq!(m(2), Some(1)); // 后缀按位移
        assert_eq!(m(3), Some(2));
        assert_eq!(m(4), Some(3));
    }

    /// 排版出来的文本必须与原文**一字不差**。
    ///
    /// 少一个换行符,`TextEdit` 的光标定位就整体错位:用户点在第 10 行,
    /// 光标落在第 9 行 —— 而颜色看上去完全正常。
    #[test]
    fn the_laid_out_text_is_byte_for_byte_the_original() {
        let ctx = egui::Context::default();
        let src = "fn main() {\n    let s = \"hi\";\n    // 尾注释\n}\n";
        let mut c = cache("a.rs");
        let mut got = String::new();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                got = c.layout(ui, src, f32::INFINITY).text().to_string();
            });
        });
        assert_eq!(got, src, "排版出来的文本和原文对不上 —— 光标会错位");
    }

    /// 文本没变的那些帧,不许重新拼 `LayoutJob`。
    ///
    /// `layouter` 每帧都跑,不挡这一层的话,一个三千行的文件在 60fps 下
    /// 每秒要拼几千份带几万个 section 的 job(T3/N3)。
    ///
    /// 自证会变红:把 `layout` 里那段 galley 缓存比对删掉。
    #[test]
    fn an_unchanged_buffer_costs_nothing_beyond_a_hash_lookup() {
        let ctx = egui::Context::default();
        let src = "let a = 1;\nlet b = 2;\n";
        let mut c = cache("a.rs");
        let mut second = 0usize;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                c.layout(ui, src, 400.0);
                c.recomputed = 999; // 哨兵:再算一次的话会被覆盖掉
                c.layout(ui, src, 400.0);
                second = c.recomputed;
            });
        });
        assert_eq!(second, 999, "文本没变却又重算了一遍");
    }
}
