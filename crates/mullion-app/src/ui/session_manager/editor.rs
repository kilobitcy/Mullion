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
const TABS: [&str; 4] = ["连接", "认证", "高级", "登录后"];

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
    let Some(buf) = ui_state.editor.as_mut() else {
        ui.centered_and_justified(|ui| {
            ui.colored_label(theme::c32(t.fg_dimmer), "从左侧选一条会话,或点「+ 新建」");
        });
        return;
    };

    let missing = super::validate::check(&buf.name, &buf.host, &buf.user);
    let reason = why(missing, &ui_state.probe);
    // 保存只被「必填未齐」挡;拨测在途时仍可保存(它只读表单,不改)。
    let disable_save = missing.any();
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
        egui::Frame::none()
            .fill(theme::c32(t.sunken_bg))
            .stroke(egui::Stroke::new(1.0, theme::c32(t.danger_soft)))
            .rounding(8.0)
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(theme::c32(t.danger_soft), msg);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("×").clicked() {
                            ui_state.error_dismissed = true;
                        }
                    });
                });
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
            // 却不知道该翻哪一页。
            let label = if missing.tab() == Some(i) {
                format!("{name} ●")
            } else {
                (*name).to_string()
            };
            if ui
                .selectable_label(ui_state.editor_tab == i, label)
                .clicked()
            {
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
    // 「取消」只置意图,不在这里改 `ui_state.editor` —— 见代码块后的借用说明。
    let mut cancel = false;
    // `why()` 里已经构造过同一份文本,别每个按钮再各建一次 String——
    // `disable_save == missing.any()`,而 `reason` 是 `Disabled::Missing`
    // 当且仅当 `missing.any()` 为真(`why()` 的分支顺序保证了这一点),
    // 所以下面 `Disabled::Missing(h) => Some(h.as_str())` 与原来
    // `if disable_save { Some(missing.hint()) } else { None }` 完全等价。
    let missing_tip = match &reason {
        Disabled::Missing(h) => Some(h.as_str()),
        _ => None,
    };
    egui::TopBottomPanel::bottom(ui.id().with("sm_editor_bottom"))
        .frame(egui::Frame::none())
        .show_separator_line(false)
        .show_inside(ui, |ui| {
            ui.separator();
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
                    !disable_save,
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
                        missing_tip,
                    );

                    cancel |= ui.button("取消").clicked();

                    if save || save_connect {
                        ui_state.save_click = Some(save_connect);
                    }
                });
            });
        });

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| match ui_state.editor_tab {
            super::TAB_CONNECT => {
                super::fields::basic(ui, t, buf, groups, sessions, ui_state.editor_id)
            }
            super::TAB_AUTH => super::fields::auth(ui, t, buf, presence, &ui_state.key_candidates),
            super::TAB_AUTOMATION => super::fields::automation(ui, t, buf),
            super::TAB_ADVANCED => super::fields::network(ui, t, buf, presence),
            // 「越界值兜底」:`editor_tab` 是既有的裸 usize 技术债,越界值
            // 落到这里比 panic 好。
            _ => super::fields::network(ui, t, buf, presence),
        });

    // `buf` 的借用到此结束,现在才能动 `ui_state.editor`。
    if cancel {
        apply_cancel(ui_state);
    }

    // F92:表单一改,上一次的成功/失败结论就不再可信 —— 但**不动世代号**:
    // 在途那次仍是针对这份表单发起的,让它跑完;世代号只在切会话/关窗时才变
    //(`cancel_probe`,app.rs)。这里够不着世代号,也不该够得着。
    expire_verdict_if_form_changed(ui_state);
}

/// 点「取消」之后的清理:回到「未编辑任何会话」态。抽成纯函数(而不是
/// 内联在 `show()` 的 `if cancel` 块里),是为了让测试能直接扎在这段清理
/// 逻辑上——`show()` 整体要驱动一次 egui 按钮点击才能触发,而这里只是
/// 一段状态赋值,不需要经过 egui。
///
/// 终审发现:这里原先漏了 `probe_cancel = true`。`close_session_manager`
/// (`ui/mod.rs`)和 `apply_switch`(同目录 `mod.rs`)清理同一组字段时都会
/// 置这个标志,唯独「取消」分支没有——后果是点「取消」时,若拨测仍在
/// `Running`,`app.rs` 收不到取消意图,`probe_epoch` 不会自增、`probe_task`
/// 也不会被 `abort()`,那个 tokio 任务会一直跑到自己的超时(最长 20 秒)
/// 才结束,期间还占着一份对已经不存在的表单的引用。`probe_cancel` 只是
/// 「意图标志」而非世代号本身——世代号和 `JoinHandle` 都在 `App` 上,
/// `UiState` 够不着,只能靠 app.rs 消费这个标志后自增世代号并 abort。
fn apply_cancel(ui_state: &mut UiState) {
    ui_state.editor = None;
    ui_state.editor_baseline = None;
    ui_state.editor_id = None;
    // F92:编辑器都关了,一个属于已关闭表单的拨测结论没有任何意义,
    // 不该留到下次打开;`probe_form` 还揣着明文凭据,一并清空
    // (与 `apply_switch`/`close_session_manager` 对齐,同构改动)。
    ui_state.probe = super::ProbeState::Idle;
    ui_state.probe_form = None;
    // 与上面两处对齐补的一条:取消也要取消在途拨测,见本函数文档。
    ui_state.probe_cancel = true;
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
        apply_cancel, decide_key_drop, ensure_key_candidates_scanned,
        expire_verdict_if_form_changed, tip, why, Disabled, KeyDrop,
    };
    use crate::ui::session_manager::validate::Missing;
    use crate::ui::session_manager::{AuthKindUi, EditorBuffer, ProbeState, TAB_AUTH, TAB_CONNECT};
    use crate::ui::UiState;

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

    /// 终审修正:点「取消」必须一并置 `probe_cancel = true`,否则若拨测
    /// 仍在 `Running`,app.rs 收不到取消意图,那个 tokio 任务会一直跑到
    /// 自己的超时(最长 20 秒)才结束。这条断言扎在真实注入点
    /// `apply_cancel` 本身,而不是经由 `show()` 驱动一次按钮点击——后者
    /// 在无头 egui 测试里需要合成指针事件,构造成本高且离题。
    ///
    /// 自证会变红方式:把 `apply_cancel` 里 `ui_state.probe_cancel = true;`
    /// 这一行删掉。删掉后 `probe_cancel` 仍是 `UiState::default()` 的初始值
    /// `false`,断言 `assert!(st.probe_cancel)` 失败。
    #[test]
    fn apply_cancel_requests_cancelling_an_in_flight_probe() {
        let mut st = UiState {
            editor: Some(EditorBuffer::default()),
            editor_baseline: Some(EditorBuffer::default()),
            editor_id: None,
            probe: ProbeState::Running,
            probe_form: Some(EditorBuffer::default()),
            probe_cancel: false,
            ..Default::default()
        };
        apply_cancel(&mut st);
        assert!(st.editor.is_none(), "取消后应回到『未编辑任何会话』态");
        assert_eq!(st.probe, ProbeState::Idle, "取消后拨测结论应复位");
        assert!(st.probe_form.is_none(), "取消后明文快照应清空");
        assert!(
            st.probe_cancel,
            "取消编辑必须一并请求取消在途拨测,否则任务会跑满超时才结束"
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
