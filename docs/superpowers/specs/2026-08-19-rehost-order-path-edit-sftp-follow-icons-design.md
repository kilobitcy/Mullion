# 换节点排序 + 路径可编辑 + SFTP 跟随分屏 + 文件图标 —— 设计

> 需求编号 **F130**(换节点弹窗顺序对齐左栏)、**F131**(文件面板路径条可编辑)、
> **F132**(`Ctrl+Shift+B` 按焦点分屏的节点开 SFTP)、**F133**(office 类文件图标)、
> **F134**(默认文件图标拆分)——五条均为新增,待写进 `spec.md` §4。
> 日期 2026-08-19。

## 为什么放在一片里

同一次实机使用暴露出来的五个点,彼此没有依赖。落点分两簇:F130/F132 在
「哪台机器」这条线上(`rehost.rs` / `app.rs` 的 sftp 生命周期),F131/F133/F134
在文件面板(`files_panel.rs` / `file_icon.rs`)。分五次发版没有收益,合成一片。

**风险不均**:F132 动 `app.rs` 的 sftp 打开/替换路径,是这五条里唯一有真实
回归风险的;其余四条的改动面都收在单个模块内。

---

## ① F130 换节点弹窗的顺序对齐左栏

### 现状

`rehost::visible` 自己遍历 `sessions`,顺序 = store 里的数组顺序。会话管理器
左栏走的是 `group_manager::group_sessions` 归桶(先按 `groups` 的顺序出分组桶,
组内保持数组顺序,悬空 `group_id` 落末尾的「未分组」桶)。两者在有分组时**必然
不一致**:左栏里挨着的两条,在换节点弹窗里可能隔着半屏。

### 做什么

顺序的**唯一真值来源**改成 `session_manager::list::visible_order`:

```rust
fn visible<'a>(
    sessions: &'a [SessionRecord],
    groups: &[GroupRecord],
    needle: &str,
) -> Vec<&'a SessionRecord> {
    // 顺序:跟左栏同一个函数算出来的。搜索词传空 —— 弹窗有自己的搜索规则,
    // 借它的顺序、不借它的过滤。
    list::visible_order(sessions, groups, "", Protocol::Ssh)
        .into_iter()
        .filter_map(|id| sessions.iter().find(|r| r.id == id))
        .filter(|r| matches(r, needle))
        .collect()
}
```

`Protocol::Ssh` 这个参数同时替掉了原来那道 `protocol == Ssh` 过滤 —— SFTP
节点仍然不出现在列表里(它没有 PTY,换过去只有黑屏),既有测试
`sftp_nodes_are_not_offered_because_they_have_no_pty` 照旧守着。

`rehost::show` 增加 `groups: &[GroupRecord]` 参数,调用方在 `app.rs` 传当前
分组列表。**不画分组头**:弹窗钉在 pane 里,高度预算已经紧(`CHROME_H = 120`,
列表夹在 48~260),分组头会挤掉本来就有限的行数。

`list::visible_order` 目前是 `pub(crate)`,`list` 模块本身在 `session_manager`
下的可见性若不够,一并放宽到 `pub(crate)` —— 不新开第二套排序。

### 守护

- 新增:弹窗给出的 id 序列 == `visible_order(.., "", Ssh)`。自证会变红:
  把 `visible` 改回 `sessions.iter()`,再让 fixture 里的分组顺序与数组顺序
  相反。
- **fixture 必须让两种顺序真的不同**(至少两个分组、且组内成员在数组里交错),
  否则这条断言在任何实现下都绿(本项目已实证过的恒绿模式)。

---

## ② F131 路径条可编辑(远端栏 + 本地栏)

### 现状

`files_panel.rs:480` 一带,路径是一个 `egui::Label` + `.truncate()`。要换目录
只能一层层点/用书签。

### 真正的要害:面板焦点下键盘根本不喂 egui

`input_route::egui_should_see_focused` 在 `Focus::FilesPanel` 时对键盘返回
`false`(T8 的注入点)。也就是说:**直接放一个 `TextEdit` 进去,它一个字都收不到**
—— 键会被 `handle_panel_key` 吃掉,Backspace 变成「回上级目录」,字母键什么都
不做。这跟 `Modal::Editor` 当年踩的是同一个坑(那条注释写着「它是个多行输入框,
不算模态的话里面压根打不出字」)。

所以解法不是「让 Backspace 让路」,而是**编辑态登记进 `Modal`**:

- `enum Modal` 加一个变体 `FilesPathEdit`,同步加进 `Modal::ALL`
  (`modal_open` 是穷尽 `match`,漏加 `ALL` 会静默失效 —— 记忆里
  「列举式门控在加档时必然漏」已经踩过三次)。
- `modal_open` 里判据:`files_owner_generation()` 指向的那个标签的
  `PanelFrame::path_edit.is_some()`。
- **不进 `touched_store`**:它一行 store 都不写(同 `Rehost` 的姿态)。

模态期间键盘全归 egui,`TextEdit` 正常收字,中文输入法也照常走 egui 的 IME
路径;同时 `Ctrl+W` / `Ctrl+Shift+B` 自动被闸门挡住 —— 打路径时按到不该生效
的快捷键,本来也是不该发生的。

### 状态与交互

`PanelFrame` 加字段(`None` 是安全默认,满足它 `Default` impl 上那条约束):

```rust
pub path_edit: Option<PathEdit>,     // 一次只有一栏在编辑(焦点唯一)
pub struct PathEdit { pub column: PanelColumn, pub buf: String }
```

- 只读态:现在这个 `Label`,外面套一层 `Sense::click`。单击 → `path_edit =
  Some(当前 cwd 的显示字符串)`,并 `request_focus`(egui 的 `TextEdit` 首帧
  要显式给焦点,否则用户得再点一下)。
- 编辑态:`TextEdit::singleline`,固定 id(同 `rehost::row_id` 的理由 ——
  稳定 id 是可测的前提)。
  - **回车**:提交 → `FileAction::GotoInput(buf)`,退出编辑态。
  - **Esc**:退出编辑态,不跳转。
  - **失焦**:同 Esc(丢弃)。丢弃而不是提交:失焦最常见的原因是用户去点了
    别处,那不是「确认」。
- 目录变化(跳转成功/刷新)时不回写编辑缓冲 —— 编辑态本来就已经退出了。

### `~` 展开放在 app 侧,不在面板里

面板不知道远端登录目录(`sftp_home` 挂在 `TabContent` 上),也不该知道。新增:

```rust
FileAction::GotoInput(String)   // 用户在路径条里敲的原文,未解析
```

`app.rs` 的两个 dispatch 分支各自解析:

- 远端:`~` / `~/x` 用 `sftp_home` 展开(复用 `files_start_dir` 已有的那套
  展开逻辑,不新写一份);其余当绝对路径原样发。相对路径(`../x`)不在客户端
  拼接,交给远端解析 —— 客户端拼路径要处理符号链接,拼错就是跳到别的目录。
- 本地:`~` 用本地 home 展开;其余原样交给 `local` 那套。
- 空串 / 只有空白:当取消,不发请求。

失败照走既有路径:远端 `Load::Failed(msg)`、本地同理。**不静默吞掉** ——
「敲了回车什么都没发生」是这一片最容易做出来的坏交互。

### 守护

- `Modal::FilesPathEdit` 进了 `ALL` 且 `modal_open` 认它(照 `ExitConfirm`
  那条源码级测试的写法;**锚点字符串必须带行首缩进**,否则会匹配到测试自己
  那一行 —— 已实证的第五类恒绿模式)。
- `GotoInput` 的解析是纯函数,单测:`~` → home、`~/x` → home/x、`/abs` 原样、
  空串 → 不跳转、`~x`(不是 home 的写法)不展开。
- Esc / 失焦丢弃、回车提交:能在 egui 测试容器里驱动(同 `files_panel` 既有的
  点击测试脚手架,注意 `Area` 预热帧 + 显式推进时间)。

---

## ③ F132 `Ctrl+Shift+B` 按焦点分屏的节点开 SFTP

### 现状

`TabContent::sftp_connection` 取 `t.ws.hosts.first()`,注释里明写:

> **换节点(B2-b)之后这里仍指第一台机器**……属于既有限制。

于是:把某块分屏换到 B 机器、焦点停在它上面按 `Ctrl+Shift+B`,侧栏连的是 A,
而目录同步却用的是 B 上报的 cwd —— **路径对了、机器错了**,是一次看不出错的
误操作。

### 做什么

sftp 仍是**每标签一条 channel**(不按 host 缓存多条:面板浏览状态只有一份,
按 host 各留一份要连状态一起拆,改动面大得多,而且 channel 泄漏是 ADR-009 已经
列过的失效模式)。改成「记住它属于哪台,不对就重开」:

- `TerminalTab` 加 `sftp_host_ix: Option<usize>` —— 这条 client 是从
  `hosts[ix]` 上开的。`None` = 还没开过。
- `TabContent::sftp_connection` 拆成按下标取:`sftp_connection_for(host_ix)`。
  `Files` 宿主(SFTP 节点标签)恒返回自己独占的那条,行为不变。
- `trigger_sftp_open` 取**焦点 pane 的 `host_ix`**,并把它随
  `spawn_sftp_open` 一路带到 `UserEvent::SftpOpened` 事件里,由
  `accept_sftp_opened` 落到 `sftp_host_ix`。
  **不能在发起时就写**:开 channel 是异步的,期间焦点可能又换了,发起时的值
  才是这条 client 的真实归属。
- 「关→开」跃迁(`sync_files_to_focused_pane`,判据仍是跃迁而不是「侧栏开着」)
  扩成三种情形:
  1. 还没有 client → 原样交给 `trigger_sftp_open`(它已经会用焦点 pane 的 cwd
     定起始目录)。
  2. 有 client 且 `sftp_host_ix == 焦点 pane 的 host_ix` → 原有的 `Goto`
     同步目录,一个字节都不多发。
  3. 有 client 但**不是同一台** → 摘掉旧 client(`sftp_mut()` 置 `None`)、
     abort 在途 sftp 任务、远端栏置 `Load::Loading`、清空选中与光标,再走
     `trigger_sftp_open`。

摘 client 只是把 `Arc` 从槽位拿走:正在跑的传输仍持有它自己的 `Arc`,能跑完,
不会被腰斩。

- 焦点分屏的连接已经断了(`Disconnected` / `Reconnecting`):**不特判**。
  照常重开、失败落 `Load::Failed`,面板上写着这台连不上 —— 比悄悄留着另一台
  机器的目录让用户以为看的是这块分屏诚实得多。
- 热键语义仍是**纯开/关切换**(用户拍板)。侧栏开着时切分屏不动面板 ——
  否则用户在面板里点开的目录会被反复拽走。想跟到另一块分屏就按两下。

### 守护

- `sftp_host_ix` 的写入点在 `accept_sftp_opened`,不在发起处:源码级守护
  (锚点带缩进),说明理由。
- 纯函数化「这次跃迁该做什么」:输入(有没有 client、`sftp_host_ix`、焦点
  pane 的 `host_ix`、cwd),输出三选一(`Nothing` / `Goto(dir)` / `Reopen`)。
  三条分支各一条单测 —— 这是这一条里唯一测得动的核心,`App` 本身在无头容器
  里造不出来(需要 `EventLoopProxy`)。
- 换 host 时远端栏被重置(选中/光标清空):跟着上面那个纯函数一起测,判据是
  「重开」这条分支的返回值。

---

## ④ F133 office 大类的图标

### 现状

`file_icon.rs` 的 `EXT_TABLE` 把 `pdf` / `doc` / `docx` 全归进 `IconKind::Doc`
(页 + 三条横线),跟 `md` / `txt` / `log` / `json` 一个样。一屏文档目录扫下去,
Word、Excel、PDF 长得完全一样。

### 做什么

`IconKind` 加四个变体,`EXT_TABLE` 相应改:

| 变体 | 扩展名 | 颜色(theme 新增) | 形状(页 + 标记) |
|---|---|---|---|
| `Pdf` | `pdf` | `icon_pdf` 红 | 页 + 底部一条实心横条 |
| `Word` | `doc` `docx` | `icon_word` 蓝 | 页 + 一个折线 W |
| `Excel` | `xls` `xlsx` `csv` | `icon_excel` 绿 | 页 + 2×2 网格 |
| `Slides` | `ppt` `pptx` | `icon_slides` 橙 | 页 + 一块横向屏幕条 |

`csv` 从 `Doc` 移到 `Excel`:用户双击它多半是想到表格里看。颜色对齐 Windows
上的既有心智(红 PDF、蓝 Word、绿 Excel、橙 PPT)——这几条不是我们发明的约定,
跟着走比自创一套省一次学习。

`Doc` 保留,继续覆盖 `md` / `txt` / `log` / `json`。

### 列举式门控

`ALL_KINDS` 从测试模块**挪进生产代码**(`pub const ALL: &[IconKind]`),并新增
一条测试:`EXT_TABLE` 里出现过的每个 kind 都必须在 `ALL` 里。这样以后加档时
忘了更新 `ALL`,测试会红 —— 而不是让「两两长得不一样」「不越格」那两条既有
测试悄悄漏掉新变体。

`outline` 与 `color_for` 都是穷尽 `match`,加变体编译期就会报错,不用额外守护。

---

## ⑤ F134 默认文件图标

### 现状

`classify` 把两类完全不同的东西都判成 `IconKind::Other`(菱形):

- 普通文件,只是扩展名不认识(`data.bin`、`Makefile`、`.bashrc`)——**绝大多数**
- `EntryKind::Other`(设备文件 / socket / 命名管道)——极少数

结果是一屏陌生扩展名的普通文件全是菱形,既不像文件,也把真正需要「这不是普通
文件」提示的那几条淹掉了。

### 做什么

拆成两个变体:

- `IconKind::File`:普通文件的默认图标 —— **折角空白页**,颜色 `icon_file`
  (中性灰,比 `icon_other` 亮)。
- `IconKind::Other`:只留 `EntryKind::Other`,继续用菱形。

`classify` 的优先级不变(EntryKind → 扩展名表 → 可执行位),只把最后那个兜底
从 `Other` 改成 `File`。

形状上 `Doc`(页 + 三条线)、`File`(折角空白页)、`Link`(折角页 + 箭头)三者
必须扫得开:`Doc` 有横线、`Link` 有箭头、`File` 两者都没有。**「够不够扫得开」
只有人眼能判**,进人工验收。

既有三条断言 `data.bin` / `Makefile` / `.bashrc` 判 `Other` 的测试改判 `File`
—— 这是行为变更后同步判据,不是放松断言(它们仍然是精确相等断言)。

---

## 不做什么

- 不给换节点弹窗加分组头(顺序对齐即可,高度预算不允许)。
- 不按 host 缓存多条 sftp channel(见 ③ 的理由)。
- 不做面包屑式路径(每一段可点):它跟「可编辑」是两种交互,叠在一栏里会互相
  抢点击;先上可编辑,面包屑等有人真的要了再说。
- 不在客户端解析相对路径(`../x`)。
- 不动 `Link` / `Dir` / `Archive` / `Image` / `Code` / `Exec` 的形状。

## 你无法验证的东西(进人工验收清单)

- 五种新图标(PDF/Word/Excel/PPT/File)在真实 DPI 与 16px 格子里**看不看得清、
  扫不扫得开**,以及颜色在深色底上的可辨识度。
- 路径条编辑态下第三方中文输入法的候选框行为。
- 换节点弹窗顺序在**真实会话库**(有分组、有拖拽排序过的顺序)下确实与左栏
  逐行一致。
- F132 在真实链路上换节点后按 `Ctrl+Shift+B`,连的确实是那台机器 ——
  验法:两台机器上分别 `touch` 一个只有自己有的文件,看面板里出现的是哪个。
- 重开 sftp 时的观感(高延迟下会短暂显示「正在读取目录…」)。
