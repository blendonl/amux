import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
}

val cargoVersion = providers
    .fileContents(rootProject.layout.projectDirectory.file("../Cargo.toml"))
    .asText
    .map { manifest ->
        Regex("""(?m)^version\s*=\s*"([^"]+)"""").find(manifest)?.groupValues?.get(1)
            ?: error("Cargo.toml has no package version")
    }

fun versionCodeOf(version: String): Int {
    val (major, minor, patch) = version.substringBefore('-').split('.').map(String::toInt)
    return major * 1_000_000 + minor * 1_000 + patch
}

val releaseKeystore = providers.environmentVariable("AMUX_RELEASE_KEYSTORE")
val releaseKeystorePassword = providers.environmentVariable("AMUX_RELEASE_KEYSTORE_PASSWORD")
val releaseKeyAlias = providers.environmentVariable("AMUX_RELEASE_KEY_ALIAS")

android {
    namespace = "io.github.blendonl.amux"
    compileSdk = 35
    buildToolsVersion = "35.0.0"
    ndkVersion = providers.environmentVariable("ANDROID_NDK_VERSION").getOrElse("27.3.13750724")

    defaultConfig {
        applicationId = "io.github.blendonl.amux"
        minSdk = 29
        targetSdk = 35
        versionName = cargoVersion.get()
        versionCode = versionCodeOf(cargoVersion.get())

        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    signingConfigs {
        if (releaseKeystore.isPresent) {
            create("release") {
                storeFile = file(releaseKeystore.get())
                storePassword = releaseKeystorePassword.get()
                keyAlias = releaseKeyAlias.get()
                keyPassword = releaseKeystorePassword.get()
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.findByName("release")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    packaging {
        jniLibs.useLegacyPackaging = true
        jniLibs.keepDebugSymbols += "**/libu_*.so"
    }

    lint {
        abortOnError = true
        textReport = true
    }
}

kotlin {
    compilerOptions {
        jvmTarget = JvmTarget.JVM_17
    }
}

dependencies {
    implementation(libs.termux.terminal.emulator)
    implementation(libs.termux.terminal.view)
    testImplementation(libs.junit)
    testImplementation(libs.json)
}
