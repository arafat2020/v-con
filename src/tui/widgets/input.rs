//! Cursor movement inside a single-line value.
//!
//! The cursor is a character index, so multibyte text stays aligned.

/// Insert `ch` at `cursor` and advance the cursor.
pub fn insert(value: &mut String, cursor: &mut usize, ch: char) {
    let index = byte_index(value, *cursor);
    value.insert(index, ch);
    *cursor += 1;
}

/// Delete the character before the cursor.
pub fn backspace(value: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    let end = byte_index(value, *cursor);
    *cursor -= 1;
    let start = byte_index(value, *cursor);
    value.replace_range(start..end, "");
}

/// Move one character left.
pub fn move_left(cursor: &mut usize) {
    *cursor = cursor.saturating_sub(1);
}

/// Move one character right, stopping at the end of `value`.
pub fn move_right(value: &str, cursor: &mut usize) {
    if *cursor < value.chars().count() {
        *cursor += 1;
    }
}

fn byte_index(value: &str, cursor: usize) -> usize {
    value
        .char_indices()
        .nth(cursor)
        .map(|(index, _)| index)
        .unwrap_or(value.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_backspace_keep_the_cursor_on_characters() {
        let mut value = "aé".to_string();
        let mut cursor = 1;
        insert(&mut value, &mut cursor, 'b');
        assert_eq!(value, "abé");
        assert_eq!(cursor, 2);
        backspace(&mut value, &mut cursor);
        assert_eq!(value, "aé");
        assert_eq!(cursor, 1);
    }
}
