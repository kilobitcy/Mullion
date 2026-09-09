//! F239:「点在弹窗外面就关掉它」的判定。
//!
//! **为什么不在每个弹窗的 `show()` 里各判一次**:需求里有一条「一次点击
//! 最多关一个弹窗」。各判各的话,两个叠着的弹窗会被同一下点击一起关掉,
//! 而「最上层是谁」只有把全部候选摆在一起才答得出来。
//!
//! **为什么不用 `ctx.top_layer_id()` 判最上层**:它走的 `Areas::order` 里
//! 关掉的窗会一直赖着(egui 只按 `Order` 排序,从不按可见性剔除,见
//! `egui-0.30.0/src/memory/mod.rs:1276`)。用它的话,一个早就关掉的窗会
//! 永远占着「最上层」,真正开着的那个再也关不掉 —— 而且完全静默。
//! 顺序改由调用方给一张**手排的候选表**,它与 `ui::build_ui` 的绘制顺序
//! 严格互逆(后画的盖在上面)。
//!
//! `locate` 只读 `egui::Context`;`pick` 是纯函数。判定与接线分开,是因为
//! 「点在哪」在无窗口测试里造得出来,「该关谁」造不出来。

/// 一次指针按下,相对某个弹窗落在哪儿。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Where {
    /// 落在这个弹窗自己身上。
    Inside,
    /// 落在它外面 —— 终端、菜单栏、标签栏、下层的另一个弹窗、空白处都算。
    Outside,
    /// 这一下不该拿来判:落在**层序高于窗口**的东西上(下拉菜单、右键菜单、
    /// tooltip、`egui::Modal` 的遮罩)。那些都是弹窗的延伸或盖在它上面的
    /// 另一个模态,按「外面」处理会让点一下下拉选项就把整个窗关掉。
    Undecided,
}

/// 一个候选弹窗。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    /// `None` = 这个弹窗根本没开着,不占「最上层」的位置。
    pub hit: Option<Where>,
    /// 有未保存的改动 —— 点外面**不关**(点错一下就把填了一半的表单
    /// 静默清掉,是这条需求唯一会造成真实损失的方式)。
    pub dirty: bool,
}

/// `pos` 处这一下,相对 `areas` 这一**组** area 落在哪。
///
/// **收的是一组而不是一个**:一个弹窗可以拥有不止一个 area。会话管理器
/// 就在自己上面开「删除凭据」/「删除隧道」两个独立的 `Order::Middle` 确认
/// 窗 —— 只登记主窗的话,点那两个确认框会先把底下的会话管理器关掉(判在
/// 按下那一刻),确认框**永远按不到**,而且完全静默。
///
/// `egui::Window::new(t)` 的 area id 恒为 `Id::new(t)`
/// (`egui-0.30.0/src/containers/window.rs:56`);`egui::Area::new(id)` 就是
/// `id` 本身。
pub fn locate(ctx: &egui::Context, areas: &[egui::Id], pos: egui::Pos2) -> Where {
    match ctx.layer_id_at(pos) {
        // 底下什么 area 都没有 = 点在面板上(菜单栏/标签栏/状态栏)或者
        // 终端上。两者都算「外面」。
        None => Where::Outside,
        Some(l) if areas.contains(&l.id) => Where::Inside,
        Some(l) if !matches!(l.order, egui::Order::Background | egui::Order::Middle) => {
            Where::Undecided
        }
        Some(_) => Where::Outside,
    }
}

/// 这一下该关掉哪个候选(返回下标)。`dialogs` **从上到下**排好。
///
/// 只看最上层那一个:开着的最上层弹窗被点在外面且不脏 → 关它;
/// 否则一个都不关(**不穿透**——上层脏着,这一下的意思是「我还在填」,
/// 不是「把底下那个关了」)。
pub fn pick(dialogs: &[Candidate]) -> Option<usize> {
    let (ix, top) = dialogs.iter().enumerate().find(|(_, d)| d.hit.is_some())?;
    match top.hit {
        Some(Where::Outside) if !top.dirty => Some(ix),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(hit: Option<Where>, dirty: bool) -> Candidate {
        Candidate { hit, dirty }
    }

    /// 点在最上层弹窗自己身上 —— 什么都不该发生。
    #[test]
    fn a_click_inside_the_dialog_closes_nothing() {
        assert_eq!(pick(&[c(Some(Where::Inside), false)]), None);
    }

    /// 点在它外面 —— 关掉它。这是本切片存在的理由。
    #[test]
    fn a_click_outside_the_dialog_closes_it() {
        assert_eq!(pick(&[c(Some(Where::Outside), false)]), Some(0));
    }

    /// 有未保存改动的弹窗,点外面**不关**。
    ///
    /// 手一滑点到终端就把填了一半的表单静默清掉,是这条需求唯一会造成
    /// 真实损失的方式。
    ///
    /// 自证会变红:把 `pick` 里的 `!top.dirty` 去掉。
    #[test]
    fn a_dirty_dialog_survives_a_click_outside() {
        assert_eq!(pick(&[c(Some(Where::Outside), true)]), None);
    }

    /// 两个叠着时**只关最上层那一个**。一次点击最多关一个窗 ——
    /// 点一下少两层,用户会以为程序崩了一半。
    ///
    /// 自证会变红:把 `pick` 的 `find` 换成对全表逐个判定(即遍历所有
    /// 开着的候选各关各的)。
    #[test]
    fn only_the_topmost_dialog_is_closed_by_one_click() {
        // 上层开着且被点在外面,下层也开着 —— 只有 0 号该关。
        assert_eq!(
            pick(&[
                c(Some(Where::Outside), false),
                c(Some(Where::Outside), false)
            ]),
            Some(0)
        );
    }

    /// 最上层那个脏了,**不许穿透**去关下层的。
    ///
    /// 自证会变红:让 `pick` 在 top 脏的时候接着往下找。
    #[test]
    fn a_dirty_top_dialog_does_not_let_the_click_fall_through() {
        assert_eq!(
            pick(&[
                c(Some(Where::Outside), true),
                c(Some(Where::Outside), false)
            ]),
            None
        );
    }

    /// 没开着的候选不占「最上层」的位置。
    #[test]
    fn closed_dialogs_do_not_claim_the_top_slot() {
        assert_eq!(
            pick(&[c(None, false), c(Some(Where::Outside), false)]),
            Some(1)
        );
    }

    /// `Undecided` 一律不判 —— 那是弹窗的延伸(下拉菜单/tooltip)或盖在它
    /// 上面的另一个模态。
    ///
    /// 少了这条,会话管理器里点一下任何一个下拉框的选项就把整个窗关掉,
    /// 而那是个每天都要用到的操作。
    ///
    /// 自证会变红:把 `pick` 里那个 `Some(Where::Outside)` 的模式放宽成
    /// 「只要不是 Inside 就关」。
    #[test]
    fn a_click_on_a_popup_belonging_to_the_dialog_decides_nothing() {
        assert_eq!(pick(&[c(Some(Where::Undecided), false)]), None);
    }

    /// 跑两帧让布局稳下来(首帧尺寸未定,`egui::Window` 需要一次量出内容
    /// 高度才收敛),再把 `draw` 摆的全部弹窗/浮层交给 `probe` 去查。
    fn probe<R>(draw: impl Fn(&egui::Context), probe: impl FnOnce(&egui::Context) -> R) -> R {
        let ctx = egui::Context::default();
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| draw(ctx));
        }
        probe(&ctx)
    }

    /// 点在这个弹窗自己的矩形里 → `Inside`。
    #[test]
    fn a_point_inside_the_dialogs_own_area_is_inside() {
        let id = egui::Id::new("Dialog");
        let where_ = probe(
            |ctx| {
                egui::Window::new("Dialog")
                    .fixed_pos(egui::pos2(0.0, 0.0))
                    .show(ctx, |ui| {
                        ui.label("content");
                    });
            },
            |ctx| {
                let rect = ctx.memory(|m| m.area_rect(id)).expect("窗口应有矩形");
                locate(ctx, &[id], rect.center())
            },
        );
        assert_eq!(where_, Where::Inside);
    }

    /// 点在空白处(底下什么 area 都没有)→ `Outside`。
    #[test]
    fn a_point_on_blank_space_is_outside() {
        let id = egui::Id::new("Dialog");
        let where_ = probe(
            |ctx| {
                egui::Window::new("Dialog")
                    .fixed_pos(egui::pos2(0.0, 0.0))
                    .show(ctx, |ui| {
                        ui.label("content");
                    });
            },
            |ctx| locate(ctx, &[id], egui::pos2(9000.0, 9000.0)),
        );
        assert_eq!(where_, Where::Outside);
    }

    /// 点在**另一个** `Order::Middle` 的窗上 → `Outside`。
    #[test]
    fn a_point_on_another_middle_order_window_is_outside() {
        let dialog_id = egui::Id::new("Dialog");
        let other_id = egui::Id::new("Other");
        let where_ = probe(
            |ctx| {
                egui::Window::new("Dialog")
                    .fixed_pos(egui::pos2(0.0, 0.0))
                    .show(ctx, |ui| {
                        ui.label("content");
                    });
                egui::Window::new("Other")
                    .fixed_pos(egui::pos2(1000.0, 0.0))
                    .show(ctx, |ui| {
                        ui.label("other content");
                    });
            },
            |ctx| {
                let other_rect = ctx.memory(|m| m.area_rect(other_id)).expect("窗口应有矩形");
                locate(ctx, &[dialog_id], other_rect.center())
            },
        );
        assert_eq!(where_, Where::Outside);
    }

    /// 点在一个 `Order::Foreground` 的 area 上(下拉菜单/tooltip 那一档)
    /// → `Undecided`。
    #[test]
    fn a_point_on_a_foreground_area_is_undecided() {
        let dialog_id = egui::Id::new("Dialog");
        let popup_id = egui::Id::new("Popup");
        let where_ = probe(
            |ctx| {
                egui::Window::new("Dialog")
                    .fixed_pos(egui::pos2(0.0, 0.0))
                    .show(ctx, |ui| {
                        ui.label("content");
                    });
                egui::Area::new(popup_id)
                    .order(egui::Order::Foreground)
                    .fixed_pos(egui::pos2(1000.0, 1000.0))
                    .show(ctx, |ui| {
                        ui.label("popup");
                    });
            },
            |ctx| {
                let popup_rect = ctx.memory(|m| m.area_rect(popup_id)).expect("浮层应有矩形");
                locate(ctx, &[dialog_id], popup_rect.center())
            },
        );
        assert_eq!(where_, Where::Undecided);
    }

    /// 一个弹窗拥有多个 area 时,点在它的任何一个 area 上都算 `Inside`。
    ///
    /// 场景对应会话管理器上叠的「删除凭据」确认窗:主窗与确认窗是两个
    /// 独立的 area,只登记主窗的话,点确认窗会被误判成「外面」。
    ///
    /// 自证会变红:把 `locate` 里 `areas.contains(&l.id)` 改成
    /// `areas.first() == Some(&l.id)`。
    #[test]
    fn a_point_on_any_area_owned_by_the_dialog_is_inside() {
        let main_id = egui::Id::new("Dialog");
        let confirm_id = egui::Id::new("Dialog::Confirm");
        let where_ = probe(
            |ctx| {
                egui::Window::new("Dialog")
                    .fixed_pos(egui::pos2(0.0, 0.0))
                    .show(ctx, |ui| {
                        ui.label("content");
                    });
                egui::Area::new(confirm_id)
                    .fixed_pos(egui::pos2(1000.0, 1000.0))
                    .show(ctx, |ui| {
                        ui.label("confirm delete?");
                    });
            },
            |ctx| {
                let confirm_rect = ctx
                    .memory(|m| m.area_rect(confirm_id))
                    .expect("确认窗应有矩形");
                locate(ctx, &[main_id, confirm_id], confirm_rect.center())
            },
        );
        assert_eq!(where_, Where::Inside);
    }
}
