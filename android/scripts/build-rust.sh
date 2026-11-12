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
  generate --library target/debug/libmullion_ffi.so --language kotlin --no-format --out-dir "$out"
