//! App 外壳的无头逻辑基座:会话→连接映射、视口尺寸、输入路由、会话存储封装。
//! 零 winit/wgpu/egui —— 可纯单测。A2b 把这些接进 app.rs 事件循环。

pub mod dial_plan;
pub mod input_route;
pub mod session_map;
pub mod store;
pub mod window_state;
pub mod workspace;
