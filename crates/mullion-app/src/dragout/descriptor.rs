//! `FILEGROUPDESCRIPTORW` 的**字节布局**(F59 / 设计 D11)。零平台代码。
//!
//! 拖出的虚拟文件靠这块内存告诉目标程序「这一拖里有几个文件、各叫什么、
//! 多大」。它是一个纯 C 结构体数组,偏移写错一个字节,资源管理器读到的
//! 就是垃圾名字/垃圾大小 —— 而错误发生在**别人的进程里**,只能看到
//! 「拖过去没反应」。所以这里不用 `#[repr(C)]` 结构体加 `transmute`,
//! 而是逐字段按偏移写进 `Vec<u8>`,让偏移本身能被断言。
//!
//! 布局(`shlobj_core.h`,全部小端、对齐 4):
//!
//! ```text
//! FILEGROUPDESCRIPTORW:
//!   0..4      UINT  cItems
//!   4..       FILEDESCRIPTORW[cItems]        每项 592 字节
//!
//! FILEDESCRIPTORW:
//!   0..4      DWORD    dwFlags
//!   4..20     CLSID    clsid
//!   20..28    SIZEL    sizel
//!   28..36    POINTL   pointl
//!   36..40    DWORD    dwFileAttributes
//!   40..48    FILETIME ftCreationTime
//!   48..56    FILETIME ftLastAccessTime
//!   56..64    FILETIME ftLastWriteTime
//!   64..68    DWORD    nFileSizeHigh
//!   68..72    DWORD    nFileSizeLow
//!   72..592   WCHAR    cFileName[MAX_PATH]   260 个 UTF-16 码元,含结尾 NUL
//! ```

/// 一项 `FILEDESCRIPTORW` 的字节数。
pub const FD_SIZE: usize = 592;
/// `cFileName` 在一项里的起始偏移。
pub const FD_NAME_OFF: usize = 72;
/// `cFileName` 能放几个 UTF-16 码元(`MAX_PATH`),**含**结尾的 NUL。
pub const FD_NAME_CAP: usize = 260;

/// `dwFlags`:`FD_ATTRIBUTES | FD_FILESIZE | FD_PROGRESSUI`。
///
/// `FD_PROGRESSUI` 是在告诉目标程序「这东西可能要读很久,请自己画进度」——
/// 拖一个 200MB 的远端文件到桌面,没有这一位的话资源管理器会白着脸卡住。
const FD_FLAGS: u32 = 0x0000_0004 | 0x0000_0040 | 0x0000_4000;
/// `FILE_ATTRIBUTE_NORMAL`。目录不拖(设计 N2),所以恒是这个。
const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;

/// 一项描述符要的全部信息。
pub struct Described<'a> {
    /// **已经净化并去重过**的落地名(见 `super::name`)。
    pub name: &'a str,
    pub size: u64,
}

/// 把一批文件拼成 `CFSTR_FILEDESCRIPTORW` 的内容。
pub fn file_group_descriptor(items: &[Described<'_>]) -> Vec<u8> {
    let mut buf = vec![0u8; 4 + FD_SIZE * items.len()];
    buf[0..4].copy_from_slice(&(items.len() as u32).to_le_bytes());
    for (i, it) in items.iter().enumerate() {
        let base = 4 + FD_SIZE * i;
        write_one(&mut buf[base..base + FD_SIZE], it);
    }
    buf
}

fn write_one(fd: &mut [u8], it: &Described<'_>) {
    fd[0..4].copy_from_slice(&FD_FLAGS.to_le_bytes());
    fd[36..40].copy_from_slice(&FILE_ATTRIBUTE_NORMAL.to_le_bytes());
    // 高位在前一个字段、低位在后 —— 这是结构体的顺序,不是数值的顺序。
    // 写反的话 4GB 以下的文件全变成 0 字节(高位恒 0 被当成低位),而
    // 小文件占绝大多数,「拖下来全是空文件」会被当成传输坏了。
    fd[64..68].copy_from_slice(&((it.size >> 32) as u32).to_le_bytes());
    fd[68..72].copy_from_slice(&((it.size & 0xFFFF_FFFF) as u32).to_le_bytes());
    let name = utf16_fixed(it.name);
    for (k, u) in name.iter().enumerate() {
        let off = FD_NAME_OFF + k * 2;
        fd[off..off + 2].copy_from_slice(&u.to_le_bytes());
    }
}

/// 名字编成定长的 UTF-16,**留一个 NUL 的位置**。
///
/// 超长时按码元截断,但**不能把代理对劈成两半** —— 劈了的话末尾是一个孤儿
/// 代理项,资源管理器那边要么显示成方块要么直接拒收。远端文件名里的 emoji
/// 和罕用汉字(扩展 B 区)都是代理对,不是理论边界。
fn utf16_fixed(name: &str) -> Vec<u16> {
    let mut v: Vec<u16> = name.encode_utf16().collect();
    if v.len() > FD_NAME_CAP - 1 {
        v.truncate(FD_NAME_CAP - 1);
        // 截完最后一个是高位代理(0xD800..=0xDBFF)= 它的低位被切走了。
        if matches!(v.last(), Some(u) if (0xD800..=0xDBFF).contains(u)) {
            v.pop();
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d<'a>(name: &'a str, size: u64) -> Described<'a> {
        Described { name, size }
    }

    fn u16_at(buf: &[u8], off: usize) -> u16 {
        u16::from_le_bytes([buf[off], buf[off + 1]])
    }

    fn u32_at(buf: &[u8], off: usize) -> u32 {
        u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
    }

    #[test]
    fn the_buffer_is_exactly_one_count_plus_n_fixed_size_descriptors() {
        // 长度算错 = 目标程序读到缓冲区外面去。
        assert_eq!(file_group_descriptor(&[]).len(), 4);
        assert_eq!(file_group_descriptor(&[d("a", 1)]).len(), 4 + 592);
        assert_eq!(
            file_group_descriptor(&[d("a", 1), d("b", 2)]).len(),
            4 + 1184
        );
    }

    #[test]
    fn the_item_count_is_the_first_four_bytes() {
        let buf = file_group_descriptor(&[d("a", 1), d("b", 2), d("c", 3)]);
        assert_eq!(u32_at(&buf, 0), 3);
    }

    #[test]
    fn the_file_name_starts_at_offset_72_of_each_descriptor() {
        // 偏移写错一个字节,资源管理器读到的就是垃圾名字,而错误发生在
        // 别人的进程里,这边只看得到「拖过去没反应」。
        //
        // **这里写死 72,不用 `FD_NAME_OFF`**:拿常量去断言常量自己算出来的
        // 布局是一句重言式 —— 把常量改成 68,这条照样绿(变异验收当场发现)。
        // 布局的真源是 `shlobj_core.h`,不是这个文件里的常量。
        let buf = file_group_descriptor(&[d("ab", 0)]);
        assert_eq!(u16_at(&buf, 4 + 72), b'a' as u16);
        assert_eq!(u16_at(&buf, 4 + 72 + 2), b'b' as u16);
        assert_eq!(u16_at(&buf, 4 + 72 + 4), 0, "必须以 NUL 收尾");
    }

    #[test]
    fn the_second_descriptor_starts_592_bytes_after_the_first() {
        // 同上,写死 592 与 72。
        let buf = file_group_descriptor(&[d("a", 0), d("b", 0)]);
        assert_eq!(u16_at(&buf, 4 + 592 + 72), b'b' as u16);
    }

    #[test]
    fn the_size_is_split_high_word_first_then_low_word() {
        // 高位字段在**前**、低位在后 —— 这是结构体的顺序,不是数值的顺序。
        // 写反的话 4GB 以下的文件(绝大多数)全变成 0 字节。
        let buf = file_group_descriptor(&[d("a", 0x1_2345_6789)]);
        assert_eq!(u32_at(&buf, 4 + 64), 0x1, "nFileSizeHigh");
        assert_eq!(u32_at(&buf, 4 + 68), 0x2345_6789, "nFileSizeLow");
    }

    #[test]
    fn a_small_file_reports_its_real_size_not_zero() {
        let buf = file_group_descriptor(&[d("a", 1234)]);
        assert_eq!(u32_at(&buf, 4 + 64), 0);
        assert_eq!(u32_at(&buf, 4 + 68), 1234);
    }

    #[test]
    fn the_flags_ask_the_target_to_draw_its_own_progress_ui() {
        // 没有 FD_PROGRESSUI,拖一个 200MB 的远端文件到桌面时资源管理器
        // 会白着脸卡住(它以为这是个本地文件,瞬间就该读完)。
        let buf = file_group_descriptor(&[d("a", 0)]);
        assert_eq!(u32_at(&buf, 4) & 0x4000, 0x4000, "FD_PROGRESSUI");
        assert_eq!(u32_at(&buf, 4) & 0x40, 0x40, "FD_FILESIZE");
    }

    #[test]
    fn a_name_longer_than_max_path_is_truncated_and_still_null_terminated() {
        let long = "あ".repeat(400);
        let buf = file_group_descriptor(&[d(&long, 0)]);
        // 最后一个码元位置必须是 NUL,否则目标程序会一直读到下一项里去。
        let last = 4 + 72 + (260 - 1) * 2;
        assert_eq!(u16_at(&buf, last), 0);
        assert_eq!(buf.len(), 4 + 592, "截断不该把描述符撑大");
    }

    #[test]
    fn truncation_never_leaves_half_a_surrogate_pair_at_the_end() {
        // emoji 和扩展 B 区汉字都是代理对。劈一半的话末尾是孤儿代理项,
        // 资源管理器要么显示成方块要么直接拒收整批。
        // 258 个 BMP 字符 + emoji:截断点正好落在代理对中间。
        let name = format!("{}{}", "a".repeat(258), "🚀");
        let buf = file_group_descriptor(&[d(&name, 0)]);
        let hi = u16_at(&buf, 4 + 72 + 258 * 2);
        assert!(
            !(0xD800..=0xDBFF).contains(&hi),
            "末尾留了一个孤儿高位代理项:{hi:#x}"
        );
    }
}
