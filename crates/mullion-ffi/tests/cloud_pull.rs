//! F346-b:ffi 端到端 —— 假 S3 → pull_latest → vault/。判据测试在 store / cloud。
#[path = "common/demo_pack.rs"]
mod demo_pack;
#[path = "common/fake_s3.rs"]
mod fake_s3;

use demo_pack::{demo_pack, DEMO_PASSWORD, DEMO_PROJECT};
use fake_s3::FakeS3;
use mullion_ffi::{
    load_cloud_form, pull_latest, reset_cloud_config, save_cloud_form, CloudForm, FfiError,
};

const KEY: [u8; 32] = [7; 32];
const NOW: &str = "20261112T000000Z";
const LATEST: &str = "mullion/000002-20261112T000000Z.mpk";

fn cloud_object() -> Vec<u8> {
    mullion_store::seal_secrets(demo_pack().as_bytes(), DEMO_PASSWORD).unwrap()
}

/// 两份备份,旧的那份是垃圾:拉错了就会报包损坏。
fn bucket() -> FakeS3 {
    let mut s = FakeS3::with_object(LATEST, cloud_object());
    s.objects.insert(
        "mullion/000001-20261111T000000Z.mpk".into(),
        b"stale".to_vec(),
    );
    s.objects.insert("mullion/readme.txt".into(), b"x".to_vec());
    s
}

fn form(port: u16) -> CloudForm {
    CloudForm {
        endpoint: format!("http://127.0.0.1:{port}"),
        region: "us-east-1".into(),
        bucket: "b".into(),
        prefix: "mullion/".into(),
        path_style: true,
        socks5: String::new(),
        access_key_id: "AK".into(),
        secret_key: "SK".into(),
        passphrase: DEMO_PASSWORD.into(),
        has_secret: false,
        has_passphrase: false,
        is_corrupt: false,
    }
}

struct Dirs {
    _tmp: tempfile::TempDir,
    vault: String,
    cloud: String,
}

fn dirs() -> Dirs {
    let tmp = tempfile::tempdir().unwrap();
    let vault = tmp.path().join("vault").to_string_lossy().into_owned();
    let cloud = tmp.path().join("cloud").to_string_lossy().into_owned();
    Dirs {
        _tmp: tmp,
        vault,
        cloud,
    }
}

fn pull(d: &Dirs) -> Result<mullion_ffi::PullSummary, FfiError> {
    pull_latest(d.vault.clone(), d.cloud.clone(), KEY.to_vec(), NOW.into())
}

#[test]
fn pulling_installs_the_latest_backup_and_never_writes() {
    let s3 = bucket().spawn("127.0.0.1:0");
    let d = dirs();
    save_cloud_form(d.cloud.clone(), KEY.to_vec(), form(s3.port)).unwrap();
    let s = pull(&d).unwrap();
    assert_eq!(s.key, LATEST);
    assert_eq!(s.import.projects, vec![DEMO_PROJECT.to_string()]);
    assert_eq!((s.import.kept, s.import.dropped), (3, 1));
    let seen = s3.seen.lock().unwrap().clone();
    assert!(
        seen.iter().all(|l| l.starts_with("GET ")),
        "发过非 GET 请求:{seen:?}"
    );
}

/// 设计 D3:`vault/` 坏了时拉取是唯一的自愈手段。
#[test]
fn pulling_repairs_a_broken_vault() {
    let s3 = bucket().spawn("127.0.0.1:0");
    let d = dirs();
    std::fs::create_dir_all(&d.vault).unwrap();
    std::fs::write(format!("{}/sessions.toml", d.vault), "坏 [").unwrap();
    std::fs::write(format!("{}/secrets.enc", d.vault), b"garbage").unwrap();
    save_cloud_form(d.cloud.clone(), KEY.to_vec(), form(s3.port)).unwrap();
    assert_eq!(
        pull(&d).unwrap().import.projects,
        vec![DEMO_PROJECT.to_string()]
    );
}

fn vault_bytes(d: &Dirs) -> Vec<(String, Vec<u8>)> {
    let mut v: Vec<_> = std::fs::read_dir(&d.vault)
        .map(|it| {
            it.flatten()
                .map(|e| {
                    (
                        e.file_name().to_string_lossy().into_owned(),
                        std::fs::read(e.path()).unwrap_or_default(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

#[test]
fn every_failure_leaves_the_vault_untouched() {
    let s3 = bucket().spawn("127.0.0.1:0");
    let d = dirs();
    save_cloud_form(d.cloud.clone(), KEY.to_vec(), form(s3.port)).unwrap();
    pull(&d).unwrap();
    let before = vault_bytes(&d);

    // 口令错
    let mut f = form(s3.port);
    f.passphrase = "nope".into();
    save_cloud_form(d.cloud.clone(), KEY.to_vec(), f).unwrap();
    assert!(matches!(pull(&d), Err(FfiError::WrongPassword)));
    assert_eq!(vault_bytes(&d), before);

    // GET 404(列举后被清理)
    let mut gone = bucket();
    gone.get_override = Some((404, "<Error><Code>NoSuchKey</Code></Error>".into()));
    let s3b = gone.spawn("127.0.0.1:0");
    save_cloud_form(d.cloud.clone(), KEY.to_vec(), form(s3b.port)).unwrap();
    let r = pull(&d);
    assert!(matches!(r, Err(FfiError::CloudBackupVanished)), "{r:?}");
    assert_eq!(vault_bytes(&d), before);

    // 403 签名错
    let mut denied = bucket();
    denied.get_override = Some((
        403,
        "<Error><Code>SignatureDoesNotMatch</Code></Error>".into(),
    ));
    let s3c = denied.spawn("127.0.0.1:0");
    save_cloud_form(d.cloud.clone(), KEY.to_vec(), form(s3c.port)).unwrap();
    assert!(matches!(pull(&d), Err(FfiError::CloudAuth)));
    assert_eq!(vault_bytes(&d), before);
}

#[test]
fn an_empty_bucket_and_a_wrong_prefix_are_told_apart() {
    let s3 = FakeS3::default().spawn("127.0.0.1:0");
    let d = dirs();
    save_cloud_form(d.cloud.clone(), KEY.to_vec(), form(s3.port)).unwrap();
    assert!(matches!(pull(&d), Err(FfiError::CloudNoBackup(_))));

    let s3b = bucket().spawn("127.0.0.1:0");
    let mut f = form(s3b.port);
    f.prefix = "mullion".into(); // 少了结尾斜杠
    save_cloud_form(d.cloud.clone(), KEY.to_vec(), f).unwrap();
    match pull(&d) {
        Err(FfiError::CloudNoRecognizedBackup { count, .. }) => assert_eq!(count, 3),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_corrupt_config_is_reported_before_missing_fields() {
    let d = dirs();
    std::fs::create_dir_all(&d.cloud).unwrap();
    std::fs::write(format!("{}/{}", d.cloud, mullion_store::CLOUD_FILE), "坏 [").unwrap();
    assert!(load_cloud_form(d.cloud.clone()).is_corrupt);
    assert!(matches!(pull(&d), Err(FfiError::CloudConfigCorrupt)));
    assert!(matches!(
        save_cloud_form(d.cloud.clone(), KEY.to_vec(), form(1)),
        Err(FfiError::CloudConfigCorrupt)
    ));
    reset_cloud_config(d.cloud.clone()).unwrap();
    assert!(!load_cloud_form(d.cloud.clone()).is_corrupt);
}

#[test]
fn secrets_sealed_with_another_key_are_unreadable_not_a_bad_pack() {
    let d = dirs();
    save_cloud_form(d.cloud.clone(), vec![1; 32], form(1)).unwrap();
    let r = pull(&d);
    assert!(matches!(r, Err(FfiError::CloudConfigUnreadable)), "{r:?}");
}

#[test]
fn missing_fields_are_listed_and_no_request_is_sent() {
    let d = dirs();
    let mut f = form(1);
    f.endpoint.clear();
    f.secret_key.clear();
    save_cloud_form(d.cloud.clone(), KEY.to_vec(), f).unwrap();
    match pull(&d) {
        Err(FfiError::CloudIncomplete(m)) => {
            assert!(m.contains("Endpoint") && m.contains("SK"), "{m}")
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_form_round_trips_and_blank_secrets_keep_the_stored_ones() {
    let s3 = bucket().spawn("127.0.0.1:0");
    let d = dirs();
    save_cloud_form(d.cloud.clone(), KEY.to_vec(), form(s3.port)).unwrap();
    let back = load_cloud_form(d.cloud.clone());
    assert_eq!(back.endpoint, format!("http://127.0.0.1:{}", s3.port));
    assert!(back.has_secret && back.has_passphrase);
    assert!(
        back.secret_key.is_empty() && back.passphrase.is_empty(),
        "不回传明文"
    );

    // 留空 = 沿用:再存一次空 SK/口令,拉取仍成功(用的是原来存的那份)
    let mut blank = form(s3.port);
    blank.secret_key.clear();
    blank.passphrase.clear();
    save_cloud_form(d.cloud.clone(), KEY.to_vec(), blank).unwrap();
    let back = load_cloud_form(d.cloud.clone());
    assert!(back.has_secret && back.has_passphrase);
    assert_eq!(
        pull(&d).unwrap().import.projects,
        vec![DEMO_PROJECT.to_string()]
    );
}

/// 读-改-写:桌面专用字段保存后原样保留。
#[test]
fn saving_keeps_desktop_only_fields() {
    let d = dirs();
    std::fs::create_dir_all(&d.cloud).unwrap();
    std::fs::write(
        format!("{}/{}", d.cloud, mullion_store::CLOUD_FILE),
        "enabled = true\nkeep = 42\nlast_seq = 7\n",
    )
    .unwrap();
    save_cloud_form(d.cloud.clone(), KEY.to_vec(), form(9)).unwrap();
    let cfg = mullion_store::load_cloud_config(std::path::Path::new(&d.cloud));
    assert!(cfg.enabled);
    assert_eq!((cfg.keep, cfg.last_seq), (42, 7));
}

/// 换了 AK 却不重填 SK:旧 SK 配新 AK 必然是错配,不写盘、不静默沿用。
#[test]
fn changing_the_access_key_requires_the_secret_again() {
    let d = dirs();
    save_cloud_form(d.cloud.clone(), KEY.to_vec(), form(9)).unwrap();
    let mut f = form(9);
    f.access_key_id = "AK2".into();
    f.secret_key.clear();
    match save_cloud_form(d.cloud.clone(), KEY.to_vec(), f) {
        Err(FfiError::CloudIncomplete(m)) => assert!(m.contains("SK"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(load_cloud_form(d.cloud.clone()).access_key_id, "AK");

    // 首次填写(已存 AK 为空、无 SK)不算「换」。
    let d2 = dirs();
    let mut f = form(9);
    f.secret_key.clear();
    save_cloud_form(d2.cloud.clone(), KEY.to_vec(), f).unwrap();
}

/// 终审 I2:云端对象超过上限报「过大」,且不动 vault。
#[test]
fn an_oversized_object_is_refused_and_the_vault_is_untouched() {
    let s3 = bucket().spawn("127.0.0.1:0");
    let d = dirs();
    save_cloud_form(d.cloud.clone(), KEY.to_vec(), form(s3.port)).unwrap();
    pull(&d).unwrap();
    let before = vault_bytes(&d);

    let mut huge = bucket();
    huge.get_override = Some((200, "x".repeat(16 * 1024 * 1024 + 1)));
    let s3b = huge.spawn("127.0.0.1:0");
    save_cloud_form(d.cloud.clone(), KEY.to_vec(), form(s3b.port)).unwrap();
    let r = pull(&d);
    assert!(matches!(r, Err(FfiError::CloudTooLarge)), "{r:?}");
    assert_eq!(vault_bytes(&d), before);
}

/// 安全 L1:SOCKS5 地址带 `user:pass@` 一律拒存,不落盘。
#[test]
fn a_socks5_address_with_credentials_is_refused_and_not_saved() {
    let d = dirs();
    let mut f = form(9);
    f.socks5 = "user:pw-MARK@127.0.0.1:1080".into();
    match save_cloud_form(d.cloud.clone(), KEY.to_vec(), f) {
        Err(FfiError::Cloud(m)) => assert!(!m.contains("pw-MARK"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(!std::path::Path::new(&d.cloud)
        .join(mullion_store::CLOUD_FILE)
        .exists());
}

/// 安全 L3:Debug 输出不含 SK / 口令 / AK。
#[test]
fn the_form_debug_output_hides_secrets() {
    let mut f = form(9);
    f.secret_key = "SK-MARK".into();
    f.passphrase = "PW-MARK".into();
    f.access_key_id = "AK-MARK".into();
    let shown = format!("{f:?}");
    for m in ["SK-MARK", "PW-MARK", "AK-MARK"] {
        assert!(!shown.contains(m), "Debug 泄露 {m}:{shown}");
    }
}
