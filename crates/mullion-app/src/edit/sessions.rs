//! F53/设计 D13:「正在编辑」的那几条,以及「什么时候该回传」「能不能回传」
//! 两条判定。
//!
//! 纯状态机,零 IO —— 「远端在我编辑期间被别人动过」这种情形在真实链路上
//! 极难复现,而它恰恰是这一片唯一会**丢别人数据**的分支。

use std::collections::BTreeMap;
use std::path::PathBuf;

use mullion_ssh::sftp::RemotePath;

pub type EditKey = u64;

/// 这一条是怎么开的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    /// 下到临时目录、交给系统默认程序,靠 mtime 轮询发现改动。
    External,
    /// 在我们自己的窗口里改,点「保存」才回传。
    Inline,
}

/// 远端那一份的身份。**mtime 与 size 一起看** —— 只看 mtime 的话,
/// 同一秒内的两次写(脚本 `sed -i` 很常见)看起来毫无变化;只看 size 的话,
/// 改一个字符的编辑同样看不出来。SFTP v3 的 mtime 只有秒级精度,
/// 两条判据都不牢靠,合起来才勉强够用。
pub type RemoteStamp = (u32, u64);

/// 本地那一份的身份。同上,mtime(秒)+ 长度。
pub type LocalStamp = (u64, u64);

#[derive(Debug, Clone, PartialEq)]
pub enum EditState {
    /// 盯着本地文件,等它变。
    Watching,
    /// 正在回传。
    Uploading,
    /// 远端在我们编辑期间变过 —— **不覆盖**,等用户处置(D13)。
    Conflict,
    /// 上一次回传失败,字符串是可读原因。
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct EditEntry {
    pub key: EditKey,
    /// 属主标签的世代号(S1 路由键)。关标签要连它一起收掉。
    pub generation: u64,
    pub kind: EditKind,
    pub remote: RemotePath,
    pub local: PathBuf,
    /// 给用户看的名字(最后一段)。
    pub label: String,
    /// 打开(或上一次成功回传)那一刻**远端**的样子。回传前拿它跟远端比。
    pub snapshot: RemoteStamp,
    /// 上一次看到的**本地**文件的样子。变了才发起回传。
    /// `None` = 还没看过(刚下下来那一瞬间)。
    pub seen: Option<LocalStamp>,
    pub state: EditState,
    /// 已经回传成功过至少一次。界面据此把「未回传」的警告收掉。
    pub saved_once: bool,
}

impl EditEntry {
    /// 有没有「已经改了但还没传上去」的内容。退出拦截(D3-12)与
    /// 「结束编辑」的二次确认都问这一条。
    ///
    /// `Uploading` 也算:进程在覆盖写到一半时被杀掉,远端留下的是一个**被
    /// 截断的**文件(`write_all_truncate` 不走临时文件 + rename,见 D19)。
    /// 那是这一片能造成的最坏结果,拦一下值。
    pub fn has_unsent_changes(&self) -> bool {
        matches!(
            self.state,
            EditState::Uploading | EditState::Conflict | EditState::Failed(_)
        )
    }
}

/// 编辑中的全部条目。挂在 `App` 上 —— **全局一份**,与传输队列同一条理由:
/// 切标签不该看见另一份编辑列表,但关标签要作废属于它的那些。
#[derive(Default)]
pub struct EditSessions {
    items: BTreeMap<EditKey, EditEntry>,
    next: EditKey,
}

impl EditSessions {
    pub fn new() -> Self {
        Self {
            items: BTreeMap::new(),
            next: 1,
        }
    }

    /// 登记一条。**同一个远端路径已经在编辑中就返回原来那条**(不新开一条)——
    /// 开两条的话两个编辑器改同一个临时文件,回传互相覆盖,而列表里看着像
    /// 两件不相干的事。
    pub fn add(
        &mut self,
        generation: u64,
        kind: EditKind,
        remote: RemotePath,
        local: PathBuf,
        snapshot: RemoteStamp,
    ) -> EditKey {
        if let Some(existing) = self.items.values().find(|e| e.remote == remote) {
            return existing.key;
        }
        let key = self.next;
        self.next += 1;
        let label = last_segment(&remote);
        self.items.insert(
            key,
            EditEntry {
                key,
                generation,
                kind,
                remote,
                local,
                label,
                snapshot,
                seen: None,
                state: EditState::Watching,
                saved_once: false,
            },
        );
        key
    }

    pub fn get(&self, key: EditKey) -> Option<&EditEntry> {
        self.items.get(&key)
    }

    pub fn get_mut(&mut self, key: EditKey) -> Option<&mut EditEntry> {
        self.items.get_mut(&key)
    }

    pub fn remove(&mut self, key: EditKey) -> Option<EditEntry> {
        self.items.remove(&key)
    }

    pub fn iter(&self) -> impl Iterator<Item = &EditEntry> {
        self.items.values()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// 关标签:作废属于它的条目,返回被摘掉的那些(调用方要删临时文件)。
    pub fn drain_generation(&mut self, generation: u64) -> Vec<EditEntry> {
        let keys: Vec<EditKey> = self
            .items
            .values()
            .filter(|e| e.generation == generation)
            .map(|e| e.key)
            .collect();
        keys.into_iter()
            .filter_map(|k| self.items.remove(&k))
            .collect()
    }

    /// 退出拦截(D3-12):有没有哪一条「改了但没传上去」。
    pub fn blocks_exit(&self) -> bool {
        self.items.values().any(|e| e.has_unsent_changes())
    }

    /// 轮询:本地文件变了、且这一条此刻**空闲**的,返回它们的 key。
    ///
    /// `stamps` 是调用方刚 stat 出来的本地时间戳(`None` = 文件没了)。
    /// 把 IO 留在外面,这条判定才测得动。
    ///
    /// **只挑 `Watching`**:正在回传的那条如果又被算成"变了",会同时起两条
    /// 写回任务打同一个远端文件,后完成的那条内容不确定。
    pub fn changed_locally(&mut self, stamps: &[(EditKey, Option<LocalStamp>)]) -> Vec<EditKey> {
        let mut out = Vec::new();
        for (key, stamp) in stamps {
            let Some(stamp) = stamp else { continue };
            let Some(e) = self.items.get_mut(key) else {
                continue;
            };
            if e.state != EditState::Watching {
                continue;
            }
            // 第一次看到:只记下来,**不算改动**。刚下下来的文件的 mtime
            // 当然跟"上次看到的"不同(压根没有上次),据此触发回传等于
            // 每打开一个文件就立刻原样传回去一遍。
            let changed = e.seen.is_some_and(|prev| prev != *stamp);
            e.seen = Some(*stamp);
            if changed {
                out.push(*key);
            }
        }
        out
    }

    /// 回传前的闸门:远端此刻的样子与打开时的快照对不上就**不许写**(D13)。
    pub fn may_write_back(&self, key: EditKey, remote_now: RemoteStamp) -> bool {
        self.items
            .get(&key)
            .is_some_and(|e| e.snapshot == remote_now)
    }

    /// 回传成功:快照跟到新的远端状态上,回到监视态。
    ///
    /// **必须更新快照** —— 不更新的话,我们自己刚写下去的那一次改动会在下一轮
    /// 被当成"别人改的",于是每保存第二次都弹一个冲突框。
    /// `local` 为 `None` = 这条在磁盘上没有文件(内置编辑器那种),
    /// 此时**不动 `seen`** —— 写一个假基线进去会让下一次轮询把它当成
    /// 「本地变了」再传一遍。
    pub fn accept_write_back(
        &mut self,
        key: EditKey,
        remote_now: RemoteStamp,
        local: Option<LocalStamp>,
    ) {
        if let Some(e) = self.items.get_mut(&key) {
            e.snapshot = remote_now;
            if local.is_some() {
                e.seen = local;
            }
            e.state = EditState::Watching;
            e.saved_once = true;
        }
    }

    /// 用户选了「保留远端」(D3-9):这一次不传,但**把快照更新成远端当前值**。
    /// 不更新的话同一个框会在下一轮轮询里再弹一遍,永远关不掉。
    pub fn keep_remote(&mut self, key: EditKey, remote_now: RemoteStamp) {
        if let Some(e) = self.items.get_mut(&key) {
            e.snapshot = remote_now;
            e.state = EditState::Watching;
        }
    }
}

fn last_segment(p: &RemotePath) -> String {
    let b = p.as_bytes();
    let last = match b.iter().rposition(|x| *x == b'/') {
        Some(ix) if ix + 1 < b.len() => &b[ix + 1..],
        _ => b,
    };
    String::from_utf8_lossy(last).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rp(s: &str) -> RemotePath {
        RemotePath::from_bytes(s.as_bytes().to_vec())
    }

    fn one() -> (EditSessions, EditKey) {
        let mut s = EditSessions::new();
        let k = s.add(
            7,
            EditKind::External,
            rp("/etc/app.conf"),
            PathBuf::from("/tmp/app.conf"),
            (100, 10),
        );
        (s, k)
    }

    /// 改没改一律看本地 mtime + 长度,**不猜编辑器进程退没退**(D13:
    /// VS Code 一个进程开一堆窗口,进程信号毫无意义)。
    #[test]
    fn a_local_change_is_detected_by_mtime_and_size_not_by_a_process_exit() {
        let (mut s, k) = one();
        // 第一次看到只是登记基线,不该触发回传 —— 否则每打开一个文件都会
        // 立刻原样传回去一遍。
        assert!(s.changed_locally(&[(k, Some((1000, 50)))]).is_empty());
        // 没变:不动。
        assert!(s.changed_locally(&[(k, Some((1000, 50)))]).is_empty());
        // 只有 mtime 变(同长度的原地改写)也要认出来。
        assert_eq!(s.changed_locally(&[(k, Some((1001, 50)))]), vec![k]);
        // 只有长度变(同一秒内追加)同样要认出来 —— SFTP 的 mtime 是秒级。
        assert_eq!(s.changed_locally(&[(k, Some((1001, 60)))]), vec![k]);
    }

    /// 正在回传的那条不能被再触发一次:两条写回任务打同一个远端文件,
    /// 谁后完成、内容是什么全凭运气。
    #[test]
    fn a_job_already_uploading_is_not_triggered_again() {
        let (mut s, k) = one();
        s.changed_locally(&[(k, Some((1000, 50)))]);
        s.get_mut(k).unwrap().state = EditState::Uploading;
        assert!(
            s.changed_locally(&[(k, Some((2000, 90)))]).is_empty(),
            "回传进行中不该再起一条"
        );
    }

    /// 远端在编辑期间被动过 → **不许写**(D13)。这是这一片唯一会丢别人
    /// 数据的分支。
    #[test]
    fn a_remote_change_since_open_blocks_the_write_back() {
        let (s, k) = one();
        assert!(s.may_write_back(k, (100, 10)), "没变就该放行");
        assert!(!s.may_write_back(k, (101, 10)), "mtime 变了必须挡住");
        assert!(!s.may_write_back(k, (100, 11)), "size 变了必须挡住");
    }

    /// 回传成功后快照要跟上去。不跟的话,我们自己写下去的那一次会在下一轮
    /// 被当成"别人改的",于是从第二次保存起每次都弹冲突框。
    #[test]
    fn a_successful_write_back_moves_the_snapshot_forward() {
        let (mut s, k) = one();
        s.accept_write_back(k, (200, 33), Some((5000, 33)));
        assert!(s.may_write_back(k, (200, 33)), "快照该跟到新的远端状态上");
        assert_eq!(s.get(k).unwrap().state, EditState::Watching);
        assert!(s.get(k).unwrap().saved_once);
    }

    /// D3-9:「保留远端」也要刷新快照,否则同一个冲突框会在下一轮再弹一遍,
    /// 用户永远关不掉它。
    #[test]
    fn keeping_the_remote_version_refreshes_the_snapshot_so_the_dialog_does_not_reopen() {
        let (mut s, k) = one();
        s.get_mut(k).unwrap().state = EditState::Conflict;
        s.keep_remote(k, (300, 44));
        assert!(s.may_write_back(k, (300, 44)), "快照没跟上,下一轮还会弹");
        assert_eq!(s.get(k).unwrap().state, EditState::Watching);
    }

    /// 同一个远端路径不许开出两条:两个编辑器改同一个临时文件,
    /// 回传互相覆盖,而列表里看着像两件不相干的事。
    #[test]
    fn opening_the_same_remote_file_twice_reuses_the_existing_entry() {
        let (mut s, k) = one();
        let again = s.add(
            9,
            EditKind::Inline,
            rp("/etc/app.conf"),
            PathBuf::from("/tmp/elsewhere/app.conf"),
            (100, 10),
        );
        assert_eq!(again, k);
        assert_eq!(s.iter().count(), 1);
    }

    /// 关标签要把属于它的条目一起摘掉,并交回给调用方去删临时文件。
    #[test]
    fn closing_a_tab_drains_only_its_own_edits() {
        let (mut s, mine) = one();
        let other = s.add(
            8,
            EditKind::Inline,
            rp("/etc/other.conf"),
            PathBuf::from("/tmp/other.conf"),
            (1, 1),
        );
        let drained = s.drain_generation(7);
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].key, mine);
        assert!(s.get(other).is_some(), "别的标签的编辑不该被牵连");
    }

    /// 退出拦截只认「改了没传上去」的那些(冲突 / 失败)。
    /// 认得太宽的话,随便打开看一眼就再也退不出去了。
    #[test]
    fn only_unsent_changes_block_the_exit() {
        let (mut s, k) = one();
        assert!(!s.blocks_exit(), "只是打开看着不该拦退出");
        s.get_mut(k).unwrap().state = EditState::Conflict;
        assert!(s.blocks_exit());
        s.get_mut(k).unwrap().state = EditState::Failed("没权限".into());
        assert!(s.blocks_exit());
        // 回传进行中被杀掉,远端留下的是一个截断的文件(D19 不走 rename)。
        s.get_mut(k).unwrap().state = EditState::Uploading;
        assert!(s.blocks_exit(), "写到一半就退出会在远端留下半个文件");
        s.get_mut(k).unwrap().state = EditState::Watching;
        assert!(!s.blocks_exit());
    }

    #[test]
    fn the_label_is_the_last_path_segment_so_the_list_is_readable() {
        let (s, k) = one();
        assert_eq!(s.get(k).unwrap().label, "app.conf");
    }
}
