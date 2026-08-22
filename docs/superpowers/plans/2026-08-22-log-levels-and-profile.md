# 日志分档 + 周期性性能剖面 + 脱敏导出 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 `mullion.log` 在默认档位下就带上足够定位性能瓶颈的量化数据，档位能在设置弹窗里改，并提供一份可以安全外发的脱敏副本。

**Architecture:** 不新起日志设施。`diag.rs` 已有「阶段打点 + 5 秒周期线程 + 内存采样 + 看门狗」，剖面长在它上面：把 `diag::mark()` 的时基从毫秒改成微秒并**顺手累计上一阶段的耗时**——事件循环里已经铺满 `mark()` 调用，于是逐阶段耗时分布是白得的，不新增任何插桩点。分位数由新的纯模块 `profile.rs`（对数桶直方图）提供，脱敏由新的纯模块 `redact.rs` 提供，两者都零 IO 零 UI、可纯单测。日志级别从「只认环境变量」改成「设置文件 + 环境变量覆盖」。

**Tech Stack:** Rust 2021 / rustc 1.96、`log` facade、egui 0.30、serde + toml、`std::sync::atomic`。

**当前版本:** 0.1.64 → 本切片发 0.1.65。

---

## 背景事实（实现前必读，全部已核实）

| 事实 | 位置 |
|---|---|
| `logx::init` 在 `main` 最早调用，那时还没读过任何设置 | `crates/mullion-app/src/main.rs:27` |
| `settings.toml` 是**明文 TOML、不经 keyring/主密码**，可以在 `logx::init` 之前读 | `crates/mullion-store/src/settings.rs:112` |
| `write_line` 逐行 flush + 同时写 stderr | `crates/mullion-app/src/logx.rs:147` |
| 依赖档默认 `warn`，注释明写「wgpu 一开 debug 就刷屏」 | `crates/mullion-app/src/logx.rs:14` |
| `diag::mark` 已铺满事件循环（Startup/Idle/UserEvent/WindowEvent/Resize/EguiRun/TextPrepare/Acquire/Encode/Present/StoreIo/Pump） | `crates/mullion-app/src/app.rs` 6148/6331/6335/6996/7285/7525/8309/8679/9622/9715/9744/9764/9822 |
| 已有计数器 `FRAMES / PRESENTS / SKIPPED / INBOUND_BYTES` 与 `sample_memory()` | `crates/mullion-app/src/diag.rs:70-97,113` |
| 已有 5 秒周期线程，心跳行在 `debug` 级 | `crates/mullion-app/src/diag.rs:231,262` |
| 设置弹窗分节骨架：`form::section(ui, t, "设置", "<节名>", &mut first)` + `form::grid(ui, "<id>", |ui| {..})` | `crates/mullion-app/src/ui/settings.rs:129-136` |
| 菜单项直接写 `ui_state` 字段，没有 `MenuAction` 枚举 | `crates/mullion-app/src/ui/chrome.rs:45-111` |
| UI 字符串里的非 ASCII 符号必须在 `ui::glyphs::VERIFIED` 里（T9，机械守护 `tests/glyph_whitelist.rs`） | `crates/mullion-app/src/ui/glyphs.rs:27` |

**T3/T7 红线**：本切片新增的所有采集都必须是「原子加法 / 原子存」量级，**绝不能**在帧路径上做格式化、加锁或写盘。格式化只发生在 5 秒线程里。

---

## 文件结构

**新建**
- `crates/mullion-app/src/profile.rs` — 对数桶直方图 + 分位数 + 剖面行渲染。纯函数、零 IO、可纯单测。
- `crates/mullion-app/src/redact.rs` — 日志行脱敏（稳定假名）。纯函数、零 IO、可纯单测。

**修改**
- `crates/mullion-store/src/settings.rs` — 加 `log_level` 字段。
- `crates/mullion-app/src/logx.rs` — 级别来源、依赖档映射、分档容量、缓冲写。
- `crates/mullion-app/src/diag.rs` — 微秒时基、阶段耗时累计、剖面行、无活动跳过、周期 flush。
- `crates/mullion-app/src/main.rs` — 先读设置再 `init`。
- `crates/mullion-app/src/ui/settings.rs` — 「诊断」分节。
- `crates/mullion-app/src/ui/chrome.rs` — 菜单项「导出诊断日志…」。
- `crates/mullion-app/src/ui/mod.rs` — `UiState` 加一个意图字段。
- `crates/mullion-app/src/app.rs` — 设置生效、导出接线、帧耗时/重绘原因/输入延迟/连接/资源计数。
- `crates/mullion-app/src/lib.rs` — 挂两个新模块。

---

## Task 1: store 加 `log_level` 字段

**Files:**
- Modify: `crates/mullion-store/src/settings.rs`
- Modify: `crates/mullion-store/src/lib.rs`（导出 `LogLevel`）

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-store/src/settings.rs` 的 `mod tests` 末尾（`tmux_bootstrap_survives_a_round_trip_when_turned_off` 之后）追加：

```rust
    /// 新字段：老的 settings.toml 里没有它，缺省必须是 `Info`。
    ///
    /// 给 `Debug` 的话所有老用户升上来日志量暴涨、盘被写满；给 `Error` 的话
    /// 他们的日志静默变空，而设置里显示的是另一回事。
    ///
    /// 自证会变红：把 `default_log_level` 的返回值改成 `LogLevel::Debug`。
    #[test]
    fn log_level_defaults_to_info_for_files_written_before_it_existed() {
        let dir = tmp();
        std::fs::write(
            dir.path().join(SETTINGS_FILE),
            "schema_version = 1\nfont_pt = 10.0\n",
        )
        .expect("写老格式文件");
        let back = load(dir.path());
        assert!(back.note.is_none(), "老文件不该有 note：{:?}", back.note);
        assert_eq!(back.settings.log_level, LogLevel::Info);
    }

    /// 改过的档位要能存住。光有上一条的话，「读不出用户改过」这种错法全绿。
    #[test]
    fn a_changed_log_level_survives_a_round_trip() {
        let dir = tmp();
        for lv in [LogLevel::Error, LogLevel::Info, LogLevel::Debug] {
            let s = Settings {
                log_level: lv,
                ..Settings::default()
            };
            save(dir.path(), &s).expect("写盘");
            assert_eq!(load(dir.path()).settings.log_level, lv, "档位 {lv:?} 没存住");
        }
    }

    /// 手改成不认识的档位名不该让整份设置作废（那会连字体一起丢）。
    /// serde 解析失败会走 `load` 的降级分支 —— 这里钉的是「降级了要有 note」，
    /// 而不是「静默当成默认」。
    #[test]
    fn an_unknown_level_name_degrades_loudly_instead_of_silently() {
        let dir = tmp();
        std::fs::write(
            dir.path().join(SETTINGS_FILE),
            "schema_version = 1\nlog_level = \"verbose\"\n",
        )
        .expect("写文件");
        let back = load(dir.path());
        assert_eq!(back.settings.log_level, LogLevel::Info);
        assert!(back.note.is_some(), "档位名不认识却一声不吭");
    }

    /// 档位的磁盘写法是小写英文单词 —— 这是要被人手改的文件，形态本身是契约。
    #[test]
    fn levels_are_written_as_lowercase_words() {
        let s = Settings {
            log_level: LogLevel::Debug,
            ..Settings::default()
        };
        let text = toml::to_string_pretty(&s).expect("序列化");
        assert!(text.contains("log_level = \"debug\""), "写法变了：\n{text}");
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-store settings 2>&1 | tail -20
```
预期：编译失败，`cannot find type LogLevel in this scope`。

- [ ] **Step 3: 最小实现**

在 `crates/mullion-store/src/settings.rs` 的 `pub const SETTINGS_FILE` 之后插入：

```rust
/// 日志详细档位（F155）。
///
/// **只有三档**，不照搬 `log::LevelFilter` 的六档：`trace`/`off` 对用户没有
/// 可解释的含义（前者是给 crate 作者看的，后者等于「出了事没证据」），
/// 而多一个档就多一种「我到底该选哪个」的犹豫。
///
/// 这里**不认识 `log` crate** —— `mullion-store` 是零依赖方向的叶子
/// （见 `layout.rs` 那条架构守护）。映射成 `LevelFilter` 是 app 侧 `logx` 的事。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// 只记错误与降级。日志最小，但出了性能问题手上没有数据。
    Error,
    /// 默认：生命周期事件 + 每 5 秒一行性能剖面。
    Info,
    /// 上面全部，外加逐事件细节。给排查用，日志会大很多。
    Debug,
}

fn default_log_level() -> LogLevel {
    LogLevel::Info
}
```

在 `Settings` 结构体里，`tmux_bootstrap` 字段之后追加：

```rust
    /// F155：日志详细档位。环境变量 `MULLION_LOG` 存在时**覆盖**这里
    /// （排障时不必先进 GUI 改设置）。
    #[serde(default = "default_log_level")]
    pub log_level: LogLevel,
```

在 `impl Default for Settings` 里，`tmux_bootstrap: true,` 之后追加：

```rust
            log_level: LogLevel::Info,
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-store settings 2>&1 | tail -20
```
预期：`test result: ok`，其中包含上面 4 条新测试。

⚠️ 若 `settings_survive_a_round_trip` 与 `an_unknown_level_name_degrades_loudly_instead_of_silently` 之外的既有测试变红，说明 `Settings` 的字面量构造处漏了新字段——把 `crates/mullion-store/src/settings.rs:185` 那个字面量改成带 `..Settings::default()` 的形式。

- [ ] **Step 5: 导出类型**

在 `crates/mullion-store/src/lib.rs` 里找到导出 `Settings` 的那一行 `pub use`，把 `LogLevel` 加进同一个 `settings::{...}` 列表。

```bash
grep -n "settings::" crates/mullion-store/src/lib.rs
```

- [ ] **Step 6: 跑绿并提交**

```bash
cargo test -p mullion-store > /tmp/t1.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t1.log
cargo clippy -p mullion-store --all-targets -- -D warnings
git add crates/mullion-store/src/settings.rs crates/mullion-store/src/lib.rs
git commit -m "feat(store): 设置里加日志档位 error/info/debug，默认 info (F155)"
```

---

## Task 2: `profile.rs` — 对数桶直方图与分位数

**Files:**
- Create: `crates/mullion-app/src/profile.rs`
- Modify: `crates/mullion-app/src/lib.rs`

- [ ] **Step 1: 写文件（含失败的测试）**

创建 `crates/mullion-app/src/profile.rs`：

```rust
//! 性能剖面的**数据结构与纯函数**（F155）：对数桶直方图 + 分位数 + 剖面行渲染。
//!
//! **零 IO、零 UI、零 async**，可纯单测。采集端（`diag.rs`）只往这里做原子加法，
//! 格式化只发生在 5 秒周期线程里 —— 帧路径上做格式化就是 T3。
//!
//! 为什么是对数桶而不是存原始样本：一秒 60 帧、跑一整天是 500 万个样本，
//! 存下来算精确分位数要几十 MB 内存和一次排序。对数桶用 24 个 `AtomicU64`
//! （192 字节）换「相对误差不超过 2 倍」的分位数 —— 而找性能瓶颈要的是
//! 「p95 是 3ms 还是 300ms」，不是「是 3.1ms 还是 3.2ms」。

use std::sync::atomic::{AtomicU64, Ordering};

/// 桶数。桶 0 = 0µs；桶 k(k≥1) 覆盖 [2^(k-1), 2^k) µs。
/// 桶 23 = [4.2s, ∞)，比看门狗的停滞阈值（3s）还大一档，够用。
pub const BUCKETS: usize = 24;

/// 一个样本落进哪个桶。
///
/// 纯函数：桶边界是要被分位数换算反过来用的，两处各写一遍必然漂。
pub fn bucket_of(us: u64) -> usize {
    if us == 0 {
        return 0;
    }
    // 1 → 1, 2..3 → 2, 4..7 → 3 ...
    let b = 64 - us.leading_zeros() as usize;
    b.min(BUCKETS - 1)
}

/// 桶 `k` 的上界（µs）。分位数报的是这个 —— **报上界而不是下界**：
/// 「p95 不超过 X」是个能直接拿去做判断的说法，「p95 至少是 X」不是。
pub fn bucket_upper_us(k: usize) -> u64 {
    if k == 0 {
        0
    } else {
        1u64 << k.min(BUCKETS - 1)
    }
}

/// 一份取下来的计数快照。
pub type Counts = [u64; BUCKETS];

/// 分位数（µs）。`q` 取 0.0..=1.0。空直方图返回 0。
///
/// 用「向上取整的目标序号」而不是线性插值：桶本身就有 2 倍误差，
/// 在桶内插值是给精度加戏。
pub fn quantile_us(counts: &Counts, q: f64) -> u64 {
    let total: u64 = counts.iter().sum();
    if total == 0 {
        return 0;
    }
    let target = ((total as f64) * q).ceil().max(1.0) as u64;
    let mut acc = 0u64;
    for (k, c) in counts.iter().enumerate() {
        acc += c;
        if acc >= target {
            return bucket_upper_us(k);
        }
    }
    bucket_upper_us(BUCKETS - 1)
}

/// 直方图里一共记了多少个样本。
pub fn total(counts: &Counts) -> u64 {
    counts.iter().sum()
}

/// 可并发写入的直方图。写入端只做一次原子加法。
#[derive(Debug)]
pub struct Histogram {
    counts: [AtomicU64; BUCKETS],
}

impl Default for Histogram {
    fn default() -> Self {
        Self::new()
    }
}

impl Histogram {
    /// `const fn`：要能直接写成 `static`。
    pub const fn new() -> Self {
        // 内联 const（Rust 1.79+ 稳定）：数组的每个元素各自求值一次。
        // 不用 `const ZERO: AtomicU64 = ..;` 那种写法 —— 具名的内部可变常量
        // 会被 `clippy::declare_interior_mutable_const` 拦下，而那条 lint 是对的：
        // 具名 const 每次使用都会复制一份，很容易写出「以为在改同一个原子、
        // 其实各改各的副本」的 bug。内联 const 没有这个歧义。
        Self {
            counts: [const { AtomicU64::new(0) }; BUCKETS],
        }
    }

    /// 记一个样本。**只有一条 relaxed 原子加**，可以放在帧路径上。
    pub fn record_us(&self, us: u64) {
        self.counts[bucket_of(us)].fetch_add(1, Ordering::Relaxed);
    }

    /// 取走并清零。周期线程每 5 秒调一次 —— 剖面报的是**这 5 秒的窗口**，
    /// 不是自启动以来的累计：累计值会被开头那几秒的启动尖峰永久污染，
    /// 跑了一小时之后 p95 反映的是一小时前的事。
    ///
    /// 逐桶 swap 不是一次原子快照：中间可能插进来一次 `record_us`，
    /// 那个样本会落到下一个窗口。对统计无影响，不值得为它上锁。
    pub fn drain(&self) -> Counts {
        let mut out = [0u64; BUCKETS];
        for (k, c) in self.counts.iter().enumerate() {
            out[k] = c.swap(0, Ordering::Relaxed);
        }
        out
    }
}

/// µs → 便于阅读的字符串。剖面行里到处要用，统一在这里。
///
/// 1000µs 以下报 µs，以上报 ms（一位小数）—— 一行里混着两种量纲的裸数字
/// 是看不出问题的主要原因。
pub fn fmt_us(us: u64) -> String {
    if us < 1000 {
        format!("{us}us")
    } else {
        format!("{:.1}ms", us as f64 / 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 桶边界：每个桶覆盖上一个桶的两倍。钉死它是因为 `bucket_upper_us`
    /// 要按同一套边界反算回去，两边漂了分位数就是错的。
    #[test]
    fn buckets_double_and_the_upper_bound_agrees_with_them() {
        assert_eq!(bucket_of(0), 0);
        assert_eq!(bucket_of(1), 1);
        assert_eq!(bucket_of(2), 2);
        assert_eq!(bucket_of(3), 2);
        assert_eq!(bucket_of(4), 3);
        assert_eq!(bucket_of(7), 3);
        assert_eq!(bucket_of(8), 4);
        // 每个样本都必须落在自己桶的上界之内 —— 这条是 `quantile_us`
        // 「p95 不超过 X」这个说法能成立的全部依据。
        for us in [1u64, 2, 3, 5, 17, 999, 1000, 16_000, 999_999] {
            let k = bucket_of(us);
            assert!(
                us <= bucket_upper_us(k),
                "{us}µs 落进桶 {k}，但那个桶的上界是 {}",
                bucket_upper_us(k)
            );
        }
    }

    /// 极大值不许越界 panic —— 采样点里有 `Instant` 差值，时钟异常时会出现
    /// 荒谬的大数，而剖面绝不能反过来把程序打死。
    #[test]
    fn an_absurdly_large_sample_saturates_into_the_last_bucket() {
        assert_eq!(bucket_of(u64::MAX), BUCKETS - 1);
        assert_eq!(bucket_of(1 << 40), BUCKETS - 1);
    }

    #[test]
    fn an_empty_histogram_reports_zero_rather_than_panicking() {
        let h = Histogram::new();
        let c = h.drain();
        assert_eq!(total(&c), 0);
        assert_eq!(quantile_us(&c, 0.5), 0);
        assert_eq!(quantile_us(&c, 0.95), 0);
    }

    /// 分位数的核心：绝大多数样本很快、偶尔卡一下时，p50 要还在微秒档，
    /// 而那个慢样本必须在尾部露头 —— 这正是本切片要回答的问题形态。
    /// 只报平均值的话，那个 1 秒会被摊成 10ms，看起来毫无异常。
    ///
    /// **100 个样本里唯一的那个慢样本不出现在 p95 上，这不是 bug**：按
    /// nearest-rank 定义，「95% 的样本不超过 X」在 100 个样本里只需数到第
    /// 95 名，那一名确实是快的。孤例靠 max 兜住 —— 剖面行同时报 p50/p95/max
    /// 就是为了这个，少报 max 的话「一小时里卡了那么一下」会彻底消失。
    #[test]
    fn a_lone_slow_sample_hides_from_p95_but_never_from_max() {
        let h = Histogram::new();
        for _ in 0..99 {
            h.record_us(1);
        }
        h.record_us(1_000_000);
        let c = h.drain();
        assert_eq!(total(&c), 100);
        assert!(quantile_us(&c, 0.5) <= 2, "p50 该还在微秒档");
        assert!(
            quantile_us(&c, 0.95) <= 2,
            "百里挑一的慢样本按定义不该抬高 p95"
        );
        assert!(
            quantile_us(&c, 1.0) >= 1_000_000,
            "max 没抓到那个一秒的样本 —— 孤例卡顿就是这么在剖面里消失的"
        );
    }

    /// 目标序号**向上取整**（nearest-rank）：10 个样本里有 1 个慢的，p95 的
    /// 序号是 `ceil(0.95 * 10) = 10`，必须把那个慢样本算进来。
    ///
    /// 向下取整会得到 9，正好把它排除在外 —— 于是「十次里有一次很慢」
    /// 在剖面里完全看不见，而那恰恰是高延迟链路上最该被看见的形态。
    ///
    /// 自证会变红：把 `quantile_us` 里的 `.ceil()` 改成 `.floor()`。
    ///
    /// **注意**：别把这条写成「99 快 + 1 慢、断言 p99 抓到慢的」——
    /// `100.0 * 0.99` 精确等于 99.0，ceil 与 floor 结果相同，那样的自证是假的
    /// （计划初稿在这里错过一次）。
    #[test]
    fn the_rank_rounds_up_so_a_tenth_of_the_samples_cannot_hide_from_p95() {
        let h = Histogram::new();
        for _ in 0..9 {
            h.record_us(1);
        }
        h.record_us(1_000_000);
        let c = h.drain();
        assert_eq!(total(&c), 10);
        assert!(
            quantile_us(&c, 0.95) >= 1_000_000,
            "序号向下取整了 —— 十分之一的慢样本被排除在 p95 之外"
        );
    }

    /// `drain` 是**取走**，不是读取。不清零的话剖面报的是自启动以来的累计，
    /// 跑一小时之后 p95 反映的还是启动那几秒的尖峰。
    ///
    /// 自证会变红：把 `drain` 里的 `swap(0, ..)` 改成 `load(..)`。
    #[test]
    fn draining_resets_the_window_so_old_spikes_do_not_haunt_it() {
        let h = Histogram::new();
        h.record_us(500_000);
        assert_eq!(total(&h.drain()), 1);
        assert_eq!(total(&h.drain()), 0, "取走之后窗口该是空的");
        h.record_us(1);
        let c = h.drain();
        assert_eq!(total(&c), 1);
        assert!(quantile_us(&c, 1.0) <= 2, "上个窗口的尖峰漏到这个窗口来了");
    }

    /// 一行里混着两种量纲的裸数字是看不出问题的主要原因，所以带单位。
    #[test]
    fn durations_carry_their_unit() {
        assert_eq!(fmt_us(0), "0us");
        assert_eq!(fmt_us(999), "999us");
        assert_eq!(fmt_us(1_000), "1.0ms");
        assert_eq!(fmt_us(16_500), "16.5ms");
    }
}
```

- [ ] **Step 2: 挂上模块**

在 `crates/mullion-app/src/lib.rs` 里，按既有 `pub mod` 的字母顺序位置插入：

```rust
pub mod profile;
```

- [ ] **Step 3: 跑测试**

```bash
cargo test -p mullion-app --lib profile:: 2>&1 | tail -20
```
预期：7 条测试全过。

- [ ] **Step 4: clippy 并提交**

```bash
cargo clippy -p mullion-app --all-targets -- -D warnings
git add crates/mullion-app/src/profile.rs crates/mullion-app/src/lib.rs
git commit -m "feat(app): 对数桶直方图与分位数，供性能剖面用 (F155)"
```

---

## Task 3: `diag.rs` 时基改微秒，`mark` 顺手累计阶段耗时

**Files:**
- Modify: `crates/mullion-app/src/diag.rs`

这是本切片**收益最高的一步**：`mark()` 已铺满事件循环，改完之后逐阶段耗时分布是白得的。

> 🔴 **已完成（commit `9f5c9e2`），实际落地形态与本节下文不同。后续 task 以这里为准：**
>
> 本节原稿把状态放在三个裸 `static`（`STAGE` / `BEAT_US` / `STAGE_US`），并试图用
> 一把 `TEST_LOCK` 让测试串行。**这条路走死了**：`host_key::tests::normal_connection_accept_persists`
> 也会经由 `persist_if_allowed()` 调到 `diag::mark()`，它不持锁，加锁只能打地鼠。
>
> 最终改成把状态收进一个结构体，**测试自己造一个实例**，时间当常量传进去 ——
> 不 sleep、不加锁、不碰全局量：
>
> ```rust
> struct StageClock {
>     stage: AtomicU8,
>     beat_us: AtomicU64,
>     hist: [crate::profile::Histogram; STAGE_COUNT],
> }
> impl StageClock {
>     const fn new() -> Self { /* 内联 const 初始化数组 */ }
>     fn mark(&self, stage: Stage, now_us: u64) { /* swap prev/since，记进 hist[prev] */ }
> }
> static CLOCK: StageClock = StageClock::new();
> ```
>
> 同时确定的几件事：
> - `pub const STAGE_COUNT: usize = 12;` 与 `pub fn stage_name(raw: u8) -> &'static str` 是公开的，`profile.rs` 用它们。
> - **`elapsed_ms()` 已彻底删除**，只留 `elapsed_us()`。留着两个时基就是在等「单位写错」这类 bug。
> - 看门狗的毫秒换算抽成了纯函数 `pub fn stuck_ms(now_us: u64, beat_us: u64) -> u64`（少了那个 `/1000`，看门狗会**静默永久失效**，所以它必须单独可测）。
> - 没有 `TEST_LOCK`、没有 `reset_for_test`。
>
> 下文原稿保留作为设计过程记录，**代码片段已过时，不要照抄**。

- [ ] **Step 1: 写失败的测试**

> ⚠️ **执行时实测到的坑**：这几条测试碰的是进程级 `static`，而 `mark()` 每次都要 swap
> `STAGE` 与 `BEAT_US` 两个**全局标量**——任何一条测试在另一条 sleep 期间调用 `mark()`，
> 都会把对方的 prev/since 这一对偷走。干扰**不走各自的桶**（桶按阶段分开），所以「按阶段
> 错开就不用串行」是错的（计划初稿这么写过），只能上锁。

在 `crates/mullion-app/src/diag.rs` 的 `mod tests` 末尾追加：

```rust
    /// 这几条测试碰的是**进程级 `static`**：`mark()` 每次都要 swap `STAGE`
    /// 与 `BEAT_US` 这两个全局标量，任何一条测试在另一条 sleep 期间调用
    /// `mark()`，都会把对方的 prev/since 这一对偷走。干扰不在各自的桶上
    /// （桶是按阶段分开的），在这两个标量上 —— 所以按阶段错开没用，只能串行。
    ///
    /// 用 crate 内的 `Mutex` 而不是引入 `serial_test`：只有两条测试需要它，
    /// 为此加一个测试专用依赖不划算。
    ///
    /// 中毒容忍（`into_inner`）：一条测试断言失败会带着锁 panic，此时别让
    /// 后面的测试全部退化成 `PoisonError` —— 那会把「一条真失败」显示成
    /// 「一片失败」，真正的原因反而被埋掉。
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_globals() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `mark` 换阶段时必须把**上一个阶段**待了多久记进它自己的直方图。
    ///
    /// 这是整份剖面的地基：事件循环里已经铺满 `mark()`，累计这一步做对了，
    /// 「pump 慢还是 present 慢」就不需要任何新插桩点。做错了（比如记进
    /// 新阶段而不是旧阶段）不会有任何报错，只会让剖面**一直指错人**。
    ///
    /// 这一条同时守住**时基**：睡了 2ms，微秒基下记的是 ~2000，毫秒基下
    /// 记的是 2 —— 差三个数量级，所以不需要另立一条脆弱的分辨率测试
    /// （`sleep(200µs)` 在负载下会睡过 1ms，那种上界断言会随机变红；
    /// 计划初稿写过那么一条，已删）。
    ///
    /// 自证会变红：把 `mark` 里的 `prev` 换成 `stage as u8`；
    /// 或把 `elapsed_us` 的 `as_micros()` 改回 `as_millis()`。
    #[test]
    fn marking_a_new_stage_charges_the_elapsed_time_to_the_stage_that_just_ended() {
        let _guard = lock_globals();
        reset_for_test();
        mark(Stage::Pump);
        std::thread::sleep(Duration::from_millis(2));
        mark(Stage::Present);

        let pump = STAGE_US[Stage::Pump as usize].drain();
        let present = STAGE_US[Stage::Present as usize].drain();
        assert_eq!(
            crate::profile::total(&pump),
            1,
            "离开 Pump 时没把它这一趟的耗时记下来"
        );
        assert!(
            crate::profile::quantile_us(&pump, 1.0) >= 1_000,
            "睡了 2ms 却只记下 {}µs —— 时基退回毫秒了（毫秒基下这里会是个位数）",
            crate::profile::quantile_us(&pump, 1.0)
        );
        assert_eq!(
            crate::profile::total(&present),
            0,
            "耗时被记到了刚进入的阶段头上 —— 剖面会一直指错人"
        );
    }

    /// `Idle` 的时长**不进直方图**。阻塞等事件本来就可以很久，把它算进去
    /// 会让「事件循环耗时」这一行永远被一个几十秒的样本主导，其余全部
    /// 淹没在噪声里。
    ///
    /// 自证会变红：把 `mark` 里那句跳过 Idle 的判断删掉。
    #[test]
    fn time_spent_idle_is_not_charged_to_anything() {
        let _guard = lock_globals();
        reset_for_test();
        mark(Stage::Idle);
        std::thread::sleep(Duration::from_millis(2));
        mark(Stage::UserEvent);
        assert_eq!(
            crate::profile::total(&STAGE_US[Stage::Idle as usize].drain()),
            0,
            "空闲等待被当成了耗时"
        );
    }

```

同时在 `mod tests` 里加一个只给测试用的复位函数（放在 `use super::*;` 之后）：

```rust
    /// 复位进程级计数器。**必须在持有 `TEST_LOCK` 时调用**，否则复位本身
    /// 就会和别的测试的 `mark()` 打架。
    fn reset_for_test() {
        let _ = ORIGIN.set(Instant::now());
        for h in &STAGE_US {
            let _ = h.drain();
        }
        STAGE.store(Stage::Idle as u8, Ordering::Relaxed);
        BEAT_US.store(elapsed_us(), Ordering::Relaxed);
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib diag:: 2>&1 | tail -20
```
预期：编译失败，`cannot find value STAGE_US` / `BEAT_US` / `elapsed_us`。

- [ ] **Step 3: 实现**

在 `crates/mullion-app/src/diag.rs` 里做四处改动。

3a. 把 `BEAT_MS` 那一组 `static` 换成微秒版，并加上逐阶段直方图。找到：

```rust
static STAGE: AtomicU8 = AtomicU8::new(Stage::Idle as u8);
static BEAT_MS: AtomicU64 = AtomicU64::new(0);
```

替换为：

```rust
static STAGE: AtomicU8 = AtomicU8::new(Stage::Idle as u8);
/// 上一次 `mark` 的时刻（µs）。**微秒**：毫秒精度下单帧各阶段几乎全是 0，
/// 剖面看着像「哪里都不慢」，瓶颈藏在被截断的小数里。
static BEAT_US: AtomicU64 = AtomicU64::new(0);
/// 每个阶段的驻留时长分布。索引 = `Stage as usize`。
///
/// **白得的**：`mark()` 本来就铺满了事件循环，离开一个阶段时顺手把这一趟的
/// 时长记进去，就有了「pump 慢还是 present 慢」的答案，不需要任何新插桩点。
static STAGE_US: [crate::profile::Histogram; STAGE_NAMES.len()] =
    [const { crate::profile::Histogram::new() }; STAGE_NAMES.len()];
```

> 注：`[const { .. }; N]` 的内联 const 数组构造在 rustc 1.79+ 稳定，本项目用 1.96，可用。
> **不要**回落成具名 const（`const H: Histogram = Histogram::new();` 再 `[H; N]`）——
> `clippy::declare_interior_mutable_const` 会在 `-D warnings` 下把它拦下，Task 2 实测撞过。

3b. 把 `elapsed_ms` 换成 `elapsed_us`，并保留一个毫秒的便捷读法给看门狗用。找到：

```rust
fn elapsed_ms() -> u64 {
    ORIGIN.get().map_or(0, |o| o.elapsed().as_millis() as u64)
}
```

替换为：

```rust
fn elapsed_us() -> u64 {
    ORIGIN.get().map_or(0, |o| o.elapsed().as_micros() as u64)
}

fn elapsed_ms() -> u64 {
    elapsed_us() / 1000
}
```

3c. 改 `mark`。找到：

```rust
pub fn mark(stage: Stage) {
    STAGE.store(stage as u8, Ordering::Relaxed);
    BEAT_MS.store(elapsed_ms(), Ordering::Relaxed);
}
```

替换为：

```rift
```
（下面是实际代码，勿照抄上面这行）

```rust
/// 进入某个阶段，并把**刚刚结束的那个阶段**的驻留时长记进直方图。
///
/// 开销：一次 `Instant::elapsed` + 四条 relaxed 原子操作 + 一次原子加法。
/// Windows 上 `Instant::now` 走 QPC，约 20~30ns —— 帧路径上可以忽略，
/// 但**绝不能**在这里做格式化或加锁（T3）。
///
/// `Idle` 不计时：阻塞等事件本来就可以很久，算进去会让直方图被一个几十秒的
/// 样本主导，其余全淹掉。
///
/// **单写者**：生产环境里只有主线程（事件循环）调用本函数，看门狗线程只读。
/// 两次 `swap` 因此不需要凑成一次原子快照 —— 多个线程并发调用会互相偷走
/// prev/since 这一对（测试里就是这么撞上的，见 `tests::TEST_LOCK`）。
pub fn mark(stage: Stage) {
    let now = elapsed_us();
    let prev = STAGE.swap(stage as u8, Ordering::Relaxed);
    let since = BEAT_US.swap(now, Ordering::Relaxed);
    if prev != Stage::Idle as u8 {
        if let Some(h) = STAGE_US.get(prev as usize) {
            h.record_us(now.saturating_sub(since));
        }
    }
}
```

3d. 看门狗里的 `stuck` 计算改用微秒源。找到 `watchdog_loop` 里：

```rust
        let now = elapsed_ms();
        let stage = STAGE.load(Ordering::Relaxed);
        let stuck = now.saturating_sub(BEAT_MS.load(Ordering::Relaxed));
```

替换为：

```rust
        let now = elapsed_ms();
        let stage = STAGE.load(Ordering::Relaxed);
        let stuck = now.saturating_sub(BEAT_US.load(Ordering::Relaxed) / 1000);
```

同一函数里 `BEAT_MS.store(0, Ordering::Relaxed);`（在 `start_watchdog` 中）改成：

```rust
    BEAT_US.store(0, Ordering::Relaxed);
```

- [ ] **Step 4: 跑测试确认通过**

```bash
for i in 1 2 3 4 5; do cargo test -p mullion-app --lib diag:: 2>&1 | grep -E "test result"; done
```
预期：**默认并行**下连跑 5 遍都是 6 passed（既有 4 条 + 新增 2 条）。既有的
`does_not_report_below_threshold` / `repeats_only_on_doubling` 是纯函数测试，不受影响。
出现任何一次 flaky 都不许加 sleep 掩盖——那是设计信号。

- [ ] **Step 5: 提交**

```bash
cargo clippy -p mullion-app --all-targets -- -D warnings
git add crates/mullion-app/src/diag.rs
git commit -m "feat(app): diag 时基改微秒，mark 顺手累计上一阶段耗时 (F155)

事件循环里已铺满 mark()，累计这一步做完，逐阶段耗时分布不需要任何新插桩点。
守护测试 diag::tests::marking_a_new_stage_charges_the_elapsed_time_to_the_stage_that_just_ended"
```

---

## Task 4: 剖面行渲染 + 无活动跳过

**Files:**
- Modify: `crates/mullion-app/src/profile.rs`（加 `Snapshot` 与 `render_line`）
- Modify: `crates/mullion-app/src/diag.rs`（周期线程改用它）

> 🔴 **已完成（commit `1b04389` + 复核修正）。与本节原稿的差异，后续 task 以这里为准：**
>
> - `take_snapshot` 里读的是 `CLOCK.hist[k].drain()`，**不是**原稿的 `STAGE_US[k]`（见 Task 3 的说明）。
> - `take_snapshot` **不在** `log_enabled!(Info)` 门里。原稿把它包进去了，那会让同一批
>   `static` 在不同档位下含义不同（info 档=「近 5 秒」，error 档=「自启动累计」），
>   而 `watchdog_loop` 的停滞报警行读的正是它们。现在是无条件 drain，只有
>   `render_line` 的格式化关在门里。停滞报警行的文案相应加了「近5s内」。
> - `is_idle` 的活动判据除了 `frames`/`inbound_bytes`/`keys`/连接/SFTP，还包含
>   `throttled` 与三个 `redraw_*` —— 「重绘全被帧闸挡下、一帧没画成」的窗口正是
>   T3 发作的样子，判成空闲就等于把要查的指标 drain 掉又不打印。
>   **`stage_us` 有意不算活动**：无关定时器唤醒事件循环也会留下阶段样本。
> - 剖面行里回显那段是 `key={}x/echo={}x/p95={}`（带样本数），**不是**原稿的
>   `echo_p95={}` —— 空直方图的 p95 是 0，与「真的量到 0µs」无法区分。
>
> 下文原稿的代码片段除上述几点外仍然成立。

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/profile.rs` 的 `mod tests` 末尾追加：

```rust
    fn busy_snapshot() -> Snapshot {
        let mut s = Snapshot {
            window_ms: 5_000,
            frames: 300,
            presents: 298,
            skipped: 2,
            inbound_bytes: 1_024_000,
            redraw_terminal: 200,
            redraw_ui: 80,
            redraw_both: 20,
            throttled: 40,
            keys: 12,
            echo_us: [0; BUCKETS],
            frame_us: [0; BUCKETS],
            stage_us: Default::default(),
            connects_ok: 1,
            connects_err: 0,
            reconnects: 0,
            sftp_ops: 3,
            tabs: 2,
            panes: 3,
            hosts: 2,
            mem_process_mb: 180,
        };
        s.frame_us[bucket_of(8_000)] = 300;
        s
    }

    /// 空窗口（一帧没画、一个字节没收）**不该产出一行**。
    ///
    /// 没有这条，笔记本合盖前挂着的 mullion 会每 5 秒写一次盘，硬盘永远
    /// 睡不下去 —— 这正是布局落盘（E7）已经踩过一次的坑。
    ///
    /// 自证会变红：把 `Snapshot::is_idle` 的函数体改成 `false`。
    #[test]
    fn an_idle_window_produces_no_line_at_all() {
        let idle = Snapshot {
            window_ms: 5_000,
            ..Snapshot::empty()
        };
        assert!(idle.is_idle());
        assert!(render_line(&idle).is_none(), "空闲窗口不该写盘");
        assert!(!busy_snapshot().is_idle());
        assert!(render_line(&busy_snapshot()).is_some());
    }

    /// 一行里必须同时有「多快」和「慢在哪一段」。只报总帧耗时的话，
    /// 看到 p95=200ms 也说不出该去优化 pump 还是 present。
    #[test]
    fn the_line_carries_both_the_frame_time_and_the_per_stage_breakdown() {
        let mut s = busy_snapshot();
        s.stage_us[crate::diag::Stage::Pump as usize][bucket_of(50_000)] = 100;
        let line = render_line(&s).expect("忙窗口该有一行");
        assert!(line.contains("frame"), "没报帧耗时：{line}");
        assert!(line.contains("p95"), "没报分位数：{line}");
        assert!(line.contains("pump="), "没报阶段耗时：{line}");
    }

    /// 剖面行是给人扫的，一行里的数字必须带单位与量纲；而且**不能换行**
    /// —— 日志按行 grep，一条记录跨行就没法用 `grep 'profile'` 拉出时间序列。
    #[test]
    fn the_line_is_single_line_and_units_are_explicit() {
        let line = render_line(&busy_snapshot()).expect("忙窗口该有一行");
        assert!(!line.contains('\n'), "剖面行不许换行：{line}");
        assert!(line.contains("KB/s") || line.contains("B/s"), "吞吐没带单位：{line}");
    }

    /// 跳帧数为 0 时也要**显式写出来**。省略掉的话，「这个窗口没跳帧」与
    /// 「这个版本忘了统计跳帧」在日志里长得一模一样。
    #[test]
    fn a_zero_count_is_printed_rather_than_omitted() {
        let mut s = busy_snapshot();
        s.skipped = 0;
        let line = render_line(&s).expect("忙窗口该有一行");
        assert!(line.contains("skip=0"), "零值被省略了：{line}");
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib profile:: 2>&1 | tail -20
```
预期：`cannot find struct Snapshot`。

- [ ] **Step 3: 实现**

在 `crates/mullion-app/src/profile.rs` 的 `fmt_us` 之后追加：

```rust
/// 一个 5 秒窗口里采到的全部东西。
///
/// **纯数据**：由 `diag.rs` 的周期线程从各个原子计数器 drain 出来填好，
/// 再交给 [`render_line`]。分成两步是为了让「这一行长什么样」可以脱离
/// 线程、时钟和全局状态单测 —— 剖面行本身是要被人读的产物，格式错了
/// （单位漏了、零值省略了、跨行了）编译器一句话都不会说。
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// 这个窗口有多长（ms）。周期线程受调度影响不会正好 5000。
    pub window_ms: u64,
    pub frames: u64,
    pub presents: u64,
    pub skipped: u64,
    pub inbound_bytes: u64,
    /// 重绘触发原因（F155）：只有终端来了字节 / 只有 egui 要重绘 / 两者都有。
    pub redraw_terminal: u64,
    pub redraw_ui: u64,
    pub redraw_both: u64,
    /// 被帧闸挡下的重绘次数（T3 的直接体感指标）。
    pub throttled: u64,
    /// 按键次数。
    pub keys: u64,
    /// 「按键 → 下一段入站字节抵达」的间隔分布。**是回显往返的近似**，
    /// 见 `diag::note_key` 的说明。
    pub echo_us: Counts,
    /// 整帧耗时分布。
    pub frame_us: Counts,
    /// 逐阶段驻留时长分布，索引 = `diag::Stage as usize`。
    pub stage_us: StageCounts,
    pub connects_ok: u64,
    pub connects_err: u64,
    pub reconnects: u64,
    pub sftp_ops: u64,
    pub tabs: u64,
    pub panes: u64,
    pub hosts: u64,
    pub mem_process_mb: u64,
}

/// 逐阶段计数。长度与 `diag::Stage` 的变体数一致。
pub type StageCounts = [Counts; crate::diag::STAGE_COUNT];

impl Snapshot {
    /// 一份全零的快照。
    pub fn empty() -> Self {
        Self {
            window_ms: 0,
            frames: 0,
            presents: 0,
            skipped: 0,
            inbound_bytes: 0,
            redraw_terminal: 0,
            redraw_ui: 0,
            redraw_both: 0,
            throttled: 0,
            keys: 0,
            echo_us: [0; BUCKETS],
            frame_us: [0; BUCKETS],
            stage_us: [[0; BUCKETS]; crate::diag::STAGE_COUNT],
            connects_ok: 0,
            connects_err: 0,
            reconnects: 0,
            sftp_ops: 0,
            tabs: 0,
            panes: 0,
            hosts: 0,
            mem_process_mb: 0,
        }
    }

    /// 这个窗口里什么都没发生。
    ///
    /// 判据只看「有没有画过帧 / 有没有收过字节 / 有没有连接动作」——
    /// 标签数、内存这类**状态量**在空闲时也非零，拿它们判活会让空闲的
    /// mullion 每 5 秒写一次盘。
    pub fn is_idle(&self) -> bool {
        self.frames == 0
            && self.inbound_bytes == 0
            && self.keys == 0
            && self.connects_ok == 0
            && self.connects_err == 0
            && self.reconnects == 0
            && self.sftp_ops == 0
    }
}

/// 把一个窗口渲染成**一行**日志。`None` = 这个窗口空闲，不该写。
///
/// 单行是硬要求：日志按行 grep，一条记录跨行就没法用
/// `grep profile mullion.log` 拉出时间序列。
pub fn render_line(s: &Snapshot) -> Option<String> {
    if s.is_idle() {
        return None;
    }
    let secs = (s.window_ms as f64 / 1000.0).max(0.001);
    let bps = s.inbound_bytes as f64 / secs;
    let rate = if bps >= 1024.0 {
        format!("{:.1}KB/s", bps / 1024.0)
    } else {
        format!("{bps:.0}B/s")
    };
    // 阶段按耗时倒序取前四段 —— 全列出来一行有十二段，人眼扫不动，
    // 而排在后面的那些恒定是零。
    let mut stages: Vec<(usize, u64, u64)> = s
        .stage_us
        .iter()
        .enumerate()
        .map(|(k, c)| (k, total(c), quantile_us(c, 0.95)))
        .filter(|(_, n, _)| *n > 0)
        .collect();
    stages.sort_by_key(|(_, n, p95)| std::cmp::Reverse(n.saturating_mul(*p95)));
    stages.truncate(4);
    let stage_part = stages
        .iter()
        .map(|(k, n, p95)| {
            format!(
                "{}={}x/p95={}",
                crate::diag::stage_name(*k as u8),
                n,
                fmt_us(*p95)
            )
        })
        .collect::<Vec<_>>()
        .join(" ");

    Some(format!(
        "profile {:.1}s frame={}x/p50={}/p95={}/max={} present={} skip={} throttle={} \
         redraw=term:{}/ui:{}/both:{} in={} key={}x/echo_p95={} {} \
         conn=ok:{}/err:{}/re:{} sftp={} tabs={} panes={} hosts={} mem={}MB",
        secs,
        s.frames,
        fmt_us(quantile_us(&s.frame_us, 0.5)),
        fmt_us(quantile_us(&s.frame_us, 0.95)),
        fmt_us(quantile_us(&s.frame_us, 1.0)),
        s.presents,
        s.skipped,
        s.throttled,
        s.redraw_terminal,
        s.redraw_ui,
        s.redraw_both,
        rate,
        s.keys,
        fmt_us(quantile_us(&s.echo_us, 0.95)),
        stage_part,
        s.connects_ok,
        s.connects_err,
        s.reconnects,
        s.sftp_ops,
        s.tabs,
        s.panes,
        s.hosts,
        s.mem_process_mb,
    ))
}
```

在 `crates/mullion-app/src/diag.rs` 里，把 `STAGE_NAMES` 的长度导出成公开常量（`profile.rs` 要用），并把 `stage_name` 改成公开。找到：

```rust
const STAGE_NAMES: [&str; 12] = [
```
改为：
```rust
/// `Stage` 一共有几个变体。`profile::StageCounts` 的长度用它。
pub const STAGE_COUNT: usize = 12;

const STAGE_NAMES: [&str; STAGE_COUNT] = [
```

并把：
```rust
fn stage_name(raw: u8) -> &'static str {
```
改为：
```rust
pub fn stage_name(raw: u8) -> &'static str {
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib profile:: 2>&1 | tail -20
```
预期：12 条全过。

- [ ] **Step 5: 周期线程改用剖面行**

在 `crates/mullion-app/src/diag.rs` 的 `watchdog_loop` 里，把整个 `if now.saturating_sub(last_metrics) >= METRICS_EVERY_MS { .. }` 块替换为：

```rust
        if now.saturating_sub(last_metrics) >= METRICS_EVERY_MS {
            let window_ms = now.saturating_sub(last_metrics);
            last_metrics = now;
            // **info 级**：这一行就是本切片存在的理由，默认档位下必须有。
            // 逐事件细节仍在 debug 级（见 `logx` 的档位映射）。
            if log::log_enabled!(target: "mullion", log::Level::Info) {
                if let Some(line) = crate::profile::render_line(&take_snapshot(window_ms)) {
                    log::info!(target: "mullion", "{line}");
                }
            }
        }
```

并在 `watchdog_loop` 之后加上采集函数：

```rust
/// 把这一窗口的所有计数器**取走**，凑成一份快照。
///
/// 全部是 `swap(0)` / `drain()`：剖面报的是「这 5 秒」，不是自启动以来的累计
/// —— 累计值会被启动那几秒的尖峰永久污染（见 `profile::Histogram::drain`）。
fn take_snapshot(window_ms: u64) -> crate::profile::Snapshot {
    let mut s = crate::profile::Snapshot::empty();
    s.window_ms = window_ms;
    s.frames = FRAMES.swap(0, Ordering::Relaxed);
    s.presents = PRESENTS.swap(0, Ordering::Relaxed);
    s.skipped = SKIPPED.swap(0, Ordering::Relaxed);
    s.inbound_bytes = INBOUND_BYTES.swap(0, Ordering::Relaxed);
    s.redraw_terminal = REDRAW_TERMINAL.swap(0, Ordering::Relaxed);
    s.redraw_ui = REDRAW_UI.swap(0, Ordering::Relaxed);
    s.redraw_both = REDRAW_BOTH.swap(0, Ordering::Relaxed);
    s.throttled = THROTTLED.swap(0, Ordering::Relaxed);
    s.keys = KEYS.swap(0, Ordering::Relaxed);
    s.echo_us = ECHO_US.drain();
    s.frame_us = FRAME_US.drain();
    for (k, h) in STAGE_US.iter().enumerate() {
        s.stage_us[k] = h.drain();
    }
    s.connects_ok = CONNECTS_OK.swap(0, Ordering::Relaxed);
    s.connects_err = CONNECTS_ERR.swap(0, Ordering::Relaxed);
    s.reconnects = RECONNECTS.swap(0, Ordering::Relaxed);
    s.sftp_ops = SFTP_OPS.swap(0, Ordering::Relaxed);
    // 状态量：读而不清 —— 它们描述的是「此刻有几个标签」，不是「这 5 秒发生了几次」。
    s.tabs = TABS.load(Ordering::Relaxed);
    s.panes = PANES.load(Ordering::Relaxed);
    s.hosts = HOSTS.load(Ordering::Relaxed);
    s.mem_process_mb = sample_memory().map_or(0, |m| m.process_bytes / (1024 * 1024));
    s
}
```

在 `static INBOUND_BYTES` 之后补上新计数器与采集入口：

```rust
static REDRAW_TERMINAL: AtomicU64 = AtomicU64::new(0);
static REDRAW_UI: AtomicU64 = AtomicU64::new(0);
static REDRAW_BOTH: AtomicU64 = AtomicU64::new(0);
static THROTTLED: AtomicU64 = AtomicU64::new(0);
static KEYS: AtomicU64 = AtomicU64::new(0);
static CONNECTS_OK: AtomicU64 = AtomicU64::new(0);
static CONNECTS_ERR: AtomicU64 = AtomicU64::new(0);
static RECONNECTS: AtomicU64 = AtomicU64::new(0);
static SFTP_OPS: AtomicU64 = AtomicU64::new(0);
/// 状态量（此刻是多少），不是「这窗口发生了几次」—— 采集时读而不清。
static TABS: AtomicU64 = AtomicU64::new(0);
static PANES: AtomicU64 = AtomicU64::new(0);
static HOSTS: AtomicU64 = AtomicU64::new(0);

static FRAME_US: crate::profile::Histogram = crate::profile::Histogram::new();
static ECHO_US: crate::profile::Histogram = crate::profile::Histogram::new();
/// 最后一次按键的时刻（µs）。0 = 还没按过 / 已被下一段入站字节消费掉。
static LAST_KEY_US: AtomicU64 = AtomicU64::new(0);

/// 整帧耗时（从 redraw 入口到 present 结束）。
pub fn record_frame_us(us: u64) {
    FRAME_US.record_us(us);
}

/// 这一帧的重绘是被谁触发的（F155）。三类分开计，才看得出「远端安静时
/// egui 还在每秒要几十次重绘」这种白烧 GPU 的情况。
pub fn count_redraw(terminal: bool, ui: bool) {
    match (terminal, ui) {
        (true, true) => &REDRAW_BOTH,
        (true, false) => &REDRAW_TERMINAL,
        (false, true) => &REDRAW_UI,
        // 两边都不脏时事件循环不会走到这里；真走到了也不该计进任何一类。
        (false, false) => return,
    }
    .fetch_add(1, Ordering::Relaxed);
}

/// 一次重绘被帧闸挡下（T3 的直接体感指标）。
pub fn count_throttled() {
    THROTTLED.fetch_add(1, Ordering::Relaxed);
}

/// 用户按下了一个会发往远端的键。记下时刻，等下一段入站字节来时算回显往返。
///
/// **这是近似**：连续打字时，第 N 个键的回显可能在第 N+1 个键之后才到，
/// 那样量到的是一个偏小的值；反过来，远端自己吐的输出（比如 `top` 刷新）
/// 也会被当成回显，量到一个偏小的值。它回答不了「精确延迟是多少」，
/// 能回答的是「这条链路的回显是十毫秒级还是几百毫秒级」——而后者正是
/// 高延迟代理链路上要看的量级。精确做法要给每个按键打序号并等它原样回来，
/// 那需要在 VT 层做匹配，是另一片的工作量。
pub fn note_key() {
    KEYS.fetch_add(1, Ordering::Relaxed);
    LAST_KEY_US.store(elapsed_us(), Ordering::Relaxed);
}

/// 有入站字节抵达。若在此之前有一次未被消费的按键，记一次回显往返。
pub fn note_inbound_for_echo() {
    let key_us = LAST_KEY_US.swap(0, Ordering::Relaxed);
    if key_us > 0 {
        ECHO_US.record_us(elapsed_us().saturating_sub(key_us));
    }
}

pub fn count_connect(ok: bool) {
    if ok {
        CONNECTS_OK.fetch_add(1, Ordering::Relaxed);
    } else {
        CONNECTS_ERR.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn count_reconnect() {
    RECONNECTS.fetch_add(1, Ordering::Relaxed);
}

pub fn count_sftp_op() {
    SFTP_OPS.fetch_add(1, Ordering::Relaxed);
}

/// 此刻的规模。App 每帧调一次（三条 relaxed 原子存，可忽略）。
pub fn set_scale(tabs: usize, panes: usize, hosts: usize) {
    TABS.store(tabs as u64, Ordering::Relaxed);
    PANES.store(panes as u64, Ordering::Relaxed);
    HOSTS.store(hosts as u64, Ordering::Relaxed);
}
```

- [ ] **Step 6: 跑绿并提交**

```bash
cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked"
cargo clippy -p mullion-app --all-targets -- -D warnings
git add crates/mullion-app/src/profile.rs crates/mullion-app/src/diag.rs
git commit -m "feat(app): 5 秒周期剖面行 + 空闲窗口不落盘 (F155)

守护测试 profile::tests::an_idle_window_produces_no_line_at_all
（没有它，合盖挂着的 mullion 每 5 秒写一次盘，硬盘睡不下去 —— E7 踩过的同一个坑）"
```

---

## Task 5: `logx` 的级别来源改为「设置 + 环境变量覆盖」

**Files:**
- Modify: `crates/mullion-app/src/logx.rs`

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/logx.rs` 的 `mod tests` 末尾追加：

```rust
    use mullion_store::LogLevel;

    /// 设置里的档位映射成两个 `LevelFilter`：自家 crate 一个、第三方一个。
    ///
    /// **第三方档位不等于自家档位**：wgpu/naga 一开 debug 就刷屏（本文件
    /// 顶部注释里记着），把它们跟自家一起提上去，剖面行会被淹没在几万行
    /// adapter 日志里 —— 那等于这个功能没做。
    ///
    /// 自证会变红：把 `levels_for` 里 Debug 档的 deps 从 `Info` 改成 `Debug`。
    #[test]
    fn the_dependency_level_never_follows_our_own_all_the_way_up() {
        assert_eq!(
            levels_for(LogLevel::Debug),
            (LevelFilter::Debug, LevelFilter::Info)
        );
        assert_eq!(
            levels_for(LogLevel::Info),
            (LevelFilter::Info, LevelFilter::Warn)
        );
        assert_eq!(
            levels_for(LogLevel::Error),
            (LevelFilter::Error, LevelFilter::Error)
        );
    }

    /// 环境变量**覆盖**设置：排障时不必先进 GUI 改设置再重启。
    ///
    /// 反过来（设置覆盖环境变量）的话，「我明明设了 MULLION_LOG=debug
    /// 怎么还是没有」会变成一个查无可查的问题。
    ///
    /// 自证会变红：把 `resolve_levels` 里两个 `parse_level` 的 default
    /// 参数换成写死的 `LevelFilter::Info`/`Warn`。
    #[test]
    fn an_environment_variable_wins_over_the_stored_setting() {
        // 设置说 error，环境变量说 debug → 用 debug。
        assert_eq!(
            resolve_levels(LogLevel::Error, Some("debug"), None),
            (LevelFilter::Debug, LevelFilter::Error),
            "MULLION_LOG 没能覆盖设置里的档位"
        );
        // 环境变量缺席 → 完全按设置。
        assert_eq!(
            resolve_levels(LogLevel::Debug, None, None),
            (LevelFilter::Debug, LevelFilter::Info)
        );
        // 依赖档有自己的环境变量，同样覆盖。
        assert_eq!(
            resolve_levels(LogLevel::Info, None, Some("off")),
            (LevelFilter::Info, LevelFilter::Off)
        );
    }

    /// 环境变量写错（`verbose`）时回落到**设置里的档位**，而不是回落到
    /// 硬编码的默认。用户在设置里选了 debug、又在环境变量里打错一个词，
    /// 结果日志静默降回 info —— 那比直接忽略更难查。
    #[test]
    fn a_typo_in_the_environment_falls_back_to_the_stored_level_not_to_a_hardcoded_one() {
        assert_eq!(
            resolve_levels(LogLevel::Debug, Some("verbose"), None),
            (LevelFilter::Debug, LevelFilter::Info)
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib logx:: 2>&1 | tail -20
```
预期：`cannot find function levels_for`。

- [ ] **Step 3: 实现**

在 `crates/mullion-app/src/logx.rs` 的 `parse_level` 之后追加：

```rust
/// 设置里的三档 → (自家 crate 档位, 第三方 crate 档位)。
///
/// **第三方永远比自家低一档**：wgpu/naga/winit 一开 debug 就刷屏（见模块
/// 顶部说明），跟着自家一起提上去的话，每 5 秒一行的剖面会被淹没在几万行
/// adapter 日志里 —— 那等于这个功能没做。
pub fn levels_for(level: mullion_store::LogLevel) -> (LevelFilter, LevelFilter) {
    match level {
        mullion_store::LogLevel::Error => (LevelFilter::Error, LevelFilter::Error),
        mullion_store::LogLevel::Info => (LevelFilter::Info, LevelFilter::Warn),
        mullion_store::LogLevel::Debug => (LevelFilter::Debug, LevelFilter::Info),
    }
}

/// 设置 + 两个环境变量 → 最终档位。**环境变量覆盖设置**。
///
/// 纯函数（环境变量由调用方读好传进来），这样「谁覆盖谁」这条规则测得动。
pub fn resolve_levels(
    stored: mullion_store::LogLevel,
    env_app: Option<&str>,
    env_deps: Option<&str>,
) -> (LevelFilter, LevelFilter) {
    let (app, deps) = levels_for(stored);
    (
        parse_level(env_app, app),
        parse_level(env_deps, deps),
    )
}
```

把 `init` 的签名与档位解析改掉。找到：

```rust
pub fn init(version: &str) {
```
改为：
```rust
/// `stored` 来自 `settings.toml`（`main` 在调用本函数**之前**读好）。
/// 读不到设置时传 `LogLevel::Info`。
pub fn init(version: &str, stored: mullion_store::LogLevel) {
```

并把函数体里：

```rust
    let app = parse_level(
        std::env::var("MULLION_LOG").ok().as_deref(),
        LevelFilter::Info,
    );
    let deps = parse_level(
        std::env::var("MULLION_LOG_DEPS").ok().as_deref(),
        LevelFilter::Warn,
    );
```
替换为：
```rust
    let env_app = std::env::var("MULLION_LOG").ok();
    let env_deps = std::env::var("MULLION_LOG_DEPS").ok();
    let (app, deps) = resolve_levels(stored, env_app.as_deref(), env_deps.as_deref());
```

- [ ] **Step 4: 加 store 依赖**

`mullion-app` 已经依赖 `mullion-store`（`session_map.rs` 在用），无需改 `Cargo.toml`。确认一下：

```bash
grep -n "mullion-store" crates/mullion-app/Cargo.toml
```

- [ ] **Step 5: 跑测试**

```bash
cargo test -p mullion-app --lib logx:: 2>&1 | tail -20
```
预期：既有 3 条 + 新增 3 条全过。（`main.rs` 此刻会因签名变更编译失败，Task 7 修；先只跑 `--lib`。）

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/logx.rs
git commit -m "feat(app): 日志档位改由设置决定，环境变量仍可覆盖 (F155)"
```

---

## Task 6: 分档容量 + 缓冲写 + 周期 flush

**Files:**
- Modify: `crates/mullion-app/src/logx.rs`
- Modify: `crates/mullion-app/src/diag.rs`（周期线程里调一次 flush）

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/logx.rs` 的 `mod tests` 末尾追加：

```rust
    /// debug 档写得多，8MB 一代几十秒就冲掉了 —— 真正出问题那一刻的记录
    /// 已经被自己刷没了。档位高时上限跟着抬。
    ///
    /// 自证会变红：把 `rotate_bytes_for` 改成恒返回 `ROTATE_AT_BYTES`。
    #[test]
    fn a_chattier_level_gets_a_bigger_file_before_rotating() {
        let info = rotate_bytes_for(LevelFilter::Info);
        let debug = rotate_bytes_for(LevelFilter::Debug);
        assert_eq!(info, ROTATE_AT_BYTES);
        assert!(
            debug >= info * 4,
            "debug 档的上限只有 {debug}，几十秒就把出问题那一刻冲掉了"
        );
    }

    /// 哪些级别必须**立刻**落盘。
    ///
    /// 本文件存在的全部理由是「卡死/被强杀时最后一行停在哪」——缓冲写会把
    /// 这个能力削掉。折中：错误与警告立刻 flush（它们稀少，代价可忽略），
    /// info/debug 走缓冲、由周期线程每秒 flush 一次，最坏丢一秒。
    ///
    /// 自证会变红：把 `flush_immediately` 改成恒 `false`。
    #[test]
    fn errors_are_flushed_at_once_while_chatter_may_wait_a_second() {
        assert!(flush_immediately(log::Level::Error));
        assert!(flush_immediately(log::Level::Warn));
        assert!(!flush_immediately(log::Level::Info));
        assert!(!flush_immediately(log::Level::Debug));
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib logx:: 2>&1 | tail -10
```
预期：`cannot find function rotate_bytes_for`。

- [ ] **Step 3: 实现**

在 `crates/mullion-app/src/logx.rs` 里，`ROTATE_AT_BYTES` 常量下面追加：

```rust
/// debug 档的单文件上限。8MB 在 debug 下几十秒就满 —— 出问题那一刻的记录
/// 会被自己后面的日志冲掉，而那正是唯一有价值的一段。
const ROTATE_AT_BYTES_DEBUG: u64 = 64 * 1024 * 1024;

/// 这个档位下，单个日志文件多大就轮转。
pub fn rotate_bytes_for(app: LevelFilter) -> u64 {
    if app >= LevelFilter::Debug {
        ROTATE_AT_BYTES_DEBUG
    } else {
        ROTATE_AT_BYTES
    }
}

/// 这一条要不要立刻落盘。
///
/// 逐行 flush 是本文件原本的设计（卡死时「最后一行停在哪」是硬证据），
/// 但 debug 档下每帧几条日志、每条都同步写盘，磁盘就进了帧预算，
/// 测出来的不再是原来的程序（T3）。折中：错误/警告立刻 flush（稀少），
/// 其余走缓冲、由 `diag` 的周期线程每秒 flush 一次，最坏丢一秒。
pub fn flush_immediately(level: log::Level) -> bool {
    level <= log::Level::Warn
}
```

把 `write_line` 改成带级别的版本。找到：

```rust
fn write_line(msg: &str) {
```
改为：
```rust
fn write_line(msg: &str) {
    write_line_at(msg, log::Level::Warn);
}

/// 真正落盘。`level` 决定要不要立刻 flush（见 [`flush_immediately`]）。
fn write_line_at(msg: &str, level: log::Level) {
```
并把函数体末尾的：
```rust
            let _ = f.write_all(full.as_bytes());
            let _ = f.flush();
```
改为：
```rust
            let _ = f.write_all(full.as_bytes());
            if flush_immediately(level) {
                let _ = f.flush();
            }
```

把 `impl Log for FileLogger` 的 `log` 改成传级别：

```rust
    fn log(&self, r: &Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        write_line_at(
            &format!("{:<5} {}: {}", r.level(), r.target(), r.args()),
            r.level(),
        );
    }

    fn flush(&self) {
        flush_now();
    }
```

在文件末尾（`write_line_at` 之后）加公开的 flush：

```rust
/// 把缓冲里的日志刷到盘上。`diag` 的周期线程每秒调一次 —— 没有它，
/// info/debug 档下卡死时最后几秒的记录会随进程一起消失。
pub fn flush_now() {
    if let Some(Some(m)) = SINK.get() {
        if let Ok(mut f) = m.lock() {
            let _ = f.flush();
        }
    }
}
```

把 `init` 里的 `rotate_if_large(p);` 改成按档位：由于档位在打开文件之后才算出来，把顺序调整为「先算档位、再开文件」。在 `init` 开头（`let path = log_path();` 之前）插入档位解析，并把 `rotate_if_large` 的调用改成：

```rust
    let env_app = std::env::var("MULLION_LOG").ok();
    let env_deps = std::env::var("MULLION_LOG_DEPS").ok();
    let (app, deps) = resolve_levels(stored, env_app.as_deref(), env_deps.as_deref());

    let path = log_path();
    let file = path.as_ref().and_then(|p| {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        rotate_if_large(p, rotate_bytes_for(app));
        OpenOptions::new().create(true).append(true).open(p).ok()
    });
    let _ = SINK.set(file.map(Mutex::new));
```
（删掉原先在下面重复计算 `app`/`deps` 的那几行。）

`rotate_if_large` 加一个参数：

```rust
fn rotate_if_large(p: &Path, limit: u64) {
    if let Ok(md) = std::fs::metadata(p) {
        if md.len() > limit {
            let _ = std::fs::rename(p, p.with_extension("log.1"));
        }
    }
}
```

最后，在 `crates/mullion-app/src/diag.rs` 的 `watchdog_loop` 循环体末尾（`if now.saturating_sub(last_metrics) ...` 块之后）加一行：

```rust
        // info/debug 档走缓冲写，靠这里把最后一秒刷下去 —— 没有它，
        // 卡死时最后几秒的日志会随进程一起消失，而那正是唯一有用的一段。
        crate::logx::flush_now();
```

- [ ] **Step 4: 跑测试**

```bash
cargo test -p mullion-app --lib logx:: 2>&1 | tail -20
```
预期：8 条全过。

- [ ] **Step 5: 提交**

```bash
cargo clippy -p mullion-app --lib -- -D warnings
git add crates/mullion-app/src/logx.rs crates/mullion-app/src/diag.rs
git commit -m "feat(app): 日志按档位分容量，info/debug 缓冲写 + 每秒 flush (F155)

守护测试 logx::tests::errors_are_flushed_at_once_while_chatter_may_wait_a_second
（逐行 flush 在 debug 档下会把磁盘写进帧预算，T3）"
```

---

## Task 7: `main.rs` 接线 —— 先读设置再 init

**Files:**
- Modify: `crates/mullion-app/src/main.rs`

- [ ] **Step 1: 改接线**

把 `crates/mullion-app/src/main.rs:25-31` 那一段：

```rust
    // 文件日志 + panic 钩子:GUI 子系统下崩溃/卡死无声消失,靠这个取证。
    // init 同时接管 `log` facade,wgpu/winit/russh 的内部诊断一并落进同一个文件。
    mullion_app::logx::init(env!("CARGO_PKG_VERSION"));
```

替换为：

```rust
    // F155:日志档位来自 settings.toml。**必须在 logx::init 之前读** ——
    // 档位决定了轮转上限和 facade 的 max_level,init 之后再改就晚了。
    //
    // settings.toml 是明文 TOML、不经 keyring/主密码(见 store 的 settings.rs),
    // 所以这一步不需要会话库打开,也不会弹解锁框。读不出来就按默认档位跑,
    // 那句降级说明等 init 之后再补记 —— 此刻还没有日志可写。
    let (log_level, settings_note) = match mullion_app::shell::store::config_dir() {
        Some(dir) => {
            let loaded = mullion_store::settings::load(&dir);
            (loaded.settings.log_level, loaded.note)
        }
        None => (mullion_store::LogLevel::Info, None),
    };

    // 文件日志 + panic 钩子:GUI 子系统下崩溃/卡死无声消失,靠这个取证。
    // init 同时接管 `log` facade,wgpu/winit/russh 的内部诊断一并落进同一个文件。
    mullion_app::logx::init(env!("CARGO_PKG_VERSION"), log_level);
    if let Some(note) = settings_note {
        mullion_app::logx::line(&format!("settings.toml:{note}"));
    }
```

- [ ] **Step 2: 编译并跑全量测试**

```bash
cargo build -p mullion-app 2>&1 | tail -20
cargo test --workspace > /tmp/t7.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t7.log
```
预期：编译通过，全部测试通过。

- [ ] **Step 3: 手工验证日志真的出来了**

```bash
MULLION_LOG=debug timeout 5 cargo run -p mullion-app 2>&1 | head -20
```
预期：stderr 里能看到 `==== mullion 0.1.64 启动;日志: ... (app=DEBUG deps=INFO) ====`。
（无显示器时窗口创建会失败退出，这没关系——我们只看日志头两行。）

- [ ] **Step 4: 提交**

```bash
git add crates/mullion-app/src/main.rs
git commit -m "feat(app): 启动时先读 settings.toml 的日志档位再初始化日志 (F155)"
```

---

## Task 8: 设置弹窗「诊断」分节

**Files:**
- Modify: `crates/mullion-app/src/ui/settings.rs`

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/ui/settings.rs` 的 `mod tests` 末尾追加：

```rust
    // ---- F155 诊断分节 ----

    /// 三档都要画出来，而且**当前档位要被选中**。只画不选中的话，用户看到
    /// 三个一样的选项，无从判断现在是哪档。
    #[test]
    fn the_diagnostics_section_lists_all_three_levels_and_shows_the_current_one() {
        let mut d = draft();
        d.log_level = mullion_store::LogLevel::Debug;
        let (texts, _) = run(&mut d, false);
        assert!(texts.iter().any(|s| s == "日志详细度"), "没画标签：{texts:?}");
        assert!(
            texts.iter().any(|s| s.contains("详细")),
            "下拉没显示当前档位：{texts:?}"
        );
    }

    /// 改档位要当场回报 `Preview`（草稿变了），「确定」时才落盘并生效。
    /// 回报 `None` 的话用户点了没反应。
    ///
    /// 自证会变红：把 `diagnostics` 里改 `draft.log_level` 那几处的
    /// `*out = SettingsOut::Preview;` 删掉。
    #[test]
    fn picking_a_level_reports_a_preview() {
        let mut d = draft();
        assert_eq!(d.log_level, mullion_store::LogLevel::Info);
        // 下拉要先展开才点得到选项，所以直接点标签行右侧的下拉本体，
        // 再在展开的列表里点「详细（排查用）」。
        let out = interact(&mut d, LEVEL_DEBUG_LABEL, egui::Vec2::ZERO, true);
        assert_eq!(d.log_level, mullion_store::LogLevel::Debug, "选项没被真的点到");
        assert_eq!(out, SettingsOut::Preview);
    }

    /// 草稿从**落盘的真值**起。起错了的症状是「用户改成 debug，重开设置又
    /// 显示默认档」——而他只要这时点确定，改过的选择就被假初值覆盖回去了。
    ///
    /// 自证会变红：把 `from_settings` 里那行改成写死的 `LogLevel::Info`。
    #[test]
    fn the_draft_starts_from_the_stored_log_level() {
        let s = mullion_store::Settings {
            log_level: mullion_store::LogLevel::Error,
            ..Default::default()
        };
        assert_eq!(
            SettingsDraft::from_settings(&s).log_level,
            mullion_store::LogLevel::Error
        );
    }

    /// 「导出脱敏日志」这颗按钮必须在，而且必须**说清楚它是尽力而为的**。
    /// 给用户一个「导出即安全」的印象，比不提供这个功能更糟 —— 他会闭着
    /// 眼把日志发出去。
    #[test]
    fn the_export_button_is_present_and_does_not_promise_perfect_redaction() {
        let mut d = draft();
        let (texts, _) = run(&mut d, false);
        assert!(
            texts.iter().any(|s| s.contains("导出脱敏日志")),
            "没有导出按钮：{texts:?}"
        );
        assert!(
            texts.iter().any(|s| s.contains("发送前请自己再看一眼")),
            "没有说明脱敏是尽力而为的：{texts:?}"
        );
    }
```

在 `mod tests` 的 `draft()` 里补上新字段：

```rust
            log_level: mullion_store::LogLevel::Info,
```
（`a_font_that_is_not_installed_is_called_out` 里那个手写的 `SettingsDraft` 字面量同样要补。）

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib ui::settings 2>&1 | tail -20
```
预期：`no field log_level on type SettingsDraft`。

- [ ] **Step 3: 实现**

在 `crates/mullion-app/src/ui/settings.rs` 顶部的 `const BOOTSTRAP_LABEL` 之后追加三个档位标签常量（实现与测试共用同一份，各写一遍的话改文案时测试会静默点不中）：

```rust
/// 三档的中文标签。**实现与测试共用同一份** —— 各写一遍的话，改文案时
/// 测试会静默地点不中，`interact` 里那句 panic 才是唯一的提示。
const LEVEL_ERROR_LABEL: &str = "只记错误";
const LEVEL_INFO_LABEL: &str = "常规（含性能剖面）";
const LEVEL_DEBUG_LABEL: &str = "详细（排查用）";

fn level_label(lv: mullion_store::LogLevel) -> &'static str {
    match lv {
        mullion_store::LogLevel::Error => LEVEL_ERROR_LABEL,
        mullion_store::LogLevel::Info => LEVEL_INFO_LABEL,
        mullion_store::LogLevel::Debug => LEVEL_DEBUG_LABEL,
    }
}
```

在 `SettingsDraft` 里，`tmux_bootstrap` 之后追加：

```rust
    /// F155：日志详细档位。
    pub log_level: mullion_store::LogLevel,
    /// F155：这一帧按了「导出脱敏日志」。**不是偏好**，由 `app.rs` 取走后
    /// 当场复位 —— 与 `new_password` 同构。
    pub export_log_request: bool,
```

在 `from_settings` 里追加：

```rust
            log_level: s.log_level,
            export_log_request: false,
```

在 `SettingsOut` 里追加一个变体：

```rust
    /// F155：按了「导出脱敏日志」。**弹窗不关** —— 导出完用户要看到那句
    /// 「已导出到 …」，关掉就看不见了。
    ExportLog,
```

在 `show` 的分节链里，「远端」与「安全」之间插入：

```rust
            form::section(ui, t, "设置", "诊断", &mut first);
            diagnostics(ui, t, draft, &mut out);
```

在 `remote` 函数之后追加：

```rust
/// 诊断分节（F155）：日志详细度 + 导出脱敏日志。
///
/// 走 `form::grid` 两列骨架（规范 #1），说明文字挂**输入列**、标签列留空
/// （规范 #6）。
fn diagnostics(ui: &mut egui::Ui, t: &Theme, draft: &mut SettingsDraft, out: &mut SettingsOut) {
    let avail = ui.available_width();
    form::grid(ui, "settings_diagnostics", |ui| {
        ui.label("日志详细度");
        let w = field_w(avail, FIELD_W_M, 0.0);
        egui::ComboBox::from_id_salt("settings_log_level")
            .width(w)
            .selected_text(level_label(draft.log_level))
            .show_ui(ui, |ui| {
                for lv in [
                    mullion_store::LogLevel::Error,
                    mullion_store::LogLevel::Info,
                    mullion_store::LogLevel::Debug,
                ] {
                    if ui
                        .selectable_label(draft.log_level == lv, level_label(lv))
                        .clicked()
                    {
                        draft.log_level = lv;
                        *out = SettingsOut::Preview;
                    }
                }
            });
        ui.end_row();

        ui.label("");
        ui.label(
            egui::RichText::new(
                "常规档每 5 秒记一行性能剖面（帧耗时、吞吐、各阶段占用、回显往返），\
                 排查卡顿靠它。详细档还会逐事件记录，日志会大很多。\
                 环境变量 MULLION_LOG 若设了，会盖过这里的选择。",
            )
            .size(11.0)
            .color(theme::c32(t.fg_dim)),
        );
        ui.end_row();

        ui.label("");
        if ui.button("导出脱敏日志…").clicked() {
            draft.export_log_request = true;
            *out = SettingsOut::ExportLog;
        }
        ui.end_row();

        ui.label("");
        ui.label(
            egui::RichText::new(
                "另存一份把主机名、用户名、IP、路径换成假名的日志，用于对外发送。\
                 替换是按模式匹配做的，覆盖不到的写法会漏 —— 发送前请自己再看一眼。",
            )
            .size(11.0)
            .color(theme::c32(t.danger)),
        );
        ui.end_row();
    });
    ui.add_space(SP_M);
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib ui::settings 2>&1 | tail -25
```
预期：既有 14 条 + 新增 4 条全过。

⚠️ 若 `picking_a_level_reports_a_preview` 因为「找不到写着『详细（排查用）』的部件」而 panic，说明 `ComboBox` 没展开——egui 的下拉列表只在展开时才画。此时改成两步：先 `interact` 点下拉本体（用 `LEVEL_INFO_LABEL` 作为锚点，它是 `selected_text`），再在返回的同一个 `ctx` 上点选项。若脚手架不支持连续两帧交互，退而求其次：把这条测试改成直接断言 `diagnostics` 的纯逻辑（`level_label` 的三个映射）+ 一条源码切片断言（`src.contains("draft.log_level = lv;")`），并在测试注释里写明为什么退化。

- [ ] **Step 5: 字形白名单检查**

```bash
cargo test -p mullion-app --test glyph_whitelist 2>&1 | tail -10
```
预期：通过。新加的文案里只有汉字、ASCII 和 `—`（已在 `VERIFIED` 里）。

- [ ] **Step 6: 提交**

```bash
cargo clippy -p mullion-app --all-targets -- -D warnings
git add crates/mullion-app/src/ui/settings.rs
git commit -m "feat(app): 设置里加「诊断」分节，日志档位可选 + 导出脱敏日志 (F155)"
```

---

## Task 9: `app.rs` 让档位改动当场生效

**Files:**
- Modify: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/app.rs` 的 `mod tests` 里追加（放在既有的设置相关测试附近）：

```rust
    /// F155：改了档位必须**当场生效**，不能等下次启动。
    ///
    /// `log::set_max_level` 是进程级的，改完立刻影响 facade 的过滤。不调它的
    /// 症状是「在设置里选了详细档，日志里什么都没多」——而设置文件里确实存
    /// 对了，下次启动又是好的，这种「改完像没生效、重启才对」最难自查。
    ///
    /// 这里扎的是**源码结构**：真正跑一遍要 `App`（无头环境构造不出来），
    /// 而 `apply_log_level` 是纯粹的副作用函数，没有可断言的返回值。
    ///
    /// 自证会变红：把 `apply_settings_action` 的 `O::Commit` 分支里那句
    /// `self.apply_log_level();` 删掉。
    #[test]
    fn committing_the_settings_applies_the_new_log_level_immediately() {
        let src = include_str!("app.rs");
        let commit = src
            .split("O::Commit => {")
            .nth(1)
            .expect("apply_settings_action 里没有 O::Commit 分支了？");
        let body = commit.split("O::Cancel").next().unwrap_or(commit);
        assert!(
            body.contains("self.apply_log_level();"),
            "点了确定却没把新档位施加到 log facade 上 —— \
             设置文件存对了、日志却没变，重启才生效"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib committing_the_settings_applies 2>&1 | tail -10
```
预期：FAILED，断言消息如上。

- [ ] **Step 3: 实现**

在 `crates/mullion-app/src/app.rs` 的 `apply_settings_action` 之前（`apply_font` 附近）追加：

```rust
    /// F155:把设置里的日志档位施加到 `log` facade 上。
    ///
    /// **环境变量仍然优先**(`logx::resolve_levels`):用户带着
    /// `MULLION_LOG=debug` 启动、又在设置里选了「只记错误」,他要的是前者。
    fn apply_log_level(&self) {
        let env_app = std::env::var("MULLION_LOG").ok();
        let env_deps = std::env::var("MULLION_LOG_DEPS").ok();
        let (app, deps) = crate::logx::resolve_levels(
            self.settings.log_level,
            env_app.as_deref(),
            env_deps.as_deref(),
        );
        crate::logx::set_levels(app, deps);
        crate::logx::line(&format!("日志档位改为 app={app} deps={deps}"));
    }
```

在 `apply_settings_action` 的 `O::Commit` 分支里，`self.apply_font();` 之后插入：

```rust
                self.apply_log_level();
```

`logx` 需要一个能在运行时改档位的入口。在 `crates/mullion-app/src/logx.rs` 里，把 `FileLogger` 的两个字段换成原子，并加 `set_levels`：

```rust
struct FileLogger {
    app: AtomicUsize,
    deps: AtomicUsize,
}

/// `LevelFilter` ↔ `usize` 的互转。`LevelFilter` 是 `#[repr(usize)]` 的
/// C 风格枚举，`as usize` / `from_usize` 是 `log` crate 自己在用的编码。
fn filter_from_usize(v: usize) -> LevelFilter {
    match v {
        0 => LevelFilter::Off,
        1 => LevelFilter::Error,
        2 => LevelFilter::Warn,
        3 => LevelFilter::Info,
        4 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    }
}

static LOGGER: OnceLock<&'static FileLogger> = OnceLock::new();

impl Log for FileLogger {
    fn enabled(&self, md: &Metadata) -> bool {
        let limit = if is_own_crate(md.target()) {
            filter_from_usize(self.app.load(std::sync::atomic::Ordering::Relaxed))
        } else {
            filter_from_usize(self.deps.load(std::sync::atomic::Ordering::Relaxed))
        };
        md.level() <= limit
    }
    // log / flush 同前
}

/// 运行时改档位（设置弹窗点了确定）。`init` 之前调用无效果。
pub fn set_levels(app: LevelFilter, deps: LevelFilter) {
    if let Some(l) = LOGGER.get() {
        l.app
            .store(app as usize, std::sync::atomic::Ordering::Relaxed);
        l.deps
            .store(deps as usize, std::sync::atomic::Ordering::Relaxed);
        log::set_max_level(app.max(deps));
    }
}
```

`init` 里建 logger 的那句改成先漏成 `&'static`：

```rust
    let logger: &'static FileLogger = Box::leak(Box::new(FileLogger {
        app: AtomicUsize::new(app as usize),
        deps: AtomicUsize::new(deps as usize),
    }));
    let _ = LOGGER.set(logger);
    if log::set_logger(logger).is_ok() {
        log::set_max_level(app.max(deps));
    }
```

并在文件顶部的 `use` 里加 `std::sync::atomic::AtomicUsize`。

- [ ] **Step 4: 跑测试**

```bash
cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked"
```
预期：全过。

- [ ] **Step 5: 提交**

```bash
cargo clippy -p mullion-app --all-targets -- -D warnings
git add crates/mullion-app/src/app.rs crates/mullion-app/src/logx.rs
git commit -m "feat(app): 设置里改日志档位当场生效，不必重启 (F155)"
```

---

## Task 10: 帧耗时与重绘原因接线

**Files:**
- Modify: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: 找到接线点**

```bash
grep -n "diag::count_frame\|RedrawAction::Throttle\|RedrawAction::Present\|frame_is_dirty" crates/mullion-app/src/app.rs
```

- [ ] **Step 2: 写失败的测试**

在 `crates/mullion-app/src/app.rs` 的 `mod tests` 里追加：

```rust
    /// F155：三个采集点必须都在事件循环里接上。
    ///
    /// 扎源码结构而不是跑帧：这三处都在 `redraw` / `about_to_wait` 里，
    /// 要真的窗口和 GPU 才跑得起来。漏接一处不会有任何报错，只会让剖面
    /// 里那一列**恒为零** —— 而「这个窗口没跳帧」与「这个版本忘了统计」
    /// 在日志里长得一模一样。
    ///
    /// 自证会变红：删掉下面任意一句接线。
    #[test]
    fn the_frame_profile_hooks_are_all_wired_into_the_event_loop() {
        let src = include_str!("app.rs");
        for needle in [
            "diag::record_frame_us(",
            "diag::count_redraw(",
            "diag::count_throttled()",
            "diag::set_scale(",
        ] {
            assert!(
                src.contains(needle),
                "剖面采集点 `{needle}` 没接进事件循环 —— 剖面里那一列会恒为零"
            );
        }
    }
```

- [ ] **Step 3: 实现**

3a. 帧耗时。在 `render_frame`（`diag::count_frame();` 所在的函数，`app.rs:9620` 附近）的开头记起点、末尾记耗时。找到：

```rust
    diag::count_frame();
```
改为：
```rust
    diag::count_frame();
    // F155：整帧耗时（到 present 结束为止）。`Instant::now` 在 Windows 上
    // 走 QPC，约 20~30ns，每帧两次可忽略；**绝不能**在这里做格式化（T3）。
    let frame_started = std::time::Instant::now();
```

在同一函数的返回之前（`diag::mark(diag::Stage::Present);` 与 `diag::count_present();` 之后）加：

```rust
    diag::record_frame_us(frame_started.elapsed().as_micros() as u64);
```

> 若该函数有多条返回路径（跳帧时提前 return），在**每一条**跳帧 return 之前也记一次——跳掉的帧同样消耗了时间，不记的话 p95 会偏乐观。跳帧路径上 `diag::count_skipped()` 已经在了，紧挨着它加即可。

3b. 重绘原因与节流。找到调用 `frame_is_dirty` 与 `limiter.plan(..)` 的那一段，在 `plan` 的结果匹配处加：

```rust
        let dirty = crate::frame::frame_is_dirty(terminal_dirty, self.ui_dirty);
        diag::count_redraw(terminal_dirty, self.ui_dirty);
        match limiter.plan(dirty, now_ms) {
            crate::frame::RedrawAction::Throttle { wait_ms } => {
                diag::count_throttled();
                // ...既有逻辑不动
            }
            // ...
        }
```

> 变量名以现场实际为准（`terminal_dirty` / `self.ui_dirty` 可能叫别的）。判据：传给 `count_redraw` 的两个布尔必须是传给 `frame_is_dirty` 的**同一对**，否则剖面里的归因是假的。

3c. 规模。在 `render_frame` 里 `diag::count_frame();` 之后加：

```rust
    diag::set_scale(
        self.tabs.len(),
        self.active_ws().map_or(0, Workspace::pane_count),
        self.active_ws().map_or(0, |ws| ws.hosts.len()),
    );
```

> 方法名以现场为准（`self.tabs.len()` / `Workspace::pane_count` 在 `app.rs:7811` 附近有先例）。

- [ ] **Step 4: 跑测试并编译**

```bash
cargo build -p mullion-app 2>&1 | tail -20
cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked"
```

- [ ] **Step 5: 跑既有的帧率守护测试**

```bash
cargo test -p mullion-app --lib redraw_is_frame_capped 2>&1 | tail -10
cargo test -p mullion-app --lib frame:: 2>&1 | tail -10
```
预期：T3/T7 的守护测试仍然全绿。

- [ ] **Step 6: 提交**

```bash
cargo clippy -p mullion-app --all-targets -- -D warnings
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 剖面接上帧耗时、重绘归因、节流次数与当前规模 (F155)

触到 T3/T7：跑了 app::tests::redraw_is_frame_capped 与 frame::tests 全组"
```

---

## Task 11: 输入延迟与吞吐接线

**Files:**
- Modify: `crates/mullion-app/src/app.rs`
- Modify: `crates/mullion-app/src/session_pump.rs`

- [ ] **Step 1: 找接线点**

```bash
grep -n "diag::count_inbound" crates/mullion-app/src/*.rs
grep -n "fn encode_key\|keymap::encode\|PtyWrite" crates/mullion-app/src/app.rs | head -20
```

- [ ] **Step 2: 写失败的测试**

在 `crates/mullion-app/src/app.rs` 的 `mod tests` 里追加：

```rust
    /// F155：回显往返靠「按键时刻」与「下一段入站字节」配对，两个点缺一
    /// 不可。只接前者的话回显永远采不到样本（剖面行里恒为 `echo=0x`）；
    /// 只接后者的话它永远没有配对的起点。
    ///
    /// 自证会变红：删掉 `diag::note_key()` 或 `diag::note_inbound_for_echo()`。
    #[test]
    fn both_ends_of_the_echo_measurement_are_wired() {
        let app_src = include_str!("app.rs");
        assert!(
            app_src.contains("diag::note_key()"),
            "按键那一端没接 —— 回显永远采不到样本，剖面行里恒为 echo=0x"
        );
        let pump_src = include_str!("session_pump.rs");
        assert!(
            pump_src.contains("note_inbound_for_echo()") || app_src.contains("note_inbound_for_echo()"),
            "入站那一端没接 —— 回显往返永远配不上对"
        );
    }
```

- [ ] **Step 3: 实现**

3a. 按键端。在 `app.rs` 里，键盘事件被判给终端、编码成字节**发出去**的那一处（`keymap` 编码结果非空、写进 pty 之前）加一行：

```rust
            // F155：记下这次按键的时刻，等下一段入站字节来时算回显往返。
            diag::note_key();
```

> 位置判据：必须在「确定这个键会发往远端」之后。加在键盘事件入口的话，Tab 补全被 egui 吞掉的那些键也会被计入，回显分布里就掺了永远等不到回显的样本。

3b. 入站端。在 `session_pump.rs` 里 `diag::count_inbound(..)` 紧邻处加：

```rust
    crate::diag::note_inbound_for_echo();
```

> 若 `count_inbound` 是在 `app.rs` 调的，就加在同一处。判据：两者必须在**同一段字节**上调用，否则往返值是错位的。

- [ ] **Step 4: 跑测试**

```bash
cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked"
cargo test -p mullion-term 2>&1 | grep -E "test result|FAILED"
```

- [ ] **Step 5: 提交**

```bash
cargo clippy -p mullion-app --all-targets -- -D warnings
git add crates/mullion-app/src/app.rs crates/mullion-app/src/session_pump.rs
git commit -m "feat(app): 剖面接上按键与入站字节，量出回显往返 (F155)"
```

---

## Task 12: 连接 / 重连 / SFTP 计数接线

**Files:**
- Modify: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: 找接线点**

```bash
grep -n "UserEvent::ConnectOk\|UserEvent::ConnectErr\|PaneReconnected" crates/mullion-app/src/app.rs | head
grep -n "accept_sftp_opened\|SftpListed\|SftpDone" crates/mullion-app/src/app.rs | head
```

- [ ] **Step 2: 写失败的测试**

```rust
    /// F155：连接成败与重连次数要进剖面。高延迟代理链路上「这一小时重连了
    /// 17 次」是最直接的线索，而它在今天的日志里只能靠人肉数 WARN 行。
    ///
    /// 自证会变红：删掉任意一句接线。
    #[test]
    fn connection_outcomes_are_counted_for_the_profile() {
        let src = include_str!("app.rs");
        assert!(src.contains("diag::count_connect(true)"), "连接成功没计数");
        assert!(src.contains("diag::count_connect(false)"), "连接失败没计数");
        assert!(src.contains("diag::count_reconnect()"), "重连没计数");
        assert!(src.contains("diag::count_sftp_op()"), "SFTP 操作没计数");
    }
```

- [ ] **Step 3: 实现**

- `UserEvent::ConnectOk` 分支开头加 `diag::count_connect(true);`
- `UserEvent::ConnectErr` 分支开头加 `diag::count_connect(false);`
- `UserEvent::PaneReconnected` 分支开头加 `diag::count_reconnect();`
- SFTP 任务完成的事件分支（列目录 / 传输结束）加 `diag::count_sftp_op();`

- [ ] **Step 4: 跑测试并提交**

```bash
cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked"
cargo clippy -p mullion-app --all-targets -- -D warnings
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 剖面接上连接成败、重连与 SFTP 操作计数 (F155)"
```

---

## Task 13: `redact.rs` 脱敏

**Files:**
- Create: `crates/mullion-app/src/redact.rs`
- Modify: `crates/mullion-app/src/lib.rs`

- [ ] **Step 1: 写文件（含失败的测试）**

创建 `crates/mullion-app/src/redact.rs`：

```rust
//! 日志脱敏(F155):把主机名、用户名、IP、路径换成**稳定假名**,得出一份
//! 可以对外发送的副本。
//!
//! **零 IO、零 UI**,纯函数,可纯单测。真正读写文件在 `app.rs`。
//!
//! ## 为什么假名必须稳定
//!
//! 同一台机器在整份日志里始终是 `host#1`。换成随机串或逐行编号的话,
//! 「同一台机器这一小时重连了 17 次」这类模式就看不出来了 —— 而那正是
//! 拿日志找优化方向的主要用途。
//!
//! ## 这不是一个安全边界
//!
//! 替换是**按模式匹配**做的:IPv4、`user@host`、Windows 盘符路径、Unix
//! 绝对路径。覆盖不到的写法(比如一条被拆成两半的主机名、或第三方 crate
//! 用我们没预料到的格式打出来的地址)会漏。UI 上那句「发送前请自己再看
//! 一眼」不是免责话术,是这个模块能力边界的如实陈述 —— 给用户一个
//! 「导出即安全」的印象,比不提供这个功能更糟。

use std::collections::HashMap;

/// 一次导出期间的假名表。
#[derive(Debug, Default)]
pub struct Redactor {
    map: HashMap<String, String>,
    counts: HashMap<&'static str, usize>,
}

impl Redactor {
    pub fn new() -> Self {
        Self::default()
    }

    /// 取(或分配)`raw` 的假名。同一个 `raw` 永远得到同一个假名。
    fn alias(&mut self, kind: &'static str, raw: &str) -> String {
        if let Some(a) = self.map.get(raw) {
            return a.clone();
        }
        let n = self.counts.entry(kind).or_insert(0);
        *n += 1;
        let alias = format!("{kind}#{n}");
        self.map.insert(raw.to_string(), alias.clone());
        alias
    }

    /// 脱敏一行。
    pub fn line(&mut self, s: &str) -> String {
        let s = self.replace_user_at_host(s);
        let s = self.replace_ipv4(&s);
        let s = self.replace_windows_path(&s);
        self.replace_unix_path(&s)
    }

    /// `user@host` / `user@host:port` → `user#1@host#1`(端口保留 —— 端口
    /// 不是私密信息,而「同一台机器的 22 与 2222」是有诊断价值的区分)。
    fn replace_user_at_host(&mut self, s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(at) = rest.find('@') {
            let (head, tail) = rest.split_at(at);
            let user_start = head
                .rfind(|c: char| !is_ident(c))
                .map_or(0, |i| i + head[i..].chars().next().map_or(1, char::len_utf8));
            let user = &head[user_start..];
            let after = &tail[1..];
            let host_end = after
                .find(|c: char| !is_host(c))
                .unwrap_or(after.len());
            let host = &after[..host_end];
            if user.is_empty() || host.is_empty() {
                out.push_str(&rest[..at + 1]);
                rest = &rest[at + 1..];
                continue;
            }
            out.push_str(&head[..user_start]);
            let ua = self.alias("user", user);
            let ha = self.alias("host", host);
            out.push_str(&ua);
            out.push('@');
            out.push_str(&ha);
            rest = &after[host_end..];
        }
        out.push_str(rest);
        out
    }

    /// 裸 IPv4 → `host#N`。
    fn replace_ipv4(&mut self, s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let bytes: Vec<char> = s.chars().collect();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i].is_ascii_digit() {
                let start = i;
                let mut j = i;
                while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == '.') {
                    j += 1;
                }
                let cand: String = bytes[start..j].iter().collect();
                if is_ipv4(&cand) {
                    let a = self.alias("host", &cand);
                    out.push_str(&a);
                    i = j;
                    continue;
                }
                out.extend(&bytes[start..j]);
                i = j;
                continue;
            }
            out.push(bytes[i]);
            i += 1;
        }
        out
    }

    /// `C:\Users\alice\...` → `path#N`。
    fn replace_windows_path(&mut self, s: &str) -> String {
        self.replace_runs(s, |cs, i| {
            if i + 2 < cs.len() && cs[i].is_ascii_alphabetic() && cs[i + 1] == ':' && cs[i + 2] == '\\'
            {
                let mut j = i + 3;
                while j < cs.len() && !cs[j].is_whitespace() {
                    j += 1;
                }
                Some(j)
            } else {
                None
            }
        })
    }

    /// `/home/alice/...` → `path#N`。**只吃两段以上的绝对路径**:
    /// 单段的 `/tmp`、`/etc` 之类没有私密信息,换掉只会让日志更难读。
    fn replace_unix_path(&mut self, s: &str) -> String {
        self.replace_runs(s, |cs, i| {
            if cs[i] != '/' || (i > 0 && !is_boundary(cs[i - 1])) {
                return None;
            }
            let mut j = i + 1;
            let mut slashes = 0;
            while j < cs.len() && !cs[j].is_whitespace() && cs[j] != '"' && cs[j] != ',' {
                if cs[j] == '/' {
                    slashes += 1;
                }
                j += 1;
            }
            (slashes >= 1 && j > i + 2).then_some(j)
        })
    }

    /// 扫一遍字符，把 `probe` 认出来的整段换成 `path#N`。
    fn replace_runs(
        &mut self,
        s: &str,
        probe: impl Fn(&[char], usize) -> Option<usize>,
    ) -> String {
        let cs: Vec<char> = s.chars().collect();
        let mut out = String::with_capacity(s.len());
        let mut i = 0;
        while i < cs.len() {
            if let Some(end) = probe(&cs, i) {
                let raw: String = cs[i..end].iter().collect();
                let a = self.alias("path", &raw);
                out.push_str(&a);
                i = end;
                continue;
            }
            out.push(cs[i]);
            i += 1;
        }
        out
    }
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'
}

fn is_host(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '.'
}

fn is_boundary(c: char) -> bool {
    c.is_whitespace() || c == '=' || c == ':' || c == '(' || c == '"' || c == ','
}

fn is_ipv4(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 3 && p.parse::<u8>().is_ok())
}

/// 导出文件开头那段说明。**必须有** —— 收到这份日志的人(包括未来的你)
/// 要知道里面的 `host#1` 是假名,以及脱敏是尽力而为的。
pub fn header() -> String {
    "# 这是一份脱敏副本:主机名/用户名/IP/路径已替换成稳定假名(同一个真实值\n\
     # 在整份文件里始终是同一个假名)。替换按模式匹配进行,覆盖不到的写法会漏 ——\n\
     # 对外发送前请自己再扫一眼。\n"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 假名必须**稳定**:同一台机器在整份日志里始终是同一个 `host#N`。
    ///
    /// 不稳定的话,「同一台机器这一小时重连了 17 次」这类模式就看不出来了 ——
    /// 而那正是拿日志找优化方向的主要用途。
    ///
    /// 自证会变红:把 `alias` 里查表那一句删掉(每次都新分配一个编号)。
    #[test]
    fn the_same_host_gets_the_same_alias_every_time() {
        let mut r = Redactor::new();
        let a = r.line("连接 alice@prod-web-01:22 成功");
        let b = r.line("prod-web-01 断线,退避重连");
        let host = a
            .split('@')
            .nth(1)
            .and_then(|s| s.split(':').next())
            .expect("该有 host 假名");
        assert!(
            b.contains(host),
            "同一台机器两次拿到了不同假名:\n{a}\n{b}"
        );
    }

    /// 真实信息必须真的不见了。这条是这个模块的**存在理由**,
    /// 松了它整个功能就是装样子。
    #[test]
    fn the_real_values_are_actually_gone() {
        let mut r = Redactor::new();
        let out = r.line("连接 alice@prod-web-01:2222 (10.20.30.40) 私钥 C:\\Users\\alice\\id_ed25519");
        for leaked in ["alice", "prod-web-01", "10.20.30.40", "id_ed25519"] {
            assert!(
                !out.contains(leaked),
                "「{leaked}」漏出去了:\n{out}"
            );
        }
        assert!(out.contains("user#1"), "没换成假名:{out}");
        assert!(out.contains("host#"), "没换成假名:{out}");
        assert!(out.contains("path#1"), "没换成假名:{out}");
    }

    /// 端口保留 —— 它不是私密信息,而「同一台机器的 22 与 2222」是有诊断
    /// 价值的区分(TOFU 键带端口那一片刚踩过)。
    #[test]
    fn the_port_survives_because_it_is_not_secret() {
        let mut r = Redactor::new();
        let out = r.line("bob@nas:2222");
        assert!(out.contains(":2222"), "端口被一起吃掉了:{out}");
    }

    /// 不同的机器必须拿到**不同**的假名。全都换成 `host#1` 的话,
    /// 「三台机器轮流断线」会看起来像「一台机器断了三次」。
    #[test]
    fn different_hosts_get_different_aliases() {
        let mut r = Redactor::new();
        let out = r.line("a@one 与 b@two");
        assert!(out.contains("host#1") && out.contains("host#2"), "{out}");
        assert!(out.contains("user#1") && out.contains("user#2"), "{out}");
    }

    /// 剖面行里全是数字,不该被误伤 —— 误伤了的话这份副本就没法用来
    /// 找性能问题了,而那是导出它的**唯一目的**。
    ///
    /// 自证会变红:把 `is_ipv4` 的 `parts.len() == 4` 改成 `>= 2`。
    #[test]
    fn a_profile_line_of_pure_numbers_is_left_alone() {
        let mut r = Redactor::new();
        let line = "profile 5.0s frame=300x/p50=8.0ms/p95=16.5ms present=298 skip=0 \
                    in=1024.0KB/s mem=180MB";
        assert_eq!(r.line(line), line, "剖面行被误伤了");
    }

    /// 版本号、时间戳这类点分数字不是 IP。
    #[test]
    fn dotted_numbers_that_are_not_addresses_are_left_alone() {
        let mut r = Redactor::new();
        assert_eq!(r.line("mullion 0.1.65 启动"), "mullion 0.1.65 启动");
        assert_eq!(r.line("耗时 1.5s"), "耗时 1.5s");
        // 256 不是合法的 IPv4 段。
        assert_eq!(r.line("256.1.1.1"), "256.1.1.1");
    }

    /// 单段的 `/tmp`、`/etc` 不换 —— 它们没有私密信息,换掉只会让日志更难读。
    #[test]
    fn short_generic_paths_stay_readable() {
        let mut r = Redactor::new();
        assert_eq!(r.line("写入 /tmp"), "写入 /tmp");
        assert!(r.line("读取 /home/alice/.ssh/config").contains("path#1"));
    }

    /// 说明头必须点明「这是假名」且「可能漏」。收到日志的人要知道自己
    /// 手上是什么东西。
    #[test]
    fn the_header_says_it_is_aliased_and_best_effort() {
        let h = header();
        assert!(h.contains("假名"));
        assert!(h.contains("会漏"));
    }
}
```

在 `crates/mullion-app/src/lib.rs` 里挂上：

```rust
pub mod redact;
```

- [ ] **Step 2: 跑测试**

```bash
cargo test -p mullion-app --lib redact:: 2>&1 | tail -25
```
预期：8 条全过。

⚠️ 若 `the_real_values_are_actually_gone` 里 `id_ed25519` 仍然漏出，说明 Windows 路径的替换段没吃到结尾——检查 `replace_windows_path` 的 `probe` 是否在遇到反斜杠后继续吃到空白为止。

- [ ] **Step 3: clippy 并提交**

```bash
cargo clippy -p mullion-app --all-targets -- -D warnings
git add crates/mullion-app/src/redact.rs crates/mullion-app/src/lib.rs
git commit -m "feat(app): 日志脱敏，主机/用户/IP/路径换成稳定假名 (F155)

守护测试 redact::tests::the_same_host_gets_the_same_alias_every_time
（假名不稳定的话，「同一台机器反复断线」这类模式就看不出来了）"
```

---

## Task 14: 「导出脱敏日志」接线

**Files:**
- Modify: `crates/mullion-app/src/ui/chrome.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs`
- Modify: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/ui/chrome.rs` 的 `mod tests` 里追加（照抄既有的
`the_session_menu_has_a_permanent_entry_to_the_history_dialog` 的源码切片手法——
菜单项要展开 `menu_button` 才画得出来，跑帧测不到）：

```rust
    /// F155：导出脱敏日志要有一个**常驻**入口。只放在设置弹窗里的话，
    /// 用户在「日志给我看看」这个语境下找不到它。
    ///
    /// 自证会变红：删掉菜单里那一项。
    #[test]
    fn the_config_menu_has_an_entry_to_export_a_redacted_log() {
        let src = include_str!("chrome.rs");
        assert!(
            src.contains("导出脱敏日志…"),
            "「配置」菜单里没有导出脱敏日志的入口"
        );
    }
```

在 `crates/mullion-app/src/app.rs` 的 `mod tests` 里追加：

```rust
    /// F155：导出的意图必须真的被消费掉。只置位不处理的话，用户点了没反应，
    /// 而且这个 bool 会永远留着 `true`，下次任何一帧都可能又导一次。
    ///
    /// 自证会变红：把 `drain_export_log_request` 的调用删掉。
    #[test]
    fn the_export_request_is_consumed_rather_than_left_set() {
        let src = include_str!("app.rs");
        assert!(
            src.contains("fn drain_export_log_request"),
            "没有消费导出意图的地方"
        );
        assert!(
            src.contains("self.drain_export_log_request()"),
            "导出意图定义了却没人调用 —— 用户点了没反应"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib export 2>&1 | tail -10
```

- [ ] **Step 3: 实现**

3a. `crates/mullion-app/src/ui/mod.rs` 的 `UiState` 里，`history_request` 之后追加：

```rust
    /// F155：菜单/设置里点了「导出脱敏日志…」→ `app.rs` 事后读日志文件、
    /// 脱敏、另存。
    ///
    /// **不在这里直接导出**：要读整个日志文件再写一个新文件，而 `ui/` 这一层
    /// 零 IO（同 `history_request`）。
    pub export_log_request: bool,
```

3b. `crates/mullion-app/src/ui/chrome.rs` 的「配置」菜单里，`设置…` 之前插入：

```rust
                    if ui.button("导出脱敏日志…").clicked() {
                        ui_state.export_log_request = true;
                        ui.close_menu();
                    }
```

3c. `crates/mullion-app/src/app.rs` 里加导出实现（放在 `apply_settings_action` 附近）：

```rust
    /// F155:把 `mullion.log` 脱敏后另存一份,并把路径告诉用户。
    ///
    /// **同步读写、在主线程上做**:日志文件上限 8MB(debug 档 64MB),
    /// 一次读+写在本机盘上是几十毫秒,而这是用户点了按钮等着看结果的动作 ——
    /// 为它开一条 task 换来的是「点完什么都没发生,过一会儿状态栏突然变了」。
    /// 落在 `Stage::StoreIo` 里,卡住的话看门狗会说出来。
    fn drain_export_log_request(&mut self) {
        if !std::mem::take(&mut self.ui.export_log_request) {
            return;
        }
        diag::mark(diag::Stage::StoreIo);
        // 缓冲里可能还压着刚刚那几行,先刷下去,否则导出的副本缺最后一段。
        crate::logx::flush_now();
        let done = crate::logx::log_path()
            .ok_or_else(|| "定位不到日志文件".to_string())
            .and_then(|src| {
                let text = std::fs::read_to_string(&src)
                    .map_err(|e| format!("读不出日志({e})"))?;
                let mut r = crate::redact::Redactor::new();
                let mut out = crate::redact::header();
                for line in text.lines() {
                    out.push_str(&r.line(line));
                    out.push('\n');
                }
                let dst = src.with_file_name("mullion-redacted.log");
                std::fs::write(&dst, out).map_err(|e| format!("写不出副本({e})"))?;
                Ok(dst)
            });
        match done {
            Ok(dst) => {
                let msg = format!("已导出脱敏日志:{}", dst.display());
                crate::logx::line(&msg);
                self.ui.set_error(msg);
            }
            Err(e) => self.ui.set_error(format!("导出脱敏日志失败:{e}")),
        }
        self.ui_dirty = true;
    }
```

> `set_error` 是现成的提示通道（状态栏错误卡片）。成功也走它是有意的：这是一条用户必须看到的路径信息，而项目里没有第二个「短暂提示」通道。若后续要区分成功/失败的配色，那是另一片的事。

3d. 在每帧处理 UI 意图的地方（`history_request` 被消费的同一处）调用：

```bash
grep -n "history_request" crates/mullion-app/src/app.rs
```
在那一句旁边加：

```rust
        self.drain_export_log_request();
```

3e. 设置弹窗里的 `SettingsOut::ExportLog` 也要接：在 `apply_settings_action` 的 `match` 里加一个分支：

```rust
            // F155:设置里点了导出。置位交给每帧的 `drain_export_log_request`
            // 统一处理 —— 两个入口(菜单/设置)共用同一条路径,不复制一遍。
            O::ExportLog => {
                self.ui.export_log_request = true;
            }
```

- [ ] **Step 4: 跑测试**

```bash
cargo test --workspace > /tmp/t14.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t14.log
```
预期：全过。

⚠️ 若 `ui::mod` 里有「`UiState` 字段完备性」或「模态门控」类的穷尽测试变红，按它的提示把新字段登记进去——`export_log_request` **不是模态**（没有弹窗、不吃键盘），不该进 `modal_open`。

- [ ] **Step 5: 提交**

```bash
cargo clippy --workspace --all-targets -- -D warnings
git add crates/mullion-app/src/ui/chrome.rs crates/mullion-app/src/ui/mod.rs crates/mullion-app/src/app.rs
git commit -m "feat(app): 菜单与设置里都能导出脱敏日志 (F155)"
```

---

## Task 15: 文档 + 发版

**Files:**
- Modify: `docs/adr-008-diagnostics.md`
- Modify: `CLAUDE.md`（只加一行指向新档位，若确有必要）
- Modify: `Cargo.toml`（版本号）

- [ ] **Step 1: 补 ADR**

在 `docs/adr-008-diagnostics.md` 末尾追加一节：

```markdown
## 增补（F155，2026-08-22）：档位化 + 周期性剖面

**决策**：日志档位从「只认 `MULLION_LOG` 环境变量」改为「`settings.toml` 里的
三档 + 环境变量覆盖」；默认档（info）下每 5 秒落一行聚合性能剖面。

**为什么不做全量事件流**：本项目是 60fps 帧循环 + SSH 字节流，逐事件记录
一分钟就是几十 MB，而 `write_line` 原本是逐行同步 flush 的——那会把磁盘写
进帧预算，测出来的不再是原来的程序（T3）。聚合剖面用 24 个原子计数器换
「p95 是 3ms 还是 300ms」这个量级的答案，采集端只做原子加法。

**为什么长在 `diag.rs` 上而不是新起一套**：`diag::mark()` 已经铺满事件循环，
改成「换阶段时顺手累计上一阶段的时长」之后，逐阶段耗时分布是白得的，
不新增任何插桩点。这也是本切片工作量能压住的原因。

**第三方档位永远比自家低一档**：wgpu/naga 一开 debug 就刷屏，跟着自家提
上去的话每 5 秒一行的剖面会被淹掉，等于功能没做。

**脱敏是尽力而为，不是安全边界**：按模式匹配替换（IPv4 / `user@host` /
盘符路径 / Unix 绝对路径），覆盖不到的写法会漏。UI 上明写「发送前请自己
再看一眼」——给用户「导出即安全」的印象，比不提供这个功能更糟。
```

- [ ] **Step 2: 跑绿**

```bash
cargo test --workspace > /tmp/final.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/final.log
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```
预期：全过、clippy 无输出、fmt 无差异。

- [ ] **Step 3: 提交文档**

```bash
git add docs/adr-008-diagnostics.md
git commit -m "docs(adr-008): 补记 F155 的档位化与周期剖面决策"
```

- [ ] **Step 4: 发版**

按 `.claude/skills/release-windows/SKILL.md` 一条龙：升 patch 到 **0.1.65** →
跑绿 → 交叉编译 + objdump 验收 → 签名 → 发 GitHub Release（走 socks 代理）→
报链接与人工验收清单。

**人工验收清单（写进 Release notes）：**

1. 双击 exe 起来，打开 `%APPDATA%\mullion\config\mullion.log`，确认里面每
   5 秒有一行 `profile ...`，且**空闲时不再增长**（放着不动 30 秒，行数不变）。
2. 连一台远端、在 tmux 里打几行字，确认 `profile` 行里 `key=` 与
   `echo=` 的样本数都非零，且后面 `p95=` 的量级与你对这条链路的体感相符。
3. 分屏、拖分隔条、滚动大段输出，确认 `frame=` 的 `p95` 与 `stage_us` 里
   排在最前的那一段能对上你看到的卡顿。
4. 设置 → 诊断 → 改成「详细（排查用）」→ 确定，**不重启**，确认日志立刻
   变多（有 wgpu/winit 的 info 行）。改回「常规」，确认又安静下来。
5. 配置菜单 → 导出脱敏日志…，确认状态栏报出路径，打开
   `mullion-redacted.log`，确认：文件头那段说明在；你的主机名/用户名/IP/
   私钥路径**都不在里面**；`profile` 行里的数字**没有被误伤**。
6. 关掉 exe 再打开，确认设置里的档位选择被记住了。

**已知不做（本切片的边界，写进 notes）：**
- SSH 握手的**分阶段**耗时（TCP / 代理 / 跳板 / 认证 / 开 channel）没有拆开，
  只有连接成败与重连次数。拆开要在 `mullion-ssh` 里插桩，而那一层不认识
  app 的 `diag`（架构不变量），需要另设一条回报通道——留给下一片。
- glyphon 的 text run 数与 atlas 占用没有单列，只能从 `text_prepare` 这一段
  的耗时间接看。取 run 数要改 `text.rs` 的热路径，等剖面先证明它确实是
  瓶颈再动。
- 回显往返是**近似**（见 `diag::note_key` 的说明），连续打字与远端自发
  输出都会让它偏小。它回答的是量级，不是精确延迟。

---

## Self-Review

**规格覆盖**

| 共识条目 | 落在哪个 Task |
|---|---|
| 三档 error/info/debug | Task 1（存储）、Task 5（映射） |
| 设置里可配 | Task 8（UI）、Task 9（当场生效） |
| 环境变量优先 | Task 5（`resolve_levels`）、Task 9 |
| 每 5 秒聚合剖面 | Task 4 |
| 空闲不写 | Task 4（`is_idle`） |
| 帧与渲染指标 | Task 3（阶段）、Task 10（帧耗时/归因/节流） |
| 终端吞吐与 VT | Task 3（`pump` 阶段）、Task 11（入站字节率）；同步块命中/超时**未覆盖**，见下 |
| 输入延迟链路 | Task 11（近似） |
| SSH/重连/隔离 | Task 12（计数）；分阶段耗时明确列为不做 |
| 启动时间线 | Task 3 白得（`Startup` 阶段耗时）+ 既有 `logx::line` 打点 |
| 进程资源 | Task 4（`mem_process_mb` / tabs / panes / hosts） |
| SFTP 与传输 | Task 12（操作计数）；速率未拆，见下 |
| 脱敏导出 | Task 13、Task 14 |
| 文字与字形 | 只有 `text_prepare` 阶段耗时，run 数未拆，已在 notes 里明说 |

**发现的缺口（已在计划内处理）**

- **同步块（CSI ?2026）命中与超时次数**没有采集点。这是用户点名要的一项，
  且是「打字慢一拍」的已知真根因。补进 Task 11：在 `mullion-term` 的
  `SyncFramePacer` 上加两个计数器，由 app 每帧读进 `diag`。
  → **执行时把这一项加进 Task 11 的 Step 3**，判据：`render::tests::sync_update_defers_present`
  仍然全绿（T2 守护），且剖面行里多出 `sync=命中/超时` 两个数。
- **SFTP 传输速率**只有操作计数。速率要在传输任务里累加字节，属于
  `files/` 那一片的热路径，本切片只记次数，已在 notes 里明说。

**占位符扫描**：无 TBD / TODO / "similar to Task N"。每个改代码的步骤都给了
完整代码或精确的锚点字符串 + 判据。

**类型一致性**：`LogLevel`（store）→ `logx::levels_for` / `resolve_levels` →
`log::LevelFilter`；`profile::Counts` = `[u64; BUCKETS]`，`profile::StageCounts`
= `[Counts; diag::STAGE_COUNT]`，`diag::STAGE_COUNT` = 12 与 `STAGE_NAMES`
同源；`SettingsOut::ExportLog` 在 Task 8 定义、Task 14 消费；
`UiState::export_log_request` 在 Task 14 定义并在同一 Task 内消费。
