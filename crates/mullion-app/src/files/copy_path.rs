//! F250:把右键菜单选中的那些路径折成一行文本,交给系统剪贴板。
//!
//! **纯函数,不碰 IO、不碰剪贴板** —— 基准目录由调用方(`app.rs`)从焦点
//! 分屏的 OSC 7 上报里取,写剪贴板也是它的事。这里只回答两个问题:
//! 「这条路径相对那个目录怎么写」和「这几条拼成一行长什么样」。

/// F250:`path` 相对 `base` 的写法。`None` = **不在 `base` 之下**。
///
/// 只往下,不回溯:`base` 是 `/home/dev`、`path` 是 `/etc/hosts` 时给
/// `None` 而不是 `../../etc/hosts`。一条爬了两级又拐进别的子树的相对路径,
/// 用户既看不懂也不会想粘出去 —— 这种时候菜单里那一项该是**灰的**,而不
/// 是给一串没人要的东西。
///
/// 判据是**按段比**,不是字符串前缀:`/a` 是 `/ab/c` 的前缀,但 `/ab/c`
/// 显然不在 `/a` 之下。少这一道就会给出 `b/c` 这种指向别处的相对路径,
/// 而且完全看不出错。
///
/// `path == base` 给 `.`(而不是空串):空串粘出去什么都不是。
pub fn relative_to(base: &[u8], path: &[u8]) -> Option<Vec<u8>> {
    let base = trim_trailing_slashes(base);
    let path = trim_trailing_slashes(path);
    if path == base {
        return Some(b".".to_vec());
    }
    // `base` 是根时它已经被 trim 成空串,`/etc` 去掉开头那个 `/` 就是答案。
    let rest = path.strip_prefix(base)?;
    // 按段比:剩下那截必须**正好**从一个分隔符开始。
    let rest = rest.strip_prefix(b"/")?;
    if rest.is_empty() {
        return None;
    }
    Some(rest.to_vec())
}

/// 去掉尾部的 `/`。根目录 `/` 会被削成空串 —— 那正是 `relative_to` 里
/// `strip_prefix` 要的形态(任何绝对路径都以空串开头)。
fn trim_trailing_slashes(p: &[u8]) -> &[u8] {
    let mut end = p.len();
    while end > 0 && p[end - 1] == b'/' {
        end -= 1;
    }
    &p[..end]
}

/// F250:把要复制的那几条折成剪贴板里的一行。
///
/// **一条就原样给,多条才按 shell 规矩引用。** 这不是偷懒:
/// - 复制**一条**路径,用途几乎总是「我要拿到这个路径」—— 粘进编辑器、
///   粘进配置文件、粘进聊天窗口。给它套上引号是纯噪音,用户还得手动删。
/// - 复制**多条**,唯一说得通的用途就是拼一条命令(`tar czf … a b c`)。
///   那时候不引用,一个带空格的文件名就把命令劈成两半,而且劈得静默 ——
///   shell 不会报错,它会去操作两个不存在的路径。
///
/// 引用用**单引号**(POSIX):里面除了 `'` 自己什么都不展开,`$`、`` ` ``、
/// `\` 一律安全。`'` 本身按 shell 的老写法拆开转义:`it's` → `'it'\''s'`。
///
/// 白名单外**一律引用**,包括以 `~` 开头的:`~foo` 不引用的话 shell 会当成
/// 「用户 foo 的主目录」去展开,而这里给的是一个**叫 `~foo` 的文件**。
/// 白名单里留着 `~` 是为了让 `a~b` 这种中间带波浪线的名字不必挨引号。
///
/// 名字里发不出 wire 请求的字节调用方已经滤掉了(`delete_targets`),
/// 这里按 UTF-8 lossy 转 —— 剩下的都转得回去。
pub fn join_for_clipboard(paths: &[Vec<u8>]) -> String {
    if let [only] = paths {
        return String::from_utf8_lossy(only).into_owned();
    }
    paths
        .iter()
        .map(|p| shell_quote(p))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 一条路径在 shell 里的安全写法。判据见 `join_for_clipboard`。
fn shell_quote(p: &[u8]) -> String {
    let s = String::from_utf8_lossy(p);
    let clean = !s.is_empty()
        && !s.starts_with('~')
        && s.bytes().all(|c| {
            matches!(c,
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'~' | b'/' | b'-')
        });
    if clean {
        return s.into_owned();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(base: &str, path: &str) -> Option<String> {
        relative_to(base.as_bytes(), path.as_bytes())
            .map(|v| String::from_utf8(v).expect("测试里都是 ASCII"))
    }

    /// 就在基准里 / 在更深处 —— 给出那一截。
    #[test]
    fn a_path_under_the_base_is_written_relative_to_it() {
        assert_eq!(
            rel("/home/dev", "/home/dev/a.txt").as_deref(),
            Some("a.txt")
        );
        assert_eq!(
            rel("/home/dev", "/home/dev/x/y/z.log").as_deref(),
            Some("x/y/z.log")
        );
    }

    /// 基准自己 → `.`。空串粘出去什么都不是。
    #[test]
    fn the_base_itself_is_a_single_dot_not_an_empty_string() {
        assert_eq!(rel("/home/dev", "/home/dev").as_deref(), Some("."));
        assert_eq!(rel("/home/dev/", "/home/dev").as_deref(), Some("."));
    }

    /// 基准是根:任何绝对路径都在它之下,去掉开头那个 `/`。
    #[test]
    fn everything_is_under_the_root() {
        assert_eq!(rel("/", "/etc/hosts").as_deref(), Some("etc/hosts"));
        assert_eq!(rel("/", "/").as_deref(), Some("."));
    }

    /// **按段比,不是按字符串前缀**:`/ab/c` 不在 `/a` 之下。少这一道会给出
    /// `b/c` —— 一条指向别处的相对路径,而且看不出错。
    #[test]
    fn a_shared_prefix_that_is_not_a_whole_segment_is_not_under_the_base() {
        assert_eq!(rel("/a", "/ab/c"), None);
        assert_eq!(rel("/home/dev", "/home/development/x"), None);
    }

    /// 不在基准之下就是 `None` —— 不给 `../..` 那种爬出去的写法。
    #[test]
    fn a_path_outside_the_base_gets_no_relative_form_at_all() {
        assert_eq!(rel("/home/dev", "/etc/hosts"), None);
        // 基准反而在路径之下 —— 同样不给。
        assert_eq!(rel("/home/dev/deep", "/home/dev"), None);
    }

    /// 一条就原样给:复制单条路径的用途是「拿到这个路径」,引号是噪音。
    /// 哪怕它带空格。
    #[test]
    fn a_single_path_is_handed_over_verbatim_even_when_it_has_spaces() {
        assert_eq!(
            join_for_clipboard(&[b"/data/my logs/a.txt".to_vec()]),
            "/data/my logs/a.txt"
        );
    }

    /// 多条 = 用户在拼命令。干净的原样,带空格的挨引号,空格分隔。
    #[test]
    fn several_paths_are_quoted_only_where_a_shell_would_otherwise_split_them() {
        assert_eq!(
            join_for_clipboard(&[
                b"/data/a.txt".to_vec(),
                b"/data/my logs/b.txt".to_vec(),
                b"/data/c-1_2.log".to_vec(),
            ]),
            "/data/a.txt '/data/my logs/b.txt' /data/c-1_2.log"
        );
    }

    /// 单引号自己按 shell 的老写法拆开 —— `'` 在单引号串里没有转义符可用。
    #[test]
    fn a_quote_inside_the_name_is_closed_escaped_and_reopened() {
        assert_eq!(
            join_for_clipboard(&[b"/a".to_vec(), b"/it's here".to_vec()]),
            r"/a '/it'\''s here'",
        );
    }

    /// 以 `~` 开头必须引用:`~foo` 不引用会被 shell 展开成某个用户的主目录,
    /// 而这里说的是一个**叫 `~foo` 的东西**。中间带波浪线的不用。
    #[test]
    fn a_leading_tilde_is_quoted_so_the_shell_cannot_expand_it() {
        assert_eq!(
            join_for_clipboard(&[b"~foo".to_vec(), b"a~b".to_vec()]),
            "'~foo' a~b"
        );
    }

    /// `$`/反引号/反斜杠这些在双引号里仍会作妖的,单引号一律罩得住。
    #[test]
    fn shell_metacharacters_are_all_covered_by_the_single_quotes() {
        assert_eq!(
            join_for_clipboard(&[b"/a".to_vec(), b"/x/$(id)`ls`\\z".to_vec()]),
            r"/a '/x/$(id)`ls`\z'"
        );
    }
}
