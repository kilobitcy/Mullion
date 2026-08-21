//! 程序图标(F152)的机械守护。
//!
//! 这里钉的三件事有一个共同点:**错了全都不报错**。资源序号对不上、ico 缺了
//! 大尺寸帧、app.rs 把序号写成字面量 —— 三种情况下 `cargo build` 和
//! `cargo test` 一律绿,交叉编译一律成功,只有人在 Windows 上盯着任务栏
//! 或资源管理器才看得出来。跟 T9 的豆腐块是同一类问题,所以用同一种手法:
//! 在 Linux 上读文件、读源码,把「只有人眼能发现」压成「测试会红」。

use std::path::PathBuf;

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// 从 `.rc` 里挑出 `N ICON "文件名"` 这一条,返回 `(N, 文件名)`。
///
/// 只认这一种形式 —— .rc 的语法远比这复杂,但本项目的资源脚本就一行,
/// 写个半吊子解析器去容忍它用不上的语法只会让这个守护更容易看走眼。
fn parse_icon_entry(rc: &str) -> (u16, String) {
    let mut found = Vec::new();
    for line in rc.lines() {
        let line = line.trim();
        // 跳过整行注释与块注释的续行。资源脚本里的 `*` 开头只可能是注释。
        if line.starts_with('/') || line.starts_with('*') {
            continue;
        }
        let Some((head, tail)) = line.split_once(" ICON ") else {
            continue;
        };
        let Ok(id) = head.trim().parse::<u16>() else {
            continue;
        };
        let name = tail.trim().trim_matches('"').to_owned();
        found.push((id, name));
    }
    assert_eq!(
        found.len(),
        1,
        "assets/mullion.rc 里应当**只有一条** ICON,实际 {found:?}。\
         多一条会让资源管理器按「ID 最小」挑走另一张图。"
    );
    found.pop().unwrap()
}

#[test]
fn the_resource_id_in_the_rc_script_matches_the_one_the_window_asks_for() {
    let rc = std::fs::read_to_string(crate_root().join("assets/mullion.rc")).expect("读 .rc");
    let (id, _) = parse_icon_entry(&rc);
    assert_eq!(
        id,
        mullion_app::icon_res::RESOURCE_ID,
        "资源序号对不上:.rc 里是 {id},`icon_res::RESOURCE_ID` 是 {}。\
         `Icon::from_resource` 会返回 Err,而我们的处置是「取不到就不设」——\
         exe 的文件图标照样在,只有标题栏/任务栏/Alt-Tab 的图标静默消失。",
        mullion_app::icon_res::RESOURCE_ID
    );
}

#[test]
fn the_icon_file_carries_every_size_windows_will_reach_for() {
    let rc = std::fs::read_to_string(crate_root().join("assets/mullion.rc")).expect("读 .rc");
    let (_, name) = parse_icon_entry(&rc);
    let path = crate_root().join("assets").join(&name);
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("{} 读不到({e}):.rc 引用的图标文件必须在", path.display()));

    let dir = ico::IconDir::read(std::io::Cursor::new(&bytes)).expect("这不是一个合法的 .ico");
    let sides: Vec<u32> = dir
        .entries()
        .iter()
        .map(|e| e.width().max(e.height()))
        .collect();

    // Windows 会按场合去挑不同的帧,挑不到就**拿别的帧缩放**(最近邻,细线条糊掉)。
    // 这三档是必挑的:16 标题栏(ICON_SMALL)、32 任务栏与 Alt-Tab(ICON_BIG),
    // 256 资源管理器的「超大图标」视图。
    for want in [mullion_app::icon_res::SMALL_PX, 32, 256] {
        assert!(
            sides.contains(&want),
            "{name} 里没有 {want}x{want} 那一帧(现有 {sides:?})。\
             缺哪一档就在对应的场合被缩放成糊的,而构建全程不报错。"
        );
    }

    // 帧要解得开。ico 允许混 BMP 帧和 PNG 帧,而 Windows XP 之前的解码器
    // 不认 PNG 帧 —— 这里只验「我们自己的解码器读得动」,坏文件当场就红。
    for e in dir.entries() {
        let side = e.width().max(e.height());
        e.decode()
            .unwrap_or_else(|err| panic!("{name} 的 {side}x{side} 帧解不开:{err}"));
    }
}

#[test]
fn the_window_takes_the_resource_id_from_the_shared_constant_not_a_literal() {
    let src = std::fs::read_to_string(crate_root().join("src/app.rs")).expect("读 app.rs");

    let calls: Vec<usize> = src
        .match_indices("from_resource(")
        .map(|(i, _)| i)
        .collect();
    assert!(
        !calls.is_empty(),
        "app.rs 里一次 `Icon::from_resource` 都没有 —— 窗口图标没挂上去,\
         而这在 Linux 上编译测试全绿(那段代码裹在 cfg(windows) 里)。"
    );
    // 每一次调用的第一个实参都必须是那个共享常量。写成字面量 `1` 也照样跑,
    // 但从此 .rc 与代码脱钩:改 .rc 的序号时上面那个测试仍然绿。
    // 取「调用点之后一小段」而不是精确匹配某个写法 —— rustfmt 会把长调用折行,
    // 钉死单一写法的守护迟早被一次 `cargo fmt` 变成假红。
    for i in calls {
        let tail = &src[i..(i + 120).min(src.len())];
        assert!(
            tail.contains("icon_res::RESOURCE_ID"),
            "app.rs 第 {} 字节处的 `from_resource` 没有走 \
             `crate::icon_res::RESOURCE_ID`:\n{tail}",
            i
        );
    }
}
