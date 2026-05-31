# aiomsg wire protocol — v1

This is the single source of truth for the bytes on the wire. Any
implementation, in any language, that follows this document will interoperate
with any other. It supersedes the provisional protocol notes that previously
lived in the README.

The protocol is deliberately tiny. Most of aiomsg's behaviour (send modes,
buffering, reconnection, delivery guarantees) is **local** to a socket and is
*not* signalled on the wire. Only the items in this document cross the network.

---

## 1. Transport

- TCP. Optionally wrapped in TLS (the protocol is identical either way; TLS is
  transparent below this layer).
- The protocol is fully symmetric: the same rules apply to the **bind** end and
  the **connect** end, and to every individual connection a socket holds.

---

## 2. Framing

Every message on the wire is length-prefixed:

```
frame = LENGTH || ENVELOPE
LENGTH   = u32, big-endian, = byte length of ENVELOPE
ENVELOPE = LENGTH bytes
```

A reader MUST read exactly 4 bytes for `LENGTH`, then exactly `LENGTH` bytes for
the envelope. A clean EOF before/between frames means the connection closed.

There is no maximum frame size mandated by the protocol; implementations MAY
impose a configurable limit and close a connection that violates it.

---

## 3. Envelope

The first byte of every envelope is the **type**. The remaining bytes are the
type-specific body.

```
ENVELOPE = TYPE || BODY
TYPE = u8
```

| TYPE | Name        | BODY                                  | Delivered to app? |
|------|-------------|---------------------------------------|-------------------|
| 0x01 | `HELLO`     | `VERSION(u8)` `IDENTITY(16 bytes)`    | no                |
| 0x02 | `HEARTBEAT` | *(empty)*                             | no                |
| 0x03 | `DATA`      | `PAYLOAD(… bytes)`                     | yes (PAYLOAD)     |
| 0x04 | `DATA_REQ`  | `MSG_ID(16 bytes)` `PAYLOAD(… bytes)`  | yes (PAYLOAD)     |
| 0x05 | `ACK`       | `MSG_ID(16 bytes)`                    | no                |

Because application data is always carried inside a `DATA` / `DATA_REQ`
envelope, payload bytes can never be confused with a control frame. All
fixed-width fields are positional; parsing never depends on payload contents.

- `VERSION` — protocol version. This document defines version `0x01`.
- `IDENTITY` — 16 bytes uniquely identifying the *socket* (not the connection).
  A UUIDv4's 16 raw bytes are the canonical choice. The identity is stable for
  the life of a socket and is reused across reconnects.
- `MSG_ID` — 16 bytes uniquely identifying a single `AT_LEAST_ONCE` message
  (UUIDv4 raw bytes). Echoed back unchanged in the matching `ACK`.

Unknown `TYPE` values: an implementation MUST ignore (skip) a frame whose type
it does not recognise, rather than crashing, to allow forward-compatible
extensions. (It has already consumed the whole frame via `LENGTH`.)

---

## 4. Handshake

Immediately after the TCP/TLS connection is established, **before any other
frame**, both ends perform a symmetric identity exchange:

1. Send a `HELLO` frame: `TYPE=0x01`, `VERSION=0x01`, `IDENTITY=<my 16 bytes>`.
2. Read the peer's `HELLO`.

Validation on the received `HELLO`:

- If `VERSION` is not supported by this implementation → log and **close** the
  connection.
- If the peer's `IDENTITY` is **already connected** to this socket → reject the
  duplicate: **close** this new connection and keep the existing one.

Only after a successful exchange does the connection become active and eligible
for `DATA`/`DATA_REQ`/`HEARTBEAT`/`ACK` traffic.

---

## 5. Heartbeat

- An active connection MUST send a `HEARTBEAT` frame after `HEARTBEAT_INTERVAL`
  of **send inactivity** (i.e. if it would otherwise have sent nothing during
  that window). Any real frame sent resets the interval.
- If a connection receives **no frame of any type** within `HEARTBEAT_TIMEOUT`,
  it MUST consider the connection dead and tear it down.
- Received `HEARTBEAT` frames are otherwise ignored (they are not delivered to
  the application; they only reset the receive timeout).

Defaults: `HEARTBEAT_INTERVAL = 5s`, `HEARTBEAT_TIMEOUT = 15s`. These are fixed
defaults in v1 (not negotiated in `HELLO`); implementations MAY expose local
overrides but interoperating peers should use compatible values.

After tear-down, the **connect** end is responsible for reconnecting (§7).

---

## 6. Delivery guarantee

Delivery guarantee is chosen by the **sending** socket; it is not negotiated.

### AT_MOST_ONCE (default)
- Application messages are sent as `DATA` frames. No acknowledgement, no resend.
- A message may be lost (e.g. process dies after the kernel buffers it).

### AT_LEAST_ONCE
- Application messages are sent as `DATA_REQ` frames, each with a fresh 16-byte
  `MSG_ID`. The sender keeps the message pending and starts a resend timer
  (default 5s).
- A receiver that gets a `DATA_REQ` MUST:
  1. deliver `PAYLOAD` to its application, and
  2. send an `ACK` carrying the same `MSG_ID` back over the **same connection**
     the `DATA_REQ` arrived on.
- On receiving the matching `ACK`, the sender cancels the pending resend. On
  timeout, it resends (bounded retry count). `ACK` frames are never delivered to
  the application.
- **AT_LEAST_ONCE is valid only for `ROUNDROBIN` and by-identity sends. It is
  NOT supported for `PUBLISH`.**
- Ordering is **not** guaranteed under AT_LEAST_ONCE: a resend can arrive after
  later messages. The guarantee is delivery one-or-more times, not order.
- Retry state is in memory only; if both ends die, pending retries are lost.

Every conformant implementation MUST be able to *reply* with `ACK` to a
`DATA_REQ`, even if it never *sends* in AT_LEAST_ONCE mode itself.

---

## 7. Connection management (local behaviour, normative where it affects peers)

These behaviours are local to a socket but are specified here because they
determine observable on-wire behaviour.

- **Multiplexing.** A socket may hold many connections at once (a bind socket
  from many peers; a connect socket via many `connect()` calls). Received
  messages from all connections are merged into one receive stream, tagged with
  the sender's `IDENTITY`.
- **Send modes** (local routing decisions, never signalled):
  - `PUBLISH` — write the message to every connected peer.
  - `ROUNDROBIN` — write the message to one peer, cycling on each send.
  - by-identity — write directly to the peer with the given `IDENTITY`,
    regardless of send mode.
- **Buffering.** If `send()` is called with no connected peers, the message is
  buffered and flushed once a peer connects. Applies to both send modes.
- **Reconnection.** The **connect** end runs a reconnect loop for the life of
  the socket: on disconnect it waits `reconnection_delay()` (a user-supplied
  strategy, default ~0.1s; add jitter/backoff to stagger fleets) and retries.
  The bind end simply waits for peers to reconnect.

---

## 8. Constants summary

| Name | Value |
|------|-------|
| Protocol version | `0x01` |
| `LENGTH` prefix | `u32` big-endian |
| `IDENTITY` width | 16 bytes |
| `MSG_ID` width | 16 bytes |
| `HELLO` | `0x01` |
| `HEARTBEAT` | `0x02` |
| `DATA` | `0x03` |
| `DATA_REQ` | `0x04` |
| `ACK` | `0x05` |
| `HEARTBEAT_INTERVAL` | 5s |
| `HEARTBEAT_TIMEOUT` | 15s |
| AT_LEAST_ONCE resend timeout | 5s |

---

## 9. Conformance checklist

An implementation conforms to aiomsg protocol v1 if it:

1. Length-prefixes every frame with a big-endian `u32`.
2. Emits and accepts the typed envelope of §3, and ignores unknown types.
3. Performs the symmetric `HELLO` handshake (§4) with version check and
   identity de-duplication.
4. Sends heartbeats on send-idle and tears down on receive-timeout (§5).
5. Multiplexes many connections behind one socket and merges receives (§7).
6. Implements `PUBLISH`, `ROUNDROBIN`, and by-identity routing locally (§7).
7. Buffers sends when no peers are connected, and (on the connect end)
   auto-reconnects (§7).
8. Replies with `ACK` to every `DATA_REQ`, and implements sender-side resend for
   AT_LEAST_ONCE (§6), rejecting the AT_LEAST_ONCE + PUBLISH combination.
