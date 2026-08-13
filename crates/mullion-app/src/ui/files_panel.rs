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
    /// 回上一级。
    Up,
    /// 刷新当前目录。
    Refresh,
    /// 切隐藏文件显示。
    ToggleHidden,
}

/// 列宽。名称列吃掉剩余宽度,其余定宽 —— 定宽列一旦跟着内容浮动,
/// 换个目录整张表就会横着抖。
const W_SIZE: f32 = 78.0;
const W_MTIME: f32 = 132.0;
const W_PERM: f32 = 86.0;
const ROW_H: f32 = 22.0;

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
/// `focused`:F6/Tab 换焦点(设计 D23,代码复核挖出的可达性缺口)——这一栏
/// 此刻是不是键盘真正落点的那一栏。`true` 才画边框,调用方(`sidebar`/
/// `content`)已经把「面板本身有没有键盘焦点」与「`active_column` 是不是
/// 这一栏」两个条件都算进去了,这里不用再判。
#[allow(clippy::too_many_arguments)] // 跟 session_manager 那批 egui 渲染函数同款,一帧要画的东西天然多
pub fn show(
    ui: &mut Ui,
    t: &Theme,
    id: &str,
    generation: u64,
    state: &mut PaneState,
    show_owner: bool,
    focused: bool,
    bookmarks: &[mullion_store::Bookmark],
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
        Load::Ready => {}
    }

    header(ui, t, id, state);

    let rows = state.rows();
    // `rows` 借着 `&state.entries` 不放(它是 `Vec<&Entry>`),闭包里不能再
    // 借一次 `&mut state`——新选中的那条先记局部变量,出了闭包再落回 `state`。
    let selected = state.selected.clone();
    let mut clicked = None;
    let mut goto = None;
    egui::ScrollArea::vertical()
        .id_salt(scroll_id_salt(id, generation))
        .auto_shrink([false, false])
        .show_rows(ui, ROW_H, rows.len(), |ui, range| {
            for ix in range {
                let e = rows[ix];
                let resp = row(ui, t, e, show_owner, selected.as_ref() == Some(&e.name));
                if resp.clicked() {
                    clicked = Some(e.name.clone());
                }
                if resp.double_clicked() {
                    if let Some(target) = state.enter_target(e) {
                        goto = Some(target);
                    }
                }
            }
        });
    if let Some(name) = clicked {
        state.selected = Some(name);
    }
    if let Some(g) = goto {
        action = Some(FileAction::Goto(g));
    }
    action
}

fn header(ui: &mut Ui, t: &Theme, id: &str, state: &mut PaneState) {
    ui.horizontal(|ui| {
        annotate::mark(ui.ctx(), format!("文件面板/{id}/列头"), ui.max_rect());
        let mut hit = None;
        let name_w = (ui.available_width() - W_SIZE - W_MTIME - W_PERM).max(80.0);
        for (label, key, w) in [
            ("名称", SortKey::Name, name_w),
            ("大小", SortKey::Size, W_SIZE),
            ("修改时间", SortKey::Mtime, W_MTIME),
            ("权限", SortKey::Perm, W_PERM),
        ] {
            let mark = if state.sort_key == key {
                match state.sort_dir {
                    crate::files::SortDir::Asc => " ▲",
                    crate::files::SortDir::Desc => " ▼",
                }
            } else {
                ""
            };
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), egui::Sense::click());
            ui.painter().text(
                rect.left_center() + egui::vec2(4.0, 0.0),
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
    show_owner: bool,
    selected: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ROW_H),
        egui::Sense::click(),
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
    let mut label = e.name.display().to_string();
    if let (EntryKind::Symlink, Some(tgt)) = (e.kind, &e.link_target) {
        label = format!("{label} → {}", tgt.display());
    }
    if !usable {
        label = format!("{label}(名称非 UTF-8,本版无法操作)");
    }
    let name_w = (rect.width() - W_SIZE - W_MTIME - W_PERM).max(80.0);
    p.text(
        rect.left_center() + egui::vec2(4.0, 0.0),
        egui::Align2::LEFT_CENTER,
        label,
        font.clone(),
        fg,
    );
    let size_x = rect.left() + name_w + W_SIZE;
    let size_text = if e.kind == EntryKind::Dir {
        String::new()
    } else {
        human_size(e.size)
    };
    p.text(
        egui::pos2(size_x, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        size_text,
        font.clone(),
        theme::c32(t.fg_mid),
    );
    p.text(
        egui::pos2(size_x + 8.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        mtime_text(e.mtime),
        font.clone(),
        theme::c32(t.fg_mid),
    );
    let mut perm = perm_string(e.mode);
    if show_owner {
        perm = format!("{perm} {}:{}", e.uid, e.gid);
    }
    p.text(
        rect.right_center() - egui::vec2(4.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        perm,
        font,
        theme::c32(t.fg_dim),
    );
    resp
}

/// 面板内当前哪一栏拥有键盘焦点(F6/Tab 换焦点,设计 D23)。远端为默认——
/// 打开一个 SFTP 标签,用户第一件事多半是看远端目录。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelColumn {
    #[default]
    Remote,
    Local,
}

impl PanelColumn {
    /// `Tab` 在两栏之间来回。
    pub fn flipped(self) -> Self {
        match self {
            PanelColumn::Remote => PanelColumn::Local,
            PanelColumn::Local => PanelColumn::Remote,
        }
    }
}

/// 一帧要画的两栏 + 列选项(F50)。
pub struct PanelFrame {
    pub remote: PaneState,
    pub local: PaneState,
    /// D21:属主:组默认隐藏,列头右键打开。
    pub show_owner: bool,
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
            show_owner: false,
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
pub fn sidebar(
    ctx: &egui::Context,
    t: &Theme,
    ui_state: &mut crate::ui::UiState,
    generation: u64,
    panel_focused: bool,
    frame: &mut PanelFrame,
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
            ui.allocate_ui(egui::vec2(ui.available_width(), h * 0.6), |ui| {
                out.0 = show(
                    ui,
                    t,
                    "远端",
                    generation,
                    &mut frame.remote,
                    frame.show_owner,
                    panel_focused && frame.active_column == PanelColumn::Remote,
                    &frame.bookmarks,
                );
            });
            ui.separator();
            out.1 = show(
                ui,
                t,
                "本地",
                generation,
                &mut frame.local,
                false,
                panel_focused && frame.active_column == PanelColumn::Local,
                &[],
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
pub fn content(
    ctx: &egui::Context,
    t: &Theme,
    generation: u64,
    panel_focused: bool,
    frame: &mut PanelFrame,
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
            let half = ui.available_width() * 0.5;
            ui.horizontal(|ui| {
                ui.allocate_ui(egui::vec2(half, ui.available_height()), |ui| {
                    out.0 = show(
                        ui,
                        t,
                        "远端",
                        generation,
                        &mut frame.remote,
                        frame.show_owner,
                        panel_focused && frame.active_column == PanelColumn::Remote,
                        &frame.bookmarks,
                    );
                });
                ui.separator();
                ui.allocate_ui(
                    egui::vec2(ui.available_width(), ui.available_height()),
                    |ui| {
                        out.1 = show(
                            ui,
                            t,
                            "本地",
                            generation,
                            &mut frame.local,
                            false,
                            panel_focused && frame.active_column == PanelColumn::Local,
                            &[],
                        );
                    },
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

    /// 两万项的目录里,一帧只该画可见那几十行 —— `show_rows` 没接对
    /// 的症状是帧时间被打穿(陷阱 T3 的同类),而它在小目录下完全看不出来。
    ///
    /// 这是**结构守护**,不是行为守护:egui 0.30 的 `show_rows` 不把
    /// 实际画出的 range 交给调用方(它内部算完直接喂进 `add_contents`,
    /// 外部拿不到),没法从渲染结果反推「到底画了几行」。改成断言源码里
    /// 用的是 `show_rows` 而不是 `show`——只要这一行还在,虚拟滚动就还在。
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
                    show(ui, &t, "远端", 1, state, false, false, &[]);
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
            show_owner: false,
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
                content(ctx, &t, 1, false, &mut frame);
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
                        show(ui, &t, "远端", 1, &mut state, false, focused, &[]);
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
            show_owner: false,
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
                show(ui, &t, "远端", 1, &mut state, false, false, &bookmarks);
            });
        });
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                show(ui, &t, "远端", 1, &mut state, false, false, &bookmarks);
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
                action = show(ui, &t, "远端", 1, &mut state, false, false, &bookmarks);
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
                    show(ui, &t, "远端", 1, &mut state, false, false, &bookmarks);
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
            show_owner: false,
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
                content(ctx, &t, 1, false, &mut frame);
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
}
