//! 生成 Android 插桩测试用的 `.mullionpack`(F346-a,全是假数据)。
//!
//!     cargo run -p mullion-ffi --example make_test_pack -- android/app/src/androidTest/assets/test.mullionpack
//!
//! 加 `--open <host>:<port>` 生成 F349 打开项目用的包(`demo_pack::open_pack`):
//!
//!     cargo run -p mullion-ffi --example make_test_pack -- android/app/src/androidTest/assets/open.mullionpack --open 10.0.2.2:2223
//!
//! 内容与预期计数见 `tests/common/demo_pack.rs` 文档。盐是随机的,每次生成字节不同 ——
//! 只在演示包内容变了时重新生成并提交。
#[path = "../tests/common/demo_pack.rs"]
mod demo_pack;

fn main() {
    let out = std::env::args()
        .nth(1)
        .expect("用法:make_test_pack <输出路径>");
    if let Some(parent) = std::path::Path::new(&out).parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let text = match std::env::args().nth(2).as_deref() {
        None => demo_pack::demo_pack(),
        Some("--open") => {
            let addr = std::env::args().nth(3).expect("--open 后面要 host:port");
            let (host, port) = addr.rsplit_once(':').expect("host:port");
            demo_pack::open_pack(&[(host.to_string(), port.parse().expect("端口"))])
        }
        Some(other) => panic!("不认识的参数 {other}"),
    };
    std::fs::write(&out, text).unwrap();
    println!("wrote {out}");
}
