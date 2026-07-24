# ADR-004: async 边界 —— app 拥有 tokio,ssh 是纯 async 库

- 状态: 已接受
- 日期: 2026-07-23
- 关联: spec.md F1/F3/F6/F34、S3;架构不变量(依赖方向 app → {core,term,ssh});陷阱 T1/T3

## 背景

`mullion-ssh` 基于 russh,是 async 的(需要 tokio 运行时);`mullion-app` 的 winit 事件循环
是**同步、单线程**的。两个世界必须接上,但不能违反架构不变量:

- `mullion-ssh` 只认字节流,不认识「pane / 窗口」,更不能依赖 winit。
- 依赖方向严格 `app → {core, term, ssh}`,ssh 不得反向知道 app/UI。
- winit 循环不能被网络 IO 阻塞,否则高延迟下 UI 卡死(违背 G1「零可见闪烁」与 N1 空闲 CPU)。

「谁拥有 tokio 运行时、字节怎么跨线程、ssh 怎么唤醒重绘」是本切片唯一的重大架构决策。

## 决策

**app 拥有唯一的 tokio 运行时;`mullion-ssh` 是纯 async 库,零 UI、零运行时所有权。**

- `mullion-ssh` 只暴露 `async fn connect(cfg, policy, wake) -> (SshSession, mpsc::Receiver<Vec<u8>>)`。
- ssh **不依赖 winit**:唤醒重绘经注入的 `wake: Arc<dyn Fn() + Send + Sync>` 回调(app 传
  `EventLoopProxy::send_event`)。
- ssh **不弹 UI**:TOFU 主机密钥决策经注入的 `policy: Arc<dyn HostKeyPolicy>`(app 传弹窗版,
  测试传 TofuAccept)。
- 一条 channel 由单个 io task 用 `channel.split()` 的读写半 + `tokio::select!` 独占收发;
  远端字节经有界 mpsc 交给 app,app 每帧 `feed` 进仿真器并把 `take_pty_writes` 回写(T1)。
- 字节泵的编排落在 app —— 唯一同时认识 term 与 ssh 的地方。

## 备选与否决理由

**A:ssh crate 自带后台 tokio 线程,对 app 只给同步句柄。**
app 完全不碰 tokio。但库内隐藏运行时是反模式:关闭、panic 传播、与宿主运行时共存都别扭;
测试要绕过这层线程,无法直接 `#[tokio::test]`。否。

**C:在 winit 循环里 `block_on` 每个操作。**
心智最简单,但网络 IO 直接阻塞单线程 UI —— 高延迟链路下冻界面,与项目存在的全部理由(跟手、
零闪烁)正面冲突,且拖高 N1 空闲 CPU。否。

选 B 的决定性理由:它让 `mullion-ssh` 成为可 `#[tokio::test]` 直接打真实 sshd / 真机的纯 async
库,把线程与唤醒的复杂度收在 app 这个本就是集成层的地方,同时干净地守住「ssh 不认识 winit/UI」
这条架构不变量。

## 后果 / 权衡

- app 新增 `tokio` 依赖(方案 B 的应有之义)。
- `SshSession::write/resize` 用有界 mpsc 的 `try_send`,winit 线程可非阻塞直接调,无需 block_on;
  代价是队列满时返回 `TrySendErr::Full`(本切片键鼠写入几乎不触发,粘贴大段的背压策略随 F18 再定)。
- io task 单任务顺序 `select!`:inbound `send().await` 期间不处理出站命令(大 burst 下键入有
  延迟,非死锁)。接 app 侧泵(后续)时复审是否需拆读/写双任务。
- 远端断线(S3)也调 `wake()`,否则断连要等无关重绘才被 app 发现。

## 重新考虑的触发条件

若实测出现「大流量下键入明显延迟」,把 io task 从单任务 select 拆成读、写两个任务(写半经
`Handle` 共享);或 app 侧泵的 rx 排空被某个等待写完成的路径卡住时,一并重构。在拿到这个症状
之前不拆。
