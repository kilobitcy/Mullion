plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.compose)
}

android {
    namespace = "io.github.kilobitcy.mullion"
    // Compose BOM 2026.09 的 foundation/animation 要求 compileSdk ≥ 37;
    // 只影响能调的 API,运行期行为由 targetSdk 决定,仍是 36。
    compileSdkVersion("android-37.2")

    defaultConfig {
        applicationId = "io.github.kilobitcy.mullion"
        minSdk = 30
        targetSdk = 36
        // 版本号独立于桌面(ADR-013 / F355):android-v0.x.y。
        versionCode = 1
        versionName = "0.0.1"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        ndk {
            // x86_64 只为模拟器测试;发布包(F355)只留 arm64-v8a。
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_21
        targetCompatibility = JavaVersion.VERSION_21
    }

    kotlin {
        jvmToolchain(21)
    }

    buildFeatures {
        compose = true
    }

    sourceSets["main"].java.srcDir("build/generated/uniffi")
}

// 每次构建前先编 Rust 并重新生成绑定,免得 .so 和绑定跟源码脱节、悄悄跑旧代码。
val buildRust by tasks.registering(Exec::class) {
    workingDir = rootProject.projectDir
    commandLine("bash", "scripts/build-rust.sh")
}
tasks.named("preBuild") { dependsOn(buildRust) }

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.activity.compose)
    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.compose.ui)
    implementation(libs.androidx.compose.material3)
    implementation(libs.kotlinx.coroutines.android)
    // uniffi 生成的 Kotlin 经 JNA 调 .so(要求 ≥5.12);aar 自带各 ABI 的 libjnidispatch。
    implementation("${libs.jna.get()}@aar")

    testImplementation(libs.junit)
    androidTestImplementation(libs.androidx.test.runner)
    androidTestImplementation(libs.androidx.test.ext.junit)
    androidTestImplementation(platform(libs.androidx.compose.bom))
    androidTestImplementation(libs.androidx.compose.ui.test.junit4)
    debugImplementation(libs.androidx.compose.ui.test.manifest)
}
