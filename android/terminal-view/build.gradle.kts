plugins {
    alias(libs.plugins.android.library)
}

android {
    namespace = "com.termux.view"
    compileSdk = 35
    buildToolsVersion = "35.0.0"

    defaultConfig {
        minSdk = 29
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    lint {
        abortOnError = true
        textReport = true
    }
}

dependencies {
    api(project(":terminal-emulator"))
    implementation(libs.androidx.annotation)
    testImplementation(libs.junit)
}
