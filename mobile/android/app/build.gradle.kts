import java.util.Properties

plugins {
    id("com.android.application")
    // The Flutter Gradle Plugin must be applied after the Android and Kotlin Gradle plugins.
    id("dev.flutter.flutter-gradle-plugin")
}

// 发布签名配置放在 android/key.properties（已 gitignore），CI 上由流水线写入。
// 文件不存在时退回 debug 签名，这样 `flutter run --release` 在开发机上照常可用。
val keystoreProperties =
    Properties().apply {
        val keystoreFile = rootProject.file("key.properties")
        if (keystoreFile.exists()) {
            keystoreFile.inputStream().use { load(it) }
        }
    }
val hasReleaseKeystore = keystoreProperties.getProperty("storeFile") != null

// 只保留真正编了 Rust 库的 ABI。
//
// 三方插件（mobile_scanner 等）带的是全 ABI 预编译 AAR，合并后 APK 会声称支持
// armeabi-v7a / x86_64；而 cargokit 只按 -Ptarget-platform 编了指定 ABI 的
// libsmelt_mobile.so。两者不一致的后果是：APK 能装进 armv7 设备，但一启动
// dlopen 就找不到 libsmelt_mobile.so 直接崩，且崩在 Dart 之前，没有任何可读的
// 错误。宁可让这类设备在应用商店/安装阶段就被判定为不兼容。
val flutterTargetPlatforms: List<String> =
    (project.findProperty("target-platform") as String?)
        ?.split(",")
        ?.filter { it.isNotBlank() }
        ?: emptyList()
val abiFiltersFromTargetPlatform: List<String> =
    flutterTargetPlatforms.mapNotNull {
        when (it) {
            "android-arm" -> "armeabi-v7a"
            "android-arm64" -> "arm64-v8a"
            "android-x86" -> "x86"
            "android-x64" -> "x86_64"
            else -> null
        }
    }

android {
    namespace = "ai.smelt.smelt_mobile"
    compileSdk = flutter.compileSdkVersion
    ndkVersion = flutter.ndkVersion

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    defaultConfig {
        applicationId = "ai.smelt.mobile"
        minSdk = flutter.minSdkVersion
        targetSdk = flutter.targetSdkVersion
        versionCode = flutter.versionCode
        versionName = flutter.versionName

        // 没传 target-platform 时不设过滤，让 Flutter 自己的默认行为生效。
        if (abiFiltersFromTargetPlatform.isNotEmpty()) {
            ndk {
                abiFilters.clear()
                abiFilters.addAll(abiFiltersFromTargetPlatform)
            }
        }
    }

    signingConfigs {
        if (hasReleaseKeystore) {
            create("release") {
                keyAlias = keystoreProperties.getProperty("keyAlias")
                keyPassword = keystoreProperties.getProperty("keyPassword")
                storeFile = rootProject.file(keystoreProperties.getProperty("storeFile"))
                storePassword = keystoreProperties.getProperty("storePassword")
            }
        }
    }

    buildTypes {
        release {
            signingConfig =
                if (hasReleaseKeystore) {
                    signingConfigs.getByName("release")
                } else {
                    signingConfigs.getByName("debug")
                }
            // Rust 侧（smelt-mobile）通过 FFI 反射不到的符号进来，代码收缩容易误删；
            // 现在包体不是瓶颈，先关掉，避免出难查的 release-only 崩溃。
            isMinifyEnabled = false
            isShrinkResources = false
        }
    }
}

kotlin {
    compilerOptions {
        jvmTarget = org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17
    }
}

flutter {
    source = "../.."
}
