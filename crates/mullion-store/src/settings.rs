//! 全局外观设置的**磁盘格式**(F84/F21,设计 §1)。零 UI、零 async。
//!
//! 独立文件 `<config_dir>/settings.toml`,与 `sessions.toml`、`layout.toml`
//! 各走各的 `schema_version`。
//!
//! **为什么不并进 `sessions.toml`**:那要动会话库的 schema 版本(v8 已被
//! F74/F120 预定),更要紧的是字体是**全局**偏好——塞进会话库,「导出会话给
//! 同事」(F46)就会顺带把本机的 DPI 偏好一起导出去。

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::StoreError;

/// 本版本写出的设置格式版本。
pub const CURRENT_SETTINGS_SCHEMA: u32 = 1;

/// 文件名。
pub const SETTINGS_FILE: &str = "settings.toml";

/// 字号下界(pt)。再小的话一屏几万个格子,每帧都要给 glyphon 建那么多
/// buffer,窗口当场卡住。
pub const MIN_FONT_PT: f32 = 6.0;

/// 字号上界(pt)。再大的话在小窗口上 `cols` 会算成 0,而
/// `Emulator::resize(0, ..)` 会按 1 列把 scrollback 碾平
/// (`shell/window_state.rs` 早就为最小化踩过这个坑)。
pub const MAX_FONT_PT: f32 = 32.0;

/// 默认字号(pt)。与本片之前硬编码的 `FONT_POINT_SIZE` 一致 —— 没设置文件的
/// 老用户升上来,画面必须一个像素都不变。
pub const DEFAULT_FONT_PT: f32 = 10.0;

/// 日志详细档位(F155)。
///
/// **只有三档**,不照搬 `log::LevelFilter` 的六档:`trace`/`off` 对用户没有
/// 可解释的含义(前者是给 crate 作者看的,后者等于「出了事没证据」),
/// 而多一个档就多一种「我到底该选哪个」的犹豫。
///
/// 这里**不认识 `log` crate** —— `mullion-store` 是零依赖方向的叶子
/// (见 `layout.rs` 那条架构守护)。映射成 `LevelFilter` 是 app 侧 `logx` 的事。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// 只记错误与降级。日志最小,但出了性能问题手上没有数据。
    Error,
    /// 默认:生命周期事件 + 每 5 秒一行性能剖面。
    Info,
    /// 上面全部,外加逐事件细节。给排查用,日志会大很多。
    Debug,
}

fn default_log_level() -> LogLevel {
    LogLevel::Info
}

/// 全局外观设置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// 格式版本。
    pub schema_version: u32,
    /// 终端字体族。`None` = 用内置默认(app 侧的 `DEFAULT_FONT_FAMILY`)。
    ///
    /// **存族名而不是文件路径**:同一个族在不同机器上装在不同位置,存路径
    /// 等于让配置绑死一台机器(与 F48「配置跨机可用」的方向相反)。
    #[serde(default)]
    pub font_family: Option<String>,
    /// 终端字号,单位 **pt**(不是像素)。
    ///
    /// 物理像素由 app 侧按窗口 `scale_factor` 换算。存像素的话换一块 DPI
    /// 不同的屏,字号语义就变了 —— 而 F21 的验收点之一正是「跟随
    /// `ScaleFactorChanged` 动态更新」。
    #[serde(default = "default_font_pt")]
    pub font_pt: f32,
    /// F124:连上之后自动开一条旁路 exec,把远端 tmux 的 `set-titles` 打开、
    /// `set-titles-string` 改成带 `#{pane_current_path}` 的格式。
    ///
    /// **默认开**:F123 的两个功能(标题条目录名 / SFTP 目录继承)在没配过的
    /// 远端上一个字节都收不到,而「没配过」是绝大多数机器的状态。关掉之后
    /// 整条自举不跑(连一次 exec 都不发),F123 退回手工配置(见
    /// `docs/remote-state-setup.md`)。
    ///
    /// 这是往用户的机器上写东西(改的是 tmux 服务器**内存里**的全局选项,
    /// 不落盘、server 退出即失效),所以必须给得出「不要」。
    #[serde(default = "default_tmux_bootstrap")]
    pub tmux_bootstrap: bool,
    /// F156-c:pane 的 shell channel 一建立,就往 PTY 注入一行,让远端 shell
    /// 从此每个提示符发一次 OSC 7(当前目录)。
    ///
    /// **默认开**:非 tmux 场景下 `PaneState.cwd` 本来一条腿都没有 ——
    /// Ubuntu 的 bash 默认不发 OSC 7,而窗口标题那条腿只要 PS1 被
    /// starship / oh-my-bash 接管就断。用户报的「`Ctrl+Shift+B` 经常留在 `~`」
    /// 就是这个。
    ///
    /// 与 [`Settings::tmux_bootstrap`] **分开两个开关**:那个改的是远端 tmux
    /// 服务器内存里的选项,这个往用户**当前这条 shell** 里写一行命令并清屏。
    /// 副作用完全不同,想只关掉其中一件是合理诉求,一个开关做不到。
    ///
    /// 不写远端任何文件 —— 只活在这条 shell 的内存里,断开即消失。命令串与
    /// 逐处理由见 `mullion_app::shell_bootstrap::OSC7_SETUP`。
    #[serde(default = "default_shell_osc7_bootstrap")]
    pub shell_osc7_bootstrap: bool,
    /// F155:日志详细档位。
    ///
    /// 契约(**本提交尚未接线**):app 侧 `logx` 在环境变量 `MULLION_LOG`
    /// 存在时应覆盖这里 —— 排障时不必先进 GUI 改设置。
    #[serde(default = "default_log_level")]
    pub log_level: LogLevel,
    /// F187:文件面板**本地栏**的收藏夹。**全局一份,不挂会话。**
    ///
    /// 与远端书签(`SftpPrefs::bookmarks`,挂在 `SessionRecord` 上)相反:
    /// `D:\work` 是**这台 Windows 机器**上的目录,跟连的是哪台远端毫无关系。
    /// 挂在会话下的代价是同一个本地目录要在每条会话里各收一次,而用户的心智
    /// 模型是「我的常用文件夹」(F154 当初照着远端书签的样子做,是错的)。
    ///
    /// **为什么放这里而不是 `sessions.toml`**:同 `font_family` 那条理由 ——
    /// 本地路径是本机偏好,导出会话给同事(F46)不该把 `D:\我的项目` 带走。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local_bookmarks: Vec<crate::sftp::Bookmark>,
    /// F253:文件面板要不要显示 `.` 开头的项。**这是「新面板的初始值」,不是
    /// 当前显示状态** —— 每个面板自己持有一份运行态,`Ctrl+H` 改那一份并写穿
    /// 回这里(见 `files::state::PaneState::show_hidden`)。
    ///
    /// **默认开**:本项目的主场景是「在远端跑 Claude Code」,工作目录里最要紧
    /// 的东西恰好全是 `.` 开头的(`.claude/`、`.git/`、`.env`)—— 默认藏起来
    /// 等于默认藏掉主场景。
    #[serde(default = "default_show_hidden_files")]
    pub show_hidden_files: bool,
    /// F187:老库里各会话名下的 `SftpPrefs::local_bookmarks` 已经并进来了没有。
    ///
    /// **必须有这个标记,不能靠「每次启动都合一遍」**:那样确实幂等,但用户
    /// 取消收藏之后,下次启动会从没清理的会话记录里原样长回来 —— 一个删不掉
    /// 的收藏,而且看不出原因。
    #[serde(default)]
    pub local_bookmarks_migrated: bool,
}

impl Settings {
    /// F187:收一个本地目录。去重判据与会话书签共用 `vault::push_deduped`
    /// (按 `path`)—— 分叉的话点「取消收藏」会看起来没反应。
    pub fn add_local_bookmark(&mut self, mark: crate::sftp::Bookmark) {
        crate::vault::push_deduped(&mut self.local_bookmarks, mark);
    }

    /// F187:取消收藏。按路径相等匹配,同 [`Self::add_local_bookmark`]。
    pub fn remove_local_bookmark(&mut self, path: &str) {
        self.local_bookmarks.retain(|b| b.path != path);
    }

    /// F187:把老库里各会话名下的本地书签并进全局列表,**只做一次**。
    ///
    /// 返回真正并进来的条数;已经并过则**一条都不看**,直接返回 0。
    ///
    /// 调用方在「返回值 > 0」**或**「调用前标记本来是假」时都要存盘 —— 只看
    /// 条数的话,一个从来没收藏过本地目录的用户永远置不上标记,每次启动都白
    /// 跑一趟(而且哪天他在老版本里收过的东西被别的机器同步过来,又会被当成
    /// 首次迁移)。
    pub fn merge_local_bookmarks(
        &mut self,
        old: impl IntoIterator<Item = crate::sftp::Bookmark>,
    ) -> usize {
        if self.local_bookmarks_migrated {
            return 0;
        }
        let before = self.local_bookmarks.len();
        for mark in old {
            self.add_local_bookmark(mark);
        }
        self.local_bookmarks_migrated = true;
        self.local_bookmarks.len() - before
    }
}

fn default_font_pt() -> f32 {
    DEFAULT_FONT_PT
}

fn default_tmux_bootstrap() -> bool {
    true
}

fn default_shell_osc7_bootstrap() -> bool {
    true
}

fn default_show_hidden_files() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SETTINGS_SCHEMA,
            font_family: None,
            font_pt: DEFAULT_FONT_PT,
            tmux_bootstrap: true,
            shell_osc7_bootstrap: true,
            log_level: LogLevel::Info,
            show_hidden_files: default_show_hidden_files(),
            local_bookmarks: Vec::new(),
            // 全新用户没有老数据要并,但标记仍从 `false` 起 —— 让首次启动
            // 走一遍(空)迁移再置上,两条路(有老库/没老库)不分叉。
            local_bookmarks_migrated: false,
        }
    }
}

/// 把手改坏的字号拉回可用区间。
///
/// **NaN 必须落回默认而不是原样返回**:`f32::clamp` 遇 NaN 返回 NaN,直接用
/// 会让 NaN 一路进 `cell_h` → wgpu 的尺寸计算(`gui-render-gotchas.md` 记过
/// 的崩溃点)。这不是防御性编程的洁癖——`settings.toml` 是明文、用户会手改。
pub fn clamp_font_pt(v: f32) -> f32 {
    if !v.is_finite() {
        return DEFAULT_FONT_PT;
    }
    v.clamp(MIN_FONT_PT, MAX_FONT_PT)
}

/// 读出来的设置 + 一句说明。
///
/// 与 [`crate::layout`] 同款:**读取永不失败**。外观读不出来只是回到默认字体,
/// 为它阻断启动不成比例。`note` 非空时由 app 记一行日志。
pub struct Loaded {
    pub settings: Settings,
    pub note: Option<String>,
}

/// 读 `<dir>/settings.toml`。**永不失败**,理由见 [`Loaded`]。
///
/// 出口处的 `font_pt` **已经夹过**——夹紧放在这里而不是让每个调用方自己夹:
/// 调用方漏夹一处,坏值就直接进了渲染路径,而那条路径上的症状(窗口卡死 /
/// scrollback 被碾平)看起来完全不像「设置文件被改坏了」。
pub fn load(dir: &Path) -> Loaded {
    let path = dir.join(SETTINGS_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Loaded {
                settings: Settings::default(),
                note: None,
            }
        }
        Err(e) => {
            return Loaded {
                settings: Settings::default(),
                note: Some(format!("读不出来({e}),这次用默认外观")),
            }
        }
    };
    let mut settings: Settings = match toml::from_str(&text) {
        Ok(s) => s,
        Err(e) => {
            return Loaded {
                settings: Settings::default(),
                note: Some(format!("解析失败({e}),这次用默认外观")),
            }
        }
    };
    // 更新版本写出来的文件:**不猜**,同 `layout.rs`。
    if settings.schema_version > CURRENT_SETTINGS_SCHEMA {
        return Loaded {
            settings: Settings::default(),
            note: Some(format!(
                "settings.toml 是 v{} 写的(本版本认到 v{CURRENT_SETTINGS_SCHEMA}),这次用默认外观",
                settings.schema_version
            )),
        };
    }
    // 空字符串的族名等价于「没设」:手改成 `font_family = ""` 的话,
    // cosmic-text 会拿它去匹配、匹配不上再静默回退,用户看到的是默认字体
    // 却在设置里看到一个空框,对不上账。
    if settings.font_family.as_deref().is_some_and(str::is_empty) {
        settings.font_family = None;
    }
    settings.font_pt = clamp_font_pt(settings.font_pt);
    Loaded {
        settings,
        note: None,
    }
}

/// 落盘前的归一化。[`save`] 与 [`mutate`] 共用 —— 各写一遍的话,以后往里加
/// 一步(再夹一个字段)必然漏改一处,而 `mutate` 交出去的那份就会与盘上那份
/// 悄悄分叉。
fn normalize(s: &mut Settings) {
    s.schema_version = CURRENT_SETTINGS_SCHEMA;
    s.font_pt = clamp_font_pt(s.font_pt);
}

/// 原子写 `<dir>/settings.toml`。
///
/// **写失败要往上报**(与 `layout::save` 相反):布局是自动存的「上次的场景」,
/// 设置是用户刚点了确定的显式动作,静默失败 = 改了没生效、而他不知道为什么。
///
/// F247:**只对 crate 内可见**。app 侧一律走 [`mutate`] —— 「整份覆盖」正是
/// 多实例互相抹掉对方改动的那个 bug,留一个公开的整份覆盖入口等于把闸门
/// 交给「以后的人记不记得」。改成 `pub(crate)` 之后这件事由编译器管。
pub(crate) fn save(dir: &Path, settings: &Settings) -> Result<(), StoreError> {
    std::fs::create_dir_all(dir)?;
    let mut out = settings.clone();
    normalize(&mut out);
    let text = toml::to_string_pretty(&out)?;
    crate::vault::write_atomic(&dir.join(SETTINGS_FILE), text.as_bytes())
}

/// F247:读-改-写。**唯一的写入口。**
///
/// 把 `f` 施加在**盘上那份**上再写回,而不是拿内存里那份整份覆盖。开两个 exe
/// 时,A 点一颗☆、B 随后改字号,整份覆盖会让 A 那颗☆ 被 B 静默抹掉 ——
/// `settings.toml` 里的 `local_bookmarks`(F187,全局收藏夹)每点一次☆ 就写一次,
/// 这条路走得最勤。
///
/// `mine` 是调用方内存里那份,只在**盘上那份不可信**时当底用:[`load`] 永不
/// 失败,读不出 / 解析失败 / schema 太新三种情形一律返回 `Settings::default()`
/// 并留下 `note`。拿那份默认值当底会把用户的字体和全部☆ 整份抹掉 —— 比原来的
/// 整份覆盖更狠。盘上没有可信内容时就没有可合并的对象,`mine` 是唯一的真值。
///
/// 交出去的是**合并并归一化后的整份**(调用方直接拿它替换自己内存里那份)加上
/// `f` 的返回值(F187 的迁移条数要拿它写日志)。
pub fn mutate<R>(
    dir: &Path,
    mine: &Settings,
    f: impl FnOnce(&mut Settings) -> R,
) -> Result<(Settings, R), StoreError> {
    let loaded = load(dir);
    let mut merged = if loaded.note.is_some() {
        mine.clone()
    } else {
        loaded.settings
    };
    let r = f(&mut merged);
    normalize(&mut merged);
    save(dir, &merged)?;
    Ok((merged, r))
}

/// F247:三方合并 —— 把「`mine` 相对 `base` 真的动过的那些项」嫁接到 `theirs` 上,
/// 其余留 `theirs` 的。零 IO,纯函数。
///
/// 用在设置弹窗点「确定」那一刻:`base` = 开窗那一瞬的快照(`settings_backup`),
/// `mine` = 用户的草稿,`theirs` = 盘上现在这份。用户在弹窗里没碰过的项,盘上
/// 那份(可能是另一个实例刚写的)照旧保留。
///
/// **在 toml 表层比,不逐字段列举**:`Settings` 是普通结构体,Rust 里没法不写
/// 宏就遍历它的字段;列举式的比对在加字段时必然漏(本仓库已踩过三次),而漏掉
/// 的那一项表现为「弹窗里改了、点确定没生效」,零报错。
///
/// **必须遍历 `base ∪ mine` 的键,「键消失」也算一种值**:`font_family` 是
/// `Option`、`local_bookmarks` 带 `skip_serializing_if = "Vec::is_empty"` ——
/// 用户在弹窗里清空字体族、或把收藏夹删到一条不剩,在 toml 层的表现都是
/// **这个键没了**。只遍历 `mine` 的键会把这两种「真的动过」静默丢掉。
pub fn graft_changed(base: &Settings, mine: &Settings, theirs: &mut Settings) {
    let (Some(b), Some(m), Some(mut t)) = (to_table(base), to_table(mine), to_table(theirs)) else {
        // 走不到:三份都是构造良好的 `Settings`。留着是为了以后加字段时
        // 这里退化成「用户的草稿赢」,而不是 panic。
        *theirs = mine.clone();
        return;
    };
    let keys: std::collections::BTreeSet<&String> = b.keys().chain(m.keys()).collect();
    for k in keys {
        match (b.get(k), m.get(k)) {
            // 弹窗里没动过这一项 → 保留盘上那份(可能是另一个实例刚写的)
            (x, y) if x == y => {}
            (_, Some(v)) => {
                t.insert(k.clone(), v.clone());
            }
            // 用户把它清空了 —— 同样是一次改动
            (_, None) => {
                t.remove(k);
            }
        }
    }
    // 同上:表里每个值都是从构造良好的 `Settings` 序列化出来的,反序列化不会败。
    *theirs = toml::Value::Table(t)
        .try_into()
        .unwrap_or_else(|_| mine.clone());
}

fn to_table(s: &Settings) -> Option<toml::Table> {
    match toml::Value::try_from(s) {
        Ok(toml::Value::Table(t)) => Some(t),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().expect("建临时目录")
    }

    #[test]
    fn settings_survive_a_round_trip() {
        let dir = tmp();
        let s = Settings {
            schema_version: CURRENT_SETTINGS_SCHEMA,
            font_family: Some("Cascadia Mono".to_string()),
            font_pt: 13.5,
            tmux_bootstrap: true,
            ..Settings::default()
        };
        save(dir.path(), &s).expect("写盘");
        let back = load(dir.path());
        assert!(back.note.is_none(), "干净的文件不该有 note:{:?}", back.note);
        assert_eq!(back.settings, s);
    }

    /// 没有设置文件 = 默认外观,**且不该有 note** —— 首次启动是正常情况,
    /// 不是异常。
    #[test]
    fn a_missing_file_is_not_an_anomaly() {
        let dir = tmp();
        let back = load(dir.path());
        assert_eq!(back.settings, Settings::default());
        assert!(back.note.is_none());
    }

    /// 手改坏的 TOML:落回默认 + 一句说明,**不阻断启动**。
    #[test]
    fn a_broken_file_degrades_to_defaults_with_a_note() {
        let dir = tmp();
        std::fs::write(dir.path().join(SETTINGS_FILE), "这不是 toml { [").expect("写坏文件");
        let back = load(dir.path());
        assert_eq!(back.settings, Settings::default());
        assert!(back.note.is_some(), "解析失败必须留一句说明");
    }

    /// 未来版本写的文件不猜着读。
    #[test]
    fn a_newer_schema_is_refused_rather_than_guessed() {
        let dir = tmp();
        std::fs::write(
            dir.path().join(SETTINGS_FILE),
            b"schema_version = 99\nfont_pt = 20.0\n",
        )
        .expect("写文件");
        let back = load(dir.path());
        assert_eq!(back.settings, Settings::default());
        assert!(back.note.is_some());
    }

    /// 字号夹紧的三个边界。**NaN 落回默认**是这条测试的重点:`f32::clamp`
    /// 遇 NaN 返回 NaN,照抄它会让 NaN 一路进 wgpu 的尺寸计算。
    ///
    /// 自证会变红:把 `clamp_font_pt` 的 `is_finite` 分支删掉。
    #[test]
    fn an_out_of_range_size_is_pulled_back_into_the_usable_band() {
        assert_eq!(clamp_font_pt(0.5), MIN_FONT_PT);
        assert_eq!(clamp_font_pt(999.0), MAX_FONT_PT);
        assert_eq!(clamp_font_pt(f32::NAN), DEFAULT_FONT_PT);
        assert_eq!(clamp_font_pt(f32::INFINITY), DEFAULT_FONT_PT);
        assert_eq!(clamp_font_pt(12.0), 12.0, "区间内的值不许被动");
    }

    /// **夹紧发生在 `load` 里**,不是留给调用方。漏夹一处,坏值就直接进渲染
    /// 路径,而那里的症状看起来完全不像「设置文件被改坏了」。
    ///
    /// 自证会变红:把 `load` 结尾那句 `settings.font_pt = clamp_font_pt(..)`
    /// 删掉。
    #[test]
    fn a_hand_edited_size_is_already_clamped_when_it_comes_out_of_load() {
        let dir = tmp();
        std::fs::write(
            dir.path().join(SETTINGS_FILE),
            b"schema_version = 1\nfont_pt = 0.25\n",
        )
        .expect("写文件");
        assert_eq!(load(dir.path()).settings.font_pt, MIN_FONT_PT);
    }

    /// 空族名等价于「没设」:否则设置里显示一个空框、画面上是默认字体,
    /// 两边对不上账。
    #[test]
    fn an_empty_family_name_means_the_builtin_default() {
        let dir = tmp();
        std::fs::write(
            dir.path().join(SETTINGS_FILE),
            b"schema_version = 1\nfont_family = \"\"\n",
        )
        .expect("写文件");
        assert_eq!(load(dir.path()).settings.font_family, None);
    }

    /// 缺字段的老文件(或手写的最小文件)按默认补齐,不报错。
    #[test]
    fn a_partial_file_fills_the_rest_in_from_the_defaults() {
        let dir = tmp();
        std::fs::write(dir.path().join(SETTINGS_FILE), b"schema_version = 1\n").expect("写文件");
        let back = load(dir.path());
        assert!(back.note.is_none());
        assert_eq!(back.settings.font_pt, DEFAULT_FONT_PT);
        assert_eq!(back.settings.font_family, None);
    }

    /// F124:自举开关默认**开**。这是新字段,老的 settings.toml 里没有它 ——
    /// `serde(default)` 必须给 `true`,给 `false` 的话所有老用户升上来功能
    /// 静默不生效,而他们在设置里看到的是「开着」。
    ///
    /// 自证会变红:把 `default_tmux_bootstrap` 的返回值改成 `false`。
    #[test]
    fn tmux_bootstrap_defaults_to_on_for_files_written_before_it_existed() {
        let dir = tmp();
        std::fs::write(
            dir.path().join(SETTINGS_FILE),
            "schema_version = 1\nfont_pt = 10.0\n",
        )
        .expect("写老格式文件");
        let back = load(dir.path());
        assert!(back.note.is_none(), "老文件不该有 note:{:?}", back.note);
        assert!(back.settings.tmux_bootstrap, "老文件缺这个字段时该默认开");
    }

    /// F253:隐藏项开关默认**开**(= 显示 `.` 开头的项)。老的 settings.toml
    /// 里没有这个字段,`serde(default)` 给 `false` 的话所有老用户升上来还是
    /// 看不见 `.claude/`,而这正是这一条要修的病。
    ///
    /// 自证会变红:把 `default_show_hidden_files` 的返回值改成 `false`。
    #[test]
    fn show_hidden_files_defaults_to_on_for_files_written_before_it_existed() {
        let dir = tmp();
        std::fs::write(
            dir.path().join(SETTINGS_FILE),
            "schema_version = 1\nfont_pt = 10.0\n",
        )
        .expect("写老格式文件");
        let back = load(dir.path());
        assert!(back.note.is_none(), "老文件不该有 note:{:?}", back.note);
        assert!(
            back.settings.show_hidden_files,
            "老文件缺这个字段时该默认显示隐藏项"
        );
    }

    /// F253:关掉之后要能存住。光有上一条的话,「读不出用户关过」这种错法
    /// 照样全绿(同 `tmux_bootstrap` 那一对的理由)。
    #[test]
    fn show_hidden_files_survives_a_round_trip_when_turned_off() {
        let dir = tmp();
        let s = Settings {
            show_hidden_files: false,
            ..Settings::default()
        };
        save(dir.path(), &s).expect("写盘");
        assert!(!load(dir.path()).settings.show_hidden_files);
    }

    /// 关掉之后要能存住 —— 默认值是 `true`,写盘再读回必须仍是 `false`。
    /// 光有上一条(缺字段时默认开)的话,「读不出用户关过」这种错法照样全绿。
    #[test]
    fn tmux_bootstrap_survives_a_round_trip_when_turned_off() {
        let dir = tmp();
        let s = Settings {
            tmux_bootstrap: false,
            ..Settings::default()
        };
        save(dir.path(), &s).expect("写盘");
        assert!(!load(dir.path()).settings.tmux_bootstrap);
    }

    /// F156-c:老的 `settings.toml` 里没有这个字段,读出来必须**默认开** ——
    /// 关着的话,所有已经在用的用户升级上来之后,非 tmux 场景仍然跟不住目录,
    /// 而他们不会知道设置里多了一个开关。
    ///
    /// 自证会变红:把 `default_shell_osc7_bootstrap` 的返回值改成 `false`。
    #[test]
    fn shell_osc7_bootstrap_defaults_to_on_for_files_written_before_it_existed() {
        let dir = tmp();
        std::fs::write(
            dir.path().join(SETTINGS_FILE),
            "schema_version = 1\nfont_pt = 10.0\n",
        )
        .expect("写老格式文件");
        let back = load(dir.path());
        assert!(back.note.is_none(), "老文件不该有 note:{:?}", back.note);
        assert!(
            back.settings.shell_osc7_bootstrap,
            "老文件缺这个字段时该默认开"
        );
        // `impl Default` 那一行单独钉一次:上面那条走的是 `serde(default)`
        // 函数,`Settings::default()` 是另一条路 —— 全新用户(没有
        // settings.toml)走的正是它。两条都写着 `true`,但没人保证下次有人
        // 改的时候两条一起改。
        assert!(
            Settings::default().shell_osc7_bootstrap,
            "全新用户拿到的默认值该是开的"
        );
    }

    /// 关掉之后要真的留得住 —— 这条命令是往用户当前这条 shell 里写东西并
    /// 清屏,「关了下次又自己开回来」是不能接受的。
    #[test]
    fn shell_osc7_bootstrap_survives_a_round_trip_when_turned_off() {
        let dir = tmp();
        let s = Settings {
            shell_osc7_bootstrap: false,
            ..Settings::default()
        };
        save(dir.path(), &s).expect("写盘");
        assert!(!load(dir.path()).settings.shell_osc7_bootstrap);
    }

    /// 新字段:老的 settings.toml 里没有它,缺省必须是 `Info`。
    ///
    /// 给 `Debug` 的话所有老用户升上来日志量暴涨、盘被写满;给 `Error` 的话
    /// 他们的日志静默变空,而设置里显示的是另一回事。
    ///
    /// 自证会变红:把 `default_log_level` 的返回值改成 `LogLevel::Debug`。
    #[test]
    fn log_level_defaults_to_info_for_files_written_before_it_existed() {
        let dir = tmp();
        std::fs::write(
            dir.path().join(SETTINGS_FILE),
            "schema_version = 1\nfont_pt = 10.0\n",
        )
        .expect("写老格式文件");
        let back = load(dir.path());
        assert!(back.note.is_none(), "老文件不该有 note:{:?}", back.note);
        assert_eq!(back.settings.log_level, LogLevel::Info);
    }

    /// 改过的档位要能存住。光有上一条的话,「读不出用户改过」这种错法全绿。
    #[test]
    fn a_changed_log_level_survives_a_round_trip() {
        let dir = tmp();
        for lv in [LogLevel::Error, LogLevel::Info, LogLevel::Debug] {
            let s = Settings {
                log_level: lv,
                ..Settings::default()
            };
            save(dir.path(), &s).expect("写盘");
            assert_eq!(
                load(dir.path()).settings.log_level,
                lv,
                "档位 {lv:?} 没存住"
            );
        }
    }

    /// 手改成不认识的档位名 → 整份设置回落到默认值,**但要带一句 note**。
    ///
    /// 这里钉的是「降级要出声」,不是「只有 log_level 受影响」——`load()` 的
    /// 解析失败分支是整份回落,字体、tmux 开关会一起丢。走的也是
    /// `a_broken_file_degrades_to_defaults_with_a_note` 的同一处代码,
    /// 留着它是因为「用户手改错档位名」是这条路径最可能被真实触发的方式。
    #[test]
    fn an_unknown_level_name_degrades_loudly_instead_of_silently() {
        let dir = tmp();
        std::fs::write(
            dir.path().join(SETTINGS_FILE),
            "schema_version = 1\nlog_level = \"verbose\"\n",
        )
        .expect("写文件");
        let back = load(dir.path());
        assert_eq!(back.settings.log_level, LogLevel::Info);
        assert!(back.note.is_some(), "档位名不认识却一声不吭");
    }

    fn mark(name: &str, path: &str) -> crate::sftp::Bookmark {
        crate::sftp::Bookmark {
            name: name.into(),
            path: path.into(),
        }
    }

    /// F187:本地收藏夹存在**全局**设置里,写盘再读回还在。
    ///
    /// 这是本片的正事 —— 改之前它挂在 `SessionRecord` 下,同一个 `D:\work`
    /// 要在每条会话里各收一次。
    #[test]
    fn local_bookmarks_are_global_and_survive_a_round_trip() {
        let dir = tmp();
        let mut s = Settings::default();
        s.add_local_bookmark(mark("工程", r"D:\work"));
        s.add_local_bookmark(mark("另一个名字", r"D:\work")); // 同路径 → 去重
        s.add_local_bookmark(mark("下载", r"C:\Users\me\Downloads"));
        save(dir.path(), &s).expect("写盘");

        let back = load(dir.path()).settings;
        assert_eq!(back.local_bookmarks.len(), 2, "同一路径收两次该去重");
        assert_eq!(back.local_bookmarks[0].path, r"D:\work");
        assert_eq!(back.local_bookmarks[0].name, "工程", "去重该留先来的那条");
        assert_eq!(back.local_bookmarks[1].path, r"C:\Users\me\Downloads");
    }

    /// 取消收藏按路径匹配,且**要留得住** —— 光有上一条的话,「remove 是空
    /// 实现」这种错法照样全绿。
    #[test]
    fn removing_a_local_bookmark_sticks_across_a_round_trip() {
        let dir = tmp();
        let mut s = Settings::default();
        s.add_local_bookmark(mark("工程", r"D:\work"));
        s.add_local_bookmark(mark("下载", r"C:\dl"));
        s.remove_local_bookmark(r"D:\work");
        save(dir.path(), &s).expect("写盘");
        let back = load(dir.path()).settings;
        assert_eq!(back.local_bookmarks.len(), 1);
        assert_eq!(back.local_bookmarks[0].path, r"C:\dl");
    }

    /// F187 迁移:老库里各会话名下的本地书签并进来一次,**跨路径去重**。
    ///
    /// 自证会变红:把 `merge_local_bookmarks` 里的 `add_local_bookmark`
    /// 换成 `self.local_bookmarks.push(mark)`(第二条断言:两条会话各收过
    /// 同一个 `D:\work`,不去重就会并出两条)。
    #[test]
    fn the_old_per_session_lists_are_merged_in_once() {
        let mut s = Settings::default();
        let n = s.merge_local_bookmarks([
            mark("工程", r"D:\work"),
            mark("下载", r"C:\dl"),
            // 另一条会话下重复收过的同一个目录
            mark("工程", r"D:\work"),
        ]);
        assert_eq!(n, 2, "并进来两条(第三条与第一条同路径)");
        assert!(s.local_bookmarks_migrated, "迁移完必须置标记");
    }

    /// **迁移只做一次。** 已经并过之后,用户取消掉的收藏不许从没清理的会话
    /// 记录里长回来 —— 那是一个删不掉的收藏,而且看不出原因。
    ///
    /// 自证会变红:删掉 `merge_local_bookmarks` 开头那个
    /// `if self.local_bookmarks_migrated { return 0; }`。
    #[test]
    fn a_bookmark_the_user_deleted_does_not_grow_back_on_the_next_launch() {
        let mut s = Settings::default();
        s.merge_local_bookmarks([mark("工程", r"D:\work")]);
        s.remove_local_bookmark(r"D:\work");
        // 下次启动:会话库里那份老数据还在(我们不改 sessions.toml)。
        let n = s.merge_local_bookmarks([mark("工程", r"D:\work")]);
        assert_eq!(n, 0);
        assert!(
            s.local_bookmarks.is_empty(),
            "取消掉的收藏又长回来了 —— 用户删不掉它,也看不出为什么"
        );
    }

    /// 老的 `settings.toml`(没有这两个键)读得进来:空列表 + **未迁移**。
    ///
    /// 标记要是默认 `true`,所有老用户的本地收藏在升级那一刻静默清零 ——
    /// 而这正是用户报的第 1 条。
    #[test]
    fn a_settings_file_written_before_local_bookmarks_existed_is_not_yet_migrated() {
        let dir = tmp();
        std::fs::write(
            dir.path().join(SETTINGS_FILE),
            "schema_version = 1\nfont_pt = 10.0\n",
        )
        .expect("写老格式文件");
        let back = load(dir.path());
        assert!(back.note.is_none(), "老文件不该有 note:{:?}", back.note);
        assert!(back.settings.local_bookmarks.is_empty());
        assert!(
            !back.settings.local_bookmarks_migrated,
            "老文件必须被当成「还没迁移」,否则升级那一刻本地收藏静默清零"
        );
    }

    /// F247:另一个实例刚写下的东西,必须在我这次写盘之后还在。
    ///
    /// 这是本条存在的全部理由:整份覆盖时 B 的字号改动会把 A 刚点的☆ 抹掉。
    ///
    /// 自证会变红:把 `mutate` 里的 `load(dir)` 换成 `mine.clone()`。
    #[test]
    fn what_another_instance_just_wrote_survives_my_write() {
        let dir = tmp();
        // A:点了一颗☆(盘上现在有它)
        let mut a = Settings::default();
        a.add_local_bookmark(mark("工程", r"D:\work"));
        save(dir.path(), &a).expect("A 写盘");

        // B:内存里那份还是 A 点☆ **之前**的样子,现在改字号
        let b_memory = Settings::default();
        let (merged, ()) = mutate(dir.path(), &b_memory, |s| s.font_pt = 14.0).expect("B 写盘");

        assert_eq!(merged.font_pt, 14.0, "B 自己改的没生效");
        assert_eq!(
            merged.local_bookmarks.len(),
            1,
            "A 刚点的☆ 被 B 抹掉了 —— 这正是多实例互相吃改动的那个 bug"
        );
        // 盘上那份也要一致,不能只是返回值好看
        assert_eq!(load(dir.path()).settings, merged);
    }

    /// F247:闭包的返回值原样透出 —— F187 的迁移条数要拿它写日志,吞掉的话
    /// 那行日志永远报 0 条,而迁移本身是静默的。
    ///
    /// 自证会变红:把 `mutate` 的 `Ok((merged, r))` 改成返回一个新造的默认值。
    #[test]
    fn the_closure_result_comes_back_out() {
        let dir = tmp();
        let (_, n) = mutate(dir.path(), &Settings::default(), |s| {
            s.merge_local_bookmarks([mark("工程", r"D:\work"), mark("下载", r"C:\dl")])
        })
        .expect("写盘");
        assert_eq!(n, 2);
    }

    /// F247:**盘上那份读不出来时,底必须是调用方内存里那份,不是默认值。**
    ///
    /// `load` 永不失败,手改坏的文件会静默降级成 `Settings::default()`。拿它
    /// 当底做读-改-写,用户的字体和全部☆ 会在下一次点☆ 时整份消失 —— 比这条
    /// 要修的「整份覆盖」更狠。
    ///
    /// 自证会变红:把 `mutate` 里那个 `loaded.note.is_some()` 分支删掉。
    #[test]
    fn a_broken_file_on_disk_does_not_wipe_what_i_have_in_memory() {
        let dir = tmp();
        std::fs::write(dir.path().join(SETTINGS_FILE), "这不是 toml { [").expect("写坏文件");

        let mut mine = Settings {
            font_family: Some("Cascadia Mono".to_string()),
            ..Settings::default()
        };
        mine.add_local_bookmark(mark("工程", r"D:\work"));

        let (merged, ()) = mutate(dir.path(), &mine, |s| s.font_pt = 14.0).expect("写盘");
        assert_eq!(
            merged.font_family.as_deref(),
            Some("Cascadia Mono"),
            "盘上文件坏了就把用户的字体抹成默认 —— 无法自愈"
        );
        assert_eq!(merged.local_bookmarks.len(), 1, "☆ 也一起没了");
        assert_eq!(merged.font_pt, 14.0);
    }

    /// F247:`mutate` 交出去的那份 = 盘上那份。夹紧/版本号只在 `save` 里做的话,
    /// 调用方拿回去的是没夹过的值,内存与磁盘从此分叉。
    ///
    /// 自证会变红:把 `mutate` 里的 `normalize(&mut merged)` 删掉。
    #[test]
    fn what_mutate_hands_back_is_what_is_on_disk() {
        let dir = tmp();
        let (merged, ()) =
            mutate(dir.path(), &Settings::default(), |s| s.font_pt = 999.0).expect("写盘");
        assert_eq!(merged.font_pt, MAX_FONT_PT);
        assert_eq!(load(dir.path()).settings, merged);
    }

    /// F247:弹窗里没动过的项留盘上那份,动过的用用户的。
    ///
    /// 自证会变红:把 `graft_changed` 里 `(x, y) if x == y => {}` 那一臂删掉
    /// (于是整份草稿覆盖盘上那份,另一个实例的改动全没)。
    #[test]
    fn the_dialog_only_grafts_what_the_user_actually_touched() {
        let base = Settings::default();
        // 用户在弹窗里只改了字号
        let mine = Settings {
            font_pt: 14.0,
            ..base.clone()
        };
        // 与此同时,另一个实例点了一颗☆、还换了日志档
        let mut theirs = Settings {
            log_level: LogLevel::Debug,
            ..base.clone()
        };
        theirs.add_local_bookmark(mark("工程", r"D:\work"));

        graft_changed(&base, &mine, &mut theirs);

        assert_eq!(theirs.font_pt, 14.0, "用户改的字号没生效");
        assert_eq!(theirs.log_level, LogLevel::Debug, "别人换的日志档被抹掉了");
        assert_eq!(theirs.local_bookmarks.len(), 1, "别人点的☆ 被抹掉了");
    }

    /// F247:**「清空」也是一种改动。**`font_family` 是 `Option`、
    /// `local_bookmarks` 带 `skip_serializing_if`,两者清空后在 toml 层的表现
    /// 都是**键消失** —— 只遍历 `mine` 的键的话,用户在弹窗里清掉的东西会原样
    /// 从盘上长回来,而且看不出原因。
    ///
    /// 自证会变红:把 `graft_changed` 的键并集改成只遍历 `m.keys()`。
    #[test]
    fn clearing_a_field_in_the_dialog_is_a_change_too() {
        let mut base = Settings {
            font_family: Some("Cascadia Mono".to_string()),
            ..Settings::default()
        };
        base.add_local_bookmark(mark("工程", r"D:\work"));

        // 用户在弹窗里把字体族清了、☆ 也删光了
        let mine = Settings {
            font_family: None,
            local_bookmarks: Vec::new(),
            ..base.clone()
        };
        let mut theirs = base.clone();

        graft_changed(&base, &mine, &mut theirs);

        assert_eq!(theirs.font_family, None, "清掉的字体族又长回来了");
        assert!(theirs.local_bookmarks.is_empty(), "删光的☆ 又长回来了");
    }

    /// 档位的磁盘写法是小写英文单词 —— 这是要被人手改的文件,形态本身是契约。
    #[test]
    fn levels_are_written_as_lowercase_words() {
        let s = Settings {
            log_level: LogLevel::Debug,
            ..Settings::default()
        };
        let text = toml::to_string_pretty(&s).expect("序列化");
        assert!(text.contains("log_level = \"debug\""), "写法变了:\n{text}");
    }
}
