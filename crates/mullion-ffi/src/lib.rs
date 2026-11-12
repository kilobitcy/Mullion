//! Android 端的 Rust 内核出口(ADR-013,F345 起)。
//!
//! 与 `mullion-app` 平级的第二个组装端:把 term / ssh / store 拼起来,
//! 经 uniffi 导出给 Kotlin。这里**不写判据**——判据在 store(F344),
//! 这里只做 IO 编排与类型转换。

uniffi::setup_scaffolding!();

mod cloud;
mod error;
mod import;
mod runtime;
mod screen;
mod terminal;

pub use cloud::{
    load_cloud_form, pull_latest, reset_cloud_config, save_cloud_form, CloudForm, PullSummary,
};
pub use error::FfiError;
pub use import::{import_pack, inspect_pack, ImportSummary, PackInfo};
pub use screen::ScreenText;
pub use terminal::{ConnectParams, ScreenListener, Terminal};

/// 内核版本号,Kotlin 侧用来确认加载的 `.so` 是不是这一版。
#[uniffi::export]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_the_workspace_version() {
        assert_eq!(super::version(), env!("CARGO_PKG_VERSION"));
    }
}
