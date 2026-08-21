# IME 寿命修复 + 文件面板多选可见性(F149~F151)

> 2026-08-21 · 起因是 v0.1.56 实机报的两个现象:
> ①「用着用着中文就打不出来了,重启才好」 ②「SFTP 想按 Ctrl 多选一起拖」

## 现象与根因

### 现象 1:中文输入用一段时间后永久失效

用户描述:刚运行 exe 中文正常;用一阵之后,在 ubuntu cli / tmux / Claude Code 对话框
里都打不出中文,**按 Windows 的中英文切换键毫无反应**;感觉与切换节点、切换分屏有关,
但复现路径说不清。重启 exe 恢复。

根因在 `egui-winit 0.30.0` `src/lib.rs:848`:

```rust
let allow_ime = ime.is_some();          // 本帧 egui 里有没有文本框在组字
if self.allow_ime != allow_ime {
    self.allow_ime = allow_ime;
    window.set_ime_allowed(allow_ime);  // ← 关掉的是整个窗口的 IME
}
```

`egui_state.allow_ime` 初值 `false`,而我们在 `resumed` 里调过一次
`window.set_ime_allowed(true)`(`app.rs:5689`)——**两边的账本从一开始就不一致**。
于是:

| 步骤 | egui 记账 | 目标值 | 实际调用 | 窗口 IME |
|---|---|---|---|---|
| 启动 | false | false | 不调 | **true**(我们设的) |
| 点进任意 egui 输入框 | false→true | true | `set_ime_allowed(true)` | true |
| 焦点离开那个输入框 | true→false | false | **`set_ime_allowed(false)`** | **false,从此不再发 `WindowEvent::Ime`** |

「任意 egui 输入框」在本项目里遍地都是:换节点弹窗的搜索框、文件面板路径条、
双击标签改名、会话管理器/设置里的每个字段。这解释了用户「说不清怎么触发」——
凡是碰过表单就中招。**没有自愈路径**:此后 egui 记账恒 `false`,除非再点进一个
输入框再离开——而那一进一出的净效果还是 `false`。

Windows 上 IME 被 `set_ime_allowed(false)` 禁用后,输入法切换键确实完全没有反应,
与用户描述逐条吻合。

这是 T8 的同族坑:**egui 的焦点系统把宿主自己的输入通道当成"没人要"给关了**。

### 现象 2:Ctrl 多选"按了没反应"

逻辑层其实早就有:

- `files/state.rs:250 click_row(name, ctrl, shift)` —— Ctrl 切换单条、Shift 闭区间,
  且有一批单测钉着(`ctrl_click_toggles_one_row_without_touching_the_rest` 等)
- `files_panel.rs:778` 读的是 `m.command`(egui 已把 macOS ⌘ 归一化过来)
- `drag.rs` 的载荷只带「从哪栏拖的」,松手时现取源栏选中集;
  `app.rs:3503 start_transfer_into` 走 `picked_entries()` —— **多选拖拽本来就是整批传**

缺的是**看得见**:`row()` 画选中底色用的是 `t.sunken_bg = #0e1018`,而面板底
`t.panel_bg = #14161f` —— 选中行比背景还暗 6 个亮度单位,在笔记本屏上人眼不可分辨。
用户连**单选**的高亮都没见过,自然认为这个功能不存在。

拖拽也没有任何跟随反馈:`Response::dnd_set_drag_payload` 只挂载荷、不画预览,
拖起来指针底下空空如也,分不清"拖没拖着"和"拖了几项"。

### 现象 3(顺带发现):IME 提交双写

`app.rs:6666` 的输入分流里,`is_kbd` 只匹配 `WindowEvent::KeyboardInput`。
`WindowEvent::Ime` 落进 else 分支被喂给 egui,**然后**又走到 `app.rs:6923` 的
`WindowEvent::Ime` 分支,无条件写进焦点 pane 的 PTY。

后果:在会话管理器 / 标签改名 / 路径条里打中文,那串中文**同时**上屏和被发到
远端 shell。因为混在命令行里不显眼,一直没人报。修法与 F149 在同一段代码上,
一并处理。

---

## F149 —— IME 开关的所有权归宿主

**规则:窗口的 IME 恒为开。egui 不许关它。**

改动落在 `app.rs` 的 `handle_platform_output` 调用点(`app.rs:9260`),
**必须在它之前**:

```rust
// F149:egui-winit 用 self.allow_ime 做去抖,本帧 egui 里没有文本框在组字时
// 它会 set_ime_allowed(false),把整个窗口的 IME 关掉。终端不是 egui 部件,
// egui 永远不知道它也需要 IME —— 于是用户点过一次任意输入框再点回终端,
// 中文输入就永久没了。在它读记账值之前把账本扳成 true,那次调用不会发生。
if crate::input::keep_ime_open(full_output.platform_output.ime.is_some()) {
    a.egui_state.set_allow_ime(true);
}
```

选**拦在源头**而不是"事后再 `set_ime_allowed(true)` 扳回来":后者在文本框
失焦那一帧会连发 `false` → `true` 两次跨进程系统调用,Windows 上有打断
正在进行的组字的风险,而且多一次没必要的 IPC(与 T3 同类顾虑)。

`State::set_allow_ime()` / `allow_ime()` 是 `egui-winit` 的公开 API
(`lib.rs:197`/`202`),正是给"宿主自己也需要 IME"这类混合场景准备的。

**为什么这样就够**:全项目只有两处会动窗口的 IME 开关 —— `resumed` 里那次
`true`,和 egui 这次。后者被拦住之后,窗口 IME 恒开。

### IME 事件按焦点路由(现象 3)

`app.rs:6923` 的 `WindowEvent::Ime` 分支,在写 PTY 之前加焦点判据:
终端持有焦点(`effective_focus()` 为终端且无模态)才写。判据取**既有的**
`shell::input_route`,不另起一套 —— 两套判据迟早会分叉,而分叉的后果是
"某些情况下中文又漏进远端"这种查起来极痛的间歇性 bug。

`Ime::Preedit` 同样要判:不判的话组字中的拼音会画进终端的内联 preedit(F126),
用户在会话名输入框里打字,终端上跟着显示拼音。

---

## F150 —— 多选可见 + 栏底状态行

### 选中高亮

`files_panel.rs:983 row()` 的选中绘制改成:

- 整行底色:`accent` @ 20% alpha
- 行首 2px 实色竖条(`accent` 原色)

新增两个 theme token,值从既有 `accent` 派生,**不引入新色相**
(spec §4.6 UI 视觉规格冻结这一条仍然成立):

```rust
sel_bg:  accent @ alpha 51 (0.2)
sel_bar: accent
```

不选整行填充 + 文字反白(资源管理器口径):那与终端深色外壳的克制风格出入太大,
且选 10 条时半屏都是高饱和色块。

### 栏底状态行

`show()` 末尾加一行,高度 `ROW_H`,`ScrollArea` 相应改 `.max_height(可用高 - ROW_H)`:

- 有选中:`已选 3 项 · 4.2 MB`(目录不计入体积 —— 目录的 `size` 在 SFTP 里
  是元数据大小,加进去只会给出一个没有意义的数)
- 无选中:`12 项`(当前可见行数,受 `show_hidden` 影响,与用户眼睛看到的一致)

文案里的 `·` 已在 `ui::glyphs::VERIFIED`(T9 过关,GBK 内 A1A4)。

这一行同时是**实机验收的硬证据**:它一旦显示 `已选 2 项`,就说明 Ctrl 多选
的逻辑层是好的,剩下的只是视觉问题。

---

## F151 —— 拖拽跟随预览

本栏正被拖时,在 `Order::Tooltip` 层、指针位置旁画一个小胶囊:

- 单项:文件名(超过预算按 `Elide::Middle` 截)
- 多项:`拖动 3 项`

文案抽成纯函数 `drag::preview_label(n: usize, first: &str) -> String`,可单测。
跟随渲染本身依赖真实指针,**无头验证不了**,进人工验收清单。

---

## 测试策略

| 目标 | 测试 | 自证会变红的改法 |
|---|---|---|
| F149 拦截逻辑 | `input::tests::ime_stays_open_when_egui_has_no_text_field` | 把 `keep_ime_open` 的返回改成 `egui_wants_ime` |
| F149 **调用顺序** | 源码级:`set_allow_ime` 那一句必须出现在 `handle_platform_output` 之前 | 两句对调 —— 顺序错了这个修复完全失效,而且照样编译、静默失灵 |
| 现象 3 路由 | `app::tests::ime_commit_goes_to_the_terminal_only_when_it_has_focus` | 去掉焦点判据 |
| F150 高亮 | kittest:选中行存在 `sel_bg` 色的 `rect_filled` + `sel_bar` 色的竖条 | 把颜色改回 `sunken_bg` |
| F150 端到端多选 | kittest:点第 2 行 → Ctrl 点第 4 行 → 下一帧状态行读到 `已选 2 项` | `click_row` 的 ctrl 分支改成 `select_only` |
| F150 状态行体积 | `已选` 文案的纯函数:目录不计体积、无选中显示总条数 | 把目录的 size 加进去 |
| F151 文案 | `drag::preview_label` 单测(1 项显名字、3 项显条数) | 单项分支也走条数 |

源码级守护那条要**切到函数体末尾且带行首缩进**取锚点(第五类恒绿模式:
锚点不带缩进会匹配到测试自己那一行,已被实证两次)。

## 无法自动验证(进人工验收清单)

1. **中文输入不再失效** —— 复现步骤:开 exe → 打开换节点弹窗、在搜索框点一下 →
   关掉 → 回终端打中文。修复前必失效,修复后应正常。再切几次分屏 / 换几次节点。
2. **egui 输入框里的中文不再漏进远端** —— 在标签改名框打「测试」→ 关掉 →
   看终端命令行里有没有多出这两个字。
3. **选中高亮看得清** —— Ctrl 点 3 条,一眼能数出选了几条。
4. **拖拽预览跟手** —— 拖 3 条时指针旁显示「拖动 3 项」。

前两条是本轮的主要交付,**在无头容器里一行都验不了**,必须实机确认。
