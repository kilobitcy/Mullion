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
    /// F133:PDF。
    Pdf,
    /// F133:Word 文档。
    Word,
    /// F133:表格(含 csv —— 双击它多半是想到表格里看)。
    Excel,
    /// F133:演示文稿。
    Slides,
    Exec,
    Link,
    /// F134:普通文件的兜底(扩展名不认识、也没有可执行位)。
    File,
    /// F134:`EntryKind::Other` 专用 —— 设备文件 / socket / 命名管道。
    /// **不含**「不认识的普通文件」,那是 `File`。
    Other,
}

impl IconKind {
    /// 全部类型。**加变体必须同时加进这里** —— 「两两长得不一样」「不越格」
    /// 两条守护都照它遍历,漏加等于新类型不受任何守护。
    /// `every_kind_used_by_the_extension_table_is_listed_in_all` 会逮住漏加。
    pub const ALL: &'static [IconKind] = &[
        IconKind::Dir,
        IconKind::Archive,
        IconKind::Image,
        IconKind::Code,
        IconKind::Doc,
        IconKind::Pdf,
        IconKind::Word,
        IconKind::Excel,
        IconKind::Slides,
        IconKind::Exec,
        IconKind::Link,
        IconKind::File,
        IconKind::Other,
    ];
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
    ("pdf", IconKind::Pdf),
    ("doc", IconKind::Word),
    ("docx", IconKind::Word),
    ("xls", IconKind::Excel),
    ("xlsx", IconKind::Excel),
    ("csv", IconKind::Excel),
    ("ppt", IconKind::Slides),
    ("pptx", IconKind::Slides),
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
    // F134:不认识的**普通文件**用普通文件的图标。`Other` 只留给
    // `EntryKind::Other`(设备/socket/命名管道)——上面已经 return 过了。
    IconKind::File
}

/// 一个图标由若干条折线组成(闭合与否由调用方按形状约定)。
/// 抽出来只为**可单测** —— 像素长什么样仍然只有人眼能判。
pub fn outline(rect: egui::Rect, kind: IconKind) -> Vec<Vec<egui::Pos2>> {
    // 留一圈内边距,图标不顶满行高。
    let r = rect.shrink(2.0);
    let (l, t, rt, b) = (r.left(), r.top(), r.right(), r.bottom());
    match kind {
        // 文件夹:带页签的梯形。
        IconKind::Dir => vec![vec![
            egui::pos2(l, b),
            egui::pos2(l, t + r.height() * 0.25),
            egui::pos2(l + r.width() * 0.4, t + r.height() * 0.25),
            egui::pos2(l + r.width() * 0.5, t),
            egui::pos2(rt, t),
            egui::pos2(rt, b),
            egui::pos2(l, b),
        ]],
        // 归档:盒子 + 一条捆带。
        IconKind::Archive => vec![
            vec![
                egui::pos2(l, t + r.height() * 0.2),
                egui::pos2(rt, t + r.height() * 0.2),
                egui::pos2(rt, b),
                egui::pos2(l, b),
                egui::pos2(l, t + r.height() * 0.2),
            ],
            vec![
                egui::pos2(l + r.width() * 0.4, t + r.height() * 0.2),
                egui::pos2(l + r.width() * 0.4, b),
            ],
            vec![
                egui::pos2(l + r.width() * 0.6, t + r.height() * 0.2),
                egui::pos2(l + r.width() * 0.6, b),
            ],
        ],
        // 图片:相框 + 里面一座山。
        IconKind::Image => vec![
            vec![
                egui::pos2(l, t),
                egui::pos2(rt, t),
                egui::pos2(rt, b),
                egui::pos2(l, b),
                egui::pos2(l, t),
            ],
            vec![
                egui::pos2(l + r.width() * 0.15, b - r.height() * 0.2),
                egui::pos2(l + r.width() * 0.4, t + r.height() * 0.45),
                egui::pos2(l + r.width() * 0.6, b - r.height() * 0.2),
            ],
        ],
        // 代码:一对尖括号。
        IconKind::Code => vec![
            vec![
                egui::pos2(l + r.width() * 0.4, t + r.height() * 0.15),
                egui::pos2(l, r.center().y),
                egui::pos2(l + r.width() * 0.4, b - r.height() * 0.15),
            ],
            vec![
                egui::pos2(rt - r.width() * 0.4, t + r.height() * 0.15),
                egui::pos2(rt, r.center().y),
                egui::pos2(rt - r.width() * 0.4, b - r.height() * 0.15),
            ],
        ],
        // 文档:页 + 三条文字线(与「归档」的盒子形状区分开)。
        IconKind::Doc => {
            let mut v = vec![vec![
                egui::pos2(l + r.width() * 0.15, t),
                egui::pos2(rt - r.width() * 0.15, t),
                egui::pos2(rt - r.width() * 0.15, b),
                egui::pos2(l + r.width() * 0.15, b),
                egui::pos2(l + r.width() * 0.15, t),
            ]];
            for i in 1..=3 {
                let y = t + r.height() * (0.2 * i as f32 + 0.1);
                v.push(vec![
                    egui::pos2(l + r.width() * 0.3, y),
                    egui::pos2(rt - r.width() * 0.3, y),
                ]);
            }
            v
        }
        // PDF:页 + 底部一条实心横条(粗到一眼能认出是色块而不是文字线)。
        IconKind::Pdf => {
            let (pl, pr) = (l + r.width() * 0.15, rt - r.width() * 0.15);
            vec![
                vec![
                    egui::pos2(pl, t),
                    egui::pos2(pr, t),
                    egui::pos2(pr, b),
                    egui::pos2(pl, b),
                    egui::pos2(pl, t),
                ],
                vec![
                    egui::pos2(pl, b - r.height() * 0.3),
                    egui::pos2(pr, b - r.height() * 0.3),
                    egui::pos2(pr, b - r.height() * 0.12),
                    egui::pos2(pl, b - r.height() * 0.12),
                    egui::pos2(pl, b - r.height() * 0.3),
                ],
            ]
        }
        // Word:页 + 一个折线 W。
        IconKind::Word => {
            let (pl, pr) = (l + r.width() * 0.15, rt - r.width() * 0.15);
            let (wt, wb) = (t + r.height() * 0.35, b - r.height() * 0.2);
            vec![
                vec![
                    egui::pos2(pl, t),
                    egui::pos2(pr, t),
                    egui::pos2(pr, b),
                    egui::pos2(pl, b),
                    egui::pos2(pl, t),
                ],
                vec![
                    egui::pos2(pl + r.width() * 0.08, wt),
                    egui::pos2(pl + r.width() * 0.22, wb),
                    egui::pos2(r.center().x, wt + r.height() * 0.18),
                    egui::pos2(pr - r.width() * 0.22, wb),
                    egui::pos2(pr - r.width() * 0.08, wt),
                ],
            ]
        }
        // Excel:页 + 2×2 网格。
        IconKind::Excel => {
            let (pl, pr) = (l + r.width() * 0.15, rt - r.width() * 0.15);
            let (gt, gb) = (t + r.height() * 0.35, b - r.height() * 0.15);
            let (gl, gr) = (pl + r.width() * 0.08, pr - r.width() * 0.08);
            vec![
                vec![
                    egui::pos2(pl, t),
                    egui::pos2(pr, t),
                    egui::pos2(pr, b),
                    egui::pos2(pl, b),
                    egui::pos2(pl, t),
                ],
                vec![
                    egui::pos2(gl, gt),
                    egui::pos2(gr, gt),
                    egui::pos2(gr, gb),
                    egui::pos2(gl, gb),
                    egui::pos2(gl, gt),
                ],
                vec![
                    egui::pos2(gl, (gt + gb) * 0.5),
                    egui::pos2(gr, (gt + gb) * 0.5),
                ],
                vec![
                    egui::pos2((gl + gr) * 0.5, gt),
                    egui::pos2((gl + gr) * 0.5, gb),
                ],
            ]
        }
        // 演示文稿:页 + 一块横向「屏幕」条(比 Excel 的网格宽而扁)。
        IconKind::Slides => {
            let (pl, pr) = (l + r.width() * 0.15, rt - r.width() * 0.15);
            vec![
                vec![
                    egui::pos2(pl, t),
                    egui::pos2(pr, t),
                    egui::pos2(pr, b),
                    egui::pos2(pl, b),
                    egui::pos2(pl, t),
                ],
                vec![
                    egui::pos2(pl + r.width() * 0.06, t + r.height() * 0.38),
                    egui::pos2(pr - r.width() * 0.06, t + r.height() * 0.38),
                    egui::pos2(pr - r.width() * 0.06, b - r.height() * 0.28),
                    egui::pos2(pl + r.width() * 0.06, b - r.height() * 0.28),
                    egui::pos2(pl + r.width() * 0.06, t + r.height() * 0.38),
                ],
            ]
        }
        // 可执行:一个朝右的三角(播放/运行)+ 底座。
        IconKind::Exec => vec![
            vec![
                egui::pos2(l + r.width() * 0.25, t + r.height() * 0.1),
                egui::pos2(rt - r.width() * 0.1, r.center().y),
                egui::pos2(l + r.width() * 0.25, b - r.height() * 0.3),
                egui::pos2(l + r.width() * 0.25, t + r.height() * 0.1),
            ],
            vec![egui::pos2(l, b), egui::pos2(rt, b)],
        ],
        // 符号链接:页 + 一个指出去的箭头。
        IconKind::Link => {
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
                vec![
                    egui::pos2(l + r.width() * 0.25, b - r.height() * 0.25),
                    egui::pos2(rt - r.width() * 0.2, t + r.height() * 0.35),
                ],
            ]
        }
        // F134:普通文件 —— 折角空白页。跟 `Doc`(页 + 三条横线)、
        // `Link`(折角页 + 箭头)靠「有没有横线 / 有没有箭头」区分。
        // 页宽的内边距取 0.1,**故意**跟 `Link`(0,贴边)和 `Doc`/四类 office
        // (0.15)都错开:这三支都是「一张页」,箭头/横线在 16px 下只有一两个
        // 像素,页宽差是第二道区分。看着不统一也别顺手对齐。
        IconKind::File => {
            let fold = r.width() * 0.3;
            vec![
                vec![
                    egui::pos2(l + r.width() * 0.1, t),
                    egui::pos2(rt - r.width() * 0.1 - fold, t),
                    egui::pos2(rt - r.width() * 0.1, t + fold),
                    egui::pos2(rt - r.width() * 0.1, b),
                    egui::pos2(l + r.width() * 0.1, b),
                    egui::pos2(l + r.width() * 0.1, t),
                ],
                vec![
                    egui::pos2(rt - r.width() * 0.1 - fold, t),
                    egui::pos2(rt - r.width() * 0.1 - fold, t + fold),
                    egui::pos2(rt - r.width() * 0.1, t + fold),
                ],
            ]
        }
        // 其他(设备文件/socket/命名管道等,SFTP 协议里存在但没有专门图标的
        // 类型):菱形,与另外几种都不同形,不落回文件/文件夹的形状假装认识它。
        IconKind::Other => vec![vec![
            egui::pos2(l + r.width() * 0.5, t),
            egui::pos2(rt, t + r.height() * 0.5),
            egui::pos2(l + r.width() * 0.5, b),
            egui::pos2(l, t + r.height() * 0.5),
            egui::pos2(l + r.width() * 0.5, t),
        ]],
    }
}

/// 把 `outline` 画出来。
pub fn paint(painter: &egui::Painter, rect: egui::Rect, kind: IconKind, color: egui::Color32) {
    for line in outline(rect, kind) {
        painter.add(egui::Shape::line(line, egui::Stroke::new(1.0, color)));
    }
}

/// F127:类型 → 颜色。**不可操作的行恒 `fg_dimmer`**,与名称文字同源 ——
/// 两套判据会出现「文字灰了图标还亮着」这种自相矛盾的行(D1 定的闸门)。
pub fn color_for(
    kind: IconKind,
    usable: bool,
    t: &crate::theme::Theme,
) -> mullion_term::snapshot::Rgb {
    if !usable {
        return t.fg_dimmer;
    }
    match kind {
        IconKind::Dir => t.icon_dir,
        IconKind::Archive => t.icon_archive,
        IconKind::Image => t.icon_image,
        IconKind::Code => t.icon_code,
        IconKind::Doc => t.icon_doc,
        IconKind::Pdf => t.icon_pdf,
        IconKind::Word => t.icon_word,
        IconKind::Excel => t.icon_excel,
        IconKind::Slides => t.icon_slides,
        IconKind::Exec => t.icon_exec,
        IconKind::Link => t.icon_link,
        IconKind::File => t.icon_file,
        IconKind::Other => t.icon_other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 图标必须**画在给定的格子里**。越界的话它会压到相邻列的文字上,
    /// 而 painter 直接按坐标画、不受布局约束,越界了编译器一声不吭。
    ///
    /// 自证会变红:把 `outline` 里的 `rect.shrink(2.0)` 改成
    /// `rect.expand(2.0)`。
    #[test]
    fn every_icon_stays_inside_its_cell() {
        let cell = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(16.0, 16.0));
        for &kind in IconKind::ALL {
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

    /// 八种类型必须两两长得不一样 —— 否则「这是目录还是压缩包还是别的什么」
    /// 这个图标本来要回答的问题它没有回答。
    ///
    /// 判据是顶点序列的 `{:?}` 全等,**只是弱守护**:它挡得住「复制粘贴成
    /// 同一支形状」这类回归,挡不住「新形状和老形状长得太像」——后者只有
    /// 人眼能判,归人工验收。
    ///
    /// 自证会变红:把 `Link` 那一支改成直接 `outline(rect, IconKind::Other)`。
    #[test]
    fn every_kind_looks_different_from_every_other_kind() {
        let cell = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(16.0, 16.0));
        let shapes: Vec<(IconKind, String)> = IconKind::ALL
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
            ("report.pdf", IconKind::Pdf),
            ("合同.DOCX", IconKind::Word),
            ("notes.doc", IconKind::Word),
            ("budget.xlsx", IconKind::Excel),
            ("data.csv", IconKind::Excel),
            ("deck.pptx", IconKind::Slides),
            ("deck.ppt", IconKind::Slides),
            ("data.bin", IconKind::File),
            ("Makefile", IconKind::File),
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
    /// 自证会变红:把 `ext_of` 里那句 `if name[..ix].is_empty() { return
    /// String::new() }` 删掉 —— 但只用 `.bashrc` 测不出来(`bashrc` 本来就不在
    /// `EXT_TABLE` 里,删不删这条分支结果都是 `Other`)。真正压到这条分支的是
    /// `.gz` 这种「裸后缀」文件名:删掉该分支后,`ext_of` 会把 `.gz` 的
    /// 「扩展名」算成 `gz`,而 `gz` **确实在表里**,于是误判成 `Archive`。
    #[test]
    fn dotfiles_have_no_extension() {
        assert_eq!(classify(EntryKind::File, ".bashrc", 0o644), IconKind::File);
        assert_eq!(
            classify(EntryKind::File, ".config.json", 0o644),
            IconKind::Doc,
            "点开头但确实有扩展名的照常判"
        );
        assert_eq!(
            classify(EntryKind::File, ".gz", 0o644),
            IconKind::File,
            "点号在开头时,哪怕「扩展名」凑巧撞上表项也不能当真"
        );
    }

    /// F127:颜色由类型决定,且**不可操作的行仍然整体变灰** —— 这是 D1 定的
    /// 闸门,不能因为加了类型色就丢掉,否则会出现「文字灰了图标还亮着」。
    ///
    /// 自证会变红:把 `color_for` 里 `if !usable` 那一支删掉。
    #[test]
    fn unusable_rows_stay_dim_even_with_type_colors() {
        let t = crate::theme::MULLION_DARK;
        assert_eq!(color_for(IconKind::Archive, true, &t), t.icon_archive);
        assert_eq!(color_for(IconKind::Archive, false, &t), t.fg_dimmer);
    }

    /// F134:「不认识的普通文件」和「设备/socket 这类特殊类型」必须是两种
    /// 图标。合成一种的话,一屏陌生扩展名的普通文件全成了菱形,而真正需要
    /// 「这不是普通文件」提示的那几条被淹掉了。
    ///
    /// 自证会变红:把 `classify` 末尾的兜底从 `IconKind::File` 改回
    /// `IconKind::Other`。
    #[test]
    fn an_unknown_regular_file_is_not_the_same_as_a_device_node() {
        assert_eq!(classify(EntryKind::File, "data.bin", 0o644), IconKind::File);
        assert_eq!(classify(EntryKind::Other, "ttyS0", 0o666), IconKind::Other);
        assert_ne!(
            outline(
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(16.0, 16.0)),
                IconKind::File
            ),
            outline(
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(16.0, 16.0)),
                IconKind::Other
            ),
            "两者共用了同一支形状"
        );
    }

    /// 加了新类型却忘了写进 `IconKind::ALL` 的话,「两两不同」「不越格」两条
    /// 守护会**悄悄漏掉**新类型 —— 本项目已经踩过三次「列举式门控在加档时
    /// 必然漏」。这条把 `EXT_TABLE` 当交叉验证:表里出现过的每个类型都必须
    /// 在 `ALL` 里。
    ///
    /// 自证会变红:往 `EXT_TABLE` 加一个 `ALL` 里没有的类型。
    #[test]
    fn every_kind_used_by_the_extension_table_is_listed_in_all() {
        for (ext, kind) in EXT_TABLE {
            assert!(
                IconKind::ALL.contains(kind),
                "{kind:?}(来自扩展名 {ext})不在 IconKind::ALL 里"
            );
        }
    }
}
