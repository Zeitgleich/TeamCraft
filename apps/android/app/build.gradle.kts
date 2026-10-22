plugins {
  alias(libs.plugins.android.application)
  alias(libs.plugins.kotlin.android)
  alias(libs.plugins.kotlin.compose)
}

fun setting(name: String): String = (findProperty("teamcraft.$name") as String?).orEmpty()

android {
  namespace = "org.teamcraft.android"
  compileSdk = 35
  // Same NDK as scripts/build.sh; AGP uses it to strip the Rust library when packaging (UniFFI's
  // binding generator needs the symbols, so the library is built unstripped).
  ndkVersion = "27.2.12479018"

  defaultConfig {
    applicationId = "org.teamcraft.android"
    minSdk = 26
    targetSdk = 35
    versionCode = 1
    versionName = "0.1.0"
    // The ABIs scripts/build.sh builds the Rust library for (phones and the x86_64 emulator).
    ndk { abiFilters += listOf("arm64-v8a", "x86_64") }
    testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"

    buildConfigField("String", "SERVER_URL", "\"${setting("serverUrl")}\"")
    buildConfigField("String", "OIDC_ISSUER", "\"${setting("oidcIssuer")}\"")
    buildConfigField("String", "OIDC_CLIENT_ID", "\"${setting("oidcClientId")}\"")
    // AppAuth's redirect activity listens on this scheme; the OIDC client must allow the URI.
    manifestPlaceholders["appAuthRedirectScheme"] = setting("oidcRedirectScheme")
    buildConfigField("String", "OIDC_REDIRECT_URI", "\"${setting("oidcRedirectScheme")}:/oauth/callback\"")
  }

  flavorDimensions += "push"
  productFlavors {
    // Google Play builds: FCM and UnifiedPush.
    create("full") {
      dimension = "push"
      buildConfigField("String", "FCM_PROJECT_ID", "\"${setting("fcmProjectId")}\"")
      buildConfigField("String", "FCM_APPLICATION_ID", "\"${setting("fcmApplicationId")}\"")
      buildConfigField("String", "FCM_API_KEY", "\"${setting("fcmApiKey")}\"")
      buildConfigField("String", "FCM_SENDER_ID", "\"${setting("fcmSenderId")}\"")
    }
    // Builds without Google services (F-Droid style): UnifiedPush only.
    create("libre") {
      dimension = "push"
      applicationIdSuffix = ".libre"
    }
  }

  // Upload key for Google Play, from properties outside the repository (README.md, "Signing").
  val keystorePath = setting("uploadKeystore")
  val uploadSigning = if (keystorePath.isNotEmpty()) {
    signingConfigs.create("upload") {
      storeFile = file(keystorePath)
      storePassword = setting("uploadKeystorePassword")
      keyAlias = setting("uploadKeyAlias")
      keyPassword = setting("uploadKeyPassword")
    }
  } else {
    null
  }

  buildTypes {
    release {
      uploadSigning?.let { signingConfig = it }
      isMinifyEnabled = true
      proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
    }
  }

  buildFeatures {
    compose = true
    buildConfig = true
  }

  compileOptions {
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
  }

  kotlinOptions {
    jvmTarget = "17"
  }

  packaging {
    jniLibs.useLegacyPackaging = false
  }
}

dependencies {
  implementation(libs.androidx.core.ktx)
  implementation(libs.androidx.activity.compose)
  implementation(libs.androidx.lifecycle.runtime.compose)
  implementation(libs.androidx.lifecycle.viewmodel.compose)
  implementation(libs.androidx.lifecycle.process)
  implementation(libs.androidx.navigation.compose)
  implementation(libs.androidx.browser)
  implementation(platform(libs.compose.bom))
  implementation(libs.compose.ui)
  implementation(libs.compose.material3)
  implementation(libs.compose.material.icons)
  implementation(libs.compose.ui.tooling.preview)
  implementation(libs.kotlinx.coroutines.android)
  implementation(libs.appauth)
  implementation(libs.unifiedpush.connector)
  // UniFFI's Kotlin bindings call the Rust library through JNA.
  implementation("${libs.jna.get()}@aar")
  "fullImplementation"(libs.firebase.messaging)

  testImplementation(libs.junit)
  androidTestImplementation(libs.androidx.test.runner)
  androidTestImplementation(libs.androidx.test.junit)
}
