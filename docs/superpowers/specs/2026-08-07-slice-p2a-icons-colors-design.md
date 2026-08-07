# 切片 P2-a：会话图标与语义色（F61 / F62）

> 日期：2026-08-07
> 前置：P1-b 登录后自动化（v0.1.21）、跳板三态 + 私钥正文入库（v0.1.22）均已合入 main
> 路线图上下文：`2026-07-30-session-management-roadmap-design.md` §6

## 1. 目标与边界

给会话一个**一眼可辨的视觉身份**：一个图标 + 一种颜色，出现在会话列表、
pane 标题条、状态栏三处。目的是在多台机器之间降低误操作率——
「我以为我在测试机上敲的这条命令」是本项目最贵的一类事故。

### 本期做

- **F61 图标**：内置形状库（8 个自绘形状）+ emoji
- **F62 语义色**：8 个预设色板 + 自由 hex，落点由 `apply_to` 决定

### 本期明确不做

| 不做的 | 为什么 |
|---|---|
| `IconKind::Custom`（自定义图片） | 要引 image 解码器，顶爆 N6 的 25MB 体积线。枚举变体保留，渲染时降级为不画 |
| F63 标签编辑 UI / 收藏 / 排序权重 | 后两者需 schema 升版；`tags` 的 Merge 继承已就绪但缺编辑入口，单独切片 |
| F64 环境等级 | 需 schema 升版新增字段 |
| 分组级外观 | `GroupRecord.appearance` 继续空置，分组管理器不改。**但解析路径照走继承**，见 §2 |
| 自由 hex 的对比度实时警告 | 额外一套 UI，YAGNI。预设色的达标由测试守死即可 |

**本切片零 schema 改动。** `AppearancePrefs` / `IconSpec` / `IconKind` / `ColorSpec` /
`ColorTarget` 在 v0.1.14（P0-a）就已落地并有 round-trip 测试，本期是纯粹的 egui 侧接线。

## 2. 数据流与继承

会话的 `AppearancePrefs` 本期只由会话自己写（分组不配），但读取路径
**仍然走已有的 `inherit::resolve`**，而不是直接读 `rec.appearance`。

成本为零，而将来给分组接上外观时三处落点一行都不用改。反过来若现在图省事直接读字段，
将来就得**记得**改三处——那种「记得」正是漏掉的来源。

```
GroupRecord.appearance (本期恒 None)
        ↓  inherit::resolve(&[会话, 分组])
SessionRecord.appearance
        ↓
ResolvedConfig { icon: Option<IconSpec>, color: Option<ColorSpec>, .. }
        ↓                    ↓                    ↓
   会话列表行           pane 标题条            状态栏
   (ListItem)          (PaneTitle)          (StatusBar)
```

**继承粒度是 `icon` 和 `color` 各自独立**——`resolve()` 对两者分别调
`resolve_override`，会话设了图标不影响 color 继续从分组继承。
整体覆盖的原子是更小的一级：`IconSpec` 内部的 `kind` + `value`、
`ColorSpec` 内部的 `hex` + `apply_to` 分别是不可拆的整体
（改 `apply_to` 不会让 `hex` 退回继承值）。

`inherit.rs` 里那段 `.map(Some)` 的注释还留了一条约束：当前模型把
「本层未设（继承）」和「本层显式设为空」**折叠成同一个 `None`**，没有区分。
本期的「清除颜色」写的就是 `None`，因此语义是「回到继承」而非「显式无色」——
分组本期恒空，两者表现一致，看不出差别。**F64 或分组外观落地时这会变成真问题**，
届时需要给复合字段加一层「显式清空」标记，不能照抄现在的写法。

### 缓存：解析结果绝不进渲染热路径

`inherit::resolve` 的文档注释明确警告：**结果应由调用方缓存，不要在渲染热路径 /
每帧里重新调用**（本项目陷阱 T3）。会话列表每帧要画几十行，逐行调 `resolve`
就是直接踩这条。

所以 app 侧摘出一个轻量结构，随记录变更（而非随帧）重算：

```rust
/// 从 ResolvedConfig 摘出的外观部分。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Appearance {
    pub icon: Option<IconSpec>,
    pub color: Option<ColorSpec>,
}
```

- **会话列表**：会话管理器打开时、以及任何一次保存 / 删除 / 分组变更后，
  重建一张 `HashMap<SessionId, Appearance>`。
- **pane**：pane 打开时算一次，存进 pane 自身状态；**保存 / 分组变更时同步刷新
  所有已打开 pane 的缓存**——用户完全可以在 pane 开着时改这条会话的颜色，
  只刷列表不刷 pane 就是「列表变了、标题条还是旧色」的陈旧态。
- **状态栏**：取当前聚焦 pane 已缓存的那份，不另算。

缓存类型的**唯一重算入口是 `rebuild()`**，内部带一个代数计数器；
`session_row` 等绘制函数的签名只收 `&Appearance`（已解析结果），
类型层面就调不了 `resolve`——不进热路径靠签名保证，不靠自觉。

三处落点拿到的都是 `&Appearance`，各自决定画不画：
`apply_to.contains(ListItem)` / `PaneTitle` / `StatusBar`。

**`ColorTarget::Tab` 在 UI 上不给勾选项**（F36 标签页排在 v0.5，还不存在），
但读到旧值时不报错、不清除——将来 F36 落地，它自动生效。

## 3. 色板与视觉 token（F62）

8 个预设色进 `theme.rs`，作为一张 const 表而**不是** `Theme` 的 8 个新字段。

理由：`Theme` 的字段都是「UI 自己用的语义色」，主题切换（F84）时整套换；
而这 8 个是**用户挑选的标识色**，存进 `ColorSpec.hex` 后就与主题脱钩了。
换个主题不该让用户标的红变成另一种红。

```rust
/// (显示名, hex, 建议用途 —— 只做 tooltip，不产生任何语义)
pub const LABEL_PALETTE: [(&str, &str, &str); 8] = [
    ("红", "#e06767", "生产 / 高危"),
    ("橙", "#e0955f", "预发 / 待处理"),
    ("黄", "#e0b767", "测试"),
    ("绿", "#7fd99b", "开发 / 安全"),
    ("青", "#67d0d9", "数据库 / 存储"),
    ("蓝", "#7c9eff", "内网 / 常用"),
    ("紫", "#b98bff", "个人 / 实验"),
    ("灰", "#8a90a8", "归档 / 弃用"),
];
```

**按颜色命名而不是按环境命名**（不叫「生产色」「测试色」）：环境语义是 F64 的地盘，
两处都定义「什么是生产」必然会漂移。用途只作为 tooltip 出现，是建议，不是规则。

红 / 黄 / 绿 / 蓝 / 灰刻意复用了 `danger_soft` / `warn` / `ok` / `info` / `fg_dimmer`
的同一组色值——同一套调色逻辑，不引入第二种审美。

**紫故意不取 `accent` 的 `#8b95ff`**：会话列表左 3px 是 accent 选中条，
右条同色就等于两边看不出区别。所以紫挪到 `#b98bff`，并配一条守护测试
防止有人「顺手统一一下」把两者合并。

三条附带决定：

- **hex 解析放 `theme.rs`**：`parse_hex(&str) -> Option<Rgb>`，只认 `#rrggbb`。
  解析失败**当作没设色**（`None`），不报错不崩——配置文件被手改坏不该让列表画不出来。
- **对比度只保证预设**。3px 竖条属非文本元素，WCAG 1.4.11 要求 3:1（不是文字的 4.5:1）。
  8 个预设全部 vs `panel_bg #14161f` 实算断言进测试；自由 hex 用户填 `#000000` 不拦。
- **不进 `apply_egui`**。这 8 色不写进 egui `Visuals`，只由三处落点显式取用，
  避免和现有 widget 状态色互相污染。

## 4. 图标载体与渲染（F61）

### 4.1 Emoji

`value` 存字符本身（`"🔥"`）。编辑器给一个常用 emoji 快捷面板 + 自由输入框。

**限制 `value.chars().count() <= 8`**：ZWJ 家庭序列（👨‍👩‍👧 是 5 个 char）
和旗帜要放得下，同时挡住用户把一整段文字粘进去撑爆行高。
刻意不引 `unicode-segmentation` 做真字素分割——为一个上限校验加依赖不划算。

校验落点分两侧：**写入侧在编辑器**（本切片零 schema 改动，store 不 gate）；
**读取侧**读到手改配置里的超限 value 时，走 §4.3 规则 2 的同一条降级路径——
不画，与「没设图标」表现一致。

> **实机预期（必须写进验收清单）**：epaint **不支持 COLR/CPAL 彩色字形**，
> 它内置的 emoji 字体（`NotoEmoji-Regular` / `emoji-icon-font`）全是黑白轮廓。
> 即使 Windows 上装了 Segoe UI Emoji 也出不来彩色。
> **所以 emoji 在界面上会是黑白剪影。** 这不是 bug，是 egui 的既有限制。

### 4.2 内置形状

`value` 存形状名，8 个：`circle` / `ring` / `square` / `diamond` / `triangle` /
`hexagon` / `star` / `bar`。全部用 `egui::Painter` 自绘，零字体依赖、零体积、
且**能被语义色染色**——这正是它相对 emoji 的价值。

### 4.3 三条渲染规则

1. **形状染色，emoji 不染。** 形状用会话的 `ColorSpec` 颜色填充，未设色时用 `fg_muted`；
   emoji 保持 `fg` 原色——黑白 🐧 染成红色就失去辨识度了。
2. **认不出的值一律不画**，与「没设图标」表现一致。旧配置手改坏、`IconKind::Custom`、
   将来新增的形状名在旧版本上、emoji 超过 8 char——四种情况共用这一条降级路径，
   向前向后都不会崩。
3. **图标只进会话列表和 pane 标题条，不进状态栏。** `IconSpec` 没有 `apply_to` 字段，
   落点由我们定。状态栏那条 `status_text` 是纯函数返回字符串，已有
   `status_text_carries_no_dot_glyph` 守着「字形不进字符串」；状态栏的**颜色**落点
   同理必须是画出来的，绝不能拼进文本——否则那个守护测试会变红，而它是对的。

### 4.4 统一出口 `ui/badge.rs`

新建，理由是这三处落点要共用同一套绘制原语；单个 section 的编辑器控件不值得独立成文件
（其余所有 section 控件都在 `fields.rs`，搬一个出去就是「两套约定混用」）。

```rust
pub fn paint_icon(p: &egui::Painter, rect: Rect, icon: &IconSpec,
                  tint: Option<Color32>, t: &Theme);
pub fn paint_edge_bar(p: &egui::Painter, rect: Rect, side: Side, color: Color32);
fn builtin_shape(name: &str) -> Option<Shape>;   // 纯函数，可单测
pub fn should_paint(a: &Appearance, target: ColorTarget) -> Option<Color32>;
```

`fields.rs` 已 1723 行，确实偏大，但那该是一次单独的重构，不该混进功能切片
（Scope Discipline）。

## 5. 编辑器 UI

「连接」页 `basic()` 里，「归类」section 之后新开「外观」section——
图标和颜色属于「这条会话是谁」，与名称 / 分组 / 备注同类，
不是「高级」，也不值得新开第五个 Tab。

```
外观
  图标   ( ) 无   (•) 形状   ( ) emoji
         ● ○ ■ ◆ ▲ ⬢ ★ ▮              ← 选「形状」时
         [🔥] 常用: 🔥 🐧 🗄 ⚙ 🌐 🔒 …   ← 选「emoji」时
  颜色   ● ● ● ● ● ● ● ●   [自定义 #______]  [清除]
  作用于 ☑ 会话列表   ☑ pane 标题条   ☐ 状态栏
```

**默认勾选 `ListItem` + `PaneTitle`，不勾 `StatusBar`**：多 pane 时状态栏
该显示哪个 pane 的颜色没有确定答案，所以做成可选而非默认。

两条与既有约定对齐的行为：

- **取消勾选所有 target 不清除颜色**。`ColorSpec { hex, apply_to: [] }` 是合法状态
  =「色留着，暂时哪都不显示」。与上一切片跳板「切到无/继承时链条缓冲不清空」
  是同一条原则：用户切走再切回，配的东西还在。
- **保存时保留 UI 未展示的 target**。编辑器只显示三个勾选框，但旧记录的
  `apply_to` 里可能有 `Tab`——保存**不得**按「勾了什么存什么」重建列表把它剥掉，
  否则就违背了 §2「读到旧值不报错、不清除」。做法：缓冲区记住进场时的
  非展示成员，保存时并回去。
- **脏判定零成本接入**。`AppearancePrefs` 已 `derive(PartialEq)`，塞进表单缓冲后
  自动参与既有的脏比对，不需要另写比较逻辑。

## 6. 三处落点几何

| 落点 | 颜色 | 图标 |
|---|---|---|
| 会话列表行（`list.rs::session_row`） | **右**边缘 3px，圆角 2（左 3px 是选中态 accent，各占一边不打架） | 名字左侧 16px，**槽位恒占**——没图标的行也留白，否则文字左边界参差 |
| pane 标题条（`pane_title.rs`） | 左边缘 3px 竖条 | 主机名前 |
| 状态栏（`chrome.rs`） | 文本前一个小色块，**painter 画** | 不画 |

pane 标题条那处特别说明：`pane_title.rs` 的注释警告过两个越界坑
（`Frame` 的 `min_rect + margin` 撑破 `Area`、`set_min_size` 只设下限）。
**在已 `allocate_rect` 的 `full` 矩形里用 painter 画，完全绕开这两个坑**——
不新增任何 widget、不参与布局计算。

## 7. 测试

全部无 GPU、纯逻辑：

1. `builtin_shape` 认全 8 个名字，未知名返回 `None`
2. `parse_hex` 正常 / 坏值 / 缺 `#` / 大小写
3. **8 个预设色 vs `panel_bg` 对比度实算 ≥ 3:1**（WCAG 1.4.11 非文本阈值）
4. 紫不等于 `accent`——防「顺手统一一下」
5. emoji `chars().count() <= 8` 校验
6. `should_paint(&Appearance, ColorTarget) -> Option<Color32>`——
   三处落点共用的判定纯函数，`apply_to` 过滤逻辑在这里被钉死
7. 继承走 `inherit::resolve` 既有路径，补一条「分组空置时会话值直通」
8. **缓存不进热路径**：`rebuild()` 的代数计数器在连续 `get` 后不递增
   （唯一重算入口 + 绘制函数签名只收 `&Appearance`，双保险）
9. **保存不丢 `Tab`**：进场时 `apply_to` 含 `Tab` 的记录，改勾选并保存后
   `Tab` 仍在
10. **回归**：`status_text_carries_no_dot_glyph` 必须仍绿

按上一切片的教训（`subagent-driven-review-lessons`），`should_paint` 和
`builtin_shape` 这两个纯函数要做**变异验红**：故意改坏实现，确认测试确实变红。
只看它们是绿的不算数。

## 8. 人工验收（无头容器验不了）

- emoji 显示为**黑白剪影**是否可接受
- 3px 竖条在真实 DPI 缩放下是否可见（125% / 150% 各看一次）
- CJK 字体回退装上后 emoji 是否被挤掉（`install_cjk_font` 把系统中文字体放在末位回退，
  emoji 字形的选择顺序需实机确认）
- 8 个预设色在真实显示器上是否互相可区分（尤其橙 / 黄、蓝 / 紫）
- 多 pane 且各自配了不同颜色时，pane 标题条的竖条是否真的帮到了辨识

## 9. 顺带订正的 spec 漂移

- **F61~F64 从未进 `spec.md`**，只存在于路线图设计文档。本切片把四条回灌进
  `spec.md` §4.7，否则提交信息和测试名引用的是不存在的编号。
- F60 的「欠账：GUI 缺删除分组入口」已过时——`GroupIntent::Add/Rename/Delete`
  三个入口都已接线。
- F74 验收里的「schema v4→v5」改为 **v5→v6**：v4 被 F40~F44 拿走、
  v5 被私钥正文入库拿走（规则是「谁先落地谁拿号」）。
