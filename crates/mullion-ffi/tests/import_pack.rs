//! F346-a:ffi 层只做类型转换与错误映射;判据的测试在 `mullion-store::phone_import`。
#[path = "common/demo_pack.rs"]
mod demo_pack;

use demo_pack::{demo_pack, pack_text, DEMO_PASSWORD, DEMO_PROJECT};
use mullion_ffi::{import_pack, inspect_pack, FfiError};

fn dir_in(tmp: &tempfile::TempDir) -> String {
    tmp.path().join("vault").to_string_lossy().into_owned()
}

#[test]
fn the_demo_pack_keeps_only_project_reachable_secrets() {
    let tmp = tempfile::tempdir().unwrap();
    let s = import_pack(dir_in(&tmp), demo_pack(), DEMO_PASSWORD.into(), vec![7; 32]).unwrap();
    assert_eq!(s.projects, vec![DEMO_PROJECT.to_string()]);
    assert_eq!(
        (s.kept, s.dropped, s.broken_nodes, s.unrecognized),
        (3, 1, 0, 0)
    );
}

#[test]
fn inspect_tells_whether_a_password_is_needed() {
    assert!(inspect_pack(demo_pack()).unwrap().has_secrets);
    assert!(
        !inspect_pack(pack_text(Vec::new(), &[]))
            .unwrap()
            .has_secrets
    );
}

#[test]
fn a_wrong_password_maps_to_wrong_password() {
    let tmp = tempfile::tempdir().unwrap();
    let err = import_pack(dir_in(&tmp), demo_pack(), "nope".into(), vec![7; 32]).unwrap_err();
    assert!(matches!(err, FfiError::WrongPassword), "{err:?}");
}

#[test]
fn a_newer_pack_format_maps_to_needs_newer_app() {
    let err = inspect_pack("format_version = 9\n".into()).unwrap_err();
    assert!(matches!(err, FfiError::NeedsNewerApp(9)), "{err:?}");
}

#[test]
fn a_newer_schema_maps_to_needs_newer_app() {
    let tmp = tempfile::tempdir().unwrap();
    let text = pack_text(
        vec![("sessions.toml", b"schema_version = 99\n".to_vec())],
        &[],
    );
    let err = import_pack(dir_in(&tmp), text, String::new(), vec![7; 32]).unwrap_err();
    assert!(matches!(err, FfiError::NeedsNewerApp(99)), "{err:?}");
}

#[test]
fn garbage_maps_to_bad_pack() {
    let err = inspect_pack("这不是 TOML [[[".into()).unwrap_err();
    assert!(matches!(err, FfiError::BadPack(_)), "{err:?}");
}

#[test]
fn a_data_key_of_the_wrong_length_is_rejected_before_touching_the_dir() {
    for len in [31, 33] {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        let err = import_pack(
            dir.to_string_lossy().into_owned(),
            demo_pack(),
            DEMO_PASSWORD.into(),
            vec![7; len],
        )
        .unwrap_err();
        assert!(matches!(err, FfiError::Internal(_)), "{len}: {err:?}");
        assert!(!dir.exists(), "{len} 字节密钥:不得创建 dir");
    }
}
