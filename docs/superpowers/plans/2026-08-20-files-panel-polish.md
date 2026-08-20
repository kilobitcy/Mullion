# 文件面板六项优化（F143~F147）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修掉文件面板五项界面缺陷，并把「字体画不出来的符号」这条复发三次的坑变成机械守护。

**Architecture:** 新增 `ui/glyphs.rs`（白名单纯函数）+ `tests/glyph_whitelist.rs`（用 `proc-macro2` 解析 `src/**/*.rs` 的 token 流，只查字符串/字符字面量）；`ui/icon.rs` 补三个自绘 `Glyph` 顶掉现有 12 处豆腐；`ui/files_panel.rs` 改内边距、书签文案、列布局与列头绘制。

**Tech Stack:** Rust / egui 0.30 / epaint / proc-macro2（新增 dev-dependency，带 `span-locations` feature）

**基线事实（已实测，不要重新推导）：**

- 判据是 **GBK/CP936**，**不是 GB18030**。GB18030 是全 Unicode 编码方案，"能编码"对任何字符恒为真，拿它当判据等于没有判据。
- 生产代码（已剥 `#[cfg(test)]`）里共 **12 处**豆腐，全表在 Task 4。
- `files/local.rs:61-62` 构造 `Entry` 时 `uid`/`gid` 恒填 `0`；`files_panel::owner_text` 对本地栏已经恒返回 `—`，所以本地栏属主列现在是一整列破折号。
- `Shape::convex_polygon` 产出的是 `Shape::Path`，`icon.rs` 测试里的 `points_of` **已经能处理**，不需要改它。

---

### Task 1: 字形白名单（纯函数）

**Files:**
- Create: `crates/mullion-app/src/ui/glyphs.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs:2-28`（模块声明，按字母序插在 `pub mod files_panel;` 之后、`pub mod group_manager;` 之前）

- [ ] **Step 1: 写失败的测试**

在新文件 `crates/mullion-app/src/ui/glyphs.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// 三类放行：ASCII、CJK 汉字、中文标点。
    #[test]
    fn ascii_cjk_and_chinese_punctuation_are_always_allowed() {
        for c in ['a', 'Z', '0', ' ', '/', ':', '\n'] {
            assert!(is_allowed(c), "ASCII {c:?} 被误拦");
        }
        for c in ['中', '文', '目', '录', '龥'] {
            assert!(is_allowed(c), "汉字 {c:?} 被误拦");
        }
        for c in ['，', '。', '、', '「', '」', '（', '）', '：'] {
            assert!(is_allowed(c), "中文标点 {c:?} 被误拦");
        }
    }

    /// 已登记的符号放行，没登记的一律拦下。
    ///
    /// 自证会变红：把 `is_allowed` 最后一行的 `VERIFIED.contains(&c)`
    /// 改成 `true`，第二组断言立刻红。
    #[test]
    fn only_registered_symbols_pass_and_the_known_tofu_does_not() {
        for c in ['—', '…', '·', '→', '↑', '↓', '×', '●', '★', '☆', '▲', '▼'] {
            assert!(is_allowed(c), "已登记的 {c:?} 应当放行");
        }
        // 这六个在 GBK 里没有 —— 微软雅黑与 egui 内置字体两边都画不出来。
        for c in ['▾', '▸', '⟳', '↻', '✕', '⚠'] {
            assert!(!is_allowed(c), "{c:?} 在 GBK 外，必须被拦下");
        }
    }

    /// 白名单里的每一个都必须真在 GBK 内 —— 这条钉的是「登记」这道闸门
    /// 本身：谁往 `VERIFIED` 里塞了一个凭想象的字符，这里会红。
    ///
    /// 判据用 `encoding_rs` 的 GBK 编码器；编不出来（返回替换字符）就是
    /// 字体多半也没有。
    ///
    /// 自证会变红：往 `VERIFIED` 里加一个 `'▾'`。
    #[test]
    fn every_registered_symbol_is_really_inside_gbk() {
        for &c in VERIFIED {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            let (bytes, _, had_errors) = encoding_rs::GBK.encode(s);
            assert!(
                !had_errors && !bytes.is_empty(),
                "U+{:04X} {c:?} 不在 GBK 内，不该出现在白名单里",
                c as u32
            );
        }
    }
}
```

- [ ] **Step 2: 加 `encoding_rs` dev-dependency**

`crates/mullion-app/Cargo.toml` 的 `[dev-dependencies]` 段末尾追加：

```toml
# 字形白名单守护(`ui::glyphs`)要判「这个字符在不在 GBK 里」——GBK 是
# 微软雅黑字形覆盖面的实用近似。**只在 test target 生效**,不进 exe。
encoding_rs = "0.8"
```

- [ ] **Step 3: 跑测试确认它失败**

```bash
cargo test -p mullion-app --lib ui::glyphs 2>&1 | tail -20
```

预期：编译失败，`cannot find function is_allowed`。

- [ ] **Step 4: 写实现**

`crates/mullion-app/src/ui/glyphs.rs` 开头（放在上面那个 `mod tests` 之前）：

```rust
//! UI 文本里允许出现的非 ASCII 符号白名单。
//!
//! **为什么需要这个模块**：egui 的字体链只有两级 —— 内置的
//! Ubuntu-Light / NotoEmoji，加上 [`super::install_cjk_font`] 追加的系统
//! CJK 字体（Windows 上第一候选是微软雅黑）。两级都没有的字形，epaint 画成
//! 豆腐块 `□`。编译不报错、测试不报错、日志不报错，**只有人眼能看见**。
//!
//! 这个坑在本项目复发过三次：走查 P0-5 的 `✕`（当时的修法是 [`super::icon`]
//! 自绘，但只救了那一个按钮）、v0.1.56 用户实测报的路径条 `▾`，以及同一轮
//! 扫描顺带挖出来的另外十处。前两次都只修了当场那一个字符，没有留下任何
//! 机械检查 —— 于是它必然回来。
//!
//! **判据是「该字符在 GBK/CP936 内」**，那是微软雅黑字形覆盖面的实用近似。
//! **不是 GB18030**：GB18030 是全 Unicode 的编码方案，"能编码" 对任何字符
//! 都成立，拿它当判据等于没有判据（这个弯路本项目已经走过一次）。
//!
//! **加新符号的纪律**：先在 Windows 实机上把它画出来看一眼，再往
//! [`VERIFIED`] 里登记。**登记这一步就是闸门** —— 它逼你去看。不想登记的，
//! 走 [`super::icon`] 自绘，那条路不受任何字体覆盖面影响。
//!
//! 守护在 `tests/glyph_whitelist.rs`（扫全部 `src/**/*.rs` 的字符串字面量）。

/// 已实机验过、允许直接写进 UI 字符串的非 ASCII 符号。
///
/// 每一个都在 GBK 内（`tests::every_registered_symbol_is_really_inside_gbk`
/// 钉着这一点，防止有人凭想象往里加）。
pub const VERIFIED: &[char] = &[
    '—', // U+2014 破折号：全项目最常用的「空值/分隔」符号
    '…', // U+2026 省略号：截断标记
    '·', // U+00B7 间隔号：状态栏各段之间
    '→', // U+2192 右箭头：符号链接目标、跳板链
    '↑', // U+2191 上箭头：上传方向、上一级
    '↓', // U+2193 下箭头：下载方向
    '×', // U+00D7 乘号：关闭、尺寸的 80×24
    '●', // U+25CF 实心圆：脏标记 / 状态点
    '≥', // U+2265 大于等于：设置里的数值说明
    '②', // U+2461 带圈数字：错误文案里的步骤编号
    '★', // U+2605 实心五角星：已收藏
    '☆', // U+2606 空心五角星：未收藏
    '▲', // U+25B2 实心上三角：升序标识
    '▼', // U+25BC 实心下三角：降序标识
];

/// 这个字符能不能直接写进 UI 字符串。
///
/// 三类放行：ASCII、CJK 汉字（含扩展 A）、中文标点与全角形式。
/// 其余非 ASCII 一律要在 [`VERIFIED`] 里登记过。
pub fn is_allowed(c: char) -> bool {
    if c.is_ascii() {
        return true;
    }
    let o = c as u32;
    // CJK 统一表意文字 + 扩展 A。
    if (0x4E00..=0x9FFF).contains(&o) || (0x3400..=0x4DBF).contains(&o) {
        return true;
    }
    // CJK 符号与标点（、。「」〈〉…）+ 全角 ASCII 形式（，：（））。
    if (0x3000..=0x303F).contains(&o) || (0xFF00..=0xFF65).contains(&o) {
        return true;
    }
    VERIFIED.contains(&c)
}
```

- [ ] **Step 5: 挂上模块**

`crates/mullion-app/src/ui/mod.rs`，在 `pub mod files_panel;`（第 9 行）之后插入：

```rust
pub mod glyphs;
```

- [ ] **Step 6: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib ui::glyphs 2>&1 | grep -E "test result|FAILED"
```

预期：`test result: ok. 3 passed`

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src/ui/glyphs.rs crates/mullion-app/src/ui/mod.rs crates/mullion-app/Cargo.toml Cargo.lock
git commit -m "feat(app): 字形白名单 —— UI 字符串只许用实机验过的符号 (F143)

判据是 GBK/CP936(微软雅黑覆盖面的近似),不是 GB18030 ——
后者是全 Unicode 编码方案,「能编码」恒为真,当判据等于没判据。"
```

---

### Task 2: 源码扫描守护测试（先红）

**Files:**
- Create: `crates/mullion-app/tests/glyph_whitelist.rs`
- Modify: `crates/mullion-app/Cargo.toml`（`[dev-dependencies]` 加 `proc-macro2`）

- [ ] **Step 1: 加 dev-dependency**

`crates/mullion-app/Cargo.toml` 的 `[dev-dependencies]` 段追加：

```toml
# 字形白名单守护要**只看字符串字面量、不看注释**。正则做不到(注释里
# 随手写个引号就崩),所以走真正的 Rust 词法分析。`span-locations` 是
# 为了报错时能给出行号 —— 一条 U+XXXX 没有行号谁也定位不了。
proc-macro2 = { version = "1", features = ["span-locations"] }
```

- [ ] **Step 2: 写测试**

创建 `crates/mullion-app/tests/glyph_whitelist.rs`：

```rust
//! 守护：UI 字符串里不许出现字体画不出来的符号（F143）。
//!
//! 判据与纪律见 `mullion_app::ui::glyphs` 的模块文档。这里只负责
//! 「把 `src/**/*.rs` 里的字符串字面量捞出来，逐字符过白名单」。
//!
//! **两条必须跳过的东西，少一条这条测试就会假红**：
//!
//! 1. **attribute 里的字符串**。`///` 文档注释在 token 流里就是
//!    `#[doc = "..."]` —— 是货真价实的字符串字面量。不跳过的话，
//!    `ui/icon.rs` 模块头里那句「源码里写的其实是 `✕`」会当场把这条
//!    测试打红，而那行字根本不会画到屏幕上。本项目记过的
//!    「源码扫描类守护会假红：注释里举反例」，这是第三种形态。
//! 2. **`#[cfg(test)]` 模块**。测试数据里有 emoji、有故意造的畸形字符串，
//!    它们同样不上屏。
//!
//! 为什么不用正则：注释里出现一个引号就会让配对错位，而本项目的注释
//! 又长又密。走 `proc-macro2` 的真词法分析是唯一可靠的做法。

use std::path::{Path, PathBuf};

use proc_macro2::{Delimiter, TokenStream, TokenTree};

/// 一条违规：文件、行号、字符、它所在的那个字面量。
#[derive(Debug)]
struct Offence {
    file: PathBuf,
    line: usize,
    ch: char,
    lit: String,
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).expect("读 src 目录失败") {
        let p = e.expect("读目录项失败").path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// 把 token 流里的字符串/字符字面量收集出来（行号 + 原文）。
fn collect_literals(ts: TokenStream, out: &mut Vec<(usize, String)>) {
    let mut it = ts.into_iter().peekable();
    // 上一个 attribute 是不是 `#[cfg(test)]` —— 是的话，随后第一个 `{}`
    // 整块跳过（`#[cfg(test)] mod tests { .. }` / `#[cfg(test)] fn .. { .. }`）。
    let mut skip_next_brace = false;
    while let Some(tt) = it.next() {
        match tt {
            // `#` + `[...]` = attribute。整块不进（见模块头第 1 条）。
            TokenTree::Punct(p) if p.as_char() == '#' => {
                let is_attr = matches!(it.peek(), Some(TokenTree::Group(g))
                    if g.delimiter() == Delimiter::Bracket);
                if is_attr {
                    let Some(TokenTree::Group(g)) = it.next() else {
                        unreachable!("上一行刚 peek 确认过是 Bracket group")
                    };
                    // `to_string()` 的空格排布随 proc-macro2 版本变，去掉再比。
                    if g.stream().to_string().replace(' ', "") == "cfg(test)" {
                        skip_next_brace = true;
                    }
                }
            }
            TokenTree::Group(g) => {
                if g.delimiter() == Delimiter::Brace && skip_next_brace {
                    skip_next_brace = false;
                    continue;
                }
                collect_literals(g.stream(), out);
            }
            TokenTree::Literal(l) => {
                let s = l.to_string();
                // 字符串 `"..."` / 原始串 `r"..."`、`r#"..."#` / 字符 `'x'`。
                // 数字字面量和生命周期不会走到这里（生命周期是 Punct+Ident）。
                if s.starts_with('"') || s.starts_with('r') || s.starts_with('\'') {
                    out.push((l.span().start().line, s));
                }
            }
            _ => {}
        }
    }
}

#[test]
fn no_ui_string_contains_a_glyph_the_font_cannot_draw() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&src, &mut files);
    assert!(files.len() > 20, "只扫到 {} 个文件，路径多半错了", files.len());

    let mut offences: Vec<Offence> = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).expect("读源文件失败");
        let ts: TokenStream = text
            .parse()
            .unwrap_or_else(|e| panic!("{} 解析失败：{e}", f.display()));
        let mut lits = Vec::new();
        collect_literals(ts, &mut lits);
        for (line, lit) in lits {
            for ch in lit.chars() {
                if !mullion_app::ui::glyphs::is_allowed(ch) {
                    offences.push(Offence {
                        file: f.clone(),
                        line,
                        ch,
                        lit: lit.clone(),
                    });
                }
            }
        }
    }

    if !offences.is_empty() {
        let mut msg = format!("{} 处 UI 字符串用了字体画不出来的符号：\n", offences.len());
        for o in &offences {
            let lit = if o.lit.chars().count() > 60 {
                o.lit.chars().take(60).collect::<String>() + "…"
            } else {
                o.lit.clone()
            };
            msg += &format!(
                "  {}:{}  U+{:04X} {:?}  在 {}\n",
                o.file.display(),
                o.line,
                o.ch as u32,
                o.ch,
                lit
            );
        }
        msg += "\n处置二选一：\
                \n  A. 换成 `ui::glyphs::VERIFIED` 里已登记的字符；\
                \n  B. 走 `ui::icon` 自绘（不受字体覆盖面影响）。\
                \n往 VERIFIED 里加新字符**必须先在 Windows 实机上看一眼**。";
        panic!("{msg}");
    }
}
```

- [ ] **Step 3: 跑，把违规集打出来**

```bash
cargo test -p mullion-app --test glyph_whitelist 2>&1 | sed -n '/处 UI 字符串/,/实机/p'
```

预期：FAIL，列出 12 处（`▾`×3、`⚠`×3、`▸`×2、`↻`、`✗`、`⟳`、`•`、`✕` 各 1）。

**若实际数目与这里不符，以实际输出为准** —— 那说明扫描器的跳过逻辑与预期不一致，先查 attribute / `cfg(test)` 两条跳过是否生效，不要直接改白名单去凑绿。

- [ ] **Step 4: 提交（红着提交，Task 4 转绿）**

```bash
git add crates/mullion-app/tests/glyph_whitelist.rs crates/mullion-app/Cargo.toml Cargo.lock
git commit -m "test(app): 字形白名单的源码守护(现红,12 处待修) (F143)

只查字符串字面量:attribute(含 /// 文档注释)与 #[cfg(test)] 模块整块跳过
—— 不跳的话 icon.rs 模块头里举的反例 ✕ 会把这条测试打成假红。"
```

---

### Task 3: `icon.rs` 补三个自绘 Glyph

**Files:**
- Modify: `crates/mullion-app/src/ui/icon.rs:14-24`（enum）、`:33-81`（`shapes`）、`:169`（测试数组）

- [ ] **Step 1: 写失败的测试**

`crates/mullion-app/src/ui/icon.rs` 的 `mod tests` 里，把第 169 行那个手写数组换成遍历 `Glyph::ALL`：

```rust
        for g in Glyph::ALL.iter().copied() {
```

并在 `mod tests` 末尾追加：

```rust
    /// 折叠三角必须朝对方向 —— 画反了不会编译错、不会 panic，只会让
    /// 「展开」看起来像「折起」。同 `arrow_up_points_up_and_arrow_down_points_down`
    /// 的理由。
    ///
    /// 自证会变红：把 `shapes()` 里 `TriangleDown` 和 `TriangleRight`
    /// 两个分支的返回值对调。
    #[test]
    fn the_collapse_triangles_point_down_and_right() {
        let down = points_of(&shapes(r(), Glyph::TriangleDown, s()));
        let right = points_of(&shapes(r(), Glyph::TriangleRight, s()));
        let c = r().center();
        // 尖端 = 离中心最远的那个点（三角只有三个点，尖端唯一）。
        let apex_down = down.iter().copied().max_by(|a, b| a.y.total_cmp(&b.y)).unwrap();
        let apex_right = right.iter().copied().max_by(|a, b| a.x.total_cmp(&b.x)).unwrap();
        assert!(apex_down.y > c.y, "TriangleDown 的尖端没朝下");
        assert!((apex_down.x - c.x).abs() < 1.0, "TriangleDown 的尖端没在竖直中线上");
        assert!(apex_right.x > c.x, "TriangleRight 的尖端没朝右");
        assert!((apex_right.y - c.y).abs() < 1.0, "TriangleRight 的尖端没在水平中线上");
    }

    /// `Glyph::ALL` 必须真的列全 —— 漏一个，`every_glyph_stays_inside_its_rect`
    /// 就悄悄不覆盖它了（本项目记过的「列举式门控在加档时必然漏」）。
    ///
    /// 没有办法让编译器数枚举变体，所以判据取「每个变体画出来的点集**互不
    /// 相同**」：至少能保证 ALL 里没有重复填充、凑数目。真正的闸门是
    /// `shapes()` 那个穷尽 `match` —— 加变体不补分支直接编译不过。
    ///
    /// 自证会变红：把 `ALL` 里的 `Glyph::TriangleRight` 改成再写一遍
    /// `Glyph::TriangleDown`。
    #[test]
    fn every_glyph_in_all_draws_something_distinct() {
        let mut seen: Vec<Vec<egui::Pos2>> = Vec::new();
        for g in Glyph::ALL.iter().copied() {
            let pts = points_of(&shapes(r(), g, s()));
            assert!(!pts.is_empty(), "{g:?} 什么都没画");
            assert!(!seen.contains(&pts), "{g:?} 与 ALL 里另一个变体画得一模一样");
            seen.push(pts);
        }
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib ui::icon 2>&1 | tail -20
```

预期：编译失败，`no associated item named ALL found` / `no variant named TriangleDown`。

- [ ] **Step 3: 写实现**

`crates/mullion-app/src/ui/icon.rs`，`enum Glyph` 补三个变体（放在 `Info` 之后）：

```rust
    /// ⓘ:说明性提示。挂在常驻灰字前面(走查 18)。
    Info,
    /// 刷新。顶掉 `⟳`(U+27F3)与 `↻`(U+21BB)——两个都不在 GBK,是豆腐。
    Refresh,
    /// 实心下三角:折叠面板的**展开**态。顶掉 `▾`(U+25BE,GBK 外)。
    TriangleDown,
    /// 实心右三角:折叠面板的**折起**态。顶掉 `▸`(U+25B8,GBK 外)。
    TriangleRight,
}

impl Glyph {
    /// 全部变体。**加变体时必须同步这里** —— 测试遍历的是它,漏了就等于
    /// 那个新图标没有任何越界守护。`shapes()` 的穷尽 `match` 拦得住「忘了
    /// 画」,拦不住「忘了登记进 ALL」。
    pub const ALL: &'static [Glyph] = &[
        Glyph::Cross,
        Glyph::ArrowUp,
        Glyph::ArrowDown,
        Glyph::Info,
        Glyph::Refresh,
        Glyph::TriangleDown,
        Glyph::TriangleRight,
    ];
}
```

`shapes()` 的 `match` 里，`Glyph::Info` 分支之后追加：

```rust
        // 顺时针 270° 圆弧 + 端点箭头。epaint 没有 arc 图元,用 16 段折线
        // 近似 —— 16px 见方下肉眼分辨不出是折线。
        //
        // 半径取 `h * 0.6` 而不是贴着 `h`:箭头的两条翼各再伸出 `h * 0.35`,
        // 加起来 0.95h 仍在框内。贴边画的话箭头会捅进邻居按钮的地盘,而
        // `every_glyph_stays_inside_its_rect` 正是为此存在。
        Glyph::Refresh => {
            const SEGS: usize = 16;
            let r = h * 0.6;
            let a0 = std::f32::consts::FRAC_PI_2;
            let sweep = std::f32::consts::PI * 1.5;
            let pts: Vec<_> = (0..=SEGS)
                .map(|i| {
                    let a = a0 + sweep * (i as f32 / SEGS as f32);
                    pos2(c.x + r * a.cos(), c.y + r * a.sin())
                })
                .collect();
            let tip = pts[SEGS];
            let wing = h * 0.35;
            vec![
                Shape::line(pts, stroke),
                Shape::LineSegment {
                    points: [tip, pos2(tip.x - wing, tip.y - wing * 0.4)],
                    stroke: stroke.into(),
                },
                Shape::LineSegment {
                    points: [tip, pos2(tip.x + wing * 0.4, tip.y + wing)],
                    stroke: stroke.into(),
                },
            ]
        }
        // 实心三角。**用填充不用描边**:12px 见方里空心三角的三条边会被
        // 反走样糊成一团灰。`convex_polygon` 产出的是 `Shape::Path`,
        // 测试里的 `points_of` 已经认得。
        Glyph::TriangleDown => vec![Shape::convex_polygon(
            vec![
                pos2(c.x - h * 0.7, c.y - h * 0.4),
                pos2(c.x + h * 0.7, c.y - h * 0.4),
                pos2(c.x, c.y + h * 0.6),
            ],
            stroke.color,
            Stroke::NONE,
        )],
        Glyph::TriangleRight => vec![Shape::convex_polygon(
            vec![
                pos2(c.x - h * 0.4, c.y - h * 0.7),
                pos2(c.x - h * 0.4, c.y + h * 0.7),
                pos2(c.x + h * 0.6, c.y),
            ],
            stroke.color,
            Stroke::NONE,
        )],
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib ui::icon 2>&1 | grep -E "test result|FAILED|panicked"
```

预期：全 pass。若 `every_glyph_stays_inside_its_rect` 红，说明 `Refresh` 的箭头翼伸出了框，调小 `wing`。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/icon.rs
git commit -m "feat(app): 自绘刷新/折叠三角三个图标 (F143)

顶掉 ⟳ ↻ ▾ ▸ 四个 GBK 外的字符。测试从手写数组改成遍历 Glyph::ALL,
并加一条「ALL 里各变体画得互不相同」防凑数。"
```

---

### Task 4: 换掉全部 12 处豆腐

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs:521`（`⟳`）、`:555`（`▾`）
- Modify: `crates/mullion-app/src/ui/transfer_panel.rs:41`（`▾`/`▸`）
- Modify: `crates/mullion-app/src/ui/edit_panel.rs:100`（`▾`/`▸`）
- Modify: `crates/mullion-app/src/tunnels.rs:158-161`（`↻`/`✗`）
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs:311`（`✕`）
- Modify: `crates/mullion-app/src/ui/session_manager/editor.rs:191`（`•`）
- Modify: `crates/mullion-app/src/ui/host_key.rs:51`（`⚠`）
- Modify: `crates/mullion-app/src/ui/session_manager/tunnel_editor.rs:286,290`（`⚠`×2）

**处置总表（照这个改，不要临场发挥）：**

| 字符 | 处 | 落点 | 改成 |
|---|---|---|---|
| `⟳` | 1 | files_panel 刷新按钮 | `icon::icon_button(.., Glyph::Refresh, ..)` |
| `▾` | 1 | files_panel 书签下拉 | `menu_custom_button` + 自绘 `TriangleDown` |
| `▾`/`▸` | 4 | transfer_panel / edit_panel 折叠 | 自绘 `TriangleDown`/`TriangleRight` + 文字按钮 |
| `↻` | 1 | tunnels 状态标记（**纯字符串**，进状态栏） | `…` |
| `✗` | 1 | 同上 | `×` |
| `✕` | 1 | fields 标签 chip 的删除 | `icon::icon_button(.., Glyph::Cross, ..)` |
| `•` | 1 | editor 脏标记圆点 | `●`（U+25CF，GBK 内，chrome.rs 已在用） |
| `⚠` | 3 | host_key 标题 / tunnel_editor 两条文案 | 中文措辞，不用符号 |

- [ ] **Step 1: files_panel 刷新按钮**

`crates/mullion-app/src/ui/files_panel.rs:520-522`，把

```rust
        if ui.small_button("⟳").on_hover_text("刷新(F5)").clicked() {
            action = Some(FileAction::Refresh);
        }
```

换成

```rust
        // F143:`⟳`(U+27F3)不在 GBK,微软雅黑与 egui 内置字体两边都没有 ——
        // 画出来是豆腐块。自绘不受字体覆盖面影响。
        if crate::ui::icon::icon_button(
            ui,
            crate::ui::icon::Glyph::Refresh,
            true,
            "刷新(F5)",
        ) {
            action = Some(FileAction::Refresh);
        }
```

- [ ] **Step 2: files_panel 书签下拉**

`crates/mullion-app/src/ui/files_panel.rs:555`，把 `ui.menu_button("▾", |ui| {` 换成：

```rust
            // F143:`▾`(U+25BE)不在 GBK,是豆腐块(用户 v0.1.56 实测报的
            // 就是这一个)。`menu_button` 只收文本,换不了自绘 —— 改用
            // `menu_custom_button` 传一个空文本按钮,再把三角画进它的 rect。
            let btn = egui::Button::new("")
                .min_size(egui::Vec2::splat(ui.spacing().interact_size.y));
            let menu = egui::menu::menu_custom_button(ui, btn, |ui| {
```

并在这个 `menu_custom_button(...)` 调用**返回之后**（即原来 `.response.on_hover_text("收藏的路径")` 那一段的位置）改成：

```rust
            });
            let resp = menu.response;
            // 按钮体是空的,三角自己画上去。颜色跟随交互态,否则禁用时
            // 三角还是亮的,跟按钮底色对不上。
            if ui.is_rect_visible(resp.rect) {
                let fg = if bookmarks.list.is_empty() {
                    ui.visuals().gray_out(ui.visuals().widgets.inactive.fg_stroke.color)
                } else {
                    ui.style().interact(&resp).fg_stroke.color
                };
                ui.painter().extend(crate::ui::icon::shapes(
                    resp.rect,
                    crate::ui::icon::Glyph::TriangleDown,
                    egui::Stroke::new(1.5, fg),
                ));
            }
            resp.on_hover_text("收藏的路径")
                .on_disabled_hover_text("还没有收藏任何路径");
```

**注意**：现有代码里 `menu_button(...)` 的结果链着 `.response.on_hover_text(..)`，改完要保证 `add_enabled_ui` 闭包的形状不变（闭包体不返回值）。

- [ ] **Step 3: transfer_panel 折叠三角**

`crates/mullion-app/src/ui/transfer_panel.rs:41-44`，把

```rust
                let arrow = if *expanded { "▾" } else { "▸" };
                if ui.button(format!("{arrow} 传输")).clicked() {
                    *expanded = !*expanded;
                }
```

换成

```rust
                // F143:`▾`/`▸` 都不在 GBK,画出来是豆腐。三角自绘,文字
                // 仍走普通按钮 —— 两个控件并排,点哪个都翻折叠。
                let g = if *expanded {
                    crate::ui::icon::Glyph::TriangleDown
                } else {
                    crate::ui::icon::Glyph::TriangleRight
                };
                let tip = if *expanded { "折起传输队列" } else { "展开传输队列" };
                if crate::ui::icon::icon_button(ui, g, true, tip) {
                    *expanded = !*expanded;
                }
                if ui.button("传输").clicked() {
                    *expanded = !*expanded;
                }
```

- [ ] **Step 4: edit_panel 折叠三角**

`crates/mullion-app/src/ui/edit_panel.rs:100-103`，同一手法：

```rust
                // F143:同 `transfer_panel`,`▾`/`▸` 是豆腐,三角改自绘。
                let g = if *expanded {
                    crate::ui::icon::Glyph::TriangleDown
                } else {
                    crate::ui::icon::Glyph::TriangleRight
                };
                let tip = if *expanded { "折起编辑中列表" } else { "展开编辑中列表" };
                if crate::ui::icon::icon_button(ui, g, true, tip) {
                    *expanded = !*expanded;
                }
                if ui.button("编辑中").clicked() {
                    *expanded = !*expanded;
                }
```

- [ ] **Step 5: tunnels 状态标记**

`crates/mullion-app/src/tunnels.rs:157-161`，把

```rust
    let mark = match severity {
        Severity::Calm => "",
        Severity::Warn => " ↻",
        Severity::Danger => " ✗",
    };
```

换成

```rust
    // F143:`↻`(U+21BB)和 `✗`(U+2717)都不在 GBK,是豆腐块。这里是**纯
    // 字符串**(进 `Indicator::text`,由状态栏当普通文本画),没有自绘的
    // 余地 —— 只能换成白名单里的字符。`…` 表「还在路上」,`×` 表「断了」。
    let mark = match severity {
        Severity::Calm => "",
        Severity::Warn => "…",
        Severity::Danger => " ×",
    };
```

**注意**：`tunnels.rs` 的 `#[cfg(test)] mod tests` 里可能有断言比对 `" ↻"` / `" ✗"`，一起改。改完跑 `cargo test -p mullion-app --lib tunnels`。

- [ ] **Step 6: fields 标签 chip 的删除按钮**

`crates/mullion-app/src/ui/session_manager/fields.rs:310-315`，把

```rust
                                        if ui
                                            .add(egui::Button::new("✕").frame(false).small())
                                            .clicked()
```

换成

```rust
                                        // F143:`✕`(U+2715)不在 GBK ——
                                        // 这正是走查 P0-5 当年报的那个豆腐块,
                                        // `icon.rs` 模块头记着它,却又在这里
                                        // 长了回来。这就是白名单守护存在的理由。
                                        if crate::ui::icon::icon_button(
                                            ui,
                                            crate::ui::icon::Glyph::Cross,
                                            true,
                                            "移除这个标签",
                                        )
```

并把上一行注释里的 `点一个 ✕ 删掉另一个。` 改成 `点一个叉删掉另一个。`（注释不参与扫描，但留着一个豆腐字符会误导下一个人）。

- [ ] **Step 7: editor 脏标记圆点**

`crates/mullion-app/src/ui/session_manager/editor.rs:191`，把 `egui::RichText::new("•")` 换成 `egui::RichText::new("●")`，并在上方注释末尾追加一句：

```rust
        // F143:圆点用 `●`(U+25CF)不用 `•`(U+2022)—— 后者不在 GBK,是豆腐。
```

`●` 比 `•` 大一圈，把同一处的 `.size(16.0)` 改成 `.size(10.0)`。

- [ ] **Step 8: host_key 标题**

`crates/mullion-app/src/ui/host_key.rs:51`，把 `"⚠ 主机密钥已变更"` 换成 `"主机密钥已变更（警告）"`。

- [ ] **Step 9: tunnel_editor 两条文案**

`crates/mullion-app/src/ui/session_manager/tunnel_editor.rs:286,290`：

- `format!("⚠ 已删除的会话 (id={})", id.0)` → `format!("（已删除）会话 id={}", id.0)`
- `format!("⚠ {} (SFTP 节点)", s.identity.name)` → `format!("（不可用）{}（SFTP 节点）", s.identity.name)`

- [ ] **Step 10: 跑守护测试确认转绿**

```bash
cargo test -p mullion-app --test glyph_whitelist 2>&1 | grep -E "test result|处 UI 字符串"
```

预期：`test result: ok. 1 passed`。若仍有违规，按报文逐条处置——**不要往白名单里加它来凑绿**。

- [ ] **Step 11: 跑全量测试**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log | grep -v "0 failed"
```

预期：无输出（有断言比对旧字符的测试会在这里暴露，逐个改）。

- [ ] **Step 12: 提交**

```bash
git add -u
git commit -m "fix(app): 换掉全部 12 处画不出来的字形 (F143)

▾▸⟳↻✕ 走自绘,↻✗ 因为是纯字符串改用白名单字符,⚠ 改中文措辞,
• 换成 GBK 内的 ●。守护测试 glyph_whitelist 由红转绿。"
```

---

### Task 5: 面板内容不贴裁剪边（F144）

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs:1487-1497`（`content()` 切两栏）、`:1489-1512`（`sidebar()` 两处 `allocate_ui`）
- Test: `crates/mullion-app/src/ui/files_panel.rs` 的 `mod tests`

- [ ] **Step 1: 写失败的测试**

在 `files_panel.rs` 的 `mod tests` 末尾追加：

```rust
    /// F144:控件不许贴着裁剪边画 —— 贴着画的话圆角描边的外半像素会被
    /// `clip_rect` 切掉,用户看到的是「↑ 按钮左边缺了 1/4 圆弧」「路径条
    /// 控件没有上边框」(v0.1.56 实测报的两条)。
    ///
    /// 判据取**真值**:拿「↑」按钮这一帧实际画出来的位置,向外扩 1pt
    /// (覆盖描边宽度)后必须仍落在本栏裁剪区内。不比 margin 常量 ——
    /// 那是拿常量断言常量(本项目记过的恒绿模式之一)。
    ///
    /// 自证会变红:把 `content()` 里 `left`/`right` 两个 rect 的
    /// `.shrink2(..)` 去掉。
    #[test]
    fn panel_content_does_not_touch_the_clip_edge() {
        let ctx = egui::Context::default();
        let mut frame = ready_panel_frame();
        let mut cols = ColWidths::default();
        let mut rect = None;
        let mut out = None;
        // 两帧:第一帧 egui 还在定布局,第二帧才稳定。
        for _ in 0..2 {
            out = Some(ctx.run(egui::RawInput::default(), |ctx| {
                content(
                    ctx,
                    &crate::theme::Theme::default(),
                    0,
                    false,
                    &mut frame,
                    0,
                    &mut cols,
                    &mut rect,
                );
            }));
        }
        let out = out.expect("跑过两帧");
        let panel = rect.expect("content() 该回填面板矩形");
        // 「↑」是路径条第一个控件。两栏各一个,取最靠左的那个(本地栏在
        // 左半时是它,在右半时也仍是本栏第一个控件 —— 判据只关心「离本栏
        // 左缘有没有留白」,取任意一个都成立)。
        let up = find_text_pos(&out.shapes, "↑");
        // 自绘之后「↑」仍是文字(只有 ⟳/▾ 改了自绘),找得到。
        let up = up.expect("路径条的「↑」没画出来");
        assert!(
            up.x > panel.left() + 1.0,
            "「↑」画在 x={},离面板左缘 {} 太近 —— 描边会被裁掉",
            up.x,
            panel.left()
        );
        assert!(
            up.y > panel.top() + 1.0,
            "「↑」画在 y={},离面板上缘 {} 太近 —— 顶部线条会被裁掉",
            up.y,
            panel.top()
        );
    }
```

**依赖**：这条测试用到 `ready_panel_frame()` 与 `find_text_pos()`。前者若不存在，照 `mod tests` 里既有测试的建法就地构造一个 `PanelFrame`（`Load::Ready` + 两三条 `Entry`）；后者第 1560 行附近已有。**先在 `mod tests` 里 grep 确认这两个名字，用现成的，不要重复造。**

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib panel_content_does_not_touch_the_clip_edge 2>&1 | tail -20
```

预期：FAIL（`「↑」画在 x=..., 离面板左缘 ... 太近`）。

- [ ] **Step 3: 写实现**

`crates/mullion-app/src/ui/files_panel.rs`，`content()` 里切两栏那一段（约 1487-1497 行），把

```rust
            let left = egui::Rect::from_min_size(full.min, egui::vec2(half, full.height()));
            let right =
                egui::Rect::from_min_max(egui::pos2(full.max.x - half, full.min.y), full.max);
```

换成

```rust
            // F144:每栏的矩形再内缩一档。`clip_rect` 仍取这个内缩后的
            // 矩形(B1 那条「必须显式裁剪」不动 —— 不裁的话本栏的滚动条和
            // 超宽内容会画进隔壁栏),内缩是为了让控件不贴着裁剪边起笔:
            // 贴着画的话圆角描边的外半像素落在 rect 之外,被 clip 掉,
            // 视觉上就是「圆弧缺一角」「顶边少一条线」。
            let pad = crate::ui::metrics::SP_XS;
            let left = egui::Rect::from_min_size(full.min, egui::vec2(half, full.height()))
                .shrink2(egui::vec2(pad, pad));
            let right =
                egui::Rect::from_min_max(egui::pos2(full.max.x - half, full.min.y), full.max)
                    .shrink2(egui::vec2(pad, pad));
```

`sidebar()` 里两处 `ui.allocate_ui(...)` 内的 `ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));` 之后，各追加一行：

```rust
                // F144:同 `content()`,内容不贴裁剪边。侧栏的左右已有 Frame
                // 的 `inner_margin` 垫着,这里只补上下。
                ui.shrink_height(crate::ui::metrics::SP_XS);
```

**若 egui 0.30 没有 `Ui::shrink_height`**（先 grep 确认），改用在 `allocate_ui` 之前 `ui.add_space(crate::ui::metrics::SP_XS);`。

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib files_panel 2>&1 | grep -E "test result|FAILED"
```

预期：全 pass。特别确认 `the_two_columns_get_independent_non_overlapping_scroll_areas` 仍绿——内缩不能破坏两栏互不串画。

- [ ] **Step 5: 提交**

```bash
git add -u
git commit -m "fix(app): 文件面板内容不贴裁剪边,补回被切掉的圆角与上边框 (F144)

clip_rect 仍在(B1 约束不动),只把两栏 rect 内缩 SP_XS。
守护 panel_content_does_not_touch_the_clip_edge 取真值,不比常量。"
```

---

### Task 6: 书签下拉显示完整路径（F145）

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs:557-570`

- [ ] **Step 1: 写失败的测试**

`files_panel.rs` 的 `mod tests` 末尾追加（**先 grep 找现有的两条书签下拉测试**：`the_bookmark_menu_*`，约 2586/2653 行，照它们的建法搭场景）：

```rust
    /// F145:书签下拉里每一条显示的是**完整绝对路径**,不是文件夹名。
    /// 用户点开下拉是为了确认「这条书签到底指哪儿」——只给个 `logs`
    /// 等于没回答这个问题(同名目录在不同机器/不同层级下遍地都是)。
    ///
    /// 自证会变红:把 `show()` 里那个 `let label = b.path.as_str();`
    /// 改回 `b.name.as_str()`。
    #[test]
    fn the_bookmark_menu_shows_the_full_path_not_just_the_folder_name() {
        let ctx = egui::Context::default();
        let mut frame = ready_panel_frame();
        frame.session_bound = true;
        frame.bookmarks = vec![mullion_store::Bookmark {
            name: "日志".into(),
            path: "/var/log/nginx".into(),
        }];
        let mut cols = ColWidths::default();
        // 先跑一帧把下拉按钮的位置拿到,再点开它,第三帧读菜单里的文字。
        // (照既有 `the_bookmark_menu_*` 两条测试的做法,不要另起一套。)
        let texts = open_bookmark_menu_and_collect_texts(&ctx, &mut frame, &mut cols);
        assert!(
            texts.iter().any(|s| s.contains("/var/log/nginx")),
            "书签菜单里没有完整路径,实得:{texts:?}"
        );
    }
```

**若既有两条测试没有可复用的 `open_bookmark_menu_and_collect_texts` 辅助**，就照它们里面「找 `▾` 位置 → 造点击 RawInput → 收集 shapes 文字」那几步就地写，**并把它抽成辅助函数供三条测试共用**（既有两条也改成调它）。注意 `▾` 在 Task 4 已改自绘，定位改用 `annotate::spot_rect` 或按钮 id。

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib the_bookmark_menu_shows_the_full_path 2>&1 | tail -20
```

预期：FAIL，实得 `["日志"]`。

- [ ] **Step 3: 写实现**

`crates/mullion-app/src/ui/files_panel.rs:557-570`，把

```rust
                    // 空名字是 store 明确允许的合法状态(`Bookmark::name` 的
                    // 文档),界面回退显示路径本身,不能画一条没有文字的项。
                    let label = if b.name.is_empty() {
                        b.path.as_str()
                    } else {
                        b.name.as_str()
                    };
                    if ui.button(label).on_hover_text(&b.path).clicked() {
```

换成

```rust
                    // F145:主文本恒是**完整绝对路径**。用户点开这个下拉
                    // 就是为了确认「这条书签指哪儿」,只给个 `logs` 等于
                    // 没回答 —— 同名目录在不同机器、不同层级下遍地都是。
                    //
                    // 用户自己起的名字不丢:非空且与路径不同时挂到 hover 上。
                    // (空名是 store 明确允许的合法状态,见 `Bookmark::name`
                    // 的文档 —— 现在这个分支不再影响主文本,只影响 hover。)
                    let label = b.path.as_str();
                    let mut item = ui.button(label);
                    if !b.name.is_empty() && b.name != b.path {
                        item = item.on_hover_text(&b.name);
                    }
                    if item.clicked() {
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib files_panel 2>&1 | grep -E "test result|FAILED"
```

- [ ] **Step 5: 提交**

```bash
git add -u
git commit -m "fix(app): 书签下拉显示完整路径而不是文件夹名 (F145)

用户自己起的名字挪到 hover,不丢。"
```

---

### Task 7: 本地栏不画属主列（F146）

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs:344-366`（`col_lefts` / `content_w`）、`:822-930`（`header_at`）、`:936-1095`（`row`）

- [ ] **Step 1: 写失败的测试**

`files_panel.rs` 的 `mod tests` 末尾追加：

```rust
    /// F146:本地栏不画「属主」列。`files/local.rs` 构造 `Entry` 时
    /// `uid`/`gid` **恒填 0**,本地栏的属主在数据源头上就不存在 ——
    /// `owner_text` 因此对本地栏恒返回 `—`,画出来是一整列破折号,
    /// 白占 120pt 又什么都不说。
    ///
    /// 判据按**栏**静态,不按数据:「本栏所有条目 uid==0」这种动态判据会
    /// 让远端一个全 root 的目录(`/etc` 之类很常见)莫名其妙少一列,
    /// 切个目录又冒出来,列宽还跟着跳。
    ///
    /// 自证会变红:把 `col_lefts` 的 `column` 入参忽略掉,恒返回五列。
    #[test]
    fn the_local_column_has_no_owner_column_but_the_remote_one_does() {
        let local = col_lefts(&ColWidths::default(), PanelColumn::Local);
        let remote = col_lefts(&ColWidths::default(), PanelColumn::Remote);
        assert!(
            !local.iter().any(|(label, ..)| *label == "属主"),
            "本地栏画了属主列,实得:{:?}",
            local.iter().map(|(l, ..)| *l).collect::<Vec<_>>()
        );
        assert!(
            remote.iter().any(|(label, ..)| *label == "属主"),
            "远端栏丢了属主列"
        );
        assert_eq!(local.len(), 4);
        assert_eq!(remote.len(), 5);
    }

    /// 内容总宽必须跟着少一列 —— 不跟的话本地栏会多出 120pt 的空白可滚
    /// 区域,横向滚动条比内容长。
    ///
    /// 自证会变红:把 `content_w` 里的 `column` 入参忽略掉。
    #[test]
    fn the_local_content_width_drops_the_owner_column_too() {
        let c = ColWidths::default();
        assert_eq!(
            content_w(&c, PanelColumn::Remote) - content_w(&c, PanelColumn::Local),
            c.owner,
            "两栏总宽之差应当正好是属主列宽"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib the_local_column_has_no_owner 2>&1 | tail -20
```

预期：编译失败（`col_lefts` 参数个数不对）。

- [ ] **Step 3: 写实现**

`crates/mullion-app/src/ui/files_panel.rs`，`col_lefts` 改签名与返回类型（定长数组改 `Vec`，因为两栏列数不同）：

```rust
/// **列布局的唯一真值来源**:`(标签, SortKey, 左边界, 宽度)`,
/// 左边界从 0 起算(相对行/列头的左边界)。
///
/// 列头(`header_at`)和行体(`row`)调的是同一份 —— 旧模型里两边各自
/// 累加、靠一条对齐测试守着不许分家,现在坐标同源,分家在物理上不可能
/// 发生。**不许在别处再写一遍这个累加。**
///
/// F146:**本地栏没有属主列**。`files/local.rs` 构造 `Entry` 时 uid/gid
/// 恒填 0,那一列在数据源头上就不存在。判据按栏静态,不按数据 —— 理由见
/// `tests::the_local_column_has_no_owner_column_but_the_remote_one_does`。
fn col_lefts(c: &ColWidths, column: PanelColumn) -> Vec<(&'static str, SortKey, f32, f32)> {
    let mut specs: Vec<(&'static str, SortKey, f32)> = vec![
        ("名称", SortKey::Name, c.name),
        ("大小", SortKey::Size, c.size),
        ("修改时间", SortKey::Mtime, c.mtime),
        ("权限", SortKey::Perm, c.perm),
    ];
    if column == PanelColumn::Remote {
        specs.push(("属主", SortKey::Owner, c.owner));
    }
    let mut out = Vec::with_capacity(specs.len());
    let mut x = 0.0;
    for (label, key, w) in specs {
        out.push((label, key, x, w));
        x += w;
    }
    out
}

/// 内容总宽 = 各列之和。视口比它窄就出横向滚动条(F136)。
/// 必须跟 `col_lefts` 走同一份列表 —— 否则本地栏会多出一整列宽的空白可滚。
fn content_w(c: &ColWidths, column: PanelColumn) -> f32 {
    col_lefts(c, column).iter().map(|(_, _, _, w)| w).sum()
}
```

`col_w_mut` 保持不变（列序号 0..3 在两栏含义相同，本地栏根本不会产生序号 4 的热区）。

**所有调用点补 `column` 实参**（用 `cargo build` 逐个找出来，预期约 6 处）：

- `show()` 里 `let total_w = content_w(cols);` → `content_w(cols, column)`
- `header_at()` 需要新增 `column: PanelColumn` 参数，内部两处 `col_lefts(cols)` → `col_lefts(cols, column)`；`show()` 里调用处补实参
- `row()` 里 `let w = content_w(cols).max(...)` → `content_w(cols, column)`；`let lay = col_lefts(cols);` → `col_lefts(cols, column)`
- `row()` 里属主那一段（约 1078-1095 行）整段包进条件：

```rust
    // 属主(右对齐)。F146:本地栏没有这一列 —— `col_lefts` 已经不返回它,
    // 这里跟着按下标存在与否判。
    if let Some(&(_, _, owner_left, owner_w)) = lay.get(4) {
        p.text(
            egui::pos2(
                rect.left() + owner_left + owner_w - crate::ui::metrics::SP_XS,
                rect.center().y,
            ),
            egui::Align2::RIGHT_CENTER,
            elide(
                &owner_text(column, e.uid, e.gid, owners),
                owner_w - crate::ui::metrics::SP_XS,
                Elide::End,
                measure,
            ),
            font,
            theme::c32(t.fg_dim),
        );
    }
```

**注意** `font` 在这一段是被 move 的（前面几段用的是 `font.clone()`）。包进 `if` 之后 move 发生在条件分支里，编译没问题。

`header_at` 里那个列宽拖拽热区循环（`for (i, (_, _, left, w)) in col_lefts(cols).into_iter().enumerate()`）自动跟着少一个热区——这正是要的（本地栏栏尾不该有一个改看不见的列宽的热区）。

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib files_panel 2>&1 | grep -E "test result|FAILED"
```

**既有测试里凡是硬编码 `lay[4]` / 五列假设的，一并改**。特别检查 `the_two_columns_get_independent_non_overlapping_scroll_areas` 和 F135 那批列宽测试。

- [ ] **Step 5: 提交**

```bash
git add -u
git commit -m "feat(app): 本地栏不画属主列 (F146)

local.rs 构造 Entry 时 uid/gid 恒填 0,那一列在数据源头上就不存在,
画出来是一整列破折号。判据按栏静态,不按数据 —— 动态判据会让远端
一个全 root 的目录莫名其妙少一列。"
```

---

### Task 8: 五列 × 两栏逐列可排序（F147 前半，验证第 5 项）

**Files:**
- Test: `crates/mullion-app/src/ui/files_panel.rs` 的 `mod tests`

- [ ] **Step 1: 写测试**

照第 3291 行那条既有测试（`点列头必须真的能排序`，用 `annotate::spot_rect` 定位 + 造点击 RawInput）的做法，在 `mod tests` 末尾追加：

```rust
    /// F147:**每一列**的列头都点得中、都真的会排序,两栏都是。
    ///
    /// 既有那条只覆盖了远端栏的「名称」。用户 v0.1.56 实测报「大小和
    /// 修改时间不支持排序」—— 这条测试就是去证伪或坐实它。
    ///
    /// 判据两段:首点 → `sort_key` 变成该列;再点 → 方向翻转。只测第一段
    /// 的话,一个「点了就恒设成 Asc」的实现也能过。
    ///
    /// 自证会变红:把 `header_at()` 末尾的 `state.click_header(k)` 注释掉。
    #[test]
    fn every_column_header_sorts_in_both_panes() {
        for (id, column, expected_cols) in [
            ("远端", PanelColumn::Remote, 5usize),
            ("本地", PanelColumn::Local, 4),
        ] {
            let lay = col_lefts(&ColWidths::default(), column);
            assert_eq!(lay.len(), expected_cols, "{id} 栏的列数不对");
            for (label, key, ..) in lay {
                let (first, second) = click_header_twice(id, column, label);
                assert_eq!(
                    first,
                    (key, crate::files::SortDir::Asc),
                    "{id} 栏点「{label}」列头没排到 {key:?} 升序 —— 点击多半没落到列头上"
                );
                assert_eq!(
                    second,
                    (key, crate::files::SortDir::Desc),
                    "{id} 栏「{label}」列头再点一次没翻成降序"
                );
            }
        }
    }
```

辅助函数 `click_header_twice(id, column, label) -> ((SortKey, SortDir), (SortKey, SortDir))`：建一个 `Load::Ready` 的 `PanelFrame`，跑一帧拿到 `annotate::spot_rect(&ctx, &format!("文件面板/{id}/列头/{label}"))`，据此造两次 `PointerButton::Primary` 的按下+抬起 RawInput，每次之后读回 `state.sort_key` / `state.sort_dir`。**照既有 3291 行那条测试里已有的建法写，不要另起一套 harness。**

- [ ] **Step 2: 跑，看结果**

```bash
cargo test -p mullion-app --lib every_column_header_sorts_in_both_panes 2>&1 | tail -30
```

**两种结果分开处置，不要预设：**

- **全绿** → 第 5 项的成因是「排序生效了但看不出来」（标识被 `elide` 截掉），Task 9 解决。这条测试留作回归网，直接进 Step 4。
- **某列红** → 是真 bug。首要嫌疑：`header_at` 开头那批列宽拖拽热区（`HANDLE_W = 6.0`，注册在列体之前）。**先做定位再改**：把热区那个 `for` 循环整段临时注释掉再跑一次，若转绿就坐实是它，修法是把热区宽度从「列右边界 ±3pt」收窄，或给热区加 `resp.dragged()` 之外不吃点击的处理。**修完把临时注释还原。**

- [ ] **Step 3: 若有红，就地修**

按 Step 2 的定位结论改，改完重跑到全绿。

- [ ] **Step 4: 提交**

```bash
git add -u
git commit -m "test(app): 五列×两栏逐列排序守护 (F147)

既有那条只盖了远端栏的「名称」。用户报「大小/修改时间不能排序」,
这条去证伪或坐实。"
```

---

### Task 9: 排序标识画在列尾（F147 后半）

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs:893-925`（`header_at` 的标题绘制）

- [ ] **Step 1: 写失败的测试**

`files_panel.rs` 的 `mod tests` 末尾追加：

```rust
    /// F147:排序标识画在**列尾**(列头右端),不是紧跟标题。
    ///
    /// 判据是「标识的 x 落在这一列的右半边」——不比精确坐标(那要把
    /// `SP_XS` 和字宽都算进来,等于把实现抄一遍),只比它在哪一侧。
    ///
    /// 自证会变红:把 `header_at()` 改回 `format!("{label}{mark}")` 一次画完。
    #[test]
    fn the_sort_marker_sits_at_the_far_end_of_the_column() {
        let ctx = egui::Context::default();
        let mut frame = ready_panel_frame();
        // 按「修改时间」降序 —— 标识是 `▼`,列宽 132pt 足够宽,标题和标识
        // 都不会被截断。
        frame.remote.sort_key = SortKey::Mtime;
        frame.remote.sort_dir = crate::files::SortDir::Desc;
        let mut cols = ColWidths::default();
        let out = render_two_frames(&ctx, &mut frame, &mut cols);

        let head = annotate::spot_rect(&ctx, "文件面板/远端/列头/修改时间")
            .expect("「修改时间」列头该登记");
        let marker = find_text_pos(&out.shapes, "▼").expect("降序标识没画出来");
        assert!(
            marker.x > head.center().x,
            "标识画在 x={},列头中线在 {} —— 它还在标题旁边,没挪到列尾",
            marker.x,
            head.center().x
        );
        assert!(
            marker.x < head.right(),
            "标识画到列外去了(x={},列右缘 {})",
            marker.x,
            head.right()
        );
    }

    /// 列窄到放不下时,**标识恒显、标题让位截断**。标识是「当前按哪列排」
    /// 的状态指示,截掉等于这个状态不可见;标题少两个字仍认得出。
    ///
    /// 自证会变红:把 `header_at()` 里给标识预留宽度那一步去掉,改回
    /// 让标题吃满整列。
    #[test]
    fn a_narrow_column_truncates_its_title_but_keeps_the_sort_marker() {
        let ctx = egui::Context::default();
        let mut frame = ready_panel_frame();
        frame.remote.sort_key = SortKey::Mtime;
        frame.remote.sort_dir = crate::files::SortDir::Desc;
        // 48 = `col_min`,「修改时间」四个字在 11 号字下放不下。
        let mut cols = ColWidths {
            mtime: 48.0,
            ..ColWidths::default()
        };
        let out = render_two_frames(&ctx, &mut frame, &mut cols);
        assert!(
            find_text_pos(&out.shapes, "▼").is_some(),
            "列窄了就把排序标识丢了 —— 用户看不出在按哪列排"
        );
        let full = find_text_pos(&out.shapes, "修改时间");
        assert!(full.is_none(), "48pt 的列里画出了完整标题,说明没截断");
    }
```

**辅助**：`render_two_frames(&ctx, &mut frame, &mut cols) -> egui::FullOutput`，跑两帧 `content(...)` 并返回第二帧的输出。若 `mod tests` 里已有等价辅助（Task 5 那条测试写过一个内联版），**抽出来共用**。

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib the_sort_marker_sits_at_the_far_end 2>&1 | tail -20
```

预期：FAIL（标识 x 落在列头左半）。

- [ ] **Step 3: 写实现**

`crates/mullion-app/src/ui/files_panel.rs:893-925`，把 `mark` 拼接 + 单次 `painter.text` 改成两次绘制：

```rust
        let mark = if state.sort_key == key {
            match state.sort_dir {
                crate::files::SortDir::Asc => "▲",
                crate::files::SortDir::Desc => "▼",
            }
        } else {
            ""
        };
        // 裁到横带内:列宽之和超过视口时,右边那几列的标题不能画到
        // 隔壁栏去(同 `content()` 里两栏各自 `set_clip_rect` 的理由)。
        let font = egui::FontId::proportional(11.0);
        let painter = ui.painter().with_clip_rect(band);
        let measure = |s: &str| {
            painter
                .layout_no_wrap(s.to_owned(), font.clone(), egui::Color32::WHITE)
                .size()
                .x
        };
        // F147:标识画在**列尾**,标题左对齐画在剩下的地方。
        //
        // 分两次画而不是拼成一个串:拼串的话标识紧跟标题(短标题时离列尾
        // 老远),而且列窄时 `elide` 会把标识连同标题一起截掉 —— 那是
        // 「当前按哪列排」这个状态直接不可见。
        //
        // 预算顺序是**标识优先**:先扣掉标识和它左边的间隙,剩下的才给
        // 标题。标识少不得,标题少两个字仍认得出。
        let mark_w = if mark.is_empty() {
            0.0
        } else {
            measure(mark) + crate::ui::metrics::SP_XS
        };
        if !mark.is_empty() {
            painter.text(
                rect.right_center() - egui::vec2(crate::ui::metrics::SP_XS, 0.0),
                egui::Align2::RIGHT_CENTER,
                mark,
                font.clone(),
                theme::c32(t.fg_muted),
            );
        }
        painter.text(
            rect.left_center() + egui::vec2(crate::ui::metrics::SP_XS, 0.0),
            egui::Align2::LEFT_CENTER,
            elide(
                label,
                w - crate::ui::metrics::SP_XS * 2.0 - mark_w,
                Elide::End,
                measure,
            ),
            font.clone(),
            theme::c32(t.fg_muted),
        );
```

**注意**：`mark` 的值从 `" ▲"` 改成了 `"▲"`（前导空格不再需要，间距由 `SP_XS` 给）。

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib files_panel 2>&1 | grep -E "test result|FAILED"
```

**既有测试 `列头标题不能横穿到隔壁列头`（约 1804 行）要一并确认仍绿** ——它比的是列头 galley 宽度不超列宽，现在标题预算变小了，应当更安全。

- [ ] **Step 5: 提交**

```bash
git add -u
git commit -m "feat(app): 排序标识挪到列尾,窄列时标题让位 (F147)

分两次画而不是拼串:拼串时 elide 会把标识连同标题一起截掉,
「当前按哪列排」这个状态直接不可见。"
```

---

### Task 10: 登记 spec 与陷阱表

**Files:**
- Modify: `spec.md`（F141/F142 那张表末尾追加五行）
- Modify: `CLAUDE.md`（领域陷阱表追加 T9）
- Modify: `docs/gui-render-gotchas.md`（追加一条）

- [ ] **Step 1: spec.md 追加五行**

在 F142 那一行之后追加（格式照既有行：`| 编号 | 描述 | 优先级 | 守护测试 |`）：

```markdown
| F143 | **UI 字符串只许用实机验过的符号**：egui 的字体链只有「内置拉丁 + `install_cjk_font` 追加的系统 CJK」两级，两级都没有的字形画成豆腐块 `□` —— 编译、测试、日志全不报错，只有人眼能看见。判据是**该字符在 GBK/CP936 内**（微软雅黑覆盖面的实用近似）；**不是 GB18030**（那是全 Unicode 编码方案，"能编码"恒为真，当判据等于没判据）。白名单在 `ui::glyphs::VERIFIED`，加新符号必须先在 Windows 实机看过再登记 —— 登记这一步就是闸门。不想登记的走 `ui::icon` 自绘。这个坑复发过三次（走查 P0-5 的 `✕`、v0.1.56 的 `▾`、以及同一轮扫出来的另外十处，共 12 处） | P1 | `tests/glyph_whitelist.rs::no_ui_string_contains_a_glyph_the_font_cannot_draw`（`proc-macro2` 词法分析，**只查字符串字面量**：attribute（含 `///` 文档注释）与 `#[cfg(test)]` 模块整块跳过，否则 `icon.rs` 模块头里举的反例 `✕` 会把它打成假红）；`ui::glyphs::tests::every_registered_symbol_is_really_inside_gbk`（钉住登记这道闸门本身）；`ui::icon::tests::the_collapse_triangles_point_down_and_right` |
| F144 | **文件面板内容不贴裁剪边**：两栏各自的 `clip_rect` 维持原样（B1 那条「必须显式裁剪」不动，不裁的话本栏滚动条和超宽内容会画进隔壁栏），但 `max_rect` 内缩 `SP_XS`。贴着裁剪边起笔的话，圆角描边的外半像素落在 rect 之外被切掉，视觉上是「↑ 按钮左边缺 1/4 圆弧」「路径条控件没有上边框」 | P2 | `files_panel::tests::panel_content_does_not_touch_the_clip_edge`（判据取真值：按钮实际画出来的位置向外扩 1pt 仍须在裁剪区内，**不比 margin 常量**） |
| F145 | **书签下拉显示完整绝对路径**，不是文件夹名。用户点开下拉就是为了确认「这条书签指哪儿」，只给个 `logs` 等于没回答。用户自己起的名字非空且与路径不同时挂到 hover 上，不丢 | P3 | `files_panel::tests::the_bookmark_menu_shows_the_full_path_not_just_the_folder_name` |
| F146 | **本地栏不画属主列**：`files/local.rs` 构造 `Entry` 时 uid/gid 恒填 0，那一列在数据源头上就不存在，画出来是一整列 `—`，白占 120pt。判据**按栏静态**而不是按数据（「本栏所有条目 uid==0 就隐藏」会让远端一个全 root 的目录莫名其妙少一列，切个目录又冒出来，列宽还跟着跳）。`col_lefts` 加 `PanelColumn` 入参，仍是列布局唯一真值来源 | P3 | `files_panel::tests::the_local_column_has_no_owner_column_but_the_remote_one_does` / `the_local_content_width_drops_the_owner_column_too` |
| F147 | **排序标识画在列尾 + 逐列可排序**：标识与标题分两次画（拼成一个串的话 `elide` 会把标识连同标题一起截掉，「当前按哪列排」这个状态直接不可见）。预算顺序是**标识优先**——标识是状态指示，少不得；标题少两个字仍认得出 | P3 | `files_panel::tests::every_column_header_sorts_in_both_panes`（五列×两栏，首点定列、再点翻向）/ `the_sort_marker_sits_at_the_far_end_of_the_column` / `a_narrow_column_truncates_its_title_but_keeps_the_sort_marker` |
```

- [ ] **Step 2: CLAUDE.md 陷阱表追加 T9**

在 T8 那一行之后追加：

```markdown
| T9 | UI 字符串里写了字体画不出来的符号 | 屏幕上是豆腐块 `□`；编译/测试/日志全不报错，**只有人眼能看见**；已复发三次 | `tests/glyph_whitelist.rs::no_ui_string_contains_a_glyph_the_font_cannot_draw`；判据是 GBK/CP936（**不是** GB18030），白名单在 `ui::glyphs::VERIFIED`，加符号先实机验再登记，不想登记的走 `ui::icon` 自绘 |
```

- [ ] **Step 3: gui-render-gotchas.md 追加一条**

在文件末尾追加：

```markdown
## 字形：写进字符串的符号，字体不一定画得出来（F143）

**症状**：源码里写的是 `▾`，屏幕上是方框 `□`。编译不报错、测试不报错、日志不报错。

**规则**：egui 的字体链只有两级 —— 内置的 Ubuntu-Light / NotoEmoji，加上
`ui::install_cjk_font` 追加的系统 CJK 字体（Windows 上第一候选是微软雅黑）。
两级都没有的字形，epaint 画成豆腐块。

判据是**该字符在 GBK/CP936 内**，那是微软雅黑字形覆盖面的实用近似。
**不是 GB18030** —— GB18030 是全 Unicode 的编码方案，"能编码"对任何字符都成立，
拿它当判据等于没有判据（这个弯路已经走过一次）。

**已知不在 GBK、看着很像"通用符号"的**：`▾ ▸ ▴ ◂`（小三角，注意 `▲ ▼` 是有的）、
`⟳ ↻ ↺`（刷新，注意 `↑ ↓ ← →` 是有的）、`✕ ✗ ✓`（叉与勾）、`⚠`（警告）、
`•`（着重号，注意 `· ●` 是有的）。

**守护**：`crates/mullion-app/tests/glyph_whitelist.rs`。它用 `proc-macro2` 做真词法
分析而不是正则 —— 本项目的注释又长又密，注释里出现一个引号就会让正则的配对错位。
**两处必须跳过**：attribute 里的字符串（`///` 文档注释在 token 流里就是
`#[doc = "..."]`，是货真价实的字面量）、`#[cfg(test)]` 模块（测试数据里有 emoji）。
少跳一处，`icon.rs` 模块头里举的反例 `✕` 会把这条测试打成假红。
```

- [ ] **Step 4: 提交**

```bash
git add spec.md CLAUDE.md docs/gui-render-gotchas.md
git commit -m "docs(spec): 登记 F143~F147,陷阱表加 T9"
```

---

### Task 11: 跑绿 + 发版

- [ ] **Step 1: 全量跑绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log | grep -v " 0 failed"
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo fmt --check
```

三条全静默才算绿。**只跑单个 crate 不算绿。**

⚠️ `cargo fmt` 可能把 Task 9 里那几段 `painter.text(...)` 拆行，**拆行会打断源码锚点类测试**（本项目记过的坑）。fmt 之后重跑一次全量测试。

- [ ] **Step 2: 发版**

调用 `release-windows` skill 走一条龙：patch bump → 跑绿 → 交叉编译 → objdump 依赖验收（出现 `libgcc_s_seh-1.dll` / `libwinpthread-1.dll` 即不合格）→ **签名**（必须在算 sha256 之前）→ push → `gh release create`（标题只能是纯版本号 `v0.1.57`）。

- [ ] **Step 3: 报人工验收清单**

1. 路径条的刷新按钮、书签下拉三角、传输面板与编辑面板的折叠三角，**都不是方框**；会话管理器里标签 chip 的删除叉、主机密钥弹窗标题、隧道编辑器的失效会话提示，同样没有方框；
2. 文件面板两栏内容四周有留白，↑ 按钮圆角完整、路径条控件上边框可见；
3. 点书签下拉，每条显示完整绝对路径（鼠标停上去才看到自己起的名字）；
4. 本地栏没有「属主」列，远端栏有；
5. 点「大小」「修改时间」列头能排序，方向标识出现在**列头右端**；
6. 把某列拖窄，标题被省略号截断，标识仍在。

---

## 自审记录

- **spec 覆盖**：设计文档五节（F143~F147）分别对应 Task 1-4 / 5 / 6 / 7 / 8-9，无遗漏。设计里「GB18030 判据」已在本计划中纠正为 GBK，spec 文档需同步（Task 10 Step 1 的 F143 行已写的是 GBK 版本；设计文档本身保留原文并不影响实现，但**若要改，改 `docs/superpowers/specs/2026-08-20-files-panel-polish-design.md` 里 F143 节的两处「GB18030」**）。
- **豆腐处数**：设计里写 3 处，实际扫出 12 处。本计划 Task 4 按实际清单执行。
- **类型一致性**：`col_lefts` 在 Task 7 从 `[...; 5]` 改成 `Vec<...>`，Task 8/9 的测试与 `header_at`/`row` 全部按 `Vec` 用（`.len()` / `.get(4)` / `.iter()`），一致。
