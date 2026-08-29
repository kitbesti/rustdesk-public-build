// 24FPS (actually 23.976FPS) is what video professionals ages ago determined to be the
// slowest playback rate that still looks smooth enough to feel real.
// Our eyes can see a slight difference and even though 30FPS actually shows
// more information and is more realistic.
// 60FPS is commonly used in game, teamviewer 12 support this for video editing user.

// how to capture with mouse cursor:
// https://docs.microsoft.com/zh-cn/windows/win32/direct3ddxgi/desktop-dup-api?redirectedfrom=MSDN

// RECORD: The following Project has implemented audio capture, hardware codec and mouse cursor drawn.
// https://github.com/PHZ76/DesktopSharing

// dxgi memory leak issue
// https://stackoverflow.com/questions/47801238/memory-leak-in-creating-direct2d-device
// but per my test, it is more related to AcquireNextFrame,
// https://forums.developer.nvidia.com/t/dxgi-outputduplication-memory-leak-when-using-nv-but-not-amd-drivers/108582

// to-do:
// https://slhck.info/video/2017/03/01/rate-control.html

use super::{display_service::check_display_changed, service::ServiceTmpl, video_qos::VideoQoS, *};
#[cfg(target_os = "linux")]
use crate::common::SimpleCallOnReturn;
#[cfg(target_os = "linux")]
use crate::platform::linux::is_x11;
use crate::privacy_mode::{get_privacy_mode_conn_id, INVALID_PRIVACY_MODE_CONN_ID};
#[cfg(windows)]
use crate::{
    platform::windows::is_process_consent_running,
    privacy_mode::{is_current_privacy_mode_impl, PRIVACY_MODE_IMPL_WIN_MAG},
    ui_interface::is_installed,
};
use hbb_common::{anyhow::anyhow, config};
#[cfg(feature = "hwcodec")]
use scrap::hwcodec::{HwRamEncoder, HwRamEncoderConfig};
#[cfg(feature = "vram")]
use scrap::vram::{VRamEncoder, VRamEncoderConfig};
#[cfg(not(windows))]
use scrap::Capturer;
use scrap::{
    aom::AomEncoderConfig,
    codec::{Encoder, EncoderCfg},
    record::{Recorder, RecorderContext},
    vpxcodec::{VpxEncoderConfig, VpxVideoCodecId},
    CodecFormat, Display, EncodeInput, TraitCapturer, TraitPixelBuffer,
};
#[cfg(windows)]
use std::sync::Once;
use std::{
    io::ErrorKind::WouldBlock,
    ops::{Deref, DerefMut},
    time::{self, Duration, Instant},
};

pub const OPTION_REFRESH: &'static str = "refresh";

// How long the capture loop waits for a change once the screen has been still, and how
// many still ticks earn that longer wait. See the loop for why it is short.
const IDLE_WAIT: Duration = Duration::from_millis(50);
const IDLE_BACKOFF_AFTER: u32 = 3;
// Once the screen is still, the last picture is coded again up to this many times, one
// per idle tick. A big change (a scroll, a window switch) is coded coarsely to fit its
// frame budget and the parts that then stay still would keep that quality for as long
// as nothing else changed; coding the same picture again lets the encoder's cyclic
// refresh bring them up to the quantizer floor while the link would otherwise be idle.
// Only where the user asked for the best picture: it is bandwidth spent on detail.
// A frame this small (in pixels per byte) carried next to nothing: two such passes in a
// row mean the picture has converged and is left alone until a change at least this
// big is coded, since a small one (a caret, a line of text) is coded at the floor
// already and the rest of the run would only cost the peer decodes.
const REFINE_PASSES: u32 = 10;
const REFINE_DONE_PX_PER_BYTE: usize = 128;
// Hardware encoders are configured once, so they get the interactive target rather
// than the current adaptive rate; the rate control only uses it to size its budget.
#[cfg(any(feature = "vram", feature = "hwcodec"))]
const INTERACTIVE_FPS: i32 = super::video_qos::FPS as i32;
// Roughly three seconds between key frames.
//
// This was previously `FPS * 3 / 10`, i.e. a key frame every 18 frames — three per
// second at the interactive target. A key frame costs many times what an inter-coded
// frame costs, so at that spacing most of the bitrate budget went on redundant full
// pictures and the picture stayed soft no matter how much bandwidth was free.
const INTERACTIVE_KEYFRAME_INTERVAL: usize = (super::video_qos::FPS as usize) * 3;

// Set by a connection that has fallen too far behind to deliver its backlog.
//
// Discarding queued video is only safe at a key frame, and with a multi-second GOP there
// is usually none available. Rather than deliver a stale backlog or corrupt the peer's
// decoder, the connection asks for a key frame and cuts to it when it arrives.
static KEY_FRAME_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Ask the capture loop for a key frame at the next opportunity.
pub fn request_key_frame() {
    KEY_FRAME_REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
}

lazy_static::lazy_static! {
    pub static ref VIDEO_QOS: Arc<Mutex<VideoQoS>> = Default::default();
    pub static ref IS_UAC_RUNNING: Arc<Mutex<bool>> = Default::default();
    pub static ref IS_FOREGROUND_WINDOW_ELEVATED: Arc<Mutex<bool>> = Default::default();
    static ref SCREENSHOTS: Mutex<HashMap<(VideoSource, usize), Screenshot>> = Default::default();
}

struct Screenshot {
    sid: String,
    tx: Sender,
    restore_vram: bool,
}

// Flow control lives in the connection send loop now: it measures how long a video
// write blocks and, for ack-required peers, how many frames are still unconfirmed,
// then reports congestion to `VideoQoS`. The previous ack-notifier machinery here
// had lost its only consumer, so every frame pushed a tuple into an unbounded
// channel that was never drained.

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum VideoSource {
    Monitor,
    Camera,
}

impl VideoSource {
    pub fn service_name_prefix(&self) -> &'static str {
        match self {
            VideoSource::Monitor => "monitor",
            VideoSource::Camera => "camera",
        }
    }

    pub fn is_monitor(&self) -> bool {
        matches!(self, VideoSource::Monitor)
    }

    pub fn is_camera(&self) -> bool {
        matches!(self, VideoSource::Camera)
    }
}

#[derive(Clone)]
pub struct VideoService {
    sp: GenericService,
    idx: usize,
    source: VideoSource,
}

impl Deref for VideoService {
    type Target = ServiceTmpl<ConnInner>;

    fn deref(&self) -> &Self::Target {
        &self.sp
    }
}

impl DerefMut for VideoService {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.sp
    }
}

pub fn get_service_name(source: VideoSource, idx: usize) -> String {
    format!("{}{}", source.service_name_prefix(), idx)
}

pub fn new(source: VideoSource, idx: usize) -> GenericService {
    let vs = VideoService {
        sp: GenericService::new(get_service_name(source, idx), true),
        idx,
        source,
    };
    GenericService::run(&vs, run);
    vs.sp
}

// Capturer object is expensive, avoiding to create it frequently.
fn create_capturer(
    privacy_mode_id: i32,
    display: Display,
    _current: usize,
    _portable_service_running: bool,
) -> ResultType<Box<dyn TraitCapturer>> {
    #[cfg(not(windows))]
    let c: Option<Box<dyn TraitCapturer>> = None;
    #[cfg(windows)]
    let mut c: Option<Box<dyn TraitCapturer>> = None;
    if privacy_mode_id > 0 {
        #[cfg(windows)]
        {
            // Windows Mode 1 can cover every local monitor with overlay windows,
            // but the legacy magnifier capture backend is still single-monitor
            // constrained. Keep display-switch gating aligned with that backend
            // limit, not just the overlay coverage.
            if let Some(c1) = crate::privacy_mode::win_mag::create_capturer(
                privacy_mode_id,
                display.origin(),
                display.width(),
                display.height(),
            )? {
                c = Some(Box::new(c1));
            }
        }
    }

    match c {
        Some(c1) => return Ok(c1),
        None => {
            #[cfg(windows)]
            {
                log::debug!("Create capturer dxgi|gdi");
                return crate::portable_service::client::create_capturer(
                    _current,
                    display,
                    _portable_service_running,
                );
            }
            #[cfg(not(windows))]
            {
                log::debug!("Create capturer from scrap");
                return Ok(Box::new(
                    Capturer::new(display).with_context(|| "Failed to create capturer")?,
                ));
            }
        }
    };
}

// This function works on privacy mode. Windows only for now.
pub fn test_create_capturer(
    privacy_mode_id: i32,
    display_idx: usize,
    timeout_millis: u64,
) -> String {
    let test_begin = Instant::now();
    loop {
        let err = match Display::all() {
            Ok(mut displays) => {
                if displays.len() <= display_idx {
                    anyhow!(
                        "Failed to get display {}, the displays' count is {}",
                        display_idx,
                        displays.len()
                    )
                } else {
                    let display = displays.remove(display_idx);
                    match create_capturer(privacy_mode_id, display, display_idx, false) {
                        Ok(_) => return "".to_owned(),
                        Err(e) => e,
                    }
                }
            }
            Err(e) => e.into(),
        };
        if test_begin.elapsed().as_millis() >= timeout_millis as _ {
            return err.to_string();
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

// Note: This function is extremely expensive, do not call it frequently.
#[cfg(windows)]
fn check_uac_switch(privacy_mode_id: i32, capturer_privacy_mode_id: i32) -> ResultType<()> {
    if capturer_privacy_mode_id != INVALID_PRIVACY_MODE_CONN_ID
        && is_current_privacy_mode_impl(PRIVACY_MODE_IMPL_WIN_MAG)
    {
        if !is_installed() {
            if privacy_mode_id != capturer_privacy_mode_id {
                if !is_process_consent_running()? {
                    bail!("consent.exe is not running");
                }
            }
            if is_process_consent_running()? {
                bail!("consent.exe is running");
            }
        }
    }
    Ok(())
}

pub(super) struct CapturerInfo {
    pub origin: (i32, i32),
    pub width: usize,
    pub height: usize,
    pub ndisplay: usize,
    pub current: usize,
    pub privacy_mode_id: i32,
    pub _capturer_privacy_mode_id: i32,
    pub capturer: Box<dyn TraitCapturer>,
}

impl Deref for CapturerInfo {
    type Target = Box<dyn TraitCapturer>;

    fn deref(&self) -> &Self::Target {
        &self.capturer
    }
}

impl DerefMut for CapturerInfo {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.capturer
    }
}

fn get_capturer_monitor(
    current: usize,
    portable_service_running: bool,
) -> ResultType<CapturerInfo> {
    #[cfg(target_os = "linux")]
    {
        if !is_x11() {
            return super::wayland::get_capturer_for_display(current);
        }
    }

    let mut displays = Display::all()?;
    let ndisplay = displays.len();
    if ndisplay <= current {
        bail!(
            "Failed to get display {}, displays len: {}",
            current,
            ndisplay
        );
    }

    let display = displays.remove(current);

    #[cfg(target_os = "linux")]
    if let Display::X11(inner) = &display {
        if let Err(err) = inner.get_shm_status() {
            log::warn!(
                "MIT-SHM extension not working properly on select X11 server: {:?}",
                err
            );
        }
    }

    let (origin, width, height) = (display.origin(), display.width(), display.height());
    let name = display.name();
    log::debug!(
        "#displays={}, current={}, origin: {:?}, width={}, height={}, cpus={}/{}, name:{}",
        ndisplay,
        current,
        &origin,
        width,
        height,
        num_cpus::get_physical(),
        num_cpus::get(),
        &name,
    );

    let privacy_mode_id = get_privacy_mode_conn_id().unwrap_or(INVALID_PRIVACY_MODE_CONN_ID);
    #[cfg(not(windows))]
    let capturer_privacy_mode_id = privacy_mode_id;
    #[cfg(windows)]
    let mut capturer_privacy_mode_id = privacy_mode_id;
    #[cfg(windows)]
    {
        if capturer_privacy_mode_id != INVALID_PRIVACY_MODE_CONN_ID
            && is_current_privacy_mode_impl(PRIVACY_MODE_IMPL_WIN_MAG)
        {
            if !is_installed() {
                if is_process_consent_running()? {
                    capturer_privacy_mode_id = INVALID_PRIVACY_MODE_CONN_ID;
                }
            }
        }
    }
    log::debug!(
        "Try create capturer with capturer privacy mode id {}",
        capturer_privacy_mode_id,
    );

    if privacy_mode_id != INVALID_PRIVACY_MODE_CONN_ID {
        if privacy_mode_id != capturer_privacy_mode_id {
            log::info!("In privacy mode, but show UAC prompt window for now");
        } else {
            log::info!("In privacy mode, the peer side cannot watch the screen");
        }
    }
    let capturer = create_capturer(
        capturer_privacy_mode_id,
        display,
        current,
        portable_service_running,
    )?;
    Ok(CapturerInfo {
        origin,
        width,
        height,
        ndisplay,
        current,
        privacy_mode_id,
        _capturer_privacy_mode_id: capturer_privacy_mode_id,
        capturer,
    })
}

fn get_capturer_camera(current: usize) -> ResultType<CapturerInfo> {
    let cameras = camera::Cameras::get_sync_cameras();
    let ncamera = cameras.len();
    if ncamera <= current {
        bail!("Failed to get camera {}, cameras len: {}", current, ncamera,);
    }
    let Some(camera) = cameras.get(current) else {
        bail!(
            "Camera of index {} doesn't exist or platform not supported",
            current
        );
    };
    let capturer = camera::Cameras::get_capturer(current)?;
    let (width, height) = (camera.width as usize, camera.height as usize);
    let origin = (camera.x as i32, camera.y as i32);
    let name = &camera.name;
    let privacy_mode_id = get_privacy_mode_conn_id().unwrap_or(INVALID_PRIVACY_MODE_CONN_ID);
    let _capturer_privacy_mode_id = privacy_mode_id;
    log::debug!(
        "#cameras={}, current={}, origin: {:?}, width={}, height={}, cpus={}/{}, name:{}",
        ncamera,
        current,
        &origin,
        width,
        height,
        num_cpus::get_physical(),
        num_cpus::get(),
        name,
    );
    return Ok(CapturerInfo {
        origin,
        width,
        height,
        ndisplay: ncamera,
        current,
        privacy_mode_id,
        _capturer_privacy_mode_id: privacy_mode_id,
        capturer,
    });
}
fn get_capturer(
    source: VideoSource,
    current: usize,
    portable_service_running: bool,
) -> ResultType<CapturerInfo> {
    match source {
        VideoSource::Monitor => get_capturer_monitor(current, portable_service_running),
        VideoSource::Camera => get_capturer_camera(current),
    }
}

fn run(vs: VideoService) -> ResultType<()> {
    let mut _raii = Raii::new(vs.sp.name());
    // Wayland only support one video capturer for now. It is ok to call ensure_inited() here.
    //
    // ensure_inited() is needed because clear() may be called.
    // to-do: wayland ensure_inited should pass current display index.
    // But for now, we do not support multi-screen capture on wayland.
    #[cfg(target_os = "linux")]
    super::wayland::ensure_inited()?;
    #[cfg(target_os = "linux")]
    let _wayland_call_on_ret = {
        // Increment active display count when starting
        let _display_count = super::wayland::increment_active_display_count();

        SimpleCallOnReturn {
            b: true,
            f: Box::new(|| {
                // Decrement active display count and only clear if this was the last display
                let remaining_count = super::wayland::decrement_active_display_count();
                if remaining_count == 0 {
                    super::wayland::clear();
                }
            }),
        }
    };

    #[cfg(windows)]
    let last_portable_service_running = crate::portable_service::client::running();
    #[cfg(not(windows))]
    let last_portable_service_running = false;

    let display_idx = vs.idx;
    let sp = vs.sp;
    let mut c = get_capturer(vs.source, display_idx, last_portable_service_running)?;
    #[cfg(windows)]
    if !scrap::codec::enable_directx_capture() && !c.is_gdi() {
        log::info!("disable dxgi with option, fall back to gdi");
        c.set_gdi();
    }
    let mut video_qos = VIDEO_QOS.lock().unwrap();
    let mut spf = video_qos.spf();
    let mut quality = video_qos.ratio();
    let record_incoming = config::option2bool(
        "allow-auto-record-incoming",
        &Config::get_option("allow-auto-record-incoming"),
    );
    let client_record = video_qos.record();
    drop(video_qos);
    let (mut encoder, encoder_cfg, codec_format, use_i444, recorder) = match setup_encoder(
        &c,
        sp.name(),
        quality,
        client_record,
        record_incoming,
        last_portable_service_running,
        vs.source,
        display_idx,
    ) {
        Ok(result) => result,
        Err(err) => {
            log::error!("Failed to create encoder: {err:?}, fallback to VP9");
            Encoder::set_fallback(&EncoderCfg::VPX(VpxEncoderConfig {
                width: c.width as _,
                height: c.height as _,
                quality,
                codec: VpxVideoCodecId::VP9,
                keyframe_interval: None,
            }));
            setup_encoder(
                &c,
                sp.name(),
                quality,
                client_record,
                record_incoming,
                last_portable_service_running,
                vs.source,
                display_idx,
            )?
        }
    };
    #[cfg(feature = "vram")]
    c.set_output_texture(encoder.input_texture());
    #[cfg(target_os = "android")]
    if vs.source.is_monitor() {
        if let Err(e) = check_change_scale(encoder.is_hardware()) {
            try_broadcast_display_changed(&sp, display_idx, &c, true).ok();
            bail!(e);
        }
    }
    VIDEO_QOS.lock().unwrap().store_bitrate(encoder.bitrate());
    VIDEO_QOS
        .lock()
        .unwrap()
        .set_support_changing_quality(&sp.name(), encoder.support_changing_quality());
    log::info!("initial quality: {quality:?}");

    if sp.is_option_true(OPTION_REFRESH) {
        sp.set_option_bool(OPTION_REFRESH, false);
    }

    let start = time::Instant::now();
    let mut last_check_displays = time::Instant::now();
    #[cfg(windows)]
    let mut try_gdi = 1;
    #[cfg(windows)]
    log::info!("gdi: {}", c.is_gdi());
    #[cfg(windows)]
    start_uac_elevation_check();

    #[cfg(target_os = "linux")]
    let mut would_block_count = 0u32;
    let mut yuv = Vec::new();
    let mut mid_data = Vec::new();
    let mut repeat_encode_counter = 0;
    let repeat_encode_max = 10;
    let mut encode_fail_counter = 0;
    let mut first_frame = true;
    // A key frame was asked for and the capturer has produced nothing since. The peer
    // that asked shows nothing until it gets one, so a still screen must not starve it.
    let mut key_frame_pending = false;
    let mut idle_streak = 0u32;
    let mut refine_passes = 0u32;
    let mut refine_small = 0u32;
    let mut refined = false;
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    let mut seen_presses = 0u64;
    let capture_width = c.width;
    let capture_height = c.height;
    let (mut second_instant, mut send_counter) = (Instant::now(), 0);
    // Capture-side counters. These separate "the capturer produced nothing" from "the
    // network could not carry it", which the connection-side line alone cannot tell apart.
    let (mut cap_stat_instant, mut cap_captured, mut cap_encoded, mut cap_blank) =
        (Instant::now(), 0usize, 0usize, 0usize);
    let mut cap_refined = 0usize;
    // Time spent inside the capturer itself. On X11 this is a blocking round trip that
    // makes the X server read the whole framebuffer, so it sets a hard ceiling on frame
    // rate that no amount of encoder or network tuning can lift. Measuring it makes that
    // ceiling visible instead of leaving `target` looking achievable when it is not.
    let mut cap_busy = Duration::ZERO;

    while sp.ok() {
        #[cfg(windows)]
        check_uac_switch(c.privacy_mode_id, c._capturer_privacy_mode_id)?;
        check_qos(
            &mut encoder,
            &mut quality,
            &mut spf,
            client_record,
            &mut send_counter,
            &mut second_instant,
            &sp.name(),
        )?;
        if sp.is_option_true(OPTION_REFRESH) {
            if vs.source.is_monitor() {
                let _ = try_broadcast_display_changed(&sp, display_idx, &c, true);
            }
            log::info!("switch to refresh");
            bail!("SWITCH");
        }
        if codec_format != Encoder::negotiated_codec() {
            log::info!(
                "switch due to codec changed, {:?} -> {:?}",
                codec_format,
                Encoder::negotiated_codec()
            );
            bail!("SWITCH");
        }
        #[cfg(windows)]
        if last_portable_service_running != crate::portable_service::client::running() {
            log::info!("switch due to portable service running changed");
            bail!("SWITCH");
        }
        if Encoder::use_i444(&encoder_cfg) != use_i444 {
            log::info!("switch due to i444 changed");
            bail!("SWITCH");
        }
        #[cfg(all(windows, feature = "vram"))]
        if c.is_gdi() && encoder.input_texture() {
            log::info!("changed to gdi when using vram");
            VRamEncoder::set_fallback_gdi(sp.name(), true);
            bail!("SWITCH");
        }
        if vs.source.is_monitor() {
            check_privacy_mode_changed(&sp, display_idx, &c)?;
        }
        #[cfg(windows)]
        {
            if crate::platform::windows::desktop_changed()
                && !crate::portable_service::client::running()
            {
                bail!("Desktop changed");
            }
        }
        let now = time::Instant::now();
        if vs.source.is_monitor() && last_check_displays.elapsed().as_millis() > 1000 {
            last_check_displays = now;
            // This check may be redundant, but it is better to be safe.
            // The previous check in `sp.is_option_true(OPTION_REFRESH)` block may be enough.
            try_broadcast_display_changed(&sp, display_idx, &c, false)?;
        }

        if KEY_FRAME_REQUESTED.swap(false, std::sync::atomic::Ordering::Relaxed) {
            encoder.codec.request_key_frame();
            c.force_next_frame();
            key_frame_pending = true;
        }

        // Capturers that can wait for a change signal do so inside `frame`, so once the
        // screen has been still for a few ticks the wait is lengthened: a change still
        // wakes it at once, and the loop stops spinning at the target rate to discover
        // nothing happened. The wait stays short because it bounds how late this loop
        // notices requests aimed at it: a refresh, a codec change, a key-frame request.
        // Capturers that ignore the timeout are unaffected; the pacing sleep below still
        // holds them to `spf`. Repeat encoders keep the short cadence until their repeats
        // are spent, since those repeats are what they exist for.
        let wait = if idle_streak >= IDLE_BACKOFF_AFTER
            && (encoder.latency_free() || repeat_encode_counter >= repeat_encode_max)
        {
            IDLE_WAIT.max(spf)
        } else {
            spf
        };

        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        {
            let presses = super::input_service::PRESSES.load(std::sync::atomic::Ordering::Relaxed);
            if presses != seen_presses {
                seen_presses = presses;
                c.input_hint();
            }
        }

        let time = now - start;
        let ms = (time.as_secs() * 1000 + time.subsec_millis() as u64) as i64;
        let cap_began = Instant::now();
        let res = match c.frame(wait) {
            Ok(frame) => {
                repeat_encode_counter = 0;
                idle_streak = 0;
                refine_passes = 0;
                refine_small = 0;
                key_frame_pending = false;
                cap_captured += 1;
                // Only real grabs count towards the capture cost; an idle tick is mostly
                // the capturer waiting for a change, which says nothing about the ceiling.
                cap_busy += cap_began.elapsed();
                if frame.valid() {
                    let screenshot_key = (vs.source, display_idx);
                    let screenshot = SCREENSHOTS.lock().unwrap().remove(&screenshot_key);
                    if let Some(mut screenshot) = screenshot {
                        let restore_vram = screenshot.restore_vram;
                        let (msg, w, h, data) = match &frame {
                            scrap::Frame::PixelBuffer(f) => match get_rgba_from_pixelbuf(f) {
                                Ok(rgba) => ("".to_owned(), f.width(), f.height(), rgba),
                                Err(e) => {
                                    let serr = e.to_string();
                                    log::error!(
                                        "Failed to convert the pix format into rgba, {}",
                                        &serr
                                    );
                                    (format!("Convert pixfmt: {}", serr), 0, 0, vec![])
                                }
                            },
                            scrap::Frame::Texture(_) => {
                                if restore_vram {
                                    // Already set one time, just ignore to break infinite loop.
                                    // Though it's unreachable, this branch is kept to avoid infinite loop.
                                    (
                                        "Please change codec and try again.".to_owned(),
                                        0,
                                        0,
                                        vec![],
                                    )
                                } else {
                                    #[cfg(all(windows, feature = "vram"))]
                                    VRamEncoder::set_not_use(sp.name(), true);
                                    screenshot.restore_vram = true;
                                    SCREENSHOTS
                                        .lock()
                                        .unwrap()
                                        .insert(screenshot_key, screenshot);
                                    _raii.try_vram = false;
                                    bail!("SWITCH");
                                }
                            }
                        };
                        std::thread::spawn(move || {
                            handle_screenshot(screenshot, msg, w, h, data);
                        });
                        if restore_vram {
                            bail!("SWITCH");
                        }
                    }

                    let frame = frame.to(encoder.yuvfmt(), &mut yuv, &mut mid_data)?;
                    let bytes = handle_one_frame(
                        display_idx,
                        &sp,
                        frame,
                        ms,
                        &mut encoder,
                        recorder.clone(),
                        &mut encode_fail_counter,
                        &mut first_frame,
                        capture_width,
                        capture_height,
                    )?;
                    if bytes >= capture_width * capture_height / REFINE_DONE_PX_PER_BYTE {
                        refined = false;
                    }
                    send_counter += 1;
                    cap_encoded += 1;
                }
                #[cfg(windows)]
                {
                    #[cfg(feature = "vram")]
                    if try_gdi == 1 && !c.is_gdi() {
                        VRamEncoder::set_fallback_gdi(sp.name(), false);
                    }
                    try_gdi = 0;
                }
                Ok(())
            }
            Err(err) => Err(err),
        };

        match res {
            Err(ref e) if e.kind() == WouldBlock => {
                cap_blank += 1;
                idle_streak = idle_streak.saturating_add(1);
                #[cfg(windows)]
                if try_gdi > 0 && !c.is_gdi() {
                    if try_gdi > 3 {
                        c.set_gdi();
                        try_gdi = 0;
                        log::info!("No image, fall back to gdi");
                    }
                    try_gdi += 1;
                }
                #[cfg(target_os = "linux")]
                {
                    would_block_count += 1;
                    if !is_x11() {
                        if would_block_count >= 100 {
                            // to-do: Unknown reason for WouldBlock 100 times (seconds = 100 * 1 / fps)
                            // https://github.com/rustdesk/rustdesk/blob/63e6b2f8ab51743e77a151e2b7ff18816f5fa2fb/libs/scrap/src/common/wayland.rs#L81
                            //
                            // Do not reset the capturer for now, as it will cause the prompt to show every few minutes.
                            // https://github.com/rustdesk/rustdesk/issues/4276
                            //
                            // super::wayland::clear();
                            // bail!("Wayland capturer none 100 times, try restart capture");
                        }
                    }
                }
                // yuv.len() > 0 means the last frame was not a texture and can be encoded
                // again.
                if key_frame_pending && yuv.len() > 0 {
                    // The screen is still, so the capturer has nothing new; the last
                    // picture, coded as a key frame, is exactly what the peer needs.
                    key_frame_pending = false;
                    handle_one_frame(
                        display_idx,
                        &sp,
                        EncodeInput::YUV(&yuv),
                        ms,
                        &mut encoder,
                        recorder.clone(),
                        &mut encode_fail_counter,
                        &mut first_frame,
                        capture_width,
                        capture_height,
                    )?;
                    send_counter += 1;
                    cap_encoded += 1;
                } else if !encoder.latency_free() && yuv.len() > 0 {
                    if repeat_encode_counter < repeat_encode_max {
                        repeat_encode_counter += 1;
                        handle_one_frame(
                            display_idx,
                            &sp,
                            EncodeInput::YUV(&yuv),
                            ms,
                            &mut encoder,
                            recorder.clone(),
                            &mut encode_fail_counter,
                            &mut first_frame,
                            capture_width,
                            capture_height,
                        )?;
                        send_counter += 1;
                        cap_encoded += 1;
                    }
                } else if !refined
                    && refine_passes < REFINE_PASSES
                    && idle_streak >= IDLE_BACKOFF_AFTER
                    && yuv.len() > 0
                    && !encoder.is_hardware()
                    && refines_still_picture()
                {
                    refine_passes += 1;
                    let bytes = handle_one_frame(
                        display_idx,
                        &sp,
                        EncodeInput::YUV(&yuv),
                        ms,
                        &mut encoder,
                        recorder.clone(),
                        &mut encode_fail_counter,
                        &mut first_frame,
                        capture_width,
                        capture_height,
                    )?;
                    if bytes < capture_width * capture_height / REFINE_DONE_PX_PER_BYTE {
                        refine_small += 1;
                        if refine_small >= 2 {
                            refined = true;
                        }
                    } else {
                        refine_small = 0;
                    }
                    send_counter += 1;
                    cap_encoded += 1;
                    cap_refined += 1;
                }
            }
            Err(err) => {
                // This check may be redundant, but it is better to be safe.
                // The previous check in `sp.is_option_true(OPTION_REFRESH)` block may be enough.
                if vs.source.is_monitor() {
                    try_broadcast_display_changed(&sp, display_idx, &c, true)?;
                }

                #[cfg(windows)]
                if !c.is_gdi() {
                    c.set_gdi();
                    log::info!("dxgi error, fall back to gdi: {:?}", err);
                    continue;
                }
                return Err(err.into());
            }
            _ => {
                #[cfg(target_os = "linux")]
                {
                    would_block_count = 0;
                }
            }
        }

        // One line per second, greppable as "rd-stat capture".
        //
        //   cap    frames the capturer returned
        //   enc    frames actually encoded and handed to subscribers
        //   idle   ticks where the screen had not changed (WouldBlock) - expected when
        //          nothing is moving, and the reason an idle desktop sends a few kbps
        //   refine still pictures coded again for detail (counted in enc as well)
        //   spf    the interval QoS is pacing the loop at
        //
        // enc well below 1/spf while idle is low means the capturer or encoder is the
        // bottleneck, not the link.
        {
            let ms = cap_stat_instant.elapsed().as_millis();
            if ms >= 1000 {
                cap_stat_instant = Instant::now();
                let per_s = |n: usize| (n as u128 * 1000 / ms.max(1)) as usize;
                let grab_ms = cap_busy.as_secs_f32() * 1000.0 / cap_captured.max(1) as f32;
                // A still screen says nothing new every second; only log seconds with work.
                if cap_captured + cap_encoded > 0 {
                    log::info!(
                        "rd-stat capture {}: cap={}/s enc={}/s idle={}/s refine={}/s grab={:.1}ms ceiling={:.0}fps spf={:.1}ms",
                        sp.name(),
                        per_s(cap_captured),
                        per_s(cap_encoded),
                        per_s(cap_blank),
                        per_s(cap_refined),
                        grab_ms,
                        if grab_ms > 0.0 { 1000.0 / grab_ms } else { 0.0 },
                        spf.as_secs_f32() * 1000.0,
                    );
                }
                cap_busy = Duration::ZERO;
                cap_captured = 0;
                cap_encoded = 0;
                cap_blank = 0;
                cap_refined = 0;
            }
        }

        let elapsed = now.elapsed();
        // may need to enable frame(timeout)
        log::trace!("{:?} {:?}", time::Instant::now(), elapsed);
        if elapsed < spf {
            std::thread::sleep(spf - elapsed);
        }
    }

    Ok(())
}

struct Raii {
    name: String,
    try_vram: bool,
}

impl Raii {
    fn new(name: String) -> Self {
        log::info!("new video service: {}", name);
        VIDEO_QOS.lock().unwrap().new_display(name.clone());
        Raii {
            name,
            try_vram: true,
        }
    }
}

impl Drop for Raii {
    fn drop(&mut self) {
        log::info!("stop video service: {}", self.name);
        #[cfg(feature = "vram")]
        if self.try_vram {
            VRamEncoder::set_not_use(self.name.clone(), false);
        }
        #[cfg(feature = "vram")]
        Encoder::update(scrap::codec::EncodingUpdate::Check);
        VIDEO_QOS.lock().unwrap().remove_display(&self.name);
    }
}

fn setup_encoder(
    c: &CapturerInfo,
    name: String,
    quality: f32,
    client_record: bool,
    record_incoming: bool,
    last_portable_service_running: bool,
    source: VideoSource,
    display_idx: usize,
) -> ResultType<(
    Encoder,
    EncoderCfg,
    CodecFormat,
    bool,
    Arc<Mutex<Option<Recorder>>>,
)> {
    let encoder_cfg = get_encoder_config(
        &c,
        name.to_string(),
        quality,
        client_record || record_incoming,
        last_portable_service_running,
        source,
    );
    Encoder::set_fallback(&encoder_cfg);
    let codec_format = Encoder::negotiated_codec();
    let recorder = get_recorder(record_incoming, display_idx, source == VideoSource::Camera);
    let use_i444 = Encoder::use_i444(&encoder_cfg);
    let encoder = Encoder::new(encoder_cfg.clone(), use_i444)?;
    Ok((encoder, encoder_cfg, codec_format, use_i444, recorder))
}

fn get_encoder_config(
    c: &CapturerInfo,
    _name: String,
    quality: f32,
    record: bool,
    _portable_service: bool,
    _source: VideoSource,
) -> EncoderCfg {
    #[cfg(all(windows, feature = "vram"))]
    if _portable_service || c.is_gdi() || _source == VideoSource::Camera {
        log::info!("gdi:{}, portable:{}", c.is_gdi(), _portable_service);
        VRamEncoder::set_not_use(_name, true);
    }
    #[cfg(feature = "vram")]
    Encoder::update(scrap::codec::EncodingUpdate::Check);
    // https://www.wowza.com/community/t/the-correct-keyframe-interval-in-obs-studio/95162
    let keyframe_interval = if record {
        Some(240)
    } else {
        Some(INTERACTIVE_KEYFRAME_INTERVAL.max(1))
    };
    let negotiated_codec = Encoder::negotiated_codec();
    match negotiated_codec {
        CodecFormat::H264 | CodecFormat::H265 => {
            #[cfg(feature = "vram")]
            if let Some(feature) = VRamEncoder::try_get(&c.device(), negotiated_codec) {
                return EncoderCfg::VRAM(VRamEncoderConfig {
                    device: c.device(),
                    width: c.width,
                    height: c.height,
                    fps: INTERACTIVE_FPS,
                    quality,
                    feature,
                    keyframe_interval,
                });
            }
            #[cfg(feature = "hwcodec")]
            if let Some(hw) = HwRamEncoder::try_get(negotiated_codec) {
                return EncoderCfg::HWRAM(HwRamEncoderConfig {
                    name: hw.name,
                    mc_name: hw.mc_name,
                    width: c.width,
                    height: c.height,
                    fps: INTERACTIVE_FPS,
                    quality,
                    keyframe_interval,
                });
            }
            EncoderCfg::VPX(VpxEncoderConfig {
                width: c.width as _,
                height: c.height as _,
                quality,
                codec: VpxVideoCodecId::VP9,
                keyframe_interval,
            })
        }
        format @ (CodecFormat::VP8 | CodecFormat::VP9) => EncoderCfg::VPX(VpxEncoderConfig {
            width: c.width as _,
            height: c.height as _,
            quality,
            codec: if format == CodecFormat::VP8 {
                VpxVideoCodecId::VP8
            } else {
                VpxVideoCodecId::VP9
            },
            keyframe_interval,
        }),
        CodecFormat::AV1 => EncoderCfg::AOM(AomEncoderConfig {
            width: c.width as _,
            height: c.height as _,
            quality,
            keyframe_interval,
        }),
        _ => EncoderCfg::VPX(VpxEncoderConfig {
            width: c.width as _,
            height: c.height as _,
            quality,
            codec: VpxVideoCodecId::VP9,
            keyframe_interval,
        }),
    }
}

fn get_recorder(
    record_incoming: bool,
    display_idx: usize,
    camera: bool,
) -> Arc<Mutex<Option<Recorder>>> {
    #[cfg(windows)]
    let root = crate::platform::is_root();
    #[cfg(not(windows))]
    let root = false;
    let recorder = if record_incoming {
        use crate::hbbs_http::record_upload;

        let tx = if record_upload::is_enable() {
            let (tx, rx) = std::sync::mpsc::channel();
            record_upload::run(rx);
            Some(tx)
        } else {
            None
        };
        Recorder::new(RecorderContext {
            server: true,
            id: Config::get_id(),
            dir: crate::ui_interface::video_save_directory(root),
            display_idx,
            camera,
            tx,
        })
        .map_or(Default::default(), |r| Arc::new(Mutex::new(Some(r))))
    } else {
        Default::default()
    };

    recorder
}

#[cfg(target_os = "android")]
fn check_change_scale(hardware: bool) -> ResultType<()> {
    use hbb_common::config::keys::OPTION_ENABLE_ANDROID_SOFTWARE_ENCODING_HALF_SCALE as SCALE_SOFT;

    // isStart flag is set at the end of startCapture() in Android, wait it to be set.
    let n = 60; // 3s
    for i in 0..n {
        if scrap::is_start() == Some(true) {
            log::info!("start flag is set");
            break;
        }
        log::info!("wait for start, {i}");
        std::thread::sleep(Duration::from_millis(50));
        if i == n - 1 {
            log::error!("wait for start timeout");
        }
    }
    let screen_size = scrap::screen_size();
    let scale_soft = hbb_common::config::option2bool(SCALE_SOFT, &Config::get_option(SCALE_SOFT));
    let half_scale = !hardware && scale_soft;
    log::info!("hardware: {hardware}, scale_soft: {scale_soft}, screen_size: {screen_size:?}",);
    scrap::android::call_main_service_set_by_name(
        "half_scale",
        Some(half_scale.to_string().as_str()),
        None,
    )
    .ok();
    let old_scale = screen_size.2;
    let new_scale = scrap::screen_size().2;
    log::info!("old_scale: {old_scale}, new_scale: {new_scale}");
    if old_scale != new_scale {
        log::info!("switch due to scale changed, {old_scale} -> {new_scale}");
        // switch is not a must, but it is better to do so.
        bail!("SWITCH");
    }
    Ok(())
}

fn check_privacy_mode_changed(
    sp: &GenericService,
    display_idx: usize,
    ci: &CapturerInfo,
) -> ResultType<()> {
    let privacy_mode_id_2 = get_privacy_mode_conn_id().unwrap_or(INVALID_PRIVACY_MODE_CONN_ID);
    if ci.privacy_mode_id != privacy_mode_id_2 {
        if privacy_mode_id_2 != INVALID_PRIVACY_MODE_CONN_ID {
            let msg_out = crate::common::make_privacy_mode_msg(
                back_notification::PrivacyModeState::PrvOnByOther,
                "".to_owned(),
            );
            sp.send_to_others(msg_out, privacy_mode_id_2);
        }
        log::info!("switch due to privacy mode changed");
        try_broadcast_display_changed(&sp, display_idx, ci, true).ok();
        bail!("SWITCH");
    }
    Ok(())
}

#[inline]
// Whether the user's quality choice is one that spends spare bandwidth on detail.
fn refines_still_picture() -> bool {
    use scrap::codec::{Quality, BR_BEST};
    match VIDEO_QOS.lock().unwrap().latest_quality() {
        Quality::Best => true,
        Quality::Custom(v) => v >= BR_BEST,
        _ => false,
    }
}

fn handle_one_frame(
    display: usize,
    sp: &GenericService,
    frame: EncodeInput,
    ms: i64,
    encoder: &mut Encoder,
    recorder: Arc<Mutex<Option<Recorder>>>,
    encode_fail_counter: &mut usize,
    first_frame: &mut bool,
    width: usize,
    height: usize,
) -> ResultType<usize> {
    sp.snapshot(|sps| {
        // so that new sub and old sub share the same encoder after switch
        if sps.has_subscribes() {
            log::info!("switch due to new subscriber");
            bail!("SWITCH");
        }
        Ok(())
    })?;

    // The size of what was sent, so a caller coding a still picture again can tell
    // when the passes stop carrying anything.
    let mut bytes = 0usize;
    let first = *first_frame;
    *first_frame = false;
    match encoder.encode_to_message(frame, ms) {
        Ok(mut vf) => {
            *encode_fail_counter = 0;
            vf.display = display as _;
            let mut msg = Message::new();
            msg.set_video_frame(vf);
            recorder
                .lock()
                .unwrap()
                .as_mut()
                .map(|r| r.write_message(&msg, width, height));
            bytes = hbb_common::protobuf::Message::compute_size(&msg) as usize;
            sp.send_video_frame(msg);
        }
        Err(e) => {
            *encode_fail_counter += 1;
            // Encoding errors are not frequent except on Android
            if !cfg!(target_os = "android") {
                log::error!("encode fail: {e:?}, times: {}", *encode_fail_counter,);
            }
            let max_fail_times = if cfg!(target_os = "android") && encoder.is_hardware() {
                9
            } else {
                3
            };
            let repeat = !encoder.latency_free();
            // repeat encoders can reach max_fail_times on the first frame
            if (first && !repeat) || *encode_fail_counter >= max_fail_times {
                *encode_fail_counter = 0;
                if encoder.is_hardware() {
                    encoder.disable();
                    log::error!("switch due to encoding fails, first frame: {first}, error: {e:?}");
                    bail!("SWITCH");
                }
            }
            match e.to_string().as_str() {
                scrap::codec::ENCODE_NEED_SWITCH => {
                    encoder.disable();
                    log::error!("switch due to encoder need switch");
                    bail!("SWITCH");
                }
                _ => {}
            }
        }
    }
    Ok(bytes)
}

#[inline]
pub fn refresh() {
    #[cfg(target_os = "android")]
    Display::refresh_size();
}

#[cfg(windows)]
fn start_uac_elevation_check() {
    static START: Once = Once::new();
    START.call_once(|| {
        if !crate::platform::is_installed() && !crate::platform::is_root() {
            std::thread::spawn(|| loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                if let Ok(uac) = is_process_consent_running() {
                    *IS_UAC_RUNNING.lock().unwrap() = uac;
                }
                if !crate::platform::is_elevated(None).unwrap_or(false) {
                    if let Ok(elevated) = crate::platform::is_foreground_window_elevated() {
                        *IS_FOREGROUND_WINDOW_ELEVATED.lock().unwrap() = elevated;
                    }
                }
            });
        }
    });
}

#[inline]
fn try_broadcast_display_changed(
    sp: &GenericService,
    display_idx: usize,
    cap: &CapturerInfo,
    refresh: bool,
) -> ResultType<()> {
    if refresh {
        // Get display information immediately.
        crate::display_service::check_displays_changed().ok();
    }
    if let Some(display) = check_display_changed(
        cap.ndisplay,
        cap.current,
        (cap.origin.0, cap.origin.1, cap.width, cap.height),
    ) {
        log::info!("Display {} changed", display);
        if let Some(msg_out) =
            make_display_changed_msg(display_idx, Some(display), VideoSource::Monitor)
        {
            let msg_out = Arc::new(msg_out);
            sp.send_shared(msg_out.clone());
            // switch display may occur before the first video frame, add snapshot to send to new subscribers
            sp.snapshot(move |sps| {
                sps.send_shared(msg_out.clone());
                Ok(())
            })?;
            bail!("SWITCH");
        }
    }
    Ok(())
}

pub fn make_display_changed_msg(
    display_idx: usize,
    opt_display: Option<DisplayInfo>,
    source: VideoSource,
) -> Option<Message> {
    let display = match opt_display {
        Some(d) => d,
        None => match source {
            VideoSource::Monitor => display_service::get_display_info(display_idx)?,
            VideoSource::Camera => camera::Cameras::get_sync_cameras()
                .get(display_idx)?
                .clone(),
        },
    };
    let mut misc = Misc::new();
    misc.set_switch_display(SwitchDisplay {
        display: display_idx as _,
        x: display.x,
        y: display.y,
        width: display.width,
        height: display.height,
        cursor_embedded: match source {
            VideoSource::Monitor => display_service::capture_cursor_embedded(),
            VideoSource::Camera => false,
        },
        #[cfg(not(target_os = "android"))]
        resolutions: Some(SupportedResolutions {
            resolutions: match source {
                VideoSource::Monitor => {
                    if display.name.is_empty() {
                        vec![]
                    } else {
                        crate::platform::resolutions(&display.name)
                    }
                }
                VideoSource::Camera => camera::Cameras::get_camera_resolution(display_idx)
                    .ok()
                    .into_iter()
                    .collect(),
            },
            ..SupportedResolutions::default()
        })
        .into(),
        original_resolution: display.original_resolution,
        ..Default::default()
    });
    let mut msg_out = Message::new();
    msg_out.set_misc(misc);
    Some(msg_out)
}

fn check_qos(
    encoder: &mut Encoder,
    ratio: &mut f32,
    spf: &mut Duration,
    client_record: bool,
    send_counter: &mut usize,
    second_instant: &mut Instant,
    name: &str,
) -> ResultType<()> {
    let mut video_qos = VIDEO_QOS.lock().unwrap();
    *spf = video_qos.spf();
    if *ratio != video_qos.ratio() {
        *ratio = video_qos.ratio();
        if encoder.support_changing_quality() {
            allow_err!(encoder.set_quality(*ratio));
            video_qos.store_bitrate(encoder.bitrate());
        } else {
            // Now only vaapi doesn't support changing quality
            if !video_qos.in_vbr_state() && !video_qos.latest_quality().is_custom() {
                log::info!("switch to change quality");
                bail!("SWITCH");
            }
        }
    }
    if client_record != video_qos.record() {
        log::info!("switch due to record changed");
        bail!("SWITCH");
    }
    if second_instant.elapsed() > Duration::from_secs(1) {
        *second_instant = Instant::now();
        video_qos.update_display_data(&name, *send_counter);
        *send_counter = 0;
    }
    drop(video_qos);
    Ok(())
}

pub fn set_take_screenshot(source: VideoSource, display_idx: usize, sid: String, tx: Sender) {
    SCREENSHOTS.lock().unwrap().insert(
        (source, display_idx),
        Screenshot {
            sid,
            tx,
            restore_vram: false,
        },
    );
}

// We need to this function, because the `stride` may be larger than `width * 4`.
fn get_rgba_from_pixelbuf<'a>(pixbuf: &scrap::PixelBuffer<'a>) -> ResultType<Vec<u8>> {
    let w = pixbuf.width();
    let h = pixbuf.height();
    let stride = pixbuf.stride();
    let Some(s) = stride.get(0) else {
        bail!("Invalid pixel buf stride.")
    };

    if *s == w * 4 {
        let mut rgba = vec![];
        scrap::convert(pixbuf, scrap::Pixfmt::RGBA, &mut rgba)?;
        Ok(rgba)
    } else {
        let bgra = pixbuf.data();
        let mut bit_flipped = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let i = s * y + 4 * x;
                bit_flipped.extend_from_slice(&[bgra[i + 2], bgra[i + 1], bgra[i], bgra[i + 3]]);
            }
        }
        Ok(bit_flipped)
    }
}

fn handle_screenshot(screenshot: Screenshot, msg: String, w: usize, h: usize, data: Vec<u8>) {
    let mut response = ScreenshotResponse::new();
    response.sid = screenshot.sid;
    if msg.is_empty() {
        if data.is_empty() {
            response.msg = "Failed to take screenshot, please try again later.".to_owned();
        } else {
            fn encode_png(width: usize, height: usize, rgba: Vec<u8>) -> ResultType<Vec<u8>> {
                let mut png = Vec::new();
                let mut encoder =
                    repng::Options::smallest(width as _, height as _).build(&mut png)?;
                encoder.write(&rgba)?;
                encoder.finish()?;
                Ok(png)
            }
            match encode_png(w as _, h as _, data) {
                Ok(png) => {
                    response.data = png.into();
                }
                Err(e) => {
                    response.msg = format!("Error encoding png: {}", e);
                }
            }
        }
    } else {
        response.msg = msg;
    }
    let mut msg_out = Message::new();
    msg_out.set_screenshot_response(response);
    if let Err(e) = screenshot
        .tx
        .send((hbb_common::tokio::time::Instant::now(), Arc::new(msg_out)))
    {
        log::error!("Failed to send screenshot, {}", e);
    }
}
