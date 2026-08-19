#!/usr/bin/env bash
# 给交叉编译出的 mullion.exe 加 Authenticode 签名（自签名证书，见
# docs/cross-compile-windows.md「代码签名」一节）。
#
# 用法：scripts/sign-windows.sh [exe路径]
#   默认 target/x86_64-pc-windows-gnu/release/mullion.exe，**原地**替换。
#
# 私钥在 ~/.mullion-signing/（chmod 600），**永不进仓库**。换开发机要重新生成，
# 且必须把新的 .cer 重新导进 Windows——指纹变了，旧信任不认新证书。
set -euo pipefail

CERT_DIR="${MULLION_SIGN_DIR:-$HOME/.mullion-signing}"
EXE="${1:-target/x86_64-pc-windows-gnu/release/mullion.exe}"

if [[ ! -f "$CERT_DIR/key.pem" || ! -f "$CERT_DIR/cert.pem" ]]; then
  echo "找不到签名证书：$CERT_DIR/{key,cert}.pem" >&2
  echo "生成步骤见 docs/cross-compile-windows.md「首次配置签名证书」。" >&2
  exit 1
fi
[[ -f "$EXE" ]] || { echo "找不到 exe：$EXE" >&2; exit 1; }

# 已签过就先摘掉旧签名——osslsigncode 不覆盖，重复签会叠成多重签名。
if osslsigncode verify "$EXE" >/dev/null 2>&1; then
  echo "· 检测到已有签名，先移除"
  osslsigncode remove-signature -in "$EXE" -out "$EXE.unsigned"
  mv "$EXE.unsigned" "$EXE"
fi

# 时间戳让签名在证书过期后仍然有效。要走代理（本机 DNS 解析不了外网）。
# 拿不到时间戳不算致命：自签名证书 10 年有效期内照样验得过，降级继续。
TS_ARGS=(-ts http://timestamp.digicert.com)
if ! http_proxy="${http_proxy:-http://127.0.0.1:7890}" \
     https_proxy="${https_proxy:-http://127.0.0.1:7890}" \
     timeout 60 osslsigncode sign \
       -certs "$CERT_DIR/cert.pem" -key "$CERT_DIR/key.pem" \
       -n "Mullion SSH Client" -i "https://github.com/kilobitcy/Mullion" \
       -h sha256 "${TS_ARGS[@]}" -in "$EXE" -out "$EXE.signed" 2>&1 | tail -3
then
  echo "· 时间戳服务不可达，退回无时间戳签名" >&2
  osslsigncode sign \
    -certs "$CERT_DIR/cert.pem" -key "$CERT_DIR/key.pem" \
    -n "Mullion SSH Client" -i "https://github.com/kilobitcy/Mullion" \
    -h sha256 -in "$EXE" -out "$EXE.signed" | tail -2
fi
mv "$EXE.signed" "$EXE"

echo "=== 验签"
# 签名链用我们自己的证书验；时间戳链是 DigiCert 的公共 CA，得用系统 CA bundle
# （拿 cert.pem 验时间戳必然报 failed——验错了链，不是签名坏了）。
osslsigncode verify -CAfile "$CERT_DIR/cert.pem" \
  -TSA-CAfile /etc/ssl/certs/ca-certificates.crt "$EXE" \
  | grep -E "Signature verification|Number of verified|Timestamp"
echo "=== 证书指纹（应与 Windows 端已导入的一致）"
openssl x509 -in "$CERT_DIR/cert.pem" -noout -fingerprint -sha1
