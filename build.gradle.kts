plugins {
    // Temporary declarations for the existing iOS bridge until direct Swift verification.
    kotlin("multiplatform") version "2.2.20" apply false
    kotlin("android") version "2.2.20" apply false
    kotlin("plugin.compose") version "2.2.20" apply false
    kotlin("plugin.atomicfu") version "2.2.20" apply false
    id("org.jetbrains.compose") version "1.9.3" apply false
    id("com.android.application") version "8.10.1" apply false
    id("com.android.library") version "8.10.1" apply false
    id("dev.gobley.cargo") version "0.3.7" apply false
    id("dev.gobley.uniffi") version "0.3.7" apply false
}
