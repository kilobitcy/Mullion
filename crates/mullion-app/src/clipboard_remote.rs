//! F230:跨进程远端剪贴板的**载荷格式与路由判据**。零平台代码、零 IO。
//!
//! Windows 的 OLE 部分在 `dragout::win`。放在这里的是两件在无头环境验得了、
//! 而且错了最致命的事:
//!
//! 1. **载荷的字节编码** —— 远端文件名不保证是 UTF-8(`RemotePath` 整个
//!    类型就是为它存在的),走一趟 `String` 会把名字换成替换字符,粘贴时
//!    打不中那个文件。
//! 2. **快路径的准入判据** —— 指纹对不上却走了快路径,`copy_tree` 会在
//!    **本机**上找同名路径(`/srv/app` 这种路径两台机器上都有),静默复制
//!    错文件。这是这一片最危险的失误,所以判据必须是值级可测的纯函数。

use crate::files::clip::{ClipMode, RemoteClip};
use mullion_ssh::sftp::RemotePath;

/// 私有剪贴板格式的名字。用 `RegisterClipboardFormatW` 登记 —— 名字相同的
/// 两个进程拿到同一个 id,这正是「两个 Mullion 互通」要的。
pub const FORMAT_NAME: &str = "MullionRemoteFiles";

/// 载荷魔数 + 版本。换格式时改这里,旧载荷自然解不开(`decode` 返回 `None`,
/// 调用方退回慢路径)—— 比「尽力解析旧版」安全:半份垃圾会变成一批发给远端
/// 的乱码路径。
const MAGIC: &[u8; 8] = b"MULCLIP1";

/// 解出来的一份跨进程剪贴板。
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteClipboard {
    /// 源端那台机器的主机密钥指纹(`SHA256:<base64>`,与 `known_hosts` 同格式)。
    pub fingerprint: String,
    pub clip: RemoteClip,
}

/// 这次粘贴走哪条路。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// 同一台机器 —— 远端直接 `copy_tree`/`rename`,零传输。
    Fast,
    /// 不同机器(或本机指纹取不到)—— 先下载再上传。
    Slow,
}

/// 编成字节。**长度前缀的二进制**,不是文本:路径是裸字节,任何以行分隔的
/// 文本格式都会被名字里的 `\n` 撑破(远端文件名里放得下换行)。
///
/// 布局:`MAGIC(8) | mode(1) | fp_len(u16le) | fp | count(u32le) |`
/// 每项 `is_dir(1) | path_len(u32le) | path`。
pub fn encode(fingerprint: &str, clip: &RemoteClip) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.push(match clip.mode {
        ClipMode::Copy => 0,
        ClipMode::Cut => 1,
    });
    let fp = fingerprint.as_bytes();
    out.extend_from_slice(&(fp.len() as u16).to_le_bytes());
    out.extend_from_slice(fp);
    out.extend_from_slice(&(clip.items.len() as u32).to_le_bytes());
    for (p, is_dir) in &clip.items {
        out.push(u8::from(*is_dir));
        let b = p.as_bytes();
        out.extend_from_slice(&(b.len() as u32).to_le_bytes());
        out.extend_from_slice(b);
    }
    out
}

/// 解回来。**任何一处对不上就整份作废**(返回 `None`),不做尽力解析 ——
/// 半份垃圾会变成一批发给远端的乱码路径。
pub fn decode(bytes: &[u8]) -> Option<RemoteClipboard> {
    let mut at = 0usize;
    let take = |at: &mut usize, n: usize| -> Option<&[u8]> {
        let end = at.checked_add(n)?;
        let s = bytes.get(*at..end)?;
        *at = end;
        Some(s)
    };
    if take(&mut at, 8)? != MAGIC {
        return None;
    }
    let mode = match take(&mut at, 1)?[0] {
        0 => ClipMode::Copy,
        1 => ClipMode::Cut,
        _ => return None,
    };
    let fp_len = u16::from_le_bytes(take(&mut at, 2)?.try_into().ok()?) as usize;
    let fingerprint = std::str::from_utf8(take(&mut at, fp_len)?).ok()?.to_owned();
    let count = u32::from_le_bytes(take(&mut at, 4)?.try_into().ok()?) as usize;
    // `with_capacity` 夹一道上限:`count` 来自**别的进程写进剪贴板的字节**,
    // 直接拿它去分配等于把一个 4 GiB 的申请交给一份 20 字节的载荷。
    let mut items = Vec::with_capacity(count.min(4096));
    for _ in 0..count {
        let is_dir = match take(&mut at, 1)?[0] {
            0 => false,
            1 => true,
            _ => return None,
        };
        let len = u32::from_le_bytes(take(&mut at, 4)?.try_into().ok()?) as usize;
        items.push((RemotePath::from_bytes(take(&mut at, len)?.to_vec()), is_dir));
    }
    // 末尾多出来的字节同样作废:那说明格式对不上,不是「多带了点东西」。
    if at != bytes.len() {
        return None;
    }
    Some(RemoteClipboard {
        fingerprint,
        clip: RemoteClip { mode, items },
    })
}

/// 这次粘贴该走哪条路。`here` = 本标签当前连接那台机器在 `known_hosts` 里的
/// 指纹;`None` = 取不到(TOFU 时用户选了「只信任这一次」,没写盘)。
///
/// **取不到就走慢路径,不许猜**:猜错的后果是在本机上按绝对路径复制 ——
/// `/srv/app` 这种路径两台机器上都存在,复制的是**另一台机器的同名文件**,
/// 而且完全静默。
///
/// 空指纹同样判不匹配:写载荷那一侧取不到指纹时给的就是空串。
pub fn route_for(payload: &str, here: Option<&str>) -> Route {
    match here {
        Some(h) if !h.is_empty() && h == payload => Route::Fast,
        _ => Route::Slow,
    }
}

/// 慢路径上「剪切」降级成「复制」。
///
/// 跨机器的移动要在**源端**删,而删除不可逆,且源端那条连接此刻可能已经断了
/// (用户关掉了那个标签)。删不掉又已经报了「已移动」,用户会以为源没了。
/// 调用方必须把降级**说出来**(toast),不能静默 —— 用户按的是剪切。
pub fn effective_mode(route: Route, mode: ClipMode) -> ClipMode {
    match route {
        Route::Fast => mode,
        Route::Slow => ClipMode::Copy,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::clip::{ClipMode, RemoteClip};
    use mullion_ssh::sftp::RemotePath;

    fn clip(mode: ClipMode) -> RemoteClip {
        RemoteClip {
            mode,
            items: vec![
                (RemotePath::from_bytes(b"/srv/app/a.txt".to_vec()), false),
                (RemotePath::from_bytes(b"/srv/app/logs".to_vec()), true),
            ],
        }
    }

    /// F230:编码 → 解码必须原样回来,**包括路径的原始字节**。远端文件名不保证
    /// 是 UTF-8(这条在本项目里是硬事实,`RemotePath` 整个类型就是为它存在的),
    /// 走一趟 `String` 会把非 UTF-8 的名字换成替换字符 —— 粘贴时打不中那个文件,
    /// 报一句 `NoSuchFile`,而用户在列表里明明看得见它。
    ///
    /// 自证会变红:把 `encode` 里写路径那段换成 `p.display().as_bytes()`。
    #[test]
    fn a_payload_round_trips_including_non_utf8_names() {
        let mut c = clip(ClipMode::Cut);
        c.items
            .push((RemotePath::from_bytes(vec![b'/', 0xFF, 0xFE]), false));
        let bytes = encode("SHA256:abc", &c);
        let got = decode(&bytes).expect("刚编出来的必须解得开");
        assert_eq!(got.fingerprint, "SHA256:abc");
        assert_eq!(got.clip, c);
    }

    /// F230:别人的剪贴板内容(或者格式换代之后的旧数据)必须**解不开**,
    /// 而不是解出半份垃圾。解出半份的后果是拿一批乱码路径去远端发请求。
    #[test]
    fn foreign_or_truncated_payloads_are_rejected() {
        assert!(decode(b"").is_none());
        assert!(decode(b"not a mullion payload").is_none());
        let good = encode("SHA256:abc", &clip(ClipMode::Copy));
        assert!(
            decode(&good[..good.len() - 1]).is_none(),
            "截断的载荷不许解开"
        );
        let mut extra = good.clone();
        extra.push(0);
        assert!(decode(&extra).is_none(), "末尾多出字节的载荷不许解开");
    }

    /// F230 的核心判据:指纹相同才走快路径(远端直接 copy/rename,零传输)。
    ///
    /// 指纹取自 `known_hosts` 里这台机器那条记录 —— `check_server_key` 过了
    /// 就意味着「实测指纹 == 记录指纹」,两者等价。指纹对不上 = 不是同一台
    /// 机器,远端根本看不到那些路径,直接 copy 会打到**本机上同名的另一个
    /// 文件**(路径是绝对的,而 `/srv/app` 这种路径在两台机器上都存在) ——
    /// 静默复制错文件,这是这一片最危险的失误。
    ///
    /// 自证会变红:把 `route_for` 的条件反过来。
    #[test]
    fn only_a_matching_fingerprint_takes_the_fast_path() {
        assert_eq!(route_for("SHA256:abc", Some("SHA256:abc")), Route::Fast);
        assert_eq!(route_for("SHA256:abc", Some("SHA256:zzz")), Route::Slow);
        // 本机这条连接的指纹取不到(TOFU 时选了「只信任这一次」,没写
        // known_hosts)—— 宁可慢,不许猜。
        assert_eq!(route_for("SHA256:abc", None), Route::Slow);
        // 两边都取不到指纹时,写载荷那侧给的是空串 —— 空 == 空**不算**同一
        // 台机器,否则任意两台「都没记进 known_hosts」的机器之间会互相走
        // 快路径,静默复制错文件。
        assert_eq!(route_for("", Some("")), Route::Slow);
    }

    /// F230:剪切在慢路径上**降级为复制**。跨机器的「移动」要在源端删,而
    /// 删除不可逆,且源端那条连接此刻可能已经断了(用户关掉了那个标签)——
    /// 删不掉又已经报了「已移动」,用户会以为源没了。
    ///
    /// 自证会变红:把 `effective_mode` 里 Slow 那条分支去掉。
    #[test]
    fn cut_degrades_to_copy_off_the_fast_path() {
        assert_eq!(effective_mode(Route::Fast, ClipMode::Cut), ClipMode::Cut);
        assert_eq!(effective_mode(Route::Slow, ClipMode::Cut), ClipMode::Copy);
        assert_eq!(effective_mode(Route::Slow, ClipMode::Copy), ClipMode::Copy);
    }
}
