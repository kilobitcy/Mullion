//! F119 表单规范的机械守护：扫源码，挡住新写的裸数字。
//!
//! 判据是「参数不得是数字字面量」，不是「只允许某几个名字」——
//! `mod.rs` 的凭据输入框是先 `let w = field_w(...)` 再 `desired_width(w)`，
//! 白名单式判据会把最规范的写法反而拦下。
//!
//! **这道网挡不住什么**（写在这里，免得有人以为它是全部保障）：
//! `let w = 80.0; desired_width(w)` 绕得过去；「用了常量但选错档位」
//! （该 `FIELD_W_S` 却写 `FIELD_W_L`）它看不出来；下面 `ALLOW` 里的
//! 白名单行也挡不住——它登记的是既有欠债，不是「加进来就绿了」的许可
//! （理由见 `ALLOW` 的文档注释）。那三类只能靠评审。
//! 规范全文见 `docs/ui-form-guidelines.md`；上面这三类局限与该文档
//! 「机械守护挡不住什么」一节是同一份内容，改一处要同步另一处。

use std::path::Path;

/// 扫描范围：会话管理器的全部 UI 源码。
const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/ui/session_manager");

/// 既有违规的行级白名单。
///
/// **不是「加进来就绿了」的口袋。** 只允许一种理由：这个数值不在
/// `SP_*` 五档刻度上，改成最近的档位会带来**未经人工验收的视觉变化**，
/// 而本切片的范围是布局重排、不是调间距（Scope Discipline）。
/// 每条都写明理由，下次视觉走查时清空。
///
/// 匹配的是**行内容**（trim 后），不是行号 —— 行号会随任何编辑漂移。
const ALLOW: &[(&str, &str, &str)] = &[
    (
        "tunnel_list.rs",
        "ui.add_space(2.0);",
        "行内紧凑间距，2.0 不在五档上；等下次视觉走查统一",
    ),
    (
        "mod.rs",
        "ui.add_space(6.0);",
        "模式条与双栏之间的呼吸，6.0 不在五档上；同上",
    ),
    (
        "editor.rs",
        "ui.add_space(6.0);",
        "标题条/错误卡片下方的呼吸，6.0 不在五档上；同上",
    ),
];

fn is_allowed(file: &str, line: &str) -> bool {
    ALLOW
        .iter()
        .any(|(f, l, _)| *f == file && line.trim() == *l)
}

/// 找出 `needle(` 后面紧跟数字字面量的行。返回 `(文件名, 行号, 行内容)`。
///
/// 只扫**渲染代码**：碰到 `#[cfg(test)]` 就停 —— 测试里为了构造场景写死
/// 尺寸是正当的（`egui::vec2(280.0, 300.0)` 之类），规范管的是产品代码。
fn scan(src: &str, file: &str, needle: &str) -> Vec<(String, usize, String)> {
    let mut out = Vec::new();
    for (i, line) in src.lines().enumerate() {
        // `break` 而非「跳过这一行」，隐含一条不变式：第一个
        // `#[cfg(test)]` 之后全是测试代码。今天成立(逐文件核实过)，
        // 但 mod.rs 已经有 `tunnel_ui_tests` / `tests` 两个独立的顶层
        // 测试模块——说明开发者确实会为逻辑切分多开测试模块。谁要是
        // 在两个测试模块之间、或最后一个测试模块之后插回产品代码，
        // 这套扫描会静默失明：那段代码不会被扫到，测试照样绿。
        if line.trim_start().starts_with("#[cfg(test)]") {
            break;
        }
        // 注释行不算 —— 文档里举反例是正常的。
        if line.trim_start().starts_with("//") {
            continue;
        }
        let mut rest = line;
        while let Some(p) = rest.find(needle) {
            let after = &rest[p + needle.len()..];
            if after.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                out.push((file.to_string(), i + 1, line.to_string()));
                break;
            }
            rest = after;
        }
    }
    out
}

fn each_source(mut f: impl FnMut(&str, &str)) {
    for entry in std::fs::read_dir(Path::new(DIR)).expect("扫描目录读不开") {
        let path = entry.expect("目录项读不出").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("文件名不是 UTF-8")
            .to_string();
        let src = std::fs::read_to_string(&path).expect("源码读不出");
        f(&name, &src);
    }
}

/// 间距只能用 `SP_XS/S/M/L/XL` 五档。裸数字让「这一处该多松」变成各人各拍
/// 脑袋，表单一路铺下来就没有节奏可言。
///
/// 自证会变红：把任意一处 `ui.add_space(SP_S)` 改回 `ui.add_space(8.0)`。
#[test]
fn no_bare_numeric_spacing_in_session_manager_ui() {
    let mut bad = Vec::new();
    each_source(|name, src| {
        for (f, line_no, line) in scan(src, name, "add_space(") {
            if !is_allowed(&f, &line) {
                bad.push(format!("{f}:{line_no}: {}", line.trim()));
            }
        }
    });
    assert!(
        bad.is_empty(),
        "间距必须用 SP_* 五档（见 docs/ui-form-guidelines.md）：\n{}",
        bad.join("\n")
    );
}

/// 输入框宽度必须过 `field_w`（扣预留 → 取上限 → 夹下界，三步缺一不可，
/// 理由见 metrics.rs）。硬编码宽度在右栏被拖宽/拖窄时一定错。
///
/// 自证会变红：把任意一处 `desired_width(field_w(...))` 改回 `desired_width(80.0)`。
#[test]
fn no_bare_numeric_field_width_in_session_manager_ui() {
    let mut bad = Vec::new();
    each_source(|name, src| {
        for (f, line_no, line) in scan(src, name, "desired_width(") {
            if !is_allowed(&f, &line) {
                bad.push(format!("{f}:{line_no}: {}", line.trim()));
            }
        }
    });
    assert!(
        bad.is_empty(),
        "输入框宽度必须过 field_w / FIELD_W_*（见 docs/ui-form-guidelines.md）：\n{}",
        bad.join("\n")
    );
}
