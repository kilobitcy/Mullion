# ADR-009: 一条 SSH 连接承载多个分屏(多 channel)

- 状态: 已接受
- 日期: 2026-07-29
- 关联: 切片 B2-a spec/plan、spec F30/F31/F35、ADR-004(async 边界)、ADR-003(tmux control mode 已否决)

## 背景

F30/F31 要在一个窗口里开 1/2/3/4 个终端分屏,每个分屏需要一个独立的远端 PTY。
问题是这些 PTY 从哪来:每个分屏各自开一条 SSH 连接,还是在一条连接上开多条 channel。

## 决策

**一条 SSH 连接(一次 TCP + 一次认证 + 一次主机密钥确认),每个 pane 在其上
`channel_open_session` 开一条独立 channel + PTY**(spec F35)。

`russh::client::Handle<ClientHandler>` 只实现了 `Drop`、没有 `Clone`,所以用
`Arc<Handle<ClientHandler>>` 在多个 pane 之间共享。开 channel 的入口是
`mullion_ssh::session::open_pty(handle: Arc<Handle<_>>, cfg, wake)
-> (SshSession, Receiver<Vec<u8>>)`。

守护测试:`mullion-ssh/tests/pty.rs::one_handshake_serves_many_ptys_f35`
(一次握手多条 channel、各 channel 回显不串)、`dropping_one_pty_keeps_the_others_alive_f35`。

## 备选与否决理由

- **每 pane 一条 TCP + 认证**。本项目的核心场景是**高延迟代理链路**,每条连接的完整
  握手 + 认证要几百 ms 到几秒;切到「4 屏」就是 4 次。更糟的是 F3 的 TOFU 主机密钥
  确认——第 2/3/4 个 pane 会各弹一次确认框,用户体感是「点个分屏弹一堆窗」。
  服务端连接数也 ×4。否掉。
- **OpenSSH `ControlMaster` 式外部复用**。依赖外部 `ssh` 二进制与 unix socket;
  Windows 11 是本项目唯一的一等公民,这条路在那儿不成立。否掉。
- **走远端 tmux 分屏、客户端只显示一个 PTY**。等于把分屏交还给复用器——ADR-003 已经
  否决过让 tmux control mode 决定客户端布局的方向,理由同样适用:布局归客户端,
  客户端才能做 GPU 加速的独立渲染与独立滚动回溯。而且用户的远端 tmux 里跑的正是
  Claude Code,不能替他动他的 tmux 布局。否掉。

## 后果与由此引入的约束

选一条连接换来了「切分屏零等待、只确认一次主机密钥」,代价是下面这些必须一直守住的
不变量。**它们都是单 pane 时代不存在的失效模式**,B2-a 的复核里每一条都真实踩到过。

1. **依赖方向不变**。`mullion-ssh` 仍然不认识「pane」——`open_pty` 只吃
   `Arc<Handle>`、吐字节流。pane 概念完全留在 `mullion-app`。

2. **一条连接断 = 所有分屏一起断**。这是本决策接受的代价,不做 per-pane 重连。

3. **channel 泄漏(单连接时代不存在)**。单 pane 时开 PTY 失败可以把整条连接一起丢;
   多 channel 下不行——`russh` 的 `Channel<Msg>` **没有**自动发 CHANNEL_CLOSE 的
   `Drop`(只有 `into_stream()` 得到的 `ChannelCloseOnDrop` 才有)。所以
   `session.rs` 里 `request_pty` / `request_shell` 失败的分支必须显式
   `channel.close().await`。不关就是每次失败泄漏一个 channel slot,攒到 sshd 的
   `MaxSessions` 上限后**再也开不出新 pane**,且症状与网络无关、极难现场归因。

4. **T1 升级为 per-pane**。`Event::PtyWrite`(同步输出探测、鼠标上报、光标查询的应答)
   必须回写**发起它的那个 pane 自己**的 channel,不能图省事写 `panes[0]`——否则
   非首个 pane 的全屏 TUI 会永久等不到应答。守护测试
   `mullion-app/src/shell/workspace/mod.rs::pty_write_goes_to_its_own_pane_channel_t1`。

5. **开 channel 是真实网络往返,存在秒级空窗期**。这是本决策最隐蔽的后果:
   `channel_open_session` 在高延迟链路下要几百 ms 到几秒,这期间用户完全可能又切了
   预设、甚至断开重连,而**已经发出的异步任务不会被取消**
   (`runtime.spawn` 的 JoinHandle 被丢弃,`Arc<Handle>` 已 clone 进任务里,
   旧连接照样把 channel 开成功)。于是迟到的 `UserEvent::PaneOpened` 必须过两道校验,
   两道都在 `app.rs::pane_still_wanted`:
   - **树成员**:这个 `PaneId` 还在 `leaves(ws.tree())` 里吗?不查的后果是孤儿 pane
     ——不渲染、用户点不到关闭,但 `pump()` 每帧仍排空它的 rx,channel 一直占着。
   - **Workspace 世代**:`PaneId` 在**跨连接**之间会重复。`Workspace::new` 的
     `next_id = id.0 + 1` 配合每次 `ConnectOk` 硬编码的首 pane `PaneId(1)`,意味着
     每个新世代的 id 都从 2 重新计数——**碰撞是必然,不是小概率巧合**。而
     `attach_pane` 是「同 id 静默覆盖」语义,所以旧世代迟到的事件会**顶掉新世代刚
     建好的 PaneState**(连同它正常的 channel 一起 drop),那个 pane 此后写到旧连接。
     `Workspace` 因此带一个 `generation: u64`,计数器 `App::next_ws_generation` 挂在
     `App` 上(挂 `Workspace` 自己身上等于没修——新世代又从同一常量开始)。

   守护测试:`app::tests::pane_still_wanted_rejects_a_leaf_dropped_by_a_later_preset_switch`、
   `app::tests::pane_still_wanted_rejects_a_stale_generations_pane_even_if_the_id_is_reused`。

第 5 条可以概括成一句给将来的人:**任何跨越 Workspace 生命周期的异步回调,回来时都
必须先证明自己还属于当前世代**。新增这类回调时照抄这条,别只查 id。
