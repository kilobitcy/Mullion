//! F220:远端内复制/剪切/粘贴的**纯逻辑**。零 egui / 零 tokio / 零 IO ——
//! 「粘到自己里面去了没」「同名怎么改」「跳过之后还剩什么」全是判据类
//! 错误,得能在没有网络的情况下复现。
//!
//! 协议动作在 `mullion_ssh::copy_tree`,编排在 `app.rs`。

use mullion_ssh::sftp::RemotePath;

/// 剪贴板里装的是复制还是剪切。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipMode {
    Copy,
    Cut,
}

/// F220:一个标签的远端剪贴板。**per-tab**(设计 B4)——里面的路径永远
/// 属于当前这条连接,不会指到另一台机器上不存在的路径去。
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteClip {
    pub mode: ClipMode,
    /// **绝对路径** + 是不是目录。复制那一刻就拼好 —— 之后用户换目录、
    /// 换排序都不影响它指向谁。
    pub items: Vec<(RemotePath, bool)>,
}

/// 同名时怎么办。**没有「静默覆盖」这一档** —— 覆盖必须是用户在框里
/// 明确选的(设计 D17 的精神)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    Overwrite,
    Skip,
    KeepBoth,
}

/// 一次粘贴要干的活。
#[derive(Debug, Clone, PartialEq)]
pub struct PastePlan {
    /// (源绝对路径, 目标绝对路径)。**目标是全路径不是目录** ——
    /// 「保留两者」时目标末段与源末段不同,只给目录的话那个新名字就丢了。
    pub pairs: Vec<(RemotePath, RemotePath)>,
    /// 按 `Skip` 滤掉了几条。用户要知道「5 条里跳过了 2 条」。
    pub skipped: usize,
}

/// 路径的末段(不含目录)。空路径 / 结尾是 `/` 时给空切片。
///
/// `pub(crate)`:F220 代码质量复核挖出 `ui::files_panel::cut_names_for`
/// 内联了逐字等价的一份 —— 这条路径解析规则的权威定义在这儿
/// (`unique_name`/`conflicts`/`plan_paste` 都靠它),不该在 crate 里拷第二份。
pub(crate) fn last_segment(p: &RemotePath) -> Vec<u8> {
    p.as_bytes()
        .rsplit(|b| *b == b'/')
        .next()
        .unwrap_or_default()
        .to_vec()
}

/// `dst` 是不是 `src` 自己或它的子孙。
///
/// **判据是字节前缀 + `/` 边界**:光比前缀的话 `/a/bb` 会被判成 `/a/b`
/// 的子孙,而那是两个毫不相干的目录 —— 用户会看到「不能粘到这里」却
/// 找不出原因。根目录(`/`)是一切的祖先,单独处理。
pub fn is_within(src: &RemotePath, dst: &RemotePath) -> bool {
    let (s, d) = (src.as_bytes(), dst.as_bytes());
    if s == d {
        return true;
    }
    if s == b"/" {
        return true;
    }
    d.len() > s.len() && d.starts_with(s) && d[s.len()] == b'/'
}

/// 目标目录里已经有的、与这批条目撞名的**末段名字**(按传入顺序)。
pub fn conflicts(
    items: &[(RemotePath, bool)],
    existing: &std::collections::BTreeSet<Vec<u8>>,
) -> Vec<Vec<u8>> {
    items
        .iter()
        .map(|(p, _)| last_segment(p))
        .filter(|n| existing.contains(n))
        .collect()
}

/// 避开 `taken` 的一个新名字:`a.txt` → `a (副本).txt` → `a (副本 2).txt`。
///
/// `is_dir` 为真时**不切扩展名** —— `v1.2` 是目录名的一部分,不是后缀。
/// 文件按**最后一个** `.` 切,且那个点不在开头(`.env` 整个是名字)。
pub fn unique_name(
    name: &[u8],
    is_dir: bool,
    taken: &std::collections::BTreeSet<Vec<u8>>,
) -> Vec<u8> {
    let (stem, ext): (&[u8], &[u8]) = if is_dir {
        (name, b"")
    } else {
        match name.iter().rposition(|b| *b == b'.') {
            Some(i) if i > 0 => (&name[..i], &name[i..]),
            _ => (name, b""),
        }
    };
    for n in 1..10_000u32 {
        let mut cand = stem.to_vec();
        cand.extend_from_slice(" (副本".as_bytes());
        if n > 1 {
            cand.extend_from_slice(format!(" {n}").as_bytes());
        }
        cand.extend_from_slice(")".as_bytes());
        cand.extend_from_slice(ext);
        if !taken.contains(&cand) {
            return cand;
        }
    }
    // 一万个同名副本 —— 到这一步只可能是调用方传了个恒真的 `taken`。
    name.to_vec()
}

/// 一次粘贴的计划。`existing` = 目标目录里现有的名字(预检查列回来的)。
///
/// **调用方要保证**:`items` 里的路径既不以 `/` 结尾、也不是根 `/`。两者的
/// 末段都是空的(见 `last_segment`),`dst_dir.join(&[])` 拼出来就是 `dst_dir`
/// **自己** —— 源会被当成「写进目标目录本身」,静默不报错。按既有惯例拼
/// (`cwd.join(readdir 给的单段名)`,同 `state.rs` 的 `delete_targets`)结构上
/// 就不可能拼出这种路径,所以这里不设补偿分支:在这一层补,只写得出恒绿的
/// 守护(它挡的输入没有任何调用方产得出来)。
pub fn plan_paste(
    items: &[(RemotePath, bool)],
    dst_dir: &RemotePath,
    policy: Policy,
    existing: &std::collections::BTreeSet<Vec<u8>>,
) -> PastePlan {
    // 同一批里改出来的新名字也要占位 —— 不占的话两条都叫 `a (副本).txt`,
    // 后一条把前一条盖掉,而用户选的正是「保留两者」。
    let mut taken = existing.clone();
    let mut pairs = Vec::new();
    let mut skipped = 0usize;
    for (src, is_dir) in items {
        let name = last_segment(src);
        let hit = taken.contains(&name);
        let target = match (policy, hit) {
            (Policy::Skip, true) => {
                skipped += 1;
                continue;
            }
            (Policy::KeepBoth, true) => unique_name(&name, *is_dir, &taken),
            _ => name,
        };
        taken.insert(target.clone());
        pairs.push((src.clone(), dst_dir.join(&target)));
    }
    PastePlan { pairs, skipped }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rp(s: &str) -> RemotePath {
        RemotePath::from_bytes(s.as_bytes().to_vec())
    }

    /// 唯一名:扩展名留在最后,别变成 `a.txt (副本)`(双击就打不开了)。
    #[test]
    fn a_duplicate_keeps_its_extension_at_the_end() {
        let taken = [b"a.txt".to_vec()].into_iter().collect();
        assert_eq!(
            unique_name(b"a.txt", false, &taken),
            b"a (\xe5\x89\xaf\xe6\x9c\xac).txt".to_vec(),
            "应为 `a (副本).txt`"
        );
    }

    /// 第二次撞 → 带序号。
    #[test]
    fn a_second_duplicate_gets_a_number() {
        let taken = [b"a.txt".to_vec(), "a (副本).txt".as_bytes().to_vec()]
            .into_iter()
            .collect();
        assert_eq!(
            unique_name(b"a.txt", false, &taken),
            "a (副本 2).txt".as_bytes().to_vec()
        );
    }

    /// 目录不切扩展名:`v1.2` 是目录名的一部分,不是后缀。
    #[test]
    fn a_directory_is_not_split_at_its_dot() {
        let taken = [b"v1.2".to_vec()].into_iter().collect();
        assert_eq!(
            unique_name(b"v1.2", true, &taken),
            "v1.2 (副本)".as_bytes().to_vec()
        );
    }

    /// 点开头的名字(`.env`)整个是名字,没有主干可切。
    #[test]
    fn a_dotfile_has_no_stem_to_split() {
        let taken = [b".env".to_vec()].into_iter().collect();
        assert_eq!(
            unique_name(b".env", false, &taken),
            ".env (副本)".as_bytes().to_vec()
        );
    }

    /// **本片最重要的一条闸门**:目标是源自身或源的子孙 → 整批拒绝。
    ///
    /// 远端 `cp` 自己会拦,但 SFTP 回退是我们**自己写的递归** ——
    /// 边列源边往源的子孙里写,会一直递归到把磁盘写满。
    ///
    /// 自证会变红:把 `is_within` 改成恒 `false`。
    #[test]
    fn pasting_into_yourself_or_your_own_descendant_is_refused() {
        assert!(is_within(&rp("/a/b"), &rp("/a/b")), "目标就是源自己");
        assert!(is_within(&rp("/a/b"), &rp("/a/b/c")), "目标在源里面");
        assert!(is_within(&rp("/a/b"), &rp("/a/b/c/d")), "目标在源的更深处");
        assert!(
            !is_within(&rp("/a/b"), &rp("/a/bb")),
            "`/a/bb` 不是 `/a/b` 的子孙"
        );
        assert!(!is_within(&rp("/a/b"), &rp("/a")), "父目录不是子孙");
        assert!(is_within(&rp("/"), &rp("/anything")), "根是一切的祖先");
    }

    /// 覆盖:每一条都落到 `dst/原名`,一条不少。
    #[test]
    fn overwriting_maps_every_item_onto_its_own_name() {
        let items = vec![(rp("/src/a.txt"), false), (rp("/src/b.txt"), false)];
        let existing = [b"a.txt".to_vec()].into_iter().collect();
        let plan = plan_paste(&items, &rp("/dst"), Policy::Overwrite, &existing);
        assert_eq!(
            plan.pairs,
            vec![
                (rp("/src/a.txt"), rp("/dst/a.txt")),
                (rp("/src/b.txt"), rp("/dst/b.txt")),
            ]
        );
        assert_eq!(plan.skipped, 0);
    }

    /// 跳过同名:**客户端把冲突项滤掉**,不靠 `cp -n`(coreutils 9.2 反转过
    /// 跳过时的退出码,会被 `succeeded()` 判成失败)。
    ///
    /// 自证会变红:把 `Policy::Skip` 那一支改成不过滤。
    #[test]
    fn skipping_drops_the_colliding_items_client_side() {
        let items = vec![(rp("/src/a.txt"), false), (rp("/src/b.txt"), false)];
        let existing = [b"a.txt".to_vec()].into_iter().collect();
        let plan = plan_paste(&items, &rp("/dst"), Policy::Skip, &existing);
        assert_eq!(plan.pairs, vec![(rp("/src/b.txt"), rp("/dst/b.txt"))]);
        assert_eq!(plan.skipped, 1);
    }

    /// 保留两者:撞名的改名,没撞的原样。
    #[test]
    fn keeping_both_renames_only_the_colliding_ones() {
        let items = vec![(rp("/src/a.txt"), false), (rp("/src/b.txt"), false)];
        let existing = [b"a.txt".to_vec()].into_iter().collect();
        let plan = plan_paste(&items, &rp("/dst"), Policy::KeepBoth, &existing);
        assert_eq!(
            plan.pairs,
            vec![
                (rp("/src/a.txt"), rp("/dst/a (副本).txt")),
                (rp("/src/b.txt"), rp("/dst/b.txt")),
            ]
        );
    }

    /// 同一批里两条都要改名时,**第二条要避开第一条刚占掉的名字** ——
    /// 不然两条都叫 `a (副本).txt`,后一条把前一条盖掉,而用户选的是「保留两者」。
    ///
    /// 自证会变红:`plan_paste` 里不把新名字加进 `taken` 就下一轮。
    #[test]
    fn two_renamed_items_in_one_batch_do_not_collide_with_each_other() {
        let items = vec![(rp("/x/a.txt"), false), (rp("/y/a.txt"), false)];
        let existing = [b"a.txt".to_vec()].into_iter().collect();
        let plan = plan_paste(&items, &rp("/dst"), Policy::KeepBoth, &existing);
        assert_eq!(
            plan.pairs,
            vec![
                (rp("/x/a.txt"), rp("/dst/a (副本).txt")),
                (rp("/y/a.txt"), rp("/dst/a (副本 2).txt")),
            ],
            "同一批里两条改名撞在一起了 —— 后一条会盖掉前一条"
        );
    }

    /// 冲突集合:只按末段名字比。
    #[test]
    fn conflicts_are_compared_by_the_last_path_segment_only() {
        let items = vec![(rp("/src/a.txt"), false), (rp("/src/b.txt"), false)];
        let existing = [b"b.txt".to_vec()].into_iter().collect();
        assert_eq!(conflicts(&items, &existing), vec![b"b.txt".to_vec()]);
    }
}
