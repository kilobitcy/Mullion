//! 「必须独占一个测试进程」的那些 target,守住它们真的还独占着。
//!
//! `tests/heap_price_model.rs` 量的是进程全局的堆账。它之所以不再 flaky,
//! 全部依赖一件事:**那个测试二进制里只有一条用例**,没有邻居在同一个读数
//! 窗口里分配和释放。
//!
//! 这个不变量破起来太容易了 —— 下一个人要量别的内存数字,最自然的动作就是
//! 往那个文件里再加一条 `#[test]`。加完之后两条并发跑,噪声原样回来,而
//! **编译不报、clippy 不报,跑一遍多半还是绿的**(原来那条也是五次才红一次)。
//! 等它再红的时候,人已经学会了「这条偶尔红,重跑一下就好」——那正是 flaky
//! 最贵的地方:真回归也会被这么放过去。
//!
//! 所以这里做一条计数对账。它自己必须待在**另一个** target 里:`cargo test`
//! 逐个运行各测试二进制,放在一起就等于亲手破坏被守的东西。
//!
//! 自证会变红:往 `heap_price_model.rs` 里再加一条 `#[test] fn …() {}`。

/// 需要独占进程的 target → 为什么。
const EXCLUSIVE: &[(&str, &str)] = &[(
    "heap_price_model.rs",
    "读进程全局的 heapgauge 差分,邻居的分配/释放会直接记在同一把尺子上",
)];

#[test]
fn a_target_that_needs_the_process_to_itself_holds_exactly_one_test() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    for (file, why) in EXCLUSIVE {
        let src = std::fs::read_to_string(dir.join(file))
            .unwrap_or_else(|e| panic!("读不到 tests/{file}:{e}"));
        let n = src.matches("#[test]").count();
        assert_eq!(
            n, 1,
            "tests/{file} 里有 {n} 条用例,但它必须独占进程({why})。\
             新的量测请另起一个 target,别加在这里 —— 加进来之后两条并发跑,\
             噪声回来了却不会有任何报错,只是偶尔红一次"
        );
    }
}
