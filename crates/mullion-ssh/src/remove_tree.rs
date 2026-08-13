//! 递归删除(F57)。**先试 `exec rm -rf`,被拒则回退 SFTP 逐文件递归。**
//!
//! 两条路都要有的理由(设计 D17):
//! - 一律走 exec:sftp-only 账号(`ForceCommand internal-sftp` +
//!   `ChrootDirectory`)会拒绝 exec,功能在那种账号上直接残缺。
//! - 一律逐文件:删一个 `node_modules` 要等到天荒地老(每文件一个 RTT,
//!   高延迟代理链路上这是几十分钟对几秒的差别)。
//!
//! **绝不跟随符号链接**:列举用 `list_dir`(readdir = lstat 语义),遇到
//! `EntryKind::Symlink` 一律当叶子删掉,不进去。搞错了就是把远端整个目标
//! 目录删了 —— 这是本模块最重要的一条不变量。

use std::sync::Arc;

use crate::exec::{exec, shell_quote};
use crate::session::SshConnection;
use crate::sftp::{EntryKind, RemotePath, SftpClient, SftpError};

/// 这一次删除实际走了哪条路。调用方用它写日志 / 断言,不影响正确性。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveReport {
    /// 走了 `exec rm -rf` 快路径。
    Exec,
    /// 回退到 SFTP 逐文件递归。
    Sftp,
}

/// 递归删掉一条(文件、链接或整棵目录树)。
///
/// `path` 发不出去(`as_wire` 被挡)时**一个请求都不发**,直接
/// `Err(SftpError::NonUtf8Name)` —— 拿一串替换字符去 `rm -rf` 是本项目
/// 能犯的最严重的错。
pub async fn remove_tree(
    sftp: &SftpClient,
    conn: &Arc<SshConnection>,
    path: &RemotePath,
) -> Result<RemoveReport, SftpError> {
    // **先过这道门**:后面两条路都不许再各自检查一遍(检查两遍就有一遍
    // 会被人改漏)。
    let _ = path.as_wire()?;

    match try_exec_rm(conn, path).await {
        Some(true) => return Ok(RemoveReport::Exec),
        // 命令跑了但没成功(权限不足、路径不存在…):**不回退**。回退只会
        // 把同一个错误再犯一遍,而且是慢一千倍地犯。
        Some(false) => {
            return Err(SftpError::Protocol(
                "远端 rm 命令执行失败(可能是权限不足)".into(),
            ))
        }
        // 命令压根没跑起来(对端拒绝 / 开不出 channel)—— 这才是该回退的信号。
        None => {}
    }
    remove_via_sftp(sftp, path).await?;
    Ok(RemoveReport::Sftp)
}

/// 试 `rm -rf`。`Some(true)` = 跑了且成功;`Some(false)` = 跑了但失败;
/// `None` = 压根没跑起来(该回退)。
async fn try_exec_rm(conn: &Arc<SshConnection>, path: &RemotePath) -> Option<bool> {
    // `--` 终止选项解析:名字以 `-` 开头的目录(`-rf` 这种真的存在)
    // 不加它会被 rm 当成选项。
    let mut cmd = b"rm -rf -- ".to_vec();
    cmd.extend_from_slice(&shell_quote(path.as_bytes()));
    match exec(conn, cmd).await {
        Ok(out) => Some(out.succeeded()),
        Err(_) => None,
    }
}

/// 回退路径:SFTP 逐文件递归。
///
/// 用显式栈而不是递归 `async fn` —— async 函数递归要装箱(`Box::pin`),
/// 而且深目录会把栈上的 future 堆得很大。
async fn remove_via_sftp(sftp: &SftpClient, root: &RemotePath) -> Result<(), SftpError> {
    // 先看这一条是什么。**lstat 语义**:指向目录的链接必须当叶子处理。
    if sftp.stat(root).await?.kind != EntryKind::Dir {
        return sftp.remove_file(root).await;
    }

    // 广度优先收集所有目录(浅→深),叶子文件边走边删。
    let mut dirs_in_order: Vec<RemotePath> = Vec::new();
    let mut pending: Vec<RemotePath> = vec![root.clone()];
    while let Some(dir) = pending.pop() {
        let entries = sftp.list_dir(&dir).await?;
        dirs_in_order.push(dir.clone());
        for e in entries {
            // 名字发不出去的条目:整棵树都删不干净了,老实报错,不装作成功
            // (放着不管的话,后面 rmdir 会因为非空而失败,那个错误用户读不懂)。
            if !e.name.is_operable() {
                return Err(SftpError::NonUtf8Name);
            }
            let child = dir.join(e.name.as_bytes());
            match e.kind {
                // **链接一律当叶子** —— 不 `list_dir` 它,不进去(D17)。
                EntryKind::Dir => pending.push(child),
                _ => sftp.remove_file(&child).await?,
            }
        }
    }
    // 从深到浅删空目录:`dirs_in_order` 是入栈顺序(浅→深),倒过来即可。
    for dir in dirs_in_order.iter().rev() {
        sftp.remove_dir(dir).await?;
    }
    Ok(())
}
