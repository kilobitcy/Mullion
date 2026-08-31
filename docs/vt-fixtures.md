# VT 快照测试：怎么录 fixture

> 规则本身（「新增 VT 相关功能必须配一个 fixture」）在 `CLAUDE.md`。
> 这里只写操作步骤，需要录的时候再读。

终端仿真的正确性没法靠眼睛看。做法是：

1. 在真实环境里录一段字节流，存到 `crates/mullion-term/tests/fixtures/*.bin`
2. **等长脱敏**（见下）
3. 测试里把字节喂进 `Emulator`，把 grid 渲染成纯文本 + 属性摘要
4. 跟 `*.snap` 比对

## 录制（实际用过的一套）

`tmux` 的 `pipe-pane` 抓的就是应用写进 pty 的原始字节，含全部转义序列，
比 `script -q` 好控制（能 `send-keys` 打字、能挑帧）：

```bash
tmux -L snap new-session -d -s p -x 100 -y 30 'sh -c "sleep 3; exec <要录的程序>"'
tmux -L snap pipe-pane -t p -o 'cat >> /tmp/probe.raw'
sleep 12
tmux -L snap send-keys -t p 'hi'      # 想录什么就打什么
sleep 3
tmux -L snap kill-server
```

- 开头那个 `sleep 3` 是为了**先把 pipe 挂上再启动程序**，否则第一屏漏掉。
- `-x/-y` 就是测试里 `Emulator::new` 要传的 cols/rows，两边必须一致。
- 想先确认流里有没有你要的那件事，直接在原始字节上 grep，例如
  `\x1b\[7m`（反显）、`\x1b\[\?25[lh]`（DECTCEM）——**这一步就是定性**，
  别跳过它去读源码猜 TUI 的行为。

## 脱敏（硬要求：等长）

TUI 的欢迎屏里往往有用户名、邮箱、组织名。落库前按字节数**一比一**替换
（`chenjp` → `tester`、`chenyujp06@gmail.com` → `devs@example.invalid`）。

不能改长度：流里全是 `CSI row;colH` 这类绝对定位，字节数一变就和内容错位，
渲出来的不再是真机上那一帧。写个小脚本做替换并 `assert len 不变`，别手改。

## 比对与更新

```bash
cargo test -p mullion-term --test vt_fixtures          # 比对
UPDATE_VT_SNAPSHOTS=1 cargo test -p mullion-term       # 确认是有意改动后重写 .snap
```

已有 fixture 与各自钉住的事实见 `crates/mullion-term/tests/fixtures/README.md`。

录制脚本：`scripts/record-fixture.sh`（还没写，需要时告诉我）。
