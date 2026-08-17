//! D1:文件类型图标。**painter 自绘,不用字体字形**。
//!
//! 为什么不用 emoji/字符:字形是否存在取决于字体,Windows 上会变豆腐块;
//! 而且字形宽度不可控,整列的名称起始位置会跟着字体飘。自绘不依赖字体。
//!
//! 颜色**不在这里决定** —— 由调用方传入,取的是 `row()` 里那套既有的语义色
//! (目录 `fg_strong`、文件 `fg`、名称不可操作 `fg_dimmer`)。图标和文字用
//! 两套判据的话,会出现「文字灰了图标还亮着」这种自相矛盾的行。

use mullion_ssh::sftp::EntryKind;

/// 一个图标由若干条折线组成(闭合与否由调用方按形状约定)。
/// 抽出来只为**可单测** —— 像素长什么样仍然只有人眼能判。
pub fn outline(rect: egui::Rect, kind: EntryKind) -> Vec<Vec<egui::Pos2>> {
    // 留一圈内边距,图标不顶满行高。
    let r = rect.shrink(2.0);
    let (l, t, rt, b) = (r.left(), r.top(), r.right(), r.bottom());
    match kind {
        // 文件夹:带页签的梯形。
        EntryKind::Dir => vec![vec![
            egui::pos2(l, b),
            egui::pos2(l, t + r.height() * 0.25),
            egui::pos2(l + r.width() * 0.4, t + r.height() * 0.25),
            egui::pos2(l + r.width() * 0.5, t),
            egui::pos2(rt, t),
            egui::pos2(rt, b),
            egui::pos2(l, b),
        ]],
        // 文件:右上角折角的页。两条折线 —— 页身 + 折角。
        EntryKind::File => {
            let fold = r.width() * 0.3;
            vec![
                vec![
                    egui::pos2(l, t),
                    egui::pos2(rt - fold, t),
                    egui::pos2(rt, t + fold),
                    egui::pos2(rt, b),
                    egui::pos2(l, b),
                    egui::pos2(l, t),
                ],
                vec![
                    egui::pos2(rt - fold, t),
                    egui::pos2(rt - fold, t + fold),
                    egui::pos2(rt, t + fold),
                ],
            ]
        }
        // 符号链接:页 + 一个指出去的箭头。
        EntryKind::Symlink => {
            let mut v = outline(rect, EntryKind::File);
            v.push(vec![
                egui::pos2(l + r.width() * 0.25, b - r.height() * 0.25),
                egui::pos2(rt - r.width() * 0.2, t + r.height() * 0.35),
            ]);
            v
        }
        // 其他(设备文件/socket/命名管道等,SFTP 协议里存在但没有专门图标的
        // 类型):菱形,与另外三种都不同形,不落回文件/文件夹的形状假装认识它。
        EntryKind::Other => vec![vec![
            egui::pos2(l + r.width() * 0.5, t),
            egui::pos2(rt, t + r.height() * 0.5),
            egui::pos2(l + r.width() * 0.5, b),
            egui::pos2(l, t + r.height() * 0.5),
            egui::pos2(l + r.width() * 0.5, t),
        ]],
    }
}

/// 把 `outline` 画出来。
pub fn paint(painter: &egui::Painter, rect: egui::Rect, kind: EntryKind, color: egui::Color32) {
    for line in outline(rect, kind) {
        painter.add(egui::Shape::line(line, egui::Stroke::new(1.0, color)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_KINDS: [EntryKind; 4] = [
        EntryKind::Dir,
        EntryKind::File,
        EntryKind::Symlink,
        EntryKind::Other,
    ];

    /// 图标必须**画在给定的格子里**。越界的话它会压到相邻列的文字上,
    /// 而 painter 直接按坐标画、不受布局约束,越界了编译器一声不吭。
    ///
    /// 自证会变红:把 `outline` 里的 `rect.shrink(2.0)` 改成
    /// `rect.expand(2.0)`。
    #[test]
    fn every_icon_stays_inside_its_cell() {
        let cell = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(16.0, 16.0));
        for kind in ALL_KINDS {
            for line in outline(cell, kind) {
                for p in line {
                    assert!(
                        cell.contains(p),
                        "{kind:?} 的顶点 {p:?} 跑出了格子 {cell:?}"
                    );
                }
            }
        }
    }

    /// 四种类型必须两两长得不一样 —— 否则「这是目录还是文件还是别的什么」
    /// 这个图标本来要回答的问题它没有回答。
    ///
    /// 自证会变红:把 `Symlink` 那一支改成直接 `outline(rect, File)`,或把
    /// `Other` 改成直接 `outline(rect, File)`。
    #[test]
    fn every_kind_looks_different_from_every_other_kind() {
        let cell = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(16.0, 16.0));
        let shapes: Vec<(EntryKind, String)> = ALL_KINDS
            .iter()
            .map(|&k| (k, format!("{:?}", outline(cell, k))))
            .collect();
        for i in 0..shapes.len() {
            for j in (i + 1)..shapes.len() {
                let (ki, si) = &shapes[i];
                let (kj, sj) = &shapes[j];
                assert_ne!(si, sj, "{ki:?} 和 {kj:?} 长得一样");
            }
        }
    }
}
