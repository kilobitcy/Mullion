//! F348~F350 端到端:导入包 → 列项目 → 打开(TOFU 确认)→ attach 计划上屏。
//! 真 SSH 走进程内 echo sshd;`Terminal` 用 ffi 自己的运行时。
mod common;
#[path = "common/demo_pack.rs"]
mod demo_pack;

use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use mullion_ffi::{
    import_pack, list_projects, open_project, trust_host_key, OpenOutcome, ScreenListener, Terminal,
};

const KEY: [u8; 32] = [7; 32];

struct Notify(Mutex<mpsc::Sender<()>>);

impl ScreenListener for Notify {
    fn on_dirty(&self) {
        let _ = self.0.lock().unwrap().send(());
    }
    fn on_closed(&self) {}
}

struct Fixture {
    _rt: tokio::runtime::Runtime,
    _tmp: tempfile::TempDir,
    dir: String,
    received: common::Received,
    project: u64,
}

/// `dead_first`:项目第一条节点指向一个没人听的端口(F304 换路)。
fn fixture(dead_first: bool) -> Fixture {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (addr, received) = rt.block_on(common::spawn_echo_with_banner("127.0.0.1:0", b"$ "));
    let mut addrs = vec![addr];
    if dead_first {
        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        addrs.insert(0, dead.local_addr().unwrap());
        drop(dead);
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("vault").to_string_lossy().into_owned();
    import_pack(
        dir.clone(),
        demo_pack::open_pack(
            &addrs
                .iter()
                .map(|a| (a.ip().to_string(), a.port()))
                .collect::<Vec<_>>(),
        ),
        demo_pack::DEMO_PASSWORD.into(),
        KEY.to_vec(),
    )
    .unwrap();
    let rows = list_projects(dir.clone(), KEY.to_vec(), String::new(), 8 * 3600).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, demo_pack::DEMO_PROJECT);
    assert_eq!(rows[0].last_opened, "从未打开");
    assert_eq!(rows[0].nodes, addrs.len() as u32);
    Fixture {
        _rt: rt,
        _tmp: tmp,
        dir,
        received,
        project: rows[0].id,
    }
}

fn open(
    fx: &Fixture,
) -> (
    Result<OpenOutcome, mullion_ffi::FfiError>,
    mpsc::Receiver<()>,
) {
    let (tx, rx) = mpsc::channel();
    let r = open_project(
        fx.dir.clone(),
        KEY.to_vec(),
        fx.project,
        200,
        10,
        Arc::new(Notify(Mutex::new(tx))),
    );
    (r, rx)
}

/// 第一次碰到未知主机 → 确认 → 重开,拿到终端与节点名。
fn open_trusting(fx: &Fixture) -> (Arc<Terminal>, String, mpsc::Receiver<()>) {
    let key = match open(fx).0.expect("首次打开不该报错") {
        OpenOutcome::UnknownHostKey { key } => key,
        OpenOutcome::Opened { .. } => panic!("空 known_hosts 不能直接放行"),
    };
    assert!(key.fingerprint.starts_with("SHA256:"), "{key:?}");
    trust_host_key(fx.dir.clone(), key).unwrap();
    match open(fx) {
        (Ok(OpenOutcome::Opened { terminal, node }), rx) => (terminal, node, rx),
        (Ok(OpenOutcome::UnknownHostKey { key }), _) => panic!("确认过还在问:{key:?}"),
        (Err(e), _) => panic!("确认后打开失败:{e}"),
    }
}

fn wait_until(rx: &mpsc::Receiver<()>, mut pred: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if pred() {
            return true;
        }
        let _ = rx.recv_timeout(Duration::from_millis(50));
    }
    pred()
}

fn sent(fx: &Fixture) -> String {
    String::from_utf8_lossy(&fx.received.lock().unwrap()).into_owned()
}

#[test]
fn unknown_host_is_confirmed_then_the_attach_plan_goes_out_and_shows_on_screen() {
    let fx = fixture(false);
    let (t, node, rx) = open_trusting(&fx);
    assert_eq!(node, "节点1");
    assert!(
        wait_until(&rx, || sent(&fx).contains("tmux has-session")),
        "attach 计划没发出去:{:?}",
        sent(&fx)
    );
    let s = sent(&fx);
    // 与桌面共挂:首连不踢别的 client(`-d` 只属于断线重连)。
    assert!(
        !s.contains("attach-session -d") && !s.contains("attach -d"),
        "{s}"
    );
    // T2':sync 登记跟着计划一起走。
    assert!(s.contains("terminal-features"), "{s}");
    assert!(
        wait_until(&rx, || t
            .screen()
            .rows
            .join("\n")
            .contains("tmux has-session")),
        "计划回显没上屏:{:?}",
        t.screen().rows
    );
    t.disconnect();
}

#[test]
fn a_dead_first_node_falls_over_to_the_next_one() {
    let fx = fixture(true);
    let (t, node, rx) = open_trusting(&fx);
    assert_eq!(node, "节点2");
    assert!(wait_until(&rx, || sent(&fx).contains("tmux has-session")));
    t.disconnect();
}

#[test]
fn typing_before_the_plan_goes_out_cancels_it() {
    let fx = fixture(false);
    let (t, _, rx) = open_trusting(&fx);
    // 提示符先到、initial_delay 还没过:用户抢先打字 = 接管。
    t.write(b"x".to_vec()).unwrap();
    assert!(wait_until(&rx, || sent(&fx).contains('x')));
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        !sent(&fx).contains("tmux"),
        "接管后计划仍发了:{:?}",
        sent(&fx)
    );
    t.disconnect();
}
