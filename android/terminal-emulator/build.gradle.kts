plugins {
    alias(libs.plugins.android.library)
}

android {
    namespace = "com.termux.terminal"
    compileSdk = 35
    buildToolsVersion = "35.0.0"
    ndkVersion = providers.environmentVariable("ANDROID_NDK_VERSION").getOrElse("27.3.13750724")

    defaultConfig {
        minSdk = 29

        externalNativeBuild {
            ndkBuild {
                cFlags += listOf("-std=c11", "-Wall", "-Wextra", "-Werror", "-Os", "-fno-stack-protector", "-Wl,--gc-sections")
                arguments += "APP_SUPPORT_FLEXIBLE_PAGE_SIZES=true"
            }
        }

        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    externalNativeBuild {
        ndkBuild {
            path = file("src/main/jni/Android.mk")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    testOptions {
        unitTests.isReturnDefaultValues = true
    }

    lint {
        abortOnError = true
        textReport = true
    }
}

dependencies {
    testImplementation(libs.junit)
}
