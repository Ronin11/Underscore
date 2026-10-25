import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "io.github.ronin11.underscore.wear"
    compileSdk = 35

    defaultConfig {
        // The same id and the same key as the phone app: the Wearable Data
        // Layer only carries messages between apps that match on both.
        applicationId = "io.github.ronin11.underscore"
        // Wear OS 3, the first with Health Services.
        minSdk = 30
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
    }

    signingConfigs {
        // The phone app's try-it key; see app/build.gradle.kts.
        create("tryIt") {
            storeFile = file("../app/try-it.keystore")
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
    // The watch's own sleep detection.
    implementation("androidx.health:health-services-client:1.1.0")
    // Its calls return ListenableFuture; the listenablefuture artifact resolves to an
    // empty placeholder, so take it from Guava itself.
    implementation("com.google.guava:guava:33.3.1-android")
    // Messages to and from the phone app.
    implementation("com.google.android.gms:play-services-wearable:19.0.0")
}
