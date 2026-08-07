//! 解析类 ssh 命令行:`user@host [-p PORT] [-i KEYPATH]`。
//! 自己写一小段,不引 clap(YAGNI)。F2 的 ssh_config 解析仍在范围外。

use std::path::PathBuf;

use mullion_ssh::config::{AuthMethod, SshConfig};

/// 从参数(不含 argv[0])解析连接配置。cols/rows 先给占位默认,
/// 窗口出来后由 window_change 校正到真实尺寸。
pub fn parse_args(args: &[String]) -> Result<SshConfig, String> {
    let mut target: Option<String> = None;
    let mut port: u16 = 22;
    let mut key: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-p" => {
                i += 1;
                let val = args.get(i).ok_or("-p 缺少端口值")?;
                port = val.parse().map_err(|_| format!("端口非法: {val}"))?;
            }
            "-i" => {
                i += 1;
                let val = args.get(i).ok_or("-i 缺少密钥路径")?;
                key = Some(PathBuf::from(val));
            }
            other if other.starts_with('-') => return Err(format!("未知参数: {other}")),
            other => {
                if target.is_some() {
                    return Err(format!("多余参数: {other}"));
                }
                target = Some(other.to_string());
            }
        }
        i += 1;
    }
    let target = target.ok_or("缺少 user@host")?;
    let (user, host) = target.split_once('@').ok_or("目标须形如 user@host")?;
    if user.is_empty() || host.is_empty() {
        return Err("user 和 host 都不能为空".into());
    }
    // `-i` 仍收路径(命令行的既有语义),但读成正文再交给 ssh 层 ——
    // `AuthMethod` v5 起只认私钥内容,读文件是调用方的事。
    let auth = match key {
        Some(path) => AuthMethod::PublicKey {
            key_data: std::fs::read_to_string(&path)
                .map_err(|e| format!("读私钥失败 {}: {e}", path.display()))?,
            passphrase: None,
        },
        None => AuthMethod::Agent,
    };
    Ok(SshConfig {
        host: host.to_string(),
        port,
        user: user.to_string(),
        auth,
        cols: 80,
        rows: 24,
        term: "xterm-256color".to_string(),
        hops: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_full_target() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("id_test");
        std::fs::write(&key, "KEYBODY").unwrap();
        let cfg = parse_args(&v(&[
            "user@example.com",
            "-p",
            "22",
            "-i",
            key.to_str().unwrap(),
        ]))
        .unwrap();
        assert_eq!(cfg.user, "user");
        assert_eq!(cfg.host, "example.com");
        assert_eq!(cfg.port, 22);
        assert_eq!(cfg.term, "xterm-256color");
        match cfg.auth {
            AuthMethod::PublicKey {
                key_data,
                passphrase,
            } => {
                // 传给 ssh 层的必须是私钥**正文**,不是路径。
                assert_eq!(key_data, "KEYBODY");
                assert!(passphrase.is_none());
            }
            _ => panic!("给了 -i 应走 PublicKey"),
        }
    }

    /// `-i` 指了个不存在的文件时要当场报错,而不是揣着空私钥去连、
    /// 最后在 ssh 层给出「解析私钥失败」这种指不到原因的报错。
    #[test]
    fn unreadable_key_file_fails_at_parse_time_naming_the_path() {
        let err = parse_args(&v(&["u@h", "-i", "/no/such/key"])).unwrap_err();
        assert!(err.contains("/no/such/key"), "报错要点名路径: {err}");
    }

    #[test]
    fn defaults_port_22_and_agent_without_key() {
        let cfg = parse_args(&v(&["user@host"])).unwrap();
        assert_eq!(cfg.port, 22);
        assert!(
            matches!(cfg.auth, AuthMethod::Agent),
            "无 -i 应回退 ssh-agent"
        );
    }

    #[test]
    fn missing_target_is_error() {
        assert!(parse_args(&v(&["-p", "22"])).is_err());
    }

    #[test]
    fn target_without_at_is_error() {
        assert!(parse_args(&v(&["justhost"])).is_err());
    }

    #[test]
    fn bad_port_is_error() {
        assert!(parse_args(&v(&["u@h", "-p", "notnum"])).is_err());
    }
}
