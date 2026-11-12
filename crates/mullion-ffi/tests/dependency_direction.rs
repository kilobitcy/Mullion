//! ADR-013 的机械守护:app 与 ffi 是两个平级组装端,**互不依赖**。
//!
//! 最省事的歪路是「ffi 里直接调 app 的某个函数」或「app 里顺手用 ffi 的
//! 类型」——一旦成立,判据就会长回 app 里,两端分叉。

use std::process::Command;

fn tree(pkg: &str) -> String {
    let out = Command::new(env!("CARGO"))
        .args(["tree", "-p", pkg, "--edges", "normal", "--prefix", "none"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo tree 应能执行");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn has(tree: &str, krate: &str) -> bool {
    tree.lines().any(|l| l.starts_with(&format!("{krate} ")))
}

#[test]
fn ffi_never_depends_on_app() {
    let t = tree("mullion-ffi");
    // 防恒绿:先确认解析拿得到它真依赖的东西。
    assert!(
        has(&t, "mullion-term"),
        "解析失效:ffi 树里找不到 mullion-term\n{t}"
    );
    assert!(
        !has(&t, "mullion-app"),
        "mullion-ffi 依赖了 mullion-app\n{t}"
    );
}

#[test]
fn app_never_depends_on_ffi() {
    let t = tree("mullion-app");
    assert!(
        has(&t, "mullion-term"),
        "解析失效:app 树里找不到 mullion-term\n{t}"
    );
    assert!(
        !has(&t, "mullion-ffi"),
        "mullion-app 依赖了 mullion-ffi\n{t}"
    );
}
