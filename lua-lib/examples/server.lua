-- The bind end of the two-process demo: broadcasts the current time once a
-- second to whichever peers are connected. Run alongside client.lua.
local here = arg[0]:match("^(.*)/[^/]*$") or "."
package.path = here .. "/../?.lua;" .. here .. "/../?/init.lua;" .. package.path

local aiomsg = require("aiomsg")

local sock = aiomsg.Socket.new({ send_mode = aiomsg.SendMode.PUBLISH })
sock:bind("127.0.0.1", 25000)
while true do
  sock:send(os.date())
  sock:run(1.0) -- pump the reactor for ~1s, which also paces the broadcast
end
