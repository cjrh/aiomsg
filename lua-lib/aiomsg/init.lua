-- Public entry point for the aiomsg Lua implementation.
--
--   local aiomsg = require("aiomsg")
--   local sock = aiomsg.Socket.new{ send_mode = aiomsg.SendMode.PUBLISH }
--
-- The wire-protocol and TLS modules are internal; Socket is the public surface.

local socket = require("aiomsg.socket")

return {
  Socket = socket.Socket,
  SendMode = socket.SendMode,
  Delivery = socket.Delivery,
}
