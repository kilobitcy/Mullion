# Plan A2a — App 外壳「无头逻辑基座」实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 `mullion-app` 里落下 App 外壳所需的**纯逻辑基座**——三个可无头单测的纯函数(会话→SshConfig 映射、视口→cols/rows、输入路由决策)加一个 `SessionStore` 薄封装,全部脱离 winit/wgpu/egui,为 Plan A2b 的 GUI 接线打好可验证的地基。

**Architecture:** 新增 `crates/mullion-app/src/shell/` 模块(`mod.rs` + `session_map.rs` + `viewport.rs` + `input_route.rs` + `store.rs`),只依赖 `mullion-ssh`(SshConfig 类型)、`mullion-store`(会话数据)、`directories`(config dir)。零 winit/wgpu/egui。app 的事件循环(`app.rs`)本片**不动**——A2b 才把这些函数接进去。

**Tech Stack:** Rust 2021 · `directories`(config dir)· `mullion-ssh`/`mullion-store`(已存在)· 纯函数 + `#[cfg(test)]` 单测(部分用 `tempfile` + `InMemoryKey`)。错误手写(匹配项目风格)。

> 关联 spec:`docs/superpowers/specs/2026-07-25-app-shell-session-manager-design.md`(切片 A)。
> 覆盖(**仅无头可测那部分**):§5 的 `SessionRecord→SshConfig` 映射、§4.2 的「rect→cols/rows」纯函数(F34/T4)、§4.5 的输入路由纯函数(T5/T6)、§3.2 的 config-dir + Vault 打开(app 侧整合)。
> **不含**(→ Plan A2b):egui 集成、`Option<Connection>` 状态机、菜单/状态栏/会话 UI、异步 connect 接线、待定 F(CLI 退出码)/G(keyring 兜底)。本片不碰 `app.rs`/`main.rs`。

---

## 文件结构

```
crates/mullion-app/Cargo.toml            修改:加 directories 依赖 + dev-dep tempfile
crates/mullion-app/src/lib.rs            修改:加 `pub mod shell;`
crates/mullion-app/src/shell/mod.rs      新建:子模块声明 + 再导出
crates/mullion-app/src/shell/session_map.rs  新建:SessionRecord+secret → SshConfig 映射 + MapError
crates/mullion-app/src/shell/viewport.rs     新建:中央区 rect + 字元尺寸 → (cols, rows),含最小夹紧
crates/mullion-app/src/shell/input_route.rs  新建:route(modal, egui_wants_*, kind) → Egui|Terminal
crates/mullion-app/src/shell/store.rs        新建:config_dir() + SessionStore(封装 mullion_store::Vault)
```

每文件单一职责,互不依赖(store 用 session_map 做映射)。

---

## Task 0:脚手架 —— 加依赖 + 建 shell 模块骨架

**Files:** Modify `crates/mullion-app/Cargo.toml`, `crates/mullion-app/src/lib.rs`; create `crates/mullion-app/src/shell/mod.rs`.

- [ ] **Step 1: 加依赖.** In `crates/mullion-app/Cargo.toml`, under `[dependencies]` add:
```toml
mullion-store = { path = "../mullion-store" }
directories = "5"
```
(`mullion-ssh` is already a dependency.) Add a dev-deps section if absent:
```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: 建模块骨架.** Create `crates/mullion-app/src/shell/mod.rs`:
```rust
//! App 外壳的无头逻辑基座:会话→连接映射、视口尺寸、输入路由、会话存储封装。
//! 零 winit/wgpu/egui —— 可纯单测。A2b 把这些接进 app.rs 事件循环。

pub mod input_route;
pub mod session_map;
pub mod store;
pub mod viewport;
```
Append to `crates/mullion-app/src/lib.rs`:
```rust
pub mod shell;
```

- [ ] **Step 3: 建四个空子模块占位**(让 `mod.rs` 能编译;后续 Task 填充):
`session_map.rs`、`viewport.rs`、`input_route.rs`、`store.rs` 各写一行 doc 注释占位,例如 `//! Task 1 填充。`。

- [ ] **Step 4: 验编译.** Run: `cargo build -p mullion-app`. Expected: 通过。
> 若 `directories = "5"` 解析失败(版本不存在),读 `~/.cargo/registry` 或 `cargo search directories` 取当前主版本并改。

- [ ] **Step 5: Commit.**
```bash
git add crates/mullion-app/Cargo.toml crates/mullion-app/src/lib.rs crates/mullion-app/src/shell/
git commit -m "feat(app): shell 模块骨架 + directories/mullion-store 依赖 (切片 A2a)"
```

---

## Task 1:`session_map` —— SessionRecord(+secret)→ SshConfig 映射

**Files:** Modify `crates/mullion-app/src/shell/session_map.rs`.

- [ ] **Step 1: 写失败测试**(END of file):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mullion_ssh::config::AuthMethod;
    use mullion_store::{AuthKind, Protocol, SecretEntry, SessionId, SessionRecord};

    fn rec(auth: AuthKind, proto: Protocol) -> SessionRecord {
        SessionRecord {
            id: SessionId(1),
            name: "s".into(),
            host: "h".into(),
            port: 2222,
            protocol: proto,
            user: "u".into(),
            note: String::new(),
            modified_at: "t".into(),
            auth,
        }
    }

    #[test]
    fn password_maps_with_secret() {
        let r = rec(AuthKind::Password, Protocol::Ssh);
        let sec = SecretEntry { password: Some("pw".into()), passphrase: None };
        let cfg = to_ssh_config(&r, Some(&sec)).unwrap();
        assert_eq!(cfg.host, "h");
        assert_eq!(cfg.port, 2222);
        assert_eq!(cfg.user, "u");
        assert_eq!(cfg.term, "xterm-256color");
        assert!(matches!(cfg.auth, AuthMethod::Password(p) if p == "pw"));
    }

    #[test]
    fn password_without_secret_errors() {
        let r = rec(AuthKind::Password, Protocol::Ssh);
        assert!(matches!(to_ssh_config(&r, None), Err(MapError::MissingSecret)));
    }

    #[test]
    fn pubkey_with_passphrase_maps() {
        let r = rec(
            AuthKind::PublicKey { path: "/k".into(), has_passphrase: true },
            Protocol::Ssh,
        );
        let sec = SecretEntry { password: None, passphrase: Some("ph".into()) };
        let cfg = to_ssh_config(&r, Some(&sec)).unwrap();
        match cfg.auth {
            AuthMethod::PublicKey { path, passphrase } => {
                assert_eq!(path, std::path::PathBuf::from("/k"));
                assert_eq!(passphrase.as_deref(), Some("ph"));
            }
            _ => panic!("应为 PublicKey"),
        }
    }

    #[test]
    fn pubkey_no_passphrase_maps_none() {
        let r = rec(
            AuthKind::PublicKey { path: "/k".into(), has_passphrase: false },
            Protocol::Ssh,
        );
        let cfg = to_ssh_config(&r, None).unwrap();
        assert!(matches!(cfg.auth, AuthMethod::PublicKey { passphrase: None, .. }));
    }

    #[test]
    fn sftp_is_rejected_in_a2() {
        let r = rec(AuthKind::Password, Protocol::Sftp);
        let sec = SecretEntry { password: Some("pw".into()), passphrase: None };
        assert!(matches!(to_ssh_config(&r, Some(&sec)), Err(MapError::SftpNotSupported)));
    }
}
```

- [ ] **Step 2: 跑测试确认失败** — `cargo test -p mullion-app --lib shell::session_map`. Expected: FAIL(`to_ssh_config`/`MapError` 未定义)。

- [ ] **Step 3: 写实现**(TOP of file):
```rust
//! 把 store 的 `SessionRecord`(+ 解密后的 `SecretEntry`)映射成 `mullion_ssh` 的连接参数。
//! SFTP 连接是切片 D,本片双击 sftp 会话在映射层直接拒绝(app 侧应更早禁用,这里兜底)。

use std::fmt;

use mullion_ssh::config::{AuthMethod, SshConfig};
use mullion_store::{AuthKind, Protocol, SecretEntry, SessionRecord};

/// 映射失败原因。
#[derive(Debug, PartialEq, Eq)]
pub enum MapError {
    /// 需要密码/口令但没提供(会话配置不完整或解密缺失)。
    MissingSecret,
    /// SFTP 连接属切片 D,本片不支持。
    SftpNotSupported,
}

impl fmt::Display for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MapError::MissingSecret => write!(f, "缺少密码/私钥口令 —— 请在会话里重新配置认证"),
            MapError::SftpNotSupported => write!(f, "SFTP 连接尚未实现(切片 D)"),
        }
    }
}

impl std::error::Error for MapError {}

/// `SessionRecord` + 解密后的敏感部分 → `SshConfig`。cols/rows 先给占位默认,
/// 窗口出来后由 window_change 校正到真实尺寸(与既有 `cli::parse_args` 一致)。
pub fn to_ssh_config(rec: &SessionRecord, secret: Option<&SecretEntry>) -> Result<SshConfig, MapError> {
    if rec.protocol == Protocol::Sftp {
        return Err(MapError::SftpNotSupported);
    }
    let auth = match &rec.auth {
        AuthKind::Password => {
            let pw = secret
                .and_then(|s| s.password.clone())
                .ok_or(MapError::MissingSecret)?;
            AuthMethod::Password(pw)
        }
        AuthKind::PublicKey { path, has_passphrase } => {
            let passphrase = if *has_passphrase {
                Some(
                    secret
                        .and_then(|s| s.passphrase.clone())
                        .ok_or(MapError::MissingSecret)?,
                )
            } else {
                None
            };
            AuthMethod::PublicKey {
                path: path.clone(),
                passphrase,
            }
        }
    };
    Ok(SshConfig {
        host: rec.host.clone(),
        port: rec.port,
        user: rec.user.clone(),
        auth,
        cols: 80,
        rows: 24,
        term: "xterm-256color".to_string(),
    })
}
```

- [ ] **Step 4: 跑测试确认通过** — `cargo test -p mullion-app --lib shell::session_map`. Expected: PASS(5 个)。

- [ ] **Step 5: clippy/fmt** — `cargo clippy -p mullion-app --all-targets -- -D warnings` + `cargo fmt --check`. 修格式即可。

- [ ] **Step 6: Commit.**
```bash
git add crates/mullion-app/src/shell/session_map.rs
git commit -m "feat(app): SessionRecord(+secret)→SshConfig 映射 + sftp 边界拒绝 (§5)"
```

---

## Task 2:`viewport` —— 中央区 rect + 字元尺寸 → (cols, rows)

**Files:** Modify `crates/mullion-app/src/shell/viewport.rs`.

> 先看 `crates/mullion-app/src/reflow.rs` 与 `grid.rs`:若已有等价「像素→格子数」纯函数,**复用它**(在本模块 re-export 或直接让 A2b 用既有的),把本 Task 降级为「确认已有覆盖 + 补一条上下栏扣除的测试」。若没有,按下方新建。

- [ ] **Step 1: 写失败测试**(END of file):
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divides_area_by_cell() {
        // 800x600 物理像素,字元 10x20 → 80 列 x 30 行
        assert_eq!(grid_dims((800, 600), (10, 20), (1, 1)), (80, 30));
    }

    #[test]
    fn subtract_chrome_before_dividing() {
        // 上下栏共占 40px 高、菜单不占宽:可用区 800x(600-40)=800x560 → 80 x 28
        let avail = (800, 600 - 40);
        assert_eq!(grid_dims(avail, (10, 20), (1, 1)), (80, 28));
    }

    #[test]
    fn clamps_to_minimum() {
        assert_eq!(grid_dims((5, 5), (10, 20), (2, 2)), (2, 2));
    }

    #[test]
    fn zero_cell_is_safe() {
        // 防除零:字元尺寸为 0 时回落到最小,不 panic
        assert_eq!(grid_dims((800, 600), (0, 0), (4, 3)), (4, 3));
    }
}
```

- [ ] **Step 2: 跑测试确认失败** — `cargo test -p mullion-app --lib shell::viewport`. Expected: FAIL。

- [ ] **Step 3: 写实现**(TOP of file):
```rust
//! 由中央区可用像素(= 窗口减去 egui 上下栏后)和字元像素尺寸,算终端网格列/行数。
//! A2b 会把这个结果喂给 reflow / window_change(F34/T4):上下栏吃掉的空间必须先扣除,
//! 否则远端 tmux 按错误列数排版。

/// `area_px`:中央区可用像素 (宽, 高);`cell_px`:单字元像素 (宽, 高);
/// `min`:最小 (列, 行),夹紧下限。字元尺寸为 0 时安全回落到 `min`(防除零)。
pub fn grid_dims(area_px: (u32, u32), cell_px: (u32, u32), min: (u16, u16)) -> (u16, u16) {
    let cols = if cell_px.0 == 0 { min.0 } else { (area_px.0 / cell_px.0) as u16 };
    let rows = if cell_px.1 == 0 { min.1 } else { (area_px.1 / cell_px.1) as u16 };
    (cols.max(min.0), rows.max(min.1))
}
```

- [ ] **Step 4: 跑测试确认通过** — `cargo test -p mullion-app --lib shell::viewport`. Expected: PASS(4 个)。

- [ ] **Step 5: clippy/fmt**(同上)。

- [ ] **Step 6: Commit.**
```bash
git add crates/mullion-app/src/shell/viewport.rs
git commit -m "feat(app): 中央区 rect→cols/rows 纯函数(扣上下栏+最小夹紧+防除零)(F34/T4)"
```

---

## Task 3:`input_route` —— 输入路由决策纯函数

**Files:** Modify `crates/mullion-app/src/shell/input_route.rs`.

- [ ] **Step 1: 写失败测试**(END of file):
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modal_captures_everything() {
        assert_eq!(route(true, false, false, InputKind::Keyboard), Route::Egui);
        assert_eq!(route(true, false, false, InputKind::Pointer), Route::Egui);
    }

    #[test]
    fn terminal_gets_keyboard_when_egui_doesnt_want_it() {
        // 关键:无模态、egui 不要键盘 → 方向键/快捷键回到终端 keymap(守 T5/T6)
        assert_eq!(route(false, false, false, InputKind::Keyboard), Route::Terminal);
    }

    #[test]
    fn egui_widget_focus_takes_keyboard() {
        assert_eq!(route(false, true, false, InputKind::Keyboard), Route::Egui);
    }

    #[test]
    fn pointer_follows_egui_want() {
        assert_eq!(route(false, false, true, InputKind::Pointer), Route::Egui);
        assert_eq!(route(false, false, false, InputKind::Pointer), Route::Terminal);
    }
}
```

- [ ] **Step 2: 跑测试确认失败** — `cargo test -p mullion-app --lib shell::input_route`. Expected: FAIL。

- [ ] **Step 3: 写实现**(TOP of file):
```rust
//! 输入分流决策(spec §4.5)。egui 的 `consumed` 在「无控件聚焦」时不足以保证方向键/
//! 快捷键回到终端——顶栏/菜单可能间歇抢键(踩 T5/T6)。故显式按这张真值表决定:
//! 有模态→全给 egui;否则按事件类型看 egui 是否真的要这类输入,不要就回终端原路。
//! A2b 用 `egui_ctx.wants_keyboard_input()` / `wants_pointer_input()` 填这两个布尔。

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum InputKind {
    Keyboard,
    Pointer,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Route {
    /// 交给 egui(菜单/状态栏/模态弹窗/表单)。
    Egui,
    /// 交给终端原路:键盘走 keymap+PtyWrite、鼠标走 SGR 上报(Shift 逃生门 T5)。
    Terminal,
}

/// `modal_open`:有模态弹窗时吞掉一切;`egui_wants_keyboard`/`egui_wants_pointer`:
/// 来自 egui 上下文的 `wants_*_input()`;`kind`:本次事件类型。
pub fn route(
    modal_open: bool,
    egui_wants_keyboard: bool,
    egui_wants_pointer: bool,
    kind: InputKind,
) -> Route {
    if modal_open {
        return Route::Egui;
    }
    let egui_wants = match kind {
        InputKind::Keyboard => egui_wants_keyboard,
        InputKind::Pointer => egui_wants_pointer,
    };
    if egui_wants {
        Route::Egui
    } else {
        Route::Terminal
    }
}
```

- [ ] **Step 4: 跑测试确认通过** — `cargo test -p mullion-app --lib shell::input_route`. Expected: PASS(4 个)。

- [ ] **Step 5: clippy/fmt**(同上)。

- [ ] **Step 6: Commit.**
```bash
git add crates/mullion-app/src/shell/input_route.rs
git commit -m "feat(app): 输入路由决策纯函数(显式真值表,守 T5/T6)(§4.5)"
```

---

## Task 4:`store` —— config_dir + SessionStore 封装

**Files:** Modify `crates/mullion-app/src/shell/store.rs`.

- [ ] **Step 1: 写失败测试**(END of file):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mullion_ssh::config::AuthMethod;
    use mullion_store::{AuthKind, InMemoryKey, Protocol, SecretEntry, SessionDraft};

    fn draft() -> SessionDraft {
        SessionDraft {
            name: "dev".into(),
            host: "192.0.2.10".into(),
            port: 22,
            protocol: Protocol::Ssh,
            user: "user".into(),
            note: String::new(),
            auth: AuthKind::Password,
            secret: Some(SecretEntry { password: Some("pw".into()), passphrase: None }),
        }
    }

    #[test]
    fn open_add_then_ssh_config_for() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SessionStore::open(dir.path().to_path_buf(), &InMemoryKey([1u8; 32])).unwrap();
        let id = store.add(draft(), "2026-07-26T00:00:00Z");
        store.save().unwrap();
        assert_eq!(store.list().len(), 1);
        // 组连接参数:解密 secret + 映射
        let cfg = store.ssh_config_for(id).unwrap();
        assert_eq!(cfg.host, "192.0.2.10");
        assert!(matches!(cfg.auth, AuthMethod::Password(p) if p == "pw"));
    }

    #[test]
    fn ssh_config_for_missing_id_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path().to_path_buf(), &InMemoryKey([1u8; 32])).unwrap();
        assert!(store.ssh_config_for(mullion_store::SessionId(999)).is_err());
    }
}
```

- [ ] **Step 2: 跑测试确认失败** — `cargo test -p mullion-app --lib shell::store`. Expected: FAIL。

- [ ] **Step 3: 写实现**(TOP of file):
```rust
//! config-dir 解析 + `SessionStore`:app 侧对 `mullion_store::Vault` 的薄封装,
//! 额外提供「取会话 → 解密 secret → 映射成 SshConfig」的一步到位方法(供双击连接用)。
//! 时间戳由调用方(A2b 用 `time` crate)注入,保持本层可确定性测试。

use std::path::PathBuf;

use mullion_ssh::config::SshConfig;
use mullion_store::{MasterKeySource, SessionDraft, SessionId, SessionRecord, StoreError, Vault};

use super::session_map::{to_ssh_config, MapError};

/// mullion 的配置目录(Windows `%APPDATA%\mullion\`、Linux `~/.config/mullion/`)。
/// 无法确定时返回 None(极少见,如无 HOME)。
pub fn config_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "mullion").map(|d| d.config_dir().to_path_buf())
}

/// 打开会话保险库的错误。
#[derive(Debug)]
pub enum StoreOpenError {
    Store(StoreError),
    Map(MapError),
    NotFound(SessionId),
}

impl std::fmt::Display for StoreOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreOpenError::Store(e) => write!(f, "{e}"),
            StoreOpenError::Map(e) => write!(f, "{e}"),
            StoreOpenError::NotFound(id) => write!(f, "会话不存在:{id:?}"),
        }
    }
}
impl std::error::Error for StoreOpenError {}
impl From<StoreError> for StoreOpenError {
    fn from(e: StoreError) -> Self {
        StoreOpenError::Store(e)
    }
}
impl From<MapError> for StoreOpenError {
    fn from(e: MapError) -> Self {
        StoreOpenError::Map(e)
    }
}

/// app 侧会话存储:薄封装 Vault,增加 `ssh_config_for`。
pub struct SessionStore {
    vault: Vault,
}

impl SessionStore {
    pub fn open(dir: PathBuf, key: &dyn MasterKeySource) -> Result<Self, StoreError> {
        Ok(Self {
            vault: Vault::open(dir, key)?,
        })
    }

    pub fn list(&self) -> &[SessionRecord] {
        self.vault.list()
    }

    pub fn add(&mut self, draft: SessionDraft, now_rfc3339: &str) -> SessionId {
        self.vault.add(draft, now_rfc3339)
    }

    pub fn update(&mut self, id: SessionId, draft: SessionDraft, now_rfc3339: &str) -> Result<(), StoreError> {
        self.vault.update(id, draft, now_rfc3339)
    }

    pub fn delete(&mut self, id: SessionId) -> Result<(), StoreError> {
        self.vault.delete(id)
    }

    pub fn save(&self) -> Result<(), StoreError> {
        self.vault.save()
    }

    /// 取会话 → 用其(已解密的)secret 组 SshConfig(双击连接用)。
    pub fn ssh_config_for(&self, id: SessionId) -> Result<SshConfig, StoreOpenError> {
        let rec = self.vault.get(id).ok_or(StoreOpenError::NotFound(id))?;
        let secret = self.vault.secret(id);
        Ok(to_ssh_config(rec, secret)?)
    }
}
```

- [ ] **Step 4: 跑测试确认通过** — `cargo test -p mullion-app --lib shell::store`. Expected: PASS(2 个)。

- [ ] **Step 5: clippy/fmt**(同上)。若 clippy 抱怨 `StoreOpenError` 某变体未用,保留(A2b 会用),必要时本 Task 的测试补一条覆盖 map 错误路径的用例而非 `#[allow]`。

- [ ] **Step 6: Commit.**
```bash
git add crates/mullion-app/src/shell/store.rs
git commit -m "feat(app): config_dir + SessionStore(封装 Vault + ssh_config_for)(§3.2/§5)"
```

---

## Task 5:全绿门 + 文档收尾

**Files:** 无代码;可能修 `crates/mullion-app/src/shell/viewport.rs`(若 Task 2 复用了既有 reflow 函数,这里补说明)。

- [ ] **Step 1: workspace 全绿.**
```bash
cargo test --workspace > /tmp/a2a-test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/a2a-test.log | grep -v "0 failed"
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```
Expected: 无非零失败行;clippy 无输出(除既有无关 `russh` future-incompat 提示);fmt 无 diff。

- [ ] **Step 2: 确认未碰 app.rs/main.rs.** Run: `git diff main --stat -- crates/mullion-app/src/app.rs crates/mullion-app/src/main.rs`. Expected: 空(本片不动事件循环,守住「A2b 才接线」的边界)。

- [ ] **Step 3: Commit(若有 viewport 说明改动;否则跳过).**
```bash
git commit -am "docs(app): A2a 无头基座完成说明" || echo "无待提交"
```

---

## 自查(写完计划的复盘)

- **Spec 覆盖(仅 A2a 范围)**:§5 映射 → Task 1;§4.2 rect→cols/rows(F34/T4)→ Task 2;§4.5 路由纯函数(T5/T6)→ Task 3;§3.2 config-dir + Vault 打开 → Task 4。均有对应 Task 且**全部无头可测**。
- **超出 A2a**(egui/状态机/会话 UI/连接接线/待定 F/G)→ **Plan A2b**,本片不含,且 Task 5 Step 2 显式断言未碰 `app.rs`/`main.rs`。
- **类型一致性**:`to_ssh_config`/`MapError`(session_map)、`grid_dims`(viewport)、`route`/`Route`/`InputKind`(input_route)、`config_dir`/`SessionStore`/`StoreOpenError`(store)跨 Task 一致;store 复用 session_map 的 `to_ssh_config`。
- **依赖方向**:新增 `mullion-app → mullion-store` 合法(app 是唯一整合者,已在 ADR-006/CLAUDE.md 记为 `app → {core,term,ssh,store}`)。shell 模块零 winit/wgpu/egui。
- **验签点**:`directories` 主版本(Task 0)、`mullion_ssh::config::{SshConfig, AuthMethod}` 字段(Task 1,写前先扫 `crates/mullion-ssh/src/config.rs`)。

## 落地后的下一步

A2a 全绿后写 **Plan A2b · GUI 外壳**:egui 0.30 三件套(已验证与 wgpu 23.0.1 / winit 0.30.13 统一)集成 + `Option<Connection>` 状态机 + 菜单/状态栏/会话 CRUD 弹窗 + 统一异步 connect,把本片四个纯件接进 `app.rs`。A2b 大量落「编译 + 人工验收清单」(egui 渲染/不撕裂/输入分流/reflow 需人眼),并交叉编译出 Windows exe 实测(含 A1 的 keyring 真机验证)。届时敲定待定 F(CLI 退出码)/G(keyring 兜底)。
