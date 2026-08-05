# 切片 P1-a 实现计划：登录后自动化的数据层 + 调度

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 `mullion-store` 里做出「登录后自动化」的可继承数据模型与生成待发字节时间表的纯函数，在 `mullion-ssh` 里做出按时间表定时写入的通用 async 函数——**零 GUI，全部可无窗口单测**。

**Architecture:** 数据与「发什么」是纯函数（store，零 IO 零 async）；「什么时候发」是 async 调度（ssh，只认 `Vec<(Duration, Vec<u8>)>`，不认识 tmux/自动化语义）。两者靠 app 在 P1-b 里接起来，本切片不碰 app。

**Tech Stack:** Rust 2021 / serde + toml / tokio（新增 `time` feature）/ 纯函数单测 + `tokio::time::pause()` 假时钟。

**上游文档：**
- 设计：`docs/superpowers/specs/2026-08-05-slice-p1-login-automation-design.md`（含 §11 复核修订，**以 §11 为准**）
- 需求：`spec.md` 4.9（F40~F44）

---

## 已经替你验证过的两件事（别再怀疑，直接用）

**1. toml 能吃下这个形状，不需要退到扁平编码。** 路线图 §4.3「风险 1」担心的是
「内部标签枚举嵌套在 table 内」。已用一次性探测跑过：`#[serde(tag = "kind")]` 的
`TmuxChoice` 嵌在 `[session.automation.tmux]` 里、`Vec<AutomationCommand>` 变成
`[[session.automation.commands]]`，序列化与反序列化 round-trip 完全相等。

也不需要为「scalar 必须排在 table 前面」调整字段顺序——当前 toml 版本的序列化器
自己会把标量提到表之前。字段顺序按可读性排即可。

**2. `tokio` 当前没有 `time` feature，`sleep` / `pause()` 编译不过。** Task 9
第一步就是加，别等编译报错才发现。

---

## 文件结构

| 文件 | 职责 | 动作 |
|---|---|---|
| `crates/mullion-store/src/automation.rs` | 自动化的数据类型 + 全部纯函数（`build_plan` / `shell_quote` / `sanitize_tmux_name`） | **新建** |
| `crates/mullion-store/src/model.rs` | `SessionRecord` 加 `automation` 字段；`CURRENT_SCHEMA` 升 4 | 修改 |
| `crates/mullion-store/src/group.rs` | `GroupRecord` 加 `automation` 字段 | 修改 |
| `crates/mullion-store/src/inherit.rs` | `PrefsLayer` 加第五个方法；`ResolvedConfig` 加字段；`resolve` 补解析 | 修改 |
| `crates/mullion-store/src/vault.rs` | `SessionDraft` 加字段；`add`/`update` 落盘；v3 升级测试 | 修改 |
| `crates/mullion-store/src/migrate.rs` | 版本号断言更新（**不新增迁移函数**，理由见 Task 9） | 修改 |
| `crates/mullion-store/src/lib.rs` | 导出新模块与类型 | 修改 |
| `crates/mullion-ssh/src/schedule.rs` | `ByteSink` + `write_scheduled` + `impl ByteSink for SshSession` | **新建** |
| `crates/mullion-ssh/src/lib.rs` | 挂上 `schedule` 模块 | 修改 |
| `Cargo.toml`（workspace） | tokio 加 `time` feature | 修改 |
| `crates/mullion-ssh/Cargo.toml` | dev-dep 加 `test-util`（`pause()` 需要） | 修改 |

**任务顺序即依赖顺序**：Task 1–5 只碰新文件（互不冲突）；Task 6–8 改既有结构体（会牵动一批测试构造点）；Task 9 独立于 store，可与 1–8 并行。

---

## Task 1: 自动化数据类型 + toml round-trip

**Files:**
- Create: `crates/mullion-store/src/automation.rs`
- Modify: `crates/mullion-store/src/lib.rs`

- [ ] **Step 1: 新建 `automation.rs`，只写类型**

```rust
//! F40~F44 登录后自动化的数据模型与纯函数。零 IO、零 async。
//!
//! 核心约束(设计 §2):**自动化只在「确定还是干净 shell」的窗口期内发字节**。
//! 每次 SSH 连接都会拿到 sshd fork 的新 login shell,所以第一个字节安全;危险的是
//! 第二个——我们自己发的 `tmux attach` 一旦生效,屏幕就归那个正在跑的 TUI 了,
//! 此后再发命令行,字符会进它的输入框。所以生成的时间表**能一步就绝不两步**。

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// 登录后自动化(可继承分节)。字段一律 `Option`,`None` 即继承上游。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationPrefs {
    /// F44 总开关。`None` = 继承;内置默认见 [`DEFAULT_AUTOMATION_ENABLED`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// F40 tmux。`None` = 继承上游;`Some(Off)` = 显式不用 tmux。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux: Option<TmuxChoice>,
    /// F41 登录后命令列表。继承策略 **Override**(整体覆盖,绝不拼接)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<Vec<AutomationCommand>>,
    /// F42 初始工作目录。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_dir: Option<String>,
    /// F43 环境变量。继承策略 **Override**。
    ///
    /// **明文存 sessions.toml,不进 secrets.enc**(设计 §6):值终归要以 `export`
    /// 行发进远端,会落进 shell 历史与 `/proc/<pid>/environ`,加密只会给用户
    /// 错误的安全承诺。这里不是存密码的地方。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<Vec<EnvVar>>,
    /// 收到首个 PTY 字节后再等多久才发第一步。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_delay_ms: Option<u32>,
    /// 拆多步时的行间默认延时(仅无 tmux 且用户配了逐条延时的分支用得上)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inter_delay_ms: Option<u32>,
    /// 从 `open_pty` 返回起算,多久没收到任何字节就判定登录失败、跳过自动化。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_timeout_ms: Option<u32>,
}

/// F40 tmux 选择。
///
/// `Off` 是**显式**不用 tmux,用来覆盖分组的 tmux 设置——与 `ProxyChoice::Direct`
/// 是同一个坑:「继承」(`None`)与「显式关闭」(`Some(Off)`)必须可区分。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TmuxChoice {
    Off,
    Attach {
        /// `None` = 由会话名称推导(见 [`sanitize_tmux_name`])。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_name: Option<String>,
    },
}

/// 一条登录后命令。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationCommand {
    pub text: String,
    /// 设了才会让整个计划拆成多步(见 `build_plan`)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<u32>,
}

/// 一个环境变量。值是明文,见 [`AutomationPrefs::env`] 的说明。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvVar {
    pub key: String,
    pub value: String,
}
```

- [ ] **Step 2: 挂上模块并导出**

在 `crates/mullion-store/src/lib.rs` 的 `pub mod` 列表里(按字母序,`crypto` 之前)加一行：

```rust
pub mod automation;
```

并在 `pub use` 区加一行（放在 `pub use error::StoreError;` 之前）：

```rust
pub use automation::{AutomationCommand, AutomationPrefs, EnvVar, TmuxChoice};
```

- [ ] **Step 3: 写 round-trip 与「Off ≠ 继承」的失败测试**

在 `automation.rs` 末尾追加：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> AutomationPrefs {
        AutomationPrefs {
            enabled: Some(true),
            tmux: Some(TmuxChoice::Attach {
                session_name: Some("dev".into()),
            }),
            commands: Some(vec![
                AutomationCommand {
                    text: "echo 'hi'".into(),
                    delay_ms: None,
                },
                AutomationCommand {
                    text: "ls".into(),
                    delay_ms: Some(500),
                },
            ]),
            work_dir: Some("/srv".into()),
            env: Some(vec![EnvVar {
                key: "RUST_LOG".into(),
                value: "debug".into(),
            }]),
            initial_delay_ms: Some(300),
            inter_delay_ms: Some(200),
            ready_timeout_ms: Some(15_000),
        }
    }

    #[test]
    fn automation_prefs_round_trips() {
        let a = full();
        let s = toml::to_string_pretty(&a).unwrap();
        let back: AutomationPrefs = toml::from_str(&s).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn unset_fields_are_not_written() {
        let s = toml::to_string_pretty(&AutomationPrefs::default()).unwrap();
        assert_eq!(s.trim(), "", "全未设的分节不应写出任何键");
    }

    /// 与 `ProxyChoice::Direct` 同款:`None` = 继承上游,`Some(Off)` = 显式关闭。
    /// 折叠成同一个值会让「分组配了 tmux、这条会话就是不想要」永远表达不出来。
    #[test]
    fn tmux_off_is_distinguishable_from_inherit() {
        let inherit = AutomationPrefs::default();
        let explicit = AutomationPrefs {
            tmux: Some(TmuxChoice::Off),
            ..Default::default()
        };
        assert_ne!(inherit, explicit, "「继承」与「显式关闭 tmux」不是同一个值");

        let s = toml::to_string_pretty(&explicit).unwrap();
        let back: AutomationPrefs = toml::from_str(&s).unwrap();
        assert_eq!(back, explicit, "显式 Off 必须能 round-trip,不能被写没");
    }

    #[test]
    fn tmux_kind_is_tagged_not_positional() {
        let s = toml::to_string_pretty(&AutomationPrefs {
            tmux: Some(TmuxChoice::Attach { session_name: None }),
            ..Default::default()
        })
        .unwrap();
        assert!(s.contains(r#"kind = "attach""#), "应有 kind 标签: {s}");
        assert!(!s.contains("session_name"), "None 的 session_name 不应写出: {s}");
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-store automation:: 2>&1 | tail -20
```

预期：4 个测试全过。（这批是 round-trip 保护网，不是 TDD 的红→绿——类型和测试
一起写才有意义。后面 Task 2–5 的纯函数一律先红后绿。）

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-store/src/automation.rs crates/mullion-store/src/lib.rs
git commit -m "feat(store): 登录后自动化的数据模型 (F40~F44)

TmuxChoice::Off 与 None 可区分(同 ProxyChoice::Direct 的坑);
toml round-trip 已验证内部标签枚举嵌套在 table 内没问题,
不需要路线图 §4.3 风险 1 说的扁平编码退路。"
```

---

## Task 2: `sanitize_tmux_name`

**Files:**
- Modify: `crates/mullion-store/src/automation.rs`

- [ ] **Step 1: 写失败测试**

在 `automation.rs` 的 `mod tests` 里追加：

```rust
    #[test]
    fn sanitize_tmux_name_replaces_dot_and_colon() {
        // tmux 用 `.` 和 `:` 做 window/pane 定址,会话名里出现就会被解析成别的东西。
        assert_eq!(sanitize_tmux_name("web01.prod:2"), "web01-prod-2");
    }

    #[test]
    fn sanitize_tmux_name_strips_control_chars() {
        // 控制字符会破坏「整个计划只有一行」这个不变量:一个 \r 就是一次额外回车。
        assert_eq!(sanitize_tmux_name("a\rb\nc"), "abc");
    }

    #[test]
    fn sanitize_tmux_name_trims_but_keeps_inner_spaces_and_cjk() {
        assert_eq!(sanitize_tmux_name("  生产 web  "), "生产 web");
    }

    #[test]
    fn sanitize_tmux_name_of_blank_is_empty() {
        assert_eq!(sanitize_tmux_name("   "), "");
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-store sanitize_tmux 2>&1 | tail -20
```

预期：编译失败，`cannot find function 'sanitize_tmux_name' in this scope`。

- [ ] **Step 3: 实现**

在 `automation.rs` 的类型定义之后、`mod tests` 之前加：

```rust
/// 把任意字符串改造成合法的 tmux 会话名。
///
/// tmux 用 `.` 与 `:` 做 `session:window.pane` 定址,名字里带上它们会被解析成
/// 别的目标;控制字符则会破坏「整个计划只有一行」这个不变量(一个 `\r` 就是一次
/// 额外回车)。两者都在这里处理掉。
///
/// 返回空串表示**没有可用的名字**,调用方应据此放弃生成计划而不是发一条残命令。
pub fn sanitize_tmux_name(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| if c == '.' || c == ':' { '-' } else { c })
        .collect()
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-store sanitize_tmux 2>&1 | tail -20
```

预期：4 passed。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-store/src/automation.rs
git commit -m "feat(store): tmux 会话名 sanitize —— 去掉 . : 与控制字符 (F40)"
```

---

## Task 3: `shell_quote`

**Files:**
- Modify: `crates/mullion-store/src/automation.rs`

- [ ] **Step 1: 写失败测试**

在 `mod tests` 里追加：

```rust
    #[test]
    fn shell_quote_wraps_in_single_quotes() {
        assert_eq!(shell_quote("/srv/app"), "'/srv/app'");
    }

    #[test]
    fn shell_quote_escapes_single_quote() {
        // 经典的 '\'' 收尾-转义-重开手法。漏了它,一个引号就能越出参数边界。
        assert_eq!(shell_quote("it's"), r#"'it'\''s'"#);
    }

    #[test]
    fn shell_quote_neutralizes_command_substitution() {
        // 单引号内一切都是字面量,`$(...)` / 反引号 / `;` 都不该被解释。
        let q = shell_quote("$(rm -rf /); `id`");
        assert_eq!(q, r#"'$(rm -rf /); `id`'"#);
    }

    #[test]
    fn shell_quote_of_empty_is_empty_quotes() {
        assert_eq!(shell_quote(""), "''");
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-store shell_quote 2>&1 | tail -20
```

预期：编译失败，`cannot find function 'shell_quote'`。

- [ ] **Step 3: 实现**

紧跟 `sanitize_tmux_name` 之后加：

```rust
/// 用单引号把任意字符串包成一个 shell 参数。
///
/// 单引号内没有任何转义与展开,所以这是唯一不需要维护「危险字符清单」的做法——
/// 唯一要处理的是单引号自己:`'` → `'\''`(收尾、转义、重开)。
///
/// **谁该走这里、谁不该,是本模块最容易搞错的地方**,见 `build_plan` 的
/// 「引号层数」说明:会话名 / 工作目录 / env 的**值**各自 quote 一次;
/// 用户命令文本**原样拼接、绝不 quote**(它本来就是 shell 语法)。
pub fn shell_quote(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', r"'\''"))
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-store shell_quote 2>&1 | tail -20
```

预期：4 passed。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-store/src/automation.rs
git commit -m "feat(store): shell 参数单引号转义 (F40/F42/F43)"
```

---

## Task 4: `ResolvedAutomation` + `build_plan` 的 tmux 分支

这是本切片的核心。设计 §2 的规则在这里变成代码：**有 tmux 时恰好一个 Step**。

**Files:**
- Modify: `crates/mullion-store/src/automation.rs`

- [ ] **Step 1: 写失败测试**

在 `mod tests` 里追加：

```rust
    fn resolved(tmux: Option<TmuxChoice>) -> ResolvedAutomation {
        ResolvedAutomation {
            enabled: true,
            tmux,
            commands: Vec::new(),
            work_dir: None,
            env: Vec::new(),
            initial_delay_ms: 300,
            inter_delay_ms: 200,
            ready_timeout_ms: 15_000,
        }
    }

    fn text_of(step: &Step) -> String {
        String::from_utf8(step.bytes.clone()).unwrap()
    }

    /// 设计 §2 的核心不变量:有 tmux 时**只发一步**,由远端自己原子判断走
    /// attach 还是 new-session。发第二步就意味着可能打进已经 attach 上的 TUI。
    #[test]
    fn build_plan_tmux_branch_is_single_atomic_line() {
        let a = resolved(Some(TmuxChoice::Attach { session_name: None }));
        let plan = build_plan(&a, "web01");

        assert_eq!(plan.len(), 1, "有 tmux 必须恰好一步,多一步就是设计错误");
        let line = text_of(&plan[0]);
        assert!(
            line.contains("tmux has-session -t 'web01' 2>/dev/null"),
            "应先探测会话是否存在: {line}"
        );
        assert!(
            line.contains("&& exec tmux attach -t 'web01'"),
            "存在则 attach: {line}"
        );
        assert!(
            line.contains("|| exec tmux new-session -s 'web01'"),
            "不存在则新建: {line}"
        );
        assert!(
            !line.contains("new-session -A"),
            "不许用 -A:它无法区分新建与附着,命令与工作目录就没有安全落点: {line}"
        );
        assert_eq!(plan[0].delay, Duration::from_millis(300));
    }

    #[test]
    fn build_plan_terminates_line_with_cr_only() {
        let a = resolved(Some(TmuxChoice::Attach { session_name: None }));
        let plan = build_plan(&a, "web01");
        let bytes = &plan[0].bytes;
        assert_eq!(bytes.last(), Some(&b'\r'), "行终止符是 \\r(复用 keymap 的 Enter 约定)");
        assert_eq!(
            bytes.iter().filter(|b| **b == b'\r' || **b == b'\n').count(),
            1,
            "整个计划只能有一次回车"
        );
    }

    #[test]
    fn build_plan_uses_explicit_session_name_over_fallback() {
        let a = resolved(Some(TmuxChoice::Attach {
            session_name: Some("claude".into()),
        }));
        let line = text_of(&build_plan(&a, "web01")[0]);
        assert!(line.contains("-t 'claude'"), "显式名应胜出: {line}");
        assert!(!line.contains("web01"), "不应再出现回退名: {line}");
    }

    #[test]
    fn build_plan_sanitizes_fallback_name() {
        let a = resolved(Some(TmuxChoice::Attach { session_name: None }));
        let line = text_of(&build_plan(&a, "web01.prod:2")[0]);
        assert!(line.contains("-t 'web01-prod-2'"), "回退名也要 sanitize: {line}");
    }

    /// 名字 sanitize 后为空 → 无法构造安全命令 → 什么都不发。
    /// 绝不能退化成 `tmux attach -t ''` 那种残命令。
    #[test]
    fn build_plan_is_empty_when_name_sanitizes_to_nothing() {
        let a = resolved(Some(TmuxChoice::Attach {
            session_name: Some("...".into()),
        }));
        assert!(build_plan(&a, "   ").is_empty() || {
            let a2 = resolved(Some(TmuxChoice::Attach { session_name: None }));
            build_plan(&a2, "   ").is_empty()
        });
        let a2 = resolved(Some(TmuxChoice::Attach { session_name: None }));
        assert!(build_plan(&a2, "  ").is_empty(), "回退名全空白时不得生成计划");
    }

    /// 设计 §3「空启动命令的边界」:没命令没 env 时不许生成 `new-session -s X ''`,
    /// 那个空参数会让 tmux 去跑一个空命令,行为随版本而异。
    #[test]
    fn empty_start_command_omits_quoted_arg() {
        let a = resolved(Some(TmuxChoice::Attach { session_name: None }));
        let line = text_of(&build_plan(&a, "web01")[0]);
        assert!(!line.contains("''"), "不应出现空的引号参数: {line}");
        assert!(!line.contains("exec $SHELL"), "没东西可跑就不该套 exec $SHELL: {line}");
    }

    #[test]
    fn work_dir_becomes_new_session_c_flag_only() {
        let mut a = resolved(Some(TmuxChoice::Attach { session_name: None }));
        a.work_dir = Some("/srv/app".into());
        let line = text_of(&build_plan(&a, "web01")[0]);
        assert!(line.contains("new-session -s 'web01' -c '/srv/app'"), "{line}");
        // attach 分支绝不能带工作目录:附着已有会话时改它的目录是越权。
        let attach_part = line.split("||").next().unwrap();
        assert!(!attach_part.contains("/srv/app"), "attach 分支不得带 -c: {line}");
    }

    /// 设计 §3「引号层数」:命令文本原样拼接,**不再 quote**——
    /// 它本来就是 shell 语法,再包一层用户写的 `echo 'hi'` 直接炸。
    #[test]
    fn command_text_is_not_quoted_so_user_shell_syntax_survives() {
        let mut a = resolved(Some(TmuxChoice::Attach { session_name: None }));
        a.commands = vec![AutomationCommand {
            text: "echo 'hi'".into(),
            delay_ms: None,
        }];
        let line = text_of(&build_plan(&a, "web01")[0]);
        // 整串被最外层单引号包住,所以用户的 ' 会被转义成 '\'' —— 但语义上
        // 到了远端仍是原样的 echo 'hi',而不是被多包了一层。
        assert!(line.contains(r#"echo '\''hi'\''"#), "命令应原样嵌入最外层引号: {line}");
        assert!(line.contains("exec $SHELL"), "跑完命令要留住 shell: {line}");
    }

    #[test]
    fn env_becomes_export_statements_with_quoted_values() {
        let mut a = resolved(Some(TmuxChoice::Attach { session_name: None }));
        a.env = vec![EnvVar {
            key: "RUST_LOG".into(),
            value: "debug,h2=off".into(),
        }];
        let line = text_of(&build_plan(&a, "web01")[0]);
        assert!(line.contains(r#"export RUST_LOG='\''debug,h2=off'\''"#), "{line}");
    }

    /// 非法 key 会让整行 shell 语法崩掉(`export 2FOO=x` 是语法错误),
    /// 更糟的是 key 里塞 `;` 能越出赋值语句。直接丢弃,不生成。
    #[test]
    fn env_with_invalid_key_is_dropped() {
        let mut a = resolved(Some(TmuxChoice::Attach { session_name: None }));
        a.env = vec![
            EnvVar { key: "2BAD".into(), value: "x".into() },
            EnvVar { key: "A;rm -rf /".into(), value: "x".into() },
            EnvVar { key: "GOOD_1".into(), value: "y".into() },
        ];
        let line = text_of(&build_plan(&a, "web01")[0]);
        assert!(!line.contains("2BAD"), "非法 key 应被丢弃: {line}");
        assert!(!line.contains("rm -rf"), "带分号的 key 应被丢弃: {line}");
        assert!(line.contains("export GOOD_1="), "合法 key 应保留: {line}");
    }

    /// F44:关掉就是一个字节都不发。
    #[test]
    fn disabled_automation_builds_empty_plan() {
        let mut a = resolved(Some(TmuxChoice::Attach { session_name: None }));
        a.enabled = false;
        a.commands = vec![AutomationCommand { text: "ls".into(), delay_ms: None }];
        assert!(build_plan(&a, "web01").is_empty(), "F44 关闭时必须零步");
    }

    /// 含换行的命令会在一行模型里变成「额外的一次回车」,正是设计要挡的东西。
    /// UI 应在输入时就拒绝(F41),这里是防御性兜底。
    #[test]
    fn command_containing_newline_is_dropped() {
        let mut a = resolved(Some(TmuxChoice::Attach { session_name: None }));
        a.commands = vec![
            AutomationCommand { text: "ls\rrm -rf /".into(), delay_ms: None },
            AutomationCommand { text: "pwd".into(), delay_ms: None },
        ];
        let line = text_of(&build_plan(&a, "web01")[0]);
        assert!(!line.contains("rm -rf"), "含控制字符的命令应整条丢弃: {line}");
        assert!(line.contains("pwd"), "其余命令不受影响: {line}");
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-store automation:: 2>&1 | tail -20
```

预期：编译失败，`cannot find type 'ResolvedAutomation'` / `cannot find type 'Step'` /
`cannot find function 'build_plan'`。

- [ ] **Step 3: 实现类型与 tmux 分支**

在 `automation.rs` 里，`shell_quote` 之后追加：

```rust
/// 内置默认:自动化本身是开着的。没配任何东西时 `build_plan` 自然返回空计划,
/// 所以「默认开」不会凭空发字节;反过来默认关会让用户配好了却不生效。
pub const DEFAULT_AUTOMATION_ENABLED: bool = true;
/// 收到首字节后再等 300ms —— 给 MOTD / 提示符打印留出余量。
pub const DEFAULT_INITIAL_DELAY_MS: u32 = 300;
/// 拆多步时的行间默认延时。
pub const DEFAULT_INTER_DELAY_MS: u32 = 200;
/// 从 `open_pty` 返回起算的等待上限。超时即跳过,**绝不补发**。
pub const DEFAULT_READY_TIMEOUT_MS: u32 = 15_000;

/// 继承解析后的自动化配置。由 `inherit::resolve` 产出。
///
/// 与 `ResolvedConfig` 同理,**不要派生 `Default`**——那会把三个延时默认成 0,
/// 而不是上面那组内置默认。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAutomation {
    pub enabled: bool,
    /// `None` = 全链路都没配 tmux;`Some(Off)` = 显式关闭。对 `build_plan`
    /// 而言两者等价(都走无 tmux 分支),但配置层必须能区分,否则分组的 tmux
    /// 就永远覆盖不掉。
    pub tmux: Option<TmuxChoice>,
    pub commands: Vec<AutomationCommand>,
    pub work_dir: Option<String>,
    pub env: Vec<EnvVar>,
    pub initial_delay_ms: u32,
    pub inter_delay_ms: u32,
    pub ready_timeout_ms: u32,
}

/// 时间表里的一步:「等 `delay`,然后把 `bytes` 写进 PTY」。
///
/// 转成 `mullion-ssh` 的 `(Duration, Vec<u8>)` 由 **app 侧一行 map** 完成——
/// ssh 不该依赖 store(单向依赖),store 也不该认识调度。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub delay: Duration,
    pub bytes: Vec<u8>,
}

/// 生成待发时间表。**纯函数**:同样的输入永远给同样的字节。
///
/// # 引号层数(最容易搞错的地方)
///
/// 只做**一层**转义,各自负责:
/// - 会话名 / 工作目录 / env 的**值** → 各自 `shell_quote` 一次
/// - 用户命令文本 → **原样拼接,不 quote**(它本来就是 shell 语法)
/// - 有 tmux 分支最外层那对包住启动命令串的引号 → 对拼好的整串统一转义一次
///
/// # 两条分支
///
/// - **有 tmux**:恰好一个 Step(设计 §2 的核心不变量)
/// - **无 tmux**:默认也是一个 Step;只有用户显式配了逐条延时才拆多步(见
///   `build_no_tmux`)
pub fn build_plan(a: &ResolvedAutomation, fallback_name: &str) -> Vec<Step> {
    if !a.enabled {
        return Vec::new();
    }
    let initial = Duration::from_millis(u64::from(a.initial_delay_ms));
    match &a.tmux {
        Some(TmuxChoice::Attach { session_name }) => {
            let raw = session_name.as_deref().unwrap_or(fallback_name);
            let name = sanitize_tmux_name(raw);
            if name.is_empty() {
                // 没有可用的会话名。宁可什么都不做,也不发 `attach -t ''` 这种残命令。
                return Vec::new();
            }
            let q = shell_quote(&name);
            let mut cmd = format!(
                "tmux has-session -t {q} 2>/dev/null && exec tmux attach -t {q} \
                 || exec tmux new-session -s {q}"
            );
            if let Some(dir) = non_empty(a.work_dir.as_deref()) {
                // 只挂在 new-session 上:附着已有会话时改它的工作目录是越权。
                cmd.push_str(&format!(" -c {}", shell_quote(dir)));
            }
            let start = start_command(a);
            if !start.is_empty() {
                cmd.push(' ');
                cmd.push_str(&shell_quote(&start));
            }
            vec![Step {
                delay: initial,
                bytes: line(&cmd),
            }]
        }
        // `None`(没配)与 `Some(Off)`(显式关)在这里等价。
        _ => build_no_tmux(a, initial),
    }
}

/// tmux `new-session` 的启动命令串。为空表示「没东西要跑」,调用方应整段省略。
fn start_command(a: &ResolvedAutomation) -> String {
    let mut parts = export_stmts(a);
    parts.extend(command_texts(a));
    if parts.is_empty() {
        return String::new();
    }
    // 跑完命令要留住 shell,否则新建的 tmux 窗口会立刻退出。
    parts.push("exec $SHELL".to_string());
    parts.join("; ")
}

/// `export K='V'` 列表。非法 key 直接丢弃:`export 2FOO=x` 是语法错误,
/// 而 key 里塞 `;` 能越出赋值语句去执行别的东西。
fn export_stmts(a: &ResolvedAutomation) -> Vec<String> {
    a.env
        .iter()
        .filter(|e| is_valid_env_key(&e.key))
        .map(|e| format!("export {}={}", e.key, shell_quote(&e.value)))
        .collect()
}

/// 用户命令文本,**原样**。含控制字符的整条丢弃——一个 `\r` 在一行模型里
/// 就是一次额外回车,正是设计 §2 要挡的东西。UI 应在输入时就拒绝(F41),
/// 这里是防御性兜底。
fn command_texts(a: &ResolvedAutomation) -> Vec<String> {
    a.commands
        .iter()
        .filter(|c| !c.text.trim().is_empty())
        .filter(|c| !c.text.chars().any(char::is_control))
        .map(|c| c.text.trim().to_string())
        .collect()
}

fn is_valid_env_key(k: &str) -> bool {
    !k.is_empty()
        && k.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn non_empty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

/// 行终止符统一 `\r`,复用 `keymap.rs` 里 Enter 键的既有约定。
/// 待 F25(回车 CR/CRLF)落地后两条路径改走同一份配置——不允许「人手敲回车」
/// 与「自动化发送」用不同换行约定。
fn line(s: &str) -> Vec<u8> {
    let mut b = s.as_bytes().to_vec();
    b.push(b'\r');
    b
}
```

同时加一个**临时**的无 tmux 分支占位（Task 5 会替换掉它的实现，先让代码编译）：

```rust
fn build_no_tmux(_a: &ResolvedAutomation, _initial: Duration) -> Vec<Step> {
    Vec::new()
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-store automation:: 2>&1 | tail -25
```

预期：本任务新增的 12 个测试全过（Task 1–3 的 12 个也仍过）。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-store/src/automation.rs
git commit -m "feat(store): build_plan 的 tmux 分支 —— 单步原子命令 (F40/F42/F43)

设计 §2 的核心不变量落成断言:有 tmux 时恰好一个 Step。
has-session && exec attach || exec new-session,不用 -A(无法区分新建与附着)。
引号只做一层:名字/目录/env 值各 quote 一次,命令文本原样拼接。"
```

---

## Task 5: `build_plan` 的无 tmux 分支

设计 §11 修订 2 的落点：**默认也收成单步**，只有显式配了逐条延时才拆。

**Files:**
- Modify: `crates/mullion-store/src/automation.rs`

- [ ] **Step 1: 写失败测试**

在 `mod tests` 里追加：

```rust
    /// 设计 §11 修订 2:无 tmux 分支默认**也只有一步**。
    /// 用户的 .bashrc 里写一句自动 attach tmux 是极常见配置,无条件拆多步
    /// 会让第二条起打进 TUI —— 与 tmux 分支是同一个坑。
    #[test]
    fn build_plan_no_tmux_is_single_step_unless_per_command_delay() {
        let mut a = resolved(None);
        a.work_dir = Some("/srv".into());
        a.env = vec![EnvVar { key: "A".into(), value: "1".into() }];
        a.commands = vec![
            AutomationCommand { text: "ls".into(), delay_ms: None },
            AutomationCommand { text: "pwd".into(), delay_ms: None },
        ];

        let plan = build_plan(&a, "web01");
        assert_eq!(plan.len(), 1, "没有逐条延时就该合并成一行");
        let line = text_of(&plan[0]);
        assert_eq!(line, "export A='1'; cd '/srv'; ls; pwd\r");
        assert_eq!(plan[0].delay, Duration::from_millis(300));
    }

    /// 显式 Off 与「没配」在生成侧等价,都走无 tmux 分支。
    #[test]
    fn explicit_tmux_off_takes_the_no_tmux_branch() {
        let mut a = resolved(Some(TmuxChoice::Off));
        a.commands = vec![AutomationCommand { text: "ls".into(), delay_ms: None }];
        let plan = build_plan(&a, "web01");
        assert_eq!(plan.len(), 1);
        assert_eq!(text_of(&plan[0]), "ls\r");
        assert!(!text_of(&plan[0]).contains("tmux"), "显式 Off 不该生成 tmux 命令");
    }

    #[test]
    fn build_plan_no_tmux_orders_export_cd_then_commands() {
        let mut a = resolved(None);
        a.work_dir = Some("/srv".into());
        a.env = vec![EnvVar { key: "A".into(), value: "1".into() }];
        a.commands = vec![
            AutomationCommand { text: "ls".into(), delay_ms: Some(500) },
            AutomationCommand { text: "pwd".into(), delay_ms: None },
        ];

        let plan = build_plan(&a, "web01");
        assert_eq!(plan.len(), 4, "配了逐条延时就拆:export / cd / ls / pwd");
        assert_eq!(text_of(&plan[0]), "export A='1'\r");
        assert_eq!(text_of(&plan[1]), "cd '/srv'\r");
        assert_eq!(text_of(&plan[2]), "ls\r");
        assert_eq!(text_of(&plan[3]), "pwd\r");

        assert_eq!(plan[0].delay, Duration::from_millis(300), "首步用 initial_delay");
        assert_eq!(plan[1].delay, Duration::from_millis(200), "未设的用 inter_delay");
        assert_eq!(plan[2].delay, Duration::from_millis(500), "设了的用自己的");
        assert_eq!(plan[3].delay, Duration::from_millis(200));
    }

    #[test]
    fn build_plan_no_tmux_with_nothing_configured_is_empty() {
        assert!(build_plan(&resolved(None), "web01").is_empty(), "没配任何东西 → 零步");
    }

    #[test]
    fn no_tmux_branch_never_emits_exec_shell() {
        // exec $SHELL 只对 tmux new-session 的启动命令有意义;
        // 直接打进用户 shell 会把当前 shell 换掉,毫无必要。
        let mut a = resolved(None);
        a.commands = vec![AutomationCommand { text: "ls".into(), delay_ms: None }];
        assert!(!text_of(&build_plan(&a, "web01")[0]).contains("exec $SHELL"));
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-store automation:: 2>&1 | grep -E "^test |test result" | tail -25
```

预期：`build_plan_no_tmux_is_single_step_unless_per_command_delay` 等 5 个 FAILED
（占位实现返回空 vec，断言 `plan.len() == 1` 失败并 panic on index）。

- [ ] **Step 3: 用真实实现替换 Task 4 的占位**

把 `build_no_tmux` 整个替换成：

```rust
/// 无 tmux 分支。
///
/// **默认合并成一行**(设计 §11 修订 2):多发一步就多一次「屏幕已经不归我们了」
/// 的机会——用户的 `.bashrc` 里自动 attach tmux 是极常见配置。只有用户显式给
/// 某条命令配了 `delay_ms`,才说明他真的想要「等一下再发下一条」,这时才拆。
fn build_no_tmux(a: &ResolvedAutomation, initial: Duration) -> Vec<Step> {
    // (这一步自己的延时, 语句文本)。export / cd 没有自己的延时。
    let mut stmts: Vec<(Option<u32>, String)> =
        export_stmts(a).into_iter().map(|s| (None, s)).collect();
    if let Some(dir) = non_empty(a.work_dir.as_deref()) {
        stmts.push((None, format!("cd {}", shell_quote(dir))));
    }
    for c in a.commands.iter() {
        if c.text.trim().is_empty() || c.text.chars().any(char::is_control) {
            continue;
        }
        stmts.push((c.delay_ms, c.text.trim().to_string()));
    }
    if stmts.is_empty() {
        return Vec::new();
    }

    let wants_multi_step = a.commands.iter().any(|c| c.delay_ms.is_some());
    if !wants_multi_step {
        let joined = stmts
            .into_iter()
            .map(|(_, s)| s)
            .collect::<Vec<_>>()
            .join("; ");
        return vec![Step {
            delay: initial,
            bytes: line(&joined),
        }];
    }

    let inter = Duration::from_millis(u64::from(a.inter_delay_ms));
    stmts
        .into_iter()
        .enumerate()
        .map(|(i, (delay_ms, s))| Step {
            // 首步一律用 initial_delay(它等的是「shell 准备好」,与行间节奏无关),
            // 其后按各自的 delay_ms,没设的落 inter_delay。
            delay: if i == 0 {
                initial
            } else {
                delay_ms.map_or(inter, |ms| Duration::from_millis(u64::from(ms)))
            },
            bytes: line(&s),
        })
        .collect()
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-store automation:: 2>&1 | tail -10
```

预期：全部通过（约 29 个）。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-store/src/automation.rs
git commit -m "feat(store): build_plan 的无 tmux 分支 —— 默认也收成单步 (F41/F42/F43)

设计 §11 修订 2:.bashrc 自动 attach tmux 是常见配置,无条件多步
与 tmux 分支同坑。只有用户显式配了逐条延时才拆。"
```

---

## Task 6: 三个记录类型加 `automation` 字段

这一步会牵动一批既有测试的结构体字面量——**全部列在下面，不要漏**。

**Files:**
- Modify: `crates/mullion-store/src/model.rs`（`SessionRecord`）
- Modify: `crates/mullion-store/src/group.rs`（`GroupRecord`）
- Modify: `crates/mullion-store/src/vault.rs`（`SessionDraft` + 两个测试辅助函数）
- Modify: `crates/mullion-store/src/inherit.rs`（两个测试辅助函数）
- Modify: `crates/mullion-store/src/jump.rs`（两个测试辅助构造）
- Modify: `crates/mullion-store/src/migrate.rs`（`migrate_v1` 的构造）

- [ ] **Step 1: 给三个结构体加字段**

`model.rs` 的 `SessionRecord`，在 `network` 之后加：

```rust
    #[serde(default)]
    pub automation: crate::automation::AutomationPrefs,
```

`group.rs` 的 `GroupRecord`，在 `network` 之后加：

```rust
    #[serde(default)]
    pub automation: crate::automation::AutomationPrefs,
```

`vault.rs` 的 `SessionDraft`，在 `network` 之后加：

```rust
    pub automation: crate::automation::AutomationPrefs,
```

- [ ] **Step 2: 编译，让编译器列出所有缺字段的构造点**

```bash
cargo build -p mullion-store --all-targets 2>&1 | grep -E "^error|--> " | head -40
```

预期：一批 `missing field 'automation' in initializer`。

- [ ] **Step 3: 逐个补齐（下面是完整清单）**

在这些位置加 `automation: crate::automation::AutomationPrefs::default(),`
（在 `inherit.rs` / `vault.rs` 的测试里已 `use` 的语境下可写
`automation: Default::default(),`，跟随该文件既有风格即可）：

- `crates/mullion-store/src/model.rs:198` 与 `:237` — `mod tests` 里两处
  `SessionRecord { .. }`
- `crates/mullion-store/src/group.rs:31` — `mod tests` 的 `GroupRecord { .. }`
- `crates/mullion-store/src/inherit.rs:180` 与 `:218` — `session_with_network()` 与
  `group_with_network()` 两个辅助函数各一处（`session()` / `group()` 是它们的
  薄包装，不用动）
- `crates/mullion-store/src/jump.rs:131` 与 `:309` — `mod tests` 的 `rec()` 辅助函数
  与一处 `GroupRecord { .. }`（**容易漏，编译器会点名**）
- `crates/mullion-store/src/vault.rs:133`（`add()` 的 `SessionRecord` 构造）、
  `:147` 起的 `update()`（字段赋值）、`:215`（`add_group()` 的 `GroupRecord` 构造）；
  `mod tests` 的 `draft_pw():377` 与 `draft():739` 两个辅助函数
- `crates/mullion-store/src/migrate.rs:57` — `migrate_v1` 的 `.map(|r| SessionRecord { .. })`

`vault.rs` 的 `add()` 里补：

```rust
            automation: draft.automation,
```

`vault.rs` 的 `update()` 里，在 `rec.network = draft.network;` 之后补：

```rust
        rec.automation = draft.automation;
```

`vault.rs` 的 `add_group()` 里补：

```rust
            automation: crate::automation::AutomationPrefs::default(),
```

- [ ] **Step 4: 写「迁移不得凭空写入自动化」的测试**

`migrate.rs` 的 `migrated_prefs_are_all_unset_so_behavior_is_unchanged` 里追加一行断言：

```rust
        assert_eq!(
            s.automation,
            crate::automation::AutomationPrefs::default(),
            "迁移不得凭空写入自动化配置"
        );
```

- [ ] **Step 5: 跑全 crate 测试**

```bash
cargo test -p mullion-store 2>&1 | tail -10
```

预期：全过（既有测试 + 新增的 29 个）。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-store/src
git commit -m "feat(store): 会话/分组/草稿三处挂上 automation 分节 (F40~F44)"
```

---

## Task 7: 继承接线（`PrefsLayer` 第五个方法 + `ResolvedConfig`）

**Files:**
- Modify: `crates/mullion-store/src/inherit.rs`
- Modify: `crates/mullion-store/src/lib.rs`

- [ ] **Step 1: 写失败测试**

在 `inherit.rs` 的 `mod tests` 里追加（先在该 mod 顶部的 `use` 里加
`use crate::automation::{AutomationCommand, AutomationPrefs, ResolvedAutomation, TmuxChoice};`）：

```rust
    fn session_with_automation(a: AutomationPrefs) -> SessionRecord {
        let mut s = session(vec![], TerminalPrefs::default(), AppearancePrefs::default());
        s.automation = a;
        s
    }

    fn group_with_automation(a: AutomationPrefs) -> GroupRecord {
        let mut g = group(vec![], TerminalPrefs::default(), AppearancePrefs::default());
        g.automation = a;
        g
    }

    #[test]
    fn automation_none_inherits_group() {
        let s = session_with_automation(AutomationPrefs::default());
        let g = group_with_automation(AutomationPrefs {
            tmux: Some(TmuxChoice::Attach {
                session_name: Some("shared".into()),
            }),
            initial_delay_ms: Some(1234),
            ..Default::default()
        });
        let got = resolve(&[&s, &g]);
        assert_eq!(
            got.automation.tmux,
            Some(TmuxChoice::Attach {
                session_name: Some("shared".into())
            }),
            "会话未设时应取分组的"
        );
        assert_eq!(got.automation.initial_delay_ms, 1234);
    }

    /// 与 `explicit_direct_overrides_group_proxy_instead_of_inheriting` 同款:
    /// 显式 `Off` 必须**覆盖**分组的 tmux,而不是被当成「未设」继续继承。
    #[test]
    fn tmux_off_is_not_inherit() {
        let s = session_with_automation(AutomationPrefs {
            tmux: Some(TmuxChoice::Off),
            ..Default::default()
        });
        let g = group_with_automation(AutomationPrefs {
            tmux: Some(TmuxChoice::Attach {
                session_name: Some("shared".into()),
            }),
            ..Default::default()
        });
        let got = resolve(&[&s, &g]);
        assert_eq!(
            got.automation.tmux,
            Some(TmuxChoice::Off),
            "会话显式关闭 tmux 必须胜出,绝不能回落到分组的 attach"
        );
    }

    /// 命令列表是 Override,不是 Merge —— 拼接会产生「为什么多跑了一条命令」
    /// 这类极难排查的问题(路线图 §4.2)。
    #[test]
    fn commands_are_overridden_wholesale_never_concatenated() {
        let s = session_with_automation(AutomationPrefs {
            commands: Some(vec![AutomationCommand {
                text: "session-cmd".into(),
                delay_ms: None,
            }]),
            ..Default::default()
        });
        let g = group_with_automation(AutomationPrefs {
            commands: Some(vec![AutomationCommand {
                text: "group-cmd".into(),
                delay_ms: None,
            }]),
            ..Default::default()
        });
        let got = resolve(&[&s, &g]);
        assert_eq!(got.automation.commands.len(), 1, "不得拼接两层的命令");
        assert_eq!(got.automation.commands[0].text, "session-cmd");
    }

    /// 显式空列表同样是覆盖:分组配了命令,会话说「我什么都不跑」。
    #[test]
    fn explicit_empty_command_list_overrides_group() {
        let s = session_with_automation(AutomationPrefs {
            commands: Some(Vec::new()),
            ..Default::default()
        });
        let g = group_with_automation(AutomationPrefs {
            commands: Some(vec![AutomationCommand {
                text: "group-cmd".into(),
                delay_ms: None,
            }]),
            ..Default::default()
        });
        let got = resolve(&[&s, &g]);
        assert!(got.automation.commands.is_empty(), "会话显式空列表必须覆盖分组");
    }

    #[test]
    fn automation_falls_back_to_builtin_defaults() {
        let s = session_with_automation(AutomationPrefs::default());
        let got = resolve(&[&s]);
        assert!(got.automation.enabled, "默认开(没配东西时计划自然为空)");
        assert_eq!(got.automation.initial_delay_ms, crate::automation::DEFAULT_INITIAL_DELAY_MS);
        assert_eq!(got.automation.inter_delay_ms, crate::automation::DEFAULT_INTER_DELAY_MS);
        assert_eq!(got.automation.ready_timeout_ms, crate::automation::DEFAULT_READY_TIMEOUT_MS);
        assert!(got.automation.tmux.is_none());
        assert!(got.automation.commands.is_empty());
        assert!(got.automation.env.is_empty());
        assert!(got.automation.work_dir.is_none());
    }

    /// F44 关闭必须能从分组继承下来。
    #[test]
    fn group_can_disable_automation_for_whole_group() {
        let s = session_with_automation(AutomationPrefs::default());
        let g = group_with_automation(AutomationPrefs {
            enabled: Some(false),
            ..Default::default()
        });
        assert!(!resolve(&[&s, &g]).automation.enabled);
    }

    /// 解析结果直接喂给 build_plan,是 P1-b 的实际用法,这里钉住这条链路。
    #[test]
    fn resolved_automation_feeds_build_plan_end_to_end() {
        let s = session_with_automation(AutomationPrefs {
            tmux: Some(TmuxChoice::Attach { session_name: None }),
            ..Default::default()
        });
        let cfg = resolve(&[&s]);
        let plan = crate::automation::build_plan(&cfg.automation, "web01");
        assert_eq!(plan.len(), 1);
        assert!(String::from_utf8(plan[0].bytes.clone())
            .unwrap()
            .contains("tmux has-session -t 'web01'"));
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-store inherit:: 2>&1 | tail -20
```

预期：编译失败，`no method named 'automation' found` / `no field 'automation' on
type 'ResolvedConfig'`。

- [ ] **Step 3: 实现**

`inherit.rs` 顶部 `use` 加：

```rust
use crate::automation::{
    AutomationPrefs, ResolvedAutomation, DEFAULT_AUTOMATION_ENABLED, DEFAULT_INITIAL_DELAY_MS,
    DEFAULT_INTER_DELAY_MS, DEFAULT_READY_TIMEOUT_MS,
};
```

`PrefsLayer` trait 加第五个方法：

```rust
    fn automation(&self) -> &AutomationPrefs;
```

三个 impl 各加（`SessionRecord` / `GroupRecord` / `SessionDraft` 都是同名字段）：

```rust
    fn automation(&self) -> &AutomationPrefs {
        &self.automation
    }
```

`ResolvedConfig` 加字段（放在 `jump` 之后）：

```rust
    /// 解析后的登录后自动化(F40~F44)。
    pub automation: ResolvedAutomation,
```

`resolve()` 的返回值里加（放在 `jump` 之后）：

```rust
        automation: ResolvedAutomation {
            enabled: resolve_override(
                layers.iter().map(|l| l.automation().enabled),
                DEFAULT_AUTOMATION_ENABLED,
            ),
            // 与 icon/color/proxy 同款的 `.map(Some)` 技巧:让「本层未设」贡献
            // 0 个元素、继续看下一层,而「本层显式设为 Off / 空列表」贡献一个
            // Some(...) 从而整体覆盖上游。
            tmux: resolve_override(
                layers.iter().map(|l| l.automation().tmux.clone().map(Some)),
                None,
            ),
            commands: resolve_override(
                layers
                    .iter()
                    .map(|l| l.automation().commands.clone().map(Some)),
                None,
            )
            .unwrap_or_default(),
            work_dir: resolve_override(
                layers
                    .iter()
                    .map(|l| l.automation().work_dir.clone().map(Some)),
                None,
            ),
            env: resolve_override(
                layers.iter().map(|l| l.automation().env.clone().map(Some)),
                None,
            )
            .unwrap_or_default(),
            initial_delay_ms: resolve_override(
                layers.iter().map(|l| l.automation().initial_delay_ms),
                DEFAULT_INITIAL_DELAY_MS,
            ),
            inter_delay_ms: resolve_override(
                layers.iter().map(|l| l.automation().inter_delay_ms),
                DEFAULT_INTER_DELAY_MS,
            ),
            ready_timeout_ms: resolve_override(
                layers.iter().map(|l| l.automation().ready_timeout_ms),
                DEFAULT_READY_TIMEOUT_MS,
            ),
        },
```

`lib.rs` 的 `pub use automation::{...}` 补上新导出的类型：

```rust
pub use automation::{
    build_plan, AutomationCommand, AutomationPrefs, EnvVar, ResolvedAutomation, Step, TmuxChoice,
};
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-store 2>&1 | tail -10
```

预期：全过。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-store/src
git commit -m "feat(store): automation 接入继承链 (F40~F44)

PrefsLayer 加第五个方法;ResolvedConfig 加 automation。
显式 Off / 空命令列表走 Override 覆盖上游,与 ProxyChoice::Direct 同款。"
```

---

## Task 8: vault 落盘 round-trip

**Files:**
- Modify: `crates/mullion-store/src/vault.rs`

- [ ] **Step 1: 写失败测试**

在 `vault.rs` 的 `mod tests` 末尾追加：

```rust
    #[test]
    fn automation_survives_save_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let id;
        {
            let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            let mut d = draft();
            d.automation = crate::automation::AutomationPrefs {
                enabled: Some(true),
                tmux: Some(crate::automation::TmuxChoice::Attach {
                    session_name: Some("claude".into()),
                }),
                commands: Some(vec![crate::automation::AutomationCommand {
                    text: "echo 'hi'".into(),
                    delay_ms: Some(500),
                }]),
                work_dir: Some("/srv".into()),
                env: Some(vec![crate::automation::EnvVar {
                    key: "RUST_LOG".into(),
                    value: "debug".into(),
                }]),
                initial_delay_ms: Some(300),
                inter_delay_ms: None,
                ready_timeout_ms: None,
            };
            id = v.add(d, "2026-08-06T00:00:00Z");
            v.save().unwrap();
        }
        let v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let a = &v.get(id).unwrap().automation;
        assert_eq!(
            a.tmux,
            Some(crate::automation::TmuxChoice::Attach {
                session_name: Some("claude".into())
            })
        );
        assert_eq!(a.commands.as_ref().unwrap()[0].text, "echo 'hi'");
        assert_eq!(a.commands.as_ref().unwrap()[0].delay_ms, Some(500));
        assert_eq!(a.work_dir.as_deref(), Some("/srv"));
        assert_eq!(a.env.as_ref().unwrap()[0].key, "RUST_LOG");
        assert_eq!(a.inter_delay_ms, None, "未设的字段不能被写成 0");
    }

    #[test]
    fn group_automation_survives_save_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let g;
        {
            let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            g = v.add_group("生产".into());
            v.group_mut(g).unwrap().automation.tmux =
                Some(crate::automation::TmuxChoice::Off);
            v.save().unwrap();
        }
        let v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        assert_eq!(
            v.groups()[0].automation.tmux,
            Some(crate::automation::TmuxChoice::Off),
            "显式 Off 必须能落盘再读回,不能被当成未设写没"
        );
    }

    #[test]
    fn resolve_for_carries_automation_from_group() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let gid = v.add_group("生产".into());
        v.group_mut(gid).unwrap().automation.tmux =
            Some(crate::automation::TmuxChoice::Attach {
                session_name: Some("shared".into()),
            });
        let mut d = draft();
        d.identity.group_id = Some(gid);
        let id = v.add(d, "2026-08-06T00:00:00Z");

        let got = v.resolve_for(id).unwrap();
        assert_eq!(
            got.automation.tmux,
            Some(crate::automation::TmuxChoice::Attach {
                session_name: Some("shared".into())
            }),
            "分组的 tmux 设置应经 resolve_for 透出"
        );
    }

    #[test]
    fn update_replaces_automation() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let mut d = draft();
        d.automation.tmux = Some(crate::automation::TmuxChoice::Attach {
            session_name: Some("old".into()),
        });
        let id = v.add(d, "t");

        let mut d2 = draft();
        d2.automation.tmux = Some(crate::automation::TmuxChoice::Off);
        v.update(id, d2, "t2").unwrap();

        assert_eq!(
            v.get(id).unwrap().automation.tmux,
            Some(crate::automation::TmuxChoice::Off),
            "update 必须把 automation 一起替换掉"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败或通过**

```bash
cargo test -p mullion-store vault:: 2>&1 | tail -15
```

预期：Task 6 已经把 `add`/`update` 接好了，这四个测试**应该直接通过**。
如果 `update_replaces_automation` 失败，说明 Task 6 Step 3 漏了
`rec.automation = draft.automation;` —— 回去补上。

- [ ] **Step 3: 提交**

```bash
git add crates/mullion-store/src/vault.rs
git commit -m "test(store): automation 落盘 round-trip 与分组继承 (F40~F44)"
```

---

## Task 9: schema 升到 v4

**这里不需要新的迁移函数。** 现有 `load_sessions` 的分支逻辑是：
`probe > CURRENT` → 拒绝；`probe <= 1` → `migrate_v1`；`1 < probe < CURRENT` →
备份后按当前结构直读（新字段靠 `serde(default)` 补齐）。v3 → v4 恰好落在第三条，
和当年 v2 → v3 是同一条路径。升版本号的价值是**让旧客户端明确拒绝**，而不是静默
丢弃 `[session.automation]` 再写回——那是无声的数据丢失。

**Files:**
- Modify: `crates/mullion-store/src/model.rs`
- Modify: `crates/mullion-store/src/migrate.rs`
- Modify: `crates/mullion-store/src/vault.rs`

- [ ] **Step 1: 写失败测试**

`vault.rs` 的 `mod tests` 里追加（放在 `V2_ON_DISK` 相关测试之后）：

```rust
    /// 真实 v3 文件:结构已含 `[session.network]`,但没有 `[session.automation]`。
    const V3_ON_DISK: &str = r#"
schema_version = 3

[[session]]
id = 1
modified_at = "2026-08-01T00:00:00Z"

[session.identity]
name = "v3sess"

[session.connection]
host = "192.0.2.30"
port = 22
protocol = "ssh"

[session.auth]
user = "u3"
kind = "password"

[session.network]

[session.network.proxy]
kind = "socks5"
host = "127.0.0.1"
port = 7891
"#;

    #[test]
    fn open_upgrades_v3_file_and_adds_empty_automation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sessions.toml"), V3_ON_DISK).unwrap();

        let vault = Vault::open(dir.path().to_path_buf(), &key())
            .expect("真实 v3 文件必须能被直接读出来");

        assert_eq!(vault.list().len(), 1, "v3 会话不能丢");
        let s = &vault.list()[0];
        assert_eq!(s.identity.name, "v3sess");
        assert_eq!(s.connection.host, "192.0.2.30");
        assert!(
            matches!(s.network.proxy, Some(crate::network::ProxyChoice::Socks5(_))),
            "v3 已有的代理配置不能在升级中丢掉"
        );
        assert_eq!(
            s.automation,
            crate::automation::AutomationPrefs::default(),
            "缺 automation 分节应落默认(全继承),迁移不得凭空写值"
        );

        assert!(
            dir.path().join("sessions.toml.bak").exists(),
            "升级前必须留备份"
        );
        let bak = std::fs::read_to_string(dir.path().join("sessions.toml.bak")).unwrap();
        assert!(
            bak.contains("schema_version = 3"),
            "备份应是升级前的原文"
        );

        let now = std::fs::read_to_string(dir.path().join("sessions.toml")).unwrap();
        assert!(now.contains("schema_version = 4"), "磁盘上应已升到 v4");
    }
```

`migrate.rs` 的 `current_schema_is_three` 改名并改断言：

```rust
    /// 升 v4 的理由与 v3 一样,不是迁移而是让旧客户端**明确拒绝** ——
    /// 否则旧客户端读到 `[session.automation]` 会静默丢弃再写回,
    /// 用户配好的登录自动化无声消失。
    #[test]
    fn current_schema_is_four() {
        assert_eq!(crate::model::CURRENT_SCHEMA, 4);
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-store -- current_schema open_upgrades_v3 2>&1 | tail -20
```

预期：`current_schema_is_four` FAILED（`4 != 3`），
`open_upgrades_v3_file_and_adds_empty_automation` FAILED（磁盘上还是 `schema_version = 3`）。

- [ ] **Step 3: 升版本号**

`model.rs`：

```rust
/// 当前 TOML 结构版本。缺失该键的文件视为 v1(见 `migrate`)。
///
/// v3 = v2 + `[session.network]` / `[group.network]`。
/// v4 = v3 + `[session.automation]` / `[group.automation]`(F40~F44)。
///
/// 结构上新版本能直接读旧版本(新字段全带 `serde(default)`),升版本号是为了让
/// **旧客户端明确拒绝**,而不是静默丢弃新分节再写回。
///
/// **号段归属**:F74(凭据实体)原定 v3→v4,被本切片先落地拿走了 4,顺延为
/// v4→v5(规则「谁先落地谁拿号」,见 `spec.md` F74)。
pub const CURRENT_SCHEMA: u32 = 4;
```

- [ ] **Step 4: 修既有测试里写死的版本号**

```bash
grep -n 'schema_version = 3' crates/mullion-store/src/vault.rs
```

命中恰好两处，**都是期望值**（`V2_ON_DISK` 常量里写的是 `= 2`，本步新增的
`V3_ON_DISK` 是输入数据、必须保持 `= 3`）：

- `vault.rs:566` —— `assert!(now.contains("schema_version = 3"), "磁盘上应已是 v3");`
  → 改成 `assert!(now.contains("schema_version = 4"), "磁盘上应已是 v4");`
- `vault.rs:622` —— `assert!(now.contains("schema_version = 3"), "磁盘上应已升到 v3");`
  → 改成 `assert!(now.contains("schema_version = 4"), "磁盘上应已升到 v4");`

断言消息里的「v3」一并改掉，别留下与断言矛盾的提示文本。

- [ ] **Step 5: 跑全 crate 测试**

```bash
cargo test -p mullion-store 2>&1 | tail -10
```

预期：全过。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-store/src
git commit -m "feat(store): schema 升 v4 —— 加入 automation 分节 (F40~F44)

不需要新迁移函数:v3→v4 走既有的「备份 + 按当前结构直读」路径,
与当年 v2→v3 同款。升号是为了让旧客户端明确拒绝,而不是
静默丢弃 [session.automation] 再写回(那是无声的数据丢失)。
F74 的 schema 号顺延为 v4→v5。"
```

---

## Task 10: `mullion-ssh` 的 `ByteSink` 与 `write_scheduled`

**Files:**
- Modify: `Cargo.toml`（workspace 根）
- Modify: `crates/mullion-ssh/Cargo.toml`
- Create: `crates/mullion-ssh/src/schedule.rs`
- Modify: `crates/mullion-ssh/src/lib.rs`

- [ ] **Step 1: 加 tokio feature**

workspace 根 `Cargo.toml` 第 26 行，给 tokio 加 `time`：

```toml
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net", "io-util", "sync", "time"] }
```

`crates/mullion-ssh/Cargo.toml` 的 dev-dependencies 加 `test-util`
（`tokio::time::pause()` 需要它）：

```toml
[dev-dependencies]
tokio = { workspace = true, features = ["rt-multi-thread", "macros", "net", "io-util", "test-util"] }
```

- [ ] **Step 2: 写失败测试**

新建 `crates/mullion-ssh/src/schedule.rs`，先只写测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 假 sink:记录收到的字节,可编程失败模式。**零网络**。
    #[derive(Default)]
    struct FakeSink {
        written: Mutex<Vec<Vec<u8>>>,
        /// 每次 write 都返回这个错(None = 一律成功)。
        fail_with: Option<TrySendErr>,
        /// 前 N 次返回 Full,之后成功。
        full_times: Mutex<u32>,
    }

    impl FakeSink {
        fn written(&self) -> Vec<Vec<u8>> {
            self.written.lock().unwrap().clone()
        }
    }

    impl ByteSink for FakeSink {
        fn write(&self, bytes: Vec<u8>) -> Result<(), TrySendErr> {
            if let Some(e) = &self.fail_with {
                return Err(match e {
                    TrySendErr::Full => TrySendErr::Full,
                    TrySendErr::Closed => TrySendErr::Closed,
                });
            }
            let mut left = self.full_times.lock().unwrap();
            if *left > 0 {
                *left -= 1;
                return Err(TrySendErr::Full);
            }
            self.written.lock().unwrap().push(bytes);
            Ok(())
        }
    }

    fn steps() -> Vec<(Duration, Vec<u8>)> {
        vec![
            (Duration::from_millis(300), b"a\r".to_vec()),
            (Duration::from_millis(200), b"b\r".to_vec()),
        ]
    }

    #[tokio::test(start_paused = true)]
    async fn write_scheduled_respects_delays() {
        let fake = Arc::new(FakeSink::default());
        let sink: Arc<dyn ByteSink> = fake.clone();
        // 注意:必须用 `_tx` 这样的具名绑定持有 sender。写成裸 `_` 会当场 drop,
        // 接收端立刻就绪 → 整个计划被判定为「已取消」,测试会莫名其妙变红。
        let (_tx, rx) = tokio::sync::oneshot::channel();

        let start = tokio::time::Instant::now();
        let out = write_scheduled(sink, steps(), rx).await;

        assert_eq!(out, ScheduleOutcome::Completed);
        assert_eq!(fake.written(), vec![b"a\r".to_vec(), b"b\r".to_vec()], "顺序必须与时间表一致");
        assert_eq!(
            start.elapsed(),
            Duration::from_millis(500),
            "延时应累加:300 + 200(假时钟,零真实等待)"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn write_scheduled_stops_on_cancel() {
        let fake = Arc::new(FakeSink::default());
        let sink: Arc<dyn ByteSink> = fake.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();

        let handle = tokio::spawn(write_scheduled(sink, steps(), rx));
        // 推进到第一步已发、第二步还在等的时刻。
        tokio::time::sleep(Duration::from_millis(350)).await;
        tx.send(()).unwrap();

        let out = handle.await.unwrap();
        assert_eq!(out, ScheduleOutcome::Cancelled);
        assert_eq!(
            fake.written(),
            vec![b"a\r".to_vec()],
            "取消后剩余步骤一个字节都不许发(用户接管优先)"
        );
    }

    /// sender 被 drop(pane 关了、状态机没了)等同取消 —— 不能傻等下去。
    #[tokio::test(start_paused = true)]
    async fn dropping_the_canceller_stops_the_schedule() {
        let fake = Arc::new(FakeSink::default());
        let sink: Arc<dyn ByteSink> = fake.clone();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        drop(tx);

        let out = write_scheduled(sink, steps(), rx).await;
        assert_eq!(out, ScheduleOutcome::Cancelled);
        assert!(fake.written().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn write_scheduled_stops_when_sink_closed() {
        let fake = Arc::new(FakeSink {
            fail_with: Some(TrySendErr::Closed),
            ..Default::default()
        });
        let sink: Arc<dyn ByteSink> = fake.clone();
        let (_tx, rx) = tokio::sync::oneshot::channel();

        let out = write_scheduled(sink, steps(), rx).await;
        assert_eq!(out, ScheduleOutcome::Disconnected, "链路断了就停,不重试");
        assert!(fake.written().is_empty());
    }

    /// 出站队列偶发满(粘贴大段 + 慢链路)应重试,而不是当场放弃。
    #[tokio::test(start_paused = true)]
    async fn transient_full_is_retried() {
        let fake = Arc::new(FakeSink {
            full_times: Mutex::new(2),
            ..Default::default()
        });
        let sink: Arc<dyn ByteSink> = fake.clone();
        let (_tx, rx) = tokio::sync::oneshot::channel();

        let out = write_scheduled(sink, vec![(Duration::ZERO, b"a\r".to_vec())], rx).await;
        assert_eq!(out, ScheduleOutcome::Completed);
        assert_eq!(fake.written(), vec![b"a\r".to_vec()]);
    }

    /// 一直满就放弃,绝不无限重试。
    #[tokio::test(start_paused = true)]
    async fn persistent_full_gives_up_as_congested() {
        let fake = Arc::new(FakeSink {
            fail_with: Some(TrySendErr::Full),
            ..Default::default()
        });
        let sink: Arc<dyn ByteSink> = fake.clone();
        let (_tx, rx) = tokio::sync::oneshot::channel();

        let out = write_scheduled(sink, vec![(Duration::ZERO, b"a\r".to_vec())], rx).await;
        assert_eq!(out, ScheduleOutcome::Congested);
        assert!(fake.written().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn empty_plan_completes_without_writing() {
        let fake = Arc::new(FakeSink::default());
        let sink: Arc<dyn ByteSink> = fake.clone();
        let (_tx, rx) = tokio::sync::oneshot::channel();

        let out = write_scheduled(sink, Vec::new(), rx).await;
        assert_eq!(out, ScheduleOutcome::Completed);
        assert!(fake.written().is_empty());
    }
}
```

- [ ] **Step 3: 跑测试确认失败**

```bash
cargo test -p mullion-ssh schedule:: 2>&1 | tail -20
```

预期：编译失败，`cannot find trait 'ByteSink'` 等。

- [ ] **Step 4: 实现**

在 `schedule.rs` 顶部（`mod tests` 之前）写：

```rust
//! 按时间表把字节写进某个 sink。**只认延时与字节**,不认识 tmux / 自动化 /
//! 会话——那些语义留在 `mullion-store` 的纯函数里(架构不变量:ssh 不依赖 store)。
//!
//! 定时靠 `tokio::time::sleep` 而**不是** app 的帧循环:堆进事件循环会与
//! 帧率节流打架(陷阱 T3/T7)。

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::oneshot;

use crate::session::{SshSession, TrySendErr};

/// 出站队列满时的重试次数(含首次尝试)。
const FULL_ATTEMPTS: u32 = 3;
/// 重试之间的退避。
const FULL_BACKOFF: Duration = Duration::from_millis(50);

/// 能收字节的东西。存在的唯一理由是**可测**:有了它就能用假 sink +
/// `tokio::time::pause()` 零网络验证顺序 / 延时 / 取消 / 断线即停。
pub trait ByteSink: Send + Sync {
    fn write(&self, bytes: Vec<u8>) -> Result<(), TrySendErr>;
}

impl ByteSink for SshSession {
    fn write(&self, bytes: Vec<u8>) -> Result<(), TrySendErr> {
        SshSession::write(self, bytes)
    }
}

/// 时间表跑完的结局。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleOutcome {
    /// 全部步骤都发完了。
    Completed,
    /// 被取消(用户接管,或取消端被 drop)。
    Cancelled,
    /// 对端已关闭(pane 关了 / 链路断了)。
    Disconnected,
    /// 出站队列持续满,放弃。
    Congested,
}

/// 依次「等 `delay` → 写 `bytes`」,直到跑完或被打断。
///
/// `cancel` 一旦就绪(收到值**或**发送端被 drop),立即停止,**剩余步骤一个字节
/// 都不再发**——这是「用户接管优先」的落点:用户已经开始打字,再插字节就是抢输入。
pub async fn write_scheduled(
    sink: Arc<dyn ByteSink>,
    steps: Vec<(Duration, Vec<u8>)>,
    mut cancel: oneshot::Receiver<()>,
) -> ScheduleOutcome {
    for (delay, bytes) in steps {
        tokio::select! {
            // 取消优先:同时就绪时先看取消,避免「刚被取消却又发了一步」。
            biased;
            _ = &mut cancel => return ScheduleOutcome::Cancelled,
            _ = tokio::time::sleep(delay) => {}
        }

        let mut attempt = 0u32;
        loop {
            match sink.write(bytes.clone()) {
                Ok(()) => break,
                Err(TrySendErr::Closed) => return ScheduleOutcome::Disconnected,
                Err(TrySendErr::Full) => {
                    attempt += 1;
                    if attempt >= FULL_ATTEMPTS {
                        return ScheduleOutcome::Congested;
                    }
                    tokio::select! {
                        biased;
                        _ = &mut cancel => return ScheduleOutcome::Cancelled,
                        _ = tokio::time::sleep(FULL_BACKOFF) => {}
                    }
                }
            }
        }
    }
    ScheduleOutcome::Completed
}
```

`crates/mullion-ssh/src/lib.rs` 的 `pub mod` 列表里（`proxy` 之后、`session` 之前）加：

```rust
pub mod schedule;
```

- [ ] **Step 5: 跑测试确认通过**

```bash
cargo test -p mullion-ssh schedule:: 2>&1 | tail -15
```

预期：7 passed。

- [ ] **Step 6: 提交**

```bash
git add Cargo.toml crates/mullion-ssh/Cargo.toml crates/mullion-ssh/src
git commit -m "feat(ssh): 按时间表定时写字节的通用函数 (F40~F44)

write_scheduled 只认 Vec<(Duration, Vec<u8>)>,不认识 tmux/自动化语义。
取消用 tokio 自带 oneshot,不引 tokio-util。ByteSink 抽象存在的唯一理由
是可测:假 sink + tokio::time::pause() 零网络验顺序/延时/取消/断线即停。
定时靠 sleep 而非帧循环 —— 堆进事件循环会与 T3/T7 帧率节流打架。"
```

---

## Task 11: 全绿验收

「绿」的定义（CLAUDE.md）：`cargo test --workspace` 全过 **且**
`clippy -D warnings` 无输出。只跑单个 crate 不叫绿。

- [ ] **Step 1: 全量测试**

```bash
cargo test --workspace > /tmp/p1a-test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/p1a-test.log
```

预期：每个 crate 都是 `test result: ok`，没有 FAILED / panicked。

- [ ] **Step 2: clippy**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -30
```

预期：无输出（或只有 `Finished` 行）。

常见需要修的点：`Duration::from_millis(x as u64)` 应写成 `u64::from(x)`
（计划里的代码已经这么写了）；`map_or` 与 `map(..).unwrap_or(..)` 的取舍。

- [ ] **Step 3: 格式**

```bash
cargo fmt --all && cargo fmt --check && echo "FMT OK"
```

- [ ] **Step 4: 若 fmt 有改动则提交**

```bash
git add -A && git commit -m "style: cargo fmt (P1-a)"
```

---

## 本切片**不做**的事（留给 P1-b，别顺手开工）

- `app.rs` 的 `AutomationState` 状态机与四条触发边
- 首字节检测、`ControlFlow::WaitUntil(deadline)` 的超时唤醒（T7：三个分支都要复位）
- 会话/分组编辑器的「登录后」分节 UI，以及 F41 输入时拒绝含换行的命令
- 会话列表右键「连接（跳过自动化）」
- 状态栏三条提示
- 版本号 bump / 交叉编译 / 发 Release

**一条给 P1-b 的前置提醒**：`write_scheduled` 收的是 `Arc<dyn ByteSink>`，而
`SshSession` 目前在 app 里是被直接拥有的。P1-b 需要把 pane 持有的 `SshSession`
改成 `Arc<SshSession>`（方法都是 `&self`，改动是机械的），否则没法把 sink 交给
spawn 出去的 task。

---

## 自审记录

**spec 覆盖**：F40（Task 2/4）、F41（Task 4/5/7）、F42（Task 4/5）、F43（Task 4/5）、
F44（Task 4/7）；三条硬规则里「登录完成判定」与「用户接管优先」的**数据与调度侧**
在 Task 10，**检测与接线侧**属 P1-b；「只有第一个 pane 跑自动化」纯属 app 接线，
本切片无对应代码。schema v4（Task 9）、`ResolvedAutomation` 喂 `build_plan` 的端到端
链路（Task 7 最后一个测试）。

**设计 §9 测试矩阵对照**：矩阵里 store 与 ssh 两层共 13 行，本计划全部覆盖，另外
补了 8 个矩阵没列但同样该有的（`tmux_kind_is_tagged`、`env_with_invalid_key_is_dropped`、
`command_containing_newline_is_dropped`、`dropping_the_canceller_stops_the_schedule`、
`transient_full_is_retried`、`persistent_full_gives_up_as_congested`、
`explicit_empty_command_list_overrides_group`、`group_can_disable_automation_for_whole_group`）。
矩阵里的 app 两行（`frame::tests`、`reflow_emits_resize`）属 P1-b。

**类型一致性**：`AutomationPrefs`（Task 1）→ `PrefsLayer::automation()`（Task 7）字段名
一致；`ResolvedAutomation` 八个字段在 Task 4 定义、Task 7 逐字段填充，无遗漏；
`Step { delay, bytes }`（Task 4）→ `(Duration, Vec<u8>)`（Task 10）的转换属 P1-b，
两边形状对得上。
