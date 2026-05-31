const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    // The library module. Links libc + OpenSSL: std.Io owns the networking and
    // concurrency, but the encrypted byte stream goes through OpenSSL (C), which
    // needs libc and the raw socket fd.
    const lib_mod = b.createModule(.{
        .root_source_file = b.path("src/aiomsg.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    lib_mod.linkSystemLibrary("ssl", .{});
    lib_mod.linkSystemLibrary("crypto", .{});

    const lib = b.addLibrary(.{ .name = "aiomsg", .root_module = lib_mod });
    b.installArtifact(lib);

    // Conformance agent example.
    const agent_mod = b.createModule(.{
        .root_source_file = b.path("examples/conformance_agent.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    agent_mod.addImport("aiomsg", lib_mod);
    agent_mod.linkSystemLibrary("ssl", .{});
    agent_mod.linkSystemLibrary("crypto", .{});
    const agent = b.addExecutable(.{ .name = "conformance_agent", .root_module = agent_mod });
    b.installArtifact(agent);

    // Tests: protocol unit tests + the integration test (TCP + TLS).
    const test_step = b.step("test", "Run all tests");

    const proto_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/protocol.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });
    test_step.dependOn(&b.addRunArtifact(proto_tests).step);

    // Path to the shared conformance certs, for the TLS integration test.
    const certs = b.pathFromRoot("../conformance/certs");
    const opts = b.addOptions();
    opts.addOption([]const u8, "cert_dir", certs);
    lib_mod.addOptions("build_options", opts);
    const integ_tests = b.addTest(.{ .root_module = lib_mod });
    test_step.dependOn(&b.addRunArtifact(integ_tests).step);
}
