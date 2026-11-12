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
