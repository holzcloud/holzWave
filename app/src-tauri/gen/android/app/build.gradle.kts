import java.util.Properties
import org.gradle.api.provider.ValueSource
import org.gradle.api.provider.ValueSourceParameters
import org.gradle.api.file.RegularFileProperty
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("rust")
}

val tauriProperties = Properties().apply {
    val propFile = file("tauri.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}

android {
    compileSdk = 37
    namespace = "ch.holzcloud.holzwave"
    defaultConfig {
        applicationId = "ch.holzcloud.holzwave"
        minSdk = 31
        targetSdk = 37
        versionCode = tauriProperties.getProperty("tauri.android.versionCode", "1").toInt()
        versionName = tauriProperties.getProperty("tauri.android.versionName", "1.0")
        // One ABI, whatever builds this project (the Tauri CLI, Gradle alone, Android Studio).
        ndk {
            abiFilters.add("arm64-v8a")
        }
    }
    buildTypes {
        getByName("debug") {
            applicationIdSuffix = ".debug"
            isDebuggable = true
            isJniDebuggable = true
            isMinifyEnabled = false
            packaging {
                jniLibs.keepDebugSymbols.add("*/arm64-v8a/*.so")
            }
        }
        getByName("release") {
            // No signingConfig here, on purpose: the release APK is built unsigned and the owner signs it with the owner's own
            // key outside this repository.
            optimization {
               enable = true
            }
            // The commit and the state of the checkout are not written into the APK: a rebuild from another checkout of the
            // same source must give the same file.
            vcsInfo {
                include = false
            }
            proguardFiles(
                *fileTree(".") {
                  include("**/*.pro")
                  exclude("build/**")
                }.files.toTypedArray()
            )
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }
    buildFeatures {
        buildConfig = true
    }
    // The Android Gradle plugin adds a dependency block to the signing block of an APK, encrypted so that only Google Play can
    // read it. It is left out: nobody who reads or rebuilds this APK can open it, and the app has no Play listing.
    dependenciesInfo {
        includeInApk = false
        includeInBundle = false
    }
}

kotlin {
    compilerOptions {
        jvmTarget = JvmTarget.JVM_1_8
    }
}

rust {
    rootDirRel = "../../../"
}

// The Kotlin component of the platform certificate verifier (the Rust crate `rustls-platform-verifier`, which both network clients
// of the core use). It is a prebuilt Maven artifact that the verifier's own project publishes on GitHub, outside Maven Central and
// Google's repository, so a build needs network access to it. The repository is asked for nothing but its own group, and the
// component's version is the version of the Rust crate `rustls-platform-verifier-android` in the repository's Cargo.lock, so the
// Kotlin and the Rust half cannot drift apart. Its checksums are pinned in gradle/verification-metadata.xml.
repositories {
    maven {
        url = uri("https://github.com/rustls/rustls-platform-verifier/raw/maven-archive/android-release-support/maven/")
        content {
            includeGroup("org.rustls")
        }
    }
}

abstract class RustlsVersion : ValueSource<String, RustlsVersion.Params> {
    interface Params : ValueSourceParameters {
        val lockFile: RegularFileProperty
    }

    companion object {
        const val CRATE_NAME = "rustls-platform-verifier-android"
    }

    override fun obtain(): String {
        val version = parameters.lockFile.get().asFile.readLines().let { lines ->
            val nameIdx = lines.indexOfFirst { it.trim() == "name = \"$CRATE_NAME\"" }
            if (nameIdx < 0) {
                null
            } else {
                lines.drop(nameIdx + 1)
                    .firstOrNull { it.trimStart().startsWith("version = ") }
                    ?.substringAfter('"', "")
                    ?.substringBefore('"', "")
                    ?.takeIf { it.isNotEmpty() }
            }
        }
        return version ?: error("$CRATE_NAME not found in Cargo.lock")
    }
}

// The Cargo.lock of the repository: this project is <root>/app/src-tauri/gen/android/app, so the root is five folders up.
val rustlsPlatformVerifierVersion = providers.of(RustlsVersion::class.java) {
    parameters.lockFile.set(layout.projectDirectory.file("../../../../../Cargo.lock"))
}

configurations.configureEach {
    resolutionStrategy.eachDependency {
        if (requested.group == "org.rustls" && requested.name == "rustls-platform-verifier") {
            useVersion(rustlsPlatformVerifierVersion.get())
            because("native component version must be identical to version of ${RustlsVersion.CRATE_NAME}")
        }
    }
}

dependencies {
    implementation("androidx.webkit:webkit:1.14.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.activity:activity-ktx:1.10.1")
    implementation("com.google.android.material:material:1.12.0")
    implementation("androidx.lifecycle:lifecycle-process:2.10.0")
    // The verifier's Kotlin component; the version is chosen by the rule above, never written here.
    implementation("org.rustls:rustls-platform-verifier")
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.4")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.0")
}

apply(from = file("tauri.build.gradle.kts"))
