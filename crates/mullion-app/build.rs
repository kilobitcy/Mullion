//! 把 `assets/mullion.rc` 里的图标编进 exe 的资源段(F152)。
//!
//! **为什么不用 `embed-resource` crate**:它确实是这个领域的事实标准,但本项目
//! 出 exe 的路径只有一条 —— Linux 上 `--target x86_64-pc-windows-gnu` 交叉编译
//! (`docs/cross-compile-windows.md`),工具链固定是 mingw-w64。为这一条路径引一棵
//! build-dependency 树,与依赖表里每条都写得出理由的克制风格不符。
//!
//! **为什么资源段是必须的**:winit 0.30.13 注册窗口类时写死 `hIcon: 0`
//! (`platform_impl/windows/window.rs:1417`),**不会**去加载 exe 的资源图标。
//! 所以资源段只负责「资源管理器/开始菜单里那个文件图标」,标题栏和任务栏的
//! 图标得由 `app.rs` 显式调 `Icon::from_resource` 再 `with_window_icon` 挂上去。
//! 两条路都要走,少哪条都是「图标只出现在一半地方」。

use std::path::PathBuf;
use std::process::Command;

fn main() {
    // 图标或资源脚本改了要重编。不写这两行的话,换了图标却拿旧资源出包,
    // 而且**不报错** —— 只有在 Windows 上看见旧图标才发现。
    println!("cargo:rerun-if-changed=assets/mullion.rc");
    println!("cargo:rerun-if-changed=assets/mullion.ico");

    // 本机(Linux)构建走到这里就结束:资源段是 PE 格式独有的东西。
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // MSVC 工具链要 `rc.exe` 而不是 windres,而本项目从没走过那条路
    // (交叉编译锁死 gnu ABI)。不静默跳过 —— 静默的后果是有人在原生
    // Windows 上编出一个没有图标的 exe 却查不出为什么。
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu") {
        println!(
            "cargo:warning=非 gnu ABI 的 Windows 目标不编图标资源(需要 rc.exe),\
             exe 将没有文件图标。本项目的出包路径见 docs/cross-compile-windows.md"
        );
        return;
    }

    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("mullion-res.o");
    // 用 `windres` 而不是 `llvm-rc`:.cargo/config.toml 已经把链接器锁成
    // `x86_64-w64-mingw32-gcc`,同一套 binutils 里的 windres 与它的目标格式
    // 天然一致,不会出「链接器不认这个 .o」。
    let status = Command::new("x86_64-w64-mingw32-windres")
        // `-I assets`:.rc 里写的是相对文件名 `"mullion.ico"`,windres 的
        // 搜索起点是**进程的工作目录**(cargo 会设成 crate 根),不是 .rc 所在目录。
        // 少了这个 include 路径,windres 会报 "can't open icon file"。
        .args(["-I", "assets", "assets/mullion.rc", "-O", "coff", "-o"])
        .arg(&out)
        .status();

    match status {
        Ok(s) if s.success() => {
            // `-bins` 而不是裸 `link-arg`:后者会连测试/example 的二进制一起塞
            // 资源,平白多几百 KB,还会让 `cargo test --target ...` 依赖 windres。
            println!("cargo:rustc-link-arg-bins={}", out.display());
        }
        // 编不出资源就让构建红。图标缺失是那种「编译过、跑起来才看得见」的
        // 问题(CLAUDE.md 陷阱表 T9 同族),放过去等于把它推给人眼。
        Ok(s) => panic!("windres 失败(退出码 {s}):assets/mullion.rc"),
        Err(e) => panic!(
            "找不到 x86_64-w64-mingw32-windres({e})。\
             装法见 docs/cross-compile-windows.md"
        ),
    }
}
