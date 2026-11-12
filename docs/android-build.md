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
- 端到端:`android/scripts/e2e.sh` 先跑两条孤立 clippy(`mullion-cloud --no-default-features`、`mullion-ffi`:
  `--workspace` 下 app 会把 cloud 的 `write` 统一成开着,关掉写路径的配置只在孤立构建里编到),
  再起宿主机 `echo_sshd`(127.0.0.1:2222,模拟器里是 10.0.2.2:2222)与 `fake_s3_pack`
  (127.0.0.1:9000,回放 `mullion/000001-20261112T000000Z.mpk` 演示包,口令同演示包;F346-b),
  然后跑 `connectedDebugAndroidTest`。云测试的地址可用 `-Pandroid.testInstrumentationRunnerArguments.s3=http://...` 覆盖。要连别的地址,传 `-Pandroid.testInstrumentationRunnerArguments.host=... .port=...`;
  **真机地址和凭据不写进任何被跟踪文件**
- Gradle 控制台不打逐条用例结果。要看哪条红/一共跑了几条:先 `installDebug installDebugAndroidTest`,
  再 `adb shell am instrument -w -r io.github.kilobitcy.mullion.test/androidx.test.runner.AndroidJUnitRunner`
  (`echo_sshd 2222` 与 `fake_s3_pack 9000` 两个 example 都要自己先起,用完 `pkill -x` 收掉),末尾是 `OK (N tests)` 或 `FAILURES!!!`
- 导入测试资产:`android/app/src/androidTest/assets/test.mullionpack` 由
  `cargo run -p mullion-ffi --example make_test_pack -- android/app/src/androidTest/assets/test.mullionpack`
  生成(假数据,内容见 `crates/mullion-ffi/tests/common/demo_pack.rs`);演示包内容变了才重生成

## 已知坑
- keyring 在 Android 上会静默回落到内存 mock 库 → store 在 Android 上不编 keyring,`KeyringSource` 不存在(守护:`mullion-store/tests/android_has_no_keyring.rs`)
- AGP 9 必须写 `android.builtInKotlin=false` / `android.newDsl=false`,否则和显式的 Kotlin 插件冲突
- Compose BOM 2026.09 要求 compileSdk ≥ 37:写 `compileSdkVersion("android-37.2")`;targetSdk 仍是 36
- uniffi 生成的 Kotlin 类自带 `AutoCloseable.close()`:Rust 侧导出方法**不能叫 `close`**(会生成两个同签名方法),所以叫 `disconnect`
- Compose 测试里 `SemanticsConfiguration.getOrNull` 是扩展函数,要显式 `import androidx.compose.ui.semantics.getOrNull`
- 主机密钥目前是进程内 TOFU(首见即收、不落盘),F350 换成真 TOFU;F355 之前不发 APK
- 本地数据:`filesDir/vault/`(与桌面同格式的 `sessions.toml` / `known_hosts.toml` / `secrets.enc`);
  数据密钥由 Keystore 包装钥 `mullion-vault-wrap-v1` 包在 `filesDir/vault-key.bin`(首字节版本号)。
  解不开即清空 `vault/` 重新生成 —— 重导即恢复(F346-a)。
  云配置在 `filesDir/cloud/cloud.toml`(F346-b,SK 与备份口令由同一把数据密钥封存),Keystore 重置时一并清空
- 备份与设备迁移都关了(`allowBackup=false` + `data_extraction_rules.xml`):Keystore 钥带不走
