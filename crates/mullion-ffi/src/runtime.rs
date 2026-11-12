//! 进程级 tokio 运行时。Kotlin 侧没有 tokio,所有 async 都在这里跑;
//! 导出给 Kotlin 的函数一律是**阻塞**调用,由 Kotlin 放到 `Dispatchers.IO` 上调。

use std::sync::OnceLock;

pub(crate) fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("mullion-ffi")
            .enable_all()
            .build()
            .expect("tokio 运行时建不起来")
    })
}
