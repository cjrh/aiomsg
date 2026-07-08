// Idiomatic Gradle build for the Java implementation. Run everything through the
// committed Gradle wrapper (./gradlew), which needs no system Gradle install:
//
//     ./gradlew build        // compile + test
//     ./gradlew test         // JUnit 5 suite (protocol + TCP/TLS integration)
//     ./gradlew runServer    // examples (also runClient, runTls)
plugins {
    application
    jacoco
}

repositories {
    mavenCentral()
}

dependencies {
    testImplementation(platform("org.junit:junit-bom:5.11.3"))
    testImplementation("org.junit.jupiter:junit-jupiter")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

java {
    // The library targets Java 21 (the floor for virtual threads); it compiles
    // cleanly on newer JDKs. Using release rather than a toolchain avoids
    // provisioning a second JDK when only a newer one is installed.
    sourceCompatibility = JavaVersion.VERSION_21
    targetCompatibility = JavaVersion.VERSION_21
}

application {
    mainClass = "aiomsg.ConformanceAgent"
}

tasks.test {
    useJUnitPlatform()
    // Where the integration test finds the shared TLS certificates.
    systemProperty("aiomsg.certDir", "$projectDir/../conformance/certs")
    testLogging { events("passed", "failed", "skipped") }
}

tasks.jacocoTestReport {
    dependsOn(tasks.test)
    reports {
        xml.required = true
        csv.required = false
        html.required = false
    }
    classDirectories.setFrom(
        files(classDirectories.files.map {
            fileTree(it) {
                exclude("aiomsg/ConformanceAgent*")
                exclude("aiomsg/examples/**")
            }
        })
    )
}

// Runnable examples, mirroring `cargo run --example` / `go run ./examples/...`.
// e.g. `./gradlew runTls --args=/path/to/certs`
fun registerExample(taskName: String, mainName: String) {
    tasks.register<JavaExec>(taskName) {
        group = "application"
        description = "Run the ${mainName.substringAfterLast('.')} example."
        classpath = sourceSets["main"].runtimeClasspath
        mainClass = mainName
    }
}
registerExample("runServer", "aiomsg.examples.Server")
registerExample("runClient", "aiomsg.examples.Client")
registerExample("runTls", "aiomsg.examples.Tls")
