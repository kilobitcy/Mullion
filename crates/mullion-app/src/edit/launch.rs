//! F53/设计 D13:用**系统默认程序**打开一个本地文件(外部编辑)。
//!
//! 与 `files::local::open_in_file_manager` 同一套做法:平台命令抽成纯函数,
//! 于是「参数有没有被拼进 shell 命令行」这件事在无头环境验得了 —— 真的
//! spawn 一个记事本在 CI 里既起不来也没法断言。

use std::ffi::OsString;
use std::path::Path;

/// 平台对应的「用默认程序打开这个文件」命令。
///
/// **绝不拼 shell 命令行**:路径直接交给 `Command::arg`,名字里的空格 /
/// 引号 / `$(...)` 一概不需要转义,也就没有注入面。
///
/// Windows 走 `cmd /c start "" <path>` 而不是 `explorer.exe <path>`:
/// 后者对没有关联程序的类型既不报错也不打开,用户只看到「点了没反应」。
/// `start` 的第一个引号串是**窗口标题**——省掉它的话,路径本身会被当成
/// 标题,于是什么都不会被打开。
fn open_command(path: &Path) -> (String, Vec<OsString>) {
    let p = path.as_os_str().to_os_string();
    #[cfg(windows)]
    {
        (
            "cmd".to_string(),
            vec!["/c".into(), "start".into(), "".into(), p],
        )
    }
    #[cfg(target_os = "macos")]
    {
        ("open".to_string(), vec![p])
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        ("xdg-open".to_string(), vec![p])
    }
}

/// 用系统默认程序打开。
///
/// 只保证「启动器起来了」,**不等编辑器退出**,也不认它的退出码:
/// VS Code 一个进程开一堆窗口,进程信号毫无意义(设计 D13)——
/// 「改没改」一律靠本地 mtime 轮询判定。
pub fn open_with_default(path: &Path) -> Result<(), String> {
    let (prog, args) = open_command(path);
    std::process::Command::new(&prog)
        .args(&args)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("打不开外部编辑器({prog}):{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 路径原样当成**一个参数**。拼进命令行字符串的话,
    /// `/tmp/a b/$(rm -rf ~)/x.conf` 这种名字就是一条注入。
    #[test]
    fn the_open_command_passes_the_path_as_a_single_argument() {
        let p = Path::new("/tmp/a b $(x)/nginx.conf");
        let (prog, args) = open_command(p);
        assert!(!prog.is_empty());
        assert_eq!(
            args.last().unwrap(),
            &OsString::from("/tmp/a b $(x)/nginx.conf"),
            "路径必须原样当一个参数,不许拼进命令行串"
        );
    }

    /// Windows 上 `start` 后面那个空标题不能省 —— 省了的话路径会被当成
    /// 窗口标题,什么都不会被打开,而命令返回成功。
    #[cfg(windows)]
    #[test]
    fn the_windows_command_keeps_the_empty_title_argument_before_the_path() {
        let (_, args) = open_command(Path::new("C:\\tmp\\a.conf"));
        assert_eq!(args[1], OsString::from("start"));
        assert_eq!(args[2], OsString::from(""), "start 的空标题参数不能省");
    }
}
