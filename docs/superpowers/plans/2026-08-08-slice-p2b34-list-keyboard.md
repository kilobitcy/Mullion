# 会话管理器 UI 走查 · 阶段 3「列表与识别」+ 阶段 4「键盘与反馈」实现计划

> **For agentic workers:** 本计划由原作者本人执行（用户全局配置禁止起 subagent），故采用**紧凑格式**：
> 每个 Task 给出文件、判据、测试名与关键设计决策，代码在实现时按判据写。
> 这偏离 `writing-plans` 的「每步贴完整代码」要求，是有意的取舍 —— 执行者与规划者是同一个上下文。

**Goal:** 收掉走查剩下的 11 条（阶段 3：3/4/6/21/22 + 协议 pill；阶段 4：13/14/15/16/20），
然后一次性升版本 `v0.1.25`、交叉编译、objdump 验收、发 GitHub Release 交人工实测。

**Architecture:** 全部改动落在 `mullion-app`。判据一律抽成**零 egui 的纯函数**（`list::status_of`、
`dedupe::disambiguate`、`tags::parse`、`highlight::segments`、`validate::port`），UI 层只做接线。
`mullion-store` **零改动**（`tags` / `group_id` schema 早就有，缺的只是编辑入口）。
`mullion-ssh` **零改动**（走查 13 的「分阶段进度」已在决策树里剥离）。

**Tech Stack:** Rust 2021 / egui 0.30 / epaint 0.30。

---

## 前置约定

1. **版本与发布。** 走查四阶段共用 `v0.1.25`。阶段 4 的最后一个 Task 完成后：升 patch → 跑绿 →
   交叉编译 → objdump 验收（出现 `libgcc_s_seh-1.dll` / `libwinpthread-1.dll` 即不合格）→
   `gh release create v0.1.25`（标题只能是纯版本号）。
2. **提交粒度。** 每 Task 一次提交，与阶段 1/2 一致（不 squash）。
3. **既有语义不动。** 「继承 vs 显式关闭」的区分（`ProxyChoice::Direct`、`AutomationPrefs` 的
   `None` vs `Some(vec![])`）在本阶段一个字节都不许合并。
4. **变异验证。** 每个新增判据函数都要临时改坏一次、确认对应测试变红，结果写进 commit body。
5. **T8 风险。** 阶段 4 加全局快捷键时，会话管理器的键盘处理**必须**留在 `modal` 分支里
   （`session_manager_open` 已在其中），不得下沉到终端输入路径 —— 否则复现 T8。

---

## 阶段 3：列表与识别

### Task 1 —— 状态点四态 + 色形双编码 + tooltip（走查 4）

**文件:** `ui/session_manager/list.rs`（新增 `Status` + `status_of` 纯函数 + 形状绘制）、
`ui/mod.rs`（`UiState` 加 `connecting` / `connect_failed`）、`app.rs`（三处写入点）。

**判据（纯函数 `status_of(id, connected, connecting, failed) -> Status`）:**

| 态 | 判据 | 颜色 | 形状 |
|---|---|---|---|
| `Connected` | `connected == Some(id)` | `t.ok` | 实心圆 |
| `Connecting` | `connecting == Some(id)` | `t.info` | 空心圆（描边） |
| `Failed` | `failed == Some(id)` | `t.danger` | 实心方块 |
| `Idle` | 其余 | `t.fg_ghost` | 实心圆（小一号） |

优先级 `Connected > Connecting > Failed > Idle` —— 连上了就不该再显示上次的失败。

**色形双编码**是这条的核心：色盲用户只看颜色分不出 ok 绿 / info 蓝，圆 vs 方 vs 空心能分。

**app.rs 写入点:** `spawn_connect` 处置 `connecting = Some(id)` + 清 `failed`；
`UserEvent::ConnectOk` 清 `connecting`；`ConnectErr` 清 `connecting` + 置 `failed = connect_request_last`。

**测试:**
- `list::tests::status_prefers_connected_over_a_stale_failure`
- `list::tests::each_status_has_a_distinct_shape_not_just_a_distinct_color`（断言 `shape_of` 四态互不相同）
- `list::tests::status_tooltip_names_the_state`

---

### Task 2 —— 图标页实时预览（走查 4 后半）

**文件:** `ui/session_manager/fields.rs::appearance()`。

列表行右边缘的语义色竖条已由 `badge::should_paint(ColorTarget::ListItem)` 驱动，
与「图标」页的颜色设置**本来就同源**。缺的是：图标页看不到自己配的东西长什么样。

在「外观」分区末尾加一行**预览**：一个 44px 高的假会话行，复用 `list::session_row` 的绘制
（抽成 `pub(super) fn preview_row(ui, t, name, sub, appearance)`），画出状态点 + 图标 + 名称 + 竖条。

**测试:** `fields::tests::the_appearance_page_previews_the_icon_and_the_color_bar`
（改 `icon_emoji_buf` 后预览里出现该 emoji；设了 `ListItem` 目标色后预览里有对应色的矩形）。

---

### Task 3 —— 协议 pill（决策树新增项，走查没提）

**文件:** `ui/session_manager/list.rs`。

`protocol != Ssh` 时在会话名后面画一个小 pill（`sunken_bg` 底 + `fg_muted` 字，
**不参与 F62 语义配色** —— 那套色是用户自选的语义，协议是客观事实，混在一起两边都读不准）。
ssh 不标：99% 的行都是 ssh，全标等于没标。

**测试:** `list::tests::only_non_ssh_rows_get_a_protocol_pill`

---

### Task 4 —— 重名检测 + 右键菜单补全（走查 3）

**文件:** 新建 `ui/session_manager/dedupe.rs`；`list.rs`（副标题 + 右键菜单）；`editor.rs`（保存前提示）。

**判据 A —— 重名（纯函数 `duplicates_of(rec, all) -> bool`）:** 同名 **且** 同 `user@host:port`
**且** 同协议才算重复。只同名不算 —— 「prod」在两个分组下各有一台是完全正常的用法。

**判据 B —— 区分信息（纯函数 `disambiguate(rec, all, groups) -> Option<String>`）:**
只有真重复时才返回。优先级：分组名 → 端口 → 备注首句。都没有则返回 `None`
（此时两行确实无法区分，提示由 A 负责喊）。

**接线:**
- 列表行副标题：`user@host` 后追加 ` · {区分信息}`。
- 编辑器保存前：重名时在按钮条上方给一条 `warn` 灰字（**不阻止保存** —— 走查原文要求）。
- 右键菜单补：「连接」（普通连接，现有菜单只有「跳过自动化」，缺主项）、「移动到分组 ▸」子菜单。

**移动到分组**复用现有 `SaveIntent` 通道会很重（要构造整份 draft）。改用新意图
`UiState::move_to_group: Option<(SessionId, Option<GroupId>)>`，app.rs 侧读记录、改 `group_id`、`store.update`。

**测试:**
- `dedupe::tests::same_name_but_different_host_is_not_a_duplicate`
- `dedupe::tests::same_everything_is_a_duplicate`
- `dedupe::tests::disambiguation_prefers_the_group_name_then_the_port`
- `dedupe::tests::a_unique_session_gets_no_suffix`
- `list::tests::right_click_offers_connect_and_move_to_group`

---

### Task 5 —— tags chips 编辑 UI（走查 6 / spec F63 欠账）

**文件:** 新建 `ui/session_manager/tags.rs`（`parse` / `format` 纯函数）；`fields.rs::basic()` 的「归类」分区。

`preserved_tags: Vec<String>` 早就在缓冲里透传，`list::matches` 也早就搜它，
只缺编辑入口 —— 搜索框写着「搜索名称 / 主机 / 标签」却没地方设标签，是个断掉的闭环。

**做法:** 一行 chips（每个 tag 一个带 ✕ 的小块）+ 一个输入框。回车 / 逗号 / 空格提交。

**纯函数 `parse(raw: &str) -> Vec<String>`:** 按 `,` / 空白切分、trim、去空、去重（保序）、
每个 tag 截断到 32 字符（防止把一整段粘贴进去撑爆列表行）。

**id 陷阱:** chips 的 ✕ 按钮必须 `push_id((索引, 文本))` —— 只用索引的话，删掉中间一个之后
后面的 chip 继承前一个的 id，egui 的 hover / 点击状态会串（P1-a 踩过同款）。

**测试:**
- `tags::tests::parsing_splits_on_commas_and_spaces_and_dedupes`
- `tags::tests::parsing_drops_empties_and_truncates_absurd_input`
- `fields::tests::removing_the_middle_tag_deletes_that_one_not_always_the_first`（同 env 的删除测试同款）

---

### Task 6 —— 搜索体验：高亮 / 空结果 / 记住上次选中（走查 22）

**文件:** 新建 `ui/session_manager/highlight.rs`；`list.rs`。

**纯函数 `segments(text: &str, query: &str) -> Vec<(Range, bool)>`:** 大小写不敏感的匹配片段切分。
**必须按字符边界切**，不能按字节 —— 中文会话名切一半会 panic。

**接线:**
- 会话行的名称与 `user@host` 用 `LayoutJob` 分段着色，命中段给 `t.accent`。
  （手绘行用的是 `painter().text()`，要换成 `painter().galley()` + 预先 `layout_job`。）
- 全部被过滤掉时，列表区中央画一句「没有匹配「xxx」的会话」+ 一个「清空搜索」按钮。
- 「记住上次选中」：`UiState` 加 `last_selected: Option<SessionId>`，`close_session_manager` **不清**它；
  下次打开会话管理器时若 `editor` 为空则自动切到它。它是**导航状态**，不是编辑状态，
  所以不进 `EditorBuffer`、不参与脏检查。

**测试:**
- `highlight::tests::matching_is_case_insensitive_and_keeps_the_rest_intact`
- `highlight::tests::a_multibyte_name_is_split_on_char_boundaries`（`"生产环境"` 搜 `"产环"`）
- `highlight::tests::an_empty_query_yields_one_unhighlighted_run`
- `list::tests::an_empty_result_set_offers_a_way_out`

---

### Task 7 —— 空态引导 + 新建预填 + 自动聚焦（走查 21）

**文件:** `list.rs`（空态）；`buffer.rs`（`EditorBuffer::new_draft(user)`）；`editor.rs`（聚焦）。

- **空态**（`sessions.is_empty()` 且未搜索）：列表区画「还没有会话」+ 一句引导 + 一个大号「+ 新建」。
  `~/.ssh/config` 导入**不做**（决策树已剥离 = spec F2，单独排）—— 但引导语里不许承诺它。
- **预填**：`port=22` / `protocol=ssh` 已有；补 `user` = 系统用户名。
  取值走 `std::env::var("USERNAME").or_else(|_| std::env::var("USER"))`（Windows 先，Linux 后），
  取不到就留空。**纯函数 `default_user(getenv) -> String`**，注入闭包才能测。
- **自动聚焦**：新建后第一帧 `ui.memory_mut(|m| m.request_focus(name_field_id))`。
  需要给名称框挂显式 id（`fields.rs` 里已有 `field()` helper，加一个可选 id 参数）。
  用 `UiState::focus_name_request: bool` 一次性标志，消费后复位 —— 否则每帧抢焦点，用户没法切到别的框。

**测试:**
- `buffer::tests::a_new_draft_prefills_the_system_user_name`
- `buffer::tests::a_new_draft_leaves_the_user_empty_when_the_environment_has_none`
- `list::tests::the_empty_state_does_not_promise_ssh_config_import`

---

## 阶段 4：键盘与反馈

### Task 8 —— 内联校验红字 + 端口范围（走查 15）

**文件:** `validate.rs`（加 `port(&str) -> Result<u16, PortError>`）；`fields.rs`（三个必填框下方红字）。

F91 已有：按钮禁用 + tooltip + Tab 红点 + 红星。缺的是**字段旁边**的红字。

- `port()`：空 → 用默认 22（不算错）；非数字 → `NotANumber`；`0` 或 `>65535` → `OutOfRange`。
  当前 `build_draft` 里的解析要改成走它，错误文案统一。
- 三个必填框（名称 / 主机 / 用户名）为空时，框**下方**一行 `t.danger` 小字「必填」。
  **只在用户碰过该框之后才显示** —— 一打开新建表单就三行红字，是在骂用户还没开始填。
  加 `EditorBuffer::touched: FieldTouched { name, host, user, port }` 位集。

**测试:**
- `validate::tests::port_rejects_zero_and_out_of_range_but_accepts_empty_as_default`
- `fields::tests::a_required_field_stays_quiet_until_the_user_has_touched_it`

---

### Task 9 —— 标题「•」+ 无修改时置灰（走查 14）

**文件:** `editor.rs`。

脏检查（`is_dirty`）与切换确认 P1-a 已完整实现，只缺两处表达：
- 标题条：脏时名称后加 ` •`（`t.warn` 色）。
- 「保存」/「保存并连接」在**不脏**时置灰，tooltip「没有未保存的修改」。
  **例外：新建草稿恒可保存** —— 新建时表单相对「空基线」天然是脏的，但如果用户什么都没填，
  必填校验已经挡住了，不需要再叠一层。

**测试:**
- `editor::tests::the_title_marks_unsaved_changes`
- `editor::tests::save_is_disabled_when_nothing_changed_but_enabled_on_a_new_draft`

---

### Task 10 —— toast + 错误全文可展开可复制（走查 13 的一半）

**文件:** 新建 `ui/toast.rs`；`editor.rs`（错误卡片）；`ui/mod.rs`（`UiState::toast`）。

**分阶段进度（DNS → TCP → 跳板 → 认证 → shell）不做** —— 要改 `mullion-ssh` 的公开签名，
决策树里已剥离。这条只做另一半。

- **toast**：`UiState::toast: Option<(String, f64)>`（文本 + 到期时刻）。
  用 `ctx.input(|i| i.time)` 取时刻（**不是** `Instant::now()` —— 测试里没法控制它，
  而 egui 的 `time` 可以由 `RawInput` 注入）。到期自动消失，右下角浮层。
  接「复制连接串」成功。**必须 `ctx.request_repaint_after`** —— 不请求重绘的话，
  没有其他输入时 toast 会一直停在屏幕上（陷阱 T3 的反面：帧率节流下没人来擦掉它）。
- **错误全文**：错误卡片当前是单行 `colored_label`，长错误被截断。改成：
  首行摘要（第一行或前 80 字）+ 「详情 ▾」`CollapsingHeader` + 「复制」按钮。
  纯函数 `summarize(msg) -> (String, Option<String>)`。

**测试:**
- `toast::tests::a_toast_expires_after_its_deadline`
- `toast::tests::showing_a_new_toast_replaces_the_old_deadline`
- `editor::tests::a_long_error_is_summarized_with_the_full_text_behind_a_disclosure`

---

### Task 11 —— 快捷键全表（走查 16）

**文件:** 新建 `ui/session_manager/keys.rs`（纯函数把按键映射成 `Action`）；`mod.rs`（接线）；
`editor.rs`（删「取消」按钮）。

**全表：**

| 键 | 动作 | 前提 |
|---|---|---|
| `Esc` | 关闭会话管理器 | 无控件占用（`wants_keyboard_input` 为假）|
| `Ctrl+S` | 保存 | 必填齐 |
| `Ctrl+Enter` | 保存并连接 | 必填齐且无在途拨测 |
| `Ctrl+N` | 新建 | 无 |
| `Ctrl+F` | 聚焦搜索框 | 无 |
| `↑` / `↓` | 在列表内移动选中 | **无控件聚焦时才生效** |
| `Enter` | 连接选中会话 | 同上 |
| `Ctrl+1..4` | 切 Tab | 无 |

**「取消」按钮删掉** —— Esc 已经能关窗，而「取消」的语义（把 editor 重置成空）
跟「关窗」不是一回事，两个都留会让用户以为它们不同。决策树已定：删。

**T8 纪律：** 全部判定发生在 `session_manager::show` 内部（`session_manager_open` 为真时），
**不碰** `app.rs` 的键盘路由。会话管理器是 modal，终端此时本来就收不到键。

**↑↓/Enter 的「无控件聚焦」判据**：`ctx.memory(|m| m.focused().is_none())`。
不加这条的话，用户在主机框里按 ↑ 会跳到别的会话、当场丢字。

**测试:**（纯函数层，注入一个假的 `Keys` 结构）
- `keys::tests::arrows_are_ignored_while_a_text_field_has_focus`
- `keys::tests::ctrl_enter_is_save_and_connect_not_just_save`
- `keys::tests::ctrl_digits_map_to_the_four_tabs_and_nothing_else`
- `mod::tests::escape_closes_the_session_manager`

---

### Task 12 —— 密码存储说明（走查 20）

**文件:** `fields.rs::auth()`。

走查这条**前提是错的**：它假设密码明文存 `sessions.toml`。实际是 `secrets.enc`
（XChaCha20-Poly1305）+ 主密钥进 Windows 凭据管理器。所以这条只做**如实说明**，
不做迁移、不做加固。「不保存，每次连接时询问」推到 F74（决策树已剥离）。

密码框下方一行灰字 + ⓘ（复用 Task 8/阶段 2 的 `icon_inline`）：
> 密码与私钥加密存进 secrets.enc，主密钥交给 Windows 凭据管理器；sessions.toml 里只留引用。

**与 env 那条灰字的对比要成立**：env 说「明文存 sessions.toml」，这里说「加密存 secrets.enc」，
两句话摆在同一个产品里必须都是真的 —— 它们确实都是真的，这正是要写清楚的理由。

**测试:** `fields::tests::the_auth_page_states_where_credentials_actually_live`
（断言文案里同时出现 `secrets.enc` 与「凭据管理器」，且**不含**「明文」）。

---

## 收尾（阶段 4 之后）

1. `cargo test --workspace` 全绿 + `clippy --workspace --all-targets -- -D warnings` 无输出 + `fmt --check`。
2. 升版本 `workspace.package.version` → `0.1.25`，单独一条 `chore:` 提交。
3. `cargo build --release --target x86_64-pc-windows-gnu -p mullion-app`。
4. objdump 依赖验收：出现 `libgcc_s_seh-1.dll` / `libwinpthread-1.dll` = 不合格。
5. `sha256sum` + `gh release create v0.1.25`（`HTTPS_PROXY=http://127.0.0.1:7890`，标题纯版本号）。
6. notes.md 写：四阶段一起修了什么 + 人工验收清单 + sha256 + `Unblock-File` 提示。

## 人工验收清单（无头环境验不了）

1. 状态点四态的**形状**在 12px 下是否真的分得出圆 / 空心圆 / 方块。
2. 搜索高亮的 accent 色在选中行的 `sunken_bg` 底上是否还看得清。
3. tags chips 在窄栏下折行是否难看；删中间一个之后 hover 有没有串位。
4. toast 出现位置是否挡住底部按钮；2 秒是否够读完。
5. 快捷键全表逐条试：特别是 ↑↓ 在输入框里**不该**生效、Esc 在输入框里是否先取消 IME 候选。
6. 中文输入法下 tags 输入框按空格提交 tag —— 会不会把候选词的空格吃掉。
7. 125% / 150% 缩放下重看以上。
