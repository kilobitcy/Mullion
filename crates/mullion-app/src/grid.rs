//! 由窗口像素尺寸算终端列/行数(F34 前置)。纯函数,可脱离窗口单测。

/// 像素尺寸 + 单元格尺寸 → (cols, rows)。向下取整,至少 1×1。
pub fn grid_size_for(px_w: u32, px_h: u32, cell_w: f32, cell_h: f32) -> (u16, u16) {
    let cols = ((px_w as f32 / cell_w).floor() as u32).clamp(1, u16::MAX as u32);
    let rows = ((px_h as f32 / cell_h).floor() as u32).clamp(1, u16::MAX as u32);
    (cols as u16, rows as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divides_pixels_by_cell() {
        assert_eq!(grid_size_for(800, 600, 10.0, 20.0), (80, 30));
    }

    #[test]
    fn floors_partial_cells() {
        assert_eq!(grid_size_for(805, 615, 10.0, 20.0), (80, 30));
    }

    #[test]
    fn clamps_to_at_least_one() {
        // 窗口比一个单元格还小时不能返回 0 列(会开出非法 PTY)。
        assert_eq!(grid_size_for(5, 5, 10.0, 20.0), (1, 1));
    }
}
