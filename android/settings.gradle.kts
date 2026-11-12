// Google 仓库先走阿里云镜像:maven.google.com 的构件会 301 到 dl.google.com,
// 而本机代理对 dl.google.com 的 TLS 会被断(docs/android-build.md)。
// google() 留在后面兜底,别的机器网络正常时照样能解析。
pluginManagement {
    repositories {
        maven("https://maven.aliyun.com/repository/google")
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        maven("https://maven.aliyun.com/repository/google")
        google()
        mavenCentral()
    }
}

rootProject.name = "Mullion"
include(":app")
