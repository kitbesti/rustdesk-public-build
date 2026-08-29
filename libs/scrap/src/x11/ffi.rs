#![allow(non_camel_case_types)]

use hbb_common::libc::c_void;

#[link(name = "xcb")]
#[link(name = "xcb-shm")]
#[link(name = "xcb-randr")]
#[link(name = "xcb-damage")]
extern "C" {
    pub fn xcb_connect(displayname: *const i8, screenp: *mut i32) -> *mut xcb_connection_t;

    pub fn xcb_disconnect(c: *mut xcb_connection_t);

    pub fn xcb_connection_has_error(c: *mut xcb_connection_t) -> i32;

    pub fn xcb_get_setup(c: *mut xcb_connection_t) -> *const xcb_setup_t;

    pub fn xcb_setup_roots_iterator(r: *const xcb_setup_t) -> xcb_screen_iterator_t;

    pub fn xcb_screen_next(i: *mut xcb_screen_iterator_t);

    pub fn xcb_generate_id(c: *mut xcb_connection_t) -> u32;

    pub fn xcb_shm_attach(
        c: *mut xcb_connection_t,
        shmseg: xcb_shm_seg_t,
        shmid: u32,
        read_only: u8,
    ) -> xcb_void_cookie_t;

    pub fn xcb_flush(c: *mut xcb_connection_t) -> ::std::os::raw::c_int;

    // --- XDamage: lets the server tell us when anything changed, instead of grabbing
    // the screen and comparing it to find out.
    pub fn xcb_damage_query_version(
        c: *mut xcb_connection_t,
        major: u32,
        minor: u32,
    ) -> xcb_void_cookie_t;

    pub fn xcb_damage_create(
        c: *mut xcb_connection_t,
        damage: u32,
        drawable: u32,
        level: u8,
    ) -> xcb_void_cookie_t;

    pub fn xcb_damage_destroy(c: *mut xcb_connection_t, damage: u32) -> xcb_void_cookie_t;

    // Clears the accumulated damage region, re-arming notification.
    pub fn xcb_damage_subtract(
        c: *mut xcb_connection_t,
        damage: u32,
        repair: u32,
        parts: u32,
    ) -> xcb_void_cookie_t;

    pub fn xcb_poll_for_event(c: *mut xcb_connection_t) -> *mut xcb_generic_event_t;

    pub fn xcb_get_file_descriptor(c: *mut xcb_connection_t) -> ::std::os::raw::c_int;

    pub fn xcb_get_extension_data(
        c: *mut xcb_connection_t,
        ext: *mut xcb_extension_t,
    ) -> *const xcb_query_extension_reply_t;

    pub static mut xcb_damage_id: xcb_extension_t;

    pub fn xcb_shm_detach(c: *mut xcb_connection_t, shmseg: xcb_shm_seg_t) -> xcb_void_cookie_t;

    pub fn xcb_shm_get_image_unchecked(
        c: *mut xcb_connection_t,
        drawable: xcb_drawable_t,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
        plane_mask: u32,
        format: u8,
        shmseg: xcb_shm_seg_t,
        offset: u32,
    ) -> xcb_shm_get_image_cookie_t;

    pub fn xcb_shm_get_image_reply(
        c: *mut xcb_connection_t,
        cookie: xcb_shm_get_image_cookie_t,
        e: *mut *mut xcb_generic_error_t,
    ) -> *mut xcb_shm_get_image_reply_t;

    pub fn xcb_randr_get_monitors_unchecked(
        c: *mut xcb_connection_t,
        window: xcb_window_t,
        get_active: u8,
    ) -> xcb_randr_get_monitors_cookie_t;

    pub fn xcb_randr_get_monitors_reply(
        c: *mut xcb_connection_t,
        cookie: xcb_randr_get_monitors_cookie_t,
        e: *mut *mut xcb_generic_error_t,
    ) -> *mut xcb_randr_get_monitors_reply_t;

    pub fn xcb_randr_get_monitors_monitors_iterator(
        r: *const xcb_randr_get_monitors_reply_t,
    ) -> xcb_randr_monitor_info_iterator_t;

    pub fn xcb_randr_monitor_info_next(i: *mut xcb_randr_monitor_info_iterator_t);

    pub fn xcb_get_atom_name(
        c: *mut xcb_connection_t,
        atom: xcb_atom_t,
    ) -> xcb_get_atom_name_cookie_t;

    pub fn xcb_get_atom_name_reply(
        c: *mut xcb_connection_t,
        cookie: xcb_get_atom_name_cookie_t,
        e: *mut *mut xcb_generic_error_t,
    ) -> *const xcb_get_atom_name_reply_t;

    pub fn xcb_get_atom_name_name(reply: *const xcb_get_atom_name_request_t) -> *const u8;

    pub fn xcb_get_atom_name_name_length(reply: *const xcb_get_atom_name_reply_t) -> i32;

    pub fn xcb_shm_query_version(c: *mut xcb_connection_t) -> xcb_shm_query_version_cookie_t;

    pub fn xcb_shm_query_version_reply(
        c: *mut xcb_connection_t,
        cookie: xcb_shm_query_version_cookie_t,
        e: *mut *mut xcb_generic_error_t,
    ) -> *const xcb_shm_query_version_reply_t;

    pub fn xcb_get_geometry_unchecked(
        c: *mut xcb_connection_t,
        drawable: xcb_drawable_t,
    ) -> xcb_get_geometry_cookie_t;

    pub fn xcb_get_geometry_reply(
        c: *mut xcb_connection_t,
        cookie: xcb_get_geometry_cookie_t,
        e: *mut *mut xcb_generic_error_t,
    ) -> *mut xcb_get_geometry_reply_t;


    // --- Core requests used to follow the windows on the root, so damage can be tracked
    // per window: on a composited desktop the root only ever says "the compositor
    // painted", the windows say what was actually drawn and where.
    pub fn xcb_change_window_attributes(
        c: *mut xcb_connection_t,
        window: xcb_window_t,
        value_mask: u32,
        value_list: *const c_void,
    ) -> xcb_void_cookie_t;

    pub fn xcb_query_tree_unchecked(
        c: *mut xcb_connection_t,
        window: xcb_window_t,
    ) -> xcb_query_tree_cookie_t;

    pub fn xcb_query_tree_reply(
        c: *mut xcb_connection_t,
        cookie: xcb_query_tree_cookie_t,
        e: *mut *mut xcb_generic_error_t,
    ) -> *mut xcb_query_tree_reply_t;

    pub fn xcb_query_tree_children(r: *const xcb_query_tree_reply_t) -> *mut xcb_window_t;

    pub fn xcb_query_tree_children_length(r: *const xcb_query_tree_reply_t) -> i32;

    pub fn xcb_query_pointer_unchecked(
        c: *mut xcb_connection_t,
        window: xcb_window_t,
    ) -> xcb_query_pointer_cookie_t;

    pub fn xcb_query_pointer_reply(
        c: *mut xcb_connection_t,
        cookie: xcb_query_pointer_cookie_t,
        e: *mut *mut xcb_generic_error_t,
    ) -> *mut xcb_query_pointer_reply_t;

    pub fn xcb_get_input_focus_unchecked(c: *mut xcb_connection_t) -> xcb_get_input_focus_cookie_t;

    pub fn xcb_get_input_focus_reply(
        c: *mut xcb_connection_t,
        cookie: xcb_get_input_focus_cookie_t,
        e: *mut *mut xcb_generic_error_t,
    ) -> *mut xcb_get_input_focus_reply_t;

    pub fn xcb_get_window_attributes_unchecked(
        c: *mut xcb_connection_t,
        window: xcb_window_t,
    ) -> xcb_get_window_attributes_cookie_t;

    pub fn xcb_get_window_attributes_reply(
        c: *mut xcb_connection_t,
        cookie: xcb_get_window_attributes_cookie_t,
        e: *mut *mut xcb_generic_error_t,
    ) -> *mut xcb_get_window_attributes_reply_t;

}

pub const XCB_IMAGE_FORMAT_Z_PIXMAP: u8 = 2;

pub type xcb_atom_t = u32;
pub type xcb_connection_t = c_void;
pub type xcb_window_t = u32;
pub type xcb_keycode_t = u8;
pub type xcb_visualid_t = u32;
pub type xcb_timestamp_t = u32;
pub type xcb_colormap_t = u32;
pub type xcb_shm_seg_t = u32;
pub type xcb_drawable_t = u32;
pub type xcb_get_atom_name_cookie_t = u32;
pub type xcb_get_atom_name_reply_t = u32;
pub type xcb_get_atom_name_request_t = xcb_get_atom_name_reply_t;

#[repr(C)]
pub struct xcb_setup_t {
    pub status: u8,
    pub pad0: u8,
    pub protocol_major_version: u16,
    pub protocol_minor_version: u16,
    pub length: u16,
    pub release_number: u32,
    pub resource_id_base: u32,
    pub resource_id_mask: u32,
    pub motion_buffer_size: u32,
    pub vendor_len: u16,
    pub maximum_request_length: u16,
    pub roots_len: u8,
    pub pixmap_formats_len: u8,
    pub image_byte_order: u8,
    pub bitmap_format_bit_order: u8,
    pub bitmap_format_scanline_unit: u8,
    pub bitmap_format_scanline_pad: u8,
    pub min_keycode: xcb_keycode_t,
    pub max_keycode: xcb_keycode_t,
    pub pad1: [u8; 4],
}

#[repr(C)]
pub struct xcb_screen_iterator_t {
    pub data: *mut xcb_screen_t,
    pub rem: i32,
    pub index: i32,
}

#[repr(C)]
pub struct xcb_screen_t {
    pub root: xcb_window_t,
    pub default_colormap: xcb_colormap_t,
    pub white_pixel: u32,
    pub black_pixel: u32,
    pub current_input_masks: u32,
    pub width_in_pixels: u16,
    pub height_in_pixels: u16,
    pub width_in_millimeters: u16,
    pub height_in_millimeters: u16,
    pub min_installed_maps: u16,
    pub max_installed_maps: u16,
    pub root_visual: xcb_visualid_t,
    pub backing_stores: u8,
    pub save_unders: u8,
    pub root_depth: u8,
    pub allowed_depths_len: u8,
}

#[repr(C)]
pub struct xcb_randr_monitor_info_iterator_t {
    pub data: *mut xcb_randr_monitor_info_t,
    pub rem: i32,
    pub index: i32,
}

#[repr(C)]
pub struct xcb_randr_monitor_info_t {
    pub name: xcb_atom_t,
    pub primary: u8,
    pub automatic: u8,
    pub n_output: u16,
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    pub width_mm: u32,
    pub height_mm: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct xcb_randr_get_monitors_cookie_t {
    pub sequence: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct xcb_shm_get_image_cookie_t {
    pub sequence: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct xcb_void_cookie_t {
    pub sequence: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct xcb_get_geometry_cookie_t {
    pub sequence: u32,
}

#[repr(C)]
pub struct xcb_generic_event_t {
    pub response_type: u8,
    pub pad0: u8,
    pub sequence: u16,
    pub pad: [u32; 7],
    pub full_sequence: u32,
}

/// Report only when the damage region goes from empty to non-empty, which is all we need
/// to answer "has anything changed since I last looked".
// Report whenever the bounding box of the accumulated damage grows, carrying that box.
pub const XCB_DAMAGE_REPORT_LEVEL_BOUNDING_BOX: u8 = 1;
pub const XCB_DAMAGE_NOTIFY: u8 = 0;

#[repr(C)]
pub struct xcb_extension_t {
    pub name: *const ::std::os::raw::c_char,
    pub global_id: ::std::os::raw::c_int,
}

#[repr(C)]
pub struct xcb_query_extension_reply_t {
    pub response_type: u8,
    pub pad0: u8,
    pub sequence: u16,
    pub length: u32,
    pub present: u8,
    pub major_opcode: u8,
    pub first_event: u8,
    pub first_error: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct xcb_rectangle_t {
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
}

#[repr(C)]
pub struct xcb_damage_notify_event_t {
    pub response_type: u8,
    pub level: u8,
    pub sequence: u16,
    pub drawable: xcb_drawable_t,
    pub damage: u32,
    pub timestamp: xcb_timestamp_t,
    pub area: xcb_rectangle_t,
    pub geometry: xcb_rectangle_t,
}

#[repr(C)]
pub struct xcb_generic_error_t {
    pub response_type: u8,
    pub error_code: u8,
    pub sequence: u16,
    pub resource_id: u32,
    pub minor_code: u16,
    pub major_code: u8,
    pub pad0: u8,
    pub pad: [u32; 5],
    pub full_sequence: u32,
}

#[repr(C)]
pub struct xcb_shm_get_image_reply_t {
    pub response_type: u8,
    pub depth: u8,
    pub sequence: u16,
    pub length: u32,
    pub visual: xcb_visualid_t,
    pub size: u32,
}

#[repr(C)]
pub struct xcb_randr_get_monitors_reply_t {
    pub response_type: u8,
    pub pad0: u8,
    pub sequence: u16,
    pub length: u32,
    pub timestamp: xcb_timestamp_t,
    pub n_monitors: u32,
    pub n_outputs: u32,
    pub pad1: [u8; 12],
}

#[repr(C)]
pub struct xcb_shm_query_version_cookie_t {
    pub sequence: u32,
}

#[repr(C)]
pub struct xcb_shm_query_version_reply_t {
    pub response_type: u8,
    pub shared_pixmaps: u8,
    pub sequence: u16,
    pub length: u32,
    pub major_version: u16,
    pub minor_version: u16,
    pub uid: u16,
    pub gid: u16,
    pub pixmap_format: u8,
    pub pad0: [u8; 15],
}

#[repr(C)]
pub struct xcb_get_geometry_reply_t {
    pub response_type: u8,
    pub depth: u8,
    pub sequence: u16,
    pub length: u32,
    pub root: xcb_window_t,
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    pub border_width: u16,
    pub pad0: [u8; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct xcb_query_tree_cookie_t {
    pub sequence: u32,
}

#[repr(C)]
pub struct xcb_query_tree_reply_t {
    pub response_type: u8,
    pub pad0: u8,
    pub sequence: u16,
    pub length: u32,
    pub root: xcb_window_t,
    pub parent: xcb_window_t,
    pub children_len: u16,
    pub pad1: [u8; 14],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct xcb_query_pointer_cookie_t {
    pub sequence: u32,
}

#[repr(C)]
pub struct xcb_query_pointer_reply_t {
    pub response_type: u8,
    pub same_screen: u8,
    pub sequence: u16,
    pub length: u32,
    pub root: xcb_window_t,
    pub child: xcb_window_t,
    pub root_x: i16,
    pub root_y: i16,
    pub win_x: i16,
    pub win_y: i16,
    pub mask: u16,
    pub pad0: [u8; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct xcb_get_input_focus_cookie_t {
    pub sequence: u32,
}

#[repr(C)]
pub struct xcb_get_input_focus_reply_t {
    pub response_type: u8,
    pub revert_to: u8,
    pub sequence: u16,
    pub length: u32,
    pub focus: xcb_window_t,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct xcb_get_window_attributes_cookie_t {
    pub sequence: u32,
}

#[repr(C)]
pub struct xcb_get_window_attributes_reply_t {
    pub response_type: u8,
    pub backing_store: u8,
    pub sequence: u16,
    pub length: u32,
    pub visual: xcb_visualid_t,
    pub class: u16,
    pub bit_gravity: u8,
    pub win_gravity: u8,
    pub backing_planes: u32,
    pub backing_pixel: u32,
    pub save_under: u8,
    pub map_is_installed: u8,
    pub map_state: u8,
    pub override_redirect: u8,
    pub colormap: xcb_colormap_t,
    pub all_event_masks: u32,
    pub your_event_mask: u32,
    pub do_not_propagate_mask: u16,
    pub pad0: [u8; 2],
}

pub const XCB_WINDOW_CLASS_INPUT_ONLY: u16 = 2;
pub const XCB_MAP_STATE_VIEWABLE: u8 = 2;
pub const XCB_CW_EVENT_MASK: u32 = 2048;
pub const XCB_EVENT_MASK_SUBSTRUCTURE_NOTIFY: u32 = 524288;

pub const XCB_CREATE_NOTIFY: u8 = 16;
pub const XCB_DESTROY_NOTIFY: u8 = 17;
pub const XCB_UNMAP_NOTIFY: u8 = 18;
pub const XCB_MAP_NOTIFY: u8 = 19;
pub const XCB_REPARENT_NOTIFY: u8 = 21;
pub const XCB_CONFIGURE_NOTIFY: u8 = 22;
pub const XCB_CIRCULATE_NOTIFY: u8 = 26;

#[repr(C)]
pub struct xcb_create_notify_event_t {
    pub response_type: u8,
    pub pad0: u8,
    pub sequence: u16,
    pub parent: xcb_window_t,
    pub window: xcb_window_t,
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    pub border_width: u16,
    pub override_redirect: u8,
    pub pad1: u8,
}

#[repr(C)]
pub struct xcb_destroy_notify_event_t {
    pub response_type: u8,
    pub pad0: u8,
    pub sequence: u16,
    pub event: xcb_window_t,
    pub window: xcb_window_t,
}

#[repr(C)]
pub struct xcb_unmap_notify_event_t {
    pub response_type: u8,
    pub pad0: u8,
    pub sequence: u16,
    pub event: xcb_window_t,
    pub window: xcb_window_t,
    pub from_configure: u8,
    pub pad1: [u8; 3],
}

#[repr(C)]
pub struct xcb_map_notify_event_t {
    pub response_type: u8,
    pub pad0: u8,
    pub sequence: u16,
    pub event: xcb_window_t,
    pub window: xcb_window_t,
    pub override_redirect: u8,
    pub pad1: [u8; 3],
}

#[repr(C)]
pub struct xcb_reparent_notify_event_t {
    pub response_type: u8,
    pub pad0: u8,
    pub sequence: u16,
    pub event: xcb_window_t,
    pub window: xcb_window_t,
    pub parent: xcb_window_t,
    pub x: i16,
    pub y: i16,
    pub override_redirect: u8,
    pub pad1: [u8; 3],
}

#[repr(C)]
pub struct xcb_configure_notify_event_t {
    pub response_type: u8,
    pub pad0: u8,
    pub sequence: u16,
    pub event: xcb_window_t,
    pub window: xcb_window_t,
    pub above_sibling: xcb_window_t,
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    pub border_width: u16,
    pub override_redirect: u8,
    pub pad1: u8,
}

#[repr(C)]
pub struct xcb_circulate_notify_event_t {
    pub response_type: u8,
    pub pad0: u8,
    pub sequence: u16,
    pub event: xcb_window_t,
    pub window: xcb_window_t,
    pub pad1: [u8; 4],
    pub place: u8,
    pub pad2: [u8; 3],
}
