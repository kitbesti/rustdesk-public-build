use super::*;
use scrap::codec::{Quality, BR_BALANCED};
use std::time::{Duration, Instant};

/*
Flow control is receiver-driven and exception-based.

The controlled side does not measure the network. It cannot: from inside an application
on a TCP socket every available signal is confounded. Round-trip drift is mostly TCP's
own queue, and a blocked write *is* TCP's backpressure - re-reading either as a fact
about the link, then feeding it into a per-frame control loop, produced a controller that
throttled healthy links and could not be distinguished from real network trouble.

Instead: send freely, and let the peer speak up. The peer is the only party that can
observe what actually matters - whether frames are piling up in front of its decoder and
how stale the picture on screen is - and it reports that through `auto_adjust_fps`, which
it sends only when the situation changes.

Silence therefore means "keep going". That is what makes this compatible with clients
that predate the convention: they simply never speak, so they are served at full rate,
which is exactly what they got before.

Two local, non-inferential safeguards remain, and neither estimates anything:
  - the send loop writes one message per turn and drains input first, so operations are
    never queued behind pixels;
  - a connection that falls too far behind discards its backlog at a key frame, asking
    the encoder for one if none is pending. "Can I write this now?" is a fact about this
    instant, not a measurement of the link.

Rate is taken as the *maximum* over connected peers, not the minimum. One encoder feeds
everyone, and a peer that cannot keep up drops and resynchronises for itself. Taking the
minimum let a single slow - or paused, or uncleanly closed and not yet timed out - viewer
hold back every other one.
*/

// Interactive target. A peer is served at this rate unless it asks for less.
//
// Above 60 deliberately: on a local link there is no reason to stop at 60, and capping
// there was an artificial limit rather than anything the hardware or network imposed.
// What is actually achieved is bounded by the capturer - a 60Hz display cannot produce
// more than 60 distinct frames a second - so this is a ceiling, not a promise.
pub const FPS: u32 = 120;
pub const MIN_FPS: u32 = 1;
pub const MAX_FPS: u32 = 120;

// Never drop below this on a peer's request; under it the session stops being interactive
// and the bitrate saved is negligible.
const ADAPT_MIN_FPS: u32 = 5;

// The bitrate presets in `scrap::codec::base_bitrate` are calibrated for this rate.
const REFERENCE_FPS: u32 = 30;
const MIN_FPS_BITRATE_SCALE: f32 = 0.7;
const MAX_FPS_BITRATE_SCALE: f32 = 1.6;

// Bitrate ratio constants for different quality levels
const BR_MAX: f32 = 40.0; // 2000 * 2 / 100
const BR_MIN_HIGH_RESOLUTION: f32 = 0.1;

// How much bitrate the current frame rate deserves relative to the presets.
//
// `base_bitrate()` depends only on resolution, so the budget it yields is identical
// whether we feed 30 or 60 frames per second into it. Running at 60fps against a 30fps
// budget halves the bits available per frame. Bits required scale with roughly the square
// root of frame rate, because consecutive frames resemble each other more the closer
// together they are. This is arithmetic, not estimation.
fn fps_bitrate_scale(fps: u32) -> f32 {
    (fps as f32 / REFERENCE_FPS as f32)
        .sqrt()
        .clamp(MIN_FPS_BITRATE_SCALE, MAX_FPS_BITRATE_SCALE)
}

#[cfg(not(test))]
#[inline]
fn now() -> Instant {
    Instant::now()
}

#[cfg(test)]
thread_local! {
    static TEST_CLOCK: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn now() -> Instant {
    TEST_CLOCK.with(|c| {
        let t = c.get().unwrap_or_else(Instant::now);
        c.set(Some(t));
        t
    })
}

#[cfg(test)]
fn advance_clock(d: Duration) {
    TEST_CLOCK.with(|c| {
        let t = c.get().unwrap_or_else(Instant::now);
        c.set(Some(t + d));
    });
}

#[cfg(test)]
fn reset_clock() {
    TEST_CLOCK.with(|c| c.set(Some(Instant::now())));
}

// What one peer has told us it can take. Absent = it has not spoken, so serve it fully.
#[derive(Default, Debug, Clone)]
struct UserData {
    // Decoder-side limit the peer reports when it falls behind.
    auto_adjust_fps: Option<u32>,
    // Explicit ceiling chosen by the user on that peer.
    custom_fps: Option<u32>,
    quality: Option<(i64, Quality)>, // (time, quality)
    record: bool,
}

impl UserData {
    // Rate this peer should be served at.
    fn target_fps(&self) -> u32 {
        let mut fps = FPS;
        for cap in [self.custom_fps, self.auto_adjust_fps]
            .into_iter()
            .flatten()
        {
            fps = fps.min(cap.max(ADAPT_MIN_FPS));
        }
        fps.clamp(ADAPT_MIN_FPS, FPS)
    }
}

#[derive(Default, Debug, Clone)]
struct DisplayData {
    support_changing_quality: bool,
}

pub struct VideoQoS {
    ratio: f32,
    users: HashMap<i32, UserData>,
    displays: HashMap<String, DisplayData>,
    bitrate_store: u32,
    abr_config: bool,
    _last_tick: Instant,
}

impl Default for VideoQoS {
    fn default() -> Self {
        VideoQoS {
            ratio: BR_BALANCED,
            users: Default::default(),
            displays: Default::default(),
            bitrate_store: 0,
            abr_config: true,
            _last_tick: now(),
        }
    }
}

impl VideoQoS {
    pub fn spf(&self) -> Duration {
        Duration::from_secs_f32(1. / (self.fps() as f32))
    }

    // Serve the fastest peer. A slower one drops and resynchronises for itself rather
    // than setting the pace for everybody.
    pub fn fps(&self) -> u32 {
        self.users
            .values()
            .map(|u| u.target_fps())
            .max()
            .unwrap_or(FPS)
            .clamp(ADAPT_MIN_FPS, FPS)
    }

    // The rate one specific peer asked for, so its connection can pace itself.
    //
    // `fps()` deliberately returns the maximum across peers, because one encoder serves
    // them all and a slow viewer must not hold back a fast one. That alone would flood
    // the slow viewer and silently discard the very report it sent, so each connection
    // also needs its own peer's figure to act on.
    pub fn user_target_fps(&self, id: i32) -> u32 {
        self.users
            .get(&id)
            .map(|u| u.target_fps())
            .unwrap_or(FPS)
            .clamp(ADAPT_MIN_FPS, FPS)
    }

    pub fn store_bitrate(&mut self, bitrate: u32) {
        self.bitrate_store = bitrate;
    }

    pub fn bitrate(&self) -> u32 {
        self.bitrate_store
    }

    // The user's quality choice, scaled for the frame rate actually in use. No estimate
    // is involved and nothing throttles this but the user's own setting.
    fn target_ratio(&self) -> f32 {
        self.latest_quality().ratio() * fps_bitrate_scale(self.fps())
    }

    pub fn ratio(&mut self) -> f32 {
        let target = self.target_ratio();
        if !(BR_MIN_HIGH_RESOLUTION..=BR_MAX * MAX_FPS_BITRATE_SCALE).contains(&self.ratio)
            || (self.ratio - target).abs() > f32::EPSILON
        {
            self.ratio = target;
        }
        self.ratio
    }

    pub fn record(&self) -> bool {
        self.users.iter().any(|u| u.1.record)
    }

    pub fn set_support_changing_quality(&mut self, video_service_name: &str, support: bool) {
        if let Some(display) = self.displays.get_mut(video_service_name) {
            display.support_changing_quality = support;
        }
    }

    pub fn in_vbr_state(&self) -> bool {
        self.abr_config && self.displays.iter().all(|e| e.1.support_changing_quality)
    }
}

// User session management
impl VideoQoS {
    pub fn on_connection_open(&mut self, id: i32) {
        self.users.insert(id, UserData::default());
        self.abr_config = Config::get_option("enable-abr") != "N";
    }

    pub fn on_connection_close(&mut self, id: i32) {
        self.users.remove(&id);
        if self.users.is_empty() {
            *self = Default::default();
        }
    }

    pub fn user_custom_fps(&mut self, id: i32, fps: u32) {
        if fps < MIN_FPS || fps > MAX_FPS {
            return;
        }
        if let Some(user) = self.users.get_mut(&id) {
            user.custom_fps = Some(fps);
        }
    }

    // The peer reporting what its decoder can sustain. This is the whole of the feedback
    // path: it is sent only when the peer's situation changes, and silence means "fine".
    pub fn user_auto_adjust_fps(&mut self, id: i32, fps: u32) {
        if fps < MIN_FPS || fps > MAX_FPS {
            return;
        }
        if let Some(user) = self.users.get_mut(&id) {
            user.auto_adjust_fps = Some(fps);
        }
    }

    pub fn user_image_quality(&mut self, id: i32, image_quality: i32) {
        let convert_quality = |q: i32| -> Quality {
            if q == ImageQuality::Balanced.value() {
                Quality::Balanced
            } else if q == ImageQuality::Low.value() {
                Quality::Low
            } else if q == ImageQuality::Best.value() {
                Quality::Best
            } else {
                let b = ((q >> 8 & 0xFFF) * 2) as f32 / 100.0;
                Quality::Custom(b.clamp(BR_MIN_HIGH_RESOLUTION, BR_MAX))
            }
        };

        let quality = Some((hbb_common::get_time(), convert_quality(image_quality)));
        if let Some(user) = self.users.get_mut(&id) {
            user.quality = quality;
            self.ratio = self.target_ratio();
        }
    }

    pub fn user_record(&mut self, id: i32, v: bool) {
        if let Some(user) = self.users.get_mut(&id) {
            user.record = v;
        }
    }

    // Retained so the TestDelay round trip keeps working for the peer's latency display.
    // Deliberately no longer feeds rate control: inferring congestion from round-trip
    // drift is what made the controller throttle healthy links.
    pub fn user_network_delay(&mut self, _id: i32, _delay: u32) {}

    pub fn user_delay_response_elapsed(&mut self, _id: i32, _elapsed: u128) {}
}

impl VideoQoS {
    pub fn new_display(&mut self, video_service_name: String) {
        self.displays
            .insert(video_service_name, DisplayData::default());
    }

    pub fn remove_display(&mut self, video_service_name: &str) {
        self.displays.remove(video_service_name);
    }

    pub fn update_display_data(&mut self, _video_service_name: &str, _send_counter: usize) {
        self._last_tick = now();
    }

    // Latest quality chosen by any peer.
    pub fn latest_quality(&self) -> Quality {
        self.users
            .iter()
            .map(|(_, u)| u.quality)
            .filter(|q| *q != None)
            .max_by(|a, b| a.unwrap_or_default().0.cmp(&b.unwrap_or_default().0))
            .flatten()
            .unwrap_or((0, Quality::Balanced))
            .1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qos_with(users: &[i32]) -> VideoQoS {
        reset_clock();
        let mut q = VideoQoS::default();
        q.new_display("monitor0".to_owned());
        q.set_support_changing_quality("monitor0", true);
        for id in users {
            q.on_connection_open(*id);
        }
        q
    }

    // Silence means "keep going". This is what makes clients that never learned to report
    // work unchanged: they simply never speak, and are served at the full rate.
    #[test]
    fn a_silent_peer_is_served_at_the_full_rate() {
        let mut q = qos_with(&[1]);
        assert_eq!(q.fps(), FPS);
        assert_eq!(q.spf(), Duration::from_secs_f32(1. / FPS as f32));

        // Round trips and slow sends must not change anything on their own any more.
        q.user_network_delay(1, 5_000);
        q.user_delay_response_elapsed(1, 9_000);
        assert_eq!(q.fps(), FPS, "the network must not be inferred from");
    }

    #[test]
    fn no_peers_at_all_still_yields_a_sane_rate() {
        let q = qos_with(&[]);
        assert_eq!(q.fps(), FPS);
    }

    // The peer speaking up is the entire feedback path.
    #[test]
    fn a_peer_reporting_trouble_is_slowed_down() {
        let mut q = qos_with(&[1]);
        q.user_auto_adjust_fps(1, 15);
        assert_eq!(q.fps(), 15);
    }

    // ...and it recovers the instant the peer says so, with no ramp to crawl back up.
    #[test]
    fn recovery_is_immediate_when_the_peer_says_it_is_fine() {
        let mut q = qos_with(&[1]);
        q.user_auto_adjust_fps(1, 10);
        assert_eq!(q.fps(), 10);
        q.user_auto_adjust_fps(1, FPS);
        assert_eq!(q.fps(), FPS, "no slow ramp - the peer already told us");
    }

    // The flaw this design removes: one struggling viewer used to throttle everybody.
    #[test]
    fn a_slow_peer_does_not_hold_back_a_fast_one() {
        let mut q = qos_with(&[1, 2]);
        q.user_auto_adjust_fps(1, 8);
        q.user_auto_adjust_fps(2, FPS);
        assert_eq!(q.fps(), FPS, "the fast peer must still be served fully");
    }

    // A peer that has gone quiet or died must not drag the session down while the
    // controlled side waits to notice.
    #[test]
    fn a_stalled_peer_stops_mattering_once_it_is_gone() {
        let mut q = qos_with(&[1, 2]);
        q.user_auto_adjust_fps(1, 5);
        q.user_auto_adjust_fps(2, FPS);
        assert_eq!(q.fps(), FPS);
        q.on_connection_close(1);
        assert_eq!(q.fps(), FPS);
    }

    // The per-peer figure must survive the max() that produces the shared encoder rate,
    // otherwise a slow viewer's report is collected and then thrown away.
    #[test]
    fn each_peers_own_rate_is_still_available_after_the_shared_max() {
        let mut q = qos_with(&[1, 2]);
        q.user_auto_adjust_fps(1, 15);
        q.user_auto_adjust_fps(2, FPS);
        assert_eq!(q.fps(), FPS, "encoder serves the fastest peer");
        assert_eq!(
            q.user_target_fps(1),
            15,
            "the slow peer's own rate is retained"
        );
        assert_eq!(q.user_target_fps(2), FPS);
    }

    #[test]
    fn an_unknown_peer_reports_the_full_rate() {
        let q = qos_with(&[1]);
        assert_eq!(q.user_target_fps(999), FPS);
    }

    #[test]
    fn a_peers_explicit_cap_is_honoured() {
        let mut q = qos_with(&[1]);
        q.user_custom_fps(1, 24);
        assert_eq!(q.fps(), 24);
    }

    #[test]
    fn the_lower_of_the_two_caps_wins_for_one_peer() {
        let mut q = qos_with(&[1]);
        q.user_custom_fps(1, 30);
        q.user_auto_adjust_fps(1, 12);
        assert_eq!(q.fps(), 12);
    }

    // Edge: a peer must never be able to talk the session into a slideshow, or above the
    // interactive target.
    #[test]
    fn peer_reports_are_clamped_at_both_ends() {
        let mut q = qos_with(&[1]);
        q.user_auto_adjust_fps(1, 1);
        assert_eq!(q.fps(), ADAPT_MIN_FPS, "floor applies");
        q.user_auto_adjust_fps(1, MAX_FPS);
        assert_eq!(q.fps(), FPS, "ceiling is the interactive target");
    }

    #[test]
    fn out_of_range_reports_are_ignored_rather_than_clamped_in() {
        let mut q = qos_with(&[1]);
        q.user_auto_adjust_fps(1, 30);
        q.user_auto_adjust_fps(1, 0); // below MIN_FPS - nonsense, ignore
        q.user_auto_adjust_fps(1, MAX_FPS + 1); // above MAX_FPS - nonsense, ignore
        assert_eq!(
            q.fps(),
            30,
            "a nonsensical report must not disturb the last good one"
        );
    }

    #[test]
    fn reports_for_an_unknown_peer_are_harmless() {
        let mut q = qos_with(&[1]);
        q.user_auto_adjust_fps(999, 5);
        q.user_custom_fps(999, 5);
        q.user_image_quality(999, ImageQuality::Low.value());
        assert_eq!(q.fps(), FPS);
    }

    // Bitrate follows the user's choice and the rate in use - never a network estimate.
    #[test]
    fn quality_choice_drives_bitrate() {
        let mut q = qos_with(&[1]);
        q.user_image_quality(1, ImageQuality::Best.value());
        let best = q.ratio();
        q.user_image_quality(1, ImageQuality::Low.value());
        let low = q.ratio();
        assert!(best > low, "Best must outspend Low: {best} vs {low}");
    }

    #[test]
    fn bitrate_scales_with_the_rate_actually_in_use() {
        let mut q = qos_with(&[1]);
        q.user_image_quality(1, ImageQuality::Balanced.value());
        let at_full = q.ratio();

        q.user_auto_adjust_fps(1, 15);
        let at_reduced = q.ratio();
        assert!(
            at_reduced < at_full,
            "fewer frames need fewer bits: {at_reduced} vs {at_full}"
        );
    }

    #[test]
    fn a_custom_quality_is_passed_through_untouched() {
        let mut q = qos_with(&[1]);
        // custom encodes the ratio in the high bits: value = ratio*100/2, shifted left 8
        let ratio = 10.0f32;
        let encoded = (((ratio * 100.0 / 2.0) as i32) << 8) | 0xFF;
        q.user_image_quality(1, encoded);
        assert!(matches!(q.latest_quality(), Quality::Custom(_)));
        assert!(
            q.ratio() > scrap::codec::BR_BEST,
            "a high custom ratio must not be capped to a preset"
        );
    }

    #[test]
    fn closing_the_last_peer_resets_cleanly() {
        let mut q = qos_with(&[1]);
        q.user_auto_adjust_fps(1, 5);
        q.user_image_quality(1, ImageQuality::Low.value());
        q.on_connection_close(1);
        assert_eq!(q.fps(), FPS);
        assert!(matches!(q.latest_quality(), Quality::Balanced));
    }

    // The interactive target must clear 60 - capping there was artificial.
    #[test]
    fn the_interactive_target_exceeds_sixty() {
        assert!(
            FPS > 60,
            "interactive target should not stop at 60, got {FPS}"
        );
        assert!(FPS <= MAX_FPS);
        let q = qos_with(&[1]);
        assert!(
            q.fps() > 60,
            "a silent peer should be served above 60, got {}",
            q.fps()
        );
        assert!(q.spf() < Duration::from_millis(17));
    }

    // A peer that wants more than 60 must actually get it, not be quietly clamped.
    #[test]
    fn a_peer_may_request_above_sixty() {
        let mut q = qos_with(&[1]);
        q.user_custom_fps(1, 90);
        assert_eq!(q.fps(), 90);
        q.user_auto_adjust_fps(1, 100);
        assert_eq!(q.fps(), 90, "the lower of the two caps still wins");
    }

    #[test]
    fn fps_bitrate_scale_is_bounded_and_monotonic() {
        assert!(fps_bitrate_scale(1) >= MIN_FPS_BITRATE_SCALE);
        assert!(fps_bitrate_scale(1000) <= MAX_FPS_BITRATE_SCALE);
        assert!(fps_bitrate_scale(60) > fps_bitrate_scale(30));
        assert!((fps_bitrate_scale(REFERENCE_FPS) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_clock_helper_advances_for_deterministic_tests() {
        reset_clock();
        let a = now();
        advance_clock(Duration::from_secs(3));
        assert!(now().duration_since(a) >= Duration::from_secs(3));
    }
}
