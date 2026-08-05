//! 扫描 `~/.ssh` 下「看起来像私钥」的文件（F93）。
//!
//! **只看文件名，绝不读内容。** 私钥是这台机器上最敏感的文件；为了在下拉框里
//! 多显示一行候选而去读它、解析它、判断它有没有口令，是拿风险换便利。
//! 同理不打印路径到日志。
//!
//! 代价是判定只能靠命名约定，会有误收（一个恰好叫 `id_notes` 的文本文件）
//! 和漏收（叫 `bastion` 且没有 `.pub` 兄弟的真私钥）。这是**候选列表**，
//! 不是自动选择——用户仍可手输或用「浏览…」，误判的成本是多看一行。

use std::path::{Path, PathBuf};

/// 单次扫描最多返回的条目数。`~/.ssh` 正常只有个位数文件；真碰上几万个
/// 文件的目录，画一个几万项的下拉框只会把 UI 卡死。截断即可，不报错。
const MAX_ENTRIES: usize = 512;

/// 扫描 `dir`，返回按文件名排序的候选私钥路径。
///
/// 目录不存在、没权限、不是目录 —— 一律返回空 vec。这些都只是
/// 「没有候选」，不是需要打扰用户的错误（Windows 上从没用过 OpenSSH
/// 的机器根本没有 `~/.ssh`）。
pub fn scan(dir: &Path) -> Vec<PathBuf> {
    let it = match std::fs::read_dir(dir) {
        Ok(it) => it,
        Err(_) => return Vec::new(),
    };

    // 先收一遍文件名，`looks_like_key` 要靠它判断有没有 `.pub` 兄弟。
    let mut files: Vec<String> = Vec::new();
    for entry in it.flatten().take(MAX_ENTRIES) {
        // 必须跟随符号链接：`~/.ssh/id_work` 常常只是指向别处（加密卷、
        // 另一台机器挂载点）真实私钥的软链，不跟随会把这类候选漏掉。
        //
        // `DirEntry::metadata()` 和 `DirEntry::file_type()` 在这件事上行为
        // 相同——两者都**不**跟随符号链接，只描述链接本身。真正跟随的是
        // 自由函数 `std::fs::metadata()`。这一点反直觉，容易被"优化"回
        // `entry.metadata()`，所以在这里写清楚。
        //
        // 跟随之后要 stat 链接目标，可能失败（悬空链接、目标无权限、
        // 链接成环）。失败一律 `continue`，当作"没这个候选"——跟本模块
        // 其余地方一样，扫描失败不打扰用户，不报错。
        let Ok(md) = std::fs::metadata(entry.path()) else {
            continue;
        };
        if !md.is_file() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue; // 非 UTF-8 文件名：显示不了，也就选不了。
        };
        files.push(name);
    }

    let mut out: Vec<PathBuf> = files
        .iter()
        .filter(|n| looks_like_key(n, &files))
        .map(|n| dir.join(n))
        .collect();
    out.sort();
    out
}

/// 返回默认的 `~/.ssh` 路径。取不到 home 时返回 `None`（不 panic）。
///
/// 用 `directories`（`shell/store.rs` 已经在用），而不是自己读 `HOME` ——
/// Windows 上正确的变量是 `USERPROFILE`，手写这段迟早在一等公民平台上出错。
pub fn default_ssh_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|b| b.home_dir().join(".ssh"))
}

/// 靠命名约定判断 `name` 像不像私钥。`siblings` 是同目录下的全部文件名。
fn looks_like_key(name: &str, siblings: &[String]) -> bool {
    if is_known_non_key(name) {
        return false;
    }
    // 线索一：有同名 `.pub` 兄弟。ssh-keygen 默认成对生成，这条最准。
    let has_pub = siblings.iter().any(|s| s == &format!("{name}.pub"));
    // 线索二：`id_` 前缀。ssh-keygen 的默认命名（id_rsa / id_ed25519…），
    // 公钥被删掉时只剩这条线索。
    has_pub || name.starts_with("id_")
}

/// `.ssh` 里那几个众所周知**不是**私钥的文件。
///
/// `config` / `authorized_keys` / `known_hosts*` 这三支在当前
/// `looks_like_key` 的判据下其实够不着——它们既没有 `.pub` 兄弟也没有
/// `id_` 前缀，过不了第一层线索判断，走不到这里就已经被排除。留着是
/// 防御性的：万一以后放宽线索（比如加一条「无扩展名即候选」），这三支
/// 立刻就有用，不留会在那时候悄悄漏收。
fn is_known_non_key(name: &str) -> bool {
    name.ends_with(".pub")
        || name == "config"
        || name == "authorized_keys"
        // known_hosts / known_hosts.old / known_hosts2 …
        || name.starts_with("known_hosts")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 造一个假的 .ssh 目录。返回 tempdir(必须持有,drop 即删)。
    fn ssh_dir(names: &[&str]) -> tempfile::TempDir {
        let d = tempfile::tempdir().expect("建临时目录");
        for n in names {
            fs::write(d.path().join(n), b"not a real key").expect("写文件");
        }
        d
    }

    fn names(paths: &[std::path::PathBuf]) -> Vec<String> {
        paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    /// F93:只认两种线索 —— 有同名 `.pub` 兄弟,或文件名以 `id_` 开头。
    /// 不读内容,所以只能靠命名约定。
    ///
    /// 自证变红的方式:把 `looks_like_key` 里 `has_pub` 那一支删掉。
    #[test]
    fn picks_id_prefixed_and_pub_paired_only() {
        let d = ssh_dir(&[
            "id_ed25519",
            "id_ed25519.pub",
            "work-bastion", // 有 .pub 兄弟 → 收
            "work-bastion.pub",
            "id_rsa",    // id_ 前缀,没 .pub → 收
            "notes.txt", // 既无 .pub 也不是 id_ → 不收
        ]);
        let got = names(&scan(d.path()));
        assert_eq!(
            got,
            vec!["id_ed25519", "id_rsa", "work-bastion"],
            "应按文件名排序"
        );
    }

    /// F93:`.ssh` 里那几个众所周知的非私钥文件必须排除,
    /// 否则用户在下拉里看到 `config` 会当成能选的东西。
    ///
    /// 自证变红的方式:把 `is_known_non_key` 的 `name.ends_with(".pub")` 分支
    /// 删掉。这条用例里真正被覆盖到的只有这一支——`config`/`authorized_keys`/
    /// `known_hosts*` 既没有 `id_` 前缀也没有 `.pub` 兄弟，本来就过不了
    /// `looks_like_key` 的第一层线索判断，删掉它们对应的分支这条用例也不会变红。
    /// `id_ed25519.pub` 则以 `id_` 开头,少了 `.pub` 分支会被误收进结果。
    #[test]
    fn excludes_config_known_hosts_and_pub() {
        let d = ssh_dir(&[
            "config",
            "known_hosts",
            "known_hosts.old",
            "authorized_keys",
            "id_ed25519.pub",
            "id_ed25519",
        ]);
        assert_eq!(names(&scan(d.path())), vec!["id_ed25519"]);
    }

    /// F93:目录不存在(Windows 上很常见 —— 从没用过 OpenSSH)或没权限读,
    /// 都只是「没有候选」,不是错误。绝不能让扫描失败冒泡成弹窗。
    ///
    /// 自证变红的方式:把 `read_dir` 的 `Ok(it) => it, Err(_) => return Vec::new()`
    /// 改成 `.expect(..)`。
    #[test]
    fn missing_dir_returns_empty_without_error() {
        let d = tempfile::tempdir().expect("建临时目录");
        let gone = d.path().join("no-such-dir");
        assert!(scan(&gone).is_empty());
    }

    /// F93:符号链接必须跟随到目标再判断——指向**目录**的链接要排除
    /// (比如误建了 `id_trap` 指向一个目录),指向**文件**的链接要收
    /// (比如 `~/.ssh/id_work` 是指向加密卷里真实私钥的软链,这是常见用法)。
    ///
    /// 自证变红的方式:把 `scan()` 里的 `std::fs::metadata(entry.path())`
    /// 换成 `entry.metadata()`(或 `entry.file_type()`)。二者不等价:
    /// `DirEntry::metadata()`/`file_type()` 在 Unix 上都**不**跟随符号链接
    /// (只描述链接本身),只有自由函数 `std::fs::metadata(path)` 跟随。
    /// 换回去之后 `id_work`(指向文件的链接)会从结果里消失,断言失败。
    #[cfg(unix)]
    #[test]
    fn symlinks_follow_to_target_so_dir_links_are_excluded_and_file_links_are_kept() {
        let d = tempfile::tempdir().expect("建临时目录");
        // 指向目录的链接 → 不该收。
        fs::create_dir(d.path().join("real_dir")).expect("建子目录");
        std::os::unix::fs::symlink(d.path().join("real_dir"), d.path().join("id_trap"))
            .expect("建目录符号链接");

        // 指向扫描目录之外的真实文件的链接 → 该收(贴近「私钥放别处,
        // .ssh 里只放软链」的真实用法)。
        let elsewhere = tempfile::tempdir().expect("建另一个临时目录");
        let real_key = elsewhere.path().join("real_key");
        fs::write(&real_key, b"x").expect("写目标文件");
        std::os::unix::fs::symlink(&real_key, d.path().join("id_work")).expect("建文件符号链接");

        assert_eq!(names(&scan(d.path())), vec!["id_work"]);
    }
}
