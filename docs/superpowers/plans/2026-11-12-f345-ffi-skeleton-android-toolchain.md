# F345 mullion-ffi 骨架 + Android 工具链 + 模拟器最小终端 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 新增第二个组装端 `mullion-ffi`(uniffi),让一个 Kotlin/Compose 的 Android 应用在 KVM 模拟器里经 `mullion-ssh` 连上 SSH、把字节喂进 `mullion-term`,并以纯文本行显示出来;全程机械可测。

**Architecture:** `mullion-ffi` 依赖 `{term, ssh, store}`,自带一个 tokio 运行时,导出 `Terminal` 对象(connect/write/resize/screen/close)和 `ScreenListener` 回调。泵任务负责 T1(PTY 应答回写)与 T2 同步块超时收口。`mullion-store` 在 Android 上**整个去掉** keyring:keyring 3.6.3 在 Android 上不报错,会静默回落到 mock 内存库,主密钥每次重启都丢。Android 工程放在 `android/`,Gradle `preBuild` 先跑 `cargo ndk` 和 uniffi 生成绑定,再编 APK。

**Tech Stack:** Rust 1.96 / uniffi 0.32 / cargo-ndk 4.1.2 / NDK 29.0.14206865 / Gradle 9.7.1 / AGP 9.4.0 / Kotlin 2.4.20 / Compose BOM 2026.09.00 / JNA 5.19.1 / 模拟器镜像 `system-images;android-36;google_apis;x86_64`

**设计来源:** `docs/adr-013-android-client.md`、`docs/superpowers/specs/2026-11-12-android-client-design.md`(§4 零期 F345)

---

## 已核实的环境事实(2026-11-12,写计划时实测)

- 已有:`~/Android/Sdk` 下 platforms 36 / 37.2、build-tools 36.0.0 / 36.1.0、platform-tools、cmdline-tools(旧版,读 XML v4 会告警);JDK 21;`~/.gradle` 已缓存 Gradle 9.7.1、AGP 9.4.0、Kotlin 2.4.20(来自 `/data/HPlayer`,该工程的 `gradle.properties` 写着 `android.builtInKotlin=false` / `android.newDsl=false`,不写这两行 AGP 9 会报插件冲突)。
- 缺:NDK、emulator、系统镜像、cargo-ndk、Rust Android target。
- **`ubuntu` 不在 `kvm` 组**,`/dev/kvm` 权限是 `root:kvm 0660`。要跑模拟器,得先 `sudo usermod -aG kvm ubuntu`,再用 `sg kvm -c` 启动(当前进程树的组成员关系不会自动刷新)。
- **网络**:
  - 代理对 `dl.google.com` 的 TLS 握手会被断。
  - `maven.google.com` 的构件会 301 到 `dl.google.com`。
  - SDK 包走腾讯镜像:`SDK_TEST_BASE_URL=https://mirrors.cloud.tencent.com/AndroidSDK/` 下 sdkmanager 能列出并安装。
  - Gradle 的 Google 仓库走阿里云:`https://maven.aliyun.com/repository/google`(AGP 9.4.0、Compose BOM 2026.09.00、androidx.test runner 1.7.0、ext-junit 1.3.0 都已确认 200)。
  - Gradle JVM 的代理在 `~/.gradle/gradle.properties` 里已配好;cargo/rustup 走环境变量里的 `HTTPS_PROXY`。
- **keyring 3.6.3**:`lib.rs:301-309` 在没有匹配平台时 `pub use mock as default`,Android(`target_os = "android"`,不是 `linux`)正好落进这一支。它的 `Cargo.toml` 里平台依赖全部按 `target_os` 分开,所以 Android 上**能编过**,问题是运行时静默。
- `mullion-ssh/tests/common/mod.rs` 有现成的进程内 echo sshd(`EchoHandler`,用户 `testuser` / 密码 `test-password`,主机密钥 `tests/fixtures/server_hostkey`)。

## 文件结构

| 路径 | 职责 |
|---|---|
| `crates/mullion-store/Cargo.toml` | keyring 改为 `cfg(not(target_os = "android"))` 的目标依赖 |
| `crates/mullion-store/src/{master_key,error,lib}.rs` | `KeyringSource` 和 `From<keyring::Error>` 按同一 cfg 隔开 |
| `crates/mullion-store/tests/android_has_no_keyring.rs` | 机械守护:Android 依赖树里没有 keyring,Windows 依赖树里有(防止测试恒绿) |
| `crates/mullion-ffi/Cargo.toml` | cdylib + lib;`bindgen` feature 控制绑定生成器 |
| `crates/mullion-ffi/uniffi.toml` | Kotlin 包名 |
| `crates/mullion-ffi/src/lib.rs` | `setup_scaffolding!`、`version()`、模块声明 |
| `crates/mullion-ffi/src/runtime.rs` | 进程级 tokio 运行时 |
| `crates/mullion-ffi/src/screen.rs` | `GridSnapshot` 投影成纯文本行(纯函数,F351 换成样式分段) |
| `crates/mullion-ffi/src/terminal.rs` | `Terminal` / `ScreenListener` / `FfiError` / 泵 |
| `crates/mullion-ffi/src/bin/uniffi-bindgen.rs` | 绑定生成器入口(`required-features = ["bindgen"]`) |
| `crates/mullion-ffi/tests/common/mod.rs` | 记录收到字节的 echo sshd(测试与 example 共用) |
| `crates/mullion-ffi/tests/terminal_roundtrip.rs` | 端到端:回显上屏、T1 应答回写、T2 超时收口 |
| `crates/mullion-ffi/tests/dependency_direction.rs` | ffi 不依赖 app;app 不依赖 ffi |
| `crates/mullion-ffi/examples/echo_sshd.rs` | 给模拟器插桩测试用的宿主机假 sshd |
| `android/**` | Gradle 工程、Compose 最小界面、插桩测试、构建与模拟器脚本 |
| `docs/android-build.md` | Android 工具链与构建运行手册 |
| `CLAUDE.md`、`spec.md`、`.gitignore` | 不变量表、边界句、构建命令;F345 标已实现;忽略生成物 |

**本切片不改 `mullion-app` 的行为**,只有 store 的 cfg 隔开和依赖方向测试。Windows 构建零变化,**不发桌面版**。Android 版本号 `0.0.1`,本切片不发 APK(发布是 F355 的事)。

---

### Task 0: 分支与基线

- [ ] **Step 1:开分支**

```bash
cd /data/Mullion && git switch -c slice/f345
```

- [ ] **Step 2:基线绿**

```bash
cargo test --workspace > /tmp/f345-base.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/f345-base.log | grep -v " 0 failed" ; echo "exit=$?"
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3
```
Expected:第一条 grep 没有输出(`exit=1`);clippy 结尾是 `Finished`。

---

### Task 1: 安装工具链(**控制者亲自执行**,含 sudo,不派子 agent)

**Files:**
- Create: `android/scripts/emulator.sh`
- Create: `docs/android-build.md`

- [ ] **Step 1:Rust target 与 cargo-ndk**

```bash
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk --version 4.1.2 --locked
cargo ndk --version
```
Expected:`cargo-ndk 4.1.2`

- [ ] **Step 2:SDK 包(走腾讯镜像)**

```bash
export SDK_TEST_BASE_URL=https://mirrors.cloud.tencent.com/AndroidSDK/
SM=~/Android/Sdk/cmdline-tools/latest/bin/sdkmanager
yes | $SM --licenses > /dev/null
$SM "cmdline-tools;latest" "ndk;29.0.14206865" "emulator" "system-images;android-36;google_apis;x86_64"
ls ~/Android/Sdk/ndk ~/Android/Sdk/emulator/emulator ~/Android/Sdk/system-images/android-36/google_apis/x86_64
```
Expected:三个路径都存在。注意 `cmdline-tools;latest` 会覆盖原来的 `latest`,之后不应再出现「SDK XML version 4」告警。

- [ ] **Step 3:KVM 权限 —— 先向用户说明并确认,再执行**

要说明的原因:模拟器需要读写 `/dev/kvm`,当前用户不在 `kvm` 组。加组是持久的系统改动。

```bash
sudo usermod -aG kvm ubuntu
sg kvm -c 'test -r /dev/kvm -a -w /dev/kvm && echo kvm-ok'
```
Expected:`kvm-ok`

- [ ] **Step 4:建 AVD**

```bash
echo no | ~/Android/Sdk/cmdline-tools/latest/bin/avdmanager create avd \
  -n mullion-api36 -k "system-images;android-36;google_apis;x86_64" -d pixel_7
```

- [ ] **Step 5:写 `android/scripts/emulator.sh`**

```bash
#!/usr/bin/env bash
# 无头模拟器起停(F345)。用 sg kvm:当前会话的组成员关系不会因 usermod 刷新。
set -euo pipefail
SDK=${ANDROID_HOME:-$HOME/Android/Sdk}
AVD=${AVD:-mullion-api36}
case "${1:-}" in
  start)
    if "$SDK/platform-tools/adb" devices | grep -q '^emulator-'; then echo "已在运行"; exit 0; fi
    sg kvm -c "nohup '$SDK/emulator/emulator' -avd '$AVD' -no-window -no-audio -no-boot-anim \
      -no-snapshot -gpu swiftshader_indirect > /tmp/mullion-emulator.log 2>&1 &"
    "$SDK/platform-tools/adb" wait-for-device
    until [ "$("$SDK/platform-tools/adb" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = 1 ]; do sleep 2; done
    echo "启动完成"
    ;;
  stop)
    "$SDK/platform-tools/adb" emu kill || true
    ;;
  *) echo "用法: $0 start|stop" >&2; exit 2 ;;
esac
```

```bash
chmod +x android/scripts/emulator.sh
timeout 300 android/scripts/emulator.sh start && ~/Android/Sdk/platform-tools/adb shell getprop ro.build.version.sdk
```
Expected:`启动完成`,然后输出 `36`。

- [ ] **Step 6:写 `docs/android-build.md` 初稿**

内容固定为下面这些。Task 9 会再补上构建与测试命令。

```markdown
# Android 构建运行手册(F345 起)

## 一次性安装
- Rust:`rustup target add aarch64-linux-android x86_64-linux-android`;`cargo install cargo-ndk --version 4.1.2 --locked`
- SDK 包**走腾讯镜像**(本机代理对 dl.google.com 的 TLS 会被断):
  `SDK_TEST_BASE_URL=https://mirrors.cloud.tencent.com/AndroidSDK/ sdkmanager "cmdline-tools;latest" "ndk;29.0.14206865" "emulator" "system-images;android-36;google_apis;x86_64"`
- Gradle 的 Google 仓库**走阿里云**(`maven.google.com` 的构件会 301 到 dl.google.com):见 `android/settings.gradle.kts`
- KVM:`sudo usermod -aG kvm $USER`;之后用 `sg kvm -c` 启模拟器,或者重新登录
- AVD:`avdmanager create avd -n mullion-api36 -k "system-images;android-36;google_apis;x86_64" -d pixel_7`

## 模拟器
`android/scripts/emulator.sh start|stop`(无头,swiftshader;日志在 `/tmp/mullion-emulator.log`)
```

- [ ] **Step 7:提交**

```bash
git add android/scripts/emulator.sh docs/android-build.md
git commit -m "chore(android): 工具链安装手册与无头模拟器脚本 (F345)"
```

---

### Task 2: store 在 Android 上去掉 keyring

**Files:**
- Create: `crates/mullion-store/tests/android_has_no_keyring.rs`
- Modify: `crates/mullion-store/Cargo.toml`(`keyring.workspace = true` 那一行)
- Modify: `crates/mullion-store/src/master_key.rs`(`KeyringSource` 结构体与两个 impl)
- Modify: `crates/mullion-store/src/error.rs:176-180`(`From<keyring::Error>`)
- Modify: `crates/mullion-store/src/lib.rs:61`(`pub use master_key::...`)

- [ ] **Step 1:写失败的守护测试**

```rust
//! F345 的机械守护:Android 上 `mullion-store` 的依赖树里**没有** keyring。
//!
//! 为什么必须隔开、而不是「编得过就行」:keyring 3.6.3 在没有匹配平台时
//! `pub use mock as default`,Android 正好落进这一支 —— 能编过、能跑,
//! 主密钥写进进程内存,**重启就丢,零报错**。所以在 Android 上必须让
//! `KeyringSource` 根本不存在,误用变成编译错误;Android 的主密钥走
//! Keystore(F346/F347)。
//!
//! 第二条断言(Windows 上**有** keyring)防止这个测试恒绿:`--target`
//! 写错、`cargo tree` 输出格式变了,第一条断言都会一直过。

use std::process::Command;

fn tree_for(target: &str) -> String {
    let out = Command::new(env!("CARGO"))
        .args([
            "tree", "-p", "mullion-store", "--target", target,
            "--edges", "normal", "--prefix", "none",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo tree 应能执行");
    assert!(
        out.status.success(),
        "cargo tree 失败: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn the_android_build_of_store_does_not_link_keyring_whose_fallback_is_a_silent_memory_store() {
    let tree = tree_for("aarch64-linux-android");
    assert!(
        !tree.lines().any(|l| l.starts_with("keyring ")),
        "Android 上 mullion-store 仍依赖 keyring(会静默回落 mock)。依赖树:\n{tree}"
    );
}

#[test]
fn the_windows_build_of_store_still_links_keyring() {
    let tree = tree_for("x86_64-pc-windows-gnu");
    assert!(
        tree.lines().any(|l| l.starts_with("keyring ")),
        "Windows 上 mullion-store 找不到 keyring —— 要么隔错了方向,要么本测试的 \
         cargo tree 解析已失效(那样上一条测试就是恒绿)。依赖树:\n{tree}"
    );
}
```

- [ ] **Step 2:确认第一条失败、第二条通过**

```bash
cargo test -p mullion-store --test android_has_no_keyring 2>&1 | grep -E "^test |test result"
```
Expected:`the_android_build_...` FAILED,`the_windows_build_...` ok。

- [ ] **Step 3:Cargo.toml 改成目标依赖**

先把 `[dependencies]` 下的 `keyring.workspace = true` 删掉,再在 `[dev-dependencies]` 之前加:

```toml
# F345:Android 上**不要** keyring。它在那里没有后端,会静默回落到进程内存
# 的 mock 库 —— 主密钥每次重启都丢、零报错。Android 的主密钥走 Keystore
# (F346/F347);`KeyringSource` 在 Android 上按同一 cfg 整个不存在。
[target.'cfg(not(target_os = "android"))'.dependencies]
keyring.workspace = true
```

- [ ] **Step 4:源码按同一 cfg 隔开**

`master_key.rs`:在 `pub struct KeyringSource`、`impl KeyringSource`、`impl Default for KeyringSource`、`impl MasterKeySource for KeyringSource` 这四处的上一行,各加一行:

```rust
#[cfg(not(target_os = "android"))]
```

再把 `KeyringSource` 结构体的文档注释末尾补一行:

```rust
/// Android 上不存在(F345):那里 keyring 会静默回落到内存库。
```

`error.rs` 的 `impl From<keyring::Error> for StoreError` 上一行也加 `#[cfg(not(target_os = "android"))]`。`StoreError::Keyring(String)` 变体保留:它装的是 String,不依赖 keyring 类型。

`lib.rs:61` 拆成两行:

```rust
pub use master_key::{InMemoryKey, MasterKeySource};
#[cfg(not(target_os = "android"))]
pub use master_key::KeyringSource;
```

如果 `master_key.rs` 顶部的 `use chacha20poly1305::aead::{KeyInit, OsRng};` / `XChaCha20Poly1305` 在 Android 上变成未使用,Step 5 的 check 会报 warning。那时把这两条 `use` 挪进 `impl MasterKeySource for KeyringSource` 的函数体里,别加 `allow`。

- [ ] **Step 5:验证**

```bash
cargo test -p mullion-store --test android_has_no_keyring 2>&1 | grep -E "test result"
RUSTFLAGS="-D warnings" cargo check -p mullion-store --target aarch64-linux-android 2>&1 | tail -2
cargo test -p mullion-store 2>&1 | grep -E "test result|FAILED" | grep -v " 0 failed"
```
Expected:守护测试 2 passed;Android check 以 `Finished` 结尾、无 warning;store 全部测试没有失败行。

- [ ] **Step 6:提交**

```bash
git add crates/mullion-store
git commit -m "fix(store): Android 上去掉 keyring,防静默回落内存库 (F345)

守护测试:android_has_no_keyring 两条(Android 树无 keyring / Windows 树有)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: `mullion-ffi` 骨架 + 依赖方向守护 + Android 交叉编译

**Files:**
- Modify: `Cargo.toml`(workspace members + `[workspace.dependencies]`)
- Create: `crates/mullion-ffi/Cargo.toml`
- Create: `crates/mullion-ffi/uniffi.toml`
- Create: `crates/mullion-ffi/src/lib.rs`
- Create: `crates/mullion-ffi/src/bin/uniffi-bindgen.rs`
- Create: `crates/mullion-ffi/tests/dependency_direction.rs`

- [ ] **Step 1:workspace**

`Cargo.toml` 的 `members` 末尾加 `"crates/mullion-ffi",`。`[workspace.dependencies]` 末尾加:

```toml
# F345(ADR-013):Android 端的 FFI。只用 proc-macro 模式(不写 UDL);
# `cli` 特性只给 `mullion-ffi` 的绑定生成器 bin 用,见该 crate 的 `bindgen` feature。
uniffi = "0.32"
```

- [ ] **Step 2:`crates/mullion-ffi/Cargo.toml`**

```toml
[package]
name = "mullion-ffi"
version.workspace = true
edition.workspace = true
license.workspace = true

# 架构不变量(ADR-013):ffi 是与 app 平级的第二个组装端。可依赖
# {core, term, ssh, store, cloud};**永不依赖 mullion-app**,app 也永不依赖它。
[lib]
name = "mullion_ffi"
# cdylib 给 Android 加载;lib 让本 crate 的集成测试能直接调 Rust API。
crate-type = ["cdylib", "lib"]

[features]
# 只给绑定生成器 bin 打开,免得常规 `cargo test --workspace` 把 uniffi 的
# CLI 依赖(clap 等)也编进来。
bindgen = ["uniffi/cli"]

[[bin]]
name = "uniffi-bindgen"
path = "src/bin/uniffi-bindgen.rs"
required-features = ["bindgen"]

[dependencies]
uniffi.workspace = true
mullion-term = { path = "../mullion-term" }
mullion-ssh = { path = "../mullion-ssh" }
mullion-store = { path = "../mullion-store" }
tokio.workspace = true

[dev-dependencies]
russh.workspace = true
tokio = { workspace = true, features = ["rt-multi-thread", "macros", "net", "io-util", "time"] }
```

在写之前先核对其他 crate 引用兄弟 crate 的写法(`grep -n "mullion-" crates/mullion-app/Cargo.toml`),要和它们保持一致。
`mullion-store` 本切片还没被调用;如果编出 `unused crate` 警告,就先不加 store,等 F346 再加。不要为了消警告去写假调用。

- [ ] **Step 3:`crates/mullion-ffi/uniffi.toml`**

```toml
[bindings.kotlin]
package_name = "io.github.kilobitcy.mullion.ffi"
```

- [ ] **Step 4:`src/lib.rs` 与 `src/bin/uniffi-bindgen.rs`**

```rust
//! Android 端的 Rust 内核出口(ADR-013,F345 起)。
//!
//! 与 `mullion-app` 平级的第二个组装端:把 term / ssh / store 拼起来,
//! 经 uniffi 导出给 Kotlin。这里**不写判据**——判据在 store(F344),
//! 这里只做 IO 编排与类型转换。

uniffi::setup_scaffolding!();

/// 内核版本号,Kotlin 侧用来确认加载的 `.so` 是不是这一版。
#[uniffi::export]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_the_workspace_version() {
        assert_eq!(super::version(), env!("CARGO_PKG_VERSION"));
    }
}
```

```rust
//! uniffi 绑定生成器。只在 `--features bindgen` 下编译,见 Cargo.toml。
fn main() {
    uniffi::uniffi_bindgen_main()
}
```

- [ ] **Step 5:写依赖方向守护 `tests/dependency_direction.rs`**

```rust
//! ADR-013 的机械守护:app 与 ffi 是两个平级组装端,**互不依赖**。
//!
//! 最省事的歪路是「ffi 里直接调 app 的某个函数」或「app 里顺手用 ffi 的
//! 类型」——一旦成立,判据就会长回 app 里,两端分叉。

use std::process::Command;

fn tree(pkg: &str) -> String {
    let out = Command::new(env!("CARGO"))
        .args(["tree", "-p", pkg, "--edges", "normal", "--prefix", "none"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo tree 应能执行");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn has(tree: &str, krate: &str) -> bool {
    tree.lines().any(|l| l.starts_with(&format!("{krate} ")))
}

#[test]
fn ffi_never_depends_on_app() {
    let t = tree("mullion-ffi");
    // 防恒绿:先确认解析拿得到它真依赖的东西。
    assert!(has(&t, "mullion-term"), "解析失效:ffi 树里找不到 mullion-term\n{t}");
    assert!(!has(&t, "mullion-app"), "mullion-ffi 依赖了 mullion-app\n{t}");
}

#[test]
fn app_never_depends_on_ffi() {
    let t = tree("mullion-app");
    assert!(has(&t, "mullion-term"), "解析失效:app 树里找不到 mullion-term\n{t}");
    assert!(!has(&t, "mullion-ffi"), "mullion-app 依赖了 mullion-ffi\n{t}");
}
```

- [ ] **Step 6:主机上跑测试**

```bash
cargo test -p mullion-ffi 2>&1 | grep -E "^test |test result"
```
Expected:`version_is_the_workspace_version`、`ffi_never_depends_on_app`、`app_never_depends_on_ffi` 全 ok。

- [ ] **Step 7:Android 交叉编译两个 ABI**

```bash
export ANDROID_NDK_HOME=$HOME/Android/Sdk/ndk/29.0.14206865
cargo ndk -t arm64-v8a -t x86_64 --platform 30 build -p mullion-ffi 2>&1 | tail -3
ls -la target/aarch64-linux-android/debug/libmullion_ffi.so target/x86_64-linux-android/debug/libmullion_ffi.so
```
Expected:两个 `.so` 都在。ring 的 C 部分由 cargo-ndk 设好的 NDK clang 编译。
如果失败,先看报错里是哪个 crate、缺什么工具,再针对性处理。同一处改两次还没过,就停下来上报。

- [ ] **Step 8:生成 Kotlin 绑定冒烟**

```bash
cargo build -p mullion-ffi
cargo run -p mullion-ffi --features bindgen --bin uniffi-bindgen -- \
  generate --library target/debug/libmullion_ffi.so --language kotlin --out-dir /tmp/f345-bindings
find /tmp/f345-bindings -name "*.kt" | head; grep -n "fun \`version\`\|fun version" -r /tmp/f345-bindings | head -3
```
Expected:生成的 `.kt` 位于 `io/github/kilobitcy/mullion/ffi/` 下,里面有 `version` 函数。
如果包名不对,说明 library mode 没读到 `uniffi.toml`。这时跑 `... uniffi-bindgen -- generate --help`,看用哪个参数把配置文件传进去,再把用法记进 Task 7 的脚本。

- [ ] **Step 9:clippy + 提交**

```bash
cargo clippy -p mullion-ffi --all-targets -- -D warnings 2>&1 | tail -2
git add Cargo.toml Cargo.lock crates/mullion-ffi
git commit -m "feat(ffi): mullion-ffi 骨架与依赖方向守护 (F345)

uniffi 0.32 proc-macro 模式;arm64-v8a / x86_64 交叉编译通过。
守护测试:dependency_direction 两条(app 与 ffi 互不依赖,带防恒绿前置断言)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: 网格 → 纯文本投影(`screen.rs`)

**Files:**
- Create: `crates/mullion-ffi/src/screen.rs`
- Modify: `crates/mullion-ffi/src/lib.rs`(加 `mod screen; pub use screen::ScreenText;`)

- [ ] **Step 1:写失败测试(连同类型签名)**

```rust
//! 网格快照 → 纯文本行。**F345 最小终端专用**:F351 会换成「按行合并同样式段」
//! 的打包(带颜色、反显、光标形状),届时这里整体替换。

use mullion_term::snapshot::GridSnapshot;

/// 一帧屏幕的纯文本投影。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ScreenText {
    /// 每行文本,行尾空白已去掉。宽字符只出现一次(右半占位格跳过)。
    pub rows: Vec<String>,
    pub cursor_row: u16,
    /// 光标所在的**格**列号(宽字符占两格),不是 `rows[r]` 里的字符下标。
    pub cursor_col: u16,
}

pub fn project(snap: &GridSnapshot) -> ScreenText {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_term::emulator::Emulator;

    fn screen_after(bytes: &[u8]) -> ScreenText {
        let mut e = Emulator::new(10, 3);
        e.feed(bytes);
        project(&e.snapshot())
    }

    #[test]
    fn plain_rows_come_out_trimmed() {
        let s = screen_after(b"ab\r\ncd");
        assert_eq!(s.rows, vec!["ab", "cd", ""]);
    }

    #[test]
    fn a_wide_char_appears_once_not_twice() {
        // 宽字符右半是 spacer 格(F16),不跳过就会变成「中中」。
        let s = screen_after("a中b".as_bytes());
        assert_eq!(s.rows[0], "a中b");
    }

    #[test]
    fn cursor_is_reported_in_cells_not_chars() {
        let s = screen_after("中".as_bytes());
        assert_eq!((s.cursor_row, s.cursor_col), (0, 2));
    }
}
```

- [ ] **Step 2:跑测试确认失败**

```bash
cargo test -p mullion-ffi screen:: 2>&1 | grep -E "^test |panicked" | head
```
Expected:3 条都 panic,报 `not yet implemented`。

- [ ] **Step 3:实现**

```rust
pub fn project(snap: &GridSnapshot) -> ScreenText {
    let rows = (0..snap.rows)
        .map(|r| {
            let line: String = snap
                .row(r)
                .iter()
                .filter(|c| !c.spacer)
                .map(|c| c.ch)
                .collect();
            line.trim_end().to_string()
        })
        .collect();
    ScreenText {
        rows,
        cursor_row: snap.cursor.row,
        cursor_col: snap.cursor.col,
    }
}
```

如果 `Emulator` 的模块路径不是 `mullion_term::emulator::Emulator`,以 `crates/mullion-term/src/lib.rs` 为准改正。

- [ ] **Step 4:跑测试确认通过,clippy,提交**

```bash
cargo test -p mullion-ffi 2>&1 | grep -E "test result"
cargo clippy -p mullion-ffi --all-targets -- -D warnings 2>&1 | tail -1
git add crates/mullion-ffi
git commit -m "feat(ffi): 网格快照投影成纯文本行 (F345)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: `Terminal` 对象与泵(T1 / T2 / T4)

**Files:**
- Create: `crates/mullion-ffi/src/runtime.rs`
- Create: `crates/mullion-ffi/src/terminal.rs`
- Create: `crates/mullion-ffi/tests/common/mod.rs`
- Create: `crates/mullion-ffi/tests/terminal_roundtrip.rs`
- Modify: `crates/mullion-ffi/src/lib.rs`(加 `mod runtime; mod terminal; pub use terminal::{ConnectParams, FfiError, ScreenListener, Terminal};`)

- [ ] **Step 1:`runtime.rs`**

```rust
//! 进程级 tokio 运行时。Kotlin 侧没有 tokio,所有 async 都在这里跑;
//! 导出给 Kotlin 的函数一律是**阻塞**调用,由 Kotlin 放到 `Dispatchers.IO` 上调。

use std::sync::OnceLock;

pub(crate) fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("mullion-ffi")
            .enable_all()
            .build()
            .expect("tokio 运行时建不起来")
    })
}
```

- [ ] **Step 2:写 `terminal.rs` 的纯函数部分和它的失败测试(T1)**

```rust
//! 一条 SSH PTY 会话 + 一个仿真器,导出给 Kotlin(F345 最小终端)。
//!
//! 泵任务的三条红线:
//! - **T1**:仿真器产生的应答(光标查询 / DA / 同步探测)必须回写 channel,
//!   否则全屏 TUI 卡死。见 [`absorb`]。
//! - **T2**:BSU 之后字节攒在 vte 肚子里,调用方不主动收口就永远不到期。
//!   泵在没有新字节时也要按 `vt_sync_deadline` 醒一次。
//! - **T4**:`resize` 同时改仿真器和远端 PTY,只改一边就错行。

use std::sync::{Arc, Mutex};

use mullion_term::emulator::Emulator;

/// 一包入站字节进仿真器,返回**必须回写 channel** 的应答(T1)。
pub(crate) fn absorb(emu: &mut Emulator, bytes: &[u8]) -> Vec<u8> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pty_write_reply_is_returned_for_writeback() {
        // T1:CSI 6n(光标位置查询)必须产出应答 CSI 1;1R。
        let mut e = Emulator::new(80, 24);
        assert_eq!(absorb(&mut e, b"\x1b[6n"), b"\x1b[1;1R".to_vec());
        // 读即清:同一份应答不会被回写两次。
        assert!(absorb(&mut e, b"").is_empty());
    }

    #[test]
    fn emulator_can_live_on_the_pump_task() {
        // 泵在 tokio 工作线程上跑,仿真器得能跨线程。
        fn assert_send<T: Send>() {}
        assert_send::<Emulator>();
    }
}
```

```bash
cargo test -p mullion-ffi terminal:: 2>&1 | grep -E "^test |panicked" | head
```
Expected:`pty_write_reply_is_returned_for_writeback` panic,报 `not yet implemented`。
如果 `emulator_can_live_on_the_pump_task` **编译不过**(`Emulator` 不是 `Send`),停下来上报:那意味着泵要改成专用线程,属于设计变更。

- [ ] **Step 3:实现 `absorb`,跑通**

```rust
pub(crate) fn absorb(emu: &mut Emulator, bytes: &[u8]) -> Vec<u8> {
    emu.feed(bytes);
    emu.take_pty_writes()
}
```

```bash
cargo test -p mullion-ffi terminal:: 2>&1 | grep -E "test result"
```

- [ ] **Step 4:写测试用 echo sshd `tests/common/mod.rs`**

它和 `mullion-ssh/tests/common/mod.rs` 的 `EchoHandler` 是同一协议,多了一点:**记下收到的全部字节**,T1 的端到端断言要靠它。主机密钥复用 ssh crate 的 fixture。

```rust
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
    concat!(env!("CARGO_MANIFEST_DIR"), "/../mullion-ssh/tests/fixtures/server_hostkey").to_string()
}

/// 在 `bind` 上起 echo sshd,返回实际地址和「收到的字节」记录。
pub async fn spawn_recording_echo(bind: &str) -> (std::net::SocketAddr, Received) {
    let host_key = russh::keys::load_secret_key(host_key_path(), None).expect("load hostkey");
    let mut config = russh::server::Config::default();
    config.keys.push(host_key);
    let config = Arc::new(config);
    let received: Received = Arc::default();

    let listener = tokio::net::TcpListener::bind(bind).await.expect("bind echo sshd");
    let addr = listener.local_addr().unwrap();
    let rec = received.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let config = config.clone();
            let handler = RecordingEcho { received: rec.clone() };
            tokio::spawn(async move {
                let _ = russh::server::run_stream(config, stream, handler).await;
            });
        }
    });
    (addr, received)
}
```

Handler 方法签名以 `crates/mullion-ssh/tests/common/mod.rs` 为准:那一份已在本仓库锁定的 russh 0.54 上编过。

- [ ] **Step 5:写失败的端到端测试 `tests/terminal_roundtrip.rs`**

```rust
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
fn server() -> (tokio::runtime::Runtime, std::net::SocketAddr, common::Received) {
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
        wait_until(&rx, || rec.lock().unwrap().windows(needle.len()).any(|w| w == needle)),
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
```

```bash
cargo test -p mullion-ffi --test terminal_roundtrip 2>&1 | grep -E "^error|cannot find" | head -5
```
Expected:编译失败,报找不到 `ConnectParams` / `Terminal` / `ScreenListener`。

- [ ] **Step 6:实现 `terminal.rs` 的其余部分**

接在 `absorb` 之后、`#[cfg(test)]` 之前:

```rust
use mullion_ssh::config::{AuthMethod, SshConfig};
use mullion_ssh::known_hosts::{KnownHosts, TofuAccept};
use mullion_ssh::session::{connect, SshSession, TrySendErr};
use tokio::sync::mpsc;

use crate::runtime::runtime;
use crate::screen::{project, ScreenText};

/// 连接参数(F345:只支持密码认证;私钥、跳板、项目是 F346/F349 的事)。
#[derive(uniffi::Record)]
pub struct ConnectParams {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub cols: u16,
    pub rows: u16,
}

/// Kotlin 侧会收到 `FfiException`。
#[derive(Debug, uniffi::Error)]
#[uniffi(flat_error)]
pub enum FfiError {
    /// 握手 / 认证 / 开 PTY 失败。
    Connect(String),
    /// 出站队列满(大段粘贴 + 慢链路)。
    Busy,
    /// channel 已关。
    Closed,
}

impl std::fmt::Display for FfiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(e) => write!(f, "连接失败:{e}"),
            Self::Busy => write!(f, "发送队列已满,稍后再试"),
            Self::Closed => write!(f, "连接已关闭"),
        }
    }
}

impl std::error::Error for FfiError {}

impl From<TrySendErr> for FfiError {
    fn from(e: TrySendErr) -> Self {
        match e {
            TrySendErr::Full => Self::Busy,
            TrySendErr::Closed => Self::Closed,
        }
    }
}

/// Kotlin 实现:屏幕有变化 / 连接没了。**在 ffi 的工作线程上回调**,
/// Kotlin 侧只能做「标记脏、请求下一帧」这种轻活,不能阻塞。
#[uniffi::export(with_foreign)]
pub trait ScreenListener: Send + Sync {
    fn on_dirty(&self);
    fn on_closed(&self);
}

#[derive(uniffi::Object)]
pub struct Terminal {
    session: Arc<SshSession>,
    emu: Arc<Mutex<Emulator>>,
    pump: tokio::task::JoinHandle<()>,
}

#[uniffi::export]
impl Terminal {
    /// 阻塞直到握手 + 开 PTY 完成。Kotlin 侧在 `Dispatchers.IO` 上调。
    ///
    /// 主机密钥:F345 用进程内 TOFU(首见即收、不落盘)。**F350 换成真 TOFU**
    /// (包里带来的指纹 + 首见弹窗);本切片的 APK 不发布。
    #[uniffi::constructor]
    pub fn connect(
        params: ConnectParams,
        listener: Arc<dyn ScreenListener>,
    ) -> Result<Arc<Self>, FfiError> {
        let cfg = SshConfig {
            host: params.host,
            port: params.port,
            user: params.user,
            auth: AuthMethod::Password(params.password),
            cols: params.cols,
            rows: params.rows,
            term: "xterm-256color".into(),
            hops: Vec::new(),
        };
        let policy = Arc::new(TofuAccept::new(Arc::new(std::sync::Mutex::new(
            KnownHosts::new(),
        ))));
        let rt = runtime();
        let (session, rx) = rt
            .block_on(connect(&cfg, policy, Arc::new(|| {})))
            .map_err(|e| FfiError::Connect(e.to_string()))?;
        let session = Arc::new(session);
        let emu = Arc::new(Mutex::new(Emulator::new(cfg.cols, cfg.rows)));
        let pump = rt.spawn(pump(rx, emu.clone(), session.clone(), listener));
        Ok(Arc::new(Self { session, emu, pump }))
    }

    /// 把键盘 / 输入框的字节发给远端。非阻塞。
    pub fn write(&self, bytes: Vec<u8>) -> Result<(), FfiError> {
        Ok(self.session.write(bytes)?)
    }

    /// T4:仿真器和远端 PTY **一起**改。
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), FfiError> {
        self.emu.lock().unwrap().resize(cols, rows);
        Ok(self.session.resize(cols, rows)?)
    }

    /// 当前屏幕的纯文本投影。
    pub fn screen(&self) -> ScreenText {
        project(&self.emu.lock().unwrap().snapshot())
    }

    /// 关 channel、停泵。可重复调用。
    pub fn close(&self) {
        self.session.close();
        self.pump.abort();
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.close();
    }
}

async fn pump(
    mut rx: mpsc::Receiver<Vec<u8>>,
    emu: Arc<Mutex<Emulator>>,
    session: Arc<SshSession>,
    listener: Arc<dyn ScreenListener>,
) {
    loop {
        // 锁只在这一句里持有,不跨 await。
        let deadline = emu.lock().unwrap().vt_sync_deadline();
        let bytes = match deadline {
            Some(d) => tokio::select! {
                b = rx.recv() => b,
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(d)) => {
                    // T2:到点了没人来问,超时就形同虚设。
                    if emu.lock().unwrap().flush_expired_sync(std::time::Instant::now()) {
                        listener.on_dirty();
                    }
                    continue;
                }
            },
            None => rx.recv().await,
        };
        let Some(bytes) = bytes else {
            listener.on_closed();
            return;
        };
        let reply = absorb(&mut emu.lock().unwrap(), &bytes);
        if !reply.is_empty() {
            // T1。队列满就丢:应答晚到比卡住泵更糟,远端会重发查询。
            let _ = session.write(reply);
        }
        listener.on_dirty();
    }
}
```

把文件顶部原来的 `use std::sync::{Arc, Mutex};` 和 `use mullion_term::emulator::Emulator;` 跟上面这批 `use` 合并到一起。`ConnectError` 如果没有 `Display`,先看 `crates/mullion-ssh/src/error.rs` 它实现了什么,再改 `map_err`(用 `{e:?}` 也可以,但 Debug 里不能有密码:`SshConfig` 的 Debug 已经打码,见 `config.rs` 的守护测试)。

- [ ] **Step 7:跑通**

```bash
cargo test -p mullion-ffi 2>&1 | grep -E "^test |test result"
```
Expected:`terminal_roundtrip` 的 5 条、单元测试、依赖方向测试全部 ok。

如果 `an_unterminated_sync_block_still_reaches_the_screen_t2` 失败,先确认 vte 的同步超时(150ms)确实被设上了:在测试里打印 `screen()`,并看 `mullion-term` 里 `vt_sync_deadline` 的守护测试是怎么构造输入的。不要去调长等待时间。

- [ ] **Step 8:clippy、Android 交叉编译复核、提交**

```bash
cargo clippy -p mullion-ffi --all-targets -- -D warnings 2>&1 | tail -1
ANDROID_NDK_HOME=$HOME/Android/Sdk/ndk/29.0.14206865 cargo ndk -t arm64-v8a -t x86_64 --platform 30 build -p mullion-ffi 2>&1 | tail -1
git add crates/mullion-ffi
git commit -m "feat(ffi): Terminal 对象与泵,T1 应答回写、T2 同步块超时收口 (F345)

守护测试:terminal::tests::pty_write_reply_is_returned_for_writeback;
terminal_roundtrip 五条(回显上屏 / T1 服务端收到 CPR / T2 无 ESU 仍上屏 / T4 / 错密码报错)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: 宿主机假 sshd(给模拟器用)

**Files:**
- Create: `crates/mullion-ffi/examples/echo_sshd.rs`

- [ ] **Step 1:写 example**

```rust
//! 宿主机上的 echo sshd,给模拟器插桩测试连(模拟器里 `10.0.2.2` = 宿主机回环)。
//! 用户 `testuser` / 密码 `test-password`,只绑回环。
//!
//!     cargo run -p mullion-ffi --example echo_sshd -- 2222
#[path = "../tests/common/mod.rs"]
mod common;

#[tokio::main]
async fn main() {
    let port: u16 = std::env::args().nth(1).and_then(|p| p.parse().ok()).unwrap_or(2222);
    let (addr, _rec) = common::spawn_recording_echo(&format!("127.0.0.1:{port}")).await;
    println!("echo sshd listening on {addr}");
    std::future::pending::<()>().await;
}
```

- [ ] **Step 2:冒烟**

```bash
(timeout 5 cargo run -q -p mullion-ffi --example echo_sshd -- 2222 &) ; sleep 4; ss -ltn | grep ':2222 '
```
Expected:能看到 `127.0.0.1:2222` 在 LISTEN。

- [ ] **Step 3:clippy + 提交**

```bash
cargo clippy -p mullion-ffi --all-targets -- -D warnings 2>&1 | tail -1
git add crates/mullion-ffi/examples
git commit -m "test(ffi): 宿主机 echo sshd 供模拟器插桩测试 (F345)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: Android Gradle 工程骨架 + Rust 构建接线

**Files:**
- Create: `android/settings.gradle.kts`、`android/build.gradle.kts`、`android/gradle.properties`、`android/gradle/libs.versions.toml`
- Create: `android/gradle/wrapper/*`、`android/gradlew`、`android/gradlew.bat`(从 `/data/HPlayer` 复制,版本一致)
- Create: `android/app/build.gradle.kts`、`android/app/src/main/AndroidManifest.xml`
- Create: `android/scripts/build-rust.sh`
- Modify: `.gitignore`

- [ ] **Step 1:wrapper**

```bash
mkdir -p android/gradle/wrapper
cp /data/HPlayer/gradlew /data/HPlayer/gradlew.bat android/
cp /data/HPlayer/gradle/wrapper/gradle-wrapper.jar /data/HPlayer/gradle/wrapper/gradle-wrapper.properties android/gradle/wrapper/
grep distributionUrl android/gradle/wrapper/gradle-wrapper.properties
```
Expected:`gradle-9.7.1-bin.zip`

- [ ] **Step 2:`android/settings.gradle.kts`**

```kotlin
// Google 仓库先走阿里云镜像:maven.google.com 的构件会 301 到 dl.google.com,
// 而本机代理对 dl.google.com 的 TLS 会被断(docs/android-build.md)。
// google() 留在后面兜底,别的机器网络正常时照样能解析。
pluginManagement {
    repositories {
        maven("https://maven.aliyun.com/repository/google")
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        maven("https://maven.aliyun.com/repository/google")
        google()
        mavenCentral()
    }
}

rootProject.name = "Mullion"
include(":app")
```

- [ ] **Step 3:`android/gradle.properties`**

```properties
org.gradle.jvmargs=-Xmx4g -Dfile.encoding=UTF-8
android.useAndroidX=true
android.nonTransitiveRClass=true
kotlin.code.style=official
# AGP 9 默认开 built-in Kotlin 与 new DSL,跟显式应用的 kotlin.android /
# kotlin.plugin.compose 插件冲突(本机 HPlayer 已踩过)。关掉,保留经典模式。
android.builtInKotlin=false
android.newDsl=false
```

- [ ] **Step 4:`android/gradle/libs.versions.toml`**

```toml
[versions]
agp = "9.4.0"
kotlin = "2.4.20"
coreKtx = "1.19.0"
composeBom = "2026.09.00"
activityCompose = "1.13.0"
coroutines = "1.11.0"
jna = "5.19.1"
junit = "4.13.2"
androidxTestRunner = "1.7.0"
androidxTestExtJunit = "1.3.0"

[libraries]
androidx-core-ktx = { module = "androidx.core:core-ktx", version.ref = "coreKtx" }
androidx-activity-compose = { module = "androidx.activity:activity-compose", version.ref = "activityCompose" }
androidx-compose-bom = { module = "androidx.compose:compose-bom", version.ref = "composeBom" }
androidx-compose-ui = { module = "androidx.compose.ui:ui" }
androidx-compose-material3 = { module = "androidx.compose.material3:material3" }
androidx-compose-ui-test-junit4 = { module = "androidx.compose.ui:ui-test-junit4" }
androidx-compose-ui-test-manifest = { module = "androidx.compose.ui:ui-test-manifest" }
kotlinx-coroutines-android = { module = "org.jetbrains.kotlinx:kotlinx-coroutines-android", version.ref = "coroutines" }
jna = { module = "net.java.dev.jna:jna", version.ref = "jna" }
junit = { module = "junit:junit", version.ref = "junit" }
androidx-test-runner = { module = "androidx.test:runner", version.ref = "androidxTestRunner" }
androidx-test-ext-junit = { module = "androidx.test.ext:junit", version.ref = "androidxTestExtJunit" }

[plugins]
android-application = { id = "com.android.application", version.ref = "agp" }
kotlin-android = { id = "org.jetbrains.kotlin.android", version.ref = "kotlin" }
kotlin-compose = { id = "org.jetbrains.kotlin.plugin.compose", version.ref = "kotlin" }
```

- [ ] **Step 5:`android/build.gradle.kts`**

```kotlin
plugins {
    alias(libs.plugins.android.application) apply false
    alias(libs.plugins.kotlin.android) apply false
    alias(libs.plugins.kotlin.compose) apply false
}
```

- [ ] **Step 6:`android/scripts/build-rust.sh`**

```bash
#!/usr/bin/env bash
# 编 libmullion_ffi.so 进 jniLibs,并生成 Kotlin 绑定(F345)。由 Gradle preBuild 调用。
# ABIS 默认两个:arm64-v8a 给真机,x86_64 只给模拟器测试(发布包在 F355 里只留 arm64)。
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
SDK=${ANDROID_HOME:-$HOME/Android/Sdk}
export ANDROID_NDK_HOME=${ANDROID_NDK_HOME:-$SDK/ndk/29.0.14206865}
ABIS=${ABIS:-"arm64-v8a x86_64"}
PROFILE=${PROFILE:-debug}

cd "$ROOT"
targets=(); for a in $ABIS; do targets+=(-t "$a"); done
release=(); [ "$PROFILE" = release ] && release=(--release)
cargo ndk "${targets[@]}" --platform 30 -o android/app/src/main/jniLibs build -p mullion-ffi "${release[@]}"

# 绑定从宿主机编的 .so 生成:library mode 只读里面的 uniffi 元数据,与 ABI 无关。
cargo build -p mullion-ffi
out=android/app/build/generated/uniffi
rm -rf "$out"
cargo run -q -p mullion-ffi --features bindgen --bin uniffi-bindgen -- \
  generate --library target/debug/libmullion_ffi.so --language kotlin --out-dir "$out"
```

如果 Task 3 Step 8 发现 library mode 读不到 `uniffi.toml`,在最后一条命令上补上当时查到的配置参数。

```bash
chmod +x android/scripts/build-rust.sh
```

- [ ] **Step 7:`android/app/build.gradle.kts`**

```kotlin
plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.compose)
}

android {
    namespace = "io.github.kilobitcy.mullion"
    compileSdk = 36

    defaultConfig {
        applicationId = "io.github.kilobitcy.mullion"
        minSdk = 30
        targetSdk = 36
        // 版本号独立于桌面(ADR-013 / F355):android-v0.x.y。
        versionCode = 1
        versionName = "0.0.1"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        ndk {
            // x86_64 只为模拟器测试;发布包(F355)只留 arm64-v8a。
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_21
        targetCompatibility = JavaVersion.VERSION_21
    }

    kotlin {
        jvmToolchain(21)
    }

    buildFeatures {
        compose = true
    }

    sourceSets["main"].java.srcDir("build/generated/uniffi")
}

// 每次构建前先编 Rust 并重新生成绑定,免得 .so 和绑定跟源码脱节、悄悄跑旧代码。
val buildRust by tasks.registering(Exec::class) {
    workingDir = rootProject.projectDir
    commandLine("bash", "scripts/build-rust.sh")
}
tasks.named("preBuild") { dependsOn(buildRust) }

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.activity.compose)
    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.compose.ui)
    implementation(libs.androidx.compose.material3)
    implementation(libs.kotlinx.coroutines.android)
    // uniffi 生成的 Kotlin 经 JNA 调 .so(要求 ≥5.12);aar 自带各 ABI 的 libjnidispatch。
    implementation("${libs.jna.get()}@aar")

    testImplementation(libs.junit)
    androidTestImplementation(libs.androidx.test.runner)
    androidTestImplementation(libs.androidx.test.ext.junit)
    androidTestImplementation(platform(libs.androidx.compose.bom))
    androidTestImplementation(libs.androidx.compose.ui.test.junit4)
    debugImplementation(libs.androidx.compose.ui.test.manifest)
}
```

如果 `"${libs.jna.get()}@aar"` 这种写法 AGP 9 / Gradle 9.7 不认,改成字面量 `implementation("net.java.dev.jna:jna:5.19.1@aar")`,版本号与 toml 保持一致。`compileSdk = 36` 用的是已经装好的 `platforms;android-36`。

- [ ] **Step 8:`android/app/src/main/AndroidManifest.xml`(先放占位 Activity,Task 8 再填内容)**

```xml
<?xml version="1.0" encoding="utf-8"?>
<manifest xmlns:android="http://schemas.android.com/apk/res/android">
    <uses-permission android:name="android.permission.INTERNET" />
    <application
        android:label="Mullion"
        android:theme="@android:style/Theme.Material.NoActionBar">
        <activity
            android:name=".MainActivity"
            android:exported="true"
            android:windowSoftInputMode="adjustResize">
            <intent-filter>
                <action android:name="android.intent.action.MAIN" />
                <category android:name="android.intent.category.LAUNCHER" />
            </intent-filter>
        </activity>
    </application>
</manifest>
```

占位 `android/app/src/main/java/io/github/kilobitcy/mullion/MainActivity.kt`:

```kotlin
package io.github.kilobitcy.mullion

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.material3.Text
import io.github.kilobitcy.mullion.ffi.version

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent { Text("mullion-ffi ${version()}") }
    }
}
```

- [ ] **Step 9:`.gitignore` 追加**

```gitignore

# Android(F345):Gradle 产物、本机 SDK 路径、cargo-ndk 编出的 .so(构建时生成)
/android/.gradle/
/android/build/
/android/app/build/
/android/local.properties
/android/app/src/main/jniLibs/
```

- [ ] **Step 10:编 APK**

```bash
cd android && ./gradlew --no-daemon assembleDebug 2>&1 | tail -5; cd ..
unzip -l android/app/build/outputs/apk/debug/app-debug.apk | grep -E "libmullion_ffi|libjnidispatch"
```
Expected:`BUILD SUCCESSFUL`;APK 里 arm64-v8a 和 x86_64 各有一份 `libmullion_ffi.so` 和 `libjnidispatch.so`。

如果拉依赖失败,先看报错里的 URL 是哪个仓库,再判断问题出在网络还是版本号。不要去改代理配置。

- [ ] **Step 11:装到模拟器上看版本号**

```bash
android/scripts/emulator.sh start
~/Android/Sdk/platform-tools/adb install -r android/app/build/outputs/apk/debug/app-debug.apk
~/Android/Sdk/platform-tools/adb shell am start -W -n io.github.kilobitcy.mullion/.MainActivity
sleep 2; ~/Android/Sdk/platform-tools/adb shell uiautomator dump /sdcard/ui.xml >/dev/null; ~/Android/Sdk/platform-tools/adb shell cat /sdcard/ui.xml | grep -o 'mullion-ffi [0-9.]*'
```
Expected:输出 `mullion-ffi 0.1.142`(workspace 版本号)。能看到这一行,说明 .so 已加载,JNA 也通了。

- [ ] **Step 12:提交**

```bash
git add .gitignore android
git status --short android | grep -E "jniLibs|/build/" && echo "!! 生成物混进来了" || true
git commit -m "feat(android): Gradle 工程骨架,preBuild 编 Rust 并生成 uniffi 绑定 (F345)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: 最小终端界面 + 模拟器插桩测试

**Files:**
- Modify: `android/app/src/main/java/io/github/kilobitcy/mullion/MainActivity.kt`
- Create: `android/app/src/main/java/io/github/kilobitcy/mullion/TerminalViewModel.kt`
- Create: `android/app/src/androidTest/java/io/github/kilobitcy/mullion/FfiRoundTripTest.kt`
- Create: `android/app/src/androidTest/java/io/github/kilobitcy/mullion/MinimalTerminalUiTest.kt`
- Create: `android/scripts/e2e.sh`

**写 Kotlin 前先读生成的绑定** `android/app/build/generated/uniffi/io/github/kilobitcy/mullion/ffi/*.kt`,核对下面几样的实际名字和类型,计划里的写法与它不一致时**以生成代码为准**:
- 命名构造函数在 Kotlin 里叫 `Terminal.connect(...)`(companion)还是别的;
- `u16` 映射成 `UShort`;
- `Vec<u8>` 映射成 `ByteArray`;
- 错误类型叫 `FfiException`;
- 回调接口的名字和签名。

- [ ] **Step 1:写插桩测试 —— 绑定层 `FfiRoundTripTest.kt`**

```kotlin
package io.github.kilobitcy.mullion

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import io.github.kilobitcy.mullion.ffi.ConnectParams
import io.github.kilobitcy.mullion.ffi.ScreenListener
import io.github.kilobitcy.mullion.ffi.Terminal
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit

/**
 * F345:在模拟器里经 JNA → libmullion_ffi.so → 真 SSH 连宿主机的 echo sshd
 * (`android/scripts/e2e.sh` 负责拉起)。地址可用 `-e host/port` 覆盖。
 */
@RunWith(AndroidJUnit4::class)
class FfiRoundTripTest {
    private val args = InstrumentationRegistry.getArguments()
    private val host = args.getString("host") ?: "10.0.2.2"
    private val port = (args.getString("port") ?: "2222").toUShort()

    @Test
    fun typedTextComesBackOnScreen() {
        val dirty = LinkedBlockingQueue<Unit>()
        val term = Terminal.connect(
            ConnectParams(host, port, "testuser", "test-password", 40u, 5u),
            object : ScreenListener {
                override fun onDirty() { dirty.offer(Unit) }
                override fun onClosed() {}
            },
        )
        try {
            term.write("hello".toByteArray())
            val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(10)
            while (term.screen().rows[0] != "hello" && System.nanoTime() < deadline) {
                dirty.poll(100, TimeUnit.MILLISECONDS)
            }
            assertEquals("hello", term.screen().rows[0])
        } finally {
            term.close()
        }
    }
}
```

- [ ] **Step 2:写 `android/scripts/e2e.sh`**

```bash
#!/usr/bin/env bash
# F345 端到端:宿主机起 echo sshd → 模拟器跑插桩测试 → 收尾。
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT"
cargo build -q -p mullion-ffi --example echo_sshd
target/debug/examples/echo_sshd 2222 > /tmp/mullion-echo-sshd.log 2>&1 &
SSHD=$!
trap 'kill $SSHD 2>/dev/null || true' EXIT
for _ in $(seq 50); do ss -ltn | grep -q '127.0.0.1:2222 ' && break; sleep 0.1; done
android/scripts/emulator.sh start
cd android && ./gradlew --no-daemon connectedDebugAndroidTest "$@"
```

```bash
chmod +x android/scripts/e2e.sh
android/scripts/e2e.sh 2>&1 | tail -15
```
Expected:`FfiRoundTripTest > typedTextComesBackOnScreen` 通过,`BUILD SUCCESSFUL`。这一步只是 Rust 内核与绑定的端到端,UI 还没写,所以应该直接绿。

如果连不上 `10.0.2.2:2222`,先 `adb shell` 里用 `nc -z 10.0.2.2 2222` 判断问题在网络还是在 SSH,再对症处理。

- [ ] **Step 3:写 UI 插桩测试 `MinimalTerminalUiTest.kt`(此时会失败)**

```kotlin
package io.github.kilobitcy.mullion

import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.performTextReplacement
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class MinimalTerminalUiTest {
    @get:Rule val rule = createAndroidComposeRule<MainActivity>()

    private val args = InstrumentationRegistry.getArguments()

    @Test
    fun connectTypeAndSeeTheEcho() {
        rule.onNodeWithTag("host").performTextReplacement(args.getString("host") ?: "10.0.2.2")
        rule.onNodeWithTag("port").performTextReplacement(args.getString("port") ?: "2222")
        rule.onNodeWithTag("user").performTextReplacement("testuser")
        rule.onNodeWithTag("password").performTextReplacement("test-password")
        rule.onNodeWithTag("connect").performClick()

        rule.waitUntil(10_000) {
            rule.onAllNodes(hasText("已连接")).fetchSemanticsNodes().isNotEmpty()
        }
        rule.onNodeWithTag("input").performTextInput("hi-from-ui")
        rule.onNodeWithTag("send").performClick()
        rule.waitUntil(10_000) {
            rule.onAllNodes(hasText("hi-from-ui", substring = true) and hasTestTagRow())
                .fetchSemanticsNodes().isNotEmpty()
        }
    }
}

private fun hasTestTagRow() =
    androidx.compose.ui.test.SemanticsMatcher("终端行") {
        it.config.getOrNull(androidx.compose.ui.semantics.SemanticsProperties.TestTag)
            ?.startsWith("row-") == true
    }
```

```bash
android/scripts/e2e.sh 2>&1 | grep -E "Test >|FAILED|BUILD" | tail -5
```
Expected:`MinimalTerminalUiTest` FAILED(找不到 tag 为 `host` 的节点)。

- [ ] **Step 4:`TerminalViewModel.kt`**

```kotlin
package io.github.kilobitcy.mullion

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import io.github.kilobitcy.mullion.ffi.ConnectParams
import io.github.kilobitcy.mullion.ffi.ScreenListener
import io.github.kilobitcy.mullion.ffi.Terminal
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * F345 最小终端的状态。**临时形态**:F351 换成 Canvas 绘制 + Choreographer 节流,
 * F352 换成本地编辑框 + 辅助键栏;这里只证明「连得上、看得见、发得出」。
 */
class TerminalViewModel : ViewModel() {
    var status by mutableStateOf("未连接")
        private set
    var rows by mutableStateOf(emptyList<String>())
        private set

    private var term: Terminal? = null

    fun connect(host: String, port: String, user: String, password: String) {
        val p = port.toUShortOrNull() ?: run { status = "端口无效"; return }
        status = "连接中…"
        viewModelScope.launch {
            status = try {
                val t = withContext(Dispatchers.IO) {
                    Terminal.connect(ConnectParams(host, p, user, password, 80u, 24u), listener)
                }
                term = t
                refresh()
                "已连接"
            } catch (e: Exception) {
                "连接失败:${e.message}"
            }
        }
    }

    fun send(text: String) {
        val t = term ?: return
        try {
            t.write((text + "\r").toByteArray())
        } catch (e: Exception) {
            status = "发送失败:${e.message}"
        }
    }

    private fun refresh() {
        term?.let { rows = it.screen().rows }
    }

    // 回调在 Rust 工作线程上:只把刷新切回主线程,不在这里做重活。
    private val listener = object : ScreenListener {
        override fun onDirty() {
            viewModelScope.launch(Dispatchers.Main) { refresh() }
        }

        override fun onClosed() {
            viewModelScope.launch(Dispatchers.Main) { status = "连接已断开" }
        }
    }

    override fun onCleared() {
        term?.close()
        term = null
    }
}
```

`androidx.lifecycle.ViewModel` 和 `viewModelScope` 是 activity-compose 传递带进来的。编译报找不到的话,在 toml 里加 `androidx.lifecycle:lifecycle-viewmodel-compose:2.11.0`(HPlayer 用的就是这个版本),并在 MainActivity 里用 `viewModel()`。

- [ ] **Step 5:`MainActivity.kt`**

```kotlin
package io.github.kilobitcy.mullion

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.viewModels
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

class MainActivity : ComponentActivity() {
    private val vm: TerminalViewModel by viewModels()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            MaterialTheme {
                Surface(Modifier.fillMaxSize()) { Screen(vm) }
            }
        }
    }
}

@Composable
private fun Screen(vm: TerminalViewModel) {
    var host by remember { mutableStateOf("") }
    var port by remember { mutableStateOf("22") }
    var user by remember { mutableStateOf("") }
    var password by remember { mutableStateOf("") }
    var input by remember { mutableStateOf("") }

    Column(Modifier.fillMaxSize().safeDrawingPadding().imePadding().padding(8.dp)) {
        if (vm.status != "已连接") {
            OutlinedTextField(host, { host = it }, Modifier.fillMaxWidth().testTag("host"), label = { Text("主机") })
            OutlinedTextField(port, { port = it }, Modifier.fillMaxWidth().testTag("port"), label = { Text("端口") })
            OutlinedTextField(user, { user = it }, Modifier.fillMaxWidth().testTag("user"), label = { Text("用户") })
            OutlinedTextField(
                password, { password = it }, Modifier.fillMaxWidth().testTag("password"),
                label = { Text("密码") }, visualTransformation = PasswordVisualTransformation(),
            )
            Button({ vm.connect(host, port, user, password) }, Modifier.testTag("connect")) { Text("连接") }
        }
        Text(vm.status, Modifier.testTag("status"))
        Column(Modifier.weight(1f).horizontalScroll(rememberScrollState())) {
            vm.rows.forEachIndexed { i, line ->
                Text(line, Modifier.testTag("row-$i"), fontFamily = FontFamily.Monospace, fontSize = 11.sp, softWrap = false)
            }
        }
        Row {
            OutlinedTextField(input, { input = it }, Modifier.weight(1f).testTag("input"))
            Button({ vm.send(input); input = "" }, Modifier.testTag("send")) { Text("发送") }
        }
    }
}
```

- [ ] **Step 6:跑通两条插桩测试**

```bash
android/scripts/e2e.sh 2>&1 | grep -E "Test >|tests completed|FAILED|BUILD" | tail -6
```
Expected:`FfiRoundTripTest` 与 `MinimalTerminalUiTest` 都通过,`BUILD SUCCESSFUL`。

- [ ] **Step 7:提交**

```bash
git add android
git commit -m "feat(android): 最小终端界面与模拟器插桩测试 (F345)

宿主机 echo sshd(10.0.2.2:2222)→ 模拟器;绑定层与 UI 层各一条插桩测试,
入口 android/scripts/e2e.sh

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9: 文档与不变量表、全量绿、终审

**Files:**
- Modify: `CLAUDE.md`(「边界」一句、架构块、依赖方向一句、「构建与测试」、目录约定里 `docs/` 关键文件列表)
- Modify: `docs/android-build.md`(补构建与测试)
- Modify: `spec.md`(F345 行标已实现)

- [ ] **Step 1:CLAUDE.md —— 逐处精确替换**

1. 「边界」最后一条:
   - 旧:`- macOS/Linux 能编过就行，**Windows 11 是唯一的一等公民**。`
   - 新:`- macOS/Linux 能编过就行；**Windows 11 与 Android（arm64，minSdk 30）是一等公民**（ADR-013）。手机端永不做分屏、内置编辑器、端口隧道、F100。`
2. 架构代码块:
   - `mullion-app` 一行末尾的「唯一允许知道其余四者的地方。」改为「组装端之一(桌面)。」;
   - 紧接其后新增一行:`mullion-ffi      uniffi 导出给 Android(Kotlin)。组装端之二,与 app 平级;tokio 编排在这里,判据不在这里(判据在 store)。`
3. 依赖方向一句:
   - 旧:`**依赖方向严格单向**：`app → {core, term, ssh, store, cloud}`，其余互不依赖。`
   - 新:`**依赖方向严格单向**：两个组装端 `app`、`ffi` 各自可依赖 `{core, term, ssh, store, cloud}`；**app 与 ffi 互不依赖**，其余互不依赖（守护:`mullion-ffi/tests/dependency_direction.rs`）。`
4. 「如果你觉得某个功能非得打破这个方向才能实现」那段保留不动。
5. 「构建与测试」的命令块之后、「**「绿」的定义**」之前插入:

````markdown
Android(F345 起,工具链与网络坑见 `docs/android-build.md`):
```bash
android/scripts/emulator.sh start             # 无头 KVM 模拟器
android/scripts/e2e.sh                        # 宿主机 echo sshd + connectedDebugAndroidTest
cd android && ./gradlew assembleDebug         # preBuild 会先 cargo ndk + 生成 uniffi 绑定
```
**Android 绿**:上面的「绿」**加上** `android/scripts/e2e.sh` 全过。只改桌面的切片不要求跑它。
````

6. 目录约定的 `docs/` 关键文件列表加一条:`- `android-build.md` —— Android 工具链/镜像源/模拟器/构建与插桩测试运行手册`

- [ ] **Step 2:`docs/android-build.md` 追加**

```markdown
## 构建与测试
- Rust 侧:`cargo test -p mullion-ffi`(含进程内 echo sshd 端到端,纳入 `cargo test --workspace`)
- APK:`cd android && ./gradlew assembleDebug`。`preBuild` 依赖 `buildRust`,也就是 `android/scripts/build-rust.sh`:
  先 `cargo ndk` 编 `arm64-v8a` + `x86_64` 进 `app/src/main/jniLibs/`(不入库),
  再用宿主机 `.so` 跑 uniffi library mode,把绑定生成到 `app/build/generated/uniffi/`
- 端到端:`android/scripts/e2e.sh` 起宿主机 `echo_sshd`(127.0.0.1:2222,模拟器里是 10.0.2.2:2222),
  再跑 `connectedDebugAndroidTest`。要连别的地址,传 `-Pandroid.testInstrumentationRunnerArguments.host=... .port=...`;
  **真机地址和凭据不写进任何被跟踪文件**

## 已知坑
- keyring 在 Android 上会静默回落到内存 mock 库 → store 在 Android 上不编 keyring,`KeyringSource` 不存在(守护:`mullion-store/tests/android_has_no_keyring.rs`)
- AGP 9 必须写 `android.builtInKotlin=false` / `android.newDsl=false`,否则和显式的 Kotlin 插件冲突
- 主机密钥目前是进程内 TOFU(首见即收、不落盘),F350 换成真 TOFU;F355 之前不发 APK
```

- [ ] **Step 3:spec.md**

在 §4.13 表里 F345 那一行的末尾追加:`**已实现(F345 切片)**:mullion-ffi(uniffi 0.32)骨架;store 在 Android 上去掉 keyring;模拟器端到端(绑定层 + UI 层插桩测试)通过`

- [ ] **Step 4:全量绿**

```bash
cargo test --workspace > /tmp/f345.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/f345.log | grep -v " 0 failed"; echo "grep-exit=$?"
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -2
cargo fmt --check && echo fmt-ok
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app 2>&1 | tail -1
android/scripts/e2e.sh 2>&1 | grep -E "tests completed|FAILED|BUILD" | tail -3
```
Expected:
- `grep-exit=1`;
- clippy 以 `Finished` 结尾;
- `fmt-ok`;
- Windows 交叉编译以 `Finished` 结尾,证明 store 的 cfg 没有碰坏桌面;
- e2e 是 `BUILD SUCCESSFUL`。

- [ ] **Step 5:提交**

```bash
git add CLAUDE.md docs/android-build.md spec.md
git commit -m "docs: 不变量表改为两个平级组装端,Android 列为一等公民,补构建手册 (F345)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 6:终审(控制者派 review 子 agent)**

本切片涉及凭据路径(store 的 keyring 隔开)和 SSH 主机密钥策略,按全局约定要起 review 子 agent 复核。重点看三处:
1. `#[cfg(not(target_os = "android"))]` 有没有漏的地方(在 Android 上 grep `keyring` 和 `KeyringSource`,应当零命中);
2. `Terminal::connect` 的错误信息里有没有可能带出密码;
3. 泵的锁有没有跨 await 持有。

- [ ] **Step 7:合回 main**

```bash
git switch main && git merge --squash slice/f345
git diff --cached | grep -nE '192\.168\.|/home/me/brain|PRIVATE KEY|ghp_|github_pat_' && echo "!! 有私密信息" || echo clean
git commit -m "feat(ffi,android): mullion-ffi 骨架、Android 工具链与模拟器最小终端 (F345)

- store 在 Android 上去掉 keyring(静默回落内存库),守护 android_has_no_keyring
- mullion-ffi:Terminal/ScreenListener,T1 应答回写、T2 同步块超时收口,依赖方向守护
- android/:Compose 最小终端,preBuild 编 Rust + 生成 uniffi 绑定;e2e.sh 跑模拟器插桩测试
- CLAUDE.md:app 与 ffi 两个平级组装端;Android 列为一等公民

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```
推送前再跑一次 `git log --stat -1` 看文件清单,确认 `jniLibs` 和 `build/` 不在里面。推送走 socks 代理(见 `github-integration-ops` 记忆)。**不发桌面 Release**:桌面行为没有变化。

---

## 自审记录

- **spec 覆盖**:spec §4 零期 F345 = 「ffi 骨架 + 工具链(NDK、cargo-ndk、Rust target)+ 模拟器跑通最小终端 + 同提交改 CLAUDE.md 不变量表」→ 分别对应 Task 3 / Task 1 / Task 7-8 / Task 9。「keyring 无 Android 后端」这个已知障碍 → Task 2。
- **陷阱守护**:T1、T2 在 ffi 层有同名语义的端到端测试;T4 在 `resize` 里两边一起改,并有测试;T3/T7(帧节流)不在本切片,F351 用 Choreographer 做;T2'(attach 前登记 sync 特性)不在本切片,F349 attach tmux 时做。
- **明确不做**:私钥认证、跳板、项目、真 TOFU、Canvas、辅助键栏、保活、发 APK —— 都在 F346~F355 里。
- **已知风险**:①`Emulator` 不是 `Send` → Task 5 Step 2 的编译期断言会挡住,并停下来上报;②uniffi library mode 不读 `uniffi.toml` → Task 3 Step 8 当场发现、当场改正;③镜像源临时不可用 → 安装步骤里写明了镜像地址,换源不需要改代码。
