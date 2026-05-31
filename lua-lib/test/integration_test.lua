-- Integration tests: two in-process Sockets over real TCP/TLS loopback. Because
-- the reactor is cooperative and single-threaded, the test drives both sockets
-- by hand — poll() advances the source while recv() advances the sink. Run with
-- `lua test/integration_test.lua`; exits non-zero on failure.
package.path = "./?.lua;./?/init.lua;" .. package.path

local socket = require("socket")
local aiomsg = require("aiomsg")

local function check(name, condition)
  if not condition then
    io.stderr:write("FAIL: " .. name .. "\n")
    os.exit(1)
  end
  print("ok - " .. name)
end

local function free_port()
  local server = assert(socket.bind("127.0.0.1", 0))
  local _, port = server:getsockname()
  server:close()
  return tonumber(port)
end

-- Pump both sockets until the sink has collected `count` messages.
local function collect(source, sink, count)
  local got = {}
  local deadline = socket.gettime() + 5
  while #got < count and socket.gettime() < deadline do
    source:poll(0)
    local data = sink:recv(0.02)
    if data then
      got[#got + 1] = data
    end
  end
  return got
end

local function same(list, expected)
  if #list ~= #expected then
    return false
  end
  for i = 1, #expected do
    if list[i] ~= expected[i] then
      return false
    end
  end
  return true
end

-- Round-robin from a connect-end source to a bind-end sink.
do
  local port = free_port()
  local sink = aiomsg.Socket.new()
  local source = aiomsg.Socket.new({ send_mode = aiomsg.SendMode.ROUND_ROBIN })
  sink:bind("127.0.0.1", port)
  source:connect("127.0.0.1", port)
  for i = 0, 4 do
    source:send("m" .. i)
  end
  check("round-robin connect->bind", same(collect(source, sink, 5), { "m0", "m1", "m2", "m3", "m4" }))
  source:close()
  sink:close()
end

-- Bind-end source buffers sends until a sink connects.
do
  local port = free_port()
  local source = aiomsg.Socket.new({ send_mode = aiomsg.SendMode.ROUND_ROBIN })
  local sink = aiomsg.Socket.new()
  source:bind("127.0.0.1", port)
  for i = 0, 2 do
    source:send("b" .. i) -- no peer yet
  end
  sink:connect("127.0.0.1", port)
  check("bind-side source buffering", same(collect(source, sink, 3), { "b0", "b1", "b2" }))
  source:close()
  sink:close()
end

-- At-least-once delivery (DATA_REQ / ACK).
do
  local port = free_port()
  local sink = aiomsg.Socket.new()
  local source = aiomsg.Socket.new({
    send_mode = aiomsg.SendMode.ROUND_ROBIN,
    delivery = aiomsg.Delivery.AT_LEAST_ONCE,
  })
  sink:bind("127.0.0.1", port)
  source:connect("127.0.0.1", port)
  for i = 0, 3 do
    source:send("a" .. i)
  end
  check("at-least-once delivery", same(collect(source, sink, 4), { "a0", "a1", "a2", "a3" }))
  source:close()
  sink:close()
end

-- TLS round-trip using the shared conformance certificate.
do
  local certs = "../conformance/certs"
  local port = free_port()
  local sink = aiomsg.Socket.new()
  local source = aiomsg.Socket.new()
  sink:bind("127.0.0.1", port, { cert = certs .. "/cert.pem", key = certs .. "/key.pem" })
  source:connect("127.0.0.1", port, { ca = certs .. "/cert.pem" })
  for i = 0, 2 do
    source:send("t" .. i)
  end
  check("messages round-trip over TLS", same(collect(source, sink, 3), { "t0", "t1", "t2" }))
  source:close()
  sink:close()
end

print("\nall integration checks passed")
