//! Windows beyond the activity's own, each on a surface the app hands over.
//!
//! `GameActivity` has one surface, GPUI's first window. Each window GPUI opens after it is a window
//! of the app's own: GPUI's thread tells the app (`on_opened`), which shows an activity of its own
//! holding a `SurfaceView`, beside the app in split screen or free in a free-window mode. That
//! activity tells GPUI, from Android's main thread, of its surface, its size, its touches, its
//! insets, its focus and its end. Each is queued here and handled on GPUI's thread at its next wake.
//! A surface going away waits until GPUI has let go of it, as `TerminateWindow` does.
//!
//! Such a window has touches alone: no keyboard text, no TalkBack tree, both the activity's own.

use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use gpui::{Edges, TouchPhase, WindowInsets, px};
pub use ndk::native_window::NativeWindow;

/// What the app's activity told of window `id`, for GPUI's thread.
pub(crate) enum Told {
    Surface(u64, NativeWindow),
    Gone(u64, mpsc::Sender<()>),
    Resized(u64),
    Touch { id: u64, pointer: i32, phase: TouchPhase, x: f32, y: f32, pressure: f32 },
    /// The bars and cutout, then the keyboard, in device pixels: left, top, right, bottom.
    Insets(u64, [i32; 4], [i32; 4]),
    Focus(u64, bool),
    /// The surface's top left on the screen, in device pixels.
    Placed(u64, i32, i32),
    /// The activity ended (Back, or closed by the operator): GPUI closes the window.
    Ended(u64),
}

static TOLD: Mutex<Vec<Told>> = Mutex::new(Vec::new());
static OPENED: OnceLock<Box<dyn Fn(u64) + Send + Sync>> = OnceLock::new();
static CLOSED: OnceLock<Box<dyn Fn(u64) + Send + Sync>> = OnceLock::new();

/// How long a surface going away waits for GPUI's thread to let go of it.
const LET_GO: Duration = Duration::from_secs(2);

fn tell(told: Told) {
    TOLD.lock().unwrap_or_else(|e| e.into_inner()).push(told);
    super::platform::wake();
}

pub(crate) fn drain() -> Vec<Told> {
    std::mem::take(&mut *TOLD.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Called on GPUI's thread with each window GPUI opens beyond the activity's own: the app shows an
/// activity for it, which tells its surface with `surface(id, …)`.
pub fn on_opened(opened: impl Fn(u64) + Send + Sync + 'static) {
    let _ = OPENED.set(Box::new(opened));
}

/// Called on GPUI's thread when GPUI let go of window `id` (it closed, or it was ended): the app
/// finishes its activity, if it still runs.
pub fn on_closed(closed: impl Fn(u64) + Send + Sync + 'static) {
    let _ = CLOSED.set(Box::new(closed));
}

pub(crate) fn opened(id: u64) {
    match OPENED.get() {
        Some(opened) => opened(id),
        None => log::warn!("gpui_android: window {id} opened, but the app shows no activity for it (on_opened)"),
    }
}

pub(crate) fn closed(id: u64) {
    if let Some(closed) = CLOSED.get() {
        closed(id);
    }
}

/// Window `id`'s activity has a surface (`surfaceCreated`), from any thread.
pub fn surface(id: u64, window: NativeWindow) {
    tell(Told::Surface(id, window));
}

/// Window `id`'s surface goes away (`surfaceDestroyed`): returns once GPUI has let go of it.
pub fn surface_gone(id: u64) {
    let (tx, rx) = mpsc::channel();
    tell(Told::Gone(id, tx));
    if rx.recv_timeout(LET_GO).is_err() {
        log::warn!("gpui_android: window {id}'s surface went before GPUI let go of it");
    }
}

/// Window `id`'s surface changed size (`surfaceChanged`).
pub fn resized(id: u64) {
    tell(Told::Resized(id));
}

/// A touch on window `id`, in its surface's pixels.
pub fn touch(id: u64, pointer: i32, phase: TouchPhase, x: f32, y: f32, pressure: f32) {
    tell(Told::Touch { id, pointer, phase, x, y, pressure });
}

/// Window `id`'s insets, in device pixels (left, top, right, bottom): the system bars and cutout,
/// and the keyboard.
pub fn insets(id: u64, safe: [i32; 4], ime: [i32; 4]) {
    tell(Told::Insets(id, safe, ime));
}

/// Window `id`'s surface has its top left at (x, y) on the screen, in device pixels
/// (`View.getLocationOnScreen`): GPUI's `bounds` start there. Id 0 is the activity's own window.
pub fn placed(id: u64, x: i32, y: i32) {
    tell(Told::Placed(id, x, y));
}

/// Window `id`'s activity gained or lost focus.
pub fn focus(id: u64, focused: bool) {
    tell(Told::Focus(id, focused));
}

/// Window `id`'s activity ended: GPUI closes the window.
pub fn ended(id: u64) {
    tell(Told::Ended(id));
}

/// Device pixels as GPUI's insets at `scale`.
pub(crate) fn as_insets(safe: [i32; 4], ime: [i32; 4], scale: f32) -> WindowInsets {
    let edges = |v: [i32; 4]| Edges {
        left: px(v[0] as f32 / scale),
        top: px(v[1] as f32 / scale),
        right: px(v[2] as f32 / scale),
        bottom: px(v[3] as f32 / scale),
    };
    WindowInsets { safe_area: edges(safe), ime: edges(ime), ..Default::default() }
}
