# Android 客户端设计(M 系列,F344~F361)

日期:2026-11-12。决策来源:grilling 会话 Q1~Q13,架构理由见 `docs/adr-013-android-client.md`。

## 1. 定位

**一期用途 = 看护 + 轻交互**:人不在电脑前时,打开项目 → attach 到项目 tmux →
看 Claude Code 进度、批准、选选项、回一两句话。不是移动开发环境。

## 2. 架构

```
android/ (Kotlin + Compose)
   │  uniffi
mullion-ffi ──→ { term, ssh, store, cloud }      ← 第二个组装端,与 app 平级
mullion-app ──→ { core, term, ssh, store, cloud }
```

- **判据一份**(Q3):项目相关纯函数下沉到 `mullion-store`,两端调用同一份。
  候选清单(以 plan 阶段逐个核实依赖为准):`search` 的分词/匹配、`project::matches`、
  `candidates`/`node_for`、`ui::project_list` 的筛选排序与空态判定、`localtime::relative`。
  **不搬**:带 egui 类型的(`icon_bg`)、依赖 `LeafIdentity` 等 shell 类型的(恢复现场一族)、
  依赖 `mullion_ssh` 的(`list_clients_command`)、async 的(`pick_node`——store 零 async)。
- **编排各写**:ffi 内用 tokio 实现「F304 探首跳 → 拨号 → 跑自动化 → 交出字节流」,
  T11(计时从 `Completed` 起算)、F304(只量首跳、最后一条不探)的守护测试照搬。

## 3. 一期范围

| 编号 | 内容 | 关键约束 |
|---|---|---|
| F346 | 导入:云端拉包 / 手动导入 `.mullionpack` | 只读,不回写云端;只导入项目节点及其跳板链引用到的凭据;密文用 Keystore 重封 |
| F347 | 凭据门控 | Keystore `setUserAuthenticationRequired` + `setUserAuthenticationParameters`,冷启动或空闲 ≥5 分钟后要生物识别/锁屏凭据 |
| F348 | 项目列表 | 搜索(F233 同一份 `matches`)、最后打开时间(F234 同一份 `relative`)、只列在用但搜索穿透归档(F257)、备忘只读(F340)。**无运行指示灯** |
| F349 | 打开项目 | F304 直连优先 + 失败换下一条;强制 attach 项目 tmux 并 `-c` 目录(F223);**attach 不带 `-d`**,与桌面共挂;一次只开一个项目 |
| F350 | TOFU | 包里带来的指纹沿用,不一致硬拒;没见过的在手机上弹窗确认,只存手机本地 |
| F351 | 终端视图 | Rust 打包可见网格(按行合并同样式段)一帧一次 FFI;Kotlin Canvas 画;字体内置 Google Sans Code,中文/emoji 回落系统;`Choreographer` 节流——有脏行才请求下一帧(T3/T7 同判据);同步块攒帧由 term 层继承(T2/T2' 的 attach 前登记 sync 照做) |
| F352 | 输入 | 默认底部本地编辑框(本地回显、本地改错、系统输入法);上方常驻辅助键栏 `Esc Tab ↑↓←→ Ctrl(粘滞) 1 2 3 Enter`,一按直通;多行:远端开了 2004 → bracketed paste + `CR`,否则逐行 `ESC CR` 分隔 + `CR`(T6) |
| F353 | 手势 | 单指拖 → 远端开鼠标上报则发滚轮,否则滚本地回看;轻点只切键盘、**不发点击**;长按拖 → 本地划选(永不上报,T5 等价)+ 系统复制菜单,hold 每条出口都归还(T13);双指捏合调字号,松手才重算列数并发一次 `window_change`(T4) |
| F354 | 生命周期与重连 | 切后台:前台服务保活 10 分钟,超时断开;回前台重连并 attach。断线重连(F128 退避表)**不带 `-d`**;channel 被远端 detach 关掉 → 不自动重连,显示「已在别处接管」+ 手动按钮 |
| F355 | 工程与分发 | 单仓库 `android/` + `crates/mullion-ffi`;发布 ABI 只 `arm64-v8a`(另编 `x86_64` 仅供模拟器测试);`minSdk 30`;版本号独立 `android-v0.x.y`;GitHub Release 发签名 APK、侧载;签名密钥只在本机 |

**一期不做**:运行指示灯、多项目并开、分屏、SFTP、隧道、编辑器、非项目会话直连、
手机上编辑任何数据、F100、截屏、设置页(只留字号)、推送通知。

### 3.1 与桌面共挂同一 tmux(Q9)

- 手机 attach 不带 `-d`;tmux 默认 `window-size latest`,尺寸跟随最后有操作的客户端,
  两端交替时 Claude Code 重排,接受。
- 桌面被别人踢掉不会自动重连(F128 只看 keepalive),手机端也规定被踢不自动重连 → 两端不会互踢循环。
- 用户 tmux 配了 `window-size smallest/manual` 时桌面会被压窄或留白:**只在首次使用提示和文档说明,不改用户 tmux 选项**。

### 3.2 验证分层

| 层 | 手段 |
|---|---|
| Rust(store 下沉件、ffi 编排、网格打包) | `cargo test`,纳入「绿」的定义 |
| Kotlin 纯逻辑(手势仲裁、辅助键编码、保活计时) | JVM 单测 |
| 输入/手势/生命周期接线 | KVM 无头模拟器插桩测试 |
| 画面观感、输入法候选、真机网络、前台服务被 ROM 杀 | **人工验收**,交付时标「未验证」 |

## 4. 路线

| 期 | 编号 | 内容 |
|---|---|---|
| 零期 | F344 | 项目判据下沉 `mullion-store`,只搬不改,桌面单独发补丁版实机验证 |
| 零期 | F345 | `mullion-ffi` 骨架 + Android 工具链(NDK、`cargo-ndk`、Rust target)+ 模拟器跑通最小终端;同提交改 `CLAUDE.md` 不变量表 |
| 一期 | F346~F355 | 见 §3 |
| 二期 | F356 | Claude Code 等批准时推送通知(F336/F337 手机版;要解决后台长连与 tmux 吞 OSC 9) |
| 二期 | F357 | 项目运行指示灯(远端批量探测 + 缓存) |
| 二期 | F358 | 多项目并挂、快速切换 |
| 二期 | F359 | 非项目会话直连(凭据导入范围改为用户勾选) |
| 三期 | F360 | SFTP 只读浏览 + 文本预览 |
| 三期 | F361 | 云端同步改按条合并(两端共用),之后才放开手机端编辑项目/备忘 |

**永不做**:手机端分屏、内置编辑器、端口隧道、F100 标注。
