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
    /// F139:把当前目录收进书签。`name` 由 UI 按路径末段算好 —— app 侧只管
    /// 落盘,不重复一遍命名规则。
    BookmarkAdd { path: String, name: String },
    /// F139:取消收藏。按 `path` 相等匹配 —— 书签的身份就是路径(重名允许,
    /// 同路径不允许重复收藏)。
    BookmarkRemove { path: String },
}

/// F139:画书签相关控件要的两样东西。
///
/// 合成一个结构体而不是给 `show` 再加一个参数:两者永远一起出现,
/// 而且分开传很容易在本地栏那个调用点只改一个(「列表给空、可编辑忘了关」
/// 会画出一个点得动但存不进任何地方的 ☆)。
#[derive(Clone, Copy)]
pub struct BookmarkView<'a> {
    /// 该会话已配的书签。
    pub list: &'a [mullion_store::Bookmark],
    /// 这个标签绑着一条会话记录(有 `SessionId`),收藏才有地方落盘。
    pub can_edit: bool,
}

impl BookmarkView<'_> {
    /// 本地栏用:没有书签,也不能收藏(本地目录收藏不在 F139 范围内)。
    pub fn none() -> Self {
        Self {
            list: &[],
            can_edit: false,
        }
    }
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

/// 列宽的默认值。用户可以拖(F135),拖出来的值放在 `ColWidths` 里;
/// 这几个常量只负责给出「第一次打开时长什么样」。
const W_SIZE: f32 = 78.0;
const W_MTIME: f32 = 132.0;
const W_PERM: f32 = 86.0;
/// 属主列(`用户名:组名`)。
///
/// D2 原本定的是 `uid:gid` 加 92pt —— 理由是「名字在 SFTP 协议层拿不到,
/// 不为此去 exec」(设计 D21)。**F142 推翻了 D21 的后半句**:用户要看名字,
/// 于是列完目录后按需批量问一次 `getent`(`files::owners`)。前半句仍然成立,
/// 这也正是必须额外跑一条命令的原因。
///
/// 宽度随之从 92 放到 120:`deploy:docker` 这种两段名字在 92pt 下会被截成
/// `deploy:doc…`,而属主列**截了就等于没有**(看不出是谁)。用户仍可拖
/// (F135)。
const W_OWNER: f32 = 120.0;
const ROW_H: f32 = 22.0;
/// F150:选中行左侧色条的宽度(逻辑点)。
const SEL_BAR_W: f32 = 2.0;
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

/// 五列的当前宽度(point)。**名称列的宽度含图标格** —— 沿用列头
/// 「图标 + 名称 = 一个合并区域」的既有语义,不额外记一份图标宽。
///
/// 放 `ui::UiState`(全局一份,远端/本地/所有标签共用),**不落盘**:
/// 拖列宽是个随手调整,不值得为它动 store schema(设计 D2)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColWidths {
    pub name: f32,
    pub size: f32,
    pub mtime: f32,
    pub perm: f32,
    pub owner: f32,
}

impl Default for ColWidths {
    /// 名称列 220:比旧模型在默认侧栏(360px)下算出来的 130 宽不少,
    /// 代价是一打开就是横向滚动状态 —— 这是设计 D1 明确接受的取舍
    /// (五列恒在 > 一眼看全)。不满意改这一行就行。
    fn default() -> Self {
        Self {
            name: 220.0,
            size: W_SIZE,
            mtime: W_MTIME,
            perm: W_PERM,
            owner: W_OWNER,
        }
    }
}

/// 列序号 → 最小宽度。名称列要放得下图标格 + 几个字;其余列至少要能
/// 放下被截断后的标题(一个字 + 省略号)。
fn col_min(i: usize) -> f32 {
    if i == 0 {
        80.0
    } else {
        48.0
    }
}

/// 列宽上限。拖到几千 px 不会崩,但滚动条会退化成一条几乎抓不住的细线。
const COL_MAX: f32 = 800.0;

/// 列序号 → 可变引用。拖拽热区按序号改宽度,`match` 只写这一份 ——
/// 散成五处 `if i == 0 { .. } else if ..` 的话,加一列必漏一处。
fn col_w_mut(c: &mut ColWidths, i: usize) -> &mut f32 {
    match i {
        0 => &mut c.name,
        1 => &mut c.size,
        2 => &mut c.mtime,
        3 => &mut c.perm,
        _ => &mut c.owner,
    }
}

/// **列布局的唯一真值来源**:`(标签, SortKey, 左边界, 宽度)`,
/// 左边界从 0 起算(相对行/列头的左边界)。
///
/// 列头(`header_at`)和行体(`row`)调的是同一份 —— 旧模型里两边各自
/// 累加、靠一条对齐测试守着不许分家,现在坐标同源,分家在物理上不可能
/// 发生。**不许在别处再写一遍这个累加。**
///
/// F146:**本地栏没有属主列**,所以列数按栏走(远端 5、本地 4)。
/// `files/local.rs` 构造 `Entry` 时 uid/gid 恒填 0,那一列在数据源头上
/// 就不存在。判据按栏静态、不按数据 —— 理由见
/// `tests::the_local_column_has_no_owner_column_but_the_remote_one_does`。
fn col_lefts(c: &ColWidths, column: PanelColumn) -> Vec<(&'static str, SortKey, f32, f32)> {
    let mut specs: Vec<(&'static str, SortKey, f32)> = vec![
        ("名称", SortKey::Name, c.name),
        ("大小", SortKey::Size, c.size),
        ("修改时间", SortKey::Mtime, c.mtime),
        ("权限", SortKey::Perm, c.perm),
    ];
    if column == PanelColumn::Remote {
        specs.push(("属主", SortKey::Owner, c.owner));
    }
    let mut out = Vec::with_capacity(specs.len());
    let mut x = 0.0;
    for (label, key, w) in specs {
        out.push((label, key, x, w));
        x += w;
    }
    out
}

/// 内容总宽 = 各列之和。视口比它窄就出横向滚动条(F136)。
///
/// 必须跟 `col_lefts` 走同一份列表 —— 各写一遍的话本地栏会多出一整列宽
/// 的空白可滚区域,横向滚动条比内容还长。
fn content_w(c: &ColWidths, column: PanelColumn) -> f32 {
    col_lefts(c, column).iter().map(|(_, _, _, w)| w).sum()
}

/// 路径条只读态那块的 id。**必须是稳定的** ——「点得中路径条」是 F131 唯一
/// 的入口,而 egui 自动分配的 id 在测试里只能靠猜坐标。按栏分:两栏各一条。
fn path_label_id(id: &str) -> egui::Id {
    egui::Id::new(("files-path-label", id))
}

/// 编辑框自己的 id。同上。
fn path_edit_id(id: &str) -> egui::Id {
    egui::Id::new(("files-path-edit", id))
}

/// F131:退出编辑态。`commit` = 这一下是回车(要跳),否则是 Esc / 失焦
/// (丢弃)。返回要发出去的动作。
///
/// **默认丢弃**:失焦最常见的原因是用户去点了别处,那不是「确认」。
fn finish_path_edit(state: &mut PaneState, commit: bool) -> Option<FileAction> {
    let buf = state.path_edit.take()?;
    commit.then_some(FileAction::GotoInput(buf))
}

/// F139:新书签的默认名 = 路径末段。
///
/// 根目录没有末段,回退成 `/` —— 空名字虽然 store 允许(界面会回退显示
/// 整条路径),但那会让下拉里冒出一条 `/`……的长路径,不如直接给个 `/`。
fn bookmark_default_name(path: &str) -> String {
    match path.trim_end_matches('/').rsplit('/').next() {
        Some(seg) if !seg.is_empty() => seg.to_owned(),
        _ => "/".to_owned(),
    }
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
    bookmarks: BookmarkView<'_>,
    drop_in: usize,
    cols: &mut ColWidths,
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
    // F151:本栏正被拖 —— 在指针旁画一个跟随的小胶囊。
    //
    // 判据是「载荷来自**本栏**」:两栏都会走到这里,不区分的话同一次拖拽
    // 会被画两遍(两个胶囊叠在一起,边缘毛糙)。
    //
    // 画在 `Order::Tooltip` 层:那是 egui 里唯一保证压在所有 panel 之上的
    // 常规层,画在当前 `ui` 的 painter 上会被另一栏的背景盖掉。
    let outgoing = egui::DragAndDrop::payload::<crate::files::drag::DragFrom>(ui.ctx())
        .is_some_and(|f| f.0 == column);
    if outgoing {
        if let Some(p) = ui.ctx().pointer_latest_pos() {
            let first = state
                .selected_paths()
                .first()
                .map(|n| n.display().into_owned())
                .unwrap_or_default();
            let label = crate::files::drag::preview_label(state.selected.len(), &first);
            let painter = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Tooltip,
                egui::Id::new(("files-drag-preview", id)),
            ));
            let font = egui::FontId::proportional(12.0);
            let galley = painter.layout_no_wrap(label, font, theme::c32(t.accent_fg));
            // 偏移一点,别让胶囊压在指针尖底下(挡住落点行的高亮)。
            let at = p + egui::vec2(crate::ui::metrics::SP_M, crate::ui::metrics::SP_M);
            let pad = egui::vec2(crate::ui::metrics::SP_S, crate::ui::metrics::SP_XS);
            let bg = egui::Rect::from_min_size(at, galley.size() + pad * 2.0);
            painter.rect_filled(bg, 4.0, theme::c32(t.accent));
            painter.galley(at + pad, galley, theme::c32(t.accent_fg));
        }
    }
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
        // F143:原先写的 U+27F3 不在 GBK,微软雅黑与 egui 内置字体两边都
        // 没有 —— 画出来是豆腐块。自绘不受字体覆盖面影响。
        if crate::ui::icon::icon_button(ui, crate::ui::icon::Glyph::Refresh, true, "刷新(F5)") {
            action = Some(FileAction::Refresh);
        }
        let path = state.cwd.display().to_string();
        // F139:收藏当前目录 + 书签下拉。**必须画在路径标签之前** —— 下面
        // 那个 `Label` 用 `available_width` 吃掉整行剩余宽度,排在它后面的
        // 按钮会被挤出可视区。
        //
        // ★/☆ 由「当前 cwd 在不在书签列表里」现算:列表就是唯一真值,
        // 不另存一个会跟它不同步的标志位。
        let starred = bookmarks.list.iter().any(|b| b.path == path);
        ui.add_enabled_ui(bookmarks.can_edit, |ui| {
            let hit = ui
                .small_button(if starred { "★" } else { "☆" })
                .on_hover_text(if starred {
                    "取消收藏这个目录"
                } else {
                    "收藏这个目录"
                })
                // 置灰时 egui 不显示普通 tooltip,得用这一个 —— 一个点不动
                // 又不说为什么的按钮比没有更糟。
                .on_disabled_hover_text("这个标签不来自已保存的会话,书签无处存放");
            if hit.clicked() {
                action = Some(if starred {
                    FileAction::BookmarkRemove { path: path.clone() }
                } else {
                    FileAction::BookmarkAdd {
                        name: bookmark_default_name(&path),
                        path: path.clone(),
                    }
                });
            }
        });
        ui.add_enabled_ui(!bookmarks.list.is_empty(), |ui| {
            // F143:原先写的 U+25BE 不在 GBK,是豆腐块(用户 v0.1.56 实测
            // 报的就是这一个)。`menu_button` 只收文本,换不了自绘 —— 改用
            // `menu_custom_button` 传一个空文本按钮,再把三角画进它的 rect。
            let btn =
                egui::Button::new("").min_size(egui::Vec2::splat(ui.spacing().interact_size.y));
            let menu = egui::menu::menu_custom_button(ui, btn, |ui| {
                for b in bookmarks.list {
                    // F145:主文本恒是**完整绝对路径**。用户点开这个下拉
                    // 就是为了确认「这条书签指哪儿」,只给个 `nginx` 等于
                    // 没回答 —— 同名目录在不同机器、不同层级下遍地都是。
                    //
                    // 用户自己起的名字不丢:非空且与路径不同时挂到 hover 上。
                    // (空名是 store 明确允许的合法状态,见 `Bookmark::name`
                    // 的文档 —— 现在这个分支不再影响主文本,只影响 hover。)
                    let mut item = ui.button(b.path.as_str());
                    if !b.name.is_empty() && b.name != b.path {
                        item = item.on_hover_text(&b.name);
                    }
                    if item.clicked() {
                        action = Some(FileAction::Goto(mullion_ssh::sftp::RemotePath::from_bytes(
                            b.path.as_bytes().to_vec(),
                        )));
                        ui.close_menu();
                    }
                }
            });
            let resp = menu.response;
            // 按钮体是空的,三角自己画上去。颜色走 `interact()` 取当前交互
            // 态;禁用时不用另外压暗 —— `add_enabled_ui` 已经给这个 `Ui` 的
            // painter 挂了 fade,画上去自动跟着淡。
            if ui.is_rect_visible(resp.rect) {
                let fg = ui.style().interact(&resp).fg_stroke.color;
                ui.painter().extend(crate::ui::icon::shapes(
                    resp.rect,
                    crate::ui::icon::Glyph::TriangleDown,
                    egui::Stroke::new(1.5, fg),
                ));
            }
            // 按钮体没有文字了,标注模式下靠文本认不出它是什么;显式登记一
            // 个候选。测试也拿它定位(原先是 `find_text_pos("▾")`)。
            annotate::mark(ui.ctx(), format!("文件面板/{id}/路径/书签"), resp.rect);
            resp.on_hover_text("收藏的路径")
                .on_disabled_hover_text("还没有收藏任何路径");
        });
        annotate::mark(ui.ctx(), format!("文件面板/{id}/路径"), ui.max_rect());
        match state.path_edit.as_mut() {
            // 编辑态。**注意它能收到键盘全靠 `Modal::FilesPathEdit`**(下一个
            // 任务接)—— 面板拿着键盘焦点时,键根本不喂给 egui
            // (`input_route::egui_should_see_focused`,T8 的注入点),不算
            // 模态的话这个框里一个字都打不出来(同 `Modal::Editor` 的坑)。
            Some(buf) => {
                let resp = ui.add(
                    egui::TextEdit::singleline(buf)
                        .id(path_edit_id(id))
                        .desired_width(ui.available_width()),
                );
                // **不在这里 `request_focus()`**——焦点只在「进入编辑态那一刻」
                // (下面 `None` 分支的点击处)请求一次。质量复核实测过:改成
                // 无条件每帧 `request_focus()`,两栏各自发现自己没焦点就抢
                // 回去,先进编辑态的那栏永远 `lost_focus()` 不了,退不出来。
                //
                // 收口条件是 `lost_focus() || clicked_elsewhere()`:光凭
                // `lost_focus()` 只覆盖 Esc/Tab/被别的控件抢焦点——复核实测
                // 点击文件行这类普通控件并不会让 `TextEdit` 失焦,不加
                // `clicked_elsewhere()` 的话「点别处取消编辑」根本不成立。
                // `clicked_elsewhere()` 触发时当帧没有 Enter 事件,`commit`
                // 自然是 `false`,不会把这一下误判成提交。
                if resp.lost_focus() || resp.clicked_elsewhere() {
                    let commit = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if let Some(a) = finish_path_edit(state, commit) {
                        action = Some(a);
                    }
                }
            }
            None => {
                // 命中区域用**整行剩余宽度**,不能用 `label.rect`——`Label`
                // 按文字实际宽度分配,复核实测 `cwd == "/"` 时那个 rect 只有
                // 4px 宽,路径条右侧一大片空白点了没反应(而这是这个功能
                // 唯一的入口)。`available_rect_before_wrap()` 在加 `Label`
                // 之前取,拿到的是这一行到行尾的整块剩余区域;`ui.interact`
                // 不会因为传了更大的 rect 而重新分配布局,`Label` 自己的
                // `.truncate()` 视觉效果不受影响。
                let row_rect = ui.available_rect_before_wrap();
                ui.add(
                    egui::Label::new(egui::RichText::new(path.clone()).color(theme::c32(t.fg_mid)))
                        .truncate(),
                );
                let hit = ui.interact(row_rect, path_label_id(id), egui::Sense::click());
                if hit.clicked() {
                    state.path_edit = Some(path);
                    ui.ctx().memory_mut(|m| m.request_focus(path_edit_id(id)));
                }
            }
        }
    });

    // F139:原来这里有一条横排书签栏(`if !bookmarks.is_empty()` + 一排
    // `small_button`)。已去掉 —— 它只在该会话已经配过书签时才出现,用户
    // 根本不知道它存在,还白占一整行高度。书签改走路径条上的 ▾ 下拉。

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

    // F136:先占住一条横带留给列头(在 `ScrollArea` 之前分配,行体才会从
    // 它下面开始排),实际的绘制挪到滚动区**之后**做,好让列头在 z 序上
    // 压在行体上面(与原设计一致)。真正决定列头位置的偏移量另外算——
    // 见下方 `header_offset_x` 那条长注释,**不是**从这里的滚动区输出里
    // 现取(那份值下一帧才轮到行体去用,读它会让列头比行体多抢一步)。
    // F150:栏底状态行要占一行,先从可用高度里扣掉,喂给下面 `ScrollArea` 的
    // `.max_height(..)`。**必须在 ScrollArea 之前算**。`max(ROW_H)` 兜住面板
    // 被拖到极窄时的负数。
    //
    // 这个约束管的是**`ScrollArea` 自己占多高**,不是状态行会不会被挤没
    // (状态行真正的位置兜底见下面画状态行那段注释,靠的是「画在 `header_at`
    // 的 `scope_builder` 之前」,不是这里)。没有这一行,`ScrollArea` 开着
    // `auto_shrink([false, false])` 会把可用高度全部吃满——即使目录只有两三
    // 条,底下也会拖出一大片仍然可滚动、可交互的空白区域,吞掉本该落到别处
    // 的点击与滚轮;插桩实测还发现更直接的后果:`ScrollArea` 结束时把 `ui`
    // 的布局光标推到一个离谱的 y 坐标,紧跟着画的状态行会整个从渲染输出里
    // 消失(不是「位置偏了」,是「这个部件没了」)。
    // 守护:`tests::the_status_row_only_renders_when_the_scroll_area_height_is_capped`。
    let body_h = (ui.available_height() - ROW_H * 2.0).max(ROW_H);

    let (header_band, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ROW_H),
        egui::Sense::hover(),
    );

    let rows = state.rows();
    // `rows` 借着 `&state.entries` 不放(它是 `Vec<&Entry>`),闭包里不能再
    // 借一次 `&mut state`——新选中的那条先记局部变量,出了闭包再落回 `state`。
    let selected = state.selected.clone();
    // F142:属主名字表。跟 `rows` 一样是不可变借用,两者共存没问题;
    // **不 clone** —— 一份表可能有上百个条目,每帧复制一遍纯属浪费。
    let owners = &state.owners;
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
    let total_w = content_w(cols, column);
    // F136:列头要用的偏移是「这一帧的行体实际拿去排版的那份」,不是
    // 「这一帧滚动结束后要存给下一帧用的那份」——egui 的 `ScrollArea` 内部
    // 在 `begin()` 时就用 persisted 偏移把行体的屏幕坐标定死了,之后处理
    // 本帧滚轮事件才更新偏移、存回去给下一帧用(`ScrollAreaOutput` 里
    // 拿到的正是这个「已更新、留给下一帧」的值)。如果列头读的是
    // `show_rows()` 返回的 `state.offset.x`,在滚轮还没被(可能跨多帧)
    // 平滑消化完之前,列头会比行体本身多走一步——不是「滞后」,是「抢先」,
    // 肉眼同样是错位。这里在调用 `show_rows` 之前,用同一个持久化 id 把
    // 上一帧存的偏移原样读出来,和行体这一帧实际用的值保证逐帧位元对位。
    let header_offset_x = egui::scroll_area::State::load(
        ui.ctx(),
        ui.make_persistent_id(egui::Id::new(scroll_id_salt(id, generation))),
    )
    .unwrap_or_default()
    .offset
    .x;
    egui::ScrollArea::both()
        .id_salt(scroll_id_salt(id, generation))
        .max_height(body_h)
        // F58:**必须关掉**。`drag_to_scroll` 默认开着,它在视口上注册一个
        // 吃 drag 的部件,把按在行上的那一下抢去当滚动手势 —— 行的
        // `drag_started()` 永远为假,拖拽整个功能安静地不存在。桌面端本来
        // 也没人用鼠标拖内容滚动(滚轮 + 滚动条都在),关掉不损失什么。
        .drag_to_scroll(false)
        .auto_shrink([false, false])
        .show_rows(ui, ROW_H, rows.len(), |ui, range| {
            // F136:**必须显式要这个宽度**。egui 0.30 的 `show_rows` 只
            // `set_height`,宽度全看内容自己撑 —— 空目录(range 为空)时一行
            // 都不画,`content_size.x` 恒等于视口宽,水平滚动条不出现,
            // 右边那几列的列头就永远滚不到。
            ui.set_min_width(total_w);
            for ix in range {
                let e = rows[ix];
                let resp = row(ui, t, e, column, selected.contains(&e.name), cols, owners);
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
    if let Some(name) = drag_start {
        state.select_only(&name);
    }
    if let Some((name, ctrl, shift)) = clicked {
        state.click_row(&name, ctrl, shift);
    }
    // F150:栏底状态行。**必须画在 `click_row` 之后、`header_at` 的
    // `scope_builder` 之前**:
    // - 画在 `click_row` 之后 —— 点击的效果要在同一帧就反映到这行字上,
    //   否则用户点一下看到的还是上一帧的数,像是没生效。
    // - 必须画在 `header_at` 之前 —— `header_at` 用
    //   `ui.scope_builder(UiBuilder::new().max_rect(header_band), ..)` 补画
    //   列头,`scope_builder` 收尾会调 `advance_cursor_after_rect`,把 `ui`
    //   的布局光标**硬重置**到列头那个子 ui 的 `min_rect`(而 `header_at`
    //   内部全是 `ui.interact`/`ui.painter()` 直接画,没有一次真正的部件分
    //   配,子 ui 的 `min_rect` 因此停在列头带顶部那个零尺寸的种子点)。
    //   于是排在 `header_at` 之后的部件,光标已经被拽回列头顶部附近,画出
    //   来的东西会贴着列头、盖在它上面,而不是接在 `ScrollArea` 内容之后
    //   ——这一点插桩实测过,状态行排在 `header_at` 之后时,不管
    //   `ScrollArea` 实际占多高,状态行的 y 坐标恒定卡在列头正下方几像素。
    //   排在这里、`ScrollArea` 刚结束、`header_at` 还没跑的位置,才用得上
    //   `ScrollArea` 自己推进的光标(见上面 `body_h` 那条注释)。
    //   守护:`tests::the_status_row_is_drawn_below_the_last_row_not_under_the_header`。
    ui.add_space(crate::ui::metrics::SP_XS);
    ui.colored_label(theme::c32(t.fg_dim), state.status_text());
    // F136:把列头补画在上面占住的横带里,用的是调用 `show_rows` 之前
    // 读到的 `header_offset_x`(见上方长注释)。**必须排在状态行之后**——
    // 见上面状态行注释,这个 `scope_builder` 会把 `ui` 的布局光标重置到列
    // 头附近,排在它之前的部件才吃得到 `ScrollArea` 真实占用的高度。
    ui.scope_builder(egui::UiBuilder::new().max_rect(header_band), |ui| {
        // **必须显式裁剪**:子 ui 的 clip_rect 默认原样继承父 painter,
        // 不裁的话列宽之和超过视口时,右边几列的标题会画到隔壁栏
        // (同 `content()` 里两栏那两处 `set_clip_rect`)。
        ui.set_clip_rect(header_band);
        header_at(ui, t, id, column, state, cols, header_band, header_offset_x);
    });
    // F58:落在空白处(行与行之间、列头下方的空白)。**必须排在行之后** ——
    // `dnd_release_payload` 会把载荷取走,背景先问的话落在目录行上的那一下
    // 会被背景吃掉,「传进子目录」永远走不到。
    if landing.is_none() {
        if let Some(from) = bg.dnd_release_payload::<crate::files::drag::DragFrom>() {
            landing = crate::files::drag::drop_target(from.0, column, None);
        }
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

/// 把列头画在 `band` 里,整体左移 `offset_x`。
///
/// `offset_x` 是横向滚动偏移(F136,`show()` 传来的 `header_offset_x`——
/// 调 `show_rows` **之前**从持久化 `ScrollArea` 状态里读到的那份,和行体
/// 这一帧实际排版用的是同一个值,见 `show()` 里那条长注释)。拆出这个
/// 函数是因为**列头必须在占位横带确定之后才画得出正确的位置**(见设计
/// §③),而占位又必须在 `ScrollArea` 之前 —— 占位与绘制天然要分成两处。
#[allow(clippy::too_many_arguments)] // 同 `show`:一帧要画的东西天然多
fn header_at(
    ui: &mut Ui,
    t: &Theme,
    id: &str,
    column: PanelColumn,
    state: &mut PaneState,
    cols: &mut ColWidths,
    band: egui::Rect,
    offset_x: f32,
) {
    annotate::mark(ui.ctx(), format!("文件面板/{id}/列头"), band);
    // F135:列宽拖拽热区。**必须先于列体注册**:egui 同层内先注册的部件
    // 拿到命中权,挂在后面的话边界那 6pt 上的按下会被排序点击吃掉。
    //
    // 热区认的是**每一列的右边界**(i = 0..5),拖动只改第 i 列的宽度,
    // 右边的列整体平移(不做此消彼长的「借宽度」—— 总宽有横向滚动兜着,
    // 没有守恒的必要)。
    const HANDLE_W: f32 = 6.0;
    for (i, (_, _, left, w)) in col_lefts(cols, column).into_iter().enumerate() {
        let x = band.left() + left + w - offset_x;
        let handle = egui::Rect::from_min_max(
            egui::pos2(x - HANDLE_W * 0.5, band.top()),
            egui::pos2(x + HANDLE_W * 0.5, band.bottom()),
        );
        let resp = ui
            .interact(
                handle,
                ui.id().with(("files-col-resize", id, i)),
                egui::Sense::drag(),
            )
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        if resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            let target = col_w_mut(cols, i);
            *target = (*target + resp.drag_delta().x).clamp(col_min(i), COL_MAX);
        }
        if resp.hovered() || resp.dragged() {
            // 抓住的是哪条线,要看得见。
            ui.painter().with_clip_rect(band).vline(
                x,
                band.y_range(),
                egui::Stroke::new(1.0, theme::c32(t.accent)),
            );
        }
    }
    let mut hit = None;
    for (i, (label, key, left, w)) in col_lefts(cols, column).into_iter().enumerate() {
        let rect = egui::Rect::from_min_size(
            egui::pos2(band.left() + left - offset_x, band.top()),
            egui::vec2(w, ROW_H),
        );
        // 逐列登记:整行那一处标不了「往名称列点」这种精确目标
        // (F100 标注模式与点击测试都靠它)。
        //
        // 登记前先跟 `band` 求交,**不能直接登记未裁剪的 `rect`**:横滚之后
        // 列会整个或部分移出可视区(`rect.left()` 甚至可能是负数),原样登记
        // 的话 F100 标注模式会报出一个屏幕上已经看不见、点不到的「候选」。
        // 完全滚出去的列交出来是非正矩形,`annotate::mark` 自己会因为
        // `is_positive()` 为假而不登记 —— 不用在这里另写一次判断。
        annotate::mark(
            ui.ctx(),
            format!("文件面板/{id}/列头/{label}"),
            rect.intersect(band),
        );
        // id 必须显式给:不再走 `allocate_exact_size` 的自动分配,而两栏
        // 的列头在同一个 `Context` 里同名,不掺 `id`(远端/本地)和列序号
        // 会撞成同一个部件。
        let resp = ui.interact(
            rect,
            ui.id().with(("files-col-head", id, i)),
            egui::Sense::click(),
        );
        let mark = if state.sort_key == key {
            match state.sort_dir {
                crate::files::SortDir::Asc => "▲",
                crate::files::SortDir::Desc => "▼",
            }
        } else {
            ""
        };
        // 裁到横带内:列宽之和超过视口时,右边那几列的标题不能画到
        // 隔壁栏去(同 `content()` 里两栏各自 `set_clip_rect` 的理由)。
        let font = egui::FontId::proportional(11.0);
        let painter = ui.painter().with_clip_rect(band);
        let measure = |s: &str| {
            painter
                .layout_no_wrap(s.to_owned(), font.clone(), egui::Color32::WHITE)
                .size()
                .x
        };
        // F147:排序标识画在**列尾**,不是跟在标题屁股后面。
        //
        // 跟在后面那一版有个必然的坏结果:标题与标识拼成一串一起送去截断,
        // 列一窄先被 `Elide::End` 吃掉的就是末尾的标识 —— 而列窄恰恰是最
        // 需要看见「按哪列排的」的时候。这里反过来:**先给标识留出预算**,
        // 标题在剩下的宽度里截断。标识永远画得出来,标题该截就截。
        let pad = crate::ui::metrics::SP_XS;
        let mark_w = if mark.is_empty() {
            0.0
        } else {
            measure(mark) + pad
        };
        painter.text(
            rect.left_center() + egui::vec2(pad, 0.0),
            egui::Align2::LEFT_CENTER,
            elide(label, w - pad * 2.0 - mark_w, Elide::End, measure),
            font.clone(),
            theme::c32(t.fg_muted),
        );
        if !mark.is_empty() {
            painter.text(
                rect.right_center() - egui::vec2(pad, 0.0),
                egui::Align2::RIGHT_CENTER,
                mark,
                font.clone(),
                theme::c32(t.fg_muted),
            );
        }
        if resp.clicked() {
            hit = Some(key);
        }
    }
    if let Some(k) = hit {
        state.click_header(k);
    }
}

fn row(
    ui: &mut Ui,
    t: &Theme,
    e: &mullion_ssh::sftp::Entry,
    column: PanelColumn,
    selected: bool,
    cols: &ColWidths,
    owners: &crate::files::owners::OwnerNames,
) -> egui::Response {
    // `click_and_drag` 而不是 `click`(F58):行要能起拖。`clicked()` /
    // `double_clicked()` 在这个 Sense 下照旧 —— egui 只有在指针真的移出
    // 拖拽阈值之后才把这一下判成拖,原地按松仍然是点击。
    //
    // 行宽取「内容总宽」与「视口宽」的**较大者**:
    // - 总宽 > 视口 → 行要撑满内容宽,否则右边几列画在行的交互 rect 之外;
    // - 总宽 < 视口 → 行要铺满视口,否则选中高亮只有半行长,右边那片空白
    //   点不中行、也接不住 drop(`a_row_in_the_tab_host_can_actually_be_clicked`
    //   与 `dropping_on_the_blank_part_of_a_column_targets_its_current_directory`
    //   两条现有测试守着这两件事)。
    let w = content_w(cols, column).max(ui.available_width());
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), egui::Sense::click_and_drag());
    if selected {
        // F150:accent 半透明铺满整行 + 行首一条实色。原来画 `sunken_bg`,
        // 比 `panel_bg` 还暗 6 个亮度单位,肉眼分辨不出来 —— 用户因此以为
        // 文件面板根本没有多选(`click_row` 的 Ctrl/Shift 语义一直都在)。
        // 色条不是装饰:底色再淡也可能被行内容夺走注意力,一条实色边给出
        // 「这一段是选中的」的轮廓,连选多行时一眼能看出范围。
        ui.painter()
            .rect_filled(rect, 2.0, theme::selection_fill(t));
        ui.painter().rect_filled(
            egui::Rect::from_min_size(rect.left_top(), egui::vec2(SEL_BAR_W, rect.height())),
            0.0,
            theme::c32(t.accent),
        );
    }
    let p = ui.painter();
    let font = egui::FontId::proportional(12.0);
    // F137:测宽用**真实字体**。`Painter::layout_no_wrap` 内部带按帧滑动窗口
    // 的 `LayoutJob` 缓存(`epaint-0.30.0` `text/fonts.rs` `begin_pass` 里
    // `flush_cache` 只保留「上一帧用过」的条目,不是整段清空)——**稳态下**
    // (列宽帧间不变)命中率高、很便宜;但 `truncate_to_width` 的二分查找
    // 每一步测的是不同长度的前缀,是不同的缓存 key,**列宽逐帧变化时**
    // (Task 6 拖拽进行中、或窗口连续 resize)这批中间前缀逐帧不同、命中不了,
    // 最坏情况(窄列 + 长名字全触发截断)一屏能摸到千级 `layout_no_wrap`/帧。
    // 颜色传 `Color32::WHITE` 占位、只取尺寸不画——但缓存 key 是整个
    // `LayoutJob`(含颜色),这个占位色和真正画字用的颜色各自产生一份独立
    // 缓存条目,是预期开销,不是 bug。
    let measure = |s: &str| {
        p.layout_no_wrap(s.to_owned(), font.clone(), egui::Color32::WHITE)
            .size()
            .x
    };
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
    // 名称列的可用宽度:让出图标格子 + 间隙,右边再留一个 `SP_XS` 的
    // 呼吸位,否则截断后的省略号会紧贴着「大小」列的数字。
    let name_budget = cols.name - name_start_x_offset() - crate::ui::metrics::SP_XS;
    p.text(
        rect.left_center() + egui::vec2(name_start_x_offset(), 0.0),
        egui::Align2::LEFT_CENTER,
        // **拼完整串再截**:符号链接的 `→ target` 和「名称非 UTF-8」那句
        // 后缀都得参与预算,先截名字再拼后缀的话后缀照样溢出。
        elide(&label, name_budget, Elide::Middle, measure),
        font.clone(),
        fg,
    );
    // 名称列的可用宽度让出图标格子 + 间隙,否则长文件名会顶到大小列上——
    // 下面四列的坐标从 `col_lefts()` 取,与 `header_at()` 同源(见其文档
    // 注释),不许在这里另起一份累加。
    let lay = col_lefts(cols, column);
    // 大小(右对齐)
    let (_, _, size_left, size_w) = lay[1];
    let size_text = if e.kind == EntryKind::Dir {
        String::new()
    } else {
        human_size(e.size)
    };
    p.text(
        egui::pos2(
            rect.left() + size_left + size_w - crate::ui::metrics::SP_XS,
            rect.center().y,
        ),
        egui::Align2::RIGHT_CENTER,
        elide(
            &size_text,
            size_w - crate::ui::metrics::SP_XS,
            Elide::End,
            measure,
        ),
        font.clone(),
        theme::c32(t.fg_mid),
    );
    // 修改时间(左对齐)
    let (_, _, mtime_left, mtime_w) = lay[2];
    p.text(
        egui::pos2(
            rect.left() + mtime_left + crate::ui::metrics::SP_S,
            rect.center().y,
        ),
        egui::Align2::LEFT_CENTER,
        elide(
            &mtime_text(e.mtime),
            mtime_w - crate::ui::metrics::SP_S,
            Elide::End,
            measure,
        ),
        font.clone(),
        theme::c32(t.fg_mid),
    );
    // 权限(右对齐,不再把 uid:gid 拼在后面 —— 属主拆成了独立列)。
    let (_, _, perm_left, perm_w) = lay[3];
    p.text(
        egui::pos2(
            rect.left() + perm_left + perm_w - crate::ui::metrics::SP_XS,
            rect.center().y,
        ),
        egui::Align2::RIGHT_CENTER,
        elide(
            &perm_string(e.mode),
            perm_w - crate::ui::metrics::SP_XS,
            Elide::End,
            measure,
        ),
        font.clone(),
        theme::c32(t.fg_dim),
    );
    // 属主(右对齐)。F146:本地栏没有这一列 —— `col_lefts` 已经不返回它,
    // 这里跟着按「下标存不存在」判,不再各写一份栏别判断。
    if let Some(&(_, _, owner_left, owner_w)) = lay.get(4) {
        p.text(
            egui::pos2(
                rect.left() + owner_left + owner_w - crate::ui::metrics::SP_XS,
                rect.center().y,
            ),
            egui::Align2::RIGHT_CENTER,
            elide(
                &owner_text(column, e.uid, e.gid, owners),
                owner_w - crate::ui::metrics::SP_XS,
                Elide::End,
                measure,
            ),
            font,
            theme::c32(t.fg_dim),
        );
    }
    resp
}

/// 属主列文案。**本地栏恒画 `—`**,判据是 `column == PanelColumn::Local`
/// 而不是 `uid == 0` —— 远端真的有 root 拥有的文件,拿 0 当「没有属主
/// 信息」的哨兵会让那些文件的属主列也变成 `—`。
///
/// F142:远端栏优先画 `用户名:组名`,名字**还没查到或查不到**时那一段
/// 回退成数字(`deploy:10001`)。缓存与查询逻辑在 `files::owners`。
fn owner_text(
    column: PanelColumn,
    uid: u32,
    gid: u32,
    owners: &crate::files::owners::OwnerNames,
) -> String {
    if column == PanelColumn::Local {
        "—".to_string()
    } else {
        owners.text(uid, gid)
    }
}

/// F137:截断方式。
#[derive(Clone, Copy, Debug, PartialEq)]
enum Elide {
    /// 尾部省略:`2026-08-19 10:3…`。给数值/时间/权限/属主与列头标题用。
    End,
    /// 中间省略、保留扩展名:`a-very-long-fil….tar.gz`。只给名称列用 ——
    /// 一列同前缀的备份文件尾部省略之后全长一样,信息量清零。
    Middle,
}

const ELLIPSIS: &str = "…";

/// 把 `text` 截到 `max_w` 以内,截掉的地方放一个 `…`。
///
/// `measure`:测一段文字的宽度。生产侧传 `|s| painter.layout_no_wrap(...)`
/// ——**必须用真实字体测**,CJK 一个字顶两个 ASCII,按字符数估算会在中文
/// 目录里全错。测试侧传桩,于是这个函数本身不需要 egui 上下文就能测。
///
/// **前提:`measure` 对前缀单调不减**(前缀越长越宽)。二分查找依赖这一点;
/// 对正常字体成立(字形宽度非负)。
fn elide(text: &str, max_w: f32, mode: Elide, measure: impl Fn(&str) -> f32) -> String {
    if measure(text) <= max_w {
        return text.to_string();
    }
    let ellipsis_w = measure(ELLIPSIS);
    if mode == Elide::Middle {
        if let Some(tail) = ext_tail(text) {
            let tail_w = measure(tail);
            // 尾段 + 省略号吃掉超过一半预算 → 退化成尾部省略。留一个前面
            // 什么都放不下的尾巴,不如老老实实从后面截。
            if tail_w + ellipsis_w <= max_w * 0.5 {
                let head_src = &text[..text.len() - tail.len()];
                let head = truncate_to_width(head_src, max_w - tail_w - ellipsis_w, &measure);
                return format!("{head}{ELLIPSIS}{tail}");
            }
        }
    }
    // 尾部省略(也是 `Middle` 的退化路径)。
    if ellipsis_w > max_w {
        // 连一个省略号都放不下:什么都不画。画半个字符更像渲染坏了。
        return String::new();
    }
    let head = truncate_to_width(text, max_w - ellipsis_w, &measure);
    format!("{head}{ELLIPSIS}")
}

/// 从右往左取至多 **2 段**扩展名,总长(字符数)不超过 10。
///
/// `a.tar.gz` → `.tar.gz`;`a.txt` → `.txt`;`x.20260819.backup` → `.backup`
/// (两段共 16 > 10);`.bashrc` → `None`(点在开头,那是名字不是扩展名);
/// `no-ext` → `None`。
fn ext_tail(name: &str) -> Option<&str> {
    let mut best = None;
    let mut rest = name;
    for _ in 0..2 {
        let Some((head, _)) = rest.rsplit_once('.') else {
            break;
        };
        if head.is_empty() {
            break;
        }
        // `head` 是 `rest` 的前缀,而 `rest` 是 `name` 的前缀 ——
        // `head.len()` 因此也是 `name` 里的字节偏移。
        let cand = &name[head.len()..];
        if cand.chars().count() > 10 {
            break;
        }
        best = Some(cand);
        rest = head;
    }
    best
}

/// `s` 里能放进 `budget` 的最长**前缀**(切在 `char` 边界上)。
///
/// 二分而不是逐字符递减:一屏有几十行 × 五列,长名字逐字符退让会把
/// `layout_no_wrap` 调用次数推到每帧上万次。
fn truncate_to_width<'a>(s: &'a str, budget: f32, measure: &impl Fn(&str) -> f32) -> &'a str {
    if measure(s) <= budget {
        return s;
    }
    let bounds: Vec<usize> = s
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(s.len()))
        .collect();
    // 找最大的 `lo` 使 `s[..bounds[lo]]` 放得下。`lo = 0`(空串)恒满足。
    let (mut lo, mut hi) = (0usize, bounds.len() - 1);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if measure(&s[..bounds[mid]]) <= budget {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    &s[..bounds[lo]]
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
    /// F139:这个标签绑着会话记录没有(`Tab::session_id.is_some()`)。没绑就
    /// 没地方存书签,☆ 按钮置灰。默认 `false` —— 与这个结构体
    /// 「新标签初值 / 借用过桥占位」的双重语境约定一致(见 `Default` 的说明):
    /// 不知道的时候按"不能写"处理,最坏结果是少一个功能,不是静默丢数据。
    pub session_bound: bool,
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
            session_bound: false,
            active_column: PanelColumn::default(),
        }
    }
}

impl PanelFrame {
    /// 用会话配置的默认本地目录 + 书签开一个新标签。`default_local` 直接转给
    /// `files::local::default_local`(`None` = 用户主目录);远端目录不在这里
    /// 决定 —— 连接尚未建立时根本没有远端可拿,拿真实登录目录/配置值是
    /// `app.rs::spawn_sftp_open` 建立连接后才做的事。
    ///
    /// `session_bound`:F139 —— 这个标签有没有 `SessionId`。**显式参数而不是
    /// 事后赋值**:加参数的话编译器会逼每个调用点表态,漏一个就编译不过;
    /// 而 `frame.session_bound = ...` 那种写法漏了没人提醒,表现是某种标签
    /// 里的 ☆ 永远置灰,而用户只会觉得"这功能坏了"。
    pub fn new(
        default_local: Option<&str>,
        bookmarks: Vec<mullion_store::Bookmark>,
        session_bound: bool,
    ) -> Self {
        Self {
            local: PaneState::new(crate::files::local::default_local(default_local)),
            bookmarks,
            session_bound,
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
/// F144 的内缩量。跟 `content()` 里那个 `pad` 是同一档 —— 抽出来是为了
/// 「两个宿主必须一致」这件事有个单一出处,而不是靠两处各写一遍 `SP_XS`。
fn column_pad() -> egui::Vec2 {
    egui::vec2(crate::ui::metrics::SP_XS, crate::ui::metrics::SP_XS)
}

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
                .stroke(theme::stroke(t))
                // F138:内容不贴边。左右比上下宽 —— 横向是「内容 vs 边框」的
                // 关系,纵向相邻的是别的面板,那边本就有分隔线垫着。取值只从
                // `metrics` 的间距五档里选,不写裸数字。
                .inner_margin(egui::Margin::symmetric(
                    crate::ui::metrics::SP_S,
                    crate::ui::metrics::SP_XS,
                )),
        )
        .show(ctx, |ui| {
            annotate::mark(ui.ctx(), "文件侧栏", ui.max_rect());
            let h = ui.available_height();
            // A3:比例**跟着远端走,不跟着位置走** —— 侧栏是「终端为主 +
            // 文件为辅」的场景,辅助视图里远端才是主体。所以上面(本地)0.4、
            // 下面(远端)0.6。
            ui.allocate_ui(egui::vec2(ui.available_width(), h * 0.4), |ui| {
                // 裁到本栏之内,理由同 `content()` 里两栏各自那句 `set_clip_rect`。
                ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                // F144:裁剪区照旧,布局预算内缩一档。理由同 `content()` ——
                // 见那里那段长注释;两处必须一起改,否则同一个控件在标签
                // 宿主里不缺角、在侧栏里缺角。
                let inner = ui.max_rect().shrink2(column_pad());
                ui.scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
                    out.1 = show(
                        ui,
                        t,
                        "本地",
                        generation,
                        PanelColumn::Local,
                        &mut frame.local,
                        panel_focused && frame.active_column == PanelColumn::Local,
                        BookmarkView::none(),
                        0,
                        &mut ui_state.files_cols,
                    );
                });
            });
            ui.separator();
            // **必须跟本地栏一样包一层把矩形限死**。直接 `show(ui, ..)` 的话
            // `ui.max_rect()` 是**整条侧栏**(egui 的 `max_rect` 是 ui 建出来
            // 那一刻的预算,不随 cursor 往下走而缩),于是 `show()` 开头那个
            // 覆盖整栏的右键菜单宿主(`ui.interact(ui.max_rect(), ..)`)会连
            // 上面的本地栏一起罩住 —— 它注册在本地栏所有控件之后,同层后注册者
            // 压在上面,本地栏的 ↑/⟳/☆/▾ 和路径条**一个都点不中**(v0.1.55
            // 用户实测)。守护:`tests::the_sidebar_local_column_still_receives_
            // clicks_under_the_remote_column`。
            ui.allocate_ui(ui.available_size(), |ui| {
                ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                // F144:同本地栏,见上面。
                let inner = ui.max_rect().shrink2(column_pad());
                ui.scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
                    out.0 = show(
                        ui,
                        t,
                        "远端",
                        generation,
                        PanelColumn::Remote,
                        &mut frame.remote,
                        panel_focused && frame.active_column == PanelColumn::Remote,
                        BookmarkView {
                            list: &frame.bookmarks,
                            can_edit: frame.session_bound,
                        },
                        drop_in,
                        &mut ui_state.files_cols,
                    );
                });
            });
        });
    // 把这一帧的实际宽度读回来。**注意它只是个镜像,驱动不了任何东西**:
    // egui 0.30 的 `SidePanel::show_inside_dyn` 先取 `default_width`,紧接着
    // 只要 `PanelState::load` 在 memory 里找得到这个 id 的存档就整个覆盖掉
    // (`egui-0.30.0/src/containers/panel.rs`)。也就是说除了**第一帧**,上面那个
    // `default_w` 都不生效,宽度由 egui 自己的持久化状态说了算。
    // 以后要做「宽度按会话记住」,光把值存进这个字段不够 —— 得连带清掉
    // egui memory 里对应的 `PanelState`,否则改了也看不见。
    ui_state.files_sidebar_w = resp.response.rect.width();
    // F59:把外框矩形留给下一帧的拖出交接判据(见 `UiState::files_panel_rect`)。
    ui_state.files_panel_rect = Some(resp.response.rect);
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
///
/// `panel_rect`:F59 —— 把本帧的外框矩形写回去,给下一帧的拖出交接判据用。
/// 独立出参而不是收整个 `UiState`:标签宿主刻意不认识 `UiState`(上面
/// `cols` 那段同理),多收一个可变引用会把这条边界打掉。
#[allow(clippy::too_many_arguments)] // 同 `show`:一帧要画的东西天然多
pub fn content(
    ctx: &egui::Context,
    t: &Theme,
    generation: u64,
    panel_focused: bool,
    frame: &mut PanelFrame,
    drop_in: usize,
    cols: &mut ColWidths,
    panel_rect: &mut Option<egui::Rect>,
) -> (Option<FileAction>, Option<FileAction>) {
    let mut out = (None, None);
    egui::CentralPanel::default()
        .frame(
            egui::Frame::none()
                .fill(theme::c32(t.panel_bg))
                .stroke(theme::stroke(t))
                // F138:同 `sidebar`,见那里的注释。
                .inner_margin(egui::Margin::symmetric(
                    crate::ui::metrics::SP_S,
                    crate::ui::metrics::SP_XS,
                )),
        )
        .show(ctx, |ui| {
            annotate::mark(ui.ctx(), "文件标签", ui.max_rect());
            // F59:见本函数 `panel_rect` 参数的说明。
            *panel_rect = Some(ui.max_rect());
            // **两栏的矩形自己切,不走 `ui.horizontal` + `allocate_ui`**:
            // 在 `horizontal` 布局里 `ui.available_height()` 给的是**当前这一行**
            // 的高度(这里是 18pt),不是整块内容区。照它分配的话两栏各只有一行
            // 高,`ScrollArea` 视口被压扁、行的交互 rect 宽度塌成 0 ——
            // 画面上文字照旧(`painter` 直接按坐标画),但整个标签宿主里
            // **一行都点不中**。这是 D4a 做拖拽时才暴露出来的既存 bug。
            let full = ui.max_rect();
            let gap = crate::ui::metrics::SP_S;
            let half = ((full.width() - gap) * 0.5).max(0.0);
            // F144:**裁剪区照旧,布局预算内缩一档**。两者必须错开,一起缩
            // 等于没缩 —— 控件是从 `max_rect.min` 起画的,`clip_rect` 跟着
            // 缩到同一个位置,描边照样贴边被切(实现时真踩过一次)。
            //
            // 裁剪区维持原样是 B1 定的(见下面 `set_clip_rect` 那段);内缩
            // 布局预算是为了让控件从裁剪边往里让出 SP_XS,圆角描边的外半
            // 像素有地方落。不让的话用户看到的是「↑ 按钮左边缺 1/4 圆弧」
            // 「路径条控件没有上边框」(v0.1.56 实测报的两条)。
            //
            // 外框那一档留白是 `CentralPanel` 的 `inner_margin` 给的(F138),
            // 管不到这里 —— 两栏是在它之内再切一刀,那一刀上本来一点余量
            // 都没有。守护:`tests::panel_content_does_not_touch_the_clip_edge`。
            let pad = column_pad();
            let left = egui::Rect::from_min_size(full.min, egui::vec2(half, full.height()));
            let right =
                egui::Rect::from_min_max(egui::pos2(full.max.x - half, full.min.y), full.max);
            ui.scope_builder(egui::UiBuilder::new().max_rect(left.shrink2(pad)), |ui| {
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
                    BookmarkView::none(),
                    0,
                    cols,
                );
            });
            ui.painter()
                .vline(full.center().x, full.y_range(), theme::stroke(t));
            ui.scope_builder(egui::UiBuilder::new().max_rect(right.shrink2(pad)), |ui| {
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
                    BookmarkView {
                        list: &frame.bookmarks,
                        can_edit: frame.session_bound,
                    },
                    drop_in,
                    cols,
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

    /// 同 `find_text_pos`,但给的是整块文字的**矩形**。判「贴着某条边」
    /// 这类事得用边界:拿中心点判要先猜字宽,猜出来的阈值不算判据。
    fn find_text_rect(shapes: &[egui::epaint::ClippedShape], needle: &str) -> Option<egui::Rect> {
        fn walk(shape: &egui::Shape, needle: &str) -> Option<egui::Rect> {
            match shape {
                egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, needle)),
                egui::Shape::Text(ts) if ts.galley.text().contains(needle) => {
                    Some(egui::Rect::from_min_size(ts.pos, ts.galley.size()))
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

    /// 测宽桩:ASCII 7pt / 非 ASCII 14pt(CJK 一个字顶两个 ASCII,省略号
    /// 也是非 ASCII)。用桩而不是真字体,这几条测试才能脱离 egui 上下文跑。
    fn stub_measure(s: &str) -> f32 {
        s.chars()
            .map(|c| if c.is_ascii() { 7.0 } else { 14.0 })
            .sum()
    }

    /// F137:名称列中间省略,**扩展名必须留住** —— 一列同前缀的
    /// `backup-2026-08-19.tar.gz` / `.log` / `.sql`,尾部省略之后全长一样,
    /// 等于把这一列的信息量清零。
    ///
    /// 自证会变红:把 `ext_tail()` 的两段扩展名循环改成只取一段
    /// (`.tar.gz` 会退化成 `.gz`),或者把 `Middle` 直接转发给 `End`。
    #[test]
    fn eliding_a_name_in_the_middle_keeps_its_extension() {
        let out = elide(
            "a-very-long-filename.tar.gz",
            140.0,
            Elide::Middle,
            stub_measure,
        );
        assert!(
            out.ends_with(".tar.gz"),
            "两段扩展名没留住,实际截成了 {out:?}"
        );
        assert!(out.contains('…'), "中间没有省略号,实际 {out:?}");
        assert!(
            stub_measure(&out) <= 140.0,
            "截完还是超宽({}),实际 {out:?}",
            stub_measure(&out)
        );

        let cjk = elide(
            "很长的中文文件名很长的中文文件名.txt",
            100.0,
            Elide::Middle,
            stub_measure,
        );
        assert!(cjk.ends_with(".txt"), "单段扩展名没留住,实际 {cjk:?}");
        assert!(
            stub_measure(&cjk) <= 100.0,
            "CJK 名字截完超宽({}),实际 {cjk:?}",
            stub_measure(&cjk)
        );
    }

    /// `ext_tail()` 直接单测,文档注释承诺的五个行为 + `.gitignore` 各断言
    /// 一遍。**开头的点不是扩展名** —— `.bashrc` / `.gitignore` 这类
    /// dotfile 必须返回 `None`,否则会被 `elide(Middle)` 截成
    /// `….gitignore` 这种「省略号 + 全名」的垃圾(省略号什么都没省掉)。
    ///
    /// 自证会变红:去掉 `ext_tail()` 里 `head.is_empty()` 那条守卫 ——
    /// 见下方实测。
    #[test]
    fn ext_tail_treats_a_leading_dot_as_part_of_the_name() {
        assert_eq!(ext_tail("a.tar.gz"), Some(".tar.gz"));
        assert_eq!(ext_tail("a.txt"), Some(".txt"));
        assert_eq!(ext_tail("x.20260819.backup"), Some(".backup"));
        assert_eq!(ext_tail(".bashrc"), None);
        assert_eq!(ext_tail("no-ext"), None);
        assert_eq!(ext_tail(".gitignore"), None);
    }

    /// 边界:放得下就原样返回;没有扩展名 / 扩展名自己就吃掉半个预算 /
    /// 预算窄到连省略号都放不下 —— 都不许 panic,也不许超宽。
    ///
    /// 自证会变红:去掉 `elide()` 里「省略号都放不下就返回空串」那条 ——
    /// 实测 `elide("abc.txt", 10.0, Elide::End, stub_measure)` 会截成
    /// `"…"`(宽 14),超过预算 10,断言在 `"abc.txt" 在预算 10 下截成了
    /// "…",宽 14` 报错。
    #[test]
    fn eliding_never_exceeds_the_budget_and_never_panics() {
        // 放得下 → 原样。
        assert_eq!(elide("a.txt", 999.0, Elide::Middle, stub_measure), "a.txt");
        for (text, budget) in [
            ("no-extension-but-really-long-indeed", 60.0),
            ("x.20260819.backup", 60.0),
            (".bashrc", 30.0),
            ("很长很长很长很长的中文名", 40.0),
            ("abc.txt", 10.0),
            ("abc.txt", 0.0),
            ("", 50.0),
        ] {
            for mode in [Elide::End, Elide::Middle] {
                let out = elide(text, budget, mode, stub_measure);
                assert!(
                    stub_measure(&out) <= budget + 0.01,
                    "{text:?} 在预算 {budget} 下截成了 {out:?},宽 {}",
                    stub_measure(&out)
                );
            }
        }
    }

    /// `Elide::End` 必须**真的截**,不能靠退化成空串蒙混过关 —— 空串宽度
    /// 恒为 0,能不劳而获地通过任何「不超预算」的上界断言。这条锁的是
    /// 正面结果:一个放不下的无扩展名串走 `End`,必须产出「原串前缀 + 省略
    /// 号」的非空结果,而不是随便什么宽度合规的东西。
    ///
    /// 自证会变红:把 `elide()` 里 `End` 路径(`ellipsis_w > max_w` 判断
    /// 之后那一段)整段改成恒 `return String::new();` —— 见下方实测。
    #[test]
    fn eliding_end_mode_actually_truncates_instead_of_degrading_to_empty() {
        let text = "no-extension-but-really-long-indeed";
        let out = elide(text, 60.0, Elide::End, stub_measure);
        assert!(out.ends_with('…'), "结果没有以省略号结尾,实际 {out:?}");
        let head = out.strip_suffix('…').expect("上面已断言以省略号结尾");
        assert!(!head.is_empty(), "省略号前面是空的,退化成了纯省略号");
        assert!(
            text.starts_with(head),
            "省略号前面的部分 {head:?} 不是原串 {text:?} 的前缀"
        );
        assert!(
            stub_measure(&out) <= 60.0 + 0.01,
            "截完还是超宽({}),实际 {out:?}",
            stub_measure(&out)
        );
    }

    /// `truncate_to_width()` 没有 `budget <= 0.0` 早退也不会 panic ——
    /// 二分本身对非正预算是良构的:`s = ""` 时 `bounds = [0]`,`hi = 0`,
    /// 循环不进,直接返回 `""`;`s` 非空且 `budget <= 0` 时,`measure` 非负
    /// 保证 `measure(&s[..bounds[mid]]) <= budget` 恒为 false,`hi` 一路收到
    /// `lo`(初值 0),同样返回 `""`,过程中 `mid - 1` 因为 `mid = (lo+hi)
    /// .div_ceil(2)` 在 `hi > lo` 时恒 `>= 1` 而不下溢。
    ///
    /// 自证会变红(两处均实测过):
    /// - 把 `lo` 初值从 `0` 改成 `1` → `"abc"` 那条炸,截成 `"a"` 而不是
    ///   `""`(`assertion left == right failed / left: "a" / right: ""`)。
    /// - 把 `if measure(&s[..bounds[mid]]) <= budget { lo = mid }` 的判断
    ///   短路成恒假(`lo` 永远停在初值不再前进)→ 正常情形那条炸,预算
    ///   14 放得下两个字符却截成 `""`(`left: "" / right: "ab"`)。
    #[test]
    fn truncate_to_width_returns_an_empty_prefix_for_a_non_positive_budget() {
        assert_eq!(truncate_to_width("", 0.0, &stub_measure), "");
        assert_eq!(truncate_to_width("abc", 0.0, &stub_measure), "");
        assert_eq!(truncate_to_width("很长的中文", -5.0, &stub_measure), "");
        // 不是只测退化路径:预算刚好放得下两个 ASCII 字符时,返回两字符前缀。
        assert_eq!(truncate_to_width("abcdef", 14.0, &stub_measure), "ab");
    }

    /// F137 的验收判据:长名字**画不到**「大小」列上。
    ///
    /// 纯函数那两条只守 `elide()` 自己算得对不对 —— 生产代码完全可以
    /// 算完了不用(或者预算传错),这条从真实渲染结果里取两个 galley
    /// 的横向区间来比,才守得住「接上了」。
    ///
    /// 自证会变红:把 `row()` 里名称那处的 `elide(...)` 换回原来的
    /// `label` 直画。
    #[test]
    fn a_long_name_is_elided_so_it_cannot_reach_the_size_column() {
        /// 找到含 `needle` 的文字,返回它的**横向区间**(左, 右)。
        fn text_span(shapes: &[egui::epaint::ClippedShape], needle: &str) -> Option<(f32, f32)> {
            fn walk(shape: &egui::Shape, needle: &str) -> Option<(f32, f32)> {
                match shape {
                    egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, needle)),
                    egui::Shape::Text(ts) if ts.galley.text().contains(needle) => {
                        Some((ts.pos.x, ts.pos.x + ts.galley.size().x))
                    }
                    _ => None,
                }
            }
            shapes.iter().find_map(|cs| walk(&cs.shape, needle))
        }

        let t = crate::theme::MULLION_DARK;
        let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
        // 长到无论如何都放不进 220pt 名称列的名字。
        state.entries = vec![entry(
            b"aaaaaaaaaa-bbbbbbbbbb-cccccccccc-dddddddddd-eeeeeeeeee.tar.gz",
            EntryKind::File,
        )];
        state.load = Load::Ready;

        let mut cols = ColWidths::default();
        let ctx = egui::Context::default();
        let mut out = None;
        for _ in 0..2 {
            out = Some(ctx.run(raw(None), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        "远端",
                        1,
                        PanelColumn::Remote,
                        &mut state,
                        false,
                        BookmarkView::none(),
                        0,
                        &mut cols,
                    );
                });
            }));
        }
        let out = out.expect("跑了两帧");
        // 名称的 galley:截断后一定含 `…`,而且尾巴留着 `.tar.gz`。
        let name = text_span(&out.shapes, "…").expect("长名字应该被截断并带上省略号");
        let value = text_span(&out.shapes, "1.0 KB").expect("行里该画出大小数值");
        assert!(
            name.1 <= value.0 + 0.01,
            "名字右边界 {} 越过了大小数值左边界 {} —— 两串文字重叠在一起了",
            name.1,
            value.0
        );
    }

    /// F137 的验收判据(列头):`row()` 那条只守行体,列头没有对应守护——
    /// 补的这一条。把「修改时间」列拖到窄于标题本身的宽度,标题必须被
    /// 截断到不超列宽,不能横穿到隔壁列头上面。
    ///
    /// 自证会变红:把 `header_at()` 里的 `elide(...)` 换回
    /// `format!("{label}{mark}")` 直画(已实测:47 条既有测试原样全绿,
    /// 说明没有任何既有测试守着列头这一处,必须补)。
    #[test]
    fn a_narrow_column_header_is_elided_so_it_does_not_overflow_the_column() {
        /// 找到「文字是 `full` 的前缀(可能带一个尾随 `…`)」的那个 galley,
        /// 返回它的宽度。不按精确字符串找 —— 截断结果具体截多少字符是
        /// `elide()` 的实现细节,这里只关心「有没有截、宽度对不对」。
        fn prefix_galley_width(shapes: &[egui::epaint::ClippedShape], full: &str) -> Option<f32> {
            fn walk(shape: &egui::Shape, full: &str) -> Option<f32> {
                match shape {
                    egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, full)),
                    egui::Shape::Text(ts) => {
                        let text = ts.galley.text();
                        let head = text.strip_suffix('…').unwrap_or(text);
                        (!head.is_empty() && full.starts_with(head)).then(|| ts.galley.size().x)
                    }
                    _ => None,
                }
            }
            shapes.iter().find_map(|cs| walk(&cs.shape, full))
        }

        let t = crate::theme::MULLION_DARK;
        let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
        state.entries = vec![entry(b"a.txt", EntryKind::File)];
        state.load = Load::Ready;

        // 「修改时间」四个 CJK 字在 11pt 下实测宽 44 —— 30pt 一定放不下、
        // 会被截断,又没窄到连省略号都放不下(那样会画出空字符串,反而
        // 测不了「有没有截断」)。
        let mut cols = ColWidths {
            mtime: 30.0,
            ..ColWidths::default()
        };
        let ctx = egui::Context::default();
        let mut out = None;
        for _ in 0..2 {
            out = Some(ctx.run(raw(None), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        "远端",
                        1,
                        PanelColumn::Remote,
                        &mut state,
                        false,
                        BookmarkView::none(),
                        0,
                        &mut cols,
                    );
                });
            }));
        }
        let out = out.expect("跑了两帧");
        let w = prefix_galley_width(&out.shapes, "修改时间")
            .expect("「修改时间」列头该画出一个是全名前缀的 galley");
        assert!(
            w <= cols.mtime + 0.01,
            "列头宽 {w} 超过了列宽 {}(mtime 列),标题横穿到隔壁列头去了",
            cols.mtime
        );
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
    ///
    /// `cols`:调用方传列宽 —— 非 UTF-8 那两条测试要用远宽于默认值的名称列
    /// (见其调用处注释:默认 220pt 下这两条只是踩着 1pt 余量幸存,字体度量/
    /// DPI/字体版本任何一点漂移都会让它们翻红)。
    fn rendered_texts(state: &mut PaneState, cols: &mut ColWidths) -> Vec<String> {
        let t = crate::theme::MULLION_DARK;
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
                        state,
                        false,
                        BookmarkView::none(),
                        0,
                        cols,
                    );
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
    ///
    /// 用远宽于默认值的名称列(900pt)跑 —— 这条守的是「提示存在」,不是
    /// 「窄列下也完整可见」;实测默认 220pt 名称列下,拼完整串
    /// `中.txt(名称非 UTF-8,本版无法操作)` 宽 193.0pt,预算
    /// `220 - 24(图标+间隙) - 4(SP_XS) = 192.0pt`,只差 1pt 就会把
    /// 「名称非 UTF-8」这句自己截掉 —— 字体度量/DPI/字体版本任何一点漂移
    /// 都会让它翻红,不该悬在这种边界上。
    #[test]
    fn a_non_utf8_name_is_shown_with_an_explicit_note() {
        let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
        state.entries = vec![entry(
            &[0xd6, 0xd0, b'.', b't', b'x', b't'],
            EntryKind::File,
        )];
        state.load = Load::Ready;

        let mut cols = ColWidths {
            name: 900.0,
            ..ColWidths::default()
        };
        let texts = rendered_texts(&mut state, &mut cols);
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
    ///
    /// 同上一条,用 900pt 名称列跑 —— 理由同见其文档注释。
    #[test]
    fn a_lossy_name_that_is_valid_utf8_is_still_marked_unusable() {
        let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
        state.entries = vec![entry("\u{fffd}\u{fffd}.txt".as_bytes(), EntryKind::File)];
        state.load = Load::Ready;
        assert!(
            state.entries[0].name.is_utf8(),
            "前提:这串本身是合法 UTF-8,这正是不能拿 is_utf8() 当判据的原因"
        );

        let mut cols = ColWidths {
            name: 900.0,
            ..ColWidths::default()
        };
        let texts = rendered_texts(&mut state, &mut cols);
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
            session_bound: false,
            active_column: PanelColumn::default(),
        };
        frame.remote.entries = vec![entry(b"remote-only.txt", EntryKind::File)];
        frame.remote.load = Load::Ready;
        frame.local.entries = vec![entry(b"local-only.txt", EntryKind::File)];
        frame.local.load = Load::Ready;

        let ctx = egui::Context::default();
        let mut cols = ColWidths::default();
        let mut texts = Vec::new();
        for _ in 0..2 {
            texts.clear();
            let out = ctx.run(egui::RawInput::default(), |ctx| {
                content(ctx, &t, 1, false, &mut frame, 0, &mut cols, &mut None);
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

    /// 数一数有多少个指定填充色的矩形。`w_range` 用来把宽窄不同的两种矩形
    /// 分开(整行底色 vs 2pt 的左侧色条)——只判颜色的话,色条和底色都算
    /// 进去,「色条没画」这种退化验不出来。
    fn count_filled_rects(
        shapes: &[egui::epaint::ClippedShape],
        color: egui::Color32,
        w_range: std::ops::RangeInclusive<f32>,
    ) -> usize {
        shapes
            .iter()
            .filter(|s| match &s.shape {
                egui::epaint::Shape::Rect(r) => {
                    r.fill == color && w_range.contains(&r.rect.width())
                }
                _ => false,
            })
            .count()
    }

    /// F150:选中行必须画成 accent 半透明 + 左侧 2pt 实色色条。
    ///
    /// 原来画的是 `sunken_bg`(#0e1018),比 `panel_bg`(#14161f)还暗 —— 用户
    /// 报「按 Ctrl 点,屏幕上完全没变化」,根因就在这儿。这条测试拿颜色本身
    /// 当判据,换回任何一个比背景暗的 token 都会红。
    #[test]
    fn a_selected_row_is_painted_with_the_accent_fill_not_the_sunken_bg() {
        let t = crate::theme::MULLION_DARK;
        let mut frame = two_columns();
        frame
            .remote
            .selected
            .insert(RemotePath::from_bytes(b"b.txt".to_vec()));
        let ctx = egui::Context::default();
        let mut cols = ColWidths::default();
        let mut render = |frame: &mut PanelFrame| {
            let mut out = None;
            let o = ctx.run(raw(None), |ctx| {
                out = Some(content(ctx, &t, 1, true, frame, 0, &mut cols, &mut None));
            });
            let _ = out;
            o
        };
        let _ = render(&mut frame);
        let out = render(&mut frame);

        assert_eq!(
            count_filled_rects(
                &out.shapes,
                crate::theme::selection_fill(&t),
                20.0..=f32::MAX
            ),
            1,
            "选中的那一行该有一块 accent 半透明的整行底色"
        );
        assert_eq!(
            count_filled_rects(&out.shapes, crate::theme::c32(t.accent), 1.0..=3.0),
            1,
            "选中的那一行该有一条 2pt 宽的 accent 实色左侧色条"
        );
        assert_eq!(
            count_filled_rects(&out.shapes, crate::theme::c32(t.sunken_bg), 20.0..=f32::MAX),
            0,
            "不该再用 sunken_bg 画选中行 —— 它比 panel_bg 还暗,等于没画"
        );
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
            let mut cols = ColWidths::default();
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
                            BookmarkView::none(),
                            0,
                            &mut cols,
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

    /// F136:内容比视口宽时,横着滚 —— 而且**列头要跟着一起滚**。
    /// 列头不跟的话,滚过去之后标题和数据完全对不上,比不能滚更糟。
    ///
    /// 判据是「位移量相同」而不是「x 相等」:列头标题左对齐、行内数值
    /// 右对齐,静态 x 本来就不相等。
    ///
    /// 自证会变红(两条各自):
    /// 1. 把 `header_at()` 里的 `- offset_x` 去掉(列头不跟随);
    /// 2. 把 `ScrollArea::both()` 改回 `vertical()`(根本滚不动,前置断言先红)。
    #[test]
    fn horizontal_scroll_moves_the_header_and_the_rows_by_the_same_amount() {
        let t = crate::theme::MULLION_DARK;
        let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
        state.entries = vec![entry(b"a.txt", EntryKind::File)];
        state.load = Load::Ready;
        // 视口给得比内容总宽窄一大截,逼出横向滚动。
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 400.0));
        let mut cols = ColWidths::default();
        let ctx = egui::Context::default();
        let render = |input: egui::RawInput, state: &mut PaneState, cols: &mut ColWidths| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        "远端",
                        1,
                        PanelColumn::Remote,
                        state,
                        false,
                        BookmarkView::none(),
                        0,
                        cols,
                    );
                });
            })
        };
        let base = egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        };
        let _ = render(base.clone(), &mut state, &mut cols);
        let before = render(base.clone(), &mut state, &mut cols);
        let head_before = find_text_pos(&before.shapes, "大小")
            .expect("该画出列头「大小」")
            .x;
        let value_before = find_text_pos(&before.shapes, "1.0 KB")
            .expect("该画出大小数值")
            .x;

        // 灌一股**水平**滚轮(Shift 不需要:MouseWheel 的 delta.x 就是横向)。
        let scroll = egui::RawInput {
            screen_rect: Some(screen),
            events: vec![
                egui::Event::PointerMoved(egui::pos2(150.0, 200.0)),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(-120.0, 0.0),
                    modifiers: egui::Modifiers::default(),
                },
            ],
            ..Default::default()
        };
        let _ = render(scroll, &mut state, &mut cols);
        // 平滑滚动有插值,多跑两帧让偏移稳定。
        let _ = render(base.clone(), &mut state, &mut cols);
        let after = render(base, &mut state, &mut cols);
        let head_after = find_text_pos(&after.shapes, "大小")
            .expect("滚动后该仍画出列头「大小」")
            .x;
        let value_after = find_text_pos(&after.shapes, "1.0 KB")
            .expect("滚动后该仍画出大小数值")
            .x;

        let d_value = value_after - value_before;
        assert!(
            d_value < -1.0,
            "灌了水平滚轮,行内数值却没左移(位移 {d_value})—— 横向滚动没生效,测试前提不成立"
        );
        let d_head = head_after - head_before;
        assert!(
            (d_head - d_value).abs() < 1.0,
            "列头位移 {d_head} 与行体位移 {d_value} 对不上 —— 列头没跟着横向滚动走"
        );
    }

    /// F100/F136:横滚之后,某一列整个移出可视区,它就**不该**再出现在标注
    /// 模式的候选表里——登记一个屏幕上已经看不见、点不到的矩形,F100 会给
    /// 用户报出一个假目标。判据放宽成「但凡登记了,矩形必须落在 `header_band`
    /// 内」而不是死抠「名称」这一列会不会消失:后者跟视口宽/列宽的具体数字
    /// 绑得太死,改一下默认列宽这条就会莫名其妙红。
    ///
    /// 直接把 `ScrollArea` 的持久化偏移量写死到一个「名称列整个滚出去了」的
    /// 值,不真的去灌一串滚轮——F136 那条已经证过滚轮驱动的动画要跨很多
    /// 真实帧才收敛,这里只关心「偏移量很大时标注还对不对」,没必要跟着
    /// 陪绑那份收敛时机。
    ///
    /// 自证会变红:把 `header_at()` 里 `annotate::mark(..., rect.intersect(band))`
    /// 改回 `annotate::mark(..., rect)`(不求交,直接登记未裁剪矩形)。
    #[test]
    fn a_column_scrolled_out_of_view_is_not_registered_with_an_off_screen_rect() {
        let t = crate::theme::MULLION_DARK;
        let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
        state.entries = vec![entry(b"a.txt", EntryKind::File)];
        state.load = Load::Ready;
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 400.0));
        let mut cols = ColWidths::default();
        let ctx = egui::Context::default();
        let base = egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        };

        // 先跑一帧拿到 `show()` 内部用来拼 `ScrollArea` 持久化 id 的那个
        // 父 `Ui` id——跟生产代码里 `header_offset_x` 用的是同一份计算
        // (`ui.make_persistent_id(egui::Id::new(scroll_id_salt(...)))`)。
        let mut parent_id = None;
        let _ = ctx.run(base.clone(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                parent_id = Some(ui.id());
                show(
                    ui,
                    &t,
                    "远端",
                    1,
                    PanelColumn::Remote,
                    &mut state,
                    false,
                    BookmarkView::none(),
                    0,
                    &mut cols,
                );
            });
        });
        let scroll_id = parent_id
            .expect("该拿到 Ui id")
            .with(egui::Id::new(scroll_id_salt("远端", 1)));

        // 直接把持久化偏移写死成「名称列(宽 220)整个滚出去」的量,不用等
        // 滚轮动画收敛。
        let mut seeded = egui::scroll_area::State::default();
        seeded.offset.x = ColWidths::default().name + 30.0;
        seeded.store(&ctx, scroll_id);

        annotate::toggle(&ctx);
        // egui 的 `Panel`/`Area` 首帧有 fade-in,多跑一帧让矩形稳定
        // (同文件其它标注测试的既有做法)。
        let _ = ctx.run(base.clone(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                show(
                    ui,
                    &t,
                    "远端",
                    1,
                    PanelColumn::Remote,
                    &mut state,
                    false,
                    BookmarkView::none(),
                    0,
                    &mut cols,
                );
            });
        });
        let _ = ctx.run(base, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                show(
                    ui,
                    &t,
                    "远端",
                    1,
                    PanelColumn::Remote,
                    &mut state,
                    false,
                    BookmarkView::none(),
                    0,
                    &mut cols,
                );
            });
        });

        let band = annotate::spot_rect(&ctx, "文件面板/远端/列头").expect("列头横带该登记");
        let paths = annotate::spot_paths(&ctx);
        let col_paths: Vec<&String> = paths
            .iter()
            .filter(|p| p.starts_with("文件面板/远端/列头/"))
            .collect();
        assert!(
            !col_paths.is_empty(),
            "至少该有几列还留在可视区内,候选表却是空的:{paths:?}"
        );
        for p in col_paths {
            let r = annotate::spot_rect(&ctx, p).unwrap_or_else(|| panic!("{p} 该找得到矩形"));
            assert!(
                band.contains_rect(r),
                "{p} 登记的矩形 {r:?} 超出了列头横带 {band:?} —— 横向滚动后 \
                 F100 报出了一个屏幕上已经看不见的候选"
            );
        }
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
            session_bound: false,
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

    /// 跑一帧远端栏,把书签视图喂进去,拿回动作和这一帧的形状树。
    ///
    /// 书签相关的守护全要「先跑一帧稳定布局,再拿坐标点下去」(egui Panel
    /// 首帧是 sizing pass),所以统一在这里收口。
    fn run_remote(
        ctx: &egui::Context,
        state: &mut PaneState,
        cols: &mut ColWidths,
        bookmarks: &[mullion_store::Bookmark],
        can_edit: bool,
        input: egui::RawInput,
    ) -> (Option<FileAction>, Vec<egui::epaint::ClippedShape>) {
        let t = crate::theme::MULLION_DARK;
        let mut action = None;
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                action = show(
                    ui,
                    &t,
                    "远端",
                    1,
                    PanelColumn::Remote,
                    state,
                    false,
                    BookmarkView {
                        list: bookmarks,
                        can_edit,
                    },
                    0,
                    cols,
                );
            });
        });
        (action, out.shapes)
    }

    /// 书签下拉按钮的位置。F143 之后它是自绘三角、没有任何文字,
    /// `find_text_pos` 找不到它了 —— 只能靠 annotate 记下的矩形。
    ///
    /// 调用前 `ctx` 必须已 `annotate::toggle`(否则 `mark` 直接 return),
    /// 且至少跑过一帧。
    fn bookmark_arrow(ctx: &egui::Context) -> egui::Pos2 {
        annotate::spot_rect(ctx, "文件面板/远端/路径/书签")
            .expect("有书签时该画下拉按钮")
            .center()
    }

    /// 打开书签下拉,在随后几帧里找 `needle` 这段文字,返回它的位置。
    ///
    /// 三条书签测试共用。菜单要到**下一帧**才画得出来,而具体是第几帧
    /// 取决于 egui 的 Area 布局,所以这里最多试三帧。
    ///
    /// 会自己开标注模式 —— 下拉按钮 F143 之后是自绘三角、没有文字,
    /// 只能靠 annotate 记下的矩形定位,而 `mark` 只在模式开着时才登记。
    fn open_bookmark_menu_and_find(
        ctx: &egui::Context,
        state: &mut PaneState,
        cols: &mut ColWidths,
        marks: &[mullion_store::Bookmark],
        needle: &str,
    ) -> Option<egui::Pos2> {
        if !annotate::is_on(ctx) {
            annotate::toggle(ctx);
        }
        run_remote(ctx, state, cols, marks, true, egui::RawInput::default());
        let arrow = bookmark_arrow(ctx);
        run_remote(ctx, state, cols, marks, true, click_at(arrow));
        for _ in 0..3 {
            let (_, shapes) = run_remote(ctx, state, cols, marks, true, egui::RawInput::default());
            if let Some(p) = find_text_pos(&shapes, needle) {
                return Some(p);
            }
        }
        None
    }

    /// 一次完整的左键点击(按下 + 抬起)。`PointerMoved` 不能省:egui 的
    /// 交互靠指针位置,只发按钮事件时 hover 判定拿不到坐标。
    fn click_at(pos: egui::Pos2) -> egui::RawInput {
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::PointerMoved(pos));
        for pressed in [true, false] {
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            });
        }
        input
    }

    fn ready_at(path: &[u8]) -> PaneState {
        let mut state = PaneState::new(RemotePath::from_bytes(path.to_vec()));
        state.load = Load::Ready;
        state
    }

    /// F139:当前目录没被收藏时路径条给的是空心 ☆,点它发出 `BookmarkAdd`,
    /// 默认名取路径末段(不是整条路径 —— 下拉里一长串没法认)。
    #[test]
    fn clicking_the_hollow_star_bookmarks_the_current_directory() {
        let ctx = egui::Context::default();
        let mut state = ready_at(b"/var/log");
        let mut cols = ColWidths::default();
        let (_, shapes) = run_remote(
            &ctx,
            &mut state,
            &mut cols,
            &[],
            true,
            egui::RawInput::default(),
        );
        let pos = find_text_pos(&shapes, "☆").expect("没收藏时该画空心星");
        let (action, _) = run_remote(&ctx, &mut state, &mut cols, &[], true, click_at(pos));
        assert_eq!(
            action,
            Some(FileAction::BookmarkAdd {
                name: "log".into(),
                path: "/var/log".into(),
            }),
            "点空心星该收藏当前目录,默认名取末段"
        );
    }

    /// F139:已收藏时是实心 ★,再点 = 取消收藏(设计定的切换语义)。
    /// ★/☆ 靠「cwd 在不在列表里」现算,这条同时守住那个判定没写反。
    #[test]
    fn clicking_the_filled_star_removes_that_bookmark() {
        let ctx = egui::Context::default();
        let mut state = ready_at(b"/var/log");
        let mut cols = ColWidths::default();
        let marks = vec![mullion_store::Bookmark {
            name: "日志".into(),
            path: "/var/log".into(),
        }];
        let (_, shapes) = run_remote(
            &ctx,
            &mut state,
            &mut cols,
            &marks,
            true,
            egui::RawInput::default(),
        );
        let pos = find_text_pos(&shapes, "★").expect("已收藏的目录该画实心星");
        let (action, _) = run_remote(&ctx, &mut state, &mut cols, &marks, true, click_at(pos));
        assert_eq!(
            action,
            Some(FileAction::BookmarkRemove {
                path: "/var/log".into()
            }),
            "点实心星该取消收藏"
        );
    }

    /// F139:标签不是从已保存的会话开出来的(CLI 直连)时没地方存书签,
    /// ☆ 必须置灰 —— 能点但存不下去是最糟的一种:用户以为收藏成功了。
    #[test]
    fn the_star_is_disabled_when_the_tab_is_not_bound_to_a_session() {
        let ctx = egui::Context::default();
        let mut state = ready_at(b"/var/log");
        let mut cols = ColWidths::default();
        let (_, shapes) = run_remote(
            &ctx,
            &mut state,
            &mut cols,
            &[],
            false,
            egui::RawInput::default(),
        );
        let pos = find_text_pos(&shapes, "☆").expect("置灰也要画出来,不是藏起来");
        let (action, _) = run_remote(&ctx, &mut state, &mut cols, &[], false, click_at(pos));
        assert_eq!(action, None, "没有会话记录时点星不该发出任何书签动作");
    }

    /// F139:书签全走下拉(横排书签栏已删)。点开再点其中一条,
    /// 要发出指向它 `path` 的 `Goto`。
    #[test]
    fn picking_a_bookmark_from_the_dropdown_emits_goto() {
        let ctx = egui::Context::default();
        let mut state = ready_at(b"/");
        let mut cols = ColWidths::default();
        let marks = vec![mullion_store::Bookmark {
            name: "日志".into(),
            path: "/var/log".into(),
        }];
        // F145 之后菜单项的主文本是路径,不是名字 —— 按路径找。
        let item = open_bookmark_menu_and_find(&ctx, &mut state, &mut cols, &marks, "/var/log")
            .expect("下拉里该有这一条");
        let (action, _) = run_remote(&ctx, &mut state, &mut cols, &marks, true, click_at(item));
        assert_eq!(
            action,
            Some(FileAction::Goto(RemotePath::from_bytes(
                b"/var/log".to_vec()
            ))),
            "点下拉里的书签该发出指向它 path 的 Goto"
        );
    }

    /// 空名字是 store 明确允许的合法状态(`Bookmark::name` 文档),下拉里
    /// 不能画一条没有任何文字、根本点不中的项。
    ///
    /// F145 之后主文本恒是路径,这条自然成立 —— 留着是因为它守的是
    /// 「空名不产生空项」,那跟主文本取什么无关,换回名字优先时还得靠它。
    #[test]
    fn a_bookmark_with_an_empty_name_falls_back_to_showing_its_path() {
        let ctx = egui::Context::default();
        let mut state = ready_at(b"/");
        let mut cols = ColWidths::default();
        let marks = vec![mullion_store::Bookmark {
            name: String::new(),
            path: "/srv/app".into(),
        }];
        let found =
            open_bookmark_menu_and_find(&ctx, &mut state, &mut cols, &marks, "/srv/app").is_some();
        assert!(found, "空名字的书签要回退显示路径");
    }

    /// F146:本地栏不画「属主」列。`files/local.rs` 构造 `Entry` 时
    /// `uid`/`gid` **恒填 0**,本地栏的属主在数据源头上就不存在 ——
    /// `owner_text` 因此对本地栏恒返回破折号,画出来是一整列破折号,
    /// 白占 120pt 又什么都不说。
    ///
    /// 判据按**栏**静态,不按数据:「本栏所有条目 uid==0」这种动态判据会
    /// 让远端一个全 root 的目录(`/etc` 之类很常见)莫名其妙少一列,
    /// 切个目录又冒出来,列宽还跟着跳。
    ///
    /// 自证会变红:把 `col_lefts` 的 `column` 入参忽略掉,恒返回五列。
    #[test]
    fn the_local_column_has_no_owner_column_but_the_remote_one_does() {
        let local = col_lefts(&ColWidths::default(), PanelColumn::Local);
        let remote = col_lefts(&ColWidths::default(), PanelColumn::Remote);
        assert!(
            !local.iter().any(|(label, ..)| *label == "属主"),
            "本地栏画了属主列,实得:{:?}",
            local.iter().map(|(l, ..)| *l).collect::<Vec<_>>()
        );
        assert!(
            remote.iter().any(|(label, ..)| *label == "属主"),
            "远端栏丢了属主列"
        );
        assert_eq!(local.len(), 4);
        assert_eq!(remote.len(), 5);
    }

    /// 内容总宽必须跟着少一列 —— 不跟的话本地栏会多出 120pt 的空白可滚
    /// 区域,横向滚动条比内容长。
    ///
    /// 自证会变红:把 `content_w` 里的 `column` 入参忽略掉。
    #[test]
    fn the_local_content_width_drops_the_owner_column_too() {
        let c = ColWidths::default();
        assert_eq!(
            content_w(&c, PanelColumn::Remote) - content_w(&c, PanelColumn::Local),
            c.owner,
            "两栏总宽之差应当正好是属主列宽"
        );
    }

    /// F145:书签下拉里每一条显示的是**完整绝对路径**,不是文件夹名。
    /// 用户点开下拉是为了确认「这条书签到底指哪儿」—— 只给个 `nginx`
    /// 等于没回答这个问题(同名目录在不同机器、不同层级下遍地都是)。
    ///
    /// 自证会变红:把 `show()` 里那个 `let label = b.path.as_str();`
    /// 改回按 `b.name` 优先。
    #[test]
    fn the_bookmark_menu_shows_the_full_path_not_just_the_folder_name() {
        let ctx = egui::Context::default();
        let mut state = ready_at(b"/");
        let mut cols = ColWidths::default();
        let marks = vec![mullion_store::Bookmark {
            name: "日志".into(),
            path: "/var/log/nginx".into(),
        }];
        assert!(
            open_bookmark_menu_and_find(&ctx, &mut state, &mut cols, &marks, "/var/log/nginx")
                .is_some(),
            "书签菜单里没有完整路径"
        );
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
    /// F138:面板内容不能贴着外框画。判据取**真值** —— 「↑」按钮实际画出来
    /// 的位置,与面板外框的左缘相比,至少留出 `SP_S`。
    ///
    /// 不拿常量断言常量(那是重言式、恒绿):把两个宿主的 `inner_margin`
    /// 删掉之后,按钮会紧贴面板左缘,这条必红。
    #[test]
    fn the_panel_does_not_draw_its_contents_flush_against_its_own_edge() {
        let t = crate::theme::MULLION_DARK;
        let mut frame = two_columns();
        let ctx = egui::Context::default();
        let mut cols = ColWidths::default();
        // 三帧:egui 的 Panel 首帧是 sizing pass,rect 还没稳定。面板外框和
        // 「↑」的位置**必须取自同一帧**,否则比的是两套布局。
        let mut out = ctx.run(raw(None), |ctx| {
            content(ctx, &t, 1, false, &mut frame, 0, &mut cols, &mut None);
        });
        for _ in 0..2 {
            out = ctx.run(raw(None), |ctx| {
                content(ctx, &t, 1, false, &mut frame, 0, &mut cols, &mut None);
            });
        }
        let mut lefts: Vec<f32> = out
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::epaint::Shape::Rect(r) => Some(r.rect.left()),
                _ => None,
            })
            .collect();
        lefts.sort_by(|a, b| a.partial_cmp(b).expect("坐标不该是 NaN"));
        // 最靠左的矩形 = 面板外框(`CentralPanel` 的 `Frame` 背景);
        // 第一个**比它靠右**的矩形 = 最靠左的内容(路径条那几个按钮的底)。
        let panel_left = *lefts
            .first()
            .expect("一个矩形都没画出来 —— 脚手架本身有问题");
        let content_left = lefts
            .iter()
            .copied()
            .find(|x| *x > panel_left)
            .expect("除了外框什么都没画");
        assert!(
            content_left >= panel_left + crate::ui::metrics::SP_S,
            "内容贴着面板边缘画(最左内容={content_left}, 面板左缘={panel_left}),\
             F138 要求至少留 {} 点内边距",
            crate::ui::metrics::SP_S
        );
        // 「↑」是路径条的第一个控件,它必须落在内容区里 —— 上面那条只保证
        // 「有个东西没贴边」,这条把它钉到具体控件上。
        let arrow = find_text_pos(&out.shapes, "↑").expect("路径条的「↑」没画出来");
        assert!(
            arrow.x >= content_left,
            "「↑」画到了内容区左缘之外(x={}, 内容左缘={content_left})",
            arrow.x
        );
    }

    /// F144:控件不许贴着**裁剪边**画。贴着画的话圆角描边的外半像素会被
    /// `clip_rect` 切掉,用户看到的是「↑ 按钮左边缺了 1/4 圆弧」「路径条
    /// 控件没有上边框」(v0.1.56 实测报的两条)。
    ///
    /// 跟 `the_panel_does_not_draw_its_contents_flush_against_its_own_edge`
    /// 不是一回事:那条比的是「内容 vs 面板**外框**」,F138 的 `inner_margin`
    /// 已经解决;这条比的是「内容 vs 本栏**裁剪区**」—— 两栏是在 margin
    /// 之内再切一刀的,那一刀上没有任何留白。
    ///
    /// 判据取**真值**:拿「↑」按钮这一帧实际画出来的矩形,和这一帧实际
    /// 作用在它身上的 `clip_rect`(`ClippedShape` 自带)比。向外扩 1pt
    /// 覆盖描边宽度。不比 margin 常量 —— 那是拿常量断言常量。
    ///
    /// 自证会变红:把 `content()` 里 `left`/`right` 两个 rect 的
    /// `.shrink2(..)` 去掉。
    #[test]
    fn panel_content_does_not_touch_the_clip_edge() {
        let t = crate::theme::MULLION_DARK;
        let mut frame = two_columns();
        let ctx = egui::Context::default();
        let mut cols = ColWidths::default();
        // 三帧,理由同上面那条:首帧是 sizing pass。
        let mut out = ctx.run(raw(None), |ctx| {
            content(ctx, &t, 1, false, &mut frame, 0, &mut cols, &mut None);
        });
        for _ in 0..2 {
            out = ctx.run(raw(None), |ctx| {
                content(ctx, &t, 1, false, &mut frame, 0, &mut cols, &mut None);
            });
        }
        let up = find_text_pos(&out.shapes, "↑").expect("路径条的「↑」没画出来");
        // 框住这个「↑」的最小矩形 = 按钮底色。取最小的那个,不然会选中
        // 面板外框(它也包含这个点)。
        let (btn, clip) = out
            .shapes
            .iter()
            .filter_map(|cs| match &cs.shape {
                egui::epaint::Shape::Rect(r) if r.rect.contains(up) => Some((r.rect, cs.clip_rect)),
                _ => None,
            })
            .min_by(|a, b| (a.0.area()).total_cmp(&b.0.area()))
            .expect("「↑」没有按钮底 —— 脚手架本身有问题");
        assert!(
            clip.contains_rect(btn.expand(1.0)),
            "「↑」按钮 {btn:?} 贴着裁剪边 {clip:?} —— 圆角描边的外半像素会被切掉"
        );
    }

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

    /// 带修饰键的点击。**修饰键必须写进 `RawInput::modifiers`**,不是只写进
    /// 事件里的那份 —— `files_panel` 读的是 `ui.input(|i| i.modifiers)`,
    /// 即全局状态。只设事件里那份的话,Ctrl 位根本传不到 `click_row`,
    /// 多选静默失效而所有断言照样绿。
    fn press_mod(
        pos: egui::Pos2,
        time: f64,
        pressed: bool,
        modifiers: egui::Modifiers,
    ) -> egui::RawInput {
        let mut input = raw(Some(time));
        input.modifiers = modifiers;
        input.events.push(egui::Event::PointerMoved(pos));
        input.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers,
        });
        input
    }

    /// Windows/Linux 上 egui 把 Ctrl 归一化到 `command` 位,`files_panel`
    /// 读的正是 `command`(写 `ctrl` 会让 macOS 用户点不出多选)。两位都置上,
    /// 与真实平台一致。
    fn ctrl() -> egui::Modifiers {
        egui::Modifiers {
            command: true,
            ctrl: true,
            ..Default::default()
        }
    }

    /// F150:**这是「Ctrl 多选」唯一的端到端守护。**
    ///
    /// `click_row` 的 Ctrl 语义在 `files::state` 里有单测,但从来没有一条
    /// 测试证明「点击真的把 ctrl 位带进去了」—— 中间隔着
    /// `ui.input(|i| i.modifiers)` 这一步,读错来源(比如读事件里那份而不是
    /// 全局状态)会让多选整个不成立,而 `files::state` 那些单测全绿。
    ///
    /// 断言打在**状态行文字**上:那是用户唯一看得见的选中证据。
    #[test]
    fn ctrl_clicking_a_second_row_adds_it_to_the_selection() {
        let t = crate::theme::MULLION_DARK;
        let mut frame = two_columns();
        let ctx = egui::Context::default();
        let mut cols = ColWidths::default();
        let mut render = |input: egui::RawInput, frame: &mut PanelFrame| {
            ctx.run(input, |ctx| {
                content(ctx, &t, 1, true, frame, 0, &mut cols, &mut None);
            })
        };
        let _ = render(raw(None), &mut frame);
        let out = render(raw(None), &mut frame);
        let b = find_text_pos(&out.shapes, "b.txt").expect("远端栏该画出 b.txt");
        let logs = find_text_pos(&out.shapes, "logs").expect("远端栏该画出 logs");

        // 平点 b.txt。
        let _ = render(press(b, 1.0, true), &mut frame);
        let _ = render(press(b, 1.1, false), &mut frame);
        // Ctrl 点 logs —— 该是「再加一条」,不是「换成这一条」。
        let _ = render(press_mod(logs, 1.2, true, ctrl()), &mut frame);
        let _ = render(press_mod(logs, 1.3, false, ctrl()), &mut frame);

        let out = render(raw(None), &mut frame);
        let texts: Vec<String> = out
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::epaint::Shape::Text(ts) => Some(ts.galley.text().to_owned()),
                _ => None,
            })
            .collect();
        assert!(
            texts.iter().any(|s| s == "已选 2 项 · 1.0 KB"),
            "Ctrl 点第二行该把它加进选择集,状态行该显示选中态文案;\
             实际画出来的是 {texts:?}"
        );
    }

    /// F150:`.max_height(body_h)` 的守护。**判据是状态行画不画得出来,不是
    /// 它画在哪**——`content_renders_both_panels_with_their_own_state` 那类
    /// 测试用的窗口是 1200×800,一个高窗口 + 只有两三行数据的栏,`auto_shrink
    /// ([false, false])` 会让 `ScrollArea` 在没有 `max_height` 时把可用高度
    /// 全部吃满(远超实际内容需要的高度)。这时 `ScrollArea` 结束后 `ui` 的
    /// 布局光标被推到一个离谱的 y 坐标,紧跟着画的状态行 `colored_label`
    /// 整个从渲染输出里消失(插桩实测过:`find_text_pos` 找不到「N 项」这个
    /// 词,不是「位置偏了」,是「根本没有这个部件」)。
    ///
    /// 自证会变红(两种坏法都试过,见 Task 5 复核记录):
    /// 1. 把 `.max_height(body_h)` 换成 `.max_height(f32::INFINITY)`。
    /// 2. 把这一行整个删掉(`body_h` 变成未使用变量,加 `let _ = body_h;`
    ///    绕开 clippy 以免混淆信号)。
    #[test]
    fn the_status_row_only_renders_when_the_scroll_area_height_is_capped() {
        let t = crate::theme::MULLION_DARK;
        let mut frame = two_columns();
        let ctx = egui::Context::default();
        let mut cols = ColWidths::default();
        let mut render = |input: egui::RawInput, frame: &mut PanelFrame| {
            ctx.run(input, |ctx| {
                content(ctx, &t, 1, true, frame, 0, &mut cols, &mut None);
            })
        };
        let _ = render(raw(None), &mut frame);
        let out = render(raw(None), &mut frame);
        assert!(
            find_text_pos(&out.shapes, "2 项").is_some(),
            "远端栏两条都没选中,状态行该显示「2 项」;`ScrollArea` 没有
             `max_height` 兜底时,这个部件会从渲染输出里整个消失"
        );
    }

    /// F150:钉住「状态行必须画在 `header_at` 补画列头之前」这条不变量
    /// (`show()` 里那段长注释)。此前这条只写在注释里,没有任何断言 ——
    /// 把状态行那两行挪到 `header_at(...)` 之后,既有 70 条测试全绿。
    ///
    /// 判据是**相对位置**,不是绝对像素:
    /// - `status.y > last_row.y`:状态行必须落在最后一行内容**下方**。
    /// - `status.y > header.y + ROW_H`:状态行与列头之间至少隔开一整行高。
    ///   两条一起判是因为单看「低于列头」不够狠 —— `header_at` 出 bug 时
    ///   状态行会被拽到贴着列头的位置,`status.y` 仍然略大于 `header.y`
    ///   (只差几像素),只判「大于」测不出这种「贴着但没真的排到栏底」的坏法,
    ///   必须再加一条「差距要有一整行那么大」。
    ///
    ///   不用绝对阈值(比如「`status.y` 必须 > 300」):窗口尺寸、行数、
    ///   `raw()` 给的 1200×800 只要改一个,阈值就得跟着重算,而相对关系
    ///   (「状态行在列头下方一整行开外」「状态行在最后一行内容下方」)
    ///   在任何窗口尺寸下都成立。
    ///
    /// 三个锚点都取远端栏独有的文字,避免撞到本地栏同名的列头/计数:
    /// - `"属主"`:F146 本地栏没有这一列,只有远端栏画得出来。
    /// - `"b.txt"`:只在远端栏的测试数据里。
    /// - `"2 项"`:远端栏两条、本地栏一条,计数不会撞。
    #[test]
    fn the_status_row_is_drawn_below_the_last_row_not_under_the_header() {
        let t = crate::theme::MULLION_DARK;
        let mut frame = two_columns();
        let ctx = egui::Context::default();
        let mut cols = ColWidths::default();
        let mut render = |input: egui::RawInput, frame: &mut PanelFrame| {
            ctx.run(input, |ctx| {
                content(ctx, &t, 1, true, frame, 0, &mut cols, &mut None);
            })
        };
        // 两帧稳定布局(egui Panel 首帧 fade_in 只记 Shape::Noop,同本文件
        // 其余测试的既定做法)。
        let _ = render(raw(None), &mut frame);
        let out = render(raw(None), &mut frame);

        let header = find_text_pos(&out.shapes, "属主").expect("远端栏该有「属主」列头");
        let last_row = find_text_pos(&out.shapes, "b.txt").expect("远端栏该画出 b.txt 这一行");
        let status = find_text_pos(&out.shapes, "2 项").expect("远端栏状态行该显示「2 项」");

        assert!(
            status.y > last_row.y,
            "状态行 y={} 应落在最后一行内容 y={} 下方 —— 状态行被画到了
             行体上面或与之重叠,像是排到了栏中间而不是栏底",
            status.y,
            last_row.y
        );
        assert!(
            status.y > header.y + ROW_H,
            "状态行 y={} 应比列头 y={} 至少低一个 ROW_H —— 差距不够说明状态行
             被 `header_at` 的 `scope_builder` 拽回了列头附近(收尾时
             `advance_cursor_after_rect` 对 TopDown 是硬赋值),而不是排在
             `ScrollArea` 内容之后",
            status.y,
            header.y
        );
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
        let mut cols = ColWidths::default();
        let mut render = |input: egui::RawInput, frame: &mut PanelFrame| {
            let mut acts = (None, None);
            let out = ctx.run(input, |ctx| {
                acts = content(ctx, &t, 1, true, frame, 0, &mut cols, &mut None);
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
        let mut cols = ColWidths::default();
        let mut render = |input: egui::RawInput, frame: &mut PanelFrame| {
            let mut acts = (None, None);
            let out = ctx.run(input, |ctx| {
                acts = content(ctx, &t, 1, true, frame, 0, &mut cols, &mut None);
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
        let mut cols = ColWidths::default();
        let mut render = |input: egui::RawInput, frame: &mut PanelFrame| {
            let mut acts = (None, None);
            let out = ctx.run(input, |ctx| {
                acts = content(ctx, &t, 1, true, frame, 0, &mut cols, &mut None);
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
        let mut cols = ColWidths::default();
        let mut render = |input: egui::RawInput, frame: &mut PanelFrame| {
            let mut acts = (None, None);
            let out = ctx.run(input, |ctx| {
                acts = content(ctx, &t, 1, true, frame, 0, &mut cols, &mut None);
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

    /// F151:多选拖拽途中,指针旁边该跟着画出「拖动 N 项」。
    ///
    /// `preview_label` 本身有纯函数测试,但接线在 `content()` 里 —— 判据
    /// 取反了(比如画成 `incoming` 而不是 `outgoing`)、层选错了(画在
    /// 当前 `ui` 的 painter 上被另一栏背景盖掉)都不会被那条纯函数测试逮到。
    #[test]
    fn a_multi_item_drag_paints_a_running_count_next_to_the_pointer() {
        let t = crate::theme::MULLION_DARK;
        let mut frame = two_columns();
        frame
            .remote
            .selected
            .insert(RemotePath::from_bytes(b"logs".to_vec()));
        frame
            .remote
            .selected
            .insert(RemotePath::from_bytes(b"b.txt".to_vec()));
        let ctx = egui::Context::default();
        let mut cols = ColWidths::default();
        let mut render = |input: egui::RawInput, frame: &mut PanelFrame| {
            ctx.run(input, |ctx| {
                content(ctx, &t, 1, true, frame, 0, &mut cols, &mut None);
            })
        };
        let _ = render(raw(None), &mut frame);
        let out = render(raw(None), &mut frame);
        let src = find_text_pos(&out.shapes, "b.txt").expect("远端栏该画出 b.txt");
        let dst = find_text_pos(&out.shapes, "a.txt").expect("本地栏该画出 a.txt");

        // 按下已选中的 b.txt(不改变选中集,仍是 {logs, b.txt} 两项)→ 移开。
        // `outgoing` 判据读的是 `DragAndDrop::payload`,而 `dnd_set_drag_payload`
        // 要到本帧行循环里才写入 —— 同一帧内前者读到的还是上一帧的值,所以
        // 起拖要多渲染一帧,预览才追得上。
        let _ = render(press(src, 1.0, true), &mut frame);
        let _ = render(moved(dst, 1.1), &mut frame);
        let out = render(moved(dst, 1.2), &mut frame);

        assert!(
            find_text_pos(&out.shapes, "拖动 2 项").is_some(),
            "两条一起拖,指针旁该显示「拖动 2 项」,而不是空手拖"
        );
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
        let mut cols = ColWidths::default();
        let mut render = |input: egui::RawInput, frame: &mut PanelFrame| {
            let mut acts = (None, None);
            let out = ctx.run(input, |ctx| {
                acts = content(ctx, &t, 1, true, frame, 0, &mut cols, &mut None);
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
        let mut cols = ColWidths::default();
        let mut render = |input: egui::RawInput, state: &mut PaneState| {
            let mut action = None;
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    action = show(
                        ui,
                        &t,
                        "远端",
                        1,
                        PanelColumn::Remote,
                        state,
                        false,
                        BookmarkView::none(),
                        0,
                        &mut cols,
                    );
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
        let mut cols = ColWidths::default();
        let mut render = |input: egui::RawInput, state: &mut PaneState| {
            let mut action = None;
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    action = show(
                        ui,
                        &t,
                        "远端",
                        1,
                        PanelColumn::Remote,
                        state,
                        false,
                        BookmarkView::none(),
                        0,
                        &mut cols,
                    );
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
            let mut cols = ColWidths::default();
            let mut local = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
            local.entries = vec![entry(b"a.txt", EntryKind::File)];
            local.load = Load::Ready;
            let mut render_local = |input: egui::RawInput, state: &mut PaneState| {
                let mut action = None;
                let out = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        action = show(
                            ui,
                            &t,
                            "本地",
                            1,
                            PanelColumn::Local,
                            state,
                            false,
                            BookmarkView::none(),
                            0,
                            &mut cols,
                        );
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
        let mut cols = ColWidths::default();
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
                        BookmarkView::none(),
                        0,
                        &mut cols,
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
        let mut cols = ColWidths::default();
        for _ in 0..3 {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |ctx| {
                    content(ctx, &t, 7, true, &mut frame, 0, &mut cols, &mut None);
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
        // 分母是侧栏**总高**,历史原因:「远端」那次 `show()` 曾经没包
        // `allocate_ui`,它内部 `annotate::mark` 记的是整条侧栏未裁剪的
        // `ui.max_rect()`(恒等于侧栏总高)——拿它跟本地栏比,无论 `sidebar()`
        // 里的比例改成几比几,这条断言都恒真(复核实测把 `h * 0.4` 改成
        // `h * 0.9` 照样绿)。那个几何事实后来被认定为真 bug(它让远端栏的
        // 交互层罩住本地栏,见 `the_sidebar_local_column_still_receives_clicks_
        // under_the_remote_column`)并已修掉,但分母仍留在总高上:参照系少一层
        // 依赖更稳。守住
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

    /// 侧栏宿主里,本地栏的按钮与路径条必须真的点得中。
    ///
    /// v0.1.55 用户实测的真 bug:`sidebar()` 里本地栏包在 `allocate_ui` 里、
    /// 矩形被限死,**远端栏那次 `show()` 却直接拿外层 `ui`** —— 它的
    /// `ui.max_rect()` 恒等于**整条侧栏**(上面那条测试的注释早就记下了这个
    /// 几何事实,只是当时只当成「断言参照系不能用它」)。于是 `show()` 开头
    /// 那个覆盖整栏的右键菜单宿主(`ui.interact(ui.max_rect(), ..)`)罩住了
    /// 本地栏;它注册在本地栏所有控件**之后**,同层后注册者压在上面,本地栏的
    /// ↑/⟳/☆/▾ 与路径条全部收不到点击。`show()` 自己那条注释("挂到栏尾会把
    /// 整栏罩住,书签按钮和行的左键点击全被它吃掉")说的正是这个失效模式,
    /// 只是没料到它会从**隔壁栏**罩过来。
    ///
    /// 标签宿主 `content()` 没这个问题:两栏各自 `scope_builder` 限死矩形。
    ///
    /// 自证会变红:把 `sidebar()` 里远端栏那次 `show()` 的 `allocate_ui`
    /// 包装去掉、改回直接 `show(ui, ..)`。
    #[test]
    fn the_sidebar_local_column_still_receives_clicks_under_the_remote_column() {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        annotate::toggle(&ctx);
        let mut ui_state = crate::ui::UiState::default();
        let mut frame = PanelFrame::default();
        frame.local.entries = vec![entry(b"local-a.txt", EntryKind::File)];
        frame.local.load = Load::Ready;
        frame.remote.entries = vec![entry(b"remote-a.txt", EntryKind::File)];
        frame.remote.load = Load::Ready;
        // 侧栏用真实窗口尺寸(见 `clicking_the_name_header_...` 的同款注释)。
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(360.0, 700.0));
        let base = || egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        };
        let click_at = |pos: egui::Pos2| {
            let mut input = base();
            input.events.push(egui::Event::PointerMoved(pos));
            for pressed in [true, false] {
                input.events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::default(),
                });
            }
            input
        };
        let run =
            |input: egui::RawInput, frame: &mut PanelFrame, ui_state: &mut crate::ui::UiState| {
                let mut acts = (None, None);
                let out = ctx.run(input, |ctx| {
                    acts = sidebar(ctx, &t, ui_state, 7, true, frame, 0);
                });
                (acts, out)
            };

        // 部件矩形要上一帧的布局结果才存在,先预热(切片 G 的坑)。
        let mut shapes = Vec::new();
        for _ in 0..3 {
            let (_, out) = run(base(), &mut frame, &mut ui_state);
            shapes = out.shapes;
        }

        // 侧栏里本地栏在上、先画,所以第一个 ↑ 就是本地那个。
        let up = find_text_pos(&shapes, "↑").expect("路径条上该有 ↑ 按钮");
        assert!(
            up.y < screen.center().y,
            "取到的 ↑ 不在侧栏上半部(本地栏),测试选错了目标: y={}",
            up.y
        );
        let ((_, local_act), _) = run(click_at(up), &mut frame, &mut ui_state);
        assert_eq!(
            local_act,
            Some(FileAction::Up),
            "点本地栏的 ↑ 没反应 —— 多半是被远端栏那个覆盖整条侧栏的交互层吃掉了"
        );

        // 路径条同理:它是 F131 唯一的入口,点不中等于这个功能不存在。
        // 取行首往下一点、行尾往左一点 —— 那片是路径标签的命中区(按钮都在左边)。
        let row = annotate::spot_rect(&ctx, "文件面板/本地/路径").expect("本地栏路径条没画");
        let pos = egui::pos2(row.max.x - 8.0, row.min.y + 6.0);
        assert!(frame.local.path_edit.is_none(), "前提:此刻不该在编辑态");
        let _ = run(click_at(pos), &mut frame, &mut ui_state);
        assert!(
            frame.local.path_edit.is_some(),
            "点本地栏路径条没进编辑态 —— 同上,交互被隔壁栏罩住了"
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

        // --- 嫌疑 1:裁剪矩形不能伸进隔壁栏。 -------------------------
        //
        // 判据比的是**隔壁栏的边界**,不是本栏自己的:F144 之后本栏的
        // `max_rect` 比裁剪区窄 `SP_XS`(内容内缩、裁剪区照旧),拿本栏自己
        // 的几何当上界会把那 4pt 判成越界 —— 而那 4pt 落在两栏之间的 gap
        // 里,一个像素也到不了隔壁栏。这条守的是「串画」,gap 不算串画。
        let clip_within_own_bounds = |active: PanelColumn| -> (egui::Rect, egui::Rect, egui::Rect) {
            let ctx = egui::Context::default();
            annotate::toggle(&ctx);
            let mut frame = PanelFrame {
                remote: PaneState::new(RemotePath::from_bytes(b"/".to_vec())),
                local: PaneState::new(RemotePath::from_bytes(b"/".to_vec())),
                bookmarks: Vec::new(),
                session_bound: false,
                active_column: active,
            };
            frame.local.entries = vec![entry(b"local-a.txt", EntryKind::File)];
            frame.local.load = Load::Ready;
            frame.remote.entries = vec![entry(b"remote-a.txt", EntryKind::File)];
            frame.remote.load = Load::Ready;
            let mut cols = ColWidths::default();

            let mut out = None;
            for _ in 0..3 {
                out = Some(ctx.run(
                    egui::RawInput {
                        screen_rect: Some(screen),
                        ..Default::default()
                    },
                    |ctx| {
                        content(ctx, &t, 7, true, &mut frame, 0, &mut cols, &mut None);
                    },
                ));
            }
            let out = out.unwrap();
            let geo = |p: &str| annotate::spot_rect(&ctx, p).expect("这一栏没画");
            // 裁剪区来自**当前聚焦那一栏**的边框(下面 `active` 决定聚焦谁)。
            let clip =
                find_stroke_clip(&out.shapes, accent).expect("聚焦边框没画出来(focused 没生效)");
            (geo("文件面板/本地"), geo("文件面板/远端"), clip)
        };

        let (_, remote_geo, local_clip) = clip_within_own_bounds(PanelColumn::Local);
        assert!(
            local_clip.max.x <= remote_geo.min.x + 0.5,
            "本地栏的裁剪矩形伸进了远端栏,内容会画进右栏: \
             clip.max.x={} 远端栏 min.x={}",
            local_clip.max.x,
            remote_geo.min.x
        );

        let (local_geo, _, remote_clip) = clip_within_own_bounds(PanelColumn::Remote);
        assert!(
            remote_clip.min.x >= local_geo.max.x - 0.5,
            "远端栏的裁剪矩形伸进了本地栏,内容会画进左栏: \
             clip.min.x={} 本地栏 max.x={}",
            remote_clip.min.x,
            local_geo.max.x
        );

        // --- 嫌疑 2:往左栏灌一次滚轮,右栏的行位置不该跟着挪。 --------
        let ctx = egui::Context::default();
        let mut frame = PanelFrame {
            remote: PaneState::new(RemotePath::from_bytes(b"/".to_vec())),
            local: PaneState::new(RemotePath::from_bytes(b"/".to_vec())),
            bookmarks: Vec::new(),
            session_bound: false,
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
        let mut cols = ColWidths::default();

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
                    content(ctx, &t, 7, true, &mut frame, 0, &mut cols, &mut None);
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
            content(ctx, &t, 7, true, &mut frame, 0, &mut cols, &mut None);
        }));
        // 再跑两帧,让滚动状态稳定下来(smooth scroll 有插值)。
        for _ in 0..2 {
            out = Some(ctx.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |ctx| {
                    content(ctx, &t, 7, true, &mut frame, 0, &mut cols, &mut None);
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

    /// F136/D1:**远端栏**五列恒定存在,不再随宽度收起。窄栏下放不下是靠
    /// 横向滚动条解决的,不是靠把列藏起来 —— 藏起来的那一版会让「属主」
    /// 这类信息在默认侧栏宽下永远看不到。
    ///
    /// (本地栏少一列属主,判据按栏静态,见
    /// `the_local_column_has_no_owner_column_but_the_remote_one_does`。)
    ///
    /// 自证会变红:在 `col_lefts()` 里按总宽过滤掉最后几列。
    #[test]
    fn all_five_columns_are_present_no_matter_how_narrow_the_panel_is() {
        let cols = ColWidths::default();
        let keys: Vec<SortKey> = col_lefts(&cols, PanelColumn::Remote)
            .iter()
            .map(|&(_, k, _, _)| k)
            .collect();
        assert_eq!(
            keys,
            vec![
                SortKey::Name,
                SortKey::Size,
                SortKey::Mtime,
                SortKey::Perm,
                SortKey::Owner
            ],
            "五列的顺序或数量变了 —— 列布局是唯一真值来源,顺序即收起顺序的时代已经结束"
        );
    }

    /// 列坐标必须是**首尾相接、无缝无叠**的累加,而且总宽等于各列之和 ——
    /// `content_w()` 与 `col_lefts()` 一旦对不上,横向滚动的可滚范围就会
    /// 比实际内容短一截,最右边那列永远滚不到。
    ///
    /// 自证会变红:把 `content_w()` 改成漏加 `owner`;或在 `col_lefts()`
    /// 的累加里插一个间距。
    #[test]
    fn column_lefts_are_contiguous_and_sum_to_the_content_width() {
        let cols = ColWidths {
            name: 111.0,
            size: 22.0,
            mtime: 33.0,
            perm: 44.0,
            owner: 55.0,
        };
        let lay = col_lefts(&cols, PanelColumn::Remote);
        assert_eq!(lay[0].2, 0.0, "第一列必须从 0 起算(相对行左边界)");
        for w in lay.windows(2) {
            let (label, _, left, width) = w[0];
            let (next_label, _, next_left, _) = w[1];
            assert!(
                (left + width - next_left).abs() < 0.01,
                "「{label}」右边界 {} 与「{next_label}」左边界 {next_left} 不相接",
                left + width
            );
        }
        let last = lay[4];
        assert!(
            (last.2 + last.3 - content_w(&cols, PanelColumn::Remote)).abs() < 0.01,
            "最后一列右边界 {} 与 content_w() {} 对不上",
            last.2 + last.3,
            content_w(&cols, PanelColumn::Remote)
        );
    }

    /// D3 的替代守护:列头与行体**画在同一组 x 上**。原来两边各自累加,
    /// 靠一条几何测试守住不许错位;现在坐标同源,几何断言会退化成重言式
    /// —— 所以改成从**真实渲染结果**里取两串文字来比:列头的「大小」标题
    /// 与行里的大小数值必须落在同一列里。
    ///
    /// **不能直接比两个文字中心的 x**:列头左对齐、数值右对齐,中心天生
    /// 不相等。改成比**中心差**——已知列头中心是「`C` 加 `size_left` 加
    /// `SP_XS` 再加 `hw` 的一半」,数值中心是「`C` 加 `size_left` 加
    /// `size_w` 减 `SP_XS` 再减 `vw` 的一半」(`C` 是面板内容区左边界,
    /// 列头与行体共用同一个 `ui`、同一个 `C`,减法里直接消掉,不需要求出
    /// 它的绝对值)。所以「期望差」只剩 `2*SP_XS + (hw+vw)/2 - size_w`,
    /// `hw`/`vw`(两串文字的实际像素宽)用 `ctx.fonts()` 现场量,不写死
    /// 数字——字体度量随平台/字体版本变,硬编码在别的机器上会假红。容差
    /// 1.0pt,只够盖浮点/像素取整的抖动。
    ///
    /// 这条测试的前一版容差是 `±size_w`(整列宽),20px 的错位量不到列宽
    /// 一半,盖不住——**已实测**:见下方两次记录。
    ///
    /// 自证(已实测,不是猜的):给 `header_at()` 的列 rect 临时加一个
    /// 20px 偏移(`egui::pos2(band.left() + left - offset_x + 20.0, ...)`)
    /// → 这条测试 FAIL(中心差偏出 1.0pt 容差,实测偏移量 ≈20pt);还原
    /// 偏移后 PASS。旧的「容差 ±size_w」写法在同样 20px 偏移下量不出来
    /// (复核挖出的问题,190px 左右才会触发旧判据)。
    #[test]
    fn the_size_header_and_the_size_value_land_in_the_same_column() {
        let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
        state.entries = vec![entry(b"a.txt", EntryKind::File)];
        state.load = Load::Ready;

        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut cols = ColWidths::default();
        let mut out = None;
        for _ in 0..2 {
            out = Some(ctx.run(raw(None), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        "远端",
                        1,
                        PanelColumn::Remote,
                        &mut state,
                        false,
                        BookmarkView::none(),
                        0,
                        &mut cols,
                    );
                });
            }));
        }
        let out = out.expect("跑了两帧");
        // `entry()` 的 size 是 1024 → `human_size` 印成 "1.0 KB"。
        let head = find_text_pos(&out.shapes, "大小").expect("列头该画出「大小」");
        let value = find_text_pos(&out.shapes, "1.0 KB").expect("行里该画出大小数值");

        // 现场量出两串文字各自的像素宽——字体/字号跟生产代码
        // (`header_at()`/`row()`)完全对应:列头用 11.0 号 `fg_muted`,
        // 数值用 12.0 号 `fg_mid`。
        let hw = ctx
            .fonts(|f| {
                f.layout_no_wrap(
                    "大小".to_string(),
                    egui::FontId::proportional(11.0),
                    theme::c32(t.fg_muted),
                )
            })
            .size()
            .x;
        let vw = ctx
            .fonts(|f| {
                f.layout_no_wrap(
                    "1.0 KB".to_string(),
                    egui::FontId::proportional(12.0),
                    theme::c32(t.fg_mid),
                )
            })
            .size()
            .x;

        let lay = col_lefts(&cols, PanelColumn::Remote);
        let (_, _, _size_left, size_w) = lay[1];
        let expected_diff = 2.0 * crate::ui::metrics::SP_XS + (hw + vw) / 2.0 - size_w;
        let actual_diff = head.x - value.x;
        assert!(
            (actual_diff - expected_diff).abs() < 1.0,
            "列头「大小」与行内数值的中心差 = {actual_diff},期望 {expected_diff}\
             (容差 1.0pt)—— 列头与行体的列坐标分家了"
        );
    }

    /// D2:本地栏的属主列画 `—`,不画 `0:0`。判据是栏别不是 uid ——
    /// 远端真的有 root(uid=0)拥有的文件。
    ///
    /// 自证会变红:把判据从 `column == PanelColumn::Local` 改成 `e.uid == 0`,
    /// 然后这条测试里那个远端 root 文件会画成 `—`。
    #[test]
    fn the_local_column_shows_a_dash_for_owner_but_remote_root_shows_zeros() {
        let none = crate::files::owners::OwnerNames::default();
        assert_eq!(owner_text(PanelColumn::Local, 0, 0, &none), "—");
        assert_eq!(owner_text(PanelColumn::Remote, 0, 0, &none), "0:0");
        assert_eq!(
            owner_text(PanelColumn::Remote, 1000, 1000, &none),
            "1000:1000"
        );
    }

    /// F142:名字查到了就画名字。**本地栏不受影响** —— 它恒画 `—`,哪怕
    /// 那份表里恰好有同号的 uid(本地栏那份表永远是空的,但判据不能依赖
    /// 这一点:靠「表是空的」来保证本地栏画 `—`,哪天两栏共用一份缓存就塌了)。
    ///
    /// 自证会变红:把 `owner_text` 远端分支的 `owners.text(uid, gid)` 换回
    /// `format!("{uid}:{gid}")`,第一条断言拿到 `1000:1000`。
    #[test]
    fn a_remote_owner_shows_its_name_once_getent_has_answered() {
        let mut o = crate::files::owners::OwnerNames::default();
        // 分隔符取生产常量,不抄字面量 —— 抄了的话改分隔符会让这条测试
        // 假红(它要守的是「名字画出来了」,不是分隔符长什么样)。
        let sep = crate::files::owners::SEP;
        o.merge(
            format!("deploy:x:1000:1000::/home/deploy:/bin/sh\n{sep}\ndocker:x:1000:\n").as_bytes(),
        );
        assert_eq!(
            owner_text(PanelColumn::Remote, 1000, 1000, &o),
            "deploy:docker"
        );
        assert_eq!(owner_text(PanelColumn::Local, 1000, 1000, &o), "—");
    }

    /// 质量复核实测:把 `row()` 里权限槽(`lay[3]`)与属主槽(`lay[4]`)
    /// 绘制的内容对调,`files_panel` 全部测试全绿——列坐标测试只比几何
    /// (label/key/left/width),完全不看「谁画在哪个槽位」。用户会在
    /// 「权限」标题下看到 `uid:gid`、在「属主」标题下看到 `rwxr-x---`,
    /// 测试网毫无反应。
    ///
    /// 这条测试直接读渲染出的 `Shape::Text`,断言每个槽位画出来的数据
    /// 文本落在**自己**那个槽位的 x 区间里。顺带把大小、修改时间两个槽位
    /// 也一并锁上——同一类判据,x 区间同样来自 `col_lefts()`,不算超范围。
    ///
    /// entry 特意挑了四个槽位互不相同、不会看混的值:大小 `777 B`、权限
    /// `rwxr-x---`、属主 `1000:33`,彼此没有子串重叠,`find_text_pos` 不会
    /// 找混。
    ///
    /// x 区间来自生产函数 `col_lefts(cols)`——`row()` 自己也调的那份计算,
    /// 测试不重抄坐标(同 `icon_rect()` 的教训)。
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
        let cols = ColWidths::default();
        let width = content_w(&cols, PanelColumn::Remote);
        let ctx = egui::Context::default();
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default()
                .frame(egui::Frame::none())
                .show(ctx, |ui| {
                    // 显式给 `row()` 一块零边距、宽度已知的矩形——不然
                    // `row_rect` 的实际宽度取决于 egui 默认 `screen_rect` /
                    // `CentralPanel` 默认边距,测试里没法跟 `col_lefts()`
                    // 对上同一个数。手法同生产代码 `content()` 切两栏
                    // (`ui.scope_builder(UiBuilder::new().max_rect(..))`)。
                    let rect =
                        egui::Rect::from_min_size(ui.max_rect().min, egui::vec2(width, ROW_H));
                    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                        row(
                            ui,
                            &t,
                            &e,
                            PanelColumn::Remote,
                            false,
                            &cols,
                            &crate::files::owners::OwnerNames::default(),
                        );
                    });
                });
        });

        let lay = col_lefts(&cols, PanelColumn::Remote);
        let expect = [
            (human_size(e.size), lay[1]),
            (mtime_text(e.mtime), lay[2]),
            (perm_string(e.mode), lay[3]),
            (
                owner_text(
                    PanelColumn::Remote,
                    e.uid,
                    e.gid,
                    &crate::files::owners::OwnerNames::default(),
                ),
                lay[4],
            ),
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

    /// F131:点一下路径条就该进入编辑态。进不去的话这个功能对用户完全
    /// 不存在(它没有别的入口 —— 没有按钮、没有菜单项)。
    ///
    /// 两帧预热是防御性的,不是必需的:实测过把预热砍到一帧这条测试照样
    /// 绿(这条路径没有 `Area`/`Panel` 首帧 fade_in 那个坑,`CentralPanel`
    /// 直接画)。留着两帧是跟其它同类测试(`clicking_a_bookmark_dispatches_
    /// goto_to_its_path` 等)保持同一手法,不是这里单独需要。
    ///
    /// 自证会变红:把 `show` 里路径条那层 `if hit.clicked()` 分支删掉
    /// (已用「注释掉该分支」实测验证过,见提交记录)。
    #[test]
    fn clicking_the_path_bar_starts_editing_it() {
        let mut state = PaneState::new(mullion_ssh::sftp::RemotePath::from_bytes(
            b"/var/log".to_vec(),
        ));
        state.load = Load::Ready;
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(1.0);
        let base = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(600.0, 500.0),
            )),
            ..Default::default()
        };
        let mut cols = ColWidths::default();
        let mut run = |input: egui::RawInput, state: &mut PaneState| {
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &crate::theme::MULLION_DARK,
                        "远端",
                        1,
                        PanelColumn::Remote,
                        state,
                        true,
                        BookmarkView::none(),
                        0,
                        &mut cols,
                    );
                });
            });
        };
        for t in [0.0_f64, 1.0] {
            run(
                egui::RawInput {
                    time: Some(t),
                    ..base()
                },
                &mut state,
            );
        }
        let rect = ctx
            .read_response(path_label_id("远端"))
            .expect("路径条没有可交互的响应 —— 它点不动")
            .rect;
        let pos = rect.center();
        let m = egui::Modifiers::default();
        run(
            egui::RawInput {
                time: Some(2.0),
                events: vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: m,
                    },
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: m,
                    },
                ],
                ..base()
            },
            &mut state,
        );
        assert_eq!(
            state.path_edit.as_deref(),
            Some("/var/log"),
            "点了路径条却没进入编辑态(或者没把当前目录填进去)"
        );
    }

    /// F131:退出编辑态**不跳转**是默认;只有回车才跳。反过来的话
    /// (失焦即提交)用户点别处就会被莫名其妙带走。
    ///
    /// 这条只测状态机本身(`finish_path_edit`),不驱动 egui —— 回车/Esc
    /// 在 egui 里都表现为 `lost_focus`,区分靠的是当帧有没有 Enter 键事件。
    #[test]
    fn leaving_the_path_editor_without_enter_discards_the_input() {
        let mut state = PaneState::new(mullion_ssh::sftp::RemotePath::from_bytes(
            b"/var/log".to_vec(),
        ));
        state.path_edit = Some("/etc".into());
        assert_eq!(finish_path_edit(&mut state, false), None);
        assert!(state.path_edit.is_none(), "退出编辑态时缓冲没清");

        state.path_edit = Some("/etc".into());
        assert_eq!(
            finish_path_edit(&mut state, true),
            Some(FileAction::GotoInput("/etc".into()))
        );
        assert!(state.path_edit.is_none());
    }

    /// F131:质量复核实测出的真 bug ——如果渲染那侧每帧都无条件
    /// `request_focus()`,两栏各自发现自己没焦点就抢回去,先进编辑态的
    /// 那栏永远抢不到 `lost_focus()`,永久退不出编辑态。这条测出的是
    /// 「后进编辑态的一栏能把先进的那栏挤出去」——两栏互斥只应该靠
    /// 「egui 键盘焦点唯一」自然发生,不需要面板代码额外判断另一栏。
    ///
    /// 自证会变红:把 `show` 里编辑态分支**整段**还原成 Task 6 最初那版
    /// (`if !resp.has_focus() { resp.request_focus(); }` 每帧无条件抢焦点、
    /// 收口条件只有 `resp.lost_focus()`,没有 `clicked_elsewhere()`)——
    /// 已实测:两栏各自每帧抢焦点,而**后画的那栏(`content()` 里远端画在
    /// 本地之后)每帧都能把焦点抢回来**,`frame.remote.path_edit` 永远退不出
    /// `Some`,断言会红(实测报错:`实际却还留在: Some("/var/log")`)。
    ///
    /// **只单独还原 `request_focus()` 那一句、留着 `clicked_elsewhere()`
    /// 不动,验证不出这条 bug**——已实测确认:那种局部还原下测试仍然绿,
    /// 因为点本地栏路径条这一下本身也会让远端栏的 `clicked_elsewhere()`
    /// 判真,把它顶出编辑态,盖住了焦点互抢那条独立的坑。真正会暴露这条
    /// bug 的是「不点任何东西、纯靠重复渲染」的场景(两栏都已经在编辑态时,
    /// 每帧都无条件 `request_focus()` 会让双方永远抢不出胜负),但那种
    /// 构造方式脱离了用户真实操作路径,没有采用;这里改用「整段代码
    /// 还原回 Task 6 最初版本」作为变异验证,因为这正是质量复核实际测出、
    /// 也是这次要修的那个真实回归。
    #[test]
    fn entering_edit_mode_in_one_column_evicts_the_other_columns_editor() {
        let t = crate::theme::MULLION_DARK;
        let mut frame = PanelFrame {
            remote: PaneState::new(RemotePath::from_bytes(b"/var/log".to_vec())),
            local: PaneState::new(RemotePath::from_bytes(b"/home/u".to_vec())),
            bookmarks: Vec::new(),
            session_bound: false,
            active_column: PanelColumn::default(),
        };
        frame.remote.load = Load::Ready;
        frame.local.load = Load::Ready;

        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(1.0);
        let base = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 600.0),
            )),
            ..Default::default()
        };
        let mut cols = ColWidths::default();
        let mut run = |input: egui::RawInput, frame: &mut PanelFrame| {
            let _ = ctx.run(input, |ctx| {
                content(ctx, &t, 1, false, frame, 0, &mut cols, &mut None);
            });
        };
        let mut clock = 0.0_f64;
        let mut tick = || {
            clock += 1.0;
            clock
        };
        for _ in 0..2 {
            run(
                egui::RawInput {
                    time: Some(tick()),
                    ..base()
                },
                &mut frame,
            );
        }

        let m = egui::Modifiers::default();
        let click_at = |pos: egui::Pos2| {
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: m,
                },
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: m,
                },
            ]
        };

        // 远端栏先点进编辑态。
        let remote_pos = ctx
            .read_response(path_label_id("远端"))
            .expect("远端路径条没有可交互的响应")
            .rect
            .center();
        run(
            egui::RawInput {
                time: Some(tick()),
                events: click_at(remote_pos),
                ..base()
            },
            &mut frame,
        );
        assert_eq!(
            frame.remote.path_edit.as_deref(),
            Some("/var/log"),
            "前提不成立:远端栏没能先进入编辑态"
        );

        // 再跑一帧,让远端栏的 `TextEdit` 真的拿到键盘焦点(焦点请求发生在
        // 点击那一帧,widget 要到下一帧渲染时才会体现 `has_focus()`)。
        run(
            egui::RawInput {
                time: Some(tick()),
                ..base()
            },
            &mut frame,
        );

        // 本地栏也点进编辑态。
        let local_pos = ctx
            .read_response(path_label_id("本地"))
            .expect("本地路径条没有可交互的响应")
            .rect
            .center();
        run(
            egui::RawInput {
                time: Some(tick()),
                events: click_at(local_pos),
                ..base()
            },
            &mut frame,
        );
        assert_eq!(
            frame.local.path_edit.as_deref(),
            Some("/home/u"),
            "前提不成立:本地栏没能也进入编辑态"
        );

        // 再多跑几帧,让远端栏因为焦点被本地栏抢走而 `lost_focus()`、
        // 自己退出编辑态 —— 不需要任何「另一栏进editing就清掉这栏」的
        // 显式判断,纯靠 egui 焦点唯一自然发生。
        for _ in 0..3 {
            run(
                egui::RawInput {
                    time: Some(tick()),
                    ..base()
                },
                &mut frame,
            );
        }

        assert_eq!(
            frame.remote.path_edit, None,
            "本地栏进了编辑态之后,远端栏该因为失去焦点自动退出编辑态,\
             实际却还留在: {:?}",
            frame.remote.path_edit
        );
    }

    /// F131:质量复核实测出的真 bug —— `Label` 的响应矩形按**文字实际
    /// 宽度**分配,`cwd == "/"` 时那个矩形只有几像素宽,路径条右侧一大片
    /// 看着像能点的空白其实点不动。而这是这个功能唯一的入口(没有按钮、
    /// 没有菜单项),命中区域小等于功能不存在。
    ///
    /// 自证会变红:把 `show` 里 `ui.interact(row_rect, ...)` 的 `row_rect`
    /// 换回 `label.rect`(Task 6 最初那版写法)。
    #[test]
    fn a_short_path_still_has_a_wide_clickable_area() {
        let mut state = PaneState::new(mullion_ssh::sftp::RemotePath::from_bytes(b"/".to_vec()));
        state.load = Load::Ready;
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(1.0);
        let base = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(600.0, 500.0),
            )),
            ..Default::default()
        };
        let mut cols = ColWidths::default();
        let mut run = |input: egui::RawInput, state: &mut PaneState| {
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &crate::theme::MULLION_DARK,
                        "远端",
                        1,
                        PanelColumn::Remote,
                        state,
                        true,
                        BookmarkView::none(),
                        0,
                        &mut cols,
                    );
                });
            });
        };
        for t in [0.0_f64, 1.0] {
            run(
                egui::RawInput {
                    time: Some(t),
                    ..base()
                },
                &mut state,
            );
        }
        let rect = ctx
            .read_response(path_label_id("远端"))
            .expect("路径条没有可交互的响应")
            .rect;
        // 点 rect 左边界往右 150px 处 —— 路径只有一个字符时,按文字宽度
        // 算的命中区域必然覆盖不到这里。
        let pos = egui::pos2(rect.left() + 150.0, rect.center().y);
        let m = egui::Modifiers::default();
        run(
            egui::RawInput {
                time: Some(2.0),
                events: vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: m,
                    },
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: m,
                    },
                ],
                ..base()
            },
            &mut state,
        );
        assert_eq!(
            state.path_edit.as_deref(),
            Some("/"),
            "点路径条右侧的空白处没能进入编辑态,命中区域太窄:{rect:?}"
        );
    }

    /// D2:列宽是**外部状态**,不是每帧现造的 —— 造在 `show()` 内部的话
    /// 拖出来的宽度下一帧就被冲掉,拖拽功能会以「拖得动、松手弹回」的形式
    /// 安静失效。判据:同一份 `ColWidths` 改了之后,画出来的列位置跟着变。
    ///
    /// 自证会变红:把 `show()` 里的 `cols` 参数忽略掉、改回内部
    /// `ColWidths::default()`。
    #[test]
    fn the_column_widths_come_from_the_caller_not_from_a_fresh_default() {
        let t = crate::theme::MULLION_DARK;
        let render = |cols: &mut ColWidths| {
            let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
            state.entries = vec![entry(b"a.txt", EntryKind::File)];
            state.load = Load::Ready;
            let ctx = egui::Context::default();
            let mut out = None;
            for _ in 0..2 {
                out = Some(ctx.run(raw(None), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        show(
                            ui,
                            &t,
                            "远端",
                            1,
                            PanelColumn::Remote,
                            &mut state,
                            false,
                            BookmarkView::none(),
                            0,
                            cols,
                        );
                    });
                }));
            }
            find_text_pos(&out.expect("跑了两帧").shapes, "1.0 KB")
                .expect("行里该画出大小数值")
                .x
        };

        let mut narrow = ColWidths::default();
        let mut wide = ColWidths {
            name: ColWidths::default().name + 120.0,
            ..ColWidths::default()
        };
        let x_narrow = render(&mut narrow);
        let x_wide = render(&mut wide);
        assert!(
            (x_wide - x_narrow - 120.0).abs() < 1.0,
            "名称列加宽 120 之后,大小数值应该右移 120(实际 {x_narrow} → {x_wide})\
             —— 列宽没有从调用方读进来"
        );
    }

    /// F135:拖列边界只改**被拖的那一列**,右边的列整体平移。
    /// 不做「向右借宽度」那种此消彼长的语义 —— 总宽本来就允许超出视口
    /// (有横向滚动兜着),没有守恒的必要,而守恒会让「我只想加宽名称列」
    /// 变成「顺手把修改时间列挤没了」。
    ///
    /// 自证会变红:把 `col_w_mut(cols, i)` 换成固定改 `cols.name`;
    /// 或者在改宽度时顺手把下一列减掉同样的量。
    ///
    /// **拖的是「大小」列的右边界,不是「名称」列的** —— 这不是随手选的:
    /// 如果拖名称列(下标 0)去验证「换成固定改 `cols.name`」这条自证,
    /// 硬编码目标(`cols.name`)和真实目标(下标 0 对应的也是 `cols.name`)
    /// 恰好重合,测试测不出任何区别(已实测:把 `col_w_mut` 硬编码成
    /// `cols.name` 后,若这条测试改拖名称列,`before.size == after.size`
    /// 与 `before.name < after.name` 两条断言照样全部通过,测试恒绿)。
    /// 换成拖中间的「大小」列(下标 1),硬编码就会显形:
    /// `cols.size` 不会变,`cols.name` 却会跟着动。
    #[test]
    fn dragging_a_column_edge_only_widens_that_column() {
        let t = crate::theme::MULLION_DARK;
        let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
        state.entries = vec![entry(b"a.txt", EntryKind::File)];
        state.load = Load::Ready;
        let mut cols = ColWidths::default();
        let before = cols;
        let ctx = egui::Context::default();
        let render = |input: egui::RawInput, state: &mut PaneState, cols: &mut ColWidths| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        "远端",
                        1,
                        PanelColumn::Remote,
                        state,
                        false,
                        BookmarkView::none(),
                        0,
                        cols,
                    );
                });
            })
        };
        // 标注模式开着才能读回 `annotate::mark` 登记的矩形(`spot_rect`
        // 文档里写明「前提:标注模式必须开着」)。这是拿列边界精确坐标
        // **唯一不靠猜字形宽度**的路子 —— 早先按「标题文字中心 - 半个
        // 猜测宽度」反推 edge_x 的写法在 CJK 字形下算出来的偏移和真实
        // 边界差了 9px,直接落在 6pt 热区外,测试假红了一版,现改用
        // `header_at()` 里为列头登记的同一份矩形。
        annotate::toggle(&ctx);
        // 两帧稳定布局。列头的登记矩形右边界就是「大小」列的右边界
        // (它跟「修改时间」列之间那道热区的中心)。
        let _ = render(raw(None), &mut state, &mut cols);
        let _ = render(raw(None), &mut state, &mut cols);
        let size_head = annotate::spot_rect(&ctx, "文件面板/远端/列头/大小")
            .expect("该登记出列头「大小」的矩形");
        let head_y = size_head.center().y;
        let edge_x = size_head.right();

        // 按下 → 拖到右边 +60 → 松手。egui 要指针移动超过阈值才判成拖,
        // 中间多灌一帧。
        let _ = render(
            press(egui::pos2(edge_x, head_y), 1.0, true),
            &mut state,
            &mut cols,
        );
        let _ = render(
            moved(egui::pos2(edge_x + 30.0, head_y), 1.1),
            &mut state,
            &mut cols,
        );
        let _ = render(
            moved(egui::pos2(edge_x + 60.0, head_y), 1.2),
            &mut state,
            &mut cols,
        );
        let _ = render(
            press(egui::pos2(edge_x + 60.0, head_y), 1.3, false),
            &mut state,
            &mut cols,
        );

        assert!(
            cols.size > before.size + 40.0,
            "拖了大小列右边界 +60,宽度却只从 {} 变成 {} —— 拖拽没接上",
            before.size,
            cols.size
        );
        assert_eq!(
            (cols.name, cols.mtime, cols.perm, cols.owner),
            (before.name, before.mtime, before.perm, before.owner),
            "拖大小列把别的列宽也改了"
        );
    }

    /// F135:再怎么往左拖也不能把列拖没 —— 宽度为 0 的列点不中、拖不回来,
    /// 用户会以为这一列被永久删掉了。
    ///
    /// 走跟上一条测试同样的真实拖拽路径(而不是直接调
    /// `.clamp(col_min(i), COL_MAX)` 再断言结果满足这个 clamp 本身的语义 ——
    /// 那测的是标准库 `f32::clamp`,不是生产代码,对生产代码里的 clamp
    /// 删不删都恒绿。这里把名称列右边界往左狠拖几千 px,断言真实拖拽路径
    /// 产出的宽度落在最小值以上。
    ///
    /// 自证(已实测):把 `header_at()` 里的
    /// `.clamp(col_min(i), COL_MAX)` 去掉 → 本条测试变红。**实情**:去掉
    /// clamp 时,先炸的是 egui 自己在 `ui.rs:908` 的
    /// `assertion failed: 0.0 <= width`(宽度被拖成负数,egui 内部对负宽度
    /// 有断言)——不是本测试下面那几条 `assert!` 先失败。红是真红,但红法
    /// 跟字面上看到的 panic 位置对不上,免得后人看见 egui 内部 panic 以为
    /// 是这条测试写坏了。
    #[test]
    fn a_column_cannot_be_dragged_below_its_minimum() {
        let t = crate::theme::MULLION_DARK;
        let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
        state.entries = vec![entry(b"a.txt", EntryKind::File)];
        state.load = Load::Ready;
        let mut cols = ColWidths::default();
        let ctx = egui::Context::default();
        let render = |input: egui::RawInput, state: &mut PaneState, cols: &mut ColWidths| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        "远端",
                        1,
                        PanelColumn::Remote,
                        state,
                        false,
                        BookmarkView::none(),
                        0,
                        cols,
                    );
                });
            })
        };
        annotate::toggle(&ctx);
        let _ = render(raw(None), &mut state, &mut cols);
        let _ = render(raw(None), &mut state, &mut cols);
        let name_head = annotate::spot_rect(&ctx, "文件面板/远端/列头/名称")
            .expect("该登记出列头「名称」的矩形");
        let head_y = name_head.center().y;
        let edge_x = name_head.right();

        // 按下 → 分两帧把指针拖到远超最小宽度的左边 → 松手。
        let _ = render(
            press(egui::pos2(edge_x, head_y), 1.0, true),
            &mut state,
            &mut cols,
        );
        let _ = render(
            moved(egui::pos2(edge_x - 500.0, head_y), 1.1),
            &mut state,
            &mut cols,
        );
        let _ = render(
            moved(egui::pos2(edge_x - 5000.0, head_y), 1.2),
            &mut state,
            &mut cols,
        );
        let _ = render(
            press(egui::pos2(edge_x - 5000.0, head_y), 1.3, false),
            &mut state,
            &mut cols,
        );

        assert!(
            cols.name >= col_min(0),
            "名称列被拖到了 {},低于最小宽度 {}(第 0 列)",
            cols.name,
            col_min(0)
        );
        assert!(
            cols.name > 0.0,
            "名称列宽度是 {},拖没了 —— 用户点不中也拖不回来",
            cols.name
        );
    }

    /// F135:再怎么往右拖也不能让列无限变宽 —— 拖到几千 px 会让横向滚动条
    /// 退化成一条几乎抓不住的细线(见 `COL_MAX` 定义处的注释),必须有上限。
    ///
    /// 跟 `a_column_cannot_be_dragged_below_its_minimum` 对称:走同一条真实
    /// 拖拽路径(而不是直接断言 `f32::clamp` 自身的语义),把名称列右边界
    /// 往右狠拖几千 px,断言真实拖拽路径产出的宽度落在 `COL_MAX` 以内。
    /// 这条测试目前是唯一守 `COL_MAX` 上界的测试 —— 在它加入之前,把
    /// `.clamp(col_min(i), COL_MAX)` 换成 `.max(col_min(i))`(只留下界,
    /// 丢掉上界)不会让当时已有的任何一条测试变红。
    ///
    /// 自证(已实测):把 `header_at()` 里的 `.clamp(col_min(i), COL_MAX)`
    /// 换成 `.max(col_min(i))` → 本条测试变红(`cols.name` 被拖到几千,
    /// 远超 `COL_MAX` 的 800.0),而 `a_column_cannot_be_dragged_below_its_minimum`
    /// 仍然绿 —— 因为 `.max(col_min(i))` 照样卡住下界,只是丢了上界。
    #[test]
    fn a_column_cannot_be_dragged_past_its_maximum() {
        let t = crate::theme::MULLION_DARK;
        let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
        state.entries = vec![entry(b"a.txt", EntryKind::File)];
        state.load = Load::Ready;
        let mut cols = ColWidths::default();
        let before = cols;
        let ctx = egui::Context::default();
        let render = |input: egui::RawInput, state: &mut PaneState, cols: &mut ColWidths| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        "远端",
                        1,
                        PanelColumn::Remote,
                        state,
                        false,
                        BookmarkView::none(),
                        0,
                        cols,
                    );
                });
            })
        };
        annotate::toggle(&ctx);
        let _ = render(raw(None), &mut state, &mut cols);
        let _ = render(raw(None), &mut state, &mut cols);
        let name_head = annotate::spot_rect(&ctx, "文件面板/远端/列头/名称")
            .expect("该登记出列头「名称」的矩形");
        let head_y = name_head.center().y;
        let edge_x = name_head.right();

        // 按下 → 分两帧把指针拖到远超上限宽度的右边 → 松手。
        let _ = render(
            press(egui::pos2(edge_x, head_y), 1.0, true),
            &mut state,
            &mut cols,
        );
        let _ = render(
            moved(egui::pos2(edge_x + 500.0, head_y), 1.1),
            &mut state,
            &mut cols,
        );
        let _ = render(
            moved(egui::pos2(edge_x + 5000.0, head_y), 1.2),
            &mut state,
            &mut cols,
        );
        let _ = render(
            press(egui::pos2(edge_x + 5000.0, head_y), 1.3, false),
            &mut state,
            &mut cols,
        );

        assert!(
            cols.name <= COL_MAX,
            "名称列被拖到了 {},超过上限 {}",
            cols.name,
            COL_MAX
        );
        assert!(
            cols.name > before.name,
            "名称列宽度是 {},跟拖之前的 {} 相比没有变化 —— 拖拽没接上",
            cols.name,
            before.name
        );
    }

    /// 拖拽热区不能把整列的排序点击吃掉 —— 点列头**中心**必须照旧改排序。
    ///
    /// **自证(已实测,不是「把 HANDLE_W 改成整列宽」这么简单)**:egui-0.30
    /// 的命中测试按 `Sense` 分组独立算(`hit_test.rs` `hit_test_on_close`:
    /// `hit_click` 只在 `sense.click` 的部件里找,`hit_drag` 只在
    /// `sense.drag` 的部件里找,互不竞争)。热区是纯 `Sense::drag()`,不带
    /// `click`,所以单纯把 `HANDLE_W` 从 6pt 改成整列宽 **测不出这条测试的
    /// 红**(已实测三种改法,`clicking_the_middle_of_a_header_still_sorts`
    /// 全绿;倒是会把 `dragging_a_column_edge_only_widens_that_column` 测
    /// 红 —— 拖满整列宽后相邻热区会在共享边界重叠,判归哪一列变得有歧义)。
    /// 真正会让本条变红的改法(已实测):热区的 `Sense::drag()` 换成
    /// `Sense::click_and_drag()`、宽度改成整列宽、**且**把这段热区循环挪到
    /// 列体循环**之后**注册(三处同时改,单独改任何一处都不够)。
    #[test]
    fn clicking_the_middle_of_a_header_still_sorts() {
        let t = crate::theme::MULLION_DARK;
        let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
        state.entries = vec![entry(b"a.txt", EntryKind::File)];
        state.load = Load::Ready;
        let before = state.sort_key;
        let mut cols = ColWidths::default();
        let ctx = egui::Context::default();
        let render = |input: egui::RawInput, state: &mut PaneState, cols: &mut ColWidths| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        "远端",
                        1,
                        PanelColumn::Remote,
                        state,
                        false,
                        BookmarkView::none(),
                        0,
                        cols,
                    );
                });
            })
        };
        let _ = render(raw(None), &mut state, &mut cols);
        let out = render(raw(None), &mut state, &mut cols);
        let pos = find_text_pos(&out.shapes, "修改时间").expect("该画出列头「修改时间」");
        let _ = render(press(pos, 1.0, true), &mut state, &mut cols);
        let _ = render(press(pos, 1.1, false), &mut state, &mut cols);
        assert_ne!(
            state.sort_key, before,
            "点了「修改时间」列头中心,排序键却没变 —— 拖拽热区把整列的点击吃掉了"
        );
        assert_eq!(state.sort_key, SortKey::Mtime);
    }

    /// F135:守的是**热区注册顺序**这条不变量。热区若挂在列体循环之后
    /// (跟 `header_at()` 开头那条「必须先于列体注册」的注释反过来),
    /// 边界那 6pt 上的按下会被列体的点击吃掉 —— 结果排序和缩放**两边都不
    /// 响应**:egui `hit_test.rs` 的 `hit_test_on_close` 对同一个点同时命中
    /// 一个纯 click 部件和一个纯 drag 部件时,谁在「上面」(在 `close` 列表
    /// 里排得靠后 = 更晚注册)谁赢;drag 在上且是纯 drag(不带 click)时,
    /// 直接把 click 结果吃成 `None`,不会退回给下面那个 click 部件
    /// (`hit_test.rs` `(Some(hit_click), Some(hit_drag))` 分支,
    /// `click_is_on_top_of_drag` 为假的那一路)。
    ///
    /// 落点必须**正好在边界 x 上**(不是边界 ±几 pt 的某个安全距离),
    /// 而且**原地按下—松开、不移动**——这样才会真正撞上「同一个点被两种
    /// sense 同时命中」的分支,跟 `clicking_the_middle_of_a_header_still_sorts`
    /// (落点在列中央,离任何热区都远)测的是两码事。
    ///
    /// 自证(已实测):把这段热区循环整体挪到列体循环**之后**
    /// (`header_at()` 里两个 `for` 循环互换顺序,其余都不动)→ 本条测试
    /// 变红,`assert_ne!(state.sort_key, before, ...)` 失败,
    /// `state.sort_key` 停在初始值 `Name` 不动(被挪到后面的列体循环没抢到
    /// 这次点击,`clicking_the_middle_of_a_header_still_sorts` 仍然
    /// 保持绿,因为它的落点在列中央,不落在这条不变量守的边界点上)。
    #[test]
    fn pressing_exactly_on_a_column_edge_still_sorts() {
        let t = crate::theme::MULLION_DARK;
        let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
        state.entries = vec![entry(b"a.txt", EntryKind::File)];
        state.load = Load::Ready;
        let before = state.sort_key;
        let mut cols = ColWidths::default();
        let ctx = egui::Context::default();
        let render = |input: egui::RawInput, state: &mut PaneState, cols: &mut ColWidths| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        "远端",
                        1,
                        PanelColumn::Remote,
                        state,
                        false,
                        BookmarkView::none(),
                        0,
                        cols,
                    );
                });
            })
        };
        annotate::toggle(&ctx);
        let _ = render(raw(None), &mut state, &mut cols);
        let _ = render(raw(None), &mut state, &mut cols);
        let size_head = annotate::spot_rect(&ctx, "文件面板/远端/列头/大小")
            .expect("该登记出列头「大小」的矩形");
        let head_y = size_head.center().y;
        // 边界 x 本身 —— 不留安全距离,故意踩在「大小」右边界 =
        // 「修改时间」左边界这条共享线上。
        let edge_x = size_head.right();

        // 原地按下—松开,不经过任何 `moved` 帧:egui 判定「没有越过拖拽
        // 阈值」,是货真价实的一次点击尝试,不是拖拽。
        let _ = render(
            press(egui::pos2(edge_x, head_y), 1.0, true),
            &mut state,
            &mut cols,
        );
        let _ = render(
            press(egui::pos2(edge_x, head_y), 1.1, false),
            &mut state,
            &mut cols,
        );

        assert_ne!(
            state.sort_key, before,
            "在「大小/修改时间」分界线上原地按下—松开,排序键却没变 —— \
             热区注册顺序反了,把这一下的点击吃掉了"
        );
        assert_eq!(state.sort_key, SortKey::Mtime);
    }

    /// F147:**每一列**都排得动,**两栏**都排得动。用户报「大小和修改时间
    /// 不能排序」,这条把整个矩阵一次锁死 —— 之前只有「修改时间」中心
    /// (`clicking_the_middle_of_a_header_still_sorts`)和「大小/修改时间」
    /// 分界线(`pressing_exactly_on_a_column_edge_still_sorts`)两个点被守着,
    /// 而且都只在远端栏。
    ///
    /// 列的清单**取自生产函数** `col_lefts()`,不在测试里另抄一份:以后再
    /// 加列会自动进入覆盖,本地栏少一列属主也自动对上(F146)。
    ///
    /// 每一列开测前先把 `sort_key` 拨到**别的**列上,于是判据统一成
    /// 「首点 → 该列 + 升序,再点 → 降序」,不用为「名称列初始就是当前列」
    /// 单开一条分支。
    ///
    /// 自证(已实测):把 `header_at()` 里 `if resp.clicked() { hit = Some(key) }`
    /// 改成 `if resp.clicked() && key == SortKey::Name`,本条变红,报
    /// 「远端栏点了列头「大小」,排序没切到这一列的升序」。
    #[test]
    fn every_column_header_sorts_in_both_panes() {
        let t = crate::theme::MULLION_DARK;
        for (id, column) in [("远端", PanelColumn::Remote), ("本地", PanelColumn::Local)] {
            for (label, key, _, _) in col_lefts(&ColWidths::default(), column) {
                let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
                state.entries = vec![entry(b"a.txt", EntryKind::File)];
                state.load = Load::Ready;
                // 拨到「别的列」:测名称列时用权限列打底,其余一律用名称列。
                state.sort_key = if key == SortKey::Name {
                    SortKey::Perm
                } else {
                    SortKey::Name
                };
                state.sort_dir = crate::files::SortDir::Asc;
                let mut cols = ColWidths::default();
                let ctx = egui::Context::default();
                let render = |input: egui::RawInput, state: &mut PaneState, c: &mut ColWidths| {
                    ctx.run(input, |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            show(
                                ui,
                                &t,
                                id,
                                1,
                                column,
                                state,
                                false,
                                BookmarkView::none(),
                                0,
                                c,
                            );
                        });
                    })
                };
                annotate::toggle(&ctx);
                // 两帧预热:列头矩形要等占位横带定下来之后才登记得出。
                let _ = render(raw(None), &mut state, &mut cols);
                let _ = render(raw(None), &mut state, &mut cols);
                let head = annotate::spot_rect(&ctx, &format!("文件面板/{id}/列头/{label}"))
                    .unwrap_or_else(|| panic!("{id}栏没登记出列头「{label}」的矩形"));
                let pos = head.center();

                let _ = render(press(pos, 1.0, true), &mut state, &mut cols);
                let _ = render(press(pos, 1.1, false), &mut state, &mut cols);
                assert_eq!(
                    (state.sort_key, state.sort_dir),
                    (key, crate::files::SortDir::Asc),
                    "{id}栏点了列头「{label}」,排序没切到这一列的升序"
                );

                let _ = render(press(pos, 2.0, true), &mut state, &mut cols);
                let _ = render(press(pos, 2.1, false), &mut state, &mut cols);
                assert_eq!(
                    (state.sort_key, state.sort_dir),
                    (key, crate::files::SortDir::Desc),
                    "{id}栏再点一次列头「{label}」,方向没翻成降序"
                );
            }
        }
    }

    /// 渲染一次远端栏的列头,返回这一帧的 shapes。排序状态由调用方给。
    fn header_shapes(
        cols: &ColWidths,
        key: SortKey,
        dir: crate::files::SortDir,
    ) -> Vec<egui::epaint::ClippedShape> {
        let t = crate::theme::MULLION_DARK;
        let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
        state.entries = vec![entry(b"a.txt", EntryKind::File)];
        state.load = Load::Ready;
        state.sort_key = key;
        state.sort_dir = dir;
        let mut cols = *cols;
        let ctx = egui::Context::default();
        let mut render = || {
            ctx.run(raw(None), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        "远端",
                        1,
                        PanelColumn::Remote,
                        &mut state,
                        false,
                        BookmarkView::none(),
                        0,
                        &mut cols,
                    );
                });
            })
        };
        // 两帧:列头要等占位横带定下来之后才画得出正确位置。
        let _ = render();
        render().shapes
    }

    /// F147:排序标识画在**列尾**,不是跟在标题后面。
    ///
    /// 判据是「标识**右边界**贴着列的右边界、只留一格 `SP_XS`」,不是
    /// 「标识 x > 标题 x」—— 后者在拼接版里也成立(标识本来就跟在标题
    /// 右边),那样的判据抓不住这次改动。用边界不用中心点:拿中心点判要
    /// 先猜字宽,猜出来的阈值不算判据。
    ///
    /// 列的 x 区间从**标题的左边界反推**(标题左 = 列左 + `SP_XS`),
    /// 不在测试里另算一份横带起点 —— 横带的绝对位置取决于 `CentralPanel`
    /// 的默认边距,那是测试算不准的东西。
    ///
    /// 自证会变红:把两处 `painter.text` 换回一处
    /// `elide(&format!("{label}{mark}"), ..)`,标识跟着标题回到列首。
    #[test]
    fn the_sort_marker_sits_at_the_far_end_of_the_column() {
        let cols = ColWidths::default();
        let shapes = header_shapes(&cols, SortKey::Size, crate::files::SortDir::Asc);
        let marker = find_text_rect(&shapes, "▲").expect("升序该画出「▲」");
        let title = find_text_rect(&shapes, "大小").expect("该画出列头「大小」");

        // 列宽取生产布局那一份,不抄常量。
        let pad = crate::ui::metrics::SP_XS;
        let (_, _, _, w) = col_lefts(&cols, PanelColumn::Remote)[1];
        let col_right = title.left() - pad + w;

        assert!(
            (col_right - marker.right() - pad).abs() < 1.0,
            "标识右边界 {} 没贴着列右边界 {col_right}(该留一格 {pad})—— 它没画在列尾",
            marker.right()
        );
        assert!(
            marker.left() > title.right(),
            "标识(从 {} 起)压在标题(到 {} 止)上或跑到了它左边",
            marker.left(),
            title.right()
        );
    }

    /// F147:列窄到放不下整个标题时,**先保标识**、截标题。
    ///
    /// 拼接版在这里必然丢标识:`Elide::End` 从尾部截,标识就在尾部。
    /// 而列窄恰恰是最需要看清「按哪列排的」的时候。
    ///
    /// 自证会变红:换回拼接版,`find_text_pos(.., "▼")` 拿到 `None`。
    #[test]
    fn a_narrow_column_truncates_its_title_but_keeps_the_sort_marker() {
        let cols = ColWidths {
            mtime: col_min(2),
            ..ColWidths::default()
        };
        let shapes = header_shapes(&cols, SortKey::Mtime, crate::files::SortDir::Desc);
        assert!(
            find_text_pos(&shapes, "▼").is_some(),
            "列窄到 {} 时降序标识被截没了 —— 标题吃掉了标识的预算",
            cols.mtime
        );
        assert!(
            find_text_pos(&shapes, "修改时间").is_none(),
            "列只有 {} 宽,整个标题却还画得下?这条测试没测到截断那一支",
            cols.mtime
        );
        assert!(
            find_text_pos(&shapes, ELLIPSIS).is_some(),
            "标题被截断了却没画省略号"
        );
    }
}
