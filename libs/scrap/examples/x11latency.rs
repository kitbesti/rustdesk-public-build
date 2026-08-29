// How long after a window draws does the X11 capturer hand out a picture showing it?
// Paints a small override-redirect window a new colour every PERIOD ms and times the
// frame that carries it, while the capturer's own rd-stat lines show what that cost.
//
//   DISPLAY=:1 cargo run -p scrap --example x11latency -- [PAINTS] [PERIOD_MS]
use scrap::x11::{Capturer, Server};
use std::{
    ffi::c_void,
    os::raw::{c_char, c_int},
    ptr,
    time::{Duration, Instant},
};

#[repr(C)]
struct XcbRect {
    x: i16,
    y: i16,
    w: u16,
    h: u16,
}

#[repr(C)]
struct Screen {
    root: u32,
    default_colormap: u32,
    white_pixel: u32,
    black_pixel: u32,
    current_input_masks: u32,
    width_in_pixels: u16,
    height_in_pixels: u16,
    width_in_millimeters: u16,
    height_in_millimeters: u16,
    min_installed_maps: u16,
    max_installed_maps: u16,
    root_visual: u32,
    backing_stores: u8,
    save_unders: u8,
    root_depth: u8,
    allowed_depths_len: u8,
}

#[repr(C)]
struct ScreenIter {
    data: *mut Screen,
    rem: c_int,
    index: c_int,
}

#[link(name = "xcb")]
extern "C" {
    fn xcb_connect(display: *const c_char, screen: *mut c_int) -> *mut c_void;
    fn xcb_get_setup(c: *mut c_void) -> *const c_void;
    fn xcb_setup_roots_iterator(setup: *const c_void) -> ScreenIter;
    fn xcb_generate_id(c: *mut c_void) -> u32;
    fn xcb_create_window(
        c: *mut c_void,
        depth: u8,
        wid: u32,
        parent: u32,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
        border_width: u16,
        class: u16,
        visual: u32,
        value_mask: u32,
        value_list: *const u32,
    ) -> u32;
    fn xcb_map_window(c: *mut c_void, window: u32) -> u32;
    fn xcb_create_gc(c: *mut c_void, cid: u32, drawable: u32, value_mask: u32, value_list: *const u32) -> u32;
    fn xcb_change_gc(c: *mut c_void, gc: u32, value_mask: u32, value_list: *const u32) -> u32;
    fn xcb_poly_fill_rectangle(c: *mut c_void, drawable: u32, gc: u32, n: u32, rects: *const XcbRect) -> u32;
    fn xcb_flush(c: *mut c_void) -> c_int;
}

const SIZE: u16 = 200;
const AT: i16 = 300;

fn main() {
    hbb_common::env_logger::init_from_env(hbb_common::env_logger::Env::default().filter_or("RUST_LOG", "info"));
    let paints: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(20);
    let period = Duration::from_millis(std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(400));

    // The painter: its own connection, so the capturer sees it as any other client.
    let (c, win, gc) = unsafe {
        let c = xcb_connect(ptr::null(), ptr::null_mut());
        let screen = &*xcb_setup_roots_iterator(xcb_get_setup(c)).data;
        let win = xcb_generate_id(c);
        // CW_BACK_PIXEL (2) | CW_OVERRIDE_REDIRECT (512), values in mask order.
        let values = [screen.black_pixel, 1];
        xcb_create_window(c, screen.root_depth, win, screen.root, AT, AT, SIZE, SIZE, 0, 1, screen.root_visual, 2 | 512, values.as_ptr());
        let gc = xcb_generate_id(c);
        xcb_create_gc(c, gc, win, 0, ptr::null());
        xcb_map_window(c, win);
        xcb_flush(c);
        (c, win, gc)
    };

    let mut cap = Capturer::new(Server::displays(Server::default().unwrap()).find(|d| d.is_default()).expect("display")).unwrap();
    let rect = cap.display().rect();
    let stride = rect.w as usize * 4;
    let probe = (AT as usize + SIZE as usize / 2 - rect.x as usize) * 4 + (AT as usize + SIZE as usize / 2 - rect.y as usize) * stride;
    // Let the window's mapping settle before timing anything.
    let until = Instant::now() + Duration::from_millis(500);
    while Instant::now() < until {
        let _ = cap.frame(Duration::from_millis(8));
    }

    let colours: [u32; 4] = [0xff0000, 0x00ff00, 0x0000ff, 0xffff00];
    let mut lat = Vec::with_capacity(paints);
    let mut missed = 0;
    for i in 0..paints {
        let colour = colours[i % colours.len()];
        let full = XcbRect { x: 0, y: 0, w: SIZE, h: SIZE };
        unsafe {
            xcb_change_gc(c, gc, 4, &colour); // GC_FOREGROUND
            xcb_poly_fill_rectangle(c, win, gc, 1, &full);
            xcb_flush(c);
        }
        let drawn = Instant::now();
        let give_up = drawn + period;
        let mut seen = None;
        while Instant::now() < give_up {
            if cap.frame(Duration::from_millis(8)).is_ok() {
                let px = &cap.current()[probe..probe + 3];
                let shown = (px[2] as u32) << 16 | (px[1] as u32) << 8 | px[0] as u32;
                if shown == colour {
                    seen = Some(drawn.elapsed());
                    break;
                }
            }
        }
        match seen {
            Some(d) => {
                println!("paint {i}: shown after {:.1} ms", d.as_secs_f64() * 1000.0);
                lat.push(d);
            }
            None => {
                println!("paint {i}: not shown within {} ms", period.as_millis());
                missed += 1;
            }
        }
        // Keep capturing through the rest of the period, as the real loop would.
        while Instant::now() < give_up {
            let _ = cap.frame(Duration::from_millis(8));
        }
    }
    if !lat.is_empty() {
        lat.sort();
        let ms = |d: &Duration| d.as_secs_f64() * 1000.0;
        let avg = lat.iter().map(ms).sum::<f64>() / lat.len() as f64;
        println!(
            "shown: {} of {} paints; latency min {:.1} ms, median {:.1} ms, avg {:.1} ms, max {:.1} ms; missed {}",
            lat.len(),
            paints,
            ms(&lat[0]),
            ms(&lat[lat.len() / 2]),
            avg,
            ms(&lat[lat.len() - 1]),
            missed
        );
    } else {
        println!("nothing shown; missed {missed}");
    }
}
