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
}
