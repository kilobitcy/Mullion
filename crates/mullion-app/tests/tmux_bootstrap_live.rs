//! F124:拿**真的 tmux** 跑一遍自举命令,断言两个选项确实被改了。
//!
//! 这不是脚手架 —— 它验的是「我们拼的那条命令串在真 tmux 上到底管不管用」,
//! 而那正是这个功能唯一会悄悄坏掉的地方(tmux 改选项名、改 format 语法)。
//! 走本机 `sh -c`,不走 SSH:命令串是共享的,SSH 那一段由 `mullion-ssh` 的
//! live 测试覆盖,这里没必要再要一台真机。
//!
//! 跑法(需要本机装了 tmux):
//! ```bash
//! cargo test -p mullion-app --test tmux_bootstrap_live -- --ignored
//! ```
//! 打的是 `-L mullion-test` 这个**隔离 socket**,不碰开发机上真在用的那个
//! tmux 服务器。

use std::process::Command;

const SOCK: &str = "mullion-test";

fn tmux(args: &[&str]) -> std::process::Output {
    Command::new("tmux")
        .args(["-L", SOCK])
        .args(args)
        .output()
        .expect("跑 tmux")
}

/// 收尾:**断言失败(panic)那条路径也要把 server 收掉**。放在函数末尾的一行
/// `kill-server` 只在全绿时执行,一旦假红就会把一个真活着的 tmux server 留在
/// 开发机上,直到下次有人手动跑这个测试。
struct KillOnDrop;

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = tmux(&["kill-server"]);
    }
}

#[test]
#[ignore = "要本机装 tmux;用 --ignored 跑"]
fn the_bootstrap_command_really_turns_tmux_reporting_on() {
    // 干净起点 + 无论怎么退出都收尾。
    let _ = tmux(&["kill-server"]);
    let _cleanup = KillOnDrop;

    // 1. 服务器不在时,命令必须**失败**且**不会顺手拉起一个空 server** ——
    //    退出码 0 的话我们会把「没配上」latch 成 done,永不再试。
    let cmd = String::from_utf8(mullion_app::remote_bootstrap::bootstrap_command_with(
        &format!("tmux -L {SOCK}"),
    ))
    .expect("命令是 ASCII");
    let cold = Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .output()
        .expect("跑 sh");
    assert!(
        !cold.status.success(),
        "tmux 服务器不在时自举居然成功了 —— 成功判据失效"
    );
    assert!(
        !tmux(&["ls"]).status.success(),
        "自举把一个空 tmux 服务器拉起来了 —— 会在用户机器上留下幽灵 server"
    );

    // 2. 有服务器时:成功,而且两个选项真的变了。
    assert!(
        tmux(&["new-session", "-d", "-s", "boot"]).status.success(),
        "起不来测试用的 tmux 会话"
    );
    let hot = Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .output()
        .expect("跑 sh");
    assert!(
        hot.status.success(),
        "自举在活着的 tmux 上失败了:{}",
        String::from_utf8_lossy(&hot.stderr)
    );

    let titles = String::from_utf8_lossy(&tmux(&["show", "-g", "set-titles"]).stdout).to_string();
    assert!(
        titles.contains("set-titles on"),
        "set-titles 没被打开:{titles}"
    );

    let fmt =
        String::from_utf8_lossy(&tmux(&["show", "-g", "set-titles-string"]).stdout).to_string();
    assert!(
        fmt.contains(mullion_app::remote_bootstrap::TMUX_TITLES_STRING),
        "set-titles-string 不是我们那串:{fmt}"
    );

    // 3. 上面那条只证明「tmux 把我们发的字符串原样存下了」—— 它的期望值和实际值
    //    **来自同一个常量**,常量改成什么它都绿。而 tmux 对**不认识的 format
    //    token 是静默接受**的:`set -g set-titles-string '#{pane_bogus}'` 退出码 0、
    //    `show -g` 原样回显,只有真去求值时才会展开成空串。也就是说光靠上面那条,
    //    「tmux 改了 format 语法」这个本文件头注释声称要守的风险**根本没守住**。
    //
    //    所以再把这串**在真 pane 上求一次值**,断言两段都出了东西:
    //    - 开头是 `boot:` → `#S` 认得(会话名那段,`parse_title` 靠它认 tmux 名)
    //    - 末段是绝对路径 → `#{pane_current_path}` 认得(目录名 + SFTP 目录继承靠它)
    let expanded = String::from_utf8_lossy(
        &tmux(&[
            "display",
            "-p",
            "-t",
            "boot",
            mullion_app::remote_bootstrap::TMUX_TITLES_STRING,
        ])
        .stdout,
    )
    .trim()
    .to_string();
    assert!(
        expanded.starts_with("boot:"),
        "`#S:#I:#W` 那段没求值出会话名:{expanded:?}"
    );
    // 切**第一个**空格,不是最后一个:模板里我们只插了一个字面空格,而路径自己
    // 可以带空格(`/mnt/c/Users/John Doe/…`)。用 `rsplit` 的话那种 cwd 下会假红。
    assert!(
        expanded
            .split_once(' ')
            .is_some_and(|(_, path)| path.starts_with('/')),
        "空格后不是绝对路径 —— `#{{pane_current_path}}` 这个 token tmux 不认了:{expanded:?}"
    );
}
