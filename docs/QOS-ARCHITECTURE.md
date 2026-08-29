# Interactive QoS — Design Direction

What the session should do, what it currently does, and why the direction is changing.

Terminology: **controlled** is the machine being viewed (captures, encodes, receives
input — the code calls it the server). **controlling** is the machine driving it (decodes,
renders, sends input — the code calls it the client).

See [`VIDEO-PIPELINE-TUNING.md`](VIDEO-PIPELINE-TUNING.md) for the history of individual
mechanisms.

---

## 1. Priorities

1. **Operations always arrive.** Keyboard and mouse are delivered and executed ahead of
   everything else, at any link quality. They cost a few kbps; there is no link on which
   they are unaffordable. A stale screen with working clicks is usable; the reverse is not.
2. **A picture always arrives.** Never frozen, never black — degrade it instead.
3. **Otherwise, be as fast as possible.** Send freely and pursue the most responsive
   experience.

## 2. Direction: stop measuring, stop guessing

The controller has been built around estimating the network — queuing delay, congestion
strikes, and most recently link capacity. **That approach is being reversed**, for reasons
that hold up:

- **Capacity cannot be measured accurately** from inside an application on a TCP socket.
  Any estimate is a guess derived from confounded signals.
- **Congestion detection duplicates TCP.** TCP already implements congestion control. A
  second control loop layered on top reacts to the first loop's behaviour, and two
  controllers fighting over one link oscillate. Our "write blocked > 120 ms" signal is
  literally an observation of TCP's own backpressure, re-interpreted as a network fact.
- **The estimates make things worse when wrong.** A bad estimate throttles a link that was
  fine, which looks exactly like network trouble and cannot be distinguished from it by
  the user. Most reported lag has come from the software throttling itself, not from the
  network.
- **In practice bandwidth is usually sufficient.** Optimising for the rare starved link at
  the cost of the common healthy one is the wrong trade.

**The intended model is therefore: send freely, never queue, never estimate.**

### 2.1 The one physical caveat

"Just send whatever you want" is right about *rate* but needs care about *latency*, and
the reason is worth stating precisely, because it decides the design:

> When you offer TCP more than the path carries, TCP does not drop your data — it
> **buffers and delays** it. Throughput stays correct; latency grows without bound.

So TCP's congestion control protects the *network*, not our *interactivity*. Left alone it
will happily build a multi-second backlog of stale frames and deliver every one of them.
That is bufferbloat, and it is indistinguishable from the lag being complained about.

The fix is **not** to measure and throttle. It is to **never build the queue**:

- offer frames freely;
- if the socket will not take a frame right now, do not accumulate — discard stale frames
  and let the encoder produce a fresh one;
- treat "can I write without blocking?" as a **boolean fact about this moment**, not as a
  measurement of the link.

That distinction is the whole design. A boolean "not right now" needs no accuracy, has no
estimator to be wrong, and cannot drift. Freshness is preserved by dropping old pixels,
which is exactly what a real-time stream should do — pixels have no value once superseded.

## 3. Per-connection independence

Rate must be **per connection**, never shared.

The current controller takes the **minimum across all users** for frame rate. That is a
real flaw, not a tuning problem:

- one slow viewer throttles every other viewer;
- a viewer that has closed uncleanly, or whose closure the controlled side has not yet
  noticed, keeps dragging the session down until it times out;
- a viewer that is merely paused or occluded is indistinguishable from a congested one.

Each connection should pace itself and drop its own frames. A slow peer degrades only
itself. The encoder is shared, so it runs at the **fastest** subscriber's rate and slow
peers skip — a slow peer must never set the pace for everyone.

## 4. What the code does today — implemented

| Mechanism | Where | Status |
|---|---|---|
| Input drained before any video is written (`biased`) | `connection.rs` | implemented |
| One video message per loop turn | `connection.rs::take_turn` | implemented |
| Key-frame-safe backlog cut, with on-demand key frames | `connection.rs`, `vpxcodec.rs`, `aom.rs` | implemented |
| Peer-reported rate (`auto_adjust_fps`), exception-based | `io_loop.rs` → `video_qos.rs` | implemented |
| Rate = **max** across peers | `video_qos.rs::fps` | implemented |
| Bitrate = user quality × `sqrt(fps/30)` | `video_qos.rs::target_ratio` | implemented |
| Interactive target 120fps | `video_qos.rs`, `client.rs`, `consts.dart` | implemented |
| Capacity probing / goodput accounting | — | **removed** |
| Congestion strikes | — | **removed** |
| Delay-banded fps, RTT estimator, delay-driven ABR | — | **removed** |

Nothing on the controlled side measures the network. `user_network_delay` and
`user_delay_response_elapsed` are retained as no-ops so the TestDelay round trip still
serves the peer's latency display, but they no longer influence rate.

### The compatibility property

Silence means "keep going". A client that predates the convention never sends
`auto_adjust_fps`, so it is served at the full rate — exactly what it received before.
A client that does report is slowed only while it says so, and recovers the moment it
stops saying so, with no ramp. No protocol addition was required: `auto_adjust_fps` is
proto field 28 and long-standing.

## 6. Remaining structural gap: one connection carries both

`tx` and `tx_video` are separate queues that drain into the **same** TCP stream. TCP has no
intra-connection priority, so a video frame already in flight delays input behind it —
head-of-line blocking, below the layer `biased` operates at.

`biased` guarantees ordering of *our* work: input is always processed before we choose to
write more video. It cannot preempt bytes already handed to the kernel.

Closing this fully requires input on its **own transport**, so video can never delay it.
Until then priority 1 is strongly approximated — input first in the loop, one frame per
turn, no queue build-up — but not physically guaranteed.

## 7. Order of work

> **Superseded.** [`PRIOR-ART-REMOTE-DESKTOP.md`](PRIOR-ART-REMOTE-DESKTOP.md) §5 replaces
> this ordering. Both VNC and RDP solve flow control by letting the receiver clock the
> sender - VNC by requesting each update, RDP by acknowledging each frame - rather than by
> inferring congestion. We already have that mechanism (`Misc::VideoReceived` /
> `video_unacked`) but only enable it for web clients. Extending it to all clients subsumes
> the never-queue and per-connection items below.


1. **Never-queue send path** (§2.1) — replaces the estimator with a boolean, and removes
   the mechanism that produced self-inflicted lag.
2. **Per-connection pacing** (§3) — stops one slow or half-dead viewer from holding
   everyone back.
3. **Delete capacity estimation and delay-banded fps** (§2) — once 1 and 2 are in, these
   have no remaining job.
4. **Latency-bounded client queue** (§5.2).
5. **Resolution fallback for the baseline guarantee** (§5.1).
6. **Separate input transport** (§6) — turns priority 1 from approximation into guarantee.
