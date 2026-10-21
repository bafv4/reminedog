import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    kotlin("jvm") version "2.4.20"
}

// リリースのワークフローは -PreminedogVersion=1.2.3 を渡す。ほかのビルドは "dev"。
val reminedogVersion: String? = providers.gradleProperty("reminedogVersion").orNull
version = reminedogVersion ?: "dev"

repositories {
    mavenCentral()
}

dependencies {
    implementation("com.formdev:flatlaf:3.7.2")

    testImplementation(kotlin("test"))
    testImplementation(platform("org.junit:junit-bom:5.14.4"))
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

// jar は PC にある Java でダブルクリックで動かすので、Java 8 のことがある。
kotlin {
    compilerOptions {
        jvmTarget = JvmTarget.JVM_1_8
        // javac の --release と同じく、Java 8 の API だけを使える。
        freeCompilerArgs.add("-Xjdk-release=1.8")
        allWarningsAsErrors = true
    }
}

tasks.withType<JavaCompile>().configureEach {
    options.release = 8
}

tasks.test {
    useJUnitPlatform()
}

// ダブルクリックで動くように、依存（FlatLaf と Kotlin のライブラリ）を入れた 1 つの jar。
tasks.jar {
    archiveFileName = reminedogVersion?.let { "reminedog-installer-$it.jar" } ?: "reminedog-installer.jar"
    manifest {
        attributes(
            "Main-Class" to "reminedog.installer.MainKt",
            // Download.INSTALLER_VERSION が読み返す。
            "Implementation-Version" to project.version,
            // FlatLaf は新しい Java 向けのクラスを持つ（マルチリリースの jar）。
            "Multi-Release" to "true",
            // FlatLaf のネイティブのライブラリ（Windows のウィンドウのタイトルバー）は JNI を使う。
            "Enable-Native-Access" to "ALL-UNNAMED",
        )
    }
    from(configurations.runtimeClasspath.map { files -> files.map { if (it.isDirectory) it else zipTree(it) } }) {
        exclude("META-INF/MANIFEST.MF", "META-INF/*.SF", "META-INF/*.DSA", "META-INF/*.RSA", "**/module-info.class")
    }
    duplicatesStrategy = DuplicatesStrategy.EXCLUDE
}
