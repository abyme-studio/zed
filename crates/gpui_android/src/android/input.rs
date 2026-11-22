//! The activity's input events as GPUI's: touches as `PlatformInput::Touch`, hardware keys as key
//! down and up with the character the device's key map gives.

use android_activity::input::{
    InputEvent, KeyAction, KeyEvent, Keycode, MetaState, MotionAction, MotionEvent, Pointer, ToolType,
};
use android_activity::{AndroidApp, InputStatus};
use gpui::{
    KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, PlatformInput, Point, TouchEvent, TouchId,
    TouchPhase, px,
};

use super::window::AndroidWindowState;

/// Delivers every pending input event to `window`. Called when the activity says input is
/// available.
pub(crate) fn drain(app: &AndroidApp, window: &AndroidWindowState, scale: f32) {
    let Ok(mut events) = app.input_events_iter() else { return };
    loop {
        let more = events.next(|event| match event {
            InputEvent::MotionEvent(motion) => motion_event(window, motion, scale),
            InputEvent::KeyEvent(key) => key_event(app, window, key),
            InputEvent::TextEvent(state) => {
                window.text_input.apply(state, &window.input_handler);
                InputStatus::Handled
            }
            InputEvent::TextAction(_) => {
                // The keyboard's action key: GPUI binds it as enter.
                let keystroke = Keystroke { modifiers: Modifiers::default(), key: "enter".into(), key_char: None };
                window.input_callback.with(|callback| {
                    callback(PlatformInput::KeyDown(KeyDownEvent {
                        keystroke: keystroke.clone(),
                        is_held: false,
                        prefer_character_input: false,
                    }));
                    callback(PlatformInput::KeyUp(KeyUpEvent { keystroke }));
                });
                InputStatus::Handled
            }
            _ => InputStatus::Unhandled,
        });
        if !more {
            break;
        }
    }
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
    /// A finger of unknown size: what a touch told from another window carries.
    pub(crate) const FINGER: Contact = Contact { kind: gpui::PointerKind::Touch, major: 0.0, minor: 0.0 };

    fn of(p: &Pointer<'_>) -> Contact {
        let kind = match p.tool_type() {
            ToolType::Stylus | ToolType::Eraser => gpui::PointerKind::Pen,
            ToolType::Mouse => gpui::PointerKind::Mouse,
            _ => gpui::PointerKind::Touch,
        };
        Contact { kind, major: p.touch_major(), minor: p.touch_minor() }
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
    };
    window.mouse_position.set(event.position);
    window.pointer.set(Some(gpui::PointerInfo {
        kind: contact.kind,
        id: id.max(0) as u64,
        width: contact.major.max(0.0) / scale,
        height: contact.minor.max(0.0) / scale,
        pressure: force,
    }));
    window.input_callback.with(|callback| callback(PlatformInput::Touch(event)));
}

fn motion_event(window: &AndroidWindowState, motion: &MotionEvent<'_>, scale: f32) -> InputStatus {
    let index = motion.pointer_index();
    match motion.action() {
        MotionAction::Down | MotionAction::PointerDown => {
            let p = motion.pointer_at_index(index);
            touch(window, p.pointer_id(), TouchPhase::Started, p.x(), p.y(), p.pressure(), scale, Contact::of(&p));
        }
        MotionAction::Up | MotionAction::PointerUp => {
            let p = motion.pointer_at_index(index);
            touch(window, p.pointer_id(), TouchPhase::Ended, p.x(), p.y(), p.pressure(), scale, Contact::of(&p));
        }
        MotionAction::Move => {
            for p in motion.pointers() {
                touch(window, p.pointer_id(), TouchPhase::Moved, p.x(), p.y(), p.pressure(), scale, Contact::of(&p));
            }
        }
        MotionAction::Cancel => {
            for p in motion.pointers() {
                touch(window, p.pointer_id(), TouchPhase::Cancelled, p.x(), p.y(), p.pressure(), scale, Contact::of(&p));
            }
        }
        _ => return InputStatus::Unhandled,
    }
    InputStatus::Handled
}

fn modifiers(meta: MetaState) -> Modifiers {
    Modifiers {
        control: meta.ctrl_on(),
        alt: meta.alt_on(),
        shift: meta.shift_on(),
        platform: meta.meta_on(),
        function: meta.function_on(),
    }
}

/// GPUI's name for a key with no character of its own.
fn key_name(code: Keycode) -> Option<&'static str> {
    Some(match code {
        Keycode::Enter | Keycode::NumpadEnter => "enter",
        Keycode::Tab => "tab",
        Keycode::Space => "space",
        Keycode::Del => "backspace",
        Keycode::ForwardDel => "delete",
        Keycode::Escape => "escape",
        Keycode::DpadUp => "up",
        Keycode::DpadDown => "down",
        Keycode::DpadLeft => "left",
        Keycode::DpadRight => "right",
        Keycode::MoveHome => "home",
        Keycode::MoveEnd => "end",
        Keycode::PageUp => "pageup",
        Keycode::PageDown => "pagedown",
        Keycode::Insert => "insert",
        Keycode::F1 => "f1",
        Keycode::F2 => "f2",
        Keycode::F3 => "f3",
        Keycode::F4 => "f4",
        Keycode::F5 => "f5",
        Keycode::F6 => "f6",
        Keycode::F7 => "f7",
        Keycode::F8 => "f8",
        Keycode::F9 => "f9",
        Keycode::F10 => "f10",
        Keycode::F11 => "f11",
        Keycode::F12 => "f12",
        _ => return None,
    })
}

/// Modifier keys alone change the modifiers, not GPUI's keystrokes.
fn is_modifier(code: Keycode) -> bool {
    matches!(
        code,
        Keycode::ShiftLeft
            | Keycode::ShiftRight
            | Keycode::CtrlLeft
            | Keycode::CtrlRight
            | Keycode::AltLeft
            | Keycode::AltRight
            | Keycode::MetaLeft
            | Keycode::MetaRight
            | Keycode::Function
            | Keycode::CapsLock
    )
}

fn key_event(app: &AndroidApp, window: &AndroidWindowState, key: &KeyEvent<'_>) -> InputStatus {
    let code = key.key_code();
    let mods = modifiers(key.meta_state());
    window.modifiers.set(mods);
    if code == Keycode::Back {
        // The back key, as the system's back gesture: GPUI's handler when it has one, else the
        // system's own (the activity finishes).
        if key.action() == KeyAction::Up && window.back() {
            return InputStatus::Handled;
        }
        return if key.action() == KeyAction::Down && window.back_callback_set() {
            InputStatus::Handled
        } else {
            InputStatus::Unhandled
        };
    }
    if is_modifier(code) {
        window.input_callback.with(|callback| {
            callback(PlatformInput::ModifiersChanged(gpui::ModifiersChangedEvent {
                modifiers: mods,
                capslock: gpui::Capslock::default(),
            }))
        });
        return InputStatus::Handled;
    }
    // The character the key types, from the device's key map; with shift for the key's name as
    // GPUI binds it, without for `key`.
    let character = |meta: MetaState| -> Option<char> {
        let map = app.device_key_character_map(key.device_id()).ok()?;
        match map.get(code, meta).ok()? {
            android_activity::input::KeyMapChar::Unicode(c) => Some(c),
            android_activity::input::KeyMapChar::CombiningAccent(c) => Some(c),
            android_activity::input::KeyMapChar::None => None,
        }
    };
    let (key_name, key_char) = match key_name(code) {
        Some(name) => (name.to_string(), (name == "space").then(|| " ".to_string())),
        None => {
            // With ctrl or meta held the map types nothing: the bare key names it then.
            let typed = character(key.meta_state());
            let Some(bare) = character(MetaState(0)).or(typed) else { return InputStatus::Unhandled };
            let key_char = (!mods.control && !mods.platform).then(|| typed.map(|c| c.to_string())).flatten();
            (bare.to_lowercase().to_string(), key_char)
        }
    };
    let keystroke = Keystroke { modifiers: mods, key: key_name, key_char };
    let input = match key.action() {
        KeyAction::Down => PlatformInput::KeyDown(KeyDownEvent {
            keystroke,
            is_held: key.repeat_count() > 0,
            prefer_character_input: false,
        }),
        KeyAction::Up => PlatformInput::KeyUp(KeyUpEvent { keystroke }),
        _ => return InputStatus::Unhandled,
    };
    let handled = window.input_callback.with(|callback| callback(input));
    match handled {
        Some(result) if !result.propagate => InputStatus::Handled,
        _ => InputStatus::Unhandled,
    }
}
