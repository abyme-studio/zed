//! Every window's view, and the activity's lifecycle, as GPUI's thread hears them.
//!
//! Each window GPUI opens is on a view of the app's (`AbymeView`): the activity's own window is id 0,
//! and each one after it is a window of the app's own: GPUI's thread tells the app (`on_opened`),
//! which shows an activity of its own holding a view, beside the app in split screen or free in a
//! free-window mode. Each view tells GPUI, from Android's main thread, of its surface, its size, its
//! touches, its insets, its focus and its end, and the activity of its lifecycle (`life`). Each is
//! queued here and handled on GPUI's thread at its next wake. A surface going away waits until GPUI
//! has let go of it.
//!
//! Its keys, the IME's edits and a drop of files come the same way (`key`, `edit`, `action`, `dropped`), and GPUI says what its
//! keyboard should show back to the view, and where its caret is (`on_keyboard`). Only the activity's own window has a
//! TalkBack tree yet.

use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use gpui::{Edges, TouchPhase, WindowInsets, px};
pub use ndk::native_window::NativeWindow;
pub use super::text_input::{Edit, TextInputState, TextSpan};

/// What the app's activity told of window `id`, for GPUI's thread.
pub(crate) enum Told {
    Surface(u64, NativeWindow),
    Gone(u64, mpsc::Sender<()>),
    Resized(u64),
    /// A pointer of a touch, with `MotionEvent`'s tool type and the contact's axes in device pixels.
    Touch { id: u64, pointer: i32, phase: TouchPhase, x: f32, y: f32, pressure: f32, tool: i32, major: f32, minor: f32 },
    /// The bars and cutout, then the keyboard, in device pixels: left, top, right, bottom.
    Insets(u64, [i32; 4], [i32; 4]),
    Focus(u64, bool),
    /// The surface's top left on the screen, in device pixels.
    Placed(u64, i32, i32),
    /// The activity ended (Back, or closed by the operator): GPUI closes the window.
    Ended(u64),
    /// A key, as Android's `KeyEvent` gives it: its code, meta state, whether down, its repeat
    /// count, and the character it types with its meta state and without (0 for none).
    Key { id: u64, code: u32, meta: u32, down: bool, repeat: i32, typed: u32, bare: u32 },
    /// The IME edited the text the app's view holds for the focused input.
    Edit(u64, Edit),
    /// The keyboard's action key.
    Action(u64),
    /// Files dropped on the view at (x, y), in its surface's pixels, as paths the app copied them to.
    Drop { id: u64, x: f32, y: f32, paths: Vec<std::path::PathBuf> },
    /// The activity's lifecycle (`life`).
    Life(Life),
}

/// What the activity went through, as `Windows.life`'s kind says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Life {
    Start,
    Resume,
    Pause,
    Stop,
    /// It finished (Back, or closed): the app ends.
    Destroy,
    LowMemory,
    /// A rotation, a resize, a theme or a density: the configuration is read again.
    Configuration,
}

impl Life {
    /// `Windows.life`'s `kind`: 0 start, 1 resume, 2 pause, 3 stop, 4 destroy, 5 low memory,
    /// 6 configuration.
    pub fn from_kind(kind: i32) -> Option<Life> {
        Some(match kind {
            0 => Life::Start,
            1 => Life::Resume,
            2 => Life::Pause,
            3 => Life::Stop,
            4 => Life::Destroy,
            5 => Life::LowMemory,
            6 => Life::Configuration,
            _ => return None,
        })
    }
}

/// What GPUI tells the app's view for a window of what its keyboard should show.
pub enum Keyboard {
    /// An input took focus: the keyboard comes up.
    Show,
    /// No input has focus: the keyboard goes.
    Hide,
    /// The focused input's kind: `EditorInfo.inputType` and the action's `imeOptions`.
    Editor { input_type: u32, action: u32 },
    /// The focused input's text around the caret, its selection and composing region (UTF-16),
    /// after the view's edit `seq` (`edit`): the view drops it if it has edited since.
    State { seq: u32, state: TextInputState },
    /// Where the caret is, then each character of the composing region, in the view's device
    /// pixels: the caret's x, top, baseline and bottom, then each character's left, top, right and
    /// bottom. The view gives it to the IME as `CursorAnchorInfo`.
    Anchor(Vec<f32>),
}

static TOLD: Mutex<Vec<Told>> = Mutex::new(Vec::new());
static OPENED: OnceLock<Box<dyn Fn(u64) + Send + Sync>> = OnceLock::new();
static CLOSED: OnceLock<Box<dyn Fn(u64) + Send + Sync>> = OnceLock::new();
static KEYBOARD: OnceLock<Box<dyn Fn(u64, Keyboard) + Send + Sync>> = OnceLock::new();

/// How long a surface going away waits for GPUI's thread to let go of it.
const LET_GO: Duration = Duration::from_secs(2);

fn tell(told: Told) {
    TOLD.lock().unwrap_or_else(|e| e.into_inner()).push(told);
    super::host::wake();
}

pub(crate) fn drain() -> Vec<Told> {
    std::mem::take(&mut *TOLD.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Called on GPUI's thread with each window GPUI opens beyond the activity's own (ids from 1): the
/// app shows an activity for it, whose view tells its surface with `surface(id, …)`.
pub fn on_opened(opened: impl Fn(u64) + Send + Sync + 'static) {
    let _ = OPENED.set(Box::new(opened));
}

/// Called on GPUI's thread when GPUI let go of window `id` (it closed, or it was ended): the app
/// finishes its activity, if it still runs.
pub fn on_closed(closed: impl Fn(u64) + Send + Sync + 'static) {
    let _ = CLOSED.set(Box::new(closed));
}

/// Called on GPUI's thread with what window `id`'s keyboard should show: the app's view for it
/// holds the text the IME edits, and shows or hides the keyboard.
pub fn on_keyboard(keyboard: impl Fn(u64, Keyboard) + Send + Sync + 'static) {
    let _ = KEYBOARD.set(Box::new(keyboard));
}

pub(crate) fn keyboard(id: u64, said: Keyboard) {
    if let Some(keyboard) = KEYBOARD.get() {
        keyboard(id, said);
    }
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

/// Window `id`'s view has a surface (`surfaceCreated`), from any thread. Id 0 is the activity's own.
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

/// A touch on window `id`, in its surface's pixels: `tool` is `MotionEvent.getToolType`, `major` and
/// `minor` its contact's axes (`getTouchMajor`, `getTouchMinor`), 0 when not reported.
#[allow(clippy::too_many_arguments)]
pub fn touch(id: u64, pointer: i32, phase: TouchPhase, x: f32, y: f32, pressure: f32, tool: i32, major: f32, minor: f32) {
    tell(Told::Touch { id, pointer, phase, x, y, pressure, tool, major, minor });
}

/// The activity's lifecycle (`Windows.life`), from any thread.
pub fn life(life: Life) {
    tell(Told::Life(life));
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

/// Window `id`'s window gained or lost focus.
pub fn focus(id: u64, focused: bool) {
    tell(Told::Focus(id, focused));
}

/// Window `id`'s activity ended: GPUI closes the window.
pub fn ended(id: u64) {
    tell(Told::Ended(id));
}

/// A key on window `id`'s view (`dispatchKeyEvent`): `code`, `meta`, `repeat` as Android's
/// `KeyEvent` has them; `typed` and `bare`, `getUnicodeChar` with its meta state and with none.
pub fn key(id: u64, code: u32, meta: u32, down: bool, repeat: i32, typed: u32, bare: u32) {
    tell(Told::Key { id, code, meta, down, repeat, typed, bare });
}

/// The IME's edit `seq` to window `id`'s focused input, in the text the view holds (UTF-16
/// offsets): `replaced` became `text` (none: only the selection or composing region moved), then
/// the selection and the composing region if any.
pub fn edit(
    id: u64,
    seq: u32,
    replaced: Option<(usize, usize)>,
    text: String,
    selection: (usize, usize),
    compose: Option<(usize, usize)>,
) {
    let span = |(start, end): (usize, usize)| TextSpan { start, end };
    tell(Told::Edit(
        id,
        Edit { seq, replaced: replaced.map(span), text, selection: span(selection), compose_region: compose.map(span) },
    ));
}

/// The keyboard's action key on window `id` (Done, Go, Send, or a new line).
pub fn action(id: u64) {
    tell(Told::Action(id));
}

/// Files dropped on window `id`'s view (`onDragEvent`'s `ACTION_DROP`) at (x, y), in its surface's
/// pixels: `paths` are files the app copied the drop's content to, since Android gives a drop as
/// content URIs. GPUI hears it as a drop of `ExternalPaths` there, as a desktop's file drop.
pub fn dropped(id: u64, x: f32, y: f32, paths: Vec<std::path::PathBuf>) {
    tell(Told::Drop { id, x, y, paths });
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
