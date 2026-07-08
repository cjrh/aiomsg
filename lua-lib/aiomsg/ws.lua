-- Server-side WebSocket (RFC 6455) byte-stream adapter for aiomsg bind sockets
-- (see ../PROTOCOL.md §10).
--
-- This module is the Lua counterpart of python-lib/aiomsg/ws.py. It is pure
-- byte manipulation — no sockets — so it slots into the reactor in socket.lua
-- and is unit-testable in isolation. It implements only the server half of
-- RFC 6455 and never negotiates any subprotocol or extension.
--
-- The two public pieces the reactor needs:
--   * the HTTP upgrade (parse_upgrade / success_response / error_response), and
--   * the frame layer: encode() to wrap one aiomsg frame as a binary WebSocket
--     message, and a parser (new_parser + ingest) that turns the concatenation
--     of inbound binary-message payloads back into the aiomsg byte stream of
--     §2, disposing of interleaved control frames (ping->pong, pong ignored,
--     close echoed). WebSocket message boundaries carry no meaning.
--
-- Like the rest of aiomsg (see protocol.lua) this stays portable across Lua 5.1,
-- LuaJIT and 5.3+ by doing its own arithmetic bit-twiddling rather than relying
-- on the native bitwise operators, which only 5.3+ can even parse. SHA-1 is
-- hand-rolled here so the dependency set stays exactly LuaSocket + LuaSec (LuaSec
-- exposes no general digest); base64 is LuaSocket's mime.

local mime = require("mime")

local M = {}

M.GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"

-- Opcodes (RFC 6455 §5.2).
M.OP_CONT = 0x0
M.OP_TEXT = 0x1
M.OP_BINARY = 0x2
M.OP_CLOSE = 0x8
M.OP_PING = 0x9
M.OP_PONG = 0xA

M.MAX_REQUEST_BYTES = 8192 -- cap the pre-upgrade HTTP request

-- --- Portable 32-bit bitwise ops --------------------------------------------
--
-- Bit-by-bit over Lua numbers, so this module parses and runs on 5.1/LuaJIT as
-- well as 5.3+ (see the note at the top). Operands and results are unsigned
-- 32-bit values; every intermediate stays below 2^32, well within a double's
-- exact-integer range. Only SHA-1 needs the full and/or/xor; the frame layer
-- gets by with plain arithmetic.

local function band(a, b)
  local r, bit = 0, 1
  while a > 0 and b > 0 do
    if a % 2 == 1 and b % 2 == 1 then
      r = r + bit
    end
    a, b, bit = math.floor(a / 2), math.floor(b / 2), bit * 2
  end
  return r
end

local function bor(a, b)
  local r, bit = 0, 1
  while a > 0 or b > 0 do
    if a % 2 == 1 or b % 2 == 1 then
      r = r + bit
    end
    a, b, bit = math.floor(a / 2), math.floor(b / 2), bit * 2
  end
  return r
end

local function bxor(a, b)
  local r, bit = 0, 1
  while a > 0 or b > 0 do
    if a % 2 ~= b % 2 then
      r = r + bit
    end
    a, b, bit = math.floor(a / 2), math.floor(b / 2), bit * 2
  end
  return r
end

-- Left-rotate a 32-bit word by n bits (0 < n < 32). The shifted-out high bits
-- and the surviving low bits land in disjoint positions, so a plain add rebuilds
-- the word, and neither term ever reaches 2^32.
local function rotl32(x, n)
  x = x % 0x100000000
  local hi = 2 ^ (32 - n)
  return (x % hi) * (2 ^ n) + math.floor(x / hi)
end

-- --- SHA-1 (hand-rolled: LuaSec exposes no digest) ---------------------------

-- SHA-1 digest of a byte string, returned as 20 raw bytes.
local function sha1(msg)
  local h0, h1, h2, h3, h4 = 0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0
  local ml = #msg * 8
  msg = msg .. "\128"
  while #msg % 64 ~= 56 do
    msg = msg .. "\0"
  end
  local hi = math.floor(ml / 4294967296) % 4294967296
  local lo = ml % 4294967296
  msg = msg
    .. string.char(math.floor(hi / 16777216) % 256, math.floor(hi / 65536) % 256,
                   math.floor(hi / 256) % 256, hi % 256,
                   math.floor(lo / 16777216) % 256, math.floor(lo / 65536) % 256,
                   math.floor(lo / 256) % 256, lo % 256)
  for chunk = 1, #msg, 64 do
    local w = {}
    for i = 0, 15 do
      local a, b, c, d = string.byte(msg, chunk + i * 4, chunk + i * 4 + 3)
      w[i] = a * 16777216 + b * 65536 + c * 256 + d
    end
    for i = 16, 79 do
      w[i] = rotl32(bxor(bxor(bxor(w[i - 3], w[i - 8]), w[i - 14]), w[i - 16]), 1)
    end
    local a, b, c, d, e = h0, h1, h2, h3, h4
    for i = 0, 79 do
      local f, k
      if i < 20 then
        f = bor(band(b, c), band(0xFFFFFFFF - b, d))
        k = 0x5A827999
      elseif i < 40 then
        f = bxor(bxor(b, c), d)
        k = 0x6ED9EBA1
      elseif i < 60 then
        f = bor(bor(band(b, c), band(b, d)), band(c, d))
        k = 0x8F1BBCDC
      else
        f = bxor(bxor(b, c), d)
        k = 0xCA62C1D6
      end
      local temp = (rotl32(a, 5) + f + e + k + w[i]) % 0x100000000
      e, d, c, b, a = d, c, rotl32(b, 30), a, temp
    end
    h0 = (h0 + a) % 0x100000000
    h1 = (h1 + b) % 0x100000000
    h2 = (h2 + c) % 0x100000000
    h3 = (h3 + d) % 0x100000000
    h4 = (h4 + e) % 0x100000000
  end
  local function bytes(h)
    return string.char(math.floor(h / 16777216) % 256, math.floor(h / 65536) % 256,
                       math.floor(h / 256) % 256, h % 256)
  end
  return bytes(h0) .. bytes(h1) .. bytes(h2) .. bytes(h3) .. bytes(h4)
end

-- The Sec-WebSocket-Accept value for a client key (RFC 6455 §4.2.2):
-- base64(SHA1(key .. GUID)). Test vector: dGhlIHNhbXBsZSBub25jZQ== ->
-- s3pPLMBiTxaQ9kYGzzhZRbK+xOo=.
function M.compute_accept(key)
  return (mime.b64(sha1(key .. M.GUID)))
end

-- --- HTTP upgrade ------------------------------------------------------------

-- Validate an HTTP upgrade request (bytes through the blank line). Returns the
-- Sec-WebSocket-Key on success, or nil plus the HTTP status line to send back on
-- any violation. Host/Origin/Sec-WebSocket-Protocol/-Extensions are ignored: no
-- subprotocol or extension is negotiated, and auth is out of scope (§1.5).
function M.parse_upgrade(request)
  local lines = {}
  for line in (request .. "\r\n"):gmatch("(.-)\r\n") do
    lines[#lines + 1] = line
  end
  local method, _, version = (lines[1] or ""):match("^(%S+)%s+(%S+)%s+(%S+)$")
  if method ~= "GET" or not (version and version:match("^HTTP/1%.")) then
    return nil, "400 Bad Request"
  end
  local headers = {}
  for i = 2, #lines do
    local name, value = lines[i]:match("^([^:]+):%s*(.*)$")
    if name then
      headers[name:lower()] = value
    end
  end
  if (headers["upgrade"] or ""):lower() ~= "websocket" then
    return nil, "400 Bad Request"
  end
  local has_upgrade = false
  for token in (headers["connection"] or ""):gmatch("[^,]+") do
    if token:gsub("%s", ""):lower() == "upgrade" then
      has_upgrade = true
    end
  end
  if not has_upgrade then
    return nil, "400 Bad Request"
  end
  if (headers["sec-websocket-version"] or "") ~= "13" then
    return nil, "426 Upgrade Required"
  end
  local key = headers["sec-websocket-key"] or ""
  local decoded = mime.unb64(key)
  if not decoded or #decoded ~= 16 then
    return nil, "400 Bad Request"
  end
  return key
end

function M.success_response(key)
  return "HTTP/1.1 101 Switching Protocols\r\n"
    .. "Upgrade: websocket\r\n"
    .. "Connection: Upgrade\r\n"
    .. "Sec-WebSocket-Accept: " .. M.compute_accept(key) .. "\r\n"
    .. "\r\n"
end

function M.error_response(status)
  if status:sub(1, 3) == "426" then
    return "HTTP/1.1 " .. status .. "\r\nSec-WebSocket-Version: 13\r\n\r\n"
  end
  return "HTTP/1.1 " .. status .. "\r\n\r\n"
end

-- --- Frame layer -------------------------------------------------------------

-- Encode one server->client frame: FIN=1, RSV=0, unmasked, given opcode.
function M.encode(opcode, payload)
  local n = #payload
  local header
  -- TODO(frame-size): enforce a configurable maximum frame length
  if n < 126 then
    header = string.char(0x80 + opcode, n)
  elseif n < 65536 then
    header = string.char(0x80 + opcode, 126, math.floor(n / 256) % 256, n % 256)
  else
    header = string.char(0x80 + opcode, 127,
      math.floor(n / 2 ^ 56) % 256, math.floor(n / 2 ^ 48) % 256,
      math.floor(n / 2 ^ 40) % 256, math.floor(n / 2 ^ 32) % 256,
      math.floor(n / 2 ^ 24) % 256, math.floor(n / 2 ^ 16) % 256,
      math.floor(n / 256) % 256, n % 256)
  end
  return header .. payload
end

local function u16(code)
  return string.char(math.floor(code / 256) % 256, code % 256)
end

-- Try to parse one masked client frame from `buf`.
-- Returns (frame, consumed) on a complete frame, (nil) if more bytes are
-- needed, or (nil, nil, code) on a protocol error (close with `code`).
local function parse_frame(buf)
  if #buf < 2 then
    return nil
  end
  local b0, b1 = string.byte(buf, 1, 2)
  if math.floor(b0 / 16) % 8 ~= 0 then -- any RSV bit set
    return nil, nil, 1002
  end
  local fin = math.floor(b0 / 128) % 2
  local opcode = b0 % 16
  if math.floor(b1 / 128) % 2 ~= 1 then -- every client frame MUST be masked
    return nil, nil, 1002
  end
  local len = b1 % 128
  local offset = 2
  if len == 126 then
    if #buf < 4 then
      return nil
    end
    local a, b = string.byte(buf, 3, 4)
    len = a * 256 + b
    offset = 4
  elseif len == 127 then
    if #buf < 10 then
      return nil
    end
    -- TODO(frame-size): enforce a configurable maximum frame length
    local b3 = string.byte(buf, 3)
    if math.floor(b3 / 128) % 2 == 1 then -- 64-bit length MSB MUST be 0
      return nil, nil, 1002
    end
    len = 0
    for i = 3, 10 do
      len = len * 256 + string.byte(buf, i)
    end
    offset = 10
  end
  if opcode >= 0x8 and (fin ~= 1 or len > 125) then -- control frames: FIN=1, <=125
    return nil, nil, 1002
  end
  if #buf < offset + 4 + len then
    return nil
  end
  local mask = { string.byte(buf, offset + 1, offset + 4) }
  local masked = string.sub(buf, offset + 5, offset + 4 + len)
  local out = {}
  for i = 1, len do
    out[i] = string.char(bxor(string.byte(masked, i), mask[(i - 1) % 4 + 1]))
  end
  return { opcode = opcode, fin = fin, payload = table.concat(out) }, offset + 4 + len
end

-- A parser accumulates raw bytes across reads (WS frames may be split).
function M.new_parser()
  return { buf = "" }
end

-- Consume `raw` bytes. Returns:
--   payload : concatenation of binary/continuation payloads (the aiomsg stream);
--   control : bytes to send back to the client (pong and/or a close frame);
--   closed  : true if the connection must be torn down after flushing.
function M.ingest(state, raw)
  state.buf = state.buf .. raw
  local payload, control = {}, {}
  local closed = false
  while true do
    local frame, consumed, err = parse_frame(state.buf)
    if err then
      control[#control + 1] = M.encode(M.OP_CLOSE, u16(err))
      closed = true
      break
    end
    if not frame then
      break -- need more bytes
    end
    state.buf = string.sub(state.buf, consumed + 1)
    local op = frame.opcode
    if op == M.OP_BINARY or op == M.OP_CONT then
      payload[#payload + 1] = frame.payload
    elseif op == M.OP_PING then
      control[#control + 1] = M.encode(M.OP_PONG, frame.payload)
    elseif op == M.OP_PONG then
      -- ignore
    elseif op == M.OP_TEXT then
      control[#control + 1] = M.encode(M.OP_CLOSE, u16(1003)) -- text is not valid for aiomsg
      closed = true
      break
    elseif op == M.OP_CLOSE then
      local code = #frame.payload >= 2 and (string.byte(frame.payload, 1) * 256 + string.byte(frame.payload, 2)) or 1000
      control[#control + 1] = M.encode(M.OP_CLOSE, u16(code))
      closed = true
      break
    else
      control[#control + 1] = M.encode(M.OP_CLOSE, u16(1002)) -- unknown opcode
      closed = true
      break
    end
  end
  return table.concat(payload), table.concat(control), closed
end

return M
