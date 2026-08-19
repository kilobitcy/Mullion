# 文件面板列宽可调 + 横向滚动 + 单元格截断 —— 设计

> 需求编号 **F135**(列头各列可拖拽调宽)、**F136**(两栏都有水平滚动条)、
> **F137**(超出列宽的内容截断,不再互相重叠)——三条均为新增,待写进 `spec.md` §4。
> 日期 2026-08-19。

## 为什么是一片

三条看着独立,实际是**同一个改动的三个面**:

现在的列模型是「名称列吃掉剩余宽度 + 放不下就从右向左收起可选列」
(`visible_col_count()` / `name_w()`)。在这个模型下:

- 列宽不可调 —— 四列是常量,名称列是被动的减法结果;
- 水平滚动条**永远不会出现** —— 名称列自适应,内容宽恒等于视口宽;
- 行体文字是 `painter.text()` 直接画的,**没有任何裁剪** —— 长文件名必然
  压到「大小」列上(这就是 F137 的根因,不是渲染 bug 是模型 bug)。

所以只能整体换成:**每列定宽(含名称列)、可拖拽、总宽 = 各列之和、超出
视口就横向滚、每个单元格按自己的列宽截断**。分三次做没有中间态可发布。

## 已定的取舍(brainstorm 结论,不再回头讨论)

| # | 决策 | 理由 / 代价 |
|---|---|---|
| D1 | **彻底删掉「窄了就收起可选列」** | 四列恒定显示,视口不够由横向滚动条兜底。列布局只剩「各列宽度累加」一条规则。代价:默认侧栏 360px 下总列宽 608 > 视口,**一打开就是横向滚动状态** |
| D2 | 列宽**只存内存,不落盘** | 存 `ui::UiState`(跟 `files_sidebar_w` 同处),全局一份、远端/本地/所有标签共用一套。不动 store schema。代价:重启后回到默认宽度 |
| D3 | 名称列**中间省略、保留扩展名**;其余列尾部省略 | `a-very-long-fil….tar.gz`。egui 不原生支持中间省略,自己写纯函数 |
| D4 | 沿用**自绘**架构,不引入 `egui_extras::TableBuilder` | `TableBuilder` 原生给列宽拖拽 + 单元格裁剪,但它把一行拆成 5 个 cell 的 `ui` 回调;而行上的 dnd 载荷(F58)、右键菜单、双击、`click_and_drag` 全挂在**单个** `Response` 上,拆开等于把 D4a 那批已经踩平的坑重踩一遍。而且它是新依赖 |
| D5 | 不做「双击分隔线自适应内容宽」 | 没人要的功能(YAGNI) |

---

## ① 列模型(F135 的地基)

### 现状

```rust
const W_SIZE: f32 = 78.0;  const W_MTIME: f32 = 132.0;
const W_PERM: f32 = 86.0;  const W_OWNER: f32 = 92.0;
const OPTIONAL_COLS: [(&str, SortKey, f32); 4] = [...];

fn visible_col_count(total) -> usize   // 从右向左收起,判据「名称列 >= 80」
fn name_w(total) -> f32                // total - 图标格 - 可见列宽之和
fn header_name_col_w(total) -> f32     // name_w + W_ICON + ICON_GAP
fn header_col_lefts(total) -> Vec<...> // 列头侧独立累加
fn row_size_col_left(row_rect) -> f32  // 行体侧起点
fn row_col_lefts(row_rect) -> Vec<...> // 行体侧独立累加
```

### 做什么

上面 6 个函数 + `OPTIONAL_COLS` 全部删除,换成一份宽度状态和一个布局函数:

```rust
/// 五列的当前宽度(point)。**名称列的宽度含图标格** —— 沿用列头
/// 「图标 + 名称 = 一个合并区域」的既有语义,不额外记一份图标宽。
///
/// 放 `ui::UiState`(全局一份,两栏 + 所有标签共用),**不落盘**(D2)。
pub struct ColWidths { pub name: f32, pub size: f32, pub mtime: f32,
                       pub perm: f32, pub owner: f32 }

impl Default for ColWidths { /* 220 / 78 / 132 / 86 / 92 */ }

/// 每列的最小宽度。名称列要放得下图标格 + 几个字,其余列放得下
/// 「修改时间」这种四字标题被截断后的省略号。
const COL_MIN: ColWidths = ColWidths { name: 80.0, size: 48.0, ... };
/// 上限。拖到几千 px 不会崩,但滚动条会退化成一条几乎没法用的细线。
const COL_MAX: f32 = 800.0;

/// **列布局的唯一真值来源**:列头和行体调的是同一份。
///
/// 原来两边各自累加、靠 `the_header_and_row_size_column_start_at_the_same_x`
/// 一条测试守住「不许错位」;现在坐标同源,错位在物理上不可能发生
/// —— 那条测试相应改成守「只有一处累加」。
fn col_lefts(w: &ColWidths) -> [(&'static str, SortKey, f32, f32); 5]

/// 内容总宽 = 各列之和。视口比它窄就出横向滚动条(F136)。
fn content_w(w: &ColWidths) -> f32
```

名称列默认 **220**。参考:现在 360px 侧栏下名称列的实际宽度是 130
(`name_w(360)`),220 更好用但一打开就要横滚 —— 这是 D1 明确接受的代价,
不满意就改 `Default` 这一行。

### 守护

- `columns_are_dropped_from_the_right_as_the_panel_gets_narrower` —— **删除**
  (它守的行为被 D1 移除了)。取而代之:窄栏下属主列**仍然存在**(见 ③ 的
  `the_owner_column_survives_a_narrow_panel_instead_of_being_dropped`)。
- `the_name_column_width_is_computed_in_exactly_one_place` —— 改成扫源码里
  `col_lefts(` 之外没有第二处列坐标累加。
- `the_header_and_row_size_column_start_at_the_same_x_across_widths` —— 改写成
  「列头每一列的 rect 与行体同名列的 rect 左右边界重合」,在几个代表列宽下跑。

---

## ② 单元格截断(F137)

### 现状

`row()` 里五处 `p.text(...)` 直接画,没有 wrap 也没有 clip。长名字画到
「大小」列上,两串文字直接叠在一起。列头同理(列宽拖窄后标题会溢出)。

### 做什么

抽一个**能脱离 egui 单测**的纯函数(测宽用注入的闭包,不碰 `ui.fonts`):

```rust
pub enum Elide { End, Middle }

/// 把 `text` 截到 `max_w` 以内,截掉的地方放一个 `…`。
///
/// `measure`:测一段文字的宽度。生产侧传 `|s| ui.fonts(|f|
/// f.layout_no_wrap(s.into(), font.clone(), color).size().x)` —— **必须**
/// 用真实字体测,CJK 一个字顶两个 ASCII,按字符数估算会在中文目录里全错。
/// 测试侧传桩(每 ASCII 字符 7.0 / 每 CJK 14.0),于是这个函数本身
/// 不需要 egui 上下文就能测。
///
/// `Middle`:尾段 = 从右往左至多 2 段扩展名、总长 ≤ 10 字符
/// (`.tar.gz` 全留;`.txt` 全留;`x.20260819.backup` 两段共 16 > 10,
/// 只留 `.backup`),前段吃剩下的宽度。没有扩展名、或扩展名自己就超过
/// 预算的一半 → **退化成 `End`**(留一个放不下前段的尾巴没有意义)。
///
/// 切分只在 `char` 边界上做(`char_indices`),非 UTF-8 名字已经在上游
/// lossy 成了 `U+FFFD`,这里不会切出半个码点。
fn elide(text: &str, max_w: f32, mode: Elide, measure: impl Fn(&str) -> f32) -> String
```

应用点(共 6 处):

| 位置 | 模式 | 预算 |
|---|---|---|
| 行·名称 | `Middle` | `name - ICON_LEFT_PAD - W_ICON - ICON_GAP - SP_XS` |
| 行·大小 / 修改时间 / 权限 / 属主 | `End` | 该列宽 - `SP_XS` |
| 列头·五个标题(含排序箭头 `▲`/`▼`) | `End` | 该列宽 - `SP_XS` |

名称列的 `label` 是**拼完整串之后**再截断的 —— 符号链接的 `a → b`、
非 UTF-8 的 `(名称非 UTF-8,本版无法操作)` 后缀都参与截断预算,不能
先截名字再拼后缀(那样后缀照样溢出)。

### 守护

- `elide_middle_keeps_the_extension` —— `.tar.gz` / `.txt` 场景各一条断言。
- `elide_never_exceeds_the_budget` —— 含 CJK 与「预算比一个 `…` 还窄」的
  边界,断言结果的测量宽度 ≤ 预算,且不 panic。
- `a_long_name_is_elided_so_it_cannot_reach_the_size_column` —— egui harness:
  画一个超长名字,从 shapes 里取那条 galley,断言它的右边界 ≤ 大小列左边界。
  **这条才是 F137 的验收判据**,前两条只守纯函数。

---

## ③ 横向滚动 + 列头跟随(F136)

### 做什么

1. `ScrollArea::vertical()` → `ScrollArea::both()`。
   `.drag_to_scroll(false)` **保留**(F58 的坑:它会把按在行上的那一下抢去
   当滚动手势,行的 `drag_started()` 永远为假)。
2. `show_rows` 闭包里,行的分配宽度改成 `content_w.max(ui.available_width())`。
   - 取 `max` 不是取 `content_w`:总列宽小于视口时,行仍要铺满整个视口宽,
     否则选中高亮只有半行长、右边那片空白点不中行也接不住 drop
     (`a_row_in_the_tab_host_can_actually_be_clicked` /
     `dropping_on_the_blank_part_of_a_column_targets_its_current_directory`
     两条现有测试守着这两件事)。
   - **这是 egui 0.30 最容易踩的坑**:`both()` 下如果行不显式要求这个宽度,
     `content_size.x` 恒等于视口宽,水平滚动条根本不出现。
3. **列头改到 `ScrollArea` 之后画**:
   ```
   let (header_rect, _) = ui.allocate_exact_size(vec2(avail_w, ROW_H), Sense::hover()); // 先占位
   let out = ScrollArea::both()....show_rows(...);                          // 再画表身
   let offset_x = out.state.offset.x;                                       // 拿本帧真值
   ui.scope_builder(UiBuilder::new().max_rect(header_rect), |ui| {
       ui.set_clip_rect(header_rect);   // 不裁的话列头会画到隔壁栏(B1 的坑)
       header(ui, t, id, state, cols, offset_x);
   });
   ```
   各列 rect 统一左移 `offset_x`。

   **为什么不是「列头一个 `ScrollArea::horizontal()`,offset 跟表身同步」**:
   那样列头拿到的是**上一帧**的 offset,拖滚动条时列头滞后一帧。上面这个
   顺序拿到的是本帧真值,零延迟。

   **z 序的副作用是好的**:列头后注册 → 压在滚动区之上,排序点击和列宽
   拖拽热区不会被滚动区的部件抢走(同 `show()` 开头那段背景菜单 z 序说明)。

### 守护

- `the_owner_column_survives_a_narrow_panel_instead_of_being_dropped` ——
  280px(侧栏拖拽下限)下 `col_lefts` 仍返回 5 列。
- `narrow_panel_produces_content_wider_than_the_viewport` —— 断言
  `ScrollAreaOutput::content_size.x > 视口宽`(横滚条出现的**充要条件**,
  比「肉眼看见滚动条」可测)。
- `the_header_follows_the_horizontal_scroll_offset` —— 灌一股水平滚轮,
  断言列头某个标题的 x 与行内同名列的 x **位移量相同**。
  (自证会变红:把 `header()` 里的 `- offset_x` 去掉。)

---

## ④ 列宽拖拽交互(F135 的手感面)

### 做什么

在 `header()` 里,每条列边界(第 i 列右边界,i = 0..5)注册一个宽 6pt、
高 `ROW_H` 的拖拽热区:

```rust
let id = ui.id().with(("files-col-resize", panel_id, i));
let resp = ui.interact(handle_rect, id, egui::Sense::drag());
if resp.hovered() || resp.dragged() {
    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    ui.painter().vline(x, rect.y_range(), Stroke::new(1.0, t.accent));
}
if resp.dragged() {
    // `col_w_mut(cols, i)` / `col_min(i)`:按列序号取字段的小访问器
    // —— `ColWidths` 是具名 struct 不是数组,五处 `match i` 只写一份。
    let w = col_w_mut(cols, i);
    *w = (*w + resp.drag_delta().x).clamp(col_min(i), COL_MAX);
}
```

- **热区先于列体注册** —— 否则边界那 6pt 上的按下会被排序点击吃掉。
- 拖第 i 列的右边界**只改第 i 列的宽度**,右边的列整体平移(不做「向右
  借宽度」那种此消彼长的语义 —— 总宽本来就允许超出视口,没有守恒的必要)。

### 守护

- `dragging_a_column_edge_only_changes_that_column` —— 拖「大小」列右边界
  +40,断言 `size` 增加 40、`mtime`/`perm`/`owner` 不变、且 `mtime` 的 left
  右移 40。
- `a_column_cannot_be_dragged_below_its_minimum` —— 往左灌一个巨大的 delta,
  断言夹在 `COL_MIN`。
- `clicking_a_column_header_still_sorts_when_the_edge_is_not_grabbed` ——
  点列头**中心**仍然改排序(证明热区没把整列吃掉)。

---

## ⑤ 调用点改动

- `show()` 增参 `cols: &mut ColWidths`。
- `content()` 增参 `ui_state: &mut crate::ui::UiState`(`sidebar()` 已经有),
  `ui/mod.rs::build_ui` 里的调用点跟着传。
- `ui::UiState` 增字段 `files_cols: ColWidths`。

## ⑥ 回归面(必须继续绿)

`files_panel.rs` 里现有的行为测试一条都不许改判据,只允许因签名变化而改
调用:行点击/双击/右键、四条 dnd 拖拽、`show_rows` 大目录、两栏独立滚动、
焦点边框、书签栏、路径条编辑。它们跟列布局无关,**如果为了让它们绿而动了
判据,说明这次改动破坏了别的东西**。

## ⑦ 你(Claude)验不了的部分

- 拖列宽跟不跟手、6pt 热区好不好抓;
- 省略号落的位置好不好看、中间省略后文件名还认不认得出;
- 横向滚动条在 Windows 上的实际观感(egui 的滚动条是自绘的,不是系统控件)。

→ 交叉编译出 exe,人工验收清单随 Release 给出。
