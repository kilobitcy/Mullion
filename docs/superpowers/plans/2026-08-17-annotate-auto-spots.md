# F100 标注模式：自动候选 + 默认详细档 — 实现 plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 标注模式的候选从「只有 45 处手工容器」扩到「手工容器 + 所有 egui 控件」，并把默认导出档从紧凑改成详细。

**Architecture:** 手工 `annotate::mark()` 那条腿原样保留（它带 `文件:行号`，是 F100 的价值所在）；新增一条腿：标注模式开着时 `ctx.enable_accesskit()`，egui 每帧吐出含全部控件的 accesskit 树，`app.rs` 在 `ctx.run` 返回后、`handle_platform_output` 之前截走它，归约成不含 accesskit 类型的 `AutoNode`，下一帧由纯函数 `auto_spots()` 挂到「包住它的最小手工容器」下组装成候选。

**Tech Stack:** Rust / egui 0.30（`accesskit` feature）/ accesskit 0.17 / 既有 `crates/mullion-app/src/ui/annotate.rs`

设计文档：`docs/superpowers/specs/2026-08-17-annotate-auto-spots-design.md`（决策 D1~D12）

**与 spec 的一处偏离（已知，需在交付时报告）**：D12 里的「分隔条」不做。分隔线是相邻
pane 各让 1px 拼出来的，没有独立几何对象，且 1px 宽的目标鼠标点不中，标注它没有实用
价值。pane 整块 / 标题条 / 终端区三处照做。

---

### Task 1: `Src` 枚举 + 默认档改详细

自动候选没有插桩点，`Spot.src: &'static Location` 塞不下它。先把类型撑开，后面两个
Task 才有地方落。

**Files:**
- Modify: `crates/mullion-app/src/ui/annotate.rs`

- [ ] **Step 1: 写失败的测试**

加到 `mod tests` 末尾：

```rust
    /// 默认档必须是**详细** —— 用户要的是「一进标注模式就能看见这片区域里都有
    /// 什么」,紧凑档只列选中项,做不到这件事。
    ///
    /// 自证会变红:把 `Detail` 上的 `#[default]` 挪回 `Compact`。
    #[test]
    fn the_default_detail_level_is_full() {
        assert_eq!(Detail::default(), Detail::Full);
    }

    /// 三种来源在导出里长得不一样:手工插桩给得出 `文件:行号`,自动候选只能给出
    /// 「包住它的那个容器」的插桩点,没有容器时必须**明说自己不知道**,不能伪造
    /// 一个看起来像真的行号。
    ///
    /// 自证会变红:把 `src_of` 的 `Auto` 两支合并成一支。
    #[test]
    fn source_rendering_tells_manual_sites_apart_from_automatic_candidates() {
        let manual = Src::Site(Location::caller());
        assert!(src_of(&manual).contains(":"), "手工插桩要给出 文件:行号");

        let with_host = Src::Auto {
            container: Some("crates/mullion-app/src/ui/settings.rs:123".into()),
        };
        let s = src_of(&with_host);
        assert!(s.contains("settings.rs:123"), "要带上容器插桩点:{s}");
        assert!(s.contains("自动"), "要标明这是自动候选,不是插桩点本身:{s}");

        let orphan = Src::Auto { container: None };
        assert!(
            src_of(&orphan).contains("无插桩容器"),
            "没有容器时必须明说,不能伪造行号"
        );
    }
```

- [ ] **Step 2: 跑测试确认它红**

Run: `cargo test -p mullion-app annotate 2>&1 | tail -20`
Expected: 编译失败，`cannot find type Src`、`Detail::Full` 与 `Detail::default()` 不等。

- [ ] **Step 3: 实现**

在 `Spot` 定义上方加：

```rust
/// 一处候选的来源。
///
/// 分两支是因为自动候选**没有**插桩点:硬给它编一个 `Location` 等于骗人,而
/// F100 导出的全部价值就是「这段文本里的位置能直接拿去读代码」。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Src {
    /// 手工 `mark()` 的插桩点。`&'static Location` 是编译期常量。
    Site(&'static Location<'static>),
    /// accesskit 自动登记。`container` = 包住它的最小手工容器的插桩点
    /// (已渲染成 `文件:行号`),没有容器则 `None`。
    Auto { container: Option<String> },
}
```

`Spot.src` 改成 `pub src: Src`；`mark()` 里改成 `src: Src::Site(Location::caller())`。

`src_of` 改签名：

```rust
/// 来源渲染成给人和 Claude 读的一行。**保留完整相对路径**(不只留文件名):
/// 全路径能直接拿去 `Read`,`list.rs:349` 还得先搜一遍。
fn src_of(src: &Src) -> String {
    match src {
        Src::Site(loc) => format!("{}:{}", loc.file(), loc.line()),
        Src::Auto {
            container: Some(c),
        } => format!("{c}(容器 · 自动候选)"),
        Src::Auto { container: None } => "(自动候选 · 无插桩容器)".to_string(),
    }
}
```

`picked_of` 里 `src: src_of(s.src)` 改成 `src: src_of(&s.src)`。`markdown` 里
`src_of(sp.src)` 同改。`Detail` 的 `#[default]` 从 `Compact` 挪到 `Full`：

```rust
pub enum Detail {
    /// 只列选中项,一行一条。
    Compact,
    /// 加上本帧一共登记了多少处,以及每条的完整层级。
    Normal,
    /// 连**没选中**的候选一起列 —— 用来问「这一片区域里都有些什么」。
    /// **默认档**:一进标注模式就该看得见这片区域里有什么(D8)。
    #[default]
    Full,
}
```

`mod tests` 里的 helper `spot()` 改成 `src: Src::Site(Location::caller())`；
`two_rows_from_the_same_instrumentation_site_are_independently_selectable` 里的
`src_of(row1.src)` 改成 `src_of(&row1.src)`（两处）；
`compact_export_carries_path_rect_and_source_location_for_each_pick` 里
`md.contains(&src_of(s.src))` 同改。

- [ ] **Step 4: 跑测试确认绿**

Run: `cargo test -p mullion-app annotate 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`（`the_three_detail_levels_actually_differ` 等既有测试
显式传档，不受默认值改动影响）

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/annotate.rs
git commit -m "refactor(app): 标注候选的来源改成枚举,默认档改详细 (F100)

Spot.src 从 &'static Location 撑成 Src 枚举,给自动候选留出「只知道容器、
不知道行号」这一支;Detail 默认值从紧凑改详细。
跑的守护测试:the_default_detail_level_is_full、
source_rendering_tells_manual_sites_apart_from_automatic_candidates"
```

---

### Task 2: 纯函数 `auto_spots` —— 挂容器、去重、同名编号

这是本切片唯一必须字字对的逻辑，所以它不碰 egui 状态、不认 accesskit 类型。

**Files:**
- Modify: `crates/mullion-app/src/ui/annotate.rs`

- [ ] **Step 1: 写失败的测试**

加到 `mod tests` 末尾：

```rust
    fn auto(role: &'static str, label: Option<&str>, rect: egui::Rect) -> AutoNode {
        AutoNode {
            rect,
            role,
            label: label.map(str::to_string),
        }
    }

    /// 自动候选必须挂到**包住它的最小**手工容器下。挂错(挂到最外层那个铺满窗口
    /// 的容器)的话,导出里每一条的源码位置都会指向同一个文件,等于没有。
    ///
    /// 自证会变红:把 `auto_spots` 里选容器的 `min_by` 改成 `.next()`。
    #[test]
    fn an_automatic_candidate_hangs_under_the_smallest_container_that_encloses_it() {
        let manual = vec![
            spot("窗口", r(0.0, 0.0, 800.0, 600.0)),
            spot("窗口/设置", r(100.0, 100.0, 400.0, 300.0)),
        ];
        let got = auto_spots(&[auto("按钮", Some("保存"), r(120.0, 120.0, 60.0, 24.0))], &manual);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, "窗口/设置/按钮「保存」");
        assert_eq!(
            got[0].src,
            Src::Auto {
                container: Some(src_of(&manual[1].src))
            },
            "源码位置要指向内层容器的插桩点"
        );
    }

    /// 落在所有手工容器之外的控件仍要能选中 —— 只是没有容器可挂。
    #[test]
    fn a_candidate_outside_every_container_is_still_selectable() {
        let got = auto_spots(&[auto("按钮", Some("确定"), r(10.0, 10.0, 40.0, 20.0))], &[]);
        assert_eq!(got[0].path, "自动/按钮「确定」");
        assert_eq!(got[0].src, Src::Auto { container: None });
    }

    /// 手工已经登记过的那块矩形,自动候选不能再来一份:候选表里同一个东西出现
    /// 两次,点上去每次命中谁全看排序,而手工那份信息更多(有真行号)。
    ///
    /// 自证会变红:把 `auto_spots` 里那句 `if manual.iter().any(same_rect)` 删掉。
    #[test]
    fn a_candidate_that_duplicates_a_manual_site_is_dropped() {
        let manual = vec![spot("状态栏/隧道指示器", r(700.0, 580.0, 80.0, 18.0))];
        // 差半个点 —— 手工 mark 传的就是 widget 自己的 rect,不会分毫不差。
        let got = auto_spots(
            &[auto("按钮", Some("隧道"), r(700.4, 580.0, 80.0, 18.0))],
            &manual,
        );
        assert!(got.is_empty(), "与手工插桩重合的自动候选必须丢掉:{got:?}");
    }

    /// 同一个容器里五个「删除」按钮,不编号的话导出里五行一模一样,用户说
    /// 「第 3 个删除按钮」跟文本对不上。
    ///
    /// 自证会变红:把 `auto_spots` 里的 `seen` 计数删掉,永远用 base。
    #[test]
    fn same_named_controls_in_one_container_get_numbered() {
        let manual = vec![spot("列表", r(0.0, 0.0, 300.0, 300.0))];
        let nodes: Vec<AutoNode> = (0..3)
            .map(|i| auto("按钮", Some("删除"), r(10.0, 10.0 + i as f32 * 30.0, 60.0, 24.0)))
            .collect();
        let paths: Vec<String> = auto_spots(&nodes, &manual).into_iter().map(|s| s.path).collect();
        assert_eq!(
            paths,
            vec!["列表/按钮「删除」", "列表/按钮「删除」[2]", "列表/按钮「删除」[3]"]
        );
    }

    /// 没有标签的控件(图标按钮、纯装饰容器)不能给出空的「」—— 那种路径念不出来,
    /// 也搜不到。退回角色名。
    #[test]
    fn an_unlabeled_control_falls_back_to_its_role_name() {
        let got = auto_spots(
            &[
                auto("滚动区", None, r(0.0, 0.0, 100.0, 100.0)),
                auto("按钮", Some(""), r(0.0, 0.0, 20.0, 20.0)),
            ],
            &[],
        );
        assert_eq!(got[0].path, "自动/滚动区");
        assert_eq!(got[1].path, "自动/按钮", "空标签要当没有标签处理");
    }

    /// 退化矩形点不到,只会在 hit test 里当噪音 —— 跟 `mark()` 那条规则一致。
    #[test]
    fn degenerate_automatic_candidates_are_dropped() {
        let got = auto_spots(
            &[
                auto("按钮", Some("空"), egui::Rect::NOTHING),
                auto("按钮", Some("零宽"), r(10.0, 10.0, 0.0, 20.0)),
            ],
            &[],
        );
        assert!(got.is_empty(), "零面积/负矩形不该进候选表:{got:?}");
    }
```

- [ ] **Step 2: 跑测试确认它红**

Run: `cargo test -p mullion-app annotate 2>&1 | tail -20`
Expected: 编译失败，`cannot find type AutoNode`、`cannot find function auto_spots`。

- [ ] **Step 3: 实现**

加在 `pick()` 之后：

```rust
/// accesskit 树归约成的一个节点。**故意不含任何 accesskit 类型** —— 组装逻辑
/// (`auto_spots`)才好单测:造三个字段的字面量,而不是去串一堆 builder。
#[derive(Clone, Debug, PartialEq)]
pub struct AutoNode {
    pub rect: egui::Rect,
    /// 中文角色名,如「按钮」。来自 `role_name()`,是编译期常量。
    pub role: &'static str,
    /// 控件上的文字。egui 对 `Role::Label` 把文本放在 value 而不是 label 上,
    /// 归约那一层已经统一取过。
    pub label: Option<String>,
}

/// 两个矩形是不是同一块。阈值 1pt:手工 `mark()` 传的就是 widget 自己的 rect,
/// 但中间经过 `Area` 偏移与 DPI 换算,不会分毫不差。
fn same_rect(a: egui::Rect, b: egui::Rect) -> bool {
    (a.left() - b.left()).abs() < 1.0
        && (a.top() - b.top()).abs() < 1.0
        && (a.right() - b.right()).abs() < 1.0
        && (a.bottom() - b.bottom()).abs() < 1.0
}

/// 把归约后的 accesskit 节点组装成候选:挂容器(D4)、去重(D6)、同名编号(D7)。
///
/// **纯函数**,不碰 egui 状态。`manual` 是本帧手工 `mark()` 的那些。
pub fn auto_spots(auto: &[AutoNode], manual: &[Spot]) -> Vec<Spot> {
    let mut out = Vec::new();
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for n in auto {
        if !n.rect.is_positive() {
            continue;
        }
        // D6:手工那份信息更多(有真行号),重合就让给它。
        if manual.iter().any(|m| same_rect(m.rect, n.rect)) {
            continue;
        }
        // D4:挂到**包住它的最小**手工容器下 —— 同 `hit()` 里「面积最小 = 最具体」
        // 的道理,挂到最外层那个铺满窗口的容器等于没挂。
        let host = manual
            .iter()
            .filter(|m| m.rect.contains_rect(n.rect))
            .min_by(|a, b| {
                a.rect
                    .area()
                    .partial_cmp(&b.rect.area())
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        let leaf = match n.label.as_deref() {
            Some(l) if !l.is_empty() => format!("{}「{}」", n.role, l),
            _ => n.role.to_string(),
        };
        let base = match host {
            Some(h) => format!("{}/{}", h.path, leaf),
            None => format!("自动/{leaf}"),
        };
        let count = seen.entry(base.clone()).or_insert(0);
        *count += 1;
        let path = if *count == 1 {
            base
        } else {
            format!("{base}[{count}]")
        };
        out.push(Spot {
            path,
            rect: n.rect,
            src: Src::Auto {
                container: host.map(|h| src_of(&h.src)),
            },
        });
    }
    out
}
```

- [ ] **Step 4: 跑测试确认绿**

Run: `cargo test -p mullion-app annotate 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/annotate.rs
git commit -m "feat(app): 自动候选的组装逻辑(挂容器/去重/编号) (F100)

纯函数 auto_spots:把归约后的 accesskit 节点挂到包住它的最小手工容器下,
与手工插桩重合的丢弃,同容器同名的加序号。
跑的守护测试:an_automatic_candidate_hangs_under_the_smallest_container_that_encloses_it、
a_candidate_that_duplicates_a_manual_site_is_dropped、same_named_controls_in_one_container_get_numbered"
```

---

### Task 3: 合并层 `all_spots` —— 让读候选的四个入口都看见自动候选

`spot_paths` / `ensure_picked` / `spot_rect` / `overlay` 现在各自读 `st.spots`。
自动候选进来之后，四处必须看同一张表，否则「悬停能选中但导出里没有」这种错位没人
查得出来。

**Files:**
- Modify: `crates/mullion-app/src/ui/annotate.rs`

- [ ] **Step 1: 写失败的测试**

```rust
    /// 读候选的每个入口都必须看见自动候选。只改 `overlay` 不改 `spot_paths` 的话,
    /// 屏上点得中、`ui_shot` 与测试却看不见 —— 这种错位在无头环境里查不出来。
    ///
    /// 自证会变红:把 `spot_paths` 改回只读 `st.spots`。
    #[test]
    fn every_reader_sees_both_manual_and_automatic_candidates() {
        let ctx = egui::Context::default();
        toggle(&ctx);
        with_state(&ctx, |st| {
            st.auto = vec![AutoNode {
                rect: r(10.0, 10.0, 60.0, 24.0),
                role: "按钮",
                label: Some("保存".into()),
            }];
        });
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            mark(ctx, "设置", r(0.0, 0.0, 200.0, 200.0));
        });

        let paths = spot_paths(&ctx);
        assert!(paths.contains(&"设置".to_string()), "手工那条腿:{paths:?}");
        assert!(
            paths.contains(&"设置/按钮「保存」".to_string()),
            "自动那条腿:{paths:?}"
        );
        assert_eq!(
            spot_rect(&ctx, "设置/按钮「保存」"),
            Some(r(10.0, 10.0, 60.0, 24.0))
        );
        assert!(ensure_picked(&ctx, "按钮「保存」"), "自动候选也要能被选中");
    }
```

- [ ] **Step 2: 跑测试确认它红**

Run: `cargo test -p mullion-app annotate::tests::every_reader 2>&1 | tail -20`
Expected: 编译失败（`State` 没有 `auto` 字段），补上字段后断言红在「自动那条腿」。

- [ ] **Step 3: 实现**

`State` 加字段：

```rust
    /// 上一帧 accesskit 树归约出来的自动候选(D9:本帧的树要等 `ctx.run` 返回后
    /// 才拿得到,只能给下一帧用)。
    auto: Vec<AutoNode>,
    /// `overlay` 那块吃指针的全屏区自己的 widget id。它 `Sense::click()` 是
    /// focusable 的,会进 accesskit 树,不剔掉就成了一个盖住全屏的候选(D11)。
    overlay_id: Option<egui::Id>,
```

加合并函数：

```rust
/// 本帧的完整候选表 = 手工插桩 + 自动候选。**读候选的地方一律走这里**,
/// 别再直接读 `st.spots`。
fn all_of(st: &State) -> Vec<Spot> {
    let mut v = st.spots.clone();
    v.extend(auto_spots(&st.auto, &st.spots));
    v
}
```

四个入口改成走它：

```rust
pub fn spot_paths(ctx: &egui::Context) -> Vec<String> {
    with_state(ctx, |st| {
        all_of(st).into_iter().map(|s| s.path).collect()
    })
}

#[cfg(test)]
pub fn spot_rect(ctx: &egui::Context, needle: &str) -> Option<egui::Rect> {
    with_state(ctx, |st| {
        let all = all_of(st);
        all.iter()
            .find(|s| s.path == needle)
            .or_else(|| all.iter().find(|s| s.path.contains(needle)))
            .map(|s| s.rect)
    })
}

pub fn ensure_picked(ctx: &egui::Context, needle: &str) -> bool {
    with_state(ctx, |st| {
        let Some(sp) = all_of(st).into_iter().find(|s| s.path.contains(needle)) else {
            return false;
        };
        let p = picked_of(&sp);
        if !st.picked.contains(&p) {
            st.picked.push(p);
        }
        true
    })
}
```

`overlay` 里取候选那一句改成：

```rust
    let (spots, mut picked, export_request, detail, auto_empty) = with_state(ctx, |st| {
        (
            all_of(st),
            st.picked.clone(),
            std::mem::take(&mut st.export_request),
            st.detail,
            st.auto.is_empty(),
        )
    });
```

`toggle` 关闭那一支补 `st.auto.clear();`（否则下次进来第一帧带着上次界面的鬼影候选）。

- [ ] **Step 4: 跑测试确认绿**

Run: `cargo test -p mullion-app annotate 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/annotate.rs
git commit -m "feat(app): 候选表统一走合并层,自动候选对四个读取入口可见 (F100)

spot_paths/spot_rect/ensure_picked/overlay 一律读 all_of(),避免「屏上点得中
但导出/测试看不见」的错位。
跑的守护测试:every_reader_sees_both_manual_and_automatic_candidates"
```

---

### Task 4: accesskit 接线（feature + enable/disable + ingest）

**Files:**
- Modify: `crates/mullion-app/Cargo.toml`（工作区已有这处改动，是可行性验证时加的，
  本 Task 负责把注释一并改对）
- Modify: `crates/mullion-app/src/ui/annotate.rs`

- [ ] **Step 1: 写失败的测试**

```rust
    /// 端到端:一个**普通的 egui 按钮**,没有任何插桩,必须在两帧之后成为候选。
    /// 这条钉住整条链 —— `enable_accesskit` → egui 构树 → `ingest_accesskit`
    /// 归约 → `auto_spots` 组装。链上任何一环断了它就红,而这是唯一能证明
    /// 「自动那半边真的通了」的测试。
    ///
    /// 自证会变红:把 `toggle` 里的 `ctx.enable_accesskit()` 注释掉。
    #[test]
    fn a_plain_egui_button_becomes_a_candidate_without_any_instrumentation() {
        let ctx = egui::Context::default();
        toggle(&ctx);

        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 300.0),
            )),
            ..Default::default()
        };
        let draw = |ctx: &egui::Context| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = ui.button("保存");
            });
        };

        // 第一帧:egui 构树,但树要等 run 返回才拿得到(D9)。
        let mut out = ctx.run(input(), |ctx| draw(ctx));
        ingest_accesskit(&ctx, out.platform_output.accesskit_update.take());

        // 第二帧:自动候选可用。在 `overlay` 之前读 —— overlay 末尾会清空 spots。
        let mut paths = Vec::new();
        let _ = ctx.run(input(), |ctx| {
            draw(ctx);
            paths = spot_paths(ctx);
        });
        assert!(
            paths.iter().any(|p| p.contains("按钮「保存」")),
            "没插桩的按钮必须成为候选,实际候选:{paths:?}"
        );
    }

    /// 模式关着时不许构树 —— 「模式关着零开销」是 F100 的硬约束(N3 红线的邻居)。
    ///
    /// 自证会变红:把 `ingest_accesskit` 开头的 `if !is_on(ctx) { return; }` 删掉,
    /// 再把 `toggle` 里的 `disable_accesskit()` 去掉。
    #[test]
    fn nothing_is_ingested_while_the_mode_is_off() {
        let ctx = egui::Context::default();
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = ui.button("保存");
            });
        });
        assert!(
            out.platform_output.accesskit_update.is_none(),
            "没开标注模式,egui 不该构 accesskit 树"
        );
    }

    /// 退出标注模式要连自动候选一起清 —— 留着的话下次进来第一帧带着上次界面的
    /// 候选(用户已经切到别的标签页了),点上去描在一片空地上。
    ///
    /// 自证会变红:把 `toggle` 里的 `st.auto.clear()` 删掉。
    #[test]
    fn leaving_the_mode_clears_the_automatic_candidates_too() {
        let ctx = egui::Context::default();
        toggle(&ctx);
        with_state(&ctx, |st| {
            st.auto = vec![AutoNode {
                rect: r(0.0, 0.0, 10.0, 10.0),
                role: "按钮",
                label: None,
            }];
        });
        exit(&ctx);
        assert!(spot_paths(&ctx).is_empty(), "退出后候选表必须是空的");
    }
```

- [ ] **Step 2: 跑测试确认它红**

Run: `cargo test -p mullion-app annotate 2>&1 | tail -20`
Expected: 编译失败，`cannot find function ingest_accesskit`；
`accesskit_update` 字段不存在（若 Cargo.toml 的 feature 还没开）。

- [ ] **Step 3: 实现**

`crates/mullion-app/Cargo.toml` 正式依赖段（工作区里已改，确认成这样）：

```toml
# F100 标注模式的自动候选:开着模式时 egui 每帧吐一棵含全部控件的 accesskit
# 树,给标注模式当候选(见 ui/annotate.rs)。`egui-winit` 的同名 feature **必须
# 同步开** —— 它对 `PlatformOutput` 做穷尽解构,少一个字段就 E0027 整片编不过。
# 我们不调 `init_accesskit`,所以不会注册 Windows UIA adapter,没有平台侧开销。
egui = { workspace = true, features = ["accesskit"] }
egui-wgpu.workspace = true
egui-winit = { workspace = true, features = ["accesskit"] }
```

同时把 dev-dependencies 段里那段「正式构建不受影响 / 正式 exe 里 egui/egui-winit
都还是原样」的注释改掉——它已经不成立了，留着就是下一次踩坑的陷阱。改成：

```toml
# 离屏 UI 截图 harness(`examples/ui_shot.rs`):无头把 egui 外壳渲染成 PNG,
# 给「改完能自己看一眼」用。**只在 example/test target 生效**,不进 exe。
egui_kittest = { version = "0.30", features = ["wgpu"] }
```

（`egui-winit = { workspace = true, features = ["accesskit"] }` 那条 dev-dependency
**删掉**：正式依赖里已经开了同一个 feature，dev 段再写一遍是死代码。）

`annotate.rs` 里 `toggle` 补上开关：

```rust
pub fn toggle(ctx: &egui::Context) -> bool {
    let on = !is_on(ctx);
    ctx.data_mut(|d| d.insert_temp(on_id(), on));
    // D2:树只在模式开着时构。关着时 egui 压根不进 accesskit 那段代码,
    // 「模式关着零开销」这条不变。
    if on {
        ctx.enable_accesskit();
    } else {
        ctx.disable_accesskit();
    }
    with_state(ctx, |st| {
        st.on = on;
        if !on {
            st.picked.clear();
            st.spots.clear();
            st.auto.clear();
            st.export_request = false;
        }
    });
    on
}
```

加归约与摄入。文件顶部的 `use` 补一行——`accesskit` 类型走 egui 的 re-export
（`egui/src/lib.rs:448` 的 `pub use accesskit;`），**不要**在 `Cargo.toml` 里再直接
依赖一个 `accesskit`，那会变成两个版本各说各话：

```rust
use egui::accesskit;
```

```rust
/// accesskit 的角色翻成人说的话。路径要照着念给 Claude 听,`TextInput` 念不出来。
fn role_name(r: accesskit::Role) -> &'static str {
    use accesskit::Role;
    match r {
        Role::Button => "按钮",
        Role::Label => "文字",
        Role::TextInput | Role::MultilineTextInput => "输入框",
        Role::CheckBox => "复选框",
        Role::RadioButton => "单选",
        Role::RadioGroup => "单选组",
        Role::ComboBox => "下拉框",
        Role::Slider => "滑块",
        Role::SpinButton => "数值框",
        Role::Link => "链接",
        Role::ColorWell => "色块",
        Role::ProgressIndicator => "进度",
        Role::ScrollView => "滚动区",
        Role::Window => "窗口",
        _ => "控件",
    }
}

/// 吃下本帧的 accesskit 树,归约成 `AutoNode` 存起来给**下一帧**当候选(D9)。
///
/// 由 `app.rs` 在 `ctx.run` 返回之后、`handle_platform_output` 之前调 ——
/// 那个函数按值吃掉整个 `PlatformOutput`,晚一步就拿不到了。
pub fn ingest_accesskit(ctx: &egui::Context, update: Option<accesskit::TreeUpdate>) {
    if !is_on(ctx) {
        return;
    }
    let Some(update) = update else {
        return;
    };
    // D11:overlay 自己那块吃指针的全屏区也会进树,不剔掉就是一个盖住全屏的候选。
    // `Id::accesskit_id()` 是 `pub(crate)`,外面调不到;`Id::value()` 是 pub,
    // 而 egui 自己那句就是 `self.value().into()`(`egui/src/id.rs:82`),所以
    // 这样构出来的 NodeId 与 egui 写进树里的那个必然相等。
    let skip = with_state(ctx, |st| {
        st.overlay_id
            .map(|id| accesskit::NodeId::from(id.value()))
    });
    let nodes: Vec<AutoNode> = update
        .nodes
        .iter()
        .filter(|(id, _)| Some(*id) != skip)
        .filter_map(|(_, n)| {
            // bounds 为 None 的是根节点之类没有几何的东西,点不到。
            let b = n.bounds()?;
            Some(AutoNode {
                rect: egui::Rect::from_min_max(
                    egui::pos2(b.x0 as f32, b.y0 as f32),
                    egui::pos2(b.x1 as f32, b.y1 as f32),
                ),
                role: role_name(n.role()),
                // egui 对 `Role::Label` 把文本写进 value 而不是 label
                // (`response.rs:1035`),两处都要看。
                label: n
                    .label()
                    .map(str::to_string)
                    .or_else(|| n.value().map(str::to_string)),
            })
        })
        .collect();
    with_state(ctx, |st| st.auto = nodes);
}
```

`overlay` 里记下自己的 id、并在还没有自动候选时催一帧（D10）：

```rust
            let resp = ui.allocate_rect(screen, egui::Sense::click());
            with_state(ui.ctx(), |st| st.overlay_id = Some(resp.id));
```

以及在 `.show(ctx, …)` 之后、写回 state 之前：

```rust
    // D10:刚进模式那一帧手上还没有树 —— 主动催一帧,否则没有别的输入时
    // 自动候选永远不出现。**只在空的时候催**:每帧无条件催就是 T3/N3 那条
    // 「每秒几千次重绘」的红线。
    if auto_empty {
        ctx.request_repaint();
    }
```

- [ ] **Step 4: 跑测试确认绿**

Run: `cargo test -p mullion-app annotate 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/Cargo.toml crates/mullion-app/src/ui/annotate.rs
git commit -m "feat(app): 标注模式接上 accesskit,没插桩的控件也能选中 (F100)

模式开着时 enable_accesskit,ingest_accesskit 把树归约成 AutoNode 给下一帧
当候选;剔掉 overlay 自己那块全屏区;首帧催一帧重绘。
正式依赖开 egui/egui-winit 的 accesskit feature(两个同名 feature 必须同步,
否则 PlatformOutput 穷尽解构 E0027)。
跑的守护测试:a_plain_egui_button_becomes_a_candidate_without_any_instrumentation、
nothing_is_ingested_while_the_mode_is_off、leaving_the_mode_clears_the_automatic_candidates_too"
```

---

### Task 5: `app.rs` 帧末把树交给 `annotate`

**Files:**
- Modify: `crates/mullion-app/src/app.rs:7553-7577`

**这一步没有自动守护测试**：它在 `render_frame` 里，那个函数要真的 GPU 与窗口。
链路本身由 Task 4 的端到端测试覆盖，接线只有一行；漏了的症状是「标注模式下自动候选
永远为空」，进人工验收清单第 2 条。

- [ ] **Step 1: 改接线**

`let full_output = a.egui_ctx.run(...)` 改成 `let mut full_output = ...`，
并在 `handle_platform_output` 之前插入：

```rust
    // F100:本帧的 accesskit 树(标注模式开着时才有)。**必须在
    // `handle_platform_output` 之前取** —— 那个函数按值吃掉整个
    // `PlatformOutput`。`take()` 走也不影响 egui-winit:我们没调
    // `init_accesskit`,它那边的 adapter 是 `None`,拿到 Some/None 都不做事。
    crate::ui::annotate::ingest_accesskit(
        &a.egui_ctx,
        full_output.platform_output.accesskit_update.take(),
    );
```

- [ ] **Step 2: 编译 + 全量测试**

Run: `cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log | tail -20`
Expected: 全部 `test result: ok.`，无 FAILED

- [ ] **Step 3: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 帧末把 accesskit 树交给标注模式 (F100)

在 handle_platform_output 之前 take 走 accesskit_update —— 那个函数按值
吃掉整个 PlatformOutput,晚一步就拿不到。
链路由 annotate::tests::a_plain_egui_button_becomes_a_candidate_without_any_instrumentation 覆盖。"
```

---

### Task 6: pane 三处手工插桩（D12）

终端网格不是 egui widget，accesskit 覆盖不到，而「这块终端」正是最常要指的东西。

**Files:**
- Modify: `crates/mullion-app/src/ui/pane_title.rs:117-124`

- [ ] **Step 1: 写失败的测试**

加到 `pane_title.rs` 的 `mod tests`（沿用该文件既有的 `ctx.run` 写法）：

```rust
    /// pane 整块 / 标题条 / 终端区都要能在标注模式里选中。终端网格是 GPU 自绘的,
    /// 不是 egui widget,accesskit 那条腿覆盖不到它 —— 只能手工插桩。
    ///
    /// **标题条关掉时(F83)仍要能标 pane 与终端区**:`show` 里那句
    /// `if tp.h == 0 { continue }` 在插桩之后,不能把它们一起跳过。
    ///
    /// 自证会变红:把 `show` 里三句 `annotate::mark` 中任意一句注释掉。
    #[test]
    fn panes_are_markable_even_when_the_title_bar_is_hidden() {
        for title_h in [0, 32] {
            let ctx = egui::Context::default();
            crate::ui::annotate::toggle(&ctx);
            // `geom_800x600_title32` 是本文件既有的 helper(见
            // `area_rect_matches_title_px_exactly_across_dpi_scales`)。
            let mut view = TitleView {
                geom: geom_800x600_title32(1),
                index: 1,
                host: Some("dev@build-01"),
                status: PaneStatus::Live,
                focused: true,
                appearance: None,
            };
            view.geom.title_px.h = title_h;
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                show(ctx, &crate::theme::MULLION_DARK, std::slice::from_ref(&view));
            });
            let paths = crate::ui::annotate::spot_paths(&ctx);
            assert!(paths.contains(&"分屏 1".to_string()), "title_h={title_h}: {paths:?}");
            assert!(
                paths.contains(&"分屏 1/终端区".to_string()),
                "title_h={title_h}: {paths:?}"
            );
            assert_eq!(
                paths.contains(&"分屏 1/标题条".to_string()),
                title_h != 0,
                "title_h={title_h}: 标题条关掉时不该登记一个零高的标题条"
            );
        }
    }
```

`PxRect` 的字段是 `x` / `y` / `w` / `h`，都是 `u32`
（`crates/mullion-app/src/shell/workspace/geom.rs:16`）。

- [ ] **Step 2: 跑测试确认它红**

Run: `cargo test -p mullion-app pane_title 2>&1 | tail -20`
Expected: FAIL —— `paths` 里没有 `分屏 1`

- [ ] **Step 3: 实现**

`show()` 的循环开头，**在 `if tp.h == 0 { continue }` 之前**：

```rust
    for v in views {
        let tp = v.geom.title_px;
        // F100:pane 三处插桩。终端网格是 GPU 自绘的,accesskit 那条腿看不见它,
        // 而「这块终端」恰恰是最常要指的东西。**放在下面那个 `continue` 之前** ——
        // 标题条关掉时(F83)pane 和终端区仍然要能标。
        let px = |r: crate::shell::workspace::PxRect| {
            egui::Rect::from_min_size(
                egui::pos2(r.x as f32 / ppp, r.y as f32 / ppp),
                egui::vec2(r.w as f32 / ppp, r.h as f32 / ppp),
            )
        };
        crate::ui::annotate::mark(ctx, format!("分屏 {}", v.index), px(v.geom.px));
        crate::ui::annotate::mark(
            ctx,
            format!("分屏 {}/终端区", v.index),
            px(v.geom.term_px),
        );
        if tp.h != 0 {
            crate::ui::annotate::mark(ctx, format!("分屏 {}/标题条", v.index), px(tp));
        }
        if tp.h == 0 {
            continue; // 标题条关掉了(F83 开关)
        }
```


- [ ] **Step 4: 跑测试确认绿**

Run: `cargo test -p mullion-app pane_title 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/pane_title.rs
git commit -m "feat(app): 分屏整块/标题条/终端区进标注模式候选 (F100/F83)

终端网格是 GPU 自绘的,accesskit 覆盖不到,只能手工插桩;插桩放在
「标题条关掉就 continue」之前,F83 关着时 pane 与终端区仍可标。
跑的守护测试:panes_are_markable_even_when_the_title_bar_is_hidden"
```

---

### Task 7: 文档 + 交付一条龙

**Files:**
- Modify: `spec.md`（F100 条目）
- Modify: `Cargo.toml`（`workspace.package.version`）

- [ ] **Step 1: 改 spec.md 的 F100 条目**

把 F100 那一行的描述改成（保留原有编号与优先级、验收列）：

```
| F100 | 标注模式：`Ctrl+Shift+F` 进/出，鼠标悬停给容器与控件描边并显示语义路径，点击选中并打上 ①②③ 编号，`Ctrl+Shift+E` 把一段 Markdown 写进剪贴板（每处含语义路径 + 屏幕矩形 + 插桩点的 `文件:行号`），`Ctrl+Shift+D` 在紧凑/标准/详细三档间循环（**默认详细**）。候选有两个来源：手工 `annotate::mark()` 的**容器**（带真实行号）与 **accesskit 自动树**（模式开着时才构，覆盖全部 egui 控件，挂在包住它的容器下） | P1 | 模式关着时零登记开销（`enable_accesskit` 只在模式开着时调）；开着时点会话行不会真的切会话；导出的文本粘进 Claude Code 后，能不经追问直接定位到对应代码 |
```

- [ ] **Step 2: 跑绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log | tail -20
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo fmt --check
```
Expected: 无 FAILED；clippy 无输出；fmt 无输出

- [ ] **Step 3: 版本 bump + 提交**

`Cargo.toml` 的 `workspace.package.version` 第三位 +1（0.1.47 → 0.1.48）：

```bash
git add spec.md Cargo.toml Cargo.lock
git commit -m "chore: 版本 0.1.48(标注模式自动覆盖全部 egui 控件 + 默认详细档)"
```

- [ ] **Step 4: 交叉编译 + objdump 验收**

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app
x86_64-w64-mingw32-objdump -p target/x86_64-pc-windows-gnu/release/mullion.exe | grep "DLL Name" | sort -u
```
Expected: 出现 `uiautomationcore.dll` / `propsys.dll`（accesskit 带来的，Windows 自带，
可接受）；**不得**出现 `libgcc_s_seh-1.dll` / `libwinpthread-1.dll`

- [ ] **Step 5: 发 Release**

```bash
cd target/x86_64-pc-windows-gnu/release
sha256sum mullion.exe > mullion.exe.sha256
HTTPS_PROXY=http://127.0.0.1:7890 gh release create v0.1.48 \
  mullion.exe mullion.exe.sha256 -t "v0.1.48" -F /tmp/notes.md --repo kilobitcy/Mullion
```

notes 里写：改了什么 + 下面这份人工验收清单 + sha256 + `Unblock-File .\mullion.exe`
的首次运行提示。

人工验收清单（无头验不了的部分）：

1. `Ctrl+Shift+F` 后，提示条显示档位「详细」。
2. 悬停能描边并选中：菜单栏各按钮、设置弹窗里的输入框 / 复选框 / 下拉框 / 滑块、
   会话管理器表单里的各字段、文件面板的行与列头。
3. 分屏整块、pane 标题条、终端区能选中；关掉标题条（F83）后前两者仍可选。
4. `Ctrl+Shift+E` 导出后粘贴，检查自动候选那些行的路径读得懂、容器前缀对。
5. 退出标注模式后：终端里 `Esc`、打字、鼠标划选一切照旧；CPU 占用回到平时水平
   （催帧那条若写错，症状是退出后仍持续满帧重绘）。
6. 若机器上开着 NVDA / 讲述人，顺手确认没有异常行为（我们不 init adapter，
   理论上系统无障碍栈感知不到本应用有变化）。
