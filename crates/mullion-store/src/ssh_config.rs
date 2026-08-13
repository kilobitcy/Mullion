//! F2:解析 OpenSSH 的 `~/.ssh/config`,把里面的主机变成可导入的会话草稿。
//!
//! 零 IO —— 吃 `&str` 吐结构,文件由调用方读。放在 store 而不是 ssh crate:
//! 产物是 `SessionDraft` 一类的东西,只有 store 认识它们(架构不变量:
//! `mullion-ssh` 不认识「会话」这个概念,只认字节流)。
//!
//! **只认 spec F2 点名的六个关键字**(`Host` / `HostName` / `Port` / `User` /
//! `IdentityFile` / `ProxyJump`)。其余一律忽略但**计数**——静默丢弃会让用户
//! 以为整份配置都搬过来了(设计 D2)。

/// 一台从 config 里认出来的主机。字段直接对应那六个关键字。
///
/// 这是**解析结果**,不是会话:`ProxyJump` 还是主机别名,要等落库拿到
/// `SessionId` 才能翻成跳板引用(设计 D4)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEntry {
    /// `Host` 行里的那个别名。会话名用它。
    pub alias: String,
    /// `HostName`;缺省时是别名本身(ssh 的语义)。
    pub hostname: String,
    pub port: u16,
    /// `User`;缺省**留空**——ssh 的缺省是本地登录名,在 Windows 上多半是错的
    /// (设计 D9)。
    pub user: String,
    /// `IdentityFile` 的原始路径。**不读正文**(设计 D5)。
    pub identity_file: Option<String>,
    /// `ProxyJump` 里的主机别名,按拨号顺序。`user@host:port` 只取 host 段。
    pub proxy_jump: Vec<String>,
}

/// 一次解析的全部产出。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedConfig {
    pub hosts: Vec<HostEntry>,
    /// 认得出、但本切片不导入的东西。原样给 UI 显示(设计 D2)。
    pub notes: Vec<SkipNote>,
}

/// 一条「没导入什么」的说明。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipNote {
    /// 不认识的关键字出现了几次。不逐条列 —— 一份真实 config 里
    /// `ServerAliveInterval` 可能出现二十遍,列出来只会淹掉真正要看的那两条。
    UnknownDirectives(usize),
    /// `Include` 认得,但没展开(设计:范围之外)。值是被引用的那一段原文。
    NotIncluded(String),
    /// `Match` 块整块跳过 —— 它的条件依赖 exec/user/localuser,
    /// 语义远超那六个关键字。
    MatchBlock,
    /// 带否定模式的 `Host` 行整块跳过。半对的匹配比不匹配更危险(设计 D3)。
    NegatedPattern(String),
    /// `Port` 不是合法端口。带上别名和原值 —— 用户要回文件里去改的正是这一行。
    BadPort { alias: String, value: String },
}

/// 一个 `Host`(或 `Match`)块:模式 + 这块里认得的键值。
struct Block {
    patterns: Vec<String>,
    /// 只存那六个关键字里出现过的,**块内首次出现的值**(ssh 的
    /// first-obtains 也作用在块内)。
    hostname: Option<String>,
    port: Option<String>,
    user: Option<String>,
    identity_file: Option<String>,
    proxy_jump: Option<String>,
}

/// 解析一份 `ssh_config`。
///
/// 取值按 ssh 的 **first-obtains** 语义:对每个具体主机,按文件顺序扫过所有
/// 匹配它的块,每个关键字取**第一次**拿到的值。`Host *` 通常写在文件末尾当
/// 默认值,正是靠这条语义生效 —— 写成「后面的覆盖前面的」会让它反过来把
/// 具体主机的设置全冲掉(设计 D3)。
pub fn parse(text: &str) -> ParsedConfig {
    let mut out = ParsedConfig::default();
    let mut blocks: Vec<Block> = Vec::new();
    let mut unknown = 0usize;
    // `None` = 还没进任何 Host 块(文件顶部的全局设置)/ 正在一个被跳过的块里。
    let mut cur: Option<usize> = None;
    let mut in_skipped_block = false;

    for raw in text.lines() {
        let Some((key, value)) = split_directive(raw) else {
            continue;
        };
        let lower = key.to_ascii_lowercase();
        match lower.as_str() {
            "host" => {
                let patterns: Vec<String> = value.split_whitespace().map(str::to_string).collect();
                if patterns.iter().any(|p| p.starts_with('!')) {
                    out.notes.push(SkipNote::NegatedPattern(value.to_string()));
                    cur = None;
                    in_skipped_block = true;
                    continue;
                }
                in_skipped_block = false;
                blocks.push(Block {
                    patterns,
                    hostname: None,
                    port: None,
                    user: None,
                    identity_file: None,
                    proxy_jump: None,
                });
                cur = Some(blocks.len() - 1);
            }
            "match" => {
                out.notes.push(SkipNote::MatchBlock);
                cur = None;
                in_skipped_block = true;
            }
            "include" => out.notes.push(SkipNote::NotIncluded(value.to_string())),
            _ => {
                // 被跳过的块里的指令不计入「未导入的关键字」——它们已经由
                // `MatchBlock`/`NegatedPattern` 那条说明覆盖了,再计一遍等于
                // 同一件事报两次。
                if in_skipped_block {
                    continue;
                }
                let Some(i) = cur else {
                    // 文件顶部、任何 Host 块之前的全局设置。ssh 认它,我们
                    // 本切片不认 —— 计数而已。
                    if !is_known(&lower) {
                        unknown += 1;
                    }
                    continue;
                };
                let b = &mut blocks[i];
                // `get_or_insert` 而不是赋值:块内也是 first-obtains。
                match lower.as_str() {
                    "hostname" => set_once(&mut b.hostname, value),
                    "port" => set_once(&mut b.port, value),
                    "user" => set_once(&mut b.user, value),
                    "identityfile" => set_once(&mut b.identity_file, value),
                    "proxyjump" => set_once(&mut b.proxy_jump, value),
                    _ => unknown += 1,
                }
            }
        }
    }

    if unknown > 0 {
        out.notes.push(SkipNote::UnknownDirectives(unknown));
    }

    // 具体别名(不含通配符)才生成会话;通配块只参与取值(设计 D3)。
    for i in 0..blocks.len() {
        for alias in blocks[i].patterns.clone() {
            if is_pattern(&alias) {
                continue;
            }
            match build_entry(&alias, &blocks) {
                Ok(entry) => out.hosts.push(entry),
                Err(note) => out.notes.push(note),
            }
        }
    }
    out
}

/// 把一个具体别名在所有匹配块里的取值合并成一条 `HostEntry`。
fn build_entry(alias: &str, blocks: &[Block]) -> Result<HostEntry, SkipNote> {
    let mut hostname = None;
    let mut port = None;
    let mut user = None;
    let mut identity_file = None;
    let mut proxy_jump = None;
    for b in blocks.iter().filter(|b| matches_any(&b.patterns, alias)) {
        first(&mut hostname, &b.hostname);
        first(&mut port, &b.port);
        first(&mut user, &b.user);
        first(&mut identity_file, &b.identity_file);
        first(&mut proxy_jump, &b.proxy_jump);
    }
    let port = match port {
        None => 22,
        Some(v) => v.parse::<u16>().map_err(|_| SkipNote::BadPort {
            alias: alias.to_string(),
            value: v.clone(),
        })?,
    };
    Ok(HostEntry {
        alias: alias.to_string(),
        // 缺 HostName 就用别名本身 —— ssh 就是这么干的(设计 D9)。
        hostname: hostname.unwrap_or_else(|| alias.to_string()),
        port,
        user: user.unwrap_or_default(),
        identity_file,
        proxy_jump: proxy_jump
            .map(|v| v.split(',').filter_map(jump_host_of).collect())
            .unwrap_or_default(),
    })
}

/// `ProxyJump` 的一跳里取主机段:`user@host:port` → `host`。
///
/// user/port 属于**跳板会话自己**的配置,不是引用点的事 —— 本项目的跳板是
/// 对另一条会话的引用,那条会话有它自己的用户名和端口(设计 D4)。
fn jump_host_of(one: &str) -> Option<String> {
    let s = one.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("none") {
        return None;
    }
    let s = s.rsplit('@').next().unwrap_or(s);
    // IPv6 字面量 `[::1]:22`:先剥方括号,再切端口。不这么做的话
    // `::1` 会被 `split(':')` 切成一堆碎片。
    if let Some(rest) = s.strip_prefix('[') {
        return rest.split(']').next().map(str::to_string);
    }
    Some(s.split(':').next().unwrap_or(s).to_string())
}

fn set_once(slot: &mut Option<String>, value: &str) {
    if slot.is_none() {
        *slot = Some(value.to_string());
    }
}

fn first(slot: &mut Option<String>, candidate: &Option<String>) {
    if slot.is_none() {
        slot.clone_from(candidate);
    }
}

fn is_known(lower: &str) -> bool {
    matches!(
        lower,
        "host" | "hostname" | "port" | "user" | "identityfile" | "proxyjump"
    )
}

/// 含 `*` 或 `?` 就是模式,不是一台机器。
pub fn is_pattern(s: &str) -> bool {
    s.contains('*') || s.contains('?')
}

fn matches_any(patterns: &[String], alias: &str) -> bool {
    patterns.iter().any(|p| glob_match(p, alias))
}

/// ssh 的通配:`*` 任意多个字符,`?` 恰好一个。递归实现 —— 模式短(一份
/// config 里最长也就 `*.example.com` 那种),不值得为它上 DP 表。
fn glob_match(pattern: &str, s: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = s.chars().collect();
    fn go(p: &[char], t: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('*') => go(&p[1..], t) || (!t.is_empty() && go(p, &t[1..])),
            Some('?') => !t.is_empty() && go(&p[1..], &t[1..]),
            Some(c) => t.first() == Some(c) && go(&p[1..], &t[1..]),
        }
    }
    go(&p, &t)
}

/// 一行 → `(关键字, 值)`。注释行/空行返回 `None`。
///
/// ssh 允许 `Key Value`、`Key=Value`、以及两者混着带空格。值里的成对引号
/// 剥掉(`IdentityFile "C:\my keys\id"`),但**不做转义展开** —— 那是 ssh 的
/// 词法细节,导入场景下把路径原样交给用户看更有用。
fn split_directive(raw: &str) -> Option<(&str, &str)> {
    let line = raw.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (key, rest) = match line.find(['=', ' ', '\t']) {
        Some(i) => (&line[..i], &line[i..]),
        None => return None,
    };
    let value = rest.trim_start_matches(['=', ' ', '\t']).trim();
    let value = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value);
    if value.is_empty() {
        return None;
    }
    Some((key, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry<'a>(p: &'a ParsedConfig, alias: &str) -> &'a HostEntry {
        p.hosts
            .iter()
            .find(|h| h.alias == alias)
            .unwrap_or_else(|| panic!("没解析出主机「{alias}」:{:?}", p.hosts))
    }

    /// spec F2 的验收标准原文:给定 fixture,解析结果与预期 struct 相等。
    #[test]
    fn a_plain_host_block_maps_field_by_field() {
        let p = parse(
            "Host prod\n  HostName 192.0.2.10\n  Port 2222\n  User ops\n  \
             IdentityFile ~/.ssh/id_ed25519\n",
        );
        assert_eq!(
            p.hosts,
            vec![HostEntry {
                alias: "prod".into(),
                hostname: "192.0.2.10".into(),
                port: 2222,
                user: "ops".into(),
                identity_file: Some("~/.ssh/id_ed25519".into()),
                proxy_jump: Vec::new(),
            }]
        );
    }

    /// 缺省值按 ssh 语义:没有 `HostName` 就用别名本身,没有 `Port` 就是 22。
    /// 没有 `User` **留空**(设计 D9)——不能拿本地登录名顶,Windows 上那个
    /// 多半是错的,而错的用户名比空的更难查。
    #[test]
    fn missing_fields_fall_back_the_way_ssh_does() {
        let p = parse("Host box\n");
        assert_eq!(
            *entry(&p, "box"),
            HostEntry {
                alias: "box".into(),
                hostname: "box".into(),
                port: 22,
                user: String::new(),
                identity_file: None,
                proxy_jump: Vec::new(),
            }
        );
    }

    /// ssh 是 **first-obtains**,不是「后面的覆盖前面的」。
    ///
    /// 这条是本模块最容易写反的地方:直觉上「后面的更具体所以该赢」,而真实
    /// 语义相反。写反的现象是 `Host *` 里的默认用户名把每台机器自己的用户名
    /// 全冲掉 —— 导进来的会话个个都用错身份登录。
    #[test]
    fn the_first_value_wins_not_the_last() {
        let p = parse("Host prod\n  User ops\nHost *\n  User root\n  Port 2200\n");
        let e = entry(&p, "prod");
        assert_eq!(e.user, "ops", "具体块先出现,它的 User 必须赢");
        assert_eq!(e.port, 2200, "自己没写的字段才落到通配块的默认值");
    }

    /// 通配块本身不是一台机器,不生成会话。
    #[test]
    fn wildcard_blocks_do_not_become_sessions() {
        let p = parse("Host *\n  User root\nHost a.example.com\n");
        assert_eq!(
            p.hosts.iter().map(|h| h.alias.as_str()).collect::<Vec<_>>(),
            vec!["a.example.com"]
        );
    }

    /// 一个 `Host` 行可以带多个别名,每个非通配的都各生成一条。
    #[test]
    fn one_host_line_with_several_aliases_yields_one_session_each() {
        let p = parse("Host web1 web2 *.dev\n  User ops\n");
        assert_eq!(
            p.hosts.iter().map(|h| h.alias.as_str()).collect::<Vec<_>>(),
            vec!["web1", "web2"]
        );
        assert!(p.hosts.iter().all(|h| h.user == "ops"));
    }

    /// `ProxyJump` 留成别名,多跳按顺序;`user@host:port` 只取 host 段。
    #[test]
    fn proxy_jump_keeps_aliases_in_dial_order() {
        let p = parse("Host target\n  ProxyJump me@bastion:2222,inner\n");
        assert_eq!(entry(&p, "target").proxy_jump, vec!["bastion", "inner"]);
    }

    /// `ProxyJump none` 是「显式不走跳板」,不是一台叫 none 的机器。
    #[test]
    fn proxy_jump_none_means_no_jump_at_all() {
        let p = parse("Host direct\n  ProxyJump none\n");
        assert!(entry(&p, "direct").proxy_jump.is_empty());
    }

    /// 不认识的关键字不报错、不阻断,但**必须计数** —— 静默丢弃会让用户
    /// 以为整份配置都搬过来了(设计 D2)。
    #[test]
    fn unknown_directives_are_counted_not_silently_dropped() {
        let p =
            parse("Host box\n  ServerAliveInterval 30\n  Compression yes\n  ForwardAgent yes\n");
        assert!(
            p.notes.contains(&SkipNote::UnknownDirectives(3)),
            "三条没导入的指令必须报出来:{:?}",
            p.notes
        );
    }

    /// `Include` 认得但没展开,单独一条说明 —— 与「不认识」不是一回事:
    /// 用户会想去看被 include 的那个文件里还有什么。
    #[test]
    fn include_is_reported_as_not_expanded() {
        let p = parse("Include ~/.ssh/conf.d/*\nHost box\n");
        assert!(
            p.notes
                .iter()
                .any(|n| matches!(n, SkipNote::NotIncluded(v) if v == "~/.ssh/conf.d/*")),
            "{:?}",
            p.notes
        );
    }

    /// `Match` 块整块跳过,且**块内的指令不再计进「不认识的关键字」** ——
    /// 同一件事报两次会让用户去数一个对不上的数。
    #[test]
    fn a_match_block_is_skipped_whole() {
        let p = parse("Match host bar exec true\n  User root\n  Compression yes\n");
        assert!(p.hosts.is_empty());
        assert!(p.notes.contains(&SkipNote::MatchBlock), "{:?}", p.notes);
        assert!(
            !p.notes
                .iter()
                .any(|n| matches!(n, SkipNote::UnknownDirectives(_))),
            "被跳过的块里的指令不该再计一遍:{:?}",
            p.notes
        );
    }

    /// 否定模式整块跳过:`Host !dev *` 的语义是「除 dev 外的一切」,
    /// 按普通通配处理会把设置错加到 dev 头上(设计 D3)。
    #[test]
    fn a_negated_host_pattern_is_skipped_whole() {
        let p = parse("Host !dev *\n  User root\nHost dev\n");
        assert_eq!(entry(&p, "dev").user, "", "否定块的 User 不该漏到 dev 上");
        assert!(
            p.notes
                .iter()
                .any(|n| matches!(n, SkipNote::NegatedPattern(_))),
            "{:?}",
            p.notes
        );
    }

    /// 端口非法 → 该主机**不生成会话**(造不出合法的 `Connection`),
    /// 并报出别名和原值 —— 用户要回文件里改的正是这一行。
    #[test]
    fn a_bad_port_drops_that_host_and_says_which_one() {
        let p = parse("Host oops\n  Port 99999\n");
        assert!(p.hosts.is_empty());
        assert!(
            p.notes.contains(&SkipNote::BadPort {
                alias: "oops".into(),
                value: "99999".into()
            }),
            "{:?}",
            p.notes
        );
    }

    /// `Key=Value`、多余空白、注释、行内引号:真实 config 里都有。
    #[test]
    fn the_lexer_survives_real_world_formatting() {
        let p = parse(
            "# 注释\n\nHost   spaced\n\tHostName=10.0.0.1\n  Port\t= 2200\n  \
             IdentityFile \"~/my keys/id\"\n",
        );
        let e = entry(&p, "spaced");
        assert_eq!(e.hostname, "10.0.0.1");
        assert_eq!(e.port, 2200);
        assert_eq!(e.identity_file.as_deref(), Some("~/my keys/id"));
    }

    /// 块内也是 first-obtains:同一个块里写两遍 `User`,第一遍赢。
    #[test]
    fn a_repeated_key_inside_one_block_keeps_the_first() {
        let p = parse("Host box\n  User first\n  User second\n");
        assert_eq!(entry(&p, "box").user, "first");
    }

    /// `*` 与 `?` 的匹配语义。写错的现象是 `Host *.example.com` 匹不上
    /// 或匹过头,而那正是「默认值到底套没套上」的关键。
    #[test]
    fn glob_matching_follows_ssh_semantics() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*.example.com", "a.example.com"));
        assert!(!glob_match("*.example.com", "example.com"));
        assert!(glob_match("web?", "web1"));
        assert!(!glob_match("web?", "web12"));
        assert!(glob_match("a*c", "abbbc"));
        assert!(!glob_match("a*c", "abbb"));
    }
}
