//! 主机密钥确认弹窗(F3)。未知主机与指纹变更两态,后者是高危警告态。
//!
//! 窗口**没有关闭按钮**:两个动作(接受 / 取消连接)都会给握手线程一个明确答复。
//! 留个 X 会让用户以为「关掉 = 什么都没发生」,而实际上握手正挂着等回答。

/// sshd `LoginGraceTime` 的默认值(秒)。超过它对端会主动断开,弹窗再点也没用,
/// 所以要把倒计时摆在用户面前,而不是让他慢慢核对完发现连接已经没了。
const LOGIN_GRACE_SECS: u64 = 120;

/// 危险/警告色。比纯 `Color32::RED` 柔和,适合大段警示文字。
const DANGER: egui::Color32 = egui::Color32::from_rgb(200, 40, 40);

/// 把协议 wire 名(`key.algorithm().to_string()`,如 `ssh-ed25519`/`rsa-sha2-256`/
/// `ecdsa-sha2-nistp256`)映射成服务器上对应的公钥文件名,供用户在服务器本机核对。
/// 指错文件会让用户拿到对不上的指纹——在 MITM 警告场景下,这比不给命令更糟
/// (复核 C3)。russh 协商默认列表(`Preferred::DEFAULT`)不含 `ssh-dss`,故不单列。
fn host_key_filename(algo: &str) -> &'static str {
    if algo.starts_with("ssh-ed25519") {
        "ssh_host_ed25519_key.pub"
    } else if algo.starts_with("ssh-rsa") || algo.starts_with("rsa-sha2-") {
        "ssh_host_rsa_key.pub"
    } else if algo.starts_with("ecdsa-sha2-") {
        "ssh_host_ecdsa_key.pub"
    } else {
        "ssh_host_*_key.pub"
    }
}

/// 弹窗要展示的只读视图。借用式:`&mut UiState` 与 `&HostKeyPrompt` 是 App 的两个
/// 不相干字段,可以同时借出,不必把 prompt 复制进 UiState 再同步两份状态。
#[derive(Clone, Copy)]
pub struct HostKeyView<'a> {
    pub host: &'a str,
    pub algo: &'a str,
    pub fingerprint: &'a str,
    /// 存档里的旧指纹;`Some` = 变更(高危)。
    pub previous: Option<&'a str>,
    /// 弹窗已开的秒数。
    pub elapsed_secs: u64,
    /// `false` = 这次接受只对本次连接有效,不写 known_hosts(F92 拨测)。
    pub persist: bool,
}

impl HostKeyView<'_> {
    pub fn is_changed(&self) -> bool {
        self.previous.is_some()
    }

    pub fn title(&self) -> &'static str {
        if self.is_changed() {
            "⚠ 主机密钥已变更"
        } else {
            "主机密钥确认"
        }
    }

    /// 握手宽限期剩余秒数,饱和到 0。
    pub fn grace_left_secs(&self) -> u64 {
        LOGIN_GRACE_SECS.saturating_sub(self.elapsed_secs)
    }
}

/// 画弹窗。用户做出选择时把 `Some(accept)` 写进 `reply`,由 app.rs 事后施加
/// (记录+落盘+回送给握手线程)——egui 闭包里借不到 `&mut App`,与会话管理器同构。
///
/// 用 `egui::Modal` 而非 `egui::Window`:普通 `Window` 不挡下层点击(复核 A5,
/// `egui-0.30.0/src/containers/modal.rs:6-8` 的 doc 明确写 `Modal` 才「blocks
/// input to the rest of the UI」),弹窗开着时用户不该还能点菜单栏发起第二次连接。
pub fn show(ctx: &egui::Context, view: &HostKeyView<'_>, reply: &mut Option<bool>) {
    // 倒计时要每秒走一格。走 egui 的 repaint_delay 通道,由 app.rs 按
    // T3/T7 的 next_frame_at/WaitUntil 排期,不绕开帧率闸。
    ctx.request_repaint_after(std::time::Duration::from_secs(1));
    egui::Modal::new(egui::Id::new("host_key_prompt")).show(ctx, |ui| {
        // Modal 没有标题栏(不像 Window),标题要在内容区自己画出来,否则
        // 「⚠ 主机密钥已变更」这个高危信号在 UI 上彻底消失。
        if view.is_changed() {
            ui.colored_label(DANGER, egui::RichText::new(view.title()).heading());
        } else {
            ui.heading(view.title());
        }
        ui.separator();
        if view.is_changed() {
            ui.colored_label(
                DANGER,
                "此主机的密钥与上次记录的不同。可能是服务器重装/换了密钥,\
                 也可能是有人在中间冒充它。确认之前不要输入任何密码。",
            );
        } else {
            ui.label("首次连接此主机,尚无指纹记录。请核对后再决定是否信任。");
        }
        ui.separator();
        // 指纹要能选中复制(spec §4.4)。egui 的 `interaction.selectable_labels`
        // 默认为 true,`ui.monospace` 产出的 Label 天然可框选——不要为此改样式,
        // 但也不要把它换成 `ui.small`/自绘文本,那会把可选性弄丢。
        egui::Grid::new("host-key-facts")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("主机");
                ui.monospace(view.host);
                ui.end_row();
                ui.label("算法");
                ui.monospace(view.algo);
                ui.end_row();
                if let Some(prev) = view.previous {
                    ui.label("原记录");
                    ui.monospace(prev);
                    ui.end_row();
                    ui.label("本次收到");
                    ui.colored_label(DANGER, egui::RichText::new(view.fingerprint).monospace());
                    ui.end_row();
                } else {
                    ui.label("指纹");
                    ui.monospace(view.fingerprint);
                    ui.end_row();
                }
            });
        ui.separator();
        ui.label("在服务器本机上核对:");
        ui.monospace(format!(
            "ssh-keygen -lf /etc/ssh/{}",
            host_key_filename(view.algo)
        ));
        let left = view.grace_left_secs();
        if left == 0 {
            ui.colored_label(DANGER, "已超过握手宽限期,远端可能已断开——取消后重连即可。");
        } else {
            ui.label(format!("远端约 {left} 秒后会因超时断开握手。"));
        }
        if !view.persist {
            ui.add_space(6.0);
            ui.colored_label(
                crate::theme::c32(crate::theme::MULLION_DARK.fg_dim),
                "本次测试不会记住此指纹,正式连接时会再次询问。",
            );
        }
        ui.separator();
        ui.horizontal(|ui| {
            // 变更态把「取消连接」放在最左(默认位),接受要多走一步。
            if view.is_changed() {
                if ui.button("取消连接").clicked() {
                    *reply = Some(false);
                }
                if ui.button("我已核对,接受并更新记录").clicked() {
                    *reply = Some(true);
                }
            } else {
                if ui.button("接受并记住").clicked() {
                    *reply = Some(true);
                }
                if ui.button("取消连接").clicked() {
                    *reply = Some(false);
                }
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_state_is_marked_dangerous_and_defaults_to_cancel() {
        // F3:变更态必须与首次连接态在视觉与默认动作上区分开,否则用户会
        // 用点「首次连接」的肌肉记忆一路点过 MITM 警告。
        let changed = HostKeyView {
            host: "h",
            algo: "ssh-ed25519",
            fingerprint: "SHA256:BBBB",
            previous: Some("SHA256:AAAA"),
            elapsed_secs: 0,
            persist: true,
        };
        let first = HostKeyView {
            previous: None,
            ..changed
        };
        assert!(changed.is_changed());
        assert!(!first.is_changed());
        assert_ne!(changed.title(), first.title());
    }

    #[test]
    fn grace_countdown_saturates_at_zero() {
        let v = HostKeyView {
            host: "h",
            algo: "ssh-ed25519",
            fingerprint: "SHA256:BBBB",
            previous: None,
            elapsed_secs: 999,
            persist: true,
        };
        // 不能出现负数/回绕的「剩余 18446744073709551615 秒」。
        assert_eq!(v.grace_left_secs(), 0);
        assert_eq!(
            HostKeyView {
                elapsed_secs: 20,
                ..v
            }
            .grace_left_secs(),
            100
        );
    }

    #[test]
    fn ed25519_host_key_points_at_ed25519_file() {
        assert_eq!(host_key_filename("ssh-ed25519"), "ssh_host_ed25519_key.pub");
    }

    #[test]
    fn rsa_host_key_points_at_rsa_file_so_user_does_not_compare_the_wrong_fingerprint() {
        // C3:algo 是协议 wire 名,RSA 主机key 协商时给的是 `rsa-sha2-256`/
        // `rsa-sha2-512`(或legacy `ssh-rsa`),不是 `ssh-ed25519`——指错文件
        // 会让用户拿一份对不上的指纹去核对,在 MITM 场景下比不给命令更糟。
        assert_eq!(host_key_filename("ssh-rsa"), "ssh_host_rsa_key.pub");
        assert_eq!(host_key_filename("rsa-sha2-256"), "ssh_host_rsa_key.pub");
        assert_eq!(host_key_filename("rsa-sha2-512"), "ssh_host_rsa_key.pub");
    }

    #[test]
    fn ecdsa_host_key_points_at_ecdsa_file() {
        assert_eq!(
            host_key_filename("ecdsa-sha2-nistp256"),
            "ssh_host_ecdsa_key.pub"
        );
    }

    #[test]
    fn unknown_algo_falls_back_to_wildcard_instead_of_a_wrong_specific_file() {
        // 未识别的 algo 宁可给通配符让用户自己 ls,也不能猜一个可能错的具体文件名。
        assert_eq!(
            host_key_filename("sk-ssh-ed25519@openssh.com"),
            "ssh_host_*_key.pub"
        );
    }
}
