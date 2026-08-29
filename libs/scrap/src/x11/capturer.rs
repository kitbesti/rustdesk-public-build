use super::display::Rect;
use super::ffi::*;
use super::Display;
use hbb_common::{libc, log};
use std::{
    io, ptr, slice,
    time::{Duration, Instant},
};

// A window's report is read after the compositor's next paint. Without a compositor the
// root reports the same drawing at once, so this only bounds the wait when neither comes.
const PAINT_WAIT: Duration = Duration::from_millis(40);
// A box that read back unchanged is read again at up to this many later paints (a slow
// compositor shows a window's drawing one to three frames after the window reported it),
// or for this long when the paints stop coming.
const RECHECK_PAINTS: u8 = 5;
const RECHECK_FOR: Duration = Duration::from_millis(500);
// A window whose reports keep reading back unchanged (one that repaints identical pixels
// every frame) is read less and less often, up to this.
const HOLD_MIN: Duration = Duration::from_millis(50);
const HOLD_MAX: Duration = Duration::from_millis(1000);
// A press that no window drew after probably changed the shell (a menu, the overview),
// which draws through the compositor without a window of its own: read everything at the
// next paint after this, and no more often than this while presses keep coming. The wait
// leaves a window whose reports prove nothing (see `quiet`) time to be read and found
// changed, which explains the press just as well.
const PRESS_WAIT: Duration = Duration::from_millis(150);
const PRESS_FULL_EVERY: Duration = Duration::from_millis(500);
// A press nothing painted after this long changed nothing on screen.
const PRESS_FORGET: Duration = Duration::from_millis(300);
// While the compositor keeps painting, read everything this often regardless, less
// often while that keeps finding nothing.
const FULL_EVERY: Duration = Duration::from_secs(1);
const FULL_EVERY_MAX: Duration = Duration::from_secs(4);
// A read-back is split into at most this many boxes.
const MAX_BOXES: usize = 6;
// A paint this soon (server time) after a window's report is still that report's.
const LATE_PAINT_MS: u32 = 100;
// Why the whole screen was read, in the order the stat line lists them.
const WHY_FORCE: usize = 0;
const WHY_PRESS: usize = 1;
const WHY_NET: usize = 2;
const WHY_ROOT: usize = 3;
const WHY_WINDOW: usize = 4;
const WHY_UNION: usize = 5;
const WHY_OVER: usize = 6;

// One shared-memory segment the X server renders grabs into, each box at its own offset.
struct Shm {
    shmid: i32,
    xcbid: u32,
    buffer: *const u8,
    size: usize,
}

// A window on the root whose drawing is reported on its own. On a composited desktop the
// root only ever reports "the compositor painted", the whole screen every time; the
// windows report what was actually drawn and where. The root itself is the first source.
struct Source {
    window: u32,
    damage: u32,
    geom: Rect,
    mapped: bool,
    // Consecutive report cycles that read back unchanged, and the hold that earned.
    quiet: u8,
    hold_until: Option<Instant>,
    held: Option<Rect>,
}

// A reported box waiting to be read.
struct Pending {
    rect: Rect,
    // The reporting window; 0 when nobody's reads are being counted.
    src: u32,
    since: Instant,
    // The root reported the same drawing at the same server time, so no compositor is in
    // between and what is read next is what is on screen.
    direct: bool,
    // The window reported again after this was scheduled: one read may not show it all.
    renewed: bool,
    // Released from a hold: reported at least a hold ago, so the compositor has long
    // shown it, and one read after the next paint is enough.
    stale: bool,
}

// A box read again at later paints: one that read back unchanged after a report, and
// one that changed, in case the window drew again before that read (the compositor shows
// the last drawing a paint or more later).
struct Recheck {
    rect: Rect,
    src: u32,
    until: Instant,
    // Paints since the box was last read, and how many are worth reading after.
    paints: u8,
    budget: u8,
    saw_change: bool,
    renewed: bool,
}

enum Kind {
    Fresh { direct: bool },
    Recheck { until: Instant, paints: u8, budget: u8, saw_change: bool },
}

struct Item {
    rect: Rect,
    src: u32,
    kind: Kind,
    renewed: bool,
    stale: bool,
}

enum Plan {
    Wait(Option<Instant>),
    Full,
    Boxes(Vec<Item>),
}

struct Tracking {
    first_event: u8,
    root: u32,
    sources: Vec<Source>,
    pending: Vec<Pending>,
    recheck: Vec<Recheck>,
    // Server time of the latest window report.
    report_ts: u32,
    paint_seen: bool,
    paint_explained: bool,
    drew_since_paint: bool,
    // Paints since a window last reported drawing; none yet is `u8::MAX`.
    paints_since_draw: u8,
    press: Option<Instant>,
    press_explained: bool,
    press_due: bool,
    last_press_full: Option<Instant>,
    last_full: Instant,
    full_every: Duration,
    paints_since_full: u32,
    // Paints the compositor was last seen to take to show a report: how many a window
    // that usually reports for nothing is read at before it is given up on again.
    lag_paints: u8,
}

pub struct Capturer {
    display: Display,
    // The picture handed to callers, always complete; reads land in `shm` and only the
    // boxes read are merged in.
    frame: Vec<u8>,
    shm: Shm,
    // Without XDamage every call reads and compares the whole screen.
    tracking: Option<Tracking>,
    // Hand the picture over on the next call whether or not it changed.
    force: bool,
    bpp: usize,
    stats: Stats,
}

// One line a second, greppable as "rd-stat x11", so the read-back the X server is asked
// for is visible next to the frame counts in the same log.
struct Stats {
    since: Instant,
    grabs: u32,
    boxes: u32,
    pixels: u64,
    equal: u32,
    full: u32,
    why: [u32; 7],
    paints: u32,
    waits: u32,
}

impl Stats {
    fn report(&mut self, screen: u64) {
        let ms = self.since.elapsed().as_millis();
        if ms < 1000 {
            return;
        }
        let per_s = |n: u32| (n as u128 * 1000 + ms / 2) / ms;
        if self.grabs > 0 || self.paints > 0 {
            log::info!(
                "rd-stat x11: grabs={}/s boxes={}/s readback={:.2} screens/s equal={}/s full={}/s (force/press/net/root/win/union/over {}) paints={}/s waits={}/s",
                per_s(self.grabs),
                per_s(self.boxes),
                self.pixels as f64 / screen.max(1) as f64 * 1000.0 / ms as f64,
                per_s(self.equal),
                per_s(self.full),
                self.why.iter().map(|n| n.to_string()).collect::<Vec<_>>().join("/"),
                per_s(self.paints),
                per_s(self.waits),
            );
        }
        *self = Stats::new();
    }

    fn new() -> Stats {
        Stats {
            since: Instant::now(),
            grabs: 0,
            boxes: 0,
            pixels: 0,
            equal: 0,
            full: 0,
            why: [0; 7],
            paints: 0,
            waits: 0,
        }
    }
}

impl Capturer {
    pub fn new(display: Display) -> io::Result<Capturer> {
        let bpp = display.pixfmt().bytes_per_pixel();
        let rect = display.rect();
        let size = (rect.w as usize) * (rect.h as usize) * bpp;
        let server = display.server().raw();

        let shmid = unsafe { libc::shmget(libc::IPC_PRIVATE, size, libc::IPC_CREAT | 0o777) };
        if shmid == -1 {
            return Err(io::Error::last_os_error());
        }
        let buffer = unsafe { libc::shmat(shmid, ptr::null(), libc::SHM_RDONLY) } as *mut u8;
        if buffer as isize == -1 {
            return Err(io::Error::last_os_error());
        }
        let xcbid = unsafe { xcb_generate_id(server) };
        unsafe {
            xcb_shm_attach(server, xcbid, shmid as u32, 0);
        }
        let shm = Shm {
            shmid,
            xcbid,
            buffer,
            size,
        };

        let mut c = Capturer {
            frame: vec![0; size],
            shm,
            tracking: None,
            force: true,
            display,
            bpp,
            stats: Stats::new(),
        };
        // Best effort: without the extension we fall back to grab-and-compare.
        c.tracking = c.start_tracking();
        Ok(c)
    }

    fn start_tracking(&mut self) -> Option<Tracking> {
        let server = self.display.server().raw();
        let root = self.display.root();
        unsafe {
            let ext = xcb_get_extension_data(server, ptr::addr_of_mut!(xcb_damage_id));
            if ext.is_null() || (*ext).present == 0 {
                return None;
            }
            xcb_damage_query_version(server, 1, 1);
            let mask = XCB_EVENT_MASK_SUBSTRUCTURE_NOTIFY;
            xcb_change_window_attributes(
                server,
                root,
                XCB_CW_EVENT_MASK,
                &mask as *const u32 as *const _,
            );
            let mut t = Tracking {
                first_event: (*ext).first_event,
                root,
                sources: Vec::new(),
                pending: Vec::new(),
                recheck: Vec::new(),
                report_ts: 0,
                paint_seen: false,
                paint_explained: false,
                drew_since_paint: false,
                paints_since_draw: u8::MAX,
                press: None,
                press_explained: false,
                press_due: false,
                last_press_full: None,
                last_full: Instant::now(),
                full_every: FULL_EVERY,
                paints_since_full: 0,
                lag_paints: 1,
            };
            // The root first: its reports are the paint clock.
            let screen = {
                let r = self.display.rect();
                Rect {
                    x: 0,
                    y: 0,
                    w: (r.x as i32 + r.w as i32).max(0) as u16,
                    h: (r.y as i32 + r.h as i32).max(0) as u16,
                }
            };
            t.add_source(server, root, screen, true);
            let tree = xcb_query_tree_reply(server, xcb_query_tree_unchecked(server, root), ptr::null_mut());
            if !tree.is_null() {
                let n = xcb_query_tree_children_length(tree).max(0) as usize;
                let kids = slice::from_raw_parts(xcb_query_tree_children(tree), n);
                let asks: Vec<_> = kids
                    .iter()
                    .map(|&w| {
                        (
                            w,
                            xcb_get_window_attributes_unchecked(server, w),
                            xcb_get_geometry_unchecked(server, w),
                        )
                    })
                    .collect();
                xcb_flush(server);
                for (w, a, g) in asks {
                    let attr = xcb_get_window_attributes_reply(server, a, ptr::null_mut());
                    let geom = xcb_get_geometry_reply(server, g, ptr::null_mut());
                    if !attr.is_null() && !geom.is_null() && (*attr).class != XCB_WINDOW_CLASS_INPUT_ONLY {
                        let r = Rect {
                            x: (*geom).x,
                            y: (*geom).y,
                            w: (*geom).width,
                            h: (*geom).height,
                        };
                        t.add_source(server, w, r, (*attr).map_state == XCB_MAP_STATE_VIEWABLE);
                    }
                    libc::free(attr as *mut _);
                    libc::free(geom as *mut _);
                }
                libc::free(tree as *mut _);
            }
            xcb_flush(server);
            Some(t)
        }
    }

    pub fn display(&self) -> &Display {
        &self.display
    }

    /// The picture as last assembled, without asking the server for anything.
    pub fn current(&self) -> &[u8] {
        &self.frame
    }

    /// Return a picture on the next call even if nothing changed, reading everything
    /// again rather than trusting what has been reported since.
    pub fn force_next_frame(&mut self) {
        self.force = true;
    }

    /// A key or button was just pressed on this screen. Whatever it changed will be
    /// reported by the window it hit; if nothing reports, the shell was hit and the whole
    /// screen is read.
    pub fn input_hint(&mut self) {
        let server = self.display.server().raw();
        if let Some(t) = &mut self.tracking {
            let now = Instant::now();
            t.press = Some(now);
            t.press_explained = false;
            t.press_due = false;
            t.full_every = FULL_EVERY;
            // Whatever the press changed should be read at once, even in a window being
            // held for repainting without changing; the count stays, so such a window is
            // held again after one more unchanged cycle. Only the windows the press can
            // have gone to: lifting every hold at every keystroke doubles the read-back
            // while typing, on windows that repaint identical pixels regardless.
            if t.sources.iter().any(|s| s.hold_until.is_some()) {
                for w in unsafe { pressed_windows(server, t.root) } {
                    if let Some(s) = t.source_mut(w).filter(|s| s.hold_until.is_some()) {
                        s.hold_until = Some(now);
                    }
                }
            }
        }
    }

    // Take in everything the server has reported.
    fn drain_events(&mut self) {
        let Some(t) = &mut self.tracking else {
            return;
        };
        let server = self.display.server().raw();
        let screen = self.display.rect();
        let now = Instant::now();
        let mut sent = false;
        unsafe {
            loop {
                let ev = xcb_poll_for_event(server);
                if ev.is_null() {
                    break;
                }
                let kind = (*ev).response_type & 0x7f;
                if kind == t.first_event + XCB_DAMAGE_NOTIFY {
                    let d = &*(ev as *const xcb_damage_notify_event_t);
                    // Re-arm at once, so the next drawing is reported on its own.
                    xcb_damage_subtract(server, d.damage, 0, 0);
                    sent = true;
                    let area = Rect {
                        x: d.geometry.x.saturating_add(d.area.x),
                        y: d.geometry.y.saturating_add(d.area.y),
                        w: d.area.width,
                        h: d.area.height,
                    };
                    if d.drawable == t.root {
                        self.stats.paints += 1;
                        t.paint(area, d.timestamp, screen, now);
                    } else {
                        t.report(d.damage, area, d.geometry, d.timestamp, screen, now);
                    }
                } else if kind == XCB_CREATE_NOTIFY {
                    let e = &*(ev as *const xcb_create_notify_event_t);
                    if e.parent == t.root {
                        let r = Rect {
                            x: e.x,
                            y: e.y,
                            w: e.width,
                            h: e.height,
                        };
                        t.add_source(server, e.window, r, false);
                        sent = true;
                    }
                } else if kind == XCB_DESTROY_NOTIFY {
                    let e = &*(ev as *const xcb_destroy_notify_event_t);
                    t.remove_source(e.window, screen, now);
                } else if kind == XCB_UNMAP_NOTIFY {
                    let e = &*(ev as *const xcb_unmap_notify_event_t);
                    t.set_mapped(e.window, false, screen, now);
                } else if kind == XCB_MAP_NOTIFY {
                    let e = &*(ev as *const xcb_map_notify_event_t);
                    t.set_mapped(e.window, true, screen, now);
                } else if kind == XCB_CONFIGURE_NOTIFY {
                    let e = &*(ev as *const xcb_configure_notify_event_t);
                    let r = Rect {
                        x: e.x,
                        y: e.y,
                        w: e.width,
                        h: e.height,
                    };
                    t.configure(e.window, r, e.above_sibling, screen, now);
                } else if kind == XCB_REPARENT_NOTIFY {
                    let e = &*(ev as *const xcb_reparent_notify_event_t);
                    if e.parent == t.root {
                        let g = xcb_get_geometry_reply(
                            server,
                            xcb_get_geometry_unchecked(server, e.window),
                            ptr::null_mut(),
                        );
                        if !g.is_null() {
                            let r = Rect {
                                x: (*g).x,
                                y: (*g).y,
                                w: (*g).width,
                                h: (*g).height,
                            };
                            libc::free(g as *mut _);
                            t.add_source(server, e.window, r, true);
                            t.mark(r, screen, now);
                            sent = true;
                        }
                    } else {
                        t.remove_source(e.window, screen, now);
                    }
                } else if kind == XCB_CIRCULATE_NOTIFY {
                    let e = &*(ev as *const xcb_circulate_notify_event_t);
                    if let Some(s) = t.source_by_window(e.window) {
                        if s.mapped {
                            let r = s.geom;
                            t.mark(r, screen, now);
                        }
                    }
                }
                libc::free(ev as *mut _);
            }
            if sent {
                xcb_flush(server);
            }
        }
    }

    // Sleep until the server has something to say, or `timeout` passes.
    fn wait(&mut self, timeout: Duration) {
        let server = self.display.server().raw();
        let mut pfd = libc::pollfd {
            fd: unsafe { xcb_get_file_descriptor(server) },
            events: libc::POLLIN,
            revents: 0,
        };
        // Rounded up: truncating would turn the last part of a millisecond into a spin.
        let ms = timeout
            .saturating_add(Duration::from_micros(999))
            .as_millis()
            .min(i32::MAX as u128) as i32;
        unsafe {
            libc::poll(&mut pfd, 1, ms);
        }
    }

    // Ask the server for `boxes` in one batch and merge what differs into the picture.
    // Returns where each box differed, and whether the whole screen was among them:
    // boxes that do not fit the segment together are replaced by it.
    fn read(&mut self, boxes: &[Rect]) -> (Vec<Option<Rect>>, bool) {
        let server = self.display.server().raw();
        let screen = self.display.rect();
        let bpp = self.bpp;
        let bytes = |r: &Rect| r.w as usize * r.h as usize * bpp;
        let total: usize = boxes.iter().map(bytes).sum();
        let whole = [screen];
        let boxes = if total > self.shm.size || boxes.is_empty() {
            self.stats.why[WHY_OVER] += 1;
            &whole[..]
        } else {
            boxes
        };
        let mut offset = 0usize;
        let mut cookies = Vec::with_capacity(boxes.len());
        unsafe {
            for r in boxes {
                cookies.push(xcb_shm_get_image_unchecked(
                    server,
                    self.display.root(),
                    r.x,
                    r.y,
                    r.w,
                    r.h,
                    !0,
                    XCB_IMAGE_FORMAT_Z_PIXMAP,
                    self.shm.xcbid,
                    offset as u32,
                ));
                offset += bytes(r);
            }
            xcb_flush(server);
        }
        self.stats.grabs += 1;
        let mut changed = Vec::with_capacity(boxes.len());
        let mut full = false;
        let mut offset = 0usize;
        for (r, cookie) in boxes.iter().zip(cookies) {
            let reply = unsafe { xcb_shm_get_image_reply(server, cookie, ptr::null_mut()) };
            let ok = !reply.is_null();
            unsafe {
                libc::free(reply as *mut _);
            }
            self.stats.boxes += 1;
            self.stats.pixels += r.w as u64 * r.h as u64;
            if *r == screen {
                self.stats.full += 1;
                full = true;
            }
            let c = if ok { self.merge(offset, *r) } else { None };
            if c.is_none() {
                self.stats.equal += 1;
            }
            changed.push(c);
            offset += bytes(r);
        }
        (changed, full)
    }

    // Copy `area` from the segment at `offset` into the picture, from the first row that
    // differs to the last. Returns the box around what differed, if anything did.
    fn merge(&mut self, offset: usize, area: Rect) -> Option<Rect> {
        let rect = self.display.rect();
        let bpp = self.bpp;
        let stride = rect.w as usize * bpp;
        let row = area.w as usize * bpp;
        let x0 = (area.x - rect.x) as usize * bpp;
        let y0 = (area.y - rect.y) as usize;
        let src = unsafe { self.shm.buffer.add(offset) };
        let (mut top, mut bottom) = (usize::MAX, 0usize);
        let (mut left, mut right) = (usize::MAX, 0usize);
        for r in 0..area.h as usize {
            let s = unsafe { slice::from_raw_parts(src.add(r * row), row) };
            let at = (y0 + r) * stride + x0;
            let d = &mut self.frame[at..at + row];
            if d == s {
                continue;
            }
            let first = d.chunks(bpp).zip(s.chunks(bpp)).position(|(a, b)| a != b).unwrap_or(0);
            let last = d.chunks(bpp).zip(s.chunks(bpp)).rposition(|(a, b)| a != b).unwrap_or(first);
            top = top.min(r);
            bottom = r;
            left = left.min(first);
            right = right.max(last);
            d.copy_from_slice(s);
        }
        if top == usize::MAX {
            return None;
        }
        Some(Rect {
            x: area.x + left as i16,
            y: area.y + top as i16,
            w: (right - left + 1) as u16,
            h: (bottom - top + 1) as u16,
        })
    }

    // What to do now: read the whole screen, read some boxes, or wait (until when).
    fn plan(&mut self, now: Instant) -> Plan {
        let Some(t) = &mut self.tracking else {
            return Plan::Full;
        };
        if self.force {
            self.stats.why[WHY_FORCE] += 1;
            return Plan::Full;
        }
        t.release_holds(now);
        let paint = std::mem::take(&mut t.paint_seen);

        // Reasons to read everything.
        if let Some(pressed) = t.press {
            if now >= pressed + PRESS_WAIT {
                if t.press_explained || now >= pressed + PRESS_FORGET {
                    t.press = None;
                } else if paint {
                    // Handled by the root's own report unless the paint had a window
                    // behind it, in which case the shell's drawing is hidden in it.
                    t.press = None;
                    if t.paint_explained
                        && t.last_press_full.map_or(true, |l| now >= l + PRESS_FULL_EVERY)
                    {
                        t.last_press_full = Some(now);
                        t.press_due = true;
                    }
                }
            }
        }
        if std::mem::take(&mut t.press_due) {
            self.stats.why[WHY_PRESS] += 1;
            return Plan::Full;
        }
        if t.paints_since_full > 0 && now >= t.last_full + t.full_every {
            self.stats.why[WHY_NET] += 1;
            return Plan::Full;
        }

        // Boxes due now.
        let mut items = Vec::new();
        let mut keep = Vec::new();
        for p in t.pending.drain(..) {
            if paint || p.direct || now >= p.since + PAINT_WAIT {
                items.push(Item {
                    rect: p.rect,
                    src: p.src,
                    kind: Kind::Fresh { direct: p.direct },
                    renewed: p.renewed,
                    stale: p.stale,
                });
            } else {
                keep.push(p);
            }
        }
        t.pending = keep;
        let mut keep = Vec::new();
        for mut r in t.recheck.drain(..) {
            if paint {
                r.paints = r.paints.saturating_add(1);
            }
            if paint || now >= r.until {
                items.push(Item {
                    rect: r.rect,
                    src: r.src,
                    kind: Kind::Recheck {
                        until: r.until,
                        paints: r.paints,
                        budget: r.budget,
                        saw_change: r.saw_change,
                    },
                    renewed: r.renewed,
                    stale: false,
                });
            } else {
                keep.push(r);
            }
        }
        t.recheck = keep;
        if !items.is_empty() {
            return Plan::Boxes(items);
        }

        let mut until: Option<Instant> = None;
        let mut soonest = |i: Instant| until = Some(until.map_or(i, |u| u.min(i)));
        for p in &t.pending {
            soonest(p.since + PAINT_WAIT);
        }
        for r in &t.recheck {
            soonest(r.until);
        }
        for s in &t.sources {
            if let (Some(h), Some(_)) = (s.hold_until, s.held) {
                soonest(h);
            }
        }
        if let Some(p) = t.press {
            if now < p + PRESS_WAIT {
                soonest(p + PRESS_WAIT);
            }
        }
        if t.paints_since_full > 0 {
            soonest(t.last_full + t.full_every);
        }
        Plan::Wait(until)
    }

    fn read_full(&mut self, now: Instant) -> bool {
        let screen = self.display.rect();
        let changed = self.read(&[screen]).0[0];
        if let Some(t) = &mut self.tracking {
            t.after_full(changed, now);
        }
        changed.is_some()
    }

    fn read_boxes(&mut self, items: Vec<Item>, now: Instant) -> bool {
        let screen = self.display.rect();
        let root = self.tracking.as_ref().map_or(0, |t| t.root);
        let rects: Vec<Rect> = if items.iter().any(|i| i.rect == screen) {
            vec![screen]
        } else if items.len() > MAX_BOXES {
            vec![items.iter().fold(items[0].rect, |u, i| union(u, i.rect))]
        } else {
            items.iter().map(|i| i.rect).collect()
        };
        if rects.len() == 1 && rects[0] == screen {
            self.stats.why[if items.iter().any(|i| i.rect == screen && i.src == root) {
                WHY_ROOT
            } else if items.iter().any(|i| i.rect == screen) {
                WHY_WINDOW
            } else {
                WHY_UNION
            }] += 1;
        }
        let (changed, full) = self.read(&rects);
        let any = changed.iter().any(|c| c.is_some());
        let Some(t) = &mut self.tracking else {
            return any;
        };
        if full {
            let b = if changed.len() != rects.len() {
                changed[0]
            } else {
                rects.iter().zip(&changed).find(|(r, _)| **r == screen).and_then(|(_, c)| *c)
            };
            t.after_full(b, now);
        }
        // One box for all of them (their union, or the whole screen): its result is each
        // one's, except the root's, whose own reads are the shell's only where no window is.
        if changed.len() != items.len() {
            let shell = changed[0].and_then(|b| t.owner_of(b)) == Some(t.root);
            for item in items {
                let c = if item.src == t.root { shell } else { any };
                t.after_read(item, c, now);
            }
            return any;
        }
        // Boxes overlap (a popup over a window, windows over each other): what a box read
        // is credited to the window on top there, not to whichever box it was read through.
        let mut credited: Vec<u32> = Vec::new();
        for (item, c) in items.iter().zip(&changed) {
            if let Some(b) = c {
                credited.push(t.owner_of(*b).unwrap_or(item.src));
            }
        }
        for r in &mut t.recheck {
            if credited.contains(&r.src) {
                r.saw_change = true;
            }
        }
        for (i, item) in items.into_iter().enumerate() {
            // A direct read shows what is on screen: its own result stands, whichever
            // window put it there. The root's box is the shell's only where no window is
            // on top (a window's change there is that window's, see `after_full`).
            let own = item.src == 0
                || (item.src != t.root && matches!(item.kind, Kind::Fresh { direct: true }));
            let c = if own {
                changed[i].is_some()
            } else {
                credited.contains(&item.src)
            };
            t.after_read(item, c, now);
        }
        any
    }

    // Without XDamage: read and compare the whole screen every call.
    fn frame_blind(&mut self) -> bool {
        let screen = self.display.rect();
        self.read(&[screen]).0[0].is_some()
    }

    pub fn frame<'b>(&'b mut self, timeout: Duration) -> io::Result<&'b [u8]> {
        let changed = if self.tracking.is_none() {
            self.frame_blind()
        } else {
            let deadline = Instant::now() + timeout;
            let mut waited = false;
            loop {
                self.drain_events();
                let now = Instant::now();
                match self.plan(now) {
                    Plan::Wait(until) => {
                        if waited || now >= deadline {
                            self.report();
                            return Err(io::ErrorKind::WouldBlock.into());
                        }
                        self.stats.waits += 1;
                        let until = until.map_or(deadline, |u| u.min(deadline));
                        self.wait(until.saturating_duration_since(now));
                        waited = true;
                    }
                    Plan::Full => break self.read_full(now),
                    Plan::Boxes(items) => break self.read_boxes(items, now),
                }
            }
        };
        self.report();
        if !changed && !self.force {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        self.force = false;
        Ok(&self.frame)
    }

    fn report(&mut self) {
        let rect = self.display.rect();
        self.stats.report(rect.w as u64 * rect.h as u64);
    }
}

impl Tracking {
    fn add_source(&mut self, server: *mut xcb_connection_t, window: u32, geom: Rect, mapped: bool) {
        if self.source_by_window(window).is_some() {
            return;
        }
        let damage = unsafe {
            let id = xcb_generate_id(server);
            xcb_damage_create(server, id, window, XCB_DAMAGE_REPORT_LEVEL_BOUNDING_BOX);
            id
        };
        self.sources.push(Source {
            window,
            damage,
            geom,
            mapped,
            quiet: 0,
            hold_until: None,
            held: None,
        });
    }

    // The damage object goes with the window when the server destroys it; when the window
    // merely left the root it keeps reporting, harmlessly, until the connection closes.
    fn remove_source(&mut self, window: u32, screen: Rect, now: Instant) {
        if let Some(i) = self.sources.iter().position(|s| s.window == window) {
            let s = self.sources.remove(i);
            if s.mapped {
                self.mark(s.geom, screen, now);
            }
        }
    }

    fn source_by_window(&self, window: u32) -> Option<&Source> {
        self.sources.iter().find(|s| s.window == window)
    }

    fn source_mut(&mut self, window: u32) -> Option<&mut Source> {
        self.sources.iter_mut().find(|s| s.window == window)
    }

    fn quiet_of(&self, window: u32) -> u8 {
        self.source_by_window(window).map_or(0, |s| s.quiet)
    }

    fn set_mapped(&mut self, window: u32, mapped: bool, screen: Rect, now: Instant) {
        if let Some(s) = self.source_mut(window) {
            if s.mapped != mapped {
                s.mapped = mapped;
                let r = s.geom;
                self.mark(r, screen, now);
            }
        }
    }

    fn configure(&mut self, window: u32, geom: Rect, above: u32, screen: Rect, now: Instant) {
        let Some(i) = self.sources.iter().position(|s| s.window == window) else {
            return;
        };
        let old = std::mem::replace(&mut self.sources[i].geom, geom);
        if self.sources[i].mapped {
            self.mark(old, screen, now);
            self.mark(geom, screen, now);
        }
        // Keep the sources in stacking order, the root at the bottom.
        let s = self.sources.remove(i);
        let at = self
            .sources
            .iter()
            .position(|s| s.window == above)
            .map_or(1.min(self.sources.len()), |a| a + 1);
        self.sources.insert(at, s);
    }

    // The mapped window on top at `area`: the one that contains it, else the one covering
    // most of it. The root, which contains everything, only when no window does.
    fn owner_of(&self, area: Rect) -> Option<u32> {
        let px = |r: Rect| r.w as u64 * r.h as u64;
        let mut best: Option<(u64, u32)> = None;
        for s in self.sources.iter().rev() {
            if !s.mapped {
                continue;
            }
            let Some(i) = intersect(s.geom, area) else {
                continue;
            };
            if s.window == self.root {
                return Some(best.map_or(s.window, |b| b.1));
            }
            if px(i) == px(area) {
                return Some(s.window);
            }
            if best.map_or(true, |b| px(i) > b.0) {
                best = Some((px(i), s.window));
            }
        }
        best.map(|b| b.1)
    }

    // Something on screen changed there for a reason the windows will not report.
    fn mark(&mut self, area: Rect, screen: Rect, now: Instant) {
        if let Some(a) = intersect(area, screen) {
            self.drew_since_paint = true;
            self.push_pending(a, 0, now, false);
        }
    }

    // A window reported drawing.
    fn report(&mut self, damage: u32, area: Rect, geometry: xcb_rectangle_t, ts: u32, screen: Rect, now: Instant) {
        let Some(s) = self.sources.iter_mut().find(|s| s.damage == damage) else {
            return;
        };
        s.geom = Rect {
            x: geometry.x,
            y: geometry.y,
            w: geometry.width,
            h: geometry.height,
        };
        let window = s.window;
        self.drew_since_paint = true;
        self.report_ts = ts;
        // A paint nobody had drawn for, still waiting to be read as the shell's, was this
        // drawing's after all: reports and paints reach here merged and out of step
        // whenever the picture was being read or encoded meanwhile.
        let root = self.root;
        self.pending.retain(|p| p.src != root || now >= p.since + PAINT_WAIT);
        let Some(a) = intersect(area, screen) else {
            return;
        };
        if s.hold_until.is_some() {
            s.held = Some(s.held.map_or(a, |h| union(h, a)));
            return;
        }
        // A window known to repaint without changing anything explains nothing.
        if self.press.is_some() && s.quiet == 0 {
            self.press_explained = true;
        }
        // A box being read again at the coming paints is read again anyway.
        if let Some(r) = self.recheck.iter_mut().find(|r| r.src == window) {
            r.rect = union(r.rect, a);
            r.renewed = true;
            return;
        }
        self.push_pending(a, window, now, false);
    }

    // A box was read. One that changed is done unless its window drew again meanwhile;
    // one that did not is read again at the coming paints until it does, or until it is
    // clear it never will.
    fn after_read(&mut self, item: Item, changed: bool, now: Instant) {
        if changed {
            self.settle(item.src, true, now);
        }
        let (until, paints, budget, saw_change) = match item.kind {
            // The root's box is read from the screen itself: nothing shows up later.
            Kind::Fresh { direct } if direct || item.src == self.root => {
                if !changed {
                    self.settle(item.src, false, now);
                }
                return;
            }
            Kind::Recheck { paints, saw_change: false, .. } if changed => {
                self.lag_paints = paints.clamp(1, RECHECK_PAINTS);
                if !item.renewed {
                    return;
                }
                (now + RECHECK_FOR, 0, self.lag_paints, true)
            }
            _ if changed => {
                if matches!(item.kind, Kind::Fresh { .. }) {
                    self.lag_paints = self.lag_paints.saturating_sub(1).max(1);
                }
                if !item.renewed {
                    return;
                }
                (now + RECHECK_FOR, 0, self.lag_paints, true)
            }
            Kind::Fresh { .. } => {
                let budget = if item.stale {
                    1
                } else if self.quiet_of(item.src) > 0 {
                    self.lag_paints
                } else {
                    RECHECK_PAINTS
                };
                (now + RECHECK_FOR, 0, budget, false)
            }
            Kind::Recheck { until, paints, budget, saw_change } => {
                if paints < budget && now < until {
                    (until, paints, budget, saw_change)
                } else {
                    if !saw_change {
                        self.settle(item.src, false, now);
                    }
                    return;
                }
            }
        };
        self.recheck.push(Recheck {
            rect: item.rect,
            src: item.src,
            until,
            paints,
            budget,
            saw_change,
            renewed: false,
        });
    }

    // The whole screen was read. A change there was found before the window it belongs
    // to was read, so the window is not as quiet as it was taken to be; one that nothing
    // was going to read is one the picture had missed, so the compositor is also slower
    // than it was taken to be.
    fn after_full(&mut self, changed: Option<Rect>, now: Instant) {
        self.last_full = now;
        self.paints_since_full = 0;
        let missed = changed.filter(|b| !self.covered(*b));
        self.full_every = if missed.is_some() {
            FULL_EVERY
        } else {
            (self.full_every * 2).min(FULL_EVERY_MAX)
        };
        if let Some(b) = changed {
            for r in &mut self.recheck {
                if intersect(r.rect, b).is_some() {
                    r.saw_change = true;
                }
            }
        }
        if let Some(b) = changed {
            if let Some(w) = self.owner_of(b).filter(|&w| w != self.root) {
                if missed.is_some() {
                    self.lag_paints = RECHECK_PAINTS;
                }
                self.settle(w, true, now);
            }
        }
    }

    // Something scheduled would have read a change at `b`: a report waiting for its paint,
    // a box being read again, or a held window's report.
    fn covered(&self, b: Rect) -> bool {
        self.pending.iter().any(|p| intersect(p.rect, b).is_some())
            || self.recheck.iter().any(|r| intersect(r.rect, b).is_some())
            || self.sources.iter().any(|s| s.held.map_or(false, |h| intersect(h, b).is_some()))
    }

    // The root reported: on a composited desktop the compositor painted (the whole
    // screen, whatever changed); without one, something drew straight to the screen.
    fn paint(&mut self, area: Rect, ts: u32, screen: Rect, now: Instant) {
        self.paint_seen = true;
        self.paints_since_full += 1;
        // A slow compositor paints more than once for one report: the paints within its
        // lag of the report are its (two at the least, for the jitter of a busy window),
        // as are those while the box is still being read, or read again.
        let drew = std::mem::take(&mut self.drew_since_paint);
        self.paints_since_draw = if drew { 1 } else { self.paints_since_draw.saturating_add(1) };
        let explained = drew
            || (self.paints_since_draw <= self.lag_paints.max(2)
                && ts.wrapping_sub(self.report_ts) <= LATE_PAINT_MS)
            || !self.pending.is_empty()
            || !self.recheck.is_empty();
        self.paint_explained = explained;
        let Some(a) = intersect(area, screen) else {
            return;
        };
        if explained {
            // The same drawing, reported by its window at the same server time: nothing
            // is composited in between, so no re-reads are needed after the read.
            if !self.pending.is_empty()
                && ts.wrapping_sub(self.report_ts) <= 1
                && self
                    .pending
                    .iter()
                    .fold(None, |u: Option<Rect>, p| Some(u.map_or(p.rect, |u| union(u, p.rect))))
                    .map_or(false, |u| contains(u, a))
            {
                for p in &mut self.pending {
                    p.direct = true;
                }
            }
            return;
        }
        // Nobody drew, yet the screen changed: the shell, or drawing on the root itself.
        // Read after the usual wait rather than at once: a window whose report reaches
        // this late (see `report`) may yet turn out to have drawn for it.
        let root = self.root;
        let Some(s) = self.source_mut(root) else {
            return;
        };
        if s.hold_until.is_some() {
            s.held = Some(s.held.map_or(a, |h| union(h, a)));
            return;
        }
        self.push_pending(a, root, now, false);
    }

    fn push_pending(&mut self, rect: Rect, src: u32, now: Instant, direct: bool) {
        if let Some(p) = self.pending.iter_mut().find(|p| p.src == src) {
            p.rect = union(p.rect, rect);
            p.direct = p.direct && direct;
            p.renewed = true;
            return;
        }
        self.pending.push(Pending {
            rect,
            src,
            since: now,
            direct,
            renewed: false,
            stale: false,
        });
    }

    fn release_holds(&mut self, now: Instant) {
        let mut released = Vec::new();
        for s in &mut self.sources {
            if let Some(h) = s.hold_until {
                if now >= h {
                    s.hold_until = None;
                    if let Some(r) = s.held.take() {
                        released.push((r, s.window));
                    }
                }
            }
        }
        for (r, w) in released {
            self.push_pending(r, w, now, false);
            if let Some(p) = self.pending.iter_mut().find(|p| p.src == w) {
                p.stale = true;
            }
        }
    }

    // A report cycle ended: the box read back changed, or never did.
    fn settle(&mut self, src: u32, changed: bool, now: Instant) {
        if src == 0 {
            return;
        }
        let Some(s) = self.source_mut(src) else {
            return;
        };
        if changed {
            // Halved, not cleared: a window that changes once in many reports (a blinking
            // caret) keeps being read at a fraction of its reports rather than at every one.
            s.quiet /= 2;
            s.hold_until = None;
            if let Some(r) = s.held.take() {
                self.push_pending(r, src, now, false);
            }
            // Whatever its reports are worth, the window did change: the press went there.
            if self.press.is_some() {
                self.press_explained = true;
            }
        } else {
            s.quiet = s.quiet.saturating_add(1).min(8);
            let hold = HOLD_MIN.saturating_mul(1 << (s.quiet - 1)).min(HOLD_MAX);
            s.hold_until = Some(now + hold);
        }
    }
}

// The windows on the root a press can have gone to: the one under the pointer, and the
// one the keyboard focus is in (or a descendant of).
unsafe fn pressed_windows(server: *mut xcb_connection_t, root: u32) -> Vec<u32> {
    let mut out = Vec::with_capacity(2);
    let p = xcb_query_pointer_reply(server, xcb_query_pointer_unchecked(server, root), ptr::null_mut());
    if !p.is_null() {
        if (*p).child != 0 {
            out.push((*p).child);
        }
        libc::free(p as *mut _);
    }
    let f = xcb_get_input_focus_reply(server, xcb_get_input_focus_unchecked(server), ptr::null_mut());
    if f.is_null() {
        return out;
    }
    let mut w = (*f).focus;
    libc::free(f as *mut _);
    // Focus is None (0) or PointerRoot (1) rather than a window; otherwise walk up to the
    // root's child the focus window is in.
    for _ in 0..16 {
        if w <= 1 {
            return out;
        }
        let t = xcb_query_tree_reply(server, xcb_query_tree_unchecked(server, w), ptr::null_mut());
        if t.is_null() {
            return out;
        }
        let parent = (*t).parent;
        libc::free(t as *mut _);
        if parent == root {
            if !out.contains(&w) {
                out.push(w);
            }
            return out;
        }
        w = parent;
    }
    out
}

fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let x1 = a.x.max(b.x) as i32;
    let y1 = a.y.max(b.y) as i32;
    let x2 = (a.x as i32 + a.w as i32).min(b.x as i32 + b.w as i32);
    let y2 = (a.y as i32 + a.h as i32).min(b.y as i32 + b.h as i32);
    if x2 <= x1 || y2 <= y1 {
        return None;
    }
    Some(Rect {
        x: x1 as i16,
        y: y1 as i16,
        w: (x2 - x1) as u16,
        h: (y2 - y1) as u16,
    })
}

fn union(a: Rect, b: Rect) -> Rect {
    let x1 = a.x.min(b.x) as i32;
    let y1 = a.y.min(b.y) as i32;
    let x2 = (a.x as i32 + a.w as i32).max(b.x as i32 + b.w as i32);
    let y2 = (a.y as i32 + a.h as i32).max(b.y as i32 + b.h as i32);
    Rect {
        x: x1 as i16,
        y: y1 as i16,
        w: (x2 - x1) as u16,
        h: (y2 - y1) as u16,
    }
}

fn contains(outer: Rect, inner: Rect) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.x as i32 + inner.w as i32 <= outer.x as i32 + outer.w as i32
        && inner.y as i32 + inner.h as i32 <= outer.y as i32 + outer.h as i32
}

impl Drop for Capturer {
    fn drop(&mut self) {
        let server = self.display.server().raw();
        if let Some(t) = &self.tracking {
            unsafe {
                for s in &t.sources {
                    xcb_damage_destroy(server, s.damage);
                }
                let mask = 0u32;
                xcb_change_window_attributes(
                    server,
                    t.root,
                    XCB_CW_EVENT_MASK,
                    &mask as *const u32 as *const _,
                );
            }
        }
        unsafe {
            xcb_shm_detach(server, self.shm.xcbid);
            xcb_flush(server);
            libc::shmdt(self.shm.buffer as *mut _);
            libc::shmctl(self.shm.shmid, libc::IPC_RMID, ptr::null_mut());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i16, y: i16, w: u16, h: u16) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn damage_on_another_display_is_not_this_displays_business() {
        // Two displays side by side; a change on the right one must not grab the left.
        let left = r(0, 0, 1920, 1080);
        assert_eq!(intersect(r(2000, 100, 50, 50), left), None);
        assert_eq!(
            intersect(r(1900, 100, 50, 50), left),
            Some(r(1900, 100, 20, 50))
        );
    }

    #[test]
    fn pending_damage_grows_to_cover_every_report() {
        let u = union(r(10, 10, 5, 5), r(100, 200, 5, 5));
        assert_eq!(u, r(10, 10, 95, 195));
        assert!(contains(u, r(10, 10, 5, 5)));
        assert!(contains(u, r(100, 200, 5, 5)));
        assert!(!contains(u, r(104, 200, 5, 5)));
    }

    #[test]
    fn containment_is_edge_inclusive() {
        let outer = r(0, 0, 100, 100);
        assert!(contains(outer, r(0, 0, 100, 100)));
        assert!(contains(outer, r(90, 90, 10, 10)));
        assert!(!contains(outer, r(91, 90, 10, 10)));
    }

    #[test]
    fn a_window_that_keeps_reporting_unchanged_pixels_is_read_less_often() {
        let now = Instant::now();
        let mut t = Tracking {
            first_event: 0,
            root: 1,
            sources: vec![Source {
                window: 7,
                damage: 70,
                geom: r(0, 0, 100, 100),
                mapped: true,
                quiet: 0,
                hold_until: None,
                held: None,
            }],
            pending: Vec::new(),
            recheck: Vec::new(),
            report_ts: 0,
            paint_seen: false,
            paint_explained: false,
            drew_since_paint: false,
            paints_since_draw: u8::MAX,
            press: None,
            press_explained: false,
            press_due: false,
            last_press_full: None,
            last_full: now,
            full_every: FULL_EVERY,
            paints_since_full: 0,
            lag_paints: 1,
        };
        t.settle(7, false, now);
        assert_eq!(t.sources[0].hold_until, Some(now + HOLD_MIN));
        for _ in 0..10 {
            t.settle(7, false, now);
        }
        assert_eq!(t.sources[0].hold_until, Some(now + HOLD_MAX));
        // Held reports pile up and come out together when the hold ends.
        let screen = r(0, 0, 1920, 1080);
        let geo = xcb_rectangle_t { x: 0, y: 0, width: 100, height: 100 };
        t.report(70, r(10, 10, 5, 5), geo, 1, screen, now);
        t.report(70, r(50, 50, 5, 5), geo, 2, screen, now);
        assert!(t.pending.is_empty());
        t.release_holds(now + HOLD_MAX);
        assert_eq!(t.pending.len(), 1);
        assert_eq!(t.pending[0].rect, r(10, 10, 45, 45));
        // A change halves it and lifts the hold.
        t.settle(7, true, now);
        assert_eq!(t.sources[0].quiet, 4);
        assert_eq!(t.sources[0].hold_until, None);
    }

    #[test]
    fn a_paint_nobody_drew_for_is_read_as_the_shell() {
        let now = Instant::now();
        let screen = r(0, 0, 1920, 1080);
        let mut t = Tracking {
            first_event: 0,
            root: 1,
            sources: vec![Source {
                window: 1,
                damage: 10,
                geom: screen,
                mapped: true,
                quiet: 0,
                hold_until: None,
                held: None,
            }],
            pending: Vec::new(),
            recheck: Vec::new(),
            report_ts: 0,
            paint_seen: false,
            paint_explained: false,
            drew_since_paint: false,
            paints_since_draw: u8::MAX,
            press: None,
            press_explained: false,
            press_due: false,
            last_press_full: None,
            last_full: now,
            full_every: FULL_EVERY,
            paints_since_full: 0,
            lag_paints: 1,
        };
        t.paint(screen, 100, screen, now);
        assert_eq!(t.pending.len(), 1);
        assert_eq!(t.pending[0].src, 1);
        assert!(!t.pending[0].direct);
        // A window's report that reaches here while that box waits was the paint's.
        t.sources.push(Source {
            window: 2,
            damage: 20,
            geom: r(0, 0, 100, 100),
            mapped: true,
            quiet: 0,
            hold_until: None,
            held: None,
        });
        let geo = xcb_rectangle_t { x: 0, y: 0, width: 100, height: 100 };
        t.report(20, r(0, 0, 100, 100), geo, 110, screen, now);
        assert!(t.pending.iter().all(|p| p.src == 2));
        // With a window's report behind it the paint adds nothing of its own.
        t.pending.clear();
        t.drew_since_paint = true;
        t.paint(screen, 130, screen, now);
        assert!(t.pending.is_empty());
        assert!(t.paint_explained);
    }
}
