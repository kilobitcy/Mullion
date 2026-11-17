# F368~F370 隧道浅入口 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 隧道可开机自动拉起(不交互、多开只拉一份)、状态栏指示器常驻可点出启停浮层、本地侦听端口查重。

**Architecture:** 判据全做成 `tunnels.rs` / `tunnel_editor.rs` 里的纯函数(自动拉起队列 `Autostart`、端口查重 `port_clash`、
指示器 `indicator` 新签名),`app.rs` 只做接线;浮层是新文件 `ui/tunnel_popup.rs`(egui `Area`,登记 `Modal::TunnelPopup`)。

**Tech Stack:** Rust / egui 0.30 / std `File::try_lock`(rustc 1.96,无新依赖)。

设计:`docs/superpowers/specs/2026-11-17-f364-f370-keyboard-memo-tunnel-entry-design.md` §F368~F370。

---

## 实现时定下的几处(设计留给实现定的)

- **CLI 直连不自动拉起**:挂钩点放在 `finish_store_open` 里 CLI `return` 之后。CLI 是一次性临时连接,开机工作站语义不适用。
  守护:源码切片判挂钩在 `self.initial.take()` 那段之后。
- **「N 条未自动启动」= 自动拉起后被静默停掉的条数**(含 `MissingSecret`/bind 失败/首连 `Failed`),
  不含仍在 Connecting/Reconnecting 的(那些已由指示器 Warn 档表达;算进去会在启动那几秒闪一句假告警)。
  用户手动启动/停止该条即从集合移除。
- **自动拉起的那条一旦到过 `Running` 就离开自动集合**:此后的失败走普通 F115 toast(运行中出事是真事,该播)。
- **浮层开关**:点指示器只「打开」,不做切换 —— 状态栏是面板,按下那一刻 F239 已按「点外面」把浮层关了,
  松手再开等于没关,做切换反而闪一下(与换节点弹窗同一姿态)。关浮层 = Esc / 点外面。
- `start_tunnel` 改为返回 `Result<(), String>`,不再自己 `set_error`;三个调用方各自处理(管理器 → 顶栏;
  浮层 → 顶栏 + 行内;自动 → 计入未自动启动,不出声)。

## 文件

- Modify `crates/mullion-store/src/vault.rs`:`clone_tunnel` 置 `autostart=false`。
- Modify `crates/mullion-app/src/tunnels.rs`:`indicator` 新签名;`elsewhere_running`;`Autostart` 队列;`StartMode`。
- Modify `crates/mullion-app/src/ui/session_manager/tunnel_editor.rs`:自动启动复选框;`port_clash`;内联提示;`show` 加 `tunnels` 参数。
- Modify `crates/mullion-app/src/ui/session_manager/mod.rs`:保存前查重拦截。
- Modify `crates/mullion-app/src/ui/session_manager/tunnel_list.rs`:抽出 `row_spec`(三项判据同源)。
- Create `crates/mullion-app/src/ui/tunnel_popup.rs`:浮层。
- Modify `crates/mullion-app/src/ui/chrome.rs`:指示器可点,回报 rect。
- Modify `crates/mullion-app/src/ui/mod.rs`:`UiState.tunnel_popup`、`UiActions.tunnel_popup`、`UiFrame.tunnel_auto_failed`、接线。
- Modify `crates/mullion-app/src/app.rs`:`Modal::TunnelPopup` 全套;`start_tunnel(id, mode)`;自动拉起泵 + 文件锁;`accept_tunnel_state` 静默停;浮层出口施加。

---

### Task 1: 克隆一律关自动启动 (F370)

- [ ] 测试 `vault.rs`:`clone_tunnel_never_carries_autostart` —— 源条 `autostart=true`,克隆出来 `false`,源条不变。
- [ ] 跑红 → `clone_tunnel` 里 `rec.autostart = false;` → 跑绿 → 提交 `feat(store): 克隆隧道一律关闭自动启动 (F370)`。

### Task 2: 端口查重纯函数 + 编辑器内联提示 + 保存拦截 (F370)

```rust
pub(crate) struct PortClash { pub other: String, pub blocking: bool }
/// 只有本地侦听的(`Local`/`Dynamic`)参与;`-R` 侦听在远端不算;排除 `editing_id` 自身。
/// `blocking` = 两条都勾了自动启动。
pub(crate) fn port_clash(buf: &TunnelEditorBuffer, editing_id: Option<TunnelId>, tunnels: &[TunnelRecord]) -> Option<PortClash>
```
- [ ] 测试:同端口 Local vs Local → 警告不拦;两条都 autostart → 拦;Local vs Dynamic 同端口算;Remote 不算(双向);排除自身;端口没填/非法 → None。
- [ ] 实现;`show` 加 `tunnels` 参数,侦听端口下一行画:不拦 = `warn` 色「本地端口 N 已被「X」占用:两条不能同时运行」;拦 = `danger_text`「…两条都勾了自动启动,开机会抢同一端口 —— 取消其中一条的自动启动」。
- [ ] 「标识」节加复选框「自动启动」+ 灰字「库打开后自动拉起;需要确认主机密钥或输入密码的不会自动拉起」。
- [ ] `session_manager::show` 保存:`build_tunnel_draft` 成功后 `port_clash(..).filter(|c| c.blocking)` → `set_error(..)` 不发 `tunnel_save_request`。测试走 `show` 帧测或抽纯函数 `save_intent(buf, id, tunnels) -> Result<TunnelSaveIntent, String>` 单测。
- [ ] 提交 `feat(app): 隧道本地侦听端口查重,自动启动同端口拦截 (F370)`。

### Task 3: 指示器常驻 + 计别的实例 + 未自动启动 (F369/F368)

```rust
pub fn indicator(states: &[TunnelState], configured: usize, elsewhere: usize, auto_failed: usize) -> Option<Indicator>
pub fn elsewhere_running(records: &[TunnelRecord], local: &[(TunnelId, TunnelState)], others: &[OtherTunnel]) -> usize
```
- [ ] 把 `indicator_is_none_when_nothing_is_configured` 改名重写为 `indicator_is_always_shown_once_something_is_configured`:`(&[],0)`→None;`(&[],3,0,0)`→`隧道 0/3` Calm。
- [ ] 新测试:分子含别处 Running;本地有该条时不重复计;`auto_failed>0` → Danger 且文本含「2 条未自动启动」。
- [ ] 改调用方 `ui/mod.rs`(新 `UiFrame.tunnel_auto_failed`)。提交 `feat(app): 状态栏隧道指示器常驻并计入别的窗口 (F369)`。

### Task 4: 启停浮层 (F369)

- [ ] `tunnel_list.rs` 抽 `row_spec(rec, state, others, sessions) -> (ButtonSpec, Option<&OtherTunnel>)`,`row` 改用它(列表与浮层同源)。
- [ ] 新 `ui/tunnel_popup.rs`:`TunnelPopup{anchor: Pos2, errors: BTreeMap<TunnelId,String>}`、`TunnelOut{Start,Stop,Edit,Manage,Close}`、
      `area_id()`、`show(ctx,t,&Option<TunnelPopup>, tunnels, states, others, sessions, credentials) -> Option<TunnelOut>`。
      每行:标题 · 转发串 · 状态(本地 `state_label` / 别处 `other_label`)· 自动标记 · [启动/停止](按 `row_spec`)· [编辑];行下一行红字 = `errors[id]`;底部「管理隧道…」;Esc → Close。
- [ ] 测试:点「启动」出 `Start(id)`;别处在跑时按钮禁用;有 errors 时画出红字;Esc → Close。
- [ ] `chrome::status_bar` 加 `tunnel_click: &mut Option<egui::Rect>`,指示器 `Sense::click()` + 手形;测试点指示器回报 rect。
- [ ] `build_ui`:点了 → `ui_state.tunnel_popup = Some(TunnelPopup::new(r))`(只开);在 memo_popup 之后画浮层,出口进 `actions.tunnel_popup`。
- [ ] 提交 `feat(app): 状态栏隧道启停浮层 (F369)`。

### Task 5: 浮层接线 + Modal 登记 (F369)

- [ ] `Modal::TunnelPopup`:ALL / modal_open / baseline=false / dismiss_areas=`tunnel_popup::area_id()` / dismiss_dirty=false / dismiss / DISMISS_ORDER(排 MemoPopup 之前)/ 完备性测试三处。
- [ ] `start_tunnel(id, StartMode) -> Result<(), String>`;管理器路径 `Err → set_error`。
- [ ] `apply_tunnel_popup(out)`:Start → `start_tunnel(Manual)`,Err → `set_error` + 写 `errors[id]`,Ok → 清 `errors[id]`;Stop → 停 + publish;Edit → 开会话管理器隧道页并选中(右栏隧道表单脏时不换缓冲,toast 提示);Manage → 开隧道页;都关浮层(Start/Stop 不关)。
- [ ] `has_real_action` 加 `a.tunnel_popup`;`tunnel_page_visible` 把浮层开着也算(2 秒读别处)。
- [ ] 接线守护测试 `the_tunnel_popup_is_wired_to_start_stop_and_edit`。提交 `feat(app): 隧道浮层接入启停与编辑跳转 (F369)`。

### Task 6: 自动拉起 (F368)

```rust
pub enum StartMode { Manual, Auto }
pub struct Autostart { queue: VecDeque<TunnelId>, waiting: Option<TunnelId>, auto: BTreeSet<TunnelId>, failed: BTreeSet<TunnelId> }
impl Autostart {
    pub fn new(records: &[TunnelRecord]) -> Self;            // autostart=true 的,按 id 升序
    pub fn next(&mut self, is_live: impl Fn(TunnelId)->bool) -> Option<TunnelId>; // 闸:waiting 已不在运行表 → 放行
    pub fn on_state(&mut self, id, &TunnelState) -> AutoVerdict; // Normal / StopSilently
    pub fn on_start_error(&mut self, id);
    pub fn forget(&mut self, id);                             // 手动启停
    pub fn failed_count(&self) -> usize;
    pub fn is_auto(&self, id) -> bool;
}
```
- [ ] 测试:只排 autostart;串行(第一条未上报前 `next` 为 None);首次上报(Connecting/Reconnecting/Running/Failed 任一)放行;
      Failed → StopSilently 且计数;Running 后离开自动集合,之后 Failed → Normal;`on_start_error` 放行并计数;等待的那条被停 → 放行;`forget` 去计数。
- [ ] app:`tunnel_autostart: Option<Autostart>`、`tunnel_autostart_lock: Option<std::fs::File>`;
      `finish_store_open` 在 CLI return 之后、`store.is_some()` 时建队列;`pump_tunnel_autostart()` 在 `about_to_wait` 调:
      取 `next` → 首次真要拉时 `try_lock` `<config>/tunnel-autostart.lock`(抢不到 → 丢队列 + logx)→ `start_tunnel(Auto)`,Err → `on_start_error`。
- [ ] `start_tunnel` Auto 档:首连 `TrustedOnlyPolicy`,不弹「隧道启动中…」toast。
- [ ] `accept_tunnel_state`:先问 `on_state`;StopSilently → `tunnels.stop` + `publish_tunnel_state(false)` + 不播 toast。
- [ ] 管理器/浮层手动启停 → `forget(id)`。`UiFrame.tunnel_auto_failed = failed_count`。
- [ ] 守护:`the_autostart_hook_runs_after_the_cli_return_and_only_with_a_store`、`an_autostart_failure_stops_silently_and_republishes`、
      `the_auto_path_dials_with_trusted_only_first_policy`(源码切片)。
- [ ] 提交 `feat(app): 隧道自动启动,不交互、多开只拉一份 (F368)`。

### Task 7: 版本 + 发版

- [ ] `Cargo.toml` 0.1.145,提交 `chore: 版本 0.1.145(...)`;按 release-windows 一条龙。

## 无法自动验证
- 双 exe 同时启动只拉一份;主密码解锁后才拉;未知主机密钥那条确实不弹框;浮层位置与观感。
