//! mullion-cloud —— S3 兼容对象存储客户端(F270)。
//!
//! # 架构不变量
//!
//! **只认「字节 + 键名」。** 不认识 `Pack`、`SessionRecord`、egui,也**不依赖
//! 任何 mullion-\* crate**。打包、加密、指纹全在 `mullion-store` 那边。
//!
//! 这条约束的价值:违反了之后,「SigV4 签名算错了」和「包组装错了」就再也
//! 分不开 —— 而前者只能靠官方测试向量证明,后者只能靠 round-trip 证明,
//! 两类证据没法互相替代。
//!
//! **零 async、零 UI、零 GPU。** ureq 是阻塞式的;app 侧用 `spawn_blocking`
//! 挖出去(绝不能在事件循环里同步跑网络 —— T3/T7)。

pub mod error;
pub mod s3;
pub mod sigv4;
pub mod url;
pub mod xml;

pub use error::CloudError;
