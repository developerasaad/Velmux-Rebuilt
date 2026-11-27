plugins {
    id("com.android.application")
    kotlin("android")
}

android {
    namespace = "com.velmux.mobile.testing"
    compileSdk = 35
    defaultConfig {
        applicationId = "com.velmux.mobile.fixtures"
        minSdk = 29
        targetSdk = 35
        versionCode = 1
        versionName = "1"
    }
    sourceSets.getByName("main") {
        manifest.srcFile("AndroidManifest.xml")
        java.setSrcDirs(listOf("src"))
        assets.srcDir(rootProject.layout.buildDirectory.dir("media-fixtures"))
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_21
        targetCompatibility = JavaVersion.VERSION_21
    }
}

kotlin { jvmToolchain(21) }
