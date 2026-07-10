package = "aiomsg"
version = "2026.7.1-1"
source = {
  url = "git+https://github.com/cjrh/aiomsg.git",
  dir = "aiomsg/lua-lib",
}
description = {
  summary = "Native Lua implementation of the aiomsg wire protocol.",
  detailed = [[
    Smart sockets for simple network communication: bind/connect to many peers,
    publish / round-robin / by-identity send modes, automatic reconnection, send
    buffering, heartbeating, an optional at-least-once delivery guarantee, and
    TLS. Interoperates on the wire with the Python reference and every other
    aiomsg language port.
  ]],
  homepage = "https://github.com/cjrh/aiomsg",
  license = "Apache-2.0",
}
dependencies = {
  "lua >= 5.1",
  "luasocket",
  "luasec",
}
build = {
  type = "builtin",
  modules = {
    ["aiomsg"] = "aiomsg/init.lua",
    ["aiomsg.protocol"] = "aiomsg/protocol.lua",
    ["aiomsg.socket"] = "aiomsg/socket.lua",
    ["aiomsg.tls"] = "aiomsg/tls.lua",
  },
}
