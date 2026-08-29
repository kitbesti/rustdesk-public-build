# How Mature Remote Desktop Protocols Handle Flow Control

Research into VNC/RFB and RDP, and what it implies for this codebase. Sources at the end.

The short version: **neither protocol infers congestion the way we do.** Both let the
*receiver* clock the sender — VNC by requesting each update, RDP by acknowledging each
frame. That is the mechanism we are missing, and having it removes the need for most of
the estimation machinery.

---

## 1. VNC / RFB — the client pulls, and that *is* the flow control

RFC 6143 is unambiguous:

> "The server must not send unsolicited updates. An update must only be sent in response
> to a request from the client."

The client sends `FramebufferUpdateRequest` with an `incremental` flag; the server replies
with exactly one `FramebufferUpdate`. Nothing moves until the client asks.

The spec then states the consequence directly:

> "The slower the client and the network are, the lower the rate of updates. With typical
> applications, changes to the same area of the framebuffer tend to happen soon after one
> another. With a slow client or network, transient states of the framebuffer can be
> ignored, resulting in less network traffic and less drawing for the client."

Three properties fall out of that, all for free:

- **Self-clocking.** A slow client requests less often, so it receives less. No bandwidth
  estimate, no congestion detector, no RTT inference — nothing to be wrong.
- **Natural frame dropping.** Intermediate states are simply never encoded. Skipping is
  the default behaviour, not an error path.
- **Per-client independence by construction.** Each client's own request rate paces its
  own stream. One slow viewer cannot affect another, and a viewer that stops asking
  stops receiving — a dead client costs nothing and needs no timeout to be detected.

That last point is precisely the flaw we identified in taking `min()` across users.

The reason VNC can drop so freely is that its encodings (Raw, RRE, Hextile, ZRLE, Tight)
are **independently decodable rectangles**. There is no GOP, so there is no such thing as
an unsafe cut point. We use inter-frame video codecs and therefore inherit a constraint
VNC does not have.

## 2. RDP — credit-based frame acknowledgement

RDP pushes rather than pulls, but it does not send blindly. MS-RDPEGFX wraps graphics in
logical frames (`RDPGFX_START_FRAME_PDU` / `RDPGFX_END_FRAME_PDU`), and the client returns
`RDPGFX_FRAME_ACKNOWLEDGE_PDU` for each. The server maintains an **Unacknowledged Frames**
list and limits how many may be outstanding.

This is classic credit-based flow control:

- the server knows exactly when each frame was actually decoded;
- outstanding-frame count is a *fact*, not an estimate;
- it degrades correctly — a client that stops acknowledging simply stops being sent to.

It also gives latency measurement for free, since the ack timing is end-to-end through the
decoder — which is the one thing the sender genuinely cannot observe on its own.

## 3. RDP does measure bandwidth — but not the way we did

Worth being precise, because it qualifies the "never measure" instinct.

MS-RDPBCGR defines both connect-time and **continuous** network characteristics detection:
`RDP_BW_START` / `RDP_BW_PAYLOAD` / `RDP_BW_STOP` for bandwidth, and `RDP_RTT_REQUEST` /
`RDP_RTT_RESPONSE` for round-trip time.

But note *how*:

- It sends an **explicit payload of known size** and measures how long it takes. That is a
  direct measurement, not an inference.
- It is used for **coarse, slow-moving decisions** — which visual features and codecs to
  enable — not per-frame rate control.
- Per-frame pacing is handled by the acknowledgement mechanism above.

Our mistake was the inverse on both counts: we *inferred* capacity from confounded signals
(RTT drift, blocked writes — themselves TCP's own backpressure), and then fed that estimate
into a **per-frame** control loop. That is the arrangement most likely to oscillate and to
throttle a healthy link.

## 4. What this means here

| Concern | VNC | RDP | RustDesk today |
|---|---|---|---|
| Primary flow control | client pull | frame acks + credit limit | **inferred congestion** |
| Per-client independence | inherent | inherent (per-channel acks) | **`min()` across users** |
| Frame dropping | free (independent rects) | codec-assisted | needs a key frame |
| Bandwidth measurement | none | explicit probes, coarse use | inferred, per-frame use |
| Input priority | separate messages | prioritised virtual channels | **shared TCP stream** |

The gap is not that we lack an estimator. It is that we lack the **receiver-driven signal**
both mature protocols are built on — and we substituted guesswork for it.

### The key realisation

**We already have the mechanism and only use it for web clients.**

`Misc::VideoReceived` is exactly RDP's frame acknowledgement. `Connection::video_unacked`
is exactly its Unacknowledged Frames list. Both exist, and both are currently gated behind
`video_ack_required`, which is set only for WebSocket peers because their sends never block.

Extending that to **every** client turns it into the primary flow control:

- **No estimation.** Outstanding-frame count is a fact.
- **Per-connection by construction.** Each peer's acks clock its own stream, so the `min()`
  across users disappears rather than being tuned — a slow, paused, or half-dead viewer
  throttles only itself and needs no timeout to be noticed.
- **Self-clocking like VNC.** Offer freely; the credit limit stops the queue forming, which
  is what the never-queue design was reaching for.
- **Real end-to-end latency**, measured through the peer's decoder — the number we actually
  care about and cannot otherwise see.

The GOP constraint remains ours alone, and the on-demand key-frame support added earlier is
the right counterpart to it: credit limiting decides *when* to stop sending, and the key
frame gives a safe place to resume.

## 5. Revised plan

Supersedes the ordering in [`QOS-ARCHITECTURE.md`](QOS-ARCHITECTURE.md) §7.

1. **Frame acknowledgement for all clients.** Drop the `video_ack_required` gate; cap
   outstanding frames per connection. This is the RDP design and it subsumes items 2 and 3
   of the old plan.
2. **Delete the inferred-congestion machinery** — capacity probing, congestion strikes,
   delay-banded fps. Once acks clock the stream, they have no job.
3. **Latency-bounded client queue**, so the ack reflects display time rather than receipt.
4. **Separate input transport** — RDP's prioritised channels are the precedent; this is the
   only thing that makes "operations always arrive" structural rather than best-effort.
5. **Resolution fallback** for the baseline guarantee. RDP's progressive codecs and
   TurboVNC's lossy-then-lossless refresh are the models: send coarse immediately, refine
   when there is room — the principled form of "blur during motion, sharpen when still".

### Compatibility note

`video_ack_required` comes from the peer's `LoginRequest`, so older clients will not send
acks. The credit limit must therefore be applied only when the peer advertises support,
with the current behaviour retained as the fallback — otherwise an old client would stall
after N frames forever.

## Sources

- [RFC 6143 — The Remote Framebuffer Protocol](https://www.rfc-editor.org/rfc/rfc6143.html)
- [MS-RDPEGFX — Unacknowledged Frames](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/5bd8ea57-60b9-4488-a21d-b93671102bbe)
- [MS-RDPEGFX — Server Implementation Requirements](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/7d40d7c2-5645-46a5-938d-d81c99c04f09)
- [MS-RDPBCGR — Connect-Time and Continuous Network Characteristics Detection](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/dc672839-4f4e-40b1-a71c-cd6a959baa38)
- [MS-RDPBCGR — Network Characteristics Detection](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/16ffa852-8aa7-481c-99a0-36c1a9a198f6)
- [RFB protocol overview](https://en.wikipedia.org/wiki/RFB_protocol)
