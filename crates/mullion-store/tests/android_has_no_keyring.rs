//! F345 的机械守护:Android 上 `mullion-store` 的依赖树里**没有** keyring。
//!
//! 为什么必须隔开、而不是「编得过就行」:keyring 3.6.3 在没有匹配平台时
//! `pub use mock as default`,Android 正好落进这一支 —— 能编过、能跑,
//! 主密钥写进进程内存,**重启就丢,零报错**。所以在 Android 上必须让
//! `KeyringSource` 根本不存在,误用变成编译错误;Android 的主密钥走
//! Keystore(F346/F347)。
//!
//! 第二条断言(Windows 上**有** keyring)防止这个测试恒绿:`--target`
//! 写错、`cargo tree` 输出格式变了,第一条断言都会一直过。

use std::process::Command;

fn tree_for(target: &str) -> String {
    let out = Command::new(env!("CARGO"))
        .args([
            "tree",
            "-p",
            "mullion-store",
            "--target",
            target,
            "--edges",
            "normal",
            "--prefix",
            "none",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo tree 应能执行");
    assert!(
        out.status.success(),
        "cargo tree 失败: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn the_android_build_of_store_does_not_link_keyring_whose_fallback_is_a_silent_memory_store() {
    let tree = tree_for("aarch64-linux-android");
    assert!(
        !tree.lines().any(|l| l.starts_with("keyring ")),
        "Android 上 mullion-store 仍依赖 keyring(会静默回落 mock)。依赖树:\n{tree}"
    );
}

#[test]
fn the_windows_build_of_store_still_links_keyring() {
    let tree = tree_for("x86_64-pc-windows-gnu");
    assert!(
        tree.lines().any(|l| l.starts_with("keyring ")),
        "Windows 上 mullion-store 找不到 keyring —— 要么隔错了方向,要么本测试的 \
         cargo tree 解析已失效(那样上一条测试就是恒绿)。依赖树:\n{tree}"
    );
}
