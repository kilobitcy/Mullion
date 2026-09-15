//! 对**真实** S3 兼容存储的端到端验证(F270)。默认 `#[ignore]`。
//!
//! 真机信息一律从环境变量传,**绝不写死进库**(同 `mullion-ssh --test live`):
//!
//! ```bash
//! MULLION_CLOUD_LIVE=1 \
//! MULLION_CLOUD_ENDPOINT=https://oss-cn-hangzhou.aliyuncs.com \
//! MULLION_CLOUD_REGION=cn-hangzhou \
//! MULLION_CLOUD_BUCKET=<你的 bucket> \
//! MULLION_CLOUD_AK=<AK> MULLION_CLOUD_SK=<SK> \
//! MULLION_CLOUD_STAMP=$(date -u +%Y%m%dT%H%M%SZ) \
//!   cargo test -p mullion-cloud --test live -- --ignored --nocapture
//! ```
//!
//! **这是片一唯一能证明 SigV4 被真实服务端接受的证据。** 假 server 不验签,
//! 官方向量只证明我们与 AWS 的规范一致 —— 阿里云对 V4 的兼容细节没有一手
//! 文档,只能靠这条测试。
//!
//! **`MULLION_CLOUD_STAMP` 每次跑都要重新生成,别 `export` 之后反复用**——
//! 它同时是 SigV4 的签名时间和对象 key 的一部分,复用同一个 stamp 重跑会
//! 让第一个 PUT 因为 key 已存在而被拒。

use mullion_cloud::s3::{Credentials, S3Client};
use mullion_cloud::url::Endpoint;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
#[ignore = "要真实 bucket 与 AK/SK,见模块文档"]
fn a_real_bucket_accepts_our_signature_and_refuses_an_overwrite() {
    // 判据是**严格等于 `"1"`**,不是「非空」——`MULLION_CLOUD_LIVE=0` 是
    // 「临时关掉」的直觉写法,按非空判会照样跑,真的往一个真实 bucket 里
    // 写对象。与 `mullion-ssh/tests/live.rs` 的 `live_enabled` 同构。
    if std::env::var("MULLION_CLOUD_LIVE").as_deref() != Ok("1") {
        eprintln!("跳过:未设 MULLION_CLOUD_LIVE=1");
        return;
    }
    // `MULLION_CLOUD_SOCKS5` 是 `host:port`,**不带 `socks5://`**。
    let socks = env("MULLION_CLOUD_SOCKS5");
    let client = S3Client::new(
        Endpoint {
            base: env("MULLION_CLOUD_ENDPOINT").expect("MULLION_CLOUD_ENDPOINT"),
            bucket: env("MULLION_CLOUD_BUCKET").expect("MULLION_CLOUD_BUCKET"),
            path_style: env("MULLION_CLOUD_PATH_STYLE").is_some(),
        },
        Credentials {
            access_key_id: env("MULLION_CLOUD_AK").expect("MULLION_CLOUD_AK"),
            secret_access_key: env("MULLION_CLOUD_SK").expect("MULLION_CLOUD_SK"),
        },
        env("MULLION_CLOUD_REGION").expect("MULLION_CLOUD_REGION"),
        socks.as_deref(),
    )
    // `new` 返回 `Result`(代理地址解析不了时报 `Config`)—— 这里不能直接
    // 当成 `S3Client` 用,那是编译不过的。
    .expect("建不起客户端 —— 多半是 MULLION_CLOUD_SOCKS5 填错了");
    // 时间戳从 env 传 —— 本 crate 不持时钟。跑之前用 `date -u +%Y%m%dT%H%M%SZ`。
    let stamp = env("MULLION_CLOUD_STAMP").expect("MULLION_CLOUD_STAMP：date -u +%Y%m%dT%H%M%SZ");
    let key = format!("mullion-live-test/{stamp}.bin");

    // 不要预设失败原因:`MULLION_CLOUD_STAMP` 一旦被 `export` 出去反复用,
    // 第二次跑的这一发就会因为 key 已存在而被拒,真实原因是 `AlreadyExists`,
    // 跟签名权限毫无关系。咬定「多半是签名或权限」会把人往错误方向带。
    if let Err(e) = client.put_no_overwrite(&key, b"mullion live probe", &stamp) {
        panic!(
            "第一次 PUT 失败:{e:?} —— 若是 AlreadyExists,多半是 \
             MULLION_CLOUD_STAMP 被复用、撞上了已存在的 key(重新 \
             `date -u +%Y%m%dT%H%M%SZ` 生成一个);其它错误多半是签名或权限"
        );
    }

    // 第二次必须被拒。**这一条是追加式布局全部并发保护的真机证明** ——
    // 若真实服务端忽略了那两个头,本地假 server 是测不出来的。
    let again = client.put_no_overwrite(&key, b"x", &stamp);
    assert!(
        matches!(again, Err(mullion_cloud::CloudError::AlreadyExists)),
        "服务端没有拒绝覆盖,拿到的是 {again:?} —— 并发保护在这台服务端上不成立"
    );

    let keys = client
        .list_keys("mullion-live-test/", &stamp)
        .expect("LIST 应该成功");
    assert!(keys.contains(&key), "刚写的对象没出现在列表里:{keys:?}");

    eprintln!("live 验证通过。请手动删掉测试对象:{key}");
}
