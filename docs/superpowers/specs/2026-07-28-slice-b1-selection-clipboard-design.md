# 切片 B1 设计 —— 划选复制 / 粘贴（F18）

日期：2026-07-28
状态：已确认，待写实现计划
上一片：切片 B0（F17 滚动回溯 + F3 TOFU），v0.1.7 已实机验收

---

## 1. 目标

让用户能在 Mullion 里把远端终端的文本选出来带走，也能把本地内容贴进去。

日常场景是：在 Windows 上通过高延迟链路操作远端 tmux 里的 Claude Code——
选中报错行贴给别处、把本地代码片段贴进 Claude Code 的输入框。当前这两件事
**完全做不到**：全仓无任何 clipboard / selection 代码。

切片 B0 刚做完滚动回溯（F17），但回溯出来的历史内容没法复制，价值只发挥了一半。

## 2. 范围

| 项 | 做 | 说明 |
|---|---|---|
| 左键拖拽划选 + 反色高亮 | ✅ | 基础项 |
| 双击选词 / 三击选行 | ✅ | 走 alacritty 的 `Semantic` / `Lines` |
| 拖到窗口上/下边缘自动滚动 | ✅ | 选区可跨越多屏 scrollback |
| 选中即复制（松开鼠标即入剪贴板） | ✅ | PuTTY / Xshell 习惯 |
| `Ctrl+Shift+C` 复制 | ✅ | 与选中即复制共存 |
| 右键粘贴 | ✅ | Windows 终端习惯，右键直接贴、不弹菜单 |
| `Ctrl+Shift+V` 粘贴 | ✅ | 与右键共存 |
| bracketed paste 包裹 | ✅ | 远端开启时用 `ESC[200~` / `ESC[201~` |
| 多行粘贴确认弹窗 | ✅ | **仅**「含换行 且 未开 bracketed paste」时弹 |

**非目标（本片不做）**：块选（矩形选区）、中键粘贴（X11 primary selection，
Windows 不是一等公民场景）、选区右键菜单、F19 终端内搜索、F20 链接识别。

## 3. 架构

依赖方向不变：`app → {core, term, ssh, store}`。

```
mullion-term   选区状态 + 坐标换算 + 复制取文本 + 粘贴编码（纯逻辑，可单测）
mullion-app    鼠标状态机 + 剪贴板 IO + 弹窗 + 反色渲染
```

**alacritty 的选区类型不外泄给 app。** `SelectionType` / `Point` / `Side` 都封在
mullion-term 内，app 只传 0-based viewport 单元格坐标和我们自己的 `SelectionKind`。
这与 B0 重导出 `TermMode` / `Scroll` 的口径一致：能封的就封，封不掉的才重导出。

### 3.1 mullion-term：Emulator 选区 API

```rust
pub enum SelectionKind { Simple, Semantic, Lines }   // 拖拽 / 双击选词 / 三击选行
pub enum CellSide { Left, Right }                     // 指针落在格内左半还是右半

impl Emulator {
    pub fn selection_start(&mut self, col: u16, row: u16, kind: SelectionKind, side: CellSide);
    pub fn selection_update(&mut self, col: u16, row: u16, side: CellSide);
    pub fn selection_clear(&mut self);
    pub fn selection_text(&self) -> Option<String>;
}
```

参数是 **0-based viewport 单元格**（左上角为 `(0, 0)`）。内部换算成 alacritty 的
`Point { line: Line(row as i32 - display_offset as i32), column: Column(col as usize) }`。

**这个换算是本片最容易错的地方**：alacritty 的 `Line` 带符号，`0` 是当前视口顶行，
负数是历史。滚上去之后同一个屏幕位置对应的 buffer 行会变。必须有测试钉死
「滚上 100 行后，同一像素位置选出来的仍是同一段文本」。

上游在 `Term::scroll_display` 里会自己 `rotate` 选区，所以**滚动时选区跟随是白拿的**，
我们不要自己再补一份跟随逻辑。

复制取文本走 `Term::selection_to_string()`：宽字符、行尾空格裁剪、跨 scrollback
拼接都是上游已实现且久经考验的行为，不重造。

### 3.2 mullion-term：粘贴编码

```rust
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8>;
```

- `bracketed == true` → `ESC[200~` + 净化后的文本 + `ESC[201~`
- `bracketed == false` → 只有净化后的文本
- 净化两件事：
  1. **剔除文本里出现的 `ESC[201~`**。否则粘贴内容可以提前闭合括号，让后半段脱离
     paste 模式被当命令执行——alacritty / wezterm 都防这个，这是真实的注入面。
  2. `\r\n` 与 `\n` 统一转成 `\r`。终端里 Enter 是 CR，不是 LF；不转的话远端 readline
     行为会怪。

### 3.3 mullion-app：三个纯函数（`input.rs`）

沿用 B0 的做法——能脱离窗口测的全抽成纯函数：

```rust
pub fn cell_side(px_x: f32, cell_w: f32) -> CellSide;
pub fn click_kind(prev: Option<PrevClick>, now: Instant, pos: (u16, u16)) -> SelectionKind;
pub fn autoscroll_lines(px_y: f32, win_h: f32, cell_h: f32) -> i32;
```

- `cell_side`：指针在格内的左/右半区。决定该格算不算进选区，直接影响"跟手"。
- `click_kind`：双击 / 三击自己做——winit 不提供。`Instant` 作为参数传入而不是函数内
  取当前时间，否则没法测。时间窗 + 位置容差，连击位置漂移超过 1 格就重新计数。
- `autoscroll_lines`：指针拖出上/下边界时每帧滚几行，在边界内返回 `0`。越界越多滚越快。

`cell_at`（像素 → 1-based 单元格）B0 已有，直接复用；选区 API 要 0-based，转换处
写一行注释说明，避免又一次 off-by-one。

### 3.4 mullion-app：鼠标状态机（事件循环）

事件循环本身不含可测逻辑，只做状态流转：

| 事件 | 动作 |
|---|---|
| `MouseInput{Left, Pressed}` | 清旧选区 → `click_kind` 定类型 → `selection_start` |
| `CursorMoved`（左键按住） | `selection_update` + `autoscroll_lines` 非 0 时滚动 |
| `MouseInput{Left, Released}` | 选区非空 → 写剪贴板（选中即复制） |
| `MouseInput{Right, Pressed}` | 读剪贴板 → 走粘贴流程 |
| 键盘输入（普通按键） | 清选区 |

**拖拽中的自动滚动必须走已有帧闸**（`frame.rs`），不能为了"跟手"绕过 T3/T7 的
帧率节流——鼠标不动但仍需滚动时，靠 `ControlFlow::WaitUntil` 下一帧唤醒，
且三分支都要显式复位 `control_flow`（T7）。

鼠标事件与 egui 的分流沿用 T8 既定规则：**指针先喂 egui 再判**，
键盘先判后喂。本片不动这条规则。

### 3.5 mullion-app：剪贴板

直接依赖 `arboard`（egui-winit 内部已在用，版本可对齐），app 层持一个实例，读写都直给。

不复用 egui 的剪贴板：egui 只有 `copy_text`，**读剪贴板只能靠 `Event::Paste` 且要 egui
有焦点**——按 T8 的教训（egui 焦点系统吞键导致终端永久收不到输入），不让 egui 掺和
终端输入路径。

剪贴板 IO 可能失败（Windows 上剪贴板被别的进程占用是常态）。失败一律
`log::warn!` + 忽略，不弹窗、不 panic：复制失败最多是用户再选一次，不值得打断。

### 3.6 mullion-app：粘贴确认弹窗

触发条件：**粘贴内容含换行 且 远端未开 bracketed paste**（`TermMode::BRACKETED_PASTE` 未置位）。
其余情况直接粘贴，不打扰——日常在 Claude Code 里贴代码（bracketed 已开）应当无感。

弹窗内容：前几行预览 + 总行数 + 「粘贴 / 取消」。复用 F3 主机密钥弹窗那套
`pending_*` 状态 + `build_ui` 绘制的模式。

与 F3 弹窗的区别：**这个弹窗可以关闭/取消**。F3 承载安全决策，不给"默默跳过"的出口；
这里取消 = 不粘贴，是明确且安全的默认，给关闭按钮没有歧义。

### 3.7 高亮渲染

`SnapCell` 加 `selected: bool`；`snapshot()` 用 `selection.to_range()` +
`SelectionRange::contains_cell` 填标记；`text.rs` 渲染时交换 fg / bg。

反色是纯查表，不加渲染 pass、不动 glyphon 的 buffer 重建路径。

## 4. 数据流

```
划选：  winit 鼠标事件 → input.rs 纯函数 → Emulator 选区 API → alacritty Selection
                                                    ↓
        snapshot() 填 selected → text.rs 反色                 松开 → selection_text()
                                                                        ↓
                                                                    arboard 写入

粘贴：  Ctrl+Shift+V / 右键 → arboard 读出 → 含换行且未开 bracketed？
                                              ├ 是 → 弹窗确认 → ↓
                                              └ 否 ────────────→ encode_paste() → SSH channel
```

## 5. 测试策略

**能自动测的（必须有）**：

| 层 | 测什么 |
|---|---|
| `mullion-term` | viewport (col,row) → alacritty `Point` 换算；滚动后选区仍指向同一段文本；三种 `SelectionKind` 各选出预期文本；空选区返回 `None`；`encode_paste` 的包裹、`ESC[201~` 剔除、`\r\n` → `\r` |
| `mullion-term` | `snapshot()` 的 `selected` 标记与选区范围一致（含宽字符两格都标中） |
| `mullion-app` | `cell_side` 半格边界；`click_kind` 的双击/三击时间窗与位置容差；`autoscroll_lines` 边界内为 0、越界方向与速度 |

**无头环境验不了，必须进人工验收清单**：

- 反色配色是否可读（尤其在已有背景色的单元格上）
- 拖拽跟手度、松开瞬间选区是否与视觉一致
- 自动滚动速度是否顺手（太快会冲过头）
- 双击选词的词边界是否符合直觉（路径 / URL / 带下划线的标识符）
- CJK 宽字符是否整字选中，不出现半个字
- 右键粘贴与 Windows 输入法右键行为是否冲突
- 剪贴板与 Windows 其他程序互通（复制到记事本、从浏览器粘贴进来）

## 6. 风险

| 风险 | 对策 |
|---|---|
| alacritty `Line` 带符号坐标换算错，选区与视觉错位 | 单测钉死滚动前后的一致性；这是本片头号坑 |
| 拖拽自动滚动绕过帧闸 → CPU 忙转（T3/T7 红线） | 自动滚动走 `WaitUntil`，事件循环三分支显式复位 `control_flow` |
| `arboard` 拉大 exe 体积（N6 已超标：33MB > 25MB） | 接完量一次体积，超出预期就在实现计划里记一笔，不在本片处理 |
| 右键粘贴与将来的右键菜单冲突 | 本片不做右键菜单（非目标）；真要加时再谈 |

## 7. 未决

无。技术选型（alacritty `Selection` + `arboard`）与交互口径均已确认。
