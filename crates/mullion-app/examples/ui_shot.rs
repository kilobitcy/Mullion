//! 离屏 UI 截图 harness —— 无头把 egui 外壳渲染成 PNG。
//!
//! **不是产品功能**:example target,不进 exe。存在的理由只有一个 —— 让改 UI
//! 的人在没有显示器的机器上能自己看一眼,而不是每改一版都走
//! 「bump → 交叉编译 → 发 Release → 等人肉眼验」这条长回路。
//!
//! ```bash
//! cargo run -p mullion-app --example ui_shot -- --list
//! cargo run -p mullion-app --example ui_shot -- list-12
//! cargo run -p mullion-app --example ui_shot -- list-12 --size 1024x700 --ppp 1.5
//! ```
//!
//! 渲染后端是 **lavapipe 软件 Vulkan**(见 `docs/ui-shot.md`):真 GPU 要
//! `/dev/dri` 权限,无头机上常常没有,而这里不做像素基线 —— 软渲染的抗锯齿
//! 差异无所谓。
//!
//! **这张图能信什么、不能信什么**:
//! - 能信:布局、层级、控件位置/尺寸、是否溢出/重叠/截断、文字换行。
//! - 不能信:与 Windows 的**像素级**一致(驱动的抗锯齿与浮点舍入不同)。
//! - 中文宽度只在装到 `msyh.ttc` 时可信 —— 判断「挤不挤 / 会不会截断」完全
//!   取决于字体度量,拿 Noto 之类替身出的图会系统性骗人。没装到时程序会在
//!   输出里显式警告,不要在那种图上对中文文本下结论。

use egui_kittest::wgpu::TestRenderer;
use egui_kittest::Harness;
use mullion_app::theme::MULLION_DARK;
use mullion_app::ui::annotate;
use mullion_app::ui::badge::AppearanceCache;
use mullion_app::ui::session_manager::{self, EditorBuffer, SecretPresence};
use mullion_app::ui::UiState;
use mullion_store::{
    Auth, AuthKind, Connection, GroupId, GroupRecord, Identity, Protocol, SessionId, SessionRecord,
};
use std::path::PathBuf;
use std::process::ExitCode;

/// 场景清单。走查时发现新的可疑状态就往这里加一条 —— 加场景比每次手写临时
/// 代码便宜,而且我看到的那一帧你也能用同一条命令复现。
const SCENES: &[(&str, &str)] = &[
    ("empty", "空库:左栏空态占位 + 右栏未选中提示"),
    ("list-12", "12 条会话 / 3 分组,默认密度档"),
    ("long-names", "超长中文会话名 + 长 host:验截断与换行"),
    ("editor-basic", "选中一条并打开编辑器,停在「基础」页"),
    (
        "annotate",
        "F100 标注模式:开着 + 已选中两处(看编号徽标与描边的位置)",
    ),
];

/// 一个场景摊平后的全部入参。`session_manager::show` 是纯数据入参(零 `App`、
/// 零 `SshSession`),所以这里能整份造出来,不需要起窗口也不需要连 SSH。
struct Fixture {
    sessions: Vec<SessionRecord>,
    groups: Vec<GroupRecord>,
    ui: UiState,
    connected: Option<SessionId>,
    presence: SecretPresence,
    store_available: bool,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "--list" || a == "--help") {
        print_usage();
        return ExitCode::SUCCESS;
    }

    let opts = match Opts::parse(&args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("参数错误:{e}");
            return ExitCode::FAILURE;
        }
    };
    if !SCENES.iter().any(|(n, _)| *n == opts.scene) {
        eprintln!("未知场景「{}」。可用场景:", opts.scene);
        print_usage();
        return ExitCode::FAILURE;
    }

    match shoot(&opts) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("渲染失败:{e}");
            ExitCode::FAILURE
        }
    }
}

struct Opts {
    scene: String,
    out: PathBuf,
    size: egui::Vec2,
    ppp: f32,
}

impl Opts {
    fn parse(args: &[String]) -> Result<Self, String> {
        let scene = args[0].clone();
        let mut out = None;
        let mut size = egui::vec2(1280.0, 800.0);
        let mut ppp = 1.0_f32;
        let mut i = 1;
        while i < args.len() {
            let need = |i: usize| -> Result<&String, String> {
                args.get(i + 1).ok_or(format!("{} 后面缺值", args[i]))
            };
            match args[i].as_str() {
                "--out" => out = Some(PathBuf::from(need(i)?)),
                "--ppp" => ppp = need(i)?.parse().map_err(|_| "--ppp 要是数字".to_string())?,
                "--size" => {
                    let v = need(i)?;
                    let (w, h) = v.split_once('x').ok_or("--size 格式是 宽x高")?;
                    let w = w.parse::<f32>().map_err(|_| "--size 的宽不是数字")?;
                    let h = h.parse::<f32>().map_err(|_| "--size 的高不是数字")?;
                    size = egui::vec2(w, h);
                }
                other => return Err(format!("不认识的参数「{other}」")),
            }
            i += 2;
        }
        let out = out.unwrap_or_else(|| PathBuf::from(format!("target/ui-shot/{scene}.png")));
        Ok(Self {
            scene,
            out,
            size,
            ppp,
        })
    }
}

fn print_usage() {
    println!("用法: cargo run -p mullion-app --example ui_shot -- <场景> [--out 路径] [--size 宽x高] [--ppp 倍率]\n");
    println!("场景:");
    for (name, desc) in SCENES {
        println!("  {name:<14} {desc}");
    }
}

fn shoot(opts: &Opts) -> Result<String, String> {
    let (device, queue, adapter_name) = open_device()?;

    let Fixture {
        sessions,
        groups,
        mut ui,
        connected,
        presence,
        store_available,
    } = fixture(&opts.scene);
    let mut appearance = AppearanceCache::default();
    appearance.rebuild(&sessions, &groups);
    let theme = MULLION_DARK;

    // F100 场景:标注模式的 overlay 在产品里由 `build_ui` 末尾调,这里的闭包只画
    // 会话管理器,所以得自己补上那一句 —— 顺序也必须一样(所有 `mark()` 之后)。
    let annotate_scene = opts.scene == "annotate";
    let mut harness = Harness::builder()
        .with_size(opts.size)
        .with_pixels_per_point(opts.ppp)
        .build(|ctx| {
            session_manager::show(
                ctx,
                &theme,
                &mut ui,
                &sessions,
                &groups,
                store_available,
                connected,
                presence,
                &appearance,
            );
            if annotate_scene {
                // 摆出「已选中两处」。按**语义路径**选,不去猜像素坐标:
                // 坐标一改布局就失效,而且图错了分不清是坐标错还是 UI 错。
                // 幂等,可以每帧无脑调;必须在 `overlay` 之前(它会清 spots)。
                annotate::ensure_picked(ctx, "左栏/搜索框");
                annotate::ensure_picked(ctx, "会话行「节点 03」");
                annotate::overlay(
                    ctx,
                    &theme,
                    &annotate::Env {
                        size: ctx.screen_rect().size(),
                        ppp: ctx.pixels_per_point(),
                        theme: "mullion-dark".into(),
                        screen: "会话管理器".into(),
                        extra: vec!["ui_shot 离屏出图".into()],
                    },
                );
            }
        });
    if annotate_scene {
        annotate::toggle(&harness.ctx);
    }

    // 跟产品同一条路(`app.rs` 里 egui ctx 建好之后就调这一句)。**漏了它图就在
    // 骗人**:`session_manager` 里手绘的部分自带主题色,但 egui 自己的部件
    // (按钮、`TextEdit`、`CollapsingHeader` 的三角、滚动条)全走 `Style`,不调
    // 就是 egui 出厂的默认深色,不是这个应用真正长的样子。第一版 harness 漏了
    // 这句,于是滚动条按 egui 默认样式画(静止态 alpha 0),图上完全看不见。
    mullion_app::theme::apply_egui(&harness.ctx, &theme);

    // 字体必须在跑帧之前装:`set_fonts` 只影响之后的帧,第一帧已经按内嵌拉丁
    // 字体排完版了。
    let font = install_dev_cjk_font(&harness.ctx);
    harness.run();

    let image = TestRenderer::create(device, queue).render(&harness);
    if let Some(dir) = opts.out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录 {}: {e}", dir.display()))?;
    }
    image
        .save(&opts.out)
        .map_err(|e| format!("写 {}: {e}", opts.out.display()))?;

    let mut report = format!(
        "场景 {scene} → {out}\n  逻辑尺寸 {w}x{h} @ ppp {ppp} = {pw}x{ph} 像素\n  适配器 {adapter_name}\n  {font}",
        scene = opts.scene,
        out = opts.out.display(),
        w = opts.size.x,
        h = opts.size.y,
        ppp = opts.ppp,
        pw = image.width(),
        ph = image.height(),
    );
    if !font.starts_with("字体") {
        report.push('\n');
    }
    Ok(report)
}

/// 打开一个离屏 wgpu device。
///
/// **不用 `TestRenderer::new()`**:它取 `enumerate_adapters` 的第一个,这台机上
/// 第一个可能是 NVIDIA —— 而 `/dev/dri/renderD128` 属 `render` 组,当前用户
/// 打不开,创建 device 会失败。这里显式把软件光栅器(`DeviceType::Cpu`,即
/// lavapipe)排前面。
fn open_device() -> Result<(wgpu::Device, wgpu::Queue, String), String> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..Default::default()
    });
    let mut adapters = instance.enumerate_adapters(wgpu::Backends::VULKAN);
    if adapters.is_empty() {
        return Err(
            "没枚举到 Vulkan 适配器。装 mesa-vulkan-drivers(lavapipe),\n\
             或显式指定 VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json"
                .into(),
        );
    }
    adapters.sort_by_key(|a| u8::from(a.get_info().device_type != wgpu::DeviceType::Cpu));

    let mut last_err = String::new();
    for adapter in &adapters {
        let info = adapter.get_info();
        match pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("ui_shot"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
            },
            None,
        )) {
            Ok((device, queue)) => {
                return Ok((
                    device,
                    queue,
                    format!("{} ({:?})", info.name, info.device_type),
                ))
            }
            Err(e) => last_err = format!("{} → {e}", info.name),
        }
    }
    Err(format!("所有适配器都打不开,最后一个:{last_err}"))
}

/// 给 harness 装中文字体。
///
/// 与 `ui::install_cjk_font` **同构但不同源**:那个只查 `C:\Windows\Fonts`,在
/// Linux 上直接 return。这里读一份人工放好的 `msyh.ttc`(不入库、不推送),
/// 为的是让字体度量与 Windows 一致 —— 否则中文的宽度/换行/截断全不可信。
fn install_dev_cjk_font(ctx: &egui::Context) -> String {
    let Some(path) = dev_cjk_font_path() else {
        return "⚠ 未装中文字体:图里中文缺字,且**不要**据此判断中文的挤/截断。\n\
                    放一份 msyh.ttc 到 ~/.local/share/mullion-dev-fonts/ 或设 MULLION_UI_SHOT_FONT"
            .to_string();
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => return format!("⚠ 读字体 {} 失败:{e}", path.display()),
    };
    // 从 default 出发,只把 CJK 追加为末位回退 —— 与产品同一姿态,保留内嵌
    // 拉丁字体作主字体,否则拉丁部分的字形也跟着换了,图就更不像实机。
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "dev-cjk".to_owned(),
        std::sync::Arc::new(egui::FontData::from_owned(bytes)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("dev-cjk".to_owned());
    }
    ctx.set_fonts(fonts);
    format!("字体 {}", path.display())
}

fn dev_cjk_font_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("MULLION_UI_SHOT_FONT") {
        let p = PathBuf::from(p);
        return p.exists().then_some(p);
    }
    let p =
        PathBuf::from(std::env::var("HOME").ok()?).join(".local/share/mullion-dev-fonts/msyh.ttc");
    p.exists().then_some(p)
}

fn fixture(scene: &str) -> Fixture {
    let mut ui = UiState {
        session_manager_open: true,
        ..Default::default()
    };
    let groups = vec![group(1, "生产"), group(2, "内网测试"), group(3, "个人 VPS")];
    match scene {
        "empty" => Fixture {
            sessions: Vec::new(),
            groups: Vec::new(),
            ui,
            connected: None,
            presence: SecretPresence::default(),
            store_available: true,
        },
        "long-names" => Fixture {
            sessions: vec![
                sess(
                    1,
                    "生产环境主控节点(华东二区,勿动)",
                    "very-long-hostname.internal.example.com",
                    Some(GroupId(1)),
                ),
                sess(
                    2,
                    "跳板机—堡垒—二次认证",
                    "bastion-01.prod.example.com",
                    Some(GroupId(1)),
                ),
                sess(3, "短名", "h", None),
            ],
            groups,
            ui,
            connected: Some(SessionId(1)),
            presence: SecretPresence::default(),
            store_available: true,
        },
        "editor-basic" => {
            let sessions = many_sessions(12);
            let buf = EditorBuffer {
                name: "生产主控".into(),
                host: "10.0.0.12".into(),
                user: "ubuntu".into(),
                ..Default::default()
            };
            ui.editor_id = Some(SessionId(1));
            ui.editor = Some(buf.clone());
            ui.editor_baseline = Some(buf);
            ui.editor_tab = 0;
            Fixture {
                sessions,
                groups,
                ui,
                connected: Some(SessionId(1)),
                presence: SecretPresence {
                    password: true,
                    ..Default::default()
                },
                store_available: true,
            }
        }
        // "list-12" 以及任何漏网的名字(main 已挡过未知场景)都落到这里。
        _ => Fixture {
            sessions: many_sessions(12),
            groups,
            ui,
            connected: Some(SessionId(3)),
            presence: SecretPresence::default(),
            store_available: true,
        },
    }
}

fn many_sessions(n: u64) -> Vec<SessionRecord> {
    (1..=n)
        .map(|i| {
            let group = match i % 4 {
                0 => None,
                k => Some(GroupId(k)),
            };
            sess(
                i,
                &format!("节点 {i:02}"),
                &format!("10.0.{}.{}", i / 8, 10 + i),
                group,
            )
        })
        .collect()
}

fn sess(id: u64, name: &str, host: &str, group: Option<GroupId>) -> SessionRecord {
    SessionRecord {
        id: SessionId(id),
        modified_at: "2026-08-09T12:00:00Z".into(),
        identity: Identity {
            name: name.into(),
            note: String::new(),
            group_id: group,
            tags: Vec::new(),
        },
        connection: Connection {
            host: host.into(),
            port: 22,
            protocol: Protocol::Ssh,
        },
        auth: Auth {
            user: "ubuntu".into(),
            kind: AuthKind::PublicKey {
                has_passphrase: false,
            },
        },
        terminal: Default::default(),
        appearance: Default::default(),
        network: Default::default(),
        automation: Default::default(),
    }
}

fn group(id: u64, name: &str) -> GroupRecord {
    GroupRecord {
        id: GroupId(id),
        name: name.into(),
        tags: Vec::new(),
        terminal: Default::default(),
        appearance: Default::default(),
        network: Default::default(),
        automation: Default::default(),
    }
}
