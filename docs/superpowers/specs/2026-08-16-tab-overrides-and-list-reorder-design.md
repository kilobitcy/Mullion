# 切片 K：标签本地覆盖 + 会话列表拖拽排序（设计）

日期：2026-08-16 · 起点分支 `feat/j-files-panel-tabs`（v0.1.43）

来源：F100 标注模式导出的三条修改点

1. 标签设置颜色后，变化的是**标签的背景色**，和下方节点无关
2. 标签设置名称后，变化的是**标签的名称**，和下方节点无关
3. 会话管理器左栏中，节点可以拖拽重新排序

前两条是对切片 J（`d122004` / `d6e8e6d`）那个决定的**反转**：当时 `ui/tab_props.rs`
刻意把标签属性写回会话记录，理由是「运行期覆盖与 F37 布局持久化语义打架」。
现在的结论是：覆盖就该是运行期的，**且不进布局快照**——打架的那一半直接不存在了。

新增 spec 编号（实现时一并写进 `spec.md`，F120 是当前最大编号）：

| 编号 | 一句话 |
|---|---|
| F121 | 会话列表手动排序：左栏拖拽调整会话顺序，可跨组 |
| F122 | 标签本地覆盖：标签的名称/颜色只作用于该标签自身，不写回会话记录 |

同时修订两条既有条目：F36 补一句「标签支持本地改名/配色，不落盘」；
F62 的落点表述要改——标签色的画法从「底部横杠」变成「整块背景」。

---

## 1. 已定死的决策（本次问答的产出）

| # | 决策 | 理由 / 影响 |
|---|---|---|
| D1 | 标签属性 = **纯运行期覆盖**，一行 store 都不碰 | 会话列表那条、pane 标题条、状态栏一律不受影响 |
| D2 | **不持久化**：关窗口即丢，重开退回会话名/会话色 | 零 schema 改动，F37 布局快照不新增字段 |
| D3 | 覆盖**挂在标签上**，不挂在会话 id 上 | 同一会话开两个标签可以分别叫「日志」「构建」、上不同颜色 |
| D4 | 颜色画成**整块背景**，前景文字按对比度自动取深/浅 | 推翻 F62 当初「只画横杠」的取舍；对比度由 `readable_fg` 兜底，不靠色板纪律 |
| D5 | 会话色与覆盖色**同一条视觉通道**，覆盖色优先 | 一个标签上不出现两种颜色；弹窗「清除」= 退回会话色 |
| D6 | 任何标签都能改名配色，**快速连接标签不再置灰** | 覆盖不需要会话记录了，禁用的前提消失 |
| D7 | 拖拽范围 = **组内排序 + 跨组拖动**；分组自身顺序不动 | 跨组拖 = 顺带替代右键「移动到分组」 |
| D8 | 顺序真值 = `sessions.toml` 的**数组顺序**，不加 `order` 字段 | 现在 `group_sessions` 归桶后就是按数组顺序渲染的，零 schema 改动 |
| D9 | 搜索非空 / `Icons` 密度档时**禁用拖拽** | 有行被藏起来时「可见顺序 ≠ 真实顺序」，落点是歧义的 |
| D10 | 活动标签底部横杠**保留**，有色时用 `readable_fg` 画 | 「哪个是活动标签」不该只靠透明度差别去认 |

---

## 2. A 组：标签属性改成运行期覆盖（F122）

### 2.1 数据

`shell/tabs.rs::Tab<C>` 增两个字段：

```rust
/// F122:用户在这个标签上改的名字。`None` = 用 `title`(连接时拼的那个)。
/// **不进 F37 布局快照**——关窗口即丢是设计,不是欠账(D2)。
pub title_override: Option<String>,
/// F122:用户在这个标签上配的色。`None` = 退回会话色(`ColorTarget::Tab`)。
pub color_override: Option<Rgb>,
```

`Rgb` 取 `mullion_term::snapshot::Rgb`（`theme::parse_hex` 的返回类型）——
shell 层不引 egui 类型，这条方向约束不破。

新增 `Tab::display_title(&self) -> &str`：`title_override` 优先，否则 `title`。
`Tabs::replace`（占位标签重连）只换 `title`/`content`，**不动两个 override**。

### 2.2 弹窗

`ui/tab_props.rs`：

- `TabPropsDraft` 的键 `session_id: SessionId` → `tab_id: TabId`
- `TabPropsAction::Save { tab_id, name, color: Option<Rgb> }`
- **删掉「应用到」那四个勾选框**与 `targets` 字段：覆盖只作用于标签自身，
  落点选择没有意义（`COLOR_TARGET_LABELS` 仍由会话编辑器用，不删）
- 头部那段「改的是会话记录本身」的注释整段重写

### 2.3 接线（`app.rs`）

- 打开弹窗：初值取**当前有效值**——`display_title()` 与「覆盖色 ?? 会话 Tab 色」，
  不再去 `store.list()` 里捞记录
- 施加：直接写 `self.tabs` 里那个 tab 的两个 override 字段，`ui_dirty = true`
- **删掉** `apply_tab_props_save`、`sync_tab_titles_for_session` 两个自由函数，
  以及它们的测试（`apply_tab_props_save_only_touches_name_and_color_leaves_everything_else_alone`、
  `saving_tab_props_retitles_every_tab_pointing_at_the_same_session_not_just_the_active_one`）
- **删掉** `touched_store` 里的 `tab_props_save` 那一项——不再写盘。
  守护它的那条测试 `renaming_a_tab_counts_as_touching_the_store_so_the_look_is_recomputed`
  **整条删除**，不改成「断言不含」——那是拿常量断言常量的重言式，护不住任何东西。
  「一行都不许写 store」这条由下面测试计划里那条逐字段等价的用例来守
- **保留** `Modal::TabProps`：弹窗里敲的字仍然不能漏给远端 shell（T8）

### 2.4 右键菜单

`chrome.rs::one_tab` 的 `add_enabled(has_session, …)` 改回 `ui.button(…)`，
连同那句「这个标签没有对应的会话记录」一起删。`TabView::session_id` 字段
本身**保留**——`appearance` 查表还要用。

---

## 3. B 组：标签整块背景色（F122 视觉部分）

### 3.1 两个新纯函数

```rust
// theme.rs
/// 铺在 `bg` 上的文字该用深色还是浅色。用现成的 `contrast_ratio` 实算,
/// 在两个候选里取比值大的那个 —— 不按亮度阈值拍脑袋。
pub fn readable_fg(bg: Rgb) -> Rgb;

// ui/chrome.rs
/// 标签底色。`color` = 有效色(覆盖 ?? 会话色),`None` 时退回主题两档底色。
pub fn tab_fill(color: Option<Color32>, active: bool, t: &Theme) -> Color32;
```

`tab_fill` 的规则：

| 有效色 | 活动 | 底色 |
|---|---|---|
| 有 | 是 | 满色 |
| 有 | 否 | 满色按固定比例混 `bar_tool`（降一档，与 `list.rs::blend` 同一手法） |
| 无 | 是 | `panel_head`（现状） |
| 无 | 否 | `bar_tool`（现状） |

标题文字色：有效色存在 → `readable_fg(底色)`；否则维持现状
（活动 `fg_strong` / 非活动 `fg_muted`）。图标底色跟着走同一份。

活动标签底部那条横杠**保留**（D10）：有效色存在时用 `readable_fg(底色)` 画，
否则维持现状的 `accent`。它现在的职责是「哪个是活动标签」这一条，不再兼职表达节点色。

### 3.2 有效色的来源

`TabView` 新增 `color: Option<Color32>`，由 `app.rs` 构造时算好：

```
color_override.map(c32)  ??  appearance.and_then(|a| should_paint(a, ColorTarget::Tab))
```

`one_tab` 里不再自己调 `should_paint`——一个落点一份判定，避免两处分叉。

---

## 4. C 组：左栏拖拽排序（F121）

### 4.1 store 侧

`mullion-store::Vault` 新增：

```rust
/// F121:把一条会话挪到 `before` 之前(`None` = 该组末尾),顺带改组。
/// 组内排序与跨组拖动共用这一个入口 —— 拆两个函数会让「跨组时位置怎么算」
/// 有两份实现。
pub fn move_session(
    &mut self,
    id: SessionId,
    group: Option<GroupId>,
    before: Option<SessionId>,
) -> Result<(), StoreError>;
```

语义（全部要有单测）：

- 数组里先 `remove` 再 `insert`，`before` 指向的记录**在移除后**重新定位
  （先算下标再移除会差一位）
- `before == Some(id)` 自身 → no-op，不报错
- `before` 指向不存在的记录 → 落到该组末尾（不报错：并发删除下这不是异常）
- `id` 不存在 → `Err`
- `group` 与 `before` 所属组不一致时，**以 `group` 为准**（UI 保证一致，store 不猜）
- `modified_at` **不更新**：换个位置不算改了这条会话的内容

落盘沿用 `save()`（数组顺序即 TOML 序列化顺序，无新字段）。

### 4.2 落点判定纯函数

`ui/session_manager/reorder.rs`（新文件）：

```rust
pub struct ReorderIntent {
    pub id: SessionId,
    pub group: Option<GroupId>,
    pub before: Option<SessionId>,
}

/// 松手落在哪 → 什么意图。`None` = 这一放什么都不做。
pub fn drop_on_row(
    dragged: SessionId,
    over: SessionId,
    over_group: Option<GroupId>,
    next_in_group: Option<SessionId>,   // 被悬停行在**该组内**的下一条
    upper_half: bool,
) -> Option<ReorderIntent>;

/// 落在分组头上 → 插到该组末尾。折叠的组、空组都靠这条进得去。
pub fn drop_on_group(dragged: SessionId, group: Option<GroupId>) -> ReorderIntent;
```

`drop_on_row` 的判据：上半 → `before = Some(over)`；下半 → `before = next_in_group`
（组内最后一行的下半 = `None` = 组末尾）。拖到自己身上返回 `None`。

### 4.3 UI 侧（`list.rs`）

- 行的 `Sense` 从 `click()` 改 `click_and_drag()`
- 外层 `ScrollArea` 必须 `.drag_to_scroll(false)`——**F58 踩过**：不关的话
  视口那个吃 drag 的部件会把按在行上的那一下抢去当滚动手势，行的
  `drag_started()` 恒假，拖拽功能安静地不存在
- 载荷 `DragSession(SessionId)`，走 `dnd_set_drag_payload` / `dnd_release_payload`
  （同 F58 的 `DragFrom`）
- 插入线：悬停行的上/下边缘画一条 `accent` 横线，宽度取行宽。**必须在松手前可见**
  （设计 D9「规则先于动作可见」）
- 分组头也接 `dnd_release_payload`，走 `drop_on_group`
- **D9 门控**：`!ui_state.search.is_empty() || d == Density::Icons` 时不挂拖源，
  行的 hover 提示追加一句「搜索/图标档下不能拖动排序」
- 意图落到 `ui_state.reorder_request: Option<ReorderIntent>`，
  `app.rs` 在既有 `touched_store` 段施加：`store.move_session(..)` + `save()`
  + `appearance.rebuild()`（跨组会改继承来源，颜色/图标可能跟着变）

---

## 5. 测试计划

| 层 | 测试 | 自证会变红的改法 |
|---|---|---|
| store | `move_session` 组内前移/后移/跨组/自身 no-op/悬空 before/不存在 id | 把 `remove` 后的重定位改回先算下标 |
| store | `move_session` 不动 `modified_at` | 在函数里补一句写时间 |
| app（纯函数） | `drop_on_row` 上半/下半/组内末行/拖自己 | 把 `upper_half` 分支交换 |
| app（纯函数） | `readable_fg` 在 8 个预设色板上对比度 ≥ 4.5:1 | 改成固定返回 `fg_strong` |
| app（纯函数） | `tab_fill` 四象限 | 让非活动有色标签直接返回满色 |
| app | 同一会话两个标签，改其中一个的名字/色，另一个纹丝不动（D3 的核心判据） | 把覆盖改回按 `session_id` 匹配 |
| app | 保存标签属性后 `store.list()` 逐字段等价（D1：一行都不许写） | 恢复 `apply_tab_props_save` 调用 |
| app | 有效色取值序：覆盖 > 会话 `ColorTarget::Tab` > 无 | 交换两者顺序 |
| app | `Modal::ALL` 仍含 `TabProps`（T8 不许因为这次重构掉） | 从 `ALL` 里删掉它 |
| app | 搜索非空时行不设拖拽载荷（D9） | 去掉门控条件 |
| app | `list.rs` 的 `ScrollArea` 关掉了 `drag_to_scroll`（读回源码文本，同既有做法） | 删掉那一行 |

「绿」= `cargo test --workspace` 全过 **且** `clippy -D warnings` 无输出 **且** `fmt --check`。

## 6. 人工验收清单（无头环境验不了的）

- [ ] 标签整块上色后，8 个预设色 + 几个自选深/浅色下标题文字都读得清
- [ ] 非活动有色标签与活动有色标签能一眼分出哪个是活动的
- [ ] 改标签名/色后：会话管理器左栏那条、pane 标题条、状态栏**都没变**
- [ ] 同一台机开两个标签，分别改名改色互不干扰；关掉再开退回会话名
- [ ] 快速连接开的标签也能改名配色
- [ ] 左栏拖拽：组内换位、拖进另一个组、拖到折叠组的头上、拖到空组
- [ ] 拖动过程中插入线位置与松手后的落点一致
- [ ] 搜索框有内容时拖不动，且提示说得清为什么
- [ ] 排序结果重启后还在

## 7. 交付

改动全部落在 `mullion-app` + `mullion-store` → 走 `CLAUDE.md` 的交付约定一条龙：
bump `0.1.44` → 跑绿 → 交叉编译 + objdump 依赖验收 → 发 GitHub Release（标题纯 `v0.1.44`）
→ 报链接 + sha256 + 上面这份验收清单。

提交切分（一件事一笔）：

1. `feat(store): 会话列表手动排序的数据入口 (F121)`
2. `feat(app): 标签的名称/颜色改成只作用于本标签的运行期覆盖 (F122)`
3. `feat(app): 标签色改画整块背景,前景按对比度自动取色 (F122/F62)`
4. `feat(app): 左栏拖拽排序 —— 组内换位与跨组拖动 (F121)`
5. `docs: spec 补 F121/F122,修订 F36/F62 的落点表述`
6. `chore: 版本 0.1.44(…)`
