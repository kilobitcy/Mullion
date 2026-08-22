# ADR-008:自诊断日志(接 `log` facade + 阶段打点 + 看门狗)

- 状态:已采纳
- 日期:2026-07-27
- 相关:[adr-007](adr-007-egui-chrome.md)、`gui-render-gotchas.md`、CLAUDE.md「你无法验证的东西」

## 背景

2026-07-27 Windows 11 真机上 mullion 0.1.4 卡死。手头两份证据:

1. `mullion.log` —— 16 行,最后一行是 `Resized(0x0)`(最小化),之后再无任何输出。
   因为 `logx` 逐行 flush,这条「日志断了」是硬证据:事件循环从此不再处理窗口事件。
2. Windows 应用程序事件日志 —— `grep -i mullion` **零匹配**。没有 AppCrash、没有 AppHang。
   同一时间窗内倒是有 Explorer 崩在 `amdxx64.dll`(AMD D3D11 UMD)、`ssh.exe` 栈溢出、
   Avira 崩溃、Winlogon 重启外壳。

关键结论:**Windows 事件日志对「GUI 进程挂起」根本不记录**。它记的是别的进程崩溃,
而那些既可能是我们的后果,也可能是我们的死因——现有证据**分不清方向**。

同时,日志本身也不够用:
- 无分级,只有一个 `line()`,想加细节就等于污染默认输出。
- 拿不到第三方内部诊断。wgpu 到底选了哪个 adapter、有没有重建 swapchain、有没有设备丢失,
  winit 收到过什么,russh 协商失败在哪一步——一概不知。
- 断了只知道「不动了」,说不出卡在 acquire、egui 还是驱动。
- 没有内存/GPU/驱动版本的现场。

## 决策

### 1. 接 `log` facade,自写 `log::Log` 实现(不引 env_logger / tracing)

`mullion-app` 直接依赖 `log 0.4`;`logx` 提供 `FileLogger`,继续落到
`<config_dir>/mullion.log`(逐行 flush + stderr)。

理由:`wgpu` / `wgpu-core` / `wgpu-hal` / `naga` / `winit` / `glyphon` / `russh` **内部全部用
`log` 打诊断**。接上 facade 后,它们的输出与我们自己的日志落进同一个文件、同一条时间线——
这是「不依赖 Windows 端日志」能拿到的最大一块信息,而且 `mullion-ssh` 一行代码都不用改
(russh 的日志自动就有了)。`log` 是零依赖 facade,且早已在 `Cargo.lock` 依赖树内,
新增传递依赖为 0。

分级用两个环境变量,取值 `off|error|warn|info|debug|trace`:

| 变量 | 作用域 | 默认 |
|---|---|---|
| `MULLION_LOG` | 自家 crate(target 前缀 `mullion`) | `info` |
| `MULLION_LOG_DEPS` | 第三方 crate | `warn` |

分开是必要的:`MULLION_LOG=debug` 若同时放开 wgpu,每帧几十行会把真正的线索冲走,
也会迅速撑爆磁盘。启动时若日志超过 8 MB 轮转一代到 `mullion.log.1`。

### 2. 阶段打点 + 看门狗(`diag.rs`)

主线程每换一个阶段写一次 `AtomicU8` + 时间戳(两条 relaxed 原子写,常开无妨):

```
idle / startup / window_event / user_event / pump / resize
egui_run / text_prepare / acquire / encode / present / store_io
```

独立看门狗线程每秒检查:**非 `idle` 阶段**停滞 ≥ 3 s 就落一条 WARN,带阶段名、
帧/present/跳帧计数、入站字节数、内存快照;持续卡则按 3 s / 6 s / 12 s 翻倍复报。

`idle` 必须排除:事件循环阻塞等事件是常态(`ControlFlow::Wait`),空闲一分钟不是卡死。
反过来,`startup` 单列是因为 adapter 枚举 / `request_device` 是阻塞调用,显卡驱动出问题时
最可能就卡在那里——这次的 AMD 驱动嫌疑正好落在这个区间。

`acquire` 与 `present` 分开打点,才能区分「等交换链」和「等驱动」。

### 3. 现场自采(替代去翻系统信息)

- **启动环境快照**:版本 / arch / os / cpus / 内存。
- **GPU 身份**:拿到 adapter 立刻记 `get_info()`(名称、device_type、backend、vendor、
  device、`driver` + `driver_info`)。这次是靠 Explorer 崩溃记录才知道驱动版本的,那条路不可靠。
- **surface 配置**:格式 / present_mode / alpha_mode / frame latency。
- **GPU 故障自报**:`Device::on_uncaptured_error` + `Device::set_device_lost_callback` 落盘。
  TDR / 设备丢失由 wgpu 直接告诉我们。
- **内存采样**:Windows 走 `K32GetProcessMemoryInfo`(PrivateUsage)+ `GlobalMemoryStatusEx`;
  Linux 读 `/proc`。看门狗报警时立即采一次;`debug` 级下每 5 s 一行心跳(兼作性能基线)。

## 备选与否决理由

- **只扩自家 `logx`(零依赖变化)**。否决:拿不到 wgpu/winit/russh 的内部诊断,
  而这次的卡点恰恰在第三方那一层。省下的一行 Cargo 依赖不值。
- **引 `env_logger`**。否决:为一个 `RUST_LOG` 语法拉进 `regex` 一系列传递依赖;
  我们需要的「自家/第三方分档」它反而要写更长的 filter 串。自己解析两个环境变量更短更准。
- **引 `tracing` + `tracing-subscriber`**。否决:span 计时确实更强,但依赖体量与本项目
  「不引重库」的约定冲突,且当前需求(卡在哪个阶段)用一个 `AtomicU8` 就够。
  将来若要做逐帧耗时剖析可以重新评估。
- **靠 Windows 事件日志 / WER dump**。否决:实测对挂起不记录;且要求用户去翻事件查看器,
  违背「交出去的 exe 自己会说话」。

## 后果

- 好的:卡死后只看 `mullion.log` 就能说出卡在哪个阶段、卡了多久、内存多少、GPU/驱动是什么;
  wgpu/winit/russh 的内部错误自动进同一条时间线;`MULLION_LOG=debug` 一开就有完整现场。
- 代价:多一行 `log` 依赖;事件循环里多了若干 `diag::mark`(两条 relaxed 原子写,可忽略);
  多一个常驻线程(每秒醒一次)。
- 未验证:看门狗在真机上是否真能在卡死时把 WARN 写出去——取决于卡死是否也阻塞了文件写
  (若整个进程被驱动挂起,写不出去也正常)。**需人工实测确认**;写不出去时至少还有
  `logx` 逐行 flush 的既有证据链。

## 增补（F155，2026-08-22）：档位化 + 周期性剖面

**决策**：日志档位从「只认 `MULLION_LOG` 环境变量」改为「`settings.toml` 里的
三档 + 环境变量覆盖」；默认档（info）下每 5 秒落一行聚合性能剖面，**空闲窗口
一个字都不写**（否则笔记本硬盘永远睡不着）。

**为什么不做全量事件流**：本项目是 60fps 帧循环 + SSH 字节流，逐事件记录
一分钟就是几十 MB，而 `write_line` 原本是逐行同步 flush 的——那会把磁盘写
进帧预算，测出来的不再是原来的程序（T3）。聚合剖面用一批原子计数器换
「p95 是 3ms 还是 300ms」这个量级的答案，采集端只做原子加法。

**为什么长在 `diag.rs` 上而不是新起一套**：`diag::mark()` 已经铺满事件循环，
改成「换阶段时顺手累计上一阶段的时长」之后，逐阶段耗时分布是白得的，
不新增任何插桩点。这也是本切片工作量能压住的原因。

**第三方档位永远比自家低一档**：wgpu/naga 一开 debug 就刷屏，跟着自家提
上去的话每 5 秒一行的剖面会被淹掉，等于功能没做。

**同步块（DEC 2026 / T2）单列两个数**：进入次数与「靠 150ms 逃生门硬挤出帧」
的次数。历史上「打字慢一拍」的真根因就是同步块超时收口，而它在帧耗时
分位数里看不出来——超时的那一帧本身并不慢，慢的是它前面被按住的 150ms。

**脱敏是尽力而为，不是安全边界**：按模式匹配替换（IPv4 / `user@host` /
盘符路径 / Unix 绝对路径），再加一遍「之前见过的裸主机名」兜底；覆盖不到
的写法会漏。UI 上明写「发送前请自己再看一眼」——给用户「导出即安全」的
印象，比不提供这个功能更糟。
