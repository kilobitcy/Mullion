//! 由中央区可用像素(= 窗口减去 egui 上下栏后)和字元像素尺寸,算终端网格列/行数。
//! A2b 会把这个结果喂给 reflow / window_change(F34/T4):上下栏吃掉的空间必须先扣除,
//! 否则远端 tmux 按错误列数排版。

/// `area_px`:中央区可用像素 (宽, 高);`cell_px`:单字元像素 (宽, 高);
/// `min`:最小 (列, 行),夹紧下限。字元尺寸为 0 时安全回落到 `min`(防除零)。
pub fn grid_dims(area_px: (u32, u32), cell_px: (u32, u32), min: (u16, u16)) -> (u16, u16) {
    // checked_div:字元尺寸为 0 时返回 None → 回落到最小(防除零),同时满足 clippy。
    // try_from 饱和到 u16::MAX,避免超大面积/极小字元时 `as u16` 静默回绕成小值。
    let cols = area_px
        .0
        .checked_div(cell_px.0)
        .map_or(min.0, |c| u16::try_from(c).unwrap_or(u16::MAX));
    let rows = area_px
        .1
        .checked_div(cell_px.1)
        .map_or(min.1, |r| u16::try_from(r).unwrap_or(u16::MAX));
    (cols.max(min.0), rows.max(min.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divides_area_by_cell() {
        // 800x600 物理像素,字元 10x20 → 80 列 x 30 行
        assert_eq!(grid_dims((800, 600), (10, 20), (1, 1)), (80, 30));
    }

    #[test]
    fn subtract_chrome_before_dividing() {
        // 上下栏共占 40px 高、菜单不占宽:可用区 800x(600-40)=800x560 → 80 x 28
        let avail = (800, 600 - 40);
        assert_eq!(grid_dims(avail, (10, 20), (1, 1)), (80, 28));
    }

    #[test]
    fn clamps_to_minimum() {
        assert_eq!(grid_dims((5, 5), (10, 20), (2, 2)), (2, 2));
    }

    #[test]
    fn zero_cell_is_safe() {
        // 防除零:字元尺寸为 0 时回落到最小,不 panic
        assert_eq!(grid_dims((800, 600), (0, 0), (4, 3)), (4, 3));
    }
}
