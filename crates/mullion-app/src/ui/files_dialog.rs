//! 远端写操作的三个对话框(F54/D17/D21):新建文件夹、删除确认、权限。
//! **重命名不在这里** —— F200 起改成列表里就地编辑(`ui::files_panel::rename_row`)。
//!
//! **一律要用户确认之后才动远端**。右键点一下就删文件这种事不该存在,
//! 而删除在 SFTP 上**没有回收站、不可逆**(设计 D17)。
//!
//! 危险措辞按 F119 表单规范:列出「将删除 N 个文件 / M 个目录」+ 完整远端
//! 路径,按钮写「删除」不写「确定」——「确定」在一个列着 40 条路径的框里
//! 说明不了用户到底确定了什么。
//!
//! **D17 唯一的明示例外(F202)**:`Shift+Delete` 直接删,不弹这个框,
//! 文件和目录一视同仁 —— 用户拿它清一批临时文件,一条条确认反而会让他
//! 养成"闭眼点确定"的习惯,那比没有框更危险。代价是手滑按到 Shift
//! 就真的没了,所以那条路必须报一句带计数的吐司([`deleting_toast`]),
//! 而**裸 `Delete` 仍旧弹框**(守护:`app::tests::
//! shift_delete_skips_the_confirmation_but_a_bare_delete_still_asks`)。

use mullion_ssh::sftp::RemotePath;

use crate::theme::{self, Theme};

/// 当前开着哪个对话框。`None` = 没开。挂在 `UiState` 上(与既有几个模态
/// 同一套做法):egui 闭包借不到 `&mut App`,意图必须落在状态里。
#[derive(Debug, Clone, PartialEq)]
pub enum FilesDialog {
    NewDir {
        /// 在哪个目录里建。
        parent: RemotePath,
        /// 输入框内容。
        name: String,
    },
    Delete {
        /// 要删的完整路径 + 它是不是目录(确认文案要分开数)。
        targets: Vec<(RemotePath, bool)>,
    },
    Chmod {
        path: RemotePath,
        /// 九宫格当前值(低 9 位)。
        mode: u32,
    },
    /// F53:回传前发现远端那份已经被别人改过(D3-8)。**必须问** ——
    /// 直接覆盖会悄悄吃掉别人的改动,而这是我们自己发起的写。
    EditConflict {
        /// 冲突的那个文件(给用户看的完整路径)。
        name: String,
        /// 是哪一条编辑。同 `Conflict::job`:按名字回填会串。
        key: u64,
    },
    /// F55:传输的目标已存在。**必须问**,绝不静默覆盖。
    Conflict {
        /// 冲突的那个名字(给用户看的,不参与拼路径)。
        name: String,
        /// 队列里那一条的 id。选择要能回填到**具体哪一条**上 ——
        /// 按名字回填的话,同名文件在两个目录里就会串。
        job: u64,
        /// 「对本批后续冲突都这么办」勾选框的状态。
        apply_all: bool,
    },
}

/// 用户在对话框里**确认**之后要执行的写操作。到这一步就没有回头路了 ——
/// `app.rs` 收到它就直接发请求。
#[derive(Debug, Clone, PartialEq)]
pub enum FileOp {
    /// 完整的目标路径(已经拼好,`app.rs` 不再拼一次 —— 拼两遍就有一遍会错)。
    NewDir(RemotePath),
    Rename {
        from: RemotePath,
        to: RemotePath,
    },
    /// 逐条删。目录走递归删除,文件走 remove。**顺序即用户看到的顺序**。
    Delete {
        targets: Vec<(RemotePath, bool)>,
    },
    Chmod {
        path: RemotePath,
        mode: u32,
    },
    /// F53:编辑冲突的处置。同 `Resolve`,界面只送回选择。
    ResolveEdit {
        key: u64,
        choice: EditResolve,
    },
    /// F55:冲突处置。**不带路径** —— 具体怎么落盘由 worker 按 `choice`
    /// 决定,界面只负责把用户的选择原样送回队列。
    Resolve {
        job: u64,
        choice: crate::files::queue::Conflict,
        apply_all: bool,
    },
}

/// F53:回传撞上「远端已被改动」时的三条出路。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditResolve {
    /// 不回传,认远端那一份。**同时把快照刷成远端当前值**(D3-9)——
    /// 不刷的话下一次保存还会撞上同一个冲突,框永远关不掉。
    KeepRemote,
    /// 拿我这份盖掉远端。会丢掉别人的改动,所以标危险色。
    Overwrite,
    /// 存成同目录下的一个副本,两份都留着,谁也不丢。
    SaveCopy,
}

/// 删除确认框的那句话。抽成纯函数是因为它是**这一片唯一一句会直接导致
/// 数据丢失的文案** —— 数错了(比如把目录也算进「文件」)用户就会低估后果。
pub fn delete_summary(targets: &[(RemotePath, bool)]) -> String {
    counted(targets, "将删除")
}

/// F202:Shift+Del 免确认时的那句吐司。用户没看见确认框,这是**唯一**
/// 能告诉他刚才那一下打中了什么的东西,所以计数照样要分开数。
///
/// 措辞是「正在删除」而不是「已删除」:字节这会儿才刚发出去,成败要等
/// `UserEvent::SftpOpDone` 回来(成了另有「已完成」,败了是错误卡片)。
/// 在发出的那一刻宣布「已删除」,链路一断就成了假消息。
pub fn deleting_toast(targets: &[(RemotePath, bool)]) -> String {
    counted(targets, "正在删除")
}

/// 上面两句共用的计数。**只写一遍**:数错了(比如把目录也算进「文件」)
/// 用户就会低估后果,而免确认那条路上没有第二次确认能兜住。
fn counted(targets: &[(RemotePath, bool)], verb: &str) -> String {
    let dirs = targets.iter().filter(|(_, is_dir)| *is_dir).count();
    let files = targets.len() - dirs;
    match (files, dirs) {
        (0, 0) => "没有选中任何条目".to_string(),
        (f, 0) => format!("{verb} {f} 个文件"),
        (0, d) => format!("{verb} {d} 个目录(连同其中全部内容)"),
        (f, d) => format!("{verb} {f} 个文件、{d} 个目录(连同其中全部内容)"),
    }
}

/// 校验一个用户输入的**名字段**。新建文件夹的对话框与 F200 的就地改名
/// 共用这一份判据 —— 两处各写一遍的话,迟早有一处忘了挡 `/`。
///
/// 返回 `Err` 时:对话框那边把确认按钮置灰并显示这条原因;就地改名那边
/// 画红框 + 悬停显示,且**不退出编辑态**。
///
/// 这里挡的是**会打到别的路径上**的输入,不是「不好看的名字」:
/// - 空 / 全空白:拼出来是父目录本身。
/// - 含 `/`:`RemotePath::join` 不做归一化(见其文档),`a/../../etc` 会
///   真的打到 `/etc` 上去。这是本函数存在的**全部理由**。
/// - `.` 与 `..`:同上,而且服务端多半直接回 Failure。
pub fn validate_name(name: &str) -> Result<(), &'static str> {
    let t = name.trim();
    if t.is_empty() {
        return Err("名字不能为空");
    }
    if name.contains('/') {
        return Err("名字里不能有 /(那会跳到另一个目录去)");
    }
    if t == "." || t == ".." {
        return Err("不能用 . 或 .. 当名字");
    }
    Ok(())
}

/// 九宫格 → 八进制。位序与 `files::perm_string` 画出来的一致:
/// 属主 rwx、属组 rwx、其他 rwx,从高位到低位。
pub fn mode_from_bits(bits: [bool; 9]) -> u32 {
    let mut m = 0;
    for (i, on) in bits.iter().enumerate() {
        if *on {
            m |= 1 << (8 - i);
        }
    }
    m
}

/// 八进制 → 九宫格。`mode_from_bits` 的逆。
pub fn bits_from_mode(mode: u32) -> [bool; 9] {
    let mut out = [false; 9];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = mode & (1 << (8 - i)) != 0;
    }
    out
}

/// F203:「取消这个框」要发出的处置。`None` = 只关框就行。
///
/// 抽成纯函数是因为它有**两个入口**:框里那颗「取消」按钮,和 F203 加在
/// 标题栏上的 ✕。两处各写一遍的话,迟早有一处漏掉后两个变体那条
/// 「不能只关框」的规矩 —— 而漏掉的症状是传输队列静静地永远走不动。
///
/// 前四个框没有在途状态,关掉就完了;后两个框各自代表一条**挂起的工作**
/// (一条 job / 一条编辑),不给出处置它们就永远挂着。
pub fn cancel_op(d: &FilesDialog) -> Option<FileOp> {
    match d {
        FilesDialog::NewDir { .. } | FilesDialog::Delete { .. } | FilesDialog::Chmod { .. } => None,
        // 取消 = 保留远端。**不能只关框**:那条编辑会一直挂在 `Conflict` 上,
        // 每次轮询都想回传、每次都撞冲突,而快照永远不动 —— 界面上只看得出
        // 「这个文件一直红着」。
        FilesDialog::EditConflict { key, .. } => Some(FileOp::ResolveEdit {
            key: *key,
            choice: EditResolve::KeepRemote,
        }),
        // 同上,按跳过算。`apply_all` 恒 `false` —— 取消是「这一条别传了」,
        // 不是「以后都别问了」,哪怕框里勾着那个复选框。
        FilesDialog::Conflict { job, .. } => Some(FileOp::Resolve {
            job: *job,
            choice: crate::files::queue::Conflict::Skip,
            apply_all: false,
        }),
    }
}

/// 五个框共用的外壳。模态框的属性(不可折叠、不可缩放、居中、标题栏上有
/// ✕)只写一处 —— 五份复制粘贴里总有一份会漏掉 `.collapsible(false)`。
///
/// 返回 **`true` = 用户点了标题栏上那颗 ✕**(F203)。不在这里直接收口,是
/// 因为「取消该干什么」按框而异(见 [`cancel_op`]),而 `body` 闭包这会儿
/// 还借着调用方的 `op`;返回之后借用结束,调用方再落处置。
///
/// ✕ 走 `egui::Window::open()` 而不是自己在正文里画一颗:egui 把它画在
/// **标题栏**上(正文里画只能低一行),而且那是两条直线不是字形,
/// 没有 T9 的豆腐块风险。
fn modal<R>(ctx: &egui::Context, title: &str, body: impl FnOnce(&mut egui::Ui) -> R) -> bool {
    let mut open = true;
    egui::Window::new(title)
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            crate::ui::annotate::mark(ui.ctx(), format!("{title}对话框"), ui.max_rect());
            body(ui)
        });
    !open
}

/// 名字输入框 + 校验提示。返回「现在能不能确认」。
/// (F200 之后只剩「新建文件夹」一个用户 —— 重命名改成列表里就地编辑了。)
fn name_field(ui: &mut egui::Ui, t: &Theme, name: &mut String) -> bool {
    ui.add(egui::TextEdit::singleline(name).desired_width(260.0));
    match validate_name(name) {
        Ok(()) => true,
        Err(why) => {
            ui.colored_label(theme::c32(t.danger_text), why);
            false
        }
    }
}

/// 画当前开着的那个对话框。返回用户确认的写操作(没确认就是 `None`)。
///
/// `dialog` 传 `&mut Option<..>`:用户点「取消」或确认之后要把它清成
/// `None`,这一步必须在这里做 —— 交给调用方做的话,总有一条分支会漏。
pub fn show(ctx: &egui::Context, t: &Theme, dialog: &mut Option<FilesDialog>) -> Option<FileOp> {
    let d = dialog.as_mut()?;
    let mut op = None;
    let mut close = false;
    // F203:✕ 与各框那颗「取消」按钮共用同一条处置。**必须在进 `match`
    // 之前算好** —— 各臂已经把 `d` 的字段借出去了,臂内再借整个 `d` 借不出来。
    let on_cancel = cancel_op(d);
    // 各臂共用的收口:点了 ✕ / 点了「取消」都走这里。
    macro_rules! cancelled {
        () => {{
            op = on_cancel.clone();
            close = true;
        }};
    }

    match d {
        FilesDialog::NewDir { parent, name } => {
            let parent_disp = parent.display().to_string();
            let x = modal(ctx, "新建文件夹", |ui| {
                ui.label(
                    egui::RichText::new(format!("位置:{parent_disp}"))
                        .color(theme::c32(t.fg_muted)),
                );
                let ok = name_field(ui, t, name);
                ui.horizontal(|ui| {
                    if ui.add_enabled(ok, egui::Button::new("新建")).clicked() {
                        op = Some(FileOp::NewDir(parent.join(name.trim().as_bytes())));
                        close = true;
                    }
                    if ui.button("取消").clicked() {
                        cancelled!();
                    }
                });
            });
            if x {
                cancelled!();
            }
        }
        FilesDialog::Delete { targets } => {
            let x = modal(ctx, "删除", |ui| {
                ui.colored_label(theme::c32(t.danger_text), delete_summary(targets));
                // 「没有回收站」必须写在框里。用户在本地删东西是有后悔药的,
                // 那个心智模型会被原样带到这儿来(设计 D17)。
                ui.label(
                    egui::RichText::new("远端删除不可逆,没有回收站。")
                        .color(theme::c32(t.fg_muted)),
                );
                egui::ScrollArea::vertical()
                    .max_height(180.0)
                    .show(ui, |ui| {
                        for (p, is_dir) in targets.iter() {
                            let mark = if *is_dir { "[目录] " } else { "" };
                            ui.label(format!("{mark}{}", p.display()));
                        }
                    });
                ui.horizontal(|ui| {
                    // 按钮写「删除」不写「确定」(F119 危险措辞)——「确定」
                    // 在一个列着 40 条路径的框里说明不了用户到底确定了什么。
                    if ui
                        .button(egui::RichText::new("删除").color(theme::c32(t.danger_text)))
                        .clicked()
                    {
                        op = Some(FileOp::Delete {
                            targets: targets.clone(),
                        });
                        close = true;
                    }
                    if ui.button("取消").clicked() {
                        cancelled!();
                    }
                });
            });
            if x {
                cancelled!();
            }
        }
        FilesDialog::Chmod { path, mode } => {
            let path_disp = path.display().to_string();
            let x = modal(ctx, "属性", |ui| {
                ui.label(egui::RichText::new(&path_disp).color(theme::c32(t.fg_muted)));
                let mut bits = bits_from_mode(*mode);
                egui::Grid::new("chmod-grid").show(ui, |ui| {
                    ui.label("");
                    ui.label("读");
                    ui.label("写");
                    ui.label("执行");
                    ui.end_row();
                    for (row, who) in ["属主", "属组", "其他"].iter().enumerate() {
                        ui.label(*who);
                        for col in 0..3 {
                            ui.checkbox(&mut bits[row * 3 + col], "");
                        }
                        ui.end_row();
                    }
                });
                *mode = mode_from_bits(bits);
                ui.label(format!("八进制:{:04o}", *mode));
                ui.horizontal(|ui| {
                    if ui.button("应用").clicked() {
                        op = Some(FileOp::Chmod {
                            path: path.clone(),
                            mode: *mode,
                        });
                        close = true;
                    }
                    if ui.button("取消").clicked() {
                        cancelled!();
                    }
                });
            });
            if x {
                cancelled!();
            }
        }
        FilesDialog::EditConflict { name, key } => {
            let key = *key;
            let name_disp = name.clone();
            let x = modal(ctx, "远端文件已被改动", |ui| {
                ui.label(format!("你在编辑「{name_disp}」期间,远端那一份变了。"));
                ui.label(
                    egui::RichText::new("直接覆盖会丢掉对方的改动,不可撤销。")
                        .color(theme::c32(t.fg_muted)),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("保留远端(丢弃我的改动)").clicked() {
                        op = Some(FileOp::ResolveEdit {
                            key,
                            choice: EditResolve::KeepRemote,
                        });
                        close = true;
                    }
                    if ui.button("另存为副本").clicked() {
                        op = Some(FileOp::ResolveEdit {
                            key,
                            choice: EditResolve::SaveCopy,
                        });
                        close = true;
                    }
                    // 「覆盖远端」标危险色(F119):三条里唯一会毁数据的。
                    if ui
                        .button(egui::RichText::new("覆盖远端").color(theme::c32(t.danger_text)))
                        .clicked()
                    {
                        op = Some(FileOp::ResolveEdit {
                            key,
                            choice: EditResolve::Overwrite,
                        });
                        close = true;
                    }
                    // 「取消」= 保留远端,**不能只关框**:那条编辑会一直挂在
                    // `Conflict` 上,每次轮询都想回传、每次都撞冲突,而快照
                    // 永远不动 —— 界面上只看得出「这个文件一直红着」。
                    // 处置本体在 `cancel_op`,与标题栏那颗 ✕ 共用一份。
                    if ui.button("取消").clicked() {
                        cancelled!();
                    }
                });
            });
            if x {
                cancelled!();
            }
        }
        FilesDialog::Conflict {
            name,
            job,
            apply_all,
        } => {
            let job = *job;
            let name_disp = name.clone();
            let x = modal(ctx, "文件已存在", |ui| {
                ui.label(format!("目标位置已经有「{name_disp}」了。"));
                ui.label(
                    egui::RichText::new("覆盖会丢掉目标端现有的内容,不可撤销。")
                        .color(theme::c32(t.fg_muted)),
                );
                ui.add_space(6.0);
                ui.checkbox(apply_all, "对本批后续冲突都这么办");
                ui.add_space(6.0);
                let all = *apply_all;
                ui.horizontal(|ui| {
                    // 「覆盖」标危险色(F119):它是这四个里唯一会毁数据的。
                    if ui
                        .button(egui::RichText::new("覆盖").color(theme::c32(t.danger_text)))
                        .clicked()
                    {
                        op = Some(FileOp::Resolve {
                            job,
                            choice: crate::files::queue::Conflict::Overwrite,
                            apply_all: all,
                        });
                        close = true;
                    }
                    if ui.button("跳过").clicked() {
                        op = Some(FileOp::Resolve {
                            job,
                            choice: crate::files::queue::Conflict::Skip,
                            apply_all: all,
                        });
                        close = true;
                    }
                    if ui.button("重命名").clicked() {
                        op = Some(FileOp::Resolve {
                            job,
                            choice: crate::files::queue::Conflict::Rename,
                            apply_all: all,
                        });
                        close = true;
                    }
                    // 「取消」也要发出一个处置(按跳过算),**不能只关框** ——
                    // 那条 job 会永远挂在 `Conflict` 上,整个队列再也走不动,
                    // 而界面上看起来只是「卡住了」。处置本体在 `cancel_op`,
                    // 与标题栏那颗 ✕ 共用一份。
                    if ui.button("取消").clicked() {
                        cancelled!();
                    }
                });
            });
            if x {
                cancelled!();
            }
        }
    }

    if close {
        *dialog = None;
    }
    op
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F203 那条「弹窗次要文字必须用 `fg_muted`」的闸门**搬走了**,不在这里了。
    ///
    /// 原版是列举式的:一个写死的 `[("files_dialog.rs", ..), ("editor_window.rs", ..)]`
    /// 数组。写下它的时候只有这两个文件用了 `fg_muted`,于是另外五个弹窗文件
    /// (settings / import_dialog / edit_panel / history / host_key)共 13 处
    /// 一直漏在闸门外,一年后还是老样子 —— 「列举式门控在加档时必然漏」。
    ///
    /// F213 改成从源码现算弹窗清单的穷尽式守护:
    /// `tests/dialog_contrast.rs::nothing_drawn_on_the_dialog_fill_uses_a_token_that_fails_aa_on_it`。
    /// 这里只留这句指路,别再往回加文件名数组。
    ///
    /// 色板侧的算式仍在
    /// `theme::tests::the_secondary_text_token_used_in_dialogs_still_clears_aa_on_the_new_fill`。
    #[allow(dead_code)]
    const F203_GATE_MOVED_TO_DIALOG_CONTRAST_TEST: () = ();

    fn rp(s: &str) -> RemotePath {
        RemotePath::from_bytes(s.as_bytes().to_vec())
    }

    /// F203:`cancel_op` 是「取消这件事」的唯一真值 —— 六个变体一个不落。
    ///
    /// 前四个只关框(没有在途状态要收);后两个**必须发出处置**:只关框的话
    /// 那条 job / 那条编辑会永远挂在 `Conflict` 上,传输队列再也走不动、
    /// 文件一直红着,而界面上只看得出「卡住了」。
    ///
    /// 用 `match` 穷尽而不是列举六个 `assert`:加了第七个变体,编译器会在
    /// `cancel_op` 那边逼你处理,这条测试也会因为 `match` 不穷尽而编不过。
    #[test]
    fn cancelling_settles_the_two_dialogs_that_own_in_flight_work_and_only_closes_the_rest() {
        let all = [
            FilesDialog::NewDir {
                parent: rp("/srv"),
                name: "x".into(),
            },
            FilesDialog::Delete {
                targets: vec![(rp("/srv/a"), false)],
            },
            FilesDialog::Chmod {
                path: rp("/srv/a"),
                mode: 0o644,
            },
            FilesDialog::EditConflict {
                name: "/srv/a".into(),
                key: 9,
            },
            FilesDialog::Conflict {
                name: "a.bin".into(),
                job: 3,
                apply_all: true,
            },
        ];
        for d in &all {
            let got = cancel_op(d);
            let want = match d {
                FilesDialog::NewDir { .. }
                | FilesDialog::Delete { .. }
                | FilesDialog::Chmod { .. } => None,
                FilesDialog::EditConflict { .. } => Some(FileOp::ResolveEdit {
                    key: 9,
                    choice: EditResolve::KeepRemote,
                }),
                FilesDialog::Conflict { .. } => Some(FileOp::Resolve {
                    job: 3,
                    choice: crate::files::queue::Conflict::Skip,
                    // 取消是「这一条别传了」,不是「以后都别问了」——
                    // 哪怕框里勾着「对本批后续冲突都这么办」。
                    apply_all: false,
                }),
            };
            assert_eq!(got, want, "取消 {d:?} 的处置不对");
        }
    }

    /// F203:六个框的标题栏上都要有一个 ✕,而且**它就是「取消」**。
    ///
    /// 用户实报:确认框弹在近黑的终端上,「右上角有 ✕,表示关闭」是他找得到
    /// 的第一个出口。走 `egui::Window::open()` 而不是自己在正文里画一颗 ——
    /// 那样画出来的按钮低于标题栏一行,而且六份复制粘贴里总有一份会漏。
    ///
    /// 点击靠 accesskit 定位(egui 给这颗按钮登记的 label 是 "Close window",
    /// `egui-0.30.0/src/containers/window.rs:1213`);它是**画出来的两条线**,
    /// 不是字形,所以按 T9 的口径没有豆腐块风险,也没法用文本定位。
    ///
    /// 自证会变红:把 `modal()` 里的 `.open(&mut open)` 去掉。
    #[test]
    fn every_dialog_offers_a_close_cross_that_means_cancel() {
        for (label, mut d) in [
            (
                "删除",
                Some(FilesDialog::Delete {
                    targets: vec![(rp("/srv/a"), false)],
                }),
            ),
            (
                "属性",
                Some(FilesDialog::Chmod {
                    path: rp("/srv/a"),
                    mode: 0o644,
                }),
            ),
            (
                "新建文件夹",
                Some(FilesDialog::NewDir {
                    parent: rp("/srv"),
                    name: "x".into(),
                }),
            ),
            ("编辑冲突", edit_conflict()),
            ("传输冲突", conflict(true)),
        ] {
            let want = cancel_op(d.as_ref().expect("前提:框是开着的"));
            let op =
                click_close_cross(&mut d).unwrap_or_else(|| panic!("{label}框的标题栏上找不到 ✕"));
            assert_eq!(op, want, "{label}框的 ✕ 和「取消」不是同一件事");
            assert!(d.is_none(), "{label}框按了 ✕ 之后没关掉");
        }
    }

    /// 找到标题栏上那颗 ✕ 并点它。返回这一帧的 `FileOp`;找不到按钮就返回
    /// `None`(与「点了但没有 op」区分不开,所以调用方先用 `cancel_op` 算好
    /// 期望值,期望值为 `None` 的框另靠 `d.is_none()` 判定点没点着)。
    fn click_close_cross(dialog: &mut Option<FilesDialog>) -> Option<Option<FileOp>> {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut update = None;
        // 两帧:egui `Window` 首帧只记 `Shape::Noop`,几何也还没落定。
        for _ in 0..2 {
            update = ctx
                .run(egui::RawInput::default(), |ctx| {
                    show(ctx, &t, dialog);
                })
                .platform_output
                .accesskit_update;
        }
        let b = update?
            .nodes
            .iter()
            .find(|(_, n)| n.label() == Some("Close window"))
            .and_then(|(_, n)| n.bounds())?;
        let pos = egui::pos2(
            (b.x0 as f32 + b.x1 as f32) / 2.0,
            (b.y0 as f32 + b.y1 as f32) / 2.0,
        );
        let mut input = egui::RawInput::default();
        for pressed in [true, false] {
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            });
        }
        let mut op = None;
        let _ = ctx.run(input, |ctx| {
            op = show(ctx, &t, dialog);
        });
        Some(op)
    }

    fn conflict(apply_all: bool) -> Option<FilesDialog> {
        Some(FilesDialog::Conflict {
            name: "a.bin".into(),
            job: 3,
            apply_all,
        })
    }

    /// F55 的硬要求:绝不静默覆盖,四条出路一条都不能少。
    #[test]
    fn the_conflict_dialog_offers_four_choices_so_nothing_is_overwritten_by_default() {
        let mut d = conflict(false);
        let texts = dialog_texts(&mut d);
        for label in ["覆盖", "跳过", "重命名", "取消"] {
            assert!(
                texts.iter().any(|s| s == label),
                "冲突对话框少了「{label}」:{texts:?}"
            );
        }
    }

    #[test]
    fn choosing_overwrite_reports_the_job_id_and_the_apply_all_flag() {
        let mut d = conflict(true);
        let op = click_button(&mut d, "覆盖");
        assert_eq!(
            op,
            Some(FileOp::Resolve {
                job: 3,
                choice: crate::files::queue::Conflict::Overwrite,
                apply_all: true,
            })
        );
        assert!(d.is_none(), "选完该把框关掉");
    }

    /// 「取消」不能只关框:那条 job 会永远挂在 `Conflict` 上,
    /// 队列再也走不动,而界面上只看得出「卡住了」。
    #[test]
    fn canceling_a_conflict_still_settles_the_job_instead_of_leaving_it_stuck() {
        let mut d = conflict(true);
        let op = click_button(&mut d, "取消");
        assert_eq!(
            op,
            Some(FileOp::Resolve {
                job: 3,
                choice: crate::files::queue::Conflict::Skip,
                // 取消是「这一条别传了」,不是「以后都别问了」。
                apply_all: false,
            }),
            "取消必须把这条 job 收掉"
        );
    }

    fn edit_conflict() -> Option<FilesDialog> {
        Some(FilesDialog::EditConflict {
            name: "/etc/nginx/nginx.conf".into(),
            key: 9,
        })
    }

    /// D3-8:回传撞上「远端也被人改了」时,三条出路一条都不能少 ——
    /// 只给「覆盖 / 取消」的话,用户为了不丢自己的改动只能选覆盖,
    /// 于是对方的改动一定没了。
    #[test]
    fn the_edit_conflict_dialog_offers_a_way_out_that_loses_nobodys_work() {
        let mut d = edit_conflict();
        let texts = dialog_texts(&mut d);
        let joined = texts.join(" ");
        assert!(joined.contains("nginx.conf"), "该说清是哪个文件:{joined}");
        assert!(joined.contains("远端"), "该说清发生了什么:{joined}");
        for label in ["保留远端(丢弃我的改动)", "另存为副本", "覆盖远端", "取消"]
        {
            assert!(
                texts.iter().any(|s| s == label),
                "冲突处置框少了「{label}」:{texts:?}"
            );
        }
    }

    #[test]
    fn choosing_overwrite_reports_which_edit_it_belongs_to() {
        let mut d = edit_conflict();
        let op = click_button(&mut d, "覆盖远端");
        assert_eq!(
            op,
            Some(FileOp::ResolveEdit {
                key: 9,
                choice: EditResolve::Overwrite,
            })
        );
        assert!(d.is_none(), "选完该把框关掉");
    }

    #[test]
    fn choosing_save_a_copy_keeps_both_sides() {
        let mut d = edit_conflict();
        assert_eq!(
            click_button(&mut d, "另存为副本"),
            Some(FileOp::ResolveEdit {
                key: 9,
                choice: EditResolve::SaveCopy,
            })
        );
    }

    /// 「取消」必须落到 `KeepRemote`,**不能只关框**:只关框的话那条编辑
    /// 一直挂在 `Conflict` 上,每次存盘都想回传、每次都撞同一个冲突,
    /// 快照永远不动,界面上只看得出「这个文件一直红着」。
    #[test]
    fn canceling_an_edit_conflict_settles_it_instead_of_leaving_the_file_stuck_red() {
        let mut d = edit_conflict();
        let op = click_button(&mut d, "取消");
        assert_eq!(
            op,
            Some(FileOp::ResolveEdit {
                key: 9,
                choice: EditResolve::KeepRemote,
            }),
            "取消必须把这条编辑收掉"
        );
        assert!(d.is_none());
    }

    /// 文件和目录要**分开数**。混着数的话,「将删除 3 个文件」后面跟着
    /// 一个装了两千个文件的目录,用户完全低估了后果 —— 而这一步不可逆。
    #[test]
    fn the_delete_summary_counts_files_and_directories_separately() {
        let s = delete_summary(&[(rp("/a"), false), (rp("/b"), true), (rp("/c"), false)]);
        assert!(s.contains("2 个文件"), "实际: {s}");
        assert!(s.contains("1 个目录"), "实际: {s}");
        assert!(
            s.contains("连同其中全部内容"),
            "有目录时必须说清是连内容一起删: {s}"
        );
    }

    /// F202:Shift+Del **不弹确认框**,那句「将删除…」用户根本看不到 ——
    /// 唯一能告诉他刚才那一下打中了什么的就是这句吐司,所以它必须带上
    /// 同样分开数的计数。
    ///
    /// 而措辞必须是「正在」不是「已」:字节这会儿才刚发出去,成败要等
    /// `SftpOpDone` 回来。在发出的那一刻宣布「已删除」,链路一断就成了
    /// 假消息 —— 而这一片不可逆,用户会据此以为不用再管了。
    ///
    /// 自证会变红:把 `deleting_toast` 转发给 `delete_summary`。
    #[test]
    fn the_no_confirm_toast_says_it_is_under_way_not_that_it_is_done() {
        let s = deleting_toast(&[(rp("/a"), false), (rp("/b"), true)]);
        assert!(s.contains("1 个文件"), "实际: {s}");
        assert!(s.contains("1 个目录"), "实际: {s}");
        assert!(
            !s.contains("已删除") && !s.contains("将删除"),
            "免确认那条路上还没有结果,不能说「已」也不该说「将」: {s}"
        );
        assert!(s.contains("正在删除"), "实际: {s}");
    }

    /// 只有文件时不该冒出「0 个目录」这种话。
    #[test]
    fn the_delete_summary_does_not_mention_a_category_that_is_empty() {
        let s = delete_summary(&[(rp("/a"), false)]);
        assert_eq!(s, "将删除 1 个文件");
        let d = delete_summary(&[(rp("/a"), true)]);
        assert!(!d.contains("文件"), "只有目录时不该提文件: {d}");
    }

    /// 名字里有 `/` 必须挡下。`RemotePath::join` **不做归一化**(见其文档),
    /// 放过去的话 `../../etc/passwd` 会真的打到 `/etc/passwd` 上 ——
    /// 在「重命名」上这等于把远端系统文件改名。
    #[test]
    fn a_name_containing_a_slash_is_refused() {
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("../../etc").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name(".").is_err());
        assert!(validate_name("   ").is_err());
        assert!(validate_name("").is_err());
    }

    /// 正常名字(含中文、空格、点)必须放行 —— 判据写宽了会让
    /// 「我的 文档.txt」这种再普通不过的名字建不出来。
    #[test]
    fn ordinary_names_including_chinese_and_spaces_are_accepted() {
        assert!(validate_name("说明.md").is_ok());
        assert!(validate_name("my file.txt").is_ok());
        assert!(validate_name(".hidden").is_ok());
        assert!(validate_name("a.b.c").is_ok());
    }

    /// 九宫格与八进制必须互为逆运算,且位序与 `perm_string` 画出来的一致。
    /// 位序反了的症状是:用户勾「属主可写」,实际改的是「其他人可写」——
    /// 一个安全事故,而且界面上看不出来。
    #[test]
    fn the_permission_grid_round_trips_and_matches_the_rendered_order() {
        for mode in [0o000, 0o644, 0o755, 0o600, 0o777, 0o111] {
            assert_eq!(mode_from_bits(bits_from_mode(mode)), mode, "mode={mode:o}");
        }
        // 第 0 格 = 属主 r,对应 0o400。
        let mut bits = [false; 9];
        bits[0] = true;
        assert_eq!(mode_from_bits(bits), 0o400);
        // 第 8 格 = 其他 x,对应 0o001。
        let mut bits = [false; 9];
        bits[8] = true;
        assert_eq!(mode_from_bits(bits), 0o001);
        // 与渲染出来的字符串对齐:0o640 该画成 rw-r-----
        assert_eq!(crate::files::perm_string(0o640), "rw-r-----");
        let b = bits_from_mode(0o640);
        assert_eq!(
            (b[0], b[1], b[2], b[3], b[4], b[5]),
            (true, true, false, true, false, false)
        );
    }

    /// 两帧渲染,收集画出来的全部文本(egui `Window` 首帧 fade_in 只记
    /// `Shape::Noop`,只画一帧一个字都读不到)。
    fn rendered(dialog: &mut Option<FilesDialog>) -> Vec<String> {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut texts = Vec::new();
        for _ in 0..2 {
            texts.clear();
            let out = ctx.run(egui::RawInput::default(), |ctx| {
                show(ctx, &t, dialog);
            });
            for shape in out.shapes.iter() {
                if let egui::epaint::Shape::Text(ts) = &shape.shape {
                    texts.push(ts.galley.text().to_owned());
                }
            }
        }
        texts
    }

    /// 删除框必须**逐条列出完整远端路径**。只说「将删除 3 个文件」而不列
    /// 路径的话,用户没法在按下那颗不可逆的按钮之前核对自己选中的是什么。
    #[test]
    fn the_delete_dialog_lists_every_full_remote_path() {
        let mut d = Some(FilesDialog::Delete {
            targets: vec![
                (rp("/srv/app/config.toml"), false),
                (rp("/srv/app/logs"), true),
            ],
        });
        let texts = rendered(&mut d);
        for needle in [
            "/srv/app/config.toml",
            "/srv/app/logs",
            "不可逆",
            "1 个文件",
            "1 个目录",
        ] {
            assert!(
                texts.iter().any(|s| s.contains(needle)),
                "删除框里缺「{needle}」,实际画出来的文本: {texts:?}"
            );
        }
    }

    /// `show` 在 `dialog` 为 `None` 时必须什么都不画、什么都不返回 ——
    /// 没开框的那 99% 的帧走的是这条路。
    #[test]
    fn nothing_is_drawn_when_no_dialog_is_open() {
        let mut d = None;
        assert!(rendered(&mut d).is_empty());
    }

    /// 在渲染结果的形状树里找文字**恰好等于** `label` 的那一处的中心点。
    ///
    /// 用全等而不是 `contains`:删除框里「将删除 1 个文件」这句摘要也含
    /// 「删除」二字,`contains` 会先撞上它,点到一句 label 上去,而按钮
    /// 纹丝不动 —— 测试于是拿不到 `FileOp`,却看不出是查找写错了。
    fn find_button_pos(shapes: &[egui::epaint::ClippedShape], label: &str) -> Option<egui::Pos2> {
        fn walk(shape: &egui::Shape, label: &str) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, label)),
                egui::Shape::Text(ts) if ts.galley.text() == label => {
                    Some(ts.pos + ts.galley.size() / 2.0)
                }
                _ => None,
            }
        }
        shapes.iter().find_map(|cs| walk(&cs.shape, label))
    }

    /// 框里画出来的**全部文字**。用来断言「某个按钮/说明在不在」——
    /// 比 `find_button_pos` 宽松,失败信息里也能直接看到实际画了什么。
    fn dialog_texts(dialog: &mut Option<FilesDialog>) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(ts) => out.push(ts.galley.text().to_string()),
                _ => {}
            }
        }
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut shapes = Vec::new();
        // 两帧稳定布局(egui `Window` 首帧 fade_in 只记 `Shape::Noop`)。
        for _ in 0..2 {
            shapes = ctx
                .run(egui::RawInput::default(), |ctx| {
                    show(ctx, &t, dialog);
                })
                .shapes;
        }
        let mut out = Vec::new();
        for cs in &shapes {
            walk(&cs.shape, &mut out);
        }
        out
    }

    /// 点一下框里写着 `label` 的按钮,返回这一帧的 `FileOp`。
    fn click_button(dialog: &mut Option<FilesDialog>, label: &str) -> Option<FileOp> {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut out = None;
        // 两帧稳定布局(egui `Window` 首帧 fade_in 只记 `Shape::Noop`)。
        for _ in 0..2 {
            out = Some(ctx.run(egui::RawInput::default(), |ctx| {
                show(ctx, &t, dialog);
            }));
        }
        let pos = find_button_pos(&out.unwrap().shapes, label)
            .unwrap_or_else(|| panic!("框里没有写着「{label}」的按钮"));
        let mut input = egui::RawInput::default();
        for pressed in [true, false] {
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            });
        }
        let mut op = None;
        let _ = ctx.run(input, |ctx| {
            op = show(ctx, &t, dialog);
        });
        op
    }

    /// 「取消」必须把框关掉。关不掉的症状是一个永远消不掉的模态 ——
    /// 用户除了重启程序没有别的出路。
    #[test]
    fn cancelling_closes_the_dialog_without_producing_an_operation() {
        let mut d = Some(FilesDialog::Delete {
            targets: vec![(rp("/srv/a"), false)],
        });
        assert_eq!(click_button(&mut d, "取消"), None, "取消不该发出任何写操作");
        assert!(d.is_none(), "取消之后框必须关掉");
    }

    /// 确认之后:发出对应的 `FileOp`,且框同样要关掉 —— 不关的话用户会
    /// 在同一个框上再点一次,同一条删除发两遍。
    #[test]
    fn confirming_a_delete_emits_the_operation_and_closes_the_dialog() {
        let targets = vec![(rp("/srv/a"), false)];
        let mut d = Some(FilesDialog::Delete {
            targets: targets.clone(),
        });
        assert_eq!(
            click_button(&mut d, "删除"),
            Some(FileOp::Delete { targets }),
            "点「删除」该发出 Delete"
        );
        assert!(d.is_none(), "确认之后框必须关掉");
    }

    /// 新建文件夹的目标路径 = 当前目录 + 名字,且首尾空白要去掉 ——
    /// 「 新文件夹 」在远端会变成一个名字带空格、命令行里极难处理的目录。
    #[test]
    fn creating_a_directory_joins_the_trimmed_name_onto_the_current_directory() {
        let mut d = Some(FilesDialog::NewDir {
            parent: rp("/srv/app"),
            name: "  日志  ".into(),
        });
        assert_eq!(
            click_button(&mut d, "新建"),
            Some(FileOp::NewDir(rp("/srv/app/日志")))
        );
    }
}
