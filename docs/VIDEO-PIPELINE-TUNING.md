# Interactive Video Pipeline — Tuning Notes

See [`QOS-ARCHITECTURE.md`](QOS-ARCHITECTURE.md) for the intended design, the
server/client responsibility split, and the known gaps. This document is the history:
why the video path behaves the way it does, what went wrong under multiple clients and
poor networks, and how it is fixed. Read this before changing frame-rate or send-path
behaviour.

## Background: the fixed-60fps change

An earlier change pinned the interactive pipeline to a fixed 60fps to make motion
smoother. It did four things:

1. `video_qos.rs` — deleted all adaptive frame-rate logic; `adjust_fps()` became a
   no-op and `fps()` returned the constant `60`.
2. `video_service.rs` — removed `VideoFrameController`, the per-frame wait that stopped
   the capture loop running ahead of what clients had acknowledged.
3. `connection.rs` — the send loop drained its channel and kept only the **newest**
   video frame, discarding the rest.
4. `client/io_loop.rs` — stopped reporting decode capability back to the server.

On a fast LAN with one client this is genuinely smoother. Under multiple clients or a
poor link it produced the reported symptoms, for the reasons below.

## What was actually wrong

### Operations appearing not to take effect

**Discarded inter-coded frames corrupt the decoder.** Every frame except a key frame is
coded as a delta against its predecessor. Dropping the middle of that chain leaves the
peer's decoder predicting from a picture it never received, so everything afterwards
decodes into garbage until the next key frame. The screen stops reflecting reality, so
clicks and keystrokes look like they did nothing.

**The connection task blocks on video writes while input goes unread.** The video send
and the peer's inbound message read live in the *same* `tokio::select!`. Once the video
branch is chosen it runs to completion, so a blocked `stream.send()` stops the loop
reading mouse and keyboard events for as long as it lasts — up to `SEND_TIMEOUT_VIDEO`
(12s). With the capture loop pushing 60fps into an unbounded channel regardless of what
the socket could absorb, blocking became the normal state on a weak link rather than
the exception.

### Speed not reaching its potential with bandwidth free

Two separate causes, both of which had to be fixed.

**The bitrate budget did not scale with frame rate.** This is the big one.
`scrap::codec::base_bitrate()` maps *resolution* to a kbps figure, and the encoder is
configured with `base_bitrate(w, h) * ratio`. Frame rate is not an input. At 1080p
Balanced that is `2073 * 0.67 ≈ 1389 kbps` whether the pipeline sends 30 frames a
second or 60.

So pinning the rate to 60fps halved the bits available per frame while asking for no
extra bandwidth at all. The encoder dutifully squeezed twice as many frames into the
same budget, the picture got softer, and the link stayed half empty — exactly "the
speed cannot reach its full potential even when bandwidth is still available".

The fix is `fps_bitrate_scale()`: the budget is now multiplied by
`sqrt(fps / REFERENCE_FPS)`, clamped to `[0.7, 1.6]`. Bits required grow with roughly
the square root of frame rate rather than linearly, because consecutive frames resemble
each other more the closer together they are. At 60fps that is ~1.41x, taking 1080p
Balanced from 1389 kbps to ~1963 kbps. Because the factor is folded into
`target_ratio()`, the ABR controller's ceiling rises with it and it *climbs into* the
new headroom as the link proves it can take it — the extra bandwidth is earned, not
assumed. It also compounds correctly in the other direction: congestion lowers the
frame rate, which lowers the budget.

**The key-frame interval was ~0.3 seconds.** `INTERACTIVE_KEYFRAME_INTERVAL` was
`FPS * 3 / 10` = 18 frames, i.e. three key frames per second at 60fps. A key frame
costs many times what an inter-coded frame costs, so most of the bitrate budget went on
redundant full pictures. The intent was evidently three *seconds*; the `/10` was a bug.

### Multi-client behaviour

One encoder feeds every subscriber. With no adaptation, running at the rate the
*fastest* peer could take meant the slowest peer's queue grew without limit until
frames had to be thrown away — corrupting that peer's stream while helping nobody.

### A memory leak

`notify_video_frame_fetched()` pushed a tuple into an unbounded channel for every frame
of every connection. Its only consumer was `VideoFrameController::try_wait_next`, and
the controller was no longer constructed. Nothing ever drained the channel, so it grew
for the life of the process — roughly 60 entries per second per client.

## How it works now

### Adaptive rate with a 60fps ceiling (`video_qos.rs`)

`FPS = 60` is now a **target and ceiling**, not a constant. On a link that keeps up,
60fps is what runs, so the smoothness intent is preserved. Each QoS tick recomputes the
target as the minimum across all users of:

- what the measured **queuing delay** supports (`fps_for_delay`, banded from 60fps under
  30 ms down to 5fps past 600 ms) — delay minus RTT, so it tracks the queue building in
  front of us rather than unavoidable path latency;
- what the **congestion** state supports — each unresolved strike halves the ceiling;
- any **client-declared cap** (`custom_fps`, `auto_adjust_fps`), treated strictly as a
  ceiling, never a floor.

The result is applied asymmetrically: **drops take effect immediately** so a degrading
link sheds load at once, while **recovery ramps** in `FPS_INCREASE_STEP` (6fps)
increments per tick. Ticks arrive at least once a second, so a session reaches the full
target within a few seconds of the link proving itself. Adaptation never falls below
`ADAPT_MIN_FPS` (5) — below that the bitrate saved is negligible and the session stops
feeling interactive. A new connection resets the rate to `INIT_FPS` (30) so a second
peer joining an established session is not immediately fed the full rate.

### Congestion detection (`connection.rs`)

A delay probe only measures the round trip of a small message; on a merely
bandwidth-starved link it stays healthy while the send queue grows. The send loop
therefore reports congestion directly, from three signals:

- a video write that took longer than `VIDEO_SEND_CONGESTED_MS` (120 ms);
- a backlog of `VIDEO_BACKLOG_CONGESTED` (3) or more queued frames;
- for ack-required (web) peers, `VIDEO_UNACKED_CONGESTED` (4) or more unconfirmed
  frames.

Congestion is reported immediately; the all-clear is rate-limited to once a second so
recovery stays gradual. A `Congestion` strike decays rather than clearing outright, so
one frame that happened to fit does not undo the whole back-off.

Web clients need the separate ack count because a WebSocket send completes into a
userspace buffer — a backed-up web client never makes our write block the way a raw TCP
peer does.

### Key-frame-safe dropping

Frames are **never** discarded merely because newer ones exist. Only once the backlog
passes `VIDEO_BACKLOG_RESYNC` (30 frames, ~0.5s) does the send loop discard — and then
only as far as the most recent key frame in the batch, which is the one place a decoder
can safely resume. If no key frame is available the backlog is kept intact and the
source rate is left to back off: corrupt video is worse than late video.

### Client decode-capability reporting (`client/io_loop.rs`)

The server can detect a slow *link* on its own, but not a slow *decoder*: the client
keeps draining the socket promptly, so the server's writes never block and the excess
piles up client-side as display latency. Once a second the client reports its raw decode
rate when its queue keeps refilling, and releases the cap in a single step when it does
not.

This is deliberately simpler than the original implementation, which derived a fraction
of the decode rate and then crawled back upward one frame at a time — that double
damping (client *and* server) is what used to leave sessions pinned well below what the
link could carry. The client now reports raw capability and the server owns all
smoothing.

### One message per turn (input delivery)

Video sending shares a `select!` with the inbound read, and a write blocks until the
socket accepts the frame. The send loop therefore writes **exactly one** message per
turn — `PendingVideo::take_turn()` — and hands control back to the select. The queue
persists across turns, so a backlog drains one frame at a time with an opportunity to
read the peer's input between each.

That bounds input latency to a *single* video write regardless of backlog depth.
Previously the loop drained the whole batch, so a 30-frame backlog meant input waited
for 30 sequential writes. `next_video()` keeps the branch ready whenever work is
queued, so the backlog still drains at full speed; it just yields between frames.

The residual limitation is that one individual write can still block for up to
`SEND_TIMEOUT_VIDEO`. Removing that entirely means giving the video path its own task
with a split stream sink, which would have to be done in `hbb_common::Stream` across
the TCP, WebSocket and WebRTC variants (the encryption state is shared between the read
and write halves). That is a larger change than this work covers, and with the queue
now kept short by the adaptive rate, single blocking writes are rare.

## Constants worth knowing

| Constant | File | Value | Meaning |
|---|---|---|---|
| `FPS` | `video_qos.rs` | 60 | Interactive target and ceiling |
| `ADAPT_MIN_FPS` | `video_qos.rs` | 5 | Adaptation floor |
| `INIT_FPS` | `video_qos.rs` | 30 | Starting rate before measurements exist |
| `FPS_INCREASE_STEP` | `video_qos.rs` | 6 | Per-tick recovery step |
| `CONGESTION_HOLD` | `video_qos.rs` | 2s | How long a strike suppresses the rate |
| `VIDEO_SEND_CONGESTED_MS` | `connection.rs` | 120 | Blocked-write threshold |
| `VIDEO_BACKLOG_CONGESTED` | `connection.rs` | 3 | Queued frames meaning "behind" |
| `VIDEO_BACKLOG_RESYNC` | `connection.rs` | 30 | Backlog forcing a key-frame resync |
| `VIDEO_UNACKED_CONGESTED` | `connection.rs` | 4 | Unconfirmed frames for web peers |
| `INTERACTIVE_KEYFRAME_INTERVAL` | `video_service.rs` | `FPS * 3` | ~3s between key frames |
| `REFERENCE_FPS` | `video_qos.rs` | 30 | Frame rate the bitrate presets are calibrated for |
| `MIN/MAX_FPS_BITRATE_SCALE` | `video_qos.rs` | 0.7 / 1.6 | Bounds on frame-rate bitrate compensation |

## Tests

```bash
cargo test --lib --features hwcodec video_qos         # controller + link simulation
cargo test --lib --features hwcodec video_send_path   # send path / input delivery
```

Both run in well under a second and need no network, no root and no display.

### Closed-loop link simulation

The controller's deadlines (congestion hold, ABR interval) are wall-clock durations,
which would make it testable only in real time. They are routed through a single `now()`
in `video_qos.rs` that the tests drive from a virtual clock, so a 100-second session
runs deterministically in milliseconds.

`Sim` runs the **real** `VideoQoS` against a modelled link: the encoder feeds it at
whatever rate and bitrate the controller currently asks for, the link drains at its
capacity, and the resulting queue is fed back exactly as `connection.rs` does it — a
delay probe once a second plus a congestion report from the send path, with the same
"report stalls immediately, rate-limit the all-clear" policy.

| Test | What it proves |
|---|---|
| `fast_link_reaches_full_rate_and_uses_available_bandwidth` | 60fps **and** a bitrate >20% above the 30fps preset — the fps compensation actually claims the spare bandwidth |
| `interference_sheds_load_and_bounds_the_queue` | A collapsed link cuts the rate hard and the backlog stays bounded |
| `rate_and_bitrate_recover_after_interference` | Both frame rate and bitrate are pulled back up once interference clears |
| `repeated_fluctuation_does_not_ratchet_the_session_down` | Three interference/recovery cycles all recover — no slow decay across a flaky session |
| `constrained_link_settles_without_oscillating` | A steadily slow link settles instead of hunting between extremes |

### Send path

`video_send_path` tests the real `PendingVideo`: key frames are recognised, a tolerable
backlog is never thinned, a deep backlog resyncs *only* at a key frame (and is kept
intact when there is none), display switches discard stale frames and are written first,
and — the input-delivery guarantee — draining N queued frames costs exactly N turns, one
input opportunity each.

### Unit tests

Also covered: a good link reaching the target, immediate shedding on queuing delay,
ramped rather than instant recovery, congestion overriding a healthy-looking delay, the
slowest user bounding the shared encoder, client caps acting as ceilings only, and the
adaptation floor.
