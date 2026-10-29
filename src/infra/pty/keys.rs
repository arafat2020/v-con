//! Translate crossterm key and paste events into bytes for a remote PTY.

use crossterm::event::{KeyCode, KeyModifiers};

/// Modes reported by the remote terminal emulator that change encoding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyModes {
    /// When true, arrows and Home/End use SS3 (`\x1bOA`…) instead of CSI.
    pub application_cursor: bool,
}

/// Encode one key press into bytes for the PTY slave.
///
/// Printable characters become UTF-8. Control and navigation keys use the
/// usual xterm sequences. Unknown keys yield an empty buffer.
pub fn encode_key(code: KeyCode, modifiers: KeyModifiers, modes: &KeyModes) -> Vec<u8> {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let alt = modifiers.contains(KeyModifiers::ALT);
    let shift = modifiers.contains(KeyModifiers::SHIFT);

    match code {
        KeyCode::Enter => prefix_alt(alt, b"\r"),
        KeyCode::Backspace => {
            if ctrl {
                prefix_alt(alt, b"\x08")
            } else {
                prefix_alt(alt, b"\x7f")
            }
        }
        KeyCode::Tab => prefix_alt(alt, b"\t"),
        KeyCode::BackTab => prefix_alt(alt, b"\x1b[Z"),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Delete => prefix_alt(alt, b"\x1b[3~"),
        KeyCode::Insert => prefix_alt(alt, b"\x1b[2~"),
        KeyCode::Home => {
            if modes.application_cursor {
                prefix_alt(alt, b"\x1bOH")
            } else {
                prefix_alt(alt, b"\x1b[H")
            }
        }
        KeyCode::End => {
            if modes.application_cursor {
                prefix_alt(alt, b"\x1bOF")
            } else {
                prefix_alt(alt, b"\x1b[F")
            }
        }
        KeyCode::PageUp => prefix_alt(alt, b"\x1b[5~"),
        KeyCode::PageDown => prefix_alt(alt, b"\x1b[6~"),
        KeyCode::Up => arrow(b'A', modes.application_cursor, alt),
        KeyCode::Down => arrow(b'B', modes.application_cursor, alt),
        KeyCode::Right => arrow(b'C', modes.application_cursor, alt),
        KeyCode::Left => arrow(b'D', modes.application_cursor, alt),
        KeyCode::F(n) => encode_fn(n, alt),
        KeyCode::Char(ch) => encode_char(ch, ctrl, alt, shift),
        _ => Vec::new(),
    }
}

/// Encode a paste. When `bracketed` is true, wrap with the xterm bracketed-paste
/// markers so apps that requested the mode can treat the text as a paste.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let mut out = Vec::with_capacity(text.len() + 12);
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.as_bytes().to_vec()
    }
}

fn encode_char(ch: char, ctrl: bool, alt: bool, _shift: bool) -> Vec<u8> {
    if ctrl && let Some(byte) = ctrl_byte(ch) {
        return prefix_alt(alt, &[byte]);
    }
    let mut buf = [0u8; 4];
    let encoded = ch.encode_utf8(&mut buf).as_bytes();
    prefix_alt(alt, encoded)
}

fn ctrl_byte(ch: char) -> Option<u8> {
    let lower = ch.to_ascii_lowercase();
    match lower {
        'a'..='z' => Some((lower as u8) & 0x1f),
        '@' | ' ' => Some(0x00),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' | '-' => Some(0x1f),
        '?' => Some(0x7f),
        _ => None,
    }
}

fn arrow(letter: u8, application: bool, alt: bool) -> Vec<u8> {
    if application {
        prefix_alt(alt, &[0x1b, b'O', letter])
    } else {
        prefix_alt(alt, &[0x1b, b'[', letter])
    }
}

fn encode_fn(n: u8, alt: bool) -> Vec<u8> {
    let seq: &[u8] = match n {
        1 => b"\x1bOP",
        2 => b"\x1bOQ",
        3 => b"\x1bOR",
        4 => b"\x1bOS",
        5 => b"\x1b[15~",
        6 => b"\x1b[17~",
        7 => b"\x1b[18~",
        8 => b"\x1b[19~",
        9 => b"\x1b[20~",
        10 => b"\x1b[21~",
        11 => b"\x1b[23~",
        12 => b"\x1b[24~",
        _ => return Vec::new(),
    };
    prefix_alt(alt, seq)
}

fn prefix_alt(alt: bool, bytes: &[u8]) -> Vec<u8> {
    if alt {
        let mut out = Vec::with_capacity(bytes.len() + 1);
        out.push(0x1b);
        out.extend_from_slice(bytes);
        out
    } else {
        bytes.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn modes(app: bool) -> KeyModes {
        KeyModes {
            application_cursor: app,
        }
    }

    #[test]
    fn printable_and_enter_backspace_tab_esc() {
        assert_eq!(
            encode_key(KeyCode::Char('a'), KeyModifiers::NONE, &modes(false)),
            b"a"
        );
        assert_eq!(
            encode_key(KeyCode::Char('字'), KeyModifiers::NONE, &modes(false)),
            "字".as_bytes()
        );
        assert_eq!(
            encode_key(KeyCode::Enter, KeyModifiers::NONE, &modes(false)),
            b"\r"
        );
        assert_eq!(
            encode_key(KeyCode::Backspace, KeyModifiers::NONE, &modes(false)),
            b"\x7f"
        );
        assert_eq!(
            encode_key(KeyCode::Tab, KeyModifiers::NONE, &modes(false)),
            b"\t"
        );
        assert_eq!(
            encode_key(KeyCode::BackTab, KeyModifiers::NONE, &modes(false)),
            b"\x1b[Z"
        );
        assert_eq!(
            encode_key(KeyCode::Esc, KeyModifiers::NONE, &modes(false)),
            b"\x1b"
        );
    }

    #[test]
    fn ctrl_letters_and_specials() {
        assert_eq!(
            encode_key(KeyCode::Char('c'), KeyModifiers::CONTROL, &modes(false)),
            b"\x03"
        );
        assert_eq!(
            encode_key(KeyCode::Char('C'), KeyModifiers::CONTROL, &modes(false)),
            b"\x03"
        );
        assert_eq!(
            encode_key(KeyCode::Char('r'), KeyModifiers::CONTROL, &modes(false)),
            b"\x12"
        );
        assert_eq!(
            encode_key(KeyCode::Char(']'), KeyModifiers::CONTROL, &modes(false)),
            b"\x1d"
        );
    }

    #[test]
    fn alt_prefixes_escape() {
        assert_eq!(
            encode_key(KeyCode::Char('x'), KeyModifiers::ALT, &modes(false)),
            b"\x1bx"
        );
        assert_eq!(
            encode_key(KeyCode::Enter, KeyModifiers::ALT, &modes(false)),
            b"\x1b\r"
        );
    }

    #[test]
    fn arrows_switch_with_application_cursor() {
        assert_eq!(
            encode_key(KeyCode::Up, KeyModifiers::NONE, &modes(false)),
            b"\x1b[A"
        );
        assert_eq!(
            encode_key(KeyCode::Up, KeyModifiers::NONE, &modes(true)),
            b"\x1bOA"
        );
        assert_eq!(
            encode_key(KeyCode::Left, KeyModifiers::NONE, &modes(true)),
            b"\x1bOD"
        );
        assert_eq!(
            encode_key(KeyCode::Home, KeyModifiers::NONE, &modes(false)),
            b"\x1b[H"
        );
        assert_eq!(
            encode_key(KeyCode::Home, KeyModifiers::NONE, &modes(true)),
            b"\x1bOH"
        );
        assert_eq!(
            encode_key(KeyCode::End, KeyModifiers::NONE, &modes(true)),
            b"\x1bOF"
        );
    }

    #[test]
    fn navigation_and_function_keys() {
        assert_eq!(
            encode_key(KeyCode::PageUp, KeyModifiers::NONE, &modes(false)),
            b"\x1b[5~"
        );
        assert_eq!(
            encode_key(KeyCode::PageDown, KeyModifiers::NONE, &modes(false)),
            b"\x1b[6~"
        );
        assert_eq!(
            encode_key(KeyCode::Insert, KeyModifiers::NONE, &modes(false)),
            b"\x1b[2~"
        );
        assert_eq!(
            encode_key(KeyCode::Delete, KeyModifiers::NONE, &modes(false)),
            b"\x1b[3~"
        );
        assert_eq!(
            encode_key(KeyCode::F(1), KeyModifiers::NONE, &modes(false)),
            b"\x1bOP"
        );
        assert_eq!(
            encode_key(KeyCode::F(5), KeyModifiers::NONE, &modes(false)),
            b"\x1b[15~"
        );
        assert_eq!(
            encode_key(KeyCode::F(12), KeyModifiers::NONE, &modes(false)),
            b"\x1b[24~"
        );
    }

    #[test]
    fn paste_respects_bracketed_mode() {
        assert_eq!(encode_paste("hi", false), b"hi");
        assert_eq!(encode_paste("hi", true), b"\x1b[200~hi\x1b[201~");
    }

    #[test]
    fn encode_key_never_panics_on_random_inputs() {
        use proptest::prelude::*;
        proptest!(|(byte in any::<u8>(), mods in 0u8..8, app in any::<bool>())| {
            let code = match byte % 12 {
                0 => KeyCode::Enter,
                1 => KeyCode::Esc,
                2 => KeyCode::Backspace,
                3 => KeyCode::Tab,
                4 => KeyCode::Up,
                5 => KeyCode::Down,
                6 => KeyCode::Left,
                7 => KeyCode::Right,
                8 => KeyCode::Home,
                9 => KeyCode::End,
                10 => KeyCode::F((byte % 12) + 1),
                _ => KeyCode::Char(char::from(byte)),
            };
            let modifiers = KeyModifiers::from_bits_truncate(mods);
            let modes = KeyModes {
                application_cursor: app,
            };
            let _ = encode_key(code, modifiers, &modes);
        });
    }
}
