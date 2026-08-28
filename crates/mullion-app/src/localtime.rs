//! 本机时区偏移(F186)。**进程启动时取一次,全程用它。**
//!
//! 为什么是「取一次」而不是每次格式化现取:`time` 的
//! `UtcOffset::current_local_offset()` 在 **Unix 上只在单线程进程里成功** ——
//! 它背后是 `localtime_r`,而那个函数会读进程级的 `TZ` 环境,别的线程同时
//! `setenv` 就是 UB,所以 `time` 直接用「进程有几个线程」当闸门。我们的进程
//! 起完 tokio 运行时和看门狗之后就是多线程,那时再取**恒返回 `Err`**。
//!
//! Windows 走 `GetTimeZoneInformation`,没有这个限制 —— 但把取值时机分成
//! 两套平台各一份没有意义,统一在 `main` 的最前面取,两个平台跑同一条路。
//!
//! **代价(已认下)**:进程跑着的时候用户改了系统时区、或跨过夏令时切换,
//! 显示的时间要重启才更新。相对「Linux 上永远拿不到时区」这个替代方案,
//! 这个代价小得多。

use std::sync::OnceLock;

use time::{OffsetDateTime, UtcOffset};

static OFFSET: OnceLock<UtcOffset> = OnceLock::new();

/// 取一次本机时区偏移并记下来。
///
/// **必须在起任何线程之前调**(见模块文档)。重复调用只有第一次算数。
/// 返回一句给日志的说明:成功时是偏移量本身,失败时说清降级到了 UTC ——
/// 「时间显示差 8 小时」在日志里没有任何别的痕迹,不留这一行就查不出来。
pub fn init() -> String {
    match UtcOffset::current_local_offset() {
        Ok(off) => {
            let _ = OFFSET.set(off);
            let (h, m, _) = off.as_hms();
            format!("本机时区偏移 UTC{h:+03}:{:02}", m.abs())
        }
        Err(e) => format!("取不到本机时区偏移({e}),文件时间按 UTC 显示"),
    }
}

/// 已记下的偏移;没调过 `init`(或取失败)时是 UTC。
///
/// 回落到 UTC 而不是 panic:时区取不着只该让时间戳差几个小时,不该让
/// 文件面板画不出来。
pub fn offset() -> UtcOffset {
    OFFSET.get().copied().unwrap_or(UtcOffset::UTC)
}

/// Unix 秒 → `2026-08-28 15:04`,按给定偏移换算。
///
/// **偏移是参数不是全局读取**:这样它在开发机上是可单测的纯函数(进程级
/// `OnceLock` 一旦被别的测试设过就再也改不动,拿它当输入的测试会互相打架)。
pub fn format_unix(secs: u32, offset: UtcOffset) -> String {
    match OffsetDateTime::from_unix_timestamp(secs as i64) {
        Ok(dt) => {
            let dt = dt.to_offset(offset);
            format!(
                "{:04}-{:02}-{:02} {:02}:{:02}",
                dt.year(),
                dt.month() as u8,
                dt.day(),
                dt.hour(),
                dt.minute()
            )
        }
        Err(_) => "—".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F186:同一个时间戳在不同时区下必须画出不同的**当地**时间。
    ///
    /// 这是本片的正事:改之前 `mtime_text` 直接用 `from_unix_timestamp` 的
    /// 结果(恒 UTC),东八区用户看到的每个文件都早 8 小时 —— 而「刚才存的
    /// 那个文件显示是上午」没有任何报错,只能靠人眼发现。
    ///
    /// 自证会变红:把 `format_unix` 里的 `.to_offset(offset)` 删掉。
    #[test]
    fn the_same_instant_renders_as_different_wall_clocks_in_different_zones() {
        // 2026-08-29T00:30:00Z
        let secs = 1_787_963_400u32;
        assert_eq!(format_unix(secs, UtcOffset::UTC), "2026-08-29 00:30");
        let east8 = UtcOffset::from_hms(8, 0, 0).expect("UTC+8");
        assert_eq!(
            format_unix(secs, east8),
            "2026-08-29 08:30",
            "东八区没换算 —— 用户看到的每个文件都早 8 小时"
        );
        // 负偏移要能跨回前一天,不是只把小时数减一下。
        let west5 = UtcOffset::from_hms(-5, 0, 0).expect("UTC-5");
        assert_eq!(
            format_unix(secs, west5),
            "2026-08-28 19:30",
            "跨日边界没退一天"
        );
    }

    /// 半小时时区(印度 UTC+5:30)是真实存在的,不是理论边界。分钟位
    /// 只取时间戳自己的、不加偏移里的分钟,在那里就会差 30 分钟。
    #[test]
    fn a_half_hour_zone_shifts_the_minutes_too() {
        let secs = 1_787_963_400u32; // 2026-08-29T00:30:00Z
        let india = UtcOffset::from_hms(5, 30, 0).expect("UTC+5:30");
        assert_eq!(format_unix(secs, india), "2026-08-29 06:00");
    }

    /// **取时区必须排在起线程之前。** 这条守的是顺序,不是「有没有调」。
    ///
    /// 排到看门狗或 tokio 运行时之后,Unix 上 `current_local_offset()` 恒返回
    /// `Err`,`init()` 悄悄降级到 UTC —— 编译过、测试全绿、Windows 上还照样对
    /// (那边走 `GetTimeZoneInformation`,没有线程闸门),只有 Linux/macOS
    /// 用户看到时间差几小时。没有比源码顺序更早的判据了。
    ///
    /// **必须先剥注释行**:`main.rs` 里那段说明和这里的文档都写着
    /// `start_watchdog` / `new_multi_thread`,不剥的话锚点会落在注释上,
    /// 位置比真调用点靠前,断言反而恒绿。
    ///
    /// 自证会变红:把 `main.rs` 里的 `localtime::init()` 挪到
    /// `start_watchdog(..)` 之后。
    #[test]
    fn the_offset_is_captured_before_the_process_grows_a_second_thread() {
        let code = include_str!("main.rs")
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let init = code
            .find("localtime::init()")
            .expect("main 没取本机时区 —— SFTP 的修改时间会一直按 UTC 画");
        for (needle, what) in [
            ("start_watchdog(", "看门狗线程"),
            ("new_multi_thread(", "tokio 运行时"),
        ] {
            let at = code
                .find(needle)
                .unwrap_or_else(|| panic!("main 里找不到 {needle} —— 这条守护的锚点失效了"));
            assert!(
                init < at,
                "取时区排在{what}之后 —— Unix 上那时已是多线程,\
                 current_local_offset() 恒 Err,静默退回 UTC"
            );
        }
    }

    /// 文件面板那一列**真的走记下来的偏移**,不是自己另开一条 UTC 的路。
    ///
    /// 这条补的是一个实测漏掉的口子:上面两条只钉 `format_unix` 这个纯函数,
    /// 把 `mtime_text` 改成 `format_unix(secs, UtcOffset::UTC)` 之后
    /// `cargo test --workspace` **全绿** —— 而那正好等于这次修复没做。
    /// 前后两层的判据必须各扎一次。
    ///
    /// **本测试二进制里唯一一处动全局 `OFFSET` 的地方。** `OnceLock` 只能设
    /// 一次、测试又是并行跑的,多一处就会互相抢、按调度顺序随机红。别的用例
    /// 要不同偏移就调 `format_unix` 那个纯函数版本。
    ///
    /// 自证会变红:把 `mtime_text` 里的 `crate::localtime::offset()` 换成
    /// `time::UtcOffset::UTC`。
    #[test]
    fn the_files_panel_column_renders_through_the_captured_offset() {
        // 设之前:没 `init` 过就是 UTC —— 时区取不着只该让时间戳差几小时,
        // 不该 panic 让整个文件面板画不出来。
        assert_eq!(offset(), UtcOffset::UTC, "没设过时该回落 UTC");

        let east8 = UtcOffset::from_hms(8, 0, 0).expect("UTC+8");
        OFFSET.set(east8).expect("本二进制里只该有这一处设它");
        assert_eq!(offset(), east8);
        assert_eq!(
            crate::ui::files_panel::mtime_text(1_787_963_400),
            "2026-08-29 08:30",
            "面板那一列没走记下来的偏移 —— 东八区用户看到的每个文件都早 8 小时"
        );
    }
}
