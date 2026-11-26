//! Unicode's Emoji_Presentation property: the characters drawn as emoji by default.
//!
//! The ranges are Unicode 16.0.0's, copied once from regex-syntax 0.8.11's generated table
//! (`unicode_tables/property_bool.rs`, `ucd-generate property-bool ucd-16.0.0 --chars`), so gpui
//! answers as `\p{Emoji_Presentation}` did without linking regex. A test checks every char.

/// Whether `c` has Unicode's Emoji_Presentation property.
pub(crate) fn is_emoji_presentation(c: char) -> bool {
    EMOJI_PRESENTATION
        .binary_search_by(|&(start, end)| {
            if end < c {
                std::cmp::Ordering::Less
            } else if start > c {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

const EMOJI_PRESENTATION: &[(char, char)] = &[
    ('⌚', '⌛'),
    ('⏩', '⏬'),
    ('⏰', '⏰'),
    ('⏳', '⏳'),
    ('◽', '◾'),
    ('☔', '☕'),
    ('♈', '♓'),
    ('♿', '♿'),
    ('⚓', '⚓'),
    ('⚡', '⚡'),
    ('⚪', '⚫'),
    ('⚽', '⚾'),
    ('⛄', '⛅'),
    ('⛎', '⛎'),
    ('⛔', '⛔'),
    ('⛪', '⛪'),
    ('⛲', '⛳'),
    ('⛵', '⛵'),
    ('⛺', '⛺'),
    ('⛽', '⛽'),
    ('✅', '✅'),
    ('✊', '✋'),
    ('✨', '✨'),
    ('❌', '❌'),
    ('❎', '❎'),
    ('❓', '❕'),
    ('❗', '❗'),
    ('➕', '➗'),
    ('➰', '➰'),
    ('➿', '➿'),
    ('⬛', '⬜'),
    ('⭐', '⭐'),
    ('⭕', '⭕'),
    ('🀄', '🀄'),
    ('🃏', '🃏'),
    ('🆎', '🆎'),
    ('🆑', '🆚'),
    ('🇦', '🇿'),
    ('🈁', '🈁'),
    ('🈚', '🈚'),
    ('🈯', '🈯'),
    ('🈲', '🈶'),
    ('🈸', '🈺'),
    ('🉐', '🉑'),
    ('🌀', '🌠'),
    ('🌭', '🌵'),
    ('🌷', '🍼'),
    ('🍾', '🎓'),
    ('🎠', '🏊'),
    ('🏏', '🏓'),
    ('🏠', '🏰'),
    ('🏴', '🏴'),
    ('🏸', '🐾'),
    ('👀', '👀'),
    ('👂', '📼'),
    ('📿', '🔽'),
    ('🕋', '🕎'),
    ('🕐', '🕧'),
    ('🕺', '🕺'),
    ('🖕', '🖖'),
    ('🖤', '🖤'),
    ('🗻', '🙏'),
    ('🚀', '🛅'),
    ('🛌', '🛌'),
    ('🛐', '🛒'),
    ('🛕', '🛗'),
    ('🛜', '🛟'),
    ('🛫', '🛬'),
    ('🛴', '🛼'),
    ('🟠', '🟫'),
    ('🟰', '🟰'),
    ('🤌', '🤺'),
    ('🤼', '🥅'),
    ('🥇', '🧿'),
    ('🩰', '🩼'),
    ('🪀', '🪉'),
    ('🪏', '🫆'),
    ('🫎', '🫜'),
    ('🫟', '🫩'),
    ('🫰', '🫸'),
];

#[cfg(test)]
mod tests {
    #[test]
    fn every_char_answers_as_the_regex_did() {
        let regex = regex::Regex::new("\\p{Emoji_Presentation}").unwrap();
        let mut buf = [0u8; 4];
        for c in (0..=char::MAX as u32).filter_map(char::from_u32) {
            assert_eq!(
                super::is_emoji_presentation(c),
                regex.is_match(c.encode_utf8(&mut buf)),
                "{:?}",
                c
            );
        }
    }
}
