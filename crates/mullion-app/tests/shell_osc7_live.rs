//! F156-c:拿**真的 bash** 跑一遍注入串,再把它吐出来的字节喂给
//! `mullion_term` 的 OSC 7 解析,断言解出来的路径就是那个 `$PWD`。
//!
//! 这条测试验的是整条链路最容易错的一环:转义要在「Rust 字面量 → shell
//! 单引号 → printf 格式串」三层之间穿过去。任何一层漏一个反斜杠,远端都
//! 只会**安静地什么都不发** —— 没有报错、没有日志,只有 SFTP 面板停在 `~`。
//! `shell_bootstrap` 的单元测试只钉了字符串的形状,证明不了这件事。
//!
//! 走本机 `bash -c`,不走 SSH:注入串是共享的,SSH 那一段由 mullion-ssh 的
//! live 测试覆盖,这里没必要再要一台真机。开发机上有 bash,所以**不加
//! `#[ignore]`**,进常规 `cargo test --workspace`。
#![cfg(unix)]

use std::os::unix::ffi::OsStrExt;
use std::process::Command;

/// 测试目录名里同时放一个空格和一个 `%s`:
/// - `%s` 钉住「`$PWD` 走 printf 的**参数**,不是拼进格式串」。拼进去的话
///   这个 `%s` 会被当成格式符、吃掉一个不存在的参数、展开成空 —— 吐出来的
///   是一条**错的绝对路径**,而错的绝对路径骗得过下游所有校验。
/// - 空格钉住 `"$PWD"` 外面那对双引号没被漏掉(漏了就在空格处断成两段)。
const DIR_NAME: &str = "mullion osc7 100%s";

/// 收尾:断言失败(panic)那条路径也要把目录删掉。
struct RmOnDrop(std::path::PathBuf);

impl Drop for RmOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

#[test]
fn a_real_bash_reports_the_directory_the_injection_asks_for() {
    let dir = std::env::temp_dir().join(DIR_NAME);
    std::fs::create_dir_all(&dir).expect("建测试目录");
    let _cleanup = RmOnDrop(dir.clone());
    // 规范化:`temp_dir()` 可能含软链,而 bash 的 `$PWD` 报的是 `getcwd()`
    // 的结果(见下面 `env_remove("PWD")`)。两边不走同一条路的话,断言会在
    // 一个跟本功能毫无关系的地方假红。
    let dir = std::fs::canonicalize(&dir).expect("规范化测试目录");

    let line =
        String::from_utf8(mullion_app::shell_bootstrap::osc7_setup_line()).expect("注入串是 ASCII");

    // `--noprofile --norc`:不读开发机上这个用户的 rc,免得他自己的
    // `PROMPT_COMMAND` 把结论搅浑。
    // `env_remove("PWD")`:继承来的 PWD 是 cargo 的工作目录,跟
    // `current_dir` 不一致。清掉之后 bash 自己从 `getcwd()` 填,与上面的
    // `canonicalize` 对得上。
    // `TERM=dumb`:`clear` 在这里可能失败,无所谓 —— 它不是最后一条命令。
    // 非交互 bash 不会自己跑 `PROMPT_COMMAND`,所以显式再调一次那个函数。
    let out = Command::new("bash")
        .args(["--noprofile", "--norc", "-c"])
        .arg(format!("{line}__mullion_osc7\n"))
        .current_dir(&dir)
        .env_remove("PWD")
        .env("TERM", "dumb")
        .output()
        .expect("跑 bash");
    assert!(
        out.status.success(),
        "注入串在真 bash 上跑不通:stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // 喂给生产用的那个嗅探器,而不是自己写一个正则 —— 这条测试要验的正是
    // 「我们发出去的东西,我们自己解得回来」。
    let mut sniffer = mullion_term::remote_state::Osc7Sniffer::default();
    let got = sniffer.feed(&out.stdout).unwrap_or_else(|| {
        panic!(
            "bash 跑完了,却没解出一条 OSC 7 —— 转义在某一层被吃掉了。\
             stdout={:?} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    assert_eq!(
        String::from_utf8_lossy(&got),
        String::from_utf8_lossy(dir.as_os_str().as_bytes()),
        "解出来的目录不是 bash 当时所在的那个"
    );
}
