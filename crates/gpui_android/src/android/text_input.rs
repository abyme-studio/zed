//! The soft keyboard and IMEs, through the app's view for each window (`others::on_keyboard`).
//!
//! The view keeps an editable copy of the text: its text, selection and composing region.
//! We fill it from the focused input's text around the caret, and when the IME changes it, apply
//! the difference to GPUI's input handler: a composing region as marked text, the rest as a plain
//! replacement. Offsets on both sides are UTF-16, as Java's and GPUI's are.

use std::cell::{Cell, RefCell};
use std::ops::Range;

use crate::android::CallbackSlot;
use gpui::{
    Autocapitalize, PlatformInputHandler, TextInputAction, TextInputConfiguration, TextInputMode,
    TextInputStateChange,
};

/// How much of the document, in UTF-16 units, the IME sees around the caret.
const WINDOW: usize = 4096;

/// A range of the text the view holds, in UTF-16 units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextSpan {
    pub start: usize,
    pub end: usize,
}

/// The text the view holds for the focused input: its text, selection and composing region.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextInputState {
    pub text: String,
    pub selection: TextSpan,
    pub compose_region: Option<TextSpan>,
}

/// `EditorInfo.inputType`'s bits, as Android's `InputType` names them.
#[derive(Clone, Copy, PartialEq, Eq)]
struct InputType(u32);

impl InputType {
    const TYPE_CLASS_TEXT: Self = Self(0x1);
    const TYPE_CLASS_NUMBER: Self = Self(0x2);
    const TYPE_CLASS_PHONE: Self = Self(0x3);
    const TYPE_NUMBER_FLAG_DECIMAL: Self = Self(0x2000);
    const TYPE_TEXT_VARIATION_EMAIL_ADDRESS: Self = Self(0x20);
    const TYPE_TEXT_VARIATION_URI: Self = Self(0x10);
    const TYPE_TEXT_FLAG_CAP_CHARACTERS: Self = Self(0x1000);
    const TYPE_TEXT_FLAG_CAP_WORDS: Self = Self(0x2000);
    const TYPE_TEXT_FLAG_CAP_SENTENCES: Self = Self(0x4000);
    const TYPE_TEXT_FLAG_AUTO_CORRECT: Self = Self(0x8000);
    const TYPE_TEXT_FLAG_MULTI_LINE: Self = Self(0x20000);
    const TYPE_TEXT_FLAG_NO_SUGGESTIONS: Self = Self(0x80000);

    fn empty() -> Self {
        Self(0)
    }

    fn bits(self) -> u32 {
        self.0
    }
}

impl std::ops::BitOr for InputType {
    type Output = Self;
    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl std::ops::BitOrAssign for InputType {
    fn bitor_assign(&mut self, other: Self) {
        self.0 |= other.0;
    }
}

/// `EditorInfo.IME_ACTION_*`.
mod ime {
    pub const UNSPECIFIED: u32 = 0;
    pub const NONE: u32 = 1;
    pub const GO: u32 = 2;
    pub const SEARCH: u32 = 3;
    pub const SEND: u32 = 4;
    pub const NEXT: u32 = 5;
    pub const DONE: u32 = 6;
    pub const PREVIOUS: u32 = 7;
}

/// What the view holds, as we last set or read it.
#[derive(Clone, PartialEq, Eq)]
struct Mirror {
    /// Where `text` starts in the document.
    base: usize,
    text: Vec<u16>,
    selection: Range<usize>,
    compose: Option<Range<usize>>,
}

/// Window `id`'s view, which holds the keyboard's text for it (others.rs).
#[derive(Clone, Copy)]
pub(crate) struct Keyboard(pub u64);

impl Keyboard {
    fn show(self) {
        super::others::keyboard(self.0, super::others::Keyboard::Show);
    }

    fn hide(self) {
        super::others::keyboard(self.0, super::others::Keyboard::Hide);
    }

    fn editor(self, input_type: InputType, action: u32) {
        super::others::keyboard(self.0, super::others::Keyboard::Editor { input_type: input_type.bits(), action });
    }

    fn state(self, state: TextInputState) {
        super::others::keyboard(self.0, super::others::Keyboard::State(state));
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
        keyboard: Keyboard,
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

    pub(crate) fn set_configuration(&self, keyboard: Keyboard, configuration: TextInputConfiguration) {
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
        let action = match configuration.input_action {
            TextInputAction::Unspecified => ime::UNSPECIFIED,
            TextInputAction::Enter => {
                if text {
                    input_type |= InputType::TYPE_TEXT_FLAG_MULTI_LINE;
                }
                ime::NONE
            }
            TextInputAction::Done => ime::DONE,
            TextInputAction::Go => ime::GO,
            TextInputAction::Next => ime::NEXT,
            TextInputAction::Previous => ime::PREVIOUS,
            TextInputAction::Search => ime::SEARCH,
            TextInputAction::Send => ime::SEND,
        };
        keyboard.editor(input_type, action);
        self.configuration.replace(configuration);
    }

    /// Sets the view's text from the focused input, unless it already holds the same.
    fn push(&self, keyboard: Keyboard, handlers: &CallbackSlot<PlatformInputHandler>) {
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

/// The focused input's text around its caret, as the view should hold it.
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
