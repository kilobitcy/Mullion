# VT 快照 fixture 录制约定

终端仿真的正确性没法靠眼睛看。做法(见项目 `CLAUDE.md`「VT 快照测试」):

1. **录制**:在真实环境里抓一段原始字节流,存成 `*.bin`。实际用过、能重复的一套:

   ```bash
   # tmux 的 pipe-pane 抓的就是应用写进 pty 的原始字节(含全部转义序列)
   tmux -L snap new-session -d -s p -x 100 -y 30 'sh -c "sleep 3; exec <要录的程序>"'
   tmux -L snap pipe-pane -t p -o 'cat >> /tmp/probe.raw'
   # …在这期间用 send-keys 打字、等它画完想要的那一帧…
   tmux -L snap kill-server
   ```

   开头那个 `sleep 3` 是为了**先把 pipe 挂上再启动程序**,否则第一屏漏掉。
   `-x/-y` 就是测试里 `Emulator::new` 要传的 cols/rows,两边必须一致。

2. **脱敏**:落库前把个人信息换掉,**且必须等长**(按字节数一比一)。
   流里全是 `CSI row;colH` 这类绝对定位,字节数一变就和内容错位,
   渲出来的不再是真机上那一帧。

3. **喂入**:测试里把 `*.bin` 的字节喂进 `Emulator::feed`,
   把 grid 渲染成「纯文本 + 属性摘要」(见 `tests/vt_fixtures.rs`)。

4. **比对**:跟同名 `*.snap` 比对。确认改动是有意的之后用
   `UPDATE_VT_SNAPSHOTS=1 cargo test -p mullion-term` 重写。

## 命名

- `<场景>.bin` —— 录制的原始字节流(输入)。
- `<场景>.snap` —— 期望的 grid 文本快照(输出)。

## 现有 fixture

| 名字 | 录自 | 钉住的事实 |
|---|---|---|
| `claude-code-input-cursor` | `tmux new-session -x 100 -y 30 claude`,输入 `hi` 后停在输入框 | Claude Code 全程 `?25l`(`?25h` 只在启动出现一次),输入位置**全靠一格 SGR 7 反显块**自绘(F197 / F198) |

## 规则

- **新增 VT 相关功能必须配一个 fixture**,否则该功能在真实 TUI 下的表现无人知晓。
- fixture 是二进制原始流,**不要手改**;要改就重录。
- `.snap` 的头两行是光标状态与反显格坐标,**语义位在文本网格之前** ——
  快照失配时先看到的该是它们,而不是去一百列宽的文本里数格子。
