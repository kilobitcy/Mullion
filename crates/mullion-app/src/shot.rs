//! F209:剪贴板里的截图 → PNG → 直传远端。
//!
//! 用户的真实诉求不是「看图/编辑图」,是**在 SSH 上跟 Claude Code 说话时能方便
//! 地引用一张截屏**:Win+Shift+S 截完,在终端里 `Ctrl+V`,图落到远端某个目录,
//! 绝对路径直接打进当前输入行。
//!
//! # 为什么自己解 DIB,而不是开 `arboard/image-data`
//!
//! `arboard` 的 `image-data` 特性在 Windows 上会拉进 `image` 0.25(png + bmp)。
//! 而本项目在 `Cargo.toml` 里两处白纸黑字写过「刻意不用 `image` crate —— N6 的
//! exe 体积已超标」(会话图标 F61 那次也是为此手写的 `ico`)。剪贴板位图这件事
//! 在 Windows 上就是一段 `CF_DIB`:头 40 字节 + 像素,自己解不到两百行,而
//! 编码用的 `png` 已经因为 `ico` 躺在依赖树里了。
//!
//! # 分层
//!
//! `dib_to_bitmap` / `encode_png` / `file_name` 是**纯函数**,在 Linux 上照样
//! 单测;只有「从剪贴板拿到那段 DIB」是 Windows 专属(`win` 子模块)。这一刀
//! 跟 F59 拖出(`dragout/win.rs`)切在同一个位置。

/// 编码出来的 PNG 超过这个大小就不传。
///
/// 20 MiB —— 4K 全屏截图编码后通常 2~6 MiB,留了三倍余量。设上限是因为这条
/// 路径**没有进度条**(它不是文件面板的传输队列,是终端里一次按键的副作用):
/// 传一个几百 MB 的东西,用户只会看到程序「卡住了」。
pub const MAX_PNG_BYTES: usize = 20 * 1024 * 1024;

/// 解出来的位图。一律 RGB8,**丢掉 alpha**(见 `dib_to_bitmap` 的说明)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitmap {
    pub w: u32,
    pub h: u32,
    /// 行优先、自上而下、每像素 3 字节。
    pub rgb: Vec<u8>,
}

fn u16_at(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(off..off + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(off..off + 4)?.try_into().ok()?))
}

fn i32_at(b: &[u8], off: usize) -> Option<i32> {
    Some(i32::from_le_bytes(b.get(off..off + 4)?.try_into().ok()?))
}

/// 从一个通道掩码算出「右移几位、再乘多少能铺满 0..=255」。
///
/// 掩码不是标准的 8 位对齐时(16 位色的 5-6-5)也算得对:先移到最低位,再按
/// 实际位宽线性拉伸。返回 `None` 表示掩码是 0(该通道不存在)。
fn mask_shift_scale(mask: u32) -> Option<(u32, u32)> {
    if mask == 0 {
        return None;
    }
    let shift = mask.trailing_zeros();
    let width = 32 - mask.leading_zeros() - shift;
    Some((shift, width))
}

fn take_channel(px: u32, mask: u32, shift_width: Option<(u32, u32)>) -> u8 {
    let Some((shift, width)) = shift_width else {
        return 0;
    };
    let v = (px & mask) >> shift;
    let max = (1u32 << width) - 1;
    if max == 0 {
        return 0;
    }
    // 四舍五入的线性拉伸:5 位的 31 要变成 255,不是 248。
    ((v * 255 + max / 2) / max) as u8
}

/// 解 `CF_DIB`(BITMAPINFOHEADER 及其 V4/V5 扩展)。
///
/// **只认 24 位和 32 位**。Windows 的截图工具(Win+Shift+S、PrintScreen)、
/// 浏览器「复制图片」给的都是这两种;调色板位图(≤8 位)在今天的桌面上基本
/// 绝迹,为它写一套调色板解码是给一条走不到的分支写代码。认不出来时报出实际
/// 位深 —— 用户至少知道该换个工具截图,而不是面对一句「粘贴失败」。
///
/// **alpha 一律丢掉,输出 RGB。** 截图工具塞进剪贴板的 32 位 DIB 里,alpha
/// 通道**经常整片是 0**(那个字节历史上是 padding,没人负责填)。照单全收的
/// 结果是一张「编码成功、传输成功、打开全透明」的 PNG —— 全程没有任何报错,
/// 是这条路上最容易踩且最难归因的坑。
pub fn dib_to_bitmap(dib: &[u8]) -> Result<Bitmap, String> {
    let header_size = u32_at(dib, 0).ok_or("剪贴板里的位图数据不完整(连头都没有)")? as usize;
    if header_size < 40 {
        return Err(format!("不认识的位图头(大小 {header_size} 字节)"));
    }
    let width = i32_at(dib, 4).ok_or("位图头不完整")?;
    let height = i32_at(dib, 8).ok_or("位图头不完整")?;
    let bit_count = u16_at(dib, 14).ok_or("位图头不完整")?;
    let compression = u32_at(dib, 16).ok_or("位图头不完整")?;

    if !(1..=65535).contains(&width) || height == 0 || !(-65535..=65535).contains(&height) {
        return Err(format!("位图尺寸不合理({width}×{height})"));
    }
    // 负高度 = 自上而下存放(top-down)。正数才是 BMP 传统的自下而上。
    let top_down = height < 0;
    let (w, h) = (width as u32, height.unsigned_abs());

    // BI_RGB=0 / BI_BITFIELDS=3。压缩过的(RLE / JPEG / PNG 塞在 DIB 里)不接。
    if compression != 0 && compression != 3 {
        return Err(format!("位图用了不支持的压缩方式({compression})"));
    }
    if bit_count != 24 && bit_count != 32 {
        return Err(format!(
            "只支持 24/32 位色的截图,剪贴板里这张是 {bit_count} 位"
        ));
    }

    // 掩码住在哪儿:BITMAPINFOHEADER(40)+BI_BITFIELDS 时紧跟在头后面三个
    // DWORD;V4/V5 头(108/124)自带,在偏移 40 起。两种摆法的**偏移恰好
    // 相同**(40),差别只在「有没有」。
    let has_masks = compression == 3;
    let (mr, mg, mb) = if has_masks {
        (
            u32_at(dib, 40).ok_or("位图声明了通道掩码却没给")?,
            u32_at(dib, 44).ok_or("位图声明了通道掩码却没给")?,
            u32_at(dib, 48).ok_or("位图声明了通道掩码却没给")?,
        )
    } else if bit_count == 32 {
        // BI_RGB 的 32 位:字节序是 B,G,R,X。
        (0x00ff_0000, 0x0000_ff00, 0x0000_00ff)
    } else {
        (0, 0, 0) // 24 位不走掩码分支
    };

    // 像素数据的起点。V4/V5 头里掩码算在头内,BITMAPINFOHEADER + 掩码则要
    // 额外跳 12 字节。24/32 位没有调色板(`biClrUsed` 对它们只是「优化用的
    // 建议色表」,截图工具不会填,填了也不影响像素解释)。
    let mut offset = header_size;
    if has_masks && header_size == 40 {
        offset += 12;
    }

    let bytes_per_px = (bit_count / 8) as usize;
    // 每行按 4 字节对齐 —— 24 位图最常见的解错点,漏了它整张图会斜。
    let stride = (w as usize * bytes_per_px).div_ceil(4) * 4;
    let need = stride.checked_mul(h as usize).ok_or("位图尺寸大到算不下")?;
    let pixels = dib.get(offset..offset + need).ok_or_else(|| {
        format!(
            "位图数据被截断(要 {need} 字节,只有 {})",
            dib.len().saturating_sub(offset)
        )
    })?;

    let sr = mask_shift_scale(mr);
    let sg = mask_shift_scale(mg);
    let sb = mask_shift_scale(mb);

    let mut rgb = Vec::with_capacity(w as usize * h as usize * 3);
    for y in 0..h {
        // 自下而上的图要倒着读,否则出来是上下颠倒的 —— 而「颠倒」这种错
        // 只有人眼看得见,任何自动判据都不会响。
        let row_ix = if top_down { y } else { h - 1 - y };
        let row = &pixels[row_ix as usize * stride..];
        for x in 0..w as usize {
            let p = &row[x * bytes_per_px..];
            if bit_count == 24 {
                rgb.extend_from_slice(&[p[2], p[1], p[0]]);
            } else {
                let v = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
                rgb.extend_from_slice(&[
                    take_channel(v, mr, sr),
                    take_channel(v, mg, sg),
                    take_channel(v, mb, sb),
                ]);
            }
        }
    }
    Ok(Bitmap { w, h, rgb })
}

/// RGB8 → PNG 字节。
pub fn encode_png(bm: &Bitmap) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, bm.w, bm.h);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc
            .write_header()
            .map_err(|e| format!("PNG 编码失败:{e}"))?;
        writer
            .write_image_data(&bm.rgb)
            .map_err(|e| format!("PNG 编码失败:{e}"))?;
    }
    Ok(out)
}

/// 上传用的文件名。
///
/// `stamp` 是调用方按本机时区格好的 `20260831-142530`,`seq` 是本进程内的
/// 序号。**两样都要**:`/tmp` 是所有用户共用的目录,同一秒里连贴两张截图
/// (完全正常的操作)撞名的话,第二张会静默盖掉第一张 —— 或者更糟,盖掉
/// 别人的同名文件。
pub fn file_name(stamp: &str, seq: u64) -> String {
    format!("mullion-{stamp}-{seq}.png")
}

/// 截图默认落在哪儿。用户可以在会话的「SFTP」分节里改。
///
/// `/tmp` 而不是家目录:这些是**说完话就没用**的临时图,堆在家目录里迟早
/// 要人手动清;`/tmp` 由系统自己回收。
pub const DEFAULT_DIR: &str = "/tmp";

/// Unix 秒 + 时区 → 文件名里那段 `20260831-142530`。
///
/// 与 `localtime::format_unix` 分开写而不是复用:那个是**给人看的**
/// `2026-08-31 14:25`(带空格和冒号),直接拿来当文件名,远端一个
/// `mullion-2026-08-31 14:25:30-1.png` 会让用户每次引用都得加引号 ——
/// 而这条路的全部意义就是「路径直接打进输入行」。
pub fn stamp(secs: i64, offset: time::UtcOffset) -> String {
    let Ok(dt) = time::OffsetDateTime::from_unix_timestamp(secs) else {
        return "unknown".into();
    };
    let dt = dt.to_offset(offset);
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        dt.year(),
        dt.month() as u8,
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second()
    )
}

/// 目录 + 文件名 → 远端绝对路径。空目录落回 [`DEFAULT_DIR`]。
///
/// 尾斜杠要吃掉:用户在表单里敲 `/tmp/` 是完全正常的写法,拼出
/// `/tmp//mullion-….png` 虽然 POSIX 上照样能打开,但**那串路径会原样打进
/// 输入行**,看起来像个 bug。
pub fn remote_join(dir: &str, name: &str) -> String {
    let dir = dir.trim();
    let dir = if dir.is_empty() { DEFAULT_DIR } else { dir };
    format!("{}/{name}", dir.trim_end_matches('/'))
}

/// 终端里裸 `Ctrl+V` 这一下该干什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipPaste {
    /// 照旧走 F18 那条文本粘贴(多行仍然弹确认)。
    Text,
    /// 剪贴板里只有位图 —— 编码上传,把远端路径打进输入行。
    Image,
    /// 两样都没有:**不要吞掉这一下按键**,原样编码成 `^V` 发下去
    /// (readline 的 quoted-insert 靠它,吞了就再也输不出控制字符)。
    Neither,
}

/// 文本优先。
///
/// 顺序不是随手定的:从浏览器/IDE 复制**图片**时,很多程序会同时往剪贴板里
/// 放一份文本(图片 URL、HTML 片段)。若图优先,用户复制一段代码后按
/// `Ctrl+V`,得到的会是一次莫名其妙的截图上传。反过来,Win+Shift+S 截屏
/// **只放位图不放文本**,文本优先不会挡住截图这条路。
pub fn clip_paste(has_text: bool, has_image: bool) -> ClipPaste {
    match (has_text, has_image) {
        (true, _) => ClipPaste::Text,
        (false, true) => ClipPaste::Image,
        (false, false) => ClipPaste::Neither,
    }
}

#[cfg(windows)]
pub use win::clipboard_dib;

/// 非 Windows 上没有这条路。返回 `None` 让调用方走「剪贴板里没有图」那一支。
#[cfg(not(windows))]
pub fn clipboard_dib() -> Option<Vec<u8>> {
    None
}

#[cfg(windows)]
mod win {
    use windows::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    };
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};

    /// `CF_DIB`。`windows` crate 把它定义成 `u32` 常量,这里直接写值免得为了
    /// 一个数字多开一个 feature。
    const CF_DIB: u32 = 8;

    /// 读剪贴板里的 `CF_DIB`,拷成自有的 `Vec`。
    ///
    /// **必须拷贝**:`GetClipboardData` 返回的句柄归剪贴板所有,`CloseClipboard`
    /// 之后随时可能失效。拿着它跨线程/跨帧用是典型的 use-after-free,而且在
    /// 本机上多半「碰巧还能用」。
    pub fn clipboard_dib() -> Option<Vec<u8>> {
        unsafe {
            if IsClipboardFormatAvailable(CF_DIB).is_err() {
                return None;
            }
            // 剪贴板被别的进程占着是 Windows 上的常态(输入法、剪贴板管理器),
            // 打不开就当这次没有图 —— 不弹窗、不重试。
            // `None` = 把剪贴板关联到当前任务(等价于传 NULL 窗口句柄)。
            // `windows` 0.59 把这个参数改成了 `Option<HWND>`,写 `HWND::default()`
            // 在 Linux 上根本不编译到、只有交叉编译那一刀才会炸。
            if OpenClipboard(None).is_err() {
                log::debug!(target: "mullion", "剪贴板被占用,这次不取图");
                return None;
            }
            let out = (|| {
                let handle = GetClipboardData(CF_DIB).ok()?;
                let hglobal = windows::Win32::Foundation::HGLOBAL(handle.0);
                let ptr = GlobalLock(hglobal);
                if ptr.is_null() {
                    return None;
                }
                let len = GlobalSize(hglobal);
                let data = std::slice::from_raw_parts(ptr as *const u8, len).to_vec();
                let _ = GlobalUnlock(hglobal);
                Some(data)
            })();
            let _ = CloseClipboard();
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 拼一个最小可用的 `CF_DIB`:40 字节 BITMAPINFOHEADER + 像素。
    fn dib(w: i32, h: i32, bit_count: u16, compression: u32, tail: &[u8], px: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&40u32.to_le_bytes());
        v.extend_from_slice(&w.to_le_bytes());
        v.extend_from_slice(&h.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes()); // planes
        v.extend_from_slice(&bit_count.to_le_bytes());
        v.extend_from_slice(&compression.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes()); // sizeImage
        v.extend_from_slice(&0i32.to_le_bytes()); // xppm
        v.extend_from_slice(&0i32.to_le_bytes()); // yppm
        v.extend_from_slice(&0u32.to_le_bytes()); // clrUsed
        v.extend_from_slice(&0u32.to_le_bytes()); // clrImportant
        v.extend_from_slice(tail);
        v.extend_from_slice(px);
        v
    }

    /// 24 位、自下而上(BMP 的传统摆法)。**这条盯的是「上下别颠倒」**——
    /// 颠倒了只有人眼看得见,编码/传输/日志全部照常成功。
    #[test]
    fn a_bottom_up_24bit_dib_comes_out_the_right_way_up() {
        // 2×2,行按 4 字节对齐(2 像素 × 3 字节 = 6 → 补到 8)。
        // 文件里先存的是**最下面那行**。
        let px = [
            // 下面一行:蓝、绿(BGR 序)
            0xff, 0x00, 0x00, /* 蓝 */ 0x00, 0xff, 0x00, /* 绿 */ 0, 0,
            // 上面一行:红、白
            0x00, 0x00, 0xff, /* 红 */ 0xff, 0xff, 0xff, /* 白 */ 0, 0,
        ];
        let bm = dib_to_bitmap(&dib(2, 2, 24, 0, &[], &px)).expect("该解得出来");
        assert_eq!((bm.w, bm.h), (2, 2));
        assert_eq!(
            bm.rgb,
            vec![
                0xff, 0x00, 0x00, // 左上:红
                0xff, 0xff, 0xff, // 右上:白
                0x00, 0x00, 0xff, // 左下:蓝
                0x00, 0xff, 0x00, // 右下:绿
            ],
            "自下而上的图没翻过来 —— 出来的截图是上下颠倒的"
        );
    }

    /// 负高度 = 自上而下,不许再翻一次。
    #[test]
    fn a_top_down_dib_is_not_flipped_again() {
        let px = [
            0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0, 0, // 第一行就是最上面那行
            0xff, 0x00, 0x00, 0x00, 0xff, 0x00, 0, 0,
        ];
        let bm = dib_to_bitmap(&dib(2, -2, 24, 0, &[], &px)).expect("该解得出来");
        assert_eq!(&bm.rgb[..3], &[0xff, 0x00, 0x00], "左上角该是红");
        assert_eq!(&bm.rgb[9..], &[0x00, 0xff, 0x00], "右下角该是绿");
    }

    /// 32 位、alpha 整片是 0(Windows 截图工具的常态)。
    ///
    /// **这条是这个模块最重要的一条**:照单全收 alpha 的话,编码成功、传输
    /// 成功、远端打开是一张全透明的图,全程零报错。
    #[test]
    fn a_32bit_dib_with_zero_alpha_still_yields_an_opaque_image() {
        // BGRA,A 全 0。
        let px = [
            0xff, 0x00, 0x00, 0x00, // 蓝
            0x00, 0x00, 0xff, 0x00, // 红
        ];
        let bm = dib_to_bitmap(&dib(2, -1, 32, 0, &[], &px)).expect("该解得出来");
        assert_eq!(bm.rgb, vec![0x00, 0x00, 0xff, 0xff, 0x00, 0x00]);
        let png = encode_png(&bm).expect("该编得出来");
        // PNG 的颜色类型写在 IHDR 第 10 个字节(偏移 8+8+8=25):2 = 真彩色
        // 无 alpha。判它而不是判「文件不空」——后者在一张全透明图上也成立。
        assert_eq!(png[25], 2, "PNG 里还带着 alpha 通道");
    }

    /// BI_BITFIELDS:掩码跟在 40 字节头后面,像素起点要多跳 12 字节。
    /// 漏跳的话图案整体偏移,但**尺寸和字节数都对**,不会报错。
    #[test]
    fn bitfield_masks_push_the_pixel_data_twelve_bytes_further() {
        let masks = [
            0x00u8, 0x00, 0xff, 0x00, // R
            0x00, 0xff, 0x00, 0x00, // G
            0xff, 0x00, 0x00, 0x00, // B
        ];
        let px = [0x11, 0x22, 0x33, 0x44];
        let bm = dib_to_bitmap(&dib(1, -1, 32, 3, &masks, &px)).expect("该解得出来");
        assert_eq!(bm.rgb, vec![0x33, 0x22, 0x11], "掩码没解对");
    }

    /// 认不出来的位深要**说出实际是多少**,别只说一句「不支持」——
    /// 用户拿这句话才知道该换个截图工具。
    #[test]
    fn an_unsupported_bit_depth_says_what_it_actually_was() {
        let e = dib_to_bitmap(&dib(2, 2, 8, 0, &[], &[0; 64])).unwrap_err();
        assert!(e.contains('8'), "错误里没提位深:{e}");
    }

    /// 数据被截断时报错,不 panic —— 剪贴板内容来自别的进程,不可信。
    #[test]
    fn a_truncated_dib_is_an_error_not_a_panic() {
        let e = dib_to_bitmap(&dib(1000, 1000, 32, 0, &[], &[0; 16])).unwrap_err();
        assert!(e.contains("截断"), "{e}");
    }

    /// 空输入、乱字节都不许 panic。
    #[test]
    fn garbage_input_is_rejected_without_panicking() {
        assert!(dib_to_bitmap(&[]).is_err());
        assert!(dib_to_bitmap(&[1, 2, 3]).is_err());
        assert!(dib_to_bitmap(&[0xff; 64]).is_err());
    }

    /// 时间戳里不许出现空格和冒号 —— 那串路径要**直接打进终端输入行**,
    /// 带空格就得加引号,这条路的全部意义就没了。
    ///
    /// 自证会变红:把 `stamp` 改成复用 `localtime::format_unix`。
    #[test]
    fn the_stamp_is_shell_safe_and_follows_the_local_zone() {
        let secs = 1_787_963_400i64; // 2026-08-29T00:30:00Z
        let east8 = time::UtcOffset::from_hms(8, 0, 0).expect("UTC+8");
        let s = stamp(secs, east8);
        assert_eq!(s, "20260829-083000");
        assert!(
            !s.contains(' ') && !s.contains(':'),
            "时间戳里有空格或冒号:{s}"
        );
        assert_eq!(stamp(secs, time::UtcOffset::UTC), "20260829-003000");
    }

    /// 尾斜杠不许拼出 `//`。
    #[test]
    fn a_trailing_slash_in_the_configured_dir_does_not_double_up() {
        assert_eq!(remote_join("/tmp/", "a.png"), "/tmp/a.png");
        assert_eq!(remote_join("/srv/shots", "a.png"), "/srv/shots/a.png");
        assert_eq!(
            remote_join("  ", "a.png"),
            "/tmp/a.png",
            "空目录该落回 /tmp"
        );
    }

    /// **文本优先**:复制一段代码时剪贴板里常常同时躺着一份位图
    /// (浏览器/IDE 的常态),图优先会把一次普通粘贴变成一次截图上传。
    ///
    /// 自证会变红:把 `clip_paste` 的第一条 match 臂换成
    /// `(_, true) => ClipPaste::Image`。
    #[test]
    fn text_wins_when_the_clipboard_holds_both() {
        assert_eq!(clip_paste(true, true), ClipPaste::Text);
        assert_eq!(clip_paste(true, false), ClipPaste::Text);
        assert_eq!(clip_paste(false, true), ClipPaste::Image);
        // 两样都没有时**不能**当成「处理过了」——那会吞掉 `^V`。
        assert_eq!(clip_paste(false, false), ClipPaste::Neither);
    }

    /// 文件名带序号 —— `/tmp` 是共用目录,同一秒贴两张不能撞名。
    #[test]
    fn two_shots_in_the_same_second_get_different_names() {
        assert_ne!(
            file_name("20260831-142530", 1),
            file_name("20260831-142530", 2)
        );
        assert_eq!(
            file_name("20260831-142530", 7),
            "mullion-20260831-142530-7.png"
        );
    }
}
