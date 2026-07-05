-- Unit tests for the pure wire-protocol layer: framing, envelope parsing, and
-- the incremental decoder. No sockets — just bytes in, bytes out. Run with
-- `lua test/protocol_test.lua`; exits non-zero on the first failure.
package.path = "./?.lua;./?/init.lua;" .. package.path

local protocol = require("aiomsg.protocol")

local passed = 0
local function check(name, condition)
  if not condition then
    io.stderr:write("FAIL: " .. name .. "\n")
    os.exit(1)
  end
  passed = passed + 1
  print("ok - " .. name)
end

-- Strip the 4-byte length prefix to get the envelope back.
local function envelope_of(frame)
  return string.sub(frame, 5)
end

-- Decode a big-endian u32 with byte arithmetic so the test runs on Lua 5.1
-- and LuaJIT, which lack string.unpack.
local function u32_of(frame)
  local b1, b2, b3, b4 = string.byte(frame, 1, 4)
  return ((b1 * 256 + b2) * 256 + b3) * 256 + b4
end

local data_frame = protocol.frame_data("hello")
check("length prefix is big-endian envelope length",
  u32_of(data_frame) == #data_frame - 4)
check("envelope length is type byte + payload", #data_frame - 4 == 1 + 5)

local id = string.rep("\7", 16)
local hello = protocol.parse_envelope(envelope_of(protocol.frame_hello(id)))
check("HELLO type", hello.type == protocol.TYPE.HELLO)
check("HELLO version", hello.version == 1)
check("HELLO identity", hello.identity == id)

local data = protocol.parse_envelope(envelope_of(protocol.frame_data("payload")))
check("DATA payload", data.payload == "payload")

local msg_id = string.rep("\9", 16)
local req = protocol.parse_envelope(envelope_of(protocol.frame_data_req(msg_id, "x")))
check("DATA_REQ msg id", req.msg_id == msg_id)
check("DATA_REQ payload", req.payload == "x")
local ack = protocol.parse_envelope(envelope_of(protocol.frame_ack(msg_id)))
check("ACK echoes msg id", ack.msg_id == msg_id)

check("unknown type parses to nil", protocol.parse_envelope("\127") == nil)
check("empty envelope parses to nil", protocol.parse_envelope("") == nil)
check("short DATA_REQ parses to nil",
  protocol.parse_envelope(string.char(protocol.TYPE.DATA_REQ) .. "ab") == nil)

local frame = protocol.frame_heartbeat()
local decoder = protocol.Decoder.new()
decoder:push(string.sub(frame, 1, 2))
check("decoder waits for a full frame", decoder:pop() == nil)
decoder:push(string.sub(frame, 3))
check("decoder reassembles a split frame", decoder:pop() == envelope_of(frame))
check("decoder is then empty", decoder:pop() == nil)

decoder:push(protocol.frame_data("a") .. protocol.frame_data("b"))
check("decoder splits coalesced frame 1",
  protocol.parse_envelope(decoder:pop()).payload == "a")
check("decoder splits coalesced frame 2",
  protocol.parse_envelope(decoder:pop()).payload == "b")

print(string.format("\n%d protocol checks passed", passed))
