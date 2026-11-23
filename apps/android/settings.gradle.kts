pluginManagement {
  repositories {
    google()
    mavenCentral()
    gradlePluginPortal()
  }
}

dependencyResolutionManagement {
  repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
  repositories {
    google()
    mavenCentral()
  }
}

rootProject.name = "teamcraft-android"
include(":app")
// Media3's AV1 (dav1d) and VP9 (libvpx) decoders, built here: they aren't on Google's Maven
// (decoders/README.md).
include(":decoder-av1", ":decoder-vp9")
project(":decoder-av1").projectDir = file("decoders/av1")
project(":decoder-vp9").projectDir = file("decoders/vp9")
