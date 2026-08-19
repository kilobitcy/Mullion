# 文件面板列宽可调 + 横向滚动 + 截断 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 文件面板的五列可拖拽调宽(F135)、放不下时出水平滚动条而不是收起列(F136)、每个单元格按自己的列宽截断不再互相重叠(F137)。

**Architecture:** 把「名称列吃剩余宽度 + 窄了从右向左收起可选列」的被动列模型,换成「五列各自定宽 → 累加出内容总宽 → 超出视口就横向滚」。列头改到 `ScrollArea` **之后**绘制,好拿到本帧真实的 `offset.x` 跟随滚动;文字截断抽成不依赖 egui 的纯函数(测宽用注入闭包)。全部改动收在 `crates/mullion-app/src/ui/files_panel.rs` 一个文件里,外加 `ui/mod.rs` 一个字段和一处调用。

**Tech Stack:** Rust / egui 0.30(`ScrollArea::both`、`UiBuilder::max_rect`、`Painter::layout_no_wrap`)。

**设计依据:** `docs/superpowers/specs/2026-08-19-files-panel-resizable-columns-design.md`。冲突时以设计文档为准。

**开工前必读:** `CLAUDE.md` 的领域陷阱表(T1/T3/T7/T8 与本片无关,但 `docs/gui-render-gotchas.md` 里 egui 那几条相关)、`files_panel.rs` 顶部 `ICON_LEFT_PAD` / `name_w` 那几段文档注释(它们记录了这套坐标是怎么踩出来的)。

---

## 文件结构

| 文件 | 责任 | 本片改动 |
|---|---|---|
| `crates/mullion-app/src/ui/files_panel.rs` | 文件面板全部绘制与交互 | 列模型、横向滚动、截断、列宽拖拽 —— 本片 95% 的改动 |
| `crates/mullion-app/src/ui/mod.rs` | `UiState` 与 `build_ui` 接线 | 加 `files_cols` 字段;`files_panel::content` 调用点多传一个参数 |
| `spec.md` | 需求编号表 | 补 F135/F136/F137 三条 |

不新建文件。`files_panel.rs` 已有 3167 行,但本片是**替换**(删掉 6 个布局函数、加 4 个),净增约 200 行,不到需要拆文件的程度;拆文件会把「列头与行体共用同一份坐标」这条不变量分到两个文件里,反而更容易失守。

---

### Task 1: 列模型换成定宽五列

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs:208-357`(常量与六个布局函数)
- Modify: `crates/mullion-app/src/ui/files_panel.rs:721-779`(`header`)
- Modify: `crates/mullion-app/src/ui/files_panel.rs:781-899`(`row`)
- Test: 同文件 `mod tests`

本任务**不接线**:`show()` 内部先 `let cols = ColWidths::default();` 自己造一份,Task 2 再换成外部传入。这样本任务自成一个可编译、可跑测试的提交。

- [ ] **Step 1: 写失败的测试**

在 `mod tests` 里,把现有的 `columns_are_dropped_from_the_right_as_the_panel_gets_narrower`(约 2716 行)和 `the_header_and_row_size_column_start_at_the_same_x_across_widths`(约 2648 行)整条**删掉**,换成下面三条:

```rust
    /// F136/D1:五列**恒定存在**,不再随宽度收起。窄栏下放不下是靠横向
    /// 滚动条解决的,不是靠把列藏起来 —— 藏起来的那一版会让「属主」这类
    /// 信息在默认侧栏宽下永远看不到。
    ///
    /// 自证会变红:在 `col_lefts()` 里按总宽过滤掉最后几列。
    #[test]
    fn all_five_columns_are_present_no_matter_how_narrow_the_panel_is() {
        let cols = ColWidths::default();
        let keys: Vec<SortKey> = col_lefts(&cols).iter().map(|&(_, k, _, _)| k).collect();
        assert_eq!(
            keys,
            vec![
                SortKey::Name,
                SortKey::Size,
                SortKey::Mtime,
                SortKey::Perm,
                SortKey::Owner
            ],
            "五列的顺序或数量变了 —— 列布局是唯一真值来源,顺序即收起顺序的时代已经结束"
        );
    }

    /// 列坐标必须是**首尾相接、无缝无叠**的累加,而且总宽等于各列之和 ——
    /// `content_w()` 与 `col_lefts()` 一旦对不上,横向滚动的可滚范围就会
    /// 比实际内容短一截,最右边那列永远滚不到。
    ///
    /// 自证会变红:把 `content_w()` 改成漏加 `owner`;或在 `col_lefts()`
    /// 的累加里插一个间距。
    #[test]
    fn column_lefts_are_contiguous_and_sum_to_the_content_width() {
        let cols = ColWidths {
            name: 111.0,
            size: 22.0,
            mtime: 33.0,
            perm: 44.0,
            owner: 55.0,
        };
        let lay = col_lefts(&cols);
        assert_eq!(lay[0].2, 0.0, "第一列必须从 0 起算(相对行左边界)");
        for w in lay.windows(2) {
            let (label, _, left, width) = w[0];
            let (next_label, _, next_left, _) = w[1];
            assert!(
                (left + width - next_left).abs() < 0.01,
                "「{label}」右边界 {} 与「{next_label}」左边界 {next_left} 不相接",
                left + width
            );
        }
        let last = lay[4];
        assert!(
            (last.2 + last.3 - content_w(&cols)).abs() < 0.01,
            "最后一列右边界 {} 与 content_w() {} 对不上",
            last.2 + last.3,
            content_w(&cols)
        );
    }

    /// D3 的替代守护:列头与行体**画在同一组 x 上**。原来两边各自累加,
    /// 靠一条几何测试守住不许错位;现在坐标同源,几何断言会退化成重言式
    /// —— 所以改成从**真实渲染结果**里取两串文字来比:列头的「大小」标题
    /// 与行里的大小数值必须落在同一列的区间内。
    ///
    /// 自证会变红:给 `header_at()` 的列 rect 加一个 20px 的偏移。
    #[test]
    fn the_size_header_and_the_size_value_land_in_the_same_column() {
        let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
        state.entries = vec![entry(b"a.txt", EntryKind::File)];
        state.load = Load::Ready;

        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut cols = ColWidths::default();
        let mut out = None;
        for _ in 0..2 {
            out = Some(ctx.run(raw(None), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        "远端",
                        1,
                        PanelColumn::Remote,
                        &mut state,
                        false,
                        &[],
                        0,
                        &mut cols,
                    );
                });
            }));
        }
        let out = out.expect("跑了两帧");
        // `entry()` 的 size 是 1024 → `human_size` 印成 "1.0 KB"。
        let head = find_text_pos(&out.shapes, "大小").expect("列头该画出「大小」");
        let value = find_text_pos(&out.shapes, "1.0 KB").expect("行里该画出大小数值");
        let lay = col_lefts(&cols);
        let (_, _, size_left, size_w) = lay[1];
        // 两者都必须落在「大小」列的横向区间内(容差给半个列宽,只要不是
        // 画到隔壁列去就算对齐 —— 一个左对齐标题、一个右对齐数值,x 本来
        // 就不该相等)。
        let span = (size_left, size_left + size_w);
        for (what, x) in [("列头「大小」", head.x), ("行内大小数值", value.x)] {
            assert!(
                x >= span.0 - size_w && x <= span.1 + size_w,
                "{what} 的 x={x} 落在「大小」列区间 {span:?} 之外 —— 列头与行体的列坐标分家了"
            );
        }
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib files_panel 2>&1 | tail -20
```
预期:编译失败,`cannot find type ColWidths`/`cannot find function col_lefts`。

- [ ] **Step 3: 换掉列模型**

把 `files_panel.rs:248-357` 之间的 `OPTIONAL_COLS`、`visible_col_count`、`name_w`、`header_name_col_w`、`header_col_lefts`、`row_size_col_left`、`row_col_lefts` **整段删掉**,换成:

```rust
/// 五列的当前宽度(point)。**名称列的宽度含图标格** —— 沿用列头
/// 「图标 + 名称 = 一个合并区域」的既有语义,不额外记一份图标宽。
///
/// 放 `ui::UiState`(全局一份,远端/本地/所有标签共用),**不落盘**:
/// 拖列宽是个随手调整,不值得为它动 store schema(设计 D2)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColWidths {
    pub name: f32,
    pub size: f32,
    pub mtime: f32,
    pub perm: f32,
    pub owner: f32,
}

impl Default for ColWidths {
    /// 名称列 220:比旧模型在默认侧栏(360px)下算出来的 130 宽不少,
    /// 代价是一打开就是横向滚动状态 —— 这是设计 D1 明确接受的取舍
    /// (五列恒在 > 一眼看全)。不满意改这一行就行。
    fn default() -> Self {
        Self {
            name: 220.0,
            size: W_SIZE,
            mtime: W_MTIME,
            perm: W_PERM,
            owner: W_OWNER,
        }
    }
}

/// 列序号 → 最小宽度。名称列要放得下图标格 + 几个字;其余列至少要能
/// 放下被截断后的标题(一个字 + 省略号)。
fn col_min(i: usize) -> f32 {
    if i == 0 {
        80.0
    } else {
        48.0
    }
}

/// 列宽上限。拖到几千 px 不会崩,但滚动条会退化成一条几乎抓不住的细线。
const COL_MAX: f32 = 800.0;

/// 列序号 → 可变引用。拖拽热区按序号改宽度,`match` 只写这一份 ——
/// 散成五处 `if i == 0 { .. } else if ..` 的话,加一列必漏一处。
fn col_w_mut(c: &mut ColWidths, i: usize) -> &mut f32 {
    match i {
        0 => &mut c.name,
        1 => &mut c.size,
        2 => &mut c.mtime,
        3 => &mut c.perm,
        _ => &mut c.owner,
    }
}

/// **列布局的唯一真值来源**:`(标签, SortKey, 左边界, 宽度)` ×5,
/// 左边界从 0 起算(相对行/列头的左边界)。
///
/// 列头(`header_at`)和行体(`row`)调的是同一份 —— 旧模型里两边各自
/// 累加、靠一条对齐测试守着不许分家,现在坐标同源,分家在物理上不可能
/// 发生。**不许在别处再写一遍这个累加。**
fn col_lefts(c: &ColWidths) -> [(&'static str, SortKey, f32, f32); 5] {
    let mut out = [("", SortKey::Name, 0.0, 0.0); 5];
    let mut x = 0.0;
    for (i, (label, key, w)) in [
        ("名称", SortKey::Name, c.name),
        ("大小", SortKey::Size, c.size),
        ("修改时间", SortKey::Mtime, c.mtime),
        ("权限", SortKey::Perm, c.perm),
        ("属主", SortKey::Owner, c.owner),
    ]
    .into_iter()
    .enumerate()
    {
        out[i] = (label, key, x, w);
        x += w;
    }
    out
}

/// 内容总宽 = 各列之和。视口比它窄就出横向滚动条(F136)。
fn content_w(c: &ColWidths) -> f32 {
    c.name + c.size + c.mtime + c.perm + c.owner
}
```

`W_SIZE`/`W_MTIME`/`W_PERM`/`W_OWNER` 四个常量**保留**(现在是 `Default` 的取值来源),把它们上面那段「定宽列一旦跟着内容浮动就会横着抖」的注释改成:

```rust
/// 列宽的默认值。用户可以拖(F135),拖出来的值放在 `ColWidths` 里;
/// 这几个常量只负责给出「第一次打开时长什么样」。
```

- [ ] **Step 4: 改 `header()`**

把 `header()`(约 721 行)整个替换成下面两个函数。注意列头从「`ui.horizontal` 里逐列 `allocate_exact_size`」改成「在一条已知的横带里按绝对坐标画」—— Task 3 要给它减一个滚动偏移,顺序累加那套写法接不上。

```rust
/// 列头。占一条 `ROW_H` 高的横带,在里面按 `col_lefts()` 的坐标画。
fn header(ui: &mut Ui, t: &Theme, id: &str, state: &mut PaneState, cols: &ColWidths) {
    let (band, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ROW_H),
        egui::Sense::hover(),
    );
    header_at(ui, t, id, state, cols, band, 0.0);
}

/// 把列头画在 `band` 里,整体左移 `offset_x`。
///
/// `offset_x` 是横向滚动偏移(F136,Task 3 接上);Task 1 阶段恒 0。
/// 拆出这个函数是因为**列头必须在 `ScrollArea` 之后才画得出正确的偏移**
/// (见设计 §③),而占位又必须在它之前 —— 占位与绘制天然要分成两处。
fn header_at(
    ui: &mut Ui,
    t: &Theme,
    id: &str,
    state: &mut PaneState,
    cols: &ColWidths,
    band: egui::Rect,
    offset_x: f32,
) {
    annotate::mark(ui.ctx(), format!("文件面板/{id}/列头"), band);
    let mut hit = None;
    for (i, (label, key, left, w)) in col_lefts(cols).into_iter().enumerate() {
        let rect = egui::Rect::from_min_size(
            egui::pos2(band.left() + left - offset_x, band.top()),
            egui::vec2(w, ROW_H),
        );
        // 逐列登记:整行那一处标不了「往名称列点」这种精确目标
        // (F100 标注模式与点击测试都靠它)。
        annotate::mark(ui.ctx(), format!("文件面板/{id}/列头/{label}"), rect);
        // id 必须显式给:不再走 `allocate_exact_size` 的自动分配,而两栏
        // 的列头在同一个 `Context` 里同名,不掺 `id`(远端/本地)和列序号
        // 会撞成同一个部件。
        let resp = ui.interact(
            rect,
            ui.id().with(("files-col-head", id, i)),
            egui::Sense::click(),
        );
        let mark = if state.sort_key == key {
            match state.sort_dir {
                crate::files::SortDir::Asc => " ▲",
                crate::files::SortDir::Desc => " ▼",
            }
        } else {
            ""
        };
        // 裁到横带内:列宽之和超过视口时,右边那几列的标题不能画到
        // 隔壁栏去(同 `content()` 里两栏各自 `set_clip_rect` 的理由)。
        ui.painter().with_clip_rect(band).text(
            rect.left_center() + egui::vec2(crate::ui::metrics::SP_XS, 0.0),
            egui::Align2::LEFT_CENTER,
            format!("{label}{mark}"),
            egui::FontId::proportional(11.0),
            theme::c32(t.fg_muted),
        );
        if resp.clicked() {
            hit = Some(key);
        }
    }
    if let Some(k) = hit {
        state.click_header(k);
    }
}
```

- [ ] **Step 5: 改 `row()`**

`row()` 签名加 `cols: &ColWidths`,行宽改成内容总宽,四列坐标改从 `col_lefts` 取:

```rust
fn row(
    ui: &mut Ui,
    t: &Theme,
    e: &mullion_ssh::sftp::Entry,
    column: PanelColumn,
    selected: bool,
    cols: &ColWidths,
) -> egui::Response {
    // 行宽取「内容总宽」与「视口宽」的**较大者**:
    // - 总宽 > 视口 → 行要撑满内容宽,否则右边几列画在行的交互 rect 之外;
    // - 总宽 < 视口 → 行要铺满视口,否则选中高亮只有半行长,右边那片空白
    //   点不中行、也接不住 drop(`a_row_in_the_tab_host_can_actually_be_clicked`
    //   与 `dropping_on_the_blank_part_of_a_column_targets_its_current_directory`
    //   两条现有测试守着这两件事)。
    let w = content_w(cols).max(ui.available_width());
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), egui::Sense::click_and_drag());
```

函数体里 `let cols = row_col_lefts(rect);` 那一行(约 848 行)改成:

```rust
    let lay = col_lefts(cols);
```

随后四处 `if let Some(&(_, _, x, w)) = cols.first()/get(1)/get(2)/get(3)` 改成直接取下标(五列恒在,没有 `Option` 了),并且每列的 x 要加上 `rect.left()`:

```rust
    // 大小(右对齐)
    let (_, _, size_left, size_w) = lay[1];
    let size_text = if e.kind == EntryKind::Dir {
        String::new()
    } else {
        human_size(e.size)
    };
    p.text(
        egui::pos2(rect.left() + size_left + size_w - crate::ui::metrics::SP_XS, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        size_text,
        font.clone(),
        theme::c32(t.fg_mid),
    );
    // 修改时间(左对齐)
    let (_, _, mtime_left, _) = lay[2];
    p.text(
        egui::pos2(rect.left() + mtime_left + crate::ui::metrics::SP_S, rect.center().y),
        egui::Align2::LEFT_CENTER,
        mtime_text(e.mtime),
        font.clone(),
        theme::c32(t.fg_mid),
    );
    // 权限(右对齐)
    let (_, _, perm_left, perm_w) = lay[3];
    p.text(
        egui::pos2(rect.left() + perm_left + perm_w - crate::ui::metrics::SP_XS, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        perm_string(e.mode),
        font.clone(),
        theme::c32(t.fg_dim),
    );
    // 属主(右对齐)。**本地栏恒画 `—`**,判据见 `owner_text` 的文档注释。
    let (_, _, owner_left, owner_w) = lay[4];
    p.text(
        egui::pos2(rect.left() + owner_left + owner_w - crate::ui::metrics::SP_XS, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        owner_text(column, e.uid, e.gid),
        font,
        theme::c32(t.fg_dim),
    );
```

> 注意「大小」列原来是 `size_left + size_w` 不减 `SP_XS`(贴着列右边界),其余右对齐列减 `SP_XS`。这次统一都减 `SP_XS` —— 列宽可拖之后,贴边的那一列会紧挨着分隔线,视觉上像是溢出。

- [ ] **Step 6: `show()` 里造一份临时 `cols`**

在 `show()` 里 `header(ui, t, id, state);` 那一行(约 603 行)之前插入:

```rust
    // Task 2 会换成调用方传入(`ui::UiState::files_cols`)。
    let cols = ColWidths::default();
```
并把该行改成 `header(ui, t, id, state, &cols);`,把闭包里 `row(ui, t, e, column, selected.contains(&e.name))` 改成 `row(ui, t, e, column, selected.contains(&e.name), &cols)`。

**Step 1 的第三条测试**里 `show(...)` 多传了一个 `&mut cols` —— 那是 Task 2 之后的签名。Task 1 阶段先把那条测试的最后一个参数删掉(`..., &[], 0)`),Task 2 再加回去。

- [ ] **Step 7: 跑测试**

```bash
cargo test -p mullion-app --lib files_panel > /tmp/t1.log 2>&1; grep -nE "test result|FAILED|panicked|error\[" /tmp/t1.log | head -30
```
预期:新加的三条 PASS。**如果别的测试红了**,先看它红在哪 —— `a_huge_directory_is_rendered_with_show_rows_not_a_full_scan` 这类结构测试不该受影响;行为测试(点击/拖拽/书签)如果红了,说明列坐标改动破坏了交互区域,**不许改测试判据**,回去看 `row()` 的 rect 宽度是不是算错了。

- [ ] **Step 8: 提交**

```bash
git add crates/mullion-app/src/ui/files_panel.rs
git commit -m "refactor(app): 文件面板列模型换成定宽五列,删掉自动收起 (F135)

列头与行体改用同一份 col_lefts() 坐标,visible_col_count()/name_w() 一系
六个函数删除。跑了 all_five_columns_are_present_no_matter_how_narrow_the_panel_is
与 the_size_header_and_the_size_value_land_in_the_same_column。"
```

---

### Task 2: 把列宽接到 `UiState`

**Files:**
- Modify: `crates/mullion-app/src/ui/mod.rs:302` 附近(`UiState` 字段)
- Modify: `crates/mullion-app/src/ui/mod.rs:817-824`(`files_panel::content` 调用)
- Modify: `crates/mullion-app/src/ui/files_panel.rs`(`show`/`sidebar`/`content` 签名 + 全部调用点)

- [ ] **Step 1: 写失败的测试**

在 `files_panel.rs` 的 `mod tests` 里加:

```rust
    /// D2:列宽是**外部状态**,不是每帧现造的 —— 造在 `show()` 内部的话
    /// 拖出来的宽度下一帧就被冲掉,拖拽功能会以「拖得动、松手弹回」的形式
    /// 安静失效。判据:同一份 `ColWidths` 改了之后,画出来的列位置跟着变。
    ///
    /// 自证会变红:把 `show()` 里的 `cols` 参数忽略掉、改回内部
    /// `ColWidths::default()`。
    #[test]
    fn the_column_widths_come_from_the_caller_not_from_a_fresh_default() {
        let t = crate::theme::MULLION_DARK;
        let render = |cols: &mut ColWidths| {
            let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
            state.entries = vec![entry(b"a.txt", EntryKind::File)];
            state.load = Load::Ready;
            let ctx = egui::Context::default();
            let mut out = None;
            for _ in 0..2 {
                out = Some(ctx.run(raw(None), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        show(
                            ui,
                            &t,
                            "远端",
                            1,
                            PanelColumn::Remote,
                            &mut state,
                            false,
                            &[],
                            0,
                            cols,
                        );
                    });
                }));
            }
            find_text_pos(&out.expect("跑了两帧").shapes, "1.0 KB")
                .expect("行里该画出大小数值")
                .x
        };

        let mut narrow = ColWidths::default();
        let mut wide = ColWidths {
            name: ColWidths::default().name + 120.0,
            ..ColWidths::default()
        };
        let x_narrow = render(&mut narrow);
        let x_wide = render(&mut wide);
        assert!(
            (x_wide - x_narrow - 120.0).abs() < 1.0,
            "名称列加宽 120 之后,大小数值应该右移 120(实际 {x_narrow} → {x_wide})\
             —— 列宽没有从调用方读进来"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib the_column_widths_come_from_the_caller 2>&1 | tail -10
```
预期:编译失败(`show` 只接 9 个参数)。

- [ ] **Step 3: 加字段**

`crates/mullion-app/src/ui/mod.rs`,在 `pub files_sidebar_w: f32,` 之后插入:

```rust
    /// F135:文件面板五列的宽度。**全局一份**——远端栏/本地栏/所有标签
    /// 共用一套(跟 `files_sidebar_w` 同款:用户拖一次就该处处生效)。
    ///
    /// **不落盘**(设计 D2):关窗口就回到默认宽度。`UiState` 走
    /// `#[derive(Default)]`,而 `ColWidths` 自己实现了 `Default`,所以
    /// 这里不需要 `files_sidebar_w` 那种 `0.0` 哨兵。
    pub files_cols: files_panel::ColWidths,
```

- [ ] **Step 4: 改三个签名**

`files_panel.rs`:
- `pub fn show(...)` 末尾(`drop_in: usize` 之后)加 `cols: &mut ColWidths,`,并删掉 Task 1 里那句临时的 `let cols = ColWidths::default();`,把 `header(ui, t, id, state, &cols)` 改成 `header(ui, t, id, state, cols)`、`row(..., &cols)` 改成 `row(..., cols)`。
- `pub fn sidebar(...)` 里两处 `show(...)` 调用末尾各加一个 `&mut ui_state.files_cols,`。
- `pub fn content(...)` 末尾(`drop_in: usize` 之后)加 `cols: &mut ColWidths,`,里面两处 `show(...)` 调用末尾各加 `cols,`。

`ui/mod.rs` 的调用点(约 817 行)改成:

```rust
        let (r, l) = files_panel::content(
            ctx,
            t,
            files_generation,
            frame.files_focused,
            files,
            hovering,
            &mut ui_state.files_cols,
        );
```

> 传 `&mut ui_state.files_cols` 而不是整个 `&mut UiState`:`content` 不需要 `UiState` 的别的东西,借得越窄,以后在同一帧里再借 `ui_state` 别的字段就越不容易撞上借用检查。

- [ ] **Step 5: 改全部测试调用点**

`files_panel.rs` 的 `mod tests` 里,12 处 `content(ctx, &t, ...)` 各自在前面加一份局部列宽、末尾多传一个参数。例如(约 1411 行):

```rust
                content(ctx, &t, 1, false, &mut frame, 0, &mut cols);
```
每个测试函数开头加 `let mut cols = ColWidths::default();`(闭包里用到的话,声明要在闭包**之前**)。4 处直接调 `show(...)` 的同理。

```bash
# 找出全部调用点
grep -n "content(ctx\|show(ui, &t\|= show(" crates/mullion-app/src/ui/files_panel.rs
```

- [ ] **Step 6: 跑测试**

```bash
cargo test -p mullion-app --lib files_panel > /tmp/t2.log 2>&1; grep -nE "test result|FAILED|panicked|error\[" /tmp/t2.log | head -30
cargo test -p mullion-app --lib ui:: > /tmp/t2b.log 2>&1; grep -nE "test result|FAILED" /tmp/t2b.log
```
预期:全绿。

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src/ui/files_panel.rs crates/mullion-app/src/ui/mod.rs
git commit -m "feat(app): 文件面板列宽提到 UiState,两栏与所有标签共用一套 (F135)

跑了 the_column_widths_come_from_the_caller_not_from_a_fresh_default。"
```

---

### Task 3: 横向滚动 + 列头跟随偏移

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs`(`show()` 的滚动区与列头顺序)
- Test: 同文件 `mod tests`

- [ ] **Step 1: 写失败的测试**

```rust
    /// F136:内容比视口宽时,横着滚 —— 而且**列头要跟着一起滚**。
    /// 列头不跟的话,滚过去之后标题和数据完全对不上,比不能滚更糟。
    ///
    /// 判据是「位移量相同」而不是「x 相等」:列头标题左对齐、行内数值
    /// 右对齐,静态 x 本来就不相等。
    ///
    /// 自证会变红(两条各自):
    /// 1. 把 `header_at()` 里的 `- offset_x` 去掉(列头不跟随);
    /// 2. 把 `ScrollArea::both()` 改回 `vertical()`(根本滚不动,前置断言先红)。
    #[test]
    fn horizontal_scroll_moves_the_header_and_the_rows_by_the_same_amount() {
        let t = crate::theme::MULLION_DARK;
        let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
        state.entries = vec![entry(b"a.txt", EntryKind::File)];
        state.load = Load::Ready;
        // 视口给得比内容总宽窄一大截,逼出横向滚动。
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 400.0));
        let mut cols = ColWidths::default();
        let ctx = egui::Context::default();
        let mut render = |input: egui::RawInput, state: &mut PaneState, cols: &mut ColWidths| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        "远端",
                        1,
                        PanelColumn::Remote,
                        state,
                        false,
                        &[],
                        0,
                        cols,
                    );
                });
            })
        };
        let base = egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        };
        let _ = render(base.clone(), &mut state, &mut cols);
        let before = render(base.clone(), &mut state, &mut cols);
        let head_before = find_text_pos(&before.shapes, "大小").expect("该画出列头「大小」").x;
        let value_before = find_text_pos(&before.shapes, "1.0 KB").expect("该画出大小数值").x;

        // 灌一股**水平**滚轮(Shift 不需要:MouseWheel 的 delta.x 就是横向)。
        let scroll = egui::RawInput {
            screen_rect: Some(screen),
            events: vec![
                egui::Event::PointerMoved(egui::pos2(150.0, 200.0)),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(-120.0, 0.0),
                    modifiers: egui::Modifiers::default(),
                },
            ],
            ..Default::default()
        };
        let _ = render(scroll, &mut state, &mut cols);
        // 平滑滚动有插值,多跑两帧让偏移稳定。
        let _ = render(base.clone(), &mut state, &mut cols);
        let after = render(base, &mut state, &mut cols);
        let head_after = find_text_pos(&after.shapes, "大小").expect("滚动后该仍画出列头「大小」").x;
        let value_after = find_text_pos(&after.shapes, "1.0 KB").expect("滚动后该仍画出大小数值").x;

        let d_value = value_after - value_before;
        assert!(
            d_value < -1.0,
            "灌了水平滚轮,行内数值却没左移(位移 {d_value})—— 横向滚动没生效,测试前提不成立"
        );
        let d_head = head_after - head_before;
        assert!(
            (d_head - d_value).abs() < 1.0,
            "列头位移 {d_head} 与行体位移 {d_value} 对不上 —— 列头没跟着横向滚动走"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib horizontal_scroll_moves 2>&1 | tail -20
```
预期:FAIL —— `灌了水平滚轮,行内数值却没左移`(此时还是 `ScrollArea::vertical()`)。

- [ ] **Step 3: 改 `show()` 的顺序**

把 `show()` 里 `header(ui, t, id, state, cols);`(约 603 行)那一行替换成占位:

```rust
    // F136:列头改在滚动区**之后**画 —— 先在这里占住一条横带,等滚动区跑完
    // 拿到本帧真实的 `offset.x` 再补画(设计 §③)。
    //
    // **不能反过来**(列头在前、用上一帧的 offset):那样拖滚动条时列头
    // 滞后一帧,肉眼可见地和数据错开。
    let (header_band, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ROW_H),
        egui::Sense::hover(),
    );
```

把 `egui::ScrollArea::vertical()` 改成 `egui::ScrollArea::both()`,并把 `.show_rows(...)` 的返回值接住:

```rust
    let total_w = content_w(cols);
    let scroll = egui::ScrollArea::both()
        .id_salt(scroll_id_salt(id, generation))
        .drag_to_scroll(false)
        .auto_shrink([false, false])
        .show_rows(ui, ROW_H, rows.len(), |ui, range| {
            // F136:**必须显式要这个宽度**。egui 0.30 的 `show_rows` 只
            // `set_height`,宽度全看内容自己撑 —— 空目录(range 为空)时一行
            // 都不画,`content_size.x` 恒等于视口宽,水平滚动条不出现,
            // 右边那几列的列头就永远滚不到。
            ui.set_min_width(total_w);
            for ix in range {
                // …原有循环体不动…
            }
        });
```

在 `show_rows` 之后、`if landing.is_none()` 之前插入列头补画:

```rust
    // F136:拿本帧真实的水平偏移,把列头补画在上面占住的横带里。
    ui.scope_builder(
        egui::UiBuilder::new().max_rect(header_band),
        |ui| {
            // **必须显式裁剪**:子 ui 的 clip_rect 默认原样继承父 painter,
            // 不裁的话列宽之和超过视口时,右边几列的标题会画到隔壁栏
            // (同 `content()` 里两栏那两处 `set_clip_rect`)。
            ui.set_clip_rect(header_band);
            header_at(ui, t, id, state, cols, header_band, scroll.state.offset.x);
        },
    );
```

> `header_at` 要 `&mut state`,而 `rows`(`Vec<&Entry>`)借着 `&state.entries`。NLL 下 `rows` 的最后一次使用在 `show_rows` 的闭包里,闭包结束后可变借用就合法了。**如果编译器报借用冲突**,说明闭包之后还有别处在用 `rows` —— 把那处挪到列头补画之前,不要给 `rows` 加 `.clone()` 绕过去(那是把 `Vec<&Entry>` 变成拷贝整棵目录树)。

`header()` 那个包装函数现在没人调了 —— **删掉它**,只留 `header_at`。

- [ ] **Step 4: 跑测试**

```bash
cargo test -p mullion-app --lib files_panel > /tmp/t3.log 2>&1; grep -nE "test result|FAILED|panicked|error\[" /tmp/t3.log | head -30
```
预期:新测试 PASS,其余全绿。

**若 `d_value` 断言仍红**(滚不动):检查 `ui.set_min_width(total_w)` 是不是漏了,或者视口宽度是不是没比 `content_w` 窄(默认列宽之和 = 220+78+132+86+92 = 608,视口给 300 才够窄)。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/files_panel.rs
git commit -m "feat(app): 文件面板改双向滚动,列头跟随水平偏移 (F136)

列头改到 ScrollArea 之后绘制以拿到本帧真实 offset.x(上一帧的会让列头滞后
一帧)。跑了 horizontal_scroll_moves_the_header_and_the_rows_by_the_same_amount。"
```

---

### Task 4: 截断纯函数

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs`(新增 `Elide`/`elide`/两个私有辅助)
- Test: 同文件 `mod tests`

这一步**不碰绘制**,只加纯函数 + 测试。

- [ ] **Step 1: 写失败的测试**

```rust
    /// 测宽桩:ASCII 7pt / 非 ASCII 14pt(CJK 一个字顶两个 ASCII,省略号
    /// 也是非 ASCII)。用桩而不是真字体,这几条测试才能脱离 egui 上下文跑。
    fn stub_measure(s: &str) -> f32 {
        s.chars().map(|c| if c.is_ascii() { 7.0 } else { 14.0 }).sum()
    }

    /// F137:名称列中间省略,**扩展名必须留住** —— 一列同前缀的
    /// `backup-2026-08-19.tar.gz` / `.log` / `.sql`,尾部省略之后全长一样,
    /// 等于把这一列的信息量清零。
    ///
    /// 自证会变红:把 `ext_tail()` 的两段扩展名循环改成只取一段
    /// (`.tar.gz` 会退化成 `.gz`),或者把 `Middle` 直接转发给 `End`。
    #[test]
    fn eliding_a_name_in_the_middle_keeps_its_extension() {
        let out = elide(
            "a-very-long-filename.tar.gz",
            140.0,
            Elide::Middle,
            stub_measure,
        );
        assert!(
            out.ends_with(".tar.gz"),
            "两段扩展名没留住,实际截成了 {out:?}"
        );
        assert!(out.contains('…'), "中间没有省略号,实际 {out:?}");
        assert!(
            stub_measure(&out) <= 140.0,
            "截完还是超宽({}),实际 {out:?}",
            stub_measure(&out)
        );

        let cjk = elide("很长的中文文件名很长的中文文件名.txt", 100.0, Elide::Middle, stub_measure);
        assert!(cjk.ends_with(".txt"), "单段扩展名没留住,实际 {cjk:?}");
        assert!(
            stub_measure(&cjk) <= 100.0,
            "CJK 名字截完超宽({}),实际 {cjk:?}",
            stub_measure(&cjk)
        );
    }

    /// 边界:放得下就原样返回;没有扩展名 / 扩展名自己就吃掉半个预算 /
    /// 预算窄到连省略号都放不下 —— 都不许 panic,也不许超宽。
    ///
    /// 自证会变红:去掉 `truncate_to_width()` 的 `budget <= 0.0` 早退
    /// (会在空串上做减法索引),或者去掉 `elide_end()` 里
    /// 「省略号都放不下就返回空串」那条。
    #[test]
    fn eliding_never_exceeds_the_budget_and_never_panics() {
        // 放得下 → 原样。
        assert_eq!(
            elide("a.txt", 999.0, Elide::Middle, stub_measure),
            "a.txt"
        );
        for (text, budget) in [
            ("no-extension-but-really-long-indeed", 60.0),
            ("x.20260819.backup", 60.0),
            (".bashrc", 30.0),
            ("很长很长很长很长的中文名", 40.0),
            ("abc.txt", 10.0),
            ("abc.txt", 0.0),
            ("", 50.0),
        ] {
            for mode in [Elide::End, Elide::Middle] {
                let out = elide(text, budget, mode, stub_measure);
                assert!(
                    stub_measure(&out) <= budget + 0.01,
                    "{text:?} 在预算 {budget} 下截成了 {out:?},宽 {}",
                    stub_measure(&out)
                );
            }
        }
    }
```

`Elide` 需要 `Copy`(上面 `for mode in [..]` 之后还要再用)。

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib eliding 2>&1 | tail -10
```
预期:编译失败,`cannot find function elide`。

- [ ] **Step 3: 实现**

加在 `files_panel.rs` 的 `owner_text()` 之后(生产代码段内):

```rust
/// F137:截断方式。
#[derive(Clone, Copy, Debug, PartialEq)]
enum Elide {
    /// 尾部省略:`2026-08-19 10:3…`。给数值/时间/权限/属主与列头标题用。
    End,
    /// 中间省略、保留扩展名:`a-very-long-fil….tar.gz`。只给名称列用 ——
    /// 一列同前缀的备份文件尾部省略之后全长一样,信息量清零。
    Middle,
}

const ELLIPSIS: &str = "…";

/// 把 `text` 截到 `max_w` 以内,截掉的地方放一个 `…`。
///
/// `measure`:测一段文字的宽度。生产侧传 `|s| painter.layout_no_wrap(...)`
/// ——**必须用真实字体测**,CJK 一个字顶两个 ASCII,按字符数估算会在中文
/// 目录里全错。测试侧传桩,于是这个函数本身不需要 egui 上下文就能测。
///
/// **前提:`measure` 对前缀单调不减**(前缀越长越宽)。二分查找依赖这一点;
/// 对正常字体成立(字形宽度非负)。
fn elide(text: &str, max_w: f32, mode: Elide, measure: impl Fn(&str) -> f32) -> String {
    if measure(text) <= max_w {
        return text.to_string();
    }
    let ellipsis_w = measure(ELLIPSIS);
    if mode == Elide::Middle {
        if let Some(tail) = ext_tail(text) {
            let tail_w = measure(tail);
            // 尾段 + 省略号吃掉超过一半预算 → 退化成尾部省略。留一个前面
            // 什么都放不下的尾巴,不如老老实实从后面截。
            if tail_w + ellipsis_w <= max_w * 0.5 {
                let head_src = &text[..text.len() - tail.len()];
                let head = truncate_to_width(head_src, max_w - tail_w - ellipsis_w, &measure);
                return format!("{head}{ELLIPSIS}{tail}");
            }
        }
    }
    // 尾部省略(也是 `Middle` 的退化路径)。
    if ellipsis_w > max_w {
        // 连一个省略号都放不下:什么都不画。画半个字符更像渲染坏了。
        return String::new();
    }
    let head = truncate_to_width(text, max_w - ellipsis_w, &measure);
    format!("{head}{ELLIPSIS}")
}

/// 从右往左取至多 **2 段**扩展名,总长(字符数)不超过 10。
///
/// `a.tar.gz` → `.tar.gz`;`a.txt` → `.txt`;`x.20260819.backup` → `.backup`
/// (两段共 16 > 10);`.bashrc` → `None`(点在开头,那是名字不是扩展名);
/// `no-ext` → `None`。
fn ext_tail(name: &str) -> Option<&str> {
    let mut best = None;
    let mut rest = name;
    for _ in 0..2 {
        let Some((head, _)) = rest.rsplit_once('.') else {
            break;
        };
        if head.is_empty() {
            break;
        }
        // `head` 是 `rest` 的前缀,而 `rest` 是 `name` 的前缀 ——
        // `head.len()` 因此也是 `name` 里的字节偏移。
        let cand = &name[head.len()..];
        if cand.chars().count() > 10 {
            break;
        }
        best = Some(cand);
        rest = head;
    }
    best
}

/// `s` 里能放进 `budget` 的最长**前缀**(切在 `char` 边界上)。
///
/// 二分而不是逐字符递减:一屏有几十行 × 五列,长名字逐字符退让会把
/// `layout_no_wrap` 调用次数推到每帧上万次。
fn truncate_to_width<'a>(s: &'a str, budget: f32, measure: &impl Fn(&str) -> f32) -> &'a str {
    if budget <= 0.0 {
        return "";
    }
    if measure(s) <= budget {
        return s;
    }
    let bounds: Vec<usize> = s
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(s.len()))
        .collect();
    // 找最大的 `lo` 使 `s[..bounds[lo]]` 放得下。`lo = 0`(空串)恒满足。
    let (mut lo, mut hi) = (0usize, bounds.len() - 1);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if measure(&s[..bounds[mid]]) <= budget {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    &s[..bounds[lo]]
}
```

> `div_ceil` 是 `usize` 的稳定方法(Rust 1.73+),clippy 会要求用它而不是 `(lo + hi + 1) / 2`。

- [ ] **Step 4: 跑测试**

```bash
cargo test -p mullion-app --lib eliding > /tmp/t4.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t4.log
cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -5
```
预期:两条 PASS;clippy 无输出。此时 `elide` 还没人调用,会有 `dead_code` 警告 —— **不要**加 `#[allow(dead_code)]` 绕过,直接接着做 Task 5(同一次 clippy 在 Task 5 结束时才需要干净)。若要中途提交,把 Task 4 和 Task 5 合成一次提交。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/files_panel.rs
git commit -m "feat(app): 加文字截断纯函数,名称列中间省略保留扩展名 (F137)

测宽用注入闭包,函数本身不依赖 egui 上下文。跑了
eliding_a_name_in_the_middle_keeps_its_extension 与
eliding_never_exceeds_the_budget_and_never_panics。"
```

---

### Task 5: 把截断接到行与列头

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs`(`row()` 五处文字、`header_at()` 一处)
- Test: 同文件 `mod tests`

- [ ] **Step 1: 写失败的测试**

```rust
    /// F137 的验收判据:长名字**画不到**「大小」列上。
    ///
    /// 纯函数那两条只守 `elide()` 自己算得对不对 —— 生产代码完全可以
    /// 算完了不用(或者预算传错),这条从真实渲染结果里取两个 galley
    /// 的横向区间来比,才守得住「接上了」。
    ///
    /// 自证会变红:把 `row()` 里名称那处的 `elide(...)` 换回原来的
    /// `label` 直画。
    #[test]
    fn a_long_name_is_elided_so_it_cannot_reach_the_size_column() {
        /// 找到含 `needle` 的文字,返回它的**横向区间**(左, 右)。
        fn text_span(
            shapes: &[egui::epaint::ClippedShape],
            needle: &str,
        ) -> Option<(f32, f32)> {
            fn walk(shape: &egui::Shape, needle: &str) -> Option<(f32, f32)> {
                match shape {
                    egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, needle)),
                    egui::Shape::Text(ts) if ts.galley.text().contains(needle) => {
                        Some((ts.pos.x, ts.pos.x + ts.galley.size().x))
                    }
                    _ => None,
                }
            }
            shapes.iter().find_map(|cs| walk(&cs.shape, needle))
        }

        let t = crate::theme::MULLION_DARK;
        let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
        // 长到无论如何都放不进 220pt 名称列的名字。
        state.entries = vec![entry(
            b"aaaaaaaaaa-bbbbbbbbbb-cccccccccc-dddddddddd-eeeeeeeeee.tar.gz",
            EntryKind::File,
        )];
        state.load = Load::Ready;

        let mut cols = ColWidths::default();
        let ctx = egui::Context::default();
        let mut out = None;
        for _ in 0..2 {
            out = Some(ctx.run(raw(None), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        "远端",
                        1,
                        PanelColumn::Remote,
                        &mut state,
                        false,
                        &[],
                        0,
                        &mut cols,
                    );
                });
            }));
        }
        let out = out.expect("跑了两帧");
        // 名称的 galley:截断后一定含 `…`,而且尾巴留着 `.tar.gz`。
        let name = text_span(&out.shapes, "…").expect("长名字应该被截断并带上省略号");
        let value = text_span(&out.shapes, "1.0 KB").expect("行里该画出大小数值");
        assert!(
            name.1 <= value.0 + 0.01,
            "名字右边界 {} 越过了大小数值左边界 {} —— 两串文字重叠在一起了",
            name.1,
            value.0
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib a_long_name_is_elided 2>&1 | tail -10
```
预期:FAIL —— `长名字应该被截断并带上省略号`(此时还没接,画的是整串)。

- [ ] **Step 3: 接到 `row()`**

在 `row()` 里 `let p = ui.painter();` 之后加一个测宽闭包:

```rust
    // F137:测宽用**真实字体**。`Painter::layout_no_wrap` 内部带 memoization,
    // 重复串很便宜;颜色随便给,只取尺寸。
    let measure = |s: &str| {
        p.layout_no_wrap(s.to_owned(), font.clone(), egui::Color32::WHITE)
            .size()
            .x
    };
```

名称那处(约 831 行 `p.text(...)` 画 `label`)改成:

```rust
    // 名称列的可用宽度:让出图标格子 + 间隙,右边再留一个 `SP_XS` 的
    // 呼吸位,否则截断后的省略号会紧贴着「大小」列的数字。
    let name_budget =
        cols.name - name_start_x_offset() - crate::ui::metrics::SP_XS;
    p.text(
        rect.left_center() + egui::vec2(name_start_x_offset(), 0.0),
        egui::Align2::LEFT_CENTER,
        // **拼完整串再截**:符号链接的 `→ target` 和「名称非 UTF-8」那句
        // 后缀都得参与预算,先截名字再拼后缀的话后缀照样溢出。
        elide(&label, name_budget, Elide::Middle, &measure),
        font.clone(),
        fg,
    );
```

其余四列各自套一层 `elide(..., Elide::End, &measure)`,预算是**该列宽减一个 `SP_XS`**:

```rust
        elide(&size_text, size_w - crate::ui::metrics::SP_XS, Elide::End, &measure),
        elide(&mtime_text(e.mtime), mtime_w - crate::ui::metrics::SP_S, Elide::End, &measure),
        elide(&perm_string(e.mode), perm_w - crate::ui::metrics::SP_XS, Elide::End, &measure),
        elide(&owner_text(column, e.uid, e.gid), owner_w - crate::ui::metrics::SP_XS, Elide::End, &measure),
```

(修改时间那列是左对齐、内缩 `SP_S`,预算相应减 `SP_S`;取宽度时把 Task 1 里 `let (_, _, mtime_left, _) = lay[2];` 改成 `let (_, _, mtime_left, mtime_w) = lay[2];`。)

- [ ] **Step 4: 接到 `header_at()`**

`header_at()` 里画标题那处:

```rust
        let font = egui::FontId::proportional(11.0);
        let painter = ui.painter().with_clip_rect(band);
        let measure = |s: &str| {
            painter
                .layout_no_wrap(s.to_owned(), font.clone(), egui::Color32::WHITE)
                .size()
                .x
        };
        painter.text(
            rect.left_center() + egui::vec2(crate::ui::metrics::SP_XS, 0.0),
            egui::Align2::LEFT_CENTER,
            elide(
                &format!("{label}{mark}"),
                w - crate::ui::metrics::SP_XS * 2.0,
                Elide::End,
                &measure,
            ),
            font.clone(),
            theme::c32(t.fg_muted),
        );
```

> 列宽拖到 50pt 时「修改时间 ▲」必须截断,否则标题会横穿两列 —— 这是 Task 6 之后立刻能看到的现象。

- [ ] **Step 5: 跑测试**

```bash
cargo test -p mullion-app --lib files_panel > /tmp/t5.log 2>&1; grep -nE "test result|FAILED|panicked|error\[" /tmp/t5.log | head -30
```
预期:新测试 PASS,其余全绿。

**特别检查** `a_non_utf8_name_is_shown_with_an_explicit_note` 与 `a_lossy_name_that_is_valid_utf8_is_still_marked_unusable` 这两条:它们断言画出来的文本里含「名称非 UTF-8」字样,而现在名称列会截断。默认名称列 220pt 放不下那句后缀 → **这两条会红**。修法**不是**放宽判据,而是在这两条测试里把列宽调宽到放得下:

```rust
        let mut cols = ColWidths {
            name: 900.0,
            ..ColWidths::default()
        };
```
(它们守的是「这句提示存在」,不是「在 220pt 下也完整可见」;`rendered_texts()` 辅助函数相应加一个 `cols` 参数或在内部用宽列宽。)

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/ui/files_panel.rs
git commit -m "feat(app): 行与列头按列宽截断,名称列中间省略 (F137)

跑了 a_long_name_is_elided_so_it_cannot_reach_the_size_column;
非 UTF-8 那两条改成用宽列宽跑(它们守的是提示存在,不是窄列下也完整)。"
```

---

### Task 6: 列宽拖拽

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs`(`header_at()` 加拖拽热区,签名改 `&mut ColWidths`)
- Test: 同文件 `mod tests`

- [ ] **Step 1: 写失败的测试**

```rust
    /// F135:拖列边界只改**被拖的那一列**,右边的列整体平移。
    /// 不做「向右借宽度」那种此消彼长的语义 —— 总宽本来就允许超出视口
    /// (有横向滚动兜着),没有守恒的必要,而守恒会让「我只想加宽名称列」
    /// 变成「顺手把修改时间列挤没了」。
    ///
    /// 自证会变红:把 `col_w_mut(cols, i)` 换成固定改 `cols.name`;
    /// 或者在改宽度时顺手把下一列减掉同样的量。
    #[test]
    fn dragging_a_column_edge_only_widens_that_column() {
        let t = crate::theme::MULLION_DARK;
        let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
        state.entries = vec![entry(b"a.txt", EntryKind::File)];
        state.load = Load::Ready;
        let mut cols = ColWidths::default();
        let before = cols;
        let ctx = egui::Context::default();
        let mut render = |input: egui::RawInput, state: &mut PaneState, cols: &mut ColWidths| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        "远端",
                        1,
                        PanelColumn::Remote,
                        state,
                        false,
                        &[],
                        0,
                        cols,
                    );
                });
            })
        };
        // 两帧稳定布局,并拿到列头横带的位置:列头是本栏第一条 ROW_H 高的
        // 横带,「名称」标题的 y 就是它的中线。
        let _ = render(raw(None), &mut state, &mut cols);
        let out = render(raw(None), &mut state, &mut cols);
        let head_y = find_text_pos(&out.shapes, "名称").expect("该画出列头「名称」").y;
        // 名称列右边界的绝对 x:列头横带左边界 + name。CentralPanel 从 0 起,
        // 但 Frame 有内边距 —— 用「大小」标题的位置反推更稳:它左对齐在
        // 名称列右边界 + SP_XS 处。
        let size_head_x = find_text_pos(&out.shapes, "大小").expect("该画出列头「大小」").x;
        // 标题的 pos 是文字中心(`find_text_pos` 加了半个 galley),这里只要
        // 一个落在热区内的 x —— 边界左右各 3pt,用标题起点往左退一点。
        let edge_x = size_head_x - crate::ui::metrics::SP_XS - 20.0;

        // 按下 → 拖到右边 +60 → 松手。egui 要指针移动超过阈值才判成拖,
        // 中间多灌一帧。
        let _ = render(press(egui::pos2(edge_x, head_y), 1.0, true), &mut state, &mut cols);
        let _ = render(moved(egui::pos2(edge_x + 30.0, head_y), 1.1), &mut state, &mut cols);
        let _ = render(moved(egui::pos2(edge_x + 60.0, head_y), 1.2), &mut state, &mut cols);
        let _ = render(press(egui::pos2(edge_x + 60.0, head_y), 1.3, false), &mut state, &mut cols);

        assert!(
            cols.name > before.name + 40.0,
            "拖了名称列右边界 +60,宽度却只从 {} 变成 {} —— 拖拽没接上",
            before.name,
            cols.name
        );
        assert_eq!(
            (cols.size, cols.mtime, cols.perm, cols.owner),
            (before.size, before.mtime, before.perm, before.owner),
            "拖名称列把别的列宽也改了"
        );
    }

    /// F135:再怎么往左拖也不能把列拖没 —— 宽度为 0 的列点不中、拖不回来,
    /// 用户会以为这一列被永久删掉了。
    ///
    /// 自证会变红:把 `.clamp(col_min(i), COL_MAX)` 去掉。
    #[test]
    fn a_column_cannot_be_dragged_below_its_minimum() {
        let mut cols = ColWidths::default();
        // 直接测夹紧规则本身(拖拽路径由上一条守着)。
        for i in 0..5 {
            *col_w_mut(&mut cols, i) = (-9999.0_f32).clamp(col_min(i), COL_MAX);
            assert!(
                *col_w_mut(&mut cols, i) >= col_min(i),
                "第 {i} 列被夹到了 {} ,低于最小宽度 {}",
                *col_w_mut(&mut cols, i),
                col_min(i)
            );
        }
    }

    /// 拖拽热区不能把整列的排序点击吃掉 —— 点列头**中心**必须照旧改排序。
    ///
    /// 自证会变红:把热区宽度从 6pt 改成整列宽。
    #[test]
    fn clicking_the_middle_of_a_header_still_sorts() {
        let t = crate::theme::MULLION_DARK;
        let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
        state.entries = vec![entry(b"a.txt", EntryKind::File)];
        state.load = Load::Ready;
        let before = state.sort_key;
        let mut cols = ColWidths::default();
        let ctx = egui::Context::default();
        let mut render = |input: egui::RawInput, state: &mut PaneState, cols: &mut ColWidths| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        "远端",
                        1,
                        PanelColumn::Remote,
                        state,
                        false,
                        &[],
                        0,
                        cols,
                    );
                });
            })
        };
        let _ = render(raw(None), &mut state, &mut cols);
        let out = render(raw(None), &mut state, &mut cols);
        let pos = find_text_pos(&out.shapes, "修改时间").expect("该画出列头「修改时间」");
        let _ = render(press(pos, 1.0, true), &mut state, &mut cols);
        let _ = render(press(pos, 1.1, false), &mut state, &mut cols);
        assert_ne!(
            state.sort_key, before,
            "点了「修改时间」列头中心,排序键却没变 —— 拖拽热区把整列的点击吃掉了"
        );
        assert_eq!(state.sort_key, SortKey::Mtime);
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib dragging_a_column_edge 2>&1 | tail -10
```
预期:FAIL —— `拖拽没接上`。

- [ ] **Step 3: 实现**

`header_at()` 的 `cols` 参数改成 `&mut ColWidths`(`show()` 传 `cols` 本身即可,它已经是 `&mut`)。在**列循环之前**先注册全部拖拽热区 —— 热区必须先于列体注册,否则边界那几点上的按下会被排序点击吃掉:

```rust
    // F135:列宽拖拽热区。**必须先于列体注册**:egui 同层内先注册的部件
    // 拿到命中权,挂在后面的话边界那 6pt 上的按下会被排序点击吃掉。
    //
    // 热区认的是**每一列的右边界**(i = 0..5),拖动只改第 i 列的宽度,
    // 右边的列整体平移(不做此消彼长的「借宽度」—— 总宽有横向滚动兜着,
    // 没有守恒的必要)。
    const HANDLE_W: f32 = 6.0;
    for (i, (_, _, left, w)) in col_lefts(cols).into_iter().enumerate() {
        let x = band.left() + left + w - offset_x;
        let handle = egui::Rect::from_min_max(
            egui::pos2(x - HANDLE_W * 0.5, band.top()),
            egui::pos2(x + HANDLE_W * 0.5, band.bottom()),
        );
        let resp = ui
            .interact(
                handle,
                ui.id().with(("files-col-resize", id, i)),
                egui::Sense::drag(),
            )
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        if resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            let target = col_w_mut(cols, i);
            *target = (*target + resp.drag_delta().x).clamp(col_min(i), COL_MAX);
        }
        if resp.hovered() || resp.dragged() {
            // 抓住的是哪条线,要看得见。
            ui.painter()
                .with_clip_rect(band)
                .vline(x, band.y_range(), egui::Stroke::new(1.0, theme::c32(t.accent)));
        }
    }
```

> 循环里 `col_lefts(cols)` 借了 `cols` 不可变,循环体里又要 `col_w_mut(cols, i)` 可变借 —— `col_lefts` 返回的是 `[(&'static str, SortKey, f32, f32); 5]`(全是 `Copy`,不借 `cols`),`.into_iter()` 之后借用就结束了,编译得过。**如果报错**,说明你把返回类型改成了带引用的形式,改回值类型。

`show()` 里调用处相应改成传 `cols`(而不是 `&*cols`)。

- [ ] **Step 4: 跑测试**

```bash
cargo test -p mullion-app --lib files_panel > /tmp/t6.log 2>&1; grep -nE "test result|FAILED|panicked|error\[" /tmp/t6.log | head -30
```
预期:三条新测试 PASS,其余全绿。

**若 `dragging_a_column_edge_only_widens_that_column` 红在「拖拽没接上」**:egui 判「拖」有位移阈值,先确认 `press`/`moved` 的 `time` 在递增(`raw(Some(time))`),再把中间的 `moved` 帧多灌两帧。**不要**为了让它绿去改断言里的 `+40.0` 容差 —— 那是在削弱测试。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/files_panel.rs
git commit -m "feat(app): 列头分隔线可拖拽调宽,夹紧最小/最大宽度 (F135)

热区先于列体注册,排序点击照旧。跑了
dragging_a_column_edge_only_widens_that_column / a_column_cannot_be_dragged_below_its_minimum /
clicking_the_middle_of_a_header_still_sorts。"
```

---

### Task 7: 补 spec 编号 + 全量绿

**Files:**
- Modify: `spec.md`(§4 需求表)

- [ ] **Step 1: 写进 spec.md**

在 `spec.md` 里 F134 那条之后,按现有格式补三条:

```markdown
- **F135** 文件面板列头可拖拽调宽(五列各自定宽,最小 80/48,最大 800;只存内存,不落盘)
- **F136** 文件面板两栏均支持水平滚动;列不再因宽度不够而自动收起,列头跟随水平偏移
- **F137** 文件面板每个单元格按自己的列宽截断:名称列中间省略保留扩展名,其余列尾部省略
```

(先 `grep -n "F134" spec.md` 找到确切位置和该处的行文格式,照抄格式,不要自创。)

- [ ] **Step 2: 全量绿**

```bash
cargo test --workspace > /tmp/all.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/all.log | head -30
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo fmt --check 2>&1 | head
```
预期:全部 `test result: ok`;clippy 与 fmt 无输出。**这才是「绿」**(见 `CLAUDE.md`)。

- [ ] **Step 3: 提交**

```bash
git add spec.md
git commit -m "docs: spec 补 F135/F136/F137"
```

---

### Task 8: 发版给人工验收

- [ ] **Step 1: 走发版一条龙**

`CLAUDE.md` 的交付约定:本片改动落在 `mullion-app`,默认执行完整流程,不要停下来问。

调用 `release-windows` skill(说「发版」即自动加载),它覆盖:升 patch 版本号 → 跑绿 → 交叉编译 → objdump 依赖验收(出现 `libgcc_s_seh-1.dll` / `libwinpthread-1.dll` 即不合格)→ **签名**(必须在算 sha256 之前)→ 先 push 再 `gh release create`(标题只能是纯版本号 `v0.1.N`)→ 报 Release 链接 + sha256。

- [ ] **Step 2: 给出人工验收清单**

Release notes 里附上(这些是你验不了、只有人眼能判的):

1. 打开文件侧栏(默认 360px 宽):底部应出现**水平滚动条**,横着滚能看到「属主」列 —— 旧版这一列在这个宽度下是被藏起来的。
2. 把鼠标移到列头两列之间:光标变成**左右箭头**,出现一条高亮竖线;按住左右拖,该列跟着变宽/变窄,右边的列整体平移。
3. 把「修改时间」列拖到很窄:标题本身要变成 `修改…`,不能横穿到「权限」列上。
4. 往左拖到底:列不会消失,停在一个还点得中的最小宽度。
5. 进一个有长文件名的目录(比如 `~/.cache` 或任意 node_modules):名字应显示成 `很长的前缀….tar.gz` 这种**中间省略**,扩展名留着,且不与「大小」列的数字重叠。
6. 点列头**中间**仍然能改排序(升/降箭头照常切换)。
7. 切到另一个标签页再切回来:列宽保持;**关掉窗口重开会回到默认宽度**(设计 D2 明确不落盘)。
8. 两栏各自的横向滚动互不影响。

---

## 自查(计划作者已跑)

**Spec 覆盖**:D1(删收起)→ Task 1;D2(存 UiState 不落盘)→ Task 2;D3(中间省略保留扩展名)→ Task 4/5;D4(不引入 egui_extras)→ 全程自绘;D5(不做双击自适应)→ 未出现在任何任务里。设计 §① → Task 1,§② → Task 4/5,§③ → Task 3,§④ → Task 6,§⑤ → Task 2,§⑥ → Task 1 Step 7 与 Task 7 Step 2,§⑦ → Task 8 Step 2。

**命名一致性**:`ColWidths` / `col_lefts` / `content_w` / `col_min` / `col_w_mut` / `COL_MAX` / `Elide` / `elide` / `ext_tail` / `truncate_to_width` / `header_at` —— 全篇同名同签名。`header()` 在 Task 1 引入、Task 3 删除(它的占位职责搬进了 `show()`),这是**有意的**,不是漏改。
