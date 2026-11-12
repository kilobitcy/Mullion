//! F345 端到端:真 SSH(进程内 echo sshd)→ 泵 → 仿真器 → 文本投影。
mod common;

use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mullion_ffi::{ConnectParams, ScreenListener, Terminal};

struct Notify(std::sync::Mutex<mpsc::Sender<()>>);

impl ScreenListener for Notify {
    fn on_dirty(&self) {
        let _ = self.0.lock().unwrap().send(());
    }
    fn on_closed(&self) {}
}

/// 测试运行时只用来托管 echo sshd;`Terminal` 用 ffi 自己的运行时。
fn server() -> (
    tokio::runtime::Runtime,
    std::net::SocketAddr,
    common::Received,
) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (addr, rec) = rt.block_on(common::spawn_recording_echo("127.0.0.1:0"));
    (rt, addr, rec)
}

fn open(addr: std::net::SocketAddr) -> (Arc<Terminal>, mpsc::Receiver<()>) {
    let (tx, rx) = mpsc::channel();
    let t = Terminal::connect(
        ConnectParams {
            host: addr.ip().to_string(),
            port: addr.port(),
            user: common::TEST_USER.into(),
            password: common::TEST_PASSWORD.into(),
            cols: 40,
            rows: 5,
        },
        Arc::new(Notify(std::sync::Mutex::new(tx))),
    )
    .expect("connect");
    (t, rx)
}

/// 等到 `pred` 成立;每次 on_dirty 或 50ms 复查一次,最多 5 秒。
fn wait_until(rx: &mpsc::Receiver<()>, mut pred: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if pred() {
            return true;
        }
        let _ = rx.recv_timeout(Duration::from_millis(50));
    }
    pred()
}

#[test]
fn typed_text_comes_back_on_screen() {
    let (_rt, addr, _rec) = server();
    let (t, rx) = open(addr);
    t.write(b"hello".to_vec()).unwrap();
    assert!(
        wait_until(&rx, || t.screen().rows[0] == "hello"),
        "回显没上屏:{:?}",
        t.screen().rows
    );
}

#[test]
fn the_emulators_reply_goes_back_over_the_channel_t1() {
    // echo 把 CSI 6n 回给我们 → 仿真器应答 CSI 1;1R → 泵必须把它写回 channel
    // → 服务端收到。漏了回写(T1),服务端永远收不到这串。
    let (_rt, addr, rec) = server();
    let (t, rx) = open(addr);
    t.write(b"\x1b[6n".to_vec()).unwrap();
    let needle = b"\x1b[1;1R";
    assert!(
        wait_until(&rx, || rec
            .lock()
            .unwrap()
            .windows(needle.len())
            .any(|w| w == needle)),
        "服务端没收到光标位置应答,收到的是 {:?}",
        String::from_utf8_lossy(&rec.lock().unwrap())
    );
}

#[test]
fn an_unterminated_sync_block_still_reaches_the_screen_t2() {
    // BSU 后不跟 ESU:字节攒在 vte 里。泵不按 deadline 主动收口的话,
    // 屏幕永远停在旧画面(高延迟链路上 ESU 被拆到下一个包就是这样)。
    let (_rt, addr, _rec) = server();
    let (t, rx) = open(addr);
    t.write(b"\x1b[?2026hlate".to_vec()).unwrap();
    assert!(
        wait_until(&rx, || t.screen().rows[0] == "late"),
        "同步块超时没收口:{:?}",
        t.screen().rows
    );
}

#[test]
fn resize_changes_the_local_grid_t4() {
    let (_rt, addr, _rec) = server();
    let (t, _rx) = open(addr);
    t.resize(20, 3).unwrap();
    assert_eq!(t.screen().rows.len(), 3);
}

#[test]
fn a_wrong_password_is_an_error_not_a_hang() {
    let (_rt, addr, _rec) = server();
    let (tx, _rx) = mpsc::channel();
    let r = Terminal::connect(
        ConnectParams {
            host: addr.ip().to_string(),
            port: addr.port(),
            user: common::TEST_USER.into(),
            password: "wrong".into(),
            cols: 40,
            rows: 5,
        },
        Arc::new(Notify(std::sync::Mutex::new(tx))),
    );
    assert!(r.is_err());
}
