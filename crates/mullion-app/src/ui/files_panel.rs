//! 文件面板的 egui 渲染(F50)。远端栏与本地栏共用这一套 —— 差别只有
//! 数据来源与「哪些操作可用」,不是两份代码(设计 D1)。
//!
//! **大目录用 `ScrollArea::show_rows` 虚拟滚动**(设计 D21):一次
//! `readdir` 全量取回,但每帧只画可见那几十行。两万项的目录不做这一步
//! 会直接把帧时间打穿(陷阱 T3 的同类)。

use egui::Ui;

use crate::files::state::{Load, PaneState};
use crate::files::{human_size, perm_string, SortKey};
use crate::theme::{self, Theme};
use crate::ui::annotate;
use mullion_ssh::sftp::EntryKind;

/// 用户在这一栏里做了什么。app 侧据此发异步请求。
#[derive(Debug, Clone, PartialEq)]
pub enum FileAction {
    /// 进这个目录(双击目录 / 跟随链接 / 点书签 / 点路径面包屑)。
    Goto(mullion_ssh::sftp::RemotePath),
    /// F131:用户在路径条里敲完回车的**原文**。故意不在面板里解析 ——
    /// `~` 要用远端登录目录展开,而那个值挂在 `TabContent` 上,面板不知道
    /// 也不该知道(同 `Reconnect` 不带参数的理由)。
    GotoInput(String),
    /// 回上一级。
    Up,
    /// 刷新当前目录。
    Refresh,
    /// 切隐藏文件显示。
    ToggleHidden,
    /// D2:请求**打开一个对话框**。真正的写操作要等用户在对话框里确认之后,
    /// 由 `UiActions::files_op` 发出 —— 右键点一下就把远端文件删了这种事
    /// 不该存在。
    Ask(FileAsk),
    /// D5:本地栏专属 —— 用系统文件管理器打开当前目录。
    OpenInExplorer,
    /// F52:把这一栏选中的东西传到对面那一栏(远端栏 = 下载,本地栏 = 上传)。
    /// **方向由发起的栏决定**,不需要额外参数 —— 调用方本来就知道自己是哪栏。
    Transfer,
    /// F53:用系统默认程序编辑**光标行**那个远端文件。
    ///
    /// 不带路径:跟 `FileAsk::Rename`/`Chmod` 一样走「光标行」这条既有约定,
    /// app 侧本来就要按光标行解析目标。双击那条路径会先把光标挪到被双击的
    /// 行上再发这个动作,所以两个入口拿到的是同一个目标。
    EditExternal,
    /// F53:在内置编辑器里编辑光标行。
    EditInline,
    /// F58:**另一栏**的东西拖过来松手了,收进来。
    ///
    /// 方向由收到这条动作的栏决定,跟 `Transfer` 正好相反 —— `Transfer` 是
    /// 「把我这栏选中的送出去」,`Drop` 是「把对面栏选中的收进来」。源永远是
    /// 另一栏的选中集(载荷里只带栏,见 `drag::DragFrom`)。
    Drop(crate::files::drag::Landing),
    /// B3:这一栏所在的连接断了,用户按了「重连」。
    ///
    /// **不带参数**:重连的目标是「这个标签」,而标签是谁由 app 侧知道
    /// (面板本身不知道自己挂在哪个标签上)。两种宿主的语义不同,分派在
    /// app 侧做:SFTP 节点标签重建整条连接;终端标签的侧栏只重开
    /// sftp channel(SSH 本体断了是终端的事,侧栏不越权)。
    Reconnect,
}

/// 要打开哪个对话框。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAsk {
    /// 在当前目录里新建文件夹。**不需要选中任何东西**。
    NewDir,
    /// 重命名**光标行**(单目标)。
    Rename,
    /// 删除**选中集**(可多条)。
    Delete,
    /// 改**光标行**的权限(单目标)。
    Chmod,
}

/// 右键菜单里的一项。抽成枚举(而不是直接在渲染里写按钮)是为了让
/// 「哪些项该出现」能脱离 egui 单测 —— egui 的 `context_menu` 要一次
/// 右键 + 一帧才展开,在测试里驱动它又脆又慢,而「本地栏不许出现删除」
/// 恰恰是这一片最不能出错的一条。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    Ask(FileAsk),
    OpenInExplorer,
    Refresh,
    Transfer,
    EditExternal,
    EditInline,
}

/// 右键那一刻的光标行。**只有「是不是普通文件」和大小** —— 那一刻手上
/// 只有 `Entry`,没有内容,所以「是不是二进制」在这里判不了
/// (D3-3:二进制留到读回内容之后判)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuTarget {
    /// 目录 / 链接 / 设备文件都不是。编辑只对普通文件成立。
    pub is_file: bool,
    pub size: u64,
}

/// 菜单里的一项。带 `disabled` 是因为「这个文件太大所以编不了」必须**说出来**:
/// 悄悄少一项,用户只会以为程序坏了(D3-2)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuEntry {
    pub label: &'static str,
    pub item: MenuItem,
    /// 置灰的理由。`None` = 可点。
    pub disabled: Option<&'static str>,
}

fn on(label: &'static str, item: MenuItem) -> MenuEntry {
    MenuEntry {
        label,
        item,
        disabled: None,
    }
}

/// 这一栏此刻该有哪些右键菜单项。
///
/// - `column`:远端栏才有写操作(设计 D5:本地文件管理外包给资源管理器)。
/// - `target`:光标行。`None` 就不给单目标操作 ——
///   给一个点了没反应的菜单项比不给更让人困惑。
pub fn menu_items_for(column: PanelColumn, target: Option<MenuTarget>) -> Vec<MenuEntry> {
    let mut out: Vec<MenuEntry> = Vec::new();
    if column == PanelColumn::Remote {
        out.push(on("新建文件夹…", MenuItem::Ask(FileAsk::NewDir)));
        if let Some(tg) = target {
            out.push(on("下载到本地", MenuItem::Transfer));
            // F53:只对普通文件出现。目录/链接上给一个「编辑」纯属误导。
            if tg.is_file {
                out.push(MenuEntry {
                    label: "用默认程序编辑",
                    item: MenuItem::EditExternal,
                    disabled: (tg.size > crate::edit::EXTERNAL_LIMIT)
                        .then_some("文件太大,用「下载到本地」取回来再处理"),
                });
                out.push(MenuEntry {
                    label: "在 Mullion 里编辑",
                    item: MenuItem::EditInline,
                    disabled: (tg.size > crate::edit::INLINE_LIMIT)
                        .then_some("超过 1 MB,请用「用默认程序编辑」"),
                });
            }
            out.push(on("重命名…", MenuItem::Ask(FileAsk::Rename)));
            out.push(on("属性(权限)…", MenuItem::Ask(FileAsk::Chmod)));
            out.push(on("删除…", MenuItem::Ask(FileAsk::Delete)));
        }
    } else {
        if target.is_some() {
            out.push(on("上传到远端", MenuItem::Transfer));
        }
        out.push(on("在资源管理器中打开", MenuItem::OpenInExplorer));
    }
    out.push(on("刷新", MenuItem::Refresh));
    out
}

impl MenuItem {
    fn into_action(self) -> FileAction {
        match self {
            MenuItem::Ask(a) => FileAction::Ask(a),
            MenuItem::OpenInExplorer => FileAction::OpenInExplorer,
            MenuItem::Refresh => FileAction::Refresh,
            MenuItem::Transfer => FileAction::Transfer,
            MenuItem::EditExternal => FileAction::EditExternal,
            MenuItem::EditInline => FileAction::EditInline,
        }
    }
}

/// 画一份右键菜单的内容。背景和每一行各挂一份(见 `show` 里的说明),
/// 抽出来免得两处的菜单项悄悄长得不一样。
fn menu_body(
    ui: &mut Ui,
    id: &str,
    column: PanelColumn,
    target: Option<MenuTarget>,
    hit: &mut Option<MenuItem>,
) {
    annotate::mark(ui.ctx(), format!("文件面板/{id}/右键菜单"), ui.max_rect());
    for e in menu_items_for(column, target) {
        match e.disabled {
            // 置灰项仍然画出来,并且把理由挂成 hover —— 灰着不说话等于没说。
            Some(why) => {
                ui.add_enabled(false, egui::Button::new(e.label))
                    .on_disabled_hover_text(why);
            }
            None => {
                if ui.button(e.label).clicked() {
                    *hit = Some(e.item);
                    ui.close_menu();
                }
            }
        }
    }
}

/// 把一行折成右键菜单要看的那两个事实。
fn menu_target(e: &mullion_ssh::sftp::Entry) -> MenuTarget {
    MenuTarget {
        // `is_operable()` 而不是 `is_utf8()`:名字送不上线的行,任何
        // 单目标操作都做不了(D1 定的 UI 闸门)。
        is_file: e.kind == EntryKind::File && e.name.is_operable(),
        size: e.size,
    }
}

/// 列宽。名称列吃掉剩余宽度,其余定宽 —— 定宽列一旦跟着内容浮动,
/// 换个目录整张表就会横着抖。
const W_SIZE: f32 = 78.0;
const W_MTIME: f32 = 132.0;
const W_PERM: f32 = 86.0;
/// D2:属主列(`uid:gid`)。SFTP v3 的 attrs 里 uid/gid 就是数字,而
/// `russh-sftp 2.4.0` 的客户端 `DirEntry` 不暴露 `longname` —— 名字在协议层
/// 拿不到,不为此去 exec 一次 `id`(设计 D21)。
const W_OWNER: f32 = 92.0;
const ROW_H: f32 = 22.0;
/// D1:图标格子的边长 + 它和名称之间的空隙。
const W_ICON: f32 = 16.0;
const ICON_GAP: f32 = 4.0;
/// 图标格子的左内边距。**单一定义源**——`icon_rect()` 用它摆图标,
/// `name_start_x_offset()` 用它算名称起点该让到哪。复核挖出的坑:曾经
/// 这个 `4.0` 散落在 `row()` 里构造 `icon_rect`、`name_start_x_offset()`、
/// 以及测试里**独立重建**的 `icon_rect` 三处——测试拿自己重建的矩形去比,
/// 生产代码里的那一份改错了照样测不出来。
const ICON_LEFT_PAD: f32 = 4.0;

/// 图标格子(相对 `row_rect` 摆放)。`row()` 和测试都调这个,不各自重建
/// ——理由见 `ICON_LEFT_PAD` 的文档注释。
fn icon_rect(row_rect: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_size(
        egui::pos2(
            row_rect.left() + ICON_LEFT_PAD,
            row_rect.center().y - W_ICON * 0.5,
        ),
        egui::vec2(W_ICON, W_ICON),
    )
}

/// 名称文字的起点 x(相对 `rect.left()` 的偏移)。抽成纯函数只为了能脱离
/// egui 单测(见 `tests::name_start_clears_the_icon_cell`)——图标格子从
/// `rect.left() + ICON_LEFT_PAD` 起、宽 `W_ICON`,名称必须让在它右边,
/// 否则名字会压在图标上面。
fn name_start_x_offset() -> f32 {
    ICON_LEFT_PAD + W_ICON + ICON_GAP
}

/// 可选列(大小/修改时间/权限/属主),固定顺序 = 宽度不够时的收起顺序
/// 倒过来(排在后面的先被收起:先丢属主,再权限,再修改时间,最后大小)。
/// `visible_col_count()`/`header()`/`row()`/两条列布局守护测试都从这份表
/// 拿宽度与 `SortKey`,不许另起一份 —— 加一列/改一列宽只用改这里一处。
const OPTIONAL_COLS: [(&str, SortKey, f32); 4] = [
    ("大小", SortKey::Size, W_SIZE),
    ("修改时间", SortKey::Mtime, W_MTIME),
    ("权限", SortKey::Perm, W_PERM),
    ("属主", SortKey::Owner, W_OWNER),
];

/// 宽度不够时,`OPTIONAL_COLS` 从右向左收起,取最大的 k(0..=4)使得留给
/// 名称列的宽度仍 `>= 80`(名称列地板)。这是唯一的收起判据 ——
/// `name_w()`、列头、行体都调它决定「这个宽度下有几列可见」。
///
/// 复核实测出的现象(默认侧栏 360px 下):不做这个收起的话,列头按四列
/// 定宽从左往右累加,总宽恒为 488px;行体的权限/属主却是从
/// `rect.right()` 倒着锚的。可用宽度不够 488 时两套定位必然分家 ——
/// 属主列有数据没标题(标题起点 396 被裁没了),权限标题反而画到了权限
/// 数据右边。收起可选列让名称列吃掉全部剩余宽度,`total >= 100` 时两套
/// 定位代数上恒等,错位从根上消失(见 `name_w()` 的文档注释)。
fn visible_col_count(total: f32) -> usize {
    let mut sum = 0.0_f32;
    let mut k = 0usize;
    for &(_, _, w) in OPTIONAL_COLS.iter() {
        if total - W_ICON - ICON_GAP - (sum + w) < 80.0 {
            break;
        }
        sum += w;
        k += 1;
    }
    k
}

/// 名称列宽度。**一处算、多处用**(`header_name_col_w()`/`row_size_col_left()`
/// 都基于它)——按 `visible_col_count()` 给名称列让出剩余宽度,只在这一处
/// 算,别处不许各算一遍。
///
/// 只要 `total >= 100`,`total - W_ICON - ICON_GAP - (可见列宽度之和)` 恒
/// `>= 80`(`visible_col_count()` 的判据保证的),`.max(80.0)` 这个地板
/// 实际不会被触发 —— 留着只是给 `total < 100` 兜底。
///
/// **「到不了」只对 `sidebar()` 成立**:它有 `width_range(280.0..=640.0)`
/// 兜着,`total` 最小也是 280,进不了 `(0,100)` 这个区间。`content()`
/// (标签宿主)是整窗口对半分,代码里没有 `min_inner_size` 之类的窗口
/// 最小尺寸兜底——窗口宽 < 约 208px 时单栏 `total` 理论上会跌进
/// `(0,100)`,这时地板生效,`header_name_col_w(total)` 会算出**超过
/// 实际可用宽度**的值(比如 `total=90` 时算出 100)。这正是本文件这批
/// 收起逻辑要根除的那类「列头请求宽度越过可用区」,只是换了个更极端的
/// 触发路径——已知限制,未处理(该场景本身没人要求兜底,加保护是没人
/// 要的功能)。
fn name_w(total: f32) -> f32 {
    let k = visible_col_count(total);
    let sum: f32 = OPTIONAL_COLS[..k].iter().map(|&(_, _, w)| w).sum();
    (total - W_ICON - ICON_GAP - sum).max(80.0)
}

/// 列头「名称」列的显示宽度。列头没有单独的图标格子——它在视觉上对应的
/// 是行体「图标格子 + 间隙 + 名称」这个**合并区域**,所以要在 `name_w()`
/// 之上再加回 `W_ICON + ICON_GAP`。抽成函数(而不是在 `header()` 里内联)
/// 是为了让对齐守护测试能在不起 egui 上下文的情况下,调到与 `header()`
/// 完全相同的这一份计算 —— 写两份的话,`header()` 里那份改错了,测试的
/// 影子计算不会跟着错,照样测不出来。
fn header_name_col_w(total: f32) -> f32 {
    name_w(total) + W_ICON + ICON_GAP
}

/// 列头「大小/修改时间/权限/属主」里当前可见的那几列,各自的
/// `(标签, SortKey, 左边界, 宽度)`,左边界从 `header_name_col_w(total)`
/// 开始累加。`header()` 拿它逐列 `allocate_exact_size` 并挂排序点击,
/// 对齐守护测试拿它跟行体那份(`row_col_lefts()`)比 —— 两边各自独立
/// 累加,不共用最终结果,一边改错了另一边不会跟着错,能测出来。
fn header_col_lefts(total: f32) -> Vec<(&'static str, SortKey, f32, f32)> {
    let k = visible_col_count(total);
    let mut x = header_name_col_w(total);
    OPTIONAL_COLS[..k]
        .iter()
        .map(|&(label, key, w)| {
            let left = x;
            x += w;
            (label, key, left, w)
        })
        .collect()
}

/// 行体「大小」列的左边界(绝对坐标,基于 `row_rect.left()`)。**`row()`
/// 和对齐守护测试都调这个,不许测试自己重建这份算式**——同 `icon_rect()`
/// 的教训(见其文档注释):测试独立重建的话,这里改错了,测试拿的还是
/// 自己那份「正确」几何,照样测不出来。
fn row_size_col_left(row_rect: egui::Rect) -> f32 {
    row_rect.left() + W_ICON + ICON_GAP + name_w(row_rect.width())
}

/// 行体「大小/修改时间/权限/属主」里当前可见的那几列,各自的
/// `(标签, SortKey, 左边界, 宽度)`,左边界从 `row_size_col_left(row_rect)`
/// 开始累加。`row()` 拿它给每一列的文字定位(视觉上仍是原来的
/// RIGHT_CENTER / 右内缩 4px,只是坐标改从这里取,不再自己用
/// `rect.right()` 倒着算)。
fn row_col_lefts(row_rect: egui::Rect) -> Vec<(&'static str, SortKey, f32, f32)> {
    let k = visible_col_count(row_rect.width());
    let mut x = row_size_col_left(row_rect);
    OPTIONAL_COLS[..k]
        .iter()
        .map(|&(label, key, w)| {
            let left = x;
            x += w;
            (label, key, left, w)
        })
        .collect()
}

/// `ScrollArea` 持久化 id 的拼装,抽成纯函数只为了能脱离 egui 单测
/// (见 `tests::scroll_id_salt_differs_by_generation`)。
///
/// **`id` 单独不够**:`id` 只有 `"远端"`/`"本地"` 两个取值,跟哪个标签
/// 无关。`egui::Context` 整窗口只建一次、跨标签切换复用,`ScrollArea::
/// id_salt` 最终落到 `ui.id.with(salt)`(`egui-0.30.0` `scroll_area.rs`);
/// `sidebar()`/`content()` 各自的根 `ui.id`(`SidePanel::right("files")`/
/// `CentralPanel` 的 viewport id)对同一种宿主而言是恒定的,不随标签变——
/// 光靠 `id` 两个标签的同一栏会撞出同一个持久化 `Id`,A 标签滚过的偏移量
/// 直接被 B 标签继承。掺进 `generation`(S1 路由键,`App::next_ws_generation`
/// 单调递增、标签一多就必然互不相同)才能让每个标签自己的滚动位置独立。
fn scroll_id_salt(id: &str, generation: u64) -> String {
    format!("files-{generation}-{id}")
}

/// 画一栏。返回本帧的动作(至多一个 —— 一帧里用户点不了两下)。
///
/// `generation`:调用方(`sidebar`/`content`)所属那个标签的世代号,只用来
/// 拼 `ScrollArea` 的持久化 id(见 `scroll_id_salt`),不参与任何业务判断。
///
/// `column`:这是远端栏还是本地栏。**只用来决定右键菜单里有哪些项**
/// (D5:写操作只在远端栏)。不复用 `id` 那个字符串做判据 —— 拿显示用的
/// 中文标签当权限开关,哪天有人把 `"远端"` 改成 `"服务器"`,本地栏就会
/// 长出一个删除远端文件的菜单。
///
/// `focused`:F6/Tab 换焦点(设计 D23,代码复核挖出的可达性缺口)——这一栏
/// 此刻是不是键盘真正落点的那一栏。`true` 才画边框,调用方(`sidebar`/
/// `content`)已经把「面板本身有没有键盘焦点」与「`active_column` 是不是
/// 这一栏」两个条件都算进去了,这里不用再判。
///
/// `drop_in`:F52 —— 此刻从资源管理器往窗口里拖着几个文件。调用方只给
/// **远端栏**传非零值(本地栏恒 `0`):拖进来的东西一律上传,本地栏收下
/// 只会是「把本地文件复制到本地」,那是资源管理器自己的事(D5)。
#[allow(clippy::too_many_arguments)] // 跟 session_manager 那批 egui 渲染函数同款,一帧要画的东西天然多
pub fn show(
    ui: &mut Ui,
    t: &Theme,
    id: &str,
    generation: u64,
    column: PanelColumn,
    state: &mut PaneState,
    focused: bool,
    bookmarks: &[mullion_store::Bookmark],
    drop_in: usize,
) -> Option<FileAction> {
    let mut action = None;
    annotate::mark(ui.ctx(), format!("文件面板/{id}"), ui.max_rect());
    // 焦点在场才画——常亮等于没有信息量(协调者复核 #2)。颜色取既有语义色
    // `t.accent`(选中态同款),不新造色值(UI 视觉规格已冻结,见 spec §4.6)。
    if focused {
        ui.painter().rect_stroke(
            ui.max_rect(),
            egui::Rounding::same(4.0),
            egui::Stroke::new(2.0, theme::c32(t.accent)),
        );
    }

    // 整栏背景的右键菜单:空白处右键也要能「新建文件夹」/「刷新」——
    // 那是用户在一个空目录里唯一的入口。
    //
    // **必须注册在所有内容之前**:egui 同层内后注册的部件压在先注册的上面,
    // 挂到栏尾会把整栏罩住,书签按钮和行的左键点击全被它吃掉
    // (`clicking_a_bookmark_dispatches_goto_to_its_path` 逮到过这一版)。
    let mut menu_hit = None;
    let bg_target = state
        .cursor
        .as_ref()
        .and_then(|c| state.entries.iter().find(|e| e.name == *c))
        .map(menu_target);
    let bg = ui.interact(
        ui.max_rect(),
        ui.id().with(("files-bg-menu", id, generation)),
        egui::Sense::click(),
    );
    bg.context_menu(|ui| menu_body(ui, id, column, bg_target, &mut menu_hit));
    // F58:对面栏正拖着东西过来 —— 整栏描边,让「松手会传到这儿」在松手
    // **之前**就看得见。判据是「载荷来自另一栏」而不是「有载荷」:同栏内
    // 拖不成立(`drag::drop_target`),给它描边等于承诺一个不会发生的动作。
    let incoming = egui::DragAndDrop::payload::<crate::files::drag::DragFrom>(ui.ctx())
        .filter(|f| f.0 != column)
        .is_some();
    if incoming && bg.contains_pointer() {
        ui.painter().rect_stroke(
            ui.max_rect(),
            egui::Rounding::same(4.0),
            egui::Stroke::new(2.0, theme::c32(t.accent)),
        );
    }
    // F52:资源管理器里正拖着文件悬在窗口上。整栏描边 + **明写落点**。
    //
    // 落点不看指针在哪:winit 0.30 的 `HoveredFile`/`DroppedFile` 不带坐标,
    // Windows 在 OLE 拖放期间也不发 `CursorMoved`,「指针压在哪一栏/哪一行」
    // 这一帧根本判不出来。于是规则定死为「扔在窗口哪儿都上传到远端当前
    // 目录」。规则反直觉,就必须在用户**松手之前**写在屏幕上(设计 D9:
    // 规则先于动作可见),而不是松手之后用一条 toast 告诉他传到别处去了。
    if drop_in > 0 {
        ui.painter().rect_stroke(
            ui.max_rect(),
            egui::Rounding::same(4.0),
            egui::Stroke::new(2.0, theme::c32(t.accent)),
        );
        ui.colored_label(
            theme::c32(t.accent),
            crate::files::drag::drop_in_hint(&state.cwd, drop_in),
        );
    }
    // 就地收口,不留到函数末尾 —— 下面 `Load` 不是 `Ready` 时会提前 return,
    // 挂在末尾的话「正在读取目录…」时右键点刷新没反应。
    if let Some(item) = menu_hit.take() {
        action = Some(item.into_action());
    }

    // 路径条 + 上级 + 刷新。
    ui.horizontal(|ui| {
        if ui
            .small_button("↑")
            .on_hover_text("上一级(Backspace)")
            .clicked()
        {
            action = Some(FileAction::Up);
        }
        if ui.small_button("⟳").on_hover_text("刷新(F5)").clicked() {
            action = Some(FileAction::Refresh);
        }
        let path = state.cwd.display().to_string();
        annotate::mark(ui.ctx(), format!("文件面板/{id}/路径"), ui.max_rect());
        ui.add(egui::Label::new(egui::RichText::new(path).color(theme::c32(t.fg_mid))).truncate());
    });

    // F120:书签栏。调用方(`sidebar`/`content`)只给远端栏传非空切片 ——
    // 本地栏永远收到 `&[]`,这里不按 `id` 分支,谁传谁画。放在路径条之后、
    // `Load` 匹配之前:哪怕目录还没读完(`Idle`/`Loading`/`Failed`)书签也该
    // 一直可点,不该等目录列出来才出现。
    if !bookmarks.is_empty() {
        ui.horizontal_wrapped(|ui| {
            annotate::mark(ui.ctx(), format!("文件面板/{id}/书签栏"), ui.max_rect());
            for b in bookmarks {
                // 空名字是 store 明确允许的合法状态(`Bookmark::name` 文档),
                // 界面回退显示路径本身,不能画一个没有任何文字的按钮。
                let label = if b.name.is_empty() {
                    b.path.as_str()
                } else {
                    b.name.as_str()
                };
                if ui.small_button(label).on_hover_text(&b.path).clicked() {
                    action = Some(FileAction::Goto(mullion_ssh::sftp::RemotePath::from_bytes(
                        b.path.as_bytes().to_vec(),
                    )));
                }
            }
        });
    }

    match &state.load {
        Load::Idle => {
            ui.colored_label(theme::c32(t.fg_dimmer), "未连接");
            return action;
        }
        Load::Loading => {
            ui.colored_label(theme::c32(t.fg_dim), "正在读取目录…");
            return action;
        }
        Load::Failed(msg) => {
            ui.colored_label(theme::c32(t.danger), msg.clone());
            return action;
        }
        Load::Disconnected => {
            ui.colored_label(theme::c32(t.danger), "连接已断开");
            if ui.button("重连").clicked() {
                action = Some(FileAction::Reconnect);
            }
            return action;
        }
        Load::Ready => {}
    }

    header(ui, t, id, state);

    let rows = state.rows();
    // `rows` 借着 `&state.entries` 不放(它是 `Vec<&Entry>`),闭包里不能再
    // 借一次 `&mut state`——新选中的那条先记局部变量,出了闭包再落回 `state`。
    let selected = state.selected.clone();
    /// 点中的那一条 + 当时按着的修饰键(ctrl, shift)。F54 的多选语义在
    /// `PaneState::click_row` 里,这里只负责把「点了什么、按着什么」带出闭包。
    type Click = (mullion_ssh::sftp::RemotePath, bool, bool);
    let mut clicked: Option<Click> = None;
    let mut goto = None;
    let mut edit = false;
    // F58:起拖的那一条如果还没选中,要先让它成为唯一选中项(同右键那条约定)。
    // 借用规则同 `clicked` —— 闭包里改不了 `state`,出了闭包再落。
    let mut drag_start: Option<mullion_ssh::sftp::RemotePath> = None;
    let mut landing: Option<crate::files::drag::Landing> = None;
    egui::ScrollArea::vertical()
        .id_salt(scroll_id_salt(id, generation))
        // F58:**必须关掉**。`drag_to_scroll` 默认开着,它在视口上注册一个
        // 吃 drag 的部件,把按在行上的那一下抢去当滚动手势 —— 行的
        // `drag_started()` 永远为假,拖拽整个功能安静地不存在。桌面端本来
        // 也没人用鼠标拖内容滚动(滚轮 + 滚动条都在),关掉不损失什么。
        .drag_to_scroll(false)
        .auto_shrink([false, false])
        .show_rows(ui, ROW_H, rows.len(), |ui, range| {
            for ix in range {
                let e = rows[ix];
                let resp = row(ui, t, e, column, selected.contains(&e.name));
                // F58:行既是拖源也是落点。
                if resp.drag_started() {
                    // 拖一条**没选中**的行:先让它成为唯一选中项。不这么做的话
                    // 用户拖的是这一条、传走的却是别处那批选中项 —— 与右键菜单
                    // 那条已知陷阱同源(见上面 `secondary_clicked` 的说明)。
                    if !selected.contains(&e.name) {
                        drag_start = Some(e.name.clone());
                    }
                    resp.dnd_set_drag_payload(crate::files::drag::DragFrom(column));
                }
                if incoming && resp.contains_pointer() && e.kind == EntryKind::Dir {
                    // 悬停在目录行上:高亮这一行,预告「松手会传进这个目录」。
                    ui.painter().rect_stroke(
                        resp.rect,
                        egui::Rounding::same(2.0),
                        egui::Stroke::new(1.0, theme::c32(t.accent)),
                    );
                }
                if let Some(from) = resp.dnd_release_payload::<crate::files::drag::DragFrom>() {
                    // 目录行 → 传进那个子目录;文件行 → 落到当前目录(不解释成
                    // 「覆盖那个文件」,理由见 `drag::drop_target`)。名字送不上线
                    // 的目录当没这一行 —— 拼出来的路径请求发不出去。
                    let over = (e.kind == EntryKind::Dir && e.name.is_operable())
                        .then(|| e.name.as_bytes().to_vec());
                    landing = crate::files::drag::drop_target(from.0, column, over);
                }
                if resp.clicked() {
                    // `command` 而不是 `ctrl`:egui 已经把 macOS 的 ⌘ 归一化
                    // 到这一位上,写 `ctrl` 会让 macOS 用户点不出多选。
                    let m = ui.input(|i| i.modifiers);
                    clicked = Some((e.name.clone(), m.command, m.shift));
                }
                if resp.secondary_clicked() && !selected.contains(&e.name) {
                    // 右键点到一条**没选中**的行:先让它成为唯一选中项。
                    // 不这么做的话,菜单里的「删除…」删的是别处那批选中项,
                    // 而用户以为删的是他右键的这一条 —— 而删除不可逆。
                    clicked = Some((e.name.clone(), false, false));
                }
                // 行自己也挂一份菜单:行是后注册的,压在背景那份上面,
                // 右键落在行上时背景那份收不到(见函数开头的 z 序说明)。
                // 目标直接取被右键的这一行 —— 不走 `state.cursor`,那个要等
                // 出了闭包才更新,这一帧里还是上一条。
                let tg = menu_target(e);
                resp.context_menu(|ui| menu_body(ui, id, column, Some(tg), &mut menu_hit));
                if resp.double_clicked() {
                    match state.enter_target(e) {
                        Some(target) => goto = Some(target),
                        // F53:双击**文件**不该是「什么都不发生」。远端栏交给
                        // 默认程序编辑;本地栏什么也不做(D5:本地文件管理外包
                        // 给资源管理器,双击本地文件应由用户在资源管理器里做)。
                        None if column == PanelColumn::Remote && tg.is_file => {
                            // 先把光标挪到被双击的这一行 —— 动作本身不带路径,
                            // app 侧按光标行解析目标。
                            clicked = Some((e.name.clone(), false, false));
                            edit = true;
                        }
                        None => {}
                    }
                }
            }
        });
    // F58:落在空白处(行与行之间、列头下方的空白)。**必须排在行之后** ——
    // `dnd_release_payload` 会把载荷取走,背景先问的话落在目录行上的那一下
    // 会被背景吃掉,「传进子目录」永远走不到。
    if landing.is_none() {
        if let Some(from) = bg.dnd_release_payload::<crate::files::drag::DragFrom>() {
            landing = crate::files::drag::drop_target(from.0, column, None);
        }
    }
    if let Some(name) = drag_start {
        state.select_only(&name);
    }
    if let Some((name, ctrl, shift)) = clicked {
        state.click_row(&name, ctrl, shift);
    }
    if let Some(l) = landing {
        action = Some(FileAction::Drop(l));
    }
    if let Some(g) = goto {
        action = Some(FileAction::Goto(g));
    }
    if edit {
        action = Some(FileAction::EditExternal);
    }
    if let Some(item) = menu_hit {
        action = Some(item.into_action());
    }
    action
}

fn header(ui: &mut Ui, t: &Theme, id: &str, state: &mut PaneState) {
    ui.horizontal(|ui| {
        annotate::mark(ui.ctx(), format!("文件面板/{id}/列头"), ui.max_rect());
        // `ui.horizontal`默认在相邻部件间插入 `item_spacing.x`(8pt)——四列之间
        // 就是 3 处空隙,共 24pt。下面 `name_w` 只按四列自身宽度求和 == 可用宽度,
        // 没有扣掉这 24pt,`ui.horizontal` 请求的总宽度因此比 `available_width()`
        // 多 24pt,会把外层 ui 的 `max_rect` 撑宽——两栏对调(F50 A 组)后这多出
        // 的 24pt 越过两栏间 8pt 的缝隙侵入邻栏,`dragging_from_the_local_column_
        // onto_a_remote_directory_row_drops_into_that_directory` 等守护测试因此
        // 变红(邻栏的行抢先拿走了 dnd 载荷)。清零间距而不是从 `name_w` 里扣,
        // 这样四列衔接处跟 `row()` 里手工画的四列(本来就是零间距的绝对坐标)
        // 对得齐,不会因为「扣了间距」反而让表头跟表身错位。
        ui.spacing_mut().item_spacing.x = 0.0;
        let mut hit = None;
        let total = ui.available_width();
        // 表头这一列没有单独的图标格子——它跟 `row()` 里「图标格子 + 名称」
        // 那个**合并宽度**对齐,所以用 `header_name_col_w()` 在 `name_w()`
        // 之上加回 `W_ICON + ICON_GAP`(该函数与 `name_w()` 同款「一处算、
        // 两处用」——夹紧地板已经在 `name_w()` 里做过一次,这里不用再抬
        // 一次地板,加法上代数等价,也省得两处维护同一个地板常量)。
        //
        // 大小/修改时间/权限/属主这四列不再是字面量数组——`total` 不够时
        // 从右向左收起(见 `visible_col_count()`),`header_col_lefts()` 只
        // 给回当前可见的那几列,收起的列这里根本不出现在循环里。
        let mut cols = vec![("名称", SortKey::Name, header_name_col_w(total))];
        cols.extend(
            header_col_lefts(total)
                .into_iter()
                .map(|(label, key, _left, w)| (label, key, w)),
        );
        for (label, key, w) in cols {
            let mark = if state.sort_key == key {
                match state.sort_dir {
                    crate::files::SortDir::Asc => " ▲",
                    crate::files::SortDir::Desc => " ▼",
                }
            } else {
                ""
            };
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), egui::Sense::click());
            // 逐列登记:上面那处只标了整行,四列各自的可点矩形并不等于整行 ——
            // 测试要往「名称」列具体点,不能拿整行中心去凑(改宽了就打偏)。
            annotate::mark(ui.ctx(), format!("文件面板/{id}/列头/{label}"), rect);
            ui.painter().text(
                rect.left_center() + egui::vec2(crate::ui::metrics::SP_XS, 0.0),
                egui::Align2::LEFT_CENTER,
                format!("{label}{mark}"),
                egui::FontId::proportional(11.0),
                theme::c32(t.fg_muted),
            );
            if resp.clicked() {
                hit = Some(key);
            }
        }
        if let Some(k) = hit {
            state.click_header(k);
        }
    });
}

fn row(
    ui: &mut Ui,
    t: &Theme,
    e: &mullion_ssh::sftp::Entry,
    column: PanelColumn,
    selected: bool,
) -> egui::Response {
    // `click_and_drag` 而不是 `click`(F58):行要能起拖。`clicked()` /
    // `double_clicked()` 在这个 Sense 下照旧 —— egui 只有在指针真的移出
    // 拖拽阈值之后才把这一下判成拖,原地按松仍然是点击。
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ROW_H),
        egui::Sense::click_and_drag(),
    );
    if selected {
        ui.painter().rect_filled(rect, 2.0, theme::c32(t.sunken_bg));
    }
    let p = ui.painter();
    let font = egui::FontId::proportional(12.0);
    // 非可操作名字画成 dim + 后缀说明:用户要能一眼看出「这个动不了」
    // 而不是点下去才发现(D16 修订)。判据是 `is_operable`,不是 `is_utf8`——
    // 后者对「线上 lossy 过的 `U+FFFD` 串」恒为真,拿它当判据会让这条
    // 说明永远画不出来(见 `mullion_ssh::sftp::RemotePath` 文档)。
    let usable = e.name.is_operable();
    let fg = if !usable {
        theme::c32(t.fg_dimmer)
    } else if e.kind == EntryKind::Dir {
        theme::c32(t.fg_strong)
    } else {
        theme::c32(t.fg)
    };
    // D1/F127:类型图标。判类看 `EntryKind` + 扩展名 + x 位,颜色跟
    // 可操作性同源(不可操作 → 与文字一样是 dim),不另算一套 —— 否则会
    // 出现「文字灰了图标还亮着」这种自相矛盾的行。排在名称文字之前画,
    // 视觉上图标在名字左边。
    let icon_kind = crate::ui::file_icon::classify(e.kind, e.name.display().as_ref(), e.mode);
    crate::ui::file_icon::paint(
        p,
        icon_rect(rect),
        icon_kind,
        theme::c32(crate::ui::file_icon::color_for(icon_kind, usable, t)),
    );

    let mut label = e.name.display().to_string();
    if let (EntryKind::Symlink, Some(tgt)) = (e.kind, &e.link_target) {
        label = format!("{label} → {}", tgt.display());
    }
    if !usable {
        label = format!("{label}(名称非 UTF-8,本版无法操作)");
    }
    p.text(
        rect.left_center() + egui::vec2(name_start_x_offset(), 0.0),
        egui::Align2::LEFT_CENTER,
        label,
        font.clone(),
        fg,
    );
    // 名称列的可用宽度让出图标格子 + 间隙,否则长文件名会顶到大小列上——
    // `row_size_col_left()` 是「大小」列左边界,生产代码与对齐守护测试
    // 共用同一份(见其文档注释)。
    //
    // 下面四列(大小/修改时间/权限/属主)宽度不够时会被从右向左收起——
    // `row_col_lefts()` 只给回当前可见的那几列,顺序固定,缺的列在这个
    // `Vec` 里直接不存在,对应的 `if let` 分支就不画。视觉上每一列仍是
    // 原来的 RIGHT_CENTER / 右内缩 4px:最后一个可见的可选列(不管是
    // 属主、权限还是别的)恒贴着 `rect.right()`——`name_w()` 保证了这点
    // (见其文档注释)。
    let cols = row_col_lefts(rect);
    if let Some(&(_, _, size_left, size_w)) = cols.first() {
        let size_text = if e.kind == EntryKind::Dir {
            String::new()
        } else {
            human_size(e.size)
        };
        p.text(
            egui::pos2(size_left + size_w, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            size_text,
            font.clone(),
            theme::c32(t.fg_mid),
        );
    }
    if let Some(&(_, _, mtime_left, _)) = cols.get(1) {
        p.text(
            egui::pos2(mtime_left + crate::ui::metrics::SP_S, rect.center().y),
            egui::Align2::LEFT_CENTER,
            mtime_text(e.mtime),
            font.clone(),
            theme::c32(t.fg_mid),
        );
    }
    // 权限列(不再把 uid:gid 拼在后面 —— 属主拆成了独立列)。
    if let Some(&(_, _, perm_left, perm_w)) = cols.get(2) {
        p.text(
            egui::pos2(
                perm_left + perm_w - crate::ui::metrics::SP_XS,
                rect.center().y,
            ),
            egui::Align2::RIGHT_CENTER,
            perm_string(e.mode),
            font.clone(),
            theme::c32(t.fg_dim),
        );
    }
    // D2:属主列。**本地栏恒画 `—`**,判据见 `owner_text` 的文档注释。
    if let Some(&(_, _, owner_left, owner_w)) = cols.get(3) {
        p.text(
            egui::pos2(
                owner_left + owner_w - crate::ui::metrics::SP_XS,
                rect.center().y,
            ),
            egui::Align2::RIGHT_CENTER,
            owner_text(column, e.uid, e.gid),
            font,
            theme::c32(t.fg_dim),
        );
    }
    resp
}

/// 属主列文案。**本地栏恒画 `—`**,判据是 `column == PanelColumn::Local`
/// 而不是 `uid == 0` —— 远端真的有 root 拥有的文件,拿 0 当「没有属主
/// 信息」的哨兵会让那些文件的属主列也变成 `—`。
fn owner_text(column: PanelColumn, uid: u32, gid: u32) -> String {
    if column == PanelColumn::Local {
        "—".to_string()
    } else {
        format!("{uid}:{gid}")
    }
}

/// 两栏之一。定义搬去了 `crate::files`(纯逻辑层,`drag.rs` 的落点判据要
/// 拿它当参数),这里重导出让老的引用路径继续可用。
pub use crate::files::PanelColumn;

/// 一帧要画的两栏 + 列选项(F50)。
pub struct PanelFrame {
    pub remote: PaneState,
    pub local: PaneState,
    /// F120:该会话配置的远端书签,原样转给远端栏画书签条。纯数据,空
    /// `Vec` 是安全默认——符合 `Default` impl 文档下面那条「加字段前先想
    /// 清楚」的约束。
    pub bookmarks: Vec<mullion_store::Bookmark>,
    /// F6/Tab 换焦点(设计 D23):面板内键盘动作(进目录/上级/刷新/切隐藏/
    /// 移动选中)作用在哪一栏。纯 UI 状态,安全默认值(`Remote`),同样符合
    /// 上面那条「加字段前先想清楚」的约束。
    pub active_column: PanelColumn,
}

impl Default for PanelFrame {
    /// **是每个终端标签的初始状态,也是 `app.rs` 里临时"挪出/放回"这个
    /// 结构体时 `std::mem::take` 要求的占位值**(见 `App::window_event` 里
    /// `Present` 分支的说明:侧栏渲染要 `&mut PanelFrame`,而同一帧里
    /// `UiFrame::automation` 借的是同一个标签的另一个字段,`self.tabs` 借不出
    /// 一份可变一份不可变,只能把 `PanelFrame` 整体挪成不再借用 `self.tabs`
    /// 的本地值)。两个用途凑巧用的是同一份初值,不是巧合之外的耦合——占位
    /// 窗口极短,期间没有任何代码会读到它。
    ///
    /// **给这个结构体加字段前先想清楚**:同一份初值要同时当「新标签的起始
    /// 状态」和「借用过桥期间的临时占位」。今天两栏都是纯数据,怎么填都无害;
    /// 哪天加进来一个「没有安全默认值」的字段(sftp handle、后台任务句柄),
    /// 这里就会被迫编一个假的出来,而编译器对这种双重语境一声不吭 ——
    /// 那种字段该挂到 `TerminalTab` 上,不该进 `PanelFrame`。
    fn default() -> Self {
        Self {
            remote: PaneState::new(mullion_ssh::sftp::RemotePath::from_bytes(b"/".to_vec())),
            local: PaneState::new(crate::files::local::default_local(None)),
            bookmarks: Vec::new(),
            active_column: PanelColumn::default(),
        }
    }
}

impl PanelFrame {
    /// 用会话配置的默认本地目录 + 书签开一个新标签。`default_local` 直接转给
    /// `files::local::default_local`(`None` = 用户主目录);远端目录不在这里
    /// 决定 —— 连接尚未建立时根本没有远端可拿,拿真实登录目录/配置值是
    /// `app.rs::spawn_sftp_open` 建立连接后才做的事。
    pub fn new(default_local: Option<&str>, bookmarks: Vec<mullion_store::Bookmark>) -> Self {
        Self {
            local: PaneState::new(crate::files::local::default_local(default_local)),
            bookmarks,
            ..Self::default()
        }
    }

    /// 键盘焦点当前落在哪一栏,连同它的状态一起给(设计 D23:F6/Tab 换焦点)。
    pub fn active_state(&self) -> (PanelColumn, &PaneState) {
        match self.active_column {
            PanelColumn::Remote => (PanelColumn::Remote, &self.remote),
            PanelColumn::Local => (PanelColumn::Local, &self.local),
        }
    }

    /// 同 [`Self::active_state`],可变版本 —— `↑`/`↓` 移动选中项要直接改
    /// `PaneState::selected`,不经过 `apply_*_file_action` 那条异步链路
    /// (选中项是纯 UI 状态,不触发网络请求)。
    pub fn active_state_mut(&mut self) -> &mut PaneState {
        match self.active_column {
            PanelColumn::Remote => &mut self.remote,
            PanelColumn::Local => &mut self.local,
        }
    }
}

/// 默认侧栏宽度(point)。见 `UiState::files_sidebar_w` 的文档注释 ——
/// 该字段用 `0.0` 当「还没拖过」的哨兵,真正的默认值代入在这里。
const DEFAULT_SIDEBAR_W: f32 = 360.0;

/// 侧栏宿主(设计 D1 的宿主之一)。**上下堆叠**:侧栏典型宽 320~450px,
/// 左右并排后每栏只剩 160~220px,四列排不下;而把侧栏加宽到 560px 会
/// 压扁终端列数,让远端 TUI 重排得很难看(设计 D4)。
///
/// **这是 T4 的真实注入点**:`SidePanel` 参与 egui 的 Panel 空间分配,
/// `ui/mod.rs::build_ui` 末尾 `ctx.available_rect()` 拿到的中央区因此变窄,
/// 下一帧 `App::compute_geoms` 用它重算列数,`apply_geometry` 发一次
/// `window_change`(T4)。**若把这里换成 `egui::Area`**——Area 不参与 Panel
/// 的空间分配,中央区不会变窄,这条链会在源头断掉(`ui::mod::tests::
/// opening_the_files_sidebar_shrinks_the_central_area` 守着这一点)。
///
/// `generation`:侧栏属主标签的世代号,原样转给 `show()` 拼 `ScrollArea`
/// 持久化 id——同一个根 `ui.id`(`SidePanel::right("files")` 恒定)跨标签
/// 复用,不掺这个号,标签 A 侧栏滚过的偏移量会被标签 B 的侧栏继承
/// (代码复核挖出的真 bug,见 `scroll_id_salt` 的文档)。
///
/// `panel_focused`:F6(设计 D23)——键盘焦点此刻是不是在这个面板上(不区分
/// 远端/本地,那是 `frame.active_column` 的事)。传下去给 `show()` 各自跟
/// `active_column` 相与,决定画不画焦点边框。
///
/// `drop_in`:F52 —— 此刻从资源管理器拖着几个文件悬在窗口上,只转给远端栏
/// (理由见 `show` 的同名参数)。
pub fn sidebar(
    ctx: &egui::Context,
    t: &Theme,
    ui_state: &mut crate::ui::UiState,
    generation: u64,
    panel_focused: bool,
    frame: &mut PanelFrame,
    drop_in: usize,
) -> (Option<FileAction>, Option<FileAction>) {
    let mut out = (None, None);
    let default_w = if ui_state.files_sidebar_w > 0.0 {
        ui_state.files_sidebar_w
    } else {
        DEFAULT_SIDEBAR_W
    };
    let resp = egui::SidePanel::right("files")
        .resizable(true)
        .default_width(default_w)
        .width_range(280.0..=640.0)
        .frame(
            egui::Frame::none()
                .fill(theme::c32(t.panel_bg))
                .stroke(theme::stroke(t)),
        )
        .show(ctx, |ui| {
            annotate::mark(ui.ctx(), "文件侧栏", ui.max_rect());
            let h = ui.available_height();
            // A3:比例**跟着远端走,不跟着位置走** —— 侧栏是「终端为主 +
            // 文件为辅」的场景,辅助视图里远端才是主体。所以上面(本地)0.4、
            // 下面(远端)0.6。
            ui.allocate_ui(egui::vec2(ui.available_width(), h * 0.4), |ui| {
                out.1 = show(
                    ui,
                    t,
                    "本地",
                    generation,
                    PanelColumn::Local,
                    &mut frame.local,
                    panel_focused && frame.active_column == PanelColumn::Local,
                    &[],
                    0,
                );
            });
            ui.separator();
            out.0 = show(
                ui,
                t,
                "远端",
                generation,
                PanelColumn::Remote,
                &mut frame.remote,
                panel_focused && frame.active_column == PanelColumn::Remote,
                &frame.bookmarks,
                drop_in,
            );
        });
    // 把这一帧的实际宽度读回来。**注意它只是个镜像,驱动不了任何东西**:
    // egui 0.30 的 `SidePanel::show_inside_dyn` 先取 `default_width`,紧接着
    // 只要 `PanelState::load` 在 memory 里找得到这个 id 的存档就整个覆盖掉
    // (`egui-0.30.0/src/containers/panel.rs`)。也就是说除了**第一帧**,上面那个
    // `default_w` 都不生效,宽度由 egui 自己的持久化状态说了算。
    // 以后要做「宽度按会话记住」,光把值存进这个字段不够 —— 得连带清掉
    // egui memory 里对应的 `PanelState`,否则改了也看不见。
    ui_state.files_sidebar_w = resp.response.rect.width();
    out
}

/// 标签宿主(设计 D1/D4 的另一个宿主)。**左右并排**,占满整个内容区 ——
/// 这种标签没有终端要跟它抢宽度,四列都排得下(跟 `sidebar` 的上下堆叠
/// 正好相反,D4 的取舍点就在这:侧栏窄、标签宿主宽)。
///
/// 用 `egui::CentralPanel`——按 egui 的 Panel 空间分配规则,它会贪婪吃掉
/// 调用时刻还剩的全部区域,所以 `ui/mod.rs::build_ui` 里必须把它排在本帧
/// **最后一个** panel 类部件之后调用,否则排在它之后 show 的
/// `TopBottomPanel`/`SidePanel` 会被顶掉。
///
/// 这种标签没有 `Workspace`——`App::compute_geoms` 对它恒短路为空
/// (`active_ws()` 返回 `None`),T4(reflow 发 `window_change`)在这条路径上
/// 无从谈起:没有 PTY,没有列数这个概念,`central_px` 缩到多小都不会喂进
/// GPU 那条画终端的路径(它只在 `panes` 非空时才跑,见 `render_frame`)。
///
/// `generation`:标签宿主自身的世代号,原样转给 `show()` 拼 `ScrollArea`
/// 持久化 id——`CentralPanel` 的根 `ui.id` 恒为 `(viewport_id, "central_panel")`
/// 跨标签复用,不掺这个号,标签 A 滚过的偏移量会被标签 B 继承
/// (代码复核挖出的真 bug,见 `scroll_id_salt` 的文档)。
///
/// `panel_focused`:同 `sidebar` 的同名参数。标签宿主按 `effective_focus`
/// 的第三条规则理论上恒为 `true`(活动标签是 Files → 焦点恒在面板上),但
/// 判据只留在 `effective_focus_of` 一处——这里原样转发调用方算好的值,不在
/// `files_panel.rs` 里重新假设一遍。
///
/// `drop_in`:同 `sidebar` 的同名参数。
pub fn content(
    ctx: &egui::Context,
    t: &Theme,
    generation: u64,
    panel_focused: bool,
    frame: &mut PanelFrame,
    drop_in: usize,
) -> (Option<FileAction>, Option<FileAction>) {
    let mut out = (None, None);
    egui::CentralPanel::default()
        .frame(
            egui::Frame::none()
                .fill(theme::c32(t.panel_bg))
                .stroke(theme::stroke(t)),
        )
        .show(ctx, |ui| {
            annotate::mark(ui.ctx(), "文件标签", ui.max_rect());
            // **两栏的矩形自己切,不走 `ui.horizontal` + `allocate_ui`**:
            // 在 `horizontal` 布局里 `ui.available_height()` 给的是**当前这一行**
            // 的高度(这里是 18pt),不是整块内容区。照它分配的话两栏各只有一行
            // 高,`ScrollArea` 视口被压扁、行的交互 rect 宽度塌成 0 ——
            // 画面上文字照旧(`painter` 直接按坐标画),但整个标签宿主里
            // **一行都点不中**。这是 D4a 做拖拽时才暴露出来的既存 bug。
            let full = ui.max_rect();
            let gap = crate::ui::metrics::SP_S;
            let half = ((full.width() - gap) * 0.5).max(0.0);
            let left = egui::Rect::from_min_size(full.min, egui::vec2(half, full.height()));
            let right =
                egui::Rect::from_min_max(egui::pos2(full.max.x - half, full.min.y), full.max);
            ui.scope_builder(egui::UiBuilder::new().max_rect(left), |ui| {
                // B1:**必须显式裁剪**。`max_rect` 只是布局预算,子 ui 的
                // `clip_rect` 默认原样继承父 painter(`egui-0.30.0`
                // `ui.rs::new_child` 直接 `clone()` 父 `painter`,不看
                // `max_rect`)——`CentralPanel` 的裁剪范围横跨整个内容区、
                // 两栏共用,不裁的话本栏画出界的东西(滚动条、超宽内容)
                // 会一路画进右栏(实测证实,见
                // `tests::the_two_columns_get_independent_non_overlapping_scroll_areas`)。
                ui.set_clip_rect(left);
                out.1 = show(
                    ui,
                    t,
                    "本地",
                    generation,
                    PanelColumn::Local,
                    &mut frame.local,
                    panel_focused && frame.active_column == PanelColumn::Local,
                    &[],
                    0,
                );
            });
            ui.painter()
                .vline(full.center().x, full.y_range(), theme::stroke(t));
            ui.scope_builder(egui::UiBuilder::new().max_rect(right), |ui| {
                // B1:同左栏,见上面的注释。
                ui.set_clip_rect(right);
                out.0 = show(
                    ui,
                    t,
                    "远端",
                    generation,
                    PanelColumn::Remote,
                    &mut frame.remote,
                    panel_focused && frame.active_column == PanelColumn::Remote,
                    &frame.bookmarks,
                    drop_in,
                );
            });
        });
    out
}

/// SFTP v3 的 mtime 是 Unix 秒。用 `time` crate 格式化 —— 它已在依赖里。
fn mtime_text(secs: u32) -> String {
    match time::OffsetDateTime::from_unix_timestamp(secs as i64) {
        Ok(dt) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            dt.year(),
            dt.month() as u8,
            dt.day(),
            dt.hour(),
            dt.minute()
        ),
        Err(_) => "—".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_ssh::sftp::{Entry, RemotePath};

    /// 在渲染结果的形状树里找**第一处**含 `needle` 的文字中心点,用来给点击
    /// 事件定位。抄自 `session_manager` 几处同名测试辅助(那边是私有的,没有
    /// 跨文件复用的路子,按项目既有做法各自留一份)。
    fn find_text_pos(shapes: &[egui::epaint::ClippedShape], needle: &str) -> Option<egui::Pos2> {
        fn walk(shape: &egui::Shape, needle: &str) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, needle)),
                egui::Shape::Text(ts) if ts.galley.text().contains(needle) => {
                    Some(ts.pos + ts.galley.size() / 2.0)
                }
                _ => None,
            }
        }
        shapes.iter().find_map(|cs| walk(&cs.shape, needle))
    }

    /// 光标停在一个「普通、不大」的文件上 —— 菜单测试的默认目标。
    fn a_file() -> MenuTarget {
        MenuTarget {
            is_file: true,
            size: 1024,
        }
    }

    fn entry(name: &[u8], kind: EntryKind) -> Entry {
        Entry {
            name: RemotePath::from_bytes(name.to_vec()),
            kind,
            size: 1024,
            mtime: 1_700_000_000,
            mode: 0o644,
            uid: 1000,
            gid: 1000,
            link_target: None,
        }
    }

    /// D1:名称文字的起点必须落在图标格子**右侧**,不能重叠 —— 否则长文件名
    /// 一开头就会压在图标上面。判据是几何关系而不是照抄实现算式(照抄的话
    /// 改错了两处、算式仍然自洽就测不出来)。
    ///
    /// **图标格子调 `icon_rect()` 而不是自己重建**——复核挖出的坑:曾经这里
    /// 独立拼过一份 `icon_rect`,生产代码里的 `ICON_LEFT_PAD` 改错了、测试
    /// 拿的还是自己那份「正确」矩形,照样测不出来。共用同一份生产逻辑,
    /// 两处才是同一个判据。
    ///
    /// 自证会变红:把 `name_start_x_offset` 里的 `ICON_GAP` 换成 `-ICON_GAP`,
    /// 或者把整个函数体改回 `4.0`(相当于没让位);也可以把 `icon_rect()`
    /// 里的 `ICON_LEFT_PAD` 换成更大的值(如 `12.0`)而不动
    /// `name_start_x_offset()`——两处一旦失步,这里也会红。
    #[test]
    fn name_start_clears_the_icon_cell() {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, ROW_H));
        let icon = icon_rect(rect);
        let name_x = rect.left() + name_start_x_offset();
        assert!(
            name_x >= icon.right(),
            "名称起点 {name_x} 落在图标格子 {icon:?} 里面,名字会压到图标上"
        );
    }

    /// 两万项的目录里,一帧只该画可见那几十行 —— `show_rows` 没接对
    /// 的症状是帧时间被打穿(陷阱 T3 的同类),而它在小目录下完全看不出来。
    ///
    /// 这是**结构守护**,不是行为守护:egui 0.30 的 `show_rows` 不把
    /// 实际画出的 range 交给调用方(它内部算完直接喂进 `add_contents`,
    /// 外部拿不到),没法从渲染结果反推「到底画了几行」。改成断言源码里
    /// 用的是 `show_rows` 而不是 `show`——只要这一行还在,虚拟滚动就还在。
    /// F52:两栏各有一个传输入口,方向由栏决定。同时守住 D5 —— 本地栏
    /// 永远不出现远端写操作(加入口时最容易顺手把整套菜单抄过去)。
    #[test]
    fn both_columns_offer_a_transfer_entry_but_only_the_remote_one_can_write() {
        let remote = menu_items_for(PanelColumn::Remote, Some(a_file()));
        let local = menu_items_for(PanelColumn::Local, Some(a_file()));
        assert!(
            remote.iter().any(|e| e.label == "下载到本地"),
            "远端栏该有下载:{remote:?}"
        );
        assert!(
            local.iter().any(|e| e.label == "上传到远端"),
            "本地栏该有上传:{local:?}"
        );
        assert!(
            !local.iter().any(|e| matches!(e.item, MenuItem::Ask(_))),
            "本地栏冒出了远端写操作(D5):{local:?}"
        );
    }

    /// 没有光标行时不给传输入口 —— 点了没反应的菜单项比没有更让人困惑
    /// (与 `重命名…`/`删除…` 同一条口径)。
    #[test]
    fn no_cursor_means_no_transfer_entry_at_all() {
        for column in [PanelColumn::Remote, PanelColumn::Local] {
            let items = menu_items_for(column, None);
            assert!(
                !items.iter().any(|e| e.item == MenuItem::Transfer),
                "{column:?} 栏在没有光标行时给出了传输入口:{items:?}"
            );
        }
    }

    #[test]
    fn a_huge_directory_is_rendered_with_show_rows_not_a_full_scan() {
        // `include_str!` 把这份测试代码自身也读了进来 —— 断言字符串若直接
        // 写死要找的那句代码,会在源码里连自己这行都算命中,变成一条自证
        // 自伪的假测试。用 `#[cfg(test)]` 之前的部分(纯生产代码,不含本
        // 测试模块)来匹配,避免这个自我引用陷阱。
        let src = include_str!("files_panel.rs");
        // 用 `split_once`:`split().next()` 在找不到分隔符时返回的是**整份源码**
        // 的 `Some`,那句 `.expect("找不到边界")` 永远走不到 —— 一条自称在兜底、
        // 实际是死代码的断言。`split_once` 找不到就是 `None`,兜底才真的存在。
        let (production, _) = src
            .split_once("#[cfg(test)]")
            .expect("找不到 #[cfg(test)] 边界");
        assert!(
            production.contains(".show_rows(ui, ROW_H, rows.len(),"),
            "渲染必须走 show_rows 虚拟滚动,否则两万项目录会把帧时间打穿"
        );
    }

    /// 把一栏画两帧,收集画出来的全部文本。
    ///
    /// 两帧不是保险起见:egui 的 `Panel`/`Area` 首帧带 fade_in,记的是
    /// `Shape::Noop`,只画一帧一个字都读不到。
    fn rendered_texts(state: &mut PaneState) -> Vec<String> {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut texts = Vec::new();
        for _ in 0..2 {
            texts.clear();
            let out = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(ui, &t, "远端", 1, PanelColumn::Remote, state, false, &[], 0);
                });
            });
            for shape in out.shapes.iter() {
                if let egui::epaint::Shape::Text(ts) = &shape.shape {
                    texts.push(ts.galley.text().to_owned());
                }
            }
        }
        texts
    }

    /// 非 UTF-8 的名字必须**看得见**(用户要知道那儿有东西)且**标注不可操作**。
    #[test]
    fn a_non_utf8_name_is_shown_with_an_explicit_note() {
        let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
        state.entries = vec![entry(
            &[0xd6, 0xd0, b'.', b't', b'x', b't'],
            EntryKind::File,
        )];
        state.load = Load::Ready;

        let texts = rendered_texts(&mut state);
        assert!(
            texts.iter().any(|s| s.contains("名称非 UTF-8")),
            "非 UTF-8 条目要带明确说明,实际画出来的文本: {texts:?}"
        );
        assert!(
            texts.iter().any(|s| s.contains('\u{fffd}')),
            "同时还要能看见那个名字本身"
        );
    }

    /// 线上 lossy 过的名字(**本身是合法 UTF-8**,只是含 `U+FFFD`)同样要
    /// 标注不可操作。
    ///
    /// 这条和上一条**不能互相替代**:上一条的 fixture 是裸的非法 UTF-8 字节,
    /// `is_utf8()` 与 `is_operable()` 在它身上答案一致,于是把 `row()` 的判据
    /// 从 `is_operable()` 改回 `is_utf8()` 它照样绿。而 `russh-sftp 2.4.0` 收包
    /// 时就把非 UTF-8 字节 lossy 成了这种串 —— 真实链路上到手的**只有**这一类,
    /// 上一条守不住的恰恰是唯一会发生的那种。
    #[test]
    fn a_lossy_name_that_is_valid_utf8_is_still_marked_unusable() {
        let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
        state.entries = vec![entry("\u{fffd}\u{fffd}.txt".as_bytes(), EntryKind::File)];
        state.load = Load::Ready;
        assert!(
            state.entries[0].name.is_utf8(),
            "前提:这串本身是合法 UTF-8,这正是不能拿 is_utf8() 当判据的原因"
        );

        let texts = rendered_texts(&mut state);
        assert!(
            texts.iter().any(|s| s.contains("名称非 UTF-8")),
            "lossy 名字也要标注不可操作,实际画出来的文本: {texts:?}"
        );
    }

    /// D1/D4:标签宿主两栏必须**都**画出来,且各自读的是自己那份状态 ——
    /// 远端 fixture 的名字不该出现在本地栏,反之亦然。用与 `rendered_texts`
    /// 相同的两帧手法(egui `Panel`/`Area` 首帧 sizing pass 的坑,见其文档)。
    ///
    /// 破坏性验证:把 `content` 里第二个 `show(ui, t, "本地", &mut frame.local, ...)`
    /// 误写成再画一次 `&mut frame.remote`(复制粘贴错一个字段名的那类真实
    /// 失误)——`local-only.txt` 必须消失,断言 2 变红。
    #[test]
    fn content_renders_both_panels_with_their_own_state() {
        let t = crate::theme::MULLION_DARK;
        let mut frame = PanelFrame {
            remote: PaneState::new(RemotePath::from_bytes(b"/".to_vec())),
            local: PaneState::new(RemotePath::from_bytes(b"/".to_vec())),
            bookmarks: Vec::new(),
            active_column: PanelColumn::default(),
        };
        frame.remote.entries = vec![entry(b"remote-only.txt", EntryKind::File)];
        frame.remote.load = Load::Ready;
        frame.local.entries = vec![entry(b"local-only.txt", EntryKind::File)];
        frame.local.load = Load::Ready;

        let ctx = egui::Context::default();
        let mut texts = Vec::new();
        for _ in 0..2 {
            texts.clear();
            let out = ctx.run(egui::RawInput::default(), |ctx| {
                content(ctx, &t, 1, false, &mut frame, 0);
            });
            for shape in out.shapes.iter() {
                if let egui::epaint::Shape::Text(ts) = &shape.shape {
                    texts.push(ts.galley.text().to_owned());
                }
            }
        }
        assert!(
            texts.iter().any(|s| s.contains("remote-only.txt")),
            "远端栏没画出来,实际画出来的文本: {texts:?}"
        );
        assert!(
            texts.iter().any(|s| s.contains("local-only.txt")),
            "本地栏没画出来,实际画出来的文本: {texts:?}"
        );
    }

    /// 在渲染结果的形状树里数「描边颜色是 `color`、宽度 > 0」的矩形个数 ——
    /// 焦点边框就是这么画的(`show` 里 `ui.painter().rect_stroke(...,
    /// Stroke::new(2.0, theme::c32(t.accent)))`)。跟 `find_text_pos` 一样
    /// 要递归 `Shape::Vec`,egui 把子控件的形状树套在里面一层。
    fn count_stroked_rects(shapes: &[egui::epaint::ClippedShape], color: egui::Color32) -> usize {
        fn walk(shape: &egui::Shape, color: egui::Color32, n: &mut usize) {
            match shape {
                egui::Shape::Vec(v) => {
                    for s in v {
                        walk(s, color, n);
                    }
                }
                egui::Shape::Rect(r) if r.stroke.color == color && r.stroke.width > 0.0 => {
                    *n += 1;
                }
                _ => {}
            }
        }
        let mut n = 0;
        for cs in shapes {
            walk(&cs.shape, color, &mut n);
        }
        n
    }

    /// 复核 #2(代码质量复核挖出的可达性缺口):F6/Tab 把键盘焦点切到文件
    /// 面板时,必须画一条能看见的边框——不然 F6/Tab 是否生效在真实使用和
    /// Task 13 的人工验收里都无从判断。未聚焦(`focused == false`)时**不能
    /// 画任何东西**,常亮的指示器等于没有信息量。颜色复用既有语义色
    /// `t.accent`(选中态同款),不新造色值(UI 视觉规格已冻结,spec §4.6)。
    ///
    /// 变异点:把 `show` 里的 `if focused { ... }` 改成恒为 `true`(或者
    /// 干脆删掉 `if focused` 只留边框绘制)——`focused == false` 分支的
    /// 断言(数量为 0)必须变红。反过来把边框绘制整段删掉,`focused ==
    /// true` 分支的断言(数量为 1)也必须变红。
    #[test]
    fn a_focus_border_is_drawn_only_when_the_panel_has_keyboard_focus() {
        let t = crate::theme::MULLION_DARK;
        let accent = theme::c32(t.accent);

        let render = |focused: bool| -> usize {
            let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
            state.entries = vec![entry(b"a.txt", EntryKind::File)];
            state.load = Load::Ready;
            let ctx = egui::Context::default();
            let mut n = 0;
            for _ in 0..2 {
                let out = ctx.run(egui::RawInput::default(), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        show(
                            ui,
                            &t,
                            "远端",
                            1,
                            PanelColumn::Remote,
                            &mut state,
                            focused,
                            &[],
                            0,
                        );
                    });
                });
                n = count_stroked_rects(&out.shapes, accent);
            }
            n
        };

        assert_eq!(
            render(false),
            0,
            "面板没有键盘焦点时不该画任何焦点边框(常亮 = 没有信息量)"
        );
        assert_eq!(render(true), 1, "面板持有键盘焦点时必须画出焦点边框");
    }

    /// 代码复核挖出的真 bug:`ScrollArea` 的持久化 id 若只拼 `id`
    /// (`"远端"`/`"本地"`),跟哪个标签无关——`egui::Context` 整窗口只建
    /// 一次、跨标签复用,两个 SFTP 标签的同一栏会撞出同一个 `Id`,标签 A
    /// 滚过的偏移量被标签 B 直接继承(症状:开两个 SFTP 节点标签,在 A 的
    /// 远端栏滚动过,切到 B、明明是完全不同的目录,滚动条却停在 A 滚到的
    /// 位置)。掺进 `generation`(S1 路由键,单调递增、标签一多就必然不同)
    /// 是唯一的修法——同一个 `id`、不同 `generation` 必须算出不同的 salt。
    ///
    /// 破坏性验证:把 `scroll_id_salt` 里的 `generation` 从格式串里删掉
    /// (退回 `format!("files-{id}")`)——这条断言必须变红。
    #[test]
    fn scroll_id_salt_differs_by_generation() {
        assert_ne!(
            scroll_id_salt("远端", 1),
            scroll_id_salt("远端", 2),
            "同一栏(远端)、不同标签(generation 1 vs 2)算出了相同的 \
             ScrollArea 持久化 id —— 两个标签会共享同一份滚动偏移"
        );
        // 顺带钉一下反面:同一个标签内部,远端栏和本地栏本来就不该撞 id
        // (这条不是本次 bug 的成因,但既然抽出了纯函数,顺手一并钉死更省心)。
        assert_ne!(
            scroll_id_salt("远端", 1),
            scroll_id_salt("本地", 1),
            "同一标签内远端栏和本地栏的 ScrollArea id 撞了"
        );
    }

    /// F6/Tab 换焦点(设计 D23):`PanelColumn::flipped` 必须真的换到另一栏,
    /// 不是恒返回同一个值。
    #[test]
    fn panel_column_flipped_swaps_remote_and_local() {
        assert_eq!(PanelColumn::Remote.flipped(), PanelColumn::Local);
        assert_eq!(PanelColumn::Local.flipped(), PanelColumn::Remote);
    }

    /// `PanelFrame::active_state`/`active_state_mut` 必须按 `active_column`
    /// 取到**对应那一栏**的状态,不能不管焦点在哪恒返回某一栏——`handle_panel_key`
    /// 的 Enter/Backspace/F5/↑/↓ 全靠这两个访问器分流到正确的一栏,选错了会让
    /// 用户在远端栏按 Enter 却进了本地目录(反之亦然)。
    #[test]
    fn active_state_follows_active_column_not_a_fixed_side() {
        let mut frame = PanelFrame {
            remote: PaneState::new(RemotePath::from_bytes(b"/remote".to_vec())),
            local: PaneState::new(RemotePath::from_bytes(b"/local".to_vec())),
            bookmarks: Vec::new(),
            active_column: PanelColumn::Remote,
        };
        assert_eq!(frame.active_state().0, PanelColumn::Remote);
        assert_eq!(
            frame.active_state().1.cwd,
            RemotePath::from_bytes(b"/remote".to_vec())
        );
        assert_eq!(
            frame.active_state_mut().cwd,
            RemotePath::from_bytes(b"/remote".to_vec())
        );

        frame.active_column = PanelColumn::Local;
        assert_eq!(frame.active_state().0, PanelColumn::Local);
        assert_eq!(
            frame.active_state().1.cwd,
            RemotePath::from_bytes(b"/local".to_vec())
        );
        assert_eq!(
            frame.active_state_mut().cwd,
            RemotePath::from_bytes(b"/local".to_vec())
        );
    }

    /// F120:书签栏要画出来,且点一下就要发出对应路径的 `Goto`。
    /// 空名字的书签(store 里明确允许)必须回退显示路径本身,不能画个空按钮
    /// 让用户根本点不到。
    #[test]
    fn clicking_a_bookmark_dispatches_goto_to_its_path() {
        let t = crate::theme::MULLION_DARK;
        let mut state = PaneState::new(RemotePath::from_bytes(b"/".to_vec()));
        state.load = Load::Ready;
        let bookmarks = vec![mullion_store::Bookmark {
            name: "日志".into(),
            path: "/var/log".into(),
        }];
        let ctx = egui::Context::default();
        // 先跑一帧稳定布局(egui Panel 首帧 fade_in 只记 Shape::Noop)。
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                show(
                    ui,
                    &t,
                    "远端",
                    1,
                    PanelColumn::Remote,
                    &mut state,
                    false,
                    &bookmarks,
                    0,
                );
            });
        });
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                show(
                    ui,
                    &t,
                    "远端",
                    1,
                    PanelColumn::Remote,
                    &mut state,
                    false,
                    &bookmarks,
                    0,
                );
            });
        });
        let pos = find_text_pos(&out.shapes, "日志").expect("书签「日志」没画出来");

        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        });
        input.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        });
        let mut action = None;
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                action = show(
                    ui,
                    &t,
                    "远端",
                    1,
                    PanelColumn::Remote,
                    &mut state,
                    false,
                    &bookmarks,
                    0,
                );
            });
        });
        assert_eq!(
            action,
            Some(FileAction::Goto(RemotePath::from_bytes(
                b"/var/log".to_vec()
            ))),
            "点书签该发出指向它 path 的 Goto"
        );
    }

    /// 空名字是 store 明确允许的合法状态(`Bookmark::name` 文档),界面必须
    /// 回退显示路径本身,而不是画一个没有任何文字的按钮。
    #[test]
    fn a_bookmark_with_an_empty_name_falls_back_to_showing_its_path() {
        let t = crate::theme::MULLION_DARK;
        let mut state = PaneState::new(RemotePath::from_bytes(b"/".to_vec()));
        state.load = Load::Ready;
        let bookmarks = vec![mullion_store::Bookmark {
            name: String::new(),
            path: "/srv/app".into(),
        }];
        let ctx = egui::Context::default();
        let mut texts = Vec::new();
        for _ in 0..2 {
            texts.clear();
            let out = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        "远端",
                        1,
                        PanelColumn::Remote,
                        &mut state,
                        false,
                        &bookmarks,
                        0,
                    );
                });
            });
            for shape in out.shapes.iter() {
                if let egui::epaint::Shape::Text(ts) = &shape.shape {
                    texts.push(ts.galley.text().to_owned());
                }
            }
        }
        assert!(
            texts.iter().any(|s| s.contains("/srv/app")),
            "空名字的书签要回退显示路径,实际画出来的文本: {texts:?}"
        );
    }

    /// D1:书签栏只在远端栏出现。用 `content()` 整体渲染,书签名只该出现
    /// 一次 —— 若本地栏调用点也误传了 `frame.bookmarks`(复制粘贴错一个
    /// 参数的那类真实失误),这个名字会画两遍。
    #[test]
    fn only_the_remote_column_gets_a_bookmarks_bar() {
        let t = crate::theme::MULLION_DARK;
        let mut frame = PanelFrame {
            remote: PaneState::new(RemotePath::from_bytes(b"/".to_vec())),
            local: PaneState::new(RemotePath::from_bytes(b"/".to_vec())),
            bookmarks: vec![mullion_store::Bookmark {
                name: "日志".into(),
                path: "/var/log".into(),
            }],
            active_column: PanelColumn::default(),
        };
        frame.remote.load = Load::Ready;
        frame.local.load = Load::Ready;

        let ctx = egui::Context::default();
        let mut texts = Vec::new();
        for _ in 0..2 {
            texts.clear();
            let out = ctx.run(egui::RawInput::default(), |ctx| {
                content(ctx, &t, 1, false, &mut frame, 0);
            });
            for shape in out.shapes.iter() {
                if let egui::epaint::Shape::Text(ts) = &shape.shape {
                    texts.push(ts.galley.text().to_owned());
                }
            }
        }
        let count = texts.iter().filter(|s| s.contains("日志")).count();
        assert_eq!(count, 1, "书签「日志」该只出现在远端栏一次,实际: {texts:?}");
    }

    /// D5:**本地栏没有写操作入口**。菜单项的存在与否是纯结构的事,
    /// 用 `menu_items_for` 这个纯函数验,不必真去点开右键菜单
    /// (egui 的 `context_menu` 要一次右键 + 一帧才展开,测起来又脆又慢)。
    #[test]
    fn the_local_column_never_offers_a_write_operation() {
        let remote = menu_items_for(PanelColumn::Remote, Some(a_file()));
        let local = menu_items_for(PanelColumn::Local, Some(a_file()));
        for ask in [
            FileAsk::NewDir,
            FileAsk::Rename,
            FileAsk::Delete,
            FileAsk::Chmod,
        ] {
            assert!(
                remote.iter().any(|e| e.item == MenuItem::Ask(ask)),
                "远端栏该有 {ask:?}"
            );
            assert!(
                !local.iter().any(|e| e.item == MenuItem::Ask(ask)),
                "本地栏不该出现 {ask:?}(D5:本地文件管理外包给资源管理器)"
            );
        }
        assert!(
            local.iter().any(|e| e.item == MenuItem::OpenInExplorer),
            "本地栏该有「在资源管理器中打开」"
        );
    }

    /// 没有光标行时,单目标操作(重命名 / 改权限 / 删除)必须不可用 ——
    /// 给一个「点了没反应」的菜单项比不给更让人困惑。
    #[test]
    fn single_target_operations_are_absent_without_a_cursor_row() {
        let items = menu_items_for(PanelColumn::Remote, None);
        for ask in [FileAsk::Rename, FileAsk::Chmod, FileAsk::Delete] {
            assert!(
                !items.iter().any(|e| e.item == MenuItem::Ask(ask)),
                "没有光标行时不该出现 {ask:?}"
            );
        }
        // 「新建文件夹」不需要选中任何东西 —— 空目录里也得能建。
        assert!(items
            .iter()
            .any(|e| e.item == MenuItem::Ask(FileAsk::NewDir)));
    }

    /// 摆一份「远端有个 logs 目录 + 一个 b.txt,本地有个 a.txt」的两栏。
    fn two_columns() -> PanelFrame {
        let mut frame = PanelFrame::default();
        frame.remote.cwd = RemotePath::from_bytes(b"/srv".to_vec());
        frame.remote.entries = vec![
            entry(b"logs", EntryKind::Dir),
            entry(b"b.txt", EntryKind::File),
        ];
        frame.remote.load = Load::Ready;
        frame.local.entries = vec![entry(b"a.txt", EntryKind::File)];
        frame.local.load = Load::Ready;
        frame
    }

    /// 拖拽测试统一用一块 1200x800 的「窗口」。**必须显式给 `screen_rect`**:
    /// `RawInput::default()` 不给的话 egui 退回到一块上万像素的默认区域,
    /// 两栏各占五千多,`find_text_pos` 算出来的坐标落在那块区域之外,指针
    /// 命中不了任何一行 —— 拖拽测试会安静地什么都测不到。
    fn raw(time: Option<f64>) -> egui::RawInput {
        egui::RawInput {
            time,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            ..Default::default()
        }
    }

    fn press(pos: egui::Pos2, time: f64, pressed: bool) -> egui::RawInput {
        let mut input = raw(Some(time));
        input.events.push(egui::Event::PointerMoved(pos));
        input.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        });
        input
    }

    fn moved(pos: egui::Pos2, time: f64) -> egui::RawInput {
        let mut input = raw(Some(time));
        input.events.push(egui::Event::PointerMoved(pos));
        input
    }

    /// F58:栏间拖拽。**这是拖拽唯一的无头守护** —— `drag.rs` 那几条纯函数
    /// 可以全绿而 payload 根本没挂上去 / 落点根本没读回来。
    #[test]
    fn dragging_from_the_local_column_onto_a_remote_directory_row_drops_into_that_directory() {
        let t = crate::theme::MULLION_DARK;
        let mut frame = two_columns();
        frame
            .local
            .selected
            .insert(RemotePath::from_bytes(b"a.txt".to_vec()));
        let ctx = egui::Context::default();
        let render = |input: egui::RawInput, frame: &mut PanelFrame| {
            let mut acts = (None, None);
            let out = ctx.run(input, |ctx| {
                acts = content(ctx, &t, 1, true, frame, 0);
            });
            (acts, out)
        };
        // 两帧稳定布局(egui Panel 首帧 fade_in 只记 Shape::Noop)。
        let _ = render(raw(None), &mut frame);
        let (_, out) = render(raw(None), &mut frame);
        let src = find_text_pos(&out.shapes, "a.txt").expect("本地栏该画出 a.txt");
        let dst = find_text_pos(&out.shapes, "logs").expect("远端栏该画出 logs");

        // 按下 →(移开超过点击阈值,egui 才判成拖)→ 在目标行松手。
        let _ = render(press(src, 1.0, true), &mut frame);
        let _ = render(moved(dst, 1.1), &mut frame);
        let (acts, _) = render(press(dst, 1.2, false), &mut frame);

        assert_eq!(
            acts.0,
            Some(FileAction::Drop(crate::files::drag::Landing::Sub(
                b"logs".to_vec()
            ))),
            "落在远端的 logs 目录行上,该发出「传进 logs」"
        );
        assert_eq!(acts.1, None, "本地栏不该同时也收到一份");
    }

    /// 落在行与行之间的空白上 → 传到当前目录,而不是「什么也没发生」。
    #[test]
    fn dropping_on_the_blank_part_of_a_column_targets_its_current_directory() {
        let t = crate::theme::MULLION_DARK;
        let mut frame = two_columns();
        frame
            .local
            .selected
            .insert(RemotePath::from_bytes(b"a.txt".to_vec()));
        let ctx = egui::Context::default();
        let render = |input: egui::RawInput, frame: &mut PanelFrame| {
            let mut acts = (None, None);
            let out = ctx.run(input, |ctx| {
                acts = content(ctx, &t, 1, true, frame, 0);
            });
            (acts, out)
        };
        let _ = render(raw(None), &mut frame);
        let (_, out) = render(raw(None), &mut frame);
        let src = find_text_pos(&out.shapes, "a.txt").expect("本地栏该画出 a.txt");
        let logs = find_text_pos(&out.shapes, "logs").expect("远端栏该画出 logs");
        // 远端栏里、所有行下面很远的空白处。
        let blank = egui::pos2(logs.x, logs.y + 400.0);

        let _ = render(press(src, 1.0, true), &mut frame);
        let _ = render(moved(blank, 1.1), &mut frame);
        let (acts, _) = render(press(blank, 1.2, false), &mut frame);

        assert_eq!(
            acts.0,
            Some(FileAction::Drop(crate::files::drag::Landing::Cwd)),
            "落在空白处该传到远端栏当前目录"
        );
    }

    /// 拖一条**没选中**的行:先让它成为唯一选中项。不这么做的话用户拖的是
    /// 这一条、传走的却是别处那批选中项 —— 与右键菜单那条陷阱同源。
    #[test]
    fn dragging_an_unselected_row_makes_it_the_only_selection_so_it_is_what_gets_transferred() {
        let t = crate::theme::MULLION_DARK;
        let mut frame = two_columns();
        // 远端栏里选中的是 b.txt,而用户去拖 logs。
        frame.remote.entries.push(entry(b"c.txt", EntryKind::File));
        frame
            .remote
            .selected
            .insert(RemotePath::from_bytes(b"b.txt".to_vec()));
        let ctx = egui::Context::default();
        let render = |input: egui::RawInput, frame: &mut PanelFrame| {
            let mut acts = (None, None);
            let out = ctx.run(input, |ctx| {
                acts = content(ctx, &t, 1, true, frame, 0);
            });
            (acts, out)
        };
        let _ = render(raw(None), &mut frame);
        let (_, out) = render(raw(None), &mut frame);
        let src = find_text_pos(&out.shapes, "c.txt").expect("远端栏该画出 c.txt");
        let dst = find_text_pos(&out.shapes, "a.txt").expect("本地栏该画出 a.txt");

        let _ = render(press(src, 1.0, true), &mut frame);
        let _ = render(moved(dst, 1.1), &mut frame);
        let _ = render(press(dst, 1.2, false), &mut frame);

        let only: Vec<String> = frame
            .remote
            .selected
            .iter()
            .map(|p| p.display().into_owned())
            .collect();
        assert_eq!(
            only,
            vec!["c.txt".to_string()],
            "拖了 c.txt,选中集却还是别的 —— 传走的会是用户没拖的那条"
        );
    }

    /// 同一栏内部拖 = 移动/改名,本切片不做。放过去的话「把远端文件上传到
    /// 它自己」会先截断再读,文件直接清零。
    #[test]
    fn dragging_inside_one_column_dispatches_nothing() {
        let t = crate::theme::MULLION_DARK;
        let mut frame = two_columns();
        frame
            .remote
            .selected
            .insert(RemotePath::from_bytes(b"b.txt".to_vec()));
        let ctx = egui::Context::default();
        let render = |input: egui::RawInput, frame: &mut PanelFrame| {
            let mut acts = (None, None);
            let out = ctx.run(input, |ctx| {
                acts = content(ctx, &t, 1, true, frame, 0);
            });
            (acts, out)
        };
        let _ = render(raw(None), &mut frame);
        let (_, out) = render(raw(None), &mut frame);
        let src = find_text_pos(&out.shapes, "b.txt").expect("远端栏该画出 b.txt");
        let dst = find_text_pos(&out.shapes, "logs").expect("远端栏该画出 logs");

        let _ = render(press(src, 1.0, true), &mut frame);
        let _ = render(moved(dst, 1.1), &mut frame);
        // 先自证「这一拖真的发生了」—— 少了这句,哪天拖拽整个不工作了,
        // 下面那两条 `None` 照样全绿。
        assert!(
            egui::DragAndDrop::has_any_payload(&ctx),
            "拖都没拖起来,下面的断言就什么也证明不了"
        );
        let (acts, _) = render(press(dst, 1.2, false), &mut frame);

        assert_eq!(acts.0, None, "同栏内拖不该发出任何传输");
        assert_eq!(acts.1, None);
    }

    /// 标签宿主里两栏各占**整块**高度。曾经用 `ui.horizontal` +
    /// `allocate_ui(vec2(half, ui.available_height()))` 分配 —— 在 horizontal
    /// 布局里那个高度是「当前这一行」的 18pt,于是两栏被压成一行高,
    /// `ScrollArea` 视口塌了、行的交互 rect 宽度变成 0:文字照画(painter
    /// 按坐标画,不看 rect),但**一行都点不中**。D4a 做拖拽时才暴露出来。
    #[test]
    fn a_row_in_the_tab_host_can_actually_be_clicked() {
        let t = crate::theme::MULLION_DARK;
        let mut frame = two_columns();
        let ctx = egui::Context::default();
        let render = |input: egui::RawInput, frame: &mut PanelFrame| {
            let mut acts = (None, None);
            let out = ctx.run(input, |ctx| {
                acts = content(ctx, &t, 1, true, frame, 0);
            });
            (acts, out)
        };
        let _ = render(raw(None), &mut frame);
        let (_, out) = render(raw(None), &mut frame);
        let pos = find_text_pos(&out.shapes, "b.txt").expect("远端栏该画出 b.txt");

        let _ = render(press(pos, 1.0, true), &mut frame);
        let _ = render(press(pos, 1.05, false), &mut frame);

        assert_eq!(
            frame
                .remote
                .cursor
                .as_ref()
                .map(|p| p.display().into_owned()),
            Some("b.txt".to_string()),
            "在标签宿主里点一行,光标该落到那一行 —— 落不上说明行根本没被点中"
        );
    }

    /// 右键菜单要真的挂上去、真的能发出动作 —— `menu_items_for` 全对但
    /// 渲染里没接线,上面两条纯函数测试照样全绿。
    #[test]
    fn right_clicking_the_remote_column_opens_a_menu_that_can_dispatch_an_ask() {
        let t = crate::theme::MULLION_DARK;
        let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
        state.entries = vec![entry(b"a.txt", EntryKind::File)];
        state.load = Load::Ready;
        let ctx = egui::Context::default();
        let render = |input: egui::RawInput, state: &mut PaneState| {
            let mut action = None;
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    action = show(ui, &t, "远端", 1, PanelColumn::Remote, state, false, &[], 0);
                });
            });
            (action, out)
        };
        // 两帧稳定布局(egui Panel 首帧 fade_in 只记 Shape::Noop)。
        let _ = render(egui::RawInput::default(), &mut state);
        let _ = render(egui::RawInput::default(), &mut state);

        // 在面板中部右键 → 菜单展开。
        let pos = egui::pos2(200.0, 200.0);
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed: true,
            modifiers: Default::default(),
        });
        input.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed: false,
            modifiers: Default::default(),
        });
        let _ = render(input, &mut state);
        let (_, out) = render(egui::RawInput::default(), &mut state);
        let target =
            find_text_pos(&out.shapes, "新建文件夹").expect("右键之后菜单里该有「新建文件夹…」");

        // 点它 → 发出 Ask(NewDir)。
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::PointerButton {
            pos: target,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        });
        input.events.push(egui::Event::PointerButton {
            pos: target,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        });
        let (action, _) = render(input, &mut state);
        assert_eq!(
            action,
            Some(FileAction::Ask(FileAsk::NewDir)),
            "点菜单里的「新建文件夹…」该发出 Ask(NewDir)"
        );
    }

    /// F53/D5:编辑的是**远端**文件。本地栏双击/右键都不该冒出这两项 ——
    /// 本地文件本来就该在资源管理器里双击。
    #[test]
    fn the_local_column_never_offers_a_remote_edit_entry() {
        let local = menu_items_for(PanelColumn::Local, Some(a_file()));
        assert!(
            !local
                .iter()
                .any(|e| matches!(e.item, MenuItem::EditExternal | MenuItem::EditInline)),
            "本地栏冒出了远端编辑入口:{local:?}"
        );
        // 反面:远端栏必须有,否则上一条断言在「谁都没有」时也是绿的。
        let remote = menu_items_for(PanelColumn::Remote, Some(a_file()));
        assert!(
            remote
                .iter()
                .any(|e| matches!(e.item, MenuItem::EditExternal | MenuItem::EditInline)),
            "远端栏该有编辑入口:{remote:?}"
        );
    }

    /// 目录 / 链接上给一个「编辑」纯属误导 —— 点下去只能报错。
    #[test]
    fn only_a_plain_file_offers_editing() {
        let dir = MenuTarget {
            is_file: false,
            size: 4096,
        };
        let items = menu_items_for(PanelColumn::Remote, Some(dir));
        assert!(
            !items
                .iter()
                .any(|e| matches!(e.item, MenuItem::EditExternal | MenuItem::EditInline)),
            "目录上不该出现编辑项:{items:?}"
        );
        // 但目录仍然可以下载 / 重命名 —— 别把整段菜单一起砍掉了。
        assert!(items.iter().any(|e| e.item == MenuItem::Transfer));
    }

    /// 太大的文件:菜单项**留着并置灰、带理由**,不是悄悄消失。
    /// 少一项用户只会以为程序坏了(D3-2)。
    #[test]
    fn an_oversize_file_keeps_a_greyed_entry_that_says_why() {
        let big = MenuTarget {
            is_file: true,
            size: crate::edit::INLINE_LIMIT + 1,
        };
        let items = menu_items_for(PanelColumn::Remote, Some(big));
        let inline = items
            .iter()
            .find(|e| e.item == MenuItem::EditInline)
            .expect("超限也该看得见这一项");
        assert!(
            inline.disabled.is_some_and(|w| w.contains("1 MB")),
            "该说清为什么点不了:{inline:?}"
        );
        // 只超内置那一档时,外部编辑还得能点。
        let ext = items
            .iter()
            .find(|e| e.item == MenuItem::EditExternal)
            .expect("该有外部编辑");
        assert!(ext.disabled.is_none(), "1 MB 出头不该拦住外部编辑:{ext:?}");

        let huge = MenuTarget {
            is_file: true,
            size: crate::edit::EXTERNAL_LIMIT + 1,
        };
        let items = menu_items_for(PanelColumn::Remote, Some(huge));
        assert!(
            items
                .iter()
                .find(|e| e.item == MenuItem::EditExternal)
                .expect("该有外部编辑")
                .disabled
                .is_some(),
            "超过外部上限该置灰:{items:?}"
        );
    }

    /// D20:双击一个**文件**在 D1/D2 里是「什么都不发生」——最容易被当成
    /// 程序卡了。F53 之后它该直接开外部编辑器。
    #[test]
    fn double_clicking_a_plain_file_opens_the_external_editor_instead_of_doing_nothing() {
        let t = crate::theme::MULLION_DARK;
        let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
        state.entries = vec![
            entry(b"a.txt", EntryKind::File),
            entry(b"sub", EntryKind::Dir),
        ];
        state.load = Load::Ready;
        let ctx = egui::Context::default();
        let render = |input: egui::RawInput, state: &mut PaneState| {
            let mut action = None;
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    action = show(ui, &t, "远端", 1, PanelColumn::Remote, state, false, &[], 0);
                });
            });
            (action, out)
        };
        let _ = render(egui::RawInput::default(), &mut state);
        let (_, out) = render(egui::RawInput::default(), &mut state);

        // `at` 是这一帧的时间戳。**必须显式给、而且两次双击要隔开** ——
        // egui 0.30 的连击计数**只看时间不看位置**(`input_state/mod.rs`
        // 的 `triple_click`),紧挨着的第二次双击会被算成三连击,
        // `double_clicked()` 只认 count == 2,于是一声不响地不触发。
        let double = |pos: egui::Pos2, at: f64| {
            let mut input = egui::RawInput {
                time: Some(at),
                ..Default::default()
            };
            input.events.push(egui::Event::PointerMoved(pos));
            for _ in 0..2 {
                for pressed in [true, false] {
                    input.events.push(egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    });
                }
            }
            input
        };

        let file_pos = find_text_pos(&out.shapes, "a.txt").expect("列表里该有 a.txt");
        let (action, _) = render(double(file_pos, 1.0), &mut state);
        assert_eq!(
            action,
            Some(FileAction::EditExternal),
            "双击文件该开外部编辑器"
        );
        // 而且光标要落在被双击的那一行 —— 动作不带路径,靠光标定目标。
        assert_eq!(
            state.cursor.as_ref().map(|c| c.as_bytes().to_vec()),
            Some(b"a.txt".to_vec()),
            "双击没把光标挪过去,app 侧会去编辑别的文件"
        );

        // 反面:同一份数据挂在**本地栏**上,双击文件什么也不该发生
        // (D5:本地文件在资源管理器里双击就行)。菜单那条口径已经有
        // `the_local_column_never_offers_a_remote_edit_entry` 守着,
        // 双击是另一条独立的入口,漏了它照样能把本地文件送去编辑远端。
        {
            let ctx = egui::Context::default();
            let mut local = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
            local.entries = vec![entry(b"a.txt", EntryKind::File)];
            local.load = Load::Ready;
            let render_local = |input: egui::RawInput, state: &mut PaneState| {
                let mut action = None;
                let out = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        action = show(ui, &t, "本地", 1, PanelColumn::Local, state, false, &[], 0);
                    });
                });
                (action, out)
            };
            let _ = render_local(egui::RawInput::default(), &mut local);
            let (_, out) = render_local(egui::RawInput::default(), &mut local);
            let pos = find_text_pos(&out.shapes, "a.txt").expect("本地栏里该有 a.txt");
            let (action, _) = render_local(double(pos, 1.0), &mut local);
            assert_eq!(action, None, "本地栏双击文件不该发出任何远端动作");
        }

        // 反面:双击**目录**仍然是进目录,不能被编辑这条路抢走。
        // 先空跑一帧让 egui 的点击计数归零 —— 紧挨着上一次双击再点,
        // 它会当成一串连击而不是新的一次双击。
        let (_, out) = render(egui::RawInput::default(), &mut state);
        let dir_pos = find_text_pos(&out.shapes, "sub").expect("列表里该有 sub");
        let (action, _) = render(double(dir_pos, 10.0), &mut state);
        assert!(
            matches!(action, Some(FileAction::Goto(_))),
            "双击目录该进目录,实际 {action:?}"
        );
    }

    /// B2:点列头必须真的能排序。功能本身早就在(`click_header`),
    /// 坏的是**点击落不到列头上** —— 整栏背景挂了一个覆盖 `max_rect` 的
    /// `interact`(右键菜单宿主),它把列头的命中吃掉了。
    ///
    /// **必须预热若干帧再点**:egui 的部件矩形要上一帧的布局结果才存在,
    /// 第一帧注入点击必然打空(这是本项目 egui 测试的既有坑,见切片 G
    /// 「预热帧数不足的两种静默症状」)。
    ///
    /// 自证会变红:把 `header()` 里 `if resp.clicked() { hit = Some(key) }`
    /// 改成 `if false { .. }`。
    #[test]
    fn clicking_the_name_header_actually_flips_the_sort_direction() {
        let t = crate::theme::MULLION_DARK;
        let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
        state.entries = vec![
            entry(b"b.txt", EntryKind::File),
            entry(b"a.txt", EntryKind::File),
        ];
        state.load = Load::Ready;
        assert_eq!(state.sort_key, SortKey::Name);
        assert_eq!(state.sort_dir, crate::files::SortDir::Asc);

        let ctx = egui::Context::default();
        // 取矩形的前提:标注模式必须开着,否则 `mark` 直接 return(Task 0)。
        annotate::toggle(&ctx);
        // **真实窗口宽度**,不能用 egui 测试默认的无穷大画布:侧栏典型宽
        // 320~450px(见 `DEFAULT_SIDEBAR_W`),不设边界的话「名称」列会占掉
        // 几乎全部宽度,点哪儿都落在它身上,测试测不出真实场景下四列挤在
        // 一起时点击会不会打偏。
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(360.0, 700.0));
        let mut header_rect = None;
        for frame in 0..4 {
            let mut input = egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            };
            if frame == 3 {
                if let Some(r) = header_rect {
                    let pos: egui::Pos2 = r;
                    input.events.push(egui::Event::PointerMoved(pos));
                    input.events.push(egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::default(),
                    });
                    input.events.push(egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::default(),
                    });
                }
            }
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        "远端",
                        1,
                        PanelColumn::Remote,
                        &mut state,
                        false,
                        &[],
                        0,
                    );
                });
            });
            header_rect = annotate::spot_rect(&ctx, "文件面板/远端/列头/名称")
                .map(|r: egui::Rect| r.center());
        }
        assert_eq!(
            state.sort_dir,
            crate::files::SortDir::Desc,
            "点了「名称」列头,排序方向没翻 —— 点击没落到列头上"
        );
    }

    /// A 组:两栏方位是「本地在前、远端在后」—— 标签宿主左本地右远端,
    /// 侧栏上本地下远端。两个宿主用**同一条**规则。
    ///
    /// 判据用 annotate 记下的矩形,不看调用顺序 —— 调用顺序是实现细节,
    /// 位置才是用户看见的东西。
    ///
    /// 自证会变红:把 `content()` 里两次 `show()` 的 left/right 换回去。
    #[test]
    fn the_local_column_comes_first_in_both_hosts() {
        let ctx = egui::Context::default();
        // 取矩形的前提:标注模式必须开着,否则 `mark` 直接 return。
        annotate::toggle(&ctx);
        let t = crate::theme::MULLION_DARK;
        let mut frame = PanelFrame::default();

        // 标签宿主用宽窗口,否则无边界画布会让左右两栏的几何断言失真。
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0));
        for _ in 0..3 {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |ctx| {
                    content(ctx, &t, 7, true, &mut frame, 0);
                },
            );
        }
        let local = annotate::spot_rect(&ctx, "文件面板/本地").expect("本地栏没画");
        let remote = annotate::spot_rect(&ctx, "文件面板/远端").expect("远端栏没画");
        assert!(
            local.center().x < remote.center().x,
            "标签宿主:本地栏必须在左边(本地 x={} 远端 x={})",
            local.center().x,
            remote.center().x
        );

        let ctx2 = egui::Context::default();
        annotate::toggle(&ctx2);
        let mut ui_state = crate::ui::UiState::default();
        let mut frame2 = PanelFrame::default();
        // 侧栏用真实窗口尺寸(见 `clicking_the_name_header_...` 的同款注释)。
        let screen2 = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(360.0, 700.0));
        for _ in 0..3 {
            let _ = ctx2.run(
                egui::RawInput {
                    screen_rect: Some(screen2),
                    ..Default::default()
                },
                |ctx| {
                    sidebar(ctx, &t, &mut ui_state, 7, true, &mut frame2, 0);
                },
            );
        }
        let local2 = annotate::spot_rect(&ctx2, "文件面板/本地").expect("本地栏没画");
        let remote2 = annotate::spot_rect(&ctx2, "文件面板/远端").expect("远端栏没画");
        assert!(
            local2.center().y < remote2.center().y,
            "侧栏:本地栏必须在上面(本地 y={} 远端 y={})",
            local2.center().y,
            remote2.center().y
        );
        // 分母必须是侧栏**总高**,不能拿 `remote2.height()` 当参照系:「远端」
        // 那次 `show()` 没包 `allocate_ui` 裁剪,它内部 `annotate::mark` 记的是
        // 整条侧栏未裁剪的 `ui.max_rect()`(恒等于侧栏总高)——拿它跟本地栏比,
        // 无论 `sidebar()` 里的比例改成几比几,这条断言都恒真(复核实测把
        // `h * 0.4` 改成 `h * 0.9` 照样绿)。改成对总高的占比区间:守住
        // 0.4:0.6 这个刻意取舍(辅助视图里远端才是主体),留一点余量给
        // `ui.separator()` 那几像素。
        let total = annotate::spot_rect(&ctx2, "文件侧栏").expect("侧栏没画");
        assert!(
            local2.height() > total.height() * 0.25 && local2.height() < total.height() * 0.5,
            "侧栏:本地栏该占约四成高度(0.4:0.6 里的 0.4),实际本地 {} / 总高 {}",
            local2.height(),
            total.height()
        );
    }

    /// 递归找 `Shape::Vec` 里描边颜色匹配的矩形,返回**它所在那个顶层
    /// `ClippedShape` 的 `clip_rect`**(不是矩形自己的几何)—— 这才是决定
    /// 它实际会不会画出边界的东西:`max_rect` 只是布局预算,`clip_rect`
    /// 才是画的时候真正生效的裁剪范围(`egui-0.30.0` `painter.rs::add`,
    /// 每笔画都记着当时 `self.clip_rect`)。
    fn find_stroke_clip(
        shapes: &[egui::epaint::ClippedShape],
        color: egui::Color32,
    ) -> Option<egui::Rect> {
        fn walk(shape: &egui::Shape, clip: egui::Rect, color: egui::Color32) -> Option<egui::Rect> {
            match shape {
                egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, clip, color)),
                egui::Shape::Rect(r) if r.stroke.color == color && r.stroke.width > 0.0 => {
                    Some(clip)
                }
                _ => None,
            }
        }
        shapes
            .iter()
            .find_map(|cs| walk(&cs.shape, cs.clip_rect, color))
    }

    /// B1:两栏的 `ScrollArea` 必须是两个独立的滚动状态,且各自的视口不越界。
    ///
    /// **嫌疑 1(几何)查证结论:成立**。`content()` 用 `ui.scope_builder(
    /// UiBuilder::new().max_rect(left))` 摆内容,但 egui-0.30 的
    /// `Ui::new_child`(`ui.rs:263`)只是把父 ui 的 `painter`(带着它的
    /// `clip_rect`)原样 `clone()` 过去,`max_rect` 不影响 `clip_rect` 分毫。
    /// 实测(临时探针,已删除)证实:两栏内部任何一笔画记下的 `ClippedShape::
    /// clip_rect` 都是整个 `CentralPanel` 的裁剪范围(跨两栏共享),不是本栏
    /// 矩形。断言借用 `show()` 里已有的聚焦边框(`focused` 时画的
    /// `rect_stroke(ui.max_rect(), ...)`)—— 它的**几何**矩形本来就等于本栏
    /// 矩形(所以拿它当参照不会像 sidebar 那次一样恒真),但它的
    /// `clip_rect` 在没有显式裁剪时会横跨两栏。
    ///
    /// **嫌疑 2(id 撞车)查证结论:不成立(未复现)**。`scroll_id_salt` 的
    /// 最终 id 是 `ui.id().with(salt)`;两个 `scope_builder` 子 ui 的 `ui.id()`
    /// 因为都没显式给 `UiBuilder::id_salt`,退化成同一个 `Id::from("child")`
    /// 常量、算出同一个 `stable_id`(`ui.rs:265,289`,实测证实)——但
    /// `scroll_id_salt(id, generation)` 自己已经把 `"远端"`/`"本地"` 拼进了
    /// salt 字符串,`Id::new(不同字符串)` 本来就不同,`ui.id().with(不同salt)`
    /// 因此依然不同。两栏当前**没有**共享滚动状态。保留这条判据当回归
    /// 守护:它在这次查证里没红,但改回 `scroll_id_salt` 忽略 `id` 的那次
    /// 变异验证里会红(两个 `scope_builder` 的 `ui.id()` 本来就相同,盐再一样
    /// 就真的会撞)。
    ///
    /// 自证会变红(两条各自):
    /// 1. 删掉下面 `content()` 里新增的 `ui.set_clip_rect(...)` —— 裁剪矩形
    ///    断言变红;
    /// 2. `scroll_id_salt` 改成 `format!("files-{generation}")`(忽略 `id`)——
    ///    滚动独立性断言变红。
    #[test]
    fn the_two_columns_get_independent_non_overlapping_scroll_areas() {
        let t = crate::theme::MULLION_DARK;
        let accent = theme::c32(t.accent);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0));

        // --- 嫌疑 1:裁剪矩形不能越过本栏自己的边界。 -----------------
        let clip_within_own_bounds = |active: PanelColumn| -> (egui::Rect, egui::Rect) {
            let ctx = egui::Context::default();
            annotate::toggle(&ctx);
            let mut frame = PanelFrame {
                remote: PaneState::new(RemotePath::from_bytes(b"/".to_vec())),
                local: PaneState::new(RemotePath::from_bytes(b"/".to_vec())),
                bookmarks: Vec::new(),
                active_column: active,
            };
            frame.local.entries = vec![entry(b"local-a.txt", EntryKind::File)];
            frame.local.load = Load::Ready;
            frame.remote.entries = vec![entry(b"remote-a.txt", EntryKind::File)];
            frame.remote.load = Load::Ready;

            let mut out = None;
            for _ in 0..3 {
                out = Some(ctx.run(
                    egui::RawInput {
                        screen_rect: Some(screen),
                        ..Default::default()
                    },
                    |ctx| {
                        content(ctx, &t, 7, true, &mut frame, 0);
                    },
                ));
            }
            let out = out.unwrap();
            let path = match active {
                PanelColumn::Local => "文件面板/本地",
                PanelColumn::Remote => "文件面板/远端",
            };
            let geo = annotate::spot_rect(&ctx, path).expect("这一栏没画");
            let clip =
                find_stroke_clip(&out.shapes, accent).expect("聚焦边框没画出来(focused 没生效)");
            (geo, clip)
        };

        let (local_geo, local_clip) = clip_within_own_bounds(PanelColumn::Local);
        assert!(
            local_clip.max.x <= local_geo.max.x + 0.5,
            "本地栏的裁剪矩形越过了它自己的右边界,内容会画进右栏: \
             clip.max.x={} 本栏 max.x={}",
            local_clip.max.x,
            local_geo.max.x
        );

        let (remote_geo, remote_clip) = clip_within_own_bounds(PanelColumn::Remote);
        assert!(
            remote_clip.min.x >= remote_geo.min.x - 0.5,
            "远端栏的裁剪矩形越过了它自己的左边界,内容会画进本地栏: \
             clip.min.x={} 本栏 min.x={}",
            remote_clip.min.x,
            remote_geo.min.x
        );

        // --- 嫌疑 2:往左栏灌一次滚轮,右栏的行位置不该跟着挪。 --------
        let ctx = egui::Context::default();
        let mut frame = PanelFrame {
            remote: PaneState::new(RemotePath::from_bytes(b"/".to_vec())),
            local: PaneState::new(RemotePath::from_bytes(b"/".to_vec())),
            bookmarks: Vec::new(),
            active_column: PanelColumn::Local,
        };
        frame.local.entries = (0..200)
            .map(|i| entry(format!("local-{i}.txt").as_bytes(), EntryKind::File))
            .collect();
        frame.local.load = Load::Ready;
        frame.remote.entries = (0..200)
            .map(|i| entry(format!("remote-{i}.txt").as_bytes(), EntryKind::File))
            .collect();
        frame.remote.load = Load::Ready;

        let text_y = |shapes: &[egui::epaint::ClippedShape], needle: &str| -> f32 {
            find_text_pos(shapes, needle)
                .unwrap_or_else(|| panic!("没找到 {needle}"))
                .y
        };

        // 预热三帧,拿滚动前的基线位置。
        let mut out = None;
        for _ in 0..3 {
            out = Some(ctx.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |ctx| {
                    content(ctx, &t, 7, true, &mut frame, 0);
                },
            ));
        }
        let before = out.unwrap();
        let local_y_before = text_y(&before.shapes, "local-3.txt");
        let remote_y_before = text_y(&before.shapes, "remote-3.txt");

        // 指针悬在本地栏(左半边)上,灌一大股滚轮。
        let scroll_input = egui::RawInput {
            screen_rect: Some(screen),
            events: vec![
                egui::Event::PointerMoved(egui::pos2(50.0, 300.0)),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, -600.0),
                    modifiers: egui::Modifiers::default(),
                },
            ],
            ..Default::default()
        };
        let mut out = Some(ctx.run(scroll_input, |ctx| {
            content(ctx, &t, 7, true, &mut frame, 0);
        }));
        // 再跑两帧,让滚动状态稳定下来(smooth scroll 有插值)。
        for _ in 0..2 {
            out = Some(ctx.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |ctx| {
                    content(ctx, &t, 7, true, &mut frame, 0);
                },
            ));
        }
        let after = out.unwrap();

        assert_ne!(
            find_text_pos(&after.shapes, "local-3.txt").map(|p| p.y),
            Some(local_y_before),
            "本地栏灌了滚轮,`local-3.txt` 的位置却没变 —— 滚动没生效,测试前提不成立"
        );
        let remote_y_after = text_y(&after.shapes, "remote-3.txt");
        assert!(
            (remote_y_after - remote_y_before).abs() < 0.5,
            "只往本地栏灌了滚轮,远端栏的 `remote-3.txt` 却跟着挪了位置 \
             (滚动前 y={remote_y_before} 滚动后 y={remote_y_after})—— \
             两栏共用了同一份滚动状态"
        );
    }

    /// D3:名称列宽度必须**一处算**。`row()` 或 `header()` 里再写一遍这个
    /// 减法表达式的话,加一列就会有一处漏改,现象是列头文字和行内容错位——
    /// 而且错得很小(几个像素),没人会当成 bug 报上来,它就一直错着。
    ///
    /// 自证会变红:在 `row()` 里加一份内联的 `total - W_ICON - ICON_GAP - sum`
    /// 影子计算(哪怕值算对,这条测试也该红——它守的是「只有一处」,不是
    /// 「算得对不对」)。
    #[test]
    fn the_name_column_width_is_computed_in_exactly_one_place() {
        let src = include_str!("files_panel.rs");
        // 扫产品代码(第一个 #[cfg(test)] 之前),数一数减法表达式出现几次。
        let prod = src.split("#[cfg(test)]").next().expect("源码切歪了");
        let inline = prod.matches("- W_ICON - ICON_GAP - sum").count();
        assert_eq!(
            inline, 1,
            "名称列宽度算了 {inline} 次 —— 必须只在 `name_w()` 里算一次,\
             两处各算一遍会让列头和行错位"
        );
    }

    /// D2/控制者补充 2/协调者复核收口:列头与行体的**每一条**列边界都必须
    /// 重合,在 k=4/3/2/1/0 五个档位各取一个代表宽度(含 280/360/488/640
    /// 四个真实值:280/640 是侧栏拖拽范围两端,360 是默认侧栏宽,488 是
    /// 旧的「四列恒定宽」临界点)。这条测试守的是:列头文字与行内容错位
    /// 几个像素这件事,错位很小,没人会当成 bug 报上来,它会一直悄悄错着。
    ///
    /// 两侧都调**生产函数**——列头侧 `header_col_lefts()`/`header_name_col_w()`
    /// 是 `header()` 自己也调的那份计算,行体侧 `row_col_lefts()`/
    /// `row_size_col_left()` 是 `row()` 自己也调的那份计算,测试不独立重建
    /// 任何一侧的算式(同 `icon_rect()` 的教训:测试自己拼一份「我以为生产
    /// 代码是这么写的」公式的话,生产代码那份改错了,测试仍然只是在跟自己
    /// 的影子比,照样测不出来)。`header_col_lefts()`/`row_col_lefts()` 各自
    /// 独立累加(不共用最终结果),一边改错了另一边不会跟着错。
    ///
    /// 自证会变红(两条各自):
    /// 1. 把 `header_name_col_w()` 里的 `+ W_ICON + ICON_GAP` 去掉;
    /// 2. 把 `row_col_lefts()` 里某个列宽换成别的常量(比如把属主列的宽度
    ///    换成 `W_PERM`)。
    #[test]
    fn the_header_and_row_size_column_start_at_the_same_x_across_widths() {
        // (宽度, 期望的可见列数 k)——见 `visible_col_count()` 的分档:
        // 150→0(深度夹紧,连一列可选列都放不下),280→1(侧栏拖拽下限),
        // 360→2(默认侧栏宽),420→3(k=3 的代表宽度,落在 [396,488) 区间),
        // 488→4(旧的「四列恒定宽」临界点,恰好卡在地板上),640→4(侧栏
        // 拖拽上限,不夹紧,富余量最大)。
        for (w, expect_k) in [
            (150.0_f32, 0usize),
            (280.0, 1),
            (360.0, 2),
            (420.0, 3),
            (488.0, 4),
            (640.0, 4),
        ] {
            // 名称列本身的边界(列头/行体各自的「合并区域」右边界)。
            let header_name_left = header_name_col_w(w);
            let row_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, ROW_H));
            let row_name_left = row_size_col_left(row_rect);
            assert!(
                (header_name_left - row_name_left).abs() < 0.01,
                "宽度 {w} 下列头/行体「名称」列右边界不重合:列头 {header_name_left}, 行体 {row_name_left}"
            );

            let header_cols = header_col_lefts(w);
            let row_cols = row_col_lefts(row_rect);
            assert_eq!(
                header_cols.len(),
                expect_k,
                "宽度 {w} 下列头侧可见列数不对:期望 {expect_k},实际 {}",
                header_cols.len()
            );
            assert_eq!(
                header_cols.len(),
                row_cols.len(),
                "宽度 {w} 下列头/行体可见列数不一致:列头 {},行体 {}",
                header_cols.len(),
                row_cols.len()
            );
            for (h, r) in header_cols.iter().zip(row_cols.iter()) {
                let (h_label, h_key, h_left, h_w) = *h;
                let (_, r_key, r_left, r_w) = *r;
                assert_eq!(
                    h_key, r_key,
                    "宽度 {w} 下列头/行体列顺序不一致:列头 {h_label:?}({h_key:?}),行体 {r_key:?}"
                );
                assert!(
                    (h_left - r_left).abs() < 0.01,
                    "宽度 {w} 下「{h_label}」列左边界不重合:列头 {h_left}, 行体 {r_left}"
                );
                assert!(
                    (h_w - r_w).abs() < 0.01,
                    "宽度 {w} 下「{h_label}」列宽不一致:列头 {h_w}, 行体 {r_w}"
                );
            }
        }
    }

    /// D2/控制者补充 2:宽度不够时,大小/修改时间/权限/属主四列必须**从右
    /// 向左**收起(先丢属主,再权限,再修改时间,最后大小)。这是复核实测
    /// 出的真实画面对应的修法:默认侧栏 360px 下,不收起的话属主列有数据
    /// 没标题(标题起点 396 被列头裁掉了),权限标题反而画到了权限数据
    /// 右边——收起可选列、让名称列吃掉剩余宽度之后,这个错位从根上消失
    /// (见 `visible_col_count()`/`name_w()` 的文档注释)。
    ///
    /// 自证会变红:
    /// 1. 把可见列数的判据 `< 80.0` 改成 `< 0.0`(等价于永不收起);
    /// 2. 把 `OPTIONAL_COLS` 的收起顺序反过来(从左往右丢)。
    #[test]
    fn columns_are_dropped_from_the_right_as_the_panel_gets_narrower() {
        assert_eq!(visible_col_count(640.0), 4, "640px(侧栏上限)应该四列全在");
        assert_eq!(visible_col_count(488.0), 4, "488px(旧临界点)应该四列全在");
        assert_eq!(visible_col_count(360.0), 2, "360px(默认侧栏宽)应该只剩两列");
        assert_eq!(visible_col_count(280.0), 1, "280px(侧栏下限)应该只剩一列");

        // 420px 落在 k=3 的区间([396,488))——可见的应该是「大小/修改时间/
        // 权限」,收起的应该是最右边的「属主」,不是别的列。
        let keys: Vec<SortKey> = header_col_lefts(420.0)
            .iter()
            .map(|&(_, key, _, _)| key)
            .collect();
        assert_eq!(
            keys,
            vec![SortKey::Size, SortKey::Mtime, SortKey::Perm],
            "420px 下理应可见「大小/修改时间/权限」三列、收起「属主」——实际可见: {keys:?}"
        );
    }

    /// D2:本地栏的属主列画 `—`,不画 `0:0`。判据是栏别不是 uid ——
    /// 远端真的有 root(uid=0)拥有的文件。
    ///
    /// 自证会变红:把判据从 `column == PanelColumn::Local` 改成 `e.uid == 0`,
    /// 然后这条测试里那个远端 root 文件会画成 `—`。
    #[test]
    fn the_local_column_shows_a_dash_for_owner_but_remote_root_shows_zeros() {
        assert_eq!(owner_text(PanelColumn::Local, 0, 0), "—");
        assert_eq!(owner_text(PanelColumn::Remote, 0, 0), "0:0");
        assert_eq!(owner_text(PanelColumn::Remote, 1000, 1000), "1000:1000");
    }

    /// 质量复核实测:把 `row()` 里权限槽(`cols.get(2)`)与属主槽
    /// (`cols.get(3)`)绘制的内容对调,`files_panel` 全部 33 个测试全绿——
    /// 对齐守护测试(`the_header_and_row_size_column_start_at_the_same_x_
    /// across_widths`)只比几何(label/key/left/width),完全不看「谁画在
    /// 哪个槽位」。用户会在「权限」标题下看到 `uid:gid`、在「属主」标题下
    /// 看到 `rwxr-x---`,测试网毫无反应。
    ///
    /// 这条测试直接读渲染出的 `Shape::Text`,断言每个槽位画出来的数据
    /// 文本落在**自己**那个槽位的 x 区间里。顺带把大小、修改时间两个槽位
    /// 也一并锁上——同一类判据,x 区间同样来自 `row_col_lefts()`,不算
    /// 超范围。
    ///
    /// entry 特意挑了四个槽位互不相同、不会看混的值:大小 `777 B`、权限
    /// `rwxr-x---`、属主 `1000:33`,彼此没有子串重叠,`find_text_pos` 不会
    /// 找混。
    ///
    /// x 区间来自生产函数 `row_col_lefts(row_rect)`——`row()` 自己也调的
    /// 那份计算,测试不重抄坐标(同 `icon_rect()`/`row_size_col_left()` 的
    /// 教训)。
    ///
    /// 自证会变红:把 `row()` 里权限槽与属主槽两处绘制的内容对调
    /// (`perm_string(e.mode)` 画进属主槽的 `p.text(...)`、
    /// `owner_text(...)` 画进权限槽的 `p.text(...)`)。
    #[test]
    fn each_optional_column_paints_its_own_data_not_a_neighbors() {
        let t = crate::theme::MULLION_DARK;
        let e = Entry {
            size: 777,
            mode: 0o750,
            uid: 1000,
            gid: 33,
            ..entry(b"f.txt", EntryKind::File)
        };
        // 700px:留足余量的 k=4 代表宽度,四个可选列全部可见。
        let width = 700.0_f32;
        let ctx = egui::Context::default();
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default()
                .frame(egui::Frame::none())
                .show(ctx, |ui| {
                    // 显式给 `row()` 一块零边距、宽度已知的矩形——不然
                    // `row_rect` 的实际宽度取决于 egui 默认 `screen_rect` /
                    // `CentralPanel` 默认边距,测试里没法跟 `row_col_lefts()`
                    // 对上同一个数。手法同生产代码 `content()` 切两栏
                    // (`ui.scope_builder(UiBuilder::new().max_rect(..))`)。
                    let rect =
                        egui::Rect::from_min_size(ui.max_rect().min, egui::vec2(width, ROW_H));
                    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                        row(ui, &t, &e, PanelColumn::Remote, false);
                    });
                });
        });

        let row_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, ROW_H));
        let cols = row_col_lefts(row_rect);
        assert_eq!(cols.len(), 4, "700px 应该四列全部可见,自查前提不成立");

        let expect = [
            (human_size(e.size), cols[0]),
            (mtime_text(e.mtime), cols[1]),
            (perm_string(e.mode), cols[2]),
            (owner_text(PanelColumn::Remote, e.uid, e.gid), cols[3]),
        ];
        for (text, (label, _key, left, w)) in expect {
            let pos = find_text_pos(&out.shapes, &text)
                .unwrap_or_else(|| panic!("没画出「{label}」列该有的文本 {text:?}"));
            assert!(
                pos.x >= left && pos.x <= left + w,
                "「{label}」列的文本 {text:?} 落在 x={},不在自己的区间 [{left},{}] 里\
                 ——画错槽位了",
                pos.x,
                left + w
            );
        }
    }
}
