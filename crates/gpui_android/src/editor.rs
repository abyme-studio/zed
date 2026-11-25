//! A field's configuration as Android's `EditorInfo` reads it, apart from the platform so it is
//! tested on any machine (`android/text_input.rs` sends it to the view).

use gpui::{Autocapitalize, TextInputAction, TextInputConfiguration, TextInputMode};

/// `EditorInfo.inputType`'s bits, as Android's `InputType` names them.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct InputType(u32);

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

    pub fn bits(self) -> u32 {
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

/// A configuration's `EditorInfo.inputType` and `imeOptions` action. Autocorrect off sets only
/// `TYPE_TEXT_FLAG_NO_SUGGESTIONS`, never Flutter's second flag, the visible-password variation: the
/// host turns autocorrect off on every field that does not ask for it, and that variation would
/// make each one a password to autofill and drop Japanese compose on many keyboards.
pub fn editor_of(c: &TextInputConfiguration) -> (InputType, u32) {
    // The text flags are bits that mean other things on the number and phone classes.
    let (mut input_type, text) = match c.input_mode {
        TextInputMode::Decimal => (InputType::TYPE_CLASS_NUMBER | InputType::TYPE_NUMBER_FLAG_DECIMAL, false),
        TextInputMode::Numeric => (InputType::TYPE_CLASS_NUMBER, false),
        TextInputMode::Tel => (InputType::TYPE_CLASS_PHONE, false),
        TextInputMode::Email => (InputType::TYPE_CLASS_TEXT | InputType::TYPE_TEXT_VARIATION_EMAIL_ADDRESS, true),
        TextInputMode::Url => (InputType::TYPE_CLASS_TEXT | InputType::TYPE_TEXT_VARIATION_URI, true),
        TextInputMode::Text | TextInputMode::Search | TextInputMode::None => (InputType::TYPE_CLASS_TEXT, true),
    };
    if text {
        input_type |= match c.autocapitalize {
            Autocapitalize::None => InputType::empty(),
            Autocapitalize::Words => InputType::TYPE_TEXT_FLAG_CAP_WORDS,
            Autocapitalize::Sentences => InputType::TYPE_TEXT_FLAG_CAP_SENTENCES,
            Autocapitalize::Characters => InputType::TYPE_TEXT_FLAG_CAP_CHARACTERS,
        };
        if c.autocorrect {
            input_type |= InputType::TYPE_TEXT_FLAG_AUTO_CORRECT;
        }
        if !c.suggestions {
            input_type |= InputType::TYPE_TEXT_FLAG_NO_SUGGESTIONS;
        }
    }
    let action = match c.input_action {
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
    (input_type, action)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(f: impl FnOnce(&mut TextInputConfiguration)) -> (u32, u32) {
        let mut c = TextInputConfiguration::default();
        f(&mut c);
        let (input_type, action) = editor_of(&c);
        (input_type.bits(), action)
    }

    #[test]
    fn each_action_maps_to_its_ime_option() {
        let cases = [
            (TextInputAction::Unspecified, ime::UNSPECIFIED),
            (TextInputAction::Enter, ime::NONE),
            (TextInputAction::Done, ime::DONE),
            (TextInputAction::Go, ime::GO),
            (TextInputAction::Next, ime::NEXT),
            (TextInputAction::Previous, ime::PREVIOUS),
            (TextInputAction::Search, ime::SEARCH),
            (TextInputAction::Send, ime::SEND),
        ];
        for (a, want) in cases {
            assert_eq!(editor(|c| c.input_action = a).1, want, "{a:?}");
        }
        // Enter is a new line on a text field, and adds nothing to a number's.
        assert_eq!(editor(|c| c.input_action = TextInputAction::Enter).0 & 0x20000, 0x20000);
        let number = editor(|c| {
            c.input_mode = TextInputMode::Numeric;
            c.input_action = TextInputAction::Enter;
        });
        assert_eq!(number.0, 0x2);
    }

    #[test]
    fn each_capitalization_sets_its_flag() {
        let cases = [
            (Autocapitalize::None, 0),
            (Autocapitalize::Words, 0x2000),
            (Autocapitalize::Sentences, 0x4000),
            (Autocapitalize::Characters, 0x1000),
        ];
        for (a, want) in cases {
            assert_eq!(editor(|c| c.autocapitalize = a).0 & 0x7000, want, "{a:?}");
        }
    }

    #[test]
    fn autocorrect_off_asks_for_no_suggestions_and_stays_plain_text() {
        let (off, _) = editor(|c| {
            c.autocorrect = false;
            c.suggestions = false;
        });
        assert_eq!(off, 0x1 | 0x80000);
        // Not the visible-password variation, which would make the field a password.
        assert_eq!(off & 0xff0, 0);
        let (on, _) = editor(|c| {
            c.autocorrect = true;
            c.suggestions = true;
        });
        assert_eq!(on, 0x1 | 0x8000);
    }

    #[test]
    fn number_and_phone_fields_take_no_text_flags() {
        for (mode, want) in [(TextInputMode::Numeric, 0x2), (TextInputMode::Decimal, 0x2002), (TextInputMode::Tel, 0x3)] {
            let (t, _) = editor(|c| {
                c.input_mode = mode;
                c.autocapitalize = Autocapitalize::Sentences;
            });
            assert_eq!(t, want, "{mode:?}");
        }
        assert_eq!(editor(|c| c.input_mode = TextInputMode::Url).0, 0x1 | 0x10 | 0x80000);
        assert_eq!(editor(|c| c.input_mode = TextInputMode::Email).0, 0x1 | 0x20 | 0x80000);
    }
}
