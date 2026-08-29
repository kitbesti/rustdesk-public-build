# Diagnosing Lag — Reading the Statistics Logs

Three lines are emitted once a second, all greppable by `rd-stat`. Together they locate a
stall to one of: the capturer, the encoder, the link, or the client's decoder.

```bash
grep rd-stat ~/.local/share/logs/RustDesk/*.log        # Linux
grep rd-stat ~/Library/Logs/RustDesk/*.log             # macOS
```

## The three lines

**Controlled side — capture/encode:**
```
rd-stat capture monitor0: cap=118/s enc=117/s idle=2/s spf=8.3ms
```
| Field | Meaning |
|---|---|
| `cap` | frames the capturer returned |
| `enc` | frames encoded and handed to subscribers |
| `idle` | ticks where the screen had not changed (`WouldBlock`) |
| `spf` | interval QoS is pacing the loop at |

**Controlled side — per connection:**
```
rd-stat server conn 3: sent=117/s 1840kbps target=120fps cfg=1963kbps q=0 drop=0 slow=0
```
| Field | Meaning |
|---|---|
| `sent` | frames written to this peer's socket |
| `kbps` | what those frames actually occupied — real throughput, not a target |
| `target` | rate QoS is asking the capturer for |
| `cfg` | bitrate the encoder is configured with |
| `q` | frames still queued for this peer |
| `drop` | frames discarded at a key frame because it fell behind |
| `slow` | writes that blocked longer than the congestion threshold |

**Controlling side:**
```
rd-stat client: in=117/s shown=117/s dec=120/s q=1 vid=1835kbps total=232.10kB/s drop=0 resync=0 direct=true
```
| Field | Meaning |
|---|---|
| `in` | video frames arriving from the peer |
| `shown` | frames handed to the renderer — **the true display rate** |
| `dec` | decoder throughput |
| `q` | frames waiting to be decoded (each is added latency) |
| `vid` | video-only throughput |
| `total` | all traffic, including audio/cursor/clipboard |
| `drop` | queue overflows |
| `resync` | key frames requested to recover from them |

`total` was previously the only figure available and mixed every stream together, which
made it useless for judging video. `vid` is the number to use.

## Reading it

Compare the chain **`cap` → `enc` → `sent` → `in` → `shown`**. Whichever step first loses
frames is the bottleneck.

| Pattern | Meaning |
|---|---|
| `cap` ≈ `enc` ≈ `sent` ≈ `in` ≈ `shown`, `q`≈0, `drop`=0 | healthy |
| `cap` low, `idle` high | nothing is changing on screen — expected, and why an idle desktop sends a few kbps |
| `cap` low, `idle` low | **capturer** is the limit (display refresh rate, or a slow capture API) |
| `enc` < `cap` | **encoder** cannot keep up |
| `sent` < `enc` | frames are being dropped for this peer before the wire |
| `in` < `sent` | loss or backlog **in transit** |
| `shown` < `in` | **client decoder** is the limit, or frames are being discarded |
| `q` growing, `slow` > 0 | link cannot absorb the offered rate |
| `drop`/`resync` non-zero | the pathological case — decode chain is being broken and rebuilt |

**Clicks appearing to do nothing** is usually `drop`/`resync` climbing on the client: the
queue overflows, `force_push` evicts the oldest frame, the decode chain has a hole, and the
screen is stale or garbage until a key frame arrives. Input itself is delivered over an
unbounded channel to a dedicated thread and is not dropped.

**`shown` far below `target`** with everything else healthy usually means the display's
refresh rate is the ceiling — a 60Hz panel cannot show more than 60 distinct frames a
second no matter what the target says.

## Enabling logs

Logs are written by default. For more detail:

```bash
RUST_LOG=info ./rustdesk       # rd-stat lines are info level
```


---

## Measured finding: the X11 capturer is the ceiling (2026-08-02)

Live logs from a 1920x1080 X11 session showed the whole pipeline healthy *except* capture:

```
rd-stat capture monitor0: cap=9/s enc=9/s idle=36/s spf=8.3ms
rd-stat server conn 150:  sent=9/s 125kbps target=120fps cfg=1658kbps q=0 drop=0 slow=0
rd-stat client:           in=9/s shown=9/s dec=85/s q=0 vid=38kbps drop=0 resync=0
```

Read across the chain: `enc == sent == in == shown`, `q=0` everywhere, `drop=0`,
`resync=0`. Nothing is lost in the encoder, on the wire, or in the decoder - which has
**8x headroom** (`dec=85` against `shown=9`). The session only ever had ~9 frames a
second to show, because that is all the capturer produced.

A direct measurement of `Capturer::frame()` on the same display:

```
display 1920x1080 = 7.9 MB/frame
iterations=129 (26/s)  changed=43  unchanged=86
mean per iteration = 38.78 ms   worst = 51.19 ms
=> capture ceiling ~26 fps
```

`x11/capturer.rs::get_image` issues `xcb_shm_get_image` and blocks on the reply, which
makes the X server read the **entire framebuffer** - typically back across the PCIe bus
from VRAM - on every tick. 7.9 MB at ~208 MB/s is the 38 ms. `frame()` then memcmp's the
whole buffer against the previous one to decide whether anything changed, and memcpy's it
when something did.

Consequences:

- **Frame rate is capped at ~26fps** on this path regardless of `target`, encoder
  settings, bitrate, or link speed. Raising the interactive target to 120 cannot help;
  `spf=8.3ms` is simply ignored because one iteration costs 38 ms.
- **Two thirds of that work is wasted.** 86 of 129 grabs found nothing had changed, having
  already paid the full readback.
- No QoS, congestion or bandwidth change can affect this. It is not a network problem.

`grab` and `ceiling` are now reported on the capture line so this is visible directly:

```
rd-stat capture monitor0: cap=9/s enc=9/s idle=36/s grab=38.8ms ceiling=26fps spf=8.3ms
```

### Update: the cause was a pipeline stall, not readback bandwidth

Further measurement contradicted the VRAM-bandwidth reading above. Capture cost turned
out to be almost independent of area:

```
      region         MB    mean ms
       64x64       0.02      20.83
     1920x540      3.96      22.11
        full       7.91      38.74
```

A 64x64 grab costs nearly as much as one two hundred times larger, so the cost is a fixed
~21 ms per call, not bytes moved. Durations also clustered hard at exactly 2x and 3x the
16.67 ms vblank interval, never 1x - the signature of a synchronisation stall.

The cause was that `get_image` issued its request and immediately blocked on the reply, so
the next request was only submitted after the previous frame had been fully consumed. It
therefore always missed the upcoming refresh and waited for the one after.

Two grabs measured back to back confirmed the server can overlap work:

```
two full-screen grabs, sequential : 76.70 ms  (26.1 fps equivalent)
two full-screen grabs, pipelined  : 42.81 ms  (46.7 fps equivalent)
```

**Fix applied:** the X11 capturer now keeps two shared-memory slots and issues the next
grab *before* waiting for the current one, so the server services it during the wait.
Ordering matters: issuing the next request *after* the wait - the obvious arrangement, and
the one tried first - leaves it only the few hundred microseconds of the compare and copy
as a head start and measures identically to no pipelining at all.

```
before: 26.6 iterations/s   p50 = 34.2 ms
after:  59.3 iterations/s   p50 =  1.6 ms   p10 = 0.8 ms
```

Capture ceiling 26 -> 59 fps, i.e. the 60 Hz panel's own refresh rate, with the median
iteration 21x cheaper. Polls are now frequent *and* cheap, so a change is noticed almost
immediately rather than up to 34 ms later.

### Remaining: avoid reading unchanged screens

Stop reading the whole screen. The X extension for this is **XDamage**: the server reports
which rectangles changed, so the client can skip the grab entirely when nothing has, and
grab only the changed region when something has. This is how x11vnc and TigerVNC work, and
it is the same insight as RFC 6143's - do not move pixels that have not changed.

Note this is now a smaller prize than it first appeared: with the stall removed, an
unchanged poll costs ~1.6 ms rather than ~34 ms, and because the fixed per-call cost
dominates, reading only damaged *regions* would save little. The remaining value of damage
is avoiding the grab and the 0.26 ms full-buffer compare entirely when nothing changed.

XDamage is X11-only. The other backends already have equivalent native mechanisms:
Windows DXGI `AcquireNextFrame` blocks until a change and exposes dirty and move rects,
macOS `CGDisplayStream` delivers frames as they become available, and Wayland receives
buffers from PipeWire on change. Only X11 lacks one.

It requires: FFI declarations for `xcb-damage`, linking `libxcb-damage` (including in the
CI containers, which currently install only `libxcb-randr0-dev`, `libxcb-shape0-dev` and
`libxcb-xfixes0-dev`), damage setup and subtraction per frame, and region-wise reads into
the persistent buffer. Staged: damage as a pure change-detector first, which removes the
two thirds of wasted grabs and makes change detection immediate; then region reads, which
lift the ceiling during motion.

### Applied: damage-driven capture

Both stages are in. The first, damage as a pure change detector, did what was expected:
an unchanged poll no longer touches the server at all. It also exposed the next problem,
which the tight-loop benchmarks above could not see because they never ran alongside a
real desktop.

**The X server's time is the shared resource.** A live session was captured on a busy
desktop (a browser rendering, the compositor active) and the server-side numbers looked
tolerable - `cap=9/s idle=30/s grab=20ms` - but `Xorg` sat at **96-99 % of a core**
before any test program was started. Every full-screen `GetImage` is milliseconds of the
server's single thread, and that thread is the same one the compositor needs to draw the
next frame and the desktop needs to deliver input. Grabbing faster does not just fail to
help past that point; it slows down the thing being captured. Two things made it worse
than it needed to be:

- The pipeline kept a grab in flight *whenever the previous one had changed*, on the
  guess that the screen was still moving. On a desktop whose damage arrives in bursts
  that guess was wrong most of the time: each miss was a full read-back the server did
  for nothing, discovered only by comparing 8 MB and finding it equal.
- The damage extension was used only as a yes/no; a caret blink still cost a full-screen
  read-back.

**Fix applied.** The capturer now:

- asks the damage extension for the **bounding box** of what changed and grabs only that
  rectangle into a scratch slot, merging the changed rows into a persistent full frame.
  A caret blink reads back a few kilobytes instead of 8 MB; a change on another monitor
  reads back nothing (the box is clipped to this display before anything happens);
- **never speculates**: the next grab is issued only when fresh damage is already
  pending, which still overlaps it with this frame's encode during continuous motion
  (video playback reports damage every refresh) but issues nothing at all between bursts;
- distinguishes stale from fresh damage by X sequence number, so a report the server
  processed before the last grab, inside its area, is dropped rather than re-grabbed, and
  one outside it is kept rather than lost;
- **waits on the X socket** for up to the caller's timeout when nothing is pending, instead
  of returning immediately and having the loop sleep and poll. A change wakes it at once,
  so the wait costs no latency, and an idle screen costs no wake-ups. The capture loop
  lengthens that wait to 50 ms once the screen has been still for a few ticks; the only
  thing the wait bounds is how late the loop notices a request aimed at it (refresh,
  codec change, key-frame request), which is why it stays short;
- forces a full grab and an unconditional frame on the first call and whenever a key
  frame is requested, so a peer that needs to resynchronise on a still screen gets its
  picture instead of waiting for the desktop to move. Capturers without such a hook fall
  back to re-encoding the last picture as a key frame.

Seen from the capture loop, an idle desktop is now `waited=1126 immediate=0` over ten
seconds - every idle tick spent blocked in `poll`, none spinning.

One caveat the new `rd-stat x11` line makes visible: under a **compositing** window
manager (GNOME/Mutter, KDE, and most modern desktops) the compositor repaints the whole
root window every frame, so the damage box it reports is always the full screen -
`readback` in screens per second equals `grabs`. There the saving is the end of
speculation (about 40 full-screen read-backs a second down to one per compositor
frame, 6-10 on a lightly animated desktop), not smaller read-backs. The partial-box path
pays off on non-composited X11 (bare window managers, Xvfb, kiosk setups), where the
box is the caret or the window that actually changed.

### Applied: per-window damage on composited desktops (2026-08-29)

The caveat above turned out to be the whole problem on a composited desktop, and it was
self-inflicted. Re-arming the root's damage before each grab made the compositor's *next*
repaint - which it does every frame, whether or not anything changed - look like fresh
damage, so the capturer read the full screen ~15 times a second for nothing (87 % of the
reads compared equal), Xorg spent ~65 % of its one thread on those reads, and every real
change queued behind them: that is the latency the previous build was blamed for.

Measured with a probe on the same desktop (GNOME/Mutter on X11, software rendering):

- with no grabs at all, the root reports **nothing**; one `DamageSubtract` is followed
  by a full-screen report ~28 ms later, every time. On a composited desktop the root's
  damage is a **paint clock**, not a change signal;
- a read-back costs about 10 ms plus the bytes (a 1x1 pixel ~10 ms, 1080p ~50 ms), so
  reading less is what saves the server, not reading less often;
- each **top-level window** reports its own drawing exactly - a 959x549 browser at its
  animation rate, a terminal at its output rate, a test window only when painted - and
  the compositor shows it on screen one to three paints later.

**Fix applied.** The capturer now tracks damage per top-level window (SubstructureNotify
on the root keeps the set current) and uses the root's reports only as the paint clock:

- a window's report is read as its box after the next paint (or 40 ms), and read again at
  the following paints while it comes back unchanged, since the compositor may not have
  shown it yet; how many paints that takes is learned and reused;
- a window whose reports keep reading back unchanged (a browser repainting identical
  pixels every frame) is read less and less often, down to once a second, and a change
  halves that back-off rather than clearing it; a press lifts it at once;
- a change is credited to the window on top where it happened (stacking order is kept
  from ConfigureNotify), so a big window underneath a small changing one is not kept
  awake by it;
- the whole screen is read only for a paint no window drew for (the shell: a menu, the
  overview), after a press that no window drew for, and as a safety net once a second
  while paints continue - backing off to every 4 s when it finds nothing. When it does
  find something nothing was scheduled to read, the owning window is un-quieted and the
  lag estimate raised;
- the input service counts injected presses and scrolls; the capture loop passes each
  one to the capturer (`input_hint`), which lifts every back-off so whatever the press
  changed is read at the next paint. A press is explained by a window that reports and
  is not a known phantom, or by any window whose box then reads back changed; only a
  press nobody drew for reads the whole screen (the shell), at most twice a second.
  Typing into a browser that repaints identical pixels every frame used to cost a
  full-screen read per keystroke;
- the socket wait is rounded up to a whole millisecond - truncating it turned the last
  fraction of every deadline into a burst of zero-length polls.

Measured on the same desktop, interleaved with the previous build while the old server
still saturated Xorg (so both numbers are pessimistic): read-back **10 screens/s → 1.3-4**
depending on what is animating, paint-to-frame latency of a test window **median
192-272 ms → 100-160 ms**, 20 of 20 test paints delivered. `cargo run -p scrap --example
x11latency` measures it; `x11verify` compares the assembled picture to a fresh grab.

The measurement that matters for this change is not the frame rate but the X server's
CPU, and the way to take it is from `/proc/<Xorg pid>/stat` (`utime+stime`) over a fixed
interval, with and without a session connected. A capture path that is doing its job
leaves the server mostly idle on a still screen and proportional to the changed area on
a moving one.

### Applied: whole-screen reads on a busy composited desktop (2026-08-29, follow-up)

The build above, installed and serving a peer, logged `full=2-6/s` (whole-screen reads,
50 ms of Xorg each) in 98 % of seconds and 5-8 screens/s of read-back, while the same
capturer run standalone beside it on the same desktop logged `full=0.03-0.2/s` and
2.4 screens/s. Nothing on the X side differed - same damage events, no input, no key
frames - so the difference was in what the capturer's own state did with the press hints
and the whole-screen reads it took. Two things were wrong and one was unmeasurable:

- `input_hint` lifted **every** window's back-off at every keystroke. A browser that
  repaints identical pixels 36 times a second was then read at every report, plus the
  re-reads that follow an unchanged box, for as long as the user typed anywhere:
  read-back doubled (2.7 → 6.1 screens/s at five presses a second). It now lifts only the
  back-off of the window under the pointer and of the toplevel that has the keyboard
  focus (`QueryPointer`, `GetInputFocus` and a walk up the tree, a few round trips per
  press), since those are the windows a press can have gone to. The whole-screen read
  after a press nobody drew for still covers the shell;
- a change found by a whole-screen read was credited to the **root** ("direct reads
  keep their own result"), so the root's back-off collapsed every time a whole-screen
  read found some window's output, and every compositor paint no window had reported
  for since the previous paint became another whole-screen read. Worse, the root's box,
  once held and released, was re-read at the following paints like any window's box -
  up to five whole-screen re-reads for one paint. Now a change found by any whole-screen
  read belongs to the window on top there (`after_full` settles it as changed, which
  also lifts its back-off, so typing into a held window is picked up by the next report
  rather than by the next whole-screen read); the root is settled as changed only where
  no window is; the root's box is never re-read at later paints (its report *is* the
  compositor's paint), and one still waiting its 40 ms to be read is dropped when a
  window's report arrives meanwhile - reports and paints reach the capturer merged and
  out of step whenever it was busy reading or encoding; a batch that contains a
  screen-sized box reads just the screen instead of overflowing the segment; and a
  paint within two paints (or the learned lag) and 100 ms of a window's report is taken
  as that report's paint rather than as "nobody drew", since a CPU-bound window's
  reports jitter against the compositor's clock;
- the `rd-stat x11` line now says **why** the screen was read in full:
  `full=N/s (force/press/net/root/win/union/over a/b/c/d/e/f/g)` - a forced frame (key
  frame), a press nobody drew for, the periodic safety net, a paint nobody drew for
  (root), a window whose box is the whole screen, more than six boxes whose union is the
  screen, and boxes that did not fit the shared segment together. With the installed
  build the breakdown was not available; the next one shows it directly.

Standalone on the same desktop (browser animating at 36 reports/s, terminal at 3/s,
Xorg already at ~95 % from the browser and the old server), 30 s each:

| scenario | read-back before | read-back after | whole-screen reads after (per 30 s) |
|---|---|---|---|
| no presses | 2.4 screens/s | 2.4 screens/s | safety net 9, paint nobody drew for 2 |
| a press every 200 ms | 6.1 screens/s | 3.4 screens/s | press 11, safety net 20, root 4, window 4 |
| a press every 100 ms, 90 ms per encode | (not taken) | 3.1 screens/s | safety net 22, root 10, window 1 |

While presses keep coming the safety net is what remains (a press re-arms it to once a
second); paints nobody drew for are down to a few per minute. A later pass on the
installed build found 90 % of its 13 grabs/s reading back equal: a box released from a
hold was re-read at up to five later paints like a fresh report, although it was
reported at least a hold ago and the compositor had long shown it. Such a box now gets
one re-read (`Pending::stale`): standalone with no presses, 11.5 → 5.5 grabs/s and 2.4 →
1.3 screens/s, since each grab costs Xorg ~10 ms before the bytes. One caveat when comparing
with older logs: `full=` (and every other per-second figure on the line) used to be
truncated, so a single whole-screen read in a second was logged as `full=0/s`; the
figures are rounded now, and the cause counts are raw.

### Applied: spending an idle link on the still picture (2026-08-29)

With "Best" the AV1 encoder runs CBR at about 20 Mbit/s (1080p, 120 fps target) with a
quantizer range of 5-25, so a screen that changes three times a second already codes
each delta at the floor - the 20-100 kbit/s such a session sends is the size of the
changes, not a limit. What the idle link was *not* buying was detail in the parts of the
picture that a big change had left coarse: a scroll or a window switch is coded within
its frame budget at a higher quantizer, and whatever then stays still keeps that quality
until the next key frame (minutes away), because nothing is encoded while nothing moves.

Two changes in `src/server/video_service.rs` and `libs/scrap/src/common/aom.rs`:

- once the screen has been still for the idle back-off, the last picture is coded again,
  one pass per idle tick, up to `REFINE_PASSES` (10) times, and only for "Best" or a
  Custom setting at least as high (`refines_still_picture`), never on hardware encoders.
  The encoders already run cyclic-refresh AQ, which re-codes a share of the still blocks
  at a lower quantizer every frame; coding the same picture again is what lets it run
  while nothing moves. The capture line reports these as `refine=N/s` (also in `enc`);
- the AV1 quantizer floor that "Best" reaches goes from 5 to 2 (index 8 of 255). 0 would
  be lossless coding and frame sizes no budget allows.

Measured with a probe on the live desktop (1080p 4:4:4, quality 9.6, a real screen
grab scrolled 24 rows per frame at 20 fps for ten frames, then the same picture again
ten times at 50 ms spacing), PSNR of the decoded picture against its source:

| stage | bytes | PSNR Y / U (dB) |
|---|---|---|
| key frame | 198 KB | 45.6 / 44.8 |
| after the ten-frame scroll | 8-32 KB each | 49.7 / 49.2 |
| refinement pass 1 | 7.6 KB | 49.8 / 49.3 |
| refinement pass 5 | 59 KB | 51.6 / 51.0 |
| refinement pass 8 | 32 KB | 54.0 / 54.0 |
| refinement pass 10 | 16 KB | 54.2 / 54.1 |

Ten passes cost ~315 KB (about 5 Mbit/s for half a second) and 6-12 ms of encode each,
and bring the still picture from "good" to visually lossless; the pass sizes fall as it
converges, so the cap is a bound, not the usual cost.

Installed and serving a peer, the first cut of this re-armed all ten passes after *every*
change, including a terminal line three times a second: `refine=19-26/s`, 300-470 kbit/s
of 1-2 KB frames carrying nothing, and as many decodes a second on the peer. A run now
ends as soon as two passes in a row come out under one byte per 128 pixels (16 KB at
1080p; `handle_one_frame` returns the size it sent for this), which is where the probe's
passes stopped gaining anything. Installed, that left `refine=5/s` on a desktop where
only a terminal line changes: two tiny passes after every change, for nothing, so the
converged state is now kept until a frame at least that big is coded - a caret or a line
of text is coded at the floor already and leaves nothing to refine, while a scroll or a
window switch starts the run again even when typing interleaves with it.

