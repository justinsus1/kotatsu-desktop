plugins {
    kotlin("jvm") version "2.2.10"
    application
}

kotlin {
    jvmToolchain(17)
}

application {
    mainClass.set("kd.MainKt")
}

// Same library versions the parsers are compiled against
dependencies {
    implementation(files("../parsers/build/libs/kotatsu-parsers-1.0.jar"))
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.10.2")
    implementation("com.squareup.okhttp3:okhttp:5.1.0")
    implementation("com.squareup.okio:okio:3.16.0")
    implementation("org.json:json:20240303")
    implementation("androidx.collection:collection:1.5.0")
    implementation("org.jsoup:jsoup:1.21.2")
}

tasks.withType<org.jetbrains.kotlin.gradle.tasks.KotlinCompile>().configureEach {
    compilerOptions {
        freeCompilerArgs.addAll(
            "-opt-in=org.koitharu.kotatsu.parsers.InternalParsersApi",
            "-opt-in=org.koitharu.kotatsu.parsers.DirectParsersAccess",
        )
    }
}

// Single fat jar: the input for GraalVM native-image
val fatJar by tasks.registering(Jar::class) {
    archiveFileName.set("kotatsu-bridge-all.jar")
    duplicatesStrategy = DuplicatesStrategy.EXCLUDE
    manifest { attributes["Main-Class"] = "kd.MainKt" }
    from(sourceSets.main.get().output)
    dependsOn(configurations.runtimeClasspath)
    from({ configurations.runtimeClasspath.get().filter { it.name.endsWith(".jar") }.map { zipTree(it) } })
    exclude("META-INF/*.SF", "META-INF/*.DSA", "META-INF/*.RSA", "META-INF/versions/9/module-info.class")
}
