//! F142:属主列显示用户名/组名而不是裸数字。**纯逻辑**——查什么、怎么解、
//! 显示成什么串,全在这里;发命令与落盘在 `app.rs`,画字在 `ui::files_panel`。
//!
//! 为什么要额外跑一条命令:SFTP v3 的 attrs 里**只有数字 uid/gid**,协议
//! 层拿不到名字。`readdir` 回来的 `longname`(`ls -l` 那一行,里面有名字)
//! 在 `russh-sftp 2.4.0` 的客户端 `DirEntry` 里被丢掉了,底层的
//! `RawSftpSession` 又够不着(`SftpSession.session` 私有)。于是只剩「列完
//! 目录之后,把这一屏出现过的 id 批量问一次 `getent`」这一条路。
//!
//! 三条纪律,少一条都会让它变成「每帧一次远端往返」:
//! - **问过就不再问**,包括**没问出结果的**(`asked_*` 是负缓存)。远端多的是
//!   本机没有 passwd 条目的 uid(容器里 bind mount 出来的文件),不记负缓存
//!   的话它们每次列目录都会重新问一遍。
//! - 一次只问**这一屏新出现的** id,不是全部。
//! - 查不到就回退成数字,**逐段回退**(`deploy:10001`)——用户拍板的格式。

use std::collections::{HashMap, HashSet};

use mullion_ssh::sftp::Entry;

/// 单次查询的 id 上限。剩下的留到下次列目录再问。
///
/// 有上限是因为命令行长度有硬限制(Linux 单个 argv 元素 128 KiB):一个
/// 一万条文件、属主各不相同的目录,不设上限就是往 `exec` 里塞十万字节。
pub const MAX_IDS_PER_QUERY: usize = 128;

/// 两段输出之间的分隔行。取一个不可能出现在 passwd/group 行里的串
/// (真实条目一定含 `:`,这个不含)。
pub(crate) const SEP: &str = "__mullion_getent__";

/// 一条 SSH 连接上的 uid/gid → 名字缓存。
///
/// 挂在**远端栏的 `PaneState`** 上(一栏 = 一条连接)。换连接时必须整份
/// 丢掉:同一个 1000 在两台机器上是两个人,留着就是把 A 机的名字画在 B 机
/// 的文件上。
#[derive(Debug, Default, Clone)]
pub struct OwnerNames {
    users: HashMap<u32, String>,
    groups: HashMap<u32, String>,
    /// 已经问过的 uid —— **不管问出来没有**。见模块头「负缓存」。
    asked_users: HashSet<u32>,
    asked_groups: HashSet<u32>,
}

/// 一次待发的查询。空查询不构造(见 [`OwnerNames::take_missing`])。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub users: Vec<u32>,
    pub groups: Vec<u32>,
}

impl Query {
    /// 拼成一条 shell 命令。
    ///
    /// `2>/dev/null` + `; ` 串联而不是 `&&`:`getent` 查不到任何一个 id 时
    /// 退出码是 2,而「查不到」是完全正常的结果(容器里的孤儿 uid)。用
    /// `&&` 的话第一段一落空,后面的 group 查询直接不跑了。
    ///
    /// id 是 `u32`,格式化出来只有数字,**不需要引号也不可能注入**——
    /// 这是这条命令唯一的用户输入来源。
    pub fn command(&self) -> Vec<u8> {
        let ids = |v: &[u32]| v.iter().map(u32::to_string).collect::<Vec<_>>().join(" ");
        let mut cmd = String::new();
        if !self.users.is_empty() {
            cmd.push_str(&format!("getent passwd {} 2>/dev/null; ", ids(&self.users)));
        }
        cmd.push_str(&format!("echo {SEP}; "));
        if !self.groups.is_empty() {
            cmd.push_str(&format!("getent group {} 2>/dev/null", ids(&self.groups)));
        }
        cmd.into_bytes()
    }
}

impl OwnerNames {
    /// 换连接了:整份丢掉。见 [`OwnerNames`] 的文档。
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// 这一屏里有哪些 id 还没问过。**返回即视为已问**(记进负缓存),
    /// 所以同一批 id 不会因为「结果还在路上」被重复发第二遍。
    ///
    /// 发送失败时调用方要 [`Self::forget`] 把它们放回去,否则一次网络抖动
    /// 会让这些 id 永远显示成数字。
    pub fn take_missing(&mut self, entries: &[Entry]) -> Option<Query> {
        let mut users: Vec<u32> = Vec::new();
        let mut groups: Vec<u32> = Vec::new();
        for e in entries {
            if !self.asked_users.contains(&e.uid) && !users.contains(&e.uid) {
                users.push(e.uid);
            }
            if !self.asked_groups.contains(&e.gid) && !groups.contains(&e.gid) {
                groups.push(e.gid);
            }
        }
        users.truncate(MAX_IDS_PER_QUERY);
        groups.truncate(MAX_IDS_PER_QUERY);
        if users.is_empty() && groups.is_empty() {
            return None;
        }
        self.asked_users.extend(users.iter().copied());
        self.asked_groups.extend(groups.iter().copied());
        Some(Query { users, groups })
    }

    /// 查询没发出去:把负缓存撤回,下次列目录会重新问。
    pub fn forget(&mut self, q: &Query) {
        for u in &q.users {
            self.asked_users.remove(u);
        }
        for g in &q.groups {
            self.asked_groups.remove(g);
        }
    }

    /// 收下一次 `getent` 的 stdout。
    pub fn merge(&mut self, stdout: &[u8]) {
        let (users, groups) = parse(stdout);
        self.users.extend(users);
        self.groups.extend(groups);
    }

    /// 属主列的文案:`用户名:组名`,任一段查不到就**那一段**回退成数字。
    pub fn text(&self, uid: u32, gid: u32) -> String {
        let u = self
            .users
            .get(&uid)
            .cloned()
            .unwrap_or_else(|| uid.to_string());
        let g = self
            .groups
            .get(&gid)
            .cloned()
            .unwrap_or_else(|| gid.to_string());
        format!("{u}:{g}")
    }

    #[cfg(test)]
    fn known(&self) -> (usize, usize) {
        (self.users.len(), self.groups.len())
    }
}

/// 解析 `getent passwd …; echo SEP; getent group …` 的 stdout。
///
/// 分隔行**必须存在**(`echo` 无条件执行);没有就说明这不是我们那条命令
/// 的输出(比如 shell 直接把命令拒了),整份丢弃 —— 与其把 group 行当成
/// passwd 行混进缓存,不如什么都不认。
fn parse(stdout: &[u8]) -> (HashMap<u32, String>, HashMap<u32, String>) {
    let mut users = HashMap::new();
    let mut groups = HashMap::new();
    let text = String::from_utf8_lossy(stdout);
    let mut seen_sep = false;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim() == SEP {
            seen_sep = true;
            continue;
        }
        // `passwd`:name:x:uid:gid:…  `group`:name:x:gid:members
        // 两者都是「第 1 段名字、第 3 段 id」。
        let mut f = line.split(':');
        let (Some(name), Some(_), Some(id)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let (Ok(id), false) = (id.parse::<u32>(), name.is_empty()) else {
            continue;
        };
        if seen_sep {
            groups.insert(id, name.to_string());
        } else {
            users.insert(id, name.to_string());
        }
    }
    if !seen_sep {
        return (HashMap::new(), HashMap::new());
    }
    (users, groups)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_ssh::sftp::{Entry, EntryKind, RemotePath};

    fn entry(name: &str, uid: u32, gid: u32) -> Entry {
        Entry {
            name: RemotePath::from_bytes(name.as_bytes().to_vec()),
            kind: EntryKind::File,
            size: 0,
            mtime: 0,
            mode: 0o644,
            uid,
            gid,
            link_target: None,
        }
    }

    /// 只问这一屏出现过的 id,而且**去重**。一个 500 条文件、属主全是
    /// 同一个人的目录,应该只问一个 uid,不是 500 个。
    #[test]
    fn the_query_asks_once_per_distinct_id_not_once_per_file() {
        let mut o = OwnerNames::default();
        let files: Vec<Entry> = (0..500)
            .map(|i| entry(&format!("f{i}"), 1000, 1000))
            .collect();
        let q = o.take_missing(&files).expect("有新 id 要问");
        assert_eq!(q.users, vec![1000]);
        assert_eq!(q.groups, vec![1000]);
        assert_eq!(
            String::from_utf8(q.command())
                .unwrap()
                .matches("1000")
                .count(),
            2
        );
    }

    /// 负缓存:问过就不再问,**哪怕一个名字都没查到**。少了这条,一个
    /// 全是孤儿 uid 的目录每次刷新都会重新跑一遍 `getent`。
    #[test]
    fn an_id_that_came_back_empty_is_still_never_asked_again() {
        let mut o = OwnerNames::default();
        let files = vec![entry("a", 10001, 10001)];
        assert!(o.take_missing(&files).is_some());
        o.merge(format!("{SEP}\n").as_bytes()); // getent 一条都没查到
        assert_eq!(o.known(), (0, 0));
        assert!(o.take_missing(&files).is_none(), "问过的 id 不该再问第二遍");
    }

    /// 发送失败要能撤回,否则一次网络抖动 = 这些 id 永远是数字。
    #[test]
    fn a_query_that_never_went_out_can_be_asked_again() {
        let mut o = OwnerNames::default();
        let files = vec![entry("a", 7, 7)];
        let q = o.take_missing(&files).expect("第一次要问");
        o.forget(&q);
        assert_eq!(o.take_missing(&files), Some(q));
    }

    /// 分隔行是 passwd 段与 group 段的**唯一**分界。同一个 id 在两段里
    /// 是两个不同的名字,串了段就会把组名画到用户位上。
    #[test]
    fn the_separator_keeps_the_user_half_and_the_group_half_apart() {
        let mut o = OwnerNames::default();
        o.merge(
            format!(
                "deploy:x:1000:1000:Deploy:/home/deploy:/bin/bash\n\
                 {SEP}\n\
                 docker:x:1000:deploy\n"
            )
            .as_bytes(),
        );
        assert_eq!(o.text(1000, 1000), "deploy:docker");
    }

    /// 逐段回退(用户拍板的格式):查到的那段画名字,没查到的那段画数字。
    #[test]
    fn each_half_falls_back_to_its_own_number_independently() {
        let mut o = OwnerNames::default();
        o.merge(format!("deploy:x:1000:1000::/home/deploy:/bin/sh\n{SEP}\n").as_bytes());
        assert_eq!(o.text(1000, 10001), "deploy:10001");
        assert_eq!(o.text(10001, 1000), "10001:1000");
    }

    /// 没有分隔行 = 这不是我们那条命令的输出(shell 把命令拒了、或者只
    /// 回了半截)。宁可一个都不认,也不能把 group 行当成 passwd 行收下。
    #[test]
    fn output_without_the_separator_is_refused_wholesale() {
        let mut o = OwnerNames::default();
        o.merge(b"deploy:x:1000:1000::/home/deploy:/bin/sh\n");
        assert_eq!(o.known(), (0, 0));
        assert_eq!(o.text(1000, 1000), "1000:1000");
    }

    /// 垃圾行跳过,不污染缓存,也不打断后面的正常行。
    #[test]
    fn a_malformed_line_is_skipped_without_dropping_the_rest() {
        let mut o = OwnerNames::default();
        o.merge(
            format!("getent: 未找到命令\n:x:1000:\nroot:x:0:0:root:/root:/bin/sh\n{SEP}\n")
                .as_bytes(),
        );
        assert_eq!(o.known(), (1, 0));
        assert_eq!(o.text(0, 0), "root:0");
    }

    /// 命令行长度有硬上限:一屏 id 再多也只问一批,剩下的下次再说。
    #[test]
    fn a_huge_directory_is_asked_in_bounded_batches() {
        let mut o = OwnerNames::default();
        let files: Vec<Entry> = (0..MAX_IDS_PER_QUERY as u32 + 50)
            .map(|i| entry(&format!("f{i}"), i, i))
            .collect();
        let q = o.take_missing(&files).expect("有新 id 要问");
        assert_eq!(q.users.len(), MAX_IDS_PER_QUERY);
        let rest = o.take_missing(&files).expect("剩下的下次再问");
        assert_eq!(rest.users.len(), 50);
    }

    /// 换连接:同一个 1000 在两台机器上是两个人。
    #[test]
    fn switching_connections_throws_the_whole_cache_away() {
        let mut o = OwnerNames::default();
        // 先走一遍完整流程,让**负缓存**也有内容 —— 只 merge 不 take_missing
        // 的话 `asked_*` 本来就是空的,下面那条断言会恒绿。
        o.take_missing(&[entry("a", 1000, 1000)])
            .expect("第一次要问");
        o.merge(format!("alice:x:1000:1000::/home/alice:/bin/sh\n{SEP}\n").as_bytes());
        o.clear();
        assert_eq!(o.text(1000, 1000), "1000:1000");
        assert!(
            o.take_missing(&[entry("a", 1000, 1000)]).is_some(),
            "清空之后连负缓存也该没了"
        );
    }
}
