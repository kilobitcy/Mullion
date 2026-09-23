//! 守护:弹窗不许排到窗口外面(F285)。
//!
//! F280 发现「设置」弹窗在小屏上排不下,给它夹了高度上界。**宽度那一半从
//! 来没生效过**:`settings.rs` 的「远端」分节里两段 200 余字的灰色说明挂在
//! `form::grid` 的单元格里,而 Grid 单元格在**自动定尺的 `egui::Window`** 中
//! 拿到的可用宽是无穷 —— 整段排成一行,把窗口撑到 2243 逻辑点,与窗口实际
//! 多宽**完全无关**。窗口是 `CENTER_CENTER` 锚定的,于是左右**对称**各切掉
//! 一半:1366 宽的窗口上左边 439 点、右边 438 点的内容都在窗外,划都划不到。
//!
//! 为什么必须是**行为式**守护(真渲染量矩形),不能照 `dialog_contrast.rs`
//! 那样扫源码:溢出是排版结果,源码里没有任何一个字符串能把它认出来。而它
//! 100% 静默 —— 编译过、测试绿、开发机屏够大时人眼也看不出来。
//!
//! 两条断言,缺一不可:
//!
//! 1. **窗口矩形 ⊆ 屏幕矩形**。egui 的 `screen_rect` 在 winit 下就是窗口
//!    客户区,所以这条读作「弹窗不许排到 Mullion 窗口外面」。
//! 2. **「确定」「取消」落在窗口矩形内**。第 1 条管不了这个:`egui::Window`
//!    会按内容裁剪,正文太高时按钮行被挤到裁剪区外——窗口矩形仍然 ⊆ 屏幕,
//!    而用户看到一个**没有出口**的弹窗(F270 踩过的「没有出口的陷阱」)。
//!
//! 覆盖范围靠 [`LEDGER`] 的**计数对账**保证:名单里每个文件记着它有几个
//! `egui::Window::new(`,与源码现扫的结果逐项对齐。新加一个弹窗、或往老文件
//! 里再加一个,这条测试立刻变红,逼调用者回来表态「真守它」还是「写明为什么
//! 不用守」。「列举式门控在加档时必然漏」在本仓库已经踩中四次
//! (见 `dialog_contrast.rs` 头部),这是它的解药。

use std::path::{Path, PathBuf};

/// 量哪几档窗口尺寸。
///
/// 1024×600 是上网本/远程桌面小窗的常见下限;1366×768 是笔记本主流;
/// 1920×1080 作对照 —— 缺了它,「在大窗口上也不许溢出」这半边没人守,而
/// F285 的病灶恰恰是**窗口越大越看不出来**(2560 屏上一点异常都没有)。
const SIZES: [(f32, f32); 3] = [(1024.0, 600.0), (1366.0, 768.0), (1920.0, 1080.0)];

/// 跑够多少帧再读几何。
///
/// 照搬 `settings.rs` 测试里那个常量的理由:弹窗里嵌了 `ScrollArea` + 两层
/// `Grid`,宽高是**逐帧往外撑**的,实测十帧上下才收敛。帧数不够的症状是
/// 几何还差几个像素 —— 于是这条守护会报一个根本不存在的溢出(假红)。
const FRAMES: usize = 24;

/// 画一遍「设置」弹窗,返回 (窗口矩形, 这一帧画出来的全部文字 shape)。
fn render_settings(w: f32, h: f32) -> (egui::Rect, Vec<(String, egui::Rect)>) {
    use mullion_app::ui::settings::{self, SettingsDraft, SettingsEnv};

    let fams = mullion_app::font_pick::sort_families(vec![
        ("Cascadia Mono".into(), true),
        ("Arial".into(), false),
    ]);
    let t = mullion_app::theme::MULLION_DARK;
    let ctx = egui::Context::default();
    let mut draft = SettingsDraft::from_settings(&mullion_store::Settings::default());
    let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w, h));

    let mut texts = Vec::new();
    for _ in 0..FRAMES {
        let input = egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        };
        let full = ctx.run(input, |ctx| {
            settings::show(
                ctx,
                &t,
                &mut draft,
                SettingsEnv {
                    families: &fams,
                    not_monospace: false,
                    has_master_password: false,
                    store_available: true,
                    cloud_has_passphrase: false,
                },
            );
        });
        texts.clear();
        for cs in &full.shapes {
            collect_text(&cs.shape, &mut texts);
        }
    }
    // 取「这一帧最大的那个 egui area」当弹窗。**不写死标题字面串**:窗口的
    // area id 恒为 `Id::new(标题)`,照着字面串反算的话,标题改一个字这条守护
    // 就静默量到别的东西(F239 踩过同一个形状)。这一帧只画了这一个弹窗,
    // 最大面积的 area 就是它。
    let rect = ctx
        .memory(|m| {
            m.areas()
                .visible_layer_ids()
                .into_iter()
                .filter_map(|l| m.area_rect(l.id))
                .max_by(|a, b| a.area().total_cmp(&b.area()))
        })
        .expect("设置弹窗没画出来 —— 这条守护量不到任何东西");
    (rect, texts)
}

fn collect_text(shape: &egui::Shape, acc: &mut Vec<(String, egui::Rect)>) {
    match shape {
        egui::Shape::Vec(v) => v.iter().for_each(|s| collect_text(s, acc)),
        egui::Shape::Text(ts) => {
            acc.push((ts.galley.text().to_string(), ts.visual_bounding_rect()))
        }
        _ => {}
    }
}

/// `inner` 整个落在 `outer` 里吗?返回越界的那几边,便于报错时直接说清。
fn spill(inner: egui::Rect, outer: egui::Rect) -> String {
    let mut s = String::new();
    for (name, d) in [
        ("左", outer.left() - inner.left()),
        ("右", inner.right() - outer.right()),
        ("上", outer.top() - inner.top()),
        ("下", inner.bottom() - outer.bottom()),
    ] {
        // 0.5 点的容差:egui 的描边宽度会让矩形外扩半个像素,那不是排版溢出。
        if d > 0.5 {
            s.push_str(&format!("{name}溢出 {d:.0} 点;"));
        }
    }
    s
}

/// F285 ①:弹窗整体不许排到窗口外面。
///
/// 自证会变红:把 `settings.rs::remote` 里那两段说明的换行宽度去掉(改回
/// 直接 `ui.label(RichText::new("连上后开一条旁路命令通道…"))`)——Grid 单元格
/// 里可用宽无穷,整段排成一行,窗口立刻撑到 2243 点。
#[test]
fn the_settings_dialog_never_spills_out_of_the_window() {
    for (w, h) in SIZES {
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w, h));
        let (rect, texts) = render_settings(w, h);
        let bad = spill(rect, screen);
        // 把「这一帧最宽的那段文字」一起报出来:撑开弹窗的几乎一定是某段没换行
        // 的文案,直接指名道姓能省掉一轮 bisect(F285 那次是靠逐节注释找出来的)。
        let widest = texts
            .iter()
            .max_by(|a, b| a.1.width().total_cmp(&b.1.width()))
            .map(|(s, r)| {
                format!(
                    "{:.0} 点宽:{:?}",
                    r.width(),
                    s.chars().take(24).collect::<String>()
                )
            })
            .unwrap_or_default();
        assert!(
            bad.is_empty(),
            "{w}x{h} 的窗口上,「设置」弹窗排到窗外了({bad}) —— \
             窗口矩形 {rect:?}。用户看不到也划不到那部分内容。\
             这一帧最宽的一段文字是 {widest}"
        );
    }
}

/// F285 ②:「确定」「取消」必须落在弹窗可见范围内。
///
/// 上面那条管不了这个:正文撑得太高时按钮行会被 `egui::Window` 的裁剪区切掉,
/// 窗口矩形照样 ⊆ 屏幕,而用户看到的是一个**关不掉也确认不了**的弹窗。
///
/// 自证会变红:把 `settings.rs::show` 里的 `bottom_up` 布局拆掉、改成按顺序
/// 「正文在前、按钮在后」——按钮会被正文顶出裁剪区(F217 已经踩过一次)。
#[test]
fn the_settings_dialog_always_shows_its_ok_and_cancel_buttons() {
    for (w, h) in SIZES {
        let (rect, texts) = render_settings(w, h);
        for want in ["确定", "取消"] {
            let btn = texts
                .iter()
                .find(|(s, _)| s == want)
                .unwrap_or_else(|| panic!("{w}x{h} 上根本没画出「{want}」"))
                .1;
            let bad = spill(btn, rect);
            assert!(
                bad.is_empty(),
                "{w}x{h} 的窗口上,「{want}」被挤出弹窗可见范围({bad}) —— \
                 按钮 {btn:?} / 弹窗 {rect:?}。这个弹窗没有出口。"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 覆盖范围的计数对账
// ---------------------------------------------------------------------------

/// 标记:这个文件里的弹窗由本文件上面那两条测试真渲染守着。
const RENDERED: &str = "@本文件真渲染守着";

/// 全库每个含 `egui::Window::new(` 的文件 → (它有几个, 守它 / 为什么不守)。
///
/// 理由一律写「**为什么这个弹窗撑不开**」,不是「它看起来不大」—— F285 那个
/// bug 的全部教训就是:撑开它的是一段谁都没多看一眼的说明文字。
const LEDGER: &[(&str, usize, &str)] = &[
    (
        "ui/mod.rs",
        1,
        "关于窗:三行常量文案 + 一个按钮,没有任何随数据长短变化的内容",
    ),
    ("ui/settings.rs", 1, RENDERED),
    (
        "ui/project_manager.rs",
        3,
        "主窗自己 `set_width(LIST_W)` + 右栏走 `form::grid` 固定档宽;另两个是确认框,\
         文案是常量。长项目名走的是行内截断,不参与撑宽",
    ),
    (
        "ui/history.rs",
        1,
        "行宽由 `ui.available_width()` 取,不由内容推;正文套了 `max_height(320)` 的 ScrollArea",
    ),
    (
        "ui/editor_window.rs",
        1,
        "`resizable(true)` + `default_size` + `min_size`,尺寸由用户和 F216/F217 那套记忆管,\
         内容(文件正文)一律在 ScrollArea 里",
    ),
    (
        "ui/import_dialog.rs",
        1,
        "`resizable(true)`,列表在 ScrollArea 里,行内截断",
    ),
    (
        "ui/session_manager/mod.rs",
        3,
        "两个删除确认框的文案是常量;主窗是 `resizable(true)` + `min_width(880)` 的\
         独立设计,它自己的高度棘轮偏差已在 `session_manager/mod.rs` 就地长篇记录\
         (egui 算 `max_height` 时标题栏高度口径不一致)。**本轮 F285 没有真渲染守它**\
         —— 那是另一条线索,不是「已经确认撑不开」",
    ),
    ("ui/unlock.rs", 1, "一个密码框 + 一行按钮,文案全是常量"),
    (
        "ui/tab_props.rs",
        1,
        "一个输入框 + 一排取色块,宽度取自 `FIELD_W_*` 档位",
    ),
    ("ui/edit_panel.rs", 1, "列表在 ScrollArea 里,行内截断"),
    ("ui/group_manager.rs", 1, "列表在 ScrollArea 里,行内截断"),
    (
        "ui/pack_dialog.rs",
        1,
        "两个输入框 + 一行按钮,路径走行内截断",
    ),
];

fn ui_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/ui")
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).expect("读 src/ui 失败") {
        let p = e.expect("读目录项失败").path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// 只留生产代码 —— `#[cfg(test)]` 里也有一堆 `egui::Window::new(`
/// (`dismiss.rs` 的夹具就开了六个),算进来这条对账从第一天起就对不上。
fn prod(src: &str) -> &str {
    src.split("#[cfg(test)]").next().expect("源码切歪了")
}

/// 去掉行注释:本仓库注释里提 `egui::Window::new(` 的地方有十几处
/// (F239 那条约定每个弹窗文件都抄了一遍),不去掉就会把注释当成真弹窗。
fn strip_comment(line: &str) -> &str {
    match line.find("//") {
        Some(i) => &line[..i],
        None => line,
    }
}

/// F285 ③:名单必须罩住全库每一个 `egui::Window`。
///
/// 自证会变红:把 `LEDGER` 里 `ui/unlock.rs` 那条删掉(第一段红);
/// 或把 `ui/project_manager.rs` 的 `3` 改成 `2`(第二段红)。
#[test]
fn every_dialog_in_the_tree_is_either_guarded_or_exempt_with_a_reason() {
    let mut files = Vec::new();
    rs_files(&ui_dir(), &mut files);

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found: Vec<(String, usize)> = Vec::new();
    for p in &files {
        let src = std::fs::read_to_string(p).expect("读源码失败");
        let n = prod(&src)
            .lines()
            .map(strip_comment)
            .map(|l| l.matches("egui::Window::new(").count())
            .sum::<usize>();
        if n > 0 {
            let rel = p.strip_prefix(&root).expect("路径不在 src 下");
            found.push((rel.to_string_lossy().replace('\\', "/"), n));
        }
    }
    found.sort();

    for (path, n) in &found {
        let entry = LEDGER.iter().find(|(p, _, _)| p == path);
        let (_, want, why) = entry.unwrap_or_else(|| {
            panic!(
                "`{path}` 里有 {n} 个 egui::Window,但 tests/dialog_bounds.rs 的 LEDGER 没登记它。\
                 要么把它加进上面那两条真渲染守护,要么在 LEDGER 里写明**它为什么撑不开**。"
            )
        });
        assert_eq!(
            n, want,
            "`{path}` 里现在有 {n} 个 egui::Window,LEDGER 记的是 {want} —— \
             新加的那个谁守?(现有理由:{why})"
        );
        assert!(!why.trim().is_empty(), "`{path}` 的豁免理由是空的");
    }

    for (path, _, _) in LEDGER {
        assert!(
            found.iter().any(|(p, _)| p == path),
            "LEDGER 里登记了 `{path}`,但它已经没有 egui::Window 了 —— 删掉这条,\
             留着会让名单看起来比实际覆盖得宽"
        );
    }
}
