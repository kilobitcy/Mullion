# 远端状态自举 + 标题条重排 + 终端区缩进 实现 Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 F123 的远端状态在**没有任何远端手工配置**的机器上真的能拿到,并按用户要的
格式显示在分屏标题条上;顺带给终端区加左右缩进。

**Architecture:** 客户端连上后开一条旁路 exec channel,把 tmux 服务器的全局
`set-titles` 打开、`set-titles-string` 改成带 `#{pane_current_path}` 的格式(实测:对已
attach 的客户端立刻生效,且这是 server 级全局选项,不需要区分是哪块分屏)。SFTP 侧补一条
`~` 展开(用 `canonicalize(".")` 拿到的真登录目录)。标题条与 `geom` 的改动是纯显示/几何。

**Tech Stack:** Rust 2021 / winit 0.30 / egui 0.30 / russh 0.54 / tokio / alacritty_terminal 0.26

设计文档:`docs/superpowers/specs/2026-08-18-remote-state-bootstrap-design.md`

---

## 文件结构

| 文件 | 责任 | 新建/修改 |
|---|---|---|
| `crates/mullion-store/src/settings.rs` | `Settings` 增一个布尔开关 | 修改 |
| `crates/mullion-app/src/remote_bootstrap.rs` | 自举命令串 + 重试判据 + 共享标志。**零 IO、零 async** | 新建 |
| `crates/mullion-app/src/lib.rs` | 注册新模块 | 修改 |
| `crates/mullion-app/src/shell/workspace/mod.rs` | `HostConn` 挂自举状态 | 修改 |
| `crates/mullion-app/src/app.rs` | `about_to_wait` 里跑 tick;`expand_tilde`/`files_start_dir`/`sync_target_of`;`spawn_sftp_open` 三元组;`sftp_home` 存取 | 修改 |
| `crates/mullion-app/src/ui/settings.rs` | 设置弹窗加复选框 | 修改 |
| `crates/mullion-app/src/ui/pane_title.rs` | `title_text` 五参拼接;删 `side_text`/`SIDE_MAX_FRAC` | 修改 |
| `crates/mullion-app/src/shell/workspace/geom.rs` | `TERM_PAD_PT` + `term_px` 内缩 | 修改 |
| `crates/mullion-app/tests/tmux_bootstrap_live.rs` | 对**本机 tmux** 真跑一遍自举命令 | 新建 |
| `docs/remote-state-setup.md` | 从「你得手配」改写成「默认自动配、可关」 | 修改 |
| `spec.md` | 新增 F124 | 修改 |

**任务顺序**:1→2→3→4→5 是自举那条链(store → 纯逻辑 → 状态 → 接线 → live 验证);
6→7 是 SFTP `~` 展开;8、9 各自独立;10 文档;11 交付。

---

### Task 1: `Settings` 增自举开关

**Files:**
- Modify: `crates/mullion-store/src/settings.rs`

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-store/src/settings.rs` 的 `mod tests` 里加两条:

```rust
    /// F124:自举开关默认**开**。这是新字段,老的 settings.toml 里没有它 ——
    /// `serde(default)` 必须给 `true`,给 `false` 的话所有老用户升上来功能
    /// 静默不生效,而他们在设置里看到的是「开着」。
    ///
    /// 自证会变红:把 `default_tmux_bootstrap` 的返回值改成 `false`。
    #[test]
    fn tmux_bootstrap_defaults_to_on_for_files_written_before_it_existed() {
        let dir = tmp();
        std::fs::write(
            dir.path().join(SETTINGS_FILE),
            "schema_version = 1\nfont_pt = 10.0\n",
        )
        .expect("写老格式文件");
        let back = load(dir.path());
        assert!(back.note.is_none(), "老文件不该有 note:{:?}", back.note);
        assert!(
            back.settings.tmux_bootstrap,
            "老文件缺这个字段时该默认开"
        );
    }

    /// 关掉之后要能存住 —— 默认值是 `true`,`serde` 不会因为「等于默认」
    /// 而漏写(`toml::to_string_pretty` 无条件写全部字段)。
    #[test]
    fn tmux_bootstrap_survives_a_round_trip_when_turned_off() {
        let dir = tmp();
        let s = Settings {
            tmux_bootstrap: false,
            ..Settings::default()
        };
        save(dir.path(), &s).expect("写盘");
        assert!(!load(dir.path()).settings.tmux_bootstrap);
    }
```

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-store settings:: 2>&1 | tail -20
```
Expected: 编译失败,`no field 'tmux_bootstrap' on type 'Settings'`。

- [ ] **Step 3: 实现**

在 `Settings` 结构体里 `font_pt` 之后加字段:

```rust
    /// F124:连上之后自动开一条旁路 exec,把远端 tmux 的 `set-titles` 打开、
    /// `set-titles-string` 改成带 `#{pane_current_path}` 的格式。
    ///
    /// **默认开**:F123 的两个功能(标题条目录名 / SFTP 目录继承)在没配过的
    /// 远端上一个字节都收不到,而「没配过」是绝大多数机器的状态。关掉之后
    /// 整条自举不跑(连一次 exec 都不发),F123 退回手工配置(见
    /// `docs/remote-state-setup.md`)。
    ///
    /// 这是往用户的机器上写东西(改的是 tmux 服务器**内存里**的全局选项,
    /// 不落盘、server 退出即失效),所以必须给得出「不要」。
    #[serde(default = "default_tmux_bootstrap")]
    pub tmux_bootstrap: bool,
```

在 `default_font_pt` 旁边加:

```rust
fn default_tmux_bootstrap() -> bool {
    true
}
```

`impl Default for Settings` 里补 `tmux_bootstrap: true,`。

`schema_version` **不动**:新字段带 `serde(default)`,v1 的文件照读不误。

同文件既有测试 `settings_survive_a_round_trip` 里那个 `Settings { .. }` 字面量要补上
`tmux_bootstrap: true,`(否则编译不过)。

- [ ] **Step 4: 跑测试确认它绿**

```bash
cargo test -p mullion-store 2>&1 | grep -E "test result|FAILED"
```
Expected: `test result: ok.`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-store/src/settings.rs
git commit -m "feat(store): 设置增加 tmux 状态上报自举开关,默认开 (F124)"
```

---

### Task 2: `remote_bootstrap` 纯逻辑模块

**Files:**
- Create: `crates/mullion-app/src/remote_bootstrap.rs`
- Modify: `crates/mullion-app/src/lib.rs`

- [ ] **Step 1: 写失败的测试**

新建 `crates/mullion-app/src/remote_bootstrap.rs`,**先只写文件头 + 测试**:

```rust
//! F124:远端状态上报自举 —— 纯逻辑。零 IO、零 async,真正发 exec 在 `app.rs`。

#[cfg(test)]
mod tests {
    use super::*;

    /// 命令必须用 `&&` 串联两条 `tmux set`:退出码 0 当且仅当两条都成功,
    /// 不需要解析 stderr。换成 `;` 的话第一条失败、第二条成功也会返回 0,
    /// 我们就会把「没配上」记成「已成功」并**永不再试**。
    ///
    /// 自证会变红:把 `&&` 换成 `;`。
    #[test]
    fn the_command_chains_both_settings_with_and_so_the_exit_code_means_both() {
        let cmd = String::from_utf8(bootstrap_command()).expect("命令是 ASCII");
        assert!(cmd.contains("set -g set-titles on"), "少了开关那条:{cmd}");
        assert!(
            cmd.contains("&& tmux set -g set-titles-string"),
            "两条不是用 && 串的:{cmd}"
        );
    }

    /// format string 必须带 `#{pane_current_path}` —— 少了它 tmux 只报会话名,
    /// 目录名和 SFTP 目录继承两个功能都静默失效。整串用**单引号**包住:
    /// `#{...}` 里的 `{`/`}` 和 `#` 在 shell 里不能裸奔。
    ///
    /// 自证会变红:把 `TMUX_TITLES_STRING` 改成 `"#S:#I:#W"`。
    #[test]
    fn the_format_string_carries_the_pane_path_and_is_single_quoted() {
        let cmd = String::from_utf8(bootstrap_command()).expect("命令是 ASCII");
        assert!(cmd.contains("'#S:#I:#W #{pane_current_path}'"), "{cmd}");
    }

    /// 换 tmux 可执行名的那条(live 测试要用 `tmux -L mullion-test`)走的是
    /// **同一份模板**,不是另抄一遍 —— 抄一遍的话 live 测试验的就不是生产命令。
    #[test]
    fn the_production_command_is_the_template_with_the_plain_tmux_binary() {
        assert_eq!(bootstrap_command(), bootstrap_command_with("tmux"));
        let alt = String::from_utf8(bootstrap_command_with("tmux -L probe")).expect("ASCII");
        assert!(alt.starts_with("tmux -L probe set -g set-titles on"), "{alt}");
        assert!(alt.contains("&& tmux -L probe set -g set-titles-string"), "{alt}");
    }

    /// 判据的五条分支。
    ///
    /// 自证会变红:
    /// - 去掉 `enabled` 那条 → 第 1 条红
    /// - 去掉 `done` 那条 → 第 2 条红
    /// - 去掉 `busy` 那条 → 第 3 条红
    /// - 把 `None` 当成「不试」→ 第 4 条红
    /// - 把 `>=` 写成 `>` 之外的任何放宽 → 第 5 条红
    #[test]
    fn should_attempt_covers_every_early_exit() {
        // 1. 开关关着:什么都不做。
        assert!(!should_attempt(false, false, false, None));
        // 2. 已经配成功过:永不再试(tmux 全局选项在 server 生命期内一直有效)。
        assert!(!should_attempt(true, true, false, Some(RETRY_MS * 10)));
        // 3. 上一次还在途:不重叠发起(链路黑洞时一次 exec 可能挂很久)。
        assert!(!should_attempt(true, false, true, Some(RETRY_MS * 10)));
        // 4. 从没试过:立刻试。
        assert!(should_attempt(true, false, false, None));
        // 5. 距上次不足 RETRY_MS:等着;到点了才试。
        assert!(!should_attempt(true, false, false, Some(RETRY_MS - 1)));
        assert!(should_attempt(true, false, false, Some(RETRY_MS)));
    }

    /// 标志位的状态机。失败**不置 done** —— 置了就永不再试,而「tmux 服务器
    /// 还没起」正是最常见的失败原因,用户几秒后开 tmux 就再也配不上了。
    ///
    /// 自证会变红:把 `finish(false)` 也写成 `done.store(true, ..)`。
    #[test]
    fn flags_only_latch_done_on_success() {
        let f = BootstrapFlags::default();
        assert!(!f.is_done() && !f.is_busy());

        f.mark_busy();
        assert!(f.is_busy());
        f.finish(false);
        assert!(!f.is_busy(), "失败后要解锁,否则再也不会重试");
        assert!(!f.is_done(), "失败不该被记成成功");

        f.mark_busy();
        f.finish(true);
        assert!(f.is_done() && !f.is_busy());
    }

    /// `clone` 出来的是**同一份**标志(后台 task 拿走一份、主循环留一份,
    /// 两边看到的必须是同一个状态)。
    ///
    /// 自证会变红:把 `BootstrapFlags` 的 `Arc<AtomicBool>` 换成裸 `bool`
    /// 并 `#[derive(Clone, Copy)]` —— 后台 task 置的 done 主循环永远看不见,
    /// 表现为每 30 秒重配一次,永不停止。
    #[test]
    fn cloned_flags_share_one_state() {
        let a = BootstrapFlags::default();
        let b = a.clone();
        b.finish(true);
        assert!(a.is_done(), "clone 出来的是另一份状态,后台结论传不回主循环");
    }
}
```

- [ ] **Step 2: 跑测试确认它红**

先在 `crates/mullion-app/src/lib.rs` 的 `pub mod reflow;` 之前(按字母序在 `pane` 与
`reflow` 之间)加一行:

```rust
pub mod remote_bootstrap;
```

```bash
cargo test -p mullion-app remote_bootstrap 2>&1 | tail -20
```
Expected: 编译失败,`cannot find function 'bootstrap_command' in this scope`。

- [ ] **Step 3: 实现**

把下面这段插到 `remote_bootstrap.rs` 的 `#[cfg(test)]` 之前:

```rust
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// 塞给 tmux 的 `set-titles-string`。
///
/// - `#S` 会话名、`#I` 窗口序号、`#W` 窗口名 —— 前两段正是 `parse_title`
///   认会话名的判据(第一段是名字、第二段必须是纯数字)。
/// - `#{pane_current_path}` 是 tmux **自己**从 OS 拿的 pane 当前目录,不依赖
///   shell 发 OSC 7(tmux 会吃掉内层 OSC 7 不转发,那是 F51 被否的理由之一)。
///
/// **刻意覆写而不是追加到用户现值**:tmux 默认值里的 `#T`(pane 标题)可能
/// 先吐出一个像路径的 token,`parse_title` 取「第一个 `/` 开头的空白分隔
/// token」会拿错。也刻意不做「只在现值是 tmux 默认时才写」——那样在改过
/// 这个选项的机器上目录继承会**静默**失效,用户看不出为什么。
pub const TMUX_TITLES_STRING: &str = "#S:#I:#W #{pane_current_path}";

/// 两次尝试之间至少隔多久(毫秒)。
///
/// **由帧驱动,不排 `WaitUntil`**:事件循环空闲时会 `ControlFlow::Wait`,
/// 再加一个定时唤醒源就要在三个分支里都复位 control_flow(T7),不值得。
/// 而且用户真去开 tmux 的时候必然在敲键盘 —— 敲键盘就有帧,判据自然会跑到。
/// 完全空闲时不重试是对的:那时也没人在开 tmux。
pub const RETRY_MS: u64 = 30_000;

/// 生产用的自举命令。
pub fn bootstrap_command() -> Vec<u8> {
    bootstrap_command_with("tmux")
}

/// 同上,但可以换 tmux 可执行名/参数 —— live 测试用 `"tmux -L mullion-test"`
/// 打到一个隔离的 socket 上,不去动开发机上真在用的那个 tmux 服务器。
///
/// 两条用 `&&` 串:退出码 0 当且仅当两条都成功。用 `;` 的话第一条失败也可能
/// 返回 0,我们会把「没配上」记成「已成功」然后**永不再试**。
pub fn bootstrap_command_with(tmux: &str) -> Vec<u8> {
    format!("{tmux} set -g set-titles on && {tmux} set -g set-titles-string '{TMUX_TITLES_STRING}'")
        .into_bytes()
}

/// 这一帧该不该发起一次自举。
///
/// `since_last_ms` = 距上次**发起**多久;`None` = 这条连接上还没试过。
pub fn should_attempt(enabled: bool, done: bool, busy: bool, since_last_ms: Option<u64>) -> bool {
    if !enabled || done || busy {
        return false;
    }
    match since_last_ms {
        None => true,
        Some(ms) => ms >= RETRY_MS,
    }
}

/// 一条连接上自举的共享状态。
///
/// `Arc<AtomicBool>` 而不是裸 `bool`:结论在 tokio task 里产生、判据在事件
/// 循环里读,两边必须看到同一份。用 `UserEvent` 回送也行,但那要多一条按世代
/// 路由的事件变体 —— 这里的结论只有一个 bit,不值得。
#[derive(Debug, Clone, Default)]
pub struct BootstrapFlags {
    done: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
}

impl BootstrapFlags {
    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }
    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::Relaxed)
    }
    /// 发起前置位,防止上一次还挂在网络上时又发一次。
    pub fn mark_busy(&self) {
        self.busy.store(true, Ordering::Relaxed);
    }
    /// 收工。**只有成功才 latch `done`** —— 失败也 latch 的话,「tmux 服务器
    /// 还没起」(最常见的失败)会让这条连接从此再不重试,用户几秒后开了
    /// tmux 也配不上。
    pub fn finish(&self, ok: bool) {
        if ok {
            self.done.store(true, Ordering::Relaxed);
        }
        self.busy.store(false, Ordering::Relaxed);
    }
}
```

- [ ] **Step 4: 跑测试确认它绿**

```bash
cargo test -p mullion-app remote_bootstrap 2>&1 | grep -E "test result|FAILED"
```
Expected: `test result: ok. 6 passed`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/remote_bootstrap.rs crates/mullion-app/src/lib.rs
git commit -m "feat(app): 远端状态自举的命令串与重试判据 (F124)"
```

---

### Task 3: `HostConn` 挂自举状态

**Files:**
- Modify: `crates/mullion-app/src/shell/workspace/mod.rs`
- Modify: `crates/mullion-app/src/app.rs:4918`、`crates/mullion-app/src/app.rs:5131`(两处
  `ws.hosts.push(HostConn { .. })`)

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/shell/workspace/mod.rs` 的 `mod tests` 里加:

```rust
    /// F124:自举状态**按主机连接**存,不是按标签存 —— B2-b「换主机」之后
    /// 一个 `Workspace` 上会有多条 `HostConn`,每台机器的 tmux 服务器是独立的,
    /// 共享一份 `done` 会让第二台机器永远配不上(第一台成功就 latch 了)。
    ///
    /// 自证会变红:把 `tmux_bootstrap` 字段从 `HostConn` 挪到 `Workspace` 上。
    #[test]
    fn each_host_connection_carries_its_own_bootstrap_state() {
        let a = crate::remote_bootstrap::BootstrapFlags::default();
        let b = crate::remote_bootstrap::BootstrapFlags::default();
        a.finish(true);
        assert!(a.is_done());
        assert!(!b.is_done(), "两条连接的自举状态串了");
    }
```

> 说明:`HostConn` 里有 `Arc<SshConnection>`,而 `SshConnection::new` 是
> `pub(crate)` 的(`mullion-ssh` 内部),`mullion-app` 这边**造不出来**一个
> `HostConn` 实例。所以这条只能验标志本身互不干扰;「字段真的挂在 `HostConn`
> 上」由 Step 3 的源码级守护测试兜。

再加一条源码级守护:

```rust
    /// **接线守护**:自举状态必须挂在 `HostConn` 上。
    ///
    /// **扎的是源码结构**(`HostConn` 造不出实例,见上一条的说明)。验证边界:
    /// 挡得住「字段整个搬走」,挡不住「两边都留一份、只是没人用这份」。
    ///
    /// 自证会变红:把 `pub tmux_bootstrap: ...` 那行从 `HostConn` 删掉。
    #[test]
    fn bootstrap_state_lives_on_the_host_connection() {
        let src = include_str!("mod.rs");
        let after = src
            .split("pub struct HostConn {")
            .nth(1)
            .expect("找不到 HostConn 的定义");
        let body = &after[..after.find("\n}\n").expect("找不到 HostConn 的结尾")];
        assert!(
            body.contains("tmux_bootstrap"),
            "HostConn 上没有自举状态 —— 多主机时会共用一份 done"
        );
        assert!(
            body.contains("tmux_last_try"),
            "HostConn 上没有「上次发起时刻」—— 重试判据拿不到 since_last_ms,\
             要么永不重试要么每帧重试"
        );
    }
```

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-app workspace::tests::bootstrap 2>&1 | tail -20
```
Expected: FAIL,`HostConn 上没有自举状态`。

- [ ] **Step 3: 实现**

`crates/mullion-app/src/shell/workspace/mod.rs`,在 `pub struct HostConn` 的
`handle` 字段之后加:

```rust
    /// F124:这台机器上的 tmux 状态上报配过没有。**按连接存**——B2-b 换主机
    /// 之后一个 `Workspace` 上会有多条 `HostConn`,每台机器的 tmux 服务器
    /// 各自独立,共享一份 `done` 会让第二台永远配不上。
    pub tmux_bootstrap: crate::remote_bootstrap::BootstrapFlags,
    /// F124:上次**发起**自举的时刻。`None` = 还没试过。判据见
    /// `remote_bootstrap::should_attempt`。
    pub tmux_last_try: Option<std::time::Instant>,
```

`crates/mullion-app/src/app.rs` 两处构造点(`4918` 与 `5131` 附近的
`ws.hosts.push(crate::shell::workspace::HostConn {`)各补:

```rust
                    tmux_bootstrap: Default::default(),
                    tmux_last_try: None,
```

- [ ] **Step 4: 跑测试确认它绿**

```bash
cargo test -p mullion-app workspace:: 2>&1 | grep -E "test result|FAILED"
```
Expected: `test result: ok.`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/shell/workspace/mod.rs crates/mullion-app/src/app.rs
git commit -m "feat(app): HostConn 挂 tmux 自举状态 (F124)"
```

---

### Task 4: app 接线 —— `about_to_wait` 里跑自举 tick

**Files:**
- Modify: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/app.rs` 的 `mod tests` 里加(放在
`about_to_wait` 那条既有守护测试附近):

```rust
    /// **接线守护 / F124**:自举 tick 必须挂在 `about_to_wait` 上。
    ///
    /// 挂在别处的后果是静默的:挂在 `RedrawRequested` 的 `Present` 分支里,
    /// 被节流掉的帧就不跑;不挂,整个功能一次都不会发起。
    ///
    /// **扎的是源码结构**:真正验它要一条活连接 + `EventLoopProxy`,这个
    /// 测试容器里造不出来。验证边界:挡得住「整个调用被删/挪走」,挡不住
    /// 「函数体被掏空」。
    ///
    /// 自证会变红:把 `self.tick_tmux_bootstrap();` 从 `about_to_wait` 里删掉。
    #[test]
    fn about_to_wait_ticks_the_tmux_bootstrap() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn about_to_wait(")
            .nth(1)
            .expect("找不到 about_to_wait");
        assert!(
            after[..2000].contains("self.tick_tmux_bootstrap();"),
            "about_to_wait 不再跑自举 tick —— F124 一次都不会发起"
        );
    }

    /// **接线守护 / F124**:tick 的三件事都得在——判据走
    /// `remote_bootstrap::should_attempt`、发的是 `bootstrap_command()`、
    /// 结论按退出码写回 `finish(..)`。
    ///
    /// 每一条漏掉都是静默的:
    /// - 不走 `should_attempt` → 要么每帧发一次 exec(高延迟链路上刷爆),
    ///   要么只发第一次(tmux 服务器晚起就永远配不上)。
    /// - 不用 `bootstrap_command()` → live 测试验的命令跟实际发的不是同一条。
    /// - 不写回 `finish` → `busy` 永远置着,第一次之后再也不重试。
    ///
    /// 自证会变红:把 `should_attempt(..)` 换成 `true`。
    #[test]
    fn the_bootstrap_tick_uses_the_shared_predicate_command_and_writes_back() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn tick_tmux_bootstrap(&mut self) {")
            .nth(1)
            .expect("找不到 tick_tmux_bootstrap 的定义");
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 tick_tmux_bootstrap 的函数结尾")];
        // 先证明切出来的确实是函数体(切歪成空串的话下面几条会空过)。
        assert!(
            body.contains("hosts"),
            "tick_tmux_bootstrap 的函数体切歪了(切出来 {} 字节)",
            body.len()
        );
        assert!(
            body.contains("remote_bootstrap::should_attempt("),
            "tick 没走共享判据"
        );
        assert!(
            body.contains("self.settings.tmux_bootstrap"),
            "tick 没读设置开关 —— 用户关掉了照样发 exec"
        );
        assert!(
            body.contains("remote_bootstrap::bootstrap_command()"),
            "tick 发的不是共享的命令串"
        );
        assert!(
            body.contains(".finish("),
            "tick 没把结论写回标志 —— busy 永远置着,再也不重试"
        );
    }
```

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-app bootstrap 2>&1 | tail -20
```
Expected: FAIL,`找不到 tick_tmux_bootstrap 的定义`。

- [ ] **Step 3: 实现**

在 `crates/mullion-app/src/app.rs` 的 `impl App` 里,紧挨着 `flush_layout_if_due`
之后加:

```rust
    /// F124:该配的连接配一遍 tmux 状态上报。
    ///
    /// **两阶段**(先收集再 spawn):遍历 `self.tabs` 要 `&mut self.tabs`,而
    /// spawn 要 `self._runtime` —— 借用检查器不让两者同时活着(同
    /// `apply_layout_actions` 拆成自由函数的理由)。
    ///
    /// 失败**只记 debug 日志**:用户没装 tmux、tmux 用的是非默认 socket、
    /// 账号被 `ForceCommand` 限制,都会走到这里,弹错误卡片不成比例。
    fn tick_tmux_bootstrap(&mut self) {
        let enabled = self.settings.tmux_bootstrap;
        let now = Instant::now();
        let mut jobs: Vec<(Arc<SshConnection>, crate::remote_bootstrap::BootstrapFlags)> =
            Vec::new();
        for tab in self.tabs.iter_mut() {
            let Some(t) = tab.content.as_terminal_mut() else {
                // SFTP 节点标签没有 PTY,也就没有 tmux 客户端在跑;占位标签
                // 连连接都没有。两者都无事可做。
                continue;
            };
            for host in &mut t.ws.hosts {
                let since = host
                    .tmux_last_try
                    .map(|at| now.duration_since(at).as_millis() as u64);
                if !crate::remote_bootstrap::should_attempt(
                    enabled,
                    host.tmux_bootstrap.is_done(),
                    host.tmux_bootstrap.is_busy(),
                    since,
                ) {
                    continue;
                }
                host.tmux_last_try = Some(now);
                host.tmux_bootstrap.mark_busy();
                jobs.push((host.handle.clone(), host.tmux_bootstrap.clone()));
            }
        }
        for (conn, flags) in jobs {
            self._runtime.spawn(async move {
                let ok = match mullion_ssh::exec::exec(
                    &conn,
                    crate::remote_bootstrap::bootstrap_command(),
                )
                .await
                {
                    Ok(out) => out.succeeded(),
                    Err(e) => {
                        log::debug!(target: "mullion", "tmux 自举失败:{e}");
                        false
                    }
                };
                log::debug!(target: "mullion", "tmux 自举结论:{}", if ok { "已配好" } else { "未配上,稍后重试" });
                flags.finish(ok);
            });
        }
    }
```

在 `fn about_to_wait` 里,`self.flush_layout_if_due();` 之后加:

```rust
        // F124:到点就配一遍远端 tmux 的状态上报。跟布局落盘同一个理由放在
        // 这里 —— 它跟渲染无关,而 `about_to_wait` 是「已经闲下来了」这个
        // 语义唯一准确的位置。
        self.tick_tmux_bootstrap();
```

> `Tabs::iter_mut`(`shell/tabs.rs:95`)和 `TabContent::as_terminal_mut`
> (`app.rs:468`)都已存在,不用新加访问器。

- [ ] **Step 4: 跑测试确认它绿**

```bash
cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"
cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -5
```
Expected: `test result: ok.`,clippy 无输出。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 连上后自动配置远端 tmux 状态上报 (F124)"
```

---

### Task 5: 设置弹窗里的开关

**Files:**
- Modify: `crates/mullion-app/src/ui/settings.rs`
- Modify: `crates/mullion-app/src/app.rs`(`apply_settings_action` 的 Commit 分支)

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/ui/settings.rs` 的 `mod tests` 里加:

```rust
    /// F124:点自举开关要当场回报 `Preview`(草稿变了、需要重画),
    /// 「确定」时才落盘。回报 `None` 的话用户点了没反应。
    ///
    /// 用文件里既有的 `interact` 脚手架:它跑满 `FRAMES` 帧预热(切片 G 吃过
    /// 「预热帧数不足 → 点击落在按钮外面」的亏),再按标签文字找到部件中心点
    /// 下去。复选框是 `Sense::click()`,要**同帧松手**(`release = true`)。
    ///
    /// 自证会变红:把 `resp.changed()` 那个分支删掉。
    #[test]
    fn toggling_the_bootstrap_checkbox_reports_a_preview() {
        let mut d = draft();
        assert!(d.tmux_bootstrap, "脚手架的初值该是开着的");
        let out = interact(&mut d, BOOTSTRAP_LABEL, egui::Vec2::ZERO, true);
        assert!(!d.tmux_bootstrap, "复选框没被真的点到,这条测试测了个寂寞");
        assert_eq!(out, SettingsOut::Preview);
    }
```

标签串抽成常量,测试与实现共用一份(照抄一遍的话改文案就静默失联):

```rust
/// 自举开关的标签。测试要靠它在画出来的 `Shape::Text` 里找到这个部件,
/// 所以实现与测试必须共用同一份 —— 各写一遍的话改文案时测试会静默地
/// 点不中,`interact` 里那句 panic 才是唯一的提示。
pub const BOOTSTRAP_LABEL: &str = "自动配置远端 tmux 的状态上报";
```

> `interact(d, label, offset, release)` 与 `draft()` 是 `settings.rs` 里**已有**的
> 测试脚手架(`interact` 在 `443` 附近,`draft` 在 `369`)。`draft()` 的
> `SettingsDraft { .. }` 字面量要补一行 `tmux_bootstrap: true,`。

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-app settings::tests::toggling 2>&1 | tail -20
```
Expected: 编译失败,`no field 'tmux_bootstrap' on type 'SettingsDraft'`。

- [ ] **Step 3: 实现**

`SettingsDraft` 加字段 + 文档:

```rust
    /// F124:自动配置远端 tmux 状态上报。
    pub tmux_bootstrap: bool,
```

`from_settings` 里补 `tmux_bootstrap: s.tmux_bootstrap,`。

在 `show` 里,「外观」与「安全」之间插一个新分节:

```rust
            form::section(ui, t, "设置", "远端", &mut first);
            remote(ui, t, draft, &mut out);
```

新增函数(放在 `appearance` 之后):

```rust
/// 远端分节:自动配置 tmux 状态上报(F124)。
fn remote(ui: &mut egui::Ui, t: &Theme, draft: &mut SettingsDraft, out: &mut SettingsOut) {
    if ui.checkbox(&mut draft.tmux_bootstrap, BOOTSTRAP_LABEL).changed() {
        *out = SettingsOut::Preview;
    }
    ui.add_space(SP_S);
    ui.label(
        egui::RichText::new(
            "连上后开一条旁路命令通道,打开远端 tmux 的 set-titles 并让它报出当前目录。\
             分屏标题条上的目录名、以及文件面板继承终端所在目录都靠它。\
             改的是 tmux 服务器内存里的全局选项,不写任何文件。",
        )
        .size(11.0)
        .color(theme::c32(t.fg_dim)),
    );
    ui.add_space(SP_M);
}
```

> 说明串**故意不含** `BOOTSTRAP_LABEL` 的任何子串:`interact` 是按 galley 全文
> 相等找部件的,不会误命中;但改文案时别把标签整句塞进说明里。
> `SP_S` / `SP_M` / `theme::c32` 以文件里既有分节的写法为准(`grep -n "SP_S\|c32("
> crates/mullion-app/src/ui/settings.rs | head`),别引入新的间距常量。

`app.rs` 的 `apply_settings_action`:在把草稿写回 `self.settings` 的地方补
`self.settings.tmux_bootstrap = draft.tmux_bootstrap;`(找 `font_family` 那一行,
紧挨着写)。

- [ ] **Step 4: 跑测试确认它绿**

```bash
cargo test -p mullion-app settings 2>&1 | grep -E "test result|FAILED"
```
Expected: `test result: ok.`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/settings.rs crates/mullion-app/src/app.rs
git commit -m "feat(app): 设置弹窗加 tmux 自举开关 (F124)"
```

---

### Task 6: live 测试 —— 对本机 tmux 真跑一遍

**Files:**
- Create: `crates/mullion-app/tests/tmux_bootstrap_live.rs`

- [ ] **Step 1: 写测试**

```rust
//! F124:拿**真的 tmux** 跑一遍自举命令,断言两个选项确实被改了。
//!
//! 这不是脚手架 —— 它验的是「我们拼的那条命令串在真 tmux 上到底管不管用」,
//! 而那正是这个功能唯一会悄悄坏掉的地方(tmux 改选项名、改 format 语法)。
//! 走本机 `sh -c`,不走 SSH:命令串是共享的,SSH 那一段由 `mullion-ssh` 的
//! live 测试覆盖,这里没必要再要一台真机。
//!
//! 跑法(需要本机装了 tmux):
//! ```bash
//! cargo test -p mullion-app --test tmux_bootstrap_live -- --ignored
//! ```
//! 打的是 `-L mullion-test` 这个**隔离 socket**,不碰开发机上真在用的那个
//! tmux 服务器。

use std::process::Command;

const SOCK: &str = "mullion-test";

fn tmux(args: &[&str]) -> std::process::Output {
    Command::new("tmux")
        .args(["-L", SOCK])
        .args(args)
        .output()
        .expect("跑 tmux")
}

#[test]
#[ignore = "要本机装 tmux;用 --ignored 跑"]
fn the_bootstrap_command_really_turns_tmux_reporting_on() {
    // 干净起点。
    let _ = tmux(&["kill-server"]);

    // 1. 服务器不在时,命令必须**失败**且**不会顺手拉起一个空 server** ——
    //    退出码 0 的话我们会把「没配上」latch 成 done,永不再试。
    let cmd = String::from_utf8(mullion_app::remote_bootstrap::bootstrap_command_with(
        &format!("tmux -L {SOCK}"),
    ))
    .expect("命令是 ASCII");
    let cold = Command::new("sh").arg("-c").arg(&cmd).output().expect("跑 sh");
    assert!(
        !cold.status.success(),
        "tmux 服务器不在时自举居然成功了 —— 成功判据失效"
    );
    assert!(
        !tmux(&["ls"]).status.success(),
        "自举把一个空 tmux 服务器拉起来了 —— 会在用户机器上留下幽灵 server"
    );

    // 2. 有服务器时:成功,而且两个选项真的变了。
    assert!(
        tmux(&["new-session", "-d", "-s", "boot"]).status.success(),
        "起不来测试用的 tmux 会话"
    );
    let hot = Command::new("sh").arg("-c").arg(&cmd).output().expect("跑 sh");
    assert!(
        hot.status.success(),
        "自举在活着的 tmux 上失败了:{}",
        String::from_utf8_lossy(&hot.stderr)
    );

    let titles = String::from_utf8_lossy(&tmux(&["show", "-g", "set-titles"]).stdout).to_string();
    assert!(titles.contains("set-titles on"), "set-titles 没被打开:{titles}");

    let fmt =
        String::from_utf8_lossy(&tmux(&["show", "-g", "set-titles-string"]).stdout).to_string();
    assert!(
        fmt.contains(mullion_app::remote_bootstrap::TMUX_TITLES_STRING),
        "set-titles-string 不是我们那串:{fmt}"
    );

    let _ = tmux(&["kill-server"]);
}
```

- [ ] **Step 2: 跑它**

```bash
cargo test -p mullion-app --test tmux_bootstrap_live -- --ignored 2>&1 | tail -20
```
Expected: `test result: ok. 1 passed`。

失败的话**先看是不是命令串真的不对**(手动跑一遍 `tmux -L mullion-test show -g
set-titles-string`),不要为了让它过去放松断言。

- [ ] **Step 3: 确认它不在默认 `cargo test` 里跑**

```bash
cargo test -p mullion-app --test tmux_bootstrap_live 2>&1 | grep -E "test result"
```
Expected: `test result: ok. 0 passed; 0 failed; 1 ignored`

- [ ] **Step 4: 提交**

```bash
git add crates/mullion-app/tests/tmux_bootstrap_live.rs
git commit -m "test(app): 拿本机真 tmux 验自举命令 (F124)"
```

---

### Task 7: `expand_tilde` + `files_start_dir` 收 home

**Files:**
- Modify: `crates/mullion-app/src/app.rs`(`files_start_dir` 在 `7085` 附近;
  `sync_target_of` 在 `7106` 附近;测试在 `12016` / `12047` 附近)

- [ ] **Step 1: 写失败的测试**

在 `app.rs` 的 `mod tests` 里,紧挨着 `files_start_dir_prefers_the_panes_cwd_but_only_if_absolute`
加:

```rust
    /// F123 补缺口:标题里拿到的常常是 `~/Mullion` 这种缩写,而 openssh 的
    /// `sftp-server` **不展开 `~`**。拿 SFTP 的真登录目录(`canonicalize(".")`)
    /// 把它拼成绝对路径,裸 shell 场景就不用配任何东西也能继承目录了。
    ///
    /// 自证会变红:让 `expand_tilde` 无条件返回 `None`(前两条红);
    /// 把 `~user` 那条也当成 `~` 展开(第四条红)。
    #[test]
    fn expand_tilde_uses_the_sftp_login_directory() {
        assert_eq!(
            expand_tilde(b"~", b"/home/dev").as_deref(),
            Some(&b"/home/dev"[..])
        );
        assert_eq!(
            expand_tilde(b"~/Mullion", b"/home/dev").as_deref(),
            Some(&b"/home/dev/Mullion"[..])
        );
        // 已经是绝对路径:不归它管(调用方那一档更优先)。
        assert_eq!(expand_tilde(b"/srv/app", b"/home/dev"), None);
        // `~user` 的语义要查远端的 passwd,我们不知道 —— **不猜**。
        assert_eq!(expand_tilde(b"~foo/x", b"/home/dev"), None);
        assert_eq!(expand_tilde(b"", b"/home/dev"), None);
        // home 自己是根目录时不能拼出 `//x`。
        assert_eq!(expand_tilde(b"~/x", b"/").as_deref(), Some(&b"/x"[..]));
        // home 带尾斜杠同理。
        assert_eq!(
            expand_tilde(b"~/x", b"/home/dev/").as_deref(),
            Some(&b"/home/dev/x"[..])
        );
    }

    /// 四档优先级:pane 报的绝对路径 > 展开后的 `~` > 配置的默认远端目录 >
    /// `None`(交给调用方落回登录目录)。
    ///
    /// 自证会变红:把 `home` 那一档删掉(第二条落到 `/srv`,红)。
    #[test]
    fn files_start_dir_expands_a_tilde_before_falling_back_to_the_configured_dir() {
        // 绝对路径最优先,home 在不在都一样。
        assert_eq!(
            files_start_dir(Some(b"/home/dev/Mullion"), Some("/srv"), Some(b"/home/dev"))
                .as_deref(),
            Some("/home/dev/Mullion")
        );
        // `~` + 已知 home → 展开。
        assert_eq!(
            files_start_dir(Some(b"~/Mullion"), Some("/srv"), Some(b"/home/dev")).as_deref(),
            Some("/home/dev/Mullion")
        );
        // `~` 但 home 未知(sftp 还没开):不展开、不猜 `/home/<user>`,
        // 落回配置值。
        assert_eq!(
            files_start_dir(Some(b"~/Mullion"), Some("/srv"), None).as_deref(),
            Some("/srv")
        );
        // pane 什么都没报:配置值。
        assert_eq!(
            files_start_dir(None, Some("/srv"), Some(b"/home/dev")).as_deref(),
            Some("/srv")
        );
        // 都没有:None。
        assert_eq!(files_start_dir(None, None, Some(b"/home/dev")), None);
        // 非 UTF-8 展开结果同样进不了 `Option<String>`,落回配置值。
        assert_eq!(
            files_start_dir(Some(b"~/\xff"), Some("/srv"), Some(b"/home/dev")).as_deref(),
            Some("/srv")
        );
    }
```

同时把既有的 `files_start_dir_prefers_the_panes_cwd_but_only_if_absolute` 与
`sync_target_of` 那几条测试里的调用补上新参数(`None` 即可,除非那条用例本来就在验
`~`)。

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-app files_start_dir 2>&1 | tail -20
```
Expected: 编译失败,`cannot find function 'expand_tilde'`。

- [ ] **Step 3: 实现**

在 `app.rs` 的 `files_start_dir` **之前**加:

```rust
/// F123 补缺口:把 `~` / `~/x` 拿远端的**真登录目录**展开成绝对路径。
///
/// 为什么需要:Ubuntu 默认 bash 报的标题是 `user@host: ~/Mullion`,而 openssh 的
/// `sftp-server` **不展开 `~`** —— 直接拿去 `canonicalize` 会失败,面板停在
/// 「取不到登录目录」,比不继承更糟。
///
/// 只认恰好是 `~` 和以 `~/` 开头两种。**`~user` 不展开**:那要查远端的 passwd,
/// 我们不知道,猜错会把用户带到别人的家目录去。已经是绝对路径的返回 `None` ——
/// 那一档由调用方更优先地处理。
fn expand_tilde(cwd: &[u8], home: &[u8]) -> Option<Vec<u8>> {
    let rest = match cwd {
        b"~" => return Some(home.to_vec()),
        _ => cwd.strip_prefix(b"~/")?,
    };
    // home 的尾斜杠剥掉再拼,否则 `/` + `/x` 会拼出 `//x`(POSIX 里 `//`
    // 开头是实现定义的,某些系统当成另一个命名空间)。
    let mut out = home.to_vec();
    while out.last() == Some(&b'/') {
        out.pop();
    }
    out.push(b'/');
    out.extend_from_slice(rest);
    Some(out)
}
```

把 `files_start_dir` 改成:

```rust
fn files_start_dir(
    pane_cwd: Option<&[u8]>,
    default_remote: Option<&str>,
    home: Option<&[u8]>,
) -> Option<String> {
    let absolute = |c: &[u8]| -> Option<String> {
        if !c.starts_with(b"/") {
            return None;
        }
        String::from_utf8(c.to_vec()).ok()
    };
    let from_pane = pane_cwd.and_then(|c| {
        absolute(c).or_else(|| {
            let home = home?;
            absolute(&expand_tilde(c, home)?)
        })
    });
    from_pane.or_else(|| default_remote.map(str::to_string))
}
```

并把它的文档注释里那段「非 UTF-8 / `~` 一律落回配置值」补一句:`~` 现在在 home
已知时会先被展开。

`sync_target_of` 加第四个参数并透传:

```rust
fn sync_target_of(
    gen: Option<u64>,
    has_client: bool,
    pane_cwd: Option<&[u8]>,
    home: Option<&[u8]>,
) -> Option<(u64, String)> {
    let gen = gen?;
    if !has_client {
        return None;
    }
    let dir = files_start_dir(pane_cwd, None, home)?;
    Some((gen, dir))
}
```

- [ ] **Step 4: 跑测试确认它绿**

```bash
cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"
```
Expected: `test result: ok.`(`sync_files_to_focused_pane` 的调用点这一步会编译
失败——顺手把第四个实参填成 `None`,Task 8 再接真值)。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): SFTP 起始目录用登录目录展开 ~ (F123)"
```

---

### Task 8: 把真登录目录接进两条继承路径

**Files:**
- Modify: `crates/mullion-app/src/app.rs`(`UserEvent::SftpOpened` 定义 `137`;
  `spawn_sftp_open` `7153`;`accept_sftp_opened` `3483`;`trigger_sftp_open` `3453`;
  `sync_files_to_focused_pane` `2261`;`TerminalTab`/`FilesTab` 结构体;
  `TabContent` 访问器;既有源码级守护测试 `11961` / `12119`)

- [ ] **Step 1: 写失败的测试**

先改既有的两条源码级守护(它们现在断言 `trigger_sftp_open` 体内调
`files_start_dir` —— 这个职责要挪进 `spawn_sftp_open`):

把 `trigger_sftp_open_passes_the_tabs_default_remote_into_spawn_sftp_open` 与
`trigger_sftp_open_inherits_the_focused_panes_directory` 两条整体替换成:

```rust
    /// **接线守护**:`trigger_sftp_open` 把「配置的默认远端目录」和「焦点 pane
    /// 报出来的 cwd」**两样都原样**递给 `spawn_sftp_open`。
    ///
    /// 起始目录的计算从这里挪进了 `spawn_sftp_open` —— `~` 展开要用远端的真
    /// 登录目录,而那个值只有 `canonicalize(".")` 回来之后才知道,在这里算
    /// 就必然拿不到。这里只负责把两个原料递下去,少递一个都是静默失效:
    /// 少了 `default_remote`,F120 配置的默认目录被丢;少了 `pane_cwd`,
    /// F123 的目录继承被丢。
    ///
    /// 自证会变红:把 `pane_cwd` 那个实参换成 `None`。
    #[test]
    fn trigger_sftp_open_hands_both_ingredients_to_spawn_sftp_open() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn trigger_sftp_open(&mut self, generation: u64) {")
            .nth(1)
            .expect("找不到 trigger_sftp_open 的定义");
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 trigger_sftp_open 的函数结尾")];

        assert!(
            body.contains("let default_remote = tab.content.sftp_default_remote();"),
            "trigger_sftp_open 没从 tab 读 default_remote"
        );
        assert!(
            body.contains("focused_pane_cwd()"),
            "trigger_sftp_open 没读焦点 pane 的当前目录 —— 目录继承会静默失效"
        );

        let call_after = body
            .split("spawn_sftp_open(")
            .nth(1)
            .expect("找不到 spawn_sftp_open 调用");
        let call_args = &call_after[..call_after
            .find(");")
            .expect("找不到 spawn_sftp_open 调用的结尾")];
        assert!(
            call_args.contains("default_remote"),
            "spawn_sftp_open 没收到 default_remote —— F120 的默认目录被静默丢弃"
        );
        assert!(
            call_args.contains("pane_cwd"),
            "spawn_sftp_open 没收到 pane_cwd —— F123 的目录继承被静默丢弃"
        );
    }

    /// **接线守护**:`accept_sftp_opened` 必须把真登录目录存进标签。
    ///
    /// 不存的话「侧栏已开着、关→开跃迁」那条路(`sync_files_to_focused_pane`)
    /// 拿不到 home,`~/Mullion` 展不开,面板停在原处 —— 而首次打开那条路
    /// 却是好的,现象成了「第一次开对、之后再开都不对」,极难对上原因。
    ///
    /// 自证会变红:把 `set_sftp_home` 那一行删掉。
    #[test]
    fn accept_sftp_opened_remembers_the_login_directory() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn accept_sftp_opened(")
            .nth(1)
            .expect("找不到 accept_sftp_opened 的定义");
        let body = &after[..after
            .find("\n    fn ")
            .expect("找不到 accept_sftp_opened 的函数结尾")];
        assert!(
            body.contains("generation"),
            "函数体切歪了({} 字节)",
            body.len()
        );
        assert!(
            body.contains("set_sftp_home("),
            "登录目录没被存下来 —— 侧栏「关→开」跃迁那条路展不开 ~"
        );
    }

    /// **接线守护**:`sync_files_to_focused_pane` 要把存下来的登录目录喂给
    /// `sync_target_of`。传死 `None` 的话这条路永远展不开 `~`。
    ///
    /// 自证会变红:把 `sync_target_of` 的第四个实参改成字面量 `None`。
    #[test]
    fn the_sidebar_sync_feeds_the_login_directory_into_the_predicate() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn sync_files_to_focused_pane(&mut self) {")
            .nth(1)
            .expect("找不到 sync_files_to_focused_pane 的定义");
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 sync_files_to_focused_pane 的函数结尾")];
        assert!(body.contains("sftp_home()"), "没读标签存下的登录目录");
        let call = body
            .split("sync_target_of(")
            .nth(1)
            .expect("没调 sync_target_of");
        let args = &call[..call.find(")").expect("找不到 sync_target_of 调用的结尾")];
        assert!(
            args.contains("home"),
            "sync_target_of 收到的不是登录目录:{args}"
        );
    }
```

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-app sftp_open 2>&1 | tail -20
```
Expected: FAIL,`登录目录没被存下来`。

- [ ] **Step 3: 实现**

**(a) 标签上存登录目录。** `TerminalTab` 的 `sftp_default_remote` 之后、`FilesTab`
的同名字段之后,各加:

```rust
    /// F123:这条 sftp 连接的**真登录目录**(`canonicalize(".")` 的结果)。
    /// `None` = sftp 还没开好。用来把标题里报的 `~/Mullion` 展开成绝对路径
    /// (`sftp-server` 不展开 `~`)。
    ///
    /// **不是「面板当前目录」**:那个在 `files.remote.cwd` 里,会随用户浏览
    /// 移动;这个一旦拿到就不变。
    sftp_home: Option<mullion_ssh::sftp::RemotePath>,
```

两处构造 `TerminalTab { .. }` / `FilesTab { .. }` 的地方补 `sftp_home: None,`
(`grep -n "sftp_default_remote:" crates/mullion-app/src/app.rs` 能一次找齐)。

`TabContent` 加两个访问器(放在 `sftp_default_remote` 旁边):

```rust
    /// F123:这个标签 sftp 的登录目录。占位标签恒 `None`。
    fn sftp_home(&self) -> Option<Vec<u8>> {
        match self {
            TabContent::Terminal(t) => t.sftp_home.as_ref().map(|p| p.as_bytes().to_vec()),
            TabContent::Files(f) => f.sftp_home.as_ref().map(|p| p.as_bytes().to_vec()),
            TabContent::Restored(_) => None,
        }
    }

    /// 同上,写入。占位标签没有 sftp,静默忽略。
    fn set_sftp_home(&mut self, home: mullion_ssh::sftp::RemotePath) {
        match self {
            TabContent::Terminal(t) => t.sftp_home = Some(home),
            TabContent::Files(f) => f.sftp_home = Some(home),
            TabContent::Restored(_) => {}
        }
    }
```

**(b) 事件带回三样。** `UserEvent::SftpOpened` 的 `result` 类型从
`Result<(Arc<SftpClient>, RemotePath), String>` 改成
`Result<(Arc<SftpClient>, RemotePath, RemotePath), String>`,并在变体的文档注释里写明:

```rust
    /// F50/D6:sftp channel 开好了。成功时三样一起回来:
    /// `(client, 登录目录, 这次要打开的目录)`。
    ///
    /// **登录目录与起始目录是两回事**:前者恒等于 `canonicalize(".")`,用来
    /// 展开标题里报的 `~/...`(F123);后者是本次要 list 的那个目录,可能来自
    /// pane 的 cwd、F120 的配置值,或者就等于登录目录。合成一个字段的话
    /// 「侧栏关→开跃迁」那条路会拿着用户浏览到的目录去当 home 用。
```

**(c) `spawn_sftp_open` 改签名 + 两次 canonicalize。** 整个函数体换成:

```rust
fn spawn_sftp_open(
    runtime: &Runtime,
    proxy: &EventLoopProxy<UserEvent>,
    generation: u64,
    handle: Arc<SshConnection>,
    default_remote: Option<String>,
    pane_cwd: Option<Vec<u8>>,
) -> tokio::task::JoinHandle<()> {
    let proxy = proxy.clone();
    runtime.spawn(async move {
        let result = match mullion_ssh::sftp::SftpClient::open(handle).await {
            Ok(client) => {
                let dot = mullion_ssh::sftp::RemotePath::from_bytes(b".".to_vec());
                match client.canonicalize(&dot).await {
                    Ok(home) => {
                        // F123:`~` 只有在这里才展得开 —— 登录目录要等
                        // `canonicalize(".")` 回来才知道,调用方那一侧算不了。
                        let start = files_start_dir(
                            pane_cwd.as_deref(),
                            default_remote.as_deref(),
                            Some(home.as_bytes()),
                        );
                        let want = configured_remote_dir(start.as_deref());
                        // 起点就是登录目录时省掉第二次往返(高延迟链路上
                        // 一次 RTT 是能看出来的),这也是最常见的情况。
                        let dir = if want.as_bytes() == b"." {
                            Ok(home.clone())
                        } else {
                            client.canonicalize(&want).await
                        };
                        match dir {
                            Ok(dir) => Ok((Arc::new(client), home, dir)),
                            Err(e) => Err(format!("SFTP 已连上,但打不开起始目录:{e}")),
                        }
                    }
                    // 这一步失败时 channel **已经开成功了**,只是取不到登录
                    // 目录。跟上面那条共用「打开 SFTP 失败」会把排查方向带偏
                    // 到连接/认证层,而真实原因通常在权限或远端 `.` 不可 stat。
                    Err(e) => Err(format!("SFTP 已连上,但取不到登录目录:{e}")),
                }
            }
            Err(e) => Err(format!("打开 SFTP 失败:{e}")),
        };
        let _ = proxy.send_event(UserEvent::SftpOpened { generation, result });
    })
}
```

**(d) `trigger_sftp_open`** 里删掉 `let start_dir = files_start_dir(..);` 那一行,
把调用改成:

```rust
        let task = spawn_sftp_open(
            &self._runtime,
            &self.proxy,
            generation,
            conn,
            default_remote,
            pane_cwd,
        );
```

**(e) `accept_sftp_opened`** 的签名与 `Ok` 分支:

```rust
            Ok((client, home, dir)) => {
                let seq = {
                    let Some(tab) = self.tabs.by_generation_mut(generation) else {
                        log::debug!(target: "mullion", "丢弃过期世代 {generation} 的 SFTP 打开结果");
                        return;
                    };
                    let Some(slot) = tab.content.sftp_mut() else {
                        log::debug!(target: "mullion", "世代 {generation} 是占位标签,丢弃 SFTP 打开结果");
                        return;
                    };
                    *slot = Some(client.clone());
                    // F123:登录目录存下来,给「侧栏关→开跃迁」那条路展开 `~`。
                    tab.content.set_sftp_home(home);
                    let Some(files) = tab.content.files_panel_mut() else {
                        return;
                    };
                    files.remote.begin_load(dir.clone())
                };
                let task =
                    spawn_sftp_list_dir(&self._runtime, &self.proxy, generation, client, dir, seq);
                self.track_sftp_task(generation, task);
            }
```

**(f) `sync_files_to_focused_pane`**:

```rust
    fn sync_files_to_focused_pane(&mut self) {
        let gen = self.files_owner_generation();
        let tab = gen.and_then(|g| self.tabs.by_generation(g));
        let has_client = tab.is_some_and(|t| t.content.sftp_client().is_some());
        let pane_cwd = tab.and_then(|t| t.content.focused_pane_cwd());
        let home = tab.and_then(|t| t.content.sftp_home());
        let Some((gen, dir)) = sync_target_of(gen, has_client, pane_cwd.as_deref(), home.as_deref())
        else {
            return;
        };
        let target = mullion_ssh::sftp::RemotePath::from_bytes(dir.into_bytes());
        self.apply_remote_file_action(gen, crate::ui::files_panel::FileAction::Goto(target));
    }
```

- [ ] **Step 4: 跑测试确认它绿**

```bash
cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"
cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -5
```
Expected: `test result: ok.`,clippy 无输出。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): SFTP 记住真登录目录,两条继承路径都能展开 ~ (F123)"
```

---

### Task 9: 标题条重排

**Files:**
- Modify: `crates/mullion-app/src/ui/pane_title.rs`
- Modify: `crates/mullion-app/src/app.rs:6021` 附近(`TitleView` 的构造)

- [ ] **Step 1: 写失败的测试**

把 `pane_title.rs` 里既有的三条 `title_text` 测试整体替换成:

```rust
    /// ⑥/F123:一整串按「序号 · 节点名 · 目录名 · tmux 名」拼。
    ///
    /// 序号保留:分屏一多要靠它认哪块是哪块。
    #[test]
    fn title_puts_host_then_directory_then_tmux_on_one_line() {
        assert_eq!(
            title_text(2, Some("build-01"), Some("Mullion"), Some("main"), PaneStatus::Live),
            "2 · build-01 · Mullion · main"
        );
    }

    /// 缺的段**连分隔符一起消失** —— 留一个孤零零的 `·` 比不显示更糟。
    ///
    /// 自证会变红:把实现改成无条件 `format!("{index} · {host} · {dir} · {tmux}")`
    /// 并把 `None` 渲染成空串。
    #[test]
    fn missing_pieces_take_their_separator_with_them() {
        assert_eq!(
            title_text(3, Some("build-01"), Some("Mullion"), None, PaneStatus::Live),
            "3 · build-01 · Mullion"
        );
        assert_eq!(
            title_text(4, Some("build-01"), None, Some("main"), PaneStatus::Live),
            "4 · build-01 · main"
        );
        assert_eq!(
            title_text(5, Some("build-01"), None, None, PaneStatus::Live),
            "5 · build-01"
        );
    }

    /// §6.3:断开的 pane 内容留着可滚可复制,但状态必须写在脸上 ——
    /// 不然用户会对着一块不响应的终端反复敲键。
    ///
    /// **断开时不带目录和 tmux 名**:那两个此刻是陈旧值(远端早就不说话了),
    /// 摆着是误导。
    ///
    /// 自证会变红:把 `Disconnected` 分支也拼上 dir/tmux。
    #[test]
    fn a_disconnected_pane_says_so_and_drops_the_stale_context() {
        let s = title_text(
            1,
            Some("build-01"),
            Some("Mullion"),
            Some("main"),
            PaneStatus::Disconnected,
        );
        assert_eq!(s, "1 · build-01 (已断开)");
    }

    /// 预分配了叶子位但 channel 还没开好的空窗期(见 Workspace::apply_preset)。
    #[test]
    fn pane_without_a_host_yet_says_connecting() {
        assert_eq!(
            title_text(3, None, Some("Mullion"), Some("main"), PaneStatus::Live),
            "3 · 连接中…"
        );
    }
```

再把既有引用 `side_text` 的测试删掉(`grep -n "side_text" crates/mullion-app/src/ui/pane_title.rs`
找齐)。

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-app pane_title 2>&1 | tail -20
```
Expected: 编译失败,`this function takes 3 arguments but 5 arguments were supplied`。

- [ ] **Step 3: 实现**

`title_text` 换成:

```rust
/// 标题条上的文字。抽成纯函数是因为格式会被人反复调,而它是唯一能自动验的部分。
///
/// 格式:`序号 · 节点名 · 最后一级目录名 · tmux 会话名`。后两段来自 F123 的
/// 远端状态,**拿不到就连分隔符一起消失**(留一个孤零零的 `·` 比不显示更糟)。
///
/// 断开时只留 `序号 · 节点名 (已断开)`:目录名和 tmux 名此刻是陈旧值,
/// 远端早就不说话了,摆着是误导。
pub fn title_text(
    index: usize,
    host: Option<&str>,
    dir: Option<&str>,
    tmux: Option<&str>,
    status: PaneStatus,
) -> String {
    let Some(h) = host else {
        // (None, Disconnected) 并进这条是安全的,不是漏判:状态机里
        // host == None 当且仅当 PaneState 还没挂上(见 Workspace::apply_preset),
        // 此时 status 走默认的 Live;一旦 PaneState 存在,host_ix 必指向真实的
        // HostConn,host 必为 Some。这个组合在当前状态机下不可达。
        return format!("{index} · 连接中…");
    };
    if status == PaneStatus::Disconnected {
        return format!("{index} · {h} (已断开)");
    }
    let mut parts = vec![index.to_string(), h.to_string()];
    parts.extend(dir.map(str::to_string));
    parts.extend(tmux.map(str::to_string));
    parts.join(" · ")
}
```

> `PaneStatus` 已经 derive 了 `PartialEq`(`shell/workspace/mod.rs:13`),
> `status == PaneStatus::Disconnected` 直接可写。

删掉 `side_text` 函数与 `SIDE_MAX_FRAC` 常量。

`show` 里删掉右区那整段(`if let Some(s) = side_text(..) { .. }`),并把左区的
`title_text` 调用改成:

```rust
                                egui::RichText::new(title_text(
                                    v.index,
                                    v.host,
                                    v.cwd_leaf.as_deref(),
                                    v.tmux,
                                    v.status,
                                ))
```

`TitleView` 的两个字段文档更新一句:它们现在进的是左区那一整串,不再是右区。

`app.rs:6021` 附近 `TitleView` 的构造**不用改**(字段名没变);
`ui/mod.rs:1401`、`ui/pane_edges.rs:203` 那些测试脚手架里的 `cwd_leaf: None, tmux: None`
也不用改。

- [ ] **Step 4: 跑测试确认它绿**

```bash
cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"
cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -5
```
Expected: `test result: ok.`,clippy 无输出。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/pane_title.rs
git commit -m "feat(app): 标题条改为 序号·节点·目录·tmux 一整串 (F123/F83)"
```

---

### Task 10: 终端区左右缩进

**Files:**
- Modify: `crates/mullion-app/src/shell/workspace/geom.rs`

- [ ] **Step 1: 写失败的测试**

在 `geom.rs` 的 `mod tests` 里加:

```rust
    /// F80:终端区左右各内缩,不再顶着 pane 边界。缩进量是**逻辑点**,
    /// 跟标题条一样随 DPI 缩放 —— 写死物理像素的话 200% 缩放下这条缝会
    /// 细成一半。
    ///
    /// 自证会变红:把 `term_pad_px` 改回 `|_| 0`。
    #[test]
    fn the_terminal_area_is_inset_on_both_sides() {
        assert_eq!(term_pad_px(1.0), 8);
        assert_eq!(term_pad_px(1.5), 12);
        assert_eq!(term_pad_px(2.0), 16);

        let g = layout_geometry(&Node::Leaf(PaneId(1)), AREA, (8.0, 16.0), false, 1.0);
        let pad = term_pad_px(1.0);
        assert_eq!(g[0].term_px.x, AREA.x + pad, "左边没内缩");
        assert_eq!(g[0].term_px.w, AREA.w - 2 * pad, "宽度没扣掉两侧缩进");
        // 纵向不动 —— 再吃半行高度不值。
        assert_eq!(g[0].term_px.y, AREA.y);
        assert_eq!(g[0].term_px.h, AREA.h);
    }

    /// 非有限 / 非正的 `ppp` 落回 1.0,理由同 `title_bar_px`:winit 在显示器
    /// 热插拔的瞬间报过 0 / NaN 的 `scale_factor`。
    ///
    /// 自证会变红:删掉 `term_pad_px` 里的 `is_finite() && > 0.0` 兜底。
    #[test]
    fn a_broken_scale_factor_falls_back_for_the_pad_too() {
        assert_eq!(term_pad_px(0.0), 8);
        assert_eq!(term_pad_px(-2.0), 8);
        assert_eq!(term_pad_px(f32::NAN), 8);
        assert_eq!(term_pad_px(f32::INFINITY), 8);
    }

    /// 极窄 pane:宽度扣到 0 而不是下溢回绕成天文数字,`grid` 仍被夹到
    /// 至少 `(1, 1)`(PTY 侧不接受 0 列)。
    ///
    /// 自证会变红:把 `saturating_sub` 换成 `-`(debug 下 panic,release 下回绕)。
    #[test]
    fn a_pane_narrower_than_the_padding_degrades_instead_of_underflowing() {
        let tiny = PxRect {
            x: 0,
            y: 0,
            w: 6,
            h: 40,
        };
        let g = layout_geometry(&Node::Leaf(PaneId(1)), tiny, (8.0, 16.0), false, 1.0);
        assert_eq!(g[0].term_px.w, 0);
        assert!(g[0].grid.0 >= 1 && g[0].grid.1 >= 1, "grid 没被夹到至少 1×1");
    }

    /// 缩进与分隔线让位(`GAP_PX`)叠加:左右分屏时,左边那块**既**要让 1px
    /// 给分隔线,**又**要内缩 —— 两者都扣,不是二选一。
    ///
    /// 自证会变红:把缩进写成 `w - 2*pad` 覆盖掉 GAP 那一步(左块宽度会多 1)。
    #[test]
    fn padding_and_the_divider_gap_both_apply_to_the_left_pane() {
        let tree = Node::Split {
            dir: mullion_core::layout::Dir::Vertical,
            ratio: 0.5,
            a: Box::new(Node::Leaf(PaneId(1))),
            b: Box::new(Node::Leaf(PaneId(2))),
        };
        let g = layout_geometry(&tree, AREA, (8.0, 16.0), false, 1.0);
        let pad = term_pad_px(1.0);
        let left = g.iter().find(|p| p.id == PaneId(1)).expect("左块");
        let right = g.iter().find(|p| p.id == PaneId(2)).expect("右块");
        assert_eq!(left.term_px.w, left.px.w - GAP_PX - 2 * pad);
        assert_eq!(right.term_px.w, right.px.w - 2 * pad, "最右块不让分隔线");
    }
```

> `AREA` 是这个 `mod tests` 里既有的常量;`Node::Split { dir, ratio, a, b }` 的
> 字段名已核对(`mullion-core/src/layout.rs:45`)。

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-app geom 2>&1 | tail -20
```
Expected: 编译失败,`cannot find function 'term_pad_px'`。

- [ ] **Step 3: 实现**

在 `geom.rs` 的 `GAP_PX` 之后加:

```rust
/// 终端网格区左右各内缩多少,**逻辑点**(F80)。
///
/// 8 点 = 标题条内边距(`shrink2(8, 4)`)的横向值,标题文字与终端首列因此落在
/// 同一条竖线上。纵向**不缩** —— 再吃半行高度不值。
pub const TERM_PAD_PT: f32 = 8.0;

/// 当前 DPI 下的左右缩进,物理像素。非有限/非正的 `ppp` 落回 1.0,
/// 理由同 [`title_bar_px`]。
pub fn term_pad_px(ppp: f32) -> u32 {
    let ppp = if ppp.is_finite() && ppp > 0.0 {
        ppp
    } else {
        1.0
    };
    (TERM_PAD_PT * ppp).round() as u32
}
```

在 `layout_geometry` 的 `.map(|(id, r)| { .. })` 里,把 `term_px` 那段改成:

```rust
            let pad = term_pad_px(ppp);
            let term_w = px
                .w
                .saturating_sub(if at_right { 0 } else { GAP_PX })
                .saturating_sub(2 * pad);
            let term_px = PxRect {
                // 左右各内缩 pad(F80)。`w` 已经扣过分隔线让位,两者叠加而
                // 不是二选一。极窄 pane 上 `saturating_sub` 让它退化到 0 ——
                // `grid_size_for` 仍会夹到至少 (1, 1),PTY 侧收不到 0 列。
                x: px.x + pad,
                y: px.y + title_h,
                w: term_w,
                h: px
                    .h
                    .saturating_sub(title_h)
                    .saturating_sub(if at_bottom { 0 } else { GAP_PX }),
            };
```

`mod.rs` 的 re-export 那行补上新符号:

```rust
pub use geom::{
    layout_geometry, term_pad_px, title_bar_px, PaneGeom, PxRect, GAP_PX, TERM_PAD_PT, TITLE_BAR_PT,
};
```

> 既有测试里凡是断言 `term_px.x == px.x` 或 `term_px.w == ...` 的,都要按新语义
> 更新(`grep -n "term_px" crates/mullion-app/src/shell/workspace/geom.rs` 一次找齐)。
> **更新的是期望值,不是删断言。**

- [ ] **Step 4: 跑测试确认它绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
```
Expected: 全部 `test result: ok.`,clippy 无输出。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/shell/workspace/geom.rs crates/mullion-app/src/shell/workspace/mod.rs
git commit -m "feat(app): 终端区左右各内缩 8 点 (F80)"
```

---

### Task 11: 文档 + spec

**Files:**
- Modify: `docs/remote-state-setup.md`
- Modify: `spec.md`

- [ ] **Step 1: 改 `spec.md`**

在 F123 那一行之后插入 F124(同一张表,格式照抄相邻行的四列结构):

```markdown
| F124 | **远端状态自举**(编号接在 F123 之后):连上之后开一条旁路 exec channel 跑 `tmux set -g set-titles on && tmux set -g set-titles-string '#S:#I:#W #{pane_current_path}'`,让 F123 在**没配过的远端上**也能拿到数据。tmux 的 `set-titles` 是**服务器全局选项**,与 pane 无关,所以旁路 exec 不受 adr-009「分不清是哪块分屏」的限制(那正是 F51 被否的理由之一)。改的是 tmux 服务器**内存里**的选项,不写任何文件,server 退出即失效。**默认开**,设置弹窗可关。tmux 服务器还没起时退出码 1(实测**不会**顺手拉起一个空 server),每 30 秒重试到成功为止,成功后永不再试 | P2 | 命令串(`&&` 串联 + 单引号包住 format string + 带 `#{pane_current_path}`)有纯函数单测;`should_attempt` 的五条分支(开关关 / 已成功 / 在途 / 从没试过 / 未到重试间隔)有纯函数单测;`BootstrapFlags` 只在成功时 latch `done`(失败也 latch 的话「tmux 还没起」会让这条连接永不重试)有单测;`clone` 出来共享同一份状态有单测;tick 挂在 `about_to_wait` 上、走共享判据/共享命令串/写回结论,有源码级守护;**对本机真 tmux 3.7b 跑的 live 测试**(`--ignored`)断言 server 不在时失败且不拉起空 server、有 server 时两个选项确实被改。覆写用户的全局 `set-titles-string` 是刻意的,理由与影响面见 `docs/remote-state-setup.md` |
```

在 F123 那一行的说明末尾追加一句:

```
**自 2026-08-18 起由 F124 自动配置远端**,「远端不配就静默降级」因此只发生在关掉 F124 开关、或远端没有 tmux 的情况下。
```

- [ ] **Step 2: 改 `docs/remote-state-setup.md`**

把「## 远端要怎么配」整节改写:开头说明**默认自动配置**(F124),把原来的手工配置
片段降级成「关掉自举之后怎么自己配」。新增一节写清楚:

- 自举跑的是哪条命令、什么时候跑(连上立刻 + 每 30 秒重试到成功)、成功判据是退出码。
- **影响面**:覆写的是 tmux 服务器的全局 `set-titles-string`,同一台机器上其它
  终端 attach 同一个 server 时窗口标题也会变成这个格式;改的是内存里的选项,
  不写 `~/.tmux.conf`,server 退出即失效。
- 怎么关(设置 → 远端 → 取消勾选)。
- 已知时延:`cd` 之后 tmux 要等下一次客户端重绘才重发标题,实测是下一次提示符刷新。

「## 降级行为」那张表加一行「开着 F124(默认)」→「`会话名 · 目录名`」→「该目录」。

「## 相关代码」补:
```
- 自举:`crates/mullion-app/src/remote_bootstrap.rs`(命令串 + 重试判据)+
  `App::tick_tmux_bootstrap`(`about_to_wait` 里跑)
- `~` 展开:`crates/mullion-app/src/app.rs` 的 `expand_tilde` / `files_start_dir`
```

「## 文件面板已经开着时,不会跟着终端 cd 跑」那节保留不动。

- [ ] **Step 3: 确认没有说法自相矛盾**

```bash
grep -n "手配\|必须自己配\|远端不配" docs/remote-state-setup.md
```
Expected: 剩下的每一处都在「关掉自举之后」的语境里。

- [ ] **Step 4: 提交**

```bash
git add spec.md docs/remote-state-setup.md
git commit -m "docs: F124 远端状态自举写进 spec 与运行手册"
```

---

### Task 12: 交付(版本 / 绿 / 交叉编译 / Release)

**Files:**
- Modify: `Cargo.toml`(`workspace.package.version`)

- [ ] **Step 1: 升 patch 版本**

把 `Cargo.toml` 的 `workspace.package.version` 第三位 +1(当前 `0.1.49` → `0.1.50`)。

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: 版本 0.1.50(自动配置远端 tmux 状态上报,标题条改成一整串,终端区加缩进)"
```

- [ ] **Step 2: 跑绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo fmt --check
cargo test -p mullion-app --test tmux_bootstrap_live -- --ignored 2>&1 | grep -E "test result"
```
Expected: 全部 `ok.`;clippy 与 fmt 无输出;live 测试 `1 passed`。

**不绿不发。**

- [ ] **Step 3: 交叉编译 + objdump 验收**

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app
```

按 `docs/cross-compile-windows.md` 做依赖验收:出现 `libgcc_s_seh-1.dll` 或
`libwinpthread-1.dll` 即为**不合格**,必须修。

- [ ] **Step 4: push 之后再发 Release**

**先 push 再发版** —— `gh release create` 会把 tag 建在远端当前 HEAD 上,
不先 push 就会把 tag 建在旧提交上。

```bash
git push
sha256sum mullion.exe > mullion.exe.sha256
HTTPS_PROXY=http://127.0.0.1:7890 gh release create v0.1.50 \
  mullion.exe mullion.exe.sha256 -t "v0.1.50" -F notes.md --repo kilobitcy/Mullion
```

`notes.md` 里写:修了什么 + sha256 + 首次运行提示(`Unblock-File .\mullion.exe`)
+ **人工验收清单**:

1. 连上一台**没配过 tmux set-titles** 的机器,在终端里 `tmux new -s demo` —— 标题条
   右半应在几秒内出现 `demo`。
2. 在 tmux 里 `cd` 到一个深目录,再敲一次回车 —— 标题条应变成
   `N · 节点名 · 目录名 · demo`。(`cd` 当下不变、下一次提示符刷新才变,是 tmux
   自己的采样时机,预期如此。)
3. `tmux detach` 回到裸 shell —— tmux 名应消失,目录名仍在。
4. `Ctrl+Shift+B` 打开文件面板,远端栏应直接落在终端所在的那个深目录。
5. 关掉「设置 → 远端 → 自动配置远端 tmux 的状态上报」,重连一台干净的机器,
   在远端跑 `tmux show -g set-titles` —— 应仍是 `off`。
6. 终端区左右是否明显不再顶着边界,以及缩进后每行的列数是否还够用。
7. 窄 pane(分屏成 4 块)下标题条截断位置是否合理,`⇆` / `✕` 两个按钮没被顶出去。
8. 远端**没装 tmux** 的机器上:不应有任何错误提示,标题条正常显示
   `N · 节点名`(裸 shell 有标题时还会带目录名)。

- [ ] **Step 5: 报给用户**

Release 链接 + sha256 + 上面那份验收清单。

---

## 自查

**spec 覆盖**:① 自举 → Task 1–6、11;② `~` 展开 → Task 7–8;③ 标题条 → Task 9;
④ 缩进 → Task 10;错误处理(静默 + 只记 debug 日志)→ Task 4 Step 3;文档 → Task 11;
交付 → Task 12。设计里「exec 有超时包裹」这一条**未单独立任务**:`exec()` 本身没有
超时,但自举失败只是不配上、`busy` 由 `finish` 解锁,挂住的最坏后果是这条连接不再
重试,不会卡 UI、不会泄漏(task 随 runtime 收口)。**这是刻意的降级,写在 Task 4 的
函数文档里即可,不额外引入超时包裹。**

**类型一致性**:`bootstrap_command()` / `bootstrap_command_with(&str)` / `RETRY_MS` /
`TMUX_TITLES_STRING` / `should_attempt(bool, bool, bool, Option<u64>)` /
`BootstrapFlags::{is_done, is_busy, mark_busy, finish}` 在 Task 2 定义,Task 3–6 与
Task 11 引用的都是这组名字。`expand_tilde(&[u8], &[u8]) -> Option<Vec<u8>>`、
`files_start_dir(Option<&[u8]>, Option<&str>, Option<&[u8]>)`、
`sync_target_of(Option<u64>, bool, Option<&[u8]>, Option<&[u8]>)` 在 Task 7 定义,
Task 8 引用一致。`title_text(usize, Option<&str>, Option<&str>, Option<&str>, PaneStatus)`
在 Task 9 定义并在同一任务里改完所有调用点。`term_pad_px(f32) -> u32` /
`TERM_PAD_PT` 在 Task 10 定义并 re-export。
