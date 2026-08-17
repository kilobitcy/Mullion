# 切片 J 视觉规格走查（Task 11）

> 范围：`crates/mullion-app/src/ui/files_panel.rs`、`crates/mullion-app/src/ui/chrome.rs`
> 两个文件的**生产代码**（`#[cfg(test)]` 模块之外的部分）。
> 判据：`spec.md` §4.6（F80~F85 已冻结的色板与尺寸）+
> `docs/ui-form-guidelines.md` 里两条与表单无关也成立的纪律
> （间距只用 `SP_XS/S/M/L/XL` 五档；颜色只用 theme token）。

## 产出方法

```bash
grep -nE "[0-9]+\.[0-9]" crates/mullion-app/src/ui/files_panel.rs | grep -v "^\s*//" > /tmp/walkthrough-files.txt
grep -nE "[0-9]+\.[0-9]" crates/mullion-app/src/ui/chrome.rs | grep -v "^\s*//" > /tmp/walkthrough-chrome.txt
```

`files_panel.rs` 命中 110 行，`chrome.rs` 命中 32 行（含 `#[cfg(test)]` 模块里的
坐标/时间戳字面量，这些不受两条纪律约束，逐条过滤见下）。

`SP_*` 五档的实际值（`crates/mullion-app/src/ui/metrics.rs`）：
`SP_XS = 4.0`、`SP_S = 8.0`、`SP_M = 12.0`、`SP_L = 16.0`、`SP_XL = 24.0`。

颜色：两个文件里除测试代码外，没有裸 `Color32::from_rgb`——生产代码全部经
`theme::c32(t.xxx)` 取色，无违规。

## 本次收敛（6 处，观感零变化）

只收敛「裸数字**恰好等于**某个 `SP_*` 值、且不落在受守护测试锁定的算式
（`icon_rect`/`name_start_x_offset`/`row_col_lefts`/`header_col_lefts`/
`visible_col_count`）里」的情形。收敛前后跑过 `cargo test --workspace`
全绿，未出现「换常量测试变红」——说明确实是等值替换。

| 文件:行 | 原裸数字 | 换成 | 语义 |
|---|---|---|---|
| `files_panel.rs:693` | `4.0` | `SP_XS` | 列头文字左内缩（`vec2(4.0, 0.0)`） |
| `files_panel.rs:786` | `8.0` | `SP_S` | 修改时间列文字左内缩 |
| `files_panel.rs:796` | `4.0` | `SP_XS` | 权限列文字右内缩 |
| `files_panel.rs:806` | `4.0` | `SP_XS` | 属主列文字右内缩 |
| `files_panel.rs:1043` | `8.0` | `SP_S` | 标签宿主左右两栏之间的缝隙（`let gap = …`） |
| `chrome.rs:192` | `4.0`（Margin 横向分量） | `SP_XS` | 标签栏 `inner_margin` 横向内边距 |
| `chrome.rs:383` | `8.0`（Margin 横向分量） | `SP_S` | 状态栏 `inner_margin` 横向内边距 |

（表格 7 行对应 6 处改动点，`chrome.rs` 两处 `Margin::symmetric` 各只换了横向
那一维——纵向分量不在五档上，见下节，未动。）

## 未处理，留给下次视觉走查

以下裸数字**不在**本次收敛范围，逐条给理由：

### 圆角（另一套刻度，spec §4.6 未定义档位）

`files_panel.rs:407/439/453` `Rounding::same(4.0)`、`files_panel.rs:573`
`Rounding::same(2.0)`、`files_panel.rs:724` `rect_filled(rect, 2.0, …)`、
`chrome.rs:245` `Rounding::same(4.0)`、`chrome.rs:252` `Rounding::same(1.0)`、
`chrome.rs:396` `Rounding::same(1.5)`——圆角是独立于间距的一套视觉刻度，
`spec.md` §4.6 目前没有定义圆角档位，硬凑进 `SP_*` 语义不通（`SP_XS` 是
"间距"不是"圆角半径"，两者恰好都是 4.0 只是巧合，参照
`metrics.rs` 里 `TEXT_EDIT_MARGIN_X` 的同款告诫："不要用 `SP_S` 顶替——
数值恰好相等但语义无关"）。留到下次视觉走查时专门定一套圆角刻度。

### 描边宽度（Stroke width，同样没有专属刻度）

`files_panel.rs:408/440/454/574` `Stroke::new(2.0, …)`/`Stroke::new(1.0, …)`、
`chrome.rs` 内没有独立的描边宽度裸数字——不属于本次两条判据（间距/颜色）中
任何一条，本次不处理，可与圆角刻度一并在下次评审时定档。

### 字号（不是间距刻度覆盖的维度）

`files_panel.rs:696` `FontId::proportional(11.0)`、`files_panel.rs:727`
`FontId::proportional(12.0)`——五档刻度定义的是"间距"，不是"字号"。项目目前
没有字号档位表，不在本任务判据范围内，未处理。

### 列宽/图标/行高几何（专属刻度，混用会带歪间距刻度）

`files_panel.rs:206-222` 的 `W_SIZE`/`W_MTIME`/`W_PERM`/`W_OWNER`/`ROW_H`/
`W_ICON`/`ICON_GAP`/`ICON_LEFT_PAD`，`chrome.rs:130/131/138` 的 `TAB_H`/
`TAB_W`/`TAB_ICON`——这些已经是具名常量，但它们是**列宽/图标/行高**专属
刻度，不是间距刻度。按控制者的明确排除：不把它们换成 `SP_*`，理由是列宽
跟间距是两套刻度，混用会让下次改间距时把列宽也带歪。`ROW_H = 22.0` 若硬凑
到 `SP_L = 16.0` 或某个新档位，会让行变矮/变高、可见行数变化——这是观感
改动，**不在本任务范围内**，需要人工验收后单独决定。

`icon_rect()`/`name_start_x_offset()`/`visible_col_count()`/`name_w()` 内部
使用这些常量的算式，按控制者要求原样未动——对齐守护测试（
`header_and_row_columns_align_at_every_breakpoint` 等）盯着这些函数，改了
会红。

### 已具名但不在五档上的既有间距常量

`chrome.rs:9` `MENU_MARGIN_Y = 3.0`、`chrome.rs:124` `TAB_MARGIN_Y = 3.0`、
`chrome.rs:134` `TAB_UNDERLINE_H = 2.0`——这三个已经是具名常量（不是本任务
瞄准的"裸数字"），但值本身不落在 `SP_XS/S/M/L/XL` 任何一档上。改成最近的
档位（如 `TAB_MARGIN_Y: 3.0 → SP_XS: 4.0`）会让菜单栏/标签栏高度变化
（`menu_px()`/`tab_bar_px()` 直接用它们算高度），是观感改动，本任务不做，
留给下次视觉走查专门评审是否要收敛、收敛到哪一档。

### 与五档不等值的裸数字（收敛会动观感）

`chrome.rs:41` `Margin::symmetric(6.0, MENU_MARGIN_Y)` 的 `6.0`、
`chrome.rs:256` `rect.shrink2(egui::vec2(6.0, 2.0))` 的 `6.0`/`2.0`、
`chrome.rs:383` `Margin::symmetric(8.0, 2.0)` 里未动的纵向 `2.0`、
`chrome.rs:192` `Margin::symmetric(4.0, TAB_MARGIN_Y)` 里未动的纵向
`TAB_MARGIN_Y`——这些都不精确等于任何 `SP_*` 值，收敛到最近档位会让内边距
变宽/变窄，属于观感改动，本任务不做，留给下次视觉走查。

### 尺寸而非间距（数值巧合等于某档 `SP_*`，语义不同）

`chrome.rs:393` `ui.allocate_exact_size(egui::vec2(crate::ui::badge::EDGE_BAR_W, 12.0), …)`
——`12.0` 恰好等于 `SP_M`，但它是状态栏那个语义色小色块的**高度**（尺寸），
不是两个元素之间的间距。比照控制者对 `W_ICON` 一类列宽/图标几何常量的
排除，这里同理不动，留给下次跟行高/图标尺寸刻度一并定档时处理。

### 比例系数与范围值（不是间距）

`files_panel.rs:903` `DEFAULT_SIDEBAR_W = 360.0`、`files_panel.rs:945`
`width_range(280.0..=640.0)`、`files_panel.rs:957` `h * 0.4`（本地/远端栏
高度比例，`0.4:0.6` 是刻意取舍，有守护测试
`stacked_local_gets_roughly_forty_percent_of_the_stacked_height` 盯着）——
均不是间距刻度覆盖的维度，未处理。

### 测试代码

`files_panel.rs:1105` 之后、`chrome.rs:434` 之后是 `#[cfg(test)]` 模块，
里面的坐标/时间戳字面量（如 `render(press(src, 1.0, true), …)` 的
`1.0`、`egui::pos2(200.0, 200.0)`）是测试夹具数据，不是产品视觉规格，
不在两条判据范围内，未处理。

## 验证

```
cargo test --workspace   # 全绿：1041 + 236 + 67 + … 全过，0 failed
cargo clippy --workspace --all-targets -- -D warnings   # 无输出（除已知的 russh future-incompat 提示，与本次改动无关）
cargo fmt --check        # 干净
```
