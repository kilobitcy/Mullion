//! 会话管理器**右栏**:标题条 + 错误卡片 + 四个 Tab + 底部按钮条(F90 Task 11)。
//!
//! 原来是一个独立的 `egui::Window`(F90 前),现在是主窗右侧的 `CentralPanel`,
//! 每帧都渲染——「关闭」表单这个概念不复存在,「取消」只是把
//! `ui_state.editor`/`editor_id`/`editor_baseline` 重置回空(等价于回到
//! 「未编辑任何会话」态,画空态提示)。
//!
//! 字段本身的布局是 Task 12 的事,这里只挂四个 Tab 的占位调用点
//! (`super::fields::{basic,auth,network,automation}`)。

use egui::Ui;

use crate::theme::{self, Theme};
use crate::ui::session_manager::SecretPresence;
use crate::ui::UiState;
use mullion_store::{GroupRecord, SessionRecord};

/// 四个 Tab 的标题。索引即 `UiState::editor_tab`,与 `super::TAB_*` 一一对应。
/// 「高级」已并入「连接」(走查 P1-8):那一页只有一行代理,右侧 70% 是空白,
/// 而代理跟主机/端口/跳板本来就是同一个「怎么连上去」的决策。
const TABS: [&str; 4] = ["连接", "认证", "登录后", "图标"];

/// 底部按钮为什么点不动。两个原因是并集,`Missing` 优先 ——
/// 表单都没填齐,就没必要提「测试连接进行中」。
enum Disabled {
    No,
    Missing(String),
    Probing,
}

fn why(missing: super::validate::Missing, probe: &super::ProbeState) -> Disabled {
    if missing.any() {
        Disabled::Missing(missing.hint())
    } else if matches!(probe, super::ProbeState::Running) {
        Disabled::Probing
    } else {
        Disabled::No
    }
}

/// 错误卡片上那一行摘要最多几个字。
const SUMMARY_CHARS: usize = 72;

/// 错误摘要:取首行,超长就截断。返回 `(摘要, 还有没有更多内容)`——
/// 后者决定要不要给「详情」按钮。
///
/// **按 `char` 截,不按字节**:错误信息里几乎必然有中文,`&s[..72]` 切在
/// 多字节字符中间会当场 panic —— 而这是一条「报错时才走到」的路径,
/// 崩在这儿等于把真正的错误信息永远埋掉。
fn summarize(msg: &str) -> (String, bool) {
    let first = msg.lines().next().unwrap_or("").trim();
    let more_lines = msg.trim_end().lines().count() > 1;
    let chars: Vec<char> = first.chars().collect();
    if chars.len() > SUMMARY_CHARS {
        let head: String = chars[..SUMMARY_CHARS].iter().collect();
        (format!("{head}…"), true)
    } else {
        (first.to_string(), more_lines)
    }
}

/// 把禁用原因摊成 tooltip 文本。可用时 `None`。
fn tip(d: &Disabled) -> Option<String> {
    match d {
        Disabled::No => None,
        Disabled::Missing(h) => Some(h.clone()),
        Disabled::Probing => Some("测试连接进行中…".to_owned()),
    }
}

/// `sessions` 只被「连接」页的跳板链编辑器用到:自定义跳板是**对另一条会话的
/// 引用**(设计 D2),要列出候选、要把 id 显示成人看得懂的名字。
pub(super) fn show(
    ui: &mut Ui,
    t: &Theme,
    ui_state: &mut UiState,
    groups: &[GroupRecord],
    sessions: &[SessionRecord],
    presence: SecretPresence,
) {
    // F93:候选只在编辑器打开后、且还没扫过时扫一次,不依赖是否选中了
    // 会话——不放在下面 `let Some(buf) = ..` 之后是因为它跟当前编辑的是
    // 哪条会话无关,纯粹是「这次打开扫没扫过 ~/.ssh」。
    ensure_key_candidates_scanned(ui_state, || {
        super::keyscan::default_ssh_dir()
            .map(|d| super::keyscan::scan(&d))
            .unwrap_or_default()
    });

    // 没选中任何会话 → 空态提示,不画一张什么都填不进去的空表单。
    //
    // 走查 21:一条会话都没有时,「从左侧选一条」是句废话 —— 左侧空的。
    // 分成两句话,第一次打开的人才知道下一步该干什么。**只说这里真能做到
    // 的事**:不提「导入 ~/.ssh/config」之类还没实现的路(那是 F2)。
    let Some(buf) = ui_state.editor.as_mut() else {
        ui.centered_and_justified(|ui| {
            ui.vertical_centered(|ui| {
                if sessions.is_empty() {
                    ui.colored_label(theme::c32(t.fg_dim), "还没有任何会话");
                    ui.add_space(4.0);
                    ui.colored_label(
                        theme::c32(t.fg_dimmer),
                        "点左下角「+ 新建」建第一条:填名称、主机、用户名就能连。",
                    );
                } else {
                    ui.colored_label(theme::c32(t.fg_dimmer), "从左侧选一条会话,或点「+ 新建」");
                }
            });
        });
        return;
    };

    // 走查 21:一次性聚焦标志。在这里 `take()` 而不是让 `fields::basic` 自己
    // 复位:那边只拿得到 `&mut EditorBuffer`,够不着 `UiState`。
    let focus_name = std::mem::take(&mut ui_state.focus_name_request);
    // 走查 14:表单相对「上次保存/刚载入」的基线有没有动过。字段级不相交借用:
    // `buf` 借的是 `ui_state.editor`,这里读的是 `editor_baseline`,互不冲突。
    let dirty = ui_state
        .editor_baseline
        .as_ref()
        .is_some_and(|base| super::is_dirty(buf, base));
    let missing = super::validate::check(&buf.name, &buf.host, &buf.user);
    // 走查 3:名称 + user@host + 端口 + 协议全撞上另一条已存在的会话时提醒一句。
    // **只提醒,不禁用保存** —— 「先存一条一样的、回头改其中一条」是合理用法,
    // 拦下来只会逼用户去改一个假名字再改回来。
    let duplicate = super::dedupe::duplicate_of(
        &buf.name,
        &buf.user,
        &buf.host,
        buf.port.trim().parse().unwrap_or(22),
        buf.protocol,
        ui_state.editor_id,
        sessions,
    )
    .is_some();
    let reason = why(missing, &ui_state.probe);
    // 走查 14:表单跟基线一模一样时,「保存」是个空动作 —— 亮着它只会让用户
    // 反复按、反复怀疑自己有没有存上。
    //
    // **新建草稿例外**(`editor_id.is_none()`):草稿的基线就是它自己刚生成的
    // 那份,永远「不脏」,按 `dirty` 判会把新建这条路整个堵死。
    let unchanged = !dirty && ui_state.editor_id.is_some();
    // 保存被「必填未齐」和「没有改动」挡;拨测在途时仍可保存(它只读表单,不改)。
    let disable_save = missing.any() || unchanged;
    // 保存并连接 / 测试连接 两个原因都挡。
    let disable_connect = !matches!(reason, Disabled::No);

    // 标题条
    ui.horizontal(|ui| {
        let title = if buf.name.trim().is_empty() {
            "新建会话".to_string()
        } else {
            buf.name.clone()
        };
        ui.label(
            egui::RichText::new(title)
                .size(16.0)
                .color(theme::c32(t.fg)),
        );
        // 走查 14:有未保存改动时标题后跟一个圆点。**不改窗口标题** ——
        // `egui::Window::new(title)` 拿标题文本当 id 源,标题一变窗口位置和
        // 尺寸就整个重置。而且脏的是这张表单,不是整个管理器。
        if dirty {
            ui.label(
                egui::RichText::new("•")
                    .size(16.0)
                    .color(theme::c32(t.accent)),
            )
            .on_hover_text("有未保存的更改");
        }
    });
    ui.add_space(6.0);

    // 未保存变更确认横幅(F90 Task 14)。`pending_switch` 落在 `mod.rs::show`
    // 里判脏,脏时置 `confirm_switch=true` 后借用已释放,这里只管画。
    // 「丢弃并切换」不能在这里直接调 `apply_switch`(它要重设
    // `ui_state.editor`,而这里正持着 `buf = ui_state.editor.as_mut()` 的
    // `&mut`,同帧内改不了同一个字段)——中转一个 bool,真正施加挪到
    // `mod.rs::show` 里 `Window::show` 借用释放之后。
    if ui_state.confirm_switch {
        egui::Frame::none()
            .fill(theme::c32(t.sunken_bg))
            .stroke(egui::Stroke::new(1.0, theme::c32(t.warn)))
            .rounding(8.0)
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(theme::c32(t.warn), "有未保存的更改");
                    if ui.button("丢弃并切换").clicked() {
                        ui_state.discard_and_switch = true;
                    }
                    if ui.button("留在这里").clicked() {
                        ui_state.pending_switch = None;
                        ui_state.confirm_switch = false;
                    }
                });
            });
        ui.add_space(8.0);
    }

    // §5.2 错误卡片:比状态栏那行显眼,且可关闭。关闭后下一个新错误会由
    // `UiState::set_error` 重新展开(它复位 error_dismissed)。
    if let (Some(msg), false) = (ui_state.last_error.clone(), ui_state.error_dismissed) {
        // 走查 13:卡片上只放摘要,全文藏在「详情」后面,另给一个「复制」——
        // russh / keyring 的错误动辄好几行带堆栈,整片糊在卡片上会把按钮条
        // 顶出屏幕;而这些字恰恰是用户唯一能拿去搜或者贴给我看的东西,
        // 截断了就等于丢了。
        let (summary, has_more) = summarize(&msg);
        egui::Frame::none()
            .fill(theme::c32(t.sunken_bg))
            .stroke(egui::Stroke::new(1.0, theme::c32(t.danger_soft)))
            .rounding(8.0)
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(theme::c32(t.danger_soft), &summary);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("×").clicked() {
                            ui_state.error_dismissed = true;
                        }
                        if ui.small_button("复制").clicked() {
                            ui.ctx().copy_text(msg.clone());
                            // 直接写字段而不是 `set_toast()`:方法调用要 `&mut`
                            // 整个 `UiState`,而这里 `buf` 正借着 `editor` 字段
                            // (字段级不相交借用只对字段访问成立)。
                            ui_state.pending_toast = Some("错误信息已复制".to_string());
                        }
                        if has_more {
                            let label = if ui_state.error_expanded {
                                "收起"
                            } else {
                                "详情"
                            };
                            if ui.small_button(label).clicked() {
                                ui_state.error_expanded = !ui_state.error_expanded;
                            }
                        }
                    });
                });
                if has_more && ui_state.error_expanded {
                    ui.add_space(6.0);
                    // 只读的多行 `TextEdit` 而不是 `Label`:用户要能划选其中
                    // 一段去搜,`Label` 选不中。
                    ui.add(
                        egui::TextEdit::multiline(&mut msg.as_str())
                            .desired_width(f32::INFINITY)
                            .font(egui::TextStyle::Monospace),
                    );
                }
            });
        ui.add_space(8.0);
    }

    // F92:拨测结果卡片。与 last_error 卡片互斥 —— 两张同款卡片叠在一起
    // 用户分不清哪条是哪条。`last_error` 优先(它多半是刚才保存失败,
    // 更紧急),拨测结果让位但**不清空**,错误关掉后它还在。
    let error_shown = ui_state.last_error.is_some() && !ui_state.error_dismissed;
    if !error_shown {
        let card = match &ui_state.probe {
            super::ProbeState::Idle => None,
            super::ProbeState::Running => Some((t.info, "正在测试连接…".to_owned())),
            super::ProbeState::Ok => Some((t.ok, "连接成功(已立即断开,未记住指纹)".to_owned())),
            super::ProbeState::Err(msg) => Some((t.danger_soft, format!("连接失败:{msg}"))),
        };
        if let Some((color, text)) = card {
            egui::Frame::none()
                .fill(theme::c32(t.sunken_bg))
                .stroke(egui::Stroke::new(1.0, theme::c32(color)))
                .rounding(8.0)
                .inner_margin(10.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(theme::c32(color), text);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("×").clicked() {
                                ui_state.probe = super::ProbeState::Idle;
                            }
                        });
                    });
                });
            ui.add_space(8.0);
        }
    }

    // 复核修正 1:认证方式一旦不是公钥,先前那条讲私钥的拖拽提示就清空——
    // 它挂在一个已经没有私钥字段的表单上,用户看不懂在说什么。
    //
    // 判据**只看 `auth_kind`,不看 `editor_tab`**:切 Tab 只改变「这一帧
    // 看不看得见」,提示本身依旧有意义,与同文件 `last_error`/F92 拨测结果
    // 两张卡片「不随 Tab 门控」的既有约定对齐(它们只由用户点 × 或状态机
    // 产生新结果驱动,不因为切换 Tab 而清空)。`auth_kind` 则不同——它决定
    // 「这条提示对应的字段还在不在表单里」,不是「看不看得见」的问题,所以
    // 单独判、不与 Tab 判据混在一起。
    //
    // 复核者还提到「拖目录被拒后,用户手动在路径框改对了,提示仍挂着」——
    // 这里刻意不修:要检测「手动改对了」得监听私钥缓冲的变化,那是
    // `expire_verdict_if_form_changed` 那套的活(拨测结论与表单强绑定,
    // 必须联动);这条提示只是一次性通知,少点一次 × 换不来监听变化的代价。
    if buf.auth_kind != super::AuthKindUi::PublicKey {
        ui_state.key_drop_note = None;
    }

    // F93:拖拽私钥。只在认证 Tab 且公钥模式下生效 —— 密码模式下拖一个
    // 文件进来没有任何合理含义,静默忽略好过写进一个用户看不到的字段。
    //
    // 放在这里(拨测卡片之后、Tab 条之前)而不是计划原文说的「认证 Tab
    // 渲染完之后」:`show()` 最后一段是 `ScrollArea::vertical()
    // .auto_shrink([false, false])`,吃满剩余高度,追加在它之后的提示条
    // 没有任何空间可用,用户永远看不到。这个位置在借用上安全 ——
    // `buf`(借的是 `ui_state.editor`)存活期间,Rust 允许读写
    // `ui_state` 的其他字段(下面 `ui_state.editor_tab` 的既有代码就是
    // 活证据),所以能同时读 `ui_state.editor_tab`、写
    // `ui_state.key_drop_note`、通过 `buf.key_data`/`buf.auth_kind` 读写。
    if ui_state.editor_tab == super::TAB_AUTH && buf.auth_kind == super::AuthKindUi::PublicKey {
        let hovering = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
        if hovering {
            // 悬停高亮:整个编辑器区域描一圈 accent 边。
            ui.painter().rect_stroke(
                ui.max_rect(),
                8.0,
                egui::Stroke::new(2.0, theme::c32(t.accent)),
            );
        }

        // 复核修正 3:这里读到的 `dropped_files` 只在真实发生拖放的那一帧
        // 非空 —— egui 在 `Context::run` 里对每个 pass 调 `RawInput::take()`
        // 时会 `std::mem::take(&mut self.dropped_files)`(egui-0.30.0
        // `src/data/input.rs:131`,已核实),拿走后原地留空;而 `RawInput`
        // 本身是调用方(`app.rs` 的 winit 事件循环,或测试里的 `ctx.run`
        // 入参)每帧新建的一个值,只有真的收到一次 OS 拖放事件那一帧才会
        // 塞进 `dropped_files`。所以下面 `decide_key_drop` 里的 `is_dir()`
        // (一次 `stat` 系统调用)不会变成每帧一次 IO,只在真实拖放发生的
        // 那一帧才跑一次——这条依赖 egui 内部实现,不是本地代码能看出来的,
        // 以后如果换了拖放输入源(比如自己维护跨帧输入队列),要重新核对
        // 这条前提还成不成立。
        let dropped: Vec<std::path::PathBuf> = ui.ctx().input(|i| {
            i.raw
                .dropped_files
                .iter()
                // `DroppedFile.path` 在 Web 上是 None;桌面端偶尔也会是
                // None(拖的是剪贴板内容而非文件)。没有路径就没法用,静默跳过。
                .filter_map(|f| f.path.clone())
                .collect()
        });

        match decide_key_drop(&dropped, |p| p.is_dir()) {
            KeyDrop::Nothing => {}
            KeyDrop::Rejected(note) => {
                ui_state.key_drop_note = Some(note);
            }
            KeyDrop::Accepted { path, note } => {
                // v5:拖进来的文件当场读成正文存进缓冲,路径不留。
                super::import_key_file(buf, std::path::Path::new(&path), |p| {
                    std::fs::read_to_string(p)
                });
                // 导入自己的提示(「已导入 x」/「不像私钥」)优先;
                // `note` 只在「拖了多个、只取第一个」时才有内容,补在后面。
                ui_state.key_drop_note = match (buf.key_note.take(), note) {
                    (Some(a), Some(b)) => Some(format!("{a};{b}")),
                    (a, b) => a.or(b),
                };
            }
        }
    }

    // 提示条。放在按钮条上方,与错误/拨测卡片同一列。
    if let Some(note) = ui_state.key_drop_note.clone() {
        ui.horizontal(|ui| {
            ui.colored_label(theme::c32(t.fg_dimmer), note);
            if ui.small_button("×").clicked() {
                ui_state.key_drop_note = None;
            }
        });
        ui.add_space(8.0);
    }

    // Tab 条
    ui.horizontal(|ui| {
        for (i, name) in TABS.iter().enumerate() {
            // F91:缺项所在的 Tab 标一个红点,否则用户看到按钮灰着
            // 却不知道该翻哪一页。走查 9 把它扩成三态(单一状态位):
            // 缺项红● > 已配置灰· > 无,判据在 `tab_badge`。
            //
            // 用 `LayoutJob` 而不是 `format!("{name} ●")` + `RichText::color`:
            // 后者只能给整条 label 一个颜色,选中的那一页会连 Tab 名一起变红。
            // `LayoutJob` 能分段着色,而 Tab 名那段给 `Color32::PLACEHOLDER`
            // 就还能保留 egui 的选中态/hover 配色 —— `SelectableLabel` 最后
            // 是 `painter().galley(pos, galley, visuals.text_color())`,第三参
            // 是 **fallback**,epaint 只替换 PLACEHOLDER 的部分(egui-0.30.0
            // `widgets/selected_label.rs` / epaint-0.30.0 `tessellator.rs`)。
            let badge = super::tab_badge::badge_of(i, missing, buf);
            let font = egui::TextStyle::Button.resolve(ui.style());
            let mut job = egui::text::LayoutJob::default();
            job.append(
                name,
                0.0,
                egui::TextFormat {
                    font_id: font.clone(),
                    color: egui::Color32::PLACEHOLDER,
                    ..Default::default()
                },
            );
            if let Some((sym, color)) = match badge {
                super::tab_badge::Badge::Missing => Some(("●", theme::c32(t.danger))),
                super::tab_badge::Badge::Configured => Some(("·", theme::c32(t.fg_dimmer))),
                super::tab_badge::Badge::None => None,
            } {
                job.append(
                    sym,
                    4.0,
                    egui::TextFormat {
                        font_id: font,
                        color,
                        ..Default::default()
                    },
                );
            }
            if ui.selectable_label(ui_state.editor_tab == i, job).clicked() {
                ui_state.editor_tab = i;
            }
        }
    });
    ui.separator();

    // 底部按钮条用 TopBottomPanel 先占位,Tab 内容吃剩余高度。
    //
    // **不要写成 `let bottom = 44.0; let body_h = ui.available_height() - bottom;`**
    // 再喂给 `ScrollArea::max_height` —— 左栏原本就是这么写的,在 Windows 11
    // 实机上把「+ 新建」按钮顶出了可见区(见 c4eb7f1)。两个原因:
    // `ui.available_height()` 在 panel 内返回的是 `Window` 的**布局高度**而非
    // 真实可见高度;硬编码的 44.0 必须与底栏实际渲染高度保持同步,一旦界面缩放
    // 或字号变大就失同步,且没有任何编译错误或测试会提示。
    // panel 布局天然保证「panel 先分配、中央区吃剩余」,不需要猜数字。
    //
    // `why()` 里已经构造过同一份文本,别每个按钮再各建一次 String——
    // `reason` 是 `Disabled::Missing` 当且仅当 `missing.any()` 为真
    // (`why()` 的分支顺序保证了这一点)。
    let missing_tip = match &reason {
        Disabled::Missing(h) => Some(h.as_str()),
        _ => None,
    };
    // 走查 14:「保存」比其它按钮多一个禁用原因。缺项优先 —— 表单都没填齐时
    // 说「没有需要保存的改动」是在误导。
    let save_tip = missing_tip.or(unchanged.then_some("没有需要保存的改动"));
    egui::TopBottomPanel::bottom(ui.id().with("sm_editor_bottom"))
        .frame(egui::Frame::none())
        .show_separator_line(false)
        .show_inside(ui, |ui| {
            ui.separator();
            // 走查 3:重名提醒贴在按钮条上方 —— 决定要不要按「保存」就在这一眼里。
            if duplicate {
                ui.colored_label(
                    theme::c32(t.warn),
                    "已有一条名称、用户、主机、端口、协议都相同的会话",
                );
            }
            // 布局:[测试连接] [复制连接串] ……… [取消] [保存] [保存并连接]
            // 唯一的实心主按钮是最右的「保存并连接」——按钮全一个样,
            // 用户就只能靠读字来找主操作。
            //
            // 不许写成 `let bottom = 44.0; let body_h = available_height() - bottom;`
            // 再喂给 ScrollArea::max_height(c4eb7f1 踩过的坑):按钮条真实高度
            // 随字号/缩放变,手算的估值一旦偏小,滚动区就把按钮压出面板外。
            ui.horizontal(|ui| {
                let probe_tip = tip(&reason);
                if super::labeled_button(
                    ui,
                    super::probe_button_id(),
                    "测试连接",
                    !disable_connect,
                    probe_tip.as_deref(),
                ) {
                    ui_state.probe_click = true;
                    // F92:记下发起这一刻的表单快照,供拨测结果返回后判定
                    // 「表单有没有改过」——不能拿 `editor_baseline`(上次保存
                    // 基线),见 `UiState::probe_form` 的文档注释。
                    ui_state.probe_form = Some(buf.clone());
                }
                if super::labeled_button(
                    ui,
                    super::copy_button_id(),
                    "复制连接串",
                    // 走查 14:这里判的是 `missing.any()` 而**不是** `disable_save` ——
                    // 「表单没改过」跟「能不能拼出一条连接串」毫无关系,跟着保存
                    // 一起灰掉会让「打开会话、复制连接串」这条最常见的路走不通。
                    !missing.any(),
                    missing_tip,
                ) {
                    ui.ctx().copy_text(super::connect_string(buf));
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // 主按钮:accent 底 + accent_fg 字。全场唯一一个实心。
                    // 圆角不硬编码 7.0——从当前样式取值(theme.rs:186 的
                    // `round` 经 `apply_egui` 写进了 `widgets.inactive.rounding`),
                    // 理由同上一提交把 `labeled_button` 里同样的硬编码换成
                    // `visuals.rounding`:硬编码值今天凑巧等于 theme.rs 设的值,
                    // 以后调 theme 这里不会跟着变,也没有任何编译错误/测试提示。
                    let rounding = ui.visuals().widgets.inactive.rounding;
                    let primary = egui::Button::new(
                        egui::RichText::new("保存并连接").color(theme::c32(t.accent_fg)),
                    )
                    .fill(theme::c32(t.accent))
                    .stroke(egui::Stroke::NONE)
                    .rounding(rounding);
                    let mut save_connect = false;
                    let resp = ui.add_enabled(!disable_connect, primary);
                    if let Some(msg) = tip(&reason) {
                        resp.clone().on_disabled_hover_text(msg);
                    }
                    save_connect |= resp.clicked();

                    let save = super::labeled_button(
                        ui,
                        super::save_button_id(),
                        "保存",
                        !disable_save,
                        save_tip,
                    );

                    // 走查 16:底部「取消」按钮已删。它做的事(把右栏退回空态)
                    // 跟「关掉管理器」高度重合,而后者现在有 Esc、标题栏 × 两条
                    // 路;留着两个语义相近的出口只会让人犹豫按哪个。
                    if save || save_connect {
                        ui_state.save_click = Some(save_connect);
                    }
                });
            });
        });

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| match ui_state.editor_tab {
            super::TAB_AUTH => super::fields::auth(
                ui,
                t,
                buf,
                presence,
                &ui_state.key_candidates,
                &mut ui_state.touched,
            ),
            super::TAB_AUTOMATION => super::fields::automation(ui, t, buf, groups),
            super::TAB_APPEARANCE => {
                super::fields::appearance(ui, t, buf, &mut ui_state.icon_error)
            }
            // `TAB_CONNECT` 与「越界值兜底」合并成同一个分支:`editor_tab`
            // 是既有的裸 usize 技术债,越界值落回首页比 panic 好。
            _ => super::fields::basic(
                ui,
                t,
                buf,
                groups,
                sessions,
                ui_state.editor_id,
                presence,
                focus_name,
                &mut ui_state.touched,
            ),
        });

    // F92:表单一改,上一次的成功/失败结论就不再可信 —— 但**不动世代号**:
    // 在途那次仍是针对这份表单发起的,让它跑完;世代号只在切会话/关窗时才变
    //(`cancel_probe`,app.rs)。这里够不着世代号,也不该够得着。
    expire_verdict_if_form_changed(ui_state);
}

/// F93:确保 `~/.ssh` 私钥候选已经扫过。**不能每帧扫**——那是每秒几十次
/// `read_dir`,与本项目陷阱 T3(把 IO 塞进渲染热路径)是同一类问题,只是
/// 这次的 IO 源头是文件系统扫描而不是 GPU 重绘。`ready` 标记保证一次打开
/// 编辑器只扫一次;关闭编辑器时(`UiState::close_session_manager`)会把
/// `ready` 复位,下次打开重新扫——用户可能刚 `ssh-keygen` 生成了新密钥。
///
/// `scan_dir` 由调用方注入,而不是在函数体内直接调
/// `keyscan::default_ssh_dir().map(|d| keyscan::scan(&d))`——这是为了让
/// 「只扫一次」这个属性可测:测试传一个计数闭包,就能断言第二次调用不再
/// 触发扫描,不需要真的去碰进程运行机器上的 `~/.ssh`。
fn ensure_key_candidates_scanned(
    ui_state: &mut UiState,
    scan_dir: impl FnOnce() -> Vec<std::path::PathBuf>,
) {
    if !ui_state.key_candidates_ready {
        ui_state.key_candidates = scan_dir();
        ui_state.key_candidates_ready = true;
    }
}

/// 表单相对「发起拨测那一刻」变了吗?变了就作废已出的结论。
/// 抽成纯函数是为了能脱离 egui 单测 —— 这段逻辑的 bug 在界面上表现为
/// 「卡片一闪而过」,靠眼睛抓不住。
///
/// 参数拿整个 `&mut UiState` 而不是拆开传字段:基线**必须**是
/// `probe_form`(发起拨测那一刻的快照),不能是 `editor_baseline`
/// (上次保存的基线)——后者是持久状态,新建会话时表单天然相对它是脏的,
/// 用它当基线会让结论在产生的下一帧就被清掉,成功卡片一帧都看不见。
/// 把「取哪个字段当基线」留在函数体内、而不是让调用方传参决定,是为了让
/// 这行取值本身成为受测的产品代码——不然误传错字段这类 bug 会绕过测试。
///
/// 在途的 `Running` 不受影响 —— 那次拨测仍是针对这份表单发起的,让它
/// 跑完;只有已经出结论(`Ok`/`Err`)的才会被作废。
///
/// 安全专项终审:结论一旦作废,连带清空 `probe_form`(发起拨测那一刻的
/// 明文表单快照,含 `password`/`passphrase`/`proxy_password`)——这份快照
/// 唯一的用途就是给这次「表单相对拨测时刻变了没有」的比对当基线,结论已经
/// 作废,它就没有下一个消费者了,继续留着只会拉长明文在内存里的驻留时间
/// (用户可能测完一次连接后一直在同一张表单上编辑,既不再测、也不切换、
/// 不关闭,不清的话这份快照能挂满整个编辑会话)。这里清空是安全的:上面
/// `matches!` 只匹配 `Ok`/`Err`,`Running` 时即便 `changed` 为真也走不进
/// 这个分支——在途拨测仍需要这份快照当「发起时是什么样」的凭证,不会被
/// 这里误清。
///
/// **本函数无条件执行,不受 `error_shown`(错误卡片是否正在遮住拨测卡片)
/// 门控**——这是刻意的,不是漏接。`error_shown` 管的是「这一帧画哪张卡片」,
/// 纯渲染层的事;这里管的是「这份结论对不对得上当前表单」,是状态机的事,
/// 判据是表单变没变,跟此刻屏幕上显示哪张卡片毫无关系。如果反过来把作废
/// 也塞进 `!error_shown` 门控,会造成更糟的结果:错误卡片显示期间用户
/// 改了表单,一个已经对不上当前表单的陈旧「连接成功」不会被作废,等用户
/// 关掉错误卡片,它会原样重新出现——这是在给用户一个假保证,比「结论在
/// 用户看不见的时候被静默作废」严重得多。所以宁可让作废发生得不可见,
/// 也不能让陈旧结论在关卡片后复活。
fn expire_verdict_if_form_changed(ui_state: &mut UiState) {
    let changed = match (ui_state.editor.as_ref(), ui_state.probe_form.as_ref()) {
        (Some(cur), Some(at_probe)) => super::is_dirty(cur, at_probe),
        _ => false,
    };
    if changed
        && matches!(
            ui_state.probe,
            super::ProbeState::Ok | super::ProbeState::Err(_)
        )
    {
        ui_state.probe = super::ProbeState::Idle;
        ui_state.probe_form = None;
    }
}

/// 拖进来的一组文件该怎么处理。抽成纯函数,是因为「取第一个 / 拒绝目录 /
/// 多文件给提示」这三条规则就是这个功能的全部行为,而真实拖放在无头环境
/// 里完全测不了(要真实窗口 + 真实文件管理器)。判目录用注入的闭包而不是
/// 直接调 `Path::is_dir()`,是为了让测试不必在磁盘上真的造一个目录。
#[derive(Debug, PartialEq, Eq)]
enum KeyDrop {
    /// 这一帧没有可用的拖放(没拖东西,或拖进来的东西都没有路径)。
    Nothing,
    /// 拖进来的第一个是目录 —— 拒绝,并说明原因。
    Rejected(String),
    /// 接受第一个文件。`note` 只在忽略了其余文件时给出说明。
    Accepted { path: String, note: Option<String> },
}

fn decide_key_drop(
    dropped: &[std::path::PathBuf],
    is_dir: impl Fn(&std::path::Path) -> bool,
) -> KeyDrop {
    let Some(first) = dropped.first() else {
        return KeyDrop::Nothing;
    };
    if is_dir(first) {
        return KeyDrop::Rejected("请拖入私钥文件,不是目录".to_owned());
    }
    let note = if dropped.len() > 1 {
        // 一个路径框只能放一条路径。明说忽略了几个,好过让用户以为
        // 拖进去的另外几个也生效了。
        Some(format!("已取第一个文件,忽略其余 {} 个", dropped.len() - 1))
    } else {
        None
    };
    KeyDrop::Accepted {
        path: first.display().to_string(),
        note,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        decide_key_drop, ensure_key_candidates_scanned, expire_verdict_if_form_changed, summarize,
        tip, why, Disabled, KeyDrop, SUMMARY_CHARS,
    };
    use crate::ui::session_manager::validate::Missing;
    use crate::ui::session_manager::{AuthKindUi, EditorBuffer, ProbeState, TAB_AUTH, TAB_CONNECT};
    use crate::ui::UiState;
    use mullion_store::SessionRecord;

    /// 按 Tab 名找到那个 galley 的 `LayoutJob`。
    fn find_galley_job(
        shapes: &[egui::epaint::ClippedShape],
        starts_with: &str,
    ) -> Option<egui::text::LayoutJob> {
        fn walk(shape: &egui::Shape, starts_with: &str, out: &mut Option<egui::text::LayoutJob>) {
            match shape {
                egui::Shape::Text(ts) => {
                    if out.is_none() && ts.galley.text().starts_with(starts_with) {
                        *out = Some((*ts.galley.job).clone());
                    }
                }
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, starts_with, out)),
                _ => {}
            }
        }
        let mut out = None;
        shapes
            .iter()
            .for_each(|cs| walk(&cs.shape, starts_with, &mut out));
        out
    }

    /// 把这一帧画出来的所有文字摊平成一串,用来断言某句话出现/没出现。
    fn all_text(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(ts) => out.push(ts.galley.text().to_string()),
                _ => {}
            }
        }
        let mut acc = Vec::new();
        shapes.iter().for_each(|cs| walk(&cs.shape, &mut acc));
        acc
    }

    /// 按**完整相等**的文字找到它的绘制锚点。`find_galley_job` 用的是
    /// `starts_with`,分不出「保存」和「保存并连接」——要点中前者只能用全等。
    fn find_text_pos_exact(
        shapes: &[egui::epaint::ClippedShape],
        needle: &str,
    ) -> Option<egui::Pos2> {
        fn walk(shape: &egui::Shape, needle: &str) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, needle)),
                egui::Shape::Text(ts) if ts.galley.text() == needle => Some(ts.pos),
                _ => None,
            }
        }
        shapes.iter().find_map(|cs| walk(&cs.shape, needle))
    }

    /// 跑两帧 `editor::show`,返回第二帧输出。
    fn run_editor(buf: EditorBuffer) -> egui::FullOutput {
        run_editor_with(buf, &[], None)
    }

    /// 同上,但带一份已存在的会话表(重名检测要它)和「正在编辑哪一条」。
    fn run_editor_with(
        buf: EditorBuffer,
        sessions: &[SessionRecord],
        editor_id: Option<mullion_store::SessionId>,
    ) -> egui::FullOutput {
        let t = crate::theme::MULLION_DARK;
        let mut ui_state = UiState {
            editor: Some(buf),
            editor_id,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut run = || {
            ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    super::show(
                        ui,
                        &t,
                        &mut ui_state,
                        &[],
                        sessions,
                        crate::ui::session_manager::SecretPresence::default(),
                    );
                });
            })
        };
        let _ = run();
        run()
    }

    /// 走查 21:一条会话都没有时,「从左侧选一条会话」是句废话 —— 左侧空的。
    /// 而且空态**不许承诺做不到的事**(`~/.ssh/config` 导入是 F2,还没做)。
    ///
    /// 自证会变红:把 `sessions.is_empty()` 分支删掉 → 空库时画的还是那句
    /// 「从左侧选一条会话」。
    #[test]
    fn the_empty_state_tells_a_first_time_user_what_to_do_without_promising_an_import() {
        fn texts(sessions: &[SessionRecord]) -> Vec<String> {
            let t = crate::theme::MULLION_DARK;
            let mut ui_state = UiState::default(); // editor = None
            let ctx = egui::Context::default();
            let out = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    super::show(
                        ui,
                        &t,
                        &mut ui_state,
                        &[],
                        sessions,
                        crate::ui::session_manager::SecretPresence::default(),
                    );
                });
            });
            fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
                match shape {
                    egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                    egui::Shape::Text(ts) => out.push(ts.galley.text().to_string()),
                    _ => {}
                }
            }
            let mut acc = Vec::new();
            out.shapes.iter().for_each(|cs| walk(&cs.shape, &mut acc));
            acc
        }

        let empty = texts(&[]);
        assert!(
            empty.iter().any(|s| s.contains("还没有任何会话")),
            "空库时该说清楚现在是什么状况:{empty:?}"
        );
        assert!(
            empty.iter().any(|s| s.contains("+ 新建")),
            "空库时该指出下一步该点哪儿:{empty:?}"
        );
        assert!(
            !empty
                .iter()
                .any(|s| s.contains("导入") || s.contains("ssh/config")),
            "不许承诺还没实现的 ~/.ssh/config 导入(那是 F2):{empty:?}"
        );

        // 库里有会话、只是没选中 → 还是原来那句。
        let one = texts(std::slice::from_ref(&sample_record()));
        assert!(
            one.iter().any(|s| s.contains("从左侧选一条会话")),
            "有会话时该引导去左边选:{one:?}"
        );
    }

    /// 走查 21:点完「+ 新建」,光标必须已经在「名称」框里 —— 不然用户得先
    /// 拿鼠标点一下才能打字,新建这条路上白多一次交互。
    ///
    /// 断言方式是**真的敲一个字**看它落在哪儿,而不是去比对 `focused()` 的
    /// 内部 id:后者要复刻 egui 的 id 生成规则,规则一变测试就假绿。
    ///
    /// 自证会变红:把 `name_resp.request_focus()` 删掉 → 字符没有落点,
    /// `name` 仍是空的。
    #[test]
    fn clicking_new_puts_the_caret_in_the_name_field_so_you_can_just_type() {
        let t = crate::theme::MULLION_DARK;
        let mut ui_state = UiState {
            editor: Some(EditorBuffer::new_draft()),
            editor_id: None,
            focus_name_request: true,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut run = |input: egui::RawInput| {
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    super::show(
                        ui,
                        &t,
                        &mut ui_state,
                        &[],
                        &[],
                        crate::ui::session_manager::SecretPresence::default(),
                    );
                });
            });
        };
        // 第一帧:表单建出来并请求聚焦。
        run(egui::RawInput::default());
        // 第二帧:直接敲字,不点任何东西。
        run(egui::RawInput {
            events: vec![egui::Event::Text("w".into())],
            ..Default::default()
        });
        assert_eq!(
            ui_state.editor.as_ref().unwrap().name,
            "w",
            "新建后直接打字该落进「名称」框"
        );
        assert!(
            !ui_state.focus_name_request,
            "聚焦请求是一次性的,用完必须清掉,否则光标每帧被拽回名称框"
        );
    }

    /// 走查 14:有未保存改动时,右栏标题后面跟一个圆点;存过/没动过就没有。
    ///
    /// 圆点挂在**右栏标题**上而不是窗口标题上:`egui::Window::new(title)` 拿
    /// 标题文本当 id 源,标题一变窗口位置尺寸整个重置。
    ///
    /// 自证会变红:把 `if dirty` 改成 `if true`,第一段断言(没改动时不该有
    /// 圆点)立刻炸。
    #[test]
    fn an_unsaved_form_marks_its_title_with_a_dot() {
        let base = EditorBuffer {
            name: "web01".into(),
            host: "10.0.0.1".into(),
            port: "22".into(),
            user: "root".into(),
            ..Default::default()
        };
        let has_dot = |buf: EditorBuffer, baseline: EditorBuffer| {
            let t = crate::theme::MULLION_DARK;
            let mut ui_state = UiState {
                editor: Some(buf),
                editor_baseline: Some(baseline),
                editor_id: Some(mullion_store::SessionId(1)),
                ..Default::default()
            };
            let ctx = egui::Context::default();
            let mut run = || {
                ctx.run(egui::RawInput::default(), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        super::show(
                            ui,
                            &t,
                            &mut ui_state,
                            &[],
                            &[],
                            crate::ui::session_manager::SecretPresence::default(),
                        );
                    });
                })
            };
            let _ = run();
            find_text_pos_exact(&run().shapes, "•").is_some()
        };

        assert!(
            !has_dot(base.clone(), base.clone()),
            "跟基线一模一样时不该有圆点 —— 那会让用户永远以为自己没存上"
        );
        let mut edited = base.clone();
        edited.note = "改了一句".into();
        assert!(has_dot(edited, base), "改过没存该有圆点");
    }

    /// 走查 14:没改过就没什么可存的,「保存」置灰。**新建草稿例外** ——
    /// 草稿的基线就是它自己刚生成的那份,永远「不脏」,按 `dirty` 判会把
    /// 新建这条路整个堵死。
    ///
    /// 自证会变红:把 `unchanged` 里的 `&& ui_state.editor_id.is_some()`
    /// 去掉,第三段断言(新建草稿必须能存)立刻炸;把整个 `|| unchanged`
    /// 去掉,第一段炸。
    #[test]
    fn save_is_greyed_out_until_something_actually_changes() {
        let filled = EditorBuffer {
            name: "web01".into(),
            host: "10.0.0.1".into(),
            port: "22".into(),
            user: "root".into(),
            ..Default::default()
        };
        // 真的点一下「保存」,看意图有没有落下来 —— 按钮禁用时
        // `labeled_button` 根本不报 click,这是唯一能分辨禁用与否的判据。
        let clicks_through =
            |buf: EditorBuffer,
             baseline: EditorBuffer,
             editor_id: Option<mullion_store::SessionId>| {
                let t = crate::theme::MULLION_DARK;
                let mut ui_state = UiState {
                    editor: Some(buf),
                    editor_baseline: Some(baseline),
                    editor_id,
                    ..Default::default()
                };
                let ctx = egui::Context::default();
                let run = |ui_state: &mut UiState, input: egui::RawInput| {
                    ctx.run(input, |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            super::show(
                                ui,
                                &t,
                                ui_state,
                                &[],
                                &[],
                                crate::ui::session_manager::SecretPresence::default(),
                            );
                        });
                    })
                };
                let _ = run(&mut ui_state, egui::RawInput::default());
                let out = run(&mut ui_state, egui::RawInput::default());
                let pos = find_text_pos_exact(&out.shapes, "保存").expect("「保存」按钮该画出来了");
                let click = |pressed| egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::default(),
                };
                let _ = run(
                    &mut ui_state,
                    egui::RawInput {
                        events: vec![egui::Event::PointerMoved(pos), click(true), click(false)],
                        ..Default::default()
                    },
                );
                ui_state.save_click.is_some()
            };

        let id = Some(mullion_store::SessionId(1));
        assert!(
            !clicks_through(filled.clone(), filled.clone(), id),
            "一个字都没改,「保存」该是灰的"
        );

        let mut edited = filled.clone();
        edited.note = "改了一句".into();
        assert!(
            clicks_through(edited, filled.clone(), id),
            "改过之后「保存」必须能按"
        );

        assert!(
            clicks_through(filled.clone(), filled, None),
            "新建草稿的基线就是它自己,按 dirty 判会把新建这条路整个堵死"
        );
    }

    /// 走查 13:错误摘要按 **char** 截。错误信息里几乎必然有中文,按字节切
    /// 会切在多字节字符中间当场 panic —— 而这是「已经出错了」才走到的路径,
    /// 崩在这儿等于把真正的错误信息永远埋掉。
    ///
    /// 自证会变红:把 `summarize` 里的 `chars[..SUMMARY_CHARS]` 换成
    /// `&first[..SUMMARY_CHARS]`,第三段直接 panic。
    #[test]
    fn a_long_multibyte_error_is_summarised_on_char_boundaries() {
        let (s, more) = summarize("连接被拒绝");
        assert_eq!(s, "连接被拒绝");
        assert!(!more, "单行短消息没有更多内容可展开");

        let (s, more) = summarize("保存失败\n堆栈第二行\n第三行");
        assert_eq!(s, "保存失败", "摘要只取首行");
        assert!(more, "还有别的行 → 该给「详情」");

        let long = "认证失败".repeat(40); // 160 个中文字符,单行
        let (s, more) = summarize(&long);
        assert!(more, "超长首行也该能展开看全文");
        assert_eq!(
            s.chars().count(),
            SUMMARY_CHARS + 1,
            "截断后应是 {SUMMARY_CHARS} 个字加一个省略号"
        );
        assert!(s.ends_with('…'));
    }

    /// 走查 13:多行错误的卡片上给「详情」和「复制」;点「详情」才展开全文。
    /// 全文是用户唯一能拿去搜、能贴给别人看的东西,埋掉就等于没报错。
    ///
    /// 自证会变红:把「复制」按钮删掉,第二段断言炸。
    #[test]
    fn a_multiline_error_offers_details_and_a_way_to_copy_it() {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut ui_state = UiState {
            editor: Some(EditorBuffer::default()),
            ..Default::default()
        };
        ui_state.set_error("保存失败:keyring 拒绝写入\n第二行细节\n第三行细节".into());
        let run = |ui_state: &mut UiState, input: egui::RawInput| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    super::show(
                        ui,
                        &t,
                        ui_state,
                        &[],
                        &[],
                        crate::ui::session_manager::SecretPresence::default(),
                    );
                });
            })
        };
        let _ = run(&mut ui_state, egui::RawInput::default());
        let out = run(&mut ui_state, egui::RawInput::default());
        let texts = all_text(&out.shapes);

        assert!(
            texts.iter().any(|s| s == "保存失败:keyring 拒绝写入"),
            "卡片上该是首行摘要:{texts:?}"
        );
        assert!(
            !texts.iter().any(|s| s.contains("第三行细节")),
            "没展开时不该把整片堆栈糊在卡片上:{texts:?}"
        );
        assert!(texts.iter().any(|s| s == "复制"), "该给复制按钮:{texts:?}");

        // 点「详情」→ 全文出来。
        let pos = find_text_pos_exact(&out.shapes, "详情").expect("多行错误该给「详情」按钮");
        let click = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let _ = run(
            &mut ui_state,
            egui::RawInput {
                events: vec![egui::Event::PointerMoved(pos), click(true), click(false)],
                ..Default::default()
            },
        );
        assert!(ui_state.error_expanded, "点「详情」该展开");
        let out = run(&mut ui_state, egui::RawInput::default());
        let texts = all_text(&out.shapes);
        assert!(
            texts.iter().any(|s| s.contains("第三行细节")),
            "展开后必须能看到全文:{texts:?}"
        );
    }

    /// 上面那条测试用的一条最小会话记录。
    fn sample_record() -> SessionRecord {
        use mullion_store::model::{Auth, AuthKind, Connection, Identity, Protocol};
        SessionRecord {
            id: mullion_store::SessionId(1),
            modified_at: "t".into(),
            identity: Identity {
                name: "web01".into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: "10.0.0.1".into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "root".into(),
                kind: AuthKind::Password,
            },
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
        }
    }

    /// 走查 3:表单跟一条已存在的会话完全撞上时,按钮条上方出一句提醒;
    /// 没撞上时不出。**提醒不禁用保存** —— 「先存一条一样的、回头改其中一条」
    /// 是合理用法,拦下来只会逼用户去改个假名字再改回来。
    #[test]
    fn a_form_that_duplicates_an_existing_session_gets_a_warning_but_can_still_be_saved() {
        use mullion_store::model::{
            Auth, AuthKind, Connection, Identity, Protocol, SessionRecord as Rec,
        };
        let existing = Rec {
            id: mullion_store::SessionId(1),
            modified_at: "t".into(),
            identity: Identity {
                name: "web01".into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: "10.0.0.1".into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "root".into(),
                kind: AuthKind::Password,
            },
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
        };
        let same = EditorBuffer {
            name: "web01".into(),
            host: "10.0.0.1".into(),
            port: "22".into(),
            user: "root".into(),
            ..Default::default()
        };

        const WARN: &str = "已有一条名称、用户、主机、端口、协议都相同的会话";

        // 新建一条一模一样的 → 提醒。
        let out = run_editor_with(same.clone(), std::slice::from_ref(&existing), None);
        assert!(
            find_galley_job(&out.shapes, WARN).is_some(),
            "撞上已有会话时应该给一句提醒"
        );
        // 提醒归提醒,「保存」不许被禁掉:真的点一下,意图必须落下来。
        // (不看按钮文字颜色 —— 那分不出「保存」和「保存并连接」,而且主按钮
        // 本来就显式染色,禁用与否颜色都一样。)
        {
            let t = crate::theme::MULLION_DARK;
            let mut ui_state = UiState {
                editor: Some(same.clone()),
                ..Default::default()
            };
            let ctx = egui::Context::default();
            let sessions = std::slice::from_ref(&existing);
            let run = |ui_state: &mut UiState, input: egui::RawInput| {
                ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        super::show(
                            ui,
                            &t,
                            ui_state,
                            &[],
                            sessions,
                            crate::ui::session_manager::SecretPresence::default(),
                        );
                    });
                })
            };
            let _ = run(&mut ui_state, egui::RawInput::default());
            let out = run(&mut ui_state, egui::RawInput::default());
            let pos = find_text_pos_exact(&out.shapes, "保存").expect("「保存」按钮该画出来了");
            let click = |pos, pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            };
            let _ = run(
                &mut ui_state,
                egui::RawInput {
                    events: vec![
                        egui::Event::PointerMoved(pos),
                        click(pos, true),
                        click(pos, false),
                    ],
                    ..Default::default()
                },
            );
            assert_eq!(
                ui_state.save_click,
                Some(false),
                "重名只是提醒,不该把「保存」禁掉"
            );
        }

        // 改一个字段(主机)→ 不再是重复,提醒消失。
        let mut other = same.clone();
        other.host = "10.0.0.2".into();
        let out = run_editor_with(other, std::slice::from_ref(&existing), None);
        assert!(
            find_galley_job(&out.shapes, WARN).is_none(),
            "主机不同就不是重复,不该提醒"
        );

        // 编辑**这一条**自己 → 不该提醒自己跟自己重复。
        let out = run_editor_with(
            same,
            std::slice::from_ref(&existing),
            Some(mullion_store::SessionId(1)),
        );
        assert!(
            find_galley_job(&out.shapes, WARN).is_none(),
            "编辑自己不该被判成重复,否则改一个字就弹一次"
        );
    }

    /// 走查 9:角标必须是**两段不同颜色**的一个 galley —— Tab 名保留 egui 的
    /// 选中态配色(`Color32::PLACEHOLDER`,由 `SelectableLabel` 用
    /// `visuals.text_color()` 填),角标自己是显式的红/灰。
    ///
    /// 整条 label 染成一色的话,选中的那一页文字会跟着角标变红 —— 那是渲染
    /// 出来才看得见的 bug,编译器和普通文字断言都拦不住。
    #[test]
    fn tab_badge_colors_only_the_dot_not_the_tab_name() {
        let t = crate::theme::MULLION_DARK;
        // 「登录后」页配过东西 → 灰点;必填齐全 → 没有红点。
        let mut buf = EditorBuffer {
            name: "web01".into(),
            host: "10.0.0.1".into(),
            user: "root".into(),
            ..Default::default()
        };
        buf.preserved_automation.enabled = Some(false);

        let out = run_editor(buf);
        let job = find_galley_job(&out.shapes, "登录后").expect("没找到「登录后」这个 Tab");
        assert!(
            job.sections.len() >= 2,
            "Tab 名和角标必须分段,否则没法各自着色:{:?}",
            job.text
        );
        assert_eq!(
            job.sections[0].format.color,
            egui::Color32::PLACEHOLDER,
            "Tab 名必须留给 SelectableLabel 填色,否则选中态高亮会失效"
        );
        assert_eq!(
            job.sections[1].format.color,
            crate::theme::c32(t.fg_dimmer),
            "「已配置」角标应该是 fg_dimmer 灰"
        );
    }

    /// 缺必填时角标是 danger 红,且压过「已配置」。
    #[test]
    fn missing_badge_is_danger_red_and_wins() {
        let t = crate::theme::MULLION_DARK;
        // 名称空 → 「连接」页缺项;同时给「连接」页配上代理 → 也满足「已配置」。
        let buf = EditorBuffer {
            host: "10.0.0.1".into(),
            user: "root".into(),
            proxy_mode: crate::ui::session_manager::ProxyModeUi::Socks5,
            ..Default::default()
        };

        let out = run_editor(buf);
        let job = find_galley_job(&out.shapes, "连接").expect("没找到「连接」这个 Tab");
        assert_eq!(
            job.sections[1].format.color,
            crate::theme::c32(t.danger),
            "缺项角标应该是 danger 红,且压过灰点"
        );
    }

    /// Tab 常量必须与 `TABS` 一一对应。合并「高级」后只剩 4 页。
    ///
    /// `TAB_AUTH` 的下标不能变:`editor.rs` 里拖入私钥文件的门控写的是
    /// `editor_tab == TAB_AUTH && auth_kind == PublicKey`,而
    /// `validate::Missing::tab()` 也返回 `TAB_AUTH`。下标一漂,这两处
    /// 全部悄悄失准,没有任何编译错误。
    ///
    /// 覆盖不到的:`show()` 里 match 是否给每个下标都接了对应的 `fields::*`。
    /// 合并「高级」后 `_ =>` 兜底分支已经和 `TAB_CONNECT` 合成同一支,渲染的是
    /// 「连接」页(不再是已经不存在的「高级」页)——但漏接其他下标(比如忘了给
    /// 某个新增 Tab 接 `fields::*`)照样会静默落到这个兜底,把用户导向「连接」页
    /// 而不是它本该去的页面。这条要端到端跑 `show()`,参数面太宽,留在人工验收
    /// 清单里。
    #[test]
    fn tab_constants_match_the_table_and_auth_keeps_index_one() {
        use crate::ui::session_manager::{TAB_APPEARANCE, TAB_AUTOMATION};
        assert_eq!(super::TABS.len(), 4, "「高级」已并入「连接」,不该还有 5 页");
        assert_eq!(super::TABS[TAB_CONNECT], "连接");
        assert_eq!(super::TABS[TAB_AUTH], "认证");
        assert_eq!(super::TABS[TAB_AUTOMATION], "登录后");
        assert_eq!(super::TABS[TAB_APPEARANCE], "图标");
        assert_eq!(TAB_AUTH, 1, "私钥拖入门控与必填校验都钉在这个下标上");
    }

    /// 都没缺、也没在拨测 → 按钮可点。
    #[test]
    fn why_is_no_when_nothing_missing_and_probe_idle() {
        let d = why(Missing::default(), &ProbeState::Idle);
        assert!(matches!(d, Disabled::No), "无缺项且未拨测时应可点");
        assert_eq!(tip(&d), None, "可点态不该有 tooltip");
    }

    /// 复核指出的 tie-break:`missing.any()` 与 `ProbeState::Running` 同时
    /// 成立时,必须优先报 `Missing`,不能报 `Probing`——这是计划里刻意的
    /// 设计(「表单都没填齐,就没必要提『测试连接进行中』」),搞反了会让
    /// 用户以为按钮是因为在拨测才点不动,翻遍界面也找不到到底缺了什么。
    ///
    /// 自证会变红的方式:把 `why()` 里 `if missing.any() { .. } else if
    /// matches!(..) { .. }` 的判断顺序换一下(先判 `Probing` 再判
    /// `Missing`),这条会报 `Disabled::Probing` 而不是 `Disabled::Missing`。
    #[test]
    fn missing_takes_priority_over_probing_when_both_conditions_hold() {
        let missing = Missing {
            name: false,
            host: true,
            user: false,
        };
        let d = why(missing, &ProbeState::Running);
        match &d {
            Disabled::Missing(hint) => {
                assert_eq!(hint, "还缺:主机", "禁用原因应该是缺项提示,而不是拨测中")
            }
            _ => panic!("missing.any() 与 ProbeState::Running 同时成立时应优先报 Missing"),
        }
    }

    /// 缺项已填齐、但拨测仍在跑 → 报 `Probing`。
    #[test]
    fn why_is_probing_when_nothing_missing_but_probe_is_running() {
        let d = why(Missing::default(), &ProbeState::Running);
        assert!(
            matches!(d, Disabled::Probing),
            "缺项已填齐、拨测在途时应报 Probing"
        );
        assert_eq!(
            tip(&d),
            Some("测试连接进行中…".to_owned()),
            "Probing 态的 tooltip 文案应固定"
        );
    }

    /// `Disabled::Missing` 的 tooltip 应该原样透出 `Missing::hint()` 的文本,
    /// 不是重新拼一遍。
    #[test]
    fn tip_for_missing_forwards_the_hint_text() {
        let missing = Missing {
            name: true,
            host: false,
            user: true,
        };
        let d = why(missing, &ProbeState::Idle);
        assert_eq!(tip(&d), Some("还缺:会话名称、用户名".to_owned()));
    }

    /// `ProbeState::Err`/`Ok` 都不该影响 `why()`——按钮禁用只看
    /// `ProbeState::Running`,拨测已经有结果(成功或失败)时不该继续挡着
    /// 保存/连接。
    #[test]
    fn why_is_no_when_probe_already_finished_ok_or_err() {
        assert!(matches!(
            why(Missing::default(), &ProbeState::Ok),
            Disabled::No
        ));
        assert!(matches!(
            why(Missing::default(), &ProbeState::Err("boom".into())),
            Disabled::No
        ));
    }

    /// 缺陷 #13 的回归测试:新建/编辑会话时,表单相对「上次保存基线」
    /// (`editor_baseline`)天然是脏的——这不该影响拨测结论。发起拨测那一刻
    /// 存的快照(`probe_form`)才是唯一有效的比较基线,且此刻两者相等
    /// (没有再改任何字段)。
    ///
    /// 自证变红方式:把 `expire_verdict_if_form_changed` 函数体里
    /// `ui_state.probe_form.as_ref()` 换成 `ui_state.editor_baseline.as_ref()`
    /// ——这正是协调者描述的历史 bug(基线取错字段)。改完这条测试会失败:
    /// `assertion failed: matches!(st.probe, ProbeState::Ok)`(因为
    /// `editor_baseline` 是空表单,当前表单已经填了字段,`is_dirty` 判真,
    /// 结论被清成 Idle)。验证后已改回。
    #[test]
    fn probe_verdict_survives_when_the_form_was_already_dirty_before_probing() {
        let filled = EditorBuffer {
            name: "生产服务器".to_owned(),
            host: "10.0.0.1".to_owned(),
            ..EditorBuffer::default()
        };
        let mut st = UiState {
            editor: Some(filled.clone()),
            // 上次保存基线仍是空表单 —— 相对它,当前表单必然是脏的。
            editor_baseline: Some(EditorBuffer::default()),
            // 发起拨测那一刻存的快照 == 当前表单(还没再改任何字段)。
            probe_form: Some(filled),
            probe: ProbeState::Ok,
            ..Default::default()
        };
        expire_verdict_if_form_changed(&mut st);
        assert!(
            matches!(st.probe, ProbeState::Ok),
            "拨测结论不该被『相对保存基线是脏的』这个天然状态清掉,实际是 {:?}",
            st.probe
        );
    }

    /// 拨测结束后,用户又改了表单(相对发起拨测时的快照 `probe_form`)——
    /// 旧结论(`Ok`)不再可信,必须清成 `Idle`。
    #[test]
    fn editing_the_form_after_a_probe_clears_the_verdict() {
        let at_probe = EditorBuffer {
            name: "生产服务器".to_owned(),
            host: "10.0.0.1".to_owned(),
            ..EditorBuffer::default()
        };
        let mut edited = at_probe.clone();
        edited.host = "10.0.0.2".to_owned();
        let mut st = UiState {
            editor: Some(edited),
            probe_form: Some(at_probe),
            probe: ProbeState::Ok,
            ..Default::default()
        };
        expire_verdict_if_form_changed(&mut st);
        assert_eq!(
            st.probe,
            ProbeState::Idle,
            "改了字段后,已出的结论应该被作废"
        );
    }

    /// 复核 Important 2:错误卡片正显示(`error_shown` 为真的等价状态,
    /// 即 `last_error` 在场且 `error_dismissed == false`)期间,用户改了
    /// 表单,`probe` 的旧结论照样被作废——**这是有意为之的行为,不是 bug**。
    ///
    /// 理由见 `expire_verdict_if_form_changed` 的函数文档:作废的判据是
    /// 「表单变了,结论对不上了」,这是客观事实,与此刻屏幕上画的是哪张
    /// 卡片无关。如果反过来让作废也只在 `!error_shown` 时才生效,会让一个
    /// 已经对不上当前表单的陈旧「连接成功」在用户关掉错误卡片后重新冒出来
    /// ——那是给用户一个假保证,比「结论在用户看不见时被静默作废」更糟。
    ///
    /// 自证会变红:在 `expire_verdict_if_form_changed` 函数体最前面加一行
    /// `if ui_state.last_error.is_some() && !ui_state.error_dismissed { return; }`
    /// (即把渲染门控错误地搬进状态作废逻辑),这条会报
    /// `assertion `left == right` failed`(`probe` 仍是 `Ok`,而不是 `Idle`)。
    #[test]
    fn expiring_the_verdict_ignores_whether_the_error_card_is_currently_shown() {
        let at_probe = EditorBuffer {
            name: "生产服务器".to_owned(),
            host: "10.0.0.1".to_owned(),
            ..EditorBuffer::default()
        };
        let mut edited = at_probe.clone();
        edited.host = "10.0.0.2".to_owned();
        let mut st = UiState {
            editor: Some(edited),
            probe_form: Some(at_probe),
            probe: ProbeState::Ok,
            // error_shown 的等价条件:last_error 在场且未 dismiss。
            last_error: Some("刚才保存失败".to_owned()),
            error_dismissed: false,
            ..Default::default()
        };
        expire_verdict_if_form_changed(&mut st);
        assert_eq!(
            st.probe,
            ProbeState::Idle,
            "即使错误卡片正显示,表单一变,拨测旧结论也必须被作废(不是 bug,是有意行为)"
        );
    }

    /// 同上的编辑场景,但拨测仍在途(`Running`)——不该被打断,那次拨测
    /// 仍是针对发起时的表单,让它跑完;只有已出结论(`Ok`/`Err`)才作废。
    #[test]
    fn a_running_probe_is_not_cleared_by_edits() {
        let at_probe = EditorBuffer {
            name: "生产服务器".to_owned(),
            host: "10.0.0.1".to_owned(),
            ..EditorBuffer::default()
        };
        let mut edited = at_probe.clone();
        edited.host = "10.0.0.2".to_owned();
        let mut st = UiState {
            editor: Some(edited),
            probe_form: Some(at_probe),
            probe: ProbeState::Running,
            ..Default::default()
        };
        expire_verdict_if_form_changed(&mut st);
        assert_eq!(
            st.probe,
            ProbeState::Running,
            "在途拨测不该被表单编辑打断,要让它跑完"
        );
    }

    /// 安全专项终审:结论作废(`Ok`/`Err` → `Idle`)时,揣着明文凭据的
    /// `probe_form` 快照必须一并清空——它唯一的用途是给这次比对当基线,
    /// 结论已经不可信,快照就没有下一个消费者了,继续留着只会拉长明文
    /// 在内存里的驻留时间。
    ///
    /// 自证会变红方式:把 `expire_verdict_if_form_changed` 里作废分支中
    /// `ui_state.probe_form = None;` 这一行删掉。删掉后 `probe_form` 仍是
    /// `Some(..)`,断言 `st.probe_form.is_none()` 失败。
    #[test]
    fn expiring_the_verdict_also_clears_the_plaintext_probe_form_snapshot() {
        let at_probe = EditorBuffer {
            name: "生产服务器".to_owned(),
            host: "10.0.0.1".to_owned(),
            password: "hunter2".to_owned(),
            ..EditorBuffer::default()
        };
        let mut edited = at_probe.clone();
        edited.host = "10.0.0.2".to_owned();
        let mut st = UiState {
            editor: Some(edited),
            probe_form: Some(at_probe),
            probe: ProbeState::Ok,
            ..Default::default()
        };
        expire_verdict_if_form_changed(&mut st);
        assert_eq!(st.probe, ProbeState::Idle, "结论应被作废");
        assert!(
            st.probe_form.is_none(),
            "结论作废后,揣着明文凭据的表单快照也必须一并清空"
        );
    }

    /// 上一条的反面:拨测仍在 `Running` 时,即便表单已经改了(`changed`
    /// 为真),`probe_form` 也必须保留——那次在途拨测仍需要这份快照当
    /// 「发起时长什么样」的基线,提前清掉会让它在结果回来那一刻失去基线。
    /// 这条守的是「不要顺手清过头」,比上一条更容易被以后的人改坏
    /// (比如有人把 `probe_form = None` 挪到 `matches!` 判断之外)。
    ///
    /// 自证会变红方式:把 `expire_verdict_if_form_changed` 改成无条件清空
    /// `probe_form`(挪到 `if changed && matches!(..)` 判断之外,对 `changed`
    /// 为真的所有情况都清)。改完这条会失败:`st.probe_form` 变成 `None`,
    /// 与 `assert!(st.probe_form.is_some())` 不符。
    #[test]
    fn a_running_probe_keeps_its_plaintext_snapshot_even_if_the_form_changed() {
        let at_probe = EditorBuffer {
            name: "生产服务器".to_owned(),
            host: "10.0.0.1".to_owned(),
            password: "hunter2".to_owned(),
            ..EditorBuffer::default()
        };
        let mut edited = at_probe.clone();
        edited.host = "10.0.0.2".to_owned();
        let mut st = UiState {
            editor: Some(edited),
            probe_form: Some(at_probe),
            probe: ProbeState::Running,
            ..Default::default()
        };
        expire_verdict_if_form_changed(&mut st);
        assert_eq!(st.probe, ProbeState::Running, "在途拨测不该被打断");
        assert!(
            st.probe_form.is_some(),
            "在途拨测仍需要 probe_form 当基线,不该被提前清空"
        );
    }

    /// F93:候选一旦扫过(`key_candidates_ready == true`),再次调用
    /// `ensure_key_candidates_scanned` 绝不能重新触发 `scan_dir`——这正是
    /// 项目陷阱 T3 的同类:`show()` 每帧都会走到这个函数,如果没有
    /// `ready` 门控,`read_dir` 就会变成每秒几十次的同步文件系统 IO,
    /// 表现为界面卡顿(T3 原文说的是「每秒几千次重绘」,这里是「每帧一次
    /// 磁盘扫描」,后果同源:把 IO 放进了渲染热路径)。
    ///
    /// 自证变红方式:把 `ensure_key_candidates_scanned` 里
    /// `if !ui_state.key_candidates_ready { .. }` 的门控删掉(直接无条件
    /// 执行函数体)。删掉后 `calls` 在两次调用后变成 2,第一条断言
    /// (`calls.get() == 1`)失败;且第二次调用会用新闭包的返回值覆盖
    /// `key_candidates`,第二条断言(候选内容保持第一次扫描结果)也会失败。
    #[test]
    fn ensure_key_candidates_scanned_only_scans_once_per_ready_flag() {
        let calls = std::cell::Cell::new(0usize);
        let mut st = UiState::default();

        ensure_key_candidates_scanned(&mut st, || {
            calls.set(calls.get() + 1);
            vec![std::path::PathBuf::from("/tmp/id_ed25519")]
        });
        ensure_key_candidates_scanned(&mut st, || {
            calls.set(calls.get() + 1);
            vec![std::path::PathBuf::from("/tmp/should_not_appear")]
        });

        assert_eq!(
            calls.get(),
            1,
            "候选已扫过(ready==true)后,第二次调用不该再触发扫描"
        );
        assert_eq!(
            st.key_candidates,
            vec![std::path::PathBuf::from("/tmp/id_ed25519")],
            "第二次调用的(未执行的)扫描结果不该覆盖第一次缓存的候选"
        );
    }

    /// F93:`key_candidates_ready == false`(编辑器刚打开、或关闭后被复位)
    /// 时,`ensure_key_candidates_scanned` 必须真的扫一次,并把结果写进
    /// `ui_state.key_candidates`——否则「候选下拉」永远是空的,`fields.rs`
    /// 里那个禁用态的按钮会一直显示,即使 `~/.ssh` 里明明有候选。
    ///
    /// 自证变红方式:把函数体里 `ui_state.key_candidates = scan_dir();`
    /// 这一行删掉(只留 `ui_state.key_candidates_ready = true;`)。删掉后
    /// `key_candidates` 仍是默认的空 `Vec`,断言失败。
    #[test]
    fn ensure_key_candidates_scanned_populates_candidates_when_not_ready() {
        let mut st = UiState::default();
        assert!(
            !st.key_candidates_ready,
            "UiState::default() 的初始态就该是『还没扫过』"
        );

        ensure_key_candidates_scanned(&mut st, || {
            vec![std::path::PathBuf::from("/home/u/.ssh/id_rsa")]
        });

        assert!(
            st.key_candidates_ready,
            "扫描后必须置 ready,否则下一帧(还是同一次打开)会再扫一次"
        );
        assert_eq!(
            st.key_candidates,
            vec![std::path::PathBuf::from("/home/u/.ssh/id_rsa")],
            "扫描结果必须写进 key_candidates,UI 的候选下拉才读得到"
        );
    }

    /// F93:空列表(没拖东西,或拖进来的东西全都没有 `path`,过滤后为空)
    /// → `Nothing`,不该有任何副作用。
    ///
    /// 自证变红方式:把 `decide_key_drop` 开头的
    /// `let Some(first) = dropped.first() else { return KeyDrop::Nothing };`
    /// 换成直接 `let first = &dropped[0];`——空切片会直接 panic
    /// (`index out of bounds`),而不是返回 `Nothing`。
    #[test]
    fn decide_key_drop_is_nothing_for_empty_list() {
        let dropped: Vec<std::path::PathBuf> = Vec::new();
        let d = decide_key_drop(&dropped, |_| false);
        assert_eq!(d, KeyDrop::Nothing, "空列表不该有任何决策");
    }

    /// 单个文件 → 直接接受,没有多余提示。
    ///
    /// 自证变红方式:把 `let note = if dropped.len() > 1 { .. } else { None
    /// };` 里的 `> 1` 改成 `>= 1`——单文件也会被判定为「有其余文件」,
    /// `note` 变成 `Some("已取第一个文件,忽略其余 0 个")`,与
    /// `assert_eq!(note, None)` 不符。
    #[test]
    fn decide_key_drop_accepts_single_file_without_note() {
        let dropped = vec![std::path::PathBuf::from("/home/u/.ssh/id_ed25519")];
        let d = decide_key_drop(&dropped, |_| false);
        assert_eq!(
            d,
            KeyDrop::Accepted {
                path: "/home/u/.ssh/id_ed25519".to_owned(),
                note: None,
            },
            "单文件应直接接受,不带提示"
        );
    }

    /// 三个文件 → 取第一个,提示文案逐字核对「已取第一个文件,忽略其余 2 个」。
    ///
    /// 自证变红方式:把 `dropped.len() - 1` 改成 `dropped.len()`——三文件时
    /// 提示会变成「忽略其余 3 个」而不是「忽略其余 2 个」,与断言的逐字
    /// 文案不符。
    #[test]
    fn decide_key_drop_accepts_first_of_three_with_exact_note_text() {
        let dropped = vec![
            std::path::PathBuf::from("/home/u/.ssh/id_ed25519"),
            std::path::PathBuf::from("/home/u/.ssh/id_rsa"),
            std::path::PathBuf::from("/home/u/.ssh/id_ecdsa"),
        ];
        let d = decide_key_drop(&dropped, |_| false);
        assert_eq!(
            d,
            KeyDrop::Accepted {
                path: "/home/u/.ssh/id_ed25519".to_owned(),
                note: Some("已取第一个文件,忽略其余 2 个".to_owned()),
            },
            "应取第一个文件,提示文案须逐字匹配"
        );
    }

    /// 第一个是目录 → 拒绝,且**不写路径**(调用方不该在 `Rejected` 分支
    /// 里碰 `buf.key_data`)。
    ///
    /// 自证变红方式:把 `if is_dir(first) { return KeyDrop::Rejected(..) }`
    /// 这一整段判断删掉——目录会被当成普通文件接受,断言
    /// `matches!(d, KeyDrop::Rejected(_))` 失败,实际得到 `Accepted`。
    #[test]
    fn decide_key_drop_rejects_when_first_is_a_directory() {
        let dropped = vec![std::path::PathBuf::from("/home/u/.ssh")];
        let d = decide_key_drop(&dropped, |_| true);
        match d {
            KeyDrop::Rejected(note) => {
                assert_eq!(note, "请拖入私钥文件,不是目录", "拒绝原因文案须匹配")
            }
            other => panic!("第一个是目录时应 Rejected,实际是 {other:?}"),
        }
    }

    /// 第一个是文件、第二个是目录 → 仍 `Accepted` 第一个。这条锁定「只看
    /// 第一个」这条语义,防止以后有人"顺手"改成"跳过目录找第一个文件"。
    ///
    /// 自证变红方式:把判目录的调用从 `is_dir(first)` 改成
    /// `dropped.iter().any(|p| is_dir(p))`(扫描整个列表而不是只看第一个)
    /// ——这条会因为第二个是目录而报 `Rejected`,与预期的 `Accepted` 不符。
    #[test]
    fn decide_key_drop_only_inspects_the_first_entry_even_if_a_later_one_is_a_dir() {
        let dropped = vec![
            std::path::PathBuf::from("/home/u/.ssh/id_ed25519"),
            std::path::PathBuf::from("/home/u/.ssh/some_dir"),
        ];
        let d = decide_key_drop(&dropped, |p| p.ends_with("some_dir"));
        assert_eq!(
            d,
            KeyDrop::Accepted {
                path: "/home/u/.ssh/id_ed25519".to_owned(),
                note: Some("已取第一个文件,忽略其余 1 个".to_owned()),
            },
            "只应检查第一个元素是不是目录,后面的元素是不是目录不该影响结果"
        );
    }

    /// 复核 Important 2:门控 `if ui_state.editor_tab == TAB_AUTH && buf.auth_kind
    /// == AuthKindUi::PublicKey`(`show()` 里,`decide_key_drop` 调用之前)
    /// 本身零测试覆盖——前面所有 `decide_key_drop_*` 测试都直接调纯函数,
    /// 根本不经过这个 `if`,删掉整个门控块也不会让它们变红。这里灌一份真实
    /// `RawInput`(带 `dropped_files`)跑一整帧 `show()`,把断言钉在
    /// `buf.key_data` 上,注入点就是这个门控 `if` 本身。
    ///
    /// `ui::mod.rs` 里的 `run_frame`/`UiFrame` 是那个文件 `tests` 模块的私有
    /// 项,跨模块用不了(`editor::tests` 是 `editor` 的子模块,够不着
    /// `ui::tests`)。这里用同样的手法自己搭一个最小版本:`ctx.run` 套一层
    /// `CentralPanel`,直接调 `super::show`(`pub(super)`,子模块可见)。
    fn run_show_with_drop(ui_state: &mut UiState, dropped_path: &std::path::Path) {
        let ctx = egui::Context::default();
        crate::theme::apply_egui(&ctx, &crate::theme::MULLION_DARK);
        let input = egui::RawInput {
            dropped_files: vec![egui::DroppedFile {
                path: Some(dropped_path.to_path_buf()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                super::show(
                    ui,
                    &crate::theme::MULLION_DARK,
                    ui_state,
                    &[],
                    &[],
                    super::SecretPresence::default(),
                );
            });
        });
    }

    /// 拖放测试共用的假私钥。必须带 `PRIVATE KEY` 标记 —— `import_key_file`
    /// 会把不带标记的文件当成误选的 `.pub` 公钥拒收。
    const PEM: &str =
        "-----BEGIN OPENSSH PRIVATE KEY-----\nBODY\n-----END OPENSSH PRIVATE KEY-----\n";

    /// 认证 Tab + 公钥模式 → 拖进的文件**正文**必须被读进 `buf.key_data`。这条
    /// 是「双向断言」里正的一半——只有它在,才能防止有人把整个门控块删光
    /// 后仍然全绿(下面两条「原样不变」的断言,门控块删光后照样通过)。
    ///
    /// 自证会变红方式:把 `show()` 里整个门控 `if ui_state.editor_tab ==
    /// super::TAB_AUTH && buf.auth_kind == super::AuthKindUi::PublicKey { .. }`
    /// 删掉(或者把条件强改成 `false`),`decide_key_drop`/`import_key_file`
    /// 都不会再执行,`buf.key_data` 仍是初始的空字符串,断言报
    /// `left: "" right: ".../id_ed25519"`(方向:实际空、期望非空)。
    #[test]
    fn dropping_a_key_file_in_auth_tab_public_key_mode_imports_the_key_body() {
        let dir = tempfile::tempdir().expect("建临时目录");
        let key_path = dir.path().join("id_ed25519");
        std::fs::write(&key_path, PEM).expect("写测试私钥文件");

        let mut st = UiState {
            editor_tab: TAB_AUTH,
            editor: Some(EditorBuffer {
                auth_kind: AuthKindUi::PublicKey,
                ..EditorBuffer::default()
            }),
            ..Default::default()
        };

        run_show_with_drop(&mut st, &key_path);

        assert_eq!(
            st.editor.expect("编辑器不该被清空").key_data,
            PEM,
            "认证 Tab + 公钥模式下拖入文件应该把**正文**读进 key_data"
        );
    }

    /// 认证 Tab、但密码模式 → 门控必须整块不执行,`key_data` 原样不变。
    /// 这是门控 `&& buf.auth_kind == AuthKindUi::PublicKey` 那半条判据的
    /// 真实注入点:如果这半条被删掉或改错,密码模式下拖文件也会被静默
    /// 接受,写进一个用户在密码模式下根本看不到的字段。
    ///
    /// 自证会变红方式:把门控 `if` 里 `&& buf.auth_kind ==
    /// super::AuthKindUi::PublicKey` 这半句删掉,只留 `ui_state.editor_tab
    /// == super::TAB_AUTH`——密码模式下拖入的文件会被接受,`key_data` 从
    /// 空字符串变成私钥正文,`assert_eq!(.., "")` 失败,报
    /// `left: "" right: ".../id_ed25519"` 的方向反过来(实际非空)。
    #[test]
    fn dropping_a_key_file_in_password_mode_leaves_key_data_untouched() {
        let dir = tempfile::tempdir().expect("建临时目录");
        let key_path = dir.path().join("id_ed25519");
        std::fs::write(&key_path, PEM).expect("写测试私钥文件");

        let mut st = UiState {
            editor_tab: TAB_AUTH,
            editor: Some(EditorBuffer {
                auth_kind: AuthKindUi::Password,
                ..EditorBuffer::default()
            }),
            ..Default::default()
        };

        run_show_with_drop(&mut st, &key_path);

        assert_eq!(
            st.editor.expect("编辑器不该被清空").key_data,
            "",
            "密码模式下拖文件必须被静默忽略,key_data 不该被写"
        );
    }

    /// 公钥模式、但不在认证 Tab(比如「连接」Tab)→ 门控同样必须整块不
    /// 执行,`key_data` 原样不变。这是门控 `ui_state.editor_tab ==
    /// super::TAB_AUTH` 那半条判据的真实注入点。
    ///
    /// 自证会变红方式:把门控 `if` 里 `ui_state.editor_tab ==
    /// super::TAB_AUTH &&` 这半句删掉,只留 `buf.auth_kind ==
    /// super::AuthKindUi::PublicKey`——「连接」Tab 下拖入的文件也会被接受,
    /// `key_data` 从空字符串变成私钥正文,断言失败。
    #[test]
    fn dropping_a_key_file_outside_auth_tab_leaves_key_data_untouched() {
        let dir = tempfile::tempdir().expect("建临时目录");
        let key_path = dir.path().join("id_ed25519");
        std::fs::write(&key_path, PEM).expect("写测试私钥文件");

        let mut st = UiState {
            editor_tab: TAB_CONNECT,
            editor: Some(EditorBuffer {
                auth_kind: AuthKindUi::PublicKey,
                ..EditorBuffer::default()
            }),
            ..Default::default()
        };

        run_show_with_drop(&mut st, &key_path);

        assert_eq!(
            st.editor.expect("编辑器不该被清空").key_data,
            "",
            "不在认证 Tab 时拖文件必须被静默忽略,key_data 不该被写"
        );
    }

    /// 拖进来的是 `.pub` 公钥 —— 必须当场拒收并解释,而不是存进去等到连接
    /// 时才报一句「解析私钥失败」。这条同时钉住「有提示」:静默丢弃的话,
    /// 用户会以为导入成功了。
    #[test]
    fn dropping_a_public_key_is_rejected_with_an_explanation() {
        let dir = tempfile::tempdir().expect("建临时目录");
        let pub_path = dir.path().join("id_ed25519.pub");
        std::fs::write(&pub_path, b"ssh-ed25519 AAAAC3Nza... u@h\n").expect("写测试公钥文件");

        let mut st = UiState {
            editor_tab: TAB_AUTH,
            editor: Some(EditorBuffer {
                auth_kind: AuthKindUi::PublicKey,
                ..EditorBuffer::default()
            }),
            ..Default::default()
        };

        run_show_with_drop(&mut st, &pub_path);

        assert_eq!(
            st.editor.expect("编辑器不该被清空").key_data,
            "",
            "公钥不该被当成私钥收下"
        );
        let note = st.key_drop_note.expect("被拒时必须给用户一行解释");
        assert!(
            note.contains("id_ed25519.pub"),
            "提示要点名是哪个文件: {note}"
        );
    }
}
