#!/usr/bin/env bash
# F345 端到端:宿主机起 echo sshd → 模拟器跑插桩测试 → 收尾。
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT"
# F346-b 设计 D4:关掉 write 的配置在 `--workspace` 下永远不被编到(app 把它统一成开着),
# dead code / unused import 只在孤立构建里冒出来。
cargo clippy -q -p mullion-cloud --no-default-features --all-targets -- -D warnings
cargo clippy -q -p mullion-ffi --all-targets -- -D warnings
cargo build -q -p mullion-ffi --example echo_sshd --example fake_s3_pack
target/debug/examples/echo_sshd 2222 > /tmp/mullion-echo-sshd.log 2>&1 &
SSHD=$!
target/debug/examples/fake_s3_pack 9000 > /tmp/mullion-fake-s3.log 2>&1 &
S3=$!
trap 'kill $SSHD $S3 2>/dev/null || true' EXIT
for _ in $(seq 50); do ss -ltn | grep -q '127.0.0.1:2222 ' && ss -ltn | grep -q '127.0.0.1:9000 ' && break; sleep 0.1; done
android/scripts/emulator.sh start
cd android && ./gradlew --no-daemon connectedDebugAndroidTest "$@"
