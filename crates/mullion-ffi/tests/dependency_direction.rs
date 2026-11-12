//! ADR-013 的机械守护:app 与 ffi 是两个平级组装端,**互不依赖**。
//!
//! 最省事的歪路是「ffi 里直接调 app 的某个函数」或「app 里顺手用 ffi 的
//! 类型」——一旦成立,判据就会长回 app 里,两端分叉。

use std::process::Command;

/// `--edges normal`:dev/build 依赖不进最终 .so,故不计。
/// `--target all`:不加的话只看宿主 target,`[target.'cfg(target_os = "android")'.dependencies]`
/// 里的依赖会漏掉(假绿)。
fn tree(pkg: &str) -> String {
    let out = Command::new(env!("CARGO"))
        .args([
            "tree", "-p", pkg, "--edges", "normal", "--target", "all", "--prefix", "none",
        ])
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

/// `-e features` 下按**单个包**解析 feature。`--workspace` 会把 app 的 `write`
/// 统一进来,所以这里一定要 `-p`(设计 D4)。
fn feature_tree(pkg: &str) -> String {
    let out = Command::new(env!("CARGO"))
        .args([
            "tree",
            "-p",
            pkg,
            "-e",
            "features,normal",
            "--target",
            "all",
            "--prefix",
            "none",
        ])
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

const CLOUD_WRITE: &str = "mullion-cloud feature \"write\"";

/// F346-b 设计 D4:「不回写」的结构性判据 —— ffi 拿得到 cloud,但拿不到 `write`。
#[test]
fn ffi_gets_cloud_without_its_write_feature() {
    let t = feature_tree("mullion-ffi");
    assert!(
        has(&t, "mullion-cloud"),
        "解析失效:ffi 树里找不到 mullion-cloud\n{t}"
    );
    assert!(
        !t.lines().any(|l| l.trim() == CLOUD_WRITE),
        "mullion-ffi 打开了 mullion-cloud 的 write feature —— 手机端能推云了\n{t}"
    );
}

/// 阳性对照:同一个解析在 app 树里**必须**看得见 `write`。否则上一条恒绿。
#[test]
fn the_write_feature_is_visible_in_the_app_tree() {
    let t = feature_tree("mullion-app");
    assert!(
        t.lines().any(|l| l.trim() == CLOUD_WRITE),
        "解析失效:app 树里看不见 {CLOUD_WRITE}\n{t}"
    );
}

/// 声明层:ffi 的 cloud 依赖必须写明 `default-features = false`。只靠上面的树判据的话,
/// 哪天 cloud 的 `default` 不再含 `write`,有人把这行删了也不会红,而再加回来就开了。
#[test]
fn ffi_declares_cloud_without_default_features() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let doc: toml::Value = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let dep = doc
        .get("dependencies")
        .and_then(|d| d.get("mullion-cloud"))
        .expect("mullion-ffi 的 Cargo.toml 没有依赖 mullion-cloud");
    assert_eq!(
        dep.get("default-features").and_then(|v| v.as_bool()),
        Some(false),
        "mullion-ffi 对 mullion-cloud 的依赖没写 default-features = false:{dep:?}"
    );
    assert!(
        dep.get("features").is_none(),
        "mullion-ffi 对 mullion-cloud 显式开了 features:{dep:?}"
    );
}
