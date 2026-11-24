//! The soft keyboard and IMEs, through the app's view for each window (`others::on_keyboard`).
//!
//! The view keeps an editable copy of the text: its text, selection and composing region.
//! We fill it from the focused input's text around the caret (`Keyboard::State`). The view sends
//! each edit the IME makes to it, numbered (`Edit`): the run it replaced and what replaced it, then
//! the selection and composing region. We apply it to GPUI's input handler, a composing region as
//! marked text, and send the input's text back if it came out otherwise, with the number of the
//! last edit applied, so the view drops it if it has edited since. Offsets on both sides are UTF-16,
//! as Java's and GPUI's are.
//!
//! We also say where the caret and the composing characters are (`Keyboard::Anchor`), so the IME
//! can place its candidates and a stylus can write in place.

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

/// One edit of the IME's to the text the view holds (`others::edit`), number `seq`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub seq: u32,
    /// The run of the text before the edit that `text` replaced; none when only the selection or
    /// the composing region moved.
    pub replaced: Option<TextSpan>,
    pub text: String,
    pub selection: TextSpan,
    pub compose_region: Option<TextSpan>,
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
/// `scale` turns GPUI's pixels into the view's.
#[derive(Clone, Copy)]
pub(crate) struct Keyboard {
    pub id: u64,
    pub scale: f32,
}

impl Keyboard {
    fn show(self) {
        super::others::keyboard(self.id, super::others::Keyboard::Show);
    }

    fn hide(self) {
        super::others::keyboard(self.id, super::others::Keyboard::Hide);
    }

    fn editor(self, input_type: InputType, action: u32, autocomplete: &str) {
        let autocomplete = autocomplete.to_string();
        super::others::keyboard(self.id, super::others::Keyboard::Editor { input_type: input_type.bits(), action, autocomplete });
    }

    fn state(self, seq: u32, state: TextInputState) {
        super::others::keyboard(self.id, super::others::Keyboard::State { seq, state });
    }

    fn anchor(self, rects: Vec<f32>) {
        super::others::keyboard(self.id, super::others::Keyboard::Anchor(rects));
    }
}

#[derive(Default)]
pub(crate) struct TextInput {
    mirror: RefCell<Option<Mirror>>,
    configuration: RefCell<TextInputConfiguration>,
    /// True while we apply the IME's edit, so GPUI's echo of it is not sent back mid-way.
    native_edit: Cell<bool>,
    /// The number of the view's last edit we applied.
    seq: Cell<u32>,
    /// The caret and composing characters we last said (`Keyboard::Anchor`).
    anchor: RefCell<Vec<f32>>,
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
                self.anchor.replace(Vec::new());
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
        keyboard.editor(input_type, action, &configuration.autocomplete);
        self.configuration.replace(configuration);
    }

    /// Sets the view's text from the focused input, unless it already holds the same, and says
    /// where its caret is.
    fn push(&self, keyboard: Keyboard, handlers: &CallbackSlot<PlatformInputHandler>) {
        let read = handlers.with(read_mirror).flatten();
        let Some(mirror) = read else { return };
        if self.mirror.borrow().as_ref() != Some(&mirror) {
            keyboard.state(
                self.seq.get(),
                TextInputState {
                    text: String::from_utf16_lossy(&mirror.text),
                    selection: TextSpan { start: mirror.selection.start, end: mirror.selection.end },
                    compose_region: mirror.compose.clone().map(|c| TextSpan { start: c.start, end: c.end }),
                },
            );
            self.mirror.replace(Some(mirror));
        }
        self.anchor(keyboard, handlers);
    }

    /// Applies the IME's edit to the focused input, then sends the input's text back if it came out
    /// otherwise than the view holds (a field that refuses a character, say).
    pub(crate) fn apply(&self, keyboard: Keyboard, edit: &Edit, handlers: &CallbackSlot<PlatformInputHandler>) {
        self.seq.set(edit.seq);
        // An edit to a text we never sent: the view's copy is stale, so send it ours.
        let Some(previous) = self.mirror.borrow().clone() else {
            self.push(keyboard, handlers);
            return;
        };
        let base = previous.base;
        let mut text = previous.text.clone();
        let inserted: Vec<u16> = edit.text.encode_utf16().collect();
        let replaced = edit.replaced.map(|r| {
            let range = r.start.min(r.end).min(text.len())..r.start.max(r.end).min(text.len());
            text.splice(range.clone(), inserted.iter().copied());
            range
        });
        let span = |s: TextSpan| s.start.min(s.end).min(text.len())..s.start.max(s.end).min(text.len());
        let selection = span(edit.selection);
        let compose = edit.compose_region.map(span).filter(|c| !c.is_empty());
        let in_document = |r: &Range<usize>| base + r.start..base + r.end;

        self.native_edit.set(true);
        handlers.with(|handler| {
            if !handler.query_accepts_focused_text_input() {
                return;
            }
            // A run that is not the composing region is replaced first; the composing region is
            // then marked whole, the caret relative to it.
            let mut marked_run = false;
            if let Some(range) = &replaced {
                let run = range.start..range.start + inserted.len();
                if compose.as_ref() == Some(&run) {
                    marked_run = true;
                    let caret = selection.start.saturating_sub(run.start)..selection.end.saturating_sub(run.start);
                    handler.replace_and_mark_text_in_range(Some(in_document(range)), &edit.text, Some(caret));
                } else {
                    handler.replace_text_in_range(Some(in_document(range)), &edit.text);
                }
            }
            match &compose {
                Some(c) if !marked_run => {
                    let caret = selection.start.saturating_sub(c.start)..selection.end.saturating_sub(c.start);
                    handler.replace_and_mark_text_in_range(
                        Some(in_document(c)),
                        &String::from_utf16_lossy(&text[c.clone()]),
                        Some(caret),
                    );
                }
                Some(_) => {}
                None if previous.compose.is_some() => handler.unmark_text(),
                None => {}
            }
            // The IME moved the caret alone (a swipe on the space bar): GPUI has no call that only
            // selects, but an empty replacement of an empty range puts the caret there.
            if replaced.is_none() && compose.is_none() && selection.is_empty() && selection != previous.selection {
                handler.replace_text_in_range(Some(in_document(&selection)), "");
            }
        });
        self.native_edit.set(false);
        self.mirror.replace(Some(Mirror { base, text, selection, compose }));
        self.push(keyboard, handlers);
    }

    /// Says where the caret and the composing characters are, when they moved.
    fn anchor(&self, keyboard: Keyboard, handlers: &CallbackSlot<PlatformInputHandler>) {
        let Some(mirror) = self.mirror.borrow().clone() else { return };
        let scale = keyboard.scale;
        let rects = handlers.with(|handler| {
            let caret = mirror.base + mirror.selection.end;
            let at = handler.bounds_for_range(caret..caret)?;
            let px = |p: gpui::Pixels| f32::from(p) * scale;
            let mut rects = vec![px(at.left()), px(at.top()), px(at.bottom()), px(at.bottom())];
            for i in mirror.compose.clone().into_iter().flatten() {
                let Some(b) = handler.bounds_for_range(mirror.base + i..mirror.base + i + 1) else { break };
                rects.extend([px(b.left()), px(b.top()), px(b.right()), px(b.bottom())]);
            }
            Some(rects)
        });
        let Some(Some(rects)) = rects else { return };
        if *self.anchor.borrow() != rects {
            keyboard.anchor(rects.clone());
            self.anchor.replace(rects);
        }
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
