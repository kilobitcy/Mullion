# F375~F378 实现 plan:双击选 URL / URL 不跳侧栏 / Alt 复制人称互换 / 隧道入口变浅

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 落地 `docs/superpowers/specs/2026-11-18-f375-f378-url-dblclick-persona-swap-tunnel-entry-design.md`,发 v0.1.147。

**Architecture:** F375 在 `mullion-term::selection` 加 `url_span` 纯函数,`Emulator::selection_start` 的 `Semantic` 分支先试它再试 `path_span`;F376 在 `files::reveal::parse` 加一道门;F377 改 `clipboard::to_first_person` 为互换;F378 改 `tunnels::indicator`、`ui/tunnel_popup.rs`、`ui/chrome.rs` 菜单与 `app.rs` 出口施加。

每条一个提交,摘要带编号。

### Task 1: F375 `url_span`
- [ ] 测试:`https://github.com/k/M/releases/tag/v0.1.146` 点任意格(含 `https` 上)→ 整条;`见：https://a.com:8080/x?a=1&b=2#f。` → 不含中文与句号;`[t](https://a.com/x)` → 不含括号;尾部 `.`/`,` 剥掉;跨行写满右缘续接;点在 scheme 之前的前缀上 → None;无 `://` → None。
- [ ] 实现:抽 `run_span(rows,row,col,pred)`(F371 的扩展+跨行),`path_span` 改用它;`url_span` 扁平化后找 `://`,向前走 scheme,向后止于段尾/下一条 URL 的 scheme 起点,剥尾标点,点击须在范围内。
- [ ] Emulator `Semantic` 分支:`url_span` → `path_span` → alacritty 语义。单测 + VT fixture(tmux 客户端流里一条被硬折的 URL)。
- [ ] 提交 `feat(term): 双击 URL 选中整条含协议头 (F375)`。

### Task 2: F376 reveal 拒 URL
- [ ] 测试:`parse("https://github.com/x")`、`parse("file:///etc/hosts")`、`parse("`https://a/b`")` → None;`parse("a/b:3")` 仍 Some。
- [ ] 实现 `has_scheme(s)`,门 4 前拒。提交 `fix(app): Ctrl+Shift+B 不把 URL 当路径跳 (F376)`。

### Task 3: F377 人称互换
- [ ] 测试改为互换语义;改名 `swap_persona`,调用点与源码守护锚点随之改;toast 文案核对。
- [ ] 提交 `feat(app): Alt 复制时你我双向互换 (F377)`。

### Task 4: F378 隧道入口
- [ ] `indicator` 配置 0 条 → `Some("隧道 0", Calm)`;改写 `indicator_is_always_shown_once_something_is_configured`。
- [ ] 浮层 `TunnelOut::New` + 「+ 新建隧道」按钮;帧测点出 `New`。app 施加:未保存守卫 → 开隧道页 + 空缓冲。
- [ ] 菜单「隧道管理器…」→ `ui_state` 标志 → 开隧道页;守护测试。
- [ ] 跑 `glyph_whitelist`。提交 `feat(app): 隧道指示器常驻、浮层可新建、菜单直达隧道页 (F378)`。

### Task 5: 绿 + 复核 + 发版 v0.1.147
