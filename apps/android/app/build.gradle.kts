import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
  alias(libs.plugins.android.application)
  alias(libs.plugins.kotlin.compose)
  // The core's chat messages are client-api JSON; the app mirrors the view models as data classes.
  alias(libs.plugins.kotlin.serialization)
}

fun setting(name: String): String = (findProperty("teamcraft.$name") as String?).orEmpty()

/**
 * The App Links filter of `teamcraft.appLinkHosts` (comma-separated host names), merged into the app's
 * manifest. Android only offers verified links for hosts listed when the app is built, so a build
 * can't cover unknown self-hosted instances; those use the `teamcraft://` filter (docs/app-links.md).
 * The paths are the places a link may name: keep in step with teamcraft-server `services/app_links.rs`.
 */
abstract class AppLinksManifestTask : DefaultTask() {
  @get:Input abstract val hosts: ListProperty<String>

  @get:OutputFile abstract val manifest: RegularFileProperty

  @TaskAction
  fun write() {
    val data = hosts.get().joinToString("\n") { host -> "        <data android:host=\"$host\" />" }
    val filter = if (hosts.get().isEmpty()) {
      ""
    } else {
      """
      <intent-filter android:autoVerify="true">
        <action android:name="android.intent.action.VIEW" />
        <category android:name="android.intent.category.DEFAULT" />
        <category android:name="android.intent.category.BROWSABLE" />
        <data android:scheme="https" />
$data
        <data android:path="/home" />
        <data android:pathPrefix="/c/" />
        <data android:pathPrefix="/s/" />
        <data android:pathPrefix="/dm/" />
        <data android:pathPrefix="/meeting/" />
      </intent-filter>
      """
    }
    manifest.get().asFile.writeText(
      """
      <?xml version="1.0" encoding="utf-8"?>
      <manifest xmlns:android="http://schemas.android.com/apk/res/android">
        <application>
          <activity android:name="de.zeitgleich.teamcraft.ui.MainActivity">$filter</activity>
        </application>
      </manifest>
      """.trimIndent(),
    )
  }
}

android {
  namespace = "de.zeitgleich.teamcraft"
  compileSdk = 37
  // Same NDK as scripts/build.sh; AGP uses it to strip the Rust library when packaging (UniFFI's
  // binding generator needs the symbols, so the library is built unstripped).
  ndkVersion = "28.2.13676358"

  defaultConfig {
    applicationId = "de.zeitgleich.teamcraft"
    minSdk = 26
    targetSdk = 37
    // docs/release.md: versionCode = MAJOR*1000000 + MINOR*10000 + PATCH*100 + stage, where stage is
    // beta.N -> N, rc.N -> 50+N and the final release -> 99, so every release sorts above the last.
    versionCode = 1002799
    versionName = "1.0.27"
    // The ABIs scripts/build.sh builds the Rust library for (phones and the x86_64 emulator).
    ndk { abiFilters += listOf("arm64-v8a", "x86_64") }
    testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"

    // README.md "Server address": no server is baked in; a self-built app may pre-select one.
    buildConfigField("String", "DEFAULT_SERVER", "\"${setting("defaultServer")}\"")
    // The push gateway of the publisher this build is made for (docs/push-gateway.md); empty: the
    // gateway the server names.
    buildConfigField("String", "PUSH_GATEWAY_URL", "\"${setting("pushGatewayUrl")}\"")
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
      // Play's policy on photo and video access: the system photo picker only, no media library
      // (no recent-photos strip, no albums). Its manifest removes the permissions.
      buildConfigField("boolean", "READS_MEDIA_LIBRARY", "false")
    }
    // Builds without Google services (F-Droid style): UnifiedPush only.
    create("libre") {
      dimension = "push"
      applicationIdSuffix = ".libre"
      // The recent-photos strip and the albums read the device's media library (libre manifest).
      buildConfigField("boolean", "READS_MEDIA_LIBRARY", "true")
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
    // Installs next to the store app, so a test build doesn't replace it.
    debug {
      applicationIdSuffix = ".debug"
      versionNameSuffix = "-debug"
    }
    release {
      uploadSigning?.let { signingConfig = it }
      isMinifyEnabled = true
      proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
      // The Rust library's symbol table goes into the App Bundle's metadata, so Google Play can
      // symbolicate native crashes (the R8 mapping is in the bundle anyway); the APKs stay stripped.
      ndk { debugSymbolLevel = "SYMBOL_TABLE" }
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

  packaging {
    jniLibs.useLegacyPackaging = false
  }
}

/**
 * `sentry-debug-meta.properties` with the ProGuard UUID of `teamcraft.proguardUuid`: sentry-android
 * sends it with every report, so the backend picks the R8 mapping the release workflow uploaded
 * under that UUID (docs/crash-reports.md, "Symbols"). Without the property nothing is generated.
 */
abstract class ProguardUuidAssetTask : DefaultTask() {
  @get:Input abstract val uuid: Property<String>

  @get:OutputDirectory abstract val outputDirectory: DirectoryProperty

  @TaskAction
  fun write() {
    outputDirectory.get().file("sentry-debug-meta.properties").asFile
      .writeText("io.sentry.ProguardUuids=${uuid.get()}\n")
  }
}

androidComponents.onVariants { variant ->
  val proguardUuid = setting("proguardUuid")
  if (proguardUuid.isNotEmpty() && variant.name == "fullRelease") {
    val uuidTask = tasks.register<ProguardUuidAssetTask>("generateFullReleaseProguardUuidAsset") {
      uuid.set(proguardUuid)
      outputDirectory.set(layout.buildDirectory.dir("generated/proguardUuid/${variant.name}"))
    }
    variant.sources.assets?.addGeneratedSourceDirectory(uuidTask, ProguardUuidAssetTask::outputDirectory)
  }
  val task = tasks.register<AppLinksManifestTask>("generate${variant.name.replaceFirstChar(Char::uppercase)}AppLinksManifest") {
    hosts.set(setting("appLinkHosts").split(',').map(String::trim).filter(String::isNotEmpty))
    manifest.set(layout.buildDirectory.file("generated/appLinks/${variant.name}/AndroidManifest.xml"))
  }
  variant.sources.manifests.addGeneratedManifestFile(task, AppLinksManifestTask::manifest)
  // A debug build installed with adb runs from the APK, without dexopt: compressed, its 60 MB of
  // unminified dex were inflated at every start (0.85 s on a mid-range phone). Stored, they are
  // mapped. Release builds stay compressed: they are minified and the installer compiles them.
  if (variant.buildType == "debug") variant.packaging.dex.useLegacyPackaging.set(false)
}

kotlin {
  compilerOptions {
    jvmTarget.set(JvmTarget.JVM_17)
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
  // The in-app camera's video recording (#1358).
  implementation(libs.androidx.camera.video)
  // QR codes for device linking, shown and scanned (pure Java, Apache-2.0; no Play services).
  // The in-app video player (#200): the platform's decoders behind Media3's demuxers and controls.
  implementation(libs.androidx.media3.exoplayer)
  implementation(libs.androidx.media3.ui)
  // Trim, mute and scale videos on the device before sending (#1358): Media3 Transformer, Apache-2.0.
  implementation(libs.androidx.media3.transformer)
  implementation(libs.androidx.media3.effect)
  implementation(libs.zxing.core)
  // Search segment prefetch on unmetered networks while charging.
  implementation(libs.androidx.work.runtime)
  // App lock: biometrics or the device's screen lock through the system prompt.
  implementation(libs.androidx.biometric)
  // Installs the baseline profile (src/main/baseline-prof.txt and the libraries' own) on devices
  // without Play, so APKs from GitHub and F-Droid start and scroll precompiled too.
  implementation(libs.androidx.profileinstaller)
  // UniFFI's Kotlin bindings call the Rust library through JNA.
  implementation("${libs.jna.get()}@aar")
  "fullImplementation"(libs.firebase.messaging)
  // Crash reports (docs/crash-reports.md): JVM, ANR and NDK crashes to the instance's backend.
  // Core and NDK only, not the `sentry-android` bundle: it adds session replay, which is never used.
  "fullImplementation"(libs.sentry.android.core)
  "fullImplementation"(libs.sentry.android.ndk)

  testImplementation(libs.junit)
  androidTestImplementation(libs.androidx.test.runner)
  androidTestImplementation(libs.androidx.test.junit)
}
