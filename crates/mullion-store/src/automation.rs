//! F40~F44 登录后自动化的数据模型与纯函数。零 IO、零 async。
//!
//! 核心约束(设计 §2):**自动化只在「确定还是干净 shell」的窗口期内发字节**。
//! 每次 SSH 连接都会拿到 sshd fork 的新 login shell,所以第一个字节安全;危险的是
//! 第二个——我们自己发的 `tmux attach` 一旦生效,屏幕就归那个正在跑的 TUI 了,
//! 此后再发命令行,字符会进它的输入框。所以生成的时间表**能一步就绝不两步**。

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// 登录后自动化(可继承分节)。字段一律 `Option`,`None` 即继承上游。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationPrefs {
    /// F44 总开关。`None` = 继承;内置默认见 `DEFAULT_AUTOMATION_ENABLED`。
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
        /// `None` = 由会话名称推导(见 `sanitize_tmux_name`)。
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

/// 把任意字符串改造成合法的 tmux 会话名。
///
/// tmux 用 `.` 与 `:` 做 `session:window.pane` 定址,名字里带上它们会被解析成
/// 别的目标;`$` 是 session-id 前缀、`%` 是 pane-id 前缀、`@` 是 window-id 前缀、
/// `=` 是精确匹配前缀——这四个字符**只在名字开头**时会被 `has-session -t` 解析成
/// 别的目标(实测:`@foo` 会报 `can't find window: @foo`,中间位置如 `foo@bar` 无害)。
/// 为了跟 `.`/`:` 保持同一套简单规则,这里不区分位置、统一替换——偏保守但安全;
/// 控制字符则会破坏「整个计划只有一行」这个不变量(一个 `\r` 就是一次额外回车)。
/// 以上都在这里处理掉。
///
/// 返回空串表示**没有可用的名字**,调用方应据此放弃生成计划而不是发一条残命令。
pub fn sanitize_tmux_name(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| {
            if matches!(c, '.' | ':' | '$' | '%' | '=' | '@') {
                '-'
            } else {
                c
            }
        })
        .collect()
}

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
        Some(TmuxChoice::Attach { .. }) => match tmux_session_name(a, fallback_name) {
            // 没有可用的会话名。宁可什么都不做,也不发 `attach -t ''` 这种残命令。
            None => Vec::new(),
            Some(name) => vec![Step {
                delay: initial,
                bytes: line(&tmux_command(a, &name, false)),
            }],
        },
        // `None`(没配)与 `Some(Off)`(显式关)在这里等价。
        _ => build_no_tmux(a, initial),
    }
}

/// F141:**断线重连**专用的计划。与 [`build_plan`] 逐字节相同,只有一处差别:
/// `attach` 带上 `-d`。
///
/// `-d` 让新 client 把**同一个会话上已有的其他 client 踢掉**。重连场景下那个
/// 「其他 client」几乎必然是我们自己的残骸:SSH 链路断了,但远端 sshd 与 tmux
/// 服务器要等 TCP 超时(默认可以是几分钟)才知道,那期间旧 client 还挂在会话上。
/// 不踢的话两个 client 同时 attach,tmux 的 `window-size` 取 `latest` 会跟着
/// 两边尺寸反复 reflow、取 `smallest` 会在窗口周围留白 —— 症状是重连之后排版
/// 莫名其妙,而且要等到远端超时才自愈。
///
/// **`-d` 只加在 `attach` 上,绝不能加在 `new-session` 上**:对 `new-session`
/// 而言 `-d` 是「建好但不 attach」,那会让远端 shell 立刻返回、用户对着一个空
/// 会话发呆(守护:`reattach_never_puts_the_detach_flag_on_new_session`)。
///
/// 返回空 = 这份配置根本不走 tmux(关了 / 没配 / 会话名 sanitize 后为空)。
/// **不静默回落成「无 tmux 的登录后命令」**——调用方要的是「把之前那个会话接
/// 回来」,悄悄换成「在新 shell 里重跑一遍命令」是另一回事,该由调用方明写。
pub fn build_plan_reattach(a: &ResolvedAutomation, fallback_name: &str) -> Vec<Step> {
    match tmux_session_name(a, fallback_name) {
        None => Vec::new(),
        Some(name) => vec![Step {
            delay: Duration::from_millis(u64::from(a.initial_delay_ms)),
            bytes: line(&tmux_command(a, &name, true)),
        }],
    }
}

/// F141:这份配置最终会 attach / 新建的 tmux 会话名。`None` = 不走 tmux
/// (自动化关着 / 没配 tmux / 显式 `Off` / 会话名 sanitize 之后是空的)。
///
/// **是 `build_plan` tmux 分支的判据本身**,不是它的复制品 —— 调用方
/// (app 侧记「这块 pane 到底 attach 了哪个会话」)与计划生成必须永远同口径,
/// 否则会出现「记着要重连的会话名,和当初真的 attach 上去的那个不是同一个」。
pub fn tmux_session_name(a: &ResolvedAutomation, fallback_name: &str) -> Option<String> {
    if !a.enabled {
        return None;
    }
    let Some(TmuxChoice::Attach { session_name }) = &a.tmux else {
        return None;
    };
    let name = sanitize_tmux_name(session_name.as_deref().unwrap_or(fallback_name));
    (!name.is_empty()).then_some(name)
}

/// tmux 分支那一行命令。`detach_others` = attach 时带 `-d`(见
/// [`build_plan_reattach`])。
///
/// `name` 必须已经过 `sanitize_tmux_name`(调用方都是从 `tmux_session_name`
/// 拿的)。
fn tmux_command(a: &ResolvedAutomation, name: &str, detach_others: bool) -> String {
    let q = shell_quote(name);
    // `||` 的回落条件是 `has-session` 探测失败(会话不存在 → 新建),这条路径
    // 实测正常。但 `exec` 会替换进程镜像:一旦 `attach` 起来了、之后才运行时
    // 失败(例如会话在探测与 attach 之间被别的客户端 kill),原 shell 已不
    // 存在,**不会**回落去新建——远端控制进程直接退出、channel 随之关闭。
    // 已知且接受:竞态窗口极小,「多开一个意外的新会话」也并不比「掉线重连」更好。
    let d = if detach_others { " -d" } else { "" };
    let mut cmd = format!(
        "tmux has-session -t {q} 2>/dev/null && exec tmux attach{d} -t {q} \
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
    cmd
}

/// F161/D1+D2:按**实测**会话名接回 tmux 的那一行命令。
///
/// 与 [`tmux_command`] 的唯一差别:**砍掉 `|| exec tmux new-session` 那半段**。
/// 这是 D2 的红线 —— 实测名是「关 exe 那一刻那块 pane 真的在里面」的会话,
/// 它不在了就说明远端重启过或用户自己 kill 了,凭空造一个同名空会话只会让
/// 用户以为现场回来了。**永不替用户在远端造东西。**
///
/// **`has-session` 守门必须留着**,砍掉的只是 `||` 后半段:裸的
/// `exec tmux attach -t X` 在会话不存在时,`exec` 已经把 shell 替换成 tmux
/// 进程,tmux 报错退出 → channel 关闭 → **pane 当场死掉**,D4 的「挂提示」和
/// D8 的「停在裸 shell」全部落空。守门之后 `&&` 短路,shell 原地活着。
/// 探测与 attach 之间的竞态窗口沿用 [`tmux_command`] 里「已知且接受」的结论。
///
/// `name` **不过 `sanitize_tmux_name`**:那是给「拿配置里的名字去新建」用的,
/// 会把 `@` 之类合法字符改掉。实测名是远端 tmux 自己报的 `#S`,已经合法。
pub fn attach_only_command(name: &str, detach_others: bool) -> String {
    let q = shell_quote(name);
    let d = if detach_others { " -d" } else { "" };
    format!("tmux has-session -t {q} 2>/dev/null && exec tmux attach{d} -t {q}")
}

/// F161:按实测会话名接回 tmux 的一步计划。空计划 = 不发任何字节。
///
/// 恒**恰好一个 Step**(与 `build_plan` 的 tmux 分支同一个不变量):attach
/// 一旦生效,屏幕就归那个 TUI 了,之后再发任何字节都是打进 TUI(D7)。
///
/// 返回空的两种情形:自动化总开关关着;名字去掉空白后为空。
pub fn build_plan_attach_measured(
    a: &ResolvedAutomation,
    name: &str,
    detach_others: bool,
) -> Vec<Step> {
    if !a.enabled || name.trim().is_empty() {
        return Vec::new();
    }
    vec![Step {
        delay: Duration::from_millis(u64::from(a.initial_delay_ms)),
        bytes: line(&attach_only_command(name, detach_others)),
    }]
}

/// 同一份配置,但**跳过 tmux 那一步**:环境变量 / 工作目录 / 登录后命令照跑。
///
/// 给「不是这条连接第一个 pane」的 pane 用 —— 分屏新开的、以及换过节点的。
/// 理由在 `AutomationHandle` 的文档里:多块 pane attach 同一个 tmux session 会
/// 内容镜像,且 `window-size` 取 `latest` 会反复 reflow、取 `smallest` 会留白。
/// 而用户配的 `cd` / `export` / 启动命令对每块新 shell 都仍然成立。
///
/// 不收 `fallback_name`:那个参数**只**服务于 tmux 会话名的兜底,这条路径上
/// 没有任何东西会用到它。留着等于给调用方一个「传什么都行」的哑参数。
pub fn build_plan_without_tmux(a: &ResolvedAutomation) -> Vec<Step> {
    if !a.enabled {
        return Vec::new();
    }
    build_no_tmux(a, Duration::from_millis(u64::from(a.initial_delay_ms)))
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
    // 只统计真正存活、进了 stmts 的命令:被过滤掉的命令(空白/含控制字符)
    // 的 delay_ms 不该有话语权,否则一条要丢弃的命令也能把计划拆成多步。
    let mut has_command_delay = false;
    for c in a.commands.iter() {
        if c.text.trim().is_empty() || c.text.chars().any(char::is_control) {
            continue;
        }
        if c.delay_ms.is_some() {
            has_command_delay = true;
        }
        stmts.push((c.delay_ms, c.text.trim().to_string()));
    }
    if stmts.is_empty() {
        return Vec::new();
    }

    let wants_multi_step = has_command_delay;
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
        assert!(
            !s.contains("session_name"),
            "None 的 session_name 不应写出: {s}"
        );
    }

    #[test]
    fn sanitize_tmux_name_replaces_dot_and_colon() {
        // tmux 用 `.` 和 `:` 做 window/pane 定址,会话名里出现就会被解析成别的东西。
        assert_eq!(sanitize_tmux_name("web01.prod:2"), "web01-prod-2");
    }

    #[test]
    fn sanitize_tmux_name_replaces_session_pane_window_and_exact_match_prefixes() {
        // $ 是 session-id 前缀、% 是 pane-id 前缀、@ 是 window-id 前缀、= 是精确匹配
        // 前缀,has-session -t 会把它们解析成别的目标,与 . 和 : 是同一类问题。
        assert_eq!(sanitize_tmux_name("$foo%bar=baz@qux"), "-foo-bar-baz-qux");
    }

    #[test]
    fn sanitize_tmux_name_replaces_prefix_chars_in_middle_too() {
        // 实测这些字符只在开头才会被 has-session -t 解析成别的目标(中间位置如
        // foo@bar 无害),但为了跟 . 和 : 统一成一套简单规则,代码不区分位置、
        // 一律替换——这是既有的保守行为,这里如实固定下来。
        assert_eq!(
            sanitize_tmux_name("foo$bar%baz=qux@quux"),
            "foo-bar-baz-qux-quux"
        );
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
        assert_eq!(
            bytes.last(),
            Some(&b'\r'),
            "行终止符是 \\r(复用 keymap 的 Enter 约定)"
        );
        assert_eq!(
            bytes
                .iter()
                .filter(|b| **b == b'\r' || **b == b'\n')
                .count(),
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
        assert!(
            line.contains("-t 'web01-prod-2'"),
            "回退名也要 sanitize: {line}"
        );
    }

    /// 名字 sanitize 后为空 → 无法构造安全命令 → 什么都不发。
    /// 绝不能退化成 `tmux attach -t ''` 那种残命令。
    #[test]
    fn build_plan_is_empty_when_name_sanitizes_to_nothing() {
        let a = resolved(Some(TmuxChoice::Attach {
            session_name: Some("...".into()),
        }));
        assert!(
            build_plan(&a, "   ").is_empty() || {
                let a2 = resolved(Some(TmuxChoice::Attach { session_name: None }));
                build_plan(&a2, "   ").is_empty()
            }
        );
        let a2 = resolved(Some(TmuxChoice::Attach { session_name: None }));
        assert!(
            build_plan(&a2, "  ").is_empty(),
            "回退名全空白时不得生成计划"
        );
    }

    /// F141:重连计划与首连计划**只差一个 `-d`**。
    ///
    /// 拿整行做差而不是逐个 `contains`:两条分支共用 `tmux_command` 是当下的
    /// 实现,但没有什么拦得住以后有人把重连那条单独改一笔(加个 `-r`、换个
    /// 回落策略)。整行相等的断言会当场变红。
    ///
    /// 自证会变红:把 `build_plan_reattach` 里的 `true` 改成 `false`。
    #[test]
    fn the_reattach_plan_is_the_first_connect_plan_plus_the_detach_flag() {
        let mut a = resolved(Some(TmuxChoice::Attach { session_name: None }));
        // 用**完整**配置(工作目录 + env + 命令),不留一条只在简单配置下成立的断言。
        a.work_dir = Some("/srv".into());
        a.env = vec![EnvVar {
            key: "RUST_LOG".into(),
            value: "debug".into(),
        }];
        a.commands = vec![AutomationCommand {
            text: "claude".into(),
            delay_ms: None,
        }];

        let first = text_of(&build_plan(&a, "web01")[0]);
        let again = text_of(&build_plan_reattach(&a, "web01")[0]);
        assert!(
            again.contains("&& exec tmux attach -d -t 'web01'"),
            "重连要踢掉断线残留的旧 client: {again}"
        );
        assert!(
            !first.contains(" -d"),
            "首次连接不该踢掉别处正 attach 着的 client: {first}"
        );
        assert_eq!(
            again.replace("attach -d -t", "attach -t"),
            first,
            "除了 attach 的 -d,重连计划必须与首连计划逐字相同"
        );
        assert_eq!(
            build_plan_reattach(&a, "web01").len(),
            1,
            "同 build_plan:有 tmux 就只发一步"
        );
    }

    /// F141:`-d` 对 `new-session` 是「建好但**不** attach」——加错地方的后果是
    /// 远端 shell 立刻返回,用户对着一个空会话发呆,而且看不出成因。
    ///
    /// 自证会变红:把 `tmux_command` 里的 `-d` 从 `attach{d}` 挪到
    /// `new-session{d}`。
    #[test]
    fn reattach_never_puts_the_detach_flag_on_new_session() {
        let a = resolved(Some(TmuxChoice::Attach { session_name: None }));
        let line = text_of(&build_plan_reattach(&a, "web01")[0]);
        assert!(
            line.contains("|| exec tmux new-session -s 'web01'"),
            "会话不在了仍要新建: {line}"
        );
        assert!(
            !line.contains("new-session -d"),
            "-d 加在 new-session 上等于「建了不进去」: {line}"
        );
    }

    /// F141:不走 tmux 的配置,重连计划是**空**的 —— 不许静默回落成
    /// 「在新 shell 里重跑一遍登录后命令」。那是另一回事,该由调用方明写
    /// (app 侧的 `pending_for_extra_pane`)。
    ///
    /// 自证会变红:把 `build_plan_reattach` 的 `None` 分支改成
    /// `build_no_tmux(a, ..)`。
    #[test]
    fn the_reattach_plan_is_empty_when_there_is_no_tmux_to_come_back_to() {
        let mut off = resolved(Some(TmuxChoice::Off));
        off.commands = vec![AutomationCommand {
            text: "claude".into(),
            delay_ms: None,
        }];
        assert!(
            build_plan_reattach(&off, "web01").is_empty(),
            "显式关掉 tmux 时不该有重连计划"
        );
        assert!(
            !build_plan_without_tmux(&off).is_empty(),
            "前提:这份配置本身是有命令要跑的,上面那条断言才不是重言式"
        );
        assert!(
            build_plan_reattach(&resolved(None), "web01").is_empty(),
            "没配 tmux 时同理"
        );
    }

    /// F141:`tmux_session_name` 必须与 `build_plan` 的 tmux 分支同口径 ——
    /// app 侧靠它记「这块 pane 当初 attach 的是哪个会话」,记岔了就会在重连时
    /// 把用户接到另一个会话上。
    #[test]
    fn the_recorded_session_name_matches_what_the_plan_actually_attaches() {
        let auto = resolved(Some(TmuxChoice::Attach { session_name: None }));
        assert_eq!(
            tmux_session_name(&auto, "web01.prod:2"),
            Some("web01-prod-2".into()),
            "回退名要跟计划一样 sanitize"
        );
        let explicit = resolved(Some(TmuxChoice::Attach {
            session_name: Some("claude".into()),
        }));
        assert_eq!(
            tmux_session_name(&explicit, "web01"),
            Some("claude".into()),
            "显式名同样胜出"
        );

        // 三种「没有 tmux 可回」的情形。
        assert_eq!(
            tmux_session_name(&resolved(Some(TmuxChoice::Off)), "web01"),
            None
        );
        assert_eq!(tmux_session_name(&resolved(None), "web01"), None);
        let mut disabled = auto.clone();
        disabled.enabled = false;
        assert_eq!(
            tmux_session_name(&disabled, "web01"),
            None,
            "F44 关掉自动化时不该记下任何会话名"
        );

        // 名字为空 → 计划放弃,记录也必须一起放弃(否则重连会去 attach 一个
        // 当初根本没建起来的会话)。
        assert_eq!(tmux_session_name(&auto, "   "), None);
        assert!(build_plan(&auto, "   ").is_empty(), "两者必须同进同退");
    }

    /// 上一条测试的两条 assert 靠 `||` 右边(回退名全空白)才通过——
    /// `sanitize_tmux_name("...")` 实际得到 `"---"`,非空。这里用能真正让
    /// **显式** `session_name` 自己 sanitize 后为空的输入(纯空白),单独钉住
    /// 这条分支:显式名一样要走空名保护,不能因为「用户填了点什么」就放过。
    #[test]
    fn build_plan_is_empty_when_explicit_session_name_sanitizes_to_nothing() {
        let a = resolved(Some(TmuxChoice::Attach {
            session_name: Some("   ".into()),
        }));
        assert!(
            build_plan(&a, "web01").is_empty(),
            "显式 session_name sanitize 后为空也不得生成计划"
        );
    }

    /// 设计 §3「空启动命令的边界」:没命令没 env 时不许生成 `new-session -s X ''`,
    /// 那个空参数会让 tmux 去跑一个空命令,行为随版本而异。
    #[test]
    fn empty_start_command_omits_quoted_arg() {
        let a = resolved(Some(TmuxChoice::Attach { session_name: None }));
        let line = text_of(&build_plan(&a, "web01")[0]);
        assert!(!line.contains("''"), "不应出现空的引号参数: {line}");
        assert!(
            !line.contains("exec $SHELL"),
            "没东西可跑就不该套 exec $SHELL: {line}"
        );
    }

    #[test]
    fn work_dir_becomes_new_session_c_flag_only() {
        let mut a = resolved(Some(TmuxChoice::Attach { session_name: None }));
        a.work_dir = Some("/srv/app".into());
        let line = text_of(&build_plan(&a, "web01")[0]);
        assert!(
            line.contains("new-session -s 'web01' -c '/srv/app'"),
            "{line}"
        );
        // attach 分支绝不能带工作目录:附着已有会话时改它的目录是越权。
        let attach_part = line.split("||").next().unwrap();
        assert!(
            !attach_part.contains("/srv/app"),
            "attach 分支不得带 -c: {line}"
        );
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
        assert!(
            line.contains(r#"echo '\''hi'\''"#),
            "命令应原样嵌入最外层引号: {line}"
        );
        assert!(line.contains("exec $SHELL"), "跑完命令要留住 shell: {line}");
    }

    /// `start_command` 的 `parts.join("; ")`:两条合法命令要用 `; ` 拼在一起,
    /// 之前的测试要么只有单条命令、要么第二条被控制字符过滤掉,没人真正走过
    /// 「两条都活下来、要拼接」这条路径。
    #[test]
    fn multiple_commands_are_joined_with_semicolons() {
        let mut a = resolved(Some(TmuxChoice::Attach { session_name: None }));
        a.commands = vec![
            AutomationCommand {
                text: "cd /srv".into(),
                delay_ms: None,
            },
            AutomationCommand {
                text: "ls".into(),
                delay_ms: None,
            },
        ];
        let line = text_of(&build_plan(&a, "web01")[0]);
        assert!(
            line.contains("cd /srv; ls; exec $SHELL"),
            "两条合法命令应以 '; ' 拼接、末尾接 exec $SHELL: {line}"
        );
    }

    #[test]
    fn env_becomes_export_statements_with_quoted_values() {
        let mut a = resolved(Some(TmuxChoice::Attach { session_name: None }));
        a.env = vec![EnvVar {
            key: "RUST_LOG".into(),
            value: "debug,h2=off".into(),
        }];
        let line = text_of(&build_plan(&a, "web01")[0]);
        assert!(
            line.contains(r#"export RUST_LOG='\''debug,h2=off'\''"#),
            "{line}"
        );
    }

    /// 非法 key 会让整行 shell 语法崩掉(`export 2FOO=x` 是语法错误),
    /// 更糟的是 key 里塞 `;` 能越出赋值语句。直接丢弃,不生成。
    #[test]
    fn env_with_invalid_key_is_dropped() {
        let mut a = resolved(Some(TmuxChoice::Attach { session_name: None }));
        a.env = vec![
            EnvVar {
                key: "2BAD".into(),
                value: "x".into(),
            },
            EnvVar {
                key: "A;rm -rf /".into(),
                value: "x".into(),
            },
            EnvVar {
                key: "GOOD_1".into(),
                value: "y".into(),
            },
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
        a.commands = vec![AutomationCommand {
            text: "ls".into(),
            delay_ms: None,
        }];
        assert!(build_plan(&a, "web01").is_empty(), "F44 关闭时必须零步");
    }

    /// 含换行的命令会在一行模型里变成「额外的一次回车」,正是设计要挡的东西。
    /// UI 应在输入时就拒绝(F41),这里是防御性兜底。
    #[test]
    fn command_containing_newline_is_dropped() {
        let mut a = resolved(Some(TmuxChoice::Attach { session_name: None }));
        a.commands = vec![
            AutomationCommand {
                text: "ls\rrm -rf /".into(),
                delay_ms: None,
            },
            AutomationCommand {
                text: "pwd".into(),
                delay_ms: None,
            },
        ];
        let line = text_of(&build_plan(&a, "web01")[0]);
        assert!(
            !line.contains("rm -rf"),
            "含控制字符的命令应整条丢弃: {line}"
        );
        assert!(line.contains("pwd"), "其余命令不受影响: {line}");
    }

    /// 设计 §11 修订 2:无 tmux 分支默认**也只有一步**。
    /// 用户的 .bashrc 里写一句自动 attach tmux 是极常见配置,无条件拆多步
    /// 会让第二条起打进 TUI —— 与 tmux 分支是同一个坑。
    #[test]
    fn build_plan_no_tmux_is_single_step_unless_per_command_delay() {
        let mut a = resolved(None);
        a.work_dir = Some("/srv".into());
        a.env = vec![EnvVar {
            key: "A".into(),
            value: "1".into(),
        }];
        a.commands = vec![
            AutomationCommand {
                text: "ls".into(),
                delay_ms: None,
            },
            AutomationCommand {
                text: "pwd".into(),
                delay_ms: None,
            },
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
        a.commands = vec![AutomationCommand {
            text: "ls".into(),
            delay_ms: None,
        }];
        let plan = build_plan(&a, "web01");
        assert_eq!(plan.len(), 1);
        assert_eq!(text_of(&plan[0]), "ls\r");
        assert!(
            !text_of(&plan[0]).contains("tmux"),
            "显式 Off 不该生成 tmux 命令"
        );
    }

    #[test]
    fn build_plan_no_tmux_orders_export_cd_then_commands() {
        let mut a = resolved(None);
        a.work_dir = Some("/srv".into());
        a.env = vec![EnvVar {
            key: "A".into(),
            value: "1".into(),
        }];
        a.commands = vec![
            AutomationCommand {
                text: "ls".into(),
                delay_ms: Some(500),
            },
            AutomationCommand {
                text: "pwd".into(),
                delay_ms: None,
            },
        ];

        let plan = build_plan(&a, "web01");
        assert_eq!(plan.len(), 4, "配了逐条延时就拆:export / cd / ls / pwd");
        assert_eq!(text_of(&plan[0]), "export A='1'\r");
        assert_eq!(text_of(&plan[1]), "cd '/srv'\r");
        assert_eq!(text_of(&plan[2]), "ls\r");
        assert_eq!(text_of(&plan[3]), "pwd\r");

        assert_eq!(
            plan[0].delay,
            Duration::from_millis(300),
            "首步用 initial_delay"
        );
        assert_eq!(
            plan[1].delay,
            Duration::from_millis(200),
            "未设的用 inter_delay"
        );
        assert_eq!(plan[2].delay, Duration::from_millis(500), "设了的用自己的");
        assert_eq!(plan[3].delay, Duration::from_millis(200));
    }

    #[test]
    fn build_plan_no_tmux_with_nothing_configured_is_empty() {
        assert!(
            build_plan(&resolved(None), "web01").is_empty(),
            "没配任何东西 → 零步"
        );
    }

    /// 分屏新开的 pane / 换过节点的 pane 要跑登录后命令,但**不能碰 tmux**:
    /// 两块 pane attach 同一个 session 会内容镜像,`window-size` 取 `latest`
    /// 还会反复 reflow、取 `smallest` 会留白(`AutomationHandle` 的原始理由)。
    /// 用户拍板的规则是「跳过 tmux,其余照跑」。
    ///
    /// 自证会变红:把 `build_plan_without_tmux` 的实现改成直接转发
    /// `build_plan(a, "")`。
    #[test]
    fn plan_without_tmux_keeps_the_commands_but_never_touches_tmux() {
        let mut a = resolved(Some(TmuxChoice::Attach {
            session_name: Some("web01".into()),
        }));
        a.work_dir = Some("/srv/app".into());
        a.env = vec![EnvVar {
            key: "FOO".into(),
            value: "bar".into(),
        }];
        a.commands = vec![AutomationCommand {
            text: "cargo watch".into(),
            delay_ms: None,
        }];

        // 前提:同一份配置走完整计划时确实会发 tmux —— 否则下面那条
        // 「不含 tmux」的断言是恒真的。
        assert!(text_of(&build_plan(&a, "fallback")[0]).contains("tmux"));

        let plan = build_plan_without_tmux(&a);
        assert_eq!(plan.len(), 1, "没配逐条延时时仍应合成一步");
        let line = text_of(&plan[0]);
        assert!(
            !line.contains("tmux"),
            "跳过 tmux 的计划里不许出现 tmux: {line}"
        );
        assert!(line.contains("export FOO='bar'"), "环境变量要照跑: {line}");
        assert!(line.contains("cd '/srv/app'"), "工作目录要照跑: {line}");
        assert!(line.contains("cargo watch"), "登录后命令要照跑: {line}");
        assert_eq!(plan[0].delay, Duration::from_millis(300));
    }

    /// 总开关关掉时,跳过 tmux 的这条路也必须是零步 —— 否则 F44「关掉自动化」
    /// 在分屏 pane 上会失效,用户关了却还是有命令被发出去。
    ///
    /// 自证会变红:把 `build_plan_without_tmux` 里的 `if !a.enabled` 删掉。
    #[test]
    fn plan_without_tmux_still_respects_the_master_switch() {
        let mut a = resolved(None);
        a.commands = vec![AutomationCommand {
            text: "ls".into(),
            delay_ms: None,
        }];
        assert!(!build_plan_without_tmux(&a).is_empty(), "前提:开着时非空");
        a.enabled = false;
        assert!(
            build_plan_without_tmux(&a).is_empty(),
            "F44 关掉就一步都不发"
        );
    }

    /// 配的是 tmux **而且没有任何命令**时,跳过 tmux 就等于无事可做 ——
    /// 必须是零步,不能凭空发一个空行(那是一次多余的回车,会打进远端 shell)。
    #[test]
    fn plan_without_tmux_is_empty_when_tmux_was_the_only_thing_configured() {
        let a = resolved(Some(TmuxChoice::Attach { session_name: None }));
        assert!(!build_plan(&a, "web01").is_empty(), "前提:完整计划非空");
        assert!(
            build_plan_without_tmux(&a).is_empty(),
            "只配了 tmux 的会话,分屏 pane 无事可做"
        );
    }

    #[test]
    fn no_tmux_branch_never_emits_exec_shell() {
        // exec $SHELL 只对 tmux new-session 的启动命令有意义;
        // 直接打进用户 shell 会把当前 shell 换掉,毫无必要。
        let mut a = resolved(None);
        a.commands = vec![AutomationCommand {
            text: "ls".into(),
            delay_ms: None,
        }];
        assert!(!text_of(&build_plan(&a, "web01")[0]).contains("exec $SHELL"));
    }

    /// 复核发现的真实 bug:`wants_multi_step` 曾扫描过滤前的 `a.commands`,
    /// 一条被过滤掉的空白命令只要带 `delay_ms`,就会把本该合并成一行的计划
    /// 拆成多步——没有任何存活语句要求拆分,却多开了一次「屏幕可能已经不
    /// 归我们」的窗口。过滤掉的命令的 delay_ms 不该有话语权。
    #[test]
    fn discarded_command_delay_does_not_force_multi_step() {
        let mut a = resolved(None);
        a.work_dir = Some("/srv".into());
        a.env = vec![EnvVar {
            key: "A".into(),
            value: "1".into(),
        }];
        a.commands = vec![AutomationCommand {
            text: "   ".into(),
            delay_ms: Some(500),
        }];

        let plan = build_plan(&a, "web01");
        assert_eq!(
            plan.len(),
            1,
            "唯一的命令是空白、已被过滤,不该因为它的 delay_ms 拆成多步"
        );
        assert_eq!(text_of(&plan[0]), "export A='1'; cd '/srv'\r");
        assert_eq!(
            plan[0].delay,
            Duration::from_millis(300),
            "应仍用 initial_delay,而不是被误判为多步计划"
        );
    }

    /// 既有行为钉住(有意设计,不是 bug):没有 export/cd 垫在前面时,首条
    /// 命令自己的 `delay_ms` 会被 `initial_delay` 覆盖——首步等的是「shell
    /// 准备好」,与行间节奏无关,所以只有当 export/cd 占了第 0 步、用户命令
    /// 挪到后面时,他配的延时才生效。这个坑要在 UI 层(P1-b)提示用户,这里
    /// 只是防止以后被误当 bug 改掉。
    #[test]
    fn first_command_delay_ms_is_overridden_by_initial_delay_without_export_or_cd() {
        let mut a = resolved(None);
        a.commands = vec![
            AutomationCommand {
                text: "slow_check".into(),
                delay_ms: Some(9000),
            },
            AutomationCommand {
                text: "pwd".into(),
                delay_ms: None,
            },
        ];

        let plan = build_plan(&a, "web01");
        assert_eq!(plan.len(), 2, "配了逐条延时就拆分");
        assert_eq!(text_of(&plan[0]), "slow_check\r");
        assert_eq!(
            plan[0].delay,
            Duration::from_millis(300),
            "首步一律用 initial_delay,用户为它配的 9000ms 被静默丢弃"
        );
        assert_eq!(text_of(&plan[1]), "pwd\r");
        assert_eq!(
            plan[1].delay,
            Duration::from_millis(200),
            "未设的落 inter_delay"
        );
    }

    /// D2 红线:按实测名接回去的命令**绝不新建会话**。
    ///
    /// 会话已经不在了(远端重启过 / 用户自己 kill 了),正确的表现是命令失败、
    /// 停在裸 shell,而不是凭空造一个同名空会话让用户以为「接回来了」。
    ///
    /// 自证会变红:在 `attach_only_command` 末尾拼回 `|| exec tmux new-session -s {q}`。
    #[test]
    fn the_attach_command_never_creates_a_session() {
        let cmd = attach_only_command("web01", false);
        assert!(
            !cmd.contains("new-session"),
            "只 attach 的命令串里出现了 new-session:{cmd}"
        );
        assert!(cmd.contains("attach"), "总得真的 attach 才行:{cmd}");
    }

    /// D2 的另一半:`has-session` 守门**不能省**。
    ///
    /// 裸的 `exec tmux attach -t X` 在会话不存在时,`exec` 已经把 shell 换成了
    /// tmux 进程,tmux 报错退出 → channel 关闭 → 这块 pane 当场死掉。那样 D4 的
    /// 「挂提示」和 D8 的「停在裸 shell」全部落空 —— shell 都没了。
    /// 有守门则 `&&` 短路,shell 原地活着。
    ///
    /// 自证会变红:把 `attach_only_command` 改成 `format!("exec tmux attach{d} -t {q}")`。
    #[test]
    fn a_failed_attach_leaves_the_shell_alive() {
        let cmd = attach_only_command("web01", false);
        assert!(
            cmd.starts_with("tmux has-session -t "),
            "没有 has-session 守门,attach 失败会连 shell 一起带走:{cmd}"
        );
        assert!(
            cmd.contains("2>/dev/null &&"),
            "守门必须用 `&&` 短路(探测失败就什么都不做):{cmd}"
        );
    }

    /// D5:带不带 `-d` 是**参数**,两种都要真的编进命令串。
    ///
    /// 自证会变红:把 `detach_others` 参数忽略掉、恒不加 `-d`(或恒加)。
    #[test]
    fn the_detach_flag_is_actually_emitted() {
        assert!(attach_only_command("a", true).contains("attach -d -t "));
        assert!(!attach_only_command("a", false).contains(" -d "));
    }

    /// 会话名走 `shell_quote`,不裸拼。远端会话名里带 `'` 或 `$(...)` 时,
    /// 裸拼就是把用户 tmux 会话名当 shell 代码执行。
    ///
    /// 自证会变红:把 `shell_quote(name)` 换成 `name.to_string()`。
    #[test]
    fn the_session_name_is_shell_quoted() {
        let cmd = attach_only_command("it's $(id)", false);
        assert!(
            cmd.contains(r#"'it'\''s $(id)'"#),
            "会话名没走 shell_quote:{cmd}"
        );
        assert!(!cmd.contains("-t it's"), "裸拼进去了:{cmd}");
    }

    /// F161:**实测名不过 `sanitize_tmux_name`**。
    ///
    /// 那个函数是给「拿会话记录的名字去**新建** tmux 会话」用的,它把
    /// `.`/`:`/`$`/`%`/`=`/`@` 换成 `-`。实测名是远端 tmux 自己报上来的
    /// `#S`,本来就合法;再 sanitize 一遍会把 `web@01` 改成 `web-01`,
    /// 于是 attach 到一个根本不存在的名字上。
    ///
    /// 自证会变红:在 `build_plan_attach_measured` 里对 `name` 调一次
    /// `sanitize_tmux_name`。
    #[test]
    fn a_measured_name_is_not_sanitized_because_the_remote_already_vouched_for_it() {
        let a = ResolvedAutomation {
            enabled: true,
            tmux: None,
            commands: Vec::new(),
            work_dir: None,
            env: Vec::new(),
            initial_delay_ms: 300,
            inter_delay_ms: 200,
            ready_timeout_ms: 15_000,
        };
        let steps = build_plan_attach_measured(&a, "web@01", false);
        let line = String::from_utf8(steps[0].bytes.clone()).unwrap();
        assert!(line.contains("'web@01'"), "实测名被改写了:{line}");
    }

    /// 空名字不发命令 —— `tmux attach -t ''` 是一条必然失败的残命令。
    #[test]
    fn an_empty_measured_name_produces_no_plan() {
        let a = ResolvedAutomation {
            enabled: true,
            tmux: None,
            commands: Vec::new(),
            work_dir: None,
            env: Vec::new(),
            initial_delay_ms: 300,
            inter_delay_ms: 200,
            ready_timeout_ms: 15_000,
        };
        assert!(build_plan_attach_measured(&a, "", false).is_empty());
        assert!(build_plan_attach_measured(&a, "   ", false).is_empty());
    }

    /// 自动化总开关关着时,实测 attach 也不发。
    ///
    /// 与 `build_plan`/`build_plan_reattach` 同口径(它们靠 `tmux_session_name`
    /// 里那句 `if !a.enabled` 把关)。用户明确关掉了「登录后自动化」,我们就
    /// 一个字节都不发 —— 哪怕我们自认为这一条对他有好处。
    ///
    /// 自证会变红:删掉 `build_plan_attach_measured` 里的 `if !a.enabled` 早退。
    #[test]
    fn turning_automation_off_also_turns_off_the_measured_attach() {
        let a = ResolvedAutomation {
            enabled: false,
            tmux: None,
            commands: Vec::new(),
            work_dir: None,
            env: Vec::new(),
            initial_delay_ms: 300,
            inter_delay_ms: 200,
            ready_timeout_ms: 15_000,
        };
        assert!(build_plan_attach_measured(&a, "web01", true).is_empty());
    }
}
