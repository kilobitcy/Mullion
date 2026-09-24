# F293 / F295 / F294 实现计划:启动页会话图标 · 快捷键一览重排 · 可配置本地热键

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 启动页会话列画图标(F293);设置里的快捷键一览改成结构化、分节、Esc 只出现一次(F295);`app.rs` 的 5 个本地热键改成 7 条可在设置里改的动作,抽屉默认 `` Ctrl+Shift+` ``(F294)。三个 commit,发 v0.1.118。

**Architecture:** `Chord`(四个修饰键 + 键枚举)放 `mullion-store::hotkeys`,TOML 里是一行字符串;`Settings.hotkeys` 稀疏存覆盖。app 侧 `hotkeys` 模块持有动作表 / 默认键 / 键口径(`key_without_modifiers`)/ 合法性 / 撞键;`ui::shortcuts::SHORTCUTS` 升级成结构化表,是显示与撞键的唯一数据源;`window_event` 里一个 `bound_hotkey_event` 算一次 chord 查一次表,替代原来 4 个函数。设置弹窗的表里可配的 7 行就地点按捕获。

**Tech Stack:** Rust / egui 0.30 / winit 0.30.13(`platform::modifier_supplement`)/ serde + toml。

**设计 spec:** `docs/superpowers/specs/2026-09-24-f293-f295-launcher-icons-hotkeys-shortcuts-design.md`(决策 D1–D30,做之前读一遍)。

---

## 已定死的设计决策(不要在实现时重新讨论)

- 会话列图标几何**复用** `project_row` 的常量(改 `pub(crate)`),不抄数字。
- `Chord` 在 store,`Action` 在 app;store 不认识动作名的含义,只存字符串键。
- 运行时**不建** `Bindings` 影子状态:每次按键 `hotkeys::resolve(&self.settings, &chord)`。
- Esc / Enter / Backspace / Space / Delete **不在键域**,表里用 `Keys::Text` 表示。
- 撞键白名单只有一对:`文件面板` 的 Ctrl+Shift+N(`shared_with_new_project: true`)对 `Action::NewProject` 豁免。
- 捕获态拦截是 `window_event` **第一道**检查;拒绝的绑定**不写进草稿**,拒绝后仍留在捕获态等用户再按,Esc 退出。
- commit 顺序:F293 → F295 → F294。三个 commit 都要跑到「绿」(`cargo test --workspace` 全过 + `clippy -D warnings` 零输出 + `cargo fmt --check`)。

## File Structure

| 文件 | 动作 | 职责 |
|---|---|---|
| `crates/mullion-app/src/ui/project_row.rs` | 改 | `ICON_X / ICON_SIDE / TEXT_X / TEXT_RIGHT_PAD` 改 `pub(crate)` |
| `crates/mullion-app/src/ui/launcher.rs` | 改 | 会话列画图标槽;两条测试 |
| `crates/mullion-store/src/hotkeys.rs` | 新建 | `Chord` / `KeyName`:解析、显示、serde(字符串) |
| `crates/mullion-store/src/lib.rs` | 改 | `pub mod hotkeys; pub use hotkeys::{Chord, KeyName};` |
| `crates/mullion-store/src/settings.rs` | 改 | `Settings.hotkeys` 稀疏表 + 两条测试 |
| `crates/mullion-app/src/ui/glyphs.rs` | 改 | 登记 `←` |
| `crates/mullion-app/src/ui/shortcuts.rs` | 重写 | 结构化行 `Keys` / 分节 / `sections()` / `occupant()` / 守护 |
| `crates/mullion-app/src/ui/settings.rs` | 改 | 表按节渲染;F294 加捕获按钮 / 恢复默认 / 错误行;`SettingsDraft` 三个新字段 |
| `crates/mullion-app/src/hotkeys.rs` | 新建 | `Action` / 默认键 / `resolve` / `chord_of_event` / `is_legal` / `vet` / `overrides` |
| `crates/mullion-app/src/lib.rs` | 改 | `pub mod hotkeys;` |
| `crates/mullion-app/src/shell/tabs.rs` | 改 | `hotkey`+`Intent` → `digit_hotkey`(只剩 Ctrl+1…9) |
| `crates/mullion-app/src/app.rs` | 改 | `bound_hotkey_event` 替代 4 个函数;`hotkey_capture_event`;`take_settings_draft` 写回;源码切片测试更新 |
| `spec.md` | 改 | F293 / F294 / F295 三行 |
| `Cargo.toml` | 改 | 0.1.117 → 0.1.118(发版步骤里做) |

---

## Commit 1 —— F293 启动页会话列图标

### Task 1: `project_row` 常量开放 + 会话列画图标

**Files:**
- Modify: `crates/mullion-app/src/ui/project_row.rs:31-40`
- Modify: `crates/mullion-app/src/ui/launcher.rs:321-390`(`sessions_column`)+ `mod tests`

- [ ] **Step 1: 写两条会变红的测试**

在 `crates/mullion-app/src/ui/launcher.rs` 的 `mod tests` 末尾(`drawn_text` 之后)加:

```rust
    // ---- F293:会话列图标 ---------------------------------------------------

    /// 只画会话列要用的那几张表,返回两帧后的全部 shape。
    ///
    /// 项目 / 历史两列都给空表:这两列里没有任何图片,于是「画面上出现了一张
    /// 图」这个判据只可能来自会话列。
    fn session_shapes(sessions: &[SessionRecord]) -> Vec<egui::epaint::ClippedShape> {
        let ctx = egui::Context::default();
        let mut ui_state = crate::ui::UiState::default();
        let lamps = std::collections::BTreeMap::new();
        let mut actions = crate::ui::UiActions::default();
        let mut cache = crate::ui::badge::AppearanceCache::default();
        cache.rebuild(sessions, &[]);
        let mut out = Vec::new();
        for _ in 0..2 {
            out = ctx
                .run(wide(), |ctx| {
                    show(
                        ctx,
                        &crate::theme::MULLION_DARK,
                        &mut ui_state,
                        &Lists {
                            projects: &[],
                            lamps: &lamps,
                            sessions,
                            groups: &[],
                            credentials: &[],
                            history: &[],
                            appearance: &cache,
                        },
                        &mut actions,
                    );
                })
                .shapes;
        }
        out
    }

    /// 给会话挂一张**真** ico:`paint_icon` 会先解码,解不开就整段不画,
    /// 拿假 base64 的话测试会因为「解码失败」而假红。
    fn with_icon(mut r: SessionRecord) -> SessionRecord {
        r.appearance.icon = Some(mullion_store::IconSpec {
            kind: mullion_store::IconKind::Ico,
            value: crate::ui::ico::import(&crate::ui::ico::tests_support::solid_ico(
                32,
                [255, 0, 0, 255],
            ))
            .expect("测试用 ico 应能导入"),
            bg: None,
        });
        r
    }

    /// 第一张「带真纹理且有面积」的 Mesh 的包围盒。判据照抄
    /// `project_row::tests::contains_image`:退化矩形也会发 Mesh,只判纹理
    /// 杀不掉「边长改 0」这条变异。
    fn image_bounds(shapes: &[egui::epaint::ClippedShape]) -> Option<egui::Rect> {
        fn walk(s: &egui::Shape) -> Option<egui::Rect> {
            match s {
                egui::Shape::Vec(v) => v.iter().find_map(walk),
                egui::Shape::Mesh(m) => {
                    let b = m.calc_bounds();
                    (m.texture_id != egui::TextureId::default()
                        && b.width() > 0.0
                        && b.height() > 0.0)
                        .then_some(b)
                }
                _ => None,
            }
        }
        shapes.iter().find_map(|cs| walk(&cs.shape))
    }

    /// 正文恰好等于 `needle` 的那段文字的左边界 x。`paint_highlighted` 一整段
    /// 名字排成一个 galley,所以按全等找得到。
    fn text_x_of(shapes: &[egui::epaint::ClippedShape], needle: &str) -> Option<f32> {
        fn walk(s: &egui::Shape, needle: &str) -> Option<f32> {
            match s {
                egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, needle)),
                egui::Shape::Text(ts) if ts.galley.text() == needle => Some(ts.pos.x),
                _ => None,
            }
        }
        shapes.iter().find_map(|cs| walk(&cs.shape, needle))
    }

    /// F293:会话行真的画出了它的图标;没图标的行不凭空画。
    ///
    /// 自证会变红:把 `sessions_column` 里 `paint_icon` 那段删掉。
    #[test]
    fn a_session_row_paints_the_icon_of_its_session() {
        assert!(
            image_bounds(&session_shapes(&[with_icon(sess(7, "web01"))])).is_some(),
            "会话有图标,启动页会话列却一张图都没画"
        );
        assert!(
            image_bounds(&session_shapes(&[sess(7, "web01")])).is_none(),
            "会话没图标却凭空画了一张图"
        );
    }

    /// F293:有 / 无图标两种行的**名字左边界一样**,而且名字不压在图标上。
    ///
    /// 两条断言缺一不可:只比「一样」的话,把文字左沿改回 `SP_S`(两种行
    /// 都压在图标上)照样绿 —— 「文字不压图标」引入了图标自己的包围盒这个
    /// 第三方参照物(本仓记过的「判据与被测量同源平移 = 恒绿」)。
    ///
    /// 自证会变红:把 `sessions_column` 里 `left` 改回 `rect.left() + SP_S`
    /// (第二条红);改成 `if icon.is_some() { TEXT_X } else { SP_S }`(第一条红)。
    #[test]
    fn the_session_name_starts_at_the_same_x_with_or_without_an_icon() {
        let with = session_shapes(&[with_icon(sess(7, "web01"))]);
        let without = session_shapes(&[sess(7, "web01")]);
        let x_with = text_x_of(&with, "web01").expect("有图标的行没画名字");
        let x_without = text_x_of(&without, "web01").expect("没图标的行没画名字");
        assert!(
            (x_with - x_without).abs() < 0.5,
            "有图标 / 没图标两种行的名字左边界不一样:{x_with} vs {x_without}"
        );
        let icon = image_bounds(&with).expect("有图标的行没画图");
        assert!(
            x_with >= icon.right(),
            "名字({x_with})压在图标({:?})上",
            icon
        );
    }
```

- [ ] **Step 2: 跑,确认两条都红**

Run: `cargo test -p mullion-app --lib ui::launcher::tests::a_session_row_paints_the_icon_of_its_session ui::launcher::tests::the_session_name_starts_at_the_same_x_with_or_without_an_icon 2>&1 | tail -20`
Expected: 两条 FAILED(第一条「一张图都没画」,第二条「压在图标上」)。

- [ ] **Step 3: 常量改 `pub(crate)`**

`crates/mullion-app/src/ui/project_row.rs`,把这四个常量的 `const` 前面加 `pub(crate) `:

```rust
/// 图标槽左边缘距行左边缘。紧挨着灯槽右沿。
///
/// F293:启动页会话列**复用**这一组常量对齐文字左沿(三列并排,会话列
/// 与项目列的名字左沿错开 10 点比缺图难看),所以开成 `pub(crate)`。
pub(crate) const ICON_X: f32 = 24.0;
/// 图标边长。走 F61 那套 32px 纹理档(`paint_icon` 按 `side <= 32` 选档),
/// 比 32 略小一点是为了在 48 点行高里上下留出呼吸。
pub(crate) const ICON_SIDE: f32 = 28.0;
/// 文字左边界 = 图标槽右沿 + 一点呼吸。**恒定**:图标是「有就画、没有就
/// 留空」的,有图标没图标的行文字左边界必须对齐(同灯槽那条理由)。
pub(crate) const TEXT_X: f32 = ICON_X + ICON_SIDE + 6.0;
/// 文字区距行右边缘的留白。
pub(crate) const TEXT_RIGHT_PAD: f32 = 8.0;
```

- [ ] **Step 4: 会话列画图标槽**

`crates/mullion-app/src/ui/launcher.rs` `sessions_column` 里,把从 `let p = ui.painter();` 到第二个 `paint_highlighted(` 调用之前那一段(`let left = …` / `let avail = …`)换成:

```rust
            let p = ui.painter();
            // F293:图标槽。几何与同屏的项目列**同源**(`project_row::ICON_X /
            // ICON_SIDE / TEXT_X`)—— 三列并排,名字左沿必须对齐。槽位**恒定**:
            // 有图标画、没有留空,不画任何占位。图标来源与会话管理器左栏一样
            // 走 `AppearanceCache`(已含会话→分组继承),底色同 `should_paint`。
            let appearance = cx.lists.appearance.get(r.id);
            if let Some(icon) = appearance.and_then(|a| a.icon.as_ref()) {
                use crate::ui::project_row::{ICON_SIDE, ICON_X};
                let slot = egui::Rect::from_center_size(
                    egui::pos2(rect.left() + ICON_X + ICON_SIDE / 2.0, rect.center().y),
                    egui::vec2(ICON_SIDE, ICON_SIDE),
                );
                let bg = appearance.and_then(|a| {
                    crate::ui::badge::should_paint(a, mullion_store::ColorTarget::ListItem)
                });
                crate::ui::badge::paint_icon(p, slot, icon, bg);
            }
            let left = rect.left() + crate::ui::project_row::TEXT_X;
            let avail = (rect.width()
                - crate::ui::project_row::TEXT_X
                - crate::ui::project_row::TEXT_RIGHT_PAD)
                .max(0.0);
```

`use crate::ui::metrics::{SP_S, SP_XS};` 这一行里若 `SP_S` 因此不再被用到,改成只引用还在用的(clippy 会报 unused)。

- [ ] **Step 5: 跑测试,确认绿**

Run: `cargo test -p mullion-app --lib ui::launcher 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok`,launcher 全部测试通过(包括原有的 `all_three_columns_draw_their_own_rows`)。

- [ ] **Step 6: 跑绿 + 提交**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log | grep -v "ok\." 
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
git add crates/mullion-app/src/ui/project_row.rs crates/mullion-app/src/ui/launcher.rs
git commit -m "feat(app): 启动页会话列画图标,几何与项目列同源 (F293)

图标来自 AppearanceCache(含会话→分组继承),底色走 should_paint(ListItem);
槽位恒定,没图标留空。project_row 的 ICON_X/ICON_SIDE/TEXT_X 开成 pub(crate) 复用。

守护:launcher::tests::a_session_row_paints_the_icon_of_its_session /
the_session_name_starts_at_the_same_x_with_or_without_an_icon"
```

---

## Commit 2 —— F295 快捷键一览重排(含 store `Chord`)

### Task 2: store `hotkeys.rs`:`Chord` / `KeyName`

**Files:**
- Create: `crates/mullion-store/src/hotkeys.rs`
- Modify: `crates/mullion-store/src/lib.rs`

- [ ] **Step 1: 建文件,先写测试**

`crates/mullion-store/src/hotkeys.rs`:

```rust
//! F294:本地热键的**组合键表示**。只认结构,不认动作 —— 「哪个动作绑了它」
//! 是 app 的事,这里只负责它长什么样、怎么落盘、怎么显示。
//!
//! # TOML 里是一行字符串
//!
//! 内存里是结构体(四个修饰键 + 键枚举,撞键要按结构比),文件里写成
//! `"ctrl+shift+`"`:修饰键小写、`+` 连接、**末段是键名**。键本身是 `+`
//! 时写 `"ctrl++"` —— 解析先剥修饰键前缀再看剩下的整段,不按 `+` 切分,
//! 所以不会歧义。`settings.toml` 是明文、用户会手改,给人读的形态比嵌套表
//! 值钱。

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// 可绑定的键。**Enter / Esc / Backspace / Space / Delete 不在这里**:它们裸按
/// 有终端语义,带修饰又是终端控制键,没有安全的绑法;需要写在一览表里的
/// 那几处走纯文字。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum KeyName {
    /// 字符键,**存小写**(`Ctrl+Shift+N` 与 `Ctrl+N` 的区别在 `shift` 位,
    /// 不在字母大小写上)。
    Char(char),
    /// F1…F12。
    F(u8),
    Tab,
    PageUp,
    PageDown,
    Home,
    End,
    Up,
    Down,
    Left,
    Right,
}

impl KeyName {
    /// 从 TOML 里的键名段解析。已经是小写。
    fn from_token(tok: &str) -> Option<Self> {
        let named = match tok {
            "tab" => Some(Self::Tab),
            "pageup" => Some(Self::PageUp),
            "pagedown" => Some(Self::PageDown),
            "home" => Some(Self::Home),
            "end" => Some(Self::End),
            "up" => Some(Self::Up),
            "down" => Some(Self::Down),
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            _ => None,
        };
        if named.is_some() {
            return named;
        }
        if let Some(n) = tok.strip_prefix('f') {
            if let Ok(n) = n.parse::<u8>() {
                return (1..=12).contains(&n).then_some(Self::F(n));
            }
        }
        let mut it = tok.chars();
        let c = it.next()?;
        if it.next().is_some() || c.is_whitespace() || c.is_control() {
            return None;
        }
        Some(Self::Char(c.to_lowercase().next().unwrap_or(c)))
    }

    /// TOML 里的键名段。
    fn token(self) -> String {
        match self {
            Self::Char(c) => c.to_string(),
            Self::F(n) => format!("f{n}"),
            Self::Tab => "tab".into(),
            Self::PageUp => "pageup".into(),
            Self::PageDown => "pagedown".into(),
            Self::Home => "home".into(),
            Self::End => "end".into(),
            Self::Up => "up".into(),
            Self::Down => "down".into(),
            Self::Left => "left".into(),
            Self::Right => "right".into(),
        }
    }

    /// 给人看的键名。方向键用箭头(四个都在 GBK 内,app 侧字形白名单已登记)。
    fn label(self) -> String {
        match self {
            Self::Char(c) => c.to_uppercase().to_string(),
            Self::F(n) => format!("F{n}"),
            Self::Tab => "Tab".into(),
            Self::PageUp => "PageUp".into(),
            Self::PageDown => "PageDown".into(),
            Self::Home => "Home".into(),
            Self::End => "End".into(),
            Self::Up => "↑".into(),
            Self::Down => "↓".into(),
            Self::Left => "←".into(),
            Self::Right => "→".into(),
        }
    }
}

/// 一个组合键。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Chord {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub sup: bool,
    pub key: KeyName,
}

/// 解析失败:原串原样带回,报错时给用户看。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChordParseError(pub String);

impl fmt::Display for ChordParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "不是合法的组合键:「{}」", self.0)
    }
}

impl std::error::Error for ChordParseError {}

impl Chord {
    /// 不带任何修饰键。
    pub const fn plain(key: KeyName) -> Self {
        Self {
            ctrl: false,
            shift: false,
            alt: false,
            sup: false,
            key,
        }
    }

    /// 解析 TOML 里那一行。大小写不敏感;修饰键顺序随意;剥完修饰键前缀
    /// 剩下的**整段**是键名(所以 `"ctrl++"` 的键是 `+`)。
    pub fn parse(s: &str) -> Result<Self, ChordParseError> {
        let lower = s.trim().to_lowercase();
        let mut rest = lower.as_str();
        let (mut ctrl, mut shift, mut alt, mut sup) = (false, false, false, false);
        loop {
            if let Some(r) = rest.strip_prefix("ctrl+") {
                ctrl = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("shift+") {
                shift = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("alt+") {
                alt = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("super+") {
                sup = true;
                rest = r;
            } else {
                break;
            }
        }
        let key = KeyName::from_token(rest).ok_or_else(|| ChordParseError(s.to_string()))?;
        Ok(Self {
            ctrl,
            shift,
            alt,
            sup,
            key,
        })
    }

    /// TOML 里的写法。与 [`Self::parse`] 互逆。
    pub fn canonical(&self) -> String {
        let mut out = String::new();
        if self.ctrl {
            out.push_str("ctrl+");
        }
        if self.shift {
            out.push_str("shift+");
        }
        if self.alt {
            out.push_str("alt+");
        }
        if self.sup {
            out.push_str("super+");
        }
        out.push_str(&self.key.token());
        out
    }

    /// 给人看的写法:`Ctrl+Shift+N` / `F6` / `Ctrl+←`。一览表与设置里的
    /// 显示文本**全部由这里生成**,不再手抄。
    pub fn display(&self) -> String {
        let mut out = String::new();
        if self.ctrl {
            out.push_str("Ctrl+");
        }
        if self.shift {
            out.push_str("Shift+");
        }
        if self.alt {
            out.push_str("Alt+");
        }
        if self.sup {
            out.push_str("Super+");
        }
        out.push_str(&self.key.label());
        out
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display())
    }
}

impl Serialize for Chord {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.canonical())
    }
}

impl<'de> Deserialize<'de> for Chord {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctrl_shift(key: KeyName) -> Chord {
        Chord {
            ctrl: true,
            shift: true,
            alt: false,
            sup: false,
            key,
        }
    }

    /// 解析 ↔ canonical 互逆,且大小写 / 顺序都归一。
    ///
    /// 自证会变红:把 `parse` 里 `to_lowercase()` 去掉(第二条红)。
    #[test]
    fn parse_and_canonical_are_inverse_and_case_insensitive() {
        for (text, want) in [
            ("ctrl+shift+`", ctrl_shift(KeyName::Char('`'))),
            ("CTRL+SHIFT+N", ctrl_shift(KeyName::Char('n'))),
            ("shift+ctrl+n", ctrl_shift(KeyName::Char('n'))),
            ("f6", Chord::plain(KeyName::F(6))),
            ("ctrl+tab", Chord { ctrl: true, ..Chord::plain(KeyName::Tab) }),
            ("alt+left", Chord { alt: true, ..Chord::plain(KeyName::Left) }),
            ("super+pageup", Chord { sup: true, ..Chord::plain(KeyName::PageUp) }),
        ] {
            let got = Chord::parse(text).unwrap_or_else(|e| panic!("{text}:{e}"));
            assert_eq!(got, want, "{text}");
            assert_eq!(Chord::parse(&got.canonical()).unwrap(), got, "canonical 不可逆:{text}");
        }
    }

    /// 键本身是 `+` 的写法:`"ctrl++"`。这是「不按 `+` 切分」的全部理由。
    #[test]
    fn a_plus_key_is_written_as_a_trailing_plus() {
        let c = Chord::parse("ctrl++").unwrap();
        assert_eq!(c.key, KeyName::Char('+'));
        assert!(c.ctrl);
        assert_eq!(c.canonical(), "ctrl++");
    }

    /// 残缺 / 不在键域的串一律拒绝,而不是猜一个。
    #[test]
    fn junk_is_rejected() {
        for bad in ["", "ctrl+", "ctrl", "ctrl+enter", "esc", "f13", "f0", "ctrl+ab", "ctrl+ "] {
            assert!(Chord::parse(bad).is_err(), "{bad:?} 不该解析成功");
        }
    }

    /// 显示文本由结构生成。
    ///
    /// 自证会变红:把 `label` 里 `to_uppercase` 去掉。
    #[test]
    fn display_is_generated_from_the_structure() {
        assert_eq!(ctrl_shift(KeyName::Char('c')).display(), "Ctrl+Shift+C");
        assert_eq!(Chord::plain(KeyName::F(6)).display(), "F6");
        assert_eq!(
            Chord { ctrl: true, ..Chord::plain(KeyName::Left) }.display(),
            "Ctrl+←"
        );
        assert_eq!(ctrl_shift(KeyName::Char('`')).display(), "Ctrl+Shift+`");
    }

    /// 走 serde 进 TOML 是一行字符串,读回相等。
    #[test]
    fn serde_writes_a_single_string_and_reads_it_back() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrap {
            k: Chord,
        }
        let w = Wrap {
            k: ctrl_shift(KeyName::Char('`')),
        };
        let text = toml::to_string(&w).unwrap();
        assert!(text.contains("k = \"ctrl+shift+`\""), "{text}");
        assert_eq!(toml::from_str::<Wrap>(&text).unwrap(), w);
        assert!(toml::from_str::<Wrap>("k = \"ctrl+enter\"").is_err());
    }
}
```

- [ ] **Step 2: 挂进 lib.rs**

`crates/mullion-store/src/lib.rs`:在 `pub mod history;` 后面加一行 `pub mod hotkeys;`;在 `pub use history::{…};` 之后加 `pub use hotkeys::{Chord, ChordParseError, KeyName};`。

- [ ] **Step 3: 跑测试**

Run: `cargo test -p mullion-store hotkeys 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: 5 passed。

### Task 3: 字形白名单登记 `←`

**Files:**
- Modify: `crates/mullion-app/src/ui/glyphs.rs`

- [ ] **Step 1: 登记**

`VERIFIED` 数组里 `'↓', // U+2193 下箭头:下载方向` 之后加一行:

```rust
    '←', // U+2190 左箭头:快捷键一览里的方向键(F294 Chord::display)
```

测试 `only_registered_symbols_pass_and_the_known_tofu_does_not` 的第一组数组里加 `'←'`。

- [ ] **Step 2: 跑**

Run: `cargo test -p mullion-app --lib ui::glyphs 2>&1 | grep -E "test result|FAILED"`
Expected: ok(`every_registered_symbol_is_really_inside_gbk` 证明 `←` 在 GBK 内)。

### Task 4: `ui/shortcuts.rs` 结构化 + 分节 + 补齐

**Files:**
- Rewrite: `crates/mullion-app/src/ui/shortcuts.rs`

- [ ] **Step 1: 整文件替换**

```rust
//! F84 / F295:快捷键一览的数据源;F294 起也是**撞键判定**的数据源。
//!
//! # 这张表仍然是手抄的,但键是结构化的
//!
//! 快捷键的实现散在几处,没有统一注册中心:
//!
//! - `mullion_term::keymap` —— 编码给远端的键(T5/T6)
//! - `crate::hotkeys` —— 7 条可配的本地动作(F294)
//! - `shell::tabs::digit_hotkey` —— Ctrl+1…9
//! - `app.rs` 的 `KeyboardInput` 分支 / `handle_panel_key` + `ui::annotate::hotkey`
//!   + `ui::session_manager::keys::scan` —— 其余本地动作
//!
//! 为一张表去把整条输入链路重构成注册中心,代价远大于收益。诚实的做法是承认
//! 它是手抄的、把真源写在这里,然后守住手抄最容易出的错:撞键、漏节、Esc
//! 写两遍([`tests`])。F295 把每行的键从字符串改成 [`Keys`]:**显示文本由
//! 结构生成**,撞键按结构比 —— 字符串比对会被「Ctrl+Shift+C」vs「Ctrl+Shift+c」
//! 这种漂移骗过。
//!
//! # 分节
//!
//! 行按 [`Shortcut::section`] 分组,顺序 = 表里首次出现的顺序。**只有 Esc
//! 合并**进「通用」一节:其余跨节重名(Ctrl+Shift+N / Ctrl+C / Ctrl+1…)各留
//! 各的行 —— 它们在不同焦点下是不同的功能,合并反而藏掉了「谁压过谁」。

use mullion_store::{Chord, KeyName};

/// 一行的键。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keys {
    /// 单个组合键。
    Chord(Chord),
    /// 同一功能的几个键,显示用「 / 」连。
    Chords(&'static [Chord]),
    /// `Ctrl+1 … Ctrl+N`。
    CtrlDigits(u8),
    /// 不在 [`KeyName`] 键域里的键(Esc / Enter / Backspace …)或鼠标手势。
    /// **不参与撞键**:它们本来就绑不了。
    Text(&'static str),
}

impl Keys {
    /// 显示文本。
    pub fn display(&self) -> String {
        match self {
            Self::Chord(c) => c.display(),
            Self::Chords(cs) => cs
                .iter()
                .map(Chord::display)
                .collect::<Vec<_>>()
                .join(" / "),
            Self::CtrlDigits(n) => format!("Ctrl+1 … Ctrl+{n}"),
            Self::Text(s) => (*s).to_string(),
        }
    }

    /// 这一行占了哪些组合键(撞键用)。`Text` 为空。
    pub fn chords(&self) -> Vec<Chord> {
        match self {
            Self::Chord(c) => vec![*c],
            Self::Chords(cs) => cs.to_vec(),
            Self::CtrlDigits(n) => (1..=*n)
                .map(|d| ctrl(KeyName::Char(char::from(b'0' + d))))
                .collect(),
            Self::Text(_) => Vec::new(),
        }
    }

    /// 这一行是否占了 `c`。
    pub fn covers(&self, c: &Chord) -> bool {
        self.chords().contains(c)
    }
}

/// 一览表里的一行。
#[derive(Debug, Clone, Copy)]
pub struct Shortcut {
    pub keys: Keys,
    /// 小节名。**撞键只在同一节内才算撞**(见 `no_two_rows_claim_the_same_chord`);
    /// 「会话管理器」一节在 F294 的改键撞键判定里整体排除 —— 弹窗开着时本地
    /// 热键整体让位(`modal_open`),不会真撞。
    pub section: &'static str,
    /// 干什么。
    pub what: &'static str,
    /// F294 撞键判定的**唯一白名单**:这一行与 `Action::NewProject` 共享同一个
    /// 组合键,靠焦点分辨(焦点在文件面板时建项目那条让位)。只有文件面板的
    /// 「新建文件夹」那一行是 `true`。
    pub shared_with_new_project: bool,
}

pub const fn plain(k: KeyName) -> Chord {
    Chord::plain(k)
}

pub const fn ctrl(k: KeyName) -> Chord {
    Chord {
        ctrl: true,
        shift: false,
        alt: false,
        sup: false,
        key: k,
    }
}

pub const fn ctrl_shift(k: KeyName) -> Chord {
    Chord {
        ctrl: true,
        shift: true,
        alt: false,
        sup: false,
        key: k,
    }
}

pub const fn shift(k: KeyName) -> Chord {
    Chord {
        ctrl: false,
        shift: true,
        alt: false,
        sup: false,
        key: k,
    }
}

const fn row(keys: Keys, section: &'static str, what: &'static str) -> Shortcut {
    Shortcut {
        keys,
        section,
        what,
        shared_with_new_project: false,
    }
}

pub const SECTION_GENERAL: &str = "通用";
pub const SECTION_TABS: &str = "标签";
pub const SECTION_TERMINAL: &str = "终端";
pub const SECTION_FILES: &str = "文件面板";
pub const SECTION_SESSION_MANAGER: &str = "会话管理器";
pub const SECTION_ANNOTATE: &str = "标注模式";
pub const SECTION_PROJECT: &str = "项目";
pub const SECTION_DRAWER: &str = "命令抽屉";

/// 全部快捷键。逐条从实现处核对过,改实现时**必须同步这里**。
pub const SHORTCUTS: &[Shortcut] = &[
    // —— 通用 —— 只有 Esc 合并到这里(设计 D11)。文案按事实写:设置 / 项目管理 /
    // 分组管理 / 导入 / 迁移包 / 解锁 / 标签属性 / 编辑器 / 传输面板 **不认 Esc**。
    row(
        Keys::Text("Esc"),
        SECTION_GENERAL,
        "退出标注模式;关掉会话管理器 / 恢复现场 / 换节点 / 选项目 / 文件对话框 / 远端栏搜索条",
    ),
    // —— 标签(crate::hotkeys + shell::tabs::digit_hotkey)——
    row(Keys::Chord(ctrl(KeyName::Tab)), SECTION_TABS, "切到下一个标签"),
    row(Keys::Chord(ctrl_shift(KeyName::Tab)), SECTION_TABS, "切到上一个标签"),
    row(
        Keys::Chord(ctrl(KeyName::Char('w'))),
        SECTION_TABS,
        "关闭当前标签(抢了 bash 的 ^W 删词)",
    ),
    row(Keys::CtrlDigits(9), SECTION_TABS, "切到第 N 个标签"),
    // —— 终端(app.rs 的 KeyboardInput 分支)——
    row(
        Keys::Chord(ctrl_shift(KeyName::Char('c'))),
        SECTION_TERMINAL,
        "复制选区(裸 Ctrl+C 照旧发给远端)",
    ),
    row(
        Keys::Chord(ctrl_shift(KeyName::Char('v'))),
        SECTION_TERMINAL,
        "粘贴(走 bracketed paste)",
    ),
    row(
        Keys::Chords(&[shift(KeyName::PageUp), shift(KeyName::PageDown)]),
        SECTION_TERMINAL,
        "本地翻页回溯(裸 PageUp / PageDown 照旧发给远端)",
    ),
    row(
        Keys::Text("Shift+拖动"),
        SECTION_TERMINAL,
        "强制本地划选(全屏 TUI 开着鼠标上报时的逃生门)",
    ),
    row(Keys::Text("Shift+Enter"), SECTION_TERMINAL, "插入换行而不提交"),
    // —— 文件面板(crate::hotkeys / app.rs::handle_panel_key)——
    // 侧栏开关与换焦点全局生效;其余只在**焦点落在文件面板**时生效,且多数只认远端栏(D5)。
    row(
        Keys::Chord(ctrl_shift(KeyName::Char('b'))),
        SECTION_FILES,
        "开关文件侧栏(有选区时先跳到选区里那条路径)",
    ),
    row(
        Keys::Chord(plain(KeyName::F(6))),
        SECTION_FILES,
        "终端与文件面板之间换焦点(面板不在场时这个键照旧发给远端)",
    ),
    row(Keys::Chord(ctrl(KeyName::Char('h'))), SECTION_FILES, "显示 / 隐藏点文件"),
    row(
        Keys::Chord(ctrl(KeyName::Char('n'))),
        SECTION_FILES,
        "在远端栏就地新建文件",
    ),
    Shortcut {
        keys: Keys::Chord(ctrl_shift(KeyName::Char('n'))),
        section: SECTION_FILES,
        what: "在远端栏就地新建文件夹(焦点在面板时压过「项目」那条)",
        shared_with_new_project: true,
    },
    row(
        Keys::Chords(&[
            ctrl(KeyName::Char('c')),
            ctrl(KeyName::Char('x')),
            ctrl(KeyName::Char('v')),
        ]),
        SECTION_FILES,
        "远端栏内的复制 / 剪切 / 粘贴",
    ),
    row(Keys::Text("Enter"), SECTION_FILES, "进入目录 / 打开文件"),
    row(Keys::Text("Backspace"), SECTION_FILES, "回到上一级目录"),
    row(Keys::Chord(plain(KeyName::F(5))), SECTION_FILES, "刷新当前栏"),
    row(
        Keys::Chord(plain(KeyName::Tab)),
        SECTION_FILES,
        "在本地栏与远端栏之间切换",
    ),
    row(
        Keys::Chords(&[plain(KeyName::Up), plain(KeyName::Down)]),
        SECTION_FILES,
        "上 / 下移动选中",
    ),
    row(Keys::Text("Delete"), SECTION_FILES, "删除选中(远端栏,先确认)"),
    row(
        Keys::Text("Shift+Delete"),
        SECTION_FILES,
        "删除选中,跳过确认(远端栏)",
    ),
    row(Keys::Chord(plain(KeyName::F(2))), SECTION_FILES, "重命名(远端栏)"),
    row(
        Keys::Text("字母 / 数字"),
        SECTION_FILES,
        "跳到以它开头的条目(1 秒内连按累积前缀,同一字母重按在同首字母间循环)",
    ),
    // —— 会话管理器(ui::session_manager::keys::scan)——
    row(
        Keys::Chords(&[plain(KeyName::Up), plain(KeyName::Down)]),
        SECTION_SESSION_MANAGER,
        "上 / 下一条会话(编辑文本时让位)",
    ),
    row(Keys::Text("Enter"), SECTION_SESSION_MANAGER, "连接选中的会话"),
    row(
        Keys::CtrlDigits(4),
        SECTION_SESSION_MANAGER,
        "切换右栏的编辑器分页",
    ),
    // —— 标注模式(ui::annotate::hotkey)——
    row(
        Keys::Chord(ctrl_shift(KeyName::Char('f'))),
        SECTION_ANNOTATE,
        "进入 / 退出标注模式",
    ),
    row(
        Keys::Chord(ctrl_shift(KeyName::Char('e'))),
        SECTION_ANNOTATE,
        "把标注导出成 Markdown 到剪贴板",
    ),
    row(
        Keys::Chord(ctrl_shift(KeyName::Char('d'))),
        SECTION_ANNOTATE,
        "在紧凑 / 标准 / 详细三档间循环",
    ),
    // —— 项目(crate::hotkeys)——
    row(
        Keys::Chord(ctrl_shift(KeyName::Char('n'))),
        SECTION_PROJECT,
        "把当前分屏的目录和 tmux 会话收成一个新项目(焦点在文件面板时让位)",
    ),
    // —— 命令抽屉(crate::hotkeys)——
    row(
        Keys::Chord(ctrl(KeyName::Char('`'))),
        SECTION_DRAWER,
        "在焦点分屏底下开 / 关命令抽屉",
    ),
];

/// 按小节分组,顺序 = 表里首次出现的顺序。设置弹窗按这个画。
pub fn sections() -> Vec<(&'static str, Vec<&'static Shortcut>)> {
    let mut out: Vec<(&'static str, Vec<&'static Shortcut>)> = Vec::new();
    for s in SHORTCUTS {
        match out.iter_mut().find(|(name, _)| *name == s.section) {
            Some((_, rows)) => rows.push(s),
            None => out.push((s.section, vec![s])),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **同一节里一个组合键只能有一个含义。**
    ///
    /// 撞键在实现处看不出来(各模块各写各的),只有在一览表里并排放着时才
    /// 暴露。按**结构**比:`Chords` / `CtrlDigits` 展开成单个组合键;`Text`
    /// 按字面比。
    ///
    /// 自证会变红:把任意一行的键改成同节里另一行的值。
    #[test]
    fn no_two_rows_claim_the_same_chord() {
        let mut seen_chords: Vec<(&str, Chord)> = Vec::new();
        let mut seen_text: Vec<(&str, &str)> = Vec::new();
        for s in SHORTCUTS {
            for c in s.keys.chords() {
                assert!(
                    !seen_chords.contains(&(s.section, c)),
                    "「{}」在「{}」里出现了两次 —— 同一个组合键不能有两个含义",
                    c.display(),
                    s.section
                );
                seen_chords.push((s.section, c));
            }
            if let Keys::Text(t) = s.keys {
                assert!(
                    !seen_text.contains(&(s.section, t)),
                    "「{t}」在「{}」里出现了两次",
                    s.section
                );
                seen_text.push((s.section, t));
            }
        }
    }

    /// F295 的实报:**Esc 整张表只许出现一次**,在「通用」节。
    ///
    /// 自证会变红:往「标注模式」加回一行 `Keys::Text("Esc")`。
    #[test]
    fn escape_is_listed_exactly_once() {
        let esc: Vec<&Shortcut> = SHORTCUTS
            .iter()
            .filter(|s| s.keys.display() == "Esc")
            .collect();
        assert_eq!(esc.len(), 1, "Esc 出现了 {} 次", esc.len());
        assert_eq!(esc[0].section, SECTION_GENERAL);
    }

    /// 两个文字字段都不许空:空的那一格在表格里就是一行看不懂的东西。
    #[test]
    fn every_row_is_filled_in() {
        for s in SHORTCUTS {
            assert!(!s.keys.display().trim().is_empty(), "有一行没写组合键");
            assert!(!s.section.trim().is_empty(), "「{}」没写小节", s.keys.display());
            assert!(!s.what.trim().is_empty(), "「{}」没写作用", s.keys.display());
        }
    }

    /// 表非空,且覆盖到了每一个有快捷键的模块 —— 漏掉一整节是这张手抄表的
    /// 另一种失效方式(撞键测试对它无感)。
    ///
    /// 自证会变红:删掉某一节的全部行。
    #[test]
    fn every_module_that_has_shortcuts_is_represented() {
        let names: Vec<&str> = sections().into_iter().map(|(n, _)| n).collect();
        assert_eq!(
            names,
            [
                SECTION_GENERAL,
                SECTION_TABS,
                SECTION_TERMINAL,
                SECTION_FILES,
                SECTION_SESSION_MANAGER,
                SECTION_ANNOTATE,
                SECTION_PROJECT,
                SECTION_DRAWER,
            ],
            "小节顺序 / 覆盖面变了"
        );
    }

    /// 显示文本是生成的:`Chords` 用「 / 」连,`CtrlDigits` 是区间写法。
    #[test]
    fn display_is_generated_from_keys() {
        assert_eq!(
            Keys::Chords(&[shift(KeyName::PageUp), shift(KeyName::PageDown)]).display(),
            "Shift+PageUp / Shift+PageDown"
        );
        assert_eq!(Keys::CtrlDigits(9).display(), "Ctrl+1 … Ctrl+9");
        assert!(Keys::CtrlDigits(9).covers(&ctrl(KeyName::Char('5'))));
        assert!(!Keys::CtrlDigits(4).covers(&ctrl(KeyName::Char('5'))));
        assert!(!Keys::CtrlDigits(9).covers(&ctrl_shift(KeyName::Char('5'))));
    }

    /// 白名单只有一对。
    ///
    /// 自证会变红:给「项目」那一行也标上 `shared_with_new_project: true`。
    #[test]
    fn the_only_shared_chord_is_the_files_panel_new_folder() {
        let shared: Vec<&Shortcut> = SHORTCUTS
            .iter()
            .filter(|s| s.shared_with_new_project)
            .collect();
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].section, SECTION_FILES);
        assert_eq!(shared[0].keys, Keys::Chord(ctrl_shift(KeyName::Char('n'))));
    }
}
```

- [ ] **Step 2: 跑 shortcuts 测试**

Run: `cargo test -p mullion-app --lib ui::shortcuts 2>&1 | grep -E "test result|FAILED|panicked|error"`
Expected: 编译错误在 `settings.rs`(`s.chord` / `s.scope` 不存在)—— 下一个 task 修。若 shortcuts 自身编译过,6 条 ok。

### Task 5: 设置弹窗按节渲染

**Files:**
- Modify: `crates/mullion-app/src/ui/settings.rs`(`shortcut_table` + tests)

- [ ] **Step 1: 替换 `shortcut_table`**

把 `fn shortcut_table(ui: &mut egui::Ui, t: &Theme) { … }` 整个换成:

```rust
fn shortcut_table(ui: &mut egui::Ui, t: &Theme) {
    egui::ScrollArea::vertical()
        .max_height(220.0)
        .show(ui, |ui| {
            for (name, rows) in crate::ui::shortcuts::sections() {
                ui.add_space(SP_S);
                ui.label(theme::hint_text(t, name));
                egui::Grid::new(("settings_shortcuts", name))
                    .num_columns(2)
                    .spacing([SP_M, SP_S])
                    .show(ui, |ui| {
                        for s in rows {
                            ui.label(
                                egui::RichText::new(s.keys.display()).color(theme::c32(t.fg)),
                            );
                            ui.label(s.what);
                            ui.end_row();
                        }
                    });
            }
        });
}
```

文件顶部 `use crate::ui::shortcuts::SHORTCUTS;` 删掉(不再直接用)。函数上方那段 F260 的注释保留(给色的理由没变),把「三列」字样改成「两列」。

- [ ] **Step 2: 加测试**

`the_shortcut_table_lists_real_chords` 保留不动(它断言 `"Ctrl+Shift+C"` 这段文字存在,现在由 `display` 生成)。在它后面加:

```rust
    /// F295:表按小节画,Esc 只出现一次。
    ///
    /// 自证会变红:把 `shortcut_table` 里 `ui.label(theme::hint_text(t, name))`
    /// 删掉(第一条红);往 `SHORTCUTS` 的「标注模式」加回一行 Esc(第二条红)。
    #[test]
    fn the_shortcut_table_is_grouped_into_sections() {
        let mut d = draft();
        let (texts, _) = run(&mut d, false);
        for want in ["通用", "标签", "文件面板", "命令抽屉"] {
            assert!(texts.iter().any(|s| s == want), "没画小节「{want}」:{texts:?}");
        }
        assert_eq!(
            texts.iter().filter(|s| s.as_str() == "Esc").count(),
            1,
            "Esc 不是恰好一次:{texts:?}"
        );
    }
```

- [ ] **Step 3: 跑**

Run: `cargo test -p mullion-app --lib ui::settings 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: ok。

- [ ] **Step 4: 跑绿 + 提交**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "FAILED|panicked" /tmp/test.log; grep -c "test result: ok" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
git add crates/mullion-store/src/hotkeys.rs crates/mullion-store/src/lib.rs crates/mullion-app/src/ui/glyphs.rs crates/mullion-app/src/ui/shortcuts.rs crates/mullion-app/src/ui/settings.rs
git commit -m "feat(app): 快捷键一览改结构化数据、按节排版、Esc 只留一行 (F295)

- store 新增 hotkeys::Chord / KeyName(内存结构体,TOML 一行字符串,给 F294 落盘用)
- ui::shortcuts 每行的键改成 Keys(Chord/Chords/CtrlDigits/Text),显示文本由结构生成
- 补齐文件面板 Enter/Backspace/F5/Tab/Delete/F2/字母定位、F6、命令抽屉、搜索条 Esc
- 只合并 Esc 到「通用」节,文案按事实写(设置等弹窗不认 Esc)
- 字形白名单登记 ←

守护:shortcuts::tests::escape_is_listed_exactly_once / no_two_rows_claim_the_same_chord;
settings::tests::the_shortcut_table_is_grouped_into_sections;mullion_store::hotkeys::tests"
```

---

## Commit 3 —— F294 可配置本地热键

### Task 6: `Settings.hotkeys` 稀疏表

**Files:**
- Modify: `crates/mullion-store/src/settings.rs`

- [ ] **Step 1: 写测试**

在 `mod tests` 里 `local_bookmarks_are_global_and_survive_a_round_trip` 之前加:

```rust
    /// F294:热键覆盖**稀疏**落盘 —— 只写与默认不同的条目(默认值归 app,
    /// store 不认识动作,所以「等于默认就删掉」这件事在 app 侧写回时做;
    /// 这里守的是「空表不写、非空表原样读回」)。
    ///
    /// 自证会变红:去掉 `hotkeys` 上的 `skip_serializing_if`(第一条红)。
    #[test]
    fn hotkey_overrides_survive_a_round_trip_and_stay_sparse() {
        let dir = tmp();
        save(dir.path(), &Settings::default()).expect("写盘");
        let text = std::fs::read_to_string(dir.path().join(SETTINGS_FILE)).unwrap();
        assert!(!text.contains("hotkeys"), "没改过热键却写了 [hotkeys]:{text}");

        let mut s = Settings::default();
        s.hotkeys.insert(
            "toggle_drawer".into(),
            crate::hotkeys::Chord::parse("ctrl+alt+d").unwrap(),
        );
        save(dir.path(), &s).expect("写盘");
        let text = std::fs::read_to_string(dir.path().join(SETTINGS_FILE)).unwrap();
        assert!(
            text.contains("toggle_drawer = \"ctrl+alt+d\""),
            "不是一行字符串:{text}"
        );
        let back = load(dir.path());
        assert!(back.note.is_none(), "{:?}", back.note);
        assert_eq!(back.settings, s);
    }

    /// F294 × F247:改热键只带走 `hotkeys` 这一个顶层键,别的实例改的字号不被抹掉。
    #[test]
    fn a_rebound_hotkey_is_grafted_without_touching_the_rest() {
        let base = Settings::default();
        let mut mine = base.clone();
        mine.hotkeys.insert(
            "close_tab".into(),
            crate::hotkeys::Chord::parse("ctrl+shift+w").unwrap(),
        );
        let mut theirs = base.clone();
        theirs.font_pt = 13.0;
        graft_changed(&base, &mine, &mut theirs);
        assert_eq!(theirs.font_pt, 13.0, "别人改的字号被抹掉了");
        assert_eq!(theirs.hotkeys, mine.hotkeys, "热键没合进去");
    }
```

- [ ] **Step 2: 跑,确认编译失败(`hotkeys` 字段不存在)**

Run: `cargo test -p mullion-store hotkey 2>&1 | grep -E "error|test result" | head -3`

- [ ] **Step 3: 加字段**

`Settings` 结构体 `local_bookmarks_migrated` 之后加:

```rust
    /// F294:本地热键覆盖。**稀疏**:键是 app 侧的动作名(`next_tab` / `prev_tab` /
    /// `close_tab` / `toggle_files` / `toggle_focus` / `new_project` / `toggle_drawer`),
    /// 只放与默认不同的条目;默认值归 app,store 不认识动作。
    ///
    /// 空表不写进文件(`skip_serializing_if`),老文件没这一节照常读。
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub hotkeys: std::collections::BTreeMap<String, crate::hotkeys::Chord>,
```

`impl Default for Settings` 里加 `hotkeys: std::collections::BTreeMap::new(),`。

- [ ] **Step 4: 跑**

Run: `cargo test -p mullion-store 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: 全 ok。若别处有完整字面量构造 `Settings { … }` 没用 `..Default::default()`,编译器会指出来,补 `hotkeys: Default::default()`。

### Task 7: app `hotkeys.rs`:动作表 / 匹配 / 合法性 / 撞键

**Files:**
- Create: `crates/mullion-app/src/hotkeys.rs`
- Modify: `crates/mullion-app/src/lib.rs`(`pub mod hotkeys;`,按字母序放在 `pub mod host_key;` 之后)
- Modify: `crates/mullion-app/src/ui/shortcuts.rs`(`Keys::Bound` + `occupant` + `what_of`)

- [ ] **Step 1: 建 `hotkeys.rs`(含测试)**

```rust
//! F294:可配置的本地热键 —— 动作表、默认键、键口径、合法性、撞键。
//!
//! # 没有「Bindings」影子状态
//!
//! 真值只有一份:`Settings.hotkeys`(稀疏覆盖)+ [`Action::default_chord`]。
//! 每次按键 [`resolve`] 直接从 settings 算,7 条比对、零分配。「先拷一份
//! 到 App 字段、改设置时记得同步」这种影子状态本项目踩过 N 次。
//!
//! # 键口径
//!
//! 捕获与匹配两边都走 winit 的 `key_without_modifiers()`:`logical_key` 对
//! `` Ctrl+Shift+` `` 给的是 `~`(布局把 Shift 算进去了),两边会漂;
//! `physical_key` 又不认布局。取不到(死键等)再退到 `logical_key`。

use mullion_store::{Chord, KeyName, Settings};
use std::collections::BTreeMap;
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey};

/// 可配的 7 条动作。**Ctrl+1…9 不在这里**(`shell::tabs::digit_hotkey`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Action {
    NextTab,
    PrevTab,
    CloseTab,
    ToggleFiles,
    ToggleFocus,
    NewProject,
    ToggleDrawer,
}

impl Action {
    pub const ALL: [Action; 7] = [
        Action::NextTab,
        Action::PrevTab,
        Action::CloseTab,
        Action::ToggleFiles,
        Action::ToggleFocus,
        Action::NewProject,
        Action::ToggleDrawer,
    ];

    /// `settings.toml` 里 `[hotkeys]` 下的键名。**改了就是 schema 变更**。
    pub fn name(self) -> &'static str {
        match self {
            Action::NextTab => "next_tab",
            Action::PrevTab => "prev_tab",
            Action::CloseTab => "close_tab",
            Action::ToggleFiles => "toggle_files",
            Action::ToggleFocus => "toggle_focus",
            Action::NewProject => "new_project",
            Action::ToggleDrawer => "toggle_drawer",
        }
    }

    /// 默认键。抽屉是 `` Ctrl+Shift+` ``(F294 实报:原来的 `` Ctrl+` ``
    /// 与某些远端工具撞);文件侧栏必须带 Shift —— `Ctrl+B` 是 tmux 的
    /// prefix,抢了它用户在远端 tmux 里寸步难行。
    pub fn default_chord(self) -> Chord {
        let ctrl = |shift: bool, key: KeyName| Chord {
            ctrl: true,
            shift,
            alt: false,
            sup: false,
            key,
        };
        match self {
            Action::NextTab => ctrl(false, KeyName::Tab),
            Action::PrevTab => ctrl(true, KeyName::Tab),
            Action::CloseTab => ctrl(false, KeyName::Char('w')),
            Action::ToggleFiles => ctrl(true, KeyName::Char('b')),
            Action::ToggleFocus => Chord::plain(KeyName::F(6)),
            Action::NewProject => ctrl(true, KeyName::Char('n')),
            Action::ToggleDrawer => ctrl(true, KeyName::Char('`')),
        }
    }
}

/// 某个动作当前绑的键:覆盖优先,没有就默认。
pub fn chord_for(settings: &Settings, action: Action) -> Chord {
    settings
        .hotkeys
        .get(action.name())
        .copied()
        .unwrap_or_else(|| action.default_chord())
}

/// 这个组合键绑了哪个动作。手改 TOML 造出两个动作同键时,按 [`Action::ALL`]
/// 的顺序取第一个 —— 设置 UI 走 [`vet`],不会造出这种表。
pub fn resolve(settings: &Settings, chord: &Chord) -> Option<Action> {
    Action::ALL
        .into_iter()
        .find(|a| chord_for(settings, *a) == *chord)
}

/// 7 条全量(设置弹窗的草稿用)。
pub fn all_chords(settings: &Settings) -> BTreeMap<Action, Chord> {
    Action::ALL
        .into_iter()
        .map(|a| (a, chord_for(settings, a)))
        .collect()
}

/// 全量 → 稀疏:只留与默认不同的,键换成 TOML 里的名字。写回 settings 用。
pub fn overrides(bound: &BTreeMap<Action, Chord>) -> BTreeMap<String, Chord> {
    bound
        .iter()
        .filter(|(a, c)| **c != a.default_chord())
        .map(|(a, c)| (a.name().to_string(), *c))
        .collect()
}

/// 修饰键本身。捕获态下按到它们 = 用户还在按组合,不算一次输入。
pub fn is_modifier(key: &WKey) -> bool {
    matches!(
        key,
        WKey::Named(
            NamedKey::Shift
                | NamedKey::Control
                | NamedKey::Alt
                | NamedKey::AltGraph
                | NamedKey::Super
                | NamedKey::Meta
                | NamedKey::Hyper
                | NamedKey::CapsLock
                | NamedKey::NumLock
                | NamedKey::ScrollLock
                | NamedKey::Fn
        )
    )
}

/// 把一个 winit 键 + 修饰键状态翻成 [`Chord`]。不在键域返回 `None`。
pub fn chord_of_key(key: &WKey, mods: ModifiersState) -> Option<Chord> {
    let name = match key {
        WKey::Character(s) => {
            let mut it = s.chars();
            let c = it.next()?;
            if it.next().is_some() || c.is_whitespace() || c.is_control() {
                return None;
            }
            KeyName::Char(c.to_lowercase().next().unwrap_or(c))
        }
        WKey::Named(n) => match n {
            NamedKey::Tab => KeyName::Tab,
            NamedKey::PageUp => KeyName::PageUp,
            NamedKey::PageDown => KeyName::PageDown,
            NamedKey::Home => KeyName::Home,
            NamedKey::End => KeyName::End,
            NamedKey::ArrowUp => KeyName::Up,
            NamedKey::ArrowDown => KeyName::Down,
            NamedKey::ArrowLeft => KeyName::Left,
            NamedKey::ArrowRight => KeyName::Right,
            NamedKey::F1 => KeyName::F(1),
            NamedKey::F2 => KeyName::F(2),
            NamedKey::F3 => KeyName::F(3),
            NamedKey::F4 => KeyName::F(4),
            NamedKey::F5 => KeyName::F(5),
            NamedKey::F6 => KeyName::F(6),
            NamedKey::F7 => KeyName::F(7),
            NamedKey::F8 => KeyName::F(8),
            NamedKey::F9 => KeyName::F(9),
            NamedKey::F10 => KeyName::F(10),
            NamedKey::F11 => KeyName::F(11),
            NamedKey::F12 => KeyName::F(12),
            _ => return None,
        },
        _ => return None,
    };
    Some(Chord {
        ctrl: mods.control_key(),
        shift: mods.shift_key(),
        alt: mods.alt_key(),
        sup: mods.super_key(),
        key: name,
    })
}

/// 一次按键事件 → [`Chord`]。**主口径是 `key_without_modifiers()`**(理由见
/// 模块文档),取不到再退到 `logical_key`。
pub fn chord_of_event(ke: &winit::event::KeyEvent, mods: ModifiersState) -> Option<Chord> {
    use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
    chord_of_key(&ke.key_without_modifiers(), mods).or_else(|| chord_of_key(&ke.logical_key, mods))
}

/// 合法绑定:带 Ctrl / Alt / Super 的任意键,或 F1…F12(可裸按)。裸字母 /
/// 数字 / 符号、仅 Shift、裸 Tab 一律拒 —— 会吃掉打字。
pub fn is_legal(c: &Chord) -> Result<(), &'static str> {
    if matches!(c.key, KeyName::F(_)) || c.ctrl || c.alt || c.sup {
        Ok(())
    } else {
        Err("要带 Ctrl / Alt / Super,或者用 F1…F12")
    }
}

/// 捕获那一刻的裁决:合法 + 不撞。撞键比对对象是另外 6 条可配动作的**当前值**
/// (`bound`)与一览表里的硬名单(`ui::shortcuts::occupant`)。`Ok` 才许写进草稿。
pub fn vet(bound: &BTreeMap<Action, Chord>, action: Action, chord: Chord) -> Result<(), String> {
    is_legal(&chord).map_err(str::to_string)?;
    if let Some((other, _)) = bound.iter().find(|(a, c)| **a != action && **c == chord) {
        return Err(format!(
            "已被「{}」占用",
            crate::ui::shortcuts::what_of(*other)
        ));
    }
    if let Some(row) = crate::ui::shortcuts::occupant(&chord, action) {
        return Err(format!("已被「{}」({})占用", row.what, row.section));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::shortcuts::{ctrl, ctrl_shift, plain, shift};

    fn defaults() -> BTreeMap<Action, Chord> {
        all_chords(&Settings::default())
    }

    /// F294 实报:抽屉默认 `` Ctrl+Shift+` ``,旧的 `` Ctrl+` `` **彻底**不再是抽屉键。
    ///
    /// 自证会变红:把 `default_chord` 里 ToggleDrawer 的 `true` 改回 `false`。
    #[test]
    fn the_drawer_default_is_ctrl_shift_backtick_and_the_old_key_is_free() {
        let s = Settings::default();
        assert_eq!(
            resolve(&s, &ctrl_shift(KeyName::Char('`'))),
            Some(Action::ToggleDrawer)
        );
        assert_eq!(resolve(&s, &ctrl(KeyName::Char('`'))), None, "旧键还在开抽屉");
    }

    /// `Ctrl+B` 是 tmux 的 prefix,文件侧栏必须带 Shift。原来是 `app.rs` 里的
    /// 源码切片守护(`!mods.shift`),现在真值在这张表里,直接测表。
    #[test]
    fn ctrl_b_is_left_to_tmux() {
        let s = Settings::default();
        assert_eq!(resolve(&s, &ctrl(KeyName::Char('b'))), None);
        assert_eq!(
            resolve(&s, &ctrl_shift(KeyName::Char('b'))),
            Some(Action::ToggleFiles)
        );
    }

    /// 覆盖优先于默认;覆盖之后旧键让出来。
    #[test]
    fn an_override_replaces_the_default_and_frees_it() {
        let mut s = Settings::default();
        s.hotkeys
            .insert("close_tab".into(), Chord::parse("ctrl+shift+w").unwrap());
        assert_eq!(
            resolve(&s, &ctrl_shift(KeyName::Char('w'))),
            Some(Action::CloseTab)
        );
        assert_eq!(resolve(&s, &ctrl(KeyName::Char('w'))), None, "改掉之后 Ctrl+W 还在关标签");
        // 不认识的键名不影响别的
        s.hotkeys
            .insert("no_such_action".into(), Chord::parse("ctrl+q").unwrap());
        assert_eq!(resolve(&s, &ctrl(KeyName::Char('q'))), None);
    }

    /// 稀疏写回:等于默认的一条都不写。
    ///
    /// 自证会变红:把 `overrides` 里的 `filter` 删掉。
    #[test]
    fn overrides_only_keep_what_differs_from_the_defaults() {
        let mut b = defaults();
        assert!(overrides(&b).is_empty());
        b.insert(Action::ToggleDrawer, Chord::parse("ctrl+alt+d").unwrap());
        let o = overrides(&b);
        assert_eq!(o.len(), 1);
        assert_eq!(o["toggle_drawer"].canonical(), "ctrl+alt+d");
        b.insert(Action::ToggleDrawer, Action::ToggleDrawer.default_chord());
        assert!(overrides(&b).is_empty(), "改回默认之后应当删掉那一条");
    }

    /// 键口径:字母归小写、Named 键映射、不在键域的返回 None。
    #[test]
    fn chord_of_key_lowercases_letters_and_maps_named_keys() {
        let none = ModifiersState::empty();
        let cs = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert_eq!(
            chord_of_key(&WKey::Character("N".into()), cs),
            Some(ctrl_shift(KeyName::Char('n')))
        );
        assert_eq!(
            chord_of_key(&WKey::Named(NamedKey::F6), none),
            Some(plain(KeyName::F(6)))
        );
        assert_eq!(
            chord_of_key(&WKey::Named(NamedKey::ArrowLeft), ModifiersState::ALT),
            Some(Chord { alt: true, ..plain(KeyName::Left) })
        );
        for k in [
            WKey::Named(NamedKey::Enter),
            WKey::Named(NamedKey::Escape),
            WKey::Named(NamedKey::Backspace),
            WKey::Named(NamedKey::Space),
            WKey::Named(NamedKey::Shift),
            WKey::Character("ab".into()),
            WKey::Character(" ".into()),
        ] {
            assert_eq!(chord_of_key(&k, cs), None, "{k:?} 不该进键域");
        }
        assert!(is_modifier(&WKey::Named(NamedKey::Control)));
        assert!(!is_modifier(&WKey::Named(NamedKey::F6)));
    }

    /// 合法性:F 键可裸;别的要带 Ctrl / Alt / Super;仅 Shift 不算。
    ///
    /// 自证会变红:把 `is_legal` 里 `c.ctrl || c.alt || c.sup` 改成 `true`。
    #[test]
    fn legality_requires_a_real_modifier_unless_it_is_an_f_key() {
        assert!(is_legal(&plain(KeyName::F(3))).is_ok());
        assert!(is_legal(&ctrl(KeyName::Char('q'))).is_ok());
        assert!(is_legal(&Chord { alt: true, ..plain(KeyName::Char('q')) }).is_ok());
        assert!(is_legal(&Chord { sup: true, ..plain(KeyName::Char('q')) }).is_ok());
        for bad in [
            plain(KeyName::Char('x')),
            plain(KeyName::Char('1')),
            plain(KeyName::Char('`')),
            shift(KeyName::Char('x')),
            plain(KeyName::Tab),
            shift(KeyName::Tab),
            plain(KeyName::Up),
        ] {
            assert!(is_legal(&bad).is_err(), "{} 不该合法", bad.display());
        }
    }

    /// 撞键:与另一条可配动作撞、与终端 / 文件面板 / 标签 Ctrl+1…9 的硬名单撞
    /// 都拒,并且报得出占用者;「会话管理器」一节不算。
    ///
    /// 自证会变红:把 `vet` 里 `occupant` 那段删掉(第二、三、四条红)。
    #[test]
    fn vet_rejects_collisions_and_names_the_occupant() {
        let b = defaults();
        let err = vet(&b, Action::ToggleDrawer, ctrl(KeyName::Char('w'))).unwrap_err();
        assert!(err.contains("关闭当前标签"), "{err}");
        let err = vet(&b, Action::ToggleDrawer, ctrl_shift(KeyName::Char('c'))).unwrap_err();
        assert!(err.contains("复制选区") && err.contains("终端"), "{err}");
        let err = vet(&b, Action::ToggleDrawer, ctrl(KeyName::Char('h'))).unwrap_err();
        assert!(err.contains("点文件"), "{err}");
        let err = vet(&b, Action::ToggleDrawer, ctrl(KeyName::Char('5'))).unwrap_err();
        assert!(err.contains("第 N 个标签"), "{err}");
        // 会话管理器的 Ctrl+1…4 已被标签的 Ctrl+1…9 盖住;单独验「会话管理器不算」
        // 要找一个只有那一节有的键 —— 它那节只有 ↑/↓/Enter/Ctrl+数字,前三个
        // 本来就不合法,所以这里验的是:`occupant` 对该节返回 None。
        assert!(
            crate::ui::shortcuts::occupant(&plain(KeyName::Up), Action::ToggleDrawer)
                .map(|r| r.section)
                != Some(crate::ui::shortcuts::SECTION_SESSION_MANAGER)
        );
        assert!(vet(&b, Action::ToggleDrawer, Chord::parse("ctrl+alt+d").unwrap()).is_ok());
        // 改成自己当前的键不算撞
        assert!(vet(&b, Action::CloseTab, ctrl(KeyName::Char('w'))).is_ok());
    }

    /// 白名单:`Ctrl+Shift+N` 对 NewProject 放行(现状默认值),对别的动作拒绝。
    ///
    /// 自证会变红:去掉 `occupant` 里 `shared_with_new_project` 那个判断。
    #[test]
    fn vet_whitelists_ctrl_shift_n_only_for_new_project() {
        let mut b = defaults();
        // 先把 NewProject 挪开,再验证它能绑回来
        b.insert(Action::NewProject, Chord::parse("ctrl+alt+p").unwrap());
        assert!(vet(&b, Action::NewProject, ctrl_shift(KeyName::Char('n'))).is_ok());
        let err = vet(&b, Action::ToggleDrawer, ctrl_shift(KeyName::Char('n'))).unwrap_err();
        assert!(err.contains("新建文件夹"), "{err}");
    }

    /// 拒绝发生在写入之前:`vet` 是纯函数,不碰 `bound`。
    #[test]
    fn vet_rejects_before_anything_is_written() {
        let b = defaults();
        let before = b.clone();
        let _ = vet(&b, Action::ToggleDrawer, plain(KeyName::Char('x')));
        assert_eq!(b, before);
    }

    /// 键口径守护:`chord_of_event` 主口径必须是 `key_without_modifiers`。
    /// 造不出 `KeyEvent`(字段私有),只能切源码。
    ///
    /// 自证会变红:把 `chord_of_event` 里 `key_without_modifiers()` 换成 `logical_key`。
    #[test]
    fn the_hotkey_chord_is_read_without_modifiers() {
        let src = include_str!("hotkeys.rs");
        let body = src
            .split("pub fn chord_of_event(")
            .nth(1)
            .expect("找不到 chord_of_event");
        let body: String = body[..body.find("\n}\n").expect("找不到函数结尾")]
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect();
        let primary = body.find("key_without_modifiers()").expect("没用 key_without_modifiers");
        let fallback = body.find("logical_key").expect("没有 logical_key 兜底");
        assert!(primary < fallback, "主口径不是 key_without_modifiers");
    }
}
```

- [ ] **Step 2: `shortcuts.rs` 加 `Keys::Bound` / `occupant` / `what_of`**

`Keys` 枚举加一个变体(放在 `Text` 之前):

```rust
    /// F294:可配动作,真值在 settings 里 —— 显示用 [`Keys::display`] 传进来的
    /// 当前值,撞键不看这里(可配动作之间的撞在 `hotkeys::vet` 里比)。
    Bound(crate::hotkeys::Action),
```

`Keys::display` 签名改成 `pub fn display(&self, bound: &std::collections::BTreeMap<crate::hotkeys::Action, Chord>) -> String`,加分支:

```rust
            Self::Bound(a) => bound
                .get(a)
                .copied()
                .unwrap_or_else(|| a.default_chord())
                .display(),
```

`Keys::chords` 加 `Self::Bound(_) | Self::Text(_) => Vec::new(),`(把原来的 `Self::Text(_) => Vec::new()` 改成这一行)。

表里 7 行改成 `Keys::Bound(..)`:

| 原来 | 改成 |
|---|---|
| `Keys::Chord(ctrl(KeyName::Tab))`(标签 · 下一个) | `Keys::Bound(crate::hotkeys::Action::NextTab)` |
| `Keys::Chord(ctrl_shift(KeyName::Tab))`(标签 · 上一个) | `Keys::Bound(crate::hotkeys::Action::PrevTab)` |
| `Keys::Chord(ctrl(KeyName::Char('w')))` | `Keys::Bound(crate::hotkeys::Action::CloseTab)` |
| `Keys::Chord(ctrl_shift(KeyName::Char('b')))` | `Keys::Bound(crate::hotkeys::Action::ToggleFiles)` |
| `Keys::Chord(plain(KeyName::F(6)))` | `Keys::Bound(crate::hotkeys::Action::ToggleFocus)` |
| `Keys::Chord(ctrl_shift(KeyName::Char('n')))`(**项目**那一行,不是文件面板那行) | `Keys::Bound(crate::hotkeys::Action::NewProject)` |
| `Keys::Chord(ctrl(KeyName::Char('`')))`(命令抽屉) | `Keys::Bound(crate::hotkeys::Action::ToggleDrawer)` |

命令抽屉那行的 `what` 保持;标签节里 `Ctrl+W` 那行的 `what` 保持。

在 `sections()` 后面加:

```rust
/// F294 撞键:硬名单里谁占了 `c`。排除「会话管理器」一节(弹窗开着时本地
/// 热键整体让位)、排除可配行(它们之间的撞在 `hotkeys::vet` 里比)、放行
/// 唯一的白名单对(`shared_with_new_project` × `Action::NewProject`)。
pub fn occupant(c: &Chord, for_action: crate::hotkeys::Action) -> Option<&'static Shortcut> {
    SHORTCUTS.iter().find(|s| {
        s.section != SECTION_SESSION_MANAGER
            && !(s.shared_with_new_project && for_action == crate::hotkeys::Action::NewProject)
            && s.keys.covers(c)
    })
}

/// 某个可配动作在表里的「干什么」,撞键报错用。
pub fn what_of(a: crate::hotkeys::Action) -> &'static str {
    SHORTCUTS
        .iter()
        .find(|s| s.keys == Keys::Bound(a))
        .map_or("(未登记的动作)", |s| s.what)
}
```

测试更新(`shortcuts.rs` 的 `mod tests`):
- `escape_is_listed_exactly_once` / `every_row_is_filled_in` / `display_is_generated_from_keys` 里的 `.display()` 改成 `.display(&crate::hotkeys::all_chords(&mullion_store::Settings::default()))`。加一个测试内 helper:

```rust
    fn defaults() -> std::collections::BTreeMap<crate::hotkeys::Action, Chord> {
        crate::hotkeys::all_chords(&mullion_store::Settings::default())
    }
```
- `the_only_shared_chord_is_the_files_panel_new_folder` 不变。
- 加:

```rust
    /// 7 条可配动作在表里各有且只有一行;每一行都登记了 `what`。
    ///
    /// 自证会变红:把命令抽屉那行改回 `Keys::Chord(..)`。
    #[test]
    fn every_configurable_action_has_exactly_one_row() {
        for a in crate::hotkeys::Action::ALL {
            let n = SHORTCUTS
                .iter()
                .filter(|s| s.keys == Keys::Bound(a))
                .count();
            assert_eq!(n, 1, "{a:?} 在表里出现了 {n} 次");
            assert_ne!(what_of(a), "(未登记的动作)");
        }
    }

    /// 可配行的显示跟着传入的当前值走,不是默认值。
    #[test]
    fn a_bound_row_displays_the_current_chord() {
        let mut b = defaults();
        b.insert(
            crate::hotkeys::Action::ToggleDrawer,
            Chord::parse("ctrl+alt+d").unwrap(),
        );
        assert_eq!(
            Keys::Bound(crate::hotkeys::Action::ToggleDrawer).display(&b),
            "Ctrl+Alt+D"
        );
    }
```

- `no_two_rows_claim_the_same_chord`:可配行 `chords()` 为空,不参与 —— 但默认值仍要在同节内不撞。在该测试开头把可配行按默认值展开:把 `for c in s.keys.chords()` 改成

```rust
            let chords = match s.keys {
                Keys::Bound(a) => vec![a.default_chord()],
                other => other.chords(),
            };
            for c in chords {
```

- [ ] **Step 3: `settings.rs` 的 `shortcut_table` 先跟上签名**(本 task 只让它编译;捕获 UI 在 Task 10)

`ui.label(egui::RichText::new(s.keys.display()) …)` 改成 `s.keys.display(&crate::hotkeys::all_chords(&mullion_store::Settings::default()))` —— **临时**,Task 10 会换成草稿里的值。

- [ ] **Step 4: 跑**

Run: `cargo test -p mullion-app --lib hotkeys ui::shortcuts 2>&1 | grep -E "test result|FAILED|panicked|^error"`
Expected: hotkeys 10 条 + shortcuts 8 条全 ok。

### Task 8: `tabs::hotkey` → `digit_hotkey`

**Files:**
- Modify: `crates/mullion-app/src/shell/tabs.rs`

- [ ] **Step 1: 替换函数与 `Intent`**

删掉 `pub enum Intent { … }` 整个定义及其文档;把 `pub fn hotkey(…) -> Option<Intent> { … }` 换成:

```rust
/// 把一次按键翻译成「切到第 N 个标签」。**纯函数**,`Ctrl+1..9`,1-based,
/// 语义同 [`Tabs::switch_to_nth`](9 = 最后一个)。
///
/// F294 起**只剩数字这一路**:Ctrl+Tab / Ctrl+Shift+Tab / Ctrl+W 已经是可配
/// 热键(`crate::hotkeys`),真值在 settings 里 —— 留在这里的话用户改掉
/// Ctrl+W 之后它照样关标签。
///
/// 返回 `Some` 就意味着调用方要把这个键**吞掉**:既不喂 egui(T8),也不编码
/// 进 PTY。只认 `ctrl` 这一路:`Alt+数字` 在 tmux 里有约定用法,`Super` 归
/// Windows;**不接受 shift**:`Ctrl+Shift+2` 之类在很多终端里是别的东西。
///
/// `modal_open` 为真时不生效:此时键盘归 egui(T8)。闸门放在纯函数里是为了
/// 让它能被真正测出来(`App` 要 `EventLoopProxy` 才能构造)。
pub fn digit_hotkey(
    key: mullion_term::keymap::Key,
    mods: mullion_term::keymap::Mods,
    modal_open: bool,
) -> Option<usize> {
    use mullion_term::keymap::Key;
    if modal_open || !mods.ctrl || mods.shift || mods.alt || mods.sup {
        return None;
    }
    match key {
        Key::Char(c) if c.is_ascii_digit() && c != '0' => Some(c as usize - '0' as usize),
        _ => None,
    }
}
```

- [ ] **Step 2: 替换测试**

删掉 `ctrl_tab_cycles_and_shift_reverses` / `ctrl_w_closes_and_ctrl_digits_jump` / `plain_keys_and_other_modifiers_are_left_alone` / `alt_and_super_combinations_are_left_to_the_system` / `tab_shortcuts_are_inert_while_a_modal_is_open` 五条,换成:

```rust
    /// Ctrl+1…9 → 1-based;`Ctrl+0`、带 Shift/Alt/Super、裸键、Tab/W 一律放行。
    ///
    /// **裸键必须放行**是这组唯一的致命失效模式:判宽了就会吞掉本该进 PTY
    /// 的键,现象是「远端某些键莫名其妙没反应」。Ctrl+Tab / Ctrl+W 也必须
    /// 放行 —— 它们归 `crate::hotkeys`,这里再认一遍等于用户改不掉。
    #[test]
    fn ctrl_digits_jump_and_everything_else_is_left_alone() {
        use mullion_term::keymap::{Key, Mods};
        assert_eq!(digit_hotkey(Key::Char('1'), mods(true, false), false), Some(1));
        assert_eq!(digit_hotkey(Key::Char('9'), mods(true, false), false), Some(9));
        assert_eq!(digit_hotkey(Key::Char('0'), mods(true, false), false), None, "Ctrl+0 是浏览器的重置缩放");
        assert_eq!(digit_hotkey(Key::Char('1'), mods(false, false), false), None, "裸数字是打字");
        assert_eq!(digit_hotkey(Key::Char('1'), mods(true, true), false), None, "Ctrl+Shift+数字在很多终端里是别的东西");
        assert_eq!(digit_hotkey(Key::Tab, mods(true, false), false), None, "Ctrl+Tab 归 hotkeys");
        assert_eq!(digit_hotkey(Key::Char('w'), mods(true, false), false), None, "Ctrl+W 归 hotkeys");
        let ctrl_alt = Mods { ctrl: true, shift: false, alt: true, sup: false };
        let ctrl_sup = Mods { ctrl: true, shift: false, alt: false, sup: true };
        assert_eq!(digit_hotkey(Key::Char('1'), ctrl_alt, false), None, "Alt+数字在 tmux 里有约定用法");
        assert_eq!(digit_hotkey(Key::Char('1'), ctrl_sup, false), None, "Super 归系统");
    }

    /// 模态开着时不生效(T8:此时键盘归 egui)。
    #[test]
    fn digit_shortcuts_are_inert_while_a_modal_is_open() {
        use mullion_term::keymap::Key;
        assert_eq!(digit_hotkey(Key::Char('3'), mods(true, false), true), None);
        assert_eq!(digit_hotkey(Key::Char('3'), mods(true, false), false), Some(3), "无模态时本该命中,否则上一条是空跑");
    }
```

`mods(ctrl, shift)` 这个测试 helper 保留。

- [ ] **Step 3: 跑(预期 app.rs 编译失败,下一个 task 修)**

Run: `cargo test -p mullion-app --lib shell::tabs 2>&1 | grep -E "^error|test result" | head -5`

### Task 9: `app.rs`:`bound_hotkey_event` 替代四个函数 + 源码切片测试更新

**Files:**
- Modify: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: 改 `tab_hotkey_event`**

```rust
    /// F36/S4:`Ctrl+1…9` 切标签的事件前置处理。返回 `true` = 这个键已被吃掉,
    /// 调用方不要再往下分流(既不喂 egui,也不编码进 PTY)。
    ///
    /// F294 起只剩数字这一路:Ctrl+Tab / Ctrl+W 走 `bound_hotkey_event`。
    /// 判定(含模态闸门)全在 `shell::tabs::digit_hotkey` 那个纯函数里,这里只接线。
    fn tab_hotkey_event(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::KeyboardInput { event: ke, .. } = event else {
            return false;
        };
        if ke.state != ElementState::Pressed {
            return false;
        }
        let Some((key, mods)) = input::translate_key(ke, self.mods) else {
            return false;
        };
        let Some(n) = shell::tabs::digit_hotkey(key, mods, self.modal_open()) else {
            return false;
        };
        self.tabs.switch_to_nth(n);
        self.request_ui_redraw();
        true
    }
```

- [ ] **Step 2: 删 `files_hotkey_event` / `drawer_hotkey_event` / `focus_hotkey_event` / `project_hotkey_event`,在 `tab_hotkey_event` 后面加 `bound_hotkey_event`**

四个函数**连同各自的文档注释**整段删掉(`apply_files_hotkey` / `apply_drawer_hotkey` / `apply_project_hotkey` 保留)。加:

```rust
    /// F294:7 条可配本地热键的事件前置处理。返回 `true` = 这个键已被吃掉。
    ///
    /// 算一次 chord(`hotkeys::chord_of_event`,口径 `key_without_modifiers`),
    /// 从 `self.settings` 查一次表(`hotkeys::resolve`,**没有影子状态**),
    /// 再按动作落地。原来的 `files/focus/project/drawer_hotkey_event` 四个函数
    /// 各写一遍「取键 → 判修饰 → 判字符」,这里合成一处;**逐动作的门原样保留**:
    ///
    /// - 全体:`modal_open()` 让位(T8,弹窗开着时键盘归 egui);
    /// - ToggleFocus:面板不在场**不吃这个键**(协调者修订 1)—— F6 是纯终端
    ///   场景里远端 TUI 也会用的功能键,截走了用户查不出原因;判据与 `Present`
    ///   分支共用 `files_owner_generation`;
    /// - NewProject:焦点在文件面板时让位 —— F226 把同一个键给了远端栏的
    ///   「就地新建文件夹」(`handle_panel_key`),那段跑在分流**里面**,位置比
    ///   这里晚,只能这边主动让;判据只看焦点不看是哪一栏(本地栏 F226 是
    ///   静默不动,那这个键在面板上就该一律不做事)。
    ///
    /// 必须在 `window_event` 里输入分流**之前**调用(T8)—— 走到下面字母会被
    /// 编码进 PTY 写给远端,`` ` `` 会被编成控制字符。
    fn bound_hotkey_event(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::KeyboardInput { event: ke, .. } = event else {
            return false;
        };
        if ke.state != ElementState::Pressed || self.modal_open() {
            return false;
        }
        let Some(chord) = crate::hotkeys::chord_of_event(ke, self.mods) else {
            return false;
        };
        let Some(action) = crate::hotkeys::resolve(&self.settings, &chord) else {
            return false;
        };
        use crate::hotkeys::Action;
        match action {
            Action::NextTab => self.tabs.switch_next(),
            Action::PrevTab => self.tabs.switch_prev(),
            Action::CloseTab => self.close_active_tab(),
            Action::ToggleFiles => self.apply_files_hotkey(),
            Action::ToggleFocus => {
                if self.files_owner_generation().is_none() {
                    return false;
                }
                self.focus = self.focus.toggled();
            }
            Action::NewProject => {
                if self.effective_focus() == shell::input_route::Focus::FilesPanel {
                    return false;
                }
                self.apply_project_hotkey();
            }
            Action::ToggleDrawer => self.apply_drawer_hotkey(),
        }
        self.request_ui_redraw();
        true
    }
```

- [ ] **Step 3: `window_event` 接线**

把这四段:

```rust
        if self.files_hotkey_event(&event) { return; }
        if self.focus_hotkey_event(&event) { return; }
        if self.project_hotkey_event(&event) { return; }
        if self.drawer_hotkey_event(&event) { return; }
```

(连同各自的注释)换成一段,位置就在 `tab_hotkey_event` 那段之后:

```rust
        // F294/T8:7 条可配热键(文件侧栏 / 换焦点 / 建项目 / 抽屉 / Ctrl+Tab /
        // Ctrl+W)同样必须在分流之前截 —— 字母走到下面会被编码进 PTY 写给
        // 远端,`` ` `` 会被编成控制字符。判定在 `crate::hotkeys`,这里只接线。
        if self.bound_hotkey_event(&event) {
            return;
        }
```

- [ ] **Step 4: 编译通过**

Run: `cargo build -p mullion-app 2>&1 | grep -E "^(error|warning)" | head`
Expected: 无。若 `use mullion_term::keymap::{Key, WheelAction};` 报 `Key` unused,改成只留 `WheelAction`。

- [ ] **Step 5: 源码切片测试逐条更新**

按函数名 grep 一遍:`grep -n "files_hotkey_event\|focus_hotkey_event\|project_hotkey_event\|drawer_hotkey_event\|tabs::Intent\|Intent::" crates/mullion-app/src/app.rs`,逐条处理:

1. `the_project_hotkey_yields_ctrl_shift_n_back_to_the_files_panel`:`.split("fn project_hotkey_event(")` → `.split("fn bound_hotkey_event(")`;`.expect("找不到 project_hotkey_event")` → `"找不到 bound_hotkey_event"`;`wev.find("self.project_hotkey_event(&event)")` → `"self.bound_hotkey_event(&event)"`;两条错误文案里的 `project_hotkey_event` 改成 `bound_hotkey_event`;文档注释里「自证会变红」改成「把 `bound_hotkey_event` 里 `NewProject` 分支那句 `effective_focus() == ...FilesPanel` 去掉」。
2. `files_shortcut_is_swallowed_before_the_input_routing` / `focus_shortcut_is_swallowed_before_the_input_routing` / `project_shortcut_is_swallowed_before_the_input_routing` / `drawer_shortcut_is_swallowed_before_the_input_routing`:**四条删掉**,换成一条(放在 `tab_shortcuts_are_swallowed_before_the_input_routing` 之后):

```rust
    /// **接线守护 / T8**:F294 的 7 条可配热键必须在输入分流**之前**被截走
    /// (原来 files/focus/project/drawer 四条各一条守护,合成一处)。不截的话,
    /// `Ctrl+Shift+B` 里的 `B` 会先被喂给 egui 的焦点系统,也会被 `KeyboardInput`
    /// 分支编码进 PTY 写给远端;`` Ctrl+Shift+` `` 会被编成控制字符。
    ///
    /// 用 `body_of` + `strip_comments`(本仓记过的坑「源码切片守护不剥注释」)。
    /// 验证边界:只挡得住「调用点跑到分流之后 / 整个没调 / 调了两次」。
    ///
    /// 自证会变红:把 `window_event` 里 `if self.bound_hotkey_event(&event)`
    /// 整段删掉,或挪到 `egui_should_see` 那段之后。
    #[test]
    fn bound_hotkeys_are_swallowed_before_the_input_routing() {
        let body = strip_comments(body_of(prod_src(), "fn window_event("));
        assert_eq!(
            body.matches("self.bound_hotkey_event(&event)").count(),
            1,
            "调用不止一次 —— 多一处占位会把位置判据骗过去"
        );
        let hotkey = body.find("self.bound_hotkey_event(&event)").unwrap();
        let routing = body.find("egui_should_see").expect("找不到输入分流那一段");
        assert!(hotkey < routing, "bound_hotkey_event 排在了输入分流之后 —— 排在后面等于没截");
        for gone in ["files_hotkey_event", "focus_hotkey_event", "project_hotkey_event", "drawer_hotkey_event"] {
            assert!(!body.contains(gone), "{gone} 还在 —— 旧的写死热键没拆干净,用户改了键它照样生效");
        }
    }
```

3. `f6_is_gated_on_the_panel_actually_being_visible`:`.split("fn focus_hotkey_event(")` → `.split("fn bound_hotkey_event(")`,两处 `expect` 文案与断言文案里的 `focus_hotkey_event` 改 `bound_hotkey_event`;文档注释「自证会变红」改成「把 `bound_hotkey_event` 里 `ToggleFocus` 分支那句 `files_owner_generation().is_none()` 删掉」。
4. `the_files_shortcut_requires_shift_so_it_cannot_steal_tmux_prefix`:**删掉**(真值搬进了 `hotkeys::Action::default_chord`,守护是 `hotkeys::tests::ctrl_b_is_left_to_tmux`)。
5. `tab_switching_never_reconnects`:在切 `tab_hotkey_event` 函数体那段之后,再切一段 `bound_hotkey_event`:

```rust
        let bound = src
            .split("fn bound_hotkey_event")
            .nth(1)
            .expect("找不到 bound_hotkey_event");
        let bound_body = &bound[..bound
            .find("\n    }\n")
            .expect("找不到 bound_hotkey_event 的函数结尾")];
        assert!(
            !bound_body.contains("spawn_connect"),
            "bound_hotkey_event(Ctrl+Tab / Ctrl+Shift+Tab)里出现了 spawn_connect"
        );
```
   文档注释里的 `Intent::Next` 改成 `Action::NextTab`。
6. `the_drawer_hotkey_is_ctrl_backtick_and_yields_to_modals`:**删掉**(默认键守护在 `hotkeys::tests::the_drawer_default_is_ctrl_shift_backtick_and_the_old_key_is_free`;模态门由下面这条守)。加:

```rust
    /// F294:可配热键整组在弹窗开着时让位(T8)。
    ///
    /// 自证会变红:把 `bound_hotkey_event` 里 `self.modal_open()` 那一项删掉。
    #[test]
    fn bound_hotkeys_yield_to_modals() {
        let body = strip_comments(body_of(prod_src(), "fn bound_hotkey_event("));
        assert!(body.contains("self.modal_open()"), "弹窗开着也响应 —— 会在输入框里打字时突然分屏");
        assert!(body.contains("hotkeys::resolve(&self.settings"), "没从 settings 查表 —— 改了键不生效");
    }
```

7. 文档注释里提到旧函数名的(`files_find_escape_event` 那几条的 `///`):把 `tab_hotkey_event`/`files_hotkey_event` 字样改成 `bound_hotkey_event`,只是注释。

- [ ] **Step 6: 跑 app 测试**

Run: `cargo test -p mullion-app --lib app::tests 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: ok。如有别的源码切片测试因四个函数消失而红,按同样思路改到 `bound_hotkey_event`,**不要**为了绿而加回旧函数。

### Task 10: 设置弹窗:捕获 / 恢复默认 / 错误 + 草稿写回

**Files:**
- Modify: `crates/mullion-app/src/ui/settings.rs`
- Modify: `crates/mullion-app/src/app.rs`(`take_settings_draft`)

- [ ] **Step 1: 写测试(settings.rs `mod tests`)**

```rust
    // ---- F294:改键 ----

    /// 点可配行的键 → 进入捕获态,按钮文字换成提示。
    ///
    /// 自证会变红:把 `shortcut_table` 里 `draft.capturing = Some(a)` 删掉。
    #[test]
    fn clicking_a_bound_chord_enters_capture() {
        let mut d = draft();
        let _ = click(&mut d, "Ctrl+Shift+`");
        assert_eq!(d.capturing, Some(crate::hotkeys::Action::ToggleDrawer));
        let (texts, _) = run(&mut d, false);
        assert!(texts.iter().any(|s| s == CAPTURE_LABEL), "没显示捕获提示:{texts:?}");
        assert!(!texts.iter().any(|s| s == "Ctrl+Shift+`"), "捕获态下还画着旧键");
    }

    /// 「恢复默认」只在改过的行出现;点了回到默认并消失。
    ///
    /// 自证会变红:把 `chord != a.default_chord()` 那个判断改成 `true`(第一条红)。
    #[test]
    fn restore_default_only_shows_for_changed_rows() {
        let mut d = draft();
        let (texts, _) = run(&mut d, false);
        assert!(!texts.iter().any(|s| s == RESTORE_LABEL), "没改过键就出了恢复按钮");
        d.hotkeys.insert(
            crate::hotkeys::Action::ToggleDrawer,
            mullion_store::Chord::parse("ctrl+alt+d").unwrap(),
        );
        let (texts, _) = run(&mut d, false);
        assert!(texts.iter().any(|s| s == "Ctrl+Alt+D"), "没画出改过的键:{texts:?}");
        assert_eq!(texts.iter().filter(|s| s.as_str() == RESTORE_LABEL).count(), 1);
        let _ = click(&mut d, RESTORE_LABEL);
        assert_eq!(
            d.hotkeys[&crate::hotkeys::Action::ToggleDrawer],
            crate::hotkeys::Action::ToggleDrawer.default_chord()
        );
    }

    /// 捕获:合法且不撞 → 写进草稿、退出捕获;撞了 → 不写、留在捕获态、错误上屏。
    ///
    /// 自证会变红:`capture` 里把 `Err` 分支也 `insert`。
    #[test]
    fn capture_writes_only_a_vetted_chord() {
        let mut d = draft();
        d.capturing = Some(crate::hotkeys::Action::ToggleDrawer);
        d.capture(crate::ui::shortcuts::ctrl_shift(mullion_store::KeyName::Char('c')));
        assert_eq!(
            d.hotkeys[&crate::hotkeys::Action::ToggleDrawer],
            crate::hotkeys::Action::ToggleDrawer.default_chord(),
            "撞键的绑定写进草稿了"
        );
        assert_eq!(d.capturing, Some(crate::hotkeys::Action::ToggleDrawer), "拒绝后应留在捕获态");
        let (texts, _) = run(&mut d, false);
        assert!(texts.iter().any(|s| s.contains("复制选区")), "没报出占用者:{texts:?}");

        d.capture(mullion_store::Chord::parse("ctrl+alt+d").unwrap());
        assert_eq!(d.hotkeys[&crate::hotkeys::Action::ToggleDrawer].canonical(), "ctrl+alt+d");
        assert_eq!(d.capturing, None);
        assert_eq!(d.hotkey_error, None);
    }
```

- [ ] **Step 2: `SettingsDraft` 加字段 + `capture`**

结构体 `cloud_pass_confirm` 之后加:

```rust
    /// F294:7 条可配热键的当前值(**全量**,默认已填上);点「确定」时只把
    /// 与默认不同的写回 `Settings.hotkeys`(`hotkeys::overrides`)。
    pub hotkeys: std::collections::BTreeMap<crate::hotkeys::Action, mullion_store::Chord>,
    /// F294:正在等用户按新组合键的那一行。
    pub capturing: Option<crate::hotkeys::Action>,
    /// F294:上一次捕获被拒的原因,画在表底下;成功捕获 / 再次点开 / Esc 时清掉。
    pub hotkey_error: Option<String>,
```

`from_settings_and_cloud` 字面量末尾加:

```rust
            hotkeys: crate::hotkeys::all_chords(s),
            capturing: None,
            hotkey_error: None,
```

`impl SettingsDraft` 里 `password_ready` 之前加:

```rust
    /// F294:捕获到一个组合键。**先裁决再写**:`vet` 不过就一个字都不动草稿,
    /// 留在捕获态等用户再按(Esc 退出在 `app.rs::hotkey_capture_event`)。
    pub fn capture(&mut self, chord: mullion_store::Chord) {
        let Some(action) = self.capturing else {
            return;
        };
        match crate::hotkeys::vet(&self.hotkeys, action, chord) {
            Ok(()) => {
                self.hotkeys.insert(action, chord);
                self.capturing = None;
                self.hotkey_error = None;
            }
            Err(msg) => self.hotkey_error = Some(msg),
        }
    }
```

常量(放在 `HIDDEN_FILES_LABEL` 附近):

```rust
/// F294:可配行进入捕获态时按钮上的字。
pub(crate) const CAPTURE_LABEL: &str = "请按下新组合键…";
/// F294:改过的行后面那个按钮。
pub(crate) const RESTORE_LABEL: &str = "恢复默认";
/// F294:按到不在键域里的键(Enter / Space / Esc 以外的控制键…)时的提示。
pub(crate) const UNBINDABLE_MSG: &str = "这个键不能用作快捷键";
```

- [ ] **Step 3: `shortcut_table` 接草稿**

签名改 `fn shortcut_table(ui: &mut egui::Ui, t: &Theme, draft: &mut SettingsDraft)`,调用处 `shortcut_table(ui, t, draft);`。函数体:

```rust
fn shortcut_table(ui: &mut egui::Ui, t: &Theme, draft: &mut SettingsDraft) {
    use crate::ui::shortcuts::Keys;
    egui::ScrollArea::vertical()
        .max_height(220.0)
        .show(ui, |ui| {
            for (name, rows) in crate::ui::shortcuts::sections() {
                ui.add_space(SP_S);
                ui.label(theme::hint_text(t, name));
                egui::Grid::new(("settings_shortcuts", name))
                    .num_columns(2)
                    .spacing([SP_M, SP_S])
                    .show(ui, |ui| {
                        for s in rows {
                            match s.keys {
                                // F294:可配行,键那一格是按钮 —— 点下去进捕获态。
                                Keys::Bound(a) => {
                                    ui.horizontal(|ui| {
                                        let chord = draft
                                            .hotkeys
                                            .get(&a)
                                            .copied()
                                            .unwrap_or_else(|| a.default_chord());
                                        let label = if draft.capturing == Some(a) {
                                            CAPTURE_LABEL.to_string()
                                        } else {
                                            chord.display()
                                        };
                                        if ui
                                            .button(egui::RichText::new(label).color(theme::c32(t.fg)))
                                            .clicked()
                                        {
                                            draft.capturing = Some(a);
                                            draft.hotkey_error = None;
                                        }
                                        if chord != a.default_chord()
                                            && ui.small_button(RESTORE_LABEL).clicked()
                                        {
                                            draft.hotkeys.insert(a, a.default_chord());
                                            draft.hotkey_error = None;
                                        }
                                    });
                                }
                                _ => {
                                    ui.label(
                                        egui::RichText::new(s.keys.display(&draft.hotkeys))
                                            .color(theme::c32(t.fg)),
                                    );
                                }
                            }
                            ui.label(s.what);
                            ui.end_row();
                        }
                    });
            }
        });
    // 拒绝原因画在表底下(表在 ScrollArea 里,画在里面会随滚动跑出视野)。
    if let Some(msg) = &draft.hotkey_error {
        ui.label(
            egui::RichText::new(msg)
                .size(11.0)
                .color(theme::c32(t.danger_text)),
        );
    }
}
```

- [ ] **Step 4: `take_settings_draft` 写回**

`crates/mullion-app/src/app.rs` `take_settings_draft` 里 `self.settings.show_hidden_files = d.show_hidden_files;` 之后加:

```rust
            // F294:只写与默认不同的(稀疏);改回默认的那条随之从文件里消失。
            self.settings.hotkeys = crate::hotkeys::overrides(&d.hotkeys);
```

同文件那条源码切片守护(`take_settings_draft` 的 body 断言,约 L23329 起)加一句:

```rust
        assert!(
            body.contains("self.settings.hotkeys = crate::hotkeys::overrides(&d.hotkeys);"),
            "热键没被搬进 settings —— 设置里改了键,点确定不生效"
        );
```

- [ ] **Step 5: 跑**

Run: `cargo test -p mullion-app --lib ui::settings app::tests 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: ok。`FRAMES = 12` 若不够(捕获按钮多了一层 `horizontal`),症状是 `click` 返回 `None`——先按 `settings.rs` 里 `FRAMES` 的注释逐帧量,不要盲目加大。

### Task 11: `app.rs`:捕获态拦截,`window_event` 第一道

**Files:**
- Modify: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: 写源码切片测试**

放在 `bound_hotkeys_are_swallowed_before_the_input_routing` 之后:

```rust
    /// F294:捕获态拦截必须是 `window_event` 的**第一道**检查 —— 排在
    /// `annotate_event` 之前。不然用户想绑 `Ctrl+Shift+F`,按下去先被标注模式
    /// 截走,看到的是「按了没反应」而不是「已被占用」。
    ///
    /// 自证会变红:把 `self.hotkey_capture_event(&event)` 挪到 `annotate_event` 之后。
    #[test]
    fn hotkey_capture_is_the_first_thing_window_event_checks() {
        let body = strip_comments(body_of(prod_src(), "fn window_event("));
        let capture = body
            .find("self.hotkey_capture_event(&event)")
            .expect("window_event 里没接 hotkey_capture_event");
        let annotate = body.find("self.annotate_event(&event)").expect("找不到 annotate_event");
        assert!(capture < annotate, "捕获拦截排在标注模式之后 —— Ctrl+Shift+F 永远捕获不到");
    }

    /// F294:捕获态里 Esc 取消、修饰键单独按下忽略、其余交给 `capture` 裁决。
    ///
    /// 自证会变红:把 `hotkey_capture_event` 里 `is_modifier` 那个分支删掉。
    #[test]
    fn hotkey_capture_handles_escape_and_modifier_keys() {
        let body = strip_comments(body_of(prod_src(), "fn hotkey_capture_event("));
        assert!(body.contains("NamedKey::Escape"), "Esc 不能退出捕获");
        assert!(body.contains("hotkeys::is_modifier("), "修饰键单独按下会被当成一次输入");
        assert!(body.contains("draft.capture("), "没把 chord 交给草稿裁决");
        assert!(body.contains("chord_of_event("), "捕获的键口径与匹配不同源");
    }
```

- [ ] **Step 2: 实现**

放在 `bound_hotkey_event` 后面:

```rust
    /// F294:设置弹窗里某一行正在等新组合键。**`window_event` 的第一道检查**
    /// (排在 `annotate_event` 之前):不然 `Ctrl+Shift+F` 之类会先被别的拦截
    /// 截走,用户看到的是「按了没反应」而不是「已被占用」。捕获期间每一个
    /// `KeyboardInput` 一律吞掉(弹窗本来就是模态,键盘不会去别处)。
    fn hotkey_capture_event(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::KeyboardInput { event: ke, .. } = event else {
            return false;
        };
        if !self.ui.settings_open {
            return false;
        }
        let mods = self.mods;
        let Some(draft) = self.ui.settings_draft.as_mut() else {
            return false;
        };
        if draft.capturing.is_none() {
            return false;
        }
        if ke.state != ElementState::Pressed {
            return true;
        }
        if matches!(
            ke.logical_key,
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
        ) {
            draft.capturing = None;
            draft.hotkey_error = None;
        } else if crate::hotkeys::is_modifier(&ke.logical_key) {
            // 用户还在按组合(先按下了 Ctrl),等下一个键。
        } else if let Some(chord) = crate::hotkeys::chord_of_event(ke, mods) {
            draft.capture(chord);
        } else {
            draft.hotkey_error = Some(crate::ui::settings::UNBINDABLE_MSG.to_string());
        }
        self.request_ui_redraw();
        true
    }
```

`window_event` 开头,`diag::mark(diag::Stage::WindowEvent);` 之后、`annotate_event` 那段之前加:

```rust
        // F294:设置弹窗正在捕获新组合键时,这一下归它 —— 必须排在**所有**
        // 拦截之前(理由见 `hotkey_capture_event` 的文档)。
        if self.hotkey_capture_event(&event) {
            return;
        }
```

- [ ] **Step 3: 跑**

Run: `cargo test -p mullion-app --lib app::tests::hotkey_capture 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: 2 passed。

### Task 12: spec.md 登记 + 跑绿 + 提交

**Files:**
- Modify: `spec.md`(F292 那一行之后)

- [ ] **Step 1: 加三行**

在 `| F292 | …` 那一行之后插入:

```markdown
| F293 | **启动页会话列显示图标** | P3 | 已实现（v0.1.118）。图标槽几何**复用**项目列的常量（`project_row::ICON_X / ICON_SIDE / TEXT_X`，开成 `pub(crate)`），三列并排名字左沿必须对齐；槽位恒定，没图标留空不画占位；图标与底色走 `AppearanceCache` + `should_paint(ListItem)`，与会话管理器左栏同源。守护：`launcher::tests::a_session_row_paints_the_icon_of_its_session`（Mesh 真纹理 + 有面积）、`the_session_name_starts_at_the_same_x_with_or_without_an_icon`（两条断言：同 x **且** 不压图标——后者引入图标包围盒作第三方参照物）。否掉的备选：首字母圆点回退（会话管理器就是留空，两处不该两副面孔）。 |
| F294 | **本地热键可配置**：`app.rs` 的 5 个本地热键改成 7 条动作（Ctrl+Tab / Ctrl+Shift+Tab / Ctrl+W / Ctrl+Shift+B / F6 / Ctrl+Shift+N / 抽屉），抽屉默认改 `` Ctrl+Shift+` `` | P2 | 已实现（v0.1.118）。`Chord`（四修饰键 + `KeyName` 枚举）在 store，TOML 里一行字符串 `"ctrl+shift+`"`；`Settings.hotkeys` **稀疏**（只写非默认，键是动作名，store 不认识动作）；运行时**不建影子状态**，每次按键 `hotkeys::resolve(&settings, ..)`。键口径 `key_without_modifiers()`（`logical_key` 对 `` Ctrl+Shift+` `` 给 `~`）。`window_event` 里一个 `bound_hotkey_event` 替代四个函数，逐动作的门（modal / F6 面板在场 / NewProject 焦点让位）原样保留；`tabs::hotkey` 缩成 `digit_hotkey` 只剩 Ctrl+1…9（不缩的话用户改掉 Ctrl+W 它照样关标签）。合法绑定：带 Ctrl/Alt/Super，或 F1…F12 可裸；撞键与另 6 条 + 一览表硬名单比（排除会话管理器节），撞了**捕获那一刻就拒**、不进草稿、报出占用者；唯一白名单 Ctrl+Shift+N（项目 ↔ 文件面板新建文件夹，靠焦点分辨）。捕获态拦截是 `window_event` **第一道**。旧 `` Ctrl+` `` 彻底不再是抽屉键。守护：`hotkeys::tests`（默认表 / 旧键释放 / Ctrl+B 留给 tmux / 合法性 / 撞键 / 白名单 / 键口径源码切片）、`app.rs::bound_hotkeys_are_swallowed_before_the_input_routing`、`hotkey_capture_is_the_first_thing_window_event_checks`、`settings::tests::capture_writes_only_a_vetted_chord`、`mullion_store::settings::tests::hotkey_overrides_survive_a_round_trip_and_stay_sparse`。人工验：Windows 装了多套键盘布局时 `Ctrl+Shift` 是系统的切布局键，若被系统吃掉就在设置里改键。 |
| F295 | **快捷键一览重排**：结构化数据、分节、Esc 只出现一次、补齐全部现实快捷键 | P3 | 已实现（v0.1.118）。每行的键从字符串改成 `Keys`（`Chord` / `Chords` / `CtrlDigits` / `Bound(Action)` / `Text`），显示文本由结构生成，撞键按结构比；分八节（通用 / 标签 / 终端 / 文件面板 / 会话管理器 / 标注模式 / 项目 / 命令抽屉），节内两列。**只合并 Esc**进「通用」节，文案按事实写（复核发现「Esc 关掉当前弹窗」是假的：设置 / 项目管理 / 分组管理 / 导入 / 迁移包 / 解锁 / 标签属性 / 编辑器 / 传输面板都不认 Esc）；其余跨节重名各留各行。补齐文件面板 Enter / Backspace / F5 / Tab / ↑↓ / Delete / Shift+Delete / F2 / 字母定位、F6、命令抽屉、搜索条 Esc。守护：`shortcuts::tests::escape_is_listed_exactly_once`、`no_two_rows_claim_the_same_chord`（结构比）、`every_module_that_has_shortcuts_is_represented`（节序）、`settings::tests::the_shortcut_table_is_grouped_into_sections`。 |
```

- [ ] **Step 2: 跑绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "FAILED|panicked" /tmp/test.log; grep -c "test result: ok" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

`tests/glyph_whitelist.rs` 若因 `hotkeys.rs` / `shortcuts.rs` 里的 `…`、`「」`、`←` 报红:`…`/`「」` 早已登记,`←` 在 Task 3 登记过;别的符号一律换成已登记的。

- [ ] **Step 3: 提交**

```bash
git add crates/mullion-store/src/settings.rs crates/mullion-app/src/hotkeys.rs crates/mullion-app/src/lib.rs crates/mullion-app/src/ui/shortcuts.rs crates/mullion-app/src/ui/settings.rs crates/mullion-app/src/shell/tabs.rs crates/mullion-app/src/app.rs spec.md
git commit -m "feat(app): 本地热键可在设置里改,抽屉默认 Ctrl+Shift+\` (F294)

- store:Settings.hotkeys 稀疏覆盖(动作名 → Chord,只写非默认)
- app::hotkeys:7 条动作 / 默认键 / resolve(无影子状态)/ key_without_modifiers 口径 / 合法性 / 撞键
- window_event:bound_hotkey_event 替代 files/focus/project/drawer 四个函数,门原样保留;
  hotkey_capture_event 排第一道;tabs::hotkey 缩成 digit_hotkey
- 设置弹窗:可配行就地点按捕获,拒绝不进草稿并报占用者,改过的行有「恢复默认」
- 旧 Ctrl+\` 不再是抽屉键

守护:hotkeys::tests 全组;app::tests::bound_hotkeys_are_swallowed_before_the_input_routing /
hotkey_capture_is_the_first_thing_window_event_checks / f6_is_gated_on_the_panel_actually_being_visible /
the_project_hotkey_yields_ctrl_shift_n_back_to_the_files_panel(T8);
settings::tests::capture_writes_only_a_vetted_chord;store settings::tests 两条 round-trip/graft"
```

### Task 13: 发版 v0.1.118

- [ ] **Step 1: 加载 `release-windows` skill,按它一步步做**(升 patch 0.1.117 → 0.1.118 → 跑绿 → 交叉编译 + objdump → 签名 → GitHub Release)。**别凭记忆做**。

- [ ] **Step 2: Release notes 附人工验收清单**(照抄 spec 的「人工验收清单」一节),尤其:
  - `` Ctrl+Shift+` `` 开抽屉、`` Ctrl+` `` 不再开;
  - Windows 多布局下 `Ctrl+Shift` 是否被系统切布局吃掉(若是:在设置里改键,验证改键生效);
  - 改键 → 确定 → 重启仍生效;`settings.toml` 只多 `[hotkeys]` 一行;恢复默认后那行消失;
  - 撞键 / 裸键 / Esc 三种拒绝路径的提示;
  - 改掉 Ctrl+W 后它发给远端;
  - 启动页会话图标与项目列对齐,竖排同样。

---

## 自审记录

- **Spec 覆盖**:D1–D4 → Task 1;D5–D13 → Task 2–5;D14–D27 → Task 6–11;D28–D30 → 提交顺序 / Task 12–13。
- **类型一致性**:`Keys::display(&BTreeMap<Action, Chord>)` 在 Task 7 Step 2 改签名后,Task 7 Step 3(settings 临时)、Task 10 Step 3、shortcuts 测试三处都按新签名;`SettingsDraft::capture(Chord)`、`hotkeys::vet(&BTreeMap<Action,Chord>, Action, Chord) -> Result<(), String>`、`shortcuts::occupant(&Chord, Action) -> Option<&'static Shortcut>`、`shortcuts::what_of(Action) -> &'static str`、`tabs::digit_hotkey(Key, Mods, bool) -> Option<usize>` 各处一致。
- **已知的空白**:`app.rs` 里除本计划列出的那些之外,若还有别的源码切片测试碰到四个被删函数,执行者按 Task 9 Step 5 的思路改,禁止加回旧函数。
