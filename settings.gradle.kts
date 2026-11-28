pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}
dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
    }
}
rootProject.name = "Velmux"
include(":UI", ":android")
include(":android-fixtures")
project(":android-fixtures").projectDir = file("tests/android-fixtures")
