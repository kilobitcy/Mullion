//! 本地文件栏的数据来源(设计 D5:本切片**只导航**,不删不改不建)。
//!
//! 复用 `mullion_ssh::sftp::Entry` 而不是另造一套类型 —— 两栏共用一套
//! 渲染与排序,D2 加拖拽时「本地↔远端」也才是同一种东西在两个方向上动。
//! 代价是本地路径也塞进 `RemotePath`(字节真源),这在 Windows 上是
//! `OsStr` 的有损投影:非 UTF-8 的 Windows 文件名(UTF-16 孤儿代理项)
//! 会走与远端同一条「显示得出、操作不了」的路。

use std::path::{Path, PathBuf};

use mullion_ssh::sftp::{Entry, EntryKind, RemotePath};

/// 列一个本地目录。错误是**已经格式化好的可读原因**。
pub fn list_dir(dir: &Path) -> Result<Vec<Entry>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| readable(dir, &e))?;
    let mut out = Vec::new();
    for item in rd {
        // 单条读不出来不该让整个目录列不出来,但**也不能静默消失** ——
        // 用户看到的是「目录里少了几个文件」,不留痕就查无可查。真实触发:
        // 读目录与 stat 之间文件被删(TOCTOU);Windows 上 OneDrive 占位符
        // 处于某些同步状态、或超过 260 字符的长路径,stat 会直接失败。
        let item = match item {
            Ok(i) => i,
            Err(e) => {
                log::warn!("跳过读不出的本地目录项({}):{e}", dir.display());
                continue;
            }
        };
        // `symlink_metadata` 而不是 `metadata`:不跟随链接,与远端的
        // lstat 语义对齐(D17)。跟随了的话,指向自己的链接会挂住列目录。
        let md = match item.path().symlink_metadata() {
            Ok(m) => m,
            Err(e) => {
                log::warn!("跳过取不到属性的本地条目 {}:{e}", item.path().display());
                continue;
            }
        };
        let ft = md.file_type();
        let kind = if ft.is_symlink() {
            EntryKind::Symlink
        } else if ft.is_dir() {
            EntryKind::Dir
        } else if ft.is_file() {
            EntryKind::File
        } else {
            EntryKind::Other
        };
        let link_target = if kind == EntryKind::Symlink {
            std::fs::read_link(item.path())
                .ok()
                .map(|p| RemotePath::from_bytes(path_bytes(&p)))
        } else {
            None
        };
        out.push(Entry {
            name: RemotePath::from_bytes(os_bytes(&item.file_name())),
            kind,
            size: md.len(),
            mtime: mtime_secs(&md),
            mode: perm_bits(&md),
            uid: 0,
            gid: 0,
            link_target,
        });
    }
    Ok(out)
}

fn readable(dir: &Path, e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => format!("目录不存在:{}", dir.display()),
        std::io::ErrorKind::PermissionDenied => format!("没有权限读取:{}", dir.display()),
        _ => format!("读取失败:{}({})", dir.display(), e.kind()),
    }
}

fn mtime_secs(md: &std::fs::Metadata) -> u32 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs().min(u32::MAX as u64) as u32)
        .unwrap_or(0)
}

/// Unix 上取真实权限位;Windows 上没有 mode 概念,只报只读与否 ——
/// 编一个 `rwxr-xr-x` 出来会让用户以为那是真的。
fn perm_bits(md: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        md.permissions().mode() & 0o7777
    }
    #[cfg(not(unix))]
    {
        if md.permissions().readonly() {
            0o444
        } else {
            0o644
        }
    }
}

#[cfg(unix)]
pub(crate) fn os_bytes(s: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    s.as_bytes().to_vec()
}

#[cfg(not(unix))]
pub(crate) fn os_bytes(s: &std::ffi::OsStr) -> Vec<u8> {
    // Windows 的 `OsStr` 是 UTF-16;`to_string_lossy` 是有损投影,
    // 孤儿代理项会变成 U+FFFD —— 与远端非 UTF-8 名走同一条
    // 「显示得出、操作不了」的路(D16 修订)。
    s.to_string_lossy().into_owned().into_bytes()
}

pub(crate) fn path_bytes(p: &Path) -> Vec<u8> {
    os_bytes(p.as_os_str())
}

/// `RemotePath` → 本地 `PathBuf`。
pub fn to_path(p: &RemotePath) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(p.as_bytes()))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(p.display().into_owned())
    }
}

/// 本地版的 join:**用平台分隔符**。`RemotePath::join` 恒用 `/`,那是
/// SFTP 线上的规矩;本地路径拼成 `D:\work/sub` 虽然 `std::path` 认,
/// 但用户拷走贴进 PowerShell 会看到一半斜杠一半反斜杠。
///
/// **`name` 只接受目录项名字,不接受用户手输的串。** `PathBuf::push`
/// 遇到绝对路径会**整体替换**而不是拼接(Windows 上 `C:\foo` 这类带盘符
/// 前缀的同理),`readdir` 给的名字里不可能有分隔符,所以现在安全;等以后
/// 接地址栏 / 书签跳转这类用户可控输入,要在调用点先挡住带分隔符的串。
pub fn join_local(base: &RemotePath, name: &[u8]) -> RemotePath {
    let mut p = to_path(base);
    p.push(to_path(&RemotePath::from_bytes(name.to_vec())));
    RemotePath::from_bytes(path_bytes(&p))
}

/// 上一级。已在根就原地不动(与 `RemotePath::parent` 同一条防呆)。
pub fn parent_local(p: &RemotePath) -> RemotePath {
    let path = to_path(p);
    match path.parent() {
        Some(up) if up != path => RemotePath::from_bytes(path_bytes(up)),
        _ => p.clone(),
    }
}

/// 默认本地目录:配置里填了就用它,留空用用户主目录,再拿不到就用
/// 当前工作目录 —— 任何一步都不 panic,面板宁可开在一个奇怪的地方
/// 也不能开不出来。
pub fn default_local(configured: Option<&str>) -> RemotePath {
    if let Some(s) = configured.filter(|s| !s.trim().is_empty()) {
        return RemotePath::from_bytes(s.as_bytes().to_vec());
    }
    let home = directories::BaseDirs::new()
        .map(|b| b.home_dir().to_path_buf())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    RemotePath::from_bytes(path_bytes(&home))
}

/// 用系统文件管理器打开一个本地目录(设计 D5:本地文件管理外包出去)。
///
/// 平台命令抽成 [`open_command`] 是为了能在无头环境验参数 —— 真的 spawn
/// 一个资源管理器在 CI 里既跑不起来也没法断言。
pub fn open_in_file_manager(dir: &RemotePath) -> Result<(), String> {
    let (prog, arg) = open_command(dir);
    std::process::Command::new(&prog)
        .arg(&arg)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("打不开文件管理器({prog}):{e}"))
}

/// 平台对应的「打开目录」命令。**不拼 shell 命令行** —— 直接给
/// `Command::arg`,路径里的空格 / 引号 / `$` 全都不需要转义,
/// 也就没有注入面。
fn open_command(dir: &RemotePath) -> (String, std::ffi::OsString) {
    let path = to_path(dir).into_os_string();
    #[cfg(windows)]
    {
        ("explorer.exe".to_string(), path)
    }
    #[cfg(target_os = "macos")]
    {
        ("open".to_string(), path)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        ("xdg-open".to_string(), path)
    }
}

/// 递归枚举出来的一个文件。
pub struct Walked {
    /// 相对枚举根的路径段。目标端按同样的层级重建 —— 只带文件名的话,
    /// 一棵三层目录树会被摊平糊到同一个目录下,同名文件互相覆盖。
    pub rel: Vec<String>,
    pub size: u64,
}

/// 递归枚举一个本地目录下的**普通文件**(F52 上传目录用)。
///
/// **不跟随符号链接**(与 `remove_tree` 的 D17 同款理由):跟随的话
/// 一个 `link -> /` 就是把整个根目录塞进传输队列。
pub fn walk_dir(root: &std::path::Path) -> Result<Vec<Walked>, String> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), Vec::<String>::new())];
    while let Some((dir, rel)) = stack.pop() {
        let rd = std::fs::read_dir(&dir).map_err(|e| format!("读不了 {}:{e}", dir.display()))?;
        for de in rd {
            let de = de.map_err(|e| format!("读不了 {}:{e}", dir.display()))?;
            // `symlink_metadata`(lstat)而不是 `metadata`(stat):
            // 后者会解引用链接,类型判定跟着变成目标的类型。
            let md = de
                .path()
                .symlink_metadata()
                .map_err(|e| format!("读不了 {}:{e}", de.path().display()))?;
            if md.is_symlink() {
                continue;
            }
            let mut child_rel = rel.clone();
            child_rel.push(de.file_name().to_string_lossy().into_owned());
            if md.is_dir() {
                stack.push((de.path(), child_rel));
            } else if md.is_file() {
                out.push(Walked {
                    rel: child_rel,
                    size: md.len(),
                });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 路径**原样**交给 `Command::arg`,不经 shell —— 名字里有空格或
    /// `$(...)` 的目录也不需要转义,更没有注入面。
    #[test]
    fn the_file_manager_command_passes_the_path_as_a_single_argument() {
        let d = RemotePath::from_bytes(b"/tmp/a b $(x)".to_vec());
        let (prog, arg) = open_command(&d);
        assert!(!prog.is_empty());
        assert_eq!(
            arg,
            std::ffi::OsString::from("/tmp/a b $(x)"),
            "路径必须原样当成一个参数,不许拼进命令行字符串"
        );
    }

    #[test]
    fn listing_a_local_directory_reports_kinds_and_sizes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hello").unwrap();

        let mut got = list_dir(dir.path()).expect("列本地目录");
        got.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        assert_eq!(got.len(), 2);

        let a = got.iter().find(|e| e.name.as_bytes() == b"a.txt").unwrap();
        assert_eq!(a.kind, EntryKind::File);
        assert_eq!(a.size, 5);

        let sub = got.iter().find(|e| e.name.as_bytes() == b"sub").unwrap();
        assert_eq!(sub.kind, EntryKind::Dir);

        // 符号链接要用 `symlink_metadata`(lstat)取类型,不能跟随
        // (`metadata`/stat 会把它跟随成目标的类型)。建符号链接在
        // Windows 上要管理员权限,这条断言只在 Unix 上跑。
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path().join("sub"), dir.path().join("link")).unwrap();
            let got2 = list_dir(dir.path()).expect("列本地目录(含符号链接)");
            let link = got2
                .iter()
                .find(|e| e.name.as_bytes() == b"link")
                .expect("符号链接应出现在列表里");
            assert_eq!(
                link.kind,
                EntryKind::Symlink,
                "symlink_metadata 应报 Symlink,不该跟随成目标的 Dir"
            );
        }
    }

    /// 读不了的目录要给**可读的原因**,不是 `Os { code: 13 }` 这种
    /// 用户看不懂的东西。
    #[test]
    fn an_unreadable_directory_yields_a_readable_reason() {
        let err = list_dir(std::path::Path::new("/definitely/not/here")).unwrap_err();
        assert!(!err.is_empty());
        assert!(
            !err.contains("Os {"),
            "别把 io::Error 的 Debug 直接丢给用户: {err}"
        );
        // `io::Error::to_string()`(如 "No such file or directory (os error 2)")
        // 不含路径,用户没法知道是哪个目录读不了 —— 必须带路径。
        assert!(
            err.contains("/definitely/not/here"),
            "错误信息必须点明是哪个目录: {err}"
        );
    }

    /// 本地路径也走 `RemotePath`(字节真源),**但分隔符按平台**:
    /// Windows 上拼出 `D:\work/sub` 虽然 `std::path` 认,显示出来很难看,
    /// 而且用户拷走贴进 PowerShell 会一半斜杠一半反斜杠。
    ///
    /// **这条在 Linux 宿主上没有区分力**:Linux 的平台分隔符本来就是 `/`,
    /// 把 `join_local` 简化成 `RemotePath::join`(恒 `/`)在这儿照样绿。
    /// 它只有在 `--target x86_64-pc-windows-gnu` 跑起来才扎得住 ——
    /// 本地 `cargo test` 通过不等于分隔符逻辑没被人顺手改简单。
    #[test]
    fn joining_a_local_path_uses_the_platform_separator() {
        let base = to_path(&RemotePath::from_bytes(b"/tmp".to_vec()));
        assert_eq!(base, std::path::PathBuf::from("/tmp"));
        let joined = join_local(&RemotePath::from_bytes(b"/tmp".to_vec()), b"sub");
        assert_eq!(
            to_path(&joined),
            std::path::PathBuf::from("/tmp").join("sub")
        );
    }

    /// 默认本地目录:配置留空 → 用户主目录。拿不到主目录(极少见)
    /// 也不能 panic,退到当前工作目录。
    ///
    /// 原先这条只断言「非空」—— 那把写死一个 `"x"`、或整个忽略入参的
    /// 实现全放过去了。判据换成:**必须是一个真的存在的目录**,再拿
    /// `HOME` 做**独立**核对(不抄实现里那串 `BaseDirs::new()`,抄一遍
    /// 就成了重言式:实现怎么错,测试跟着怎么错)。
    #[test]
    fn the_default_local_directory_falls_back_to_the_home_directory() {
        let d = default_local(None);
        assert!(
            to_path(&d).is_dir(),
            "默认目录得真的存在,否则面板一开就是一条读不出来的错误:{}",
            d.display()
        );
        if let Ok(home) = std::env::var("HOME") {
            assert_eq!(to_path(&d), PathBuf::from(home), "留空时开在用户主目录");
        }
    }

    /// 递归传输靠这个把「一个目录」摊成「一串文件 + 它们在目标端的
    /// 相对位置」。相对路径算错 = 整棵树糊到同一个目录下。
    #[test]
    fn walking_a_directory_yields_files_with_paths_relative_to_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("a");
        std::fs::create_dir_all(root.join("b")).unwrap();
        std::fs::write(root.join("one.txt"), b"1").unwrap();
        std::fs::write(root.join("b/two.txt"), b"22").unwrap();

        let mut got: Vec<(String, u64)> = walk_dir(&root)
            .expect("枚举")
            .into_iter()
            .map(|w| (w.rel.join("/"), w.size))
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec![("b/two.txt".to_string(), 2), ("one.txt".to_string(), 1)]
        );
    }

    /// D17 同款理由:跟随链接会把链接指向的整棵树拖进传输队列。
    /// 建符号链接在 Windows 上要管理员权限,只在 Unix 上跑。
    #[cfg(unix)]
    #[test]
    fn walking_does_not_follow_symlinks_out_of_the_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("a");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(tmp.path().join("target.txt"), b"x").unwrap();
        std::os::unix::fs::symlink(tmp.path().join("target.txt"), root.join("link")).unwrap();

        let got = walk_dir(&root).expect("枚举");
        assert!(
            got.iter().all(|w| w.rel != vec!["link".to_string()]),
            "符号链接不该进传输列表:{:?}",
            got.iter().map(|w| w.rel.clone()).collect::<Vec<_>>()
        );
    }

    /// 配置里填了就**原样**用它 —— 忽略入参的实现在上一条里是抓不到的
    /// (它照样返回一个存在的主目录)。
    #[test]
    fn a_configured_local_directory_is_used_verbatim() {
        let d = default_local(Some("/srv/incoming"));
        assert_eq!(d.as_bytes(), b"/srv/incoming");
        // 只有空白等于没填:面板不能开在一个名叫「   」的目录上。
        assert_ne!(default_local(Some("   ")).as_bytes(), b"   ");
    }
}
