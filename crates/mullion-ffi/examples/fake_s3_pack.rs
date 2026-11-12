//! F346-b:Android 插桩测试用的宿主机假 S3(全是假数据)。
//!
//!     cargo run -p mullion-ffi --example fake_s3_pack -- 9000
//!
//! 监听 127.0.0.1:<端口>(模拟器里是 10.0.2.2),path-style、bucket `b`,
//! 前缀 `mullion/` 下回放一份演示包(口令 = `demo_pack::DEMO_PASSWORD`)。
#[path = "../tests/common/demo_pack.rs"]
mod demo_pack;
#[path = "../tests/common/fake_s3.rs"]
mod fake_s3;

fn main() {
    let port = std::env::args().nth(1).unwrap_or_else(|| "9000".into());
    let object =
        mullion_store::seal_secrets(demo_pack::demo_pack().as_bytes(), demo_pack::DEMO_PASSWORD)
            .unwrap();
    let s3 = fake_s3::FakeS3::with_object("mullion/000001-20261112T000000Z.mpk", object)
        .spawn(&format!("127.0.0.1:{port}"));
    println!("fake s3 on 127.0.0.1:{}", s3.port);
    loop {
        std::thread::park();
    }
}
