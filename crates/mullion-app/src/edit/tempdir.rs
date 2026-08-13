//! F53/设计 D13:外部编辑用的临时文件放哪。
//!
//! `<data_local>/Mullion/edit/<净化会话名>/<远端路径哈希>/<原文件名>`
//!
//! **原文件名要保留** —— 编辑器靠扩展名认语法高亮,把 `nginx.conf` 存成
//! `a1b2c3d4` 打开就是一片灰。目录用哈希分隔,于是两个同名不同路径的文件
//! (`/etc/app/config.toml` 与 `/srv/app/config.toml`)不会互相覆盖。

use std::path::{Path, PathBuf};

use mullion_ssh::sftp::RemotePath;

/// 临时根目录:`<data_local>/Mullion/edit`。
///
/// 取不到系统目录时退到 `std::env::temp_dir()` —— 编辑打不开比东西存到
/// 一个次优位置严重得多。
pub fn root() -> PathBuf {
    directories::BaseDirs::new()
        .map(|b| b.data_local_dir().to_path_buf())
        .unwrap_or_else(std::env::temp_dir)
        .join("Mullion")
        .join("edit")
}

/// 一个远端文件对应的本地临时路径。
pub fn temp_path(root: &Path, session: &str, remote: &RemotePath) -> PathBuf {
    root.join(sanitize(session))
        .join(format!("{:016x}", fnv1a64(remote.as_bytes())))
        .join(file_name_of(remote))
}

/// 会话名当目录名。**Windows 的非法字符全换成 `_`** —— 会话名是用户随手起的
/// (「生产机 / 备份」这种斜杠很常见),原样当目录名会拼出一个建不出来的路径,
/// 而报错会指向「编辑打不开」,没人会想到是会话名的问题。
///
/// 空名(或者净化完只剩空白)给一个固定占位,不留空段。
pub fn sanitize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => out.push('_'),
            c if (c as u32) < 0x20 => out.push('_'),
            c => out.push(c),
        }
    }
    // 尾随点/空格在 Windows 上会被静默吃掉,拼出来的父目录和我们记的那个
    // 就对不上了(同 F52 的 Windows 名字闸门)。
    let trimmed = out.trim_end_matches([' ', '.']).trim();
    if trimmed.is_empty() {
        return "session".to_string();
    }
    trimmed.to_string()
}

/// 远端路径的最后一段,拿来当本地文件名。同样要过 [`sanitize`] ——
/// POSIX 上合法的 `a:b.txt` 在 Windows 上建不出来。
fn file_name_of(remote: &RemotePath) -> String {
    let bytes = remote.as_bytes();
    let last = match bytes.iter().rposition(|b| *b == b'/') {
        Some(ix) if ix + 1 < bytes.len() => &bytes[ix + 1..],
        _ => bytes,
    };
    sanitize(&String::from_utf8_lossy(last))
}

/// FNV-1a 64。**自己写是为了不为一个临时目录名加一个哈希依赖**
/// (N6:exe 体积)。这里对哈希的要求只有两条:同一个远端路径永远算出同一个
/// 目录、不同路径极难撞上 —— 不是密码学强度。
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 删掉整个 `edit` 临时根目录(设计 D13:程序退出时清理)。
///
/// 失败**只记日志不打断退出** —— 用户已经点了「退出」,为了一个临时目录
/// 把窗口留在屏幕上是最糟的收场。
pub fn purge(root: &Path) {
    if !root.exists() {
        return;
    }
    if let Err(e) = std::fs::remove_dir_all(root) {
        log::warn!("清理编辑临时目录失败({}):{e}", root.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rp(s: &str) -> RemotePath {
        RemotePath::from_bytes(s.as_bytes().to_vec())
    }

    /// 两个同名不同路径的文件不能落到同一个本地文件上 —— 落到一起的话,
    /// 编辑 `/srv/app/config.toml` 会把 `/etc/app/config.toml` 的内容
    /// 回传到另一处去。
    #[test]
    fn two_different_remote_paths_never_share_a_temp_file() {
        let root = Path::new("/tmp/root");
        let a = temp_path(root, "prod", &rp("/etc/app/config.toml"));
        let b = temp_path(root, "prod", &rp("/srv/app/config.toml"));
        assert_ne!(a, b, "同名不同路径撞到了同一个临时文件");
        // 同一个远端路径必须**每次都算出同一个位置**,否则「上次编辑到一半」
        // 的文件下次找不回来,监视也就跟丢了。
        assert_eq!(a, temp_path(root, "prod", &rp("/etc/app/config.toml")));
        // 不同会话也要分开:两台机器上同路径的文件是两个东西。
        assert_ne!(a, temp_path(root, "staging", &rp("/etc/app/config.toml")));
    }

    /// 保留原文件名,编辑器才认得出类型(D13)。
    #[test]
    fn the_original_file_name_is_kept_so_the_editor_can_highlight_it() {
        let p = temp_path(Path::new("/tmp/root"), "prod", &rp("/etc/nginx/nginx.conf"));
        assert_eq!(p.file_name().unwrap(), "nginx.conf");
    }

    /// 会话名里的 Windows 非法字符会让整条路径建不出来,而报错指向
    /// 「编辑打不开」,没人会想到是会话名的锅。
    #[test]
    fn a_session_name_with_windows_illegal_characters_is_sanitized() {
        assert_eq!(sanitize("生产机 / 备份"), "生产机 _ 备份");
        assert_eq!(sanitize("a:b*c?"), "a_b_c_");
        assert_eq!(sanitize("尾随点."), "尾随点");
        assert_eq!(sanitize("   "), "session", "空名不许留出一个空目录段");
        // 反面:正常名字不许被改 —— 改了的话每次运行都可能落到不同目录。
        assert_eq!(sanitize("prod-web-01"), "prod-web-01");
    }

    /// 远端文件名同样要净化:POSIX 上合法的 `a:b` 在 Windows 上建不出来。
    #[test]
    fn a_remote_name_that_windows_cannot_store_is_sanitized_too() {
        let p = temp_path(Path::new("/tmp/root"), "prod", &rp("/data/a:b.txt"));
        assert_eq!(p.file_name().unwrap(), "a_b.txt");
    }
}
