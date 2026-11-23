//! A view's input as GPUI's: touches as `PlatformInput::Touch`, keys as key down and up with the
//! characters the view read from Android's `KeyEvent`.

use gpui::{
    KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, PlatformInput, Point, TouchEvent, TouchId,
    TouchPhase, px,
};

use super::window::AndroidWindowState;

/// Android's `KeyEvent.KEYCODE_*` that GPUI names or treats apart.
mod code {
    pub const BACK: u32 = 4;
    pub const DPAD_UP: u32 = 19;
    pub const DPAD_DOWN: u32 = 20;
    pub const DPAD_LEFT: u32 = 21;
    pub const DPAD_RIGHT: u32 = 22;
    pub const ALT_LEFT: u32 = 57;
    pub const ALT_RIGHT: u32 = 58;
    pub const SHIFT_LEFT: u32 = 59;
    pub const SHIFT_RIGHT: u32 = 60;
    pub const TAB: u32 = 61;
    pub const SPACE: u32 = 62;
    pub const ENTER: u32 = 66;
    pub const DEL: u32 = 67;
    pub const PAGE_UP: u32 = 92;
    pub const PAGE_DOWN: u32 = 93;
    pub const ESCAPE: u32 = 111;
    pub const FORWARD_DEL: u32 = 112;
    pub const CTRL_LEFT: u32 = 113;
    pub const CTRL_RIGHT: u32 = 114;
    pub const CAPS_LOCK: u32 = 115;
    pub const META_LEFT: u32 = 117;
    pub const META_RIGHT: u32 = 118;
    pub const FUNCTION: u32 = 119;
    pub const MOVE_HOME: u32 = 122;
    pub const MOVE_END: u32 = 123;
    pub const INSERT: u32 = 124;
    pub const F1: u32 = 131;
    pub const F12: u32 = 142;
    pub const NUMPAD_ENTER: u32 = 160;
}

/// Android's `KeyEvent.META_*_ON`.
mod meta {
    pub const SHIFT: u32 = 0x1;
    pub const ALT: u32 = 0x2;
    pub const FUNCTION: u32 = 0x8;
    pub const CTRL: u32 = 0x1000;
    pub const META: u32 = 0x10000;
}

/// The keyboard's action key: GPUI binds it as enter.
pub(crate) fn action(window: &AndroidWindowState) {
    let keystroke = Keystroke { modifiers: Modifiers::default(), key: "enter".into(), key_char: None };
    window.input_callback.with(|callback| {
        callback(PlatformInput::KeyDown(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        }));
        callback(PlatformInput::KeyUp(KeyUpEvent { keystroke }));
    });
}

/// A key told from a window's view (others.rs): `typed` and `bare` are the characters it types with
/// its meta state and with none (0 for none).
pub(crate) fn told_key(window: &AndroidWindowState, code: u32, meta: u32, down: bool, repeat: i32, typed: u32, bare: u32) {
    let character = |c: u32| char::from_u32(c).filter(|c| *c != '\0');
    key(window, code, meta, down, repeat, character(typed), character(bare));
}

fn position(x: f32, y: f32, scale: f32) -> Point<gpui::Pixels> {
    Point { x: px(x / scale), y: px(y / scale) }
}

/// What touched, as `PlatformWindow::pointer` reports it: the tool, and the contact's major and minor
/// axes in device pixels (0 when not reported).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Contact {
    pub kind: gpui::PointerKind,
    pub major: f32,
    pub minor: f32,
}

impl Contact {
    /// `MotionEvent.getToolType`'s tool (1 finger, 2 stylus, 3 mouse, 4 eraser) and its axes.
    pub(crate) fn of(tool: i32, major: f32, minor: f32) -> Contact {
        let kind = match tool {
            2 | 4 => gpui::PointerKind::Pen,
            3 => gpui::PointerKind::Mouse,
            _ => gpui::PointerKind::Touch,
        };
        Contact { kind, major, minor }
    }
}

pub(crate) fn touch(window: &AndroidWindowState, id: i32, phase: TouchPhase, x: f32, y: f32, pressure: f32, scale: f32, contact: Contact) {
    let force = (pressure > 0.0).then_some(pressure.clamp(0.0, 1.0));
    let event = TouchEvent {
        id: TouchId(id as u64),
        phase,
        position: position(x, y, scale),
        predicted_position: None,
        force,
        tilt: None,
        coalesced: Vec::new(),
    };
    window.mouse_position.set(event.position);
    window.pointer.set(Some(gpui::PointerInfo {
        kind: contact.kind,
        id: id.max(0) as u64,
        width: contact.major.max(0.0) / scale,
        height: contact.minor.max(0.0) / scale,
        pressure: force,
        tilt: None,
    }));
    window.input_callback.with(|callback| callback(PlatformInput::Touch(event)));
}

fn modifiers(state: u32) -> Modifiers {
    let on = |bit: u32| state & bit != 0;
    Modifiers {
        control: on(meta::CTRL),
        alt: on(meta::ALT),
        shift: on(meta::SHIFT),
        platform: on(meta::META),
        function: on(meta::FUNCTION),
    }
}

/// GPUI's name for a key with no character of its own.
fn key_name(key: u32) -> Option<&'static str> {
    const F: [&str; 12] = ["f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12"];
    Some(match key {
        code::ENTER | code::NUMPAD_ENTER => "enter",
        code::TAB => "tab",
        code::SPACE => "space",
        code::DEL => "backspace",
        code::FORWARD_DEL => "delete",
        code::ESCAPE => "escape",
        code::DPAD_UP => "up",
        code::DPAD_DOWN => "down",
        code::DPAD_LEFT => "left",
        code::DPAD_RIGHT => "right",
        code::MOVE_HOME => "home",
        code::MOVE_END => "end",
        code::PAGE_UP => "pageup",
        code::PAGE_DOWN => "pagedown",
        code::INSERT => "insert",
        code::F1..=code::F12 => F[(key - code::F1) as usize],
        _ => return None,
    })
}

/// Modifier keys alone change the modifiers, not GPUI's keystrokes.
fn is_modifier(key: u32) -> bool {
    matches!(
        key,
        code::SHIFT_LEFT
            | code::SHIFT_RIGHT
            | code::CTRL_LEFT
            | code::CTRL_RIGHT
            | code::ALT_LEFT
            | code::ALT_RIGHT
            | code::META_LEFT
            | code::META_RIGHT
            | code::FUNCTION
            | code::CAPS_LOCK
    )
}

/// A key as GPUI's key down or up: `typed` is what it types with its meta state, `bare` with none,
/// which names the key as GPUI binds it. Whether GPUI took it.
fn key(window: &AndroidWindowState, key: u32, meta: u32, down: bool, repeat: i32, typed: Option<char>, bare: Option<char>) -> bool {
    let mods = modifiers(meta);
    window.modifiers.set(mods);
    if key == code::BACK {
        // The back key, as the system's back gesture: GPUI's handler when it has one, else the
        // system's own (the activity finishes).
        return if down { window.back_callback_set() } else { window.back() };
    }
    if is_modifier(key) {
        window.input_callback.with(|callback| {
            callback(PlatformInput::ModifiersChanged(gpui::ModifiersChangedEvent {
                modifiers: mods,
                capslock: gpui::Capslock::default(),
            }))
        });
        return true;
    }
    let (key_name, key_char) = match key_name(key) {
        Some(name) => (name.to_string(), (name == "space").then(|| " ".to_string())),
        None => {
            // With ctrl or meta held the map types nothing: the bare key names it then.
            let Some(bare) = bare.or(typed) else { return false };
            let key_char = (!mods.control && !mods.platform).then(|| typed.map(|c| c.to_string())).flatten();
            (bare.to_lowercase().to_string(), key_char)
        }
    };
    let keystroke = Keystroke { modifiers: mods, key: key_name, key_char };
    let input = if down {
        PlatformInput::KeyDown(KeyDownEvent { keystroke, is_held: repeat > 0, prefer_character_input: false })
    } else {
        PlatformInput::KeyUp(KeyUpEvent { keystroke })
    };
    matches!(window.input_callback.with(|callback| callback(input)), Some(result) if !result.propagate)
}
