//! F278:文件面板的**递归模糊搜索**。纯逻辑 —— 零 egui、零 IO、零 async。
//!
//! 遍历本身是一台状态机([`Walk`]),形状照抄 `files/queue.rs`:每帧
//! [`Walk::take_runnable`] 吐出最多 n 个待列目录,app 侧各起一条 `list_dir`,
//! 结果回来喂 [`Walk::accept`]。这么切的理由和 `queue.rs` 逐字相同 ——
//! 「BFS 顺序对不对」「封顶卡没卡住」「最后一批还在飞的时候会不会提前报
//! 搜完」全是状态机 bug,必须能在没有网络的情况下复现。
//!
//! **为什么不发一句远端 `find`**(spec F278 已定):SFTP-only 的节点压根
//! 没有 exec 通道;有 exec 的也可能是 busybox,`-iname`/`-maxdepth` 参数
//! 不齐;再加上文件名里的引号与空格转义,是一摊比逐目录遍历更难对的事。

use mullion_ssh::sftp::RemotePath;

/// 名字里出现的这几个字符按**子序列**匹配。大小写不敏感。
///
/// **是子序列不是子串**(fzf 式):用户记得住的往往是 `apprs` 这种骨架,
/// 而不是完整的 `app.rs`。`crate::search::matches` 那一套(会话/项目列表用的)
/// 刻意**不是**子序列 —— 那边的字段里有多行长文本(F237 的项目说明),子序列
/// 在长文本上几乎命中一切。这边匹配的是**单个文件名**,短,子序列正合适。
/// 两处判据不同是有意的,不要合并。
///
/// 空查询恒不匹配 —— 调用方(`Walk::new`)在起搜前就该挡掉空串,
/// 这里返回 `false` 只是兜底:恒 `true` 会让一次空搜索把整棵树当成 500 个
/// 命中吐出来。
pub fn matches(query: &str, name: &str) -> bool {
    if query.is_empty() {
        return false;
    }
    let mut want = query.chars().flat_map(char::to_lowercase).peekable();
    for c in name.chars().flat_map(char::to_lowercase) {
        match want.peek() {
            Some(&w) if w == c => {
                want.next();
            }
            Some(_) => {}
            None => return true,
        }
    }
    want.peek().is_none()
}

/// 结果行显示的那一串:`root` 下的相对路径。
///
/// **显示相对路径而不是绝对路径**(spec F278 已定):搜索的心智是「在**这个
/// 目录下面**找」,绝对路径每行都顶着同一段前缀,把唯一有信息量的后半截
/// 挤出可视区。
///
/// `path` 不在 `root` 下时**原样返回绝对路径** —— 那说明调用方把两棵树的
/// 东西混一起了,硬切前缀会切出一串没头没尾的乱码,不如把真相摆出来。
pub fn relative(root: &RemotePath, path: &RemotePath) -> String {
    let r = root.as_bytes();
    let p = path.as_bytes();
    // 根是 `/` 时没有「前缀 + 分隔符」可言,直接剥掉那一个字节。
    let cut = if r == b"/" {
        1
    } else if p.starts_with(r) && p.get(r.len()) == Some(&b'/') {
        r.len() + 1
    } else {
        return path.display().to_string();
    };
    RemotePath::from_bytes(p[cut..].to_vec())
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rp(s: &str) -> RemotePath {
        RemotePath::from_bytes(s.as_bytes().to_vec())
    }

    /// 子序列,不是子串 —— 用户记得住的是骨架。
    ///
    /// 自证会变红:把 `matches` 改成 `name.to_lowercase().contains(&query.to_lowercase())`
    /// (第一条断言红)。
    #[test]
    fn the_query_matches_as_a_subsequence_not_a_substring() {
        assert!(matches("apprs", "app.rs"));
        assert!(matches("app.rs", "app.rs"));
        assert!(matches("mn", "main.rs"));
        assert!(!matches("rsapp", "app.rs"), "顺序不对不算命中");
        assert!(!matches("appx", "app.rs"));
    }

    /// 大小写不敏感 —— 远端多半是 Linux,用户按 Windows 习惯打小写。
    ///
    /// 自证会变红:把两处 `to_lowercase` 删掉。
    #[test]
    fn matching_ignores_case_on_both_sides() {
        assert!(matches("APP", "app.rs"));
        assert!(matches("app", "APP.RS"));
        assert!(matches("Gm", "GAMMA"));
    }

    /// 空查询恒不匹配。恒 `true` 的话一次空搜索会把整棵树当成 500 个命中吐
    /// 出来 —— 而用户什么都没输。
    ///
    /// 自证会变红:把 `matches` 开头那三行 `if query.is_empty()` 删掉。
    #[test]
    fn an_empty_query_matches_nothing_rather_than_everything() {
        assert!(!matches("", "app.rs"));
        assert!(!matches("", ""));
    }

    /// 相对路径剥掉根 + 那一个分隔符。
    ///
    /// 自证会变红:把 `r.len() + 1` 改成 `r.len()`(会多出个前导 `/`)。
    #[test]
    fn a_hit_is_shown_relative_to_the_directory_the_search_started_in() {
        assert_eq!(
            relative(&rp("/home/u/proj"), &rp("/home/u/proj/src/app.rs")),
            "src/app.rs"
        );
        assert_eq!(
            relative(&rp("/home/u/proj"), &rp("/home/u/proj/a.txt")),
            "a.txt"
        );
    }

    /// 根是 `/` 时只剥那一个字节 —— 按「根 + 分隔符」算会多剥一个字符,
    /// `/etc/hosts` 会显示成 `tc/hosts`。
    ///
    /// 自证会变红:把 `if r == b"/"` 那一支删掉。
    #[test]
    fn searching_from_the_filesystem_root_does_not_eat_a_character() {
        assert_eq!(relative(&rp("/"), &rp("/etc/hosts")), "etc/hosts");
    }

    /// 不在根下面的路径原样给绝对路径 —— 硬切前缀会切出没头没尾的乱码。
    ///
    /// 自证会变红:把最后那条 `return path.display().to_string()` 改成
    /// 无条件切 `r.len()`。
    #[test]
    fn a_path_outside_the_root_keeps_its_absolute_form() {
        assert_eq!(
            relative(&rp("/home/u/proj"), &rp("/var/log/x")),
            "/var/log/x"
        );
        // 前缀撞上但不是目录边界(`/home/u/project` vs `/home/u/proj`)。
        assert_eq!(
            relative(&rp("/home/u/proj"), &rp("/home/u/project/x")),
            "/home/u/project/x"
        );
    }
}
