# 切片 J：文件面板与标签栏打磨 —— 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修掉文件面板与标签栏上十二项「用起来不对劲」的地方（三个真 bug + 两栏方位 + 图标/属主列 + 编辑器可缩放 + 标签栏交互 + 模态门控结构性化 + 视觉走查），不动数据模型、不动架构不变量。

**Architecture:** 全部改动落在 `mullion-app`。`mullion-core` / `mullion-store` **零改动**，无 schema 变更。改动分两类：(1) **纯逻辑**抽成可单测的函数放 `crate::files` / `crate::ui::file_icon`；(2) **egui 渲染**改动只能靠离屏 harness 或人工验收。模态门控从「一串 `||` 列举」改成「`enum Modal` + `match` 穷尽性」，让编译器当守护。

**Tech Stack:** Rust / egui 0.30 / winit 0.30 / wgpu 23 / russh-sftp 2.4.0

**设计文档：** `docs/superpowers/specs/2026-08-15-slice-j-files-panel-tabs-design.md`（已审核修订，commit `caabf49`）

---

## 通用纪律（每个任务都适用）

1. **每条新测试写完必须变异验收**：把被测代码改坏，确认测试真的变红，再改回来。已知的恒绿模式：`||` 掩盖、常量断言常量（重言式）、分支不可达、冗余防御吃掉变异。每个任务都写明了"自证会变红"的具体改法——照做。
2. **「绿」的定义**：`cargo test --workspace` 全过 **且** `cargo clippy --workspace --all-targets -- -D warnings` 无输出 **且** `cargo fmt --check` 通过。只跑单个 crate 不叫绿。
3. **大输出先落盘再 grep**：
   ```bash
   cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
   ```
4. **提交信息**：中文，一行摘要带 spec 编号，触到领域陷阱的在正文写明跑了哪个守护测试。
5. **动 `app.rs` 事件循环或 `emulator.rs` 前后各跑一次**：`cargo test -p mullion-term`、`cargo test -p mullion-app frame::`（T1/T3/T7）。
6. **写渲染/输入代码前先读** `docs/gui-render-gotchas.md`。

---

## 文件结构

| 文件 | 责任 | 本切片动作 |
|---|---|---|
| `crates/mullion-app/src/app.rs` | 事件循环、标签生命周期、模态门控 | 改 `modal_open`（Task 1）、断线重连分派（Task 4）、标签标题取值与改名接线（Task 10） |
| `crates/mullion-app/src/files/state.rs` | `PaneState` / `Load` | 加 `Load::Disconnected`（Task 4） |
| `crates/mullion-app/src/files/mod.rs` | 文件面板纯逻辑（排序、格式化） | 加 `SortKey::Owner`（Task 7） |
| `crates/mullion-app/src/files/fail.rs` | **新建**：sftp 错误分类（会话级 / 路径级） | Task 4 |
| `crates/mullion-app/src/ui/file_icon.rs` | **新建**：文件类型图标的形状几何 + 绘制 | Task 6 |
| `crates/mullion-app/src/ui/files_panel.rs` | 两栏渲染 | Task 2/3/4/5/6/7/11 |
| `crates/mullion-app/src/ui/chrome.rs` | 菜单栏 / 标签栏 / 状态栏 | Task 9/10/11 |
| `crates/mullion-app/src/ui/tab_props.rs` | **新建**：标签属性弹窗（改名 + 配色） | Task 10 |
| `crates/mullion-app/src/ui/editor_window.rs` | 内置编辑器窗口 | Task 8 |
| `crates/mullion-app/src/ui/mod.rs` | `UiState` / `UiActions` / `build_ui` 接线 | Task 4/10 |
| `crates/mullion-app/src/ui/annotate.rs` | F100 标注模式 | Task 0：加测试用的按名取矩形接口 |

---

## Task 0：给标注模式加「按名取矩形」的测试接口

**为什么排最前**：Task 2/3/5/9 四条测试都要「拿到某个部件本帧的矩形，往它中心注入点击」。现有 `annotate` 模块只有 `spot_paths()`（只给路径字符串）和 `ensure_picked()`（给出图脚本用），**没有**按名取矩形的接口。

**两个必须先知道的事实（已核实源码）：**
1. **`annotate::mark()` 在标注模式关着时直接 return**（`annotate.rs:294-296`），`spots` 永远是空的。**所有依赖矩形的测试必须先 `annotate::toggle(&ctx)`**——不开就是「找不到部件」，且看起来像插桩没铺。
2. **`rect_of` 这个名字已被占用**——`annotate.rs:339` 附近有个私有的 `fn rect_of(r: Rect) -> String`（把矩形渲染成 `(左,上)-(右,下)` 文本）。新接口**必须另起名**，叫 `spot_rect`。

**Files:**
- Modify: `crates/mullion-app/src/ui/annotate.rs`（挨着 `spot_paths` 加）

- [ ] **Step 1：写测试（先红）**

`annotate.rs` 的 `#[cfg(test)] mod tests` 里：

```rust
/// 按语义路径取回本帧登记的矩形。测试要往某个部件的中心注入点击 ——
/// 硬编码坐标一改布局就静默打空,所以必须问 `annotate` 要。
///
/// 顺带钉住那条最容易踩的前提:**标注模式关着时 `mark` 直接 return**,
/// 不开模式就一处也找不到。
///
/// 自证会变红:把 `spot_rect` 改成恒返回 `None`。
#[test]
fn spot_rect_finds_a_marked_widget_but_only_when_the_mode_is_on() {
    let ctx = egui::Context::default();
    let r = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(100.0, 30.0));

    // 模式关着 —— 登记不进去。
    ctx.run(egui::RawInput::default(), |ctx| {
        mark(ctx, "甲/乙", r);
    });
    assert_eq!(
        spot_rect(&ctx, "甲/乙"),
        None,
        "标注模式关着时 mark 应该直接 return"
    );

    // 打开模式再登记。
    toggle(&ctx);
    ctx.run(egui::RawInput::default(), |ctx| {
        mark(ctx, "甲/乙", r);
    });
    assert_eq!(spot_rect(&ctx, "甲/乙"), Some(r));
    // 子串匹配,跟 `ensure_picked` 同一条判据。
    assert_eq!(spot_rect(&ctx, "乙"), Some(r));
    assert_eq!(spot_rect(&ctx, "丙"), None);
}
```

- [ ] **Step 2：跑，确认红**

```bash
cargo test -p mullion-app spot_rect_finds 2>&1 | grep -E "test result|error\["
```
预期：编译失败 `cannot find function spot_rect`。

- [ ] **Step 3：实现**

紧挨 `spot_paths`（`annotate.rs:234`）之后加：

```rust
/// 读回「路径里含 `needle` 的第一处」本帧登记的矩形。
///
/// **给测试用** —— 无头环境里要往某个部件的中心注入点击,硬编码坐标一改
/// 布局就静默打空(点了个空地方,测试却因为别的原因绿着)。判据跟
/// `ensure_picked` 一致:子串匹配、取第一处。
///
/// **前提:标注模式必须开着**(`toggle`)。关着时 `mark` 直接 return,
/// 这里必然返回 `None`。
pub fn spot_rect(ctx: &egui::Context, needle: &str) -> Option<egui::Rect> {
    with_state(ctx, |st| {
        st.spots.iter().find(|s| s.path.contains(needle)).map(|s| s.rect)
    })
}
```

- [ ] **Step 4：跑，确认绿 + 变异验收**

```bash
cargo test -p mullion-app spot_rect_finds 2>&1 | grep "test result"
```
变异：`spot_rect` 改成恒返回 `None` → 测试红。改回。

- [ ] **Step 5：提交**

```bash
cargo clippy --workspace --all-targets -- -D warnings
git add -u
git commit -m "test(app): 标注模式加按名取矩形的测试接口 spot_rect (F100)

后面几条交互测试要往部件中心注入点击,硬编码坐标一改布局就静默打空 ——
点了个空地方,测试却因为别的原因绿着。改成问 annotate 要矩形。

名字不叫 rect_of:那个名字已被一个私有的 Rect→String 渲染函数占着。

守护测试 spot_rect_finds_a_marked_widget_but_only_when_the_mode_is_on ——
它顺带钉住最容易踩的前提:标注模式关着时 mark 直接 return。"
```

---

## 所有交互测试的公共前提

Task 2/3/5/9/10 的测试都要拿矩形，因此**每条都必须在 `ctx.run` 之前调一次 `annotate::toggle(&ctx)`**，且用 `annotate::spot_rect(&ctx, "…")` 取矩形。下面各任务的测试代码已包含这两点——照抄即可，不要自己简化掉 `toggle`。

---

## Task 1：F 组 —— `modal_open` 改成编译器守护的结构性判据

**为什么排第一**：Task 8（编辑器）依赖它才能打字；而它本身是安全问题——三个弹窗开着时用户敲的字**同时被发给远端 shell**（T8）。

**Files:**
- Modify: `crates/mullion-app/src/app.rs:1848-1868`（`modal_open`）
- Modify: `crates/mullion-app/src/app.rs`（测试模块，`7008-7059` 附近两条既有测试的邻位）

**背景事实（已核实）：**
- 现在列举 7 项，漏了三个：`self.editor`（`app.rs:1230`，文件编辑器）、`self.ui.files_dialog`（`ui/mod.rs:301`）、`self.ui.group_manager_open`（`ui/mod.rs:227`）
- `app.rs:1850-1852` 的注释已承认 `group_manager_open` 是既有缺口
- 既有两条守护测试（`app.rs:7018`、`7042`）**扎源码**断言 `modal_open` 函数体里含某个字符串。新写法必须让 `match` 臂**留在 `modal_open` 函数体内**，这两条才继续有效——不要把 `match` 抽到别的方法里

- [ ] **Step 1：写完备性测试（先红）**

加在 `app.rs` 测试模块里，紧挨既有那两条 modal 测试之后：

```rust
/// T8:模态表的**完备性**守护。`Modal::ALL` 少写一个变体编译器不管 ——
/// 这条测试补上那个缺口。
///
/// 与上面两条不同,这条不扎源码:`Modal` 是纯枚举,不需要真 `App` 就能数。
///
/// 自证会变红:从 `Modal::ALL` 里删掉任意一个变体。
#[test]
fn every_modal_variant_is_listed_in_all() {
    // 变体总数。**加变体时这个数字要跟着改** —— 改不动就说明
    // `ALL` 也忘了加。
    const VARIANT_COUNT: usize = 10;
    assert_eq!(
        Modal::ALL.len(),
        VARIANT_COUNT,
        "Modal::ALL 漏了变体:漏掉的那种弹窗开着时,键盘会漏给远端 shell(T8)"
    );
    // 去重后仍是同一个数 —— 防「复制粘贴写重了一项来凑数」。
    let mut seen = std::collections::HashSet::new();
    for m in Modal::ALL {
        assert!(seen.insert(format!("{m:?}")), "Modal::ALL 里有重复项:{m:?}");
    }
}
```

- [ ] **Step 2：跑，确认编译失败**

```bash
cargo test -p mullion-app every_modal_variant 2>&1 | tail -20
```
预期：编译错误 `cannot find type Modal in this scope`。

- [ ] **Step 3：写 `Modal` 枚举 + 改写 `modal_open`**

替换 `app.rs:1848-1868` 整个 `modal_open`，并在它上方加枚举：

```rust
/// 一种会盖住主界面的模态弹窗。
///
/// **存在的唯一理由是让编译器当守护**:`modal_open` 过去是一串 `||` 列举,
/// 新增弹窗时全靠人记得补一行 —— 已经漏过三次(editor / files_dialog /
/// group_manager),后果是那个弹窗开着时用户敲的字**同时被发给远端 shell**
/// (T8)。改成枚举之后,加一个变体就必须在 `is_open` 的 `match` 里给出
/// 「它现在开着吗」,不给就编译不过。
///
/// **加变体时要同步两处**:`ALL` 和测试里的 `VARIANT_COUNT`
/// (`every_modal_variant_is_listed_in_all`)。`ALL` 少写一项编译器管不着,
/// 那条测试就是补这个缺口的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Modal {
    SessionManager,
    About,
    Settings,
    Unlock,
    HostKey,
    Paste,
    Import,
    /// F53:内置文件编辑器。**过去漏了** —— 它是个多行输入框,不算模态
    /// 的话里面压根打不出字(键盘全被判给终端)。
    Editor,
    /// D2:远端写操作确认框(新建文件夹 / 重命名 / 删除 / 改权限)。
    /// **过去漏了** —— 新建文件夹时敲的目录名会同时发给远端 shell。
    FilesDialog,
    /// F60:分组管理器。**过去漏了**(`modal_open` 的旧注释已承认)——
    /// 里面有分组名输入框。
    GroupManager,
}

impl Modal {
    const ALL: &'static [Modal] = &[
        Modal::SessionManager,
        Modal::About,
        Modal::Settings,
        Modal::Unlock,
        Modal::HostKey,
        Modal::Paste,
        Modal::Import,
        Modal::Editor,
        Modal::FilesDialog,
        Modal::GroupManager,
    ];
}
```

`modal_open` 改成（**保留原有的文档注释**，把最后一段「`group_manager_open` 不在这张表里，是既有缺口」删掉——它已经不成立了）：

```rust
    /// 有没有模态盖着。分流(§4.5)与标签快捷键共用同一个判据 —— 两处各写一遍
    /// 的话,新增一种弹窗时漏改一处,现象是「弹窗开着按 Ctrl+W 把背后的标签关了」。
    ///
    /// **`match` 必须留在这个函数体里**:既有的两条守护测试
    /// (`the_unlock_dialog_counts_as_a_modal_...` / `the_import_preview_...`)
    /// 扎的是这个函数的源码文本,抽到别的方法里会让它们空过。
    fn modal_open(&self) -> bool {
        Modal::ALL.iter().any(|m| match m {
            Modal::SessionManager => self.ui.session_manager_open,
            Modal::About => self.ui.about_open,
            // F84:设置弹窗里有输入框(手填族名)。不算模态的话,敲进去的字
            // 会同时被发给远端 —— T8 那条「弹窗开着时键盘归 egui」。
            Modal::Settings => self.ui.settings_open,
            // F71:解锁框里输的是主密码。不算模态的话,它会一边被 egui 收进
            // 输入框、一边被原样发给远端 shell —— T8。
            Modal::Unlock => self.ui.unlock.is_some(),
            Modal::HostKey => self.pending_host_key.is_some(),
            Modal::Paste => self.pending_paste.is_some(),
            // F2:导入预览弹窗。里面没有输入框,但有「导入 N 条」这种一按就
            // 落库的按钮,而空格/回车在 egui 里是按钮的激活键 —— T8。
            Modal::Import => self.ui.import.is_some(),
            Modal::Editor => self.editor.is_some(),
            Modal::FilesDialog => self.ui.files_dialog.is_some(),
            Modal::GroupManager => self.ui.group_manager_open,
        })
    }
```

- [ ] **Step 4：跑，确认绿**

```bash
cargo test -p mullion-app every_modal_variant 2>&1 | grep "test result"
cargo test -p mullion-app modal 2>&1 | grep "test result"
```
预期：全 PASS，包括既有那两条扎源码的测试。

- [ ] **Step 5：变异验收（三次）**

1. 从 `Modal::ALL` 删掉 `Modal::Editor` → `every_modal_variant_is_listed_in_all` 变红。改回。
2. 删掉 `match` 里 `Modal::Editor => ...` 那一臂 → **编译失败**（`non-exhaustive patterns`）。改回。
3. 把 `modal_open` 里 `self.ui.unlock.is_some()` 删掉（连同那一臂）→ 编译失败 + 既有测试红。改回。

第 2 条是这个任务的全部意义所在——**必须亲手确认编译真的过不去**。

- [ ] **Step 6：跑全量 + 提交**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
git add crates/mullion-app/src/app.rs
git commit -m "fix(app): 模态门控改成编译器守护,补上三个漏登记的弹窗 (T8)

modal_open 过去是一串 || 列举,漏了内置编辑器 / 文件对话框 / 分组管理器
三个弹窗——它们开着时键盘仍被判给终端,用户敲进输入框的字同时被原样发给
远端 shell;编辑器里则是压根打不出字。

改成 enum Modal + match 穷尽性:加一种弹窗忘了登记就编译不过。ALL 的
完备性另配一条测试(编译器管不着数组少写一项)。

跑了 T8 的守护测试:the_unlock_dialog_counts_as_a_modal_so_the_password_never_reaches_the_shell、
the_import_preview_counts_as_a_modal_so_keys_do_not_leak_to_the_shell、
every_modal_variant_is_listed_in_all。"
```

---

## Task 2：B2 —— 列头点击排序失效（调查 + 修）

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs:492-527`（`header`）、`262-279`（背景菜单 `interact`）

**背景事实（已核实）**：排序功能**已经实现**（`header()` 里四个 `allocate_exact_size` + `Sense::click()` → `state.click_header(k)`，`▲`/`▼` 也画了）。用户点不出效果 = 点击没落到列头上。

**头号嫌疑**：`show()` 开头那个覆盖 `ui.max_rect()` 的背景右键菜单 `interact`（`files_panel.rs:274-278`）。它的 `Sense::click()` 罩住整栏。egui 的命中判定是「后注册的部件优先」，列头是后注册**应该**赢——但 `ui.interact()` 拿到的 `Response` 在同一帧内已经消费了指针状态，需要实测。

- [ ] **Step 1：先复现，确认根因**

在 `header()` 里 `if resp.clicked()` 那一行前面临时插一行诊断：

```rust
if resp.hovered() {
    log::warn!("列头 {label} hovered=true clicked={}", resp.clicked());
}
```

跑 GUI（需要显示器；无头环境跳到 Step 2 的静态判据）：

```bash
MULLION_LOG=warn cargo run -p mullion-app -- user@host -p 22 -i /path/key
```

三种可能，**对号入座再决定修法**：
- **A. `hovered=false`**：列头压根没拿到指针 → 背景 `interact` 吃了。修法见 Step 3-A。
- **B. `hovered=true, clicked=false`**：拿到了悬停但点击被别处消费 → 同样是 z 序问题，修法同 A。
- **C. 两者都 true 但界面没变**：点击到了，是 `click_header` / `rows()` 的逻辑问题 → 修法见 Step 3-C。

**无头环境替代判据**：`ui.interact()` 在 egui 0.30 里注册的是一个真实部件，其 `Rect` 覆盖 `ui.max_rect()`。用离屏 harness 跑一帧，在列头矩形中心注入一次点击，断言 `state.sort_dir` 翻转——这条测试无论根因是哪种都成立，先写它（Step 2）。

- [ ] **Step 2：写失败测试**

`files_panel.rs` 的 `#[cfg(test)] mod tests` 里加（参照该模块已有的 egui harness 写法，如 `clicking_a_bookmark_dispatches_goto_to_its_path`）：

```rust
/// B2:点列头必须真的能排序。功能本身早就在(`click_header`),
/// 坏的是**点击落不到列头上** —— 整栏背景挂了一个覆盖 `max_rect` 的
/// `interact`(右键菜单宿主),它把列头的命中吃掉了。
///
/// **必须预热若干帧再点**:egui 的部件矩形要上一帧的布局结果才存在,
/// 第一帧注入点击必然打空(这是本项目 egui 测试的既有坑,见切片 G
/// 「预热帧数不足的两种静默症状」)。
///
/// 自证会变红:把 `header()` 里 `if resp.clicked() { hit = Some(key) }`
/// 改成 `if false { .. }`。
#[test]
fn clicking_the_name_header_actually_flips_the_sort_direction() {
    let mut state = ready_state_with_entries();   // 见下方 helper
    assert_eq!(state.sort_key, SortKey::Name);
    assert_eq!(state.sort_dir, crate::files::SortDir::Asc);

    let ctx = egui::Context::default();
    // 取矩形的前提:标注模式必须开着,否则 `mark` 直接 return(Task 0)。
    crate::ui::annotate::toggle(&ctx);
    let t = Theme::dark();
    // 列头中心的估算坐标:面板左上角 + 路径条一行 + 半个 ROW_H。
    // 用 annotate 记下的「文件面板/远端/列头」矩形来定位,不硬编码 ——
    // 硬编码会在改了路径条高度之后静默打空。
    let mut header_rect = None;
    for frame in 0..4 {
        let mut input = egui::RawInput::default();
        if frame == 3 {
            if let Some(r) = header_rect {
                input.events.push(egui::Event::PointerMoved(center_of(r)));
                input.events.push(egui::Event::PointerButton {
                    pos: center_of(r),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                });
                input.events.push(egui::Event::PointerButton {
                    pos: center_of(r),
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::default(),
                });
            }
        }
        ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                show(ui, &t, "远端", 1, PanelColumn::Remote, &mut state,
                     false, &[], 0);
            });
        });
        header_rect = crate::ui::annotate::spot_rect(&ctx, "文件面板/远端/列头");
    }
    assert_eq!(
        state.sort_dir,
        crate::files::SortDir::Desc,
        "点了「名称」列头,排序方向没翻 —— 点击没落到列头上"
    );
}
```

**先确认 `annotate` 模块有没有按名字取矩形的接口**：

```bash
grep -n "pub fn" crates/mullion-app/src/ui/annotate.rs
```
没有的话，在 `annotate.rs` 里加一个只在 `#[cfg(test)]` 下编译的 `pub fn rect_of(ctx, name) -> Option<Rect>`，从它已有的存储里查——`annotate::mark` 本来就把 `(名字, 矩形)` 存进了 ctx 的 memory，读回来即可。

`ready_state_with_entries` / `center_of` 两个 helper 若模块里已有同款就复用，没有就写：

```rust
fn center_of(r: egui::Rect) -> egui::Pos2 {
    r.center()
}

fn ready_state_with_entries() -> PaneState {
    let mut s = PaneState::new(mullion_ssh::sftp::RemotePath::from_bytes(b"/".to_vec()));
    s.entries = vec![
        entry("b.txt", EntryKind::File),
        entry("a.txt", EntryKind::File),
    ];
    s.load = Load::Ready;
    s
}
```
（`entry(..)` helper 模块里已有，先 `grep -n "fn entry" crates/mullion-app/src/ui/files_panel.rs` 确认签名再用。）

- [ ] **Step 3：跑，确认失败**

```bash
cargo test -p mullion-app clicking_the_name_header 2>&1 | grep -A5 "test result"
```
预期：FAIL，`sort_dir` 仍是 `Asc`。

**若它意外地通过了**：说明根因不在点击命中，而在真实 GUI 的别处（例如列头被路径条盖住）。停下来，回到 Step 1 用真实 GUI 复现，不要为了让测试红而改测试。

- [ ] **Step 4-A：修（若根因是背景 `interact` 吃点击）**

背景那个 `interact` 只为承载**右键**菜单，不需要左键。把它的 `Sense` 收窄：

```rust
    let bg = ui.interact(
        ui.max_rect(),
        ui.id().with(("files-bg-menu", id, generation)),
        // B2:**只要右键**。原来是 `Sense::click()`,它把左键也一并接管了,
        // 罩住整栏、把列头的左键命中吃掉 —— 排序点了没反应就是这么来的。
        // 右键菜单只需要 secondary,收窄之后左键落到它下面的部件上。
        egui::Sense::click(),
    );
```

egui 0.30 的 `Sense` 没有「只 secondary」的开关（`Sense::click()` 同时接管左右键）。可行的两种改法，**按实测选**：

**改法 1（推荐）**：把背景 `interact` 的矩形从「整栏」缩到「列头之外的区域」：

```rust
    // 列头要先量出来才能扣掉 —— 但列头是后面才画的。改成两步:
    // 背景 interact 挪到列头**之后**注册(egui 同层后注册者优先,
    // 列头反而会赢),矩形仍是整栏。
```
即：把 `let bg = ui.interact(...)` 连同它的 `context_menu` 整体**下移到 `header(...)` 调用之后**。注意 `bg` 在 `Load` 提前 return 的分支里也要可用——把 `Load` 匹配那段之前的空白区域菜单单独处理，或把 `bg` 的注册拆成两次（`Ready` 与非 `Ready` 各一次）。

**改法 2**：给列头用更高的 `Order`——`ui.with_layer_id(LayerId::new(Order::Middle, id), |ui| header(..))`。代价是列头进了独立层，`ui.max_rect` 的裁剪要自己管。

**先试改法 1**，它不引入新层。

- [ ] **Step 4-C：修（若根因是逻辑）**

若 Step 1 的现象是 C（点击到了但界面没变），检查 `PaneState::click_header`：

```bash
grep -n "fn click_header" -A 15 crates/mullion-app/src/files/state.rs
```
预期它应该是「同一列 → 翻方向；不同列 → 换列并重置为 `Asc`」，且**改完要重排 `entries`**。若它只改了 `sort_key`/`sort_dir` 而 `rows()` 没据此排序，那就是漏了排序调用。

- [ ] **Step 5：跑，确认绿 + 变异验收**

```bash
cargo test -p mullion-app clicking_the_name_header 2>&1 | grep "test result"
```
变异：把 `header()` 里 `if resp.clicked() { hit = Some(key); }` 改成 `if false {}` → 测试必须变红。改回。

- [ ] **Step 6：提交**

```bash
git add crates/mullion-app/src/ui/files_panel.rs crates/mullion-app/src/ui/annotate.rs
git commit -m "fix(app): 列头点击排序失效 —— 背景右键菜单罩住整栏吃掉左键命中 (F50)

排序逻辑本来就在(click_header + ▲▼ 标记),坏的是点击落不到列头上:
整栏背景为承载右键菜单挂了一个覆盖 max_rect 的 Sense::click() interact,
把列头的左键命中吃了。

守护测试 clicking_the_name_header_actually_flips_the_sort_direction ——
它注入真实点击事件,预热三帧后才点(egui 的部件矩形要上一帧布局才存在)。"
```

---

## Task 3：B1 —— 滚动条串栏 + 侵入对面栏（调查 + 修）

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs:820-862`（`content` 的两栏切分）、`390-398`（`ScrollArea` 构造）

**验收判据（spec B1）：**
1. 拖左栏滚动条，只有左栏内容滚动，右栏纹丝不动
2. 滚动条画在本栏矩形之内，不越过两栏之间那条分隔线

**两条独立嫌疑（可能同时成立）：**
- **嫌疑 1（几何）**：`content()` 用 `ui.scope_builder(UiBuilder::new().max_rect(left))` 摆内容，但 `show()` 里 `ScrollArea` 用 `auto_shrink([false, false])`。需要确认它取的是 `max_rect` 还是父 ui 的 `available_rect`——若是后者，滚动条会画到 `full` 的右边界，正好落在对面栏。
- **嫌疑 2（id 撞车）**：`scroll_id_salt(id, generation)` 两栏 salt 不同（`"远端"`/`"本地"`），但最终 id 是 `ui.id.with(salt)`。**两个 `scope_builder` 子 ui 的 `ui.id` 是否互不相同需要实测**——`UiBuilder::new()` 不指定 `id_salt` 时，egui 0.30 用什么派生子 id 决定了这一点。

- [ ] **Step 1：写判别测试（同时验两条嫌疑）**

```rust
/// B1:两栏的 `ScrollArea` 必须是两个独立的滚动状态,且各自的视口不越界。
///
/// 这条测试同时钉住两件事:
/// 1. **id 不撞**——撞了的话拖一栏的滚动条会滚另一栏
/// 2. **视口不越界**——越界的话滚动条画到对面栏的地盘上
///
/// 自证会变红:把 `scroll_id_salt` 改成忽略 `id` 参数
/// (`format!("files-{generation}")`),两栏就会共用一个滚动状态。
#[test]
fn the_two_columns_get_independent_non_overlapping_scroll_areas() {
    let ctx = egui::Context::default();
    // 取矩形的前提:标注模式必须开着,否则 `mark` 直接 return(Task 0)。
    crate::ui::annotate::toggle(&ctx);
    let t = Theme::dark();
    let mut frame = PanelFrame::default();
    frame.remote.load = Load::Ready;
    frame.local.load = Load::Ready;
    frame.remote.entries = (0..200).map(|i| entry(&format!("r{i}"), EntryKind::File)).collect();
    frame.local.entries = (0..200).map(|i| entry(&format!("l{i}"), EntryKind::File)).collect();

    for _ in 0..3 {
        ctx.run(egui::RawInput::default(), |ctx| {
            content(ctx, &t, 7, true, &mut frame, 0);
        });
    }

    let left = crate::ui::annotate::spot_rect(&ctx, "文件面板/本地")
        .expect("本地栏没画出来");
    let right = crate::ui::annotate::spot_rect(&ctx, "文件面板/远端")
        .expect("远端栏没画出来");
    assert!(
        left.max.x <= right.min.x + 0.5,
        "两栏矩形重叠了:左栏右边界 {} > 右栏左边界 {} —— 滚动条会画进对面栏",
        left.max.x, right.min.x
    );
}
```

> 注：此测试假定 Task 5（方位调转）**还没做**时左栏是"远端"。**按当前代码的实际方位写 `rect_of` 的名字**，Task 5 做完后再把名字对调——或者干脆等 Task 5 之后再写这条测试。**推荐：先做 Task 5 再做本任务**，避免写两遍。若严格按 spec §8 顺序（B 在 A 之前），这里用当前方位的名字（左="远端"、右="本地"）。

- [ ] **Step 2：跑，看它红在哪一条**

```bash
cargo test -p mullion-app the_two_columns_get_independent 2>&1 | grep -B5 "test result"
```

- [ ] **Step 3：修嫌疑 1（几何越界）**

`ui.scope_builder(UiBuilder::new().max_rect(left))` 只设了 `max_rect`，**没设裁剪**。补上：

```rust
            ui.scope_builder(egui::UiBuilder::new().max_rect(left), |ui| {
                // B1:**必须显式裁剪**。`max_rect` 只是布局预算,不阻止子部件
                // 画到框外 —— `ScrollArea` 的滚动条正是这么溜进对面栏的。
                ui.set_clip_rect(left);
                out.0 = show(/* ... */);
            });
```

同样处理 `right`。（`chrome.rs:251` 的 `content.set_clip_rect(inner)` 是同一手法，标签内容越界那次就是这么修的。）

- [ ] **Step 4：修嫌疑 2（id 撞车）**

若测试显示两栏共用滚动状态，给两个 `scope_builder` 各自一个显式 id：

```rust
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(left)
                    // B1:显式 id_salt。不给的话两个 scope 的 `ui.id` 由
                    // 调用序号派生,`ScrollArea::id_salt` 最终落到
                    // `ui.id.with(salt)` —— 父 id 相同时两栏会撞出同一个
                    // 持久化状态,拖一栏的滚动条滚的是另一栏。
                    .id_salt("files-col-local"),
                |ui| { /* ... */ },
            );
```

**先查 egui 0.30 的 `UiBuilder` 有没有 `id_salt` 方法**：

```bash
grep -n "pub fn id_salt\|pub fn ui_stack_info\|pub struct UiBuilder" -A 3 \
  ~/.cargo/registry/src/*/egui-0.30.0/src/ui_builder.rs
```
没有的话改用 `ui.push_id("files-col-local", |ui| { ... })` 包一层。

- [ ] **Step 5：跑，确认绿 + 变异验收**

变异：把 `scroll_id_salt` 改成 `format!("files-{generation}")`（忽略 `id`）→ 测试必须变红。改回。

- [ ] **Step 6：提交**

```bash
git add crates/mullion-app/src/ui/files_panel.rs
git commit -m "fix(app): 两栏滚动条串栏 + 侵入对面栏 (F50)

标签宿主里两栏各自 scope_builder(max_rect) 摆内容,但 max_rect 只是布局
预算、不裁剪,ScrollArea 的滚动条因此画进对面栏;两个 scope 的 ui.id 由
调用序号派生,ScrollArea 的持久化 id 撞车,拖一栏滚的是另一栏。

修法:显式 set_clip_rect + 给两栏各自的 id_salt。

守护测试 the_two_columns_get_independent_non_overlapping_scroll_areas。
滚动条的视觉位置仍需人工验收(无头环境验不了)。"
```

---

## Task 4：B3 —— 断联后转「已断开」态 + 重连

**Files:**
- Create: `crates/mullion-app/src/files/fail.rs`
- Modify: `crates/mullion-app/src/files/mod.rs`（挂 `mod fail;`）
- Modify: `crates/mullion-app/src/files/state.rs:11-18`（`Load` 加变体）
- Modify: `crates/mullion-app/src/ui/files_panel.rs:16-50`（`FileAction` 加变体）、`358-372`（`Load` 匹配）
- Modify: `crates/mullion-app/src/app.rs`（`SftpListed` 的错误落地处 + `FileAction::Reconnect` 分派）

- [ ] **Step 1：写 `classify` 的失败测试**

新建 `crates/mullion-app/src/files/fail.rs`：

```rust
//! B3:sftp 失败的**分类**。判据是「界面该怎么反应」,不是「错误码是什么」——
//! 所以它住在 `mullion-app` 而不是 `mullion-ssh`。
//!
//! 两类:
//! - `Session` —— 连接/通道级。整条链路没了,当前目录这个概念都不成立,
//!   面板要转断开态、给用户一个回到可用状态的入口
//! - `Path` —— 路径级。链路好好的,只是这一个路径动不了(没权限/不存在),
//!   面板停在原地报一句就行
//!
//! **分类拿字符串匹配是权宜**:`mullion_ssh::sftp` 目前把错误压成了
//! `String`(见 `app.rs::spawn_sftp_list_dir` 的 `map_err`)。哪天那边给出
//! 结构化错误类型,这里要改成按类型分类 —— 字符串匹配对服务端换措辞
//! 是脆的,下面这组测试覆盖的是**当前**真实文案。

/// 一次 sftp 失败该让界面怎么反应。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailKind {
    /// 连接/通道级 —— 面板转断开态。
    Session,
    /// 路径级 —— 停在原地报错。
    Path,
}

/// 把一条已格式化的错误文案分类。
///
/// **默认落 `Path`**:分错方向的代价不对称 —— 把路径错误误判成断开,会
/// 让用户对着一个好好的连接点「重连」(白等一次拨号);把断开误判成路径
/// 错误,只是少一个重连按钮、用户还能自己关标签重开。前者更烦人,所以
/// 只有**明确**认得出的连接级关键词才判 `Session`。
pub fn classify(msg: &str) -> FailKind {
    const SESSION_MARKERS: &[&str] = &[
        "channel",
        "Channel",
        "connection",
        "Connection",
        "disconnect",
        "Disconnect",
        "EOF",
        "eof",
        "closed",
        "broken pipe",
        "not connected",
        "session",
    ];
    if SESSION_MARKERS.iter().any(|m| msg.contains(m)) {
        FailKind::Session
    } else {
        FailKind::Path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 连接级失败必须判成 `Session` —— 否则用户在一条死链路上点目录,
    /// 只会看见一串底层错误,没有任何回到可用状态的入口。
    ///
    /// 自证会变红:把 `classify` 改成恒返回 `FailKind::Path`。
    #[test]
    fn connection_level_failures_are_session_kind() {
        for msg in [
            "读取目录失败:channel closed",
            "读取目录失败:Connection reset by peer",
            "读取目录失败:unexpected EOF",
        ] {
            assert_eq!(classify(msg), FailKind::Session, "误判为路径级:{msg}");
        }
    }

    /// 路径级失败必须判成 `Path` —— 否则点一个没权限的目录会把整栏
    /// 打成「连接已断开」,而连接好好的。
    ///
    /// 自证会变红:把 `classify` 改成恒返回 `FailKind::Session`。
    #[test]
    fn path_level_failures_are_path_kind() {
        for msg in [
            "读取目录失败:Permission denied",
            "读取目录失败:No such file or directory",
            "读取目录失败:Not a directory",
        ] {
            assert_eq!(classify(msg), FailKind::Path, "误判为会话级:{msg}");
        }
    }
}
```

> **实现前必做**：跑 `grep -rn "map_err\|format!(\"读取目录" crates/mullion-app/src/app.rs | head` 核对**真实**错误文案前缀，把上面测试里的字符串换成真实的。若发现 `mullion_ssh::sftp` 已经有结构化错误类型（`grep -n "pub enum .*Error" crates/mullion-ssh/src/sftp.rs`），**改用类型匹配**，别用字符串。

- [ ] **Step 2：跑，确认红**

```bash
cargo test -p mullion-app files::fail 2>&1 | grep -E "test result|error\["
```
预期：编译失败（模块还没挂）。在 `files/mod.rs` 顶部加 `pub mod fail;` 后重跑，预期两条测试 FAIL（`classify` 还没写就是编译错；已按上面写全则应直接 PASS——那就先把 `classify` 改成恒返回 `Path`，确认第一条红，再改回）。

- [ ] **Step 3：`Load` 加 `Disconnected`**

`crates/mullion-app/src/files/state.rs:11-18`：

```rust
pub enum Load {
    /// 还没连上 / 还没发过第一次请求。
    Idle,
    Loading,
    Ready,
    /// 出错了,字符串是已经格式化好的可读原因。**路径级**失败才用这个 ——
    /// 连接级失败走 `Disconnected`(B3)。
    Failed(String),
    /// B3:连接/通道没了。跟 `Failed` 分开是因为**界面动作不同** ——
    /// 这个状态要给一个重连入口,而 `Failed` 只是报一句、停在原地。
    Disconnected,
}
```

编译会在所有 `match state.load` 处报缺分支——**逐个补，不要用 `_ =>` 兜底**（兜底等于把「新状态该怎么画」这个决定藏起来，正是 Task 1 要根除的那种模式）。

- [ ] **Step 4：面板画断开态**

`files_panel.rs:358-372` 的 `match &state.load` 加一支：

```rust
        Load::Disconnected => {
            ui.colored_label(theme::c32(t.danger), "连接已断开");
            if ui.button("重连").clicked() {
                action = Some(FileAction::Reconnect);
            }
            return action;
        }
```

`FileAction` 加变体（`files_panel.rs:18-50`）：

```rust
    /// B3:这一栏所在的连接断了,用户按了「重连」。
    ///
    /// **不带参数**:重连的目标是「这个标签」,而标签是谁由 app 侧知道
    /// (面板本身不知道自己挂在哪个标签上)。两种宿主的语义不同,分派在
    /// app 侧做:SFTP 节点标签重建整条连接;终端标签的侧栏只重开
    /// sftp channel(SSH 本体断了是终端的事,侧栏不越权)。
    Reconnect,
```

- [ ] **Step 5：app 侧接线**

在 `SftpListed` 的错误落地处（`app.rs`，`grep -n "SftpListed" crates/mullion-app/src/app.rs` 定位），把

```rust
// 原来大意是:state.load = Load::Failed(msg)
```

改成：

```rust
                    Err(msg) => {
                        // B3:连接级失败要转断开态(给重连入口),路径级
                        // 停在原地报一句。判据在纯函数里,可单测。
                        pane.load = match crate::files::fail::classify(&msg) {
                            crate::files::fail::FailKind::Session => Load::Disconnected,
                            crate::files::fail::FailKind::Path => Load::Failed(msg),
                        };
                    }
```

`FileAction::Reconnect` 的分派（在 `apply_remote_file_action` / `apply_local_file_action` 附近，`grep -n "FileAction::Refresh =>" crates/mullion-app/src/app.rs` 定位同款分支）：

```rust
            FileAction::Reconnect => {
                // B3:两种宿主两种语义,**不共用一条路径**。
                match self.tabs.active_content() {
                    // SFTP 节点标签独占自己的连接(ADR-010/D6)——重连 =
                    // 重建整条连接。就地降级成占位标签,复用 F37 那条
                    // 「拨号 → ConnectOk 就地替换」的既有链路,不新造拨号路径。
                    Some(TabContent::Files(_)) => self.demote_files_tab_and_reconnect(),
                    // 终端标签的侧栏蹭 ws.hosts[0] 的连接。sftp channel 单独
                    // 死掉时重开它即可;SSH 本体断了是终端的事,侧栏不越权
                    // 重建整条连接。
                    Some(TabContent::Terminal(_)) => self.trigger_sftp_open(),
                    _ => {}
                }
            }
```

`demote_files_tab_and_reconnect` 新写（放在 `reconnect_tab` 附近，`app.rs:1714` 上方）：

```rust
    /// B3:SFTP 节点标签断了之后按「重连」。
    ///
    /// **就地降级成 `RestoredTab` 再走 `reconnect_tab`**,而不是另写一条
    /// 拨号链路 —— F37 已经有一条完整的「拨号 → `ConnectOk` 就地替换标签」
    /// 路径,再写一条就有两处要维护(而且第二条一定会漏掉 `pending_restore`
    /// 那道防连点的闸)。
    ///
    /// 代价:重连后回默认远端目录,不回断线前那个。可接受 —— F120 明确
    /// 「不记忆上次打开的目录」。
    fn demote_files_tab_and_reconnect(&mut self) {
        let Some(tab) = self.tabs.active_mut() else {
            return;
        };
        let Some(session_id) = tab.session_id else {
            // 快速连接开出来的 SFTP 标签没有会话记录,无从重连。
            self.ui.set_error("这个标签没有对应的会话记录,无法重连");
            return;
        };
        let tab_id = tab.id;
        let generation = self.next_ws_generation;
        self.next_ws_generation += 1;
        // 旧连接的后台任务必须先收口 —— 每个任务经 Arc 持有一份连接保活
        // 引用,只替换 content 收不了口(同 `wind_down` 那条纪律)。
        self.wind_down(&mut tab.content);
        tab.content = TabContent::Restored(RestoredTab {
            session_id,
            tree: Vec::new(),
            focus_leaf: 0,
            generation,
            wants_sftp: true,
            dialing: false,
        });
        self.reconnect_tab(tab_id);
    }
```

> **先核对三个 API 的真实签名再写**：`self.tabs.active_content()` / `active_mut()`（`grep -n "pub fn active" crates/mullion-app/src/shell/tabs.rs`）、`wind_down` 的签名（`grep -n "fn wind_down" -A 5 crates/mullion-app/src/app.rs`）、`trigger_sftp_open` 的签名。名字对不上就按真实的改。

- [ ] **Step 6：跑全量 + 变异验收 + 提交**

变异：把 `classify` 改成恒返回 `FailKind::Path` → `connection_level_failures_are_session_kind` 必须红。改回。

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
git add -u && git add crates/mullion-app/src/files/fail.rs
git commit -m "feat(app): SFTP 断联转「已断开」态 + 重连入口 (F50)

过去连接断了之后点目录,只把底层错误串糊在面板上,用户没有任何回到可用
状态的入口。现在按错误分类(纯函数 files::fail::classify)分流:连接级 →
Load::Disconnected + 重连按钮;路径级 → 停在原地报一句人话。

重连按宿主分两种语义:SFTP 节点标签就地降级成占位标签、复用 F37 的拨号
链路重建整条连接;终端标签的侧栏只重开 sftp channel(SSH 本体断了是终端
的事,侧栏不越权)。

守护测试 connection_level_failures_are_session_kind /
path_level_failures_are_path_kind。"
```

---

## Task 5：A 组 —— 两栏方位调转成「本地在前、远端在后」

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs:736-766`（`sidebar`）、`812-862`（`content`）

- [ ] **Step 1：写方位测试（先红）**

```rust
/// A 组:两栏方位是「本地在前、远端在后」—— 标签宿主左本地右远端,
/// 侧栏上本地下远端。两个宿主用**同一条**规则。
///
/// 判据用 annotate 记下的矩形,不看调用顺序 —— 调用顺序是实现细节,
/// 位置才是用户看见的东西。
///
/// 自证会变红:把 `content()` 里两次 `show()` 的 left/right 换回去。
#[test]
fn the_local_column_comes_first_in_both_hosts() {
    let ctx = egui::Context::default();
    // 取矩形的前提:标注模式必须开着,否则 `mark` 直接 return(Task 0)。
    crate::ui::annotate::toggle(&ctx);
    let t = Theme::dark();
    let mut frame = PanelFrame::default();

    for _ in 0..3 {
        ctx.run(egui::RawInput::default(), |ctx| {
            content(ctx, &t, 7, true, &mut frame, 0);
        });
    }
    let local = crate::ui::annotate::spot_rect(&ctx, "文件面板/本地").expect("本地栏没画");
    let remote = crate::ui::annotate::spot_rect(&ctx, "文件面板/远端").expect("远端栏没画");
    assert!(
        local.center().x < remote.center().x,
        "标签宿主:本地栏必须在左边(本地 x={} 远端 x={})",
        local.center().x, remote.center().x
    );

    let ctx2 = egui::Context::default();
    crate::ui::annotate::toggle(&ctx2);
    let mut ui_state = crate::ui::UiState::default();
    let mut frame2 = PanelFrame::default();
    for _ in 0..3 {
        ctx2.run(egui::RawInput::default(), |ctx| {
            sidebar(ctx, &t, &mut ui_state, 7, true, &mut frame2, 0);
        });
    }
    let local2 = crate::ui::annotate::spot_rect(&ctx2, "文件面板/本地").expect("本地栏没画");
    let remote2 = crate::ui::annotate::spot_rect(&ctx2, "文件面板/远端").expect("远端栏没画");
    assert!(
        local2.center().y < remote2.center().y,
        "侧栏:本地栏必须在上面(本地 y={} 远端 y={})",
        local2.center().y, remote2.center().y
    );
    assert!(
        remote2.height() > local2.height(),
        "侧栏:远端栏必须更高(0.6 : 0.4)—— 辅助视图里远端才是主体"
    );
}
```

- [ ] **Step 2：跑，确认红**

```bash
cargo test -p mullion-app the_local_column_comes_first 2>&1 | grep -B3 "test result"
```

- [ ] **Step 3：调换 `content()` 的两栏**

`files_panel.rs:832-861`，把 `left` 那个 `scope_builder` 里的 `show(..)` 参数换成本地栏那一组，`right` 换成远端栏那一组。**只换 `show()` 的参数，不换 `left`/`right` 的几何计算**：

```rust
            ui.scope_builder(egui::UiBuilder::new().max_rect(left), |ui| {
                ui.set_clip_rect(left);   // Task 3 加的,保留
                out.1 = show(              // 注意:本地栏的结果落 out.1
                    ui, t, "本地", generation, PanelColumn::Local,
                    &mut frame.local,
                    false,
                    panel_focused && frame.active_column == PanelColumn::Local,
                    &[],
                    0,
                );
            });
            ui.painter().vline(full.center().x, full.y_range(), theme::stroke(t));
            ui.scope_builder(egui::UiBuilder::new().max_rect(right), |ui| {
                ui.set_clip_rect(right);
                out.0 = show(              // 远端栏的结果仍落 out.0
                    ui, t, "远端", generation, PanelColumn::Remote,
                    &mut frame.remote,
                    frame.show_owner,
                    panel_focused && frame.active_column == PanelColumn::Remote,
                    &frame.bookmarks,
                    drop_in,
                );
            });
```

**关键：返回值 `out` 的元组语义不许变**——`out.0` 永远是远端栏的动作、`out.1` 永远是本地栏的。调用方按位置解包，换了会让远端的动作被当成本地的执行（对错侧下手）。

- [ ] **Step 4：调换 `sidebar()` 的两栏 + 高度比例**

`files_panel.rs:736-766`：

```rust
        .show(ctx, |ui| {
            annotate::mark(ui.ctx(), "文件侧栏", ui.max_rect());
            let h = ui.available_height();
            // A3:比例**跟着远端走,不跟着位置走** —— 侧栏是「终端为主 +
            // 文件为辅」的场景,辅助视图里远端才是主体。所以上面(本地)0.4、
            // 下面(远端)0.6。
            ui.allocate_ui(egui::vec2(ui.available_width(), h * 0.4), |ui| {
                out.1 = show(
                    ui, t, "本地", generation, PanelColumn::Local,
                    &mut frame.local,
                    false,
                    panel_focused && frame.active_column == PanelColumn::Local,
                    &[],
                    0,
                );
            });
            ui.separator();
            out.0 = show(
                ui, t, "远端", generation, PanelColumn::Remote,
                &mut frame.remote,
                frame.show_owner,
                panel_focused && frame.active_column == PanelColumn::Remote,
                &frame.bookmarks,
                drop_in,
            );
        });
```

- [ ] **Step 5：跑，确认绿 + 回归**

```bash
cargo test -p mullion-app files 2>&1 | grep "test result"
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED" /tmp/test.log
```

**特别检查**：`ui/mod.rs` 里 `sidebar`/`content` 的调用方怎么解包这个二元组（`grep -n "files_panel::sidebar\|files_panel::content" -A 5 crates/mullion-app/src/ui/mod.rs`）。若调用方写的是 `let (remote_act, local_act) = ...`，语义没变就不用动；若它按「第一个 = 上面那栏」理解，**必须一起改**。

- [ ] **Step 6：变异验收 + 提交**

变异：把 `content()` 的 left/right 换回去 → 测试变红。改回。

```bash
git add -u
git commit -m "feat(app): 文件面板两栏改成「本地在前、远端在后」(F50)

标签宿主左本地右远端、侧栏上本地下远端 —— 两个宿主同一条规则,跟
WinSCP/FileZilla 的心智模型一致。侧栏高度比例跟着远端走(远端 0.6、
本地 0.4):辅助视图里远端才是主体。

返回值元组语义未变(out.0 恒为远端栏动作),否则远端的动作会被当成本地的
执行 —— 那是对错侧下手。

守护测试 the_local_column_comes_first_in_both_hosts。"
```

---

## Task 6：D1 —— 文件类型图标（painter 自绘）

**Files:**
- Create: `crates/mullion-app/src/ui/file_icon.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs`（挂 `pub mod file_icon;`）
- Modify: `crates/mullion-app/src/ui/files_panel.rs:529-607`（`row`）

- [ ] **Step 1：写几何纯函数的测试**

新建 `crates/mullion-app/src/ui/file_icon.rs`：

```rust
//! D1:文件类型图标。**painter 自绘,不用字体字形**。
//!
//! 为什么不用 emoji/字符:字形是否存在取决于字体,Windows 上会变豆腐块;
//! 而且字形宽度不可控,整列的名称起始位置会跟着字体飘。自绘不依赖字体。
//!
//! 颜色**不在这里决定** —— 由调用方传入,取的是 `row()` 里那套既有的语义色
//! (目录 `fg_strong`、文件 `fg`、名称不可操作 `fg_dimmer`)。图标和文字用
//! 两套判据的话,会出现「文字灰了图标还亮着」这种自相矛盾的行。

use mullion_ssh::sftp::EntryKind;

/// 一个图标由若干条折线组成(闭合与否由调用方按形状约定)。
/// 抽出来只为**可单测** —— 像素长什么样仍然只有人眼能判。
pub fn outline(rect: egui::Rect, kind: EntryKind) -> Vec<Vec<egui::Pos2>> {
    // 留一圈内边距,图标不顶满行高。
    let r = rect.shrink(2.0);
    let (l, t, rt, b) = (r.left(), r.top(), r.right(), r.bottom());
    match kind {
        // 文件夹:带页签的梯形。
        EntryKind::Dir => vec![vec![
            egui::pos2(l, b),
            egui::pos2(l, t + r.height() * 0.25),
            egui::pos2(l + r.width() * 0.4, t + r.height() * 0.25),
            egui::pos2(l + r.width() * 0.5, t),
            egui::pos2(rt, t),
            egui::pos2(rt, b),
            egui::pos2(l, b),
        ]],
        // 文件:右上角折角的页。两条折线 —— 页身 + 折角。
        EntryKind::File => {
            let fold = r.width() * 0.3;
            vec![
                vec![
                    egui::pos2(l, t),
                    egui::pos2(rt - fold, t),
                    egui::pos2(rt, t + fold),
                    egui::pos2(rt, b),
                    egui::pos2(l, b),
                    egui::pos2(l, t),
                ],
                vec![
                    egui::pos2(rt - fold, t),
                    egui::pos2(rt - fold, t + fold),
                    egui::pos2(rt, t + fold),
                ],
            ]
        }
        // 符号链接:页 + 一个指出去的箭头。
        EntryKind::Symlink => {
            let mut v = outline(rect, EntryKind::File);
            v.push(vec![
                egui::pos2(l + r.width() * 0.25, b - r.height() * 0.25),
                egui::pos2(rt - r.width() * 0.2, t + r.height() * 0.35),
            ]);
            v
        }
    }
}

/// 把 `outline` 画出来。
pub fn paint(painter: &egui::Painter, rect: egui::Rect, kind: EntryKind, color: egui::Color32) {
    for line in outline(rect, kind) {
        painter.add(egui::Shape::line(line, egui::Stroke::new(1.0, color)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 图标必须**画在给定的格子里**。越界的话它会压到相邻列的文字上,
    /// 而 painter 直接按坐标画、不受布局约束,越界了编译器一声不吭。
    ///
    /// 自证会变红:把 `outline` 里的 `rect.shrink(2.0)` 改成
    /// `rect.expand(2.0)`。
    #[test]
    fn every_icon_stays_inside_its_cell() {
        let cell = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(16.0, 16.0));
        for kind in [EntryKind::Dir, EntryKind::File, EntryKind::Symlink] {
            for line in outline(cell, kind) {
                for p in line {
                    assert!(
                        cell.contains(p),
                        "{kind:?} 的顶点 {p:?} 跑出了格子 {cell:?}"
                    );
                }
            }
        }
    }

    /// 三种类型必须长得不一样 —— 否则「这是目录还是文件」这个图标本来
    /// 要回答的问题它一个也没回答。
    ///
    /// 自证会变红:把 `Symlink` 那一支改成直接 `outline(rect, File)`。
    #[test]
    fn the_three_kinds_look_different() {
        let cell = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(16.0, 16.0));
        let d = format!("{:?}", outline(cell, EntryKind::Dir));
        let f = format!("{:?}", outline(cell, EntryKind::File));
        let s = format!("{:?}", outline(cell, EntryKind::Symlink));
        assert_ne!(d, f, "目录和文件长得一样");
        assert_ne!(f, s, "文件和链接长得一样");
        assert_ne!(d, s, "目录和链接长得一样");
    }
}
```

- [ ] **Step 2：挂模块，跑，确认绿**

`crates/mullion-app/src/ui/mod.rs` 的模块声明区加 `pub mod file_icon;`（按字母序插进既有那批 `pub mod` 里）。

```bash
cargo test -p mullion-app file_icon 2>&1 | grep "test result"
```

- [ ] **Step 3：接进 `row()`**

`files_panel.rs:529-607`。在名称文字之前画图标，**并把名称的起始 x 右移**：

```rust
/// 图标格子的边长 + 它和名称之间的空隙。
const W_ICON: f32 = 16.0;
const ICON_GAP: f32 = 4.0;
```

`row()` 里，在 `p.text(rect.left_center() + vec2(4.0, 0.0), ..)` 之前插入：

```rust
    // D1:类型图标。颜色跟文字**同源**(上面那个 `fg`),不另算一套。
    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 4.0, rect.center().y - W_ICON * 0.5),
        egui::vec2(W_ICON, W_ICON),
    );
    crate::ui::file_icon::paint(p, icon_rect, e.kind, fg);
```

名称文字的起点改成 `rect.left() + 4.0 + W_ICON + ICON_GAP`。

- [ ] **Step 4：跑全量 + 提交**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
git add crates/mullion-app/src/ui/file_icon.rs && git add -u
git commit -m "feat(app): 文件面板加类型图标 —— painter 自绘,不依赖字体字形 (F50)

目录/文件/符号链接三形状。不用 emoji:字形是否存在取决于字体(Windows 上
会变豆腐块),且字形宽度不可控会让名称列的起始位置跟着字体飘。

颜色与行文字同源(目录 fg_strong / 文件 fg / 不可操作 fg_dimmer),不另算
一套 —— 否则会出现「文字灰了图标还亮着」的自相矛盾行。

守护测试 every_icon_stays_inside_its_cell(越界会压到相邻列,painter 直接
按坐标画、编译器管不着)、the_three_kinds_look_different。
图标的实际观感需人工验收。"
```

---

## Task 7：D2/D3 —— 属主列 + 名称列宽度同源 + 删 `show_owner`

**Files:**
- Modify: `crates/mullion-app/src/files/mod.rs:40-45`（`SortKey`）+ 排序函数
- Modify: `crates/mullion-app/src/ui/files_panel.rs`（`header` / `row` / `show` 签名 / `PanelFrame`）

**背景事实（已核实）**：`show_owner` **全仓没有任何翻转点**——它是恒 `false` 的死字段，「列头右键打开」（D21 注释所称）从未实现。属主信息在现版**永远显示不出来**。

- [ ] **Step 1：写名称列宽度同源的测试**

`files_panel.rs` 的测试模块：

```rust
/// D3:名称列宽度必须**一处算、两处用**。`header()` 和 `row()` 各写一遍
/// 的话,加一列就会有一处漏改,现象是列头文字和行内容错位 —— 而且错得
/// 很小(几个像素),没人会当成 bug 报上来,它就一直错着。
///
/// 自证会变红:把 `row()` 里的 `name_w(..)` 换回内联的减法表达式,
/// 并从减数里漏掉 `W_OWNER`。
#[test]
fn the_name_column_width_is_computed_in_exactly_one_place() {
    let src = include_str!("files_panel.rs");
    // 扫产品代码(第一个 #[cfg(test)] 之前),数一数减法表达式出现几次。
    let prod = src.split("#[cfg(test)]").next().expect("源码切歪了");
    let inline = prod.matches("- W_SIZE - W_MTIME - W_PERM").count();
    assert_eq!(
        inline, 1,
        "名称列宽度算了 {inline} 次 —— 必须只在 `name_w()` 里算一次,\
         两处各算一遍会让列头和行错位"
    );
}
```

- [ ] **Step 2：跑，确认红（当前是 2 次）**

```bash
cargo test -p mullion-app the_name_column_width 2>&1 | grep -B3 "test result"
```
预期：FAIL，`inline` = 2。

- [ ] **Step 3：抽出 `name_w()` + 加属主列常量**

`files_panel.rs` 常量区：

```rust
const W_SIZE: f32 = /* 原值 */;
const W_MTIME: f32 = 132.0;
const W_PERM: f32 = 86.0;
/// D2:属主列(`uid:gid`)。SFTP v3 的 attrs 里 uid/gid 就是数字,
/// 而 `russh-sftp 2.4.0` 的客户端 `DirEntry` 不暴露 `longname` ——
/// 名字在协议层拿不到,不为此去 exec 一次 `id`(设计 D21)。
const W_OWNER: f32 = 92.0;

/// 名称列宽度。**一处算、两处用**(`header` 与 `row`)——两处各算一遍
/// 的话,加一列就会有一处漏改,列头和行会错位几个像素,而那种错位不会
/// 有人报上来。
fn name_w(total: f32) -> f32 {
    (total - W_ICON - ICON_GAP - W_SIZE - W_MTIME - W_PERM - W_OWNER).max(80.0)
}
```

`header()` 与 `row()` 里的两处内联减法都换成 `name_w(ui.available_width())` / `name_w(rect.width())`。

- [ ] **Step 4：加 `SortKey::Owner` + 排序**

`crates/mullion-app/src/files/mod.rs:40-45`：

```rust
pub enum SortKey {
    Name,
    Size,
    Mtime,
    Perm,
    /// D2:按 `uid` 排,`uid` 相同再按 `gid`。
    Owner,
}
```

排序函数（`grep -n "SortKey::Perm =>" -B5 -A5 crates/mullion-app/src/files/mod.rs` 定位）加一支：

```rust
        SortKey::Owner => a.uid.cmp(&b.uid).then(a.gid.cmp(&b.gid)),
```

配一条测试：

```rust
/// D2:属主列可排序。按 uid 排,uid 相同再按 gid。
///
/// 自证会变红:把 `SortKey::Owner` 那一支改成 `Ordering::Equal`。
#[test]
fn sorting_by_owner_orders_by_uid_then_gid() {
    let mut v = vec![
        entry_with_owner("c", 1000, 1000),
        entry_with_owner("a", 0, 0),
        entry_with_owner("b", 1000, 5),
    ];
    sort_entries(&mut v, SortKey::Owner, SortDir::Asc);
    let names: Vec<_> = v.iter().map(|e| e.name.display().to_string()).collect();
    assert_eq!(names, vec!["a", "b", "c"], "属主排序不对:应按 uid 再 gid");
}
```
（`sort_entries` 的真实函数名先 `grep -n "pub fn sort" crates/mullion-app/src/files/mod.rs` 确认；`entry_with_owner` helper 按模块里既有的 `entry` helper 加两个参数。）

- [ ] **Step 5：列头加「属主」+ 行画属主 + 删 `show_owner`**

`header()` 的列表加一项：

```rust
        for (label, key, w) in [
            ("名称", SortKey::Name, name_w(ui.available_width())),
            ("大小", SortKey::Size, W_SIZE),
            ("修改时间", SortKey::Mtime, W_MTIME),
            ("权限", SortKey::Perm, W_PERM),
            ("属主", SortKey::Owner, W_OWNER),
        ] {
```

`row()` 的签名去掉 `show_owner`，加 `column: PanelColumn`；权限那段拆成两列：

```rust
    // 权限列(不再把 uid:gid 拼在后面)。
    p.text(
        egui::pos2(rect.right() - W_OWNER - 4.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        perm_string(e.mode),
        font.clone(),
        theme::c32(t.fg_dim),
    );
    // D2:属主列。**本地栏恒画 `—`**,判据是 `PanelColumn::Local` 而不是
    // `uid == 0` —— 远端真的有 root 拥有的文件,拿 0 当「没有属主信息」的
    // 哨兵会让那些文件的属主列也变成 `—`。
    let owner = if column == PanelColumn::Local {
        "—".to_string()
    } else {
        format!("{}:{}", e.uid, e.gid)
    };
    p.text(
        rect.right_center() - egui::vec2(4.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        owner,
        font,
        theme::c32(t.fg_dim),
    );
```

**删除 `show_owner`**：`PanelFrame::show_owner` 字段（`files_panel.rs:618`）、`Default` 里的 `show_owner: false`（`647`）、`show()` 的参数（`245`）、`row()` 的参数（`533`）、`sidebar`/`content` 的两处传参（`747`/`840`）、`ui/mod.rs` 三处 `show_owner: false`（`1739`/`1800`/`1943`）、测试里两处（`1076`/`1228`）。

配一条测试：

```rust
/// D2:本地栏的属主列画 `—`,不画 `0:0`。判据是栏别不是 uid ——
/// 远端真的有 root(uid=0)拥有的文件。
///
/// 自证会变红:把判据从 `column == PanelColumn::Local` 改成 `e.uid == 0`,
/// 然后这条测试里那个远端 root 文件会画成 `—`。
#[test]
fn the_local_column_shows_a_dash_for_owner_but_remote_root_shows_zeros() {
    assert_eq!(owner_text(PanelColumn::Local, 0, 0), "—");
    assert_eq!(owner_text(PanelColumn::Remote, 0, 0), "0:0");
    assert_eq!(owner_text(PanelColumn::Remote, 1000, 1000), "1000:1000");
}
```
——为此把上面那段属主文案抽成纯函数 `fn owner_text(column: PanelColumn, uid: u32, gid: u32) -> String`，`row()` 调它。

- [ ] **Step 6：跑全量 + 变异验收 + 提交**

变异三次：(1) `name_w` 漏掉 `W_OWNER` → 宽度测试红；(2) `SortKey::Owner` 排序改成 `Equal` → 排序测试红；(3) 属主判据改成 `e.uid == 0` → 属主文案测试红。

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
git add -u
git commit -m "feat(app): 文件面板加属主列,删掉恒 false 的 show_owner 死字段 (F50)

show_owner 全仓没有任何翻转点,「列头右键打开」从未实现 —— 属主信息在
现版永远显示不出来,这正是走查提出这一条的原因。改成默认可见的独立列,
可排序(按 uid 再 gid)。

内容是数字 uid:gid:SFTP v3 的 attrs 里就是数字,而 russh-sftp 2.4.0 的
客户端 DirEntry 不暴露 longname,名字在协议层拿不到(不为此 exec id,D21)。
本地栏画 —,判据是栏别不是 uid==0(远端真的有 root 拥有的文件)。

顺带把名称列宽度抽成 name_w() 一处算两处用 —— 加列时两处各算一遍会让
列头和行错位几个像素,而那种错位不会有人报上来。

守护测试 the_name_column_width_is_computed_in_exactly_one_place /
sorting_by_owner_orders_by_uid_then_gid /
the_local_column_shows_a_dash_for_owner_but_remote_root_shows_zeros。"
```

---

## Task 8：C 组 —— 内置编辑器可拖可缩 + 最大化

**Files:**
- Modify: `crates/mullion-app/src/ui/editor_window.rs:126-162`
- Modify: `crates/mullion-app/src/ui/editor_window.rs`（`EditorState` 加 `maximized` 字段）

**前置**：Task 1 必须已完成（否则编辑器里打不出字，改了也验不了）。

- [ ] **Step 1：写「不再锁死尺寸」的源码守护测试**

egui 的窗口几何在无头环境里能跑，但「窗口能不能拖」取决于 `anchor` 是否存在——这是源码事实，扎源码最直接：

```rust
/// C 组:编辑器窗口不许锚死、不许写死编辑区高度。
///
/// `anchor` 会让 `egui::Window` 完全无法拖动(egui 0.30:设了 anchor 就
/// 忽略用户拖拽的位移);`max_height(360.0)` 让编辑区在窗口放大后仍停在
/// 360 —— 两者合起来就是「没法全屏」。
///
/// 扎源码而不是造窗口:这两条都是**代码里有没有这一行**的事实,
/// 而窗口的实际可拖性要真人拖一下才知道。
///
/// 自证会变红:把 `.anchor(egui::Align2::CENTER_CENTER, ..)` 加回去。
#[test]
fn the_editor_window_is_neither_anchored_nor_height_locked() {
    let src = include_str!("editor_window.rs");
    let prod = src.split("#[cfg(test)]").next().expect("源码切歪了");
    assert!(
        !prod.contains(".anchor("),
        "编辑器窗口还锚着 —— 锚死的 egui::Window 拖不动"
    );
    assert!(
        !prod.contains("max_height(360.0)"),
        "编辑区高度还写死在 360 —— 窗口放大了它也不跟着长"
    );
}
```

- [ ] **Step 2：跑，确认红**

```bash
cargo test -p mullion-app the_editor_window_is_neither 2>&1 | grep -B3 "test result"
```

- [ ] **Step 3：改窗口 + 加最大化**

`EditorState` 加字段：

```rust
    /// C 组:最大化状态。`true` 时窗口铺满主窗口客户区。
    /// 纯 UI 运行态,不持久化 —— 关掉编辑器就没了(与 F37 的布局持久化
    /// 无关,那存的是标签与分屏形状)。
    pub maximized: bool,
```
（`EditorState::new(..)` 里初始化为 `false`。）

`show()` 里的 `egui::Window` 改成：

```rust
    let screen = ctx.screen_rect();
    let mut win = egui::Window::new("编辑文件")
        .collapsible(false)
        .resizable(true)
        .default_size(egui::vec2(720.0, 480.0));
    if s.maximized {
        // 最大化:钉住位置与尺寸。**每帧都钉** —— 用户在最大化状态下
        // 拖窗口边缘时,不钉的话 egui 会记住那个尺寸,再按「还原」就
        // 还原不回去了。
        win = win
            .current_pos(screen.min)
            .fixed_size(screen.size());
    }
    win.show(ctx, |ui| {
        // …原有内容…
```

标题行加按钮（在 `ui.label(RichText::new(&title)..)` 那一行所在的 horizontal 里）：

```rust
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&title).color(theme::c32(t.fg_mid)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if s.maximized { "还原" } else { "最大化" };
                    if ui.small_button(label).clicked() {
                        s.maximized = !s.maximized;
                    }
                });
            });
```

编辑区的 `ScrollArea` 去掉 `max_height(360.0)`，改成按剩余高度：

```rust
            // C 组:高度跟着窗口走。**减去底部按钮行的预算** —— 不减的话
            // `ScrollArea` 会把可用高度吃光,保存/关闭那一行被挤出窗口。
            let reserve = ui.spacing().interact_size.y + ui.spacing().item_spacing.y * 2.0;
            let h = (ui.available_height() - reserve).max(80.0);
            egui::ScrollArea::vertical()
                .max_height(h)
                .show(ui, |ui| {
```

- [ ] **Step 4：跑，确认绿**

```bash
cargo test -p mullion-app editor 2>&1 | grep "test result"
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED" /tmp/test.log
```

- [ ] **Step 5：变异验收 + 提交**

变异：把 `.anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))` 加回去 → 测试红。改回。

```bash
git add -u
git commit -m "feat(app): 内置编辑器可拖可缩 + 最大化 (F53)

原来带 anchor(CENTER_CENTER) 锚死位置(egui 0.30 设了 anchor 就忽略拖拽
位移)、编辑区 max_height 写死 360 —— 合起来就是「没法全屏」。

现在窗口能拖能缩,标题行加最大化/还原;编辑区高度按可用高度减去底部按钮
行的预算算(不减的话 ScrollArea 吃光高度,保存/关闭被挤出窗口)。

「能打字」由 Task 1 的模态门控修复保证 —— 编辑器过去不在 modal_open 表里,
键盘全被判给终端(T8)。

守护测试 the_editor_window_is_neither_anchored_nor_height_locked。
拖拽/缩放/最大化的实际手感需人工验收。"
```

---

## Task 9：E1 —— 标签整块可点

**Files:**
- Modify: `crates/mullion-app/src/ui/chrome.rs:221-290`（`one_tab`）

**头号嫌疑**：标题那个 `Label`。egui 0.30 默认 `interaction.selectable_labels = true`，可选中文本的 Label 会 sense click，抢走落在标题上的命中。

- [ ] **Step 1：写失败测试**

```rust
/// E1:标签矩形内**任何**不在 × 上的点都要能切换标签。
///
/// 过去点标题文字那一片没反应:egui 0.30 默认 `selectable_labels = true`,
/// 标题 Label 会 sense click 把命中抢走,而它自己什么也不做。
///
/// 自证会变红:把标题 `Label` 的 `.selectable(false)` 去掉。
#[test]
fn clicking_anywhere_on_a_tab_switches_to_it() {
    let t = Theme::dark();
    // 标签 1 是活动的,点标签 2 的**标题正中**应该切过去。
    let views = [
        TabView { title: "第一个", active: true, appearance: None },
        TabView { title: "第二个", active: false, appearance: None },
    ];
    let ctx = egui::Context::default();
    // 取矩形的前提:标注模式必须开着,否则 `mark` 直接 return(Task 0)。
    crate::ui::annotate::toggle(&ctx);
    let mut got = None;
    let mut rect = None;
    for frame in 0..4 {
        let mut input = egui::RawInput::default();
        if frame == 3 {
            if let Some(r) = rect {
                // 标题区 = 标签矩形去掉右侧的 × 之后的左半部分中心。
                let p = egui::pos2(r.left() + r.width() * 0.35, r.center().y);
                input.events.push(egui::Event::PointerMoved(p));
                input.events.push(egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                });
                input.events.push(egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::default(),
                });
            }
        }
        ctx.run(input, |ctx| {
            if let Some(a) = tab_bar(ctx, &t, &views) {
                got = Some(a);
            }
        });
        rect = crate::ui::annotate::spot_rect(&ctx, "标签栏/标签 2");
    }
    assert_eq!(
        got,
        Some(TabAction::Switch(1)),
        "点标签 2 的标题区没切过去 —— 标题 Label 把点击吃了"
    );
}
```

- [ ] **Step 2：跑，确认红**

```bash
cargo test -p mullion-app clicking_anywhere_on_a_tab 2>&1 | grep -B3 "test result"
```

- [ ] **Step 3：修**

`chrome.rs:266-273` 的标题 Label 加 `.selectable(false)`：

```rust
            ui.add(
                egui::Label::new(egui::RichText::new(v.title).color(theme::c32(if v.active {
                    t.fg_strong
                } else {
                    t.fg_muted
                })))
                .truncate()
                // E1:**必须关掉**。egui 0.30 默认 `selectable_labels = true`,
                // 可选中文本的 Label 会 sense click 把命中抢走,自己又不做
                // 任何事 —— 现象是「点标签的文字那一片没反应,点空白处才切」。
                .selectable(false),
            );
```

**若测试仍红**：图标那个 `allocate_exact_size(.., Sense::hover())` 也可能挡路（hover sense 不吃 click，但会影响命中链）；再检查 `content` 那个 `new_child` 是否注册了背景交互。逐个排除，每排除一个跑一次测试。

- [ ] **Step 4：跑，确认绿 + 变异验收 + 提交**

变异：去掉 `.selectable(false)` → 测试红。改回。

```bash
git add -u
git commit -m "fix(app): 标签整块可点 —— 标题 Label 抢走了点击命中 (F36)

egui 0.30 默认 selectable_labels = true,标题那个 Label 会 sense click
把命中吃掉、自己又不做任何事 —— 现象是点标签文字那一片没反应,点空白处
才切得过去。

守护测试 clicking_anywhere_on_a_tab_switches_to_it(注入真实点击到标题区
正中,预热三帧后才点)。"
```

---

## Task 10：E2/E3 —— 标签属性弹窗（改名 + 配色）

**Files:**
- Create: `crates/mullion-app/src/ui/tab_props.rs`
- Modify: `crates/mullion-app/src/ui/chrome.rs`（`TabAction` 加变体、`one_tab` 加双击/右键）
- Modify: `crates/mullion-app/src/ui/mod.rs`（`UiState` 加字段、`build_ui` 调 `tab_props::show`、`UiActions` 加保存意图）
- Modify: `crates/mullion-app/src/app.rs`（`Modal` 加变体、`touched_store` 加一项、标签标题取值与同步）

**这是本切片最大的一个任务**，分两次提交：先弹窗本身（Step 1-6），再 app 侧接线（Step 7-11）。

- [ ] **Step 1：`TabAction` 加变体 + `one_tab` 接双击/右键**

`chrome.rs:158-164`：

```rust
pub enum TabAction {
    Switch(usize),
    Close(usize),
    NewSession,
    /// E2/E3:请求打开「标签属性」弹窗(改名 + 配色)。双击标签、或右键
    /// 菜单里选「重命名…」/「设置颜色…」都发它。
    ///
    /// **只是「请求打开弹窗」,不是「已经改好了」** —— 真正的写回要等
    /// 用户在弹窗里按保存(同 `FileAsk` 那条约定:右键点一下就把东西改了
    /// 这种事不该存在)。
    Props(usize),
}
```

`one_tab` 末尾的判定链加两条（**排在 `closed` 之后、`clicked` 之前**）：

```rust
    let double = resp.double_clicked();
    let mut props = false;
    resp.context_menu(|ui| {
        // E3:没有会话记录的标签(快速连接 / 占位标签)改不了名也配不了色 ——
        // 改的是会话记录本身。禁用而不是隐藏:隐藏了用户会以为功能不存在。
        let has_session = v.appearance.is_some();
        if ui.add_enabled(has_session, egui::Button::new("重命名…")).clicked() {
            props = true;
            ui.close_menu();
        }
        if ui.add_enabled(has_session, egui::Button::new("设置颜色…")).clicked() {
            props = true;
            ui.close_menu();
        }
        if !has_session {
            ui.label(egui::RichText::new("这个标签没有对应的会话记录")
                .color(theme::c32(t.fg_dimmer)));
        }
        ui.separator();
        if ui.button("关闭").clicked() {
            closed = true;
            ui.close_menu();
        }
    });
```

**同时给 `TabView` 加 `session_id` 字段**（这一步是动作，不是建议）：`TabView` 现在只有 `appearance: Option<&Appearance>`，它为 `None` 既可能是「没有会话记录」也可能是「这条会话没配颜色」——拿它当 `has_session` 的判据会把「配过色但没记录」和「有记录但没配色」两种情况都判错。

```rust
pub struct TabView<'a> {
    pub title: &'a str,
    pub active: bool,
    /// E3:改名与配色改的是**会话记录**,没有记录的标签(快速连接 /
    /// 占位标签)两项都得禁掉。不能拿 `appearance.is_some()` 代替 ——
    /// 那是「有没有配过颜色」,不是「有没有会话记录」。
    pub session_id: Option<mullion_store::SessionId>,
    pub appearance: Option<&'a Appearance>,
}
```

`Tab<C>` 上本来就有 `session_id`（`shell/tabs.rs:38`），调用方（`app.rs:5269` 附近构造 `TabView` 的地方）直接传。

> **加字段会让 Task 9 的测试编译不过**（它构造的 `TabView` 少一个字段）——给那两处补上 `session_id: None`。这是加字段的正常连带修改，不是把 Task 9 的成果推翻。

上面 `one_tab` 里的 `has_session` 相应改成 `v.session_id.is_some()`。

返回链改成：

```rust
    if closed {
        Some(TabAction::Close(ix))
    } else if props || double {
        Some(TabAction::Props(ix))
    } else if clicked {
        Some(TabAction::Switch(ix))
    } else {
        None
    }
```

- [ ] **Step 2：写「双击发 Props」的测试**

```rust
/// E2:双击标签 = 请求打开属性弹窗(改名 + 配色),不是切换。
///
/// 自证会变红:把 `props || double` 里的 `double` 去掉。
#[test]
fn double_clicking_a_tab_asks_for_the_properties_dialog() {
    let t = Theme::dark();
    let views = [
        TabView { title: "第一个", active: true, session_id: None, appearance: None },
        TabView { title: "第二个", active: false, session_id: Some(SessionId::from_raw(7)), appearance: None },
    ];
    let ctx = egui::Context::default();
    crate::ui::annotate::toggle(&ctx);
    let mut got = None;
    let mut rect = None;
    for frame in 0..4 {
        let mut input = egui::RawInput::default();
        // egui 的双击判定看**两次点击的时间间隔**,间隔要小于
        // `double_click_delay`(0.3s)。`RawInput::time` 不推进的话所有事件
        // 都在 t=0,判定不出「两次」;推得太大就变成两次单击。每帧 +0.05。
        input.time = Some(f64::from(frame) * 0.05);
        if frame == 3 {
            if let Some(r) = rect {
                let p = egui::pos2(r.left() + r.width() * 0.35, r.center().y);
                input.events.push(egui::Event::PointerMoved(p));
                for _ in 0..2 {
                    input.events.push(egui::Event::PointerButton {
                        pos: p,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::default(),
                    });
                    input.events.push(egui::Event::PointerButton {
                        pos: p,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::default(),
                    });
                }
            }
        }
        ctx.run(input, |ctx| {
            if let Some(a) = tab_bar(ctx, &t, &views) {
                got = Some(a);
            }
        });
        rect = crate::ui::annotate::spot_rect(&ctx, "标签栏/标签 2");
    }
    assert_eq!(
        got,
        Some(TabAction::Props(1)),
        "双击标签 2 没请求属性弹窗(拿到的是 {got:?})"
    );
}
```

> `SessionId::from_raw` 的真实构造方式先确认：`grep -n "impl SessionId" -A 12 crates/mullion-store/src/model.rs`。没有公开构造函数就用 `store` 里现成的取值方式，或给测试加一个 `#[cfg(test)]` 构造。
>
> **若双击判定在 harness 里跑不出来**（两次点击被合成一次单击，或 `got` 拿到 `Switch(1)`）：先试把每帧 `time` 步长调到 `0.02`，仍不行就**改为只测右键菜单那条路径**（`resp.context_menu` 在无头下也不好触发，那就退到扎源码断言 `chrome.rs` 里含 `resp.double_clicked()`），并在人工验收清单里点名"双击标签改名"。**不要放宽断言让它过**——把 `assert_eq!` 改成 `assert!(got.is_some())` 是恒绿模式（`Switch(1)` 也满足）。

- [ ] **Step 3：写弹窗**

新建 `crates/mullion-app/src/ui/tab_props.rs`：

```rust
//! E2/E3:标签属性弹窗 —— 改名 + 配色。
//!
//! **改的是会话记录本身**(`identity.name` / `appearance.color`),不是
//! 「只对这个标签生效的运行期覆盖」:那要新增一份不持久化的标题/颜色
//! 状态,与 F37 的布局持久化语义打架(关窗口存的是会话 id,不是标题)。
//!
//! **这个弹窗必须同时登记进两张表**(切片 I 的教训):
//! - `app.rs::modal_open` 的 `Modal` 枚举 —— 否则里面敲的字会漏给远端 shell(T8)
//! - `app.rs::touched_store` —— 否则改了颜色要重启才看得见(F61/F62 的
//!   `AppearanceCache` 只在 store 变更后 rebuild,陷阱 T3)

use mullion_store::{ColorSpec, ColorTarget, SessionId};

/// 弹窗的编辑缓冲。
pub struct TabPropsDraft {
    pub session_id: SessionId,
    pub name: String,
    /// `None` = 不配颜色(退回主题 accent)。
    pub color: Option<egui::Color32>,
    pub targets: Vec<ColorTarget>,
}

/// 用户在弹窗里按下的东西。
#[derive(Debug, Clone, PartialEq)]
pub enum TabPropsAction {
    /// 保存到会话记录。
    Save {
        session_id: SessionId,
        name: String,
        color: Option<ColorSpec>,
    },
    Cancel,
}

pub fn show(
    ctx: &egui::Context,
    t: &crate::theme::Theme,
    draft: &mut Option<TabPropsDraft>,
) -> Option<TabPropsAction> {
    let d = draft.as_mut()?;
    let mut action = None;
    let mut close = false;
    egui::Window::new("标签属性")
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            crate::ui::annotate::mark(ui.ctx(), "标签属性".to_string(), ui.max_rect());
            ui.horizontal(|ui| {
                ui.label("名称");
                ui.add(egui::TextEdit::singleline(&mut d.name)
                    .desired_width(crate::ui::metrics::FIELD_W_M));
            });
            ui.add_space(crate::ui::metrics::SP_S);
            ui.horizontal(|ui| {
                ui.label("颜色");
                let mut c = d.color.unwrap_or(theme::c32(t.accent));
                if ui.color_edit_button_srgba(&mut c).changed() {
                    d.color = Some(c);
                }
                if ui.button("清除").clicked() {
                    d.color = None;
                }
            });
            ui.add_space(crate::ui::metrics::SP_S);
            ui.label("应用到");
            // ColorTarget 的真实变体(`mullion-store/src/model.rs:170`):
            // Tab / ListItem / PaneTitle / StatusBar。四个都列,别自造变体。
            for target in [
                ColorTarget::Tab,
                ColorTarget::ListItem,
                ColorTarget::PaneTitle,
                ColorTarget::StatusBar,
            ] {
                let mut on = d.targets.contains(&target);
                if ui.checkbox(&mut on, target_label(target)).changed() {
                    if on {
                        d.targets.push(target);
                    } else {
                        d.targets.retain(|x| *x != target);
                    }
                }
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("保存").clicked() {
                    action = Some(TabPropsAction::Save {
                        session_id: d.session_id,
                        name: d.name.clone(),
                        color: d.color.map(|c| ColorSpec {
                            hex: format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b()),
                            apply_to: d.targets.clone(),
                        }),
                    });
                    close = true;
                }
                if ui.button("取消").clicked() {
                    action = Some(TabPropsAction::Cancel);
                    close = true;
                }
            });
        });
    if close {
        *draft = None;
    }
    action
}
```

> **先核对三件事**：(1) `ColorTarget` 的真实变体（`grep -n "enum ColorTarget" -A 8 crates/mullion-store/src/model.rs`）；(2) `ColorSpec` 的字段名；(3) `metrics::FIELD_W_M` / `SP_S` 是否 `pub`。名字对不上按真实的改。

- [ ] **Step 4：给 hex 格式化配测试**

```rust
/// E3:颜色写回 store 的格式必须是 `#RRGGBB` —— `ColorSpec::hex` 的
/// 消费方(`badge::should_paint`)按这个格式解析,格式不对颜色会静默不生效。
///
/// 自证会变红:把 `{:02X}` 改成 `{:X}`(小于 16 的分量会少一位)。
#[test]
fn the_color_is_written_back_as_six_digit_hex() {
    let c = egui::Color32::from_rgb(0x0A, 0xB5, 0x03);
    let hex = format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b());
    assert_eq!(hex, "#0AB503");
    assert_eq!(hex.len(), 7, "hex 必须是 #RRGGBB 七个字符");
}
```

> 这条测试**重言式风险高**（断言的是刚写下的常量表达式）。改进：把格式化抽成 `pub fn hex_of(c: egui::Color32) -> String`，测试调它。这样变异（改 `{:02X}` → `{:X}`）才真的能让它红。**按抽函数的写法做。**

- [ ] **Step 5：接进 `build_ui` + `UiState`**

`ui/mod.rs`：`UiState` 加 `pub tab_props: Option<tab_props::TabPropsDraft>,`；`UiActions` 加 `pub tab_props: Option<tab_props::TabPropsAction>,`；`build_ui` 里在 `files_dialog::show` 附近加 `actions.tab_props = tab_props::show(ctx, t, &mut ui_state.tab_props);`。

**注意**：`ui/mod.rs` 有一条既有纪律——「加字段时记得同步 `app.rs::has_real_action`」（`ui/mod.rs:431-432`）。`tab_props` 是会落库的动作，**必须加进 `has_real_action`**，否则它会在 egui 的 discard 趟被静默吃掉。

- [ ] **Step 6：提交（弹窗部分）**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
git add crates/mullion-app/src/ui/tab_props.rs && git add -u
git commit -m "feat(app): 标签属性弹窗 —— 双击/右键改名与配色 (F36/F61/F62)

双击标签或右键菜单里选「重命名…」/「设置颜色…」打开同一个弹窗。没有
对应会话记录的标签(快速连接 / 占位标签)两项禁用并写明原因 —— 改的是
会话记录本身,不做只对本标签生效的运行期覆盖(那与 F37 的布局持久化打架)。

app 侧写回与缓存重算在下一笔。"
```

- [ ] **Step 7：`Modal` 加变体（T8）**

`app.rs` 的 `Modal` 枚举加 `TabProps`，`ALL` 加一项，`match` 加一臂 `Modal::TabProps => self.ui.tab_props.is_some(),`，并把测试里的 `VARIANT_COUNT` 从 10 改成 11。

**这一步不做的话，弹窗里敲的标签名会同时发给远端 shell。**

- [ ] **Step 8：`touched_store` 加一项（T3/F61/F62）**

`app.rs:5721-5727`：

```rust
                let touched_store = self.ui.delete_request.is_some()
                    || self.ui.save_request.is_some()
                    || self.ui.group_intent.is_some()
                    || self.ui.move_to_group.is_some()
                    || self.ui.import_request.is_some()
                    // E3:标签属性弹窗改的是会话的 identity.name /
                    // appearance.color —— 不算进来的话,改了颜色要重启才
                    // 看得见(AppearanceCache 只在 store 变更后 rebuild)。
                    || self.ui.tab_props_save.is_some();
```

> `tab_props_save` 是把 `UiActions::tab_props` 里的 `Save` 意图落到 `UiState` 上的字段——按既有 `save_request` 的模式加，或直接判 `actions.tab_props` 里是不是 `Save`。**按 `app.rs` 里既有的 intent 施加模式走**，别自创第三种。

配一条守护测试（照抄 `importing_sessions_counts_as_touching_the_store_so_the_look_is_recomputed` 的扎源码写法）：

```rust
/// F61/F62:标签属性弹窗改的是会话的颜色 —— 不算进 `touched_store` 的话,
/// 用户改了颜色,标签横杠要等下次重启才变。
///
/// 自证会变红:把 `touched_store` 里的 `tab_props` 那一项删掉。
#[test]
fn renaming_a_tab_counts_as_touching_the_store_so_the_look_is_recomputed() {
    let src = include_str!("app.rs");
    let after = src.split("let touched_store = ").nth(1)
        .expect("找不到 touched_store 的赋值");
    let expr = &after[..after.find(";\n").expect("找不到该赋值的结尾")];
    assert!(expr.contains("self.ui.save_request"), "切歪了 —— 下面那条会空过");
    assert!(
        expr.contains("tab_props"),
        "标签属性没算进 touched_store:改了颜色要重启才看得见(F61/F62)"
    );
}
```

- [ ] **Step 9：写回 store + 同步已开标签的标题**

在 intent 施加区（`app.rs:5736` 那一片）加：

```rust
                if let Some(crate::ui::tab_props::TabPropsAction::Save {
                    session_id, name, color,
                }) = self.ui.tab_props_save.take()
                {
                    if let Some(store) = self.store.as_mut() {
                        // 写回会话记录。
                        if let Some(rec) = store.get_mut(session_id) {
                            rec.identity.name = name.clone();
                            rec.appearance.color = color;
                        }
                        match store.save() {
                            Ok(()) => self.ui.set_toast("已保存标签属性"),
                            Err(e) => self.ui.set_error(format!("保存失败:{e}")),
                        }
                    }
                    // E2:**眼前的标签必须立刻跟着变**。标签标题是连接时拼的
                    // `user@host` 运行态快照,不引用 `identity.name` —— 只写回
                    // store 的话,这个标签纹丝不动,用户会认为改名坏了。
                    // 同步**所有**引用该会话的标签,不只是活动那个。
                    for tab in self.tabs.iter_mut() {
                        if tab.session_id == Some(session_id) && !name.is_empty() {
                            tab.title = name.clone();
                        }
                    }
                    self.ui_dirty = true;
                }
```

> `store.get_mut(session_id)` 的真实 API 先确认：`grep -n "pub fn get_mut\|pub fn update\|pub fn upsert" crates/mullion-app/src/shell/store.rs crates/mullion-store/src/vault.rs`。没有可变借用接口就用「取出 → 改 → upsert」的既有模式（`save_request` 那条路径怎么写的就照着写）。

- [ ] **Step 10：新标签标题优先取会话名**

`app.rs:4203-4205`：

```rust
                let title = cfg
                    .as_ref()
                    .map_or_else(|| "远端".to_string(), |c| format!("{}@{}", c.user, c.host));
```

改成：

```rust
                // E2:标题优先取会话名 —— 否则改过名的会话下次连上又变回
                // `user@host`,用户会以为改名没存住。会话名为空(或没有会话
                // 记录)才退回 `user@host`。
                let session_name = session_id
                    .and_then(|id| self.store.as_ref().and_then(|s| {
                        s.list().iter().find(|r| r.id == id)
                            .map(|r| r.identity.name.clone())
                    }))
                    .filter(|n| !n.is_empty());
                let title = session_name.unwrap_or_else(|| {
                    cfg.as_ref()
                        .map_or_else(|| "远端".to_string(), |c| format!("{}@{}", c.user, c.host))
                });
```

配一条纯函数测试——**把这段取值抽成纯函数**才测得动：

```rust
/// E2:标签标题优先取会话名,空名字退回 `user@host`。
///
/// 抽成纯函数才测得动 —— 原地写在 `ConnectOk` 分支里要造一个真 `App`。
///
/// 自证会变红:把 `.filter(|n| !n.is_empty())` 去掉,空名字会让标题变空白。
#[test]
fn the_tab_title_prefers_the_session_name_but_falls_back_to_user_at_host() {
    assert_eq!(tab_title(Some("生产库"), Some(("root", "10.0.0.1"))), "生产库");
    assert_eq!(tab_title(Some(""), Some(("root", "10.0.0.1"))), "root@10.0.0.1");
    assert_eq!(tab_title(None, Some(("root", "10.0.0.1"))), "root@10.0.0.1");
    assert_eq!(tab_title(None, None), "远端");
}
```

- [ ] **Step 11：跑全量 + 变异验收 + 提交**

变异三次：(1) `Modal::ALL` 删 `TabProps` → 完备性测试红；(2) `touched_store` 删 `tab_props` → 那条扎源码测试红；(3) `tab_title` 去掉 `.filter(|n| !n.is_empty())` → 标题测试红。

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
git add -u
git commit -m "feat(app): 标签改名/配色写回会话,眼前的标签立刻跟着变 (F36/F61/F62)

三处接线,少一处功能就是半残的:
1. Modal 加 TabProps —— 不加的话弹窗里敲的标签名同时发给远端 shell(T8)
2. touched_store 加 tab_props —— 不加的话改了颜色要重启才看得见
   (AppearanceCache 只在 store 变更后 rebuild,T3)
3. 保存后同步所有引用该会话的标签的 title,且新标签标题优先取会话名 ——
   标题原本是连接时拼的 user@host 运行态快照,不引用 identity.name,
   只写回 store 的话眼前这个标签纹丝不动

守护测试 renaming_a_tab_counts_as_touching_the_store_so_the_look_is_recomputed /
every_modal_variant_is_listed_in_all /
the_tab_title_prefers_the_session_name_but_falls_back_to_user_at_host。"
```

---

## Task 11：G 组 —— 视觉规格走查

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs`、`crates/mullion-app/src/ui/chrome.rs`
- Create: `docs/ui-walkthrough-slice-j.md`（走查清单，随本切片留档）

**判据**：`spec.md` §4.6（F80~F85 已冻结的色板与尺寸）+ `docs/ui-form-guidelines.md` 里两条与表单无关也成立的纪律（间距只用 `SP_XS/S/M/L/XL` 五档；颜色只用 theme token）。

**不做**：扩 `tests/form_guidelines.rs` 的扫描范围。它扫的是 `add_space(数字)` / `desired_width(数字)`，而 `files_panel.rs` 几乎全用 painter 按坐标画，扫不到——把它加进 `EXTRA` 会得到一条**一条都扫不到却绿着**的测试，那正是本项目反复踩的恒绿模式。

- [ ] **Step 1：产出清单**

```bash
grep -nE "[0-9]+\.[0-9]" crates/mullion-app/src/ui/files_panel.rs | grep -v "^\s*//" > /tmp/walkthrough-files.txt
grep -nE "[0-9]+\.[0-9]" crates/mullion-app/src/ui/chrome.rs | grep -v "^\s*//" > /tmp/walkthrough-chrome.txt
wc -l /tmp/walkthrough-*.txt
```

把结果整理成 `docs/ui-walkthrough-slice-j.md`，每条写：文件:行、当前值、违反哪条、建议档位、**改了会不会动观感**。

- [ ] **Step 2：收敛（只改不动观感的）**

把落在五档上的裸数字换成具名常量：`4.0` → `SP_XS`、`8.0` → `SP_S`、`12.0` → `SP_M`、`16.0` → `SP_L`。

**不在档位上的**（如 `ROW_H = 22.0`、圆角 `2.0`）：**不硬凑**。`22.0` 收敛到 `24.0` 会让行变高、可见行数变少——这种在清单里单独标注「需人工验收」，本任务**不改**。

圆角是另一套刻度（不是间距），若 `spec.md` §4.6 没定义圆角档位，保持现状并在清单里记一笔「圆角无规格，下次视觉走查时定」。

- [ ] **Step 3：跑全量 + 提交**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
git add docs/ui-walkthrough-slice-j.md && git add -u
git commit -m "refactor(app): 文件面板与标签栏视觉走查 —— 裸数字收敛到 SP_* 五档 (F80)

按 spec §4.6 与 ui-form-guidelines 的两条通用纪律走查,清单留档在
docs/ui-walkthrough-slice-j.md。

只收敛「换了不动观感」的那批(落在五档上的裸数字换成具名常量)。不在档位
上的(ROW_H=22、圆角 2.0)不硬凑 —— 22 收敛到 24 会让行变高、可见行数变少,
那是观感改动,单独标注进人工验收清单。

没有扩 tests/form_guidelines.rs 的扫描范围:它扫 add_space/desired_width,
而这两个文件几乎全用 painter 按坐标画,加进去会得到一条一条都扫不到却
绿着的测试。"
```

---

## Task 12：交付（按 CLAUDE.md「交付约定」一条龙）

**前置**：Task 1-11 全部完成且全绿。

- [ ] **Step 1：升 patch 版本号**

```bash
grep -n "^version" Cargo.toml
```
把 `workspace.package.version` 第三位 +1（当前 `0.1.42` → `0.1.43`）。

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: 版本 0.1.43(文件面板与标签栏打磨:三个真 bug + 两栏方位 + 图标/属主列 + 标签改名配色 + 模态门控)"
```

- [ ] **Step 2：跑绿（三件套，缺一不发）**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 3：交叉编译 + objdump 验收**

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app
```

按 `docs/cross-compile-windows.md` 做依赖验收——**出现 `libgcc_s_seh-1.dll` 或 `libwinpthread-1.dll` 即为不合格，必须修**：

```bash
x86_64-w64-mingw32-objdump -p target/x86_64-pc-windows-gnu/release/mullion.exe \
  | grep "DLL Name"
```

- [ ] **Step 4：发 Release**

```bash
cd target/x86_64-pc-windows-gnu/release
sha256sum mullion.exe > mullion.exe.sha256
```

`notes.md` 必须写：修了什么 + **人工验收清单** + sha256 + 首次运行提示（`Unblock-File .\mullion.exe`）。

人工验收清单（从各 Task 汇总，逐条抄进 notes）：
- [ ] 滚动条不再串栏、不再侵入对面栏（Task 3）
- [ ] 点列头真的排序，五个列头都点得中（Task 2）
- [ ] 断联后「重连」按钮真能连回来；点没权限的目录**不**转断开态（Task 4）
- [ ] 两栏方位：标签宿主左本地右远端；侧栏上本地下远端且远端更高（Task 5）
- [ ] 文件/文件夹/链接图标画得对，CJK 文件名对齐没乱（Task 6）
- [ ] 属主列显示 `uid:gid`，本地栏是 `—`；按属主排序有效（Task 7）
- [ ] 编辑器能拖、能缩、能最大化，**能打字**（Task 1 + 8）
- [ ] 新建文件夹框 / 分组管理器开着时打字**不漏给远端 shell**（Task 1）
- [ ] 标签整块可点（点标题文字也能切）（Task 9）
- [ ] 双击标签改名、右键配色；改完**眼前的标签立刻变**，不用重连（Task 10）
- [ ] 走查改动后的整体观感（Task 11）

```bash
HTTPS_PROXY=http://127.0.0.1:7890 gh release create v0.1.43 \
  mullion.exe mullion.exe.sha256 -t "v0.1.43" -F notes.md --repo kilobitcy/Mullion
```

**Release 标题只能是纯版本号 `v0.1.43`** —— 不带破折号、不带摘要、不带 emoji。

- [ ] **Step 5：报告**

给出 Release 链接 + sha256 + 上面那份人工验收清单。

---

## 附：Task 之间的依赖

```
Task 0 (spot_rect) ──────────► Task 2, 3, 5, 9, 10 (全部交互测试)
Task 1 (modal) ──────────────► Task 8 (编辑器能打字)
                       └─────► Task 10 Step 7 (弹窗不漏键)
Task 3 (set_clip_rect) ──────► Task 5 (方位调转沿用那两行)
Task 6 (W_ICON/ICON_GAP) ────► Task 7 (name_w 要减它们)
Task 9 (标签可点) ───────────► Task 10 (双击/右键挂在同一个 resp 上)
Task 1-11 ───────────────────► Task 12 (交付)
```

其余任务之间无依赖，但**建议按编号顺序执行**：Task 2/3 会大改 `files_panel.rs` 的同一片区域，并行做会冲突。
