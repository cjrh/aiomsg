-- The connect end of the two-process demo: prints whatever the server sends,
-- reconnecting automatically if the server restarts. Run alongside server.lua.
local here = arg[0]:match("^(.*)/[^/]*$") or "."
package.path = here .. "/../?.lua;" .. here .. "/../?/init.lua;" .. package.path

local aiomsg = require("aiomsg")

local sock = aiomsg.Socket.new()
sock:connect("127.0.0.1", 25000)
for message in sock:messages() do
  print(message)
end
