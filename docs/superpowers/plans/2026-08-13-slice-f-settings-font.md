# 切片 F:设置弹窗 + 可配置字体(F84 / F21)实施计划

> 设计定案:`docs/superpowers/specs/2026-08-13-settings-font-design.md`
> 每个任务自成一次「能编过、测试全绿」的提交,除非任务里写明「与下一任务同提交」。

**目标**:菜单「设置」打开弹窗,能换终端字体族与字号(即时预览、取消回滚),
带一张只读快捷键一览;字号跟随显示器 DPI。

**架构**:`mullion-store` 出一个新的 `settings.toml` 盘面(零 UI、可纯单测);
`mullion-app` 侧 `TextLayer` 支持换字体、`ui/settings.rs` 出弹窗。
**尺寸传播一律走既有 `compute_geoms` → `apply_geometry`,不新开路径(T4)。**

---

## Task 1:store —— `settings.toml` 盘面

**Files**:Create `crates/mullion-store/src/settings.rs`;Modify `crates/mullion-store/src/lib.rs`

- [ ] `Settings { schema_version: u32, font_family: Option<String>, font_pt: f32 }`,
      `Default` = `{1, None, 10.0}`(与今天 `FONT_POINT_SIZE` 一致)
- [ ] `pub const MIN_FONT_PT: f32 = 6.0; pub const MAX_FONT_PT: f32 = 32.0;`
      `pub fn clamp_font_pt(v: f32) -> f32`——**NaN 也要落回默认**
      (`f32::clamp` 遇 NaN 返回 NaN,直接用会把 NaN 一路带到 `cell_h`,
      wgpu 尺寸 NaN 是 `gui-render-gotchas.md` 记过的崩溃点)
- [ ] `load(dir) -> Loaded { settings, note }`,不返回 `Result`;文件不存在 =
      默认无 note;解析失败 = 默认 + note
- [ ] `save(dir, &Settings) -> Result<(), StoreError>`,复用既有 `write_atomic`
- [ ] 测试:round-trip;`clamp_font_pt(0.5) == 6.0` / `(999.0) == 32.0` /
      `(f32::NAN) == 10.0`;坏 TOML 落回默认且 note 非空;
      **`load` 出来的 `font_pt` 必须已经夹过**(在 `load` 里夹,不是让调用方夹)
- [ ] 变异:把 `load` 里那句 clamp 删掉 → 夹紧那条测试必须变红

## Task 2:纯函数 —— 字体族清单与等宽判定

**Files**:Create `crates/mullion-app/src/font_pick.rs`;Modify `crates/mullion-app/src/main.rs`(或 `lib` 挂载点)

- [ ] `pub struct FontChoice { pub name: String, pub monospaced: bool }`
- [ ] `pub fn sort_families(raw: Vec<(String, bool)>) -> Vec<FontChoice>`:
      按族名去重(同族多字重会重复出现)、**等宽在前**、组内按名字排序
- [ ] `pub fn is_monospace_advance(m: f32, i: f32) -> bool`:
      `(m - i).abs() <= m * 0.01`(1% 容差,吸收 hinting 的亚像素差)
- [ ] `pub fn family_missing(chosen: &str, known: &[FontChoice]) -> bool`
      ——大小写不敏感比对(fontdb 的族名大小写不稳)
- [ ] 测试:去重(同名两条只出一条)、等宽排前、`is_monospace_advance` 边界
      (相等 / 差 0.5% / 差 20%)、`family_missing` 大小写不敏感
- [ ] 变异:把「等宽在前」的排序键去掉 → 排序测试变红

## Task 3:`TextLayer` 支持换字体

**Files**:Modify `crates/mullion-app/src/text.rs`

- [ ] `FONT_FAMILY` → `pub const DEFAULT_FONT_FAMILY`
- [ ] `TextLayer` 加 `family: Option<String>` / `font_px: f32` 字段;
      `new` 多收 `family` 参数
- [ ] `prepare_panes` 里 `Attrs::family(..)` 改读字段
      (`Family::Name(self.family.as_deref().unwrap_or(DEFAULT_FONT_FAMILY))`)
- [ ] `pub fn set_font(&mut self, family: Option<&str>, font_px: f32)`:
      重算 `cell_h`/`cell_w`,**`font_px <= 0.0` 或非有限值直接返回**
      (不让 NaN/0 进 cell 尺寸)
- [ ] `pub fn families(&self) -> Vec<FontChoice>`:从 `font_system.db().faces()`
      收 `(family, monospaced)` 交给 `sort_families`
- [ ] `pub fn advance_of(&mut self, ch: char) -> f32`:量单字 advance,
      给等宽校验用(复用 `measure_cell_w` 的做法)
- [ ] GPU 胶水无单测——本任务判据是「编过 + 下游 Task 5/6 的守护测试」,
      在提交信息里写明

## Task 4:快捷键一览表

**Files**:Create `crates/mullion-app/src/ui/shortcuts.rs`;Modify `crates/mullion-app/src/ui/mod.rs`

- [ ] `pub struct Shortcut { pub chord: &'static str, pub scope: &'static str,
      pub what: &'static str }` + `pub const SHORTCUTS: &[Shortcut]`
- [ ] 逐条从**实现处**核对再抄(`mullion_term::keymap`、`shell::tabs::hotkey`、
      `app.rs` 的 `KeyboardInput` 分支、`ui/mod.rs` 的 egui 快捷键),
      模块文档里写明「这张表是手抄的,唯一真源在实现处」
- [ ] 测试 `no_two_rows_claim_the_same_chord`(撞键当场变红)、
      `every_row_is_filled_in`、`the_table_is_not_empty`
- [ ] 变异:表里插一条重复 chord → 查重测试变红

## Task 5:设置弹窗

**Files**:Create `crates/mullion-app/src/ui/settings.rs`;Modify `crates/mullion-app/src/ui/mod.rs`

- [ ] `pub struct SettingsDraft { pub family: Option<String>, pub font_pt: f32,
      pub typed: String }`(`typed` = 手填框的文本)
- [ ] `pub enum SettingsOut { None, Preview, Commit, Cancel }`,
      `pub fn show(ctx, t, draft: &mut SettingsDraft, env: SettingsEnv<'_>) -> SettingsOut`
      (`env` 带 `families: &[FontChoice]`、`mono_warning: bool`、`missing: bool`)
- [ ] 分节走 `form::section` + `form::grid`;宽度只用 `FIELD_W_*`、间距只用
      `SP_*`(`tests/form_guidelines.rs` 会扫)
- [ ] 主题那一栏 `add_enabled(false, ..)` + `on_disabled_hover_text`
      (「暂只有一套 Mullion Dark;换主题要重算 F62 的对比度闸门」)
- [ ] 字号滑块 `MIN_FONT_PT..=MAX_FONT_PT`,**拖动即 `Preview`**
- [ ] 非等宽 / 字体不存在 → 输入列下方灰字(挂输入列,不挂标签列,规范 #5/#6)
- [ ] `annotate::mark` 登记
- [ ] 测试(egui 跑帧,模式同 `ui/restored.rs`):
      - `dragging_the_size_slider_reports_a_preview_not_a_commit`
      - `cancel_reports_cancel_so_the_caller_can_roll_back`
      - `a_non_monospace_font_is_called_out_next_to_the_field`
      - `the_theme_row_is_disabled_and_says_why`
- [ ] 变异:把 `Preview` 改成 `None` 返回 → 预览测试变红

## Task 6:app 接线

**Files**:Modify `crates/mullion-app/src/app.rs`、`crates/mullion-app/src/ui/mod.rs`、`crates/mullion-app/src/ui/chrome.rs`

- [ ] 菜单「设置」入口(放「会话」旁边,或既有 `top_menu` 的合适位置)
      → `ui_state.settings_open = true`
- [ ] `UiActions` 加 `settings: Option<SettingsOut>` + `has_real_action` 同步
      (**漏加就是 D4b 那条老坑:动作在 egui 的 discard 帧被吞掉**),
      配一条 `settings_alone_counts_as_a_real_action_for_the_discard_guard`
- [ ] `App` 加 `settings: mullion_store::Settings` / `settings_backup: Option<..>`;
      `resumed()` 里在 `TextLayer::new` **之前**读 `settings::load`,
      用它的 `font_family`/`font_pt` 建 `TextLayer`
- [ ] `Preview` → `set_font` + 标脏;`Commit` → `settings::save`(失败 →
      `ui.set_error`,见设计 §1);`Cancel` → 装回 `settings_backup` 再
      `set_font` 一次
- [ ] `ScaleFactorChanged` → 重算 `font_px` → `set_font` → 标脏
      (今天只记日志,`app.rs:4517`)
- [ ] 守护测试:
      - `changing_the_cell_size_changes_what_the_remote_is_told`——
        `layout_geometry` 在两组 cell 尺寸下 `cols/rows` 不同(纯函数,无 GPU)
      - 结构守护 `a_font_change_goes_through_the_same_geometry_path_as_a_resize`:
        `set_font` 的调用点附近必须标脏,且 `app.rs` 里**除 `compute_geoms`
        之外没有第二处**用 `cell_w`/`cell_h` 算 `cols`/`rows`
      - 结构守护 `the_scale_factor_change_actually_rebuilds_the_font`:
        `ScaleFactorChanged` 分支必须含 `set_font(`
- [ ] 变异:删掉 `ScaleFactorChanged` 里的 `set_font(` → 变红;
      `has_real_action` 去掉 settings 一项 → 变红

## Task 7:全量变异验收 + 交付

- [ ] 逐条跑 Task 1~6 里列的变异,确认全部变红
- [ ] `cargo test --workspace` + `clippy -D warnings` + `fmt --check`
- [ ] `chore: 版本 0.1.39(…)`
- [ ] 交叉编译 `x86_64-pc-windows-gnu` + objdump 验收
      (出现 `libgcc_s_seh-1.dll` / `libwinpthread-1.dll` = 不合格)
- [ ] Release `v0.1.39`(标题纯版本号)+ 人工验收清单:
      换字体后**远端 tmux 里的 TUI 不错行**(T4 的真实验收)、
      非等宽字体的警告文案、高 DPI 屏拖到低 DPI 屏字号跟随、
      取消回滚、快捷键一览与实际按键一致
