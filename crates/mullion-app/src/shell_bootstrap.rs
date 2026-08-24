//! F156-c:非 tmux 场景的 shell OSC 7 自举 —— 纯逻辑。零 IO、零 async,
//! 真正往 PTY 写在 `app.rs` 的 `App::on_pane_ready`。
//!
//! 为什么需要:非 tmux 时 `PaneState.cwd` 一条腿都没有 ——
//! Ubuntu 的 bash **默认不发 OSC 7**,而「窗口标题」那条腿只要 PS1 被
//! starship / oh-my-bash / 自定义 rc 接管就断。用户报的
//! 「`Ctrl+Shift+B` 经常留在 `~`」就是这个。tmux 场景能跟住,是因为 F124
//! 把 `#{pane_current_path}` 塞进了 tmux **自己**发的标题,绕开了 shell。
//!
//! 这是 F124 的 shell 版:pane 的 shell channel 一建立就往 PTY 写一次,
//! 让远端 shell 从此每个提示符发一次 OSC 7。**不写远端任何文件** ——
//! 这条命令只活在这条 shell 的内存里,断开即消失。那正是它能默认开启、
//! 而「往 `~/.bashrc` 追加」不能的原因。
//!
//! 与 `remote_bootstrap`(F124)同构,但两者是**两个独立开关**:那个改的是
//! 远端 tmux 服务器内存里的全局选项,这个往用户当前这条 shell 里写命令
//! 并清屏。副作用不同,想只关掉其中一件是合理诉求。

/// 注入给远端 shell 的那一行(**不含结尾换行**,由 [`osc7_setup_line`] 补)。
///
/// 逐处的理由:
/// - **前导一个空格**:Ubuntu 默认 `HISTCONTROL=ignoreboth`(含 `ignorespace`),
///   这条就不进 shell history。不是所有发行版都这么配,所以它是**尽力而为**,
///   不是保证。
/// - **`printf '...%s...' "$PWD"` 而不是把 `$PWD` 拼进格式串**:目录名含 `%`
///   时会被 printf 当格式符吃掉,吐出一条**错的绝对路径** —— 而错的绝对路径
///   骗得过下游所有「是不是绝对路径」的校验,会把 SFTP 面板带到一个不存在的
///   目录去。这一条由 `tests/shell_osc7_live.rs` 拿真 bash 钉住。
/// - **主机名段留空**(`file:///path`):`parse_osc7` 本来就忽略主机名段
///   (它在 tmux/容器里经常是错的)。留空省掉 `$HOSTNAME`(bash)与 `$HOST`(zsh)
///   的差异,注入串短一截、少一处能出错的地方。
/// - **`${PROMPT_COMMAND:+;$PROMPT_COMMAND}` 保留用户原有的**:直接覆盖的话
///   会把用户自己那条(很可能正是发窗口标题的那条,也就是 F123 的另一条腿)
///   一起干掉,换成净负收益。
/// - **函数名带 `__mullion_` 前缀**:双下划线开头 + 项目名,撞用户已有函数的
///   概率可以忽略;真撞了也是覆盖我们自己的,不会破坏用户的 shell。
/// - **末尾 `clear`**:用户拍板要清屏。代价是 motd / 登录横幅一起被清掉。
///
/// 已知限制(进人工验收清单):fish / csh 下这一行是语法错误,屏幕上会打一行
/// 报错(fish 3.x 起本来就默认发 OSC 7,不做兼容,用户可以关掉开关);tmux
/// 场景无效但无害(tmux 吃掉内层 OSC 7 不转发,那个场景走 F124 那条腿);
/// 注入只发生在 pane 建立那一刻,用户之后在 pane 里 `ssh` 到第三台机器,
/// `PROMPT_COMMAND` 不会跟过去。
pub const OSC7_SETUP: &str = r#" __mullion_osc7() { printf '\033]7;file://%s\033\\' "$PWD"; }; if [ -n "$BASH_VERSION" ]; then PROMPT_COMMAND="__mullion_osc7${PROMPT_COMMAND:+;$PROMPT_COMMAND}"; elif [ -n "$ZSH_VERSION" ]; then precmd_functions+=(__mullion_osc7); fi; clear"#;

/// 真正写进 PTY 的字节:注入串 + 一个换行。
///
/// **换行不能省** —— 少了它这条命令只是躺在提示符上没有回车,用户敲的下一个
/// 字符会直接接在它后面,屏幕上出现一行莫名其妙的长命令。
pub fn osc7_setup_line() -> Vec<u8> {
    format!("{OSC7_SETUP}\n").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 没有换行 = 这条命令永远不会被执行,只是躺在提示符上,然后跟用户敲的
    /// 下一个字符拼成一行乱码。
    ///
    /// 自证会变红:把 `format!("{OSC7_SETUP}\n")` 里的 `\n` 去掉。
    #[test]
    fn the_line_ends_with_a_newline_so_the_shell_actually_runs_it() {
        let line = String::from_utf8(osc7_setup_line()).expect("注入串是 ASCII");
        assert!(line.ends_with('\n'), "没有换行,这条命令不会被执行:{line:?}");
        assert_eq!(line.matches('\n').count(), 1, "多了换行会多跑一个空提示符");
    }

    /// 前导空格是 `HISTCONTROL=ignorespace` 的钩子 —— 没有它,用户按一下 ↑
    /// 就是我们塞进去的这一长串,而他自己上一条命令被挤到第二格。
    ///
    /// 自证会变红:把 `OSC7_SETUP` 开头那个空格删掉。
    #[test]
    fn the_line_starts_with_a_space_so_it_stays_out_of_shell_history() {
        assert!(
            OSC7_SETUP.starts_with(' '),
            "少了前导空格,这条会进用户的 shell history:{OSC7_SETUP:?}"
        );
    }

    /// `$PWD` 必须当**参数**传给 printf,不能拼进格式串。
    ///
    /// 拼进去的话,目录名里的 `%` 会被 printf 当格式符吃掉,吐出一条错的
    /// **绝对路径** —— 而错的绝对路径骗得过下游所有「是不是绝对路径」的校验,
    /// SFTP 面板会被带到一个不存在的目录去(比不继承更糟)。
    ///
    /// 整条链路(Rust 字面量 → shell 单引号 → printf 格式串)由
    /// `tests/shell_osc7_live.rs` 拿真 bash 验;这里只钉形状。
    ///
    /// 自证会变红:把 `'...%s...' "$PWD"` 改成 `"...$PWD..."`。
    #[test]
    fn the_pwd_is_an_argument_not_spliced_into_the_format_string() {
        assert!(
            OSC7_SETUP.contains(r#"printf '\033]7;file://%s\033\\' "$PWD""#),
            "printf 的写法变了,目录名含 % 时会吐出一条错的绝对路径:{OSC7_SETUP:?}"
        );
    }

    /// 主机名段留空(`file://` 紧跟 `%s`,而 `%s` 展开出来的绝对路径自带
    /// 开头的 `/`,凑成 `file:///path`)。`parse_osc7` 忽略主机名段,拿
    /// `$HOSTNAME`/`$HOST` 去填只会多一处 bash/zsh 的差异。
    ///
    /// 自证会变红:把 `file://%s` 改成 `file://$HOSTNAME%s`。
    #[test]
    fn the_hostname_segment_is_left_empty() {
        assert!(OSC7_SETUP.contains("file://%s"), "{OSC7_SETUP:?}");
        assert!(
            !OSC7_SETUP.contains("HOSTNAME") && !OSC7_SETUP.contains("$HOST"),
            "别去填主机名段,那是 bash/zsh 变量名不一样的又一处坑:{OSC7_SETUP:?}"
        );
    }

    /// 用户原有的 `PROMPT_COMMAND` 必须保留。直接覆盖的话,会把他自己那条
    /// (很可能正是发窗口标题的那条,也就是 F123 的另一条腿)一起干掉 ——
    /// 那样我们补上一条腿、砍掉另一条,净收益可能是负的。
    ///
    /// 自证会变红:把 `"__mullion_osc7${PROMPT_COMMAND:+;$PROMPT_COMMAND}"`
    /// 改成 `"__mullion_osc7"`。
    #[test]
    fn the_users_own_prompt_command_is_kept() {
        assert!(
            OSC7_SETUP.contains("${PROMPT_COMMAND:+;$PROMPT_COMMAND}"),
            "会把用户自己的 PROMPT_COMMAND 覆盖掉:{OSC7_SETUP:?}"
        );
    }

    /// bash 与 zsh 各有一条分支 —— zsh 里 `PROMPT_COMMAND` 不是钩子,
    /// 只走 bash 那条的话 zsh 用户什么都收不到,而且不报错。
    ///
    /// 自证会变红:把 `elif [ -n "$ZSH_VERSION" ]` 那一整段删掉。
    #[test]
    fn both_bash_and_zsh_get_a_branch() {
        assert!(OSC7_SETUP.contains("$BASH_VERSION"), "{OSC7_SETUP:?}");
        assert!(OSC7_SETUP.contains("$ZSH_VERSION"), "{OSC7_SETUP:?}");
        assert!(
            OSC7_SETUP.contains("precmd_functions+=(__mullion_osc7)"),
            "zsh 那条没挂进 precmd_functions:{OSC7_SETUP:?}"
        );
    }

    /// 末尾清屏(用户拍板)。少了它,屏幕上会永久留着我们塞进去的这一长串。
    ///
    /// 自证会变红:把结尾的 `; clear` 删掉。
    #[test]
    fn the_line_clears_the_screen_when_it_is_done() {
        assert!(OSC7_SETUP.ends_with("; clear"), "{OSC7_SETUP:?}");
    }

    /// 整条注入串必须是 ASCII:它要穿过 PTY 直接进远端 shell,而远端的
    /// locale 我们不知道。非 ASCII 在 `LANG=C` 的机器上会变成一串问号,
    /// 而那时它已经是一条**语法不同**的命令了。
    #[test]
    fn the_whole_line_is_ascii() {
        assert!(OSC7_SETUP.is_ascii(), "{OSC7_SETUP:?}");
    }
}
