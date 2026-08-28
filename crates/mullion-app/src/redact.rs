//! 日志脱敏(F155):把主机名、用户名、IP、路径换成**稳定假名**,得出一份
//! 可以对外发送的副本。
//!
//! **零 IO、零 UI**,纯函数,可纯单测。真正读写文件在 `app.rs`。
//!
//! ## 为什么假名必须稳定
//!
//! 同一台机器在整份日志里始终是 `host#1`。换成随机串或逐行编号的话,
//! 「同一台机器这一小时重连了 17 次」这类模式就看不出来了 —— 而那正是
//! 拿日志找优化方向的主要用途。
//!
//! ## 这不是一个安全边界
//!
//! 替换是**按模式匹配**做的:IPv4、`user@host`、Windows 盘符路径、Unix
//! 绝对路径。覆盖不到的写法(比如一条被拆成两半的主机名、或第三方 crate
//! 用我们没预料到的格式打出来的地址)会漏。UI 上那句「发送前请自己再看
//! 一眼」不是免责话术,是这个模块能力边界的如实陈述 —— 给用户一个
//! 「导出即安全」的印象,比不提供这个功能更糟。

use std::collections::HashMap;

/// 一次导出期间的假名表。
#[derive(Debug, Default)]
pub struct Redactor {
    map: HashMap<String, String>,
    counts: HashMap<&'static str, usize>,
}

impl Redactor {
    pub fn new() -> Self {
        Self::default()
    }

    /// 取(或分配)`raw` 的假名。同一个 `raw` 永远得到同一个假名。
    fn alias(&mut self, kind: &'static str, raw: &str) -> String {
        if let Some(a) = self.map.get(raw) {
            return a.clone();
        }
        let n = self.counts.entry(kind).or_insert(0);
        *n += 1;
        let alias = format!("{kind}#{n}");
        self.map.insert(raw.to_string(), alias.clone());
        alias
    }

    /// 脱敏一行。
    pub fn line(&mut self, s: &str) -> String {
        let s = self.replace_user_at_host(s);
        let s = self.replace_ipv4(&s);
        let s = self.replace_windows_path(&s);
        let s = self.replace_unix_path(&s);
        self.replace_known_bare_tokens(&s)
    }

    /// 之前在别的行里(经 `user@host`/IP/路径这些"有上下文标记"的写法)
    /// 见过的原始值,哪怕这一行里原样裸露出现、没有任何标记,也要换成
    /// 同一个假名 —— 不然「同一台机器这一小时重连了 17 次」这类跨行模式,
    /// 一旦某一行只写了裸主机名就串不起来了。
    fn replace_known_bare_tokens(&mut self, s: &str) -> String {
        if self.map.is_empty() {
            return s.to_string();
        }
        let cs: Vec<char> = s.chars().collect();
        let mut out = String::with_capacity(s.len());
        let mut i = 0;
        while i < cs.len() {
            if is_host(cs[i]) && (i == 0 || !is_host(cs[i - 1])) {
                let start = i;
                let mut j = i;
                while j < cs.len() && is_host(cs[j]) {
                    j += 1;
                }
                let cand: String = cs[start..j].iter().collect();
                if self.map.contains_key(&cand) {
                    let alias = self.alias("host", &cand);
                    out.push_str(&alias);
                    i = j;
                    continue;
                }
                out.extend(&cs[start..j]);
                i = j;
                continue;
            }
            out.push(cs[i]);
            i += 1;
        }
        out
    }

    /// `user@host` / `user@host:port` → `user#1@host#1`(端口保留 —— 端口
    /// 不是私密信息,而「同一台机器的 22 与 2222」是有诊断价值的区分)。
    fn replace_user_at_host(&mut self, s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(at) = rest.find('@') {
            let (head, tail) = rest.split_at(at);
            let user_start = head.rfind(|c: char| !is_ident(c)).map_or(0, |i| {
                i + head[i..].chars().next().map_or(1, char::len_utf8)
            });
            let user = &head[user_start..];
            let after = &tail[1..];
            let host_end = after.find(|c: char| !is_host(c)).unwrap_or(after.len());
            let host = &after[..host_end];
            if !looks_like_user_at_host(user, host) {
                out.push_str(&rest[..at + 1]);
                rest = &rest[at + 1..];
                continue;
            }
            out.push_str(&head[..user_start]);
            let ua = self.alias("user", user);
            let ha = self.alias("host", host);
            out.push_str(&ua);
            out.push('@');
            out.push_str(&ha);
            rest = &after[host_end..];
        }
        out.push_str(rest);
        out
    }

    /// 裸 IPv4 → `host#N`。
    fn replace_ipv4(&mut self, s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let bytes: Vec<char> = s.chars().collect();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i].is_ascii_digit() {
                let start = i;
                let mut j = i;
                while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == '.') {
                    j += 1;
                }
                let cand: String = bytes[start..j].iter().collect();
                if is_ipv4(&cand) {
                    let a = self.alias("host", &cand);
                    out.push_str(&a);
                    i = j;
                    continue;
                }
                out.extend(&bytes[start..j]);
                i = j;
                continue;
            }
            out.push(bytes[i]);
            i += 1;
        }
        out
    }

    /// `C:\Users\alice\...` → `path#N`。
    fn replace_windows_path(&mut self, s: &str) -> String {
        self.replace_runs(s, |cs, i| {
            if i + 2 < cs.len()
                && cs[i].is_ascii_alphabetic()
                && cs[i + 1] == ':'
                && cs[i + 2] == '\\'
            {
                let mut j = i + 3;
                while j < cs.len() && !cs[j].is_whitespace() {
                    j += 1;
                }
                Some(j)
            } else {
                None
            }
        })
    }

    /// `/home/alice/...` → `path#N`。**只吃两段以上的绝对路径**:
    /// 单段的 `/tmp`、`/etc` 之类没有私密信息,换掉只会让日志更难读。
    fn replace_unix_path(&mut self, s: &str) -> String {
        self.replace_runs(s, |cs, i| {
            if cs[i] != '/' || (i > 0 && !is_boundary(cs[i - 1])) {
                return None;
            }
            let mut j = i + 1;
            let mut slashes = 0;
            while j < cs.len() && !cs[j].is_whitespace() && cs[j] != '"' && cs[j] != ',' {
                if cs[j] == '/' {
                    slashes += 1;
                }
                j += 1;
            }
            (slashes >= 1 && j > i + 2).then_some(j)
        })
    }

    /// 扫一遍字符，把 `probe` 认出来的整段换成 `path#N`。
    fn replace_runs(&mut self, s: &str, probe: impl Fn(&[char], usize) -> Option<usize>) -> String {
        let cs: Vec<char> = s.chars().collect();
        let mut out = String::with_capacity(s.len());
        let mut i = 0;
        while i < cs.len() {
            if let Some(end) = probe(&cs, i) {
                let raw: String = cs[i..end].iter().collect();
                let a = self.alias("path", &raw);
                out.push_str(&a);
                i = end;
                continue;
            }
            out.push(cs[i]);
            i += 1;
        }
        out
    }
}

/// F180:这个 `@` 两边真的是一对「用户名 @ 主机名」吗。
///
/// **这道判据存在的理由是自家格式与自家启发式同形。** `profile.pane` 的段是
/// `in=<字节量>@<脏带表>/<总带数>`(F173),`in=0B@-/6` 与 `in=6.5KB@b11,b23/24`
/// 长得就是 `user@host` —— 实测 34 分钟的日志里,`0B`/`6.5KB` 被收成用户假名、
/// 空脏带占位符 `-` 与带号 `b5` 被收成主机假名,184 处 `wev=-`、184 处 `dirty=-`
/// 全变成同一个 `host#2`,**凭空多出一台主机**;而 `-` 恰恰是三处埋点明定的
/// 「这一栏是空的」占位符,脱敏正好摧毁了它存在的理由。
///
/// **不匹配时整对放行,不许只否掉一边**:一边像字节量就说明这个 `@` 压根不是
/// 地址,另一边的 `b11` 同样是带号而不是主机名。分开判的话带号照样会被收进
/// 假名表,换汤不换药。
///
/// **单字符 token 一律不收**:即便真有一台单字母主机,泄露量是零,而它与
/// 占位符 / 分隔符在字符层面不可区分 —— 宁可漏一个零信息量的字符,也不能
/// 把占位符换成假主机名。
fn looks_like_user_at_host(user: &str, host: &str) -> bool {
    collectible(user) && collectible(host) && !is_byte_magnitude(user)
}

/// 值得进假名表的 token:两个字符以上,且至少含一个 ASCII 字母数字。
fn collectible(t: &str) -> bool {
    t.chars().count() >= 2 && t.chars().any(|c| c.is_ascii_alphanumeric())
}

/// `0B` / `6.5KB` / `180MB` —— 我们自己打出来的字节量。
///
/// **只堵 `-` 是不够的**:字节量每个窗口都不一样,每出现一个新值就多分配一个
/// 用户假名,实测日志里 `user#265`→`#266`→`#267` 逐窗口递增。**假名编号单调
/// 跑飞本身就是模式匹配抓错东西的信号。**
fn is_byte_magnitude(t: &str) -> bool {
    let digits_end = t
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(t.len());
    let (num, unit) = t.split_at(digits_end);
    !num.is_empty() && num.parse::<f64>().is_ok() && matches!(unit, "B" | "KB" | "MB" | "GB" | "TB")
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'
}

fn is_host(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '.'
}

fn is_boundary(c: char) -> bool {
    c.is_whitespace() || c == '=' || c == ':' || c == '(' || c == '"' || c == ','
}

fn is_ipv4(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 3 && p.parse::<u8>().is_ok())
}

/// 导出文件开头那段说明。**必须有** —— 收到这份日志的人(包括未来的你)
/// 要知道里面的 `host#1` 是假名,以及脱敏是尽力而为的。
pub fn header() -> String {
    "# 这是一份脱敏副本:主机名/用户名/IP/路径已替换成稳定假名(同一个真实值\n\
     # 在整份文件里始终是同一个假名)。替换按模式匹配进行,覆盖不到的写法会漏 ——\n\
     # 对外发送前请自己再扫一眼。\n"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 假名必须**稳定**:同一台机器在整份日志里始终是同一个 `host#N`。
    ///
    /// 不稳定的话,「同一台机器这一小时重连了 17 次」这类模式就看不出来了 ——
    /// 而那正是拿日志找优化方向的主要用途。
    ///
    /// 自证会变红:把 `alias` 里查表那一句删掉(每次都新分配一个编号)。
    #[test]
    fn the_same_host_gets_the_same_alias_every_time() {
        let mut r = Redactor::new();
        let a = r.line("连接 alice@prod-web-01:22 成功");
        let b = r.line("prod-web-01 断线,退避重连");
        let host = a
            .split('@')
            .nth(1)
            .and_then(|s| s.split(':').next())
            .expect("该有 host 假名");
        assert!(b.contains(host), "同一台机器两次拿到了不同假名:\n{a}\n{b}");
    }

    /// 真实信息必须真的不见了。这条是这个模块的**存在理由**,
    /// 松了它整个功能就是装样子。
    #[test]
    fn the_real_values_are_actually_gone() {
        let mut r = Redactor::new();
        let out =
            r.line("连接 alice@prod-web-01:2222 (10.20.30.40) 私钥 C:\\Users\\alice\\id_ed25519");
        for leaked in ["alice", "prod-web-01", "10.20.30.40", "id_ed25519"] {
            assert!(!out.contains(leaked), "「{leaked}」漏出去了:\n{out}");
        }
        assert!(out.contains("user#1"), "没换成假名:{out}");
        assert!(out.contains("host#"), "没换成假名:{out}");
        assert!(out.contains("path#1"), "没换成假名:{out}");
    }

    /// 端口保留 —— 它不是私密信息,而「同一台机器的 22 与 2222」是有诊断
    /// 价值的区分(TOFU 键带端口那一片刚踩过)。
    #[test]
    fn the_port_survives_because_it_is_not_secret() {
        let mut r = Redactor::new();
        let out = r.line("bob@nas:2222");
        assert!(out.contains(":2222"), "端口被一起吃掉了:{out}");
    }

    /// 不同的机器必须拿到**不同**的假名。全都换成 `host#1` 的话,
    /// 「三台机器轮流断线」会看起来像「一台机器断了三次」。
    #[test]
    fn different_hosts_get_different_aliases() {
        let mut r = Redactor::new();
        let out = r.line("ann@one 与 bob@two");
        assert!(out.contains("host#1") && out.contains("host#2"), "{out}");
        assert!(out.contains("user#1") && out.contains("user#2"), "{out}");
    }

    /// F180 的**已知代价**,明写出来免得以后被当成 bug 报回来:单字符
    /// 用户名/主机名不再换假名。真实 SSH 用户名不会只有一个字符,而单个
    /// 字符的泄露量是零;换来的是占位符 `-`、带号、字节量不再被冒充成主机。
    #[test]
    fn a_single_letter_name_is_left_alone_and_that_is_the_deliberate_trade() {
        let mut r = Redactor::new();
        assert_eq!(r.line("a@one"), "a@one");
    }

    /// 剖面行里全是数字,不该被误伤 —— 误伤了的话这份副本就没法用来
    /// 找性能问题了,而那是导出它的**唯一目的**。
    ///
    /// 自证会变红:把 `is_ipv4` 的 `parts.len() == 4` 改成 `>= 2`。
    #[test]
    fn a_profile_line_of_pure_numbers_is_left_alone() {
        let mut r = Redactor::new();
        let line = "profile 5.0s frame=300x/p50=8.0ms/p95=16.5ms present=298 skip=0 \
                    in=1024.0KB/s mem=180MB";
        assert_eq!(r.line(line), line, "剖面行被误伤了");
    }

    /// 版本号、时间戳这类点分数字不是 IP。
    #[test]
    fn dotted_numbers_that_are_not_addresses_are_left_alone() {
        let mut r = Redactor::new();
        assert_eq!(r.line("mullion 0.1.65 启动"), "mullion 0.1.65 启动");
        assert_eq!(r.line("耗时 1.5s"), "耗时 1.5s");
        // 256 不是合法的 IPv4 段。
        assert_eq!(r.line("256.1.1.1"), "256.1.1.1");
    }

    /// 单段的 `/tmp`、`/etc` 不换 —— 它们没有私密信息,换掉只会让日志更难读。
    #[test]
    fn short_generic_paths_stay_readable() {
        let mut r = Redactor::new();
        assert_eq!(r.line("写入 /tmp"), "写入 /tmp");
        assert!(r.line("读取 /home/alice/.ssh/config").contains("path#1"));
    }

    /// **F180:我们自己打出来的剖面行,脱敏后必须逐字节不变。**
    ///
    /// 判据不是手编的字符串,而是**生产渲染器的真实输出** —— 手编的行踩不中
    /// 「自家格式与自家启发式同形」这种碰撞,那正是这个 bug 从 F155 一路活到
    /// F173 都没被既有测试抓住的原因(既有的
    /// `a_profile_line_of_pure_numbers_is_left_alone` 用的就是手编行,而且恰好
    /// 挑了一条不含 `@` 的)。这条测试也因此不会随格式演进而失效:`profile.pane`
    /// 以后再加字段,它跟着变。
    ///
    /// 自证会变红:把 `looks_like_user_at_host` 换回原来的
    /// `!user.is_empty() && !host.is_empty()`。
    #[test]
    fn every_line_our_own_profiler_emits_survives_redaction_byte_for_byte() {
        use crate::profile::{PaneDetail, Snapshot};
        let mut r = Redactor::new();
        // 三种真实形态:空脏带(`@-/24`)、单带、多带 —— 前两种是实测里被
        // 吃掉的那两种(184 处 `-` 和一批带号变成了假主机)。
        for (bands, total, bytes) in [(0u64, 24u32, 0u64), (1 << 5, 24, 6656), (0b1010, 24, 512)] {
            let mut s = Snapshot::empty();
            s.window_ms = 5_000;
            s.frames = 5;
            s.cpu_bp = Some(63);
            s.main_cpu_bp = Some(240);
            s.cpu_cores = 16;
            s.mem_process_mb = 180;
            s.thread_available = true;
            s.thread_groups = vec![("tokio", 3_100), ("其他", 900)];
            s.thread_unmapped = vec![("wgpu-poll".to_string(), 500)];
            s.pane_detail = vec![PaneDetail {
                id: 1,
                in_bytes: bytes,
                dirty_bands: bands,
                band_total: total,
                frames: 5,
            }];
            for line in crate::profile::render_lines(&s, true) {
                assert_eq!(
                    r.line(&line),
                    line,
                    "剖面行被脱敏器改写了 —— 这份副本的唯一用途就是读这些行"
                );
            }
        }
        assert!(
            r.map.is_empty(),
            "剖面行里不该有任何东西值得起假名,却收了:{:?}",
            r.map
        );
    }

    /// F180:`-` 是三处埋点明定的空表占位符,一旦被收进假名表,
    /// `replace_known_bare_tokens` 会把**整份日志里所有孤立的 `-`** 换掉。
    ///
    /// 自证会变红:把 `collectible` 的 `>= 2` 改成 `>= 1`。
    #[test]
    fn the_empty_table_placeholder_never_becomes_a_fake_host() {
        let mut r = Redactor::new();
        let pane = r.line("profile.pane p0 in=0B@-/24 frames=3");
        assert_eq!(pane, "profile.pane p0 in=0B@-/24 frames=3");
        // 就算它先在别处出现过,后面那些 `dirty=-`/`wev=-` 也必须留着 ——
        // 「后面什么都没有」= 埋点根本没接上,与「这一窗口确实一次没响」
        // 必须在日志里长得不一样。
        let overview = r.line("wake=1x/rr=sched:0,evt:1 dirty=- wev=- curdup=0");
        assert!(
            overview.contains("dirty=- wev=-"),
            "空表占位符被换成了假主机名:{overview}"
        );
    }

    /// 说明头必须点明「这是假名」且「可能漏」。收到日志的人要知道自己
    /// 手上是什么东西。
    #[test]
    fn the_header_says_it_is_aliased_and_best_effort() {
        let h = header();
        assert!(h.contains("假名"));
        assert!(h.contains("会漏"));
    }
}
