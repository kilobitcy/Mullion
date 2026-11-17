//! 系统剪贴板(F18)。薄封装,把「失败」这件事一次性处理掉。
//!
//! 不用 egui 的剪贴板:egui 只有 `copy_text`,**读剪贴板只能靠 `Event::Paste`
//! 且要 egui 持有焦点**。按 T8 的教训(egui 焦点系统吞掉 Tab,终端永久收不到键),
//! 不让 egui 掺和终端输入路径。
//!
//! 所有失败一律 `log::warn!` + 忽略:Windows 上剪贴板被别的进程短暂占用是常态,
//! 复制失败最多是用户再选一次,不值得弹窗打断,更不值得 panic。

/// 系统剪贴板句柄。打开失败时内部是 `None`,读写退化成 no-op。
pub struct Clipboard {
    inner: Option<arboard::Clipboard>,
}

impl Clipboard {
    /// 打开剪贴板。失败只记一行日志——GUI 已经起来了,不该因为剪贴板起不来而崩。
    pub fn new() -> Self {
        let inner = match arboard::Clipboard::new() {
            Ok(c) => Some(c),
            Err(e) => {
                log::warn!(target: "mullion", "剪贴板不可用,复制/粘贴将被忽略: {e}");
                None
            }
        };
        Self { inner }
    }

    /// 写入文本。空串不写:那等于把用户剪贴板里原有的内容清掉。
    pub fn set(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let Some(c) = self.inner.as_mut() else { return };
        if let Err(e) = c.set_text(text.to_owned()) {
            log::warn!(target: "mullion", "写剪贴板失败: {e}");
        }
    }

    /// 读出文本。剪贴板为空、内容不是文本(图片/文件)、或被占用都返回 `None`。
    ///
    /// 空串按「没内容」处理:与 [`Clipboard::set`] 的空串短路对称,也免得
    /// 调用方把空串一路带到 `encode_paste`,往远端发一段空的 bracketed paste
    /// 包裹。
    pub fn get(&mut self) -> Option<String> {
        let c = self.inner.as_mut()?;
        match c.get_text() {
            Ok(t) if t.is_empty() => None,
            Ok(t) => Some(t),
            Err(e) => {
                log::warn!(target: "mullion", "读剪贴板失败: {e}");
                None
            }
        }
    }
}

impl Default for Clipboard {
    fn default() -> Self {
        Self::new()
    }
}

/// F365:Alt+拖选复制时把「你」「您」逐字换成「我」,其余一字不动。返回替换次数。
///
/// 放在 app 层而不是 `mullion-term`:单行拼接是终端语义(有 fixture 守着),
/// 人称替换是产品策略。不做反向「我→你」、不做语义判断(「迷你」也会被改,
/// 用户 2026-11-17 复议后维持一律替换,由调用方飘 Warn toast 兜底)。
pub fn to_first_person(s: &str) -> (String, usize) {
    let mut n = 0;
    let out = s
        .chars()
        .map(|c| match c {
            '你' | '您' => {
                n += 1;
                '我'
            }
            c => c,
        })
        .collect();
    (out, n)
}

#[cfg(test)]
mod tests {
    use super::to_first_person;

    /// F365:你/您逐字换成我,别的字一个不动;次数如实返回(调用方据此飘 toast)。
    #[test]
    fn every_ni_and_nin_becomes_wo_and_nothing_else_moves() {
        assert_eq!(
            to_first_person("需要你拍板的"),
            ("需要我拍板的".to_string(), 1)
        );
        assert_eq!(
            to_first_person("请您确认,你们呢"),
            ("请我确认,我们呢".to_string(), 2)
        );
        assert_eq!(
            to_first_person("git commit -m x"),
            ("git commit -m x".to_string(), 0)
        );
        assert_eq!(to_first_person(""), (String::new(), 0));
    }
}
