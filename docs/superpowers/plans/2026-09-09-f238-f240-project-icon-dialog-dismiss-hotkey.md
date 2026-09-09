# F238/F239/F240 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 项目可自设图标(F238);弹窗点在外面即关、脏了不关(F239);焦点分屏上 `Ctrl+Shift+N` 从终端区现场信息预填出一条新项目(F240)。

**Architecture:** 三件事共用一条主线 —— **判据先做成纯函数,接线只负责喂参数**。
F238 把「这个项目该画哪张图标」收成 `project::icon_for` 一个函数,三处列表 + pane 标题条共用;
F239 把「这一下点在弹窗里还是外面」收成 `ui::dismiss` 两个纯函数(`locate` 只读 egui 层信息、
`pick` 全纯),app.rs 只留一张**穷尽 match** 的候选表;F240 把「从 pane 现场推出一份项目草稿」
收成 `project::prefill_from_pane`,app.rs 只做快捷键接线。

**Tech Stack:** Rust workspace(`mullion-store` / `mullion-app`)、egui 0.30、`ico`+`base64`
(已有的 `ui::ico`)、`egui_kittest` 不用(既有列表测试全走 `egui::Context::run` + `Shape` 比对)。

---

## 背景事实(已核实,写代码时直接用,不要再猜)

| 事实 | 出处 |
|---|---|
| `egui::Window::new(title)` 的 area id 恒为 `Id::new(title)` | `egui-0.30.0/src/containers/window.rs:56` |
| `egui::Modal` 的 area 是 `Order::Foreground`,且**自带**遮罩点击关闭 | `containers/modal.rs:38`、`modal.rs:155` |
| `ctx.layer_id_at(pos)` 只认「上一帧或本帧可见」的 area,关掉的窗不会残留 | `memory/mod.rs:1199-1216` |
| `ctx.top_layer_id()` 走的 `order` **不过滤可见性**,关掉的窗会赖着 → **不能用它判最上层** | `memory/mod.rs:1276-1282` |
| `Modal::ALL` 是 23 个变体的穷尽表,`modal_open()` 的 match 必须留在原函数体里 | `app.rs:2406-2506`、`app.rs:3661-3663` |
| `build_ui` 的绘制顺序 = 弹窗的叠放顺序(后画的盖在上面) | `ui/mod.rs:883-1036` |
| `paste.rs` 已经接了 `resp.should_close()`(遮罩点击 + Esc) | `ui/paste.rs:87-91` |
| `session_manager::buffer::is_dirty(buf, baseline)` 已存在 | `ui/session_manager/buffer.rs:207` |
| `project_manager::form_column` 已有 `let dirty = stored != Some(&*draft);` | `ui/project_manager.rs` |
| `ProjectIntent::Add(name)` **立刻落盘**(`store.add_project` 不校验) | `app.rs:12053-12105`、`vault.rs` |
| `files_hotkey_event` 是快捷键接线的模板,且必须排在输入分流**之前**(T8) | `app.rs:3794-3813`、`app.rs:10213-10221` |
| `TabContent::focused_pane_cwd()` / `focused_pane_host_ix()` 已存在 | `app.rs:1106` / `app.rs:1114` |
| `ui::ico::import(&[u8]) -> Result<String, ImportError>`,`ImportError::message()` | `ui/ico.rs:97`、`ico.rs:53` |
| `badge::paint_icon(painter, rect, icon, bg)` 只认 `IconKind::Ico`,32/64 两档 | `ui/badge.rs:222` |
| `ui_state.pick_icon_request` → app.rs 起线程开系统文件框 → `UserEvent::IconPathPicked` | `ui/session_manager/mod.rs:879`、`app.rs:9899` |
| `CURRENT_SCHEMA = 10`,升号规则见其文档注释;两条测试钉着这个数 | `model.rs:234`、`migrate.rs:262`、`project.rs:492` |

---

## File Structure

**新建**
- `crates/mullion-app/src/ui/dismiss.rs` —— F239 的判定。零状态,只读 `egui::Context`,`pick` 全纯。

**修改**
- `crates/mullion-store/src/model.rs` —— `CURRENT_SCHEMA` 10 → 11 + 文档
- `crates/mullion-store/src/migrate.rs` —— 版本钉死那条测试
- `crates/mullion-store/src/project.rs` —— `ProjectRecord.icon` + 版本断言 + 往返测试
- `crates/mullion-app/src/project.rs` —— `icon_for` / `icon_bg` / `prefill_from_pane`
- `crates/mullion-app/src/ui/project_row.rs` —— 图标槽位 + 坐标右移
- `crates/mullion-app/src/ui/project_manager.rs` —— 三处 `Row` 补字段 + 右栏「外观」分节
- `crates/mullion-app/src/ui/launcher.rs` / `ui/project_pick.rs` —— `Row` 补字段
- `crates/mullion-app/src/ui/pane_title.rs` —— `TitleView.project_icon` + 顶掉会话图标
- `crates/mullion-app/src/ui/tab_props.rs` —— `is_dirty`
- `crates/mullion-app/src/ui/files_dialog.rs` —— `title_of`
- `crates/mullion-app/src/ui/session_manager/mod.rs` —— `WINDOW_TITLE` 转 `pub`、`is_dirty` 转 `pub`
- `crates/mullion-app/src/ui/import_dialog.rs` —— `ImportState.picked0`
- `crates/mullion-app/src/ui/rehost.rs` / `ui/project_pick.rs` —— `area_id` 由 `pub(super)` 转 `pub(crate)`
- `crates/mullion-app/src/ui/mod.rs` —— `mod dismiss;`、`icon_target`、`UiFrame` 传图标
- `crates/mullion-app/src/ui/shortcuts.rs` —— `Ctrl+Shift+N` 一行 + 新 scope
- `crates/mullion-app/src/app.rs` —— F239 候选表/接线、F240 快捷键、F238 图标落盘
- `spec.md` —— 登记 F238/F239/F240

---

# Part A —— F238 项目图标

### Task 1: `ProjectRecord.icon` 与 schema v11

**Files:**
- Modify: `crates/mullion-store/src/project.rs`
- Modify: `crates/mullion-store/src/model.rs:203-234`
- Modify: `crates/mullion-store/src/migrate.rs:256-263`

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-store/src/project.rs` 的 `mod tests` 里,紧挨着
`unset_optional_fields_are_not_written_out` 加两条,并把版本断言那条改掉:

```rust
    /// F238:项目自设的图标要能原样往返。**跟着 `[[project]]` 存在一起**,
    /// 不另开一张表 —— 图标是项目的一个属性,拆开存会让「删项目」多一处
    /// 要记得清的地方,而漏清的症状是下一个拿到同一个 id 的项目莫名带上
    /// 前任的图标。
    ///
    /// 自证会变红:把 `ProjectRecord.icon` 字段删掉(编译不过)或加上
    /// `#[serde(skip)]`(读回来是 `None`)。
    #[test]
    fn a_project_icon_round_trips_through_toml() {
        let mut p = super::tests::helpers::with_id(1, "web", None);
        p.icon = Some(crate::model::IconSpec {
            kind: crate::model::IconKind::Ico,
            value: "AAAA".into(),
            bg: None,
        });
        let s = toml::to_string_pretty(&p).expect("项目应能序列化");
        let back: ProjectRecord = toml::from_str(&s).expect("项目应能读回来");
        assert_eq!(back.icon, p.icon, "图标没往返回来:{s}");
    }

    /// 没设图标的项目不该往 TOML 里写空键(同 `tmux_name`/`last_accessed_at`)。
    #[test]
    fn a_project_without_an_icon_writes_no_icon_key() {
        let p = super::tests::helpers::with_id(1, "web", None);
        let s = toml::to_string_pretty(&p).expect("项目应能序列化");
        assert!(!s.contains("icon"), "没设图标不该写出 icon 键:{s}");
    }
```

并把既有的版本断言改成 11(两处):

```rust
    /// schema 必须升到 11:旧客户端读到 v11 会把 `[[project]].icon` 当未知
    /// 字段丢掉再写回 —— **用户设的图标静默消失**。拒绝比装作能用好。
    #[test]
    fn the_schema_version_is_bumped_so_old_clients_refuse_instead_of_dropping_projects() {
        assert_eq!(crate::model::CURRENT_SCHEMA, 11);
    }
```

`crates/mullion-store/src/migrate.rs:260-263` 那条同改:

```rust
    /// 版本号是**故意**钉死的:动它就意味着用户的库要迁移一次,不该被
    /// 顺手改掉。v11 的理由见 `CURRENT_SCHEMA` 文档 —— `[[project]]` 多了
    /// `icon` 键,旧客户端读 v11 会把它当未知字段丢掉再写回,**用户设的
    /// 图标静默消失**,拒绝比装作能用好。
    #[test]
    fn current_schema_is_eleven() {
        assert_eq!(crate::model::CURRENT_SCHEMA, 11);
    }
```

- [ ] **Step 2: 跑测试确认它红**

Run: `cargo test -p mullion-store project:: 2>&1 | tail -20`
Expected: FAIL —— `no field 'icon' on type ProjectRecord` / `left: 10, right: 11`

- [ ] **Step 3: 最小实现**

`crates/mullion-store/src/project.rs`,在 `ProjectRecord` 的 `last_accessed_at` 之后加:

```rust
    /// F238:项目自设的图标。`None` = 回落首选节点的图标(解析在 app 侧的
    /// `project::icon_for`,store 不认识 `SessionRecord` 的外观)。
    ///
    /// 复用会话侧同一个 `IconSpec` 而不是新开一个类型:导入归一化
    /// (`ui::ico`)、渲染(`badge::paint_icon`)两条路径都只认它,新开类型
    /// 等于把那两条路径各复制一遍。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<crate::model::IconSpec>,
```

`crates/mullion-store/src/model.rs:234` 附近,在 v10 那段之后补一段文档、改常量:

```rust
/// v11 = v10 + `[[project]].icon`:项目可自设图标(F238)。
///
/// 同样**没有一行迁移转换代码**(旧文件没这个键 → `serde(default)` 补 `None`)。
/// 升号的理由与 v10 一致:旧客户端读 v11 会把 `icon` 当未知字段丢掉再写回,
/// **用户设的图标静默消失**。
pub const CURRENT_SCHEMA: u32 = 11;
```

- [ ] **Step 4: 跑测试确认它绿**

Run: `cargo test -p mullion-store 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`(若有别处构造 `ProjectRecord` 的字面量报缺字段,逐个补 `icon: None`)

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-store/src/project.rs crates/mullion-store/src/model.rs crates/mullion-store/src/migrate.rs
git commit -m "feat(store): 项目记录带图标,schema 升 v11 (F238)"
```

---

### Task 2: 图标解析纯函数 `project::icon_for` / `icon_bg`

**Files:**
- Modify: `crates/mullion-app/src/project.rs`

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/project.rs` 的 `mod tests` 末尾加:

```rust
    /// F238:项目自设了图标就用自己的。
    #[test]
    fn a_project_with_its_own_icon_uses_it() {
        let mut cache = crate::ui::badge::AppearanceCache::default();
        cache.rebuild(&[sess_with_icon(7, "node")], &[]);
        let mut p = proj(1, "web");
        p.nodes = vec![SessionId(7)];
        p.preferred = Some(SessionId(7));
        p.icon = Some(ico("PROJECT"));
        assert_eq!(
            icon_for(&p, &cache).map(|i| i.value.as_str()),
            Some("PROJECT"),
            "项目自设的图标应该赢过节点的"
        );
    }

    /// 没设就回落**首选节点**的已解析图标 —— 项目在列表里跟会话挨着,
    /// 一个空槽会被读成「这个项目坏了」,而它本来就有一张现成的图可用。
    ///
    /// 自证会变红:把 `icon_for` 的 `.or_else(..)` 整段删掉。
    #[test]
    fn a_project_without_an_icon_falls_back_to_its_preferred_node() {
        let mut cache = crate::ui::badge::AppearanceCache::default();
        cache.rebuild(&[sess_with_icon(7, "NODE")], &[]);
        let mut p = proj(1, "web");
        p.nodes = vec![SessionId(7)];
        p.preferred = Some(SessionId(7));
        assert_eq!(
            icon_for(&p, &cache).map(|i| i.value.as_str()),
            Some("NODE"),
            "没设图标应回落首选节点的"
        );
    }

    /// 一个节点都没勾的项目没有图标可回落,返回 `None` 而不是 panic。
    #[test]
    fn a_project_with_no_nodes_has_no_icon_to_fall_back_to() {
        let cache = crate::ui::badge::AppearanceCache::default();
        assert!(icon_for(&proj(1, "web"), &cache).is_none());
    }
```

同一个 `mod tests` 里补两个 helper(若 `proj` 已存在就复用既有的,别新建同名):

```rust
    fn ico(v: &str) -> mullion_store::IconSpec {
        mullion_store::IconSpec {
            kind: mullion_store::IconKind::Ico,
            value: v.into(),
            bg: None,
        }
    }

    /// 带图标的会话。`AppearanceCache::rebuild` 只认 `SessionRecord.appearance`。
    fn sess_with_icon(id: u64, icon_value: &str) -> mullion_store::SessionRecord {
        let mut s = sess(id);
        s.appearance.icon = Some(ico(icon_value));
        s
    }
```

> 若本文件 `mod tests` 里还没有 `proj` / `sess` 这两个构造器,照抄
> `crates/mullion-app/src/ui/project_row.rs` 的 `mod tests` 里那两个
> (`proj(id, name, dir, accessed)` / `sess(id, name)`),并把上面三条测试的
> 调用改成对应签名。**不要**改 `project_row` 里那两个的签名。

- [ ] **Step 2: 跑测试确认它红**

Run: `cargo test -p mullion-app --lib project::tests 2>&1 | tail -20`
Expected: FAIL —— `cannot find function 'icon_for' in this scope`

- [ ] **Step 3: 最小实现**

`crates/mullion-app/src/project.rs`,紧挨着 `node_for` 加:

```rust
/// F238:这个项目该画哪张图标。
///
/// 项目自设的优先;没设就回落**首选节点**的已解析外观 —— 走
/// [`node_for`],和 `plan_open` 真拨号、和列表副标题写的节点名是**同一个
/// 函数**。各写一份的话,行上画着 A 的图标、副标题写着 B 的名字。
///
/// 回落而不是留空:项目列表和会话列表在同一个程序里挨着,一列空槽会被读成
/// 「这个项目坏了」,而首选节点的图标本来就是用户为这台机器挑的那一张。
pub fn icon_for<'a>(
    p: &'a mullion_store::ProjectRecord,
    appearance: &'a crate::ui::badge::AppearanceCache,
) -> Option<&'a mullion_store::IconSpec> {
    p.icon.as_ref().or_else(|| {
        node_for(p)
            .and_then(|id| appearance.get(id))
            .and_then(|a| a.icon.as_ref())
    })
}

/// F238:图标底色。**不做「项目色」** —— 同源回落首选节点的节点色,
/// 与 pane 标题条/标签栏取色是同一份 `should_paint`,两处各算一遍的话
/// 同一个项目在两个地方会是两种颜色。
pub fn icon_bg(
    p: &mullion_store::ProjectRecord,
    appearance: &crate::ui::badge::AppearanceCache,
    target: mullion_store::ColorTarget,
) -> Option<egui::Color32> {
    node_for(p)
        .and_then(|id| appearance.get(id))
        .and_then(|a| crate::ui::badge::should_paint(a, target))
}
```

- [ ] **Step 4: 跑测试确认它绿**

Run: `cargo test -p mullion-app --lib project::tests 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/project.rs
git commit -m "feat(app): 项目图标解析收成一个纯函数,未设时回落首选节点 (F238)"
```

---

### Task 3: 共享行画出图标

**Files:**
- Modify: `crates/mullion-app/src/ui/project_row.rs`
- Modify: `crates/mullion-app/src/ui/project_manager.rs:231`
- Modify: `crates/mullion-app/src/ui/launcher.rs:94`
- Modify: `crates/mullion-app/src/ui/project_pick.rs:151`

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/ui/project_row.rs` 的 `mod tests` 末尾加:

```rust
    /// F238:行上真的画出了那张图。
    ///
    /// 判据是「画面上出现了一张 `Shape::Image`」,不是「调用了 `paint_icon`」——
    /// 后者是读源码,换个函数名就恒绿。
    ///
    /// 自证会变红:把 `show` 里那段 `paint_icon` 删掉;或把 `ICON_SIDE`
    /// 改成 `0.0`(矩形退化,`paint_icon` 走降级不画)。
    #[test]
    fn a_row_paints_the_icon_it_was_given() {
        assert!(
            has_image(&row_shapes(Some(&test_ico()))),
            "给了图标却一张图都没画出来"
        );
        assert!(
            !has_image(&row_shapes(None)),
            "没给图标却凭空画了一张图"
        );
    }

    /// 有图标没图标的行,**文字左边界必须一样** —— 两种行混在一列里,
    /// 名字左右错开 30 点比缺一张图难看得多(同灯槽那条恒定判据)。
    ///
    /// 自证会变红:把 `show` 里的 `text_left` 改成
    /// `rect.left() + if row.icon.is_some() { TEXT_X } else { LAMP_X + 12.0 }`。
    #[test]
    fn the_text_starts_at_the_same_x_whether_or_not_there_is_an_icon() {
        let with = first_text_x(&row_shapes(Some(&test_ico())));
        let without = first_text_x(&row_shapes(None));
        assert_eq!(
            with, without,
            "有图标/没图标两种行的文字左边界不一样:{with:?} vs {without:?}"
        );
    }

    fn test_ico() -> mullion_store::IconSpec {
        // 一张 1x1 的真 ico:`paint_icon` 会先解码,解不开就整段不画,
        // 拿假 base64 的话这条测试会因为「解码失败」而假红。
        mullion_store::IconSpec {
            kind: mullion_store::IconKind::Ico,
            value: crate::ui::ico::import(&one_pixel_ico()).expect("测试用 ico 应能导入"),
            bg: None,
        }
    }

    /// 32x32 全不透明的 PNG 帧打包成的最小 ico。直接用 `ui::ico` 自己的
    /// 编码路径造 —— 手写字节表一改版本就烂掉。
    fn one_pixel_ico() -> Vec<u8> {
        crate::ui::ico::tests_support::solid_ico(32, [255, 0, 0, 255])
    }

    /// 跑两帧,返回这一行画出来的全部 shape。
    fn row_shapes(icon: Option<&mullion_store::IconSpec>) -> Vec<egui::epaint::ClippedShape> {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let p = proj(3, "接口", "/srv/api", None);
        let ss = vec![sess(7, "web01")];
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SCREEN_W, 400.0));
        let mut out = Vec::new();
        for _ in 0..2 {
            out = ctx
                .run(
                    egui::RawInput {
                        screen_rect: Some(screen),
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            show(
                                ui,
                                &t,
                                &Row {
                                    project: &p,
                                    lamp: crate::project::Lamp::Unknown,
                                    sessions: &ss,
                                    query: "",
                                    selected: false,
                                    now: now(),
                                    list: "test",
                                    icon,
                                    icon_bg: None,
                                },
                            );
                        });
                    },
                )
                .shapes;
        }
        out
    }

    fn has_image(shapes: &[egui::epaint::ClippedShape]) -> bool {
        fn walk(s: &egui::Shape) -> bool {
            match s {
                egui::Shape::Vec(v) => v.iter().any(walk),
                egui::Shape::Mesh(_) | egui::Shape::Rect(_) => false,
                egui::Shape::Text(_) => false,
                _ => matches!(s, egui::Shape::Mesh(_)) || matches!(s, egui::Shape::Rect(_)),
            }
        }
        let _ = walk;
        shapes.iter().any(|cs| contains_image(&cs.shape))
    }

    fn contains_image(s: &egui::Shape) -> bool {
        match s {
            egui::Shape::Vec(v) => v.iter().any(contains_image),
            egui::Shape::Mesh(m) => m.texture_id != egui::TextureId::default(),
            _ => false,
        }
    }

    /// 这一行第一段文字的左边界 x。
    fn first_text_x(shapes: &[egui::epaint::ClippedShape]) -> Option<u32> {
        fn walk(s: &egui::Shape, out: &mut Vec<f32>) {
            match s {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(ts) => out.push(ts.pos.x),
                _ => {}
            }
        }
        let mut xs = Vec::new();
        shapes.iter().for_each(|cs| walk(&cs.shape, &mut xs));
        // 时间列在最右,名称/副标题在左 —— 取最小的那个就是文字左边界。
        xs.into_iter()
            .map(|x| x.round() as u32)
            .min()
    }
```

> `has_image` 里那段 `walk` 是死代码,删掉它,只留 `contains_image` 一条路径。
> (写测试时直接写成下面这样:)

```rust
    fn has_image(shapes: &[egui::epaint::ClippedShape]) -> bool {
        shapes.iter().any(|cs| contains_image(&cs.shape))
    }
```

- [ ] **Step 2: 给 `ui::ico` 补一个测试用的造图工具**

`crates/mullion-app/src/ui/ico.rs` 末尾加(**不在 `#[cfg(test)]` 里**,因为
`project_row` 的测试是同 crate 的另一个模块,`cfg(test)` 下可见,故仍加
`#[cfg(test)]` —— 同 crate 的 `cfg(test)` 互相可见):

```rust
/// 只给同 crate 的测试用:造一张纯色的 `.ico` 原始字节。
///
/// 手写字节表的话,`ico` crate 一升版本就烂掉,而症状是别处的测试假红。
#[cfg(test)]
pub mod tests_support {
    /// `side` 边长的纯色方图,打包成一个单帧 ico。
    pub fn solid_ico(side: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut img = ico::IconImage::from_rgba_data(
            side,
            side,
            std::iter::repeat(rgba)
                .take((side * side) as usize)
                .flatten()
                .collect(),
        );
        let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
        dir.add_entry(ico::IconDirEntry::encode(&img).expect("单帧应能编码"));
        let mut out = Vec::new();
        dir.write(&mut out).expect("ico 应能写出");
        let _ = &mut img;
        out
    }
}
```

> 写代码时先 `grep -n "^use\|ico::" crates/mullion-app/src/ui/ico.rs` 看
> `ico` crate 的实际用法(`import` 里已经在用),照它的写法调,不要照抄上面
> 这段的 API 名。判据:`cargo test -p mullion-app --lib ui::ico` 全绿。

- [ ] **Step 3: 跑测试确认它红**

Run: `cargo test -p mullion-app --lib ui::project_row 2>&1 | tail -20`
Expected: FAIL —— `struct 'Row' has no field named 'icon'`

- [ ] **Step 4: 实现**

`crates/mullion-app/src/ui/project_row.rs`:

坐标常量整段替换(文件头 28-44 行那一段):

```rust
/// 灯的槽位中心距行左边缘(逻辑点)。
const LAMP_X: f32 = 14.0;
/// 图标槽左边缘距行左边缘。紧挨着灯槽右沿。
const ICON_X: f32 = 24.0;
/// 图标边长。走 F61 那套 32px 纹理档(`paint_icon` 按 `side <= 32` 选档),
/// 比 32 略小一点是为了在 48 点行高里上下留出呼吸。
const ICON_SIDE: f32 = 28.0;
/// 文字左边界 = 图标槽右沿 + 一点呼吸。**恒定**:图标是「有就画、没有就
/// 留空」的,有图标没图标的行文字左边界必须对齐(同灯槽那条理由)。
const TEXT_X: f32 = ICON_X + ICON_SIDE + 6.0;
/// 文字区距行右边缘的留白。
const TEXT_RIGHT_PAD: f32 = 8.0;
/// 名称行顶距行顶。
const NAME_TOP: f32 = 6.0;
/// 副标题行顶距行顶。
const SUB_TOP: f32 = 27.0;
/// 名称字号。
const NAME_SIZE: f32 = 14.0;
/// 副标题与时间字号。
const SUB_SIZE: f32 = 11.0;
/// 名称与时间列之间的最小间隙 —— 顶到一起会读成一个词。
const NAME_TIME_GAP: f32 = 8.0;
```

`Row` 结构体末尾补两个字段:

```rust
    /// F238:这一行的图标。由调用方用 [`crate::project::icon_for`] 解析好
    /// 传进来 —— 三处列表各解析一遍必然漂移。`None` = 项目没设、首选节点
    /// 也没有,槽位留空但**不收窄**(文字左边界恒定)。
    pub icon: Option<&'a mullion_store::IconSpec>,
    /// 图标底色。走 [`crate::project::icon_bg`],同源回落首选节点的节点色。
    pub icon_bg: Option<egui::Color32>,
```

`show` 里,在灯的 tooltip 那段之后、`let text_left = ...` 之前插入:

```rust
    // 图标(F238)。**画在灯之后、文字之前**:槽位固定,有就画、没有就空着。
    if let Some(icon) = row.icon {
        let slot = egui::Rect::from_center_size(
            egui::pos2(rect.left() + ICON_X + ICON_SIDE / 2.0, rect.center().y),
            egui::vec2(ICON_SIDE, ICON_SIDE),
        );
        crate::ui::badge::paint_icon(p, slot, icon, row.icon_bg);
    }
```

三处调用点各补两行(`crate::ui::badge::AppearanceCache` 从各自的 `show` 参数拿,
见 Step 5):

```rust
                                        icon: crate::project::icon_for(p, appearance),
                                        icon_bg: crate::project::icon_bg(
                                            p,
                                            appearance,
                                            mullion_store::ColorTarget::ListItem,
                                        ),
```

- [ ] **Step 5: 把 `AppearanceCache` 传到三处列表**

- `ui/project_manager.rs::show` 的参数表末尾加 `appearance: &crate::ui::badge::AppearanceCache,`
- `ui/launcher.rs::show` 同上
- `ui/project_pick.rs::show` 同上
- `ui/mod.rs` 的三处调用各补 `frame.appearance`(`UiFrame.appearance` 已存在,
  见 `ui/mod.rs:554`)
- 三个文件里既有的测试若直接调 `show`,补
  `&crate::ui::badge::AppearanceCache::default()`

- [ ] **Step 6: 修既有落点测试**

`ICON_X`/`TEXT_X` 变了,`project_row`/`launcher`/`project_pick`/`project_manager`
里按坐标点行的测试要跟着改。逐条跑,按报错改:

Run: `cargo test -p mullion-app --lib ui:: 2>&1 | grep -E "^test .* FAILED|test result"`
Expected: 全 `ok`。**不许**为了让它过而放松断言(比如把 `assert_eq!` 换成
`assert!(.. > 0.0)`)——落点测试的价值就在于精确。

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src/ui/project_row.rs crates/mullion-app/src/ui/project_manager.rs \
        crates/mullion-app/src/ui/launcher.rs crates/mullion-app/src/ui/project_pick.rs \
        crates/mullion-app/src/ui/ico.rs crates/mullion-app/src/ui/mod.rs
git commit -m "feat(app): 三处项目列表的共享行画出项目图标 (F238)"
```

---

### Task 4: pane 标题条用项目图标顶掉会话图标

**Files:**
- Modify: `crates/mullion-app/src/ui/pane_title.rs`
- Modify: `crates/mullion-app/src/app.rs:11031-11094`(`TitleView` 组装)

- [ ] **Step 1: 写失败的测试**

`crates/mullion-app/src/ui/pane_title.rs` 的 `mod tests` 末尾加:

```rust
    /// F238:pane 属于某个项目时,标题条画的是**项目**的图标,不是会话的。
    ///
    /// 理由与 `title_text` 里「项目名顶掉 tmux 名」逐字相同:用户此刻的
    /// 心智单位是项目,一台机器上开着三个项目时三块 pane 图标全一样,
    /// 那个图标就不承担任何区分职责了。
    ///
    /// 判据是「画出来的那张图的纹理与项目图标的一致」,不是「读了哪个字段」。
    ///
    /// 自证会变红:把 `icon_of` 里 `project_icon` 那一支去掉。
    #[test]
    fn a_pane_that_belongs_to_a_project_shows_the_project_icon() {
        let sess = ico("SESSION");
        let proj = ico("PROJECT");
        let a = crate::ui::badge::Appearance {
            icon: Some(sess.clone()),
            color: None,
        };
        assert_eq!(
            icon_of(Some(&a), Some(&proj)).map(|i| i.value.as_str()),
            Some("PROJECT"),
            "属于项目时该用项目图标"
        );
        assert_eq!(
            icon_of(Some(&a), None).map(|i| i.value.as_str()),
            Some("SESSION"),
            "不属于任何项目时仍用会话图标"
        );
        assert!(icon_of(None, None).is_none());
    }

    fn ico(v: &str) -> mullion_store::IconSpec {
        mullion_store::IconSpec {
            kind: mullion_store::IconKind::Ico,
            value: v.into(),
            bg: None,
        }
    }
```

- [ ] **Step 2: 跑测试确认它红**

Run: `cargo test -p mullion-app --lib ui::pane_title 2>&1 | tail -20`
Expected: FAIL —— `cannot find function 'icon_of'`

- [ ] **Step 3: 实现**

`ui/pane_title.rs`,`TitleView` 结构体加一个字段:

```rust
    /// F238:这块 pane 所属项目的图标(已由 `app.rs` 用
    /// `crate::project::icon_for` 解析好)。`None` = 不属于任何项目。
    pub project_icon: Option<&'a mullion_store::IconSpec>,
```

紧挨着 `icon_side` 加纯函数:

```rust
/// 这块 pane 的标题条该画哪张图标。
///
/// 属于项目就用项目的 —— 同一台机器上开三个项目时,三块 pane 的会话图标
/// 完全一样,那张图就不承担任何区分职责了。与 `title_text` 里「项目名顶掉
/// tmux 名」是同一条判断,两处**必须同向**:一边写着项目名、一边画着会话
/// 图标,用户读不出这块 pane 到底是什么。
pub fn icon_of<'a>(
    appearance: Option<&'a crate::ui::badge::Appearance>,
    project_icon: Option<&'a mullion_store::IconSpec>,
) -> Option<&'a mullion_store::IconSpec> {
    project_icon.or_else(|| appearance.and_then(|a| a.icon.as_ref()))
}
```

`pane_title.rs:418` 那处 `paint_icon` 的图标实参换成
`icon_of(v.appearance, v.project_icon)` 的结果(保持原来的 `if let Some(icon) = ..` 形状)。

`app.rs` 的 `TitleView` 组装处(`project:` 那一行旁边)补:

```rust
                    project_icon: crate::project::project_of(
                        ws.pane(g.id).and_then(|p| p.tmux.as_deref()),
                        projects_now,
                    )
                    .and_then(|p| crate::project::icon_for(p, &self.appearance)),
```

> `project:` 那一行已经算过一次 `project_of`。**把它提成一个 `let` 绑定再用两次**,
> 不要调两遍 —— 两次调用之间没有任何东西会变,但读代码的人会以为有。

- [ ] **Step 4: 跑测试确认它绿**

Run: `cargo test -p mullion-app --lib ui::pane_title 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/pane_title.rs crates/mullion-app/src/app.rs
git commit -m "feat(app): pane 标题条属于项目时用项目图标 (F238)"
```

---

### Task 5: 项目管理器右栏「外观」分节

**Files:**
- Modify: `crates/mullion-app/src/ui/mod.rs`(`icon_target` 字段)
- Modify: `crates/mullion-app/src/ui/project_manager.rs`(`form_column`)
- Modify: `crates/mullion-app/src/app.rs:9899`(`IconPathPicked`)

- [ ] **Step 1: 写失败的测试**

`ui/project_manager.rs` 的 `mod tests` 末尾加:

```rust
    /// F238:右栏有一个「外观」分节,能导入 .ico。
    ///
    /// 判据是**画面上的字**,不是「调了哪个函数」。
    ///
    /// 自证会变红:把 `appearance_section` 的调用注释掉。
    #[test]
    fn the_form_has_an_appearance_section_with_an_icon_import_button() {
        let texts = form_texts_with_selection();
        assert!(
            texts.iter().any(|s| s == "外观"),
            "右栏没有「外观」分节:{texts:?}"
        );
        assert!(
            texts.iter().any(|s| s.contains("导入 .ico")),
            "右栏没有导入 .ico 的入口:{texts:?}"
        );
    }

    /// 已经设了图标才有「清除」—— 没设时摆一颗按不动的按钮,用户会以为
    /// 自己漏看了什么(同会话侧「外观」页的判据)。
    ///
    /// 自证会变红:把 `has_icon` 那个条件去掉,让「清除」无条件出现。
    #[test]
    fn the_clear_button_only_shows_up_once_an_icon_is_set() {
        assert!(
            !form_texts_with_selection().iter().any(|s| s == "清除"),
            "还没设图标就摆出了「清除」"
        );
        let texts = form_texts_with_icon();
        assert!(
            texts.iter().any(|s| s == "清除"),
            "设了图标却没有「清除」:{texts:?}"
        );
    }
```

> `form_texts_with_selection()` / `form_texts_with_icon()`:照抄本文件既有的
> `form_texts_on_a_short_screen()`,把屏幕高度换成 900.0、并在
> `ui_state.project_draft` 里分别放「没图标的草稿」和「有图标的草稿」。
> 既有那个 helper 的实现直接读源码复用,不要重写一份取文字的逻辑。

- [ ] **Step 2: 跑测试确认它红**

Run: `cargo test -p mullion-app --lib ui::project_manager 2>&1 | tail -20`
Expected: FAIL —— 断言失败,`右栏没有「外观」分节`

- [ ] **Step 3: 实现**

`ui/mod.rs`,`UiState` 里 `icon_error` 旁边加:

```rust
    /// F238:文件对话框选完 `.ico` 之后,那份正文该落到哪。
    ///
    /// **一个 flag 两个去处**:会话编辑器和项目管理器共用
    /// `pick_icon_request` 与系统文件框那条路(`picker_busy.icon`),各开
    /// 一条的话「同时开着两个窗」时两条路会互相盖掉 `picker_busy`。
    pub icon_target: IconTarget,
```

同文件里加:

```rust
/// [`UiState::icon_target`] 的取值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IconTarget {
    /// 会话编辑器的「外观」页(F61)。
    #[default]
    Session,
    /// 项目管理器右栏的「外观」分节(F238)。
    Project,
}
```

`ui/project_manager.rs`,`form_column` 里「基本」分节之后、「节点」分节之前插入:

```rust
    appearance_section(ui, t, draft, ui_state, &mut first);
```

并加函数(照 `session_manager::fields::appearance` 的形状,**不复用它** ——
那个函数吃的是 `EditorBuffer`,项目这边没有那个结构):

```rust
/// F238:项目图标。与会话编辑器的「外观」分节同构 —— 两处长得不一样的话,
/// 用户会以为项目的图标是另一种东西。
fn appearance_section(
    ui: &mut egui::Ui,
    t: &Theme,
    draft: &mut mullion_store::ProjectRecord,
    ui_state_icon_error: &mut Option<String>,
    pick_clicked: &mut bool,
    first: &mut bool,
) {
    crate::ui::form::section(ui, t, "项目管理器/右栏", "外观", first);
    crate::ui::form::grid(ui, "pm_appearance", |ui| {
        ui.label("图标");
        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("导入 .ico…").clicked() {
                    *pick_clicked = true;
                }
                if draft.icon.is_some() && ui.button("清除").clicked() {
                    draft.icon = None;
                    *ui_state_icon_error = None;
                }
            });
            if let Some(e) = ui_state_icon_error.as_deref() {
                ui.colored_label(crate::theme::c32(t.danger_text), e);
            }
            if let Some(icon) = draft.icon.as_ref() {
                let side = crate::ui::ico::SMALL as f32;
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
                crate::ui::badge::paint_icon(ui.painter(), rect, icon, None);
            } else {
                // 没设时说清回落到哪 —— 一片空白会被读成「这里坏了」。
                ui.colored_label(
                    crate::theme::c32(t.fg_muted),
                    "没设:跟首选节点的图标走",
                );
            }
        });
        ui.end_row();
    });
}
```

> `crate::ui::form::section` / `grid` 的真实路径按
> `grep -n "fn section\|fn grid" crates/mullion-app/src/ui/` 的结果填 ——
> `session_manager::fields` 里用的是同名的两个,照它的 `use` 写。
> 参数按借用检查实际需要拆(`ui_state` 整体传进去会跟 `draft` 撞借用,
> 所以上面拆成了三个 `&mut`)。

`show` 里在窗口闭包**之外**(借用释放之后)加:

```rust
    // 「导入 .ico…」被点了 → 转成 `pick_icon_request`,由 `app.rs` 事后
    // 另起线程开系统文件框(不能在 egui 闭包里同步阻塞)。同会话编辑器。
    if std::mem::take(&mut pick_icon_clicked) {
        ui_state.icon_target = crate::ui::IconTarget::Project;
        ui_state.pick_icon_request = true;
    }
```

`ui/session_manager/mod.rs:879` 那处一并补一行,把归属写明:

```rust
        if std::mem::take(&mut buf.pick_icon_clicked) {
            ui_state.icon_target = crate::ui::IconTarget::Session;
            ui_state.pick_icon_request = true;
        }
```

`app.rs:9899` 的 `IconPathPicked` 改成按归属分流:

```rust
            UserEvent::IconPathPicked(picked) => {
                self.picker_busy.icon = false;
                if let Some(p) = picked {
                    match self.ui.icon_target {
                        crate::ui::IconTarget::Session => {
                            if let Some(buf) = self.ui.editor.as_mut() {
                                self.ui.icon_error =
                                    crate::ui::session_manager::import_icon_file(buf, &p, |p| {
                                        std::fs::read(p)
                                    })
                                    .err();
                            }
                        }
                        // F238:项目草稿。**只写草稿不落盘** —— 落盘是「保存」
                        // 那颗按钮的事,在这里写等于绕开了 `validate_project`。
                        crate::ui::IconTarget::Project => {
                            if let Some(d) = self.ui.project_draft.as_mut() {
                                self.ui.icon_error = match std::fs::read(&p) {
                                    Err(e) => Some(format!("读不了 {}:{e}", p.display())),
                                    Ok(bytes) => match crate::ui::ico::import(&bytes) {
                                        Ok(b64) => {
                                            d.icon = Some(mullion_store::IconSpec {
                                                kind: mullion_store::IconKind::Ico,
                                                value: b64,
                                                bg: None,
                                            });
                                            None
                                        }
                                        Err(e) => Some(e.message()),
                                    },
                                };
                            }
                        }
                    }
                }
                self.request_ui_redraw();
            }
```

- [ ] **Step 4: 跑测试确认它绿**

Run: `cargo test -p mullion-app --lib ui::project_manager 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`

- [ ] **Step 5: 字形白名单 + 表单规范**

Run: `cargo test -p mullion-app --test glyph_whitelist --test form_guidelines 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.` ×2。红了就按它报的那个字符/那条规范改文案
(**不要**往 `VERIFIED` 里塞 GBK 外的字形)。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/ui/mod.rs crates/mullion-app/src/ui/project_manager.rs \
        crates/mullion-app/src/ui/session_manager/mod.rs crates/mullion-app/src/app.rs
git commit -m "feat(app): 项目管理器右栏可导入项目图标 (F238)"
```

---

# Part B —— F239 弹窗点外面即关

### Task 6: `ui/dismiss.rs` —— 判定

**Files:**
- Create: `crates/mullion-app/src/ui/dismiss.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs`(加 `pub mod dismiss;`)

- [ ] **Step 1: 写失败的测试(先写文件,测试在文件里)**

新建 `crates/mullion-app/src/ui/dismiss.rs`,先只写 `mod tests`:

```rust
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
    /// 自证会变红:把 `pick` 的 `find` 换成 `position` 之后再对全表判一遍
    /// (即遍历所有开着的候选各关各的)。
    #[test]
    fn only_the_topmost_dialog_is_closed_by_one_click() {
        // 上层开着且被点在外面,下层也开着 —— 只有 0 号该关。
        assert_eq!(
            pick(&[c(Some(Where::Outside), false), c(Some(Where::Outside), false)]),
            Some(0)
        );
    }

    /// 最上层那个脏了,**不许穿透**去关下层的。
    ///
    /// 自证会变红:让 `pick` 在 top 脏的时候接着往下找。
    #[test]
    fn a_dirty_top_dialog_does_not_let_the_click_fall_through() {
        assert_eq!(
            pick(&[c(Some(Where::Outside), true), c(Some(Where::Outside), false)]),
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

    /// 点在下拉菜单/tooltip 上 —— 那是弹窗的延伸,不判。
    ///
    /// 少了这条,会话管理器里点一下任何一个下拉框的选项就把整个窗关掉,
    /// 而那是个每天都要用到的操作。
    ///
    /// 自证会变红:把 `locate` 里 `Order::Foreground` 那一支改成 `Outside`。
    #[test]
    fn clicking_a_popup_belonging_to_the_dialog_decides_nothing() {
        assert_eq!(pick(&[c(Some(Where::Undecided), false)]), None);
    }
}
```

- [ ] **Step 2: 跑测试确认它红**

Run: `cargo test -p mullion-app --lib ui::dismiss 2>&1 | tail -20`
Expected: FAIL —— `cannot find type 'Where'`(先在 `ui/mod.rs` 里
`pub mod dismiss;`,否则是「找不到模块」)

- [ ] **Step 3: 实现**

`crates/mullion-app/src/ui/dismiss.rs` 顶部(在 `mod tests` 之前):

```rust
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

/// `pos` 处这一下,相对 `area` 这个弹窗落在哪。
///
/// `area` 是弹窗的 egui area id:`egui::Window::new(t)` 恒为 `Id::new(t)`
/// (`egui-0.30.0/src/containers/window.rs:56`),`egui::Area::new(id)` 就是
/// `id` 本身。
pub fn locate(ctx: &egui::Context, area: egui::Id, pos: egui::Pos2) -> Where {
    match ctx.layer_id_at(pos) {
        // 底下什么 area 都没有 = 点在面板上(菜单栏/标签栏/状态栏)或者
        // 终端上。两者都算「外面」。
        None => Where::Outside,
        Some(l) if l.id == area => Where::Inside,
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
```

`ui/mod.rs` 的模块声明区加 `pub mod dismiss;`(按字母序插进既有那一串)。

- [ ] **Step 4: 跑测试确认它绿**

Run: `cargo test -p mullion-app --lib ui::dismiss 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`,7 passed

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/dismiss.rs crates/mullion-app/src/ui/mod.rs
git commit -m "feat(app): 点外面关弹窗的判定收成纯函数 (F239)"
```

---

### Task 7: 候选表与三张穷尽 match

**Files:**
- Modify: `crates/mullion-app/src/app.rs`
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs`(`WINDOW_TITLE`、`is_dirty` 转 `pub`)
- Modify: `crates/mullion-app/src/ui/files_dialog.rs`(`title_of`)
- Modify: `crates/mullion-app/src/ui/tab_props.rs`(`is_dirty`)
- Modify: `crates/mullion-app/src/ui/import_dialog.rs`(`ImportState.picked0`)
- Modify: `crates/mullion-app/src/ui/rehost.rs` / `ui/project_pick.rs`(`area_id` 提可见性)

- [ ] **Step 1: 写失败的测试**

`app.rs` 的 `mod tests` 里加:

```rust
    /// F239:`DISMISS_ORDER` 必须恰好覆盖「非豁免」的每一个弹窗,一个不多
    /// 一个不少。
    ///
    /// 这是本切片的**完备性闸门**。新加一种弹窗时,`dismiss_area` 的
    /// 穷尽 match 会逼作者表态(编译不过),但表了态之后忘了往
    /// `DISMISS_ORDER` 里加一行,编译照样过 —— 症状是那个新弹窗点外面
    /// 关不掉,而且完全静默。切片 I 的教训(「弹窗要同时进两张表」)
    /// 这里是第三次踩。
    ///
    /// 自证会变红:从 `DISMISS_ORDER` 里删掉任意一行。
    #[test]
    fn every_dialog_is_either_in_the_dismiss_order_or_explicitly_exempt() {
        let app = test_app();
        let exempt: Vec<Modal> = Modal::ALL
            .iter()
            .copied()
            .filter(|m| DISMISS_EXEMPT.contains(m))
            .collect();
        for m in Modal::ALL {
            let in_order = DISMISS_ORDER.contains(m);
            let is_exempt = exempt.contains(m);
            assert!(
                in_order != is_exempt,
                "{m:?} 既不在 DISMISS_ORDER 里也不在豁免表里(或两张表都进了)"
            );
        }
        assert_eq!(
            DISMISS_ORDER.len() + exempt.len(),
            Modal::ALL.len(),
            "两张表加起来必须等于弹窗总数"
        );
        // 顺带钉住:豁免表里那几个,`dismiss_area` 必须恒 `None`。
        for m in &exempt {
            assert!(
                app.dismiss_area(*m).is_none(),
                "{m:?} 在豁免表里,却报出了一个可判定的 area"
            );
        }
    }

    /// F239:豁免的是哪四类,写死在这里。
    ///
    /// **不是**「实现是什么就断言什么」——这四类各有各的理由,任何一个被
    /// 顺手挪出豁免表都会造成真实损失:主密码框关掉 = 回到无库状态;
    /// TOFU 框关掉 = 一次必须显式回答的安全判断被逃掉;编辑器关掉 =
    /// 未回传的远端文件正文没了;三个就地输入框根本不是窗口,没有「外面」。
    /// 粘贴确认框走 `egui::Modal` 自带的遮罩点击(`ui/paste.rs:87`),
    /// 不在这里重复接。
    ///
    /// 自证会变红:把任意一个变体从 `DISMISS_EXEMPT` 里挪走。
    #[test]
    fn the_exemptions_are_exactly_the_four_that_would_lose_something() {
        let want = [
            Modal::Unlock,
            Modal::HostKey,
            Modal::Editor,
            Modal::FilesPathEdit,
            Modal::FilesRename,
            Modal::FilesNewName,
            Modal::Paste,
        ];
        for m in want {
            assert!(DISMISS_EXEMPT.contains(&m), "{m:?} 应该豁免");
        }
        assert_eq!(DISMISS_EXEMPT.len(), want.len());
    }
```

> `test_app()`:本文件的 `mod tests` 里已经有一个造 `App` 的 helper
> (`grep -n "fn .*-> App\b" crates/mullion-app/src/app.rs` 找它)。复用,
> 不要新建。找不到就用既有测试里造 `App` 的那几行原样搬成 helper。

`ui/files_dialog.rs` 的 `mod tests` 里加:

```rust
    /// F239:`title_of` 报的标题必须真的是这一帧画出来的那个窗口的 id。
    ///
    /// 两处各写一遍标题字符串,漂了的话点外面永远关不掉那个框,**完全静默**
    /// (`area_rect` 查不到就一律不判)。所以判据是「按 `title_of` 算出来的
    /// area 这一帧确实存在」,不是「两个字符串常量相等」。
    ///
    /// 自证会变红:给 `title_of` 的任意一臂末尾加一个空格。
    #[test]
    fn the_title_we_report_is_the_window_we_actually_draw() {
        for d in every_dialog_variant() {
            let ctx = egui::Context::default();
            let mut open = Some(d.clone());
            for _ in 0..2 {
                let _ = ctx.run(Default::default(), |ctx| {
                    let _ = show(ctx, &crate::theme::MULLION_DARK, &mut open);
                });
            }
            let id = egui::Id::new(title_of(&d));
            assert!(
                ctx.memory(|m| m.area_rect(id)).is_some(),
                "{d:?} 的标题报成了 {:?},但这一帧根本没有这个窗口",
                title_of(&d)
            );
        }
    }

    /// 六个变体各造一个。**逐个列出**而不是从 `Default` 推 —— 漏一个的话
    /// 上面那条测试就少罩一个框。
    fn every_dialog_variant() -> Vec<FilesDialog> { /* 按 FilesDialog 的实际
        变体逐个构造,字段填最小合法值 */ }
```

- [ ] **Step 2: 跑测试确认它红**

Run: `cargo test -p mullion-app --lib dismiss 2>&1 | tail -20`
Expected: FAIL —— `cannot find value 'DISMISS_ORDER'`

- [ ] **Step 3: 提可见性 / 补小函数**

1. `ui/session_manager/mod.rs:232`:`const WINDOW_TITLE` → `pub const WINDOW_TITLE`
2. 同文件 `pub(crate) use buffer::{...}` 那一串里把 `is_dirty` 挪到 `pub use buffer::{...}` 那一串(若它当前不在导出里,加进 `pub use`)
3. `ui/rehost.rs:29` 与 `ui/project_pick.rs:27`:`pub(super) fn area_id` → `pub(crate) fn area_id`
4. `ui/files_dialog.rs` 加:

```rust
/// F239:当前开着的是哪个框 —— 它的窗口标题,也就是它的 egui area id。
///
/// **必须与 `show` 里传给 `modal()` 的那个字符串逐字一致**:两处漂了的话,
/// 点外面永远关不掉这个框,而且完全静默。守护见
/// `the_title_we_report_is_the_window_we_actually_draw`。
pub fn title_of(d: &FilesDialog) -> &'static str {
    match d {
        FilesDialog::Delete { .. } => "删除",
        // …其余五个变体照 `show` 里各臂 `modal(ctx, "…", ..)` 的第二个实参
        // 逐字抄过来。
    }
}
```

5. `ui/tab_props.rs` 加:

```rust
/// F239:草稿相对这个标签**当前**的两个覆盖字段脏不脏。
///
/// 走快照比对而不是手工打脏标记(F37 的教训):改了又改回来,不算脏。
pub fn is_dirty(
    d: &TabPropsDraft,
    title_override: Option<&str>,
    color_override: Option<Rgb>,
) -> bool {
    d.name.trim() != title_override.unwrap_or_default() || to_rgb(d.color) != color_override
}
```

> `to_rgb`:本文件里已经有一处把 `Option<egui::Color32>` 转成 `Option<Rgb>`
> 的代码(`TabPropsAction::Save { color }` 那里)。**把它提成 `fn to_rgb`
> 再两处共用**,不要复制一份 —— 两份转换漂了的话「颜色改了却判不脏」。

6. `ui/import_dialog.rs`:`ImportState` 加一个字段并在两处构造点填上:

```rust
    /// F239:刚打开时每一行的勾选状态。用来判「用户动过没有」——
    /// 走快照比对,不打脏标记(勾了又勾回来,不算脏)。
    pub picked0: Vec<bool>,
```

`app.rs:9887` 与 `ui/import_dialog.rs:428` 两处构造点各加一行
(在 `rows` 算出来之后):

```rust
    let rows = crate::ui::import_dialog::build_rows(&parsed, existing);
    let picked0 = rows.iter().map(|r| r.selected).collect();
```

- [ ] **Step 4: `app.rs` 里加三张表 + 两个常量**

`Modal` 的 derive 补齐(现在是 `#[derive(Debug, Clone, Copy, PartialEq, Eq)]`,
少哪个补哪个;`DISMISS_ORDER.contains` 要 `PartialEq`)。

在 `impl Modal` 之后加:

```rust
/// F239:能「点外面关掉」的弹窗,**从上到下**排。
///
/// 与 `ui::build_ui` 的绘制顺序严格互逆(后画的盖在上面,见 `ui/mod.rs`
/// 883~1036 那一段的注释)。这张表决定「一次点击关谁」,排错了的症状是
/// 点一下关掉了底下那个、上面那个还杵着。
const DISMISS_ORDER: &[Modal] = &[
    Modal::ExitConfirm,
    Modal::ProjectPick,
    Modal::Rehost,
    Modal::TabProps,
    Modal::FilesDialog,
    Modal::Import,
    Modal::ProjectTakeoverConfirm,
    Modal::ProjectOpenConfirm,
    Modal::ProjectManager,
    Modal::GroupManager,
    Modal::SessionManager,
    Modal::History,
    Modal::Settings,
    Modal::About,
];

/// F239:**不**响应「点外面」的弹窗。每一条都有具体损失,不是「懒得接」。
///
/// - `Unlock`:关掉 = 回到无库状态,而用户没有别的路把它叫回来。
/// - `HostKey`:TOFU 是一次必须**显式**回答的安全判断,不能被误点逃掉。
/// - `Editor`:里面是还没传回远端的文件正文。
/// - `FilesPathEdit`/`FilesRename`/`FilesNewName`:就地输入框,不是窗口,
///   没有「外面」可言(它们的 area 就是文件面板本身)。
/// - `Paste`:走 `egui::Modal` 自带的遮罩点击(`ui/paste.rs:87`),
///   在这里再接一遍等于同一个行为两条路。
const DISMISS_EXEMPT: &[Modal] = &[
    Modal::Unlock,
    Modal::HostKey,
    Modal::Editor,
    Modal::FilesPathEdit,
    Modal::FilesRename,
    Modal::FilesNewName,
    Modal::Paste,
];
```

`impl App` 里加三个方法:

```rust
    /// F239:这个弹窗这一帧的 egui area id。`None` = 没开着,或豁免。
    ///
    /// **穷尽 match**:新加一种弹窗时编译不过,作者必须表态。
    fn dismiss_area(&self, m: Modal) -> Option<egui::Id> {
        fn w(title: &str) -> egui::Id {
            // `egui::Window::new(t)` 的 area id 恒为 `Id::new(t)`
            // (egui-0.30.0/src/containers/window.rs:56)。
            egui::Id::new(title)
        }
        match m {
            Modal::Unlock
            | Modal::HostKey
            | Modal::Editor
            | Modal::FilesPathEdit
            | Modal::FilesRename
            | Modal::FilesNewName
            | Modal::Paste => None,
            Modal::About => self.ui.about_open.then(|| w("关于")),
            Modal::Settings => self.ui.settings_open.then(|| w("设置")),
            Modal::History => self.ui.history.is_some().then(|| w("恢复上次的现场")),
            Modal::SessionManager => self
                .ui
                .session_manager_open
                .then(|| w(crate::ui::session_manager::WINDOW_TITLE)),
            Modal::GroupManager => self.ui.group_manager_open.then(|| w("分组管理")),
            Modal::ProjectManager => self.ui.project_manager_open.then(|| w("项目管理")),
            Modal::ProjectOpenConfirm => self
                .ui
                .project_open_confirm
                .is_some()
                .then(|| w("打开项目前先确认")),
            Modal::ProjectTakeoverConfirm => self
                .ui
                .project_takeover
                .is_some()
                .then(|| w("项目已在别处打开")),
            Modal::Import => self.ui.import.is_some().then(|| w("导入 ssh config")),
            Modal::FilesDialog => self
                .ui
                .files_dialog
                .as_ref()
                .map(|d| w(crate::ui::files_dialog::title_of(d))),
            Modal::TabProps => self.ui.tab_props.is_some().then(|| w("标签属性")),
            Modal::ExitConfirm => self.ui.exit_pending.then(|| w("还有改动没传回远端")),
            Modal::Rehost => self
                .ui
                .rehost
                .as_ref()
                .map(|d| crate::ui::rehost::area_id(d.pane)),
            Modal::ProjectPick => self
                .ui
                .project_pick
                .as_ref()
                .map(|d| crate::ui::project_pick::area_id(d.pane)),
        }
    }

    /// F239:这个弹窗有没有未保存的改动。
    ///
    /// 一律**快照比对**(草稿 vs 原记录),不打脏标记 —— F37 那条教训:
    /// 手工标记必然漏一个赋值点,而漏了的症状是「改了却判不脏」,
    /// 一次误点就把改动清了。改回原样不算脏,这是快照比对白送的。
    ///
    /// 换节点/切项目弹窗恒**不脏**:它们没有任何要保存的数据(只有一个
    /// 搜索词,丢了零成本),点外面永远能关掉。
    fn dismiss_dirty(&self, m: Modal) -> bool {
        match m {
            Modal::SessionManager => {
                match (self.ui.editor.as_ref(), self.ui.editor_baseline.as_ref()) {
                    (Some(e), Some(b)) => crate::ui::session_manager::is_dirty(e, b),
                    _ => false,
                }
            }
            // 设置是实时预览的:草稿一旦生效就写进了 `self.settings`,
            // 「打开那一刻的备份」才是原记录。
            Modal::Settings => self
                .settings_backup
                .as_ref()
                .is_some_and(|b| *b != self.settings),
            Modal::ProjectManager => {
                match (self.ui.project_draft.as_ref(), self.ui.project_selected) {
                    (Some(d), Some(id)) => {
                        let stored = self
                            .store
                            .as_ref()
                            .map_or(&[][..], |s| s.projects())
                            .iter()
                            .find(|p| p.id == id);
                        stored != Some(d)
                    }
                    _ => false,
                }
            }
            // 分组管理器唯一的草稿就是那个「新建分组」输入框。
            Modal::GroupManager => !self.ui.group_name_buf.trim().is_empty(),
            Modal::TabProps => self.ui.tab_props.as_ref().is_some_and(|d| {
                self.tabs.iter().find(|t| t.id == d.tab_id).is_some_and(|t| {
                    crate::ui::tab_props::is_dirty(
                        d,
                        t.title_override.as_deref(),
                        t.color_override,
                    )
                })
            }),
            Modal::Import => self.ui.import.as_ref().is_some_and(|st| {
                st.rows.len() != st.picked0.len()
                    || st
                        .rows
                        .iter()
                        .zip(&st.picked0)
                        .any(|(r, was)| r.selected != *was)
            }),
            _ => false,
        }
    }

    /// F239:把这个弹窗按它**既有的**「取消 / 不恢复 / 关闭」出口关掉。
    ///
    /// **不新增出口变体**:每一条都复用那个弹窗自己的取消路径,否则
    /// 「点 × 关」和「点外面关」会变成两种语义,而差别只在某些状态没清。
    fn dismiss(&mut self, m: Modal) {
        match m {
            Modal::About => self.ui.about_open = false,
            // 走 `Cancel`:它要把 `settings_backup` 倒回去,只置 false
            // 的话实时预览过的字体/日志档就永久留下了。
            Modal::Settings => {
                self.apply_settings_action(crate::ui::settings::SettingsOut::Cancel)
            }
            // `HistoryOut::Dismiss` 的施加就是「清掉草稿、什么都不恢复」。
            Modal::History => self.ui.history = None,
            // 必须走 `close_session_manager`:它顺带清 `pending_delete` /
            // 在途拨测 / `probe_form` 里那份明文凭据副本。
            Modal::SessionManager => self.ui.close_session_manager(),
            Modal::GroupManager => self.ui.group_manager_open = false,
            Modal::ProjectManager => self.ui.project_manager_open = false,
            Modal::ProjectOpenConfirm => self.ui.project_open_confirm = None,
            Modal::ProjectTakeoverConfirm => self.ui.project_takeover = None,
            Modal::Import => self.ui.import = None,
            // 文件写操作确认框的「取消」按框而异(`cancel_op`),不能只
            // 清成 `None` —— 有的框取消时要撤掉一个已经发出去的意图。
            Modal::FilesDialog => {
                if let Some(op) = self
                    .ui
                    .files_dialog
                    .as_ref()
                    .and_then(crate::ui::files_dialog::cancel_op)
                {
                    self.apply_file_op(op);
                }
                self.ui.files_dialog = None;
            }
            Modal::TabProps => self.ui.tab_props = None,
            Modal::ExitConfirm => self.ui.exit_pending = false,
            Modal::Rehost => self.ui.rehost = None,
            Modal::ProjectPick => self.ui.project_pick = None,
            Modal::Unlock
            | Modal::HostKey
            | Modal::Editor
            | Modal::FilesPathEdit
            | Modal::FilesRename
            | Modal::FilesNewName
            | Modal::Paste => {}
        }
        mark_ui_dirty!(self.ui_dirty);
    }
```

> `cancel_op` 现在是 `ui/files_dialog.rs` 里的私有函数,签名是
> `fn cancel_op(d: &FilesDialog) -> Option<FileOp>`。提成 `pub(crate)`。
> `apply_file_op`:按 `grep -n "actions.files_op" crates/mullion-app/src/app.rs`
> 找到既有的施加点,把那段抽成一个方法再两处共用 —— **不要**复制一份。

- [ ] **Step 5: 跑测试确认它绿**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`

- [ ] **Step 6: 提交**

```bash
git add -A crates/mullion-app/src
git commit -m "feat(app): F239 的候选表与三张穷尽 match,复用各弹窗既有的取消出口 (F239)"
```

---

### Task 8: 接线到 `window_event`

**Files:**
- Modify: `crates/mullion-app/src/app.rs:10197-10221`

- [ ] **Step 1: 写失败的测试**

`app.rs` 的 `mod tests` 里加:

```rust
    /// F239:判在**指针按下**那一刻,不判松开。
    ///
    /// 松开判的话,设置里按住滑块一路拖到窗口外面再松手,会把整个设置窗
    /// 关掉 —— 而拖滑块出界是个再正常不过的动作。
    ///
    /// 自证会变红:把 `dismiss_on_outside_press` 里的
    /// `ElementState::Pressed` 改成 `ElementState::Released`。
    #[test]
    fn the_verdict_is_taken_on_press_not_on_release() {
        assert!(!press_closes(ElementState::Released));
        assert!(press_closes(ElementState::Pressed));
    }

    /// F239:这一下被吃掉了,**不会**接着落到终端上。
    ///
    /// 不吃的话:弹窗在这一帧关掉 → 下面那段分流重新算 `modal_open()` 得
    /// `false` → 同一下点击被判给终端,于是「关掉弹窗」顺带在终端里起了
    /// 一段划选(甚至发出一次鼠标上报)。
    ///
    /// 自证会变红:把 `window_event` 里那个 `if self.dismiss_on_outside_press(&event) { return; }`
    /// 的 `return` 去掉。
    #[test]
    fn the_click_that_closes_a_dialog_does_not_also_reach_the_terminal() {
        // 判据:`dismiss_on_outside_press` 返回 `true`,而 `window_event`
        // 在它为真时立刻 `return`(源码切片不算数 —— 这里用行为判:
        // 关掉之后终端的选区仍是 `None`)。
        // 具体构造见实现时补:造一个开着「关于」窗的 App,喂一次落在窗外
        // 的 MouseInput::Pressed,断言 `about_open == false` 且焦点 pane
        // 的选区仍为 `None`。
    }
```

> 第二条测试的正文在实现时补实。**不许留成空壳** —— 若造不出带终端的
> `App`(需要真实 SSH 连接),就改成钉住「`dismiss_on_outside_press`
> 返回 `true` 时 `self.dragging` / `self.press_anchor` 没有被写过」这个
> 可测的等价判据,并把理由写进注释。

- [ ] **Step 2: 跑测试确认它红**

Run: `cargo test -p mullion-app --lib dismiss_on_outside 2>&1 | tail -20`
Expected: FAIL —— `cannot find function 'dismiss_on_outside_press'`

- [ ] **Step 3: 实现**

`impl App` 里加(紧挨着 `files_hotkey_event`):

```rust
    /// F239:一次指针按下,如果落在最上层那个弹窗外面就把它关掉。
    ///
    /// 返回 `true` = 这一下已经被这条路径吃掉,调用方必须立刻 `return`。
    /// 不吃的话,弹窗在这一帧关掉之后,下面那段分流会重新算 `modal_open()`
    /// 得 `false`,同一下点击被判给终端 —— 「关掉弹窗」会顺带在终端里起一段
    /// 划选。
    ///
    /// **判按下不判松开**:松开判的话,在设置里按住滑块一路拖出窗口再松手
    /// 会把整个设置窗关掉,而那是个正常操作。
    ///
    /// 位置在输入分流**之前**(同标签/文件/焦点三个快捷键,T8):走到下面
    /// 就已经晚了 —— 那时这一下要么被 egui 收走、要么被编码进 PTY。
    fn dismiss_on_outside_press(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::MouseInput { state, button, .. } = event else {
            return false;
        };
        if *state != ElementState::Pressed || *button != winit::event::MouseButton::Left {
            return false;
        }
        let Some(ctx) = self.active.as_ref().map(|a| a.egui_ctx.clone()) else {
            return false;
        };
        // 光标位置从像素换成 egui 的逻辑点(同 `rehost_rect` 的换算)。
        let ppp = ctx.pixels_per_point();
        let pos = egui::pos2(self.cursor_px.0 / ppp, self.cursor_px.1 / ppp);
        let candidates: Vec<crate::ui::dismiss::Candidate> = DISMISS_ORDER
            .iter()
            .map(|m| crate::ui::dismiss::Candidate {
                hit: self
                    .dismiss_area(*m)
                    .map(|id| crate::ui::dismiss::locate(&ctx, id, pos)),
                dirty: self.dismiss_dirty(*m),
            })
            .collect();
        let Some(ix) = crate::ui::dismiss::pick(&candidates) else {
            return false;
        };
        self.dismiss(DISMISS_ORDER[ix]);
        self.request_ui_redraw();
        true
    }
```

`window_event` 里,在 `focus_hotkey_event` 那个 `if` 之后、`let modal = self.modal_open();`
之前插入:

```rust
        // F239:点在最上层弹窗外面 → 走它自己的取消出口关掉,并把这一下
        // **吃掉**。必须在分流之前(理由见 `dismiss_on_outside_press` 的
        // 文档)——放到下面的话,这一下会先被 egui 或终端收走。
        if self.dismiss_on_outside_press(&event) {
            return;
        }
```

- [ ] **Step 4: 跑测试确认它绿**

Run: `cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: 全 `ok.`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 弹窗点在外面即关,判在按下那一刻并吃掉这一下 (F239)"
```

---

# Part C —— F240 `Ctrl+Shift+N` 从终端区建项目

### Task 9: 预填纯函数

**Files:**
- Modify: `crates/mullion-app/src/project.rs`

- [ ] **Step 1: 写失败的测试**

`crates/mullion-app/src/project.rs` 的 `mod tests` 末尾加:

```rust
    /// F240:从 pane 的现场信息推出一份新项目草稿。
    ///
    /// `dir` 取 pane 上报的**完整** cwd(不是最后一级):项目的 `dir` 是
    /// 终端 `work_dir` 与文件面板落脚点,截短了打开项目会落到别处。
    /// `name` 取最后一级 —— 那是人认得出来的那个词。
    #[test]
    fn a_draft_from_a_pane_takes_the_full_path_as_dir_and_the_leaf_as_name() {
        let d = prefill_from_pane("/srv/api/web", Some("claude-web"), SessionId(7), &[]).unwrap();
        assert_eq!(d.dir, "/srv/api/web");
        assert_eq!(d.name, "web");
        assert_eq!(d.nodes, vec![SessionId(7)]);
        assert_eq!(d.preferred, Some(SessionId(7)));
    }

    /// tmux 名取该 pane **当前上报的那个**(不是从项目名推)。
    ///
    /// 这个键的全部价值是「把眼前这个活原样收成一个项目」——推一个新名字
    /// 出来,下次打开它会 attach 到一个空会话,而用户眼前正跑着的那个
    /// 还在原地,**完全静默**。
    ///
    /// 自证会变红:把 `tmux_name` 那一支改成 `None`。
    #[test]
    fn the_draft_keeps_the_tmux_session_the_pane_is_actually_in() {
        let d = prefill_from_pane("/srv/api", Some("claude-api"), SessionId(7), &[]).unwrap();
        assert_eq!(d.tmux_name.as_deref(), Some("claude-api"));
    }

    /// 不在 tmux 里就退回 `None`(= 由项目名推导)。编一个名字出来的话,
    /// 那个名字跟眼前这个 shell 没有任何关系。
    #[test]
    fn a_pane_outside_tmux_leaves_the_tmux_name_unset() {
        let d = prefill_from_pane("/srv/api", None, SessionId(7), &[]).unwrap();
        assert_eq!(d.tmux_name, None);
    }

    /// 名字撞了**不自作主张改名**:原样填进去,让右栏的
    /// `validate_project` 当场把话说清楚(「跟『接口』重名」),光标停在
    /// 名字框上。自动改成「web 2」的话,用户多半不会注意到,过两天多出
    /// 一个莫名其妙的项目。
    ///
    /// 自证会变红:让 `prefill_from_pane` 在撞名时调 `fresh_project_name`。
    #[test]
    fn a_name_clash_is_left_for_the_form_to_complain_about() {
        let mut existing = proj(1, "web");
        existing.dir = "/elsewhere".into();
        let d = prefill_from_pane("/srv/api/web", None, SessionId(7), &[existing]).unwrap();
        assert_eq!(d.name, "web", "撞名了也原样填,交给校验去说");
    }

    /// cwd 是根目录、或者根本推不出最后一级 —— 不硬编一个名字出来。
    #[test]
    fn a_root_directory_has_no_leaf_to_name_the_project_after() {
        assert!(prefill_from_pane("/", None, SessionId(7), &[]).is_none());
        assert!(prefill_from_pane("", None, SessionId(7), &[]).is_none());
    }
```

- [ ] **Step 2: 跑测试确认它红**

Run: `cargo test -p mullion-app --lib project::tests 2>&1 | tail -20`
Expected: FAIL —— `cannot find function 'prefill_from_pane'`

- [ ] **Step 3: 实现**

`crates/mullion-app/src/project.rs`:

```rust
/// F240:把一块 pane 眼下的现场收成一份**新项目草稿**。
///
/// `None` = 推不出来(cwd 没有可用的最后一级),调用方该出一条 toast 说
/// 原因,而不是弹一个填了一半的表单。
///
/// **不落盘、不改名、不校验**:草稿原样交给项目管理器右栏,撞名由
/// `validate_project` 当场报出来。自动改名的话用户多半不会注意到,
/// 过两天库里多出一个莫名其妙的项目。
///
/// `id`/`created_at` 由调用方(拿得到 store 与时钟的那一层)填。
pub fn prefill_from_pane(
    cwd: &str,
    tmux: Option<&str>,
    node: mullion_store::SessionId,
    _existing: &[mullion_store::ProjectRecord],
) -> Option<mullion_store::ProjectRecord> {
    let dir = cwd.trim_end_matches('/');
    let name = dir.rsplit('/').next().filter(|s| !s.is_empty())?;
    Some(mullion_store::ProjectRecord {
        id: mullion_store::ProjectId(0),
        name: name.to_string(),
        note: String::new(),
        nodes: vec![node],
        preferred: Some(node),
        dir: cwd.to_string(),
        // 该 pane **当前上报的**那个 tmux 名。不在 tmux 里就留空
        // (= 由项目名推导,见 `project_tmux_name`)。
        tmux_name: tmux.map(str::to_string),
        created_at: String::new(),
        last_accessed_at: None,
        icon: None,
    })
}
```

> `_existing` 现在没用上。**保留这个参数**是有意的:它把「撞名不自作主张」
> 这条决策钉在签名上 —— 有人想加自动改名时,会先看见它。若 clippy 报
> `needless_pass_by_value` 之类,加 `#[allow]` 并把上面这句写进注释。
> 若 clippy 只报未使用,`_` 前缀已经够。

- [ ] **Step 4: 跑测试确认它绿**

Run: `cargo test -p mullion-app --lib project::tests 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/project.rs
git commit -m "feat(app): 从 pane 现场推出项目草稿的纯函数 (F240)"
```

---

### Task 10: 快捷键接线

**Files:**
- Modify: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: 写失败的测试**

`app.rs` 的 `mod tests` 里加:

```rust
    /// F240:cwd 拿不到就**不弹**表单,出一条 toast 说原因。
    ///
    /// 弹一个 `dir` 空着的表单的话,这个键就没省下任何事(用户还得自己
    /// 去别处把路径抄过来),而且看起来像是功能坏了。
    ///
    /// 自证会变红:把 `apply_project_hotkey` 里那条 `else` 分支的 toast
    /// 删掉,改成照弹表单。
    #[test]
    fn a_pane_that_never_reported_its_directory_gets_a_toast_not_a_form() {
        let mut app = test_app();
        app.apply_project_hotkey();
        assert!(!app.ui.project_manager_open, "没有 cwd 不该弹表单");
        assert!(
            app.ui.pending_toast.is_some(),
            "没有 cwd 时得说一句为什么"
        );
    }
```

> 这条测试要求 `test_app()` 造出来的 App 没有任何终端 pane(默认就是)。
> 另外三条行为(预填内容、撞名报错、已属项目改成编辑)在 Task 9 与
> `project_manager` 侧已有覆盖,这里不重复。

- [ ] **Step 2: 跑测试确认它红**

Run: `cargo test -p mullion-app --lib project_hotkey 2>&1 | tail -20`
Expected: FAIL —— `no method named 'apply_project_hotkey'`

- [ ] **Step 3: 实现**

`impl App` 里加(紧挨着 `files_hotkey_event` / `apply_files_hotkey`):

```rust
    /// F240:`Ctrl+Shift+N` —— 从焦点分屏的现场建项目。
    ///
    /// 与另外三个快捷键同形状:判在 `translate_key`,闸门是 `modal_open()`,
    /// 位置在输入分流**之前**(T8)——走到下面 `N` 会被编码进 PTY,给远端
    /// shell 写一个字母。
    fn project_hotkey_event(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::KeyboardInput { event: ke, .. } = event else {
            return false;
        };
        if ke.state != ElementState::Pressed {
            return false;
        }
        let Some((key, mods)) = input::translate_key(ke, self.mods) else {
            return false;
        };
        if self.modal_open() || !mods.ctrl || !mods.shift || mods.alt || mods.sup {
            return false;
        }
        if !matches!(key, Key::Char('n' | 'N')) {
            return false;
        }
        self.apply_project_hotkey();
        self.request_ui_redraw();
        true
    }

    /// F240 的落地。三条出口:
    ///
    /// 1. 这块 pane 已经属于某个项目 → 打开**那个项目的编辑**表单。
    ///    (再建一个的话,同一台机器同一个目录会有两个项目,而它们会算出
    ///    同一个 tmux 名 —— `validate_project` 会拦,但用户会一头雾水。)
    /// 2. 拿得到 cwd → 落一条骨架进库,再把预填内容只写进草稿(**不落盘**)。
    /// 3. 拿不到 cwd → 一条 toast 说清为什么,不弹空表单。
    fn apply_project_hotkey(&mut self) {
        let cwd = self
            .tabs
            .active()
            .and_then(|t| t.content.focused_pane_cwd())
            .map(|b| String::from_utf8_lossy(&b).into_owned());
        let tmux = self
            .active_ws()
            .and_then(|ws| ws.focused())
            .and_then(|p| p.tmux.clone());
        let node = self
            .active_ws()
            .and_then(|ws| ws.focused())
            .and_then(|p| ws_session_id(ws_of(self), p.host_ix));

        // ① 已经属于某个项目 → 改成打开它的编辑表单。
        let existing: Vec<mullion_store::ProjectRecord> = self
            .store
            .as_ref()
            .map_or(&[][..], |s| s.projects())
            .to_vec();
        if let Some(p) = crate::project::project_of(tmux.as_deref(), &existing) {
            self.ui.project_manager_open = true;
            self.ui.project_selected = Some(p.id);
            self.ui.project_draft = Some(p.clone());
            self.ui.project_focus_name = true;
            return;
        }

        // ② / ③
        let Some((cwd, node)) = cwd.zip(node) else {
            self.ui.set_toast(
                crate::ui::toast::Kind::Warn,
                "这块分屏还没报过当前目录,建不出项目",
            );
            return;
        };
        let Some(mut draft) = crate::project::prefill_from_pane(
            &cwd,
            tmux.as_deref(),
            node,
            &existing,
        ) else {
            self.ui.set_toast(
                crate::ui::toast::Kind::Warn,
                format!("从「{cwd}」推不出项目名,请手动新建"),
            );
            return;
        };

        // 先落一条**骨架**(名字用 `fresh_project_name` 保证不撞),再把预填
        // 内容只写进草稿。这样草稿相对库里那条天然是**脏**的 → F239 的脏
        // 保护自动生效,手一滑点到终端不会把预填清掉;而撞名由右栏的
        // `validate_project` 当场报出来,「保存」灰着。
        let Some(store) = self.store.as_mut() else {
            self.ui
                .set_toast(crate::ui::toast::Kind::Warn, "会话库还没打开");
            return;
        };
        let now = crate::localtime::now_rfc3339();
        let skeleton = crate::project::fresh_project_name(store.projects());
        let id = store.add_project(skeleton, String::new(), &now);
        if let Err(e) = store.save() {
            self.ui.set_error(format!("项目没能存下来:{e}"));
        }
        draft.id = id;
        draft.created_at = now;
        self.ui.project_manager_open = true;
        self.ui.project_selected = Some(id);
        self.ui.project_draft = Some(draft);
        self.ui.project_focus_name = true;
    }
```

> `ws_session_id(ws_of(self), p.host_ix)` 是占位写法。**实际按 `app.rs` 里
> `TitleView` 组装那段的写法来**(`ws.hosts.get(p.host_ix).and_then(|h| h.session_id)`),
> 一次性把 `ws` 借出来算完 `tmux` / `node` 两个值,避免三次 `active_ws()`。
> `crate::localtime::now_rfc3339()`:按 `grep -rn "now_rfc3339\|created_at:" crates/mullion-app/src/app.rs`
> 找到 F221 建项目时用的那个时钟调用,原样复用,**不要**新引一个。

`window_event` 里,在 `files_hotkey_event` 那个 `if` 之后加:

```rust
        // F240/T8:从终端区建项目同样必须在分流之前截 —— `N` 走到下面会被
        // 编码进 PTY,给远端 shell 写一个字母。
        if self.project_hotkey_event(&event) {
            return;
        }
```

- [ ] **Step 4: 跑测试确认它绿**

Run: `cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: 全 `ok.`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): Ctrl+Shift+N 从终端区现场预填一条新项目 (F240)"
```

---

### Task 11: 快捷键一览表

**Files:**
- Modify: `crates/mullion-app/src/ui/shortcuts.rs`

- [ ] **Step 1: 加一行 + 放宽 scope 名单**

`SHORTCUTS` 里加(排在 `Ctrl+Shift+E` 一类的后面):

```rust
    Shortcut {
        chord: "Ctrl+Shift+N",
        scope: "项目",
        what: "把当前分屏的目录和 tmux 会话收成一个新项目",
    },
```

`every_module_that_has_shortcuts_is_represented` 那条测试的 scope 白名单
加 `"项目"`。

- [ ] **Step 2: 跑测试**

Run: `cargo test -p mullion-app --lib ui::shortcuts 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`(`no_two_rows_claim_the_same_chord` 会顺带证明
`Ctrl+Shift+N` 没被占用)

- [ ] **Step 3: 提交**

```bash
git add crates/mullion-app/src/ui/shortcuts.rs
git commit -m "docs(app): 快捷键一览表登记 Ctrl+Shift+N (F240)"
```

---

# Part D —— 收口

### Task 12: spec 登记、跑绿、发版

**Files:**
- Modify: `spec.md`

- [ ] **Step 1: spec.md 登记三条**

照既有条目的格式加 F238 / F239 / F240,各写清「是什么 / 边界」。
F239 那条务必写上**豁免七类**与**脏了不关**,那是这条需求里唯一会
造成真实损失的地方。

- [ ] **Step 2: 全绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
cargo fmt --check
```
Expected: 全 `ok.`;clippy 无输出;fmt 无输出。
**只跑单个 crate 不叫绿。**

- [ ] **Step 3: 变异自证**

逐条按各测试注释里的「自证会变红」做一次变异,确认它真的红,再
`git checkout` 回来。**做之前先 `git status` 确认没有未提交的编辑**
(前面已经踩过五次「`git checkout` 吞掉未提交编辑」)。

重点自证的六条(其余按注释走):
1. `pick` 去掉 `!top.dirty` → `a_dirty_dialog_survives_a_click_outside` 红
2. `pick` 让脏的时候穿透 → `a_dirty_top_dialog_does_not_let_the_click_fall_through` 红
3. `locate` 的 `Foreground` 一支改成 `Outside` → `clicking_a_popup_...` 红
4. `DISMISS_ORDER` 删一行 → `every_dialog_is_either_in_the_dismiss_order_or_explicitly_exempt` 红
5. `title_of` 任意一臂加空格 → `the_title_we_report_is_the_window_we_actually_draw` 红
6. `prefill_from_pane` 的 `tmux_name` 改 `None` → `the_draft_keeps_the_tmux_session...` 红

- [ ] **Step 4: 提交 spec**

```bash
git add spec.md
git commit -m "docs: spec 登记 F238~F240(项目图标/弹窗点外即关/从终端区建项目)"
```

- [ ] **Step 5: 发版**

按 `.claude/skills/release-windows/SKILL.md` 一条龙:升 patch(0.1.98 → 0.1.99)
→ 跑绿 → 交叉编译 + objdump 验收 → 签名 → 发 GitHub Release(走 socks 代理)
→ 报链接。

**人工验收清单**(写进 Release notes,这些本机验不了):

F238
1. 项目管理器右栏「外观」里导入一张 .ico,三处项目列表(启动页 / 项目管理器左栏 / pane 标题条的切项目弹窗)都换成了这张图
2. 清除之后回落成首选节点的图标,底色跟着节点色
3. 属于某项目的 pane,标题条上的图标是**项目**的那张,不是会话的
4. 图标在 32px 档下不糊、不是豆腐块

F239
5. 会话管理器 / 设置 / 项目管理器什么都没改时,点终端区一下就关掉
6. 改了一个字之后再点终端区,**不关**;改回原样再点,关
7. 会话管理器里点开任意一个下拉框,点它的选项 —— 窗口**不关**
8. 项目管理器上叠着「打开项目前先确认」时,点管理器身上只关掉确认框
9. 主密码框 / TOFU 框 / 编辑器 / 文件面板的就地改名,点外面都**不关**
10. 设置里按住字号滑块拖到窗口外面再松手,窗口**不关**
11. 关掉弹窗的那一下**没有**在终端里起划选

F240
12. 焦点分屏在 tmux 里、有 cwd 时按 `Ctrl+Shift+N`,项目管理器弹出并停在预填好的新建记录上,`目录` = 完整路径、`tmux 名` = 眼下这个会话名
13. 名字撞了时右栏当场报「跟『X』重名」,光标在名字框里,「保存」灰着
14. 刚开机、pane 还没报过目录时按同一个键,只出一条 toast,不弹表单
15. 已属某项目的 pane 上按同一个键,打开的是**那个项目的编辑**表单
16. 这个键**没有**给远端 shell 写进一个 `N`

---

## Self-Review

**Spec 覆盖**

| 约定 | 落点 |
|---|---|
| F238 项目加 `icon`,schema v10→v11 | Task 1 |
| 未设时回落首选节点的图标 / 底色回落节点色 / 不加项目色 | Task 2 |
| 三处项目列表(走共享行) | Task 3 |
| pane 标题条顶掉会话图标 | Task 4 |
| 设图标的入口在项目管理器右栏,与会话侧同构 | Task 5 |
| F239 默认全开、走既有取消出口、不新增出口变体 | Task 7 `dismiss` |
| 豁免 `Unlock`/`HostKey`/`Editor`/三个就地输入框(+`Paste` 已自带) | Task 7 `DISMISS_EXEMPT` |
| 脏了不关,脏判据走快照比对 | Task 7 `dismiss_dirty` |
| 只关最上层,一次一个 | Task 6 `pick` |
| 判在按下不判松开 | Task 8 |
| F240 `Ctrl+Shift+N` 弹预填好的新建表单 | Task 10 |
| `dir`=完整 cwd / `name`=最后一级 / `tmux_name`=当前上报的 | Task 9 |
| 撞名不自作主张,交给 `validate_project` | Task 9 |
| cwd 拿不到就不弹,出 toast | Task 10 |
| pane 已属项目 → 改开该项目的编辑表单 | Task 10 |
| 一览表加一行 + 新 scope | Task 11 |
| 一个切片、一次发版 | Task 12 |

**已知会跟着改的东西**(不是遗漏,是代价)
- `project_row` 的 `ICON_X`/`TEXT_X` 让行内坐标整体右移 → 四个文件的落点测试要跟着改(Task 3 Step 6 已列)。
- `ImportState` 多一个字段 → 两处构造点都要填(Task 7 Step 3)。
- `Row` 多两个字段 → 三处调用点都要填(Task 3 Step 4)。

**留给实现者补实的两处**(**不许留成空壳**)
1. Task 3 Step 2 的 `ico::tests_support::solid_ico` —— 按 `ico` crate 的实际 API 调,判据是 `cargo test -p mullion-app --lib ui::ico` 全绿。
2. Task 8 Step 1 第二条测试的正文 —— 若造不出带终端的 `App`,换成钉住 `dragging`/`press_anchor` 没被写过的等价判据,并把理由写进注释。
