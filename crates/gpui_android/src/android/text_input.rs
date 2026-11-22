//! The soft keyboard and IMEs, through GameActivity's own `InputConnection` (GameTextInput), or,
//! for a window beyond the activity's own, through the app's view for it (`others::on_keyboard`).
//!
//! GameTextInput keeps an editable copy of the text: its text, selection and composing region.
//! We fill it from the focused input's text around the caret, and when the IME changes it, apply
//! the difference to GPUI's input handler: a composing region as marked text, the rest as a plain
//! replacement. Offsets on both sides are UTF-16, as Java's and GPUI's are.

use std::cell::{Cell, RefCell};
use std::ops::Range;

use android_activity::AndroidApp;
use android_activity::input::{ImeOptions, InputType, TextInputState, TextSpan};
use crate::android::CallbackSlot;
use gpui::{
    Autocapitalize, PlatformInputHandler, TextInputAction, TextInputConfiguration, TextInputMode,
    TextInputStateChange,
};

/// How much of the document, in UTF-16 units, the IME sees around the caret.
const WINDOW: usize = 4096;

/// What GameTextInput holds, as we last set or read it.
#[derive(Clone, PartialEq, Eq)]
struct Mirror {
    /// Where `text` starts in the document.
    base: usize,
    text: Vec<u16>,
    selection: Range<usize>,
    compose: Option<Range<usize>>,
}

/// Where the keyboard is: the activity's own, or the app's view for window `id` (others.rs).
#[derive(Clone, Copy)]
pub(crate) enum Keyboard<'a> {
    Activity(&'a AndroidApp),
    Other(u64),
}

impl Keyboard<'_> {
    fn show(self) {
        match self {
            Keyboard::Activity(app) => app.show_soft_input(false),
            Keyboard::Other(id) => super::others::keyboard(id, super::others::Keyboard::Show),
        }
    }

    fn hide(self) {
        match self {
            Keyboard::Activity(app) => app.hide_soft_input(false),
            Keyboard::Other(id) => super::others::keyboard(id, super::others::Keyboard::Hide),
        }
    }

    fn editor(self, input_type: InputType, action: android_activity::input::TextInputAction) {
        match self {
            Keyboard::Activity(app) => app.set_ime_editor_info(input_type, action, ImeOptions::IME_FLAG_NO_FULLSCREEN),
            Keyboard::Other(id) => {
                use android_activity::input::TextInputAction as Ime;
                // `EditorInfo.IME_ACTION_*`.
                let action = match action {
                    Ime::None => 1,
                    Ime::Go => 2,
                    Ime::Search => 3,
                    Ime::Send => 4,
                    Ime::Next => 5,
                    Ime::Done => 6,
                    Ime::Previous => 7,
                    _ => 0,
                };
                super::others::keyboard(id, super::others::Keyboard::Editor { input_type: input_type.bits(), action })
            }
        }
    }

    fn state(self, state: TextInputState) {
        match self {
            Keyboard::Activity(app) => app.set_text_input_state(state),
            Keyboard::Other(id) => super::others::keyboard(id, super::others::Keyboard::State(state)),
        }
    }
}

#[derive(Default)]
pub(crate) struct TextInput {
    mirror: RefCell<Option<Mirror>>,
    configuration: RefCell<TextInputConfiguration>,
    /// True while we apply the IME's edit, so GPUI's echo of it is not sent back.
    native_edit: Cell<bool>,
}

impl TextInput {
    pub(crate) fn state_changed(
        &self,
        keyboard: Keyboard<'_>,
        handlers: &CallbackSlot<PlatformInputHandler>,
        change: TextInputStateChange,
    ) {
        match change {
            TextInputStateChange::FocusGained => {
                self.mirror.replace(None);
                self.push(keyboard, handlers);
                keyboard.show();
            }
            TextInputStateChange::FocusLost => {
                self.mirror.replace(None);
                keyboard.hide();
            }
            TextInputStateChange::SelectionChanged | TextInputStateChange::ContentChanged => {
                if !self.native_edit.get() {
                    self.push(keyboard, handlers);
                }
            }
        }
    }

    pub(crate) fn set_configuration(&self, keyboard: Keyboard<'_>, configuration: TextInputConfiguration) {
        if *self.configuration.borrow() == configuration {
            return;
        }
        // The text flags are bits that mean other things on the number and phone classes.
        let (mut input_type, text) = match configuration.input_mode {
            TextInputMode::Decimal => (InputType::TYPE_CLASS_NUMBER | InputType::TYPE_NUMBER_FLAG_DECIMAL, false),
            TextInputMode::Numeric => (InputType::TYPE_CLASS_NUMBER, false),
            TextInputMode::Tel => (InputType::TYPE_CLASS_PHONE, false),
            TextInputMode::Email => (InputType::TYPE_CLASS_TEXT | InputType::TYPE_TEXT_VARIATION_EMAIL_ADDRESS, true),
            TextInputMode::Url => (InputType::TYPE_CLASS_TEXT | InputType::TYPE_TEXT_VARIATION_URI, true),
            TextInputMode::Text | TextInputMode::Search | TextInputMode::None => (InputType::TYPE_CLASS_TEXT, true),
        };
        if text {
            input_type |= match configuration.autocapitalize {
                Autocapitalize::None => InputType::empty(),
                Autocapitalize::Words => InputType::TYPE_TEXT_FLAG_CAP_WORDS,
                Autocapitalize::Sentences => InputType::TYPE_TEXT_FLAG_CAP_SENTENCES,
                Autocapitalize::Characters => InputType::TYPE_TEXT_FLAG_CAP_CHARACTERS,
            };
            if configuration.autocorrect {
                input_type |= InputType::TYPE_TEXT_FLAG_AUTO_CORRECT;
            }
            if !configuration.suggestions {
                input_type |= InputType::TYPE_TEXT_FLAG_NO_SUGGESTIONS;
            }
        }
        use android_activity::input::TextInputAction as Ime;
        let action = match configuration.input_action {
            TextInputAction::Unspecified => Ime::Unspecified,
            TextInputAction::Enter => {
                if text {
                    input_type |= InputType::TYPE_TEXT_FLAG_MULTI_LINE;
                }
                Ime::None
            }
            TextInputAction::Done => Ime::Done,
            TextInputAction::Go => Ime::Go,
            TextInputAction::Next => Ime::Next,
            TextInputAction::Previous => Ime::Previous,
            TextInputAction::Search => Ime::Search,
            TextInputAction::Send => Ime::Send,
        };
        keyboard.editor(input_type, action);
        self.configuration.replace(configuration);
    }

    /// Sets GameTextInput from the focused input, unless it already holds the same.
    fn push(&self, keyboard: Keyboard<'_>, handlers: &CallbackSlot<PlatformInputHandler>) {
        let read = handlers.with(read_mirror).flatten();
        let Some(mirror) = read else { return };
        if self.mirror.borrow().as_ref() == Some(&mirror) {
            return;
        }
        keyboard.state(TextInputState {
            text: String::from_utf16_lossy(&mirror.text),
            selection: TextSpan { start: mirror.selection.start, end: mirror.selection.end },
            compose_region: mirror.compose.clone().map(|c| TextSpan { start: c.start, end: c.end }),
        });
        self.mirror.replace(Some(mirror));
    }

    /// Applies the IME's new state to the focused input.
    pub(crate) fn apply(
        &self,
        state: &TextInputState,
        handlers: &CallbackSlot<PlatformInputHandler>,
    ) {
        let new: Vec<u16> = state.text.encode_utf16().collect();
        let span = |s: TextSpan| s.start.min(s.end).min(new.len())..s.start.max(s.end).min(new.len());
        let selection = span(state.selection);
        let compose = state.compose_region.map(span).filter(|c| !c.is_empty());
        let previous = self.mirror.borrow().clone();
        let base = previous.as_ref().map_or(0, |m| m.base);
        let old: &[u16] = previous.as_ref().map_or(&[], |m| &m.text);

        // The changed run: what lies between the common prefix and suffix, widened to cover the
        // composing region so all of it ends up marked.
        let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
        let max_suffix = old.len().min(new.len()) - prefix;
        let suffix = old.iter().rev().zip(new.iter().rev()).take(max_suffix).take_while(|(a, b)| a == b).count();
        let (mut start, mut new_end) = (prefix, new.len() - suffix);
        if let Some(c) = &compose {
            start = start.min(c.start);
            new_end = new_end.max(c.end);
        }
        let old_end = old.len() - (new.len() - new_end);
        let replaced = base + start..base + old_end;
        let inserted = String::from_utf16_lossy(&new[start..new_end]);
        let text_changed = old != new.as_slice();
        let compose_changed = previous.as_ref().map(|m| &m.compose) != Some(&compose);

        self.native_edit.set(true);
        handlers.with(|handler| {
            if !handler.query_accepts_focused_text_input() {
                return;
            }
            if compose.is_some() {
                // The caret, relative to the marked text.
                let caret = selection.start.saturating_sub(start)..selection.end.saturating_sub(start);
                handler.replace_and_mark_text_in_range(Some(replaced.clone()), &inserted, Some(caret));
            } else if text_changed {
                handler.replace_text_in_range(Some(replaced.clone()), &inserted);
            } else if compose_changed {
                handler.unmark_text();
            }
        });
        self.native_edit.set(false);
        self.mirror.replace(Some(Mirror { base, text: new, selection, compose }));
    }
}

/// The focused input's text around its caret, as GameTextInput should hold it.
fn read_mirror(handler: &mut PlatformInputHandler) -> Option<Mirror> {
    if !handler.query_accepts_focused_text_input() {
        return None;
    }
    let document = handler
        .text_input_editable_range()
        .or_else(|| handler.text_length_utf16().map(|length| 0..length))
        .filter(|range| range.start <= range.end)?;
    let selection = handler.selected_text_range(false)?.range;
    let start = selection.start.saturating_sub(WINDOW / 2).max(document.start);
    let end = (start + WINDOW).min(document.end);
    let mut adjusted = None;
    let text = handler.text_for_range(start..end, &mut adjusted)?;
    let base = adjusted.map_or(start, |range| range.start);
    let text: Vec<u16> = text.encode_utf16().collect();
    let local = |range: Range<usize>| {
        range.start.saturating_sub(base).min(text.len())..range.end.saturating_sub(base).min(text.len())
    };
    let compose = handler.marked_text_range().map(local).filter(|c| !c.is_empty());
    Some(Mirror { base, selection: local(selection), compose, text })
}
