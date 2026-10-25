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

    signingConfigs {
        // A key checked into the repo, like Android's own debug key, so every
        // try-it APK installs over the last one whether CI or a laptop built
        // it. Not a secret and not a release key: nothing signed with it goes
        // to a store. The password is the same as the alias.
        create("tryIt") {
            storeFile = file("try-it.keystore")
            storePassword = "underscore"
            keyAlias = "try-it"
            keyPassword = "underscore"
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName("tryIt")
        }
        debug {
            signingConfig = signingConfigs.getByName("tryIt")
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
    // Google's sleep API, for the phone's own guess that the listener fell asleep.
    implementation("com.google.android.gms:play-services-location:21.3.0")
}
