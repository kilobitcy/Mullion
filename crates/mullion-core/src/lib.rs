//! mullion-core —— 布局树(自研分屏)。零 UI、零 IO、零 async,可纯单测。
//!
//! 架构不变量:本 crate 不依赖 term/ssh/app,也不引入任何 UI/IO/async 类型。
//! 布局 bug 能在没有窗口的情况下写测试复现,这是本项目可测试性的核心资产。

pub mod layout;
