---
name: release-windows
description: Mullion 的 Windows 发版一条龙——升版本号、跑绿、交叉编译 exe、objdump 依赖验收、发 GitHub Release。当本轮改动落到 mullion-app（或任何影响 Windows 端行为的地方）、要拿去实机验收时使用；也在用户说「发版」「出个 exe」「bump 版本」「发 Release」时使用。
---

# Windows 发版一条龙

触发条件：改动落到 `mullion-app`（或任何影响 Windows 端行为的地方）且要拿去实机验。
**一条龙做完，不要停下来问「要不要 bump / 要不要发版」。**

## 1. 升 patch 版本号

`Cargo.toml` 的 `workspace.package.version`，第三位 +1。单独一个提交：

```
chore: 版本 0.1.N(一句话说清这版修了什么)
```

## 2. 跑绿

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

**不绿不发。** 只跑单个 crate 不叫绿。

## 3. 交叉编译 + objdump 验收

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app
```

按 `docs/cross-compile-windows.md` 做 objdump 依赖验收。
**出现 `libgcc_s_seh-1.dll` / `libwinpthread-1.dll` 即为不合格，必须修。**

## 3.5 签名

```bash
scripts/sign-windows.sh   # 原地替换，幂等，带 RFC3161 时间戳
```

自签名证书在 `~/.mullion-signing/`（私钥永不进仓库）。**必须在算 sha256 之前签**——
签名会改文件内容，顺序反了 hash 就对不上。首次配置见
`docs/cross-compile-windows.md`「首次配置签名证书」。

## 4. 发 GitHub Release

用户从 GitHub 下载，不要只在本地留 exe、也不要让用户手动 scp。

```bash
sha256sum mullion.exe > mullion.exe.sha256
HTTPS_PROXY=http://127.0.0.1:7890 gh release create v0.1.N \
  mullion.exe mullion.exe.sha256 ~/.mullion-signing/mullion-codesign.cer \
  -t "v0.1.N" -F notes.md --repo kilobitcy/Mullion
```

`.cer` 是公钥证书（公开无害），每版都附——用户换机器时能就地导入。

**Release 标题只能是纯版本号 `v0.1.N`** —— 不带破折号、不带一句话摘要、不带 emoji，
列表里要一眼扫清版本序列。想说的话全部写进 notes 正文。

`notes.md` 里写：

- 修了什么
- **人工验收清单** —— 无头环境验不了的那些（见 CLAUDE.md 的「你无法验证的东西」：
  是否不闪 / 字形与 CJK 对齐 / 输入法 / 手感）
- sha256
- **下载方式** —— 让用户用 `gh release download`，**别用浏览器**：浏览器打 MotW
  （`Zone.Identifier`），这才是「每版都被拦」的真根因；`gh` 不打，也就不用 `Unblock-File`。
  已用浏览器下过的补一句 `Unblock-File .\mullion.exe`。
- **签名指纹** —— exe 已自签名，附上证书 SHA1 指纹供核对。首次导入证书的步骤给一次链接
  （`docs/cross-compile-windows.md`），已导入过的版本不必重复贴。

**先 push 再发版**：`gh release create` 会把 tag 建在远端当前 HEAD 上，
本地提交没 push 就发版会让 tag 指向旧 commit。

## 5. 报给用户

Release 链接 + sha256 + 人工验收清单。

## 网络约束

**本机 DNS 解析不了 github。**

- `gh` / `curl` 必须带 `HTTPS_PROXY=http://127.0.0.1:7890`
- SSH push 走 `socks5 127.0.0.1:7891` 的 ProxyCommand
- GitHub Actions 因账单锁不可用，`release.yml` 虽然正确但发不出去，一律走上面的手动路线
