/// Render a single bounded terminal line from untrusted text.
#[must_use]
pub fn terminal_text(value: &str, maximum_length: usize) -> String {
    let sanitized: String = value
        .chars()
        .map(|character| {
            if matches!(character, '\n' | '\r' | '\t') {
                ' '
            } else if is_unsafe_terminal_character(character) {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect();
    let compact = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");

    if compact.chars().count() <= maximum_length {
        return compact;
    }
    if maximum_length == 0 {
        return String::new();
    }

    let mut bounded: String = compact.chars().take(maximum_length - 1).collect();
    bounded.push('…');
    bounded
}

fn is_unsafe_terminal_character(character: char) -> bool {
    character.is_control()
        || matches!(
            character as u32,
            0x007f..=0x009f | 0x202a..=0x202e | 0x2066..=0x2069
        )
}

#[cfg(test)]
mod tests {
    use super::terminal_text;

    #[test]
    fn removes_terminal_and_bidirectional_controls() {
        let hostile = "Fix docs\n\u{1b}]0;forged\u{7}\u{1b}[31mred\u{1b}[0m\u{202e}abc";
        let rendered = terminal_text(hostile, 300);

        assert!(!rendered.contains('\n'));
        assert!(!rendered.contains('\u{1b}'));
        assert!(!rendered.contains('\u{7}'));
        assert!(!rendered.contains('\u{202e}'));
        assert!(rendered.starts_with("Fix docs "));
    }

    #[test]
    fn bounds_by_unicode_scalar_count() {
        let rendered = terminal_text(&"x".repeat(500), 20);
        assert_eq!(rendered.chars().count(), 20);
        assert!(rendered.ends_with('…'));
        assert_eq!(terminal_text("anything", 0), "");
    }
}
