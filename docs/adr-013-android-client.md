# ADR-013:做 Android 客户端——原生外壳 + Rust 内核,新增 `mullion-ffi` 组装端

## 状态
已采纳(2026-11-12)。推翻 spec 非目标 N-G5「移动端」。

## 背景
主场景是「远端 tmux 里跑着 Claude Code,人在 Windows 前操作」。人离开电脑之后,
Claude Code 停下来等批准、跑完一段要人看一眼,这段时间里没有任何入口。
用户拍板要做 Android 端,第一期必须带「项目」(F221 起的管理单位)。

一期用途定为**看护 + 轻交互**:打开项目 → attach 到项目 tmux → 看进度、批准、
选选项、回短句。不是在手机上做完整开发。

## 决策
1. **外壳用 Kotlin + Jetpack Compose,内核复用 Rust**:`mullion-term` / `mullion-ssh` /
   `mullion-store` / `mullion-cloud` 编成 `.so`,经新 crate `mullion-ffi`(uniffi)暴露给 Kotlin。
2. **架构不变量改写**:原文「`app` 是唯一允许知道其余四者的地方」改为
   「**`app` 与 `ffi` 是两个平级的组装端**,各自可依赖 `{core, term, ssh, store, cloud}`;
   两个组装端互不依赖,其余 crate 之间仍互不依赖」。
3. **项目判据只有一份**:`mullion-app` 里与项目相关、只依赖 store 类型的纯函数
   (搜索匹配、列表筛选与排序、候选节点顺序、相对时间等)下沉进 `mullion-store`,
   两端共用。只搬不改,桌面单独发一个补丁版实机验证无回归。
4. **IO 编排各写各的**:打开项目的拨号、F304 探测换节点、跑自动化,在 `mullion-ffi`
   里用 tokio 重写一份,不从 `app.rs` 的事件循环里抽。T11 / F304 的守护测试照搬。
5. **终端画面用 Android Canvas 画**,不用 wgpu/glyphon。
6. **数据单向流入手机**:手机只读消费云端包 / `.mullionpack`,不回写。

## 备选与为什么否掉

**全 Rust 沿用 winit + wgpu + glyphon + egui**:被一个硬事实否掉——锁定版本
winit 0.30.13 的 Android 后端只有 `set_ime_allowed → show_soft_input`,**不发任何
`WindowEvent::Ime`**,`set_ime_purpose` 是空实现(`platform_impl/android/mod.rs:918-926`)。
打不了中文,而「给 Claude Code 回一句中文」是一期刚需。另外 egui 在触屏上的焦点问题
(T8/T10 那一族)会原样带过去。

**纯 Kotlin 重写(Termux terminal-emulator + sshj)**:丢掉 VT 层(T1/T2'/T13 打磨过的)、
SSH 层(代理链/跳板/TOFU)、store 层(密文格式、`.mullionpack`)。项目数据和凭据没法
与桌面互通,陷阱再踩一遍。

**把项目编排整体抽成新 crate(`mullion-session` 之类)给两端共用**:长期方向是对的,
但它要同时依赖 ssh 与 store,打破「其余互不依赖」;且前置是把 `app.rs`(3.6 万行)
里的编排解出来,一期会被绑在大重构上。手机端单 pane、无 rehost、无分屏,
编排形态本来就不同,硬抽共享层反而两边都扭曲。

**手机双向同步**:云端是整包 LWW(F270)。手机只要写一次(哪怕只是 `last_accessed_at`),
就可能拿旧包盖掉桌面刚做的修改且零报错——F247/F248「整份覆盖」那一族。
双向要先把同步改成按条合并,列为三期。

**Rust wgpu 画到 SurfaceView**:会把 glyphon 图集淘汰/`trim`/尺寸 NaN 那批坑
(`gui-render-gotchas.md`)原样搬过去,还得同时维护 Vulkan 与 GLES。手机竖屏约
50×30 格,Canvas 硬件加速绰绰有余;系统字体回落链自带中日文与 emoji,T9 那类豆腐块天然消失。

## 代价
- 第二个组装端 = 第二套 IO 编排。桌面修了拨号/重连类 bug,要人工判断手机端是否同病。
  缓解:判据(纯函数)只有一份;编排侧每条陷阱配同名守护测试。
- 新增 Kotlin/Gradle 工具链与 NDK,构建与发版流程多一条线。
- `mullion-store` 的职责从「持久化」扩到「项目判据」;它仍是零 UI、零 async、可纯单测,
  不变量本身不破。
- 「Windows 11 是唯一的一等公民」改为「Windows 11 与 Android(arm64,minSdk 30)
  是一等公民;macOS/Linux 能编过就行」。`CLAUDE.md` 的不变量表与边界段在 `mullion-ffi`
  落地的同一提交里改。

## 明确永不做(手机端)
分屏、内置编辑器、端口隧道、F100 标注。
