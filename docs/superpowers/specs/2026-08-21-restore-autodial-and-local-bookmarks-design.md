# 恢复现场自动重连 + 单击即恢复 + 本地目录收藏 + 换程序图标(F153/F154)

> 2026-08-21 · 起因是 v0.1.62 实机报的四条:
> ①「选中一条现场点恢复,没有自动重连、也没接回 tmux」
> ②「想用左键单击就恢复,不想双击/找按钮」
> ③「文件面板本地栏路径条的 ☆ 点不动」
> ④「favicon.ico 换了,打包要用新的」

四条彼此独立,一次做完。前两条同属 F148 恢复现场那条路,合成新编号 **F153**;
第三条是 F139 的补齐,新编号 **F154**;第四条是 F152 的资源替换,无新编号。

---

## ① F153-a:恢复现场后自动串行重连

### 现状与用户诉求

`restore_history`(`app.rs:2639`)只把上次的标签摆成 `TabContent::Restored` 占位骨架,
一条连接都不建;拨号要用户在中央区点「重连」(`ui/restored.rs`),或走菜单里的
「全部重连」(`reconnect_next_restored`,`app.rs:2511`)——而后者**也只连第一个**,
剩下的等用户再按一次。

原设计(F37 §1)否掉自动重连的理由是「别让高延迟代理链路上同时挤一堆握手」。
这条理由**只否掉并发,不否掉自动**:一条接一条地拨号既满足用户诉求,也不违反它。

### 前置:必须先修一个既存真 bug

`UserEvent::ConnectErr`(`app.rs:6677`)的处置只有「记日志 + `ui.set_error`」,
**既不清 `pending_restore`,也不复位那个标签的 `RestoredTab::dialing`**。而
`reconnect_tab`(`app.rs:2407`)开头有一道闸:

```rust
if self.pending_restore.is_some() { return; }
```

于是:**任何一次占位标签重连失败之后,这个进程里所有占位标签就再也连不上了** ——
点「重连」静默无反应(闸挡住),按钮本身还永远停在禁用的「连接中…」(`dialing`
没复位)。没有自愈路径,只能重启 exe。这与 T10「用户点过一次输入框后中文永久
打不出来」是同一类:**一次失败换来永久坏,且没有任何报错**。

自动串行拨号必然踩到它(第一条失败 = 整条队列断在那里),所以这是本节的第一步,
不是「顺手优化」。

修法:`ConnectErr` 分支里 `self.pending_restore.take()`,并把取出来的 `tab_id`
对应标签的 `dialing` 复位成 `false`。

### 设计

新增 App 字段:

```rust
/// F153:恢复现场后正在自动串行拨号。`None` = 没在自动拨。
struct AutoDial {
    /// 已经试过的标签(不管成没成)。**不能省** —— 失败的那条 `dialing`
    /// 被复位后,`reconnect_next_restored` 的「第一个未 dialing 的占位标签」
    /// 判据会把它反复选中,成死循环。
    tried: Vec<TabId>,
    ok: usize,
    err: usize,
}
```

- **起点**:`restore_history` 摆完标签后开启 `auto_dial`,立即拨第一条。
- **推进**:`accept_connect_ok`(`app.rs:5177`)与 `ConnectErr` 收尾各调一次
  `advance_auto_dial()`,分别累加 `ok` / `err`。
- **选下一条**:纯函数 `next_auto_dial(tabs, tried) -> Option<TabId>` ——
  取第一个「是 `Restored`、不在 `tried` 里」的标签。选中即先记进 `tried`,
  再交给既有的 `reconnect_tab`(不分叉出第二条拨号路径)。
- **失败照走**:一条失败继续拨下一条(用户定的)。失败那条留在占位态,
  中央区的「重连」按钮可用,用户可手动再点。
- **收尾**:没有下一条时 `auto_dial = None`,一条 toast 汇总
  「N 条已连接,M 条失败」。全成功时也报,让用户知道自动拨号结束了。
- **菜单「全部重连」维持原样**:它仍只拨一条。改它属于本次请求之外
  (Scope Discipline),而且那条路径上没有「用户刚刚明确选了要恢复」这个前提。

### tmux attach:零新代码

attach 是**登录后自动化**(F40~F44)的一部分:拨号成功后 `accept_connect_ok`
末尾会 `take_pending` 出计划并 `start_automation`,tmux 分支发的就是
`tmux has-session -t X && exec tmux attach -d -t X || exec tmux new-session -s X`
(`mullion-store/src/automation.rs:242`)。所以只要真的发起了连接,attach 就跟着跑。

**会话名取自会话配置里的自动化**(用户选的方案)。F141 那份「当初真的 attach 上去
的会话名」(`TerminalTab::tmux_attach`)只活在当前进程里,不进现场记录;跨重启
恢复时按配置名 attach。代价写明:**没在「登录后自动化」里配 tmux 的会话,
恢复后只登录、不 attach** —— 这不是 bug,是配置问题,验收时要分清。

### 测试

| 判据 | 手段 |
|---|---|
| `next_auto_dial` 跳过已试过的标签,不会反复选中失败那条 | 纯函数单测(构造 tabs + tried) |
| 没有占位标签时返回 `None`(不空转) | 同上 |
| `ConnectErr` 清 `pending_restore` **且**复位 `dialing` | 接线守护(读 `app.rs` 源码,两条断言分开写——只钉一条会漏掉另一条) |
| `restore_history` 开启 `auto_dial` | 接线守护 |

**无头环境测不到的**:真实拨号、tmux 是否接回原会话、串行的实际观感。
这些进人工验收清单。

---

## ② F153-b:恢复弹窗单击一行即恢复

`ui/history.rs:182` 现在单击只 `d.selected = i`,双击才 `Restore`。改成单击直接
`out = Some(HistoryOut::Restore(row.id.clone()))`。

- `selected` 与「恢复」按钮**保留**:按钮是键盘/无鼠标路径的出口,而且
  `selected` 决定按钮拿哪条 id。
- `double_clicked()` 那条分支随之删除(单击已经触发,双击的第二次点击同样
  会置 `clicked()`,留着是死代码)。
- **提示语必须一起改**:现在写的是「摆回来的标签不会自动连接,点「重连」才拨号」
  —— ① 之后这句话变成假的。改成「选一条摆回标签栏,会依次自动重连。」

风险认下:手滑点错行就直接恢复了,无撤销。用户明确选了这个语义;缓解是恢复本身
是**追加**标签(D13),不破坏现有连接,大不了把多出来的标签关掉。

### 测试

- 单击第 i 行返回 `Restore(第 i 行的 id)` —— 用既有的 `click()` 脚手架改造
  (它现在按文本找按钮,要加一个按行序号算坐标的版本)。
- 「光把弹窗画出来不恢复任何东西」那条既有测试必须继续绿(防止改出「一出现
  就自己恢复」)。
- 提示语里不再出现「点「重连」才拨号」——文案守护,防止 ① 和 ② 只做一半。

---

## ③ F154:本地目录收藏

### 现状

`files_panel::sidebar`/`content` 给本地栏传的是 `BookmarkView::none()`
(`files_panel.rs:1523`/`1672`),即 `list: &[], can_edit: false` —— 本地栏的 ☆
**恒置灰**,▾ 恒空。这是 F139 当初划的范围(书签 = 会话 SFTP 配置下的远端路径)。
用户要它能用。

### 设计

- **store**:`SftpPrefs` 加 `local_bookmarks: Vec<Bookmark>`,带 `#[serde(default)]`。
  **不动 `CURRENT_SCHEMA`(维持 9)**:老文件缺这个键时 serde 给空 vec,没有任何
  值需要转换 —— 与 `model.rs:159` 已有的先例同一条判据。
- **Vault**:加 `add_local_bookmark` / `remove_local_bookmark`。去重规则(按 `path`
  相等)与远端共用一个私有 helper,两边不许分叉。
- **PanelFrame**:加 `local_bookmarks: Vec<Bookmark>` 字段,`new()` 多收一个参数
  (两个调用点:`app.rs:5234` 的 SFTP 标签、`app.rs:5335` 的终端标签侧栏)。
- **files_panel**:本地栏改传 `BookmarkView { list: &frame.local_bookmarks,
  can_edit: frame.session_bound }`。`BookmarkView::none()` 随之没有调用方,删掉
  (这是本次改动导致其无用,符合 Scope Discipline)。
- **app**:`apply_local_file_action` 里那条「本地栏收到了书签动作,已忽略」的
  warn 分支(`app.rs:3102`)改成真处理,调本地版本的 `add_bookmark`/`remove_bookmark`。
  同 F139 纪律:**store 一份、`PanelFrame` 这一帧的镜像一份,两处都要写**
  (`app.rs:3260` 那段注释说的就是这件事)。

### 认下的后果

- 本地书签挂在**会话记录**下 → 快速连接开的标签(无 `SessionId`)本地 ☆ 仍置灰,
  与远端同规则、同 hover 文案。
- 同一台 Windows 机器上开两条会话,各存各的本地书签,不共享。用户已知情选择。
- 路径是 Windows 形态(`C:\Users\...`),与远端书签共用 `Bookmark` 类型但存在
  **两个不同的列表**里,不会互相污染。

### 测试

| 判据 | 手段 |
|---|---|
| 本地书签存盘后重开还在、同路径收藏两次去重 | `mullion-store` 单测(照抄远端那两条的结构) |
| 老 TOML(没有 `local_bookmarks` 键)读得进来且为空 | store 单测 |
| 本地栏点空心 ☆ 发 `BookmarkAdd`、点实心 ★ 发 `BookmarkRemove` | `files_panel` 跑帧测试(照抄远端那两条) |
| 远端书签**不出现在**本地栏下拉里,反之亦然 | 跑帧测试,两个列表给不同内容 |
| 本地版 add/remove 都 `store.save()` 了 | 接线守护(扩既有的 `app.rs:10352` 那条,把两个新方法名加进列表) |

---

## ④ 换程序图标

仓库根目录的 `favicon.ico` 与现有 `crates/mullion-app/assets/mullion.ico` 帧结构
完全一致(6 帧:16/32/48/64/128/256,32bpp BMP,均非 PNG 压缩),内容不同。

- 直接覆盖 `crates/mullion-app/assets/mullion.ico`。`build.rs` 的
  `rerun-if-changed=assets/mullion.ico` 保证重编资源段,`.rc` 与
  `icon_res::RESOURCE_ID` 都不动。
- `tests/icon_resource.rs` 的尺寸帧断言能过(新 ico 尺寸齐全)。
- **替换后删掉仓库根目录那份 `favicon.ico`**:它不在 `.gitignore` 里,留着会变成
  未跟踪文件;图标源应当只有一份,两份迟早对不上。

无法自动验证:Windows 上任务栏/标题栏/资源管理器里那张图**长什么样**。进人工
验收清单。

---

## 交付

改动落在 `mullion-app`(和 `mullion-store`)→ 按 CLAUDE.md 的交付约定一条龙:
版本 0.1.62 → **0.1.63**,跑绿(`cargo test --workspace` + `clippy -D warnings`),
交叉编译 + objdump 验收,签名,发 GitHub Release,给人工验收清单。

### 人工验收清单(Windows 11 实机)

1. 换过图标的 exe:资源管理器里的文件图标、标题栏左上角、任务栏、Alt-Tab
   四处都是新图。
2. 关掉 exe(留下现场记录)→ 重开 → 恢复列表里**单击**一条(不是双击)→
   标签依次自动连上,一条接一条,不是同时。
3. 配了 tmux 自动化的会话:恢复后回到原来那个 tmux 会话(窗口/pane 还在),
   不是一个新 shell。
4. 故意让其中一条连不上(改错端口/断网):其余的照样继续连,末尾 toast 报
   「N 条已连接,M 条失败」;失败那条点「重连」**必须有反应**(这条钉的是既存
   bug 的修复)。
5. 文件面板本地栏:点 ☆ 能收藏当前本地目录,▾ 下拉里点一条能跳过去,重启后
   书签还在;远端栏的书签不出现在本地栏下拉里。
