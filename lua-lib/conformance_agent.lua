-- Conformance test agent for the cross-language interop suite (Lua).
--
-- Same CLI as every other agent (see ../conformance). A `sink` prints each
-- received message (utf-8) on its own line and exits after --count messages; a
-- `source` sends --count messages then lingers; an `echo` reflects each message
-- back to its sender. Run with `lua conformance_agent.lua --role ...`.

-- Make `require("aiomsg")` resolve against this script's directory.
local here = arg[0]:match("^(.*)/[^/]*$") or "."
package.path = here .. "/?.lua;" .. here .. "/?/init.lua;" .. package.path

local aiomsg = require("aiomsg")

local function parse_args()
  local a = {}
  local i = 1
  while i < #arg do
    local key = arg[i]
    if key:sub(1, 2) == "--" then
      a[key:sub(3)] = arg[i + 1]
      i = i + 2
    else
      i = i + 1
    end
  end
  return a
end

local function from_hex(s)
  return (s:gsub("..", function(byte)
    return string.char(tonumber(byte, 16))
  end))
end

local a = parse_args()
local role = a.role or "connect"
local host = a.host or "127.0.0.1"
local port = tonumber(a.port or "25000")
local behavior = a.behavior or "sink"
local count = tonumber(a.count or "10")
local prefix = a.prefix or "m"
local linger = tonumber(a.linger or "1.0")
local tls_on = a.tls == "true"

local sock = aiomsg.Socket.new({
  send_mode = (a["send-mode"] == "publish") and aiomsg.SendMode.PUBLISH or aiomsg.SendMode.ROUND_ROBIN,
  delivery = (a.delivery == "at-least-once") and aiomsg.Delivery.AT_LEAST_ONCE or aiomsg.Delivery.AT_MOST_ONCE,
  identity = (a.identity and a.identity ~= "") and from_hex(a.identity) or nil,
})

if role == "bind" then
  sock:bind(host, port, tls_on and { cert = a["tls-cert"], key = a["tls-key"] } or nil)
else
  sock:connect(host, port, tls_on and { ca = a["tls-ca"], server_name = a["tls-server-name"] } or nil)
end

if behavior == "source" then
  for i = 0, count - 1 do
    sock:send(prefix .. i)
  end
  sock:run(linger)
elseif behavior == "echo" then
  for _ = 1, count do
    local data, sender = sock:recv()
    if not data then
      break
    end
    sock:send_to(sender, data)
  end
  sock:run(linger)
else -- sink
  for _ = 1, count do
    local data = sock:recv()
    if not data then
      break
    end
    io.write(data, "\n")
    io.flush()
  end
end

sock:close()
