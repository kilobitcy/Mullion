//! 已知主机密钥指纹的持久化(F3 / TOFU)。
//!
//! 自有 TOML 格式,**不复用** OpenSSH 的 `known_hosts`:我们只需要
//! 「host → (算法, SHA256 指纹)」这一条映射,不做 hashed hostname、通配、证书、
//! `@revoked` 标记。解析别人的格式=继承别人的全部边界情况,得不偿失。
//!
//! 明文存储:指纹是公开信息,泄露无害;加密只会挡住用户自己拿
//! `ssh-keygen -lf` 核对,反而降低安全性。
//!
//! 零 async、零 UI——架构不变量:store 只做同步 IO。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::StoreError;
use crate::vault::write_atomic;

/// 一条主机密钥记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostKeyEntry {
    /// 密钥算法,如 `ssh-ed25519`。只用于弹窗展示与人工核对。
    pub algo: String,
    /// `SHA256:<base64-unpadded>` —— 与 `ssh-keygen -lf` 输出的第二列同格式,
    /// 用户可以直接肉眼比对。
    pub fingerprint: String,
}

/// 磁盘上的文件结构。单独一层是为了将来加字段(如 `version`)不破坏格式。
#[derive(Debug, Default, Serialize, Deserialize)]
struct KnownHostsToml {
    #[serde(default)]
    hosts: BTreeMap<String, HostKeyEntry>,
}

/// 已知主机表 + 它的落盘位置。`Default`(path=None)= 纯内存表,不落盘。
#[derive(Debug, Default)]
pub struct KnownHostsFile {
    path: Option<PathBuf>,
    hosts: BTreeMap<String, HostKeyEntry>,
    /// 读到的文件解析失败。此时 `save()` 必须先把原文件备份成 `.bak` 再写——
    /// 直接覆盖等于把用户全部指纹静默清空,以后每台主机都退化成「首次连接」,
    /// 而这恰好是 MITM 最想要的状态。
    corrupt: bool,
}

impl KnownHostsFile {
    /// 从目录加载 `known_hosts.toml`。文件不存在 = 空表(首次运行,不是错误)。
    ///
    /// 解析失败也不是错误:标记 corrupt + 当空表用,用户还能继续连(会重新弹
    /// TOFU 窗),而不是被一个坏文件锁死在门外。
    ///
    /// 只有 `NotFound` 才当「文件不存在」处理。其余 IO 错误(非 UTF-8、
    /// 权限拒绝等)一律保守当 corrupt——因为此时文件**确实存在**,内容拿不到
    /// 不代表内容是空的;如果按「不存在」处理,后续 `save()` 会直接覆盖,
    /// 等于把用户已有的全部指纹静默清空(见下方 `corrupt` 字段注释)。
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("known_hosts.toml");
        let (hosts, corrupt) = match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<KnownHostsToml>(&text) {
                Ok(parsed) => (parsed.hosts, false),
                Err(_) => (BTreeMap::new(), true),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (BTreeMap::new(), false),
            Err(_) => (BTreeMap::new(), true),
        };
        Self {
            path: Some(path),
            hosts,
            corrupt,
        }
    }

    /// 磁盘上的文件是否解析失败(上层据此提示用户)。
    pub fn is_corrupt(&self) -> bool {
        self.corrupt
    }

    /// 不对 `host` 做任何规范化(大小写/端口/IPv6 方括号写法各不同就各开一条
    /// 独立记录)。调用方(未来 host-key 校验层)必须自己保证 key 一致,否则
    /// 同一台主机会被当成两台,TOFU 保护形同虚设。
    pub fn get(&self, host: &str) -> Option<&HostKeyEntry> {
        self.hosts.get(host)
    }

    /// 记进内存表(**不落盘**,落盘由调用方决定时机)。返回被顶掉的旧记录。
    ///
    /// 同 `get`:不对 `host` 做规范化,一字不差地当 map key 用。
    pub fn record(&mut self, host: &str, entry: HostKeyEntry) -> Option<HostKeyEntry> {
        self.hosts.insert(host.to_string(), entry)
    }

    /// 落盘。`path=None`(纯内存表)是 no-op。
    /// corrupt 时先把原文件另存 `known_hosts.toml.bak`,再写新文件——注意如果
    /// 连续两次都是从 corrupt 状态 save,第二次的 `.bak` 会**覆盖**第一次的
    /// (`fs::rename` 语义:目标已存在则替换),只丢最早一次的取证字节,不影响
    /// 当前活跃表。
    pub fn save(&mut self) -> Result<(), StoreError> {
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        if self.corrupt {
            match std::fs::rename(&path, path.with_extension("toml.bak")) {
                Ok(()) => {}
                // 源文件已被外部删除 = 没有需要备份的东西,继续写新表;
                // 若在这里 `?` 掉,corrupt 永远清不了,这个实例往后每次 save 都失败。
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            self.corrupt = false;
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(&KnownHostsToml {
            hosts: self.hosts.clone(),
        })?;
        write_atomic(&path, text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(fp: &str) -> HostKeyEntry {
        HostKeyEntry {
            algo: "ssh-ed25519".into(),
            fingerprint: fp.into(),
        }
    }

    #[test]
    fn record_then_save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let mut kh = KnownHostsFile::load(dir.path());
        assert!(kh.get("h1").is_none(), "空目录应是空表");

        kh.record("h1", entry("SHA256:AAAA"));
        kh.save().unwrap();

        let re = KnownHostsFile::load(dir.path());
        assert_eq!(re.get("h1"), Some(&entry("SHA256:AAAA")));
        assert!(!re.is_corrupt());
    }

    #[test]
    fn corrupt_file_is_treated_as_empty_and_backed_up_not_clobbered() {
        // F3:文件坏了不能静默清空——否则用户的全部指纹凭空消失,
        // 每台主机都变回「首次连接」,MITM 再也不会被拦下。
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("known_hosts.toml"), b"this is not toml {{{").unwrap();

        let mut kh = KnownHostsFile::load(dir.path());
        assert!(kh.is_corrupt());
        assert!(kh.get("h1").is_none(), "损坏文件当空表,不该凭空造出条目");

        kh.record("h1", entry("SHA256:BBBB"));
        kh.save().unwrap();

        let bak = std::fs::read_to_string(dir.path().join("known_hosts.toml.bak")).unwrap();
        assert!(bak.contains("not toml"), "原文件内容必须完整保留在 .bak 里");
        assert_eq!(
            KnownHostsFile::load(dir.path()).get("h1"),
            Some(&entry("SHA256:BBBB"))
        );
    }

    #[test]
    fn save_leaves_no_tmp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let mut kh = KnownHostsFile::load(dir.path());
        kh.record("h1", entry("SHA256:CCCC"));
        kh.save().unwrap();
        assert!(
            !dir.path().join("known_hosts.tmp").exists(),
            "原子写的临时文件必须被 rename 掉"
        );
    }

    #[test]
    fn record_returns_the_replaced_entry() {
        let mut kh = KnownHostsFile::default();
        assert_eq!(kh.record("h1", entry("SHA256:AAAA")), None);
        assert_eq!(
            kh.record("h1", entry("SHA256:BBBB")),
            Some(entry("SHA256:AAAA")),
            "覆盖时要能拿回旧值,上层要拿它做变更提示"
        );
    }

    #[test]
    fn unreadable_file_is_corrupt_not_missing_so_save_cannot_clobber_it() {
        // 非 UTF-8 字节:fs::read_to_string 返回 InvalidData,不是 NotFound。
        // 文件明明存在,若被当成「不存在」,save() 会跳过备份直接覆盖——
        // 原始字节永久丢失,这正是 Critical 缺陷复现的场景。
        let dir = tempfile::tempdir().unwrap();
        let raw: &[u8] = &[0xff, 0xfe, 0x00, 0x80, b'x', b'y', b'z'];
        std::fs::write(dir.path().join("known_hosts.toml"), raw).unwrap();

        let mut kh = KnownHostsFile::load(dir.path());
        assert!(kh.is_corrupt(), "非 UTF-8 必须判 corrupt,不能当空表/不存在");

        kh.record("h1", entry("SHA256:DEAD"));
        kh.save().unwrap();

        let bak = std::fs::read(dir.path().join("known_hosts.toml.bak")).unwrap();
        assert_eq!(bak, raw, "备份必须是原始字节,不能丢/不能转码");
    }

    #[test]
    fn corrupt_save_survives_source_file_deleted_between_load_and_save() {
        // corrupt=true 时 save() 先 rename 备份;如果源文件在 load 之后、
        // save 之前被外部删掉,rename 会失败(NotFound)。这不该让 corrupt
        // 永远清不掉——否则这个实例往后每次 save 都失败,指纹永久存不下去。
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("known_hosts.toml"), b"not toml {{{").unwrap();
        let mut kh = KnownHostsFile::load(dir.path());
        assert!(kh.is_corrupt());

        std::fs::remove_file(dir.path().join("known_hosts.toml")).unwrap();

        kh.record("h1", entry("SHA256:FEED"));
        let r = kh.save();
        assert!(r.is_ok(), "源文件已被删除时 save 仍应成功: {r:?}");
        assert!(!kh.is_corrupt(), "成功写入新表后 corrupt 必须清零");

        assert_eq!(
            KnownHostsFile::load(dir.path()).get("h1"),
            Some(&entry("SHA256:FEED"))
        );
    }

    #[test]
    fn default_in_memory_table_save_is_ok_and_keeps_the_entry() {
        // path=None 时 save() 走 early-return。「没在磁盘上留下东西」这一点靠
        // 类型保证(save() 唯一的写入路径来自 self.path,为 None 就无路可写),
        // 测试里没有可锚定的目录可断言——硬造一个无关 tempdir 去断言「它是空的」
        // 是空转的:实现即使写到别处,那个 tempdir 也照样空。
        let mut kh = KnownHostsFile::default();
        kh.record("h1", entry("SHA256:AAAA"));
        assert!(kh.save().is_ok(), "纯内存表 save() 是 no-op,不该报错");
        assert_eq!(kh.get("h1"), Some(&entry("SHA256:AAAA")));
    }

    #[test]
    fn empty_or_comment_only_toml_is_not_corrupt_but_wrong_schema_is() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("known_hosts.toml"), b"# just a comment\n").unwrap();
        let kh = KnownHostsFile::load(dir.path());
        assert!(!kh.is_corrupt(), "空/纯注释 TOML 是合法空表,不是损坏");
        assert!(kh.get("h1").is_none());
        drop(kh);

        std::fs::write(
            dir.path().join("known_hosts.toml"),
            b"hosts = [\"a\", \"b\"]\n",
        )
        .unwrap();
        let kh = KnownHostsFile::load(dir.path());
        assert!(
            kh.is_corrupt(),
            "hosts 类型对不上 schema(数组非表)必须判 corrupt"
        );
    }
}
