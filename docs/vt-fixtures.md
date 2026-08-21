# VT 快照测试：怎么录 fixture

> 规则本身（「新增 VT 相关功能必须配一个 fixture」）在 `CLAUDE.md`。
> 这里只写操作步骤，需要录的时候再读。

终端仿真的正确性没法靠眼睛看。做法是：

1. 在真实环境里录一段字节流：`ssh host 'tmux new -d …'` 后用 `script -q` 抓原始输出，
   存到 `crates/mullion-term/tests/fixtures/*.bin`
2. 测试里把字节喂进 `Emulator`，把 grid 渲染成纯文本 + 属性摘要
3. 跟 `*.snap` 比对

已有 fixture 见 `crates/mullion-term/tests/fixtures/`。

录制脚本：`scripts/record-fixture.sh`（还没写，需要时告诉我）。
