//! 传输的**纯逻辑**(F52):落盘用的临时名、Windows 文件名合法性、冲突改名。
//! 零 egui / 零 tokio / 零 IO —— 这三件事全是「算错了才发现、发现时文件
//! 已经落在错的地方」的类型,必须能在没有网络、没有窗口的情况下单测。

/// 半截文件的后缀。取一个**一眼能认出是我们留下的**名字 —— 用 `.tmp`
/// 之类的通名,断线现场留下的垃圾会跟用户自己的临时文件混在一起。
pub const PART_SUFFIX: &str = ".mullion-part";

/// 写入时实际用的名字。
///
/// 设计 D19:**新建**目标先写 `<name>.mullion-part` 再 rename —— 断线留下的
/// 半截文件一眼能认出来,也不会被误当成完整文件;**覆盖**已存在的目标则
/// 直接写,不走 rename —— rename 会换掉 inode,属主 / 权限 / ACL / 硬链接
/// 全部丢失,而用户的心智模型只是「换了内容」。
pub fn staging_name(final_name: &str, overwriting: bool) -> String {
    if overwriting {
        final_name.to_string()
    } else {
        format!("{final_name}{PART_SUFFIX}")
    }
}

/// Windows 上非法的文件名 → `Some(建议名)`;合法 → `None`。
///
/// 设计 D16:**打断并给建议名**,不静默改写。静默改写的后果是「下下来的
/// 文件到底叫什么」无法预测,再传回去就成了另一个文件;而 Windows 是本
/// 项目唯一的一等公民,这条路径每天都会走到。
pub fn illegal_on_windows(name: &str) -> Option<String> {
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    // 设备名判定看的是**第一个点之前**那段:`nul.txt` 在 Windows 上照样
    // 打不开,只有 `console.txt` 这种「前缀相同但不是同一个词」才合法。
    let stem = name.split('.').next().unwrap_or("");
    let reserved = RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem));
    let bad_char =
        |c: char| matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c < ' ';
    // 结尾的点和空格 Windows 会**静默吃掉**,等于悄悄换了个名字 ——
    // 于是「传上去再传下来」不是同一个文件。
    let bad_tail = name.ends_with('.') || name.ends_with(' ');
    if !reserved && !name.chars().any(bad_char) && !bad_tail {
        return None;
    }
    let mut fixed: String = name
        .chars()
        .map(|c| if bad_char(c) { '_' } else { c })
        .collect();
    while fixed.ends_with('.') || fixed.ends_with(' ') {
        fixed.pop();
    }
    if reserved {
        fixed.insert(0, '_');
    }
    if fixed.is_empty() {
        fixed.push('_');
    }
    Some(fixed)
}

/// 冲突选「重命名」时生成的新名字:`a.txt` → `a (1).txt`。
///
/// 扩展名只算**最后一段**(`a.tar.gz` → `a.tar (1).gz`)。这跟资源管理器
/// 的行为一致 —— 特判 `.tar.gz` 一类复合扩展名要维护一张表,而表里没有
/// 的那些(`.pkg.tar.zst`…)反而更别扭。
///
/// `taken` 由调用方提供(远端要发 stat、本地查磁盘),这里只管算名字 ——
/// 掺了 IO 就没法单测,而「计数器插在扩展名前还是后」正是会写错的地方。
pub fn dedup_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    let (stem, ext) = split_ext(name);
    for i in 1..10_000 {
        let cand = if ext.is_empty() {
            format!("{stem} ({i})")
        } else {
            format!("{stem} ({i}).{ext}")
        };
        if !taken(&cand) {
            return cand;
        }
    }
    // 一万个重名是病态输入。给一个必然不同的兜底,别 panic ——
    // 传输队列里 panic 掉一条 worker,整个队列就再也走不动了。
    format!("{name}{PART_SUFFIX}.dup")
}

/// 拆成「主干 + 扩展名」。`.bashrc` 整个算主干(开头的点不是扩展名分隔,
/// 拆了会生成 ` (1).bashrc`,既难看又不再是隐藏文件),`a.tar.gz` 只把
/// 最后一段当扩展名。
fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(0) | None => (name, ""),
        Some(i) => (&name[..i], &name[i + 1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_target_is_written_through_a_part_file_so_a_crash_leaves_no_half_file() {
        assert_eq!(staging_name("a.bin", false), "a.bin.mullion-part");
    }

    #[test]
    fn overwriting_an_existing_target_writes_in_place_to_keep_inode_and_permissions() {
        // D19:覆盖不能走 .part + rename —— rename 换 inode,
        // 属主 / 权限 / ACL / 硬链接全丢,而用户以为只是「换了内容」。
        assert_eq!(staging_name("a.bin", true), "a.bin");
    }

    #[test]
    fn windows_reserved_characters_are_reported_instead_of_being_silently_rewritten() {
        let bad = illegal_on_windows("a:b?.txt");
        assert!(bad.is_some(), "冒号和问号应当被判非法");
        assert_eq!(bad.unwrap(), "a_b_.txt", "建议名应把非法字符换成下划线");
        assert!(
            illegal_on_windows("普通名字.txt").is_none(),
            "中文名是合法的"
        );
    }

    #[test]
    fn windows_reserved_device_names_are_reported_too() {
        assert!(illegal_on_windows("CON").is_some(), "CON 是设备名");
        assert!(
            illegal_on_windows("nul.txt").is_some(),
            "带扩展名也仍然是设备名"
        );
        assert!(
            illegal_on_windows("console.txt").is_none(),
            "只是前缀相同,合法"
        );
    }

    #[test]
    fn a_trailing_dot_is_reported_because_windows_would_swallow_it() {
        // 不挡的话:传上去叫 `a.`,传下来变成 `a`,两边对不上还查不出原因。
        let sug = illegal_on_windows("a.").expect("结尾的点该被判非法");
        assert_eq!(sug, "a");
        assert!(illegal_on_windows("b ").is_some(), "结尾的空格同理");
    }

    #[test]
    fn renaming_on_conflict_inserts_the_counter_before_the_extension() {
        // 扩展名只算最后一段(与资源管理器一致),所以计数器插在 `.gz` 前。
        let taken = |n: &str| ["a.tar.gz", "a.tar (1).gz"].contains(&n);
        assert_eq!(dedup_name("a.tar.gz", taken), "a.tar (2).gz");
    }

    #[test]
    fn renaming_a_dotfile_does_not_treat_the_leading_dot_as_an_extension() {
        let taken = |n: &str| n == ".bashrc";
        assert_eq!(dedup_name(".bashrc", taken), ".bashrc (1)");
    }
}
