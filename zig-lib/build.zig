const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    // The public library module, consumable by dependents via `zig fetch` +
    // `dependency("aiomsg", ...).module("aiomsg")`. Links libc + OpenSSL:
    // std.Io owns the networking and concurrency, but the encrypted byte stream
    // goes through OpenSSL (C), which needs libc and the raw socket fd.
    const lib_mod = b.addModule("aiomsg", .{
        .root_source_file = b.path("src/aiomsg.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    lib_mod.linkSystemLibrary("ssl", .{});
    lib_mod.linkSystemLibrary("crypto", .{});

    const lib = b.addLibrary(.{ .name = "aiomsg", .root_module = lib_mod });
    b.installArtifact(lib);

    // Examples + the conformance agent. Each imports the library module and,
    // because it links OpenSSL through that module, also links libc + ssl/crypto.
    const exes = [_][]const u8{ "server", "client", "tls", "conformance_agent" };
    for (exes) |name| {
        const ex_mod = b.createModule(.{
            .root_source_file = b.path(b.fmt("examples/{s}.zig", .{name})),
            .target = target,
            .optimize = optimize,
            .link_libc = true,
        });
        ex_mod.addImport("aiomsg", lib_mod);
        ex_mod.linkSystemLibrary("ssl", .{});
        ex_mod.linkSystemLibrary("crypto", .{});
        const exe = b.addExecutable(.{ .name = name, .root_module = ex_mod });
        b.installArtifact(exe);

        const run_cmd = b.addRunArtifact(exe);
        if (b.args) |a| run_cmd.addArgs(a);
        const run_step = b.step(b.fmt("run-{s}", .{name}), b.fmt("Run the {s} example", .{name}));
        run_step.dependOn(&run_cmd.step);
    }

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
