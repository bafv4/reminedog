import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    kotlin("jvm") version "2.4.20"
}

repositories {
    mavenCentral()
}

dependencies {
    implementation("com.formdev:flatlaf:3.7.2")

    testImplementation(kotlin("test"))
    testImplementation(platform("org.junit:junit-bom:5.14.4"))
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

// The jar is double-clicked with whatever Java the PC has, which can be Java 8.
kotlin {
    compilerOptions {
        jvmTarget = JvmTarget.JVM_1_8
        // Like javac's --release: only the Java 8 API can be used.
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

// One jar with the dependencies (FlatLaf and the Kotlin library), so that it runs by double-clicking.
tasks.jar {
    archiveFileName = "reminedog-installer.jar"
    manifest {
        attributes(
            "Main-Class" to "reminedog.installer.MainKt",
            // FlatLaf has classes for newer Java versions (a multi-release jar).
            "Multi-Release" to "true",
            // FlatLaf's native library (window title bar on Windows) uses JNI.
            "Enable-Native-Access" to "ALL-UNNAMED",
        )
    }
    from(configurations.runtimeClasspath.map { files -> files.map { if (it.isDirectory) it else zipTree(it) } }) {
        exclude("META-INF/MANIFEST.MF", "META-INF/*.SF", "META-INF/*.DSA", "META-INF/*.RSA", "**/module-info.class")
    }
    duplicatesStrategy = DuplicatesStrategy.EXCLUDE
}
