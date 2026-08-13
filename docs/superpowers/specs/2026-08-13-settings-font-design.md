# 切片 F:设置弹窗 + 可配置字体(F84 / F21)设计定案

> 上游需求:`spec.md` F84(设置弹窗:终端主题 / 字号滑块 / 快捷键一览)、
> F21(可配置显示字体,随 DPI 缩放,跟随 `ScaleFactorChanged` 动态更新)。
> 表单形态遵 `docs/ui-form-guidelines.md`;色板不重复,见 spec §4.6。

## 0. 范围(已拍板)

**做**:字体族选择(枚举系统字体 + 可手填)、字号滑块、快捷键一览(只读)、
DPI 跟随。

**不做**(用户明确拍板 / spec 明确排除):

- **终端主题不动**。F80 已把色板、终端背景色三处同源、F62 的对比度闸门
  (≥3:1 / ≥4.5:1)全部冻结成常量与单测。加一套浅色主题会让那批闸门大面积
  变红,得重算一批预设色——那是独立一片的工作量。弹窗里主题那一栏**照画**,
  但置灰并写明「暂只有一套」,不留一个看不出为什么点不动的空白。
- **快捷键改键不做**,只做一览(F84 原文就是「快捷键一览」)。
- **不做按会话覆盖字体**。F84 是设置弹窗 = 全局;字体属于「这台机器上看着
  舒服」,不属于「这条会话连到哪」,进会话记录等于让每条会话各存一份 DPI 偏好。

## 1. 落盘:新文件 `settings.toml`

配置目录下与 `sessions.toml` / `layout.toml` 平级的第三个文件。

**为什么不并进 `sessions.toml`**:那要动 schema 版本,而 v8 已经被 F74/F120
预定;更重要的是外观是**全局**的,塞进会话库会让「导出会话给同事」顺带把本机
的 DPI 偏好也导出去(F46 的导出范围会被迫解释这件事)。

```toml
schema_version = 1
font_family = "Cascadia Mono"   # 缺省 = 内置默认
font_pt = 10.0
```

- **读失败降级默认 + 一句 note**,不返回 `Result`(同 `layout.rs`,理由一致:
  外观读不出来不该阻断启动)。
- **写失败要报错卡片**——这里与 `layout.toml` 相反:布局是自动存的「上次的
  场景」,设置是用户刚点了确定的**显式动作**,静默失败 = 改了没生效、且他不知道
  为什么。
- **值域在 `load` 里夹紧**(`font_pt` 夹 6.0~32.0)。手改坏文件写个 `0.5`,
  不夹的话一屏几万个格子,窗口当场卡死;写个 `999` 则一个字符占满屏、
  `cols` 变 0——`Emulator::resize(0, …)` 会按 1 列碾平 scrollback
  (`window_state.rs` 早就为最小化踩过这个坑)。

## 2. 字号存 pt,不存 px

物理像素 `font_px = font_pt * scale_factor * 96 / 72`,与今天 `app.rs:3789`
一字不差。存 px 的话换一块 DPI 不同的屏,字号语义就变了——而 F21 的验收点
之一正是「跟随 `ScaleFactorChanged` 动态更新」。

`ScaleFactorChanged` 今天只记一行日志(`app.rs:4517`),本片改成:重算
`font_px` → `TextLayer::set_font` → 标脏。**不新开尺寸传播路径**(见 §4)。

## 3. 字体族:枚举 + 手填,选完当场量等宽

- 枚举来源 `FontSystem::db().faces()`(fontdb 0.16:`FaceInfo.families:
  Vec<(String, Language)>` + `monospaced: bool`)。同族多个字重会重复出现,
  按族名去重。
- **等宽的排在前面并打标**,非等宽的也留着——`monospaced` 这个标志位来自
  字体自己的 `post` 表,有的等宽字体没置位,一律过滤掉会让用户找不到他装的
  那款。
- 下拉之外**保留手填**:枚举不到的(刚装、或 fontdb 没扫到的目录)还能用。
- 选中后**当场量 `M` 与 `i` 的 advance**,不相等就在字段下方给一条灰字警告
  (不阻止保存)。终端用非等宽字体的症状是整屏错列,而错列看起来像「程序有
  bug」不像「字体选错了」——这条警告是把因果关系摆到用户眼前的唯一机会。
- **字体族不存在时不静默**:cosmic-text 会回退到默认字体,画面看着正常,
  用户只会以为设置没生效。弹窗里当场比对枚举结果,不在列表里就给灰字提示。

## 4. T4:字体变了怎么让远端知道

**什么都不用新做。** `App::compute_geoms` 直接读 `a.text.cell_w/cell_h`
(`app.rs:3005`),每帧 `Present` 分支算完交给 `Workspace::apply_geometry`,
它比对 `last_grid` 不同才发 `window_change`。所以只要 `TextLayer` 的 cell
尺寸更新了,下一帧自然发,链路与「拖窗口 / 开侧栏 / 标签栏出现」完全同一条。

**这条不变量必须被守住**:任何「为了改字体方便」而绕开 `compute_geoms`
自己算一份 cell 尺寸的写法,都会让 T4/F34 出现第二条尺寸传播路径——那正是
`tab_bar_height_reaches_the_remote_as_a_window_change` 当初要挡的东西。

守护:`layout_geometry` 在两组 cell 尺寸下产出不同的 `cols/rows`(纯函数,
无 GPU),外加一条结构守护「`set_font` 之后必须标脏请求重绘」。

## 5. `TextLayer` 怎么改

加两个字段 `family: Option<String>` / `font_px: f32`,`FONT_FAMILY` 常量
降级成 `DEFAULT_FONT_FAMILY`(内置默认,仍是 Google Sans Code)。

```rust
pub fn set_font(&mut self, family: Option<&str>, font_px: f32)
```

重算 `cell_h = (font_px * 1.25).ceil()` 与 `cell_w = measure_cell_w(..)`,
`prepare_panes` 里的 `Attrs::family(..)` 改读字段。

- **不重建 `TextLayer`**:`TextAtlas`/`TextRenderer`/`Viewport` 都要
  `device`/`queue`,而且重建会丢掉整张图集。改字体只是让旧字形变成垃圾,
  下次 `atlas.trim()` 自然回收(`gui-render-gotchas.md` 里 trim 的时机
  已有定论,本片不动它)。
- `FontSystem` **必须留着**:字体枚举也用它,重建一次要重扫系统字体目录。

## 6. 快捷键一览:一张表,外加查重守护

`ui/shortcuts.rs` 一张 `&[Shortcut { chord, scope, what }]` 常量表。

它**注定是手抄的**——快捷键实现散在 `mullion_term::keymap`、
`shell::tabs::hotkey`、`app.rs` 事件分支三处,没有统一注册中心,凭空造一个
反而是为了这张只读表格去重构整条输入链路。诚实的做法是承认它是手抄的,
然后守住手抄**最容易出的那个错**:

- `no_two_rows_claim_the_same_chord` —— 同一个组合键不许在表里出现两次。
  spec F50 里那句「不能用 `Ctrl+Shift+B` 之外的 `Ctrl+Shift+F`,已被 F100
  占用」就是撞键的现场记录;将来加快捷键时,这条会在**加进一览表**的那一刻
  变红,比在用户手上撞车早。
- 表非空、每行三个字段都不为空。

## 7. 弹窗形态

菜单「设置」新入口 → `egui::Window`(不是会话管理器那种自绘外框——那是
F94 为可拖宽做的特例,设置弹窗内容定高,用不着)。

- 过 F100 标注登记(`annotate::mark`),否则标注模式下这块是个洞。
- 三个分节走 `form::section` + `form::grid`:**外观**(字体族 / 字号 /
  主题(置灰))、**快捷键**(只读表)、底部一行「确定 / 取消」。
- 宽度用 `FIELD_W_*` 三档,间距用 `SP_*` 五档
  (`tests/form_guidelines.rs` 会扫)。
- 主题那一栏置灰后**必须给 `on_disabled_hover_text`**(规范 #9)。

## 8. 生效时机:即时预览,取消回滚

字号滑块**拖动即生效**——字号是「看着舒不舒服」,不试怎么知道。「取消」把
进弹窗前的值原样装回去并再 `set_font` 一次。

这意味着拖滑块的每一帧都会发 `window_change`。可接受:远端 tmux 本来就要
处理拖窗口时的连续 resize,频率同量级;而做防抖会引入一个「松手后才生效」
的延迟,反而更难判断字号合不合适。
