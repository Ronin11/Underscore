import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

// Stamped into BuildConfig so a bug report says which build it came from.
// CI sets both; a local build gets the short hash and "local".
val commit: String = System.getenv("GITHUB_SHA")?.take(7) ?: runCatching {
    val git = ProcessBuilder("git", "rev-parse", "--short", "HEAD")
        .directory(rootDir).redirectErrorStream(true).start()
    git.inputStream.bufferedReader().readText().trim().takeIf { git.waitFor() == 0 }
}.getOrNull().orEmpty().ifEmpty { "unknown" }
val buildNumber: String = System.getenv("GITHUB_RUN_NUMBER")?.let { "apk-$it" } ?: "local"

android {
    namespace = "io.github.ronin11.underscore"
    compileSdk = 35

    defaultConfig {
        applicationId = "io.github.ronin11.underscore"
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
        ndk {
            // The Rust library is built for these by cargo-ndk in CI.
            abiFilters += listOf("arm64-v8a")
        }
        buildConfigField("String", "COMMIT", "\"$commit\"")
        buildConfigField("String", "BUILD", "\"$buildNumber\"")
    }

    buildFeatures {
        buildConfig = true
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            // Signed with the debug key until a release key exists; this is
            // a try-it build, not a store build.
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_17)
    }
}

dependencies {
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("androidx.activity:activity-ktx:1.9.3")
    // MediaSessionCompat and the media-style notification.
    implementation("androidx.media:media:1.7.0")
}
