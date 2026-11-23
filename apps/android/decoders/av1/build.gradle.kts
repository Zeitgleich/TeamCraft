// Media3's AV1 decoder module (Libdav1dVideoRenderer over dav1d), vendored from the Media3 release in
// ../media3-version.txt; README.md says what is changed and how to update it.
plugins {
  alias(libs.plugins.android.library)
}

// The vendored code uses Media3's unstable API: the app's Media3 must be the same minor release
// (patch releases keep it).
val vendoredMedia3 = file("../media3-version.txt").readText().trim()
check(libs.versions.media3.get().substringBeforeLast('.') == vendoredMedia3.substringBeforeLast('.')) {
  "decoders/ is Media3 $vendoredMedia3, the app uses ${libs.versions.media3.get()}: update decoders/ (README.md)"
}

android {
  namespace = "androidx.media3.decoder.av1"
  compileSdk = 37
  // The app's NDK (app/build.gradle.kts, scripts/build.sh).
  ndkVersion = "28.2.13676358"

  defaultConfig {
    minSdk = 26
    ndk { abiFilters += listOf("arm64-v8a", "x86_64") }
    externalNativeBuild { cmake { targets += "dav1dJNI" } }
    consumerProguardFiles("proguard-rules.txt")
    testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
  }

  compileOptions {
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
  }
}

// dav1d is fetched and built by build-native.sh (scripts/build.sh runs it). Without it the module
// builds without its JNI library and the renderer reports itself unavailable, so an IDE sync works;
// a release build refuses that.
if (file("src/main/jni/nativelib").exists()) {
  android.externalNativeBuild.cmake {
    path = file("src/main/jni/CMakeLists.txt")
    version = "3.21.0+"
  }
} else if (gradle.startParameter.taskNames.any { it.contains("Release") }) {
  throw GradleException("decoders/av1: dav1d is missing; run decoders/build-native.sh (README.md)")
}

dependencies {
  api(libs.androidx.media3.decoder)
  implementation(libs.androidx.media3.exoplayer)
  implementation(libs.androidx.annotation)
  // On a device: the decoder alone plays the clips in src/androidTest/assets (README.md, "Tests").
  androidTestImplementation(libs.androidx.test.runner)
  androidTestImplementation(libs.androidx.test.junit)
}
