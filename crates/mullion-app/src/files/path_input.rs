//! F131:路径条里用户敲进来的那一串,解析成一个真的能跳过去的路径。
//!
//! **纯函数,不碰 IO** —— 远端登录目录由调用方(`app.rs`)从 `sftp_home` 取,
//! 本地主目录由调用方从 `files::local` 取。面板自己既不知道 home 是什么,
//! 也不该知道。
//!
//! 相对路径拼在**当前目录**后面,不是发给远端让它按登录目录解析:用户在
//! `/var/log` 里敲 `nginx`,心智一定是 `/var/log/nginx`。`.` / `..` 在这里
//! 按**纯字符串**规整(不解析符号链接)——与既有的 `RemotePath::parent()`
//! (上级按钮)同一套语义,两处不同源的话「敲 `..` 回车」和「点 ↑」会去到
//! 两个不同的地方。

use mullion_ssh::sftp::RemotePath;

/// POSIX 路径的纯字符串规整:吃掉空段与 `.`,`..` 就地回退一级,到根为止。
/// **不解析符号链接** —— 与 `RemotePath::parent()` 同一套语义(见模块头)。
fn normalize_posix(bytes: &[u8]) -> Vec<u8> {
    let mut out: Vec<&[u8]> = Vec::new();
    for seg in bytes.split(|b| *b == b'/') {
        match seg {
            b"" | b"." => {}
            b".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    let mut v = Vec::with_capacity(bytes.len().max(1));
    for seg in out {
        v.push(b'/');
        v.extend_from_slice(seg);
    }
    if v.is_empty() {
        v.push(b'/');
    }
    v
}

/// F131:远端路径条里敲的一串 → 要跳过去的绝对路径。
///
/// `None` = 这一下不跳(空输入,或 `~` 但登录目录还不知道)。
pub fn resolve_remote_input(
    input: &str,
    cwd: &RemotePath,
    home: Option<&[u8]>,
) -> Option<RemotePath> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    let b = s.as_bytes();
    let joined: Vec<u8> = if b.starts_with(b"/") {
        b.to_vec()
    } else if b == b"~" || b.starts_with(b"~/") {
        let home = home?;
        let mut v = home.to_vec();
        v.extend_from_slice(&b[1..]);
        v
    } else {
        let mut v = cwd.as_bytes().to_vec();
        v.push(b'/');
        v.extend_from_slice(b);
        v
    };
    Some(RemotePath::from_bytes(normalize_posix(&joined)))
}

/// F131:本地路径条里敲的一串 → 要跳过去的本地路径。
///
/// 规整交给 `std::path`,这里只负责「相对拼在当前目录后面」与 `~` 展开。
/// **用平台分隔符**(见 `local::join_local` 的说明:`D:\work/sub` 虽然能用,
/// 但拷给 PowerShell 很难看)。
///
/// 已知偏门行为:Windows 上的盘符相对路径(`C:foo`)会被 `PathBuf::join`
/// 整体替换掉当前目录。等价于「当绝对路径处理」,不会跳到别的目录去,
/// 不为它特判。
pub fn resolve_local_input(
    input: &str,
    cwd: &RemotePath,
    home: Option<&RemotePath>,
) -> Option<RemotePath> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    if s == "~" {
        return home.cloned();
    }
    if let Some(rest) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        let home = home?;
        return Some(crate::files::local::join_local(home, rest.as_bytes()));
    }
    let p = std::path::Path::new(s);
    if p.is_absolute() {
        return Some(RemotePath::from_bytes(crate::files::local::path_bytes(p)));
    }
    Some(crate::files::local::join_local(cwd, s.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rp(s: &str) -> RemotePath {
        RemotePath::from_bytes(s.as_bytes().to_vec())
    }

    /// 绝对路径原样走。
    #[test]
    fn an_absolute_path_goes_through_untouched() {
        assert_eq!(
            resolve_remote_input("/var/log", &rp("/home/dev"), Some(b"/home/dev")),
            Some(rp("/var/log"))
        );
    }

    /// `~` / `~/x` 用远端登录目录展开 —— openssh 的 sftp-server **不认 `~`**,
    /// 原样发过去只会得到「找不到」。
    #[test]
    fn tilde_expands_with_the_sftp_login_directory() {
        assert_eq!(
            resolve_remote_input("~", &rp("/var"), Some(b"/home/dev")),
            Some(rp("/home/dev"))
        );
        assert_eq!(
            resolve_remote_input("~/Mullion", &rp("/var"), Some(b"/home/dev")),
            Some(rp("/home/dev/Mullion"))
        );
    }

    /// 登录目录还不知道时,`~` 无从展开 —— 不跳(而不是把字面量 `~` 发过去
    /// 换一条看不懂的错误)。
    #[test]
    fn tilde_without_a_known_home_does_not_jump() {
        assert_eq!(resolve_remote_input("~/x", &rp("/var"), None), None);
    }

    /// 相对路径拼在当前目录后面。
    #[test]
    fn a_relative_path_is_joined_onto_the_current_directory() {
        assert_eq!(
            resolve_remote_input("nginx", &rp("/var/log"), Some(b"/home/dev")),
            Some(rp("/var/log/nginx"))
        );
        assert_eq!(
            resolve_remote_input("./nginx/", &rp("/var/log"), Some(b"/home/dev")),
            Some(rp("/var/log/nginx"))
        );
    }

    /// `..` 就地规整,跟点「上一级」去到同一个地方。
    #[test]
    fn dotdot_is_normalised_the_same_way_the_up_button_does_it() {
        assert_eq!(
            resolve_remote_input("..", &rp("/var/log"), None),
            Some(rp("/var"))
        );
        assert_eq!(
            resolve_remote_input("../lib/x", &rp("/var/log"), None),
            Some(rp("/var/lib/x"))
        );
        // 根之上还是根 —— 不能跑出一个 `/..` 这种打不开的路径。
        assert_eq!(
            resolve_remote_input("../..", &rp("/var"), None),
            Some(rp("/"))
        );
    }

    /// 空串 / 全空白 = 用户没打算跳,当取消。
    #[test]
    fn an_empty_input_is_a_cancel_not_a_jump_to_root() {
        assert_eq!(resolve_remote_input("", &rp("/var"), None), None);
        assert_eq!(resolve_remote_input("   ", &rp("/var"), None), None);
    }

    /// `~x` 不是 home 的写法(那是「叫 ~x 的目录」),不展开。
    #[test]
    fn tilde_glued_to_a_name_is_not_a_home_reference() {
        assert_eq!(
            resolve_remote_input("~x", &rp("/var"), Some(b"/home/dev")),
            Some(rp("/var/~x"))
        );
    }

    /// 本地栏:绝对路径原样,相对拼当前目录,`~` 用传进来的本地主目录。
    /// 用平台分隔符拼(`join_local` 的规矩),所以断言按平台算期望值。
    #[test]
    fn local_input_resolves_against_the_local_cwd_and_home() {
        let home = rp(&std::path::PathBuf::from("/h/dev").to_string_lossy());
        let cwd = rp(&std::path::PathBuf::from("/w").to_string_lossy());
        let got = resolve_local_input("sub", &cwd, Some(&home)).expect("该给出路径");
        assert_eq!(
            crate::files::local::to_path(&got),
            std::path::Path::new("/w").join("sub")
        );
        assert_eq!(resolve_local_input("  ", &cwd, Some(&home)), None);
        assert_eq!(resolve_local_input("~", &cwd, Some(&home)), Some(home));
    }
}
