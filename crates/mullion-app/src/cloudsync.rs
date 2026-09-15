//! F273:云端备份的上传编排。
//!
//! **`mullion-store` 负责「装什么、封成什么」,`mullion-cloud` 负责「送出去」,
//! 这里是唯一同时知道两者的地方**(架构不变量:app 是唯一允许知道其余
//! 几个 crate 的地方)。
//!
//! # 一次上传拆成两半
//!
//! [`prepare`](主线程) → [`upload_blocking`](`spawn_blocking` 线程)。
//!
//! 拆的理由是 **`Vault` 搬不进线程**:它住在 `App.store` 里、没有 `Clone`,
//! 而给它加 `Clone` 等于允许「两份 Vault 各自 `save()` 互相覆盖」——
//! 正是 F247/F248 刚修完的「整份覆盖」缺陷族。于是凡是要 vault 的活
//! (封载荷、解 SK)留在事件循环线程上,搬进线程的只有字节。
//!
//! 主线程那一半全是纯 CPU(读几十 KB、一次 sha256、一次 XChaCha20),微秒级。
//! 内容没变时它直接回 `Unchanged`,连 `spawn_blocking` 都不起。
//!
//! # 阻塞
//!
//! `mullion-cloud` 是阻塞式的(ureq)。[`upload_blocking`] 必须挖进
//! `spawn_blocking` —— 在事件循环里同步跑网络会把帧率打到零(T3/T7 红线)。
//!
//! **调用点要走 `Runtime` 句柄上的 `spawn_blocking`,不是自由函数
//! `tokio::task::spawn_blocking`**:GUI 线程不在 runtime 上下文里,自由函数
//! 形态会在运行期直接 panic(编译得过、测试全绿,只有真机才炸)。
//! 这个约束靠 `app.rs` 那边的调用点守着。

use std::path::Path;

use mullion_cloud::error::CloudError;
use mullion_cloud::s3::{Credentials, S3Client};
use mullion_cloud::url::Endpoint;
use mullion_store::{cloud, portable, CloudConfig, Vault};

/// 撞号之后最多再试几次。**必须有限**,见上面那条测试的理由。
pub const MAX_PUT_ATTEMPTS: u32 = 4;

/// 一次上传的结果,回送给事件循环。
#[derive(Debug, Clone)]
pub enum UploadOutcome {
    /// 推上去了。带上新的指纹与序号,由 `app.rs` 写回 `cloud.toml`。
    Ok {
        fingerprint: String,
        seq: u64,
        at: String,
    },
    /// 内容没变,什么都没做。**不是失败** —— 状态栏不该因此报红。
    Unchanged,
    /// 失败。已格式化的、可给用户看的一句话。
    Failed(String),
}

/// 撞号之后该用哪个序号。抽成纯函数只为可测 —— 真实的下一个序号要
/// 重新 List 才知道,这里给的是「至少要比刚才那个大」这条不变量。
fn plan_after_collision(taken: u64) -> Option<u64> {
    taken.checked_add(1)
}

/// 成功之后推进游标。**指纹与序号必须一起推**,见测试里的两条症状。
pub fn record_success(cfg: &mut CloudConfig, fingerprint: &str, seq: u64, at: &str) {
    cfg.last_fingerprint = fingerprint.to_string();
    cfg.last_seq = seq;
    cfg.last_ok_at = at.to_string();
}

/// 失败之后**什么都不改**。单独写成一个函数而不是「在调用点什么都不写」:
/// 有名字的空操作挡得住「顺手在这里记一下免得下次重试」那种改动,
/// 而那种改动会让这次没推上去的改动永远推不上去。
pub fn record_failure(_cfg: &mut CloudConfig) {}

/// 把 `cloud.toml` 里的 `last_ok_at` 折算成 [`cloud::should_upload`] 要的
/// 「距上次成功过了几分钟」。**折算住在 app 这一侧** —— store 不持时钟,
/// 也没有 `time` 依赖(别为这一个函数给它加一个)。
///
/// 两种输入都折算成 `u64::MAX`(「已经过了任意久」,该推):
///
/// - **空 / 读不懂** —— 从没成功推过。折算成 0 的话,刚开启云备份的用户
///   要等满一个 `interval_min` 才见到第一份(默认 30 分钟),而他正盯着
///   状态栏看。
/// - **在未来** —— 时钟回拨,或者另一台时钟不准的机器推过一份。
///   **本项目在 F253~F256 上踩过同一形状**(`is_alive` 把未来的心跳算成
///   「永远活着」):照那样写,这台机器要等到那个未来时刻才会再备份,
///   可能是几个月,期间一片安静、零报错。
pub fn minutes_since_last_ok(last_ok_at: &str, now: time::OffsetDateTime) -> u64 {
    let Ok(then) =
        time::OffsetDateTime::parse(last_ok_at, &time::format_description::well_known::Rfc3339)
    else {
        return u64::MAX;
    };
    let secs = (now - then).whole_seconds();
    if secs < 0 {
        return u64::MAX;
    }
    (secs as u64) / 60
}

/// 这一轮内容的指纹。[`prepare`] 里也要算一次 —— **这是有意的重复**。
///
/// 定时那一路必须先拿到指纹才问得了 [`cloud::should_upload`](「内容变没变」
/// 是它四道闸里的一道),而 `prepare` 那次顺带还把载荷封好了、只在真要推的
/// 时候才跑。省掉这里这次的唯一办法是把四道闸拆散塞进 `prepare`,那样
/// `should_upload` 就没人调用了 —— 而配置完整性那道闸也就永远不会跑,
/// 开着开关但没填完的用户每轮发一次注定 403 的请求,状态栏报「备份失败」,
/// 把真正的原因吃掉。
///
/// 成本:读四个几十 KB 的文件 + 一次 sha256,每个轮询 tick 一次。微秒级。
pub fn fingerprint_now(dir: &Path) -> String {
    let files = portable::collect_top_level(dir);
    let secrets = std::fs::read(dir.join("secrets.enc")).unwrap_or_default();
    cloud::fingerprint(&files, &secrets)
}

/// 一次上传的**主线程那一半**的产物。
pub struct Payload {
    /// 这一份的内容指纹。上传成功后由 `app.rs` 写回游标。
    pub fingerprint: String,
    /// 已经用 vault key 整体封好的字节。**云上那份就是它。**
    pub sealed: Vec<u8>,
    /// 解出来的 Access Key Secret。
    pub secret_access_key: String,
}

/// [`prepare`] 的三种结局。
pub enum Prepared {
    /// 内容没变。**连线程都不用起** —— 更不用建 TCP。
    Unchanged,
    Ready(Payload),
    /// 还没送出去就失败了(没设主密码、SK 读不出、打包失败)。
    Failed(String),
}

/// 上传的**主线程那一半**:装 + 封 + 解 SK。
///
/// # 为什么拆成两半
///
/// `Vault` **搬不进 `spawn_blocking`**:它住在 `App.store` 里、没有 `Clone`,
/// 而给它加一个 `Clone` 等于允许「两份 Vault 各自 `save()` 互相覆盖」——
/// 那正是 F247/F248 刚修完的「整份覆盖」缺陷族。于是凡是要 vault 的活
/// (封载荷、解 SK)全留在事件循环线程上,搬进线程的只有**字节**。
///
/// 这一半**全是纯 CPU**:读四个几十 KB 的文件、一次 sha256、一次
/// XChaCha20 —— 微秒级,不会卡帧。真正会卡的是网络,那一半在
/// [`upload_blocking`] 里。
///
/// 顺带的好处:指纹比对也在这儿做,内容没变时连 `spawn_blocking` 都不起。
pub fn prepare(dir: &Path, vault: &Vault, cfg: &CloudConfig, stamp_rfc3339: &str) -> Prepared {
    // ① 装:顶层三文件 + 密文。**不带 layouts**(设计 D7)。
    //
    // 这里把 `secrets.enc` **原样**放进包,而不是像 F46-a 的本地迁移包那样
    // 用一次性口令重封(`portable::seal_secrets`)。成立的前提**只有一条**:
    // 设计 D6 要求云备份必须先设主密码,于是 `secrets.enc` 的文件头一定是
    // Argon2id、盐随文件走,另一台机器拿主密码就解得开。
    //
    // **这条前提一旦松动(比如哪天允许钥匙串方案也上传),这里必须同步改成
    // 重封** —— 否则拉回来的密文用的是源机钥匙串里的密钥,换台机器一个字
    // 都解不开,而症状是「每条会话都要重新输密码」且零报错(本项目在 F46-a
    // 上已经踩过一次)。`seal_with_master` 在钥匙串方案下会返回
    // `NoMasterPassword`,那是今天挡住这条路的东西,别把它绕过去。
    let files = portable::collect_top_level(dir);
    let secrets = std::fs::read(dir.join("secrets.enc")).unwrap_or_default();
    let fp = cloud::fingerprint(&files, &secrets);
    // 定时那一路已经在 `drive_cloud_backup` 里问过 `should_upload` 了(其中
    // 一道闸就是指纹)。这里**还要再判一次**,因为**手动**那一路是绕过
    // `should_upload` 的 —— 用户点「立刻备份到云」时,「开关关着」「还没到点」
    // 都不该拦他,但「内容没变」要拦(并且要说出这句话,见 `spawn_cloud_backup`)。
    if fp == cfg.last_fingerprint {
        return Prepared::Unchanged;
    }
    // 注意 `secrets.enc` 的字节**不是内容的函数**:`crypto::encrypt` 每次换
    // 一个随机 nonce,所以 vault 存一次盘、密文整个变一遍,哪怕里头一个字段
    // 都没改。后果是「任何一次 vault save 都会触发一次上传」。今天可以接受
    // (vault save 本来就对应一次真实改动),但若以后出现「定时重写 secrets.enc」
    // 之类的路径,这一条会把 keep 份历史窗口刷光 —— 那时候要做的是把指纹的
    // 密文分量换成对**明文载荷**取,不是去调大 interval。

    // ② 封:先拼成 F46-a 的包文本,再**整体**用 vault key 加密(设计 D5)。
    //    整体加密之后,sessions.toml 里的真机 IP / 用户名 / 跳板拓扑不落云端明文。
    let text = match portable::write_pack(files, &secrets, env!("CARGO_PKG_VERSION"), stamp_rfc3339)
    {
        Ok(t) => t,
        Err(e) => return Prepared::Failed(format!("打包失败:{e}")),
    };
    let sealed = match vault.seal_with_master(text.as_bytes()) {
        Ok(b) => b,
        Err(mullion_store::StoreError::NoMasterPassword) => {
            return Prepared::Failed(
                "云端备份需要先设置主密码 —— 钥匙串里的密钥换台机器解不开".into(),
            )
        }
        Err(e) => return Prepared::Failed(format!("加密失败:{e}")),
    };

    // ③ 解 SK。**这是最后一件需要 vault 的事**,做完之后线程那一半就只剩字节了。
    let sk = match cloud::secret_key(cfg, vault) {
        Ok(s) if !s.is_empty() => s,
        Ok(_) => return Prepared::Failed("还没填 Access Key Secret".into()),
        Err(e) => return Prepared::Failed(format!("读不出 Access Key Secret:{e}")),
    };

    Prepared::Ready(Payload {
        fingerprint: fp,
        sealed,
        secret_access_key: sk,
    })
}

/// 上传的**阻塞那一半**:只碰网络。**必须挖进 `spawn_blocking` 调用**
/// (走 `Runtime` 句柄,见模块文档)。
///
/// **签名里不许出现 `Vault`**,见 [`prepare`] 的那段理由(有守护测试钉着)。
///
/// `stamp_compact` = `YYYYMMDD'T'HHMMSS'Z'`(既当 SigV4 的 `x-amz-date`,
/// 也当对象键里那一段 —— 两者同源,省得出现「键上写着 10 点、签名说 11 点」)。
pub fn upload_blocking(
    payload: Payload,
    cfg: &CloudConfig,
    stamp_compact: &str,
    stamp_rfc3339: &str,
) -> UploadOutcome {
    // `S3Client::new` 返回 `Result`(代理串解析不了时报 `Config`)——
    // **不要写成 `.unwrap()`**:那条路上用户填错代理地址就是当场 panic。
    let socks5 = (!cfg.socks5.is_empty()).then_some(cfg.socks5.as_str());
    let client = match S3Client::new(
        Endpoint {
            base: cfg.endpoint.clone(),
            bucket: cfg.bucket.clone(),
            path_style: cfg.path_style,
        },
        Credentials {
            access_key_id: cfg.access_key_id.clone(),
            secret_access_key: payload.secret_access_key,
        },
        cfg.region.clone(),
        socks5,
    ) {
        Ok(c) => c,
        Err(e) => return UploadOutcome::Failed(format!("云端客户端建不起来:{e}")),
    };

    let start = match client.list_keys(&cfg.prefix, stamp_compact) {
        Ok(keys) => cloud::next_seq(&cfg.prefix, &keys),
        Err(e) => return UploadOutcome::Failed(format!("列举云端对象失败:{e}")),
    };
    match put_with_retry(&cfg.prefix, start, stamp_compact, |key| {
        client.put_no_overwrite(key, &payload.sealed, stamp_compact)
    }) {
        Ok(seq) => UploadOutcome::Ok {
            fingerprint: payload.fingerprint,
            seq,
            at: stamp_rfc3339.to_string(),
        },
        Err(msg) => UploadOutcome::Failed(msg),
    }
}

/// 撞号重试的循环本体。**把网络那一步收进闭包,是为了让这段逻辑测得到。**
///
/// 本项目登记过一种恒绿:「纯函数测得扎实、接线没人看着」。`upload_blocking`
/// 整体要真网络才跑得起来,于是最容易出错的那一段(撞号往前挪、上限、
/// 成功时返回的到底是哪个序号)就变成没人守。抽出来之后,假的 `put`
/// 闭包就能把三种形状全测到。
fn put_with_retry(
    prefix: &str,
    mut seq: u64,
    stamp: &str,
    mut put: impl FnMut(&str) -> Result<(), CloudError>,
) -> Result<u64, String> {
    for _ in 0..MAX_PUT_ATTEMPTS {
        let key = cloud::object_key(prefix, seq, stamp);
        match put(&key) {
            Ok(()) => return Ok(seq),
            // 别的机器抢先用掉了这个序号。**往前挪再试** —— 这是没有 CAS
            // 的服务端上唯一的并发保护(设计 D8)。
            Err(CloudError::AlreadyExists) => match plan_after_collision(seq) {
                Some(next) => seq = next,
                None => return Err("序号用尽".into()),
            },
            Err(e) => return Err(format!("上传失败:{e}")),
        }
    }
    Err(format!(
        "连试 {MAX_PUT_ATTEMPTS} 个序号都被占用 —— 可能有别的机器正在频繁上传"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 撞上「序号已被占用」时必须**重新 List、换一个序号再试**,而不是
    /// 直接报失败。这正是追加式布局在没有 CAS 的服务端上的全部并发保护
    /// (设计 D8:OSS 的 PutObject 没有 If-Match)。
    #[test]
    fn a_taken_sequence_number_is_retried_with_a_fresh_one() {
        let plan = plan_after_collision(3);
        assert_eq!(plan, Some(4), "撞号之后没有往前挪");
    }

    /// 重试必须有上限。没有上限的话,一个总是回 409 的服务端会让这个
    /// 后台线程永远转下去,而用户只看得见「备份一直在转」。
    ///
    /// **判据是「真的只打了这么多次」,不是「常量落在某个区间」。**
    /// 后者是对一个字面量做断言:循环写成 `loop {}` 忘了用这个常量,
    /// 它照样绿。
    #[test]
    fn retries_are_bounded_so_a_always_409_server_cannot_spin_forever() {
        let mut calls = 0;
        let r = put_with_retry("p/", 1, "20260915T101500Z", |_| {
            calls += 1;
            Err(CloudError::AlreadyExists)
        });
        assert!(r.is_err(), "全程 409 却报成功");
        assert_eq!(
            calls, MAX_PUT_ATTEMPTS as usize,
            "实际打了 {calls} 次,与上限对不上 —— 循环没用这个常量"
        );
    }

    /// 撞号之后返回的必须是**真正写成功的那个序号**,不是一开始那个。
    ///
    /// 返回错的话,游标会被推到一个并不存在的序号上,下一轮从那儿 +1,
    /// 中间空出来的号永远不会被用 —— 而 `keep` 份的清理是按序号算的。
    #[test]
    fn the_sequence_that_comes_back_is_the_one_that_actually_landed() {
        let mut left = 2;
        let seq = put_with_retry("p/", 5, "20260915T101500Z", |_| {
            if left > 0 {
                left -= 1;
                Err(CloudError::AlreadyExists)
            } else {
                Ok(())
            }
        })
        .expect("第三次该成功");
        assert_eq!(seq, 7, "撞了两次之后落在 7,返回的却是 {seq}");
    }

    /// 不是撞号的错误**立刻停**,不要拿它去消耗重试次数 —— 403(签名/权限)
    /// 重试四次还是 403,只是把用户等待的时间乘以四。
    #[test]
    fn a_non_collision_error_stops_immediately() {
        let mut calls = 0;
        let r = put_with_retry("p/", 1, "20260915T101500Z", |_| {
            calls += 1;
            Err(CloudError::Status {
                code: 403,
                body: "SignatureDoesNotMatch".into(),
            })
        });
        assert!(r.is_err());
        assert_eq!(calls, 1, "非撞号的错误也在重试 —— 打了 {calls} 次");
    }

    /// 成功之后游标必须**同时**推进指纹与序号。
    ///
    /// 只推指纹的话:下一次会算出同一个序号,`ForbidOverwrite` 把它挡掉,
    /// 表现成「备份莫名其妙失败」。
    /// 只推序号的话:指纹永远对不上,每一轮都重推一份内容相同的包,
    /// N 份历史窗口在几小时内被自己刷光。
    ///
    // `CloudConfig` 的 `corrupt` 字段是私有的(Task 8 的设计),于是
    // `CloudConfig { .., ..Default::default() }` 在 **mullion-store 之外**
    // 编不过(E0451:field `corrupt` is private)。只能 default 完再逐字段赋,
    // 而那正好是 `field_reassign_with_default` 要抓的形状 —— 这里没有别的写法,
    // 不是懒。**别把 `corrupt` 改成 pub 来迎合这条 lint**:它私有的理由
    // (守护必须待在 `save` 内部)比这条 style lint 重要得多。
    #[allow(clippy::field_reassign_with_default)]
    #[test]
    fn a_successful_upload_advances_both_the_fingerprint_and_the_sequence() {
        let mut cfg = mullion_store::CloudConfig::default();
        cfg.last_fingerprint = "old".into();
        cfg.last_seq = 3;
        record_success(&mut cfg, "new-fp", 4, "2026-09-15T10:15:00Z");
        assert_eq!(cfg.last_fingerprint, "new-fp");
        assert_eq!(cfg.last_seq, 4);
        assert_eq!(cfg.last_ok_at, "2026-09-15T10:15:00Z");
    }

    /// 失败**不许推进游标**。推了的话下一轮 `should_upload` 会认为
    /// 「内容没变」,于是这次没推上去的改动永远推不上去了,且零报错。
    // `#[allow]` 的理由同上一条:`corrupt` 私有,FRU 在本 crate 里编不过。
    #[allow(clippy::field_reassign_with_default)]
    #[test]
    fn a_failed_upload_leaves_the_cursor_alone() {
        let mut cfg = mullion_store::CloudConfig::default();
        cfg.last_fingerprint = "old".into();
        cfg.last_seq = 3;
        let before = cfg.clone();
        record_failure(&mut cfg);
        assert_eq!(
            cfg, before,
            "失败之后游标被动过了 —— 这次的改动会永远推不上去"
        );
    }

    /// 「从没成功推过」必须折算成**已经过了任意久**,不是 0。
    ///
    /// 折算成 0 的话,刚开启云备份的用户要等满一个 `interval_min` 才有
    /// 第一份 —— 默认 30 分钟,而他刚点完「确定」正盯着状态栏看。
    /// `should_upload` 的文档把这个契约写死成 `u64::MAX`,这里钉住它。
    #[test]
    fn a_config_that_never_succeeded_reads_as_overdue_not_as_just_now() {
        assert_eq!(minutes_since_last_ok("", some_time()), u64::MAX);
        assert_eq!(
            minutes_since_last_ok("不是时间戳", some_time()),
            u64::MAX,
            "读不懂的时间戳被当成「刚刚推过」—— 那会永远推不出去且零报错"
        );
    }

    /// **未来的 `last_ok_at` 要当成「到点了」,不是「刚刚才推过」。**
    ///
    /// 本项目在 F253~F256 上踩过同一形状:`is_alive` 把未来的心跳算成
    /// 「永远活着」。这里若照那样写,一次时钟回拨(或者换台时区/时钟不准的
    /// 机器推过一份)就会让这台机器**在那个未来时刻到来之前永不备份** ——
    /// 可能是几个月,期间状态栏一片安静,零报错。
    #[test]
    fn a_timestamp_from_the_future_counts_as_overdue_not_as_fresh() {
        let now = some_time();
        let ahead = now + time::Duration::days(400);
        let ahead_s = ahead
            .format(&time::format_description::well_known::Rfc3339)
            .expect("格式化");
        assert_eq!(
            minutes_since_last_ok(&ahead_s, now),
            u64::MAX,
            "未来的时间戳被当成「刚推过」—— 这台机器要等到那一刻才会再备份"
        );
    }

    /// 正常情况按分钟折算,且**向下取整**(59 秒不算一分钟)。
    #[test]
    fn a_normal_gap_converts_to_whole_minutes() {
        let now = some_time();
        for (secs, want) in [(0_i64, 0_u64), (59, 0), (60, 1), (5400, 90)] {
            let then = now - time::Duration::seconds(secs);
            let s = then
                .format(&time::format_description::well_known::Rfc3339)
                .expect("格式化");
            assert_eq!(minutes_since_last_ok(&s, now), want, "差 {secs} 秒");
        }
    }

    /// 测试用的固定时刻。**不取 `now_utc()`** —— 拿真实时钟的测试会在
    /// 某些时刻偶发地红,而那种红没人查得动。
    fn some_time() -> time::OffsetDateTime {
        time::OffsetDateTime::from_unix_timestamp(1_789_000_000).expect("固定时刻")
    }

    /// `upload_blocking` **不许认识 `Vault`**。
    ///
    /// 它跑在 `spawn_blocking` 线程上,而 `Vault` 住在 `App.store` 里、
    /// 没有 `Clone` —— 今天靠借用检查挡着。但只要有人哪天给 `Vault` 加一个
    /// `Clone`,「顺手把 vault 传进去」就编得过了,而那等于允许两份 Vault
    /// 各自 `save()` 互相覆盖(F247/F248 刚修完的「整份覆盖」缺陷族)。
    ///
    /// 扎在**签名**上而不是整个函数体:函数体里出现 `Vault` 这个词的地方
    /// 还有文档注释,而签名是唯一说明「什么东西跨了线程」的那一行。
    ///
    /// 自证会变红:把 `vault: &Vault` 加回 `upload_blocking` 的参数表。
    #[test]
    fn the_blocking_half_does_not_know_about_the_vault() {
        let src = include_str!("cloudsync.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .expect("测试模块分界变了,这条测试的锚点失效了");
        assert!(
            prod.len() < src.len(),
            "没能切掉测试模块 —— 下面这条断言会恒真"
        );
        let at = prod
            .find("pub fn upload_blocking(")
            .expect("找不到 upload_blocking");
        let tail = &prod[at..];
        let end = tail.find(") -> ").expect("签名没闭合");
        let sig = &tail[..end];
        assert!(
            !sig.contains("Vault"),
            "upload_blocking 的签名里出现了 Vault —— 它跑在别的线程上:{sig}"
        );
    }
}
