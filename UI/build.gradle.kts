import gobley.gradle.Variant
import gobley.gradle.rust.targets.RustAndroidTarget
import gobley.gradle.rust.targets.RustAppleMobileTarget

plugins {
    kotlin("multiplatform")
    kotlin("plugin.compose")
    kotlin("plugin.atomicfu")
    id("org.jetbrains.compose")
    id("com.android.library")
    id("dev.gobley.cargo")
    id("dev.gobley.uniffi")
}

kotlin {
    jvmToolchain(21)
    androidTarget()
    iosArm64()
    iosSimulatorArm64()
    targets.withType<org.jetbrains.kotlin.gradle.plugin.mpp.KotlinNativeTarget>().configureEach {
        binaries.framework {
            baseName = "VelmuxUI"
            isStatic = true
            binaryOption("bundleId", "com.velmux.ui")
            // Native dependency objects and both native hosts retain iOS 15 compatibility.
            freeCompilerArgs += listOf(
                "-Xoverride-konan-properties=osVersionMin.ios_arm64=15.0;osVersionMin.ios_simulator_arm64=15.0",
            )
        }
    }
    sourceSets {
        commonMain {
            kotlin.setSrcDirs(listOf("src"))
            dependencies {
                api(compose.runtime)
                implementation(compose.foundation)
                implementation(compose.material3)
                implementation(compose.ui)
                implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.10.2")
                implementation("org.jetbrains.kotlinx:atomicfu:0.29.0")
            }
        }
        commonTest {
            kotlin.setSrcDirs(listOf("tests"))
            dependencies { implementation(kotlin("test")) }
        }
        androidMain { kotlin.setSrcDirs(listOf("integration/android")) }
        iosMain { kotlin.setSrcDirs(listOf("integration/ios")) }
    }
}

android {
    namespace = "com.velmux.ui"
    compileSdk = 35
    ndkVersion = "27.2.12479018"
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_21
        targetCompatibility = JavaVersion.VERSION_21
    }
    defaultConfig {
        minSdk = 29
        ndk { abiFilters += setOf("arm64-v8a", "x86_64") }
    }
}

compose.resources {
    customDirectory(
        sourceSetName = "commonMain",
        directoryProvider = provider { layout.projectDirectory.dir("resources") },
    )
}

cargo { packageDirectory = layout.projectDirectory.dir("../core") }
uniffi {
    generateFromLibrary {
        namespace = "velmux_core"
        build = when (providers.gradleProperty("bindingsTarget").orElse("android").get()) {
            "android" -> RustAndroidTarget.Arm64
            "ios" -> RustAppleMobileTarget.IosArm64
            else -> error("bindingsTarget must be android or ios")
        }
        variant = Variant.Debug
    }
}
