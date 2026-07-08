// Public entry point for the aiomsg browser package.
//
// The Socket class is the whole public surface; SendMode/Delivery/Message are
// the value types you pass to and receive from it. The wire-protocol module and
// the WebSocket transport are internal and intentionally not re-exported —
// exactly as in javascript-lib, whose API this mirrors one-to-one minus bind().
export { Socket, SendMode, Delivery, Message } from "./socket.js";
