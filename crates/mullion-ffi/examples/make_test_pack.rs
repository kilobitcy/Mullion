//! 生成 Android 插桩测试用的 `.mullionpack`(F346-a,全是假数据)。
//!
//!     cargo run -p mullion-ffi --example make_test_pack -- android/app/src/androidTest/assets/test.mullionpack
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
    std::fs::write(&out, demo_pack::demo_pack()).unwrap();
    println!("wrote {out}");
}
