//! F270~F273:云端备份的数据层。**零网络** —— 真正的 PUT/LIST 在
//! `mullion-cloud`,这里只负责「装什么、封成什么、什么时候该推」。
//!
//! # 为什么云配置不住 settings.toml
//!
//! `settings.toml` **在同步包里**。AK/SK 进去的话,拉一份云端配置下来会把本机
//! 的 AK/SK 覆盖成云端那份的 —— 两台机用不同 RAM 子账号就串了;更糟的是云端
//! 那份若是几个月前推的、AK 已经轮换,拉下来本机拿到一把过期密钥,之后永远
//! 推不上去,而报的是 `403`,跟「配置被覆盖」毫无关联。
//!
//! endpoint / bucket / 游标同理:它们是「本机对云的看法」,不该被云端的内容
//! 覆盖。`cloud.toml` 不在 [`crate::portable::TOP_LEVEL_FILES`] 白名单里,
//! 于是 `install` 天然碰不到它。**加新配置文件时记得回来看这一条。**

use sha2::{Digest, Sha256};

/// 云配置文件名。**刻意不进 [`crate::portable::TOP_LEVEL_FILES`]**,见模块文档。
pub const CLOUD_FILE: &str = "cloud.toml";

/// 云端载荷的内容指纹(十六进制 SHA-256)。
///
/// **内容寻址,不是列举式脏标记**:后者在加新配置文件时必然漏一笔,而漏掉的
/// 后果是那个文件的改动永远推不上去且零报错(本项目「列举式门控」已踩三次)。
///
/// 排序之后再喂:指纹是「内容一样吗」的判据,让它依赖文件的枚举顺序,等于埋
/// 一颗「某次无关重构之后每一轮都重推」的雷 —— 而那会把 N 份历史窗口刷光。
///
/// **调用方要保证 `path` 互不重复**。`sort_by` 是稳定排序,两条同名不同正文
/// 的记录排完仍按传入顺序排列,指纹于是又跟顺序挂上钩。今天两个产出路径
/// (`collect_top_level` 走互不相同的 `TOP_LEVEL_FILES`、`collect` 的
/// `layouts/*.toml` 按文件名唯一)都构造不出这种输入,所以不加运行期防御。
pub fn fingerprint(files: &[crate::portable::PackFile], secrets: &[u8]) -> String {
    let mut sorted: Vec<&crate::portable::PackFile> = files.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut h = Sha256::new();
    for f in sorted {
        // 长度前缀:不加的话 ("ab","c") 与 ("a","bc") 算出同一个指纹,
        // 于是「把一段内容从一个文件挪到另一个文件」这种改动会被漏掉。
        h.update((f.path.len() as u64).to_le_bytes());
        h.update(f.path.as_bytes());
        h.update((f.body.len() as u64).to_le_bytes());
        h.update(f.body.as_bytes());
    }
    h.update((secrets.len() as u64).to_le_bytes());
    h.update(secrets);
    let mut s = String::with_capacity(64);
    for b in h.finalize() {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

use std::path::Path;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::error::StoreError;

/// 对象键的默认前缀。
pub const DEFAULT_PREFIX: &str = "mullion/";
/// 默认保留几份。
pub const DEFAULT_KEEP: u32 = 20;
/// 默认多久算一次指纹(分钟)。
pub const DEFAULT_INTERVAL_MIN: u32 = 30;

/// 本机对云端的全部看法。**整份住 `cloud.toml`,不进迁移包**,见模块文档。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub bucket: String,
    #[serde(default = "default_prefix")]
    pub prefix: String,
    /// `true` = `<endpoint>/<bucket>/<key>`。自建 MinIO 多半要打开。
    #[serde(default)]
    pub path_style: bool,
    #[serde(default = "default_keep")]
    pub keep: u32,
    #[serde(default = "default_interval")]
    pub interval_min: u32,
    /// SOCKS5 代理,形如 `127.0.0.1:1080`。空 = 直连。
    ///
    /// **不带 `socks5://` 前缀**:`S3Client::new` 收到的是 `host:port`,
    /// 自己 `format!("socks5://{p}")` 补前缀(已核实 `mullion-cloud/src/s3.rs:69`)。
    /// 带着前缀传进去会拼成 `socks5://socks5://…`,`ureq::Proxy::new` 直接报
    /// `Config` 错。那条错误文案是清楚的,所以不在这里做容错剥前缀 ——
    /// 加一段没有守护测试的容错,比让用户看见一条准确的报错更糟。
    ///
    /// **这个字段不补的话 `socks5` 参数就是条死线**:`mullion-cloud` 为它
    /// 开了 ureq 的 `socks-proxy` 特性、`S3Client::new` 专门收了这个参数,
    /// 而设计 D15 把「SOCKS 代理链路通不通」列进了片一的真机验收项 ——
    /// 没有配置入口的话那条永远传 `None`,验收项验的是一条从没走过的路
    /// (本项目登记过同一形状:「量具存在≠接在那条路上」)。
    #[serde(default)]
    pub socks5: String,
    /// AK 是标识不是秘密,明文存。
    #[serde(default)]
    pub access_key_id: String,
    /// SK **用 vault key 封过再 base64**(F271)。空 = 还没填。
    #[serde(default)]
    pub secret_sealed: String,
    /// 上次成功推上去的那一份的内容指纹。
    #[serde(default)]
    pub last_fingerprint: String,
    /// 上次成功推上去的序号。
    #[serde(default)]
    pub last_seq: u64,
    /// 上次成功的时刻(RFC3339)。只给状态栏看。
    #[serde(default)]
    pub last_ok_at: String,
    /// 这份是从**读不懂的文件**上来的。`save` 见到它就拒绝写。
    ///
    /// **不落盘**(`serde(skip)`),**私有**(只有 [`load`] 能置位)。
    ///
    /// 为什么标记住在结构体里而不是让 `load` 返回 `Result`:守护必须待在
    /// `save` 内部。挪到调用方就成了「每个调用点都要记得判一下」,而这正是
    /// 本项目已经踩过三次的「列举式门控在加档时必然漏」。
    ///
    /// 为什么**不**把标记塞进 `endpoint` 之类的数据字段:设置弹窗直接把
    /// `endpoint` 绑到文本框(Task 12)。塞进去的话用户会在输入框里看见那串
    /// 哨兵,而他只要改一下 endpoint 就把标记冲掉了 —— `save` 当场放行,
    /// `secret_sealed` 连同别的字段一起被默认值抹掉。守护在它最该生效的
    /// 那条路上恰好失效。
    #[serde(skip)]
    corrupt: bool,
}

fn default_prefix() -> String {
    DEFAULT_PREFIX.to_string()
}
fn default_keep() -> u32 {
    DEFAULT_KEEP
}
fn default_interval() -> u32 {
    DEFAULT_INTERVAL_MIN
}

impl Default for CloudConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: String::new(),
            region: String::new(),
            bucket: String::new(),
            prefix: default_prefix(),
            path_style: false,
            keep: default_keep(),
            interval_min: default_interval(),
            socks5: String::new(),
            access_key_id: String::new(),
            secret_sealed: String::new(),
            last_fingerprint: String::new(),
            last_seq: 0,
            last_ok_at: String::new(),
            corrupt: false,
        }
    }
}

/// 读 `cloud.toml`。读不懂时返回一份**关着且不可回写**的配置。
pub fn load(dir: &Path) -> CloudConfig {
    let path = dir.join(CLOUD_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return CloudConfig::default();
    };
    match toml::from_str::<CloudConfig>(&text) {
        Ok(c) => c,
        Err(_) => CloudConfig {
            corrupt: true,
            ..CloudConfig::default()
        },
    }
}

fn b64() -> base64::engine::general_purpose::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// 把 SK 用 vault 的密钥封进 `cfg.secret_sealed`。
///
/// **用 `seal_local` 而不是 `seal_with_master`**:`cloud.toml` 是本机文件,
/// 不跟着任何包走,所以钥匙串方案下也该能存 —— 用户可以先填好云配置,
/// 再去设主密码。云备份**本身**要求主密码(设计 D6),但那道闸在上传那一步,
/// 不该把「填配置」也一起挡掉。
pub fn set_secret_key(
    cfg: &mut CloudConfig,
    vault: &crate::vault::Vault,
    sk: &str,
) -> Result<(), StoreError> {
    cfg.secret_sealed = b64().encode(vault.seal_local(sk.as_bytes())?);
    Ok(())
}

/// 取回 SK 明文。空 = 还没填过(**不是错误**)。
pub fn secret_key(cfg: &CloudConfig, vault: &crate::vault::Vault) -> Result<String, StoreError> {
    if cfg.secret_sealed.is_empty() {
        return Ok(String::new());
    }
    let blob = b64()
        .decode(&cfg.secret_sealed)
        .map_err(|e| StoreError::CorruptSecrets(format!("cloud.toml 的密文不是合法 base64:{e}")))?;
    let plain = vault.open_local(&blob)?;
    String::from_utf8(plain).map_err(StoreError::from)
}

/// 写 `cloud.toml`。
pub fn save(dir: &Path, cfg: &CloudConfig) -> Result<(), StoreError> {
    let path = dir.join(CLOUD_FILE);
    if cfg.corrupt {
        // **文案里必须带完整路径**:这条错会直接显示在设置弹窗上,而用户唯一的
        // 自救办法是去删掉这个文件。只说文件名的话,普通用户在 `%APPDATA%`
        // 底下根本找不到它 —— 设置弹窗本身没有「重置云配置」这个动作(片二)。
        return Err(StoreError::CorruptSecrets(format!(
            "{} 读不懂,拒绝回写 —— 先把它改好或删掉",
            path.display()
        )));
    }
    let text = toml::to_string_pretty(cfg)?;
    crate::vault::write_atomic(&path, text.as_bytes())
}

/// 序号在键里占几位。6 位 = 一百万份,以每 30 分钟一份算够用 57 年。
///
/// **定宽零填充是硬要求**:「最新那份」= List 结果里序号最大的那条,而
/// ListObjectsV2 按**字典序**返回 —— 不填充的话 `10` 会排在 `2` 前面。
const SEQ_WIDTH: usize = 6;

/// 云端对象的扩展名。
const OBJ_EXT: &str = ".mpk";

/// 一份备份的对象键。
pub fn object_key(prefix: &str, seq: u64, stamp: &str) -> String {
    format!("{prefix}{seq:0SEQ_WIDTH$}-{stamp}{OBJ_EXT}")
}

/// 从对象键里抠出序号。不是我们生成的键 → `None`(跳过,不是错误:
/// bucket 是用户自己的,里头有什么我们管不着)。
pub fn parse_seq(prefix: &str, key: &str) -> Option<u64> {
    let rest = key.strip_prefix(prefix)?;
    let rest = rest.strip_suffix(OBJ_EXT)?;
    let (seq, _stamp) = rest.split_once('-')?;
    if seq.len() != SEQ_WIDTH || !seq.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    seq.parse().ok()
}

/// 下一个该用的序号。**空列表从 1 起** —— 0 与「从没推过」的游标初值撞在
/// 一起,那两种状态就再也分不开了。
pub fn next_seq(prefix: &str, keys: &[String]) -> u64 {
    keys.iter()
        .filter_map(|k| parse_seq(prefix, k))
        .max()
        .map_or(1, |m| m + 1)
}

/// 这一轮该不该推。**纯函数** —— 时钟由调用方折算成
/// `minutes_since_last_ok` 传进来(store 不持时钟)。
///
/// 判据顺序是有意的:先看开关、再看配置完不完整、再看指纹、最后才看时间。
/// 「配置没填完」与「指纹没变」在 UI 上要说不同的话,混成一条的话状态栏只能
/// 报「备份失败」,把真正的原因吃掉。
pub fn should_upload(cfg: &CloudConfig, now_fingerprint: &str, minutes_since_last_ok: u64) -> bool {
    if !cfg.enabled {
        return false;
    }
    if cfg.endpoint.is_empty()
        || cfg.bucket.is_empty()
        || cfg.access_key_id.is_empty()
        || cfg.secret_sealed.is_empty()
    {
        return false;
    }
    if cfg.last_fingerprint == now_fingerprint {
        return false;
    }
    minutes_since_last_ok >= u64::from(cfg.interval_min)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cloud_config_file_is_not_in_the_pack_whitelist() {
        assert!(
            !crate::portable::TOP_LEVEL_FILES.contains(&CLOUD_FILE),
            "cloud.toml 进了迁移包白名单 —— 导入一份包会把本机的 AK/SK 与游标覆盖掉"
        );
        assert!(
            crate::portable::entry_target(std::path::Path::new("/tmp/x"), CLOUD_FILE).is_none(),
            "entry_target 认了 cloud.toml —— 一个包就能改掉本机的云配置"
        );
    }

    /// 云端载荷**不带 layouts**(设计 D7):窗口几何与分屏树是本机属性,
    /// 且它是配置目录里变动最频繁的东西 —— 带上等于让「内容变了」近似恒真。
    #[test]
    fn the_cloud_payload_carries_no_layout_records() {
        let dir = tempfile::tempdir().expect("临时目录");
        std::fs::write(dir.path().join("sessions.toml"), b"x = 1").expect("写会话");
        let hd = crate::history::history_dir(dir.path());
        std::fs::create_dir_all(&hd).expect("建现场目录");
        std::fs::write(hd.join("123-4.toml"), b"y = 2").expect("写现场");

        let files = crate::portable::collect_top_level(dir.path());
        assert!(
            files
                .iter()
                .all(|f| !f.path.contains(crate::history::HISTORY_DIR)),
            "云端载荷带上了现场记录:{:?}",
            files.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
        // 反过来:本地迁移包**仍然要带**(F46-a 的语义没变)。
        let local = crate::portable::collect(dir.path());
        assert!(
            local
                .iter()
                .any(|f| f.path.contains(crate::history::HISTORY_DIR)),
            "本地迁移包不带现场了 —— 这是 F46-a 的回归,不是本切片该改的东西"
        );
    }

    /// 指纹必须**只由内容决定**:同样的内容算两次必须相等,否则定时器每一轮
    /// 都会认为「变了」,于是每 30 分钟推一份一模一样的包,N 份历史窗口当场
    /// 被自己刷光。
    #[test]
    fn the_fingerprint_is_stable_for_identical_content() {
        let a = vec![
            crate::portable::PackFile {
                path: "sessions.toml".into(),
                body: "AAA".into(),
            },
            crate::portable::PackFile {
                path: "settings.toml".into(),
                body: "BBB".into(),
            },
        ];
        assert_eq!(fingerprint(&a, b"secret"), fingerprint(&a, b"secret"));
    }

    /// 改任何一个文件的内容,指纹必须变 —— 否则那个文件的改动永远推不上去,
    /// 而且没有任何报错。
    #[test]
    fn the_fingerprint_changes_when_any_part_changes() {
        let base = vec![crate::portable::PackFile {
            path: "sessions.toml".into(),
            body: "AAA".into(),
        }];
        let base_fp = fingerprint(&base, b"secret");

        let mut body_changed = base.clone();
        body_changed[0].body = "AAB".into();
        assert_ne!(
            fingerprint(&body_changed, b"secret"),
            base_fp,
            "正文变了指纹没变"
        );

        let mut path_changed = base.clone();
        path_changed[0].path = "settings.toml".into();
        assert_ne!(
            fingerprint(&path_changed, b"secret"),
            base_fp,
            "文件名变了指纹没变"
        );

        // **两份密文必须等长**。原先写的是 `b"secret"`(6) vs `b"other"`(5),
        // 长度一不同,光靠长度前缀就把它们分开了 —— 于是「把 `h.update(secrets)`
        // 整句删掉」这个真缺陷照样全绿。实测过这条变异在等长之前杀不掉。
        // 症状:密文改了但字节数没变(vault 换个 nonce 重写就是这样),
        // 指纹认为「没变」,这次改动永远推不上去且零报错。
        assert_ne!(fingerprint(&base, b"secreT"), base_fp, "密文变了指纹没变");
    }

    /// 长度前缀**单独守一条**。
    ///
    /// 上一条测试杀不掉「把三处长度前缀删了」这个变异:`sessions.toml`+`AAA`
    /// 与 `settings.toml`+`AAA` 直接拼起来本来就不同,改动照样被看见。真正
    /// 的漏洞是**边界歧义** —— 不加前缀时 `("a","bc")` 与 `("ab","c")` 喂进
    /// 哈希的字节完全一样。症状:把一段内容从一个文件挪到另一个文件,指纹
    /// 不变,这次改动永远推不上去且零报错。
    #[test]
    fn the_fingerprint_separates_the_name_from_the_body() {
        let a = vec![crate::portable::PackFile {
            path: "a".into(),
            body: "bc".into(),
        }];
        let b = vec![crate::portable::PackFile {
            path: "ab".into(),
            body: "c".into(),
        }];
        assert_ne!(
            fingerprint(&a, b"s"),
            fingerprint(&b, b"s"),
            "名字与正文的边界没进哈希 —— 内容在文件之间搬家会被漏掉"
        );
    }

    /// 文件顺序不该影响指纹。`collect_top_level` 今天是定序的,但指纹是
    /// 「内容一样吗」的判据,让它依赖一个可能被重构掉的顺序,等于埋一颗
    /// 「某次无关重构之后每轮都重推」的雷。
    #[test]
    fn the_fingerprint_does_not_depend_on_file_order() {
        let a = vec![
            crate::portable::PackFile {
                path: "sessions.toml".into(),
                body: "AAA".into(),
            },
            crate::portable::PackFile {
                path: "settings.toml".into(),
                body: "BBB".into(),
            },
        ];
        let b = vec![a[1].clone(), a[0].clone()];
        assert_eq!(fingerprint(&a, b"s"), fingerprint(&b, b"s"));
    }

    fn cfg() -> CloudConfig {
        CloudConfig {
            enabled: true,
            endpoint: "https://oss-cn-hangzhou.aliyuncs.com".into(),
            region: "cn-hangzhou".into(),
            bucket: "my-bucket".into(),
            prefix: "mullion/".into(),
            path_style: false,
            keep: 20,
            interval_min: 30,
            socks5: String::new(),
            access_key_id: String::new(),
            secret_sealed: String::new(),
            last_fingerprint: String::new(),
            last_seq: 0,
            last_ok_at: String::new(),
            corrupt: false,
        }
    }

    #[test]
    fn a_config_round_trips_through_the_file() {
        let dir = tempfile::tempdir().expect("临时目录");
        save(dir.path(), &cfg()).expect("写");
        assert_eq!(load(dir.path()), cfg());
    }

    /// 没有文件时给一份**关着的**默认配置 —— 不是「开着但字段是空的」。
    /// 后者会让定时器每一轮都尝试连一个空 endpoint,状态栏一直报错。
    #[test]
    fn a_missing_file_yields_a_disabled_default() {
        let dir = tempfile::tempdir().expect("临时目录");
        let c = load(dir.path());
        assert!(!c.enabled, "默认必须是关着的");
        assert_eq!(c.prefix, DEFAULT_PREFIX);
        assert_eq!(c.keep, DEFAULT_KEEP);
    }

    /// 坏文件**不许当成默认值**(F247/F248 的「整份覆盖」缺陷族):
    /// 照 default 兜底的话,下一次 `save` 会把用户手打的 endpoint/bucket
    /// 连同 AK/SK 一起抹掉,而这一切零报错。
    #[test]
    fn a_corrupt_file_is_not_silently_replaced_by_defaults() {
        let dir = tempfile::tempdir().expect("临时目录");
        std::fs::write(dir.path().join(CLOUD_FILE), "这不是 toml { [").expect("写坏文件");
        let c = load(dir.path());
        assert!(!c.enabled, "读不懂的配置必须当成关着的");
        let err =
            save(dir.path(), &c).expect_err("读坏之后还允许回写 —— 那会把用户的 AK/SK 静默抹掉");
        // 文案里必须有**目录**,不能只有文件名。断言写成 `contains(CLOUD_FILE)`
        // 是恒绿的 —— 老文案里就字面写着 "cloud.toml"。
        assert!(
            err.to_string().contains(&dir.path().display().to_string()),
            "错误文案里没有目录,用户在 %APPDATA% 底下找不到该删哪个文件:{err}"
        );
        // 标记**不许寄生在数据字段上**:设置弹窗把 `endpoint` 直接绑到文本框,
        // 用户改一下就把标记冲掉,`save` 当场放行、`secret_sealed` 被抹。
        assert!(
            c.endpoint.is_empty(),
            "损坏标记污染了 endpoint:{:?} —— 它会出现在设置弹窗的输入框里,\
             而用户改掉它就等于把守护关掉了",
            c.endpoint
        );
    }

    /// 坏标记**不能落盘**。写出去的话下次 `load` 会把一份好文件读成坏的,
    /// 于是云备份从此永久拒绝回写,且没有任何办法自愈。
    #[test]
    fn the_corrupt_mark_never_reaches_the_file() {
        let dir = tempfile::tempdir().expect("临时目录");
        save(dir.path(), &cfg()).expect("写");
        let text = std::fs::read_to_string(dir.path().join(CLOUD_FILE)).expect("读");
        assert!(!text.contains("corrupt"), "损坏标记落盘了:{text}");
    }

    /// 游标(`last_seq` / `last_fingerprint`)是写在这个文件里的,而这个文件
    /// 不进包 —— 这条钉住的是「有人以后为了省事把游标挪进 settings.toml」。
    #[test]
    fn the_cursor_lives_in_the_file_that_the_pack_cannot_touch() {
        let dir = tempfile::tempdir().expect("临时目录");
        let mut c = cfg();
        c.last_seq = 42;
        c.last_fingerprint = "deadbeef".into();
        save(dir.path(), &c).expect("写");
        let text = std::fs::read_to_string(dir.path().join(CLOUD_FILE)).expect("读");
        assert!(text.contains("last_seq"), "游标没落在 cloud.toml 里");
        let settings = dir.path().join(crate::settings::SETTINGS_FILE);
        assert!(
            !settings.exists()
                || !std::fs::read_to_string(&settings)
                    .unwrap()
                    .contains("last_seq"),
            "游标漏进了 settings.toml —— 那个文件会被导入的包整份替换掉"
        );
    }

    /// 缺字段**必须**按默认值读进来,不算损坏 —— 这是**有意的**版本兼容合约。
    ///
    /// 复核时有人提议把关键字段改成「缺了就判 corrupt」,理由是「缺
    /// `secret_sealed` 会被 `save` 写回空串」。那条修法是错的:我们每个切片都在
    /// 往这个结构体加字段(`socks5` 就是这个切片加的),一旦缺字段判损坏,
    /// **上一版写出来的 `cloud.toml` 在这一版眼里全是坏的** —— 用户升级一次就得
    /// 把云配置重填一遍,而且报的是「读不懂,拒绝回写」这种看不出原因的话。
    ///
    /// 至于「写回空串会抹掉 SK」:那一行已经**不在盘上**了,`load` 本来就没东西
    /// 可还原。这跟 F247/F248 不一样 —— 那里的前提是「另一份内存副本手上有更新
    /// 的数据」,这里没有第二个写者(`vault::write_atomic` 是 tmp+rename,我们
    /// 自己写不出半截文件)。
    ///
    /// 这条同时补上三个 serde 默认函数的覆盖:上面那条「没有文件」走的是
    /// `CloudConfig::default()`,**碰不到 `#[serde(default = "..")]` 指的那几个
    /// 函数**,于是把 `default_interval()` 改成返回 999999 也能全绿(复核实测过)。
    #[test]
    fn a_file_written_by_an_older_version_still_loads_with_defaults() {
        let dir = tempfile::tempdir().expect("临时目录");
        // 只写两行 —— 模拟一份「还不认识后来才加的字段」的老文件。
        std::fs::write(
            dir.path().join(CLOUD_FILE),
            "enabled = true\nbucket = \"b\"\n",
        )
        .expect("写老文件");
        let c = load(dir.path());
        assert!(
            c.enabled,
            "缺字段被当成了损坏 —— 老版本写的配置升级一次就全废了"
        );
        assert_eq!(c.bucket, "b");
        assert_eq!(c.prefix, DEFAULT_PREFIX, "prefix 的 serde 默认没接上");
        assert_eq!(c.keep, DEFAULT_KEEP, "keep 的 serde 默认没接上");
        assert_eq!(
            c.interval_min, DEFAULT_INTERVAL_MIN,
            "interval_min 的 serde 默认没接上 —— 定时器周期会变成一个谁也没写过的值"
        );
    }

    /// SK 落盘必须是密文。这条同 `tests/f70_no_plaintext.rs` 的姿态:
    /// 在**文件字节**里搜明文,而不是相信调用链。
    #[test]
    fn the_secret_key_never_hits_the_disk_in_the_clear() {
        let dir = tempfile::tempdir().expect("临时目录");
        let mut v = crate::vault::Vault::open(
            dir.path().to_path_buf(),
            &crate::master_key::InMemoryKey([5u8; 32]),
        )
        .expect("开库");
        v.set_master_password("hunter2").expect("设主密码");

        let mut c = cfg();
        set_secret_key(&mut c, &v, "TOP-SECRET-SK-VALUE").expect("封 SK");
        save(dir.path(), &c).expect("写");

        let bytes = std::fs::read(dir.path().join(CLOUD_FILE)).expect("读");
        assert!(
            !String::from_utf8_lossy(&bytes).contains("TOP-SECRET-SK-VALUE"),
            "SK 明文落到了 cloud.toml 里"
        );
        assert_eq!(secret_key(&c, &v).expect("解 SK"), "TOP-SECRET-SK-VALUE");
    }

    /// **改主密码必须连带重封 `cloud.toml`**(设计 D12)。
    ///
    /// 不重封的症状:AK/SK 当场解不开,云备份静默失效,而错误要等到几十分钟后
    /// 的一次定时上传才冒出来 —— 那时候用户早就不记得自己改过主密码了。
    #[test]
    fn changing_the_master_password_reseals_the_cloud_secret() {
        let dir = tempfile::tempdir().expect("临时目录");
        let mut v = crate::vault::Vault::open(
            dir.path().to_path_buf(),
            &crate::master_key::InMemoryKey([5u8; 32]),
        )
        .expect("开库");
        v.set_master_password("old").expect("设旧密码");

        let mut c = cfg();
        set_secret_key(&mut c, &v, "SK-VALUE").expect("封");
        save(dir.path(), &c).expect("写");

        v.set_master_password("new").expect("改密码");

        let after = load(dir.path());
        assert_eq!(
            secret_key(&after, &v).expect("改完密码之后应该还解得开"),
            "SK-VALUE",
            "改主密码没有重封 cloud.toml —— AK/SK 从此解不开且零报错"
        );
    }

    /// `clear_master_password` 走同一条路:退回钥匙串方案之后,SK 必须仍然
    /// 解得开(它改用钥匙串密钥封),否则用户只是「取消了主密码」,云配置
    /// 却连带坏掉。
    #[test]
    fn clearing_the_master_password_also_reseals_the_cloud_secret() {
        let dir = tempfile::tempdir().expect("临时目录");
        let ks = crate::master_key::InMemoryKey([5u8; 32]);
        let mut v = crate::vault::Vault::open(dir.path().to_path_buf(), &ks).expect("开库");
        v.set_master_password("old").expect("设密码");
        let mut c = cfg();
        set_secret_key(&mut c, &v, "SK-VALUE").expect("封");
        save(dir.path(), &c).expect("写");

        v.clear_master_password(&ks).expect("取消主密码");

        let after = load(dir.path());
        assert_eq!(secret_key(&after, &v).expect("仍应解得开"), "SK-VALUE");
    }

    /// 重封失败之后的**自愈路径**:重填一次 SK 就好。
    ///
    /// 这条钉的是 `StoreError::CloudReseal` 那句文案里让用户做的事真的有效。
    /// 文案说「去设置里把 Access Key Secret 重填一遍」,而 `set_secret_key`
    /// 若用的不是**当前**密钥,那句话就是在骗人 —— 用户照做之后仍然解不开,
    /// 从此再没有任何自愈路径(复核登记的 I1:第二次改密码时旧密文解不开,
    /// 被 `.ok()` 吞成 `None`,重封永远不再发生)。
    ///
    /// 自证会变红:让 `set_secret_key` 用一把跟 `secret_key` 不同的密钥。
    #[test]
    fn refilling_the_secret_key_recovers_from_a_reseal_that_never_happened() {
        let dir = tempfile::tempdir().expect("临时目录");
        let ks = crate::master_key::InMemoryKey([5u8; 32]);
        let mut v = crate::vault::Vault::open(dir.path().to_path_buf(), &ks).expect("开库");
        let mut c = cfg();
        set_secret_key(&mut c, &v, "SK-VALUE").expect("封");

        // **故意不把 c 写进 cloud.toml** —— 于是改密码时 `take_cloud_secret_plain`
        // 读不到东西、重封不会发生,手上这份 `c` 正好就是「一次失败过的 reseal
        // 留下来的状态」:密文用旧密钥封,而 vault 已经换了新密钥。
        v.set_master_password("new").expect("改密码");
        assert!(
            matches!(secret_key(&c, &v), Err(StoreError::Crypto)),
            "构造失败:这份密文本该已经解不开了,否则下面测的是个假场景"
        );

        set_secret_key(&mut c, &v, "SK-VALUE").expect("重填");
        assert_eq!(
            secret_key(&c, &v).expect("重填之后必须解得开"),
            "SK-VALUE",
            "重填 SK 也救不回来 —— 那条错误文案是在骗用户"
        );
    }

    /// 序号必须**零填充定宽**。不填充的话字典序是 `1, 10, 2` ——
    /// 而「最新那份」= List 结果里序号最大的那条,靠的正是字典序。
    #[test]
    fn the_sequence_number_is_zero_padded_so_lexical_order_equals_numeric_order() {
        let a = object_key("mullion/", 2, "20260915T101500Z");
        let b = object_key("mullion/", 10, "20260915T101500Z");
        assert!(a < b, "字典序与数值序不一致:{a} 应该排在 {b} 前面");
    }

    #[test]
    fn a_key_round_trips_through_parse() {
        let k = object_key("mullion/", 42, "20260915T101500Z");
        assert_eq!(parse_seq("mullion/", &k), Some(42));
    }

    /// 别人往同一个前缀下丢了别的文件时,不认识的键**跳过**而不是
    /// 让整次上传失败 —— bucket 是用户自己的,里头有什么我们管不着。
    #[test]
    fn a_key_we_do_not_recognise_is_skipped_not_fatal() {
        assert_eq!(parse_seq("mullion/", "mullion/readme.txt"), None);
        assert_eq!(parse_seq("mullion/", "other/000001-x.mpk"), None);
    }

    /// 下一个序号 = 已有的最大值 + 1。**空列表从 1 起**,不是 0 ——
    /// 0 与「没推过」的游标初值撞在一起,分不出「从没推过」和「推过第 0 份」。
    #[test]
    fn the_next_sequence_is_one_past_the_largest_existing() {
        assert_eq!(next_seq("mullion/", &[]), 1);
        assert_eq!(
            next_seq(
                "mullion/",
                &[
                    "mullion/000001-a.mpk".to_string(),
                    "mullion/000007-b.mpk".to_string(),
                    "mullion/000003-c.mpk".to_string(),
                ]
            ),
            8
        );
    }

    /// 一份**填完整了**的配置。
    ///
    /// **`should_upload` 的测试一律用这个,不要用 `cfg()`。** `cfg()` 的
    /// `access_key_id` / `secret_sealed` 是空的(前一个任务那几条测的是读写往返,
    /// 不需要填),而 `should_upload` 的第一道闸就是「配置完不完整」——
    /// 拿 `cfg()` 去测的话,`a_changed_fingerprint_...` 会直接红,而
    /// `an_unchanged_fingerprint_...` 会**恒绿**:它返回 false 是因为「没填完」,
    /// 跟指纹判据一点关系都没有,把指纹那一条整个删掉它照样绿。
    fn ready_cfg() -> CloudConfig {
        CloudConfig {
            access_key_id: "AK".into(),
            secret_sealed: "sealed".into(),
            ..cfg()
        }
    }

    /// 先钉住 `ready_cfg` 真的是「会推」的那一档 —— 否则下面每一条
    /// `assert!(!should_upload(..))` 都可能是因为别的原因恒假。
    #[test]
    fn the_ready_config_is_actually_uploadable() {
        assert!(
            should_upload(&ready_cfg(), "brand-new", 999),
            "ready_cfg 本身就推不动 —— 下面那几条「不推」的断言全都测不到自己想测的东西"
        );
    }

    /// 关着的时候永远不推 —— 哪怕内容变了。
    #[test]
    fn a_disabled_config_never_uploads() {
        let mut c = ready_cfg();
        c.enabled = false;
        assert!(!should_upload(&c, "new-fp", 999));
    }

    /// 指纹没变就不推。**这是保住 N 份历史窗口的全部** —— 不判的话每 30 分钟
    /// 推一份一模一样的包,20 份历史会在 10 小时内被自己刷光。
    #[test]
    fn an_unchanged_fingerprint_does_not_upload() {
        let mut c = ready_cfg();
        c.last_fingerprint = "same".into();
        assert!(!should_upload(&c, "same", 999));
    }

    #[test]
    fn a_changed_fingerprint_uploads_once_the_interval_has_passed() {
        let mut c = ready_cfg();
        c.last_fingerprint = "old".into();
        c.interval_min = 30;
        assert!(!should_upload(&c, "new", 29), "还没到点就推了");
        assert!(should_upload(&c, "new", 30), "到点了却不推");
    }

    /// 从没推过(指纹为空)时,**到点就推第一份**。
    /// 若写成「指纹为空 → 不推」,开了开关的用户永远等不到第一份备份。
    #[test]
    fn a_config_that_never_uploaded_still_gets_its_first_push() {
        let mut c = ready_cfg();
        c.last_fingerprint = String::new();
        assert!(should_upload(&c, "first", 30));
    }

    /// 配置不全(endpoint/bucket/AK/SK 任一为空)时不推 —— 推了也只会拿到一条
    /// 网络错误,而状态栏会把它报成「备份失败」,掩盖真正的原因是「没填完」。
    ///
    /// **四个字段逐个试**,不是只试一个:少判任一个的症状都一样(开着开关、
    /// 每轮都发一次注定 403 的请求),而只试一个的话漏掉的那几个零报错。
    #[test]
    fn an_incomplete_config_does_not_upload() {
        for spoil in ["endpoint", "bucket", "ak", "sk"] {
            let mut c = ready_cfg();
            match spoil {
                "endpoint" => c.endpoint = String::new(),
                "bucket" => c.bucket = String::new(),
                "ak" => c.access_key_id = String::new(),
                _ => c.secret_sealed = String::new(),
            }
            assert!(!should_upload(&c, "fp", 999), "{spoil} 为空时不该推");
        }
    }
}
