# F347 Android 凭据门控 · 设计

> 状态:定案(2026-11-13)。总设计见 `2026-11-12-android-client-design.md`,决策依据 ADR-013。
> 前序切片:F346-a(`VaultKey` 包裹密钥)、F346-b(云端拉包)。

## 目标

手机端解开本地 vault(导入的会话/凭据,云端设置)之前,必须过一次系统生物识别或锁屏凭据:
**冷启动**时要验;App 在**后台累计 ≥5 分钟**后回到前台,也要再验。

## 已定决策(brainstorming 问答)

| # | 决策 | 否掉的备选 |
|---|---|---|
| Q1 | Keystore key 用 **per-use**(`setUserAuthenticationParameters(0, …)`),5 分钟计时由 App 自己算 | 按时长放行(300 s):任何一次解锁屏幕都会顺带放行这把 key,「冷启动必验」无从保证 |
| Q2 | 设备**没有安全锁屏**就拒绝使用,整页提示去系统设置 | 退化为无门控:等于 F347 不存在 |
| Q3 | `setInvalidatedByBiometricEnrollment(false)`:新增指纹不让 key 失效 | 默认 true:录个指纹本地数据全没,代价过大;攻击者能录指纹说明已经拿到锁屏凭据 |
| Q4 | 锁**整个界面**;空闲按「在后台的时长」算 | 只锁敏感操作:导入后的会话列表本身就泄露信息;按无操作时长算要钩全部输入,收益小 |
| Q5 | v1 包裹密钥**不迁移**,视为失效,用户重新导入 | 迁移:一期还没有正式用户,迁移代码是一次性负债 |

## 一、组成部分

### 1. `VaultKey`(改)

- 新 alias `mullion-vault-wrap-v2`,AES-256-GCM,参数:
  - `setUserAuthenticationRequired(true)`
  - `setUserAuthenticationParameters(0, KeyProperties.AUTH_BIOMETRIC_STRONG or KeyProperties.AUTH_DEVICE_CREDENTIAL)`(API 30 起可用,minSdk 30)
  - `setInvalidatedByBiometricEnrollment(false)`
- 解包拆两步,中间夹系统验证:
  - `prepareUnwrap(ctx): Cipher?` —— 读 `vault-key.bin`,**先校验长度与版本字节**,再用文件里的 IV 初始化 decrypt Cipher。文件损坏/版本不对在这一步就清掉重建,不会验证两次。返回 `null` 表示「没有可解的 key,走首次生成」。
  - `finishUnwrap(cipher): ByteArray` —— 验证通过后 `doFinal`,得到 32 字节数据密钥。
- 首次使用(或重建)同样要验:生成随机数据密钥后,wrap 用的 encrypt Cipher 也要经过验证。
- v1(`mullion-vault-wrap-v1` / 版本字节 1)视为失效:删 `vault-key.bin`、清 `vault/` 与 `cloud/`,并**显式 `deleteEntry` v1 alias**,不在 Keystore 留孤儿。
- 现有永久失效判据(`isPermanentLoss`:UnrecoverableKey / KeyPermanentlyInvalidated / AEADBadTag)沿用。

### 2. `Gate`(新,纯 Kotlin,零 Android 依赖)

- 状态:`Locked`、`Unlocking`、`Unlocked`。
- 输入(时间由调用方注入,一律取 `SystemClock.elapsedRealtime()`,不受改系统时间影响):
  - `onBackground(now)`、`onForeground(now)`
  - `onAuthStarted()`、`onAuthSucceeded()`、`onAuthFailed()`
- 输出:
  - `isUnlocked`
  - **弹框沿信号** `consumePromptRequest(): Boolean` —— 只在**进入 Locked 的那一刻**置位一次,读后清除。系统锁屏密码页本身是独立 Activity,会让我们走一遍 onStop/onStart;这种前后台切换不会再次置位,因此不会无限弹框。
- 阈值常量 `IDLE_LOCK_MS = 5 * 60 * 1000`;`>=` 即锁。

### 3. `Authenticator`(新,单方法接口)—— 测试缝

```kotlin
fun interface Authenticator {
    suspend fun authenticate(cipher: Cipher): Cipher?   // null = 取消/失败
}
```

- 唯一实现 `SystemPromptAuthenticator`:系统自带 `android.hardware.biometrics.BiometricPrompt` + `CryptoObject(cipher)`,`setAllowedAuthenticators(BIOMETRIC_STRONG or DEVICE_CREDENTIAL)`。允许设备凭据时**不得**设 negative button(系统会抛异常)。不引入 `androidx.biometric`。
- **release 与 debug 同一实现,不留后门**。插桩测试走真实 per-use key + 真实系统验证框,由 UiAutomator 在系统 PIN 页输入。

### 4. 持有位置:`VaultSession`(Application 级单例)

- 持有 `Gate` 与已解开的数据密钥。屏幕旋转/Activity 重建不需要重验。
- 进程被杀 = 冷启动,不做任何持久化。
- 锁定时数据密钥 `ByteArray.fill(0)` 后丢弃。
- 已知债(登记不修):Rust 侧 `InMemoryKey` 不零化。

### 5. `MainActivity`

- 锁定时整页遮盖层 + 「解锁」按钮,遮住一切内容。
- `onStop`/`onStart` 通知 Gate;收到沿信号即自动弹框。
- Activity 重建会取消系统验证框,此时**不自动重弹**,用户点「解锁」。
- `FLAG_SECURE` 只在 `!BuildConfig.DEBUG` 时设(debug 包要能截图做真机验收)。
- manifest 加 `android.permission.USE_BIOMETRIC`。

### 6. 用钥匙的地方

- 导入(`.mullionpack` 手动导入、云端拉包)一律从 `VaultSession` 取数据密钥。
- 系统文件选择器等**异步回调里**,用钥匙前再问一次 `Gate.isUnlocked`;已锁则丢弃本次结果并提示。
- 在途任务:锁定前已开始的导入/拉取持有钥匙副本,允许跑完。这是可接受行为,不做中途取消。

## 二、流程

- **冷启动**:Locked → 沿信号 → 自动弹框 → 验证通过 → `finishUnwrap` 得数据密钥 → Unlocked。
- **回前台**:后台时长 < 5 分钟保持 Unlocked;≥ 5 分钟 → 清钥匙 → Locked → 自动弹框一次。
- **无安全锁屏**(`KeyguardManager.isDeviceSecure == false`):整页提示「请先在系统设置里设置锁屏密码」,不生成 key,其他功能都不可用。
- **首次使用**:无 key 文件 → 生成 v2 key → 验证 → wrap 随机数据密钥落盘 → Unlocked。

## 三、错误处理

| 情形 | 行为 |
|---|---|
| 用户取消/按返回 | 保持 Locked,遮盖层上留「解锁」按钮,不退出 App |
| 连续失败被系统锁定(lockout) | 显示系统给的错误文案,按钮可重试 |
| `KeyPermanentlyInvalidatedException`(用户删了锁屏等) | 走永久失效路径:清空 `vault/` 与 `cloud/`,提示「锁屏凭据已变更,本地数据已清除,请重新导入」 |
| key 文件损坏/版本不对 | `prepareUnwrap` 阶段清掉重建,只验证一次 |
| 验证通过但 `doFinal` 抛 AEADBadTag | 视为永久失效,同上 |
| 无安全锁屏 | 整页提示,见流程 |

## 四、测试

### JVM 单测(新建 `android/app/src/test`)

`testImplementation(libs.junit)` 已在 `app/build.gradle.kts`,只缺目录与运行入口。`GateTest`:

- 冷启动即 Locked,且沿信号置位一次
- 后台 4:59 回前台不锁;5:00 锁
- 沿信号读一次后清除;锁定期间再次 stop/start(系统密码页)不重复置位
- 验证失败保持 Locked;成功后 Unlocked

`android/scripts/e2e.sh` 在 `connectedDebugAndroidTest` 之前先跑 `testDebugUnitTest`。

### 插桩测试

- `android/scripts/emulator.sh start` 一次性、幂等地 `adb shell locksettings set-pin <固定测试 PIN>`(只作用于模拟器;测试 PIN 不是秘密)。
- 新增 **androidTest 专用**依赖 `androidx.test.uiautomator`,release 不带。
- 公共辅助 `unlockWithPin()`:等系统 PIN 页出现 → 输入 PIN → 回车 → 等遮盖层消失。现有测试类中,凡启动 `MainActivity` 或要取数据密钥的,在开头调用它(纯 FFI 往返、不碰 vault 的不需要)。
- 新增 `CredentialGateTest`:
  - 冷启动出现遮盖层,输入 PIN 后消失、表单可见
  - 预置 v1 key 文件与 v1 alias → 启动后二者都被清除
  - 取消验证框后「解锁」按钮仍在,点它能再次弹框
- `VaultKeyTest` 改写到 v2 两步接口。

### 未验证,需人工确认(列入 P50 验收清单)

- 鸿蒙 4.2(华为 P50)上系统 `BiometricPrompt` + `DEVICE_CREDENTIAL` 的实际行为
- 指纹路径(模拟器只走 PIN)
- release 包 `FLAG_SECURE`:最近任务缩略图与截图应为黑
- 后台 5 分钟后回前台的真实体验

## 五、同步修改

- `docs/superpowers/specs/2026-11-12-android-client-design.md` F347 行:改为 per-use key + App 侧 5 分钟计时。
- `spec.md` F347 行验收列:补 per-use key 与插桩自动输 PIN。

## 六、第二轮复核修订(2026-11-16,优先于上文)

- **第 0 步 P50 spike**:动手前在 P50 上验证 v2 参数 key + `BiometricPrompt`(PIN)→ `doFinal` 成功。失败则退路:`setUserAuthenticationParameters(10, …)` 短时效 key,Gate 仍强制弹框。
- **测试设备安全态集中在 `e2e.sh`**(`emulator.sh` 只管起停):`locksettings verify --old <PIN>` 不通才 `set-pin`;`svc power stayon true`;keyguard 在则唤醒 + 输 PIN。否则第二次冷启动停在锁屏、CE 存储未解锁,全套红。
- **`VaultSession.lockAndForget()`**:只能降权的测试缝(回 Locked、清钥)。凡删 key 文件的 `@Before` 先调它;`unlockWithPin()` 已解锁时直接返回。
- **debug 判定**用 `applicationInfo.flags and FLAG_DEBUGGABLE`(工程未开 `buildConfig`)。
- **钥匙交出去一律 `copyOf()`**:锁定时原地清零不能波及在途任务;配测试。
- **沿信号**:`Unlocking → Locked`(取消/失败)**不**置位;`Unlocking` 下「解锁」按钮可点(先 cancel 旧 `CancellationSignal`);验证协程挂 Application 级 scope,`finally` 归位。自动弹框在 `onResume` 触发。
- **API 30+ 设备凭据页由 SystemUI 渲染**,宿主 Activity 一般不 onStop;`GateTest` 改守「失败/取消不重弹」。UiAutomator 按 SystemUI 控件找输入框。
- **解锁时的重置**(v1 失效 / 凭据失效 / AEADBadTag)进共享 `VaultLock`,完成后 `CloudViewModel.refresh()`。v1 清理文案单列:「数据格式已升级,请重新导入」。AEADBadTag 路径如实为**两次**验证。
- **S1**:`PackImport.run`、`CloudPull.save/pull` 改为调用方传入 `dataKey`;语义测试注入随机 32 字节,不碰 Keystore。`Loaded.reset` / `FailedAfterReset` / `RESET_NOTE` 链路随之删改。只有 `VaultKeyTest`、`CredentialGateTest` 与 Compose UI 测试需要真 PIN。
- **`testDebugUnitTest` 与 `connectedDebugAndroidTest` 同一次 gradlew 调用**。
- 同步修改补 `docs/android-build.md:40`(v1 alias)。

## 不做

- 自定义 PIN / App 独立密码
- 敏感操作级别的二次验证
- v1 → v2 迁移
- 中途取消在途导入任务
