//! F298:节点状态(内存 / 磁盘 / 出口国家)。命令、解析、调度、显示,全是纯函数;
//! 唯一有状态的是 [`StatsCell`] —— 后台 task 写、事件循环读的共享格子,
//! 同 `remote_bootstrap::BootstrapFlags` 的跨线程模式。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const SAMPLE_EVERY: Duration = Duration::from_secs(10);
pub const COUNTRY_EVERY: Duration = Duration::from_secs(300);
/// exec 整体超时。挂住的 exec 不超时的话 `busy` 永远置着,这条连接从此不再采样。
pub const EXEC_TIMEOUT: Duration = Duration::from_secs(15);
pub const WARN_PCT: u8 = 90;

/// 内存 + 磁盘一次取完。输出两行:`mem <total_kb> <avail_kb>` / `disk <used_kb> <avail_kb>`。
/// 没有 `/proc/meminfo`(非 Linux)时第一行不出现 → 内存那格省掉。
/// `LC_ALL=C`:df 的表头/数字格式跟着 locale 走。
pub fn sample_command() -> Vec<u8> {
    b"LC_ALL=C; export LC_ALL; \
awk '/^MemTotal:/{t=$2} /^MemAvailable:/{a=$2} END{if(t>0&&a!=\"\")print \"mem\",t,a}' /proc/meminfo 2>/dev/null; \
df -Pk / 2>/dev/null | awk 'NR==2{print \"disk\",$3,$4}'"
        .to_vec()
}

/// 出口国家。curl 优先、没有就 wget、都没有退出 127。
/// `-f`:HTTP 错误(限流 429)给非零退出码,不把错误页当国家码。
pub fn country_command() -> Vec<u8> {
    b"if command -v curl >/dev/null 2>&1; then curl -fsS --max-time 5 https://ipinfo.io/country; \
elif command -v wget >/dev/null 2>&1; then wget -qO- -T 5 https://ipinfo.io/country; \
else exit 127; fi"
        .to_vec()
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Reading<T> {
    /// 还没取到过。不显示(连上头几秒不闪一个 `--`)。
    #[default]
    Unknown,
    Ok(T),
    /// 取失败,字符串是给 hover 的原因。显示 `--`。
    Failed(String),
    /// 这台机器上没有这一项(非 Linux 没有内存)。不显示。
    Absent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub used_kb: u64,
    pub total_kb: u64,
}

impl Usage {
    pub fn pct(&self) -> u8 {
        if self.total_kb == 0 {
            return 0;
        }
        ((self.used_kb as f64 * 100.0 / self.total_kb as f64).round() as u64).min(100) as u8
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub mem: Reading<Usage>,
    pub disk: Reading<Usage>,
    pub country: Reading<String>,
}

/// 解析 [`sample_command`] 的输出。
pub fn parse_sample(stdout: &str) -> (Reading<Usage>, Reading<Usage>) {
    let mut mem = Reading::Absent;
    let mut disk = Reading::Failed("df 没有输出".into());
    for line in stdout.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        let n = |i: usize| f.get(i).and_then(|s| s.parse::<u64>().ok());
        match f.first().copied() {
            Some("mem") => {
                mem = match (n(1), n(2)) {
                    (Some(t), Some(a)) if t > 0 => Reading::Ok(Usage {
                        used_kb: t.saturating_sub(a),
                        total_kb: t,
                    }),
                    _ => Reading::Failed(format!("看不懂 /proc/meminfo:{line}")),
                };
            }
            Some("disk") => {
                disk = match (n(1), n(2)) {
                    // df 的 Use% 口径 = used / (used + avail),不是 used / total
                    // (ext4 的保留块会让后者偏低)。与用户在远端敲 df 看到的一致。
                    (Some(u), Some(a)) if u + a > 0 => Reading::Ok(Usage {
                        used_kb: u,
                        total_kb: u + a,
                    }),
                    _ => Reading::Failed(format!("看不懂 df 输出:{line}")),
                };
            }
            _ => {}
        }
    }
    (mem, disk)
}

/// 解析国家命令的结果。`exit` = 远端退出码(`None` = 远端没报)。
pub fn parse_country(exit: Option<u32>, stdout: &str) -> Reading<String> {
    match exit {
        Some(0) | None => {
            let s = stdout.trim();
            if s.len() == 2 && s.bytes().all(|b| b.is_ascii_uppercase()) {
                Reading::Ok(s.to_owned())
            } else {
                Reading::Failed(format!(
                    "ipinfo 返回的不是国家代码:{}",
                    s.chars().take(40).collect::<String>()
                ))
            }
        }
        Some(127) => Reading::Failed("远端没有 curl 也没有 wget".into()),
        Some(28) | Some(4) => Reading::Failed("访问 ipinfo.io 超时 / 网络不通".into()),
        Some(22) | Some(8) => Reading::Failed("ipinfo.io 返回错误(可能被限流)".into()),
        Some(c) => Reading::Failed(format!("取国家失败,退出码 {c}")),
    }
}

/// 到点没有。`busy` 时恒不到(上一次还挂在网络上)。
pub fn due(last: Option<Instant>, busy: bool, every: Duration, now: Instant) -> bool {
    !busy && last.is_none_or(|at| now.duration_since(at) >= every)
}

/// 下一次该醒的时刻(并入 `next_timer_wake`)。`busy` 时 `None` —— 结果回来的
/// 事件会唤醒,在这里报一个过去的时刻会让事件循环忙转(T7)。
pub fn next_due(
    last: Option<Instant>,
    busy: bool,
    every: Duration,
    now: Instant,
) -> Option<Instant> {
    if busy {
        return None;
    }
    Some(last.map_or(now, |at| at + every))
}

#[derive(Debug, Default)]
struct Inner {
    snap: Snapshot,
    sample_at: Option<Instant>,
    sample_busy: bool,
    country_at: Option<Instant>,
    country_busy: bool,
    /// F298 复核:`reset_schedule` 每次都递增。换连接(重连/换节点)之后,
    /// 挂在旧连接上、迟迟才回来的探针带的是**旧世代号**——`finish_sample`/
    /// `finish_country` 拿它跟当前世代比,对不上就整条丢弃(不写快照、不清
    /// busy),不然旧连接的结果会盖掉新连接刚置上的 busy 或者写进一份过期
    /// 数据。
    generation: u64,
}

/// 一条连接的状态格子。`Clone` = 同一份(`Arc`)。
#[derive(Debug, Clone, Default)]
pub struct StatsCell(Arc<Mutex<Inner>>);

/// 事件循环这一侧要发起什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Plan {
    pub sample: bool,
    pub country: bool,
    /// 发起这一轮时的世代号。task 把它原样带回 `finish_sample`/
    /// `finish_country`,用来分辨「这条结果是不是已经作废的上一轮」。
    pub generation: u64,
}

impl StatsCell {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // 中毒 = 某个 task 在持锁时 panic;数据只是显示用,照读。
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn snapshot(&self) -> Snapshot {
        self.lock().snap.clone()
    }
    /// 判到点并**同一次加锁里**置 busy、记发起时刻(发起侧只有事件循环一个线程,
    /// 同 `BootstrapFlags` 的前提)。
    pub fn plan(&self, now: Instant) -> Plan {
        let mut g = self.lock();
        let p = Plan {
            sample: due(g.sample_at, g.sample_busy, SAMPLE_EVERY, now),
            country: due(g.country_at, g.country_busy, COUNTRY_EVERY, now),
            generation: g.generation,
        };
        if p.sample {
            g.sample_busy = true;
            g.sample_at = Some(now);
        }
        if p.country {
            g.country_busy = true;
            g.country_at = Some(now);
        }
        p
    }
    pub fn next_wake(&self, now: Instant) -> Option<Instant> {
        let g = self.lock();
        [
            next_due(g.sample_at, g.sample_busy, SAMPLE_EVERY, now),
            next_due(g.country_at, g.country_busy, COUNTRY_EVERY, now),
        ]
        .into_iter()
        .flatten()
        .min()
    }
    /// `generation` 必须是发起这次采样时 [`Plan::generation`] 给的那个值。
    /// 对不上当前世代(其间 `reset_schedule` 被调过)就整条丢弃——**既不
    /// 写快照,也不清 busy**:busy 是新世代自己置的,旧世代的结果没资格
    /// 清它,清了会让新一轮误以为「已经有结果回来了」而提前放行下一次 due。
    pub fn finish_sample(&self, generation: u64, mem: Reading<Usage>, disk: Reading<Usage>) {
        let mut g = self.lock();
        if generation != g.generation {
            return;
        }
        g.snap.mem = mem;
        g.snap.disk = disk;
        g.sample_busy = false;
    }
    /// 同 [`Self::finish_sample`] 的世代校验。
    pub fn finish_country(&self, generation: u64, c: Reading<String>) {
        let mut g = self.lock();
        if generation != g.generation {
            return;
        }
        g.snap.country = c;
        g.country_busy = false;
    }
    /// 用户点了国家那格:下一次 tick 立刻取(在途时不叠发)。
    pub fn refresh_country_now(&self) {
        self.lock().country_at = None;
    }
    /// 断线重连换了 handle:两项都立刻重取,旧值留着(不闪空)。世代号
    /// 递增——挂在旧连接上还在途的探针,回来时会因为世代对不上被丢弃。
    pub fn reset_schedule(&self) {
        let mut g = self.lock();
        g.sample_at = None;
        g.country_at = None;
        g.sample_busy = false;
        g.country_busy = false;
        g.generation = g.generation.wrapping_add(1);
    }
}

/// 标题条上的一格。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub text: String,
    pub hover: String,
    pub warn: bool,
    /// 点它刷新国家。
    pub is_country: bool,
}

fn gib(kb: u64) -> String {
    format!("{:.1} GiB", kb as f64 / 1024.0 / 1024.0)
}

/// 快照 → 标题条上的几格(从左到右:内存、磁盘、国家)。`Unknown`/`Absent` 不出格。
pub fn pieces(s: &Snapshot) -> Vec<Piece> {
    let mut out = Vec::new();
    let usage = |label: &str, r: &Reading<Usage>, out: &mut Vec<Piece>| match r {
        Reading::Ok(u) => out.push(Piece {
            text: format!("{label} {}%", u.pct()),
            hover: format!("{label}已用 {} / {}", gib(u.used_kb), gib(u.total_kb)),
            warn: u.pct() >= WARN_PCT,
            is_country: false,
        }),
        Reading::Failed(why) => out.push(Piece {
            text: format!("{label} --"),
            hover: why.clone(),
            warn: false,
            is_country: false,
        }),
        Reading::Unknown | Reading::Absent => {}
    };
    usage("内存", &s.mem, &mut out);
    usage("磁盘", &s.disk, &mut out);
    match &s.country {
        Reading::Ok(c) => out.push(Piece {
            text: c.clone(),
            hover: "远端出口国家(ipinfo.io),点击刷新".into(),
            warn: false,
            is_country: true,
        }),
        Reading::Failed(why) => out.push(Piece {
            text: "--".into(),
            hover: format!("{why}\n点击重试"),
            warn: false,
            is_country: true,
        }),
        Reading::Unknown | Reading::Absent => {}
    }
    out
}

/// 窄条取舍:状态段宽 `stats_w`、标题全宽 `title_w`、可用 `avail`。
/// 标题保底 `min(title_w, TITLE_RESERVE)`,剩下的放得下状态段才画 ——
/// 状态段先于标题让位(用户确认的取舍:认 pane 靠标题)。
pub const TITLE_RESERVE: f32 = 220.0;
pub fn stats_fit(avail: f32, stats_w: f32, title_w: f32) -> bool {
    avail - stats_w >= title_w.min(TITLE_RESERVE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 自证会变红:把 disk 的 total 改成只取 `$2`(总量)口径。
    #[test]
    fn a_linux_sample_parses_into_memory_and_df_style_disk_usage() {
        let (m, d) = parse_sample("mem 8000000 2000000\ndisk 710 290\n");
        assert_eq!(
            m,
            Reading::Ok(Usage {
                used_kb: 6000000,
                total_kb: 8000000
            })
        );
        assert_eq!(
            d,
            Reading::Ok(Usage {
                used_kb: 710,
                total_kb: 1000
            })
        );
        assert_eq!(
            match d {
                Reading::Ok(u) => u.pct(),
                _ => 0,
            },
            71
        );
    }

    /// 非 Linux:没有 mem 行 → 内存 Absent(不出格),磁盘照常。
    /// 自证会变红:把 `mem` 初值改成 `Reading::Failed(..)`。
    #[test]
    fn no_meminfo_means_memory_is_absent_not_failed() {
        let (m, d) = parse_sample("disk 1 1\n");
        assert_eq!(m, Reading::Absent);
        assert!(matches!(d, Reading::Ok(_)));
        assert!(pieces(&Snapshot {
            mem: m,
            disk: d,
            country: Reading::Unknown
        })
        .iter()
        .all(|p| !p.text.starts_with("内存")));
    }

    #[test]
    fn garbage_is_a_failure_with_a_reason() {
        let (m, d) = parse_sample("mem x y\n");
        assert!(matches!(m, Reading::Failed(_)));
        assert!(matches!(d, Reading::Failed(_)));
    }

    /// 自证会变红:删掉 127 分支(落到通用「退出码」文案)。
    #[test]
    fn country_exit_codes_map_to_reasons_a_user_can_act_on() {
        assert_eq!(parse_country(Some(0), "JP\n"), Reading::Ok("JP".into()));
        assert!(matches!(
            parse_country(Some(0), "<html>"),
            Reading::Failed(_)
        ));
        match parse_country(Some(127), "") {
            Reading::Failed(s) => assert!(s.contains("curl"), "{s}"),
            _ => panic!(),
        }
        match parse_country(Some(22), "") {
            Reading::Failed(s) => assert!(s.contains("限流"), "{s}"),
            _ => panic!(),
        }
    }

    /// busy 时不到点、也不报唤醒时刻(报过去的时刻 = T7 忙转)。
    /// 自证会变红:把 `next_due` 里 `if busy { return None; }` 删掉。
    #[test]
    fn a_busy_probe_is_never_due_and_never_asks_for_a_wakeup() {
        let now = Instant::now();
        assert!(!due(None, true, SAMPLE_EVERY, now));
        assert_eq!(next_due(None, true, SAMPLE_EVERY, now), None);
        assert!(due(None, false, SAMPLE_EVERY, now));
        let last = now - Duration::from_secs(3);
        assert!(!due(Some(last), false, SAMPLE_EVERY, now));
        assert_eq!(
            next_due(Some(last), false, SAMPLE_EVERY, now),
            Some(last + SAMPLE_EVERY)
        );
    }

    /// plan 同时置 busy:连续两次 plan,第二次不会再发。
    /// 自证会变红:删掉 `plan` 里 `g.sample_busy = true;`。
    ///
    /// 注意:仅靠「同一个 `now` 连打两次 plan」抓不住这处删除 —— `sample_at`
    /// 已经写成 `Some(now)`,`due()` 光靠这一条就会判「未到点」,busy 与否不
    /// 影响结果。真正需要 busy 的场景是「探针还没回来、但已经过了一个采样
    /// 周期」,所以下面额外把时钟拨过 `SAMPLE_EVERY`、且**不**调用
    /// `finish_sample`(探针仍在途)再 plan 一次。
    #[test]
    fn planning_marks_busy_so_a_second_tick_does_not_double_fire() {
        let c = StatsCell::default();
        let now = Instant::now();
        assert_eq!(
            c.plan(now),
            Plan {
                sample: true,
                country: true,
                generation: 0,
            }
        );
        assert_eq!(c.plan(now), Plan::default());
        // 探针仍未返回(没调 finish_sample),但时钟已经过了一个采样周期:
        // 没有 busy 挡着的话,due() 会因为 sample_at 过期而重新判定到点。
        let still_busy_but_overdue = now + SAMPLE_EVERY + Duration::from_secs(1);
        assert!(
            !c.plan(still_busy_but_overdue).sample,
            "上一次探针还没回来,不该在它还在途时重发"
        );
        c.finish_sample(0, Reading::Absent, Reading::Absent);
        assert!(!c.plan(now).sample, "刚采过,10 秒内不该再采");
        c.refresh_country_now();
        c.finish_country(0, Reading::Unknown);
        assert!(c.plan(now).country, "点了刷新,下一次 tick 该立刻取");
    }

    /// F298 复核(Important):`reset_schedule`(换连接/换节点)之后,挂在
    /// 旧连接上迟迟才回来的采样结果,既不能覆盖快照,也不能清掉新一轮的
    /// busy——不然旧连接的过期数据会显示成「当前」,或者让新一轮误以为
    /// 已经有结果回来而提前放行下一次 due。
    ///
    /// 自证会变红:把 `finish_sample` 里的世代校验删掉(`if generation !=
    /// g.generation { return; }`)。
    #[test]
    fn a_late_result_from_before_a_reset_neither_overwrites_the_snapshot_nor_clears_the_new_busy() {
        let c = StatsCell::default();
        let now = Instant::now();

        // 第一轮:发起采样,拿到世代号(此时是 0),但探针还没回来。
        let old_gen = c.plan(now).generation;

        // 换连接:调度重置,世代号往前走一格;新一轮立刻发起(sample_at
        // 被清空、busy 也被清空,due() 会重新判定到点),busy 再次置起。
        c.reset_schedule();
        let new_gen = c.plan(now).generation;
        assert_ne!(old_gen, new_gen, "reset_schedule 必须递增世代号");

        // 旧世代的探针这时候才姗姗来迟。
        c.finish_sample(
            old_gen,
            Reading::Ok(Usage {
                used_kb: 999,
                total_kb: 1000,
            }),
            Reading::Ok(Usage {
                used_kb: 999,
                total_kb: 1000,
            }),
        );

        // 快照没被这份过期数据污染——还是初始的 Unknown。
        assert_eq!(
            c.snapshot().mem,
            Reading::Unknown,
            "旧世代的结果不该写进快照"
        );
        assert_eq!(
            c.snapshot().disk,
            Reading::Unknown,
            "旧世代的结果不该写进快照"
        );
        // busy 也没被清掉——新一轮的探针还在途,due() 该继续判「未到点」。
        assert!(
            !c.plan(now).sample,
            "旧世代的结果不该清掉新一轮的 busy,不然会误判成已经有结果回来了"
        );

        // 对照:带上正确的当前世代号,结果才应该生效。
        c.finish_sample(
            new_gen,
            Reading::Ok(Usage {
                used_kb: 1,
                total_kb: 1000,
            }),
            Reading::Ok(Usage {
                used_kb: 1,
                total_kb: 1000,
            }),
        );
        assert_eq!(
            c.snapshot().mem,
            Reading::Ok(Usage {
                used_kb: 1,
                total_kb: 1000
            })
        );
    }

    /// 同上,`finish_country` 那一路。
    #[test]
    fn a_late_country_result_from_before_a_reset_is_dropped_too() {
        let c = StatsCell::default();
        let now = Instant::now();
        let old_gen = c.plan(now).generation;
        c.reset_schedule();
        let new_gen = c.plan(now).generation;

        c.finish_country(old_gen, Reading::Ok("JP".into()));
        assert_eq!(
            c.snapshot().country,
            Reading::Unknown,
            "旧世代的国家结果不该写进快照"
        );
        assert!(!c.plan(now).country, "旧世代的结果不该清掉新一轮的 busy");

        c.finish_country(new_gen, Reading::Ok("JP".into()));
        assert_eq!(c.snapshot().country, Reading::Ok("JP".into()));
    }

    /// ≥90% 标 warn,89% 不标。自证会变红:`>=` 改 `>`(90% 不标)。
    #[test]
    fn ninety_percent_is_a_warning() {
        let at = |used| Snapshot {
            mem: Reading::Ok(Usage {
                used_kb: used,
                total_kb: 100,
            }),
            ..Default::default()
        };
        assert!(pieces(&at(90))[0].warn);
        assert!(!pieces(&at(89))[0].warn);
    }

    #[test]
    fn failures_render_as_dashes_with_the_reason_on_hover() {
        let s = Snapshot {
            disk: Reading::Failed("df 没有输出".into()),
            country: Reading::Failed("远端没有 curl 也没有 wget".into()),
            ..Default::default()
        };
        let p = pieces(&s);
        assert_eq!(p[0].text, "磁盘 --");
        assert_eq!(p[0].hover, "df 没有输出");
        assert_eq!(p[1].text, "--");
        assert!(p[1].is_country);
    }

    /// 窄条让位:放不下时状态段先让。三点:放得下 / 恰好 / 差一点。
    /// 自证会变红:把 `>=` 改成 `>`(恰好那点翻转)。
    #[test]
    fn the_stats_segment_yields_before_the_title() {
        assert!(stats_fit(600.0, 200.0, 300.0));
        assert!(stats_fit(420.0, 200.0, 300.0), "恰好留够 220 保底");
        assert!(!stats_fit(419.0, 200.0, 300.0));
        assert!(
            stats_fit(300.0, 200.0, 100.0),
            "标题本身短于保底时按标题宽算"
        );
    }
}
