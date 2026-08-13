//! 「左侧列表有两条完全相同的会话」(走查 3)。**纯函数,零 egui。**
//!
//! 两个判据,分得很清楚,因为它们回答的是不同的问题:
//!
//! - [`looks_same`] —— **列表上看起来是不是同一行**。列表行只画名称和
//!   `user@host`,所以这三样一致就足以让用户分不出来。它决定要不要给行
//!   追加一段区分信息。
//! - [`duplicate_of`] —— **是不是真的重复了**。除上面三样外还要求端口和
//!   协议都一致。它决定保存时要不要提醒。
//!
//! 把两者合成一个的话必然出错:同名同主机、只是端口不同的两条会话在列表上
//! 确实分不出来(该加区分信息),但它们是**两台不同的服务**,不是重复(不该
//! 提醒用户"你存重了")。

use mullion_store::{GroupRecord, SessionId, SessionRecord};

/// 在列表上会不会被看成同一行。列表只显示名称 + `user@host`,所以只比这三样
/// ——**外加协议**:列表按协议分页(F118)之后,两个协议的记录永远不会同框
/// 出现,把它们判成同形等于给一个不存在的视觉冲突加噪音。而「同一台机器
/// 建一条 SSH + 一条 SFTP」是 D1 明确接受的正常用法。
fn looks_same(a: &SessionRecord, b: &SessionRecord) -> bool {
    a.identity.name == b.identity.name
        && a.auth.user == b.auth.user
        && a.connection.host == b.connection.host
        && a.connection.protocol == b.connection.protocol
}

/// 这份表单是不是跟某条**已存在**的会话重了。返回那条会话的 id。
///
/// `editing` 是当前正在编辑的会话 —— 编辑一条已有会话时它当然跟自己「重复」,
/// 必须排除,否则每次改一个字都会弹一句「你存重了」。
///
/// 取散装参数而不是 `SessionRecord`:调用方手上是一份**还没保存**的表单缓冲,
/// 造不出记录来(`SessionRecord` 要 id、要 `modified_at`)。
#[allow(clippy::too_many_arguments)]
pub(super) fn duplicate_of(
    name: &str,
    user: &str,
    host: &str,
    port: u16,
    protocol: mullion_store::Protocol,
    editing: Option<SessionId>,
    all: &[SessionRecord],
) -> Option<SessionId> {
    // 比较前 trim:表单里名字末尾多一个空格,存进去在列表上跟没空格的完全
    // 一样,却躲过了重复检测 —— 那正是这条要防的情况。
    let (name, user, host) = (name.trim(), user.trim(), host.trim());
    all.iter()
        .find(|r| {
            Some(r.id) != editing
                && r.identity.name == name
                && r.auth.user == user
                && r.connection.host == host
                && r.connection.port == port
                && r.connection.protocol == protocol
        })
        .map(|r| r.id)
}

/// 这一行要不要追加区分信息,追加什么。
///
/// 只在**列表上真的有另一行长得一样**时才返回 —— 平白给每行加个后缀,列表
/// 会变得又长又吵。
///
/// 优先级:分组名 → 端口 → 备注首句。分组排第一是因为它是用户自己划的语义
/// (「生产」和「测试」各一台 web01 是最常见的情形);端口次之(客观、短);
/// 备注最后(可能很长,只取第一句)。三样都区分不开时返回 `None` —— 那是真
/// 重复了,该由保存时的提醒负责,不该在列表上塞一段没有信息量的文字。
pub(super) fn disambiguate(
    rec: &SessionRecord,
    all: &[SessionRecord],
    groups: &[GroupRecord],
) -> Option<String> {
    let twins: Vec<&SessionRecord> = all
        .iter()
        .filter(|r| r.id != rec.id && looks_same(r, rec))
        .collect();
    if twins.is_empty() {
        return None;
    }

    let group_name = |r: &SessionRecord| -> Option<String> {
        let gid = r.identity.group_id?;
        groups.iter().find(|g| g.id == gid).map(|g| g.name.clone())
    };
    let mine = group_name(rec);
    if twins.iter().any(|r| group_name(r) != mine) {
        // 未分组的那条也得说话,否则它是唯一没后缀的一行,看起来像"正常的
        // 那条"。
        return Some(mine.unwrap_or_else(|| "未分组".to_string()));
    }

    if twins
        .iter()
        .any(|r| r.connection.port != rec.connection.port)
    {
        return Some(format!(":{}", rec.connection.port));
    }

    let note = first_sentence(&rec.identity.note);
    if twins
        .iter()
        .any(|r| first_sentence(&r.identity.note) != note)
    {
        // 自己没备注、别人有 —— 也要标出来(空串没法当后缀,给一句实话)。
        return Some(if note.is_empty() {
            "无备注".to_string()
        } else {
            note
        });
    }

    None
}

/// 备注的第一句。列表行只有一行的宽度,整段备注放不下。
///
/// 断句取中英文句号和换行,再兜一个 24 字符的硬上限 —— 一段没有标点的长备注
/// 照样会把行撑爆。**按字符截断不是按字节**:中文备注按字节切会 panic。
fn first_sentence(note: &str) -> String {
    let head = note
        .split(['。', '.', '\n', ';', ';'])
        .next()
        .unwrap_or("")
        .trim();
    head.chars().take(24).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::{Auth, AuthKind, Connection, GroupId, Identity, Protocol};

    fn rec(id: u64, name: &str, host: &str, port: u16) -> SessionRecord {
        SessionRecord {
            id: SessionId(id),
            modified_at: "t".into(),
            identity: Identity {
                name: name.into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: host.into(),
                port,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "brain".into(),
                kind: AuthKind::Password,
            },
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
            sftp: Default::default(),
        }
    }

    fn group(id: u64, name: &str) -> GroupRecord {
        GroupRecord {
            id: GroupId(id),
            name: name.into(),
            tags: Vec::new(),
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
        }
    }

    /// 同名不等于重复。「prod」在两个分组下各有一台是完全正常的用法,
    /// 见谁重名就喊一声的话,用户三天就学会无视这条提醒了。
    #[test]
    fn same_name_but_different_host_is_not_a_duplicate() {
        let all = vec![rec(1, "prod", "10.0.0.1", 22)];
        assert_eq!(
            duplicate_of("prod", "brain", "10.0.0.2", 22, Protocol::Ssh, None, &all),
            None,
            "主机不同就不是重复"
        );
        assert_eq!(
            duplicate_of("prod", "brain", "10.0.0.1", 2222, Protocol::Ssh, None, &all),
            None,
            "端口不同是两台不同的服务,不是重复"
        );
    }

    /// 走查 3 的正主:名称 / user@host / 端口 / 协议全一样 —— 用户在列表里
    /// 根本分不出来,存的时候就该提醒。
    #[test]
    fn same_everything_is_a_duplicate() {
        let all = vec![rec(1, "brain", "192.168.1.23", 22)];
        assert_eq!(
            duplicate_of(
                "brain",
                "brain",
                "192.168.1.23",
                22,
                Protocol::Ssh,
                None,
                &all
            ),
            Some(SessionId(1))
        );
        // 名字末尾多一个空格,列表上看起来完全一样,不能靠它躲过检测。
        assert_eq!(
            duplicate_of(
                "brain ",
                " brain",
                "192.168.1.23",
                22,
                Protocol::Ssh,
                None,
                &all
            ),
            Some(SessionId(1)),
            "前后空格不该让重复检测失灵"
        );
        // 编辑这条会话本身时,它当然「跟自己重复」——必须排除,否则改一个字
        // 就弹一次提醒。
        assert_eq!(
            duplicate_of(
                "brain",
                "brain",
                "192.168.1.23",
                22,
                Protocol::Ssh,
                Some(SessionId(1)),
                &all
            ),
            None,
            "编辑自己不该被判成重复"
        );
    }

    /// 区分信息的优先级:分组名 → 端口 → 备注首句。分组第一,因为那是用户
    /// 自己划的语义。
    #[test]
    fn disambiguation_prefers_the_group_name_then_the_port() {
        let groups = vec![group(1, "生产"), group(2, "测试")];

        // 分组不同 → 用分组名。
        let mut a = rec(1, "web01", "10.0.0.1", 22);
        let mut b = rec(2, "web01", "10.0.0.1", 22);
        a.identity.group_id = Some(GroupId(1));
        b.identity.group_id = Some(GroupId(2));
        let all = vec![a.clone(), b.clone()];
        assert_eq!(disambiguate(&a, &all, &groups), Some("生产".into()));
        assert_eq!(disambiguate(&b, &all, &groups), Some("测试".into()));

        // 同分组、端口不同 → 用端口。
        let mut c = rec(3, "web01", "10.0.0.1", 2222);
        c.identity.group_id = Some(GroupId(1));
        let all = vec![a.clone(), c.clone()];
        assert_eq!(disambiguate(&a, &all, &groups), Some(":22".into()));
        assert_eq!(disambiguate(&c, &all, &groups), Some(":2222".into()));

        // 分组端口都一样、只有备注不同 → 用备注首句(截到第一个句号)。
        let mut d = rec(4, "web01", "10.0.0.1", 22);
        d.identity.group_id = Some(GroupId(1));
        d.identity.note = "老板那台。别乱动".into();
        let all = vec![a.clone(), d.clone()];
        assert_eq!(disambiguate(&d, &all, &groups), Some("老板那台".into()));
        assert_eq!(
            disambiguate(&a, &all, &groups),
            Some("无备注".into()),
            "没备注的那条也得说话,否则它看起来像「正常的那条」"
        );

        // 一条未分组、一条有分组 —— 未分组那条也要标出来。
        let e = rec(5, "web01", "10.0.0.1", 22); // group_id = None
        let all = vec![a.clone(), e.clone()];
        assert_eq!(disambiguate(&e, &all, &groups), Some("未分组".into()));
    }

    /// 列表上独一无二的会话不该被加任何后缀 —— 平白给每行加尾巴,列表会
    /// 变得又长又吵,真有重复时反而看不见。
    #[test]
    fn a_unique_session_gets_no_suffix() {
        let a = rec(1, "web01", "10.0.0.1", 22);
        let b = rec(2, "db01", "10.0.0.2", 22);
        let all = vec![a.clone(), b.clone()];
        assert_eq!(disambiguate(&a, &all, &[]), None);

        // 三样都区分不开(真重复)→ 也返回 None:该由保存时的提醒负责,
        // 列表上塞一段没有信息量的文字没有意义。
        let twin = rec(2, "web01", "10.0.0.1", 22);
        let all = vec![a.clone(), twin];
        assert_eq!(disambiguate(&a, &all, &[]), None);
    }

    /// 备注按**字符**截断。中文备注按字节切会当场 panic。
    #[test]
    fn a_long_multibyte_note_is_truncated_on_char_boundaries() {
        let long = "这是一段很长的备注".repeat(10);
        let got = first_sentence(&long);
        assert_eq!(got.chars().count(), 24);
        assert!(long.starts_with(&got));
    }

    /// 列表按协议分页之后，两个协议的记录永远不同框出现 —— 给它们互相追加
    /// 「区分信息」是纯噪音。而按 D1 的推荐做法，同一台机器建一条 SSH 会话
    /// + 一条 SFTP 节点正是**正常用法**，每行都挂个后缀会让人以为配错了。
    #[test]
    fn records_of_different_protocols_are_never_twins() {
        let mut a = rec(1, "web01", "10.0.0.1", 22);
        // 端口特意跟 a 不同:如果 looks_same 漏看协议,旧逻辑会把 a/b 判成
        // 同形、再靠端口互相追加区分信息(返回 Some(":22")/Some(":2222")),
        // 这条断言就能真的抓到"漏看协议"的回归 —— 若端口跟 a 一样,三项
        // 区分信息(分组/端口/备注)会碰巧全部相同,判成"真重复"返回
        // None,跟协议判据是否生效无关,断言就成了恒绿的假测试。
        let mut b = rec(2, "web01", "10.0.0.1", 2222);
        a.connection.protocol = mullion_store::Protocol::Ssh;
        b.connection.protocol = mullion_store::Protocol::Sftp;
        let all = vec![a.clone(), b.clone()];
        assert_eq!(
            disambiguate(&a, &all, &[]),
            None,
            "跨协议的两条不该被判成同形"
        );
        assert_eq!(disambiguate(&b, &all, &[]), None);

        // 反面：同协议、其它都一样时仍然要追加区分信息，否则这条改动
        // 把原有保护一起弄没了。
        let mut c = rec(3, "web01", "10.0.0.1", 2222);
        c.connection.protocol = mullion_store::Protocol::Ssh;
        let all = vec![a.clone(), c];
        assert_eq!(disambiguate(&a, &all, &[]), Some(":22".into()));
    }
}
