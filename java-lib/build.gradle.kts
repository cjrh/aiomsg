// Idiomatic Gradle build for the Java implementation. Plain javac also works
// (see the justfile), which is what the cross-language conformance suite uses;
// this descriptor is for when a Gradle install is available.
plugins {
    application
}

java {
    toolchain {
        // Java 21 is the floor for virtual threads (the project uses 25).
        languageVersion = JavaLanguageVersion.of(21)
    }
}

application {
    mainClass = "aiomsg.ConformanceAgent"
}

// The test runner is a plain main() (no JUnit dependency), so it builds with
// nothing but the JDK. Run it via `gradle runTests`.
tasks.register<JavaExec>("runTests") {
    group = "verification"
    description = "Run the dependency-free protocol + integration tests."
    classpath = sourceSets["test"].runtimeClasspath
    mainClass = "aiomsg.Tests"
    args = listOf("../conformance/certs")
}
