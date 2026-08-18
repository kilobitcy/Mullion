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
}

fn default_font_pt() -> f32 {
    DEFAULT_FONT_PT
}

fn default_tmux_bootstrap() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SETTINGS_SCHEMA,
            font_family: None,
            font_pt: DEFAULT_FONT_PT,
            tmux_bootstrap: true,
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

/// 原子写 `<dir>/settings.toml`。
///
/// **写失败要往上报**(与 `layout::save` 相反):布局是自动存的「上次的场景」,
/// 设置是用户刚点了确定的显式动作,静默失败 = 改了没生效、而他不知道为什么。
pub fn save(dir: &Path, settings: &Settings) -> Result<(), StoreError> {
    std::fs::create_dir_all(dir)?;
    let mut out = settings.clone();
    out.schema_version = CURRENT_SETTINGS_SCHEMA;
    out.font_pt = clamp_font_pt(out.font_pt);
    let text = toml::to_string_pretty(&out)?;
    crate::vault::write_atomic(&dir.join(SETTINGS_FILE), text.as_bytes())
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
}
