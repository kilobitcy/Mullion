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

签名（`scripts/sign-windows.sh`，见下文「首次配置签名证书」），**再**算 sha256 交给
用户核对传输完整性：`sha256sum <exe>`。顺序不能反——签名会改文件内容。

## 交给用户实测（Windows PowerShell）

**用 `gh` 下载，别用浏览器**——这一条就消掉了每版都要 `Unblock-File` 的麻烦，理由见下节：

```powershell
gh release download v0.1.N -p mullion.exe --repo kilobitcy/Mullion
Get-FileHash .\mullion.exe -Algorithm SHA256   # 与 Release notes 里的 sha256 一致
.\mullion.exe user@192.0.2.10 -p 22 -i C:\keys\key.pem
```
- 用 PowerShell 跑（不是双击）：能看到 `eprintln!` 的连接诊断。
- 私钥要在 Windows 本地，`-i` 传 Windows 路径。
- 字体 `Google Sans Code` 需在 Windows 已安装，否则回退默认字体（不崩，但对齐可能差）。

### 首次运行被拦：先分清是哪个机制

历史上这一节把三件事混成一件，结论也跟着错。它们的触发条件和解法都不一样：

| 机制 | 谁触发的 | 签名能否解决 |
|---|---|---|
| **MotW**（Mark-of-the-Web，`Zone.Identifier` 备用数据流） | **下载器**打的标记，浏览器打、`gh`/`curl.exe`/`Invoke-WebRequest` 不打 | ❌ **完全不能**。签了名照样有 MotW |
| **SmartScreen 应用信誉** | 云端查这个文件 hash / 这张证书有没有口碑 | ⚠️ 部分，且要靠积累 |
| **Smart App Control**（Win11） | 开启时**直接阻止**未签名 exe 运行 | ✅ 能。当前没开（否则 `Unblock-File` 也救不了） |

**「每个新版本都被拦」的实际根因是第一个。** 所以对策也在第一个上：

**① 换下载方式（首选，零成本，当场生效）**

MotW 不是 Windows 强加的，是下载器调 `IAttachmentExecute` 打上去的。`gh` 不调：

```powershell
gh release download v0.1.N -p mullion.exe --repo kilobitcy/Mullion
```

没有 MotW → 不走 SmartScreen 检查 → 不用 `Unblock-File`。`curl.exe`、
`Invoke-WebRequest` 同理。已经用浏览器下过的，`Unblock-File .\mullion.exe`
（或右键属性勾「解除锁定」）补救一次。

**② 自签名证书 + 本机信任（一次性配置，见下节）**

消掉 UAC / 属性页里的「发布者：未知」，显示 `Mullion`。**注意边界**：SmartScreen 走的是
云端信誉，不认你本机的信任链——② 是 ① 的补充，不是替代。真正的收益是可验证来源
（指纹对得上就是这台开发机编的）和将来开 Smart App Control / AppLocker 的基础。

**不做的方案，以及为什么**：

| 方案 | 结论 | 理由 |
|---|---|---|
| EV 代码签名证书 | ❌ 别买 | **「即时信誉」特权已作废**：微软 2024-08 起把 EV Code Signing OID 从受信任根程序移除，OV/EV 一视同仁 |
| OV/标准代码签名证书 | ❌ 不值 | ~$100–200/年，仍要攒 hash 信誉；2023-06 起私钥强制存 HSM/USB token，自动签名很麻烦 |
| Azure Artifact Signing | ⏸ 要对外分发时再评估 | $9.99/月起（原 Trusted Signing，2026-01 GA）。个人身份目前只覆盖美/加；且**新构建的 hash 信誉照样要重新积累** |
| Microsoft Store | ⏸ 同上 | 完全免疫，但要过审、要改打包方式 |

一个佐证：2026-03 微软迁移中间 CA（`Microsoft ID Verified CS EOC CA 03`）时，一大批
**本来已受信任**的已签名应用又开始弹 SmartScreen。付费签名也不是一劳永逸。

### 首次配置签名证书（换开发机才需重做）

私钥留在 `~/.mullion-signing/`，**永不进仓库、永不推送**。

```bash
sudo -E apt-get -o Acquire::http::Proxy=http://127.0.0.1:7890 install -y osslsigncode

D=~/.mullion-signing && mkdir -p "$D" && chmod 700 "$D" && cd "$D"
cat > openssl.cnf <<'EOF'
[req]
distinguished_name = dn
x509_extensions    = v3
prompt             = no
[dn]
CN = Mullion
O  = Mullion
C  = CN
[v3]
basicConstraints     = critical,CA:FALSE
keyUsage             = critical,digitalSignature
extendedKeyUsage     = critical,codeSigning
subjectKeyIdentifier = hash
EOF
openssl req -x509 -newkey rsa:3072 -keyout key.pem -out cert.pem \
  -days 3650 -nodes -sha256 -config openssl.cnf
chmod 600 key.pem
openssl x509 -in cert.pem -outform DER -out mullion-codesign.cer   # 给 Windows 导入用
openssl x509 -in cert.pem -noout -fingerprint -sha1                # 记下指纹
```

有效期给 10 年，省得每年重做。`extendedKeyUsage = codeSigning` 不能省——缺了它
Windows 不认这是代码签名证书。

**签名**（发版流程第 3.5 步，产物原地替换）：

```bash
scripts/sign-windows.sh   # 默认签 target/x86_64-pc-windows-gnu/release/mullion.exe
```

脚本幂等（重复跑先摘旧签名，不会叠成多重签名），带 DigiCert 的 RFC3161 时间戳
（走代理；拿不到就降级为无时间戳，不致命）。

**Windows 端一次性导入**（管理员 PowerShell，`mullion-codesign.cer` 随 Release 附带）：

```powershell
Import-Certificate -FilePath .\mullion-codesign.cer -CertStoreLocation Cert:\LocalMachine\Root
Import-Certificate -FilePath .\mullion-codesign.cer -CertStoreLocation Cert:\LocalMachine\TrustedPublisher
Get-AuthenticodeSignature .\mullion.exe    # 应为 Valid，SignerCertificate 指纹对得上
```

导入前**必须核对指纹**与上面 `openssl x509 -fingerprint` 输出一致——往
`LocalMachine\Root` 里塞证书等于让本机无条件信任它签的一切，装错了是真实风险。
以后每版都不用再导，除非换开发机重新生成了证书（指纹会变）。

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
scripts/sign-windows.sh                      # 先签名
sha256sum <exe> > mullion.exe.sha256         # 再算 hash
gh release create v0.1.1 <exe> mullion.exe.sha256 \
  ~/.mullion-signing/mullion-codesign.cer -t v0.1.1 -F notes.md
```

CI 那条路线目前发不出去（Actions 账单锁），且 **CI 上没有签名私钥**——真要走 CI
得先决定私钥怎么托管（GitHub Secrets 或换 Azure Artifact Signing），现在不做。

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
