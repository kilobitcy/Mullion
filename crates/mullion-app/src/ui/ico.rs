//! 会话图标(F61):`.ico` 的导入归一化与解码。
//!
//! 别跟隔壁 `icon.rs` 搞混:那个是**控制图标**(自绘的箭头/叉,不走字体),
//! 这个是**会话图标**(用户导入的 .ico 文件)。
//!
//! **为什么要「归一化」而不是原样存用户的文件**:用户随手挑的 ico 里可能有
//! 1 帧也可能有 8 帧、可能是 16px 也可能是 256px、可能是 BMP 帧也可能是 PNG 帧。
//! 界面上要用的只有 32 和 64 两个尺寸(列表的两个紧凑档),而正文要 base64
//! 内嵌进 `sessions.toml` —— 直接存原文件意味着一张 256x256 的图标能让配置
//! 文件涨几十 KB,而且渲染时每次都要挑帧、缩放。
//!
//! 所以导入时一次性做完:挑最合适的一帧 → 重采样出 32 和 64 → 重新编码成
//! 一个**只含这两帧**的 ico → base64。之后渲染路径拿到的永远是确定的两张图。
//!
//! **刻意不用 `image` crate**:见 workspace `Cargo.toml` 里 `arboard` 那条注释
//! (N6 exe 体积)。`ico` 只拉 byteorder + png。
//!
//! 本模块**零 egui、零 IO**,全是纯函数 —— 图标解码错了在界面上只表现为
//! 「一团糊」或「什么都没有」,肉眼判不出是挑帧错了还是重采样错了,只能靠单测。

use base64::Engine as _;

/// 归一化后的 ico 里固定含有的两个尺寸。小的给「32px + 名称」档,
/// 大的给「只有图标」档。
pub const SMALL: u32 = 32;
pub const LARGE: u32 = 64;

/// 允许读进来的 `.ico` 原始文件上限。
///
/// 这是**解码侧**的防线,不是存储侧的 —— 归一化之后真正进配置的只有两帧
/// (PNG 压缩后通常 1~4 KB)。1 MiB 足够放下带 256x256 帧的图标包,又不至于
/// 让人拿一个改了扩展名的视频文件把内存打满。
pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;

/// 导入失败的原因。每个变体都要能直接说给用户听 —— 「导入失败」四个字
/// 等于没说,用户不知道是该换个文件还是该换个工具。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportError {
    /// 文件太大(附实际字节数)。
    TooBig(usize),
    /// 根本不是 ico,或者结构坏了。
    NotIco,
    /// 是 ico,但一帧都没有。
    Empty,
    /// 某一帧解不开(混了不认识的压缩)。
    BadFrame,
    /// 归一化之后重新编码失败。正常路径不会走到,留着是为了不 `unwrap`。
    Encode,
}

impl ImportError {
    /// 给用户看的说明。
    pub fn message(self) -> String {
        match self {
            Self::TooBig(n) => format!(
                "文件太大({} KB),上限 {} KB",
                n / 1024,
                MAX_SOURCE_BYTES / 1024
            ),
            Self::NotIco => "这不是一个 .ico 文件,或者文件已损坏".into(),
            Self::Empty => "这个 .ico 里没有任何图像".into(),
            Self::BadFrame => "这个 .ico 里的图像解不开,换一个文件试试".into(),
            Self::Encode => "图标转换失败".into(),
        }
    }
}

/// 一张 RGBA8 位图。`px.len() == (size * size * 4)`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    pub size: u32,
    pub px: Vec<u8>,
}

/// 归一化之后的两张图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frames {
    pub small: Rgba,
    pub large: Rgba,
}

impl Frames {
    /// 按需要的边长取一张。只认 `SMALL`/`LARGE` 两档 —— 中间尺寸交给 GPU
    /// 采样,比在 CPU 上再重采样一次快得多也清楚得多。
    pub fn pick(&self, want: u32) -> &Rgba {
        if want <= SMALL {
            &self.small
        } else {
            &self.large
        }
    }
}

/// 把用户选的 `.ico` 原始字节归一化成「只含 32 与 64 两帧」的 ico,再 base64。
///
/// 返回的字符串就是存进 `sessions.toml` 的 `IconSpec::value`。
pub fn import(bytes: &[u8]) -> Result<String, ImportError> {
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(ImportError::TooBig(bytes.len()));
    }
    let frames = normalize(bytes)?;

    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for f in [&frames.small, &frames.large] {
        let img = ico::IconImage::from_rgba_data(f.size, f.size, f.px.clone());
        // `encode_as_png` 而不是 `encode`:同一张 32x32 图标,BMP 帧是裸的
        // 4 KB,PNG 帧压完通常几百字节。这份数据要 base64 塞进 TOML,
        // 体积直接决定配置文件好不好看。
        dir.add_entry(ico::IconDirEntry::encode_as_png(&img).map_err(|_| ImportError::Encode)?);
    }
    let mut out = Vec::new();
    dir.write(&mut out).map_err(|_| ImportError::Encode)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(out))
}

/// 从存进配置的 base64 解回两张图。
///
/// 返回 `None` 而不是 `Result`:调用方是渲染路径,拿不到图就是不画,没有
/// 第二条分支可走。真正需要区分原因的是导入那一侧(见 `ImportError`)。
pub fn decode(b64: &str) -> Option<Frames> {
    let raw = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    normalize(&raw).ok()
}

/// 解析 ico → 挑帧 → 重采样出两个尺寸。
///
/// 导入和解码共用这一条路:归一化后的 ico 再走一遍 `normalize`,挑帧会精确
/// 命中已有的 32/64 两帧、重采样退化成恒等,所以解码路径不会二次失真。
fn normalize(bytes: &[u8]) -> Result<Frames, ImportError> {
    let dir = ico::IconDir::read(std::io::Cursor::new(bytes)).map_err(|_| ImportError::NotIco)?;
    if dir.entries().is_empty() {
        return Err(ImportError::Empty);
    }
    Ok(Frames {
        small: frame_at(&dir, SMALL)?,
        large: frame_at(&dir, LARGE)?,
    })
}

/// 取出边长为 `want` 的一帧(必要时重采样)。
fn frame_at(dir: &ico::IconDir, want: u32) -> Result<Rgba, ImportError> {
    let entry = best_entry(dir, want).ok_or(ImportError::Empty)?;
    let img = entry.decode().map_err(|_| ImportError::BadFrame)?;
    let src = Rgba {
        // 非正方形的 ico 是合法的(极少见)。按短边裁剪太粗暴,直接按长边
        // 当成正方形来采样 —— 重采样是按比例映射的,长宽不等只会让图被拉伸,
        // 而不是错位或越界。
        size: img.width().max(img.height()).max(1),
        px: rgba_square(&img),
    };
    Ok(resample(&src, want))
}

/// 把可能非正方形的一帧补成正方形(空白处透明)。
fn rgba_square(img: &ico::IconImage) -> Vec<u8> {
    let (w, h) = (img.width(), img.height());
    let n = w.max(h).max(1);
    if w == h && w == n {
        return img.rgba_data().to_vec();
    }
    let mut out = vec![0u8; (n * n * 4) as usize];
    let src = img.rgba_data();
    for y in 0..h.min(n) {
        for x in 0..w.min(n) {
            let s = ((y * w + x) * 4) as usize;
            let d = ((y * n + x) * 4) as usize;
            if s + 4 <= src.len() && d + 4 <= out.len() {
                out[d..d + 4].copy_from_slice(&src[s..s + 4]);
            }
        }
    }
    out
}

/// 挑最适合缩到 `want` 的一帧:**优先不小于目标的最小帧**,没有就取最大的。
///
/// 从大缩到小丢的是细节,从小放到大糊的是整张图 —— 后者在 64px 那一档
/// 尤其难看(用户导入一个 16x16 的老图标,放到 64 就是一团马赛克),所以
/// 宁可拿 256 缩下来也不拿 16 放上去。
fn best_entry(dir: &ico::IconDir, want: u32) -> Option<&ico::IconDirEntry> {
    let side = |e: &ico::IconDirEntry| e.width().max(e.height());
    dir.entries()
        .iter()
        .filter(|e| side(e) >= want)
        .min_by_key(|e| side(e))
        .or_else(|| dir.entries().iter().max_by_key(|e| side(e)))
}

/// 盒式重采样到 `dst` 边长。
///
/// **RGB 必须按 alpha 加权平均**,不能直接算术平均:PNG/ICO 里全透明像素的
/// RGB 通常是 0(黑),直接平均会让图标每一条透明边缘都渗出一圈黑边 ——
/// 缩得越狠越明显,而 32px 档正是缩得最狠的那一档。
///
/// 放大时源矩形退化成一个像素,等价于最近邻;边长相同时是恒等变换。
fn resample(src: &Rgba, dst: u32) -> Rgba {
    let (sn, dn) = (src.size.max(1), dst.max(1));
    if sn == dn && src.px.len() == (sn * sn * 4) as usize {
        return src.clone();
    }
    let mut out = vec![0u8; (dn * dn * 4) as usize];
    let at = |x: u32, y: u32| -> Option<[u8; 4]> {
        let i = ((y * sn + x) * 4) as usize;
        src.px.get(i..i + 4).map(|s| [s[0], s[1], s[2], s[3]])
    };
    for dy in 0..dn {
        for dx in 0..dn {
            // 目标像素在源图上覆盖的矩形 [x0,x1) × [y0,y1)。整数运算,
            // 不引浮点 —— 边界能被整除时(32→64、64→32)划分是精确的。
            let x0 = dx * sn / dn;
            let x1 = ((dx + 1) * sn).div_ceil(dn).max(x0 + 1).min(sn);
            let y0 = dy * sn / dn;
            let y1 = ((dy + 1) * sn).div_ceil(dn).max(y0 + 1).min(sn);

            let (mut wr, mut wg, mut wb, mut sa, mut n) = (0u64, 0u64, 0u64, 0u64, 0u64);
            for y in y0..y1 {
                for x in x0..x1 {
                    let Some(p) = at(x, y) else { continue };
                    let a = p[3] as u64;
                    wr += p[0] as u64 * a;
                    wg += p[1] as u64 * a;
                    wb += p[2] as u64 * a;
                    sa += a;
                    n += 1;
                }
            }
            let d = ((dy * dn + dx) * 4) as usize;
            if n == 0 {
                continue;
            }
            // 整块全透明:alpha 权重全是 0,RGB 无从谈起,留全 0(反正 alpha=0)。
            let (r, g, b) = match (wr.checked_div(sa), wg.checked_div(sa), wb.checked_div(sa)) {
                (Some(r), Some(g), Some(b)) => (r as u8, g as u8, b as u8),
                _ => (0, 0, 0),
            };
            out[d] = r;
            out[d + 1] = g;
            out[d + 2] = b;
            out[d + 3] = (sa / n) as u8;
        }
    }
    Rgba { size: dn, px: out }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个 `size` 边长的纯色 ico(单帧,PNG 编码)。
    fn solid_ico(size: u32, rgba: [u8; 4]) -> Vec<u8> {
        let px: Vec<u8> = std::iter::repeat_n(rgba, (size * size) as usize)
            .flatten()
            .collect();
        let img = ico::IconImage::from_rgba_data(size, size, px);
        let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
        dir.add_entry(ico::IconDirEntry::encode_as_png(&img).unwrap());
        let mut out = Vec::new();
        dir.write(&mut out).unwrap();
        out
    }

    /// 导入必须**永远**吐出 32 和 64 两帧,不管用户给的是多大的图 —— 列表的
    /// 两个紧凑档直接按这两个尺寸取图,少一帧就是那一档没图标可画。
    ///
    /// 自证会变红:把 `normalize` 里的 `large` 改成也取 `SMALL`,第二组断言炸。
    #[test]
    fn importing_any_size_always_yields_both_a_32_and_a_64_frame() {
        for src in [16u32, 32, 48, 64, 256] {
            let b64 = import(&solid_ico(src, [200, 30, 40, 255])).expect("导入应当成功");
            let f = decode(&b64).expect("刚存进去的图标必须解得回来");
            assert_eq!(f.small.size, SMALL, "源图 {src}px 的小帧");
            assert_eq!(f.large.size, LARGE, "源图 {src}px 的大帧");
            assert_eq!(f.small.px.len(), (SMALL * SMALL * 4) as usize);
            assert_eq!(f.large.px.len(), (LARGE * LARGE * 4) as usize);
        }
    }

    /// 挑帧规则:宁可从大的缩下来,也不把小的放上去。
    ///
    /// 一个同时有 16 和 128 两帧的 ico,要 64px 时必须挑 128 那一帧 ——
    /// 挑 16 的话 64px 档就是一团马赛克。用颜色区分两帧来验证挑了哪个。
    ///
    /// 自证会变红:把 `best_entry` 的 `filter(>= want)` 去掉(退化成「挑最接近的」),
    /// 128 和 16 到 64 的距离分别是 64 和 48,会挑 16,断言炸。
    #[test]
    fn picking_a_frame_prefers_scaling_down_over_scaling_up() {
        let small = ico::IconImage::from_rgba_data(
            16,
            16,
            std::iter::repeat_n([255u8, 0, 0, 255], 16 * 16)
                .flatten()
                .collect(),
        );
        let big = ico::IconImage::from_rgba_data(
            128,
            128,
            std::iter::repeat_n([0u8, 0, 255, 255], 128 * 128)
                .flatten()
                .collect(),
        );
        let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
        dir.add_entry(ico::IconDirEntry::encode_as_png(&small).unwrap());
        dir.add_entry(ico::IconDirEntry::encode_as_png(&big).unwrap());
        let mut raw = Vec::new();
        dir.write(&mut raw).unwrap();

        let f = decode(&base64::engine::general_purpose::STANDARD.encode(&raw)).unwrap();
        assert_eq!(
            &f.large.px[..4],
            &[0, 0, 255, 255],
            "64px 应当来自 128 那一帧(蓝),而不是 16 那一帧(红)"
        );
    }

    /// 重采样必须**按 alpha 加权**平均 RGB。
    ///
    /// 半透明边缘是图标最常见的形态。一个「一半全透明黑、一半不透明红」的
    /// 2x2 缩成 1x1:正确结果是红色 + 半透明;算术平均会得到暗红(128,0,0),
    /// 也就是每条边缘渗一圈黑 —— 32px 档缩得最狠,一眼能看出脏。
    ///
    /// 自证会变红:把 `wr / sa` 改成 `wr / (n * 255)` 之类的算术平均,断言炸。
    #[test]
    fn downscaling_weights_colour_by_alpha_so_edges_do_not_go_black() {
        let src = Rgba {
            size: 2,
            px: vec![
                255, 0, 0, 255, // 不透明红
                0, 0, 0, 0, // 全透明黑
                0, 0, 0, 0, // 全透明黑
                255, 0, 0, 255, // 不透明红
            ],
        };
        let out = resample(&src, 1);
        assert_eq!(out.size, 1);
        assert_eq!(
            &out.px[..3],
            &[255, 0, 0],
            "颜色应当是纯红,不该被透明像素的黑拉暗"
        );
        assert_eq!(out.px[3], 127, "alpha 是四个像素的算术平均");
    }

    /// 整块全透明时不该除以 0。这不是假想输入 —— 图标四角基本都是全透明的。
    #[test]
    fn a_fully_transparent_block_does_not_divide_by_zero() {
        let src = Rgba {
            size: 2,
            px: vec![0; 16],
        };
        let out = resample(&src, 1);
        assert_eq!(out.px, vec![0, 0, 0, 0]);
    }

    /// 边长相同时是恒等变换 —— 归一化后的图标再走一遍解码不能二次失真
    /// (`import` 存的和 `decode` 读的是同一条 `normalize` 路径)。
    #[test]
    fn resampling_to_the_same_size_is_the_identity() {
        let src = Rgba {
            size: 2,
            px: (0..16).collect(),
        };
        assert_eq!(resample(&src, 2), src);
    }

    /// 存进去什么色,取出来还是什么色。这条守的是整条
    /// 「导入 → base64 → TOML → 解码 → 上屏」的往返:中间任何一环把通道
    /// 顺序搞反(RGBA/BGRA 是 ico 里最容易踩的坑),这里就变红。
    #[test]
    fn a_colour_survives_the_round_trip_through_base64() {
        let b64 = import(&solid_ico(64, [10, 200, 90, 255])).unwrap();
        let f = decode(&b64).unwrap();
        assert_eq!(&f.large.px[..4], &[10, 200, 90, 255]);
        assert_eq!(&f.small.px[..4], &[10, 200, 90, 255]);
    }

    /// 垃圾输入要给出**能说给用户听**的理由,而不是 panic 或一句「失败」。
    #[test]
    fn rubbish_input_is_rejected_with_a_reason_a_user_can_act_on() {
        assert_eq!(import(b"not an icon at all"), Err(ImportError::NotIco));
        assert_eq!(
            import(&vec![0u8; MAX_SOURCE_BYTES + 1]),
            Err(ImportError::TooBig(MAX_SOURCE_BYTES + 1))
        );
        // 每个变体都得有话说 —— 空字符串等于没提示。
        for e in [
            ImportError::TooBig(2 * 1024 * 1024),
            ImportError::NotIco,
            ImportError::Empty,
            ImportError::BadFrame,
            ImportError::Encode,
        ] {
            assert!(!e.message().trim().is_empty(), "{e:?} 没有给用户的说明");
        }
        assert!(decode("这不是 base64").is_none());
        assert!(decode("").is_none());
    }

    /// 归一化之后的体积必须小到能塞进 TOML。用户存几十个会话,每个图标
    /// 几十 KB 的话配置文件就没法看了(也没法手工编辑)。
    ///
    /// 4 KB 是有余量的上限:实测纯色 64px 只有几百字节,复杂图标 1~3 KB。
    #[test]
    fn a_normalised_icon_is_small_enough_to_live_inside_the_config_file() {
        let b64 = import(&solid_ico(256, [1, 2, 3, 255])).unwrap();
        assert!(
            b64.len() < 4096,
            "归一化后的 base64 有 {} 字节,太大了",
            b64.len()
        );
    }

    /// 非正方形的一帧不能让重采样越界或 panic。
    #[test]
    fn a_non_square_frame_is_padded_instead_of_crashing() {
        let img = ico::IconImage::from_rgba_data(
            32,
            16,
            std::iter::repeat_n([9u8, 9, 9, 255], 32 * 16)
                .flatten()
                .collect(),
        );
        let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
        dir.add_entry(ico::IconDirEntry::encode_as_png(&img).unwrap());
        let mut raw = Vec::new();
        dir.write(&mut raw).unwrap();
        let f = decode(&base64::engine::general_purpose::STANDARD.encode(&raw))
            .expect("非正方形也要能解出来");
        assert_eq!(f.small.size, SMALL);
        assert_eq!(f.large.size, LARGE);
    }
}
