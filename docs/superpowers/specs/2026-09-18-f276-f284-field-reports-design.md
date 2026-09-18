# F276~F284 十条实报（做九撤一）——设计定案

> 2026-09-18 grilling 定案。原始报告十条，第 4 条（关键字提醒）用户撤销不做。
> 分三片交付：片一 = F276/F277/F279/F280/F281/F282/F284（七条小修，一个版本）；
> 片二 = F283（备份口令解耦主密码）；片三 = F278（文件面板递归搜索）。

## 决策表

| # | 决策 | 理由 / 已查明的事实 |
|---|---|---|
| D1 | **F276 切项目弹窗项目灯**：把「切项目弹窗开着」加进 `project_lamps` 的计算条件 | 根因不是没画灯——`project_pick.rs` 与 launcher 共用 `project_row::show`（F233~F237 三处共享行），灯、底色、「正在跑」标签全在。是 `app.rs` F224 的省算门控（「只在有人会看见时才算」）只列举了项目管理器和 launcher 两个观众，切项目弹窗开着时灯表是**空 BTreeMap**。「列举式门控加观众时必然漏」第 N 次 |
| D2 | **F277 `files_dialog` 全家换 `egui::Modal`（带遮罩变暗）** | 用户报「文件已存在」不显眼；实况是六个框共用的 `modal()` 辅助走普通 `egui::Window`，无遮罩。仓库现成模式：`paste.rs:60`、`host_key.rs:78`，注释写明「普通 Window 不挡下层点击」。用户拍板换**全家**（六个框），不是只换一个。注意 `egui::Modal` 无标题栏无 ✕，标题与关闭钮要自己画（照 paste.rs），F203 的「✕=取消」语义要保住 |
| D3 | **F279 上传读失败要看得见 + mtime 前后比对**：用户场景是本地文件被 Windows 程序独占锁着（选项 C+甲） | 事实：`run_transfer` 的读失败**有**报错（`读不了本地文件:{e}`），但 `TransferDone` 的 Err 只落进队列的 `JobState::Failed`，**从不 `set_error`**——用户不开传输列表就永远看不见。修法：TransferDone 收到 Err（排除 CONFLICT_MARKER 与「已取消」）时弹错误卡；上传前后各 stat 一次本地文件（mtime+len），变了则整个 job 判失败并明说「已上传但源文件在传输期间被改动，内容可能是半截，请重传」 |
| D4 | **F280 设置弹窗按可视区收缩** | 设置窗无任何尺寸约束、无 ScrollArea，内容多高窗口多高；egui 的 constrain 只**平移**不压缩，小屏上上下均匀溢出。照 `project_manager.rs:114-128` 的写法：`room = screen_rect().bottom() - ui.cursor().top() - SP_M` 实测剩余（**不用 `Window::max_height`**，F217 栽过），`set_max_height(room.max(160))`，正文套 `ScrollArea::vertical()`，确定/取消按钮留在滚动区外常驻 |
| D5 | **F281 状态栏报错加「复制」按钮** | 「复制不了」的根因不是 Label 不可选——egui Label 默认可划选；是 **Ctrl+C 被键盘分流送进终端**（T8：判给终端的键永不喂 egui），egui 收不到复制键。修法不动分流（动了就是拆 T8），在状态栏错误文字旁加复制图标按钮，走「ui 写 intent → app.rs 调 `clipboard.set()`」既有模式（参照 F100 标注导出）。图标自绘（T9） |
| D6 | **F282 OSS 拒收 `If-None-Match` 自动降级**：收到 400 NotImplemented 且正文点名 If-None-Match → 只带 `x-oss-forbid-overwrite` 重试一次，并把「此服务端不吃 If-None-Match」落盘进 `cloud.toml`（bool，serde 缺省 false） | F273 当初的假设「不认识的头会被忽略」对 OSS 是**事实错误**：OSS 对 PUT 上的 `If-None-Match` 返回 400 NotImplemented（实机报文为证）。防覆盖不降级——OSS 靠 `x-oss-forbid-overwrite` 撞车回 409，本来就映射 `AlreadyExists`。否掉「服务商风味下拉」：多一个没人懂的配置项。分层：`mullion-cloud` 只负责识别并返回专用错误变体 + 接受「跳过 If-None-Match」开关，**重试与记忆在 app 侧**（cloud crate 不认识 cloud.toml） |
| D7 | **F283 独立备份口令（片二）**：云备份口令与主密码完全解耦。Argon2id 派生（复用 F46-a 的 `kdf.rs`，盐随包走），口令本身 `seal_local` 封进 `cloud.toml`，定时备份不再要求主密码；新机恢复时输口令。不提供「用主密码当备份口令」快捷项（会把 D12 那套耦合请回来一半）。D6/D12 的「清主密码自动关云备份」整套拆除；AK/SK 随 vault 密钥重封的逻辑**保留**（那是另一把钥匙） | 用户明确要解耦（选项 A）。**忘备份口令 = 云上历史备份作废，无找回**，设置页要写明。原 D6 捆绑的理由（钥匙串密钥不跨机）被「独立口令派生」取代 |
| D8 | **F278 递归模糊搜索（片三）**：路径条尾放大镜按钮 → 下方弹搜索条；**Enter 起搜**（不边输边搜）；SFTP 逐目录遍历（不走远端 find——SFTP-only 节点无 exec、busybox 参数不齐、转义一摊事）；大小写不敏感**子序列**匹配（自己写，不引依赖）；封顶 500 结果 / 2000 目录、可取消、显示「已扫 N 个目录」；**不跟符号链接**（防环）；隐藏目录随现有 `show_hidden`；结果列表出相对路径，↑↓ 选、Enter/双击 → 跳到所在目录并置为选中（复用 F218 `reveal_pick`/`scroll_to`）。命中在当前目录时退化成「定位模式」 | 用户拍板：定位模式 + 递归。本地栏走本地遍历，三种栏（本地/远端/纯 SFTP）同构 |
| D9 | **F284 tmux 软换行复制拼接**：先录真实 tmux 字节流 fixture 确诊，再上启发式——选区内某物理行**最后一列非空**且选区继续向下 → 该处不加 `\n` | alacritty 对 WRAPLINE 行复制时本来就拼一行（上游 `line_to_string`）；用户仍拿到换行 ⇒ 推定 tmux 逐行显式定位重绘、WRAPLINE 没打上。**fixture 若证明 WRAPLINE 其实还在，停下重新诊断，不许盲上启发式**。已知误伤（用户已接受）：恰好排满整行宽的真换行会被拼掉，包括 TUI 全宽边框行。另：用户说的「右键选中即复制」实为左键松开复制（右键是直接粘贴，F18） |
| D10 | **撤销**：关键字提醒（原第 4 条）用户明确不做。若将来重提，本次 grilling 已定过的默认（全局关键字表 / 只扫可见屏 / 闪 3 秒转常亮 / 复用 WaitUntil 定时）记录在案 | 附带查明的落点备用：`SnapCell` 加字段 + `hash_row` 穷尽解构会逼着进 F12 指纹 + `quads_for` 加分支 + `next_timer_wake` 加定时源 |

## 编号

F276=项目灯、F277=Modal 遮罩、F278=递归搜索、F279=上传读失败、F280=设置窗收缩、
F281=报错复制、F282=OSS 降级、F283=备份口令、F284=软换行拼接。
（原报告第 4 条不占号。）
