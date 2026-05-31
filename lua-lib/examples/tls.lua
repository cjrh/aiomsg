-- Self-contained TLS demo: a bind socket (TLS server) and a connect socket (TLS
-- client) in one process, using the shared conformance certificate. Because the
-- reactor is cooperative, both sockets are driven by hand — poll() advances the
-- server while recv() advances the client.
local here = arg[0]:match("^(.*)/[^/]*$") or "."
package.path = here .. "/../?.lua;" .. here .. "/../?/init.lua;" .. package.path

local aiomsg = require("aiomsg")

local certs = here .. "/../../conformance/certs"
local server = aiomsg.Socket.new()
server:bind("127.0.0.1", 25000, { cert = certs .. "/cert.pem", key = certs .. "/key.pem" })

local client = aiomsg.Socket.new()
client:connect("127.0.0.1", 25000, { ca = certs .. "/cert.pem", server_name = "localhost" })

server:send("hello over TLS")
local message
while not message do
  server:poll(0) -- advance the server's reactor
  message = client:recv(0.1) -- advance the client's, waiting briefly
end
print(message)

client:close()
server:close()
