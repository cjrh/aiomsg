// Public entry point for the aiomsg JavaScript implementation.
//
// The Socket class is the whole public surface; SendMode/Delivery/Message are
// the value types you pass to and receive from it. The wire-protocol module is
// internal and intentionally not re-exported.
export { Socket, SendMode, Delivery, Message } from "./socket.js";
