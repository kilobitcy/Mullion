# 从 Linux 开发机交叉编译 Windows exe（运行手册）

> 目的：在这台 Ubuntu 开发机上直接产出可在 **Windows 11 实测**的 `mullion.exe`。
> Windows 是本项目唯一一等公民，但代码平时在 Linux 无头容器里写/测——交叉编译是
> 在真机之外**最快暴露 Windows 平台差异**的手段（见「已暴露的坑」）。

## 一次性环境（本机已装好，换机器才需重做）

1. **出网走代理**（关键，不然全断）：`http_proxy`/`https_proxy=http://127.0.0.1:7890`、
   `all_proxy=socks5h://127.0.0.1:7891`；`no_proxy` 含 `192.168.0.0/16`（内网直连，
   如真机 `192.0.2.10` 不走代理）。
   - **`sudo` 会清掉这些环境变量** → `apt` 直连公网必失败（现象：全是 `Ign:`）。
     必须 `sudo -E`，或显式传：
     ```bash
     sudo -E apt-get -o Acquire::http::Proxy=http://127.0.0.1:7890 \
                     -o Acquire::https::Proxy=http://127.0.0.1:7890 install -y mingw-w64
     ```
   - 判断出网是否通别用 `/dev/tcp`（它不走代理，会误判「不可达」）。用 `curl`：
     `curl -sSI https://static.rust-lang.org/...` 返回 `HTTP/2 200` 即通。
2. **链接器**：`mingw-w64`（提供 `x86_64-w64-mingw32-gcc`，GNU ABI）。
3. **Rust target**：`rustup target add x86_64-pc-windows-gnu`（rust-std，经代理下载）。
4. **`.cargo/config.toml`** 已配 windows-gnu 用 mingw 链接器（仅交叉时生效，不影响 host）。
5. **加密后端切 ring**（见 [adr-005](adr-005-crypto-backend-ring.md)）：去掉 aws-lc-sys 的
   C/cmake/NASM 构建，交叉编译才干净。

## 构建

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app
# 产物：target/x86_64-pc-windows-gnu/release/mullion.exe（约 26M，PE32+ console）
```

## 验收（每次交叉编译后必做）

**运行时依赖只应有系统 DLL**——mingw 产物容易漏静态链接，用户机器会缺 DLL 起不来：
```bash
x86_64-w64-mingw32-objdump -p target/x86_64-pc-windows-gnu/release/mullion.exe \
  | grep -i 'DLL Name'
```
只该见 `kernel32/ntdll/user32/ws2_32/bcrypt/d3dcompiler_47/opengl32/imm32` 等系统 DLL。
**若出现 `libgcc_s_seh-1.dll` 或 `libwinpthread-1.dll` → 没静态链接，必须修**
（当前配置下是干净的）。

算出 sha256 交给用户核对传输完整性：`sha256sum <exe>`。

## 交给用户实测（Windows PowerShell）

```powershell
scp <你>@<本机IP>:/data/Mullion/target/x86_64-pc-windows-gnu/release/mullion.exe .
Get-FileHash .\mullion.exe -Algorithm SHA256   # 与上面 sha256 一致
.\mullion.exe user@192.0.2.10 -p 22 -i C:\keys\key.pem
```
- 用 PowerShell 跑（不是双击）：能看到 `eprintln!` 的连接诊断。
- 私钥要在 Windows 本地，`-i` 传 Windows 路径。
- 字体 `Google Sans Code` 需在 Windows 已安装，否则回退默认字体（不崩，但对齐可能差）。

## 发布 Release（正式分发，推荐 CI）

`.github/workflows/release.yml`：push `v*` tag 时，CI 在 ubuntu runner 上按同一条
mingw + ring 路线交叉编译、objdump 验收，再用内置 `GITHUB_TOKEN` 发布 Release，
附 `mullion.exe` + `mullion.exe.sha256`。**发版只需**：

```bash
git tag v0.1.1 && git push origin v0.1.1
```

产物由 CI 从源码构建、可复现，不依赖某台开发机；零外部凭证（用 `GITHUB_TOKEN`）。

手动兜底（CI 不可用时，如 GitHub Actions 额度/账单问题）——本机交叉编译后用 gh 发布：

```bash
sha256sum <exe> > mullion.exe.sha256
gh release create v0.1.1 <exe> mullion.exe.sha256 -t v0.1.1 -F notes.md
```

## 真机 SSH 验证（在本机就能做，验加密后端/协商）

真机信息经环境变量传入（脱敏后不写死在库里）：

```bash
MULLION_LIVE=1 \
  MULLION_LIVE_HOST=<真机 IP/域名> MULLION_LIVE_USER=<用户> MULLION_LIVE_KEY=<本地私钥> \
  cargo test -p mullion-ssh --test live -- --ignored --nocapture
```
- 未设这些 env 时用占位（`example.com`）连不通，属预期。
- `pubkey_live` 应过（证明与真实 OpenSSH 的 KEX/cipher/hostkey 协商 OK）。
- `agent_live` 在无 ssh-agent 环境**会失败**（`SSH_AUTH_SOCK` 未设），非 bug。

## 边界

- **agent 认证仅 Unix**：`AgentClient::connect_env()` 是 Unix 专属；Windows 上 agent
  路径返回可操作错误，用 `-i`（见 [gui-render-gotchas](gui-render-gotchas.md)）。
- **GPU/窗口无法在这里验证**：能编、能产 exe、能验协商，但「是否真的不闪、字形/CJK
  对齐、输入法」只有人在 Windows 上眼看（见 CLAUDE.md「你无法验证的东西」）。
- **cargo-xwin（MSVC ABI）路线未采用**：aws-lc-sys 的 C 构建 + 下 ~1GB MS SDK，
  投入产出比差；切 ring + mingw 后没必要。要分发 MSVC ABI 产物时再考虑（+ 微软 SDK 授权）。
