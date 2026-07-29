//! 多行粘贴确认弹窗(F18)。
//!
//! 只在「粘贴内容多于一行(按 [`mullion_term::keymap::paste_line_count`] 的口径,
//! 尾随的单个换行不算)**且** 远端没开 bracketed paste」时出现:这种组合下每个
//! 换行都会被远端当回车执行,一次误贴能连着跑好几条命令。其余情况直接粘贴——
//! 在 Claude Code 里贴代码(bracketed 已开)必须无感,从浏览器/IDE 复制带尾随
//! 换行的单条命令也必须无感。
//!
//! 与主机密钥弹窗(`host_key.rs`,故意不给关闭按钮)相反,这个窗**可以取消**:
//! 取消 = 不粘贴,是明确且安全的默认,没有「以为没事发生、其实还挂着」的歧义。

/// 预览最多显示几行。
const PREVIEW_LINES: usize = 5;
/// 每行预览最多显示几个字符。一行几万字符(minified js / base64)同样能撑爆窗。
const PREVIEW_COLS: usize = 120;

/// 生成预览文本与总行数。纯函数,可单测。
///
/// 行数口径与 [`mullion_term::keymap::paste_line_count`] 同源(该函数 doc 有
/// 完整理由):裸 `\r` 也算一次回车,尾随的最后一个换行不计——弹窗报的行数
/// 必须等于远端实际执行的回车数,少报比不报更糟。分行显示同样按归一后的
/// `\r` 切,否则含裸 `\r` 的内容会被 `str::lines()` 挤成一行显示。
pub fn preview_of(text: &str) -> (String, usize) {
    let total = mullion_term::keymap::paste_line_count(text);
    if total == 0 {
        return (String::new(), 0);
    }
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    let body = normalized.strip_suffix('\r').unwrap_or(&normalized);
    let mut out = String::new();
    for line in body.split('\r').take(PREVIEW_LINES) {
        // 按字符数而非字节数截断:按字节切会把多字节字符切成两半(panic 或乱码)。
        if line.chars().count() > PREVIEW_COLS {
            out.extend(line.chars().take(PREVIEW_COLS));
            out.push('…');
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if total > PREVIEW_LINES {
        out.push_str(&format!("… 还有 {} 行", total - PREVIEW_LINES));
    }
    (out, total)
}

/// 弹窗要展示的只读视图。借用式,与 `host_key::HostKeyView` 同构。
#[derive(Clone, Copy)]
pub struct PasteView<'a> {
    pub text: &'a str,
}

/// 画弹窗。用户做出选择时把 `Some(accept)` 写进 `reply`,由 app.rs 事后施加
/// (取出 `pending_paste` 并发送)——egui 闭包里借不到 `&mut App`。
///
/// 用 `egui::Modal` 而非 `egui::Window`:普通 `Window` 不挡下层点击,弹窗开着时
/// 用户不该还能往终端里打字(与 F3 主机密钥弹窗同一理由)。
pub fn show(ctx: &egui::Context, view: &PasteView<'_>, reply: &mut Option<bool>) {
    let (preview, total) = preview_of(view.text);
    let resp = egui::Modal::new(egui::Id::new("paste_confirm")).show(ctx, |ui| {
        // Modal 没有标题栏(不像 Window),标题得自己画。
        ui.heading("确认粘贴多行内容");
        ui.separator();
        ui.label(format!(
            "剪贴板里有 {total} 行。远端没有开启 bracketed paste,\
             每个换行都会被当成回车直接执行。"
        ));
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("paste_preview")
            .max_height(160.0)
            .show(ui, |ui| {
                ui.monospace(preview);
            });
        ui.separator();
        ui.horizontal(|ui| {
            // 「取消」放最左(默认位),同 host_key.rs 变更态的理由:防止用户
            // 用点惯了「安全默认」的肌肉记忆误点到有风险的那个按钮。
            if ui.button("取消").clicked() {
                *reply = Some(false);
            }
            if ui.button("粘贴").clicked() {
                *reply = Some(true);
            }
        });
    });
    // backdrop 点击与 Esc 都走 egui 自己的出口(`ModalResponse::should_close`):
    // 它用 `consume_key` 消费掉 Esc(仅在本 modal 是最顶层且无 popup 打开时),
    // 也覆盖了「点弹窗外面」——手写 `key_pressed` 只补了半边且不消费按键,
    // backdrop 点击会没有任何反应。关闭一律等价于取消:不粘贴是明确且安全的默认。
    if resp.should_close() {
        *reply = Some(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_shows_all_lines_when_short() {
        let (text, total) = preview_of("a\nb\nc");
        assert_eq!(total, 3);
        assert_eq!(text, "a\nb\nc\n");
        assert!(!text.contains("还有"), "行数没超上限不该出现省略提示");
    }

    #[test]
    fn preview_truncates_long_input_and_reports_remainder() {
        // 用户贴进来的可能是几千行的日志,预览必须有上限,否则弹窗把屏幕撑满、
        // 「取消」按钮被挤到窗外——反而点不掉。
        let input = (1..=20)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let (text, total) = preview_of(&input);
        assert_eq!(total, 20);
        assert!(text.starts_with("1\n2\n3\n4\n5\n"));
        assert!(text.contains("还有 15 行"));
        assert!(!text.contains("\n6\n"), "超出上限的行不该出现在预览里");
    }

    #[test]
    fn preview_clips_overlong_single_line() {
        // 一行几万字符(minified js / base64)同样能把窗撑爆。
        let long = "x".repeat(500);
        let (text, total) = preview_of(&long);
        assert_eq!(total, 1);
        assert!(text.chars().count() < 200, "超长行必须截断");
        assert!(text.contains('…'), "截断处要有省略号,别让用户以为就这么多");
    }

    #[test]
    fn preview_line_count_matches_what_remote_will_execute() {
        // 与 keymap::paste_line_count 同源:裸 \r 也算一次回车。
        // 弹窗报的行数必须等于远端实际执行的回车数,少报比不报更糟。
        let (_, total) = preview_of("a\rb\rc\nd");
        assert_eq!(total, 4);
        // 尾随换行不算:单行命令不该被报成多行。
        let (_, total) = preview_of("ls -la\n");
        assert_eq!(total, 1);
    }
}
