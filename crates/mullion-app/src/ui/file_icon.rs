//! D1:文件类型图标。**painter 自绘,不用字体字形**。
//!
//! 为什么不用 emoji/字符:字形是否存在取决于字体,Windows 上会变豆腐块;
//! 而且字形宽度不可控,整列的名称起始位置会跟着字体飘。自绘不依赖字体。
//!
//! 颜色**不在这里决定** —— 由调用方传入,取的是 `row()` 里那套既有的语义色
//! (目录 `fg_strong`、文件 `fg`、名称不可操作 `fg_dimmer`)。图标和文字用
//! 两套判据的话,会出现「文字灰了图标还亮着」这种自相矛盾的行。

use mullion_ssh::sftp::EntryKind;

/// F127:图标类型。比 `EntryKind` 细 —— 一屏全是同一个页角图标时,
/// 用户扫视找不到目标文件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    Dir,
    Archive,
    Image,
    Code,
    Doc,
    Exec,
    Link,
    Other,
}

/// 扩展名 → 类型。**唯一的一张表**,加类型只改这里。小写比对,调用方负责归一。
const EXT_TABLE: &[(&str, IconKind)] = &[
    ("zip", IconKind::Archive),
    ("tar", IconKind::Archive),
    ("gz", IconKind::Archive),
    ("tgz", IconKind::Archive),
    ("bz2", IconKind::Archive),
    ("xz", IconKind::Archive),
    ("7z", IconKind::Archive),
    ("rar", IconKind::Archive),
    ("png", IconKind::Image),
    ("jpg", IconKind::Image),
    ("jpeg", IconKind::Image),
    ("gif", IconKind::Image),
    ("bmp", IconKind::Image),
    ("svg", IconKind::Image),
    ("webp", IconKind::Image),
    ("ico", IconKind::Image),
    ("rs", IconKind::Code),
    ("py", IconKind::Code),
    ("sh", IconKind::Code),
    ("js", IconKind::Code),
    ("ts", IconKind::Code),
    ("c", IconKind::Code),
    ("h", IconKind::Code),
    ("cpp", IconKind::Code),
    ("go", IconKind::Code),
    ("java", IconKind::Code),
    ("rb", IconKind::Code),
    ("lua", IconKind::Code),
    ("toml", IconKind::Code),
    ("yaml", IconKind::Code),
    ("yml", IconKind::Code),
    ("md", IconKind::Doc),
    ("txt", IconKind::Doc),
    ("log", IconKind::Doc),
    ("json", IconKind::Doc),
    ("csv", IconKind::Doc),
    ("pdf", IconKind::Doc),
    ("doc", IconKind::Doc),
    ("docx", IconKind::Doc),
    ("exe", IconKind::Exec),
    ("msi", IconKind::Exec),
    ("bat", IconKind::Exec),
    ("cmd", IconKind::Exec),
];

/// 取小写扩展名。**点开头的隐藏文件没有扩展名**(`.bashrc` 不是「bashrc 类型」)。
fn ext_of(name: &str) -> String {
    let Some(ix) = name.rfind('.') else {
        return String::new();
    };
    if name[..ix].is_empty() {
        return String::new(); // `.bashrc`
    }
    name[ix + 1..].to_ascii_lowercase()
}

/// F127:一行该画哪种图标。
///
/// 优先级(顺序不可换):
/// 1. `EntryKind` 里的目录/链接/其他 —— 一个叫 `backup.zip` 的目录仍是目录。
/// 2. 扩展名查表 —— **优先于可执行位**:远端脚本本来就常带 `+x`,
///    反过来会让半屏 `.sh` 全变成齿轮。
/// 3. 可执行位(`mode` 的任意 x 位)。
/// 4. 其他。
pub fn classify(kind: EntryKind, name: &str, mode: u32) -> IconKind {
    match kind {
        EntryKind::Dir => return IconKind::Dir,
        EntryKind::Symlink => return IconKind::Link,
        EntryKind::Other => return IconKind::Other,
        EntryKind::File => {}
    }
    let ext = ext_of(name);
    if let Some((_, k)) = EXT_TABLE.iter().find(|(e, _)| *e == ext) {
        return *k;
    }
    if mode & 0o111 != 0 {
        return IconKind::Exec;
    }
    IconKind::Other
}

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

    /// F127:扩展名 → 类型的表驱动判类。大小写不敏感(远端上 `.PNG` 常见)。
    #[test]
    fn classify_maps_extensions_to_kinds() {
        for (name, want) in [
            ("a.zip", IconKind::Archive),
            ("a.TAR.GZ", IconKind::Archive),
            ("a.tgz", IconKind::Archive),
            ("photo.png", IconKind::Image),
            ("photo.JPEG", IconKind::Image),
            ("main.rs", IconKind::Code),
            ("build.sh", IconKind::Code),
            ("Cargo.toml", IconKind::Code),
            ("README.md", IconKind::Doc),
            ("app.log", IconKind::Doc),
            ("setup.exe", IconKind::Exec),
            ("data.bin", IconKind::Other),
            ("Makefile", IconKind::Other),
        ] {
            assert_eq!(
                classify(EntryKind::File, name, 0o644),
                want,
                "{name} 判错了"
            );
        }
    }

    /// F127:目录 / 链接 由 `EntryKind` 决定,**扩展名说了不算** ——
    /// 一个叫 `backup.zip` 的目录仍然是目录。
    ///
    /// 自证会变红:把 `classify` 里 `EntryKind::Dir` 那一支删掉。
    #[test]
    fn entry_kind_wins_over_extension_for_dirs_and_links() {
        assert_eq!(classify(EntryKind::Dir, "backup.zip", 0o755), IconKind::Dir);
        assert_eq!(
            classify(EntryKind::Symlink, "latest.png", 0o777),
            IconKind::Link
        );
        assert_eq!(classify(EntryKind::Other, "ttyS0", 0o666), IconKind::Other);
    }

    /// F127:**扩展名优先于可执行位**。远端上的脚本本来就常带 `+x`,
    /// 反过来的话半屏 `.sh`/`.py` 会全变成齿轮,类型信息反而丢了。
    ///
    /// 自证会变红:把 `classify` 里可执行位那段判断挪到扩展名查表之前。
    #[test]
    fn extension_wins_over_the_execute_bit() {
        assert_eq!(classify(EntryKind::File, "run.sh", 0o755), IconKind::Code);
        assert_eq!(
            classify(EntryKind::File, "mullion", 0o755),
            IconKind::Exec,
            "没扩展名 + 有 x 位才判可执行"
        );
    }

    /// F127:点开头的隐藏文件不能把整个名字当扩展名 —— `.bashrc` 的
    /// 「扩展名」是空的,该落到 `Other`,而不是去查一个叫 `bashrc` 的扩展名。
    ///
    /// 自证会变红:把 `ext_of` 里那句 `if stem.is_empty() { return "" }` 删掉。
    #[test]
    fn dotfiles_have_no_extension() {
        assert_eq!(classify(EntryKind::File, ".bashrc", 0o644), IconKind::Other);
        assert_eq!(
            classify(EntryKind::File, ".config.json", 0o644),
            IconKind::Doc,
            "点开头但确实有扩展名的照常判"
        );
    }
}
