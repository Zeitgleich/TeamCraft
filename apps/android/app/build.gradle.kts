plugins {
  alias(libs.plugins.android.application)
  alias(libs.plugins.kotlin.android)
  alias(libs.plugins.kotlin.compose)
  // The core's chat messages are client-api JSON; the app mirrors the view models as data classes.
  alias(libs.plugins.kotlin.serialization)
}

fun setting(name: String): String = (findProperty("teamcraft.$name") as String?).orEmpty()

android {
  namespace = "de.zeitgleich.teamcraft"
  compileSdk = 35
  // Same NDK as scripts/build.sh; AGP uses it to strip the Rust library when packaging (UniFFI's
  // binding generator needs the symbols, so the library is built unstripped).
  ndkVersion = "27.2.12479018"

  defaultConfig {
    applicationId = "de.zeitgleich.teamcraft"
    minSdk = 26
    targetSdk = 35
    // docs/release.md: versionCode = MAJOR*1000000 + MINOR*10000 + PATCH*100 + stage, where stage is
    // beta.N -> N, rc.N -> 50+N and the final release -> 99, so every release sorts above the last.
    versionCode = 1000099
    versionName = "1.0.0"
    // The ABIs scripts/build.sh builds the Rust library for (phones and the x86_64 emulator).
    ndk { abiFilters += listOf("arm64-v8a", "x86_64") }
    testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"

    buildConfigField("String", "SERVER_URL", "\"${setting("serverUrl")}\"")
    // The push gateway of the publisher this build is made for (docs/push-gateway.md); empty: the
    // gateway the server names.
    buildConfigField("String", "PUSH_GATEWAY_URL", "\"${setting("pushGatewayUrl")}\"")
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
  implementation(libs.kotlinx.serialization.json)
  implementation(libs.appauth)
  implementation(libs.unifiedpush.connector)
  // Calls: Telecom integration and Android Auto calling.
  implementation(libs.androidx.core.telecom)
  implementation(libs.androidx.car.app)
  // Camera for video calls and the device-link QR scanner (CameraX, Apache-2.0).
  implementation(libs.androidx.camera.camera2)
  implementation(libs.androidx.camera.lifecycle)
  implementation(libs.androidx.camera.view)
  // QR codes for device linking, shown and scanned (pure Java, Apache-2.0; no Play services).
  implementation(libs.zxing.core)
  // Search segment prefetch on unmetered networks while charging.
  implementation(libs.androidx.work.runtime)
  // UniFFI's Kotlin bindings call the Rust library through JNA.
  implementation("${libs.jna.get()}@aar")
  "fullImplementation"(libs.firebase.messaging)

  testImplementation(libs.junit)
  androidTestImplementation(libs.androidx.test.runner)
  androidTestImplementation(libs.androidx.test.junit)
}
