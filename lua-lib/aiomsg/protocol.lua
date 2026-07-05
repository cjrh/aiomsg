-- Wire protocol: framing and typed envelopes (the Lua counterpart of
-- ../PROTOCOL.md).
--
-- Pure data — no sockets — so it can be unit-tested in isolation. Lua strings
-- are immutable byte arrays, which suits the wire format perfectly: frames are
-- [u32 big-endian length][envelope] and an envelope is [u8 type][body]. The
-- length prefix is packed/unpacked with plain byte arithmetic rather than
-- string.pack so the module runs on Lua 5.1 and LuaJIT as well as 5.3+. The
-- frame_* helpers return a ready-to-write string; the Decoder reassembles
-- frames from a byte stream that may arrive in arbitrary chunks.

local M = {}

M.VERSION = 1
M.IDENTITY_SIZE = 16
M.MSG_ID_SIZE = 16

M.TYPE = {
  HELLO = 0x01,
  HEARTBEAT = 0x02,
  DATA = 0x03,
  DATA_REQ = 0x04,
  ACK = 0x05,
}

-- Big-endian u32 <-> 4-byte string, portable across Lua versions.
local function encode_u32(n)
  return string.char(
    math.floor(n / 16777216) % 256,
    math.floor(n / 65536) % 256,
    math.floor(n / 256) % 256,
    n % 256
  )
end

local function decode_u32(bytes)
  local b1, b2, b3, b4 = string.byte(bytes, 1, 4)
  return ((b1 * 256 + b2) * 256 + b3) * 256 + b4
end

-- Build [u32 length][type][body] as a string.
local function frame(msg_type, body)
  local envelope = string.char(msg_type) .. body
  return encode_u32(#envelope) .. envelope
end

function M.frame_hello(identity)
  return frame(M.TYPE.HELLO, string.char(M.VERSION) .. identity)
end

function M.frame_heartbeat()
  return frame(M.TYPE.HEARTBEAT, "")
end

function M.frame_data(payload)
  return frame(M.TYPE.DATA, payload)
end

function M.frame_data_req(msg_id, payload)
  return frame(M.TYPE.DATA_REQ, msg_id .. payload)
end

function M.frame_ack(msg_id)
  return frame(M.TYPE.ACK, msg_id)
end

-- Parse one envelope (no length prefix). Returns a table {type, ...} or nil if
-- empty, truncated, or an unknown type — callers skip such frames per the
-- protocol's forward-compatibility rule.
function M.parse_envelope(envelope)
  if #envelope < 1 then
    return nil
  end
  local msg_type = string.byte(envelope, 1)
  local body = string.sub(envelope, 2)
  if msg_type == M.TYPE.HELLO then
    if #body < 1 + M.IDENTITY_SIZE then
      return nil
    end
    return {
      type = msg_type,
      version = string.byte(body, 1),
      identity = string.sub(body, 2, 1 + M.IDENTITY_SIZE),
      payload = "",
    }
  elseif msg_type == M.TYPE.HEARTBEAT then
    return { type = msg_type, payload = "" }
  elseif msg_type == M.TYPE.DATA then
    return { type = msg_type, payload = body }
  elseif msg_type == M.TYPE.DATA_REQ then
    if #body < M.MSG_ID_SIZE then
      return nil
    end
    return {
      type = msg_type,
      msg_id = string.sub(body, 1, M.MSG_ID_SIZE),
      payload = string.sub(body, M.MSG_ID_SIZE + 1),
    }
  elseif msg_type == M.TYPE.ACK then
    if #body < M.MSG_ID_SIZE then
      return nil
    end
    return { type = msg_type, msg_id = string.sub(body, 1, M.MSG_ID_SIZE), payload = "" }
  else
    return nil
  end
end

-- Incremental frame reassembler: push arbitrary bytes, pop complete envelopes.
-- Tolerates a frame split across several pushes and several frames coalesced
-- into one chunk.
local Decoder = {}
Decoder.__index = Decoder

function Decoder.new()
  return setmetatable({ buffer = "" }, Decoder)
end

function Decoder:push(bytes)
  self.buffer = self.buffer .. bytes
end

-- The next complete envelope, or nil.
function Decoder:pop()
  if #self.buffer < 4 then
    return nil
  end
  local length = decode_u32(self.buffer)
  if #self.buffer < 4 + length then
    return nil
  end
  local envelope = string.sub(self.buffer, 5, 4 + length)
  self.buffer = string.sub(self.buffer, 5 + length)
  return envelope
end

M.Decoder = Decoder

return M
