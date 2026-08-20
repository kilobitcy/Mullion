//! UI 文本里允许出现的非 ASCII 符号白名单。
//!
//! **为什么需要这个模块**：egui 的字体链只有两级 —— 内置的
//! Ubuntu-Light / NotoEmoji，加上 [`super::install_cjk_font`] 追加的系统
//! CJK 字体（Windows 上第一候选是微软雅黑）。两级都没有的字形，epaint 画成
//! 豆腐块。编译不报错、测试不报错、日志不报错，**只有人眼能看见**。
//!
//! 这个坑在本项目复发过三次：走查 P0-5 的那个删除按钮（当时的修法是
//! [`super::icon`] 自绘，但只救了那一个按钮）、v0.1.56 用户实测报的路径条
//! 下拉箭头，以及同一轮扫描顺带挖出来的另外十处。前两次都只修了当场那一个
//! 字符，没有留下任何机械检查 —— 于是它必然回来。
//!
//! **判据是「该字符在 GBK/CP936 内」**，那是微软雅黑字形覆盖面的实用近似。
//! **不是 GB18030**：GB18030 是全 Unicode 的编码方案，"能编码" 对任何字符
//! 都成立，拿它当判据等于没有判据（这个弯路本项目已经走过一次）。
//!
//! **加新符号的纪律**：先在 Windows 实机上把它画出来看一眼，再往
//! [`VERIFIED`] 里登记。**登记这一步就是闸门** —— 它逼你去看。不想登记的，
//! 走 [`super::icon`] 自绘，那条路不受任何字体覆盖面影响。
//!
//! 守护在 `tests/glyph_whitelist.rs`（扫全部 `src/**/*.rs` 的字符串字面量）。

/// 已实机验过、允许直接写进 UI 字符串的非 ASCII 符号。
///
/// 每一个都在 GBK 内（`tests::every_registered_symbol_is_really_inside_gbk`
/// 钉着这一点，防止有人凭想象往里加）。
pub const VERIFIED: &[char] = &[
    '—', // U+2014 破折号：全项目最常用的「空值/分隔」符号
    '…', // U+2026 省略号：截断标记
    '·', // U+00B7 间隔号：状态栏各段之间
    '→', // U+2192 右箭头：符号链接目标、跳板链
    '↑', // U+2191 上箭头：上传方向、上一级
    '↓', // U+2193 下箭头：下载方向
    '×', // U+00D7 乘号：关闭、尺寸的 80×24
    '●', // U+25CF 实心圆：脏标记 / 状态点
    '≥', // U+2265 大于等于：设置里的数值说明
    '②', // U+2461 带圈数字：错误文案里的步骤编号
    '★', // U+2605 实心五角星：已收藏
    '☆', // U+2606 空心五角星：未收藏
    '▲', // U+25B2 实心上三角：升序标识
    '▼', // U+25BC 实心下三角：降序标识
];

/// 这个字符能不能直接写进 UI 字符串。
///
/// 三类放行：ASCII、CJK 汉字（含扩展 A）、中文标点与全角形式。
/// 其余非 ASCII 一律要在 [`VERIFIED`] 里登记过。
pub fn is_allowed(c: char) -> bool {
    if c.is_ascii() {
        return true;
    }
    let o = c as u32;
    // CJK 统一表意文字 + 扩展 A。
    if (0x4E00..=0x9FFF).contains(&o) || (0x3400..=0x4DBF).contains(&o) {
        return true;
    }
    // CJK 符号与标点（、。「」〈〉…）+ 全角 ASCII 形式（，：（））。
    if (0x3000..=0x303F).contains(&o) || (0xFF00..=0xFF65).contains(&o) {
        return true;
    }
    VERIFIED.contains(&c)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 三类放行：ASCII、CJK 汉字、中文标点。
    #[test]
    fn ascii_cjk_and_chinese_punctuation_are_always_allowed() {
        for c in ['a', 'Z', '0', ' ', '/', ':', '\n'] {
            assert!(is_allowed(c), "ASCII {c:?} 被误拦");
        }
        for c in ['中', '文', '目', '录', '龥'] {
            assert!(is_allowed(c), "汉字 {c:?} 被误拦");
        }
        for c in ['，', '。', '、', '「', '」', '（', '）', '：'] {
            assert!(is_allowed(c), "中文标点 {c:?} 被误拦");
        }
    }

    /// 已登记的符号放行，没登记的一律拦下。
    ///
    /// 自证会变红：把 `is_allowed` 最后一行的 `VERIFIED.contains(&c)`
    /// 改成 `true`，第二组断言立刻红。
    #[test]
    fn only_registered_symbols_pass_and_the_known_tofu_does_not() {
        for c in ['—', '…', '·', '→', '↑', '↓', '×', '●', '★', '☆', '▲', '▼'] {
            assert!(is_allowed(c), "已登记的 {c:?} 应当放行");
        }
        // 这六个在 GBK 里没有 —— 微软雅黑与 egui 内置字体两边都画不出来。
        for c in ['▾', '▸', '⟳', '↻', '✕', '⚠'] {
            assert!(!is_allowed(c), "{c:?} 在 GBK 外，必须被拦下");
        }
    }

    /// 白名单里的每一个都必须真在 GBK 内 —— 这条钉的是「登记」这道闸门
    /// 本身：谁往 `VERIFIED` 里塞了一个凭想象的字符，这里会红。
    ///
    /// 判据用 `encoding_rs` 的 GBK 编码器；编不出来（返回替换字符）就是
    /// 字体多半也没有。
    ///
    /// 自证会变红：往 `VERIFIED` 里加一个 `'▾'`。
    #[test]
    fn every_registered_symbol_is_really_inside_gbk() {
        for &c in VERIFIED {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            let (bytes, _, had_errors) = encoding_rs::GBK.encode(s);
            assert!(
                !had_errors && !bytes.is_empty(),
                "U+{:04X} {c:?} 不在 GBK 内，不该出现在白名单里",
                c as u32
            );
        }
    }
}
