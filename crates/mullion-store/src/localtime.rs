//! F234 的相对时间文案。**纯函数**:`now` 与 `offset` 都是参数,
//! 不读进程级时区(那一份 `OnceLock` 留在 app 的 `localtime`,它要求
//! 在起任何线程之前取值,是进程启动的事,不属于 store)。F344 从 app 搬来。

use time::{OffsetDateTime, UtcOffset};

/// F234:「最后打开」的相对时间文案。
///
/// `then_rfc3339` 是我们自己写出去的时间戳(`OffsetDateTime::now_utc()` 配
/// `Rfc3339`,一律 UTC)。档位:刚刚 / N 分钟前 / N 小时前 / 昨天 / N 天前 /
/// 超 30 天落回本地日期。
///
/// **`now` 与 `offset` 都是参数不是全局读取**(同 `format_unix`):进程级
/// `OnceLock` 一旦被别的测试设过就再也改不动,拿它当输入的测试会互相打架;
/// 而「现在几点」写死才能让断言不随真实时钟在跨整点/跨午夜时随机变红。
///
/// **「昨天」与「N 天前」按本地日界判,不按 UTC 日期。**
/// `2026-09-07T17:00Z` 在 UTC 下是 9 月 7 日(昨天),在 UTC+8 下却是
/// 9 月 8 日凌晨 1 点(今天)。拿 UTC 日期算的话,用户每天早上 8 点前都会看到
/// 错误的「昨天」—— 而且完全静默:编译、测试、日志一律正常,只有人眼能发现。
///
/// **返回值不会自己更新**:帧闸只在有事件时重绘,这行字会停在画出来那一刻
/// 直到用户动一下。为一行时间文字去请求定时重绘,和 F157~F183 一连八个切片
/// 抠空闲帧的方向直接相反 —— 这个代价是认下的。
///
/// 解析不出来时**返回原文**:那只可能来自手改配置文件,而编一句「时间未知」
/// 既没有可操作性,又把「配置里写了什么」这唯一线索藏起来。
pub fn relative(then_rfc3339: &str, now: OffsetDateTime, offset: UtcOffset) -> String {
    let Ok(then) =
        OffsetDateTime::parse(then_rfc3339, &time::format_description::well_known::Rfc3339)
    else {
        return then_rfc3339.to_string();
    };
    let secs = (now - then).whole_seconds();
    // `< 60` 而不是 `(0..60)`:时钟回拨(或配置来自一台快钟的机器)时负数差
    // 要落进「刚刚」,不能算出「-3 分钟前」。
    if secs < 60 {
        return "刚刚".to_string();
    }
    if secs < 3600 {
        return format!("{} 分钟前", secs / 60);
    }
    let then_local = then.to_offset(offset).date();
    let now_local = now.to_offset(offset).date();
    let days = (now_local - then_local).whole_days();
    if days == 0 {
        return format!("{} 小时前", secs / 3600);
    }
    if days == 1 {
        return "昨天".to_string();
    }
    if days <= 30 {
        return format!("{days} 天前");
    }
    format!(
        "{:04}-{:02}-{:02}",
        then_local.year(),
        then_local.month() as u8,
        then_local.day()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 固定一个「现在」:`2026-09-08T12:00:00Z`。测试不许用真实时钟 ——
    /// 那样断言会在跨过整点/午夜时随机变红。
    fn now() -> OffsetDateTime {
        OffsetDateTime::parse(
            "2026-09-08T12:00:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .expect("测试基准时间写错了")
    }

    /// UTC+8(本项目主场景)。
    fn cn() -> UtcOffset {
        UtcOffset::from_hms(8, 0, 0).expect("UTC+8 是合法偏移")
    }

    /// 一分钟以内不报「0 分钟前」—— 那句话读起来像出错了。
    ///
    /// 自证会变红:把 `secs < 60` 那一支删掉。
    #[test]
    fn anything_within_a_minute_reads_as_just_now() {
        assert_eq!(relative("2026-09-08T11:59:30Z", now(), cn()), "刚刚");
    }

    #[test]
    fn minutes_and_hours_are_counted_off_the_raw_difference() {
        assert_eq!(relative("2026-09-08T11:30:00Z", now(), cn()), "30 分钟前");
        assert_eq!(relative("2026-09-08T09:00:00Z", now(), cn()), "3 小时前");
    }

    /// **本片最重要的一条。**「昨天」按**本地**日界判,不按 UTC 日期。
    ///
    /// `2026-09-07T17:00Z` 在 UTC 下是 9 月 7 日(昨天),在 UTC+8 下却是
    /// 9 月 8 日凌晨 1 点(今天,19 小时前)。拿 UTC 日期算的话,用户每天
    /// 早上 8 点前都会看到错误的「昨天」—— 而且完全静默:编译、测试、日志
    /// 一律正常,只有人眼能发现。
    ///
    /// 自证会变红:把 `then.to_offset(offset).date()` 与
    /// `now.to_offset(offset).date()` 里的 `to_offset(offset)` 都去掉,
    /// 第二条断言会变成「昨天」。
    #[test]
    fn yesterday_is_decided_by_the_local_day_boundary_not_the_utc_one() {
        // UTC+8 下这是 9 月 7 日 23:00 —— 真的是昨天。
        assert_eq!(relative("2026-09-07T15:00:00Z", now(), cn()), "昨天");
        // UTC+8 下这是 9 月 8 日 01:00 —— 今天凌晨,19 小时前。
        assert_eq!(relative("2026-09-07T17:00:00Z", now(), cn()), "19 小时前");
    }

    #[test]
    fn a_few_days_back_counts_days_and_a_month_back_falls_back_to_a_date() {
        assert_eq!(relative("2026-09-05T12:00:00Z", now(), cn()), "3 天前");
        // 31 天前 → 落回本地日期。UTC+8 下 2026-08-08T00:00Z 是 08-08 08:00。
        assert_eq!(relative("2026-08-08T00:00:00Z", now(), cn()), "2026-08-08");
    }

    /// 时钟回拨(或配置来自一台快钟的机器)时不能显示「-3 分钟前」。
    ///
    /// 自证会变红:把 `secs < 60` 改成 `(0..60).contains(&secs)`。
    #[test]
    fn a_timestamp_from_the_future_reads_as_just_now_not_a_negative_count() {
        assert_eq!(relative("2026-09-08T12:05:00Z", now(), cn()), "刚刚");
    }

    /// 解析不出来时**返回原文**,不编一句「时间未知」—— 那句话既没有可操作
    /// 性,又把「配置里到底写了什么」这唯一的线索藏起来。
    ///
    /// 自证会变红:把早退分支改成返回 `"未知".to_string()`。
    #[test]
    fn an_unparseable_stamp_is_shown_verbatim_instead_of_a_made_up_placeholder() {
        assert_eq!(relative("昨天下午", now(), cn()), "昨天下午");
    }
}
