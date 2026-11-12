//! F346-b:只回放 LIST / GET 的假 S3(path-style,bucket `b`)。`tests/cloud_pull.rs`
//! 与 `examples/fake_s3_pack.rs` 用 `#[path]` 共用。**不验签**(签名对不对是
//! mullion-cloud `sigv4.rs` 官方向量的活)。
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

#[derive(Clone, Default)]
pub struct FakeS3 {
    /// 键 → 对象字节。LIST 列出全部键(前缀匹配),GET 按键取。
    pub objects: BTreeMap<String, Vec<u8>>,
    /// 设了就对 GET 一律回这个(状态码,正文),用来测 404/403。
    pub get_override: Option<(u16, String)>,
}

pub struct Running {
    pub port: u16,
    /// 收到的请求行(`GET /b/...`),测试断言「没发过写请求」用。
    pub seen: Arc<Mutex<Vec<String>>>,
}

impl FakeS3 {
    pub fn with_object(key: &str, body: Vec<u8>) -> Self {
        let mut objects = BTreeMap::new();
        objects.insert(key.to_string(), body);
        Self {
            objects,
            get_override: None,
        }
    }

    /// 后台线程里一直服务到进程结束。`addr` 例如 `"127.0.0.1:0"`。
    pub fn spawn(self, addr: &str) -> Running {
        let listener = TcpListener::bind(addr).expect("bind");
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(s) = stream else { break };
                self.handle(s, &log);
            }
        });
        Running { port, seen }
    }

    fn handle(&self, mut s: TcpStream, log: &Mutex<Vec<String>>) {
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut line = String::new();
        if r.read_line(&mut line).is_err() {
            return;
        }
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                break;
            }
        }
        log.lock().unwrap().push(line.trim().to_string());
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or("");
        let target = parts.next().unwrap_or("");
        let (code, body): (u16, Vec<u8>) = if method != "GET" {
            (405, Vec::new())
        } else if let Some(query) = target.strip_prefix("/b?") {
            let prefix = query
                .split('&')
                .find_map(|kv| kv.strip_prefix("prefix="))
                .map(|p| p.replace("%2F", "/"))
                .unwrap_or_default();
            let mut xml = String::from("<ListBucketResult><IsTruncated>false</IsTruncated>");
            for k in self.objects.keys().filter(|k| k.starts_with(&prefix)) {
                xml.push_str(&format!("<Contents><Key>{k}</Key></Contents>"));
            }
            xml.push_str("</ListBucketResult>");
            (200, xml.into_bytes())
        } else if let Some((c, b)) = &self.get_override {
            (*c, b.clone().into_bytes())
        } else {
            let key = target.strip_prefix("/b/").unwrap_or("");
            match self.objects.get(key) {
                Some(v) => (200, v.clone()),
                None => (404, b"<Error><Code>NoSuchKey</Code></Error>".to_vec()),
            }
        };
        let head = format!(
            "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = s.write_all(head.as_bytes());
        let _ = s.write_all(&body);
        let _ = s.flush();
    }
}
