import gobley.gradle.Variant
import gobley.gradle.cargo.dsl.*
import gobley.gradle.rust.targets.RustAndroidTarget

plugins {
    id("com.android.application")
    kotlin("android")
    kotlin("plugin.compose")
    kotlin("plugin.atomicfu")
    id("dev.gobley.cargo")
    id("dev.gobley.uniffi")
}

android {
    namespace = "com.velmux.mobile"
    compileSdk = 35
    ndkVersion = "27.2.12479018"
    defaultConfig {
        applicationId = "com.velmux.mobile"
        minSdk = 29
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        ndk { abiFilters += setOf("arm64-v8a", "x86_64") }
    }
    sourceSets {
        getByName("main") {
            manifest.srcFile("AndroidManifest.xml")
            java.setSrcDirs(listOf("src"))
            res.setSrcDirs(listOf("resources"))
        }
        getByName("debug") {
            manifest.srcFile("tests/target-debug-manifest.xml")
        }
        getByName("androidTest") {
            java.setSrcDirs(listOf("tests"))
            manifest.srcFile("tests/AndroidManifest.xml")
            assets.srcDir(rootProject.layout.buildDirectory.dir("media-fixtures"))
        }
    }
    buildFeatures { compose = true }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_21
        targetCompatibility = JavaVersion.VERSION_21
    }
}

kotlin { jvmToolchain(21) }

dependencies {
    // Match the Android artifacts underlying the verified Compose 1.9.3 stack.
    implementation("androidx.compose.runtime:runtime:1.9.4")
    implementation("androidx.compose.foundation:foundation:1.9.4")
    implementation("androidx.compose.ui:ui:1.9.4")
    implementation("androidx.compose.material3:material3:1.4.0")
    implementation("androidx.activity:activity-compose:1.10.1")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.9.4")
    implementation("androidx.media3:media3-exoplayer:1.8.1")
    implementation("androidx.media3:media3-ui:1.8.1")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.10.2")
    implementation("org.jetbrains.kotlinx:atomicfu:0.29.0")
    implementation("net.java.dev.jna:jna:5.18.1@aar") {
        version { strictly("5.18.1") }
    }
    androidTestImplementation("androidx.test:runner:1.6.2")
    androidTestImplementation("androidx.test:core:1.6.1")
    androidTestImplementation("androidx.test.ext:junit:1.2.1")
    androidTestImplementation("androidx.test.uiautomator:uiautomator:2.3.0")
    androidTestImplementation("androidx.media3:media3-exoplayer:1.8.1")
    androidTestImplementation("androidx.media3:media3-ui:1.8.1")
}

// Gobley is used solely for native Android Cargo/UniFFI integration. It retains
// the verified target linker, build variants and exactly one shared C++ runtime.
cargo {
    packageDirectory = layout.projectDirectory.dir("../core")
    builds.android {
        dynamicLibraries.addAll("c++_shared")
        variants {
            buildTaskProvider.configure {
                additionalEnvironment.put(
                    "RUSTFLAGS",
                    providers.environmentVariable("RUSTFLAGS").orElse("").map {
                        "$it -C link-args=-Wl,-z,max-page-size=16384".trim()
                    },
                )
            }
        }
    }
}

uniffi {
    generateFromLibrary {
        namespace = "velmux_core"
        build = RustAndroidTarget.Arm64
        variant = Variant.Debug
    }
}
