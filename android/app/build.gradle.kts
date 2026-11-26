plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.bombatomica64.segnaledebole"
    compileSdk = 35

    defaultConfig {
        applicationId = "dev.bombatomica64.segnaledebole"
        minSdk = 26
        targetSdk = 35
        versionCode = System.getenv("ANDROID_VERSION_CODE")?.toIntOrNull() ?: 3
        versionName = System.getenv("ANDROID_VERSION_NAME")?.takeIf { it.isNotBlank() } ?: "1.0.0"
        ndk {
            // `libsegnale_debole.so` is built for arm64 only (cargo ndk -t arm64-v8a).
            abiFilters += "arm64-v8a"
        }
    }

    val releaseKeystorePath = System.getenv("ANDROID_KEYSTORE_PATH")?.takeIf { it.isNotBlank() }
    val releaseKeystorePassword = System.getenv("ANDROID_KEYSTORE_PASSWORD")
    val releaseKeyAlias = System.getenv("ANDROID_KEY_ALIAS")
    val releaseKeyPassword = System.getenv("ANDROID_KEY_PASSWORD")
    val hasReleaseSigning = releaseKeystorePath != null &&
        !releaseKeystorePassword.isNullOrEmpty() &&
        !releaseKeyAlias.isNullOrEmpty() &&
        !releaseKeyPassword.isNullOrEmpty()

    signingConfigs {
        if (hasReleaseSigning) {
            create("release") {
                storeFile = file(releaseKeystorePath!!)
                storePassword = releaseKeystorePassword
                keyAlias = releaseKeyAlias
                keyPassword = releaseKeyPassword
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            // Use the configured release keystore when provided (required for
            // in-place updates across releases); fall back to debug signing so
            // local/CI preview builds stay directly sideloadable.
            signingConfig = if (hasReleaseSigning) {
                signingConfigs.getByName("release")
            } else {
                signingConfigs.getByName("debug")
            }
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    packaging {
        jniLibs {
            // Keep the Rust library uncompressed and page-aligned (faster load).
            useLegacyPackaging = false
        }
    }
}

dependencies {
    // Must match the GameActivity C glue compiled into android-activity 0.6.1
    // (GAMEACTIVITY 4.4.0), which Bevy 0.20.1 uses.
    implementation("androidx.games:games-activity:4.4.0")
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.appcompat:appcompat:1.7.0")
}
