//! ⑥:从 pane 自己的字节流里认出「当前目录」和「tmux 会话名」。
//!
//! 为什么只能从字节流拿:adr-009 一条 SSH 连接承载所有分屏,旁路 exec 通道的
//! `$SSH_CONNECTION` 四元组完全相同,分不出是哪块分屏;`channel.set_env` 又受
//! sshd `AcceptEnv` 限制(默认只放 `LANG`/`LC_*`)。所以只剩 OSC。
//!
//! 两条腿:
//! - **OSC 7**(`ESC ] 7 ; file://host/path BEL|ST`):现代 shell 上报 cwd 的
//!   标准做法。alacritty 0.26 **不解析**它(`ansi::Handler` 里没有
//!   `set_current_directory`),所以我们自己在 [`crate::emulator::Emulator::feed`]
//!   里扫一遍。
//! - **OSC 0/2**(窗口标题):alacritty 已经解析成 `Event::Title`。tmux 开了
//!   `set-titles on` 之后会按 `#S:#I:#W` 发,会话名就在第一段。
//!
//! **cwd 以 OSC 7 为准**:它是路径本身;标题里那个是给人看的(带 `~` 缩写、
//! 可能被 shell 截断)。远端要怎么配见 `docs/remote-state-setup.md`。

/// 一次采集到的远端状态。**拿不到就是 `None`,不猜、不填占位** ——
/// ② 会拿 `cwd` 去当 SFTP 起始目录,猜错比不显示危险得多。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteState {
    /// 当前目录。**字节**语义:远端文件名不保证是 UTF-8,而这个值最终要拼成
    /// SFTP 路径(`mullion-ssh` 的 `RemotePath` 也是字节)。这里不引
    /// `RemotePath` —— 依赖方向是 `app → {core, term, ssh, store}`,
    /// term 不认识 ssh。
    ///
    /// **来自 `parse_title` 的值可能在空格处被截断**(它用空白分隔 token 找
    /// 路径,远端目录名含空格时会截断到第一个空格),但截断后仍是一个外观
    /// 合法的绝对路径,骗得过「是不是绝对路径」这类下游校验。所以这个来源
    /// 的 `cwd` **只可用于展示末级目录名,禁止直接当 SFTP 起点**;需要精确
    /// 路径时只认 `parse_osc7` 那条腿 —— 它给的是路径本身,不会被空格截断。
    pub cwd: Option<Vec<u8>>,
    /// tmux 会话名。
    pub tmux: Option<String>,
    /// 这一批里收到过新标题没有。
    ///
    /// 收到了的话 `tmux` 就是**权威值,包括「没有」**:用户退出 tmux 之后
    /// bash 会发自己的标题,这时必须把会话名清掉,否则标题条上会永久挂着一个
    /// 已经不存在的 tmux 名。`cwd` 不吃这一套 —— 它只增不清,拿不到新值时
    /// 保留上一个已知值比闪成「未知」有用。
    pub title_seen: bool,
}

/// 一条未完成的 OSC 最多攒多少字节。超了就丢弃当前这条并回到「找 `ESC ]`」
/// 状态 —— 没有上限的话,一个畸形的流(有 `ESC ] 7 ;` 却永远不发终止符)
/// 会让我们无界增长。
pub const MAX_PENDING: usize = 4096;

/// 解析 OSC 7 的载荷(`7;` 之后那一段)。
///
/// 形如 `file://hostname/path`;主机名段**忽略** —— 远端自己报的名字对我们
/// 没用,而且在 tmux/容器里经常是错的。
///
/// 不是 `file://` 开头、或者主机名段之后没有 `/` 的,一律 `None`。
pub fn parse_osc7(payload: &[u8]) -> Option<Vec<u8>> {
    let rest = payload.strip_prefix(b"file://")?;
    let slash = rest.iter().position(|&b| b == b'/')?;
    Some(percent_decode(&rest[slash..]))
}

/// 按**字节**解 `%XX`。残缺的转义原样留着 —— `%` 是合法文件名字符,
/// 吞掉它会把路径改成一个不存在的目录。
fn percent_decode(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' && i + 2 < s.len() {
            if let (Some(h), Some(l)) = (hex(s[i + 1]), hex(s[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// 从窗口标题里认 tmux 会话名和 cwd。
///
/// 规则:
/// 1. **tmux 会话名**:标题形如 `<非空白>:<纯数字>:…` 就取第一段。tmux 的
///    默认 `set-titles-string` 是 `#S:#I:#W`(如 `main:0:bash`)。第二段
///    必须是纯数字 —— 否则 Ubuntu 默认 bash 的 `user@host: ~/dir` 会被当成
///    会话名 `user@host`,用户没开 tmux 却看到一个会话名。
/// 2. **cwd**:第一个以 `/`、`~/` 开头或恰好是 `~` 的空白分隔 token。
///    `~` **不展开** —— 展开需要知道远端的 `$HOME`,而我们不知道;调用方
///    (标题条)只拿它取最后一级目录名,② 那边则明确只接受绝对路径。
pub fn parse_title(title: &str) -> RemoteState {
    let mut out = RemoteState {
        title_seen: true,
        ..RemoteState::default()
    };
    if let Some((name, rest)) = title.split_once(':') {
        let second_is_index = rest
            .split_once(':')
            .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if !name.is_empty() && !name.contains(char::is_whitespace) && second_is_index {
            out.tmux = Some(name.to_string());
        }
    }
    out.cwd = title
        .split_whitespace()
        .find(|tok| tok.starts_with('/') || tok.starts_with("~/") || *tok == "~")
        .map(|tok| tok.as_bytes().to_vec());
    out
}

/// OSC 7 嗅探器。**有状态**:一条 OSC 可能被 TCP 切在任意字节位置(高延迟
/// 链路上是常态,正是本项目的主场景),所以未完成的前缀要留着。
#[derive(Debug, Default)]
pub struct Osc7Sniffer {
    /// `None` = 不在一条 OSC 里面。`Some(buf)` = 正在攒载荷(不含 `ESC ]`)。
    pending: Option<Vec<u8>>,
    /// 上一个字节是不是 `ESC`。认 `ESC ]`(开头)和 `ESC \`(ST 结尾)都要
    /// 它 —— 这两个二字节序列本身就可能跨 `feed` 被切开。
    saw_esc: bool,
}

impl Osc7Sniffer {
    /// 喂一段字节,返回这一段里**最后一条**完整 OSC 7 给出的路径。
    ///
    /// 只认 `7;` 开头的;其余(标题的 `0;`/`2;`、调色板的 `4;`……)攒到终止符
    /// 就丢 —— 它们由 alacritty 那条腿处理,我们只是路过。
    pub fn feed(&mut self, bytes: &[u8]) -> Option<Vec<u8>> {
        let mut found = None;
        for &b in bytes {
            match &mut self.pending {
                None => {
                    if self.saw_esc && b == b']' {
                        self.pending = Some(Vec::new());
                    }
                    self.saw_esc = b == 0x1b;
                }
                Some(buf) => {
                    if b == 0x07 || (self.saw_esc && b == b'\\') {
                        if self.saw_esc {
                            buf.pop(); // 把上一轮攒进去的 ESC 拿掉
                        }
                        if let Some(p) = buf.strip_prefix(b"7;").and_then(parse_osc7) {
                            found = Some(p);
                        }
                        self.pending = None;
                        self.saw_esc = false;
                        continue;
                    }
                    buf.push(b);
                    self.saw_esc = b == 0x1b;
                    if buf.len() > MAX_PENDING {
                        self.pending = None;
                        self.saw_esc = false;
                    }
                }
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc7_yields_the_path_and_ignores_the_hostname() {
        assert_eq!(
            parse_osc7(b"file://build-01/home/dev/Mullion").as_deref(),
            Some(&b"/home/dev/Mullion"[..])
        );
        // 主机名段空着也合法(shell 拿不到 hostname 时会这么发)。
        assert_eq!(parse_osc7(b"file:///tmp").as_deref(), Some(&b"/tmp"[..]));
    }

    /// 百分号转义按**字节**解码。先转成 `String` 再解码会在非 UTF-8 路径上
    /// 炸掉(而路径里的非 ASCII 恰恰就是以 `%XX` 编码进来的)。
    #[test]
    fn osc7_percent_escapes_are_decoded_as_bytes() {
        assert_eq!(
            parse_osc7(b"file://h/tmp/a%20b").as_deref(),
            Some(&b"/tmp/a b"[..])
        );
        assert_eq!(
            parse_osc7(b"file://h/%E4%B8%AD").as_deref(),
            Some("/中".as_bytes())
        );
        // 残缺的转义原样留着,不吞字节 —— `%` 是合法文件名字符。
        assert_eq!(parse_osc7(b"file://h/a%").as_deref(), Some(&b"/a%"[..]));
        assert_eq!(parse_osc7(b"file://h/a%zz").as_deref(), Some(&b"/a%zz"[..]));
    }

    /// 认不出来就 `None`。**宁可不显示,也不要显示一个错的目录** ——
    /// ② 会拿它当 SFTP 起始目录。
    ///
    /// 自证会变红:把 `strip_prefix(b"file://")?` 换成
    /// `strip_prefix(b"file://").unwrap_or(payload)`。
    #[test]
    fn a_malformed_osc7_is_rejected_rather_than_guessed() {
        assert_eq!(parse_osc7(b""), None);
        assert_eq!(parse_osc7(b"file:/tmp"), None);
        assert_eq!(parse_osc7(b"/tmp"), None);
        assert_eq!(parse_osc7(b"file://hostname-with-no-path"), None);
    }

    /// tmux 的默认 `set-titles-string` 是 `#S:#I:#W`。
    #[test]
    fn a_tmux_default_title_gives_up_the_session_name() {
        let st = parse_title("main:0:bash");
        assert_eq!(st.tmux.as_deref(), Some("main"));
        assert_eq!(st.cwd, None, "默认串里没有路径,不该凭空造一个");
    }

    /// **第二段必须是纯数字**,否则 Ubuntu 默认 bash 的 `user@host: ~/dir`
    /// 会被当成 tmux 会话名 `user@host` —— 标题条上永久挂一个不存在的会话名,
    /// 而用户根本没开 tmux。
    ///
    /// 注:`"dev@build-01: ~/Mullion"` 只有一个冒号,`split_once(':')` 之后
    /// `rest` 里已经没有第二个冒号,`second_is_index` 无论数字判定在不在都是
    /// `false` —— 这条用例本身并不能钉住 `is_ascii_digit()`。真正钉住数字
    /// 判定的是下面 `a_bash_title_with_a_colon_in_the_path_is_not_mistaken_either`
    /// 那条(标题里有两个冒号、第二段非数字)。
    #[test]
    fn a_plain_bash_title_is_not_mistaken_for_a_tmux_session() {
        let st = parse_title("dev@build-01: ~/Mullion");
        assert_eq!(st.tmux, None);
        assert_eq!(st.cwd.as_deref(), Some("~/Mullion".as_bytes()));
    }

    /// 钉住「第二段必须是纯数字」:标题里两个冒号都占了,第二段是路径的一部分
    /// (`~/a`)而不是数字,不能被当成 tmux 的 `#I` 索引。
    ///
    /// 自证会变红:把 `parse_title` 里 `is_ascii_digit()` 那个条件删掉。
    #[test]
    fn a_bash_title_with_a_colon_in_the_path_is_not_mistaken_either() {
        let st = parse_title("dev@h: ~/a:b");
        assert_eq!(st.tmux, None, "路径里带冒号是合法的,不该被当成会话名");
    }

    #[test]
    fn a_title_carrying_an_absolute_path_gives_up_the_cwd() {
        let st = parse_title("main:0:bash /home/dev/Mullion");
        assert_eq!(st.tmux.as_deref(), Some("main"));
        assert_eq!(st.cwd.as_deref(), Some(&b"/home/dev/Mullion"[..]));
    }

    /// **这是已知降级,不是期望行为**:`parse_title` 用空白分隔 token 找路径,
    /// 远端目录名含空格时会截断到第一个空格 —— 但截断后仍是一个外观合法的
    /// 绝对路径,骗得过「是不是绝对路径」这类下游校验(见 [`RemoteState::cwd`]
    /// 字段文档)。之所以可接受:OSC 7(`parse_osc7`)优先且不会被截断,
    /// 这条腿给出的 `cwd` 只用于展示。
    #[test]
    fn a_space_in_the_title_path_truncates_it_which_is_why_osc7_wins() {
        let st = parse_title("main:0:bash /home/dev/My Documents");
        assert_eq!(st.cwd.as_deref(), Some(&b"/home/dev/My"[..]));
    }

    /// `title_seen` 恒为 `true` 是 `parse_title` 的语义:只要收到了一条标题,
    /// 这件事本身就是信息 —— 哪怕标题里既没有 tmux 名也没有 cwd,`tmux`
    /// 这一路仍然是权威值(用户退出 tmux 之后 bash 发的标题就长这样,必须
    /// 拿它把上一个已知会话名清掉)。所以这里不能拿 `RemoteState::default()`
    /// 比较 —— default 的 `title_seen` 是 `false`。
    #[test]
    fn a_title_with_neither_gives_up_nothing() {
        let expect_neither = RemoteState {
            title_seen: true,
            ..RemoteState::default()
        };
        assert_eq!(parse_title("bash"), expect_neither);
        assert_eq!(parse_title(""), expect_neither);
    }

    /// **本项目的主场景是高延迟链路**,一条 OSC 被 TCP 切在任意字节位置是常态
    /// (Nagle + 延迟 ACK)。切在哪里都必须认得出来 —— 认不出的现象是目录名
    /// 时有时无地闪,而且只在慢链路上出现,本地怎么试都试不出来。
    ///
    /// 自证会变红:把 `Osc7Sniffer::feed` 改成不留 `pending`(每次调用从
    /// 头找 `ESC ]`),`cut` 落在序列中间的那些用例立刻红。
    #[test]
    fn an_osc7_split_at_any_byte_boundary_is_still_recognised() {
        let seq = b"\x1b]7;file://host/home/dev/Mullion\x07";
        for cut in 0..=seq.len() {
            let mut s = Osc7Sniffer::default();
            let a = s.feed(&seq[..cut]);
            let b = s.feed(&seq[cut..]);
            assert_eq!(
                a.or(b).as_deref(),
                Some(&b"/home/dev/Mullion"[..]),
                "切在第 {cut} 字节就认不出来了"
            );
        }
    }

    /// ST(`ESC \`)也是合法终止符,而且它本身跨得过 `feed` 边界。
    #[test]
    fn st_terminated_osc7_works_including_across_a_feed_boundary() {
        let mut s = Osc7Sniffer::default();
        assert_eq!(
            s.feed(b"\x1b]7;file://h/tmp\x1b\\").as_deref(),
            Some(&b"/tmp"[..])
        );

        let mut s = Osc7Sniffer::default();
        assert_eq!(s.feed(b"\x1b]7;file://h/tmp\x1b"), None);
        assert_eq!(s.feed(b"\\").as_deref(), Some(&b"/tmp"[..]));
    }

    /// 标题那条 OSC(`0;` / `2;`)不能被当成 cwd —— 它由 alacritty 那条腿
    /// 处理,我们只是路过。
    ///
    /// 自证:把 `feed` 里 `buf.strip_prefix(b"7;").and_then(parse_osc7)` 最字面地
    /// 改成 `parse_osc7(buf)`(即去掉 `strip_prefix(b"7;")` 本身)**验不出这条测试** ——
    /// `"2;file://h/tmp"` 不以 `"file://"` 开头,`parse_osc7` 照样给 `None`;真正
    /// 兜住第二条断言的是 `parse_osc7` 自己的前缀校验,不是这里的 `"7;"` 网关。
    /// 第一条断言(`"0;dev@h: ~/x"`)同理靠的也是这层校验,不是 `"7;"` 网关。
    ///
    /// 真正能单独钉住这条测试的是「去掉 OSC 号网关、无条件取第一个 `;` 之后
    /// 的内容」这个语义等价的变异:把判断改成
    /// `buf.iter().position(|&b| b == b';').and_then(|i| parse_osc7(&buf[i + 1..]))`。
    /// 这样 `"2;file://h/tmp"` 会被当成 `"file://h/tmp"` 解出 `/tmp`,第二条断言
    /// 才会红(第一条仍然是 `None`,因为 `"dev@h: ~/x"` 本来就不含 `file://`)。
    #[test]
    fn a_title_osc_is_not_mistaken_for_a_cwd() {
        let mut s = Osc7Sniffer::default();
        assert_eq!(s.feed(b"\x1b]0;dev@h: ~/x\x07"), None);
        assert_eq!(s.feed(b"\x1b]2;file://h/tmp\x07"), None);
    }

    /// 畸形流(开了头永不终止)不能把内存吃光,而且**丢掉之后要能恢复**——
    /// 后面那条正常的 OSC 7 仍须认出来。只测「没 OOM」是不够的:把
    /// `pending = None` 写成 `pending = Some(Vec::new())` 会永远卡在
    /// OSC 状态里,内存不涨但从此再也认不出任何 cwd。
    ///
    /// 自证会变红:删掉 `feed` 里的 `> MAX_PENDING` 那一段(第一条断言的
    /// 内存不涨没法直接测,但第二条会因为 4096 之后的字节继续攒着、
    /// 后面那条 OSC 被当成前一条的载荷而红)。
    #[test]
    fn an_unterminated_osc_is_dropped_and_the_sniffer_recovers() {
        let mut s = Osc7Sniffer::default();
        assert_eq!(s.feed(b"\x1b]7;file://h/"), None);
        assert_eq!(s.feed(&vec![b'x'; MAX_PENDING + 16]), None);
        assert_eq!(
            s.feed(b"\x1b]7;file://h/tmp\x07").as_deref(),
            Some(&b"/tmp"[..]),
            "丢掉畸形那条之后应该能重新认出正常的 OSC 7"
        );
    }

    /// 一次 `feed` 里有多条时取**最后一条** —— cwd 是「当前值」,不是流水。
    #[test]
    fn the_last_osc7_in_a_batch_wins() {
        let mut s = Osc7Sniffer::default();
        assert_eq!(
            s.feed(b"\x1b]7;file://h/a\x07\x1b]7;file://h/b\x07")
                .as_deref(),
            Some(&b"/b"[..])
        );
    }
}
