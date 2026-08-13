//! F2:`~/.ssh/config` 导入预览。
//!
//! 解析在 `mullion_store::ssh_config`(零 IO 纯函数),这里只管
//! 「哪几条要导、每条什么状态」以及把一条 `HostEntry` 变成 `SessionDraft`。
//! 真正落库在 `app.rs::apply_import` —— UI 层不碰 store,同会话/凭据两侧。

use mullion_store::{
    AppearancePrefs, Auth, AuthKind, Connection, HostEntry, Identity, NetworkPrefs, ParsedConfig,
    Protocol, SessionDraft, SessionRecord, SkipNote, TerminalPrefs,
};

/// 预览里一行的状态。决定默认勾不勾、显示什么颜色的说明。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    /// 库里没有同名会话,可以直接导。
    New,
    /// 库里已有同名会话。**默认不勾,勾上也是新建一条而非覆盖**(设计 D6)。
    Duplicate,
    /// config 里没写 `User`。仍可导入,导入后要自己补(设计 D9)。
    MissingUser,
}

/// 预览里的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRow {
    pub entry: HostEntry,
    pub selected: bool,
    pub status: RowStatus,
}

/// 解析结果 + 现有会话 → 预览行。
///
/// 同名判定用**会话名**(`identity.name`)而不是主机+端口:导入产生的会话名
/// 就是 config 里的别名,用户认的也是这个;两台机器指向同一个 host:port
/// 却起了不同别名,是完全正常的配置。
pub fn build_rows(parsed: &ParsedConfig, existing: &[SessionRecord]) -> Vec<ImportRow> {
    parsed
        .hosts
        .iter()
        .map(|entry| {
            let duplicate = existing.iter().any(|s| s.identity.name == entry.alias);
            let status = if duplicate {
                RowStatus::Duplicate
            } else if entry.user.trim().is_empty() {
                RowStatus::MissingUser
            } else {
                RowStatus::New
            };
            ImportRow {
                entry: entry.clone(),
                // 重名的默认不勾 —— 导入是加法,不该默认再塞一条同名的进去。
                selected: !duplicate,
                status,
            }
        })
        .collect()
}

/// 一条 `HostEntry` → `SessionDraft`。
///
/// `network.jump` 这里一律留 `Some(vec![])`(显式直连):`ProxyJump` 指向的是
/// 主机别名,要等全批落库拿到 `SessionId` 才翻得成引用,由 `app.rs` 第二阶段
/// 回填(设计 D4)。留 `None` 的话那是「继承上游」,语义完全不同。
pub fn draft_of(entry: &HostEntry) -> SessionDraft {
    // 私钥**只记路径到备注,不读正文**(设计 D5):v5 起私钥正文入库、不存
    // 路径,导入时批量读一堆私钥、遇加密的还要口令,等于替用户做主。
    let note = match &entry.identity_file {
        Some(p) => format!("从 ~/.ssh/config 导入。私钥:{p}(需在「认证」页手动导入正文)"),
        None => "从 ~/.ssh/config 导入。".to_string(),
    };
    let kind = match entry.identity_file {
        Some(_) => AuthKind::PublicKey {
            has_passphrase: false,
        },
        None => AuthKind::Password,
    };
    SessionDraft {
        identity: Identity {
            name: entry.alias.clone(),
            note,
            group_id: None,
            tags: Vec::new(),
        },
        connection: Connection {
            host: entry.hostname.clone(),
            port: entry.port,
            protocol: Protocol::Ssh,
        },
        auth: Auth::inline(entry.user.trim(), kind),
        terminal: TerminalPrefs::default(),
        appearance: AppearancePrefs::default(),
        network: NetworkPrefs {
            proxy: None,
            jump: Some(Vec::new()),
        },
        automation: Default::default(),
        sftp: Default::default(),
        secret: None,
    }
}

/// 「另有什么没导进来」那几行。空 = 整份文件都认得。
pub fn skip_lines(parsed: &ParsedConfig) -> Vec<String> {
    parsed
        .notes
        .iter()
        .map(|n| match n {
            SkipNote::UnknownDirectives(n) => {
                format!("另有 {n} 条指令本版不认识,未导入(如 ServerAliveInterval)")
            }
            SkipNote::NotIncluded(v) => format!("`Include {v}` 未展开 —— 请另行导入该文件"),
            SkipNote::MatchBlock => "有 `Match` 块被整块跳过(条件依赖 exec/user)".to_string(),
            SkipNote::NegatedPattern(v) => {
                format!("`Host {v}` 带否定模式,整块跳过")
            }
            SkipNote::BadPort { alias, value } => {
                format!("「{alias}」的 Port「{value}」不是合法端口,这台没导入")
            }
        })
        .collect()
}

/// 勾选行里,`ProxyJump` 指向了**本批之外**的别名的那些。
///
/// 用来在预览里当场标出来:那些会话照常导入,但跳板会留空(设计 D4)——
/// 不当场说,用户要到第一次连接失败才发现跳板没了。
pub fn dangling_jumps(rows: &[ImportRow]) -> Vec<(String, String)> {
    let inside: Vec<&str> = rows
        .iter()
        .filter(|r| r.selected)
        .map(|r| r.entry.alias.as_str())
        .collect();
    let mut out = Vec::new();
    for r in rows.iter().filter(|r| r.selected) {
        for j in &r.entry.proxy_jump {
            if !inside.contains(&j.as_str()) {
                out.push((r.entry.alias.clone(), j.clone()));
            }
        }
    }
    out
}

/// 预览弹窗的全部状态。`UiState::import` 是 `Some` 就等于弹窗开着。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportState {
    /// 选中的那个文件,原样显示 —— 用户手上可能有好几份 config。
    pub path: String,
    pub rows: Vec<ImportRow>,
    /// `skip_lines` 的结果,解析时算一次存着(每帧重算等于每帧重新格式化
    /// 一批字符串)。
    pub skipped: Vec<String>,
}

pub fn show(ctx: &egui::Context, t: &crate::theme::Theme, ui_state: &mut crate::ui::UiState) {
    use crate::ui::metrics::{SP_M, SP_S};

    let Some(st) = ui_state.import.as_mut() else {
        return;
    };
    let mut open = true;
    let mut confirm = false;
    let mut cancel = false;
    let win = egui::Window::new("导入 ssh config")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(560.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            ui.colored_label(crate::theme::c32(t.fg_dimmer), &st.path);
            ui.add_space(SP_S);

            if st.rows.is_empty() {
                ui.label("这份文件里没有可导入的主机。");
            } else {
                ui.horizontal(|ui| {
                    if ui.button("全选").clicked() {
                        for r in st.rows.iter_mut() {
                            r.selected = true;
                        }
                    }
                    if ui.button("全不选").clicked() {
                        for r in st.rows.iter_mut() {
                            r.selected = false;
                        }
                    }
                });
                ui.add_space(SP_S);
                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for r in st.rows.iter_mut() {
                            row_ui(ui, t, r);
                        }
                    });
            }

            // 跳板落在批外的,在按钮**之前**说清楚 —— 说在导入之后就成了
            // 事后通知(设计 D4)。
            let dangling = dangling_jumps(&st.rows);
            if !dangling.is_empty() {
                ui.add_space(SP_S);
                for (from, to) in &dangling {
                    ui.colored_label(
                        crate::theme::c32(t.warn),
                        format!("「{from}」的跳板「{to}」不在本次导入范围内,跳板会留空"),
                    );
                }
            }
            if !st.skipped.is_empty() {
                ui.add_space(SP_S);
                for line in &st.skipped {
                    ui.colored_label(crate::theme::c32(t.fg_dimmer), line);
                }
            }

            ui.add_space(SP_M);
            ui.separator();
            let picked = st.rows.iter().filter(|r| r.selected).count();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(picked > 0, egui::Button::new(format!("导入 {picked} 条")))
                    .clicked()
                {
                    confirm = true;
                }
                if ui.button("取消").clicked() {
                    cancel = true;
                }
            });
        });
    if let Some(w) = win {
        crate::ui::annotate::mark(ctx, "导入 ssh config 弹窗", w.response.rect);
    }

    if confirm {
        ui_state.import_request = ui_state.import.take().map(|s| s.rows);
    } else if cancel || !open {
        ui_state.import = None;
    }
}

fn row_ui(ui: &mut egui::Ui, t: &crate::theme::Theme, r: &mut ImportRow) {
    ui.horizontal(|ui| {
        ui.checkbox(&mut r.selected, "");
        ui.label(&r.entry.alias);
        let user = if r.entry.user.is_empty() {
            "?".to_string()
        } else {
            r.entry.user.clone()
        };
        ui.colored_label(
            crate::theme::c32(t.fg_dimmer),
            format!("{}@{}:{}", user, r.entry.hostname, r.entry.port),
        );
        match r.status {
            RowStatus::New => {}
            RowStatus::Duplicate => {
                ui.colored_label(
                    crate::theme::c32(t.warn),
                    "同名会话已存在 —— 勾上是新建一条",
                );
            }
            RowStatus::MissingUser => {
                ui.colored_label(crate::theme::c32(t.warn), "没写 User,导入后需补");
            }
        }
        if !r.entry.proxy_jump.is_empty() {
            ui.colored_label(
                crate::theme::c32(t.fg_dimmer),
                format!("跳板 {}", r.entry.proxy_jump.join(" → ")),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::{parse_ssh_config, SessionId};

    fn session(id: u64, name: &str) -> SessionRecord {
        SessionRecord {
            id: SessionId(id),
            modified_at: "t".into(),
            identity: Identity {
                name: name.into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: "h".into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth::inline("u", AuthKind::Password),
            terminal: Default::default(),
            appearance: Default::default(),
            network: NetworkPrefs::default(),
            automation: Default::default(),
            sftp: Default::default(),
        }
    }

    /// 重名的行默认**不勾**:导入是加法,不该默认再塞一条同名的进去
    /// (设计 D6)。用户仍可手动勾上 —— 那是新建,不是覆盖。
    #[test]
    fn a_row_clashing_with_an_existing_session_is_flagged_and_left_unchecked() {
        let parsed = parse_ssh_config("Host prod\n  User ops\nHost fresh\n  User ops\n");
        let rows = build_rows(&parsed, &[session(1, "prod")]);
        let prod = rows.iter().find(|r| r.entry.alias == "prod").expect("prod");
        assert_eq!(prod.status, RowStatus::Duplicate);
        assert!(!prod.selected, "重名的默认不该勾上");
        let fresh = rows
            .iter()
            .find(|r| r.entry.alias == "fresh")
            .expect("fresh");
        assert_eq!(fresh.status, RowStatus::New);
        assert!(fresh.selected);
    }

    /// 缺 `User` 的仍可导入,但要标出来 —— 拦下来的话,一份靠 ssh 默认
    /// 取本地登录名的 config 会一条都导不进来(设计 D9)。
    #[test]
    fn a_host_without_a_user_is_still_importable_but_flagged() {
        let rows = build_rows(&parse_ssh_config("Host box\n"), &[]);
        assert_eq!(rows[0].status, RowStatus::MissingUser);
        assert!(rows[0].selected, "缺用户名不该拦住导入");
    }

    /// 有 `IdentityFile` → 建成公钥认证,路径写进备注,**私钥正文不读**
    /// (设计 D5)。
    #[test]
    fn an_identity_file_becomes_a_note_not_a_stored_private_key() {
        let parsed = parse_ssh_config("Host prod\n  User ops\n  IdentityFile ~/.ssh/id_ed25519\n");
        let d = draft_of(&parsed.hosts[0]);
        assert!(
            matches!(
                d.auth.as_inline().map(|a| &a.kind),
                Some(AuthKind::PublicKey { .. })
            ),
            "有 IdentityFile 就该是公钥认证"
        );
        assert!(
            d.identity.note.contains("~/.ssh/id_ed25519"),
            "私钥路径必须留在备注里,否则这条信息就丢了:{}",
            d.identity.note
        );
        assert!(
            d.secret.is_none(),
            "导入不该往 secrets.enc 里搬私钥正文(设计 D5)"
        );
    }

    /// `jump` 必须是 `Some(vec![])`(显式直连)而不是 `None`(继承上游)。
    ///
    /// 写成 `None` 的现象很隐蔽:导入的会话没分组时看着一样,一旦用户把它
    /// 移进一个配了跳板的分组,它会**悄悄开始走那条跳板**——而 config 里
    /// 明明没写 ProxyJump。第二阶段回填也只会覆盖真有跳板的那几条。
    #[test]
    fn a_host_without_proxy_jump_is_explicitly_direct_not_inheriting() {
        let d = draft_of(&parse_ssh_config("Host box\n").hosts[0]);
        assert_eq!(d.network.jump, Some(Vec::new()));
    }

    /// 指向本批之外的跳板要当场标出来 —— 不标的话,用户要到第一次连接
    /// 失败才发现跳板没跟过来(设计 D4)。
    #[test]
    fn a_jump_pointing_outside_the_batch_is_reported() {
        let parsed = parse_ssh_config(
            "Host target\n  ProxyJump bastion\nHost bastion\n  User ops\n\
             Host lonely\n  ProxyJump gone\n",
        );
        let mut rows = build_rows(&parsed, &[]);
        assert!(
            !dangling_jumps(&rows)
                .iter()
                .any(|(from, _)| from == "target"),
            "两条都勾着时 bastion 在批内,target 不该被报"
        );
        // 取消勾选 bastion:target 的跳板就落到批外了。
        for r in rows.iter_mut() {
            if r.entry.alias == "bastion" {
                r.selected = false;
            }
        }
        let d = dangling_jumps(&rows);
        assert!(
            d.contains(&("target".to_string(), "bastion".to_string())),
            "取消勾选跳板后必须报出来:{d:?}"
        );
        assert!(d.contains(&("lonely".to_string(), "gone".to_string())));
    }

    /// 「没导进来什么」必须逐条说清 —— 静默丢弃会让用户以为整份配置都搬
    /// 过来了(设计 D2)。
    #[test]
    fn every_skipped_thing_gets_a_line_the_user_can_act_on() {
        let parsed = parse_ssh_config(
            "Include ~/.ssh/conf.d/x\nHost oops\n  Port 99999\nHost box\n  Compression yes\n",
        );
        let lines = skip_lines(&parsed);
        assert!(lines.iter().any(|l| l.contains("Include")), "{lines:?}");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("oops") && l.contains("99999")),
            "坏端口要点名是哪台、原值是什么:{lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("1 条指令")),
            "不认识的指令要报条数:{lines:?}"
        );
    }

    /// 弹窗渲染 + 点击的公共部分。跑 `FRAMES` 帧再按 `label` 找部件 ——
    /// 首帧 `Window`/`ScrollArea` 只记 `Shape::Noop`,位置也要几帧才收敛。
    ///
    /// `label = None` 只画不点。返回(画面文字, 跑完之后的 UiState)。
    fn run(rows: Vec<ImportRow>, label: Option<&str>) -> (Vec<String>, crate::ui::UiState) {
        const FRAMES: usize = 6;
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut ui_state = crate::ui::UiState {
            import: Some(ImportState {
                path: "/home/u/.ssh/config".into(),
                rows,
                skipped: Vec::new(),
            }),
            ..Default::default()
        };
        let mut shapes = Vec::new();
        for _ in 0..FRAMES {
            let full = ctx.run(egui::RawInput::default(), |ctx| {
                show(ctx, &t, &mut ui_state)
            });
            shapes = full.shapes;
        }
        let texts = all_text(&shapes);
        if let Some(label) = label {
            let pos = find_text_center(&shapes, label)
                .unwrap_or_else(|| panic!("导入弹窗里没有写着「{label}」的部件:{texts:?}"));
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
            let _ = ctx.run(input, |ctx| show(ctx, &t, &mut ui_state));
        }
        (texts, ui_state)
    }

    fn all_text(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
        fn walk(shape: &egui::Shape, acc: &mut Vec<String>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, acc)),
                egui::Shape::Text(ts) => acc.push(ts.galley.text().to_string()),
                _ => {}
            }
        }
        let mut acc = Vec::new();
        for cs in shapes {
            walk(&cs.shape, &mut acc);
        }
        acc
    }

    fn find_text_center(shapes: &[egui::epaint::ClippedShape], needle: &str) -> Option<egui::Pos2> {
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

    fn rows_of(text: &str) -> Vec<ImportRow> {
        build_rows(&parse_ssh_config(text), &[])
    }

    /// 预览要把每台机器的落点(user@host:port)摆出来,并且按钮上写清**这一次
    /// 会导几条** —— 「导入」两个字不带数字的话,用户是在没数的情况下按下的。
    ///
    /// 自证会变红:把按钮文案改成不带 `picked` 的 "导入"。
    #[test]
    fn the_preview_shows_each_target_and_counts_what_will_be_imported() {
        let mut rows = rows_of(
            "Host a
  HostName 10.0.0.1
  User root
Host b
  User dev
",
        );
        rows[1].selected = false;
        let (texts, _) = run(rows, None);
        assert!(
            texts.iter().any(|t| t.contains("root@10.0.0.1:22")),
            "落点没画出来:{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t == "导入 1 条"),
            "按钮上要写这次导几条:{texts:?}"
        );
    }

    /// 按下「导入」= 把勾好的行交给 `app.rs` 并关掉弹窗。两件事都要发生:
    /// 只关不交等于点了个寂寞,只交不关会在下一帧**再交一次**(重复导入)。
    ///
    /// 自证会变红:把 `confirm` 分支的 `import.take()` 改成 `import.clone()`。
    #[test]
    fn confirming_hands_the_rows_over_and_closes_the_dialog() {
        let rows = rows_of(
            "Host a
  User root
",
        );
        let (_, st) = run(rows, Some("导入 1 条"));
        let handed = st.import_request.expect("按了导入却没交出任何行");
        assert_eq!(handed.len(), 1);
        assert!(st.import.is_none(), "交出去了却没关窗 —— 下一帧会再导一次");
    }

    /// 取消什么都不该发生。
    #[test]
    fn cancelling_hands_nothing_over() {
        let rows = rows_of(
            "Host a
  User root
",
        );
        let (_, st) = run(rows, Some("取消"));
        assert!(st.import_request.is_none(), "取消却把行交出去了");
        assert!(st.import.is_none());
    }

    /// 跳板落在本批之外时,预览里要**当场**说 —— 事后才发现的话,用户已经
    /// 在拿一条跳板为空的会话连不上了(设计 D4)。
    ///
    /// 自证会变红:把 `show()` 里那段 `dangling_jumps` 的黄字删掉。
    #[test]
    fn a_jump_outside_the_batch_is_called_out_before_the_import_button() {
        let rows = rows_of(
            "Host target
  User root
  ProxyJump bastion
",
        );
        let (texts, _) = run(rows, None);
        assert!(
            texts
                .iter()
                .any(|t| t.contains("bastion") && t.contains("跳板会留空")),
            "批外跳板没在预览里说明:{texts:?}"
        );
    }
}
