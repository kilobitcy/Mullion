//! F53/设计 D20:一段字节能不能拿来编辑,以及怎么在「字节」和「编辑器里的
//! 那个 `String`」之间来回。
//!
//! 纯函数、零 IO —— 这一片最不能出错的三件事(二进制别当文本打开、编码别猜、
//! 换行别静默改)全都在这里,而它们在真实 SSH 链路上极难复现。

/// 文件用的换行符。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eol {
    Lf,
    Crlf,
    /// 两种混着用。**不静默统一**(设计 D3-4):保存前要用户明说选哪种。
    Mixed,
    /// 压根没有换行(单行文件 / 空文件)。写回时按 LF 处理,反正没有换行可写。
    None,
}

/// 一段字节的体检结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// 含 NUL 字节 = 二进制。**唯一判据**(设计 D20)。
    pub binary: bool,
    /// 去掉 BOM 之后是不是合法 UTF-8。
    pub utf8: bool,
    /// 带 UTF-8 BOM(`EF BB BF`)。
    pub bom: bool,
    pub eol: Eol,
}

const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

impl Probe {
    /// 能不能编辑;不能就给一句**可读的原因**。
    ///
    /// 只回 `bool` 的话,界面只能说「打不开」,而用户下一步该干什么
    /// (换外部编辑器 / 先转编码 / 别动它)完全取决于是哪一类。
    pub fn read_only_reason(&self) -> Option<&'static str> {
        if self.binary {
            return Some("这是二进制文件(含 NUL 字节),编辑保存会毁掉它");
        }
        if !self.utf8 {
            return Some("内容不是 UTF-8,只读打开 —— 猜错编码保存回去会静默毁文件");
        }
        None
    }
}

/// 体检。**不看扩展名** —— `.txt` 里装着 gzip、`.bin` 里装着 JSON 都很常见,
/// 而扩展名猜错的代价是把一个二进制文件当文本存回去。
pub fn probe(bytes: &[u8]) -> Probe {
    let bom = bytes.starts_with(BOM);
    let body = if bom { &bytes[BOM.len()..] } else { bytes };
    let binary = body.contains(&0);
    let utf8 = std::str::from_utf8(body).is_ok();
    Probe {
        binary,
        utf8,
        bom,
        eol: eol_of(body),
    }
}

/// 换行符统计。`\r\n` 与**孤立的** `\n` 都出现过就是混用。
///
/// 老 Mac 的孤立 `\r` 不单独成一类:它在今天的服务器上几乎绝迹,而多一个
/// 分类就多一条没人验过的写回路径。它会被算成「没有换行」,于是原样写回。
fn eol_of(body: &[u8]) -> Eol {
    let mut crlf = false;
    let mut lf = false;
    let mut prev = 0u8;
    for b in body {
        if *b == b'\n' {
            if prev == b'\r' {
                crlf = true;
            } else {
                lf = true;
            }
        }
        prev = *b;
    }
    match (crlf, lf) {
        (true, true) => Eol::Mixed,
        (true, false) => Eol::Crlf,
        (false, true) => Eol::Lf,
        (false, false) => Eol::None,
    }
}

/// 字节 → 编辑器里的文本。**换行统一成 `\n`**(egui 的 `TextEdit` 只认它),
/// BOM 去掉(留着的话它会以一个看不见的字符的形式出现在第一行行首,
/// 用户按 Home 再按退格就把它删了,而屏幕上什么都没变)。
///
/// 非 UTF-8 的字节走 lossy —— 那种情况下 [`Probe::read_only_reason`] 已经
/// 把编辑器锁成只读了,这里只是让用户**看得见**里面是什么。
pub fn decode(bytes: &[u8], p: &Probe) -> String {
    let body = if p.bom { &bytes[BOM.len()..] } else { bytes };
    String::from_utf8_lossy(body).replace("\r\n", "\n")
}

/// 编辑器里的文本 → 字节。按 `eol` 还原换行、按 `bom` 还原 BOM。
///
/// `Eol::Mixed` 在这里**不可能出现** —— 界面要求用户在保存前明说选哪种
/// (设计 D3-4),给到这里的一定是 `Lf`/`Crlf`/`None`。真传进来就按 LF 处理,
/// 不 panic:一个保存动作因为断言崩掉整个程序,比统一成 LF 严重得多。
pub fn encode(text: &str, eol: Eol, bom: bool) -> Vec<u8> {
    let body = match eol {
        Eol::Crlf => text.replace('\n', "\r\n"),
        _ => text.to_string(),
    };
    let mut out = Vec::with_capacity(body.len() + 3);
    if bom {
        out.extend_from_slice(BOM);
    }
    out.extend_from_slice(body.as_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 二进制的唯一判据是 NUL。判错的代价是「编辑完把 .png 毁了」。
    #[test]
    fn a_nul_byte_means_binary_so_a_png_can_never_be_edited() {
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";
        let p = probe(png);
        assert!(p.binary);
        assert!(p.read_only_reason().is_some(), "二进制必须给出拒绝理由");
        // 反面:普通文本不许被误判成二进制,否则整个内置编辑器等于不存在。
        assert!(!probe(b"hello\nworld\n").binary);
    }

    /// 非 UTF-8 只读打开(D20)。猜编码写回去 = 静默毁文件,比不给编辑严重。
    #[test]
    fn invalid_utf8_opens_read_only_instead_of_guessing_an_encoding() {
        // GBK 的「中文」:两个双字节序列,在 UTF-8 下非法。
        let gbk = b"\xd6\xd0\xce\xc4\n";
        let p = probe(gbk);
        assert!(!p.utf8);
        assert!(!p.binary, "非 UTF-8 不等于二进制,这两条判据不能混为一谈");
        assert!(p.read_only_reason().unwrap().contains("UTF-8"));
        // 反面:合法的 UTF-8 中文必须可编辑。判据写宽了(比如「有非 ASCII 就
        // 只读」)会让绝大多数中文配置文件都改不了。
        let p2 = probe("中文\n".as_bytes());
        assert!(p2.utf8);
        assert!(p2.read_only_reason().is_none());
    }

    /// BOM 读到就保留。少了 BOM 的 `.bat`/`.csv` 在 Windows 侧行为会变,
    /// 而这个改动在编辑器里**完全看不见**。
    #[test]
    fn a_bom_is_kept_so_the_file_does_not_change_shape_on_save() {
        let src = b"\xEF\xBB\xBFhello\n";
        let p = probe(src);
        assert!(p.bom);
        let text = decode(src, &p);
        assert_eq!(text, "hello\n", "BOM 不该出现在编辑器的文本里");
        assert_eq!(encode(&text, p.eol, p.bom), src, "写回时 BOM 要带上");
    }

    /// 换行混用要报出来,让界面能逼用户明说(设计 D3-4)。
    #[test]
    fn mixed_line_endings_are_reported_so_the_user_can_pick_one() {
        assert_eq!(probe(b"a\nb\n").eol, Eol::Lf);
        assert_eq!(probe(b"a\r\nb\r\n").eol, Eol::Crlf);
        assert_eq!(probe(b"a\r\nb\nc\r\n").eol, Eol::Mixed);
        assert_eq!(probe(b"single line").eol, Eol::None);
    }

    /// 什么都没改时,`decode` → `encode` 必须**逐字节**回到原样。
    /// 回不去的话,用户只是打开看了一眼、按了保存,整个文件的换行就被换掉了。
    #[test]
    fn encoding_back_restores_the_original_bytes_when_nothing_was_edited() {
        for src in [
            &b"a\nb\n"[..],
            &b"a\r\nb\r\n"[..],
            &b"\xEF\xBB\xBFa\r\nb\r\n"[..],
            &b"no trailing newline"[..],
            "中文\n第二行\n".as_bytes(),
        ] {
            let p = probe(src);
            let text = decode(src, &p);
            assert_eq!(
                encode(&text, p.eol, p.bom),
                src,
                "往返没回到原样:{:?}",
                String::from_utf8_lossy(src)
            );
        }
    }

    /// CRLF 文件里加一行,新行也要是 CRLF —— 半个文件 LF 半个 CRLF 的
    /// 提交在 diff 里是整片飘红。
    #[test]
    fn a_new_line_added_to_a_crlf_file_is_written_back_as_crlf() {
        let out = encode("a\nb\nc\n", Eol::Crlf, false);
        assert_eq!(out, b"a\r\nb\r\nc\r\n");
    }
}
