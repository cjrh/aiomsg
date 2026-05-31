-- aiomsg — native Lua smart sockets (a cooperative LuaSocket reactor).
--
-- A single Socket multiplexes many TCP connections behind one object, with
-- ZMQ-like distribution patterns (publish / round-robin / by-identity),
-- automatic reconnection, send buffering, heartbeating, an optional
-- at-least-once delivery guarantee, and TLS (LuaSec). It speaks the
-- language-independent aiomsg wire protocol (see ../PROTOCOL.md) and
-- interoperates on the wire with the Python reference and every other port.
--
-- Concurrency model. Lua has no threads, so — like every LuaSocket program of
-- any size — the socket owns a single cooperative reactor built on non-blocking
-- sockets and socket.select. There is no background execution: the reactor is
-- driven by the calls that wait on it. recv() pumps it until a message arrives;
-- run(seconds) pumps it for a fixed time (to keep connections alive, flush
-- sends, answer heartbeats and acks) without receiving. All connection
-- management — handshakes, heartbeats, dead-peer detection, reconnection,
-- at-least-once resends — happens inside that one loop.

local socket = require("socket")
local protocol = require("aiomsg.protocol")
local tls = require("aiomsg.tls")

local now = socket.gettime

math.randomseed(math.floor((now() * 1e6) % 2147483647))

local HEARTBEAT = 5.0
local TIMEOUT = 15.0
local HANDSHAKE_TIMEOUT = 15.0
local CONNECT_TIMEOUT = 1.0
local RECONNECT = 0.1
local RESEND = 5.0
local MAX_RETRIES = 5
local READ_CHUNK = 65536
local MAX_IDLE_TICK = 1.0 -- cap select() blocking so timers stay responsive

local M = {}

M.SendMode = { ROUND_ROBIN = "round-robin", PUBLISH = "publish" }
M.Delivery = { AT_MOST_ONCE = "at-most-once", AT_LEAST_ONCE = "at-least-once" }

local function random_bytes(n)
  local chars = {}
  for i = 1, n do
    chars[i] = string.char(math.random(0, 255))
  end
  return table.concat(chars)
end

local function to_hex(bytes)
  return (bytes:gsub(".", function(c)
    return string.format("%02x", string.byte(c))
  end))
end

local Socket = {}
Socket.__index = Socket
M.Socket = Socket

-- opts = {send_mode=, delivery=, identity=}
function Socket.new(opts)
  opts = opts or {}
  local self = setmetatable({}, Socket)
  self.send_mode = opts.send_mode or M.SendMode.ROUND_ROBIN
  self.delivery = opts.delivery or M.Delivery.AT_MOST_ONCE
  self.id = opts.identity or random_bytes(protocol.IDENTITY_SIZE)

  self.conns = {} -- active (post-handshake) connections, in round-robin order
  self.rr = 0
  self.buffer = {} -- {target=, data=} queued while no peer is connected
  self.pending = {} -- hex(msg_id) -> {target=, data=, retries=, deadline=}

  self.servers = {} -- listening sockets -> tls params or false
  self.connectors = {} -- {host=, port=, tls=, conn=, retry_at=}
  self.all_conns = {} -- every connection object, active or handshaking
  self.inbox = {} -- {data=, sender=} delivered to the application
  self.closed = false
  return self
end

function Socket:identity()
  return self.id
end

-- --- bind / connect ----------------------------------------------------

-- tls_params (optional): {cert=path, key=path} to listen for TLS peers.
function Socket:bind(host, port, tls_params)
  local server, err = socket.bind(host, port)
  if not server then
    error("bind failed: " .. tostring(err))
  end
  server:settimeout(0)
  self.servers[server] = tls_params and tls.server_params(tls_params.cert, tls_params.key) or false
  return self
end

-- tls_params (optional): {ca=path, server_name=name} to connect over TLS.
function Socket:connect(host, port, tls_params)
  local params = nil
  if tls_params then
    local name = tls_params.server_name
    if name == nil or name == "" then
      name = host
    end
    params = tls.client_params(tls_params.ca, name)
  end
  self.connectors[#self.connectors + 1] =
    { host = host, port = port, tls = params, conn = nil, retry_at = now() }
  return self
end

-- --- send / receive ----------------------------------------------------

function Socket:send(data)
  self:_dispatch(nil, data)
  self:_step(0) -- make progress without blocking
end

function Socket:send_to(identity, data)
  self:_dispatch(identity, data)
  self:_step(0)
end

-- Block (pumping the reactor) until the next message; returns data, sender.
-- With a timeout (seconds) returns nil on expiry. Returns nil once closed.
function Socket:recv(timeout)
  local deadline = timeout and (now() + timeout) or nil
  while true do
    if #self.inbox > 0 then
      local message = table.remove(self.inbox, 1)
      return message.data, message.sender
    end
    if self.closed then
      return nil
    end
    local tick = self:_next_timeout()
    if deadline then
      local remaining = deadline - now()
      if remaining <= 0 then
        return nil
      end
      tick = math.min(tick, remaining)
    end
    self:_step(tick)
  end
end

-- Iterator over incoming messages: `for data, sender in sock:messages() do`.
function Socket:messages()
  return function()
    return self:recv()
  end
end

-- Pump the reactor for `seconds` (or forever if nil) without receiving. Use to
-- keep connections alive long enough to flush sends and answer acks.
function Socket:run(seconds)
  local deadline = seconds and (now() + seconds) or nil
  while not self.closed do
    local tick = self:_next_timeout()
    if deadline then
      local remaining = deadline - now()
      if remaining <= 0 then
        return
      end
      tick = math.min(tick, remaining)
    end
    self:_step(tick)
  end
end

-- Advance the reactor by a single turn, blocking at most `timeout` seconds (0 =
-- non-blocking). Useful for driving more than one socket from one thread; recv()
-- and run() are the usual single-socket drivers.
function Socket:poll(timeout)
  self:_step(timeout or 0)
end

function Socket:close()
  if self.closed then
    return
  end
  self.closed = true
  for server in pairs(self.servers) do
    server:close()
  end
  for _, conn in ipairs(self.all_conns) do
    pcall(function()
      conn.sock:close()
    end)
  end
end

-- --- broker (routing, buffering, delivery, at-least-once) --------------

local function find_conn(self, identity)
  for _, conn in ipairs(self.conns) do
    if conn.peer == identity then
      return conn
    end
  end
  return nil
end

local function queue_frame(conn, framed)
  conn.outbuf = conn.outbuf .. framed
  conn.last_send = now() -- any real frame resets the heartbeat interval
end

function Socket:_route(target, framed)
  if self.closed or #self.conns == 0 then
    return
  end
  if target then
    local conn = find_conn(self, target) -- by-identity: drop if peer is gone
    if conn then
      queue_frame(conn, framed)
    end
  elseif self.send_mode == M.SendMode.PUBLISH then
    for _, conn in ipairs(self.conns) do
      queue_frame(conn, framed)
    end
  else
    self.rr = self.rr % #self.conns + 1
    queue_frame(self.conns[self.rr], framed)
  end
end

function Socket:_transmit(target, data, retries)
  local at_least_once = self.delivery == M.Delivery.AT_LEAST_ONCE
    and (target ~= nil or self.send_mode == M.SendMode.ROUND_ROBIN)
  if not at_least_once then
    self:_route(target, protocol.frame_data(data))
    return
  end
  local msg_id = random_bytes(protocol.MSG_ID_SIZE)
  self.pending[to_hex(msg_id)] =
    { target = target, data = data, retries = retries, deadline = now() + RESEND }
  self:_route(target, protocol.frame_data_req(msg_id, data))
end

function Socket:_dispatch(target, data)
  if self.closed then
    return
  end
  if #self.conns == 0 then
    self.buffer[#self.buffer + 1] = { target = target, data = data }
  else
    self:_transmit(target, data, MAX_RETRIES)
  end
end

function Socket:_flush_buffer()
  local queued = self.buffer
  self.buffer = {}
  for _, item in ipairs(queued) do
    self:_dispatch(item.target, item.data)
  end
end

function Socket:_received(sender, envelope)
  if envelope.type == protocol.TYPE.DATA then
    self.inbox[#self.inbox + 1] = { data = envelope.payload, sender = sender }
  elseif envelope.type == protocol.TYPE.DATA_REQ then
    self.inbox[#self.inbox + 1] = { data = envelope.payload, sender = sender }
    self:_route(sender, protocol.frame_ack(envelope.msg_id)) -- ack on same conn
  elseif envelope.type == protocol.TYPE.ACK then
    self.pending[to_hex(envelope.msg_id)] = nil
  end
end

function Socket:_sweep()
  local current = now()
  for key, p in pairs(self.pending) do
    if p.deadline <= current then
      self.pending[key] = nil
      if p.retries > 0 then
        if #self.conns == 0 then
          self.buffer[#self.buffer + 1] = { target = p.target, data = p.data }
        else
          self:_transmit(p.target, p.data, p.retries - 1)
        end
      end
    end
  end
end

-- --- connection lifecycle ---------------------------------------------

-- Create a connection object wrapping a freshly-accepted or freshly-connected
-- non-blocking socket. `connector` is set for client connections (for reconnect).
local function new_conn(sock, is_server, connector)
  return {
    sock = sock,
    is_server = is_server,
    connector = connector,
    state = "connecting", -- connecting -> tls -> open
    peer = nil,
    handshaked = false,
    decoder = protocol.Decoder.new(),
    outbuf = "",
    last_send = now(),
    last_recv = now(),
    deadline = now() + CONNECT_TIMEOUT,
  }
end

function Socket:_start_connect(connector)
  local sock = socket.tcp()
  sock:settimeout(0)
  local conn = new_conn(sock, false, connector)
  connector.conn = conn
  self.all_conns[#self.all_conns + 1] = conn
  -- Non-blocking connect returns immediately; completion shows up as writable.
  sock:connect(connector.host, connector.port)
end

function Socket:_accept(server, tls_params)
  local client = server:accept()
  if not client then
    return
  end
  client:settimeout(0)
  local conn = new_conn(client, true, nil)
  self.all_conns[#self.all_conns + 1] = conn
  if tls_params then
    conn.tls = tls_params
    conn.sock = tls.wrap(client, tls_params)
    conn.sock:settimeout(0)
    conn.state = "tls"
  else
    conn.state = "open"
    queue_frame(conn, protocol.frame_hello(self.id)) -- our half of the handshake
  end
end

-- A client TCP connect has completed (socket is writable): verify it, then
-- either start the TLS handshake or send HELLO.
function Socket:_connected(conn)
  if not conn.sock:getpeername() then
    self:_drop(conn) -- connect failed
    return
  end
  if conn.connector.tls then
    conn.tls = conn.connector.tls
    conn.sock = tls.wrap(conn.sock, conn.tls)
    conn.sock:settimeout(0)
    conn.state = "tls"
    conn.deadline = now() + HANDSHAKE_TIMEOUT
  else
    conn.state = "open"
    conn.deadline = now() + HANDSHAKE_TIMEOUT
    queue_frame(conn, protocol.frame_hello(self.id))
  end
end

-- Drive a non-blocking TLS handshake one step; returns when it can't progress.
function Socket:_tls_step(conn)
  local ok, err = conn.sock:dohandshake()
  if ok then
    conn.state = "open"
    conn.deadline = now() + HANDSHAKE_TIMEOUT
    queue_frame(conn, protocol.frame_hello(self.id))
  elseif err ~= "wantread" and err ~= "wantwrite" and err ~= "timeout" then
    self:_drop(conn) -- handshake failed
  end
end

-- Read whatever bytes are available on a non-blocking socket. Returns the bytes
-- (possibly "") and whether the peer closed the connection.
local function read_available(sock)
  local parts = {}
  while true do
    local chunk, err, partial = sock:receive(READ_CHUNK)
    if chunk then
      parts[#parts + 1] = chunk
    else
      if partial and #partial > 0 then
        parts[#parts + 1] = partial
      end
      if err == "closed" then
        return table.concat(parts), true
      end
      return table.concat(parts), false -- timeout / wantread: nothing more now
    end
  end
end

function Socket:_on_readable(conn)
  local data, peer_closed = read_available(conn.sock)
  if #data > 0 then
    conn.last_recv = now()
    conn.decoder:push(data)
    while true do
      local envelope = conn.decoder:pop()
      if not envelope then
        break
      end
      local parsed = protocol.parse_envelope(envelope)
      if parsed then
        if not conn.handshaked then
          if not self:_complete_handshake(conn, parsed) then
            return
          end
        elseif parsed.type ~= protocol.TYPE.HEARTBEAT then
          self:_received(conn.peer, parsed)
        end
      end
    end
  end
  if peer_closed then
    self:_drop(conn)
  end
end

-- Validate the peer's HELLO and register the connection, or drop it.
function Socket:_complete_handshake(conn, envelope)
  if envelope.type ~= protocol.TYPE.HELLO or envelope.version ~= protocol.VERSION then
    self:_drop(conn)
    return false
  end
  if find_conn(self, envelope.identity) then
    self:_drop(conn) -- duplicate identity: keep the existing connection
    return false
  end
  conn.peer = envelope.identity
  conn.handshaked = true
  self.conns[#self.conns + 1] = conn
  self:_flush_buffer()
  return true
end

-- Flush as much of conn.outbuf as the socket will take right now. LuaSocket's
-- send returns the index of the last byte sent on success, or nil,err,last on a
-- partial write; either way we trim exactly what went out and keep the rest for
-- the next writable event.
function Socket:_on_writable(conn)
  if #conn.outbuf == 0 then
    return
  end
  local i, err, last = conn.sock:send(conn.outbuf)
  local sent = i or last or 0
  conn.outbuf = string.sub(conn.outbuf, sent + 1)
  if err == "closed" then
    self:_drop(conn)
  end
end

local function remove_from(list, item)
  for i, value in ipairs(list) do
    if value == item then
      table.remove(list, i)
      return
    end
  end
end

function Socket:_drop(conn)
  if conn.dropped then
    return
  end
  conn.dropped = true
  pcall(function()
    conn.sock:close()
  end)
  remove_from(self.conns, conn)
  remove_from(self.all_conns, conn)
  if conn.connector then
    conn.connector.conn = nil
    conn.connector.retry_at = now() + RECONNECT
  end
end

-- Smallest delay until something needs doing (heartbeat, timeout, resend,
-- reconnect), so select() never sleeps past a due timer.
function Socket:_next_timeout()
  local tick = MAX_IDLE_TICK
  local current = now()
  for _, conn in ipairs(self.conns) do
    tick = math.min(tick, (conn.last_send + HEARTBEAT) - current)
    tick = math.min(tick, (conn.last_recv + TIMEOUT) - current)
  end
  for _, conn in ipairs(self.all_conns) do
    if conn.state ~= "open" or not conn.handshaked then
      tick = math.min(tick, conn.deadline - current)
    end
  end
  for _, connector in ipairs(self.connectors) do
    if not connector.conn then
      tick = math.min(tick, connector.retry_at - current)
    end
  end
  if next(self.pending) then
    tick = math.min(tick, RESEND)
  end
  return math.max(tick, 0)
end

-- A shallow copy, so handlers can drop connections while we iterate.
local function copy_list(list)
  local out = {}
  for i, value in ipairs(list) do
    out[i] = value
  end
  return out
end

-- One turn of the reactor: start due (re)connects, select on every socket for
-- up to `timeout` seconds, service whatever is ready, then run the timers.
function Socket:_step(timeout)
  local current = now()

  -- Start any pending (re)connections whose backoff has elapsed.
  if not self.closed then
    for _, connector in ipairs(self.connectors) do
      if not connector.conn and current >= connector.retry_at then
        self:_start_connect(connector)
      end
    end
  end

  -- Build the select sets and a reverse map from socket back to its connection.
  local readers, writers, conn_by_sock = {}, {}, {}
  for server in pairs(self.servers) do
    readers[#readers + 1] = server
  end
  for _, conn in ipairs(self.all_conns) do
    conn_by_sock[conn.sock] = conn
    if conn.state == "connecting" then
      writers[#writers + 1] = conn.sock
    elseif conn.state == "tls" then
      readers[#readers + 1] = conn.sock
      writers[#writers + 1] = conn.sock
    else -- open
      readers[#readers + 1] = conn.sock
      if #conn.outbuf > 0 then
        writers[#writers + 1] = conn.sock
      end
    end
  end

  if #readers == 0 and #writers == 0 then
    if timeout > 0 then
      socket.sleep(timeout)
    end
  else
    local readable, writable = socket.select(readers, writers, timeout)
    for _, sock in ipairs(writable) do
      local conn = conn_by_sock[sock]
      if conn and not conn.dropped then
        if conn.state == "connecting" then
          self:_connected(conn)
        elseif conn.state == "tls" then
          self:_tls_step(conn)
        end
        if conn.state == "open" and not conn.dropped then
          self:_on_writable(conn)
        end
      end
    end
    for _, sock in ipairs(readable) do
      local conn = conn_by_sock[sock]
      if conn then
        if not conn.dropped and conn.state == "tls" then
          self:_tls_step(conn)
        end
        if not conn.dropped and conn.state == "open" then
          self:_on_readable(conn)
        end
      else
        self:_accept(sock, self.servers[sock]) -- a listening server
      end
    end
  end

  self:_process_timers()
end

-- Heartbeats on send-idle connections, dead-peer detection on receive-idle ones,
-- handshake/connect deadlines, and at-least-once resends.
function Socket:_process_timers()
  local current = now()
  for _, conn in ipairs(copy_list(self.all_conns)) do
    if (conn.state ~= "open" or not conn.handshaked) and current > conn.deadline then
      self:_drop(conn)
    end
  end
  for _, conn in ipairs(copy_list(self.conns)) do
    if current - conn.last_recv >= TIMEOUT then
      self:_drop(conn)
    elseif current - conn.last_send >= HEARTBEAT then
      queue_frame(conn, protocol.frame_heartbeat())
    end
  end
  self:_sweep()
end

return M
