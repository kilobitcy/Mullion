//! F190:Rust 堆的存活字节数。
//!
//! **为什么需要它**:v0.1.80 的实机日志里 `profile.mem` 的 `其他:` 一项在
//! 52 分钟内从 233MB 单调涨到 319MB(commit 233→357、ws 145→258),阶梯与
//! `WindowEvent::Resized` 对得上,而 `scroll:`/`text:` 两项都会跌回去。
//! 问题是 `其他:` 是个**减法残差**(commit 减掉三笔记账),里面同时装着
//!
//! - 我们自己在 Rust 堆上分配、但没进那三笔记账的东西,和
//! - 完全不在 Rust 堆上的东西:AMD Vulkan 驱动的内部分配、交换链、
//!   glyphon 图集、代码段、各线程栈、CRT 自己的堆。
//!
//! 这两类的下一步完全不同(前者能继续往数据结构里切,后者只能从 wgpu/驱动
//! 那头查,而 F176 已经确认 **wgpu 的两个内存 API 都看不见那一块**),
//! 而日志上它们长得一模一样。这个 gauge 只做一件事:把那条界画出来。
//!
//! **口径**:统计的是**经过 Rust `GlobalAlloc` 的存活字节**,按调用方要的
//! `Layout::size()` 记,不含分配器自己的头部与对齐浪费,所以它是真实堆占用的
//! **下界**。`commit - 堆` 就是「堆外」那一半。
//!
//! **不做的事**:不记录调用栈、不分类型、不采样。那些要么要在每次分配上做
//! 事(帧路径上不许,T3),要么得引第三方 profiler。这里只有两个 relaxed
//! `fetch_add`,一次分配多这么两条指令,相对 `HeapAlloc` 本身可以忽略。
//!
//! **F190 推翻了 F169 当初「不上自定义分配器」那条**(2026-08-28 用户重新
//! 拍板):原判断是在没有证据时做的成本权衡,现在有 52 分钟实机数据指着
//! 一个查不下去的黑箱。

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

/// 一对只增计数器。
///
/// 分成「累计分配」和「累计释放」两个只增的,而不是一个带符号的净值:分配
/// 与释放随时可能落在不同线程上,净值那种写法在竞态下会瞬时为负,而
/// `AtomicU64` 减到负数会绕回成一个天文数字 —— 那是个「像模像样的错值」
/// (同 F168 里 `as u32` 绕回那一族)。两个只增计数器各自单调,读的时候
/// `saturating_sub` 一次,最坏也只是差一次分配。
///
/// **做成一个可以有多个实例的类型,而不是两个 `static`**,是为了测试:
/// 进程全局的那对计数器在并行 test runner 下每时每刻都在被别的用例推着走,
/// 任何「分配前后差值等于 N」的断言都会随机红或者随机绿。用例自己持有一对
/// 谁也碰不到的计数器,才能写等号。
pub struct Counters {
    alloced: AtomicU64,
    freed: AtomicU64,
}

impl Counters {
    pub const fn new() -> Self {
        Self {
            alloced: AtomicU64::new(0),
            freed: AtomicU64::new(0),
        }
    }

    fn on_alloc(&self, n: usize) {
        self.alloced.fetch_add(n as u64, Ordering::Relaxed);
    }

    fn on_free(&self, n: usize) {
        self.freed.fetch_add(n as u64, Ordering::Relaxed);
    }

    /// 当前存活字节(下界,见模块文档的口径说明)。
    pub fn live(&self) -> u64 {
        self.alloced
            .load(Ordering::Relaxed)
            .saturating_sub(self.freed.load(Ordering::Relaxed))
    }
}

impl Default for Counters {
    fn default() -> Self {
        Self::new()
    }
}

/// 进程全局的那一对。`lib.rs` 的 `#[global_allocator]` 指着它。
pub static GLOBAL: Counters = Counters::new();

/// 包一层记账的分配器,分配动作本身原样转交 [`System`]。
///
/// **四个方法必须全部实现并各自记账。** `GlobalAlloc` 给 `realloc` 和
/// `alloc_zeroed` 提供了默认实现(前者 = alloc + copy + dealloc,后者 =
/// alloc + 清零),只包 `alloc`/`dealloc` 的话账目仍然是**对的**,但每次
/// `Vec` 扩容都退化成「新分配 + 逐字节拷贝」,丢掉了 `HeapReAlloc` 原地扩展
/// 的机会 —— 一个为了看内存而加的探针把内存操作本身拖慢,得不偿失。
/// 转交给 `System` 之后,记账就得自己补上;补漏了不会有任何报错,只会让
/// 这个数悄悄偏小。
pub struct CountingAlloc(pub &'static Counters);

// SAFETY:每个方法都把实参原样转交 `System`,分配/释放语义完全由它决定;
// 这里额外做的只有两个 relaxed 原子加,不触碰返回的指针,也不在任何路径上
// 再次分配(分配器里分配 = 无限递归)。
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            self.0.on_alloc(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        self.0.on_free(layout.size());
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            self.0.on_alloc(layout.size());
        }
        p
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            // **先销旧账再记新账**。只记新尺寸的话,一个反复扩容的 `Vec` 会把
            // 堆用量越算越大而进程内存纹丝不动 —— 一个只涨不跌、看着特别像
            // 内存泄漏的假象,恰好长在这个专门用来找泄漏的数上。
            //
            // 返回空 = 扩容失败、**原块仍然存活**,此时一个字节都不许动账;
            // 这也是这两句必须待在 `if` 里面的理由。
            self.0.on_free(layout.size());
            self.0.on_alloc(new_size);
        }
        p
    }
}

/// 当前存活的 Rust 堆字节数(下界,见模块文档的口径说明)。
pub fn live_bytes() -> u64 {
    GLOBAL.live()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个只属于本用例的分配器 —— 进程全局那对计数器在并行 runner 下
    /// 每时每刻都被别的用例推着走,任何等号断言都会随机红。
    ///
    /// `CountingAlloc` 要 `&'static Counters`,所以泄漏一个 `Box`:测试
    /// 进程里几十个字节,换来的是可以写等号而不是「落在某个区间」。
    fn probe() -> CountingAlloc {
        CountingAlloc(Box::leak(Box::new(Counters::new())))
    }

    fn live_of(a: &CountingAlloc) -> u64 {
        a.0.live()
    }

    const N: usize = 4096;

    fn layout(n: usize) -> Layout {
        Layout::from_size_align(n, 8).expect("合法布局")
    }

    /// 自证会变红:把 `alloc` 里的 `on_alloc` 删掉(第一条断言),或把
    /// `dealloc` 里的 `on_free` 删掉(第二条)。
    #[test]
    fn an_allocation_shows_up_and_a_free_takes_it_back_off_the_books() {
        let a = probe();
        let l = layout(N);
        let p = unsafe { a.alloc(l) };
        assert!(!p.is_null());
        assert_eq!(live_of(&a), N as u64, "分配之后账上应当正好是这一块");
        unsafe { a.dealloc(p, l) };
        assert_eq!(live_of(&a), 0, "释放之后账应当归零");
    }

    /// `vec![0u8; N]` 走的是 `alloc_zeroed`,**不是** `alloc` —— 只在 `alloc`
    /// 里记账的版本对这条路径完全没有账,而清零分配在本项目里到处都是。
    ///
    /// 自证会变红:把 `alloc_zeroed` 里的 `on_alloc` 删掉。
    #[test]
    fn a_zeroed_allocation_is_on_the_books_too() {
        let a = probe();
        let l = layout(N);
        let p = unsafe { a.alloc_zeroed(l) };
        assert!(!p.is_null());
        assert_eq!(live_of(&a), N as u64);
        unsafe { a.dealloc(p, l) };
    }

    /// 扩容走 `realloc`:账上必须**换掉**旧块,而不是叠加。
    ///
    /// 自证会变红:把 `realloc` 里的 `on_free` 删掉 —— 账会变成 `N + 4N`
    /// 而不是 `4N`。
    #[test]
    fn growing_a_block_swaps_the_old_size_for_the_new_one_instead_of_stacking_them() {
        let a = probe();
        let l = layout(N);
        let p = unsafe { a.alloc(l) };
        let big = N * 4;
        let p2 = unsafe { a.realloc(p, l, big) };
        assert!(!p2.is_null());
        assert_eq!(live_of(&a), big as u64, "扩容后账上应当只有新块,旧块要销掉");
        unsafe { a.dealloc(p2, layout(big)) };
        assert_eq!(live_of(&a), 0);
    }

    /// 缩容也走 `realloc`,方向反过来 —— 只加不减的实现在这条路径上会让
    /// 账**越缩越大**。
    #[test]
    fn shrinking_a_block_moves_the_books_down_not_up() {
        let a = probe();
        let big = N * 4;
        let p = unsafe { a.alloc(layout(big)) };
        let p2 = unsafe { a.realloc(p, layout(big), N) };
        assert!(!p2.is_null());
        assert_eq!(live_of(&a), N as u64);
        unsafe { a.dealloc(p2, layout(N)) };
    }

    /// 完备性:`GlobalAlloc` 的四个方法**每一个**都必须动过计数器。
    ///
    /// 行为测试只覆盖得到走得通的那几条路径;将来 std 给 `GlobalAlloc`
    /// 加方法、或者有人把某个方法改成纯转发,漏掉的那条不会有任何报错,
    /// 只会让这个数悄悄偏小 ——「列举式门控在加档时必然漏」这条教训在本
    /// 项目已经踩过四次,这里直接上机械判据。
    ///
    /// 自证会变红:把 `realloc` 或 `alloc_zeroed` 的方法体改成只有一句
    /// `unsafe { System.… }`。
    #[test]
    fn every_allocator_method_touches_the_counters() {
        let prod = prod_src();
        for m in [
            "fn alloc(",
            "fn dealloc(",
            "fn alloc_zeroed(",
            "fn realloc(",
        ] {
            let at = prod.find(m).unwrap_or_else(|| panic!("找不到 {m}"));
            let body = &prod[at..];
            let end = body[1..]
                .find("\n    unsafe fn ")
                .map_or(body.len(), |i| i + 1);
            let body = &body[..end];
            assert!(
                body.contains("self.0.on_alloc(") || body.contains("self.0.on_free("),
                "{m} 的方法体里没有任何记账"
            );
        }
    }

    /// 这一层的价值全部依赖「它真的是进程的全局分配器」。挂错地方(比如
    /// 挂在 `main.rs` 上)时,库里这些用例照样全绿、生产里也照样能跑,
    /// 只有日志上那个数恒为 0 —— 而「0MB」和「一个字节都没分配」长得一样。
    ///
    /// 自证会变红:把 `lib.rs` 里的 `#[global_allocator]` 那一段删掉,
    /// 或者把它指向别的分配器。
    #[test]
    fn the_process_actually_runs_on_this_allocator() {
        let src = include_str!("lib.rs");
        let at = src
            .find("#[global_allocator]")
            .expect("lib.rs 里没有 #[global_allocator]");
        let decl = &src[at..(at + 200).min(src.len())];
        assert!(
            decl.contains("heapgauge::CountingAlloc"),
            "全局分配器不是本模块这一个:{decl}"
        );
        // 而且它确实在数:测试进程本身就跑在它上面,活字节不可能是 0。
        assert!(
            live_bytes() > 0,
            "跑着测试的进程报堆用量为 0 —— 计数器没接上"
        );
    }

    /// 生产段(剥掉 `#[cfg(test)]` 之后的部分)。注释行一并剥掉:上面那条
    /// 完备性判据找的是 `on_alloc(`/`on_free(`,而本文件的注释里就写着这
    /// 两个名字,不剥的话某个方法体即使一句记账都没有也照样绿。
    fn prod_src() -> String {
        include_str!("heapgauge.rs")
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .expect("生产段")
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}
