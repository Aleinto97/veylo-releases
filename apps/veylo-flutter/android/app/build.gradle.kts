plugins {
    id("com.android.application")
    // The Flutter Gradle Plugin must be applied after the Android and Kotlin Gradle plugins.
    id("dev.flutter.flutter-gradle-plugin")
}

android {
    namespace = "app.veylo.veylo_flutter"
    compileSdk = flutter.compileSdkVersion
    ndkVersion = flutter.ndkVersion

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    defaultConfig {
        // TODO: Specify your own unique Application ID (https://developer.android.com/studio/build/application-id.html).
        applicationId = "app.veylo.veylo_flutter"
        // You can update the following values to match your application needs.
        // For more information, see: https://flutter.dev/to/review-gradle-config.
        minSdk = flutter.minSdkVersion
        targetSdk = flutter.targetSdkVersion
        // Uses the version code from pubspec.yaml. When using split APKs, 1000 * ABI_VERSION
        // is added automatically by Flutter. (https://developer.android.com/studio/build/configure-apk-splits#configure-APK-versions)
        // You can force using the value of versionCode by specifying the `-P force-version-code-ignoring-abi=true`
        // flag during build.
        versionCode = flutter.versionCode
        versionName = flutter.versionName
    }

    // Picks AI (on-device synopsis embeddings), from the pinned assets prepared by
    // `python3 tools/picks-ai/fetch.py android-arm64 android-armv7` (same files as the DX app):
    // the model + tokenizer as APK assets (unpacked by MainActivity), ONNX Runtime as a native lib.
    // Plain paths: AGP refuses Provider instances in the SourceSet API.
    sourceSets["main"].assets.srcDir(layout.buildDirectory.get().asFile.resolve("picks-ai/assets"))
    sourceSets["main"].jniLibs.srcDir(layout.buildDirectory.get().asFile.resolve("picks-ai/jni"))

    buildTypes {
        release {
            // TODO: Add your own signing config for the release build.
            // Signing with the debug keys for now, so `flutter run --release` works.
            signingConfig = signingConfigs.getByName("debug")
        }
    }
}

kotlin {
    compilerOptions {
        jvmTarget = org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17
    }
}

flutter {
    source = "../.."
}

val picksAiRoot = rootProject.file("../../veylo-dx/picks-ai")
val stagePicksAi by tasks.registering(Sync::class) {
    description = "Stage Picks AI model assets and ONNX Runtime for the APK"
    into(layout.buildDirectory.dir("picks-ai"))
    from(File(picksAiRoot, "model")) {
        include("model.onnx", "tokenizer.json", "LICENSE", "NOTICE")
        into("assets/picks-ai")
    }
    from(File(picksAiRoot, "android-arm64")) {
        include("LICENSE", "ThirdPartyNotices.txt")
        into("assets/picks-ai/runtime")
    }
    from(File(picksAiRoot, "android-arm64")) { include("libonnxruntime.so"); into("jni/arm64-v8a") }
    from(File(picksAiRoot, "android-armv7")) { include("libonnxruntime.so"); into("jni/armeabi-v7a") }
    from(File(picksAiRoot, "android-x86_64")) { include("libonnxruntime.so"); into("jni/x86_64") }
    doFirst {
        if (!File(picksAiRoot, "model/model.onnx").isFile) {
            logger.warn("Picks AI assets missing: run python3 tools/picks-ai/fetch.py android-arm64 android-armv7 android-x86_64 (Picks falls back to rules)")
        }
    }
}
tasks.named("preBuild") { dependsOn(stagePicksAi) }
