//! 红线 2 的机械守护:`mullion-ssh` 的依赖树里永不出现 `mullion-store`。
//!
//! 为什么要一个测试:P0-b 让 ssh 认识了「跳板」这个概念,最省事的写法是直接
//! 收一个 `SessionRecord`。那样 ssh 就依赖了 store,依赖方向从单向变成网状,
//! 「布局/键码 bug 能脱离窗口写测试」这条项目根基就没了(CLAUDE.md 架构不变量)。
//! 靠人自觉守不住,靠这个测试守。

use std::process::Command;

#[test]
fn ssh_crate_never_depends_on_store() {
    let out = Command::new(env!("CARGO"))
        .args([
            "tree",
            "-p",
            "mullion-ssh",
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
    let tree = String::from_utf8_lossy(&out.stdout);
    assert!(
        !tree.contains("mullion-store"),
        "红线 2 被打破:mullion-ssh 依赖了 mullion-store。\n\
         跳板信息必须由 app 的 dial_plan 物化成 Hop 再传进来。\n依赖树:\n{tree}"
    );
    // 顺带钉住整条红线:ssh 也不该认识 core/term/app。
    for forbidden in ["mullion-core", "mullion-term", "mullion-app"] {
        assert!(
            !tree.contains(forbidden),
            "mullion-ssh 依赖了 {forbidden},违反单向依赖 app → {{core,term,ssh,store}}"
        );
    }
}
