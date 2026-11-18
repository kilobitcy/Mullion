# F371~F374 设计:双击选路径 / 侧栏 SFTP 对账 / 备忘浮层居中 / Ctrl+Shift+B 用最近复制

> 2026-11-17 grill 定案;2026-11-18 按 `/review` 复核修订(致命 a/b/c 与 F371 判据)。来源:用户对 v0.1.145 提的四条。一个批次,发 v0.1.146。

---

## F371 双击选中完整路径

事实:双击走 alacritty `SelectionType::Semantic`,分隔符是默认
`` ,│`|:"' ()[]{}<>\t ``(`alacritty_terminal-0.26.0/src/term/mod.rs:45`),**没有任何全角
标点**。`设计文档：docs/superpowers/specs/xxx-design.md` 里的「：」不算分隔符,双击整段连中文一起选。

决策:
1. **路径感知双击优先**。双击点落在「路径字符」上(ASCII 字母数字与 `._-/~+@%=`),向左右扩到
   路径字符的最大连续段;该段**含 `/`** 才算路径 → 只选这一段。**段尾不剥 `.`**(复核):与
   `files/reveal.rs` 的 `TRAILING_PUNCT` 对齐 —— 剥了会把 `..` 吃成空串,且 `.` 结尾文件名合法;
   `,:;` 本就不是路径字符,不会出现在段尾。中文与路径紧贴(`见docs/x.md`)能正确切出;
   英文句末 `docs/x.md.` 会多带一个 `.`,接受。
2. 不满足 → 退回 alacritty 语义选择,但 `semantic_escape_chars` 在默认表上**追加全角标点**
   `：，。、；！？（）「」『』《》【】“”‘’`。
3. **跨行续选用 F335 的「写满右缘」判据**(复核修订):主场景在 tmux 里,tmux 绝对定位重画,
   断行**一律不带 `WRAPLINE`**(`mullion-term/src/selection.rs:42`),按 `WRAPLINE` 续零收益。
   改为:路径段写到**最后一列**且下一行**行首**即是路径字符 → 续接,**不补空格**(Ink 在宽度处
   硬切超宽 token,真机字节已验证,fixture `claude-code-wrapped-command`)。上一行同理向上续。
   已知误拼:恰好写满 + 恰好下一行以路径字符开头而两者无关 → 多选一截,接受。
4. 已知代价:中文文件名的路径在中文处截断(`docs/设计.md` 只选 `docs/`),退回拖选。
5. 判据是 `mullion-term` 里的纯函数(输入:若干行字符 + 点击点,输出:**起止点**(可跨行)),配 VT fixture。
   改动面:`Emulator::selection_start` 的 `Semantic` 分支内先走路径感知(经 `point_at`/`display_offset`
   换算、跳过 `WIDE_CHAR_SPACER`),命中则落成 `Selection::new(Simple, 起点, Left)` + `update(终点, Right)`。
   **app 侧 `SelectionKind` 保持 `Semantic`**(复核):`selection_release` 对 `Simple` 且同格松手会清
   选区,双击必然同格。T13/F309 选区留底照常适用。

## F372 侧栏 SFTP 节点跟随栏头(不变量对账)

事实:栏头节点名按侧栏归属 pane(`files_owner_pane`)**每帧现取**;SFTP channel 只在 F341
`rehost_resyncs_sidebar` 三条件(归属=该 pane、侧栏开在该标签、该 pane 是焦点)全满足时才重开。
用户实报(复现路径多半是「切项目弹窗」换到另一台机器上的项目):栏头变了、SFTP 仍连旧节点。
切项目同走 `spawn_rehost_on(.., UserPicked, ..)`,字面上已被 F341 覆盖 —— 有隐藏条件
(候选:同步那一刻 sftp 还在打开中 `has_client=false` → `SyncPlan::Nothing`,开完连的仍是旧节点;
或多节点项目探测时序),实现时按 systematic-debugging 定根因并配守护。

决策:
1. **不变量对账**(复核后仍选对账,不改事件补偿:F341 补洞后又漏,对账能收敛将来的新路径):
   `about_to_wait` 里比「归属 pane 所在 host_ix」与「`sftp_host_ix`」,两个整数,零 IO(T3 合规)。
   **门(全满足才对账,复核)**:`files_owner_pane` 的世代 == `files_owner_generation()`(侧栏开在该
   终端标签上;关着/切走/`Files` 标签 → 不对账);归属 pane 仍在 ws 里;`sftp_client().is_some()`;
   远端栏不在 `Loading`。不一致 → F132 同款重开,但目标是**归属 pane** 的 host 与 cwd
   (抽 `reopen_sftp_on(gen, host_ix, cwd)`,不再隐含取焦点 pane)。F341 事件同步保留作快路径。
2. **「尝试过」闸**(复核):按 `(世代, 归属 pane, 目标 host_ix)` 记已尝试,同一键不再自动重开
   (不只闸失败 —— 一切非收敛都不空转);归属 pane 换到别的节点 = 新键。
3. **F218 跨机跳转同步改归属**(复核致命 a):`reveal_remote` 跨机重开时把 `files_owner_pane` 改成
   发起 pane,否则对账把刚搬过去的 channel 拽回原节点 →「路径对了、机器错了」。
4. 旧节点上在途传输照常跑完(F132 纪律,不 abort `sftp_tasks`)。
5. 已确认存在的第二个洞(复核):多 pane 项目切换时归属 pane 不是焦点 → F341 三条件不满足 → 永不同步;
   对账覆盖它。

## F373 备忘浮层居中

决策:Ctrl+Shift+M 浮层在 pane 内**水平垂直居中**,每帧按 pane 中心对齐(内容变高向上下两头长),
夹在 pane 内,底部出口沿用 `CHROME_H` 预留(不调)。只改备忘浮层;切项目弹窗、换节点弹窗不动。
egui 0.30 `Area::pivot(CENTER_CENTER)` 用上一帧尺寸,首帧会从左上跳到中心闪一下 —— 首帧
`sizing_pass`/不可见一帧规避,实现时定。

## F374 Ctrl+Shift+B 用最近一次终端复制的路径

事实:`reveal_target` 只读焦点 pane **此刻的选区**;侧栏开着时认不出路径 → 关栏。终端划选
即复制,但 TUI 重画会把选区冲掉(T13,F309 只补「逐字未变」),于是「复制了路径再按键」被判
「无路径 → 关栏」。异机跳转 F218 已支持。

决策:
1. 终端划选复制时记下 `(文字, 来源 pane(世代+PaneId))`。**写点挂在 `clipboard.set` 旁,记真正写进
   剪贴板的那份**(Alt 复制改人称后的、Ctrl+Shift+C 的)。按键时焦点 pane 选区认不出路径,且
   **剪贴板当前内容 == 记下的文字** → 用它,按**来源 pane** 的节点与 cwd 解析并跳转(不一定是焦点
   pane);剪贴板被别处覆盖即自然失效。不读任意来源的剪贴板。
   - **跨标签 = 无记忆**(复核致命 b):`host_ix` 是各 `ws.hosts` 自己的下标,来源 pane 世代 ≠
     活动标签世代时拿它去查会静默连错机器 → 视为没有记忆,按现规则关栏。
   - **剪贴板读失败 = 未知**(复核):Windows 剪贴板被别的进程短暂占用是常态(`clipboard.rs:7`),
     读不到时不关栏、飘 toast「剪贴板暂时读不到,再按一次」,不当「不相等」。
2. 跳到来源 pane 的节点时,**侧栏归属 pane 一并改成来源 pane** —— 否则 F372 对账会把 SFTP 拽回去。
3. 仍关栏的两条照旧:键盘焦点在文件面板上(F243);面板已停在该路径(`arrived`)。
   只有「认不出路径」那一支改成先查记忆、没有才关。

## 人工验收(无头验不了)
- Claude Code 输出里各种路径双击(中文冒号后、反引号里、句末、软折行、tmux 分屏硬折)。
- 切项目到另一台机器后侧栏 SFTP 跟过去;SFTP 打开中途切项目。
- 备忘浮层居中观感,小 pane 下出口按钮可见。
- 复制路径后选区被 TUI 冲掉,再按 Ctrl+Shift+B 跳到来源 pane 所在节点;栏头随之改写成来源 pane。
- 选区路径跨机跳转(F218)后 SFTP 不被对账拽回。
