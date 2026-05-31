-- TLS configuration over LuaSec, from the PEM files used by the conformance
-- suite.
--
-- The bind side presents a certificate + private key; the connect side trusts a
-- self-signed CA (the same PEM file, which is its own trust anchor) and requires
-- the peer certificate to chain to it. LuaSec verifies the chain against
-- `cafile`; for a client we also send SNI when the target is a hostname (an IP
-- literal is left out, matching the certificate's IP SAN on loopback).

local ssl = require("ssl")

local M = {}

-- Params for a TLS server presenting the given certificate + key.
function M.server_params(cert_path, key_path)
  return {
    mode = "server",
    protocol = "any",
    certificate = cert_path,
    key = key_path,
    verify = "none",
    options = { "all" },
  }
end

-- Params for a TLS client trusting `ca_path` and verifying the peer name.
function M.client_params(ca_path, server_name)
  return {
    mode = "client",
    protocol = "any",
    cafile = ca_path,
    verify = "peer",
    options = { "all" },
    server_name = server_name,
  }
end

local function is_ip(name)
  return name and name:match("^%d+%.%d+%.%d+%.%d+$") ~= nil
end

-- Wrap a connected TCP socket in a TLS layer (handshake driven separately, in
-- non-blocking mode, by the reactor).
function M.wrap(sock, params)
  local conn = assert(ssl.wrap(sock, params))
  if params.mode == "client" and params.server_name and not is_ip(params.server_name) then
    pcall(function()
      conn:sni(params.server_name)
    end)
  end
  return conn
end

return M
