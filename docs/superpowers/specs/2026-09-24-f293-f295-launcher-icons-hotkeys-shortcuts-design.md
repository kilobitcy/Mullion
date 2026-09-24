# F293 / F294 / F295 设计:启动页会话图标 · 可配置本地热键 · 快捷键一览重排

> 来源:2026-09-24 grill-me 会话 + 复核修订。三条实报一次做完,三个 commit,发一版 v0.1.118。
> 实现 plan:`docs/superpowers/plans/2026-09-24-f293-f295-launcher-icons-hotkeys-shortcuts.md`。

## 三条实报

1. **F293** 启动页(项目列表)会话列没有图标 —— 项目列有、会话管理器左栏有,唯独这一列是光秃秃的两行字。
2. **F294** 抽屉热键 `` Ctrl+` `` 改成默认 `` Ctrl+Shift+` ``,并且能在设置里改。
3. **F295** 设置里的快捷键一览有毛病:Esc 出现两次、缺了一堆真实存在的键、按 scope 平铺读不出结构。

## 决策表

每条决策都是用户在 grill 里拍板的,复核阶段推翻的已按修订后写。

### F293 启动页会话列图标

| # | 决策 | 理由 / 否掉的备选 |
|---|---|---|
| D1 | 会话列每行**固定留一个图标槽**,有图标画、没有留空,不画任何占位符 | 否掉「首字母圆点」等回退:会话管理器左栏就是留空,两处不该长两副面孔 |
| D2 | 几何**对齐同屏的项目列**:图标左沿 24、边长 28、文字左沿 58(`project_row::ICON_X / ICON_SIDE / TEXT_X`),常量改 `pub(crate)` 直接复用,不另抄一份 | 否掉照抄会话管理器的 24/32/48:三列并排,项目列与会话列的文字左沿错开 10 点比缺图难看 |
| D3 | 图标来源 `lists.appearance.get(session_id)`(`AppearanceCache`,已含会话→分组继承);底色 `should_paint(a, ColorTarget::ListItem)`,与会话管理器左栏同源 | 不做 edge bar:启动页没有选中态 |
| D4 | 横排(`Columns`)与竖排(`Stacked`)走同一段绘制代码 | 本来就是同一个 `sessions_column` |

### F295 快捷键一览重排

| # | 决策 | 理由 / 否掉的备选 |
|---|---|---|
| D5 | 整张表升级成**结构化数据**:每行的键是机器可读的 `Keys`(单个 `Chord` / 多个 `Chord` / `Ctrl+1…N` / 纯文字如「Shift+拖动」),显示文本**从数据生成**,不再手抄 | 撞键检测与 F294 的改键冲突判定必须比对结构,比字符串会被「Ctrl+Shift+C」vs「Ctrl+Shift+c」这种漂移骗过 |
| D6 | `Chord` 类型放 **mullion-store**(`hotkeys.rs`):四个修饰键 bool + `KeyName` 枚举 | 它要落盘(F294),store 是唯一能持久化它的层;app 只依赖 store,方向合法 |
| D7 | `KeyName` 键域:字符键 `Char(char)`(存小写)、`F(1..=12)`、Tab、PageUp、PageDown、Home、End、Up、Down、Left、Right | 覆盖所有现实快捷键;Enter/Esc/Backspace/Space **不进键域**(它们裸按有终端语义,带修饰又是终端控制键,没有安全的绑法) |
| D8 | TOML 里 `Chord` 写成一行字符串 `"ctrl+shift+`"`(修饰键小写、`+` 连接、末段是键名;键名 `f6`/`tab`/`pageup`/`up`…;键本身是 `+` 时写 `"ctrl++"`) | 内存里是结构体(用户决定),文件里给人读、给人手改;解析严格,解析失败按现有 settings 的「损坏」路径处理 |
| D9 | **补齐**现实存在的全部快捷键,一条不落:文件面板的 Enter / Backspace / F5 / Tab / Delete / Shift+Delete / F2 / ↑↓ / 字母定位、F6 换焦点、命令抽屉、远端栏搜索条的 Esc | 手抄表漏项的失效方式(`every_module_that_has_shortcuts_is_represented` 只守 scope 不守行) |
| D10 | 排版**分小节**(通用 / 标签 / 终端 / 文件面板 / 会话管理器 / 标注模式 / 项目 / 命令抽屉),节内两列:键 · 作用 | 原来的三列(键/范围/作用)里「范围」一列信息量低,拆成小节标题后省一列 |
| D11 | **只合并 Esc**,写死在「通用」节,文案**按事实写**:「退出标注模式;关掉会话管理器 / 恢复现场 / 换节点 / 选项目 / 文件对话框 / 远端栏搜索条」 | 复核发现「Esc 关掉当前弹窗」是假的:设置 / 项目管理 / 分组管理 / 导入 / 迁移包 / 解锁 / 标签属性 / 编辑器 / 传输面板 都不认 Esc。其余跨 scope 重名(Ctrl+Shift+N、Ctrl+C/X/V、Ctrl+1…9)**保持各自一行** |
| D12 | 保留并加强守护:同节不撞键、每行非空、每节有行、**Esc 整表恰好一行**、显示文本由 `Chord::display` 生成 | 「Esc 出现两次」就是这次的实报,要有一条测试直接钉它 |
| D13 | 字形白名单登记 `←`(GBK 内,与已登记的 `→ ↑ ↓` 同族) | `Chord::display` 会生成方向键文本 |

### F294 可配置本地热键

| # | 决策 | 理由 / 否掉的备选 |
|---|---|---|
| D14 | 可配范围:`app.rs` 里那 5 个本地热键拆成 **7 条单键动作**:NextTab(Ctrl+Tab)、PrevTab(Ctrl+Shift+Tab)、CloseTab(Ctrl+W)、ToggleFiles(Ctrl+Shift+B)、ToggleFocus(F6)、NewProject(Ctrl+Shift+N)、ToggleDrawer(`` Ctrl+Shift+` ``);**Ctrl+1…9 不碰** | 否掉「只改抽屉一个键」:同形状的 5 个函数各写一遍判定,以后每加一个都得再问一次要不要可配 |
| D15 | 抽屉默认 `` Ctrl+Shift+` ``;旧 `` Ctrl+` `` **彻底不再是抽屉键**,恢复成普通键发给远端 | 否掉「两个都认」:那样设置里「改键」是假的 |
| D16 | 合法绑定:带 Ctrl / Alt / Super 的任意键域内的键,或 **F1…F12 可裸按**;裸字母/数字/符号、仅 Shift、裸 Tab 一律拒绝 | 裸键 / 仅 Shift 会吃掉打字;F 键裸按是通用约定(F6 现状) |
| D17 | 撞键处置:与另外 6 条可配 + 表里全部**不可配硬名单**比对(排除「会话管理器」一节 —— 弹窗开着时热键整体让位,不会撞),撞了**拒绝**并报出占用者 | 否掉「允许撞、后者赢」:撞了之后先到先得的顺序在源码里,用户看不见 |
| D18 | **唯一白名单**:「项目」NewProject 与「文件面板」新建文件夹**共享** Ctrl+Shift+N,靠焦点分辨(现状),表里那一行带 `shared_with_new_project` 标记;只有这一对豁免 | 不豁免的话现状默认值自己就撞了 |
| D19 | 持久化**稀疏**:`Settings.hotkeys: BTreeMap<String, Chord>`,键是动作名(`next_tab` …),**只写与默认不同的**;等于默认的条目在写回时删掉;`settings.toml` 仍是 schema v1,`#[serde(default, skip_serializing_if)]` | store 不认识动作枚举(它只存字符串键),默认值归 app;跨实例合并走现有 `graft_changed`(`hotkeys` 是一个顶层键,整表按「改没改」合并) |
| D20 | 运行时**不建影子状态**:每次按键从 `self.settings` 直接算 `hotkeys::resolve(&settings, &chord)`(7 条,零分配) | 「影子状态」本项目踩了 N 次;7 条比对的成本可以忽略 |
| D21 | 键口径统一走 winit 的 `KeyEventExtModifierSupplement::key_without_modifiers()`(Windows / macOS / X11 / Wayland 都有),取不到再退到 `logical_key` | `logical_key` 对 `Ctrl+Shift+`` ` 给的是 `~`,捕获与匹配两边都会漂;`physical_key` 又不认布局 |
| D22 | 拦截点:`window_event` 里**一个** `bound_hotkey_event` 替代原来 4 个(`files/focus/project/drawer_hotkey_event`),算一次 chord 查一次表;逐动作的门保留:全体 `modal_open` 让位;ToggleFocus 面板不在场不吃键;NewProject 焦点在文件面板让位;**仍在输入分流之前**(T8) | `tab_hotkey_event` 留着只管 Ctrl+1…9(`shell::tabs::digit_hotkey`),Ctrl+Tab / Ctrl+W 从 `tabs::hotkey` 里搬走 —— 不搬的话用户改掉 Ctrl+W 之后它照样关标签 |
| D23 | 改键交互:**按钮式捕获态** —— 表里可配的 7 行,键那一格是按钮;点下去变成「请按下新组合键…」,下一个非修饰键按下即捕获;Esc 取消捕获;修饰键单独按下忽略;不能作快捷键的键(Enter/Space…)给一句提示 | 否掉文本框手填:格式要教、解析要报错 |
| D24 | 拒绝时机:**捕获那一刻**就做合法性 + 撞键判定,不合法 / 撞了**不写进草稿**,在表底下红字报出原因(命名占用者);合法则写进草稿,点「确定」才落盘,「取消」整个丢弃(走现有 draft 模型) | 否掉「写进草稿、确定时再拒」:用户看见表里已经改了,点确定却弹错 |
| D25 | 每一条**与默认不同**的行后面出一个「恢复默认」按钮 | 单条恢复比整表恢复贴近使用场景 |
| D26 | 捕获态的拦截是 `window_event` 的**第一道**检查(排在 `annotate_event` 之前);捕获期间所有 `KeyboardInput` 一律吞掉 | 不放最前,`Ctrl+Shift+F` 会先被标注模式截走,用户永远绑不了它(虽然它本来也该被撞键拒绝,但拒绝理由要显示出来,不是静默) |
| D27 | 一张表:F295 的表就是改键的表,不另开一节 | 用户决定 |

### 交付

| # | 决策 |
|---|---|
| D28 | 三个 commit:F293 → F295 → F294(F295 在前是因为 F294 的改键 UI 长在重排后的表上;`Chord` 随 F295 进 store) |
| D29 | 版本 0.1.117 → 0.1.118,走 `release-windows` skill,一版发完 |
| D30 | spec.md 登记 F293 / F294 / F295 三行 |

## 守护测试清单

| 判据 | 测试 | 自证变红 |
|---|---|---|
| 会话行真画出了图标(Mesh + 真纹理 + 有面积),没图标不画 | `launcher::tests::a_session_row_paints_the_icon_of_its_session` | 删掉 `paint_icon` 调用 |
| 有/无图标文字左沿一致,且文字不压图标 | `launcher::tests::the_session_name_starts_at_the_same_x_with_or_without_an_icon` | 文字左沿改回 `SP_S` |
| `Chord` 字符串往返、大小写归一、`+` 键、非法串拒绝 | `mullion_store::hotkeys::tests::*` | 改 parse |
| 显示文本由数据生成 | `hotkeys::tests::display_is_generated_from_the_structure` | 把 display 写死 |
| Esc 整表恰好一行 | `shortcuts::tests::escape_is_listed_exactly_once` | 把标注模式的 Esc 加回去 |
| 同节不撞键(结构比对) | `shortcuts::tests::no_two_rows_claim_the_same_chord` | 改任意一行成同节另一行 |
| 每节都有行 | `shortcuts::tests::every_module_that_has_shortcuts_is_represented` | 删一节 |
| 设置弹窗画出了小节标题与真实键 | `settings::tests::the_shortcut_table_is_grouped_into_sections` | 删 `section_header` |
| `hotkeys` 稀疏落盘、只写非默认、读回一致;`graft_changed` 只带走改过的那一键 | `mullion_store::settings::tests::hotkey_overrides_survive_a_round_trip_and_stay_sparse` / `..::a_rebound_hotkey_is_grafted_without_touching_the_rest` | 去掉 `skip_serializing_if` / 改 graft |
| 默认表:抽屉 = `` Ctrl+Shift+` ``,旧 `` Ctrl+` `` 解析成 None;Ctrl+B 不是文件键 | `hotkeys::tests::the_drawer_default_is_ctrl_shift_backtick_and_the_old_key_is_free` / `ctrl_b_is_left_to_tmux` | 改默认 |
| 合法性 | `hotkeys::tests::legality_*` | 放开裸键 |
| 撞键:与终端/文件/标签 Ctrl+1…9 撞拒绝、会话管理器节不算、Ctrl+Shift+N 白名单只对 NewProject | `hotkeys::tests::vet_*` | 去掉 `shared_with_new_project` 判断 |
| 键口径:`key_without_modifiers` 为主、`logical_key` 兜底 | `hotkeys::tests::chord_of_key_*` + `app.rs` 源码切片 `the_hotkey_chord_is_read_without_modifiers` | 换回 `logical_key` |
| 拦截在分流之前(T8)、门保留、旧函数不存在 | `app.rs`:`bound_hotkeys_are_swallowed_before_the_input_routing` / `f6_is_gated_on_the_panel_actually_being_visible` / `the_project_hotkey_yields_ctrl_shift_n_back_to_the_files_panel` / `tab_switching_never_reconnects` | 挪调用点 |
| 捕获拦截排第一、Esc 取消、修饰键忽略、拒绝不进草稿 | `app.rs`:`hotkey_capture_is_the_first_thing_window_event_checks`;`settings::tests::clicking_a_bound_chord_enters_capture` / `restore_default_only_shows_for_changed_rows`;`hotkeys::tests::vet_rejects_before_anything_is_written` | 调用点后挪 / 捕获时先写再判 |
| `tabs::digit_hotkey` 只认 Ctrl+数字 | `tabs::tests::ctrl_digits_jump_and_everything_else_is_left_alone` | 加回 Tab 分支 |

## 人工验收清单(无头环境验不了)

- 启动页会话列:有图标的会话画出图标(含继承分组图标的),无图标的行文字左沿与有图标的对齐,且与项目列文字左沿对齐;竖排(窄窗)同样。
- 设置 → 快捷键:分节可读,Esc 只出现一次;表在 220 高度里能滚到底(F217 棘轮:打开弹窗时窗口不应自己长高)。
- `` Ctrl+Shift+` `` 开/关抽屉;`` Ctrl+` `` 现在发给远端(在 bash 里应无反应或按 readline 处理),**不再**开抽屉。
- **Windows 输入法风险**:`Ctrl+Shift` 是 Windows 默认的「切换键盘布局」组合(装了多套布局时)。若按 `` Ctrl+Shift+` `` 触发了布局切换而抽屉没开,记录下来 —— 那就是用户该在设置里改键的场景,而不是我们的 bug;但要确认改键之后能生效。
- 改键:点某行的键 → 「请按下新组合键…」→ 按 `Ctrl+Alt+D` → 表里即时更新 → 确定 → 重启 exe 仍生效;`settings.toml` 里只多了 `[hotkeys]` 下的一行。
- 改键拒绝:按 `Ctrl+Shift+C` → 红字「已被「复制选区」(终端)占用」,表不变;按裸 `x` → 红字说明需带修饰键;按 Esc → 退出捕获。
- 恢复默认:改过的行后面有按钮,点完消失;`settings.toml` 里那一行随之消失(确定后)。
- 改掉 Ctrl+W 之后,Ctrl+W 应发给远端(bash 删词),不再关标签。
- 输入法:捕获态下按中文输入法的切换键(Shift)不应触发任何绑定。
