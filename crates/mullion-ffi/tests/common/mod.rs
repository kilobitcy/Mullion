//! 进程内 echo sshd:收到什么回什么,并把收到的字节全记下来(T1 断言用)。
//! `examples/echo_sshd.rs` 用 `#[path]` 共用这一份。
#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use russh::keys::PublicKey;
use russh::server::{Auth, Handler, Msg, Session};
use russh::{Channel, ChannelId, CryptoVec};

pub const TEST_USER: &str = "testuser";
pub const TEST_PASSWORD: &str = "test-password";

pub type Received = Arc<Mutex<Vec<u8>>>;

pub struct RecordingEcho {
    pub received: Received,
}

impl Handler for RecordingEcho {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        if user == TEST_USER && password == TEST_PASSWORD {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn auth_publickey(&mut self, _user: &str, _key: &PublicKey) -> Result<Auth, Self::Error> {
        Ok(Auth::reject())
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.received.lock().unwrap().extend_from_slice(data);
        session.data(channel, CryptoVec::from(data.to_vec()))?;
        Ok(())
    }
}

pub fn host_key_path() -> String {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../mullion-ssh/tests/fixtures/server_hostkey"
    )
    .to_string()
}

/// 在 `bind` 上起 echo sshd,返回实际地址和「收到的字节」记录。
pub async fn spawn_recording_echo(bind: &str) -> (std::net::SocketAddr, Received) {
    let host_key = russh::keys::load_secret_key(host_key_path(), None).expect("load hostkey");
    let mut config = russh::server::Config::default();
    config.keys.push(host_key);
    let config = Arc::new(config);
    let received: Received = Arc::default();

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .expect("bind echo sshd");
    let addr = listener.local_addr().unwrap();
    let rec = received.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let config = config.clone();
            let handler = RecordingEcho {
                received: rec.clone(),
            };
            tokio::spawn(async move {
                let _ = russh::server::run_stream(config, stream, handler).await;
            });
        }
    });
    (addr, received)
}
