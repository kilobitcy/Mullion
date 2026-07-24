# VT 快照 fixture 录制约定

终端仿真的正确性没法靠眼睛看。做法(见项目 `CLAUDE.md`「VT 快照测试」):

1. **录制**:在真实环境里抓一段原始字节流,存成 `*.bin`:
   ```bash
   ssh host 'tmux new -d -s snap "…"'
   # 用 script -q 抓原始输出(含所有转义序列),落盘为 fixtures/<名字>.bin
   ```
   录制脚本 `scripts/record-fixture.sh` 尚未编写,需要时提出。

2. **喂入**:测试里把 `*.bin` 的字节喂进 `Emulator::feed`,
   把 grid 渲染成「纯文本 + 属性摘要」。

3. **比对**:跟同名 `*.snap` 比对(快照)。

## 命名

- `<场景>.bin` —— 录制的原始字节流(输入)。
- `<场景>.snap` —— 期望的 grid 文本快照(输出)。

例:`claude-fullscreen.bin` / `claude-fullscreen.snap`。

## 规则

- **新增 VT 相关功能必须配一个 fixture**,否则该功能在真实 TUI 下的表现无人知晓。
- fixture 是二进制原始流,不要手改;要改就重录。
- 骨架阶段本目录只立约定,暂无 `.bin`/`.snap`。
