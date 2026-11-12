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

## 构建与测试
- Rust 侧:`cargo test -p mullion-ffi`(含进程内 echo sshd 端到端,纳入 `cargo test --workspace`)
- APK:`cd android && ./gradlew assembleDebug`。`preBuild` 依赖 `buildRust`,也就是 `android/scripts/build-rust.sh`:
  先 `cargo ndk` 编 `arm64-v8a` + `x86_64` 进 `app/src/main/jniLibs/`(不入库),
  再用宿主机 `.so` 跑 uniffi library mode,把绑定生成到 `app/build/generated/uniffi/`
- 端到端:`android/scripts/e2e.sh` 起宿主机 `echo_sshd`(127.0.0.1:2222,模拟器里是 10.0.2.2:2222),
  再跑 `connectedDebugAndroidTest`。要连别的地址,传 `-Pandroid.testInstrumentationRunnerArguments.host=... .port=...`;
  **真机地址和凭据不写进任何被跟踪文件**
- Gradle 控制台不打逐条用例结果。要看哪条红/一共跑了几条:先 `installDebug installDebugAndroidTest`,
  再 `adb shell am instrument -w -r io.github.kilobitcy.mullion.test/androidx.test.runner.AndroidJUnitRunner`
  (echo sshd 要自己先起),末尾是 `OK (N tests)` 或 `FAILURES!!!`

## 已知坑
- keyring 在 Android 上会静默回落到内存 mock 库 → store 在 Android 上不编 keyring,`KeyringSource` 不存在(守护:`mullion-store/tests/android_has_no_keyring.rs`)
- AGP 9 必须写 `android.builtInKotlin=false` / `android.newDsl=false`,否则和显式的 Kotlin 插件冲突
- Compose BOM 2026.09 要求 compileSdk ≥ 37:写 `compileSdkVersion("android-37.2")`;targetSdk 仍是 36
- uniffi 生成的 Kotlin 类自带 `AutoCloseable.close()`:Rust 侧导出方法**不能叫 `close`**(会生成两个同签名方法),所以叫 `disconnect`
- Compose 测试里 `SemanticsConfiguration.getOrNull` 是扩展函数,要显式 `import androidx.compose.ui.semantics.getOrNull`
- 主机密钥目前是进程内 TOFU(首见即收、不落盘),F350 换成真 TOFU;F355 之前不发 APK
