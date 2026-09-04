//! F220:远端**内部**的复制 / 移动。**先试 exec `cp -a` / `mv`,被拒则回退
//! SFTP 逐文件递归** —— 与 `remove_tree` 逐字对称的取舍(设计 D17):
//!
//! - 一律走 exec:sftp-only 账号(`ForceCommand internal-sftp`)会拒绝 exec,
//!   功能在那种账号上直接残缺。
//! - 一律逐文件:同一台机器上拷一个 `node_modules`,每个字节都要拉到客户端
//!   再送回去 —— 高延迟代理链路上是几十分钟对几秒的差别。
//!
//! **绝不跟随符号链接**:列举用 `list_dir`(readdir = lstat 语义),遇到
//! `EntryKind::Symlink` 用 `read_link` + `symlink` 原样重建,不进去。搞错了
//! 就是把链接指向的整个目录复制一遍。
//!
//! **`overwrite == true` 时的命令语义**:`cp -a src dst` / `mv src dst` 在
//! `dst` 已存在且是目录时,是把 src 拷成 `dst/basename(src)`(嵌进去),
//! **不是**替换 `dst`——`-f` 只救得了「目标是已存在的文件」这一种。这里
//! 统一先 `rm -rf -- dst` 再 `cp`/`mv`,让 exec 路径与 SFTP 回退路径(先
//! `remove_tree` 再拷)语义完全一致,而不是依赖 GNU 专有的 `--no-target-
//! directory`(busybox / BSD 上没有,而我们不知道对端是什么)。
//!
//! **调用方必须先过 `files::clip::is_within` 那道闸**(目标不能是源自身或
//! 其子孙):远端 `cp` 自己会拦,但下面这段递归是我们自己写的,会一直
//! 递归到把磁盘写满。

use std::sync::Arc;

use crate::exec::{exec, shell_quote};
use crate::session::SshConnection;
use crate::sftp::{EntryKind, RemotePath, SftpClient, SftpError};

/// 复制还是移动。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyMode {
    Copy,
    Move,
}

/// 这一次实际走了哪条路。调用方用它写日志 / 断言,不影响正确性。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferReport {
    Exec,
    Sftp,
}

/// `a` 是 `b` 的祖先,或两者相等。字节前缀 + `/` 边界判定,照
/// `files::clip::is_within`(`crates/mullion-app/src/files/clip.rs`)的写法——
/// 光比前缀会把 `/a/bb` 误判成 `/a/b` 的子孙:那是两个不相干的目录,合法的
/// 「拷贝 `/a/b` → `/a/bb`」不能被冤枉挡掉。
fn is_ancestor_or_self(a: &[u8], b: &[u8]) -> bool {
    if a == b || a == b"/" {
        return true;
    }
    b.len() > a.len() && b.starts_with(a) && b[a.len()] == b'/'
}

/// 把 `pairs` 里的每一条(源绝对路径 → 目标绝对路径)拷 / 挪过去。
///
/// `overwrite` 为真时目标可以已存在(命令先 `rm -rf` 目标、回退路上也先删
/// 目标);为假时调用方(`files::clip::plan_paste`)已经保证目标不存在。
///
/// 任一路径 `as_wire()` 过不了就**一个请求都不发**(同 `remove_tree`
/// 开头那道门):拿一串替换字符去 `cp` 是本项目能犯的最严重的错。
pub async fn transfer_into(
    sftp: &SftpClient,
    conn: &Arc<SshConnection>,
    pairs: &[(RemotePath, RemotePath)],
    mode: CopyMode,
    overwrite: bool,
) -> Result<TransferReport, SftpError> {
    for (a, b) in pairs {
        let _ = a.as_wire()?;
        let _ = b.as_wire()?;
    }
    // C1(2026-09 复核追加):`to` 是 `from` 的祖先(或反过来)时,「先
    // rm -rf 清目标再拷」(exec 覆盖分支)与「先 remove_tree(to) 再拷」
    // (SFTP 回退,下面那个循环)都会把 `from` 所在的那棵树连根删掉——
    // 真实 `cp -a /a/p/p /a/p` 自己会因为「不能把目录拷进它自身」拒绝,
    // 是我们加的「先删目标」把一个本该被拒绝的操作变成了不可逆的数据
    // 摧毁。`files::clip::is_within` 只判「`dst` 是 `src` 的子孙」这一个
    // 方向,管的是 app 层的 UX 提示;这里是 exec 和 SFTP 两条路都必经的
    // 唯一位置,一道纯内存比较同时保护两条路,成本是零往返——即使上游
    // 忘了挡,这里也不许把源删掉。挡住就一个请求都不发,失败方式同上面
    // `as_wire()` 那道门一致。
    for (from, to) in pairs {
        let (f, t) = (from.as_bytes(), to.as_bytes());
        if is_ancestor_or_self(f, t) || is_ancestor_or_self(t, f) {
            return Err(SftpError::Protocol(format!(
                "{} 与 {} 存在包含关系,不能互相覆盖",
                from.display(),
                to.display()
            )));
        }
    }
    if pairs.is_empty() {
        return Ok(TransferReport::Exec);
    }

    match try_exec(conn, pairs, mode, overwrite).await {
        Some(true) => return Ok(TransferReport::Exec),
        // 命令跑了但没成功(权限不足、磁盘满…):**不回退**。回退只会把
        // 同一个错误再犯一遍,而且是慢一千倍地犯(同 `remove_tree`)。
        Some(false) => {
            return Err(SftpError::Protocol(
                "远端 cp/mv 命令执行失败(可能是权限不足或磁盘已满)".into(),
            ))
        }
        None => {}
    }

    for (from, to) in pairs {
        if overwrite {
            // 目标可能是个非空目录 —— `rename`/逐文件写都盖不掉它。
            // 结果**故意丢弃**(M1):清场没做干净的话,残留会被随后的
            // `create_dir`(目标还在会报「已存在」)或 `open_write` 的
            // TRUNCATE 语义自然暴露出来,不会静默产出一份半新半旧的
            // 合并结果——不需要在这里就把错误值攥住。
            let _ = crate::remove_tree::remove_tree(sftp, conn, to).await;
        }
        match mode {
            CopyMode::Copy => copy_one(sftp, from, to).await?,
            CopyMode::Move => {
                // 同一文件系统内 `rename` 是一次往返就完事的最优解;
                // 跨设备(EXDEV)会失败,那时才退成「拷完删源」。
                if sftp.rename(from, to).await.is_err() {
                    copy_one(sftp, from, to).await?;
                    crate::remove_tree::remove_tree(sftp, conn, from).await?;
                }
            }
        }
    }
    Ok(TransferReport::Sftp)
}

/// 拼一条命令发出去。`None` = 对端**拒绝**执行(sftp-only 账号),该回退;
/// `Some(false)` = 命令跑了但失败了。
async fn try_exec(
    conn: &Arc<SshConnection>,
    pairs: &[(RemotePath, RemotePath)],
    mode: CopyMode,
    overwrite: bool,
) -> Option<bool> {
    // 每一对一条子命令,`&&` 串起来 —— 一次往返干完整批,且前一条失败
    // 就停(半途而废好过继续往一个已经出错的目标里灌)。
    let head: &[u8] = match mode {
        CopyMode::Copy => b"cp -a -- ",
        CopyMode::Move => b"mv -- ",
    };
    let mut cmd: Vec<u8> = Vec::new();
    for (from, to) in pairs {
        if !cmd.is_empty() {
            cmd.extend_from_slice(b" && ");
        }
        if overwrite {
            // `cp -a`/`mv` 撞上一个已存在的目标目录时是嵌进去,不是替换
            // (模块文档)。先清干净目标、再放,和 SFTP 回退路径同一套
            // 语义:「先删目标再拷」。
            cmd.extend_from_slice(b"rm -rf -- ");
            cmd.extend_from_slice(&shell_quote(to.as_bytes()));
            cmd.extend_from_slice(b" && ");
        }
        cmd.extend_from_slice(head);
        cmd.extend_from_slice(&shell_quote(from.as_bytes()));
        cmd.push(b' ');
        cmd.extend_from_slice(&shell_quote(to.as_bytes()));
    }
    match exec(conn, cmd).await {
        Ok(out) => Some(out.succeeded()),
        Err(_) => None,
    }
}

/// SFTP 回退:拷一条(文件 / 链接 / 整棵目录树)。**不跟随链接**。
///
/// **递归用 `Box::pin`,不是显式栈**(与 `remove_tree::remove_via_sftp`
/// 不同的取舍,2026-09 复核 I1):这里的拷贝要等子项全部拷完、算出目标
/// 目录的最终内容之后,才轮到给这一层的父目录 `set_permissions`——
/// 后序语义,换成显式栈要把「哪一层轮到设权限」这份状态搬到调用方手里
/// 自己维护,复杂度在这个切片里不值当。代价是 `Box::pin` 只解决了
/// 「递归 async fn 类型无限大、编译不过」,poll 时的调用栈深度一点没
/// 减——父 future 的 `poll()` 仍然同步调子 future 的 `poll()`,深目录树
/// 有栈溢出风险,与 `remove_tree.rs:73-74` 那条注释说的是同一个模式,
/// 这里只是选了不同的应对(即:没应对,只是记在这里别让人以为两边一样安全)。
async fn copy_one(sftp: &SftpClient, from: &RemotePath, to: &RemotePath) -> Result<(), SftpError> {
    let meta = sftp.stat(from).await?;
    match meta.kind {
        EntryKind::Dir => {
            sftp.create_dir(to).await?;
            // 列举用 `list_dir`(readdir = lstat 语义):链接在这里是
            // `Symlink`,不会被当成它指向的目录。
            for e in sftp.list_dir(from).await? {
                let child_from = from.join(e.name.as_bytes());
                let child_to = to.join(e.name.as_bytes());
                Box::pin(copy_one(sftp, &child_from, &child_to)).await?;
            }
            sftp.set_permissions(to, meta.mode & 0o7777).await?;
        }
        EntryKind::Symlink => {
            // 原样重建,**不跟进去**。目标从 `read_link` 单独问一次:
            // `stat`(lstat 语义)恒返回 `link_target: None`(只有
            // `list_dir` 的 readdir 顺带过一次 readlink),而这里的
            // `from` 不一定来自某次 `list_dir`——顶层这一条粘贴的可能
            // 就是链接本身,没有「父目录列表」可复用。
            let target = sftp.read_link(from).await?;
            sftp.symlink(to, &target).await?;
        }
        EntryKind::File => {
            copy_file_bytes(sftp, from, to).await?;
            sftp.set_permissions(to, meta.mode & 0o7777).await?;
        }
        // 设备 / FIFO / socket(2026-09 复核 I3):`open_read` 对一个没有
        // 写端的具名管道会永久阻塞在第一次 `read_chunk`,这个 await 没有
        // 超时,用户唯一的出路是断开整条连接。明确拒绝好过悄悄挂起——
        // 同「认不出的 exec 命令行回 127」那条哲学一致。
        EntryKind::Other => {
            return Err(SftpError::Protocol(format!(
                "远端 {} 不是普通文件、目录或符号链接,已跳过",
                from.display()
            )));
        }
    }
    Ok(())
}

/// 一个普通文件的字节搬运。分块走,不整个读进内存 —— 远端上一个几 GB 的
/// core dump 会把客户端 OOM 掉(同 `read_all` 那条上限的理由)。
async fn copy_file_bytes(
    sftp: &SftpClient,
    from: &RemotePath,
    to: &RemotePath,
) -> Result<(), SftpError> {
    let mut src = sftp.open_read(from).await?;
    let mut dst = sftp.open_write(to, true).await?;
    // 256 KiB:对齐 russh-sftp 的单包上限,往返数才是成本(F214)。
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = src.read_chunk(&mut buf).await?;
        if n == 0 {
            break;
        }
        dst.write_chunk(&buf[..n]).await?;
    }
    dst.finish().await
}
