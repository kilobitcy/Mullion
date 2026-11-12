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
