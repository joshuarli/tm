/// Key code: lower 21 bits = Unicode codepoint or special key, upper bits = modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct KeyCode(pub(crate) u32);

impl KeyCode {
    pub(crate) const CTRL: u32 = 1 << 24;
    pub(crate) const META: u32 = 1 << 25;
    pub(crate) const SHIFT: u32 = 1 << 26;

    // Special keys (values above Unicode range)
    pub(crate) const UP: u32 = 0x110000;
    pub(crate) const DOWN: u32 = 0x110001;
    pub(crate) const LEFT: u32 = 0x110002;
    pub(crate) const RIGHT: u32 = 0x110003;
    pub(crate) const HOME: u32 = 0x110004;
    pub(crate) const END: u32 = 0x110005;
    pub(crate) const INSERT: u32 = 0x110006;
    pub(crate) const DELETE: u32 = 0x110007;
    pub(crate) const PAGEUP: u32 = 0x110008;
    pub(crate) const PAGEDOWN: u32 = 0x110009;
    pub(crate) const F1: u32 = 0x110010;
    pub(crate) const F2: u32 = 0x110011;
    pub(crate) const F3: u32 = 0x110012;
    pub(crate) const F4: u32 = 0x110013;
    pub(crate) const F5: u32 = 0x110014;
    pub(crate) const F6: u32 = 0x110015;
    pub(crate) const F7: u32 = 0x110016;
    pub(crate) const F8: u32 = 0x110017;
    pub(crate) const F9: u32 = 0x110018;
    pub(crate) const F10: u32 = 0x110019;
    pub(crate) const F11: u32 = 0x11001A;
    pub(crate) const F12: u32 = 0x11001B;
    pub(crate) const ESCAPE: u32 = 0x1B;
    pub(crate) const ENTER: u32 = 0x0D;
    pub(crate) const TAB: u32 = 0x09;
    pub(crate) const BACKSPACE: u32 = 0x7F;

    pub(crate) fn char(ch: char) -> Self {
        Self(ch as u32)
    }

    pub(crate) fn ctrl(ch: char) -> Self {
        Self((ch as u32) | Self::CTRL)
    }

    pub(crate) fn base(self) -> u32 {
        self.0 & 0x1FFFFF
    }

    pub(crate) fn has_ctrl(self) -> bool {
        self.0 & Self::CTRL != 0
    }

    pub(crate) fn has_shift(self) -> bool {
        self.0 & Self::SHIFT != 0
    }

    pub(crate) fn has_meta(self) -> bool {
        self.0 & Self::META != 0
    }
}

/// Mouse event types.
#[derive(Clone, Copy, Debug)]
pub(crate) enum MouseEvent {
    Press {
        button: u8,
        x: u32,
        y: u32,
    },
    Release {
        x: u32,
        y: u32,
    },
    Drag {
        button: u8,
        x: u32,
        y: u32,
    },
    WheelUp {
        x: u32,
        y: u32,
    },
    WheelDown {
        x: u32,
        y: u32,
    },
}

/// Parsed input event.
#[derive(Clone, Debug)]
pub(crate) enum InputEvent {
    Key(KeyCode),
    Mouse(MouseEvent),
    Paste(Vec<u8>),
    FocusIn,
    FocusOut,
}

/// Parse raw input bytes into events. Returns events and bytes consumed.
pub(crate) fn parse_input(buf: &[u8]) -> (Vec<InputEvent>, usize) {
    let mut events = Vec::new();
    let mut pos = 0;

    while pos < buf.len() {
        let remaining = &buf[pos..];

        // Try to parse an escape sequence
        if remaining[0] == 0x1B {
            if remaining.len() == 1 {
                // Lone ESC — could be prefix to a sequence, wait for more data
                // But if this is all we have, treat as Escape key
                break;
            }

            // Focus events: ESC [ I / ESC [ O
            if remaining.len() >= 3 && remaining[1] == b'[' {
                if remaining[2] == b'I' {
                    events.push(InputEvent::FocusIn);
                    pos += 3;
                    continue;
                }
                if remaining[2] == b'O' {
                    events.push(InputEvent::FocusOut);
                    pos += 3;
                    continue;
                }
            }

            // Bracketed paste: ESC [ 200 ~ ... ESC [ 201 ~
            if remaining.starts_with(b"\x1b[200~") {
                let start = 6;
                if let Some(end_offset) = find_subsequence(&remaining[start..], b"\x1b[201~") {
                    let paste_data = remaining[start..start + end_offset].to_vec();
                    events.push(InputEvent::Paste(paste_data));
                    pos += start + end_offset + 6;
                    continue;
                } else {
                    // Incomplete paste — wait for more data
                    break;
                }
            }

            // SGR mouse: ESC [ < Cb ; Cx ; Cy M/m
            if remaining.len() >= 4 && remaining[1] == b'[' && remaining[2] == b'<' {
                if let Some((evt, consumed)) = parse_sgr_mouse(&remaining[3..]) {
                    events.push(InputEvent::Mouse(evt));
                    pos += 3 + consumed;
                    continue;
                }
                // Could be incomplete — wait if no final M/m found
                if !remaining[3..]
                    .iter()
                    .any(|&b| b == b'M' || b == b'm')
                {
                    break;
                }
            }

            // CSI sequences: ESC [ ...
            if remaining.len() >= 3 && remaining[1] == b'[' {
                if let Some((key, consumed)) = parse_csi_key(&remaining[2..]) {
                    events.push(InputEvent::Key(key));
                    pos += 2 + consumed;
                    continue;
                }
            }

            // SS3 sequences: ESC O ...
            if remaining.len() >= 3 && remaining[1] == b'O' {
                if let Some(key) = parse_ss3_key(remaining[2]) {
                    events.push(InputEvent::Key(key));
                    pos += 3;
                    continue;
                }
            }

            // ESC + printable = Meta + key
            if remaining.len() >= 2 && remaining[1] >= 0x20 && remaining[1] <= 0x7E {
                let key = KeyCode(remaining[1] as u32 | KeyCode::META);
                events.push(InputEvent::Key(key));
                pos += 2;
                continue;
            }

            // ESC + ctrl char
            if remaining.len() >= 2 && remaining[1] < 0x20 {
                // e.g., ESC Ctrl-A
                let ch = (remaining[1] + b'a' - 1) as u32;
                let key = KeyCode(ch | KeyCode::CTRL | KeyCode::META);
                events.push(InputEvent::Key(key));
                pos += 2;
                continue;
            }

            // Unrecognized ESC sequence — treat as Escape key
            events.push(InputEvent::Key(KeyCode(KeyCode::ESCAPE)));
            pos += 1;
            continue;
        }

        // Regular bytes
        match remaining[0] {
            0x00 => {
                // Ctrl-Space
                events.push(InputEvent::Key(KeyCode(b' ' as u32 | KeyCode::CTRL)));
                pos += 1;
            }
            0x01..=0x1A => {
                // Ctrl-A through Ctrl-Z
                let ch = (remaining[0] + b'a' - 1) as u32;
                events.push(InputEvent::Key(KeyCode(ch | KeyCode::CTRL)));
                pos += 1;
            }
            0x1B => unreachable!(), // handled above
            0x1C..=0x1F => {
                // Other control chars
                pos += 1;
            }
            0x7F => {
                // Backspace
                events.push(InputEvent::Key(KeyCode(KeyCode::BACKSPACE)));
                pos += 1;
            }
            0x20..=0x7E => {
                // Printable ASCII
                events.push(InputEvent::Key(KeyCode(remaining[0] as u32)));
                pos += 1;
            }
            0xC0..=0xDF if pos + 1 < buf.len() => {
                pos += 2; // 2-byte UTF-8 — consume
            }
            0xE0..=0xEF if pos + 2 < buf.len() => {
                pos += 3;
            }
            0xF0..=0xF7 if pos + 3 < buf.len() => {
                pos += 4;
            }
            _ => {
                pos += 1;
            }
        }
    }

    (events, pos)
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

fn parse_sgr_mouse(buf: &[u8]) -> Option<(MouseEvent, usize)> {
    // Format: Cb;Cx;CyM or Cb;Cx;Cym
    let mut nums = [0u32; 3];
    let mut num_idx = 0;
    let mut i = 0;

    while i < buf.len() {
        match buf[i] {
            b'0'..=b'9' => {
                if num_idx < 3 {
                    nums[num_idx] = nums[num_idx] * 10 + (buf[i] - b'0') as u32;
                }
                i += 1;
            }
            b';' => {
                num_idx += 1;
                i += 1;
            }
            b'M' | b'm' => {
                let is_release = buf[i] == b'm';
                let cb = nums[0];
                let cx = nums[1].max(1) - 1;
                let cy = nums[2].max(1) - 1;

                let evt = if is_release {
                    MouseEvent::Release { x: cx, y: cy }
                } else if cb & 64 != 0 {
                    // Wheel
                    if cb & 1 != 0 {
                        MouseEvent::WheelDown { x: cx, y: cy }
                    } else {
                        MouseEvent::WheelUp { x: cx, y: cy }
                    }
                } else if cb & 32 != 0 {
                    // Drag
                    MouseEvent::Drag {
                        button: (cb & 3) as u8,
                        x: cx,
                        y: cy,
                    }
                } else {
                    MouseEvent::Press {
                        button: (cb & 3) as u8,
                        x: cx,
                        y: cy,
                    }
                };
                return Some((evt, i + 1));
            }
            _ => return None,
        }
    }
    None
}

fn parse_csi_key(buf: &[u8]) -> Option<(KeyCode, usize)> {
    // Collect parameter bytes and find the final byte
    let mut i = 0;
    let mut params = Vec::new();
    let mut current = 0u32;
    let mut has_digit = false;

    while i < buf.len() {
        match buf[i] {
            b'0'..=b'9' => {
                current = current * 10 + (buf[i] - b'0') as u32;
                has_digit = true;
                i += 1;
            }
            b';' => {
                params.push(if has_digit { current } else { 0 });
                current = 0;
                has_digit = false;
                i += 1;
            }
            b'~' => {
                if has_digit {
                    params.push(current);
                }
                let key = match params.first().copied().unwrap_or(0) {
                    1 => KeyCode::HOME,
                    2 => KeyCode::INSERT,
                    3 => KeyCode::DELETE,
                    4 => KeyCode::END,
                    5 => KeyCode::PAGEUP,
                    6 => KeyCode::PAGEDOWN,
                    15 => KeyCode::F5,
                    17 => KeyCode::F6,
                    18 => KeyCode::F7,
                    19 => KeyCode::F8,
                    20 => KeyCode::F9,
                    21 => KeyCode::F10,
                    23 => KeyCode::F11,
                    24 => KeyCode::F12,
                    _ => return None,
                };
                let modifiers = xterm_modifiers(params.get(1).copied().unwrap_or(0));
                return Some((KeyCode(key | modifiers), i + 1));
            }
            b'A'..=b'Z' | b'a'..=b'z' => {
                if has_digit {
                    params.push(current);
                }
                let key = match buf[i] {
                    b'A' => KeyCode::UP,
                    b'B' => KeyCode::DOWN,
                    b'C' => KeyCode::RIGHT,
                    b'D' => KeyCode::LEFT,
                    b'H' => KeyCode::HOME,
                    b'F' => KeyCode::END,
                    b'P' => KeyCode::F1,
                    b'Q' => KeyCode::F2,
                    b'R' => KeyCode::F3,
                    b'S' => KeyCode::F4,
                    b'Z' => return Some((KeyCode(KeyCode::TAB | KeyCode::SHIFT), i + 1)),
                    _ => return None,
                };
                let modifiers = if params.len() >= 2 {
                    xterm_modifiers(params[1])
                } else if params.len() == 1 && params[0] > 1 {
                    xterm_modifiers(params[0])
                } else {
                    0
                };
                return Some((KeyCode(key | modifiers), i + 1));
            }
            _ => return None,
        }
    }
    None
}

fn parse_ss3_key(byte: u8) -> Option<KeyCode> {
    match byte {
        b'A' => Some(KeyCode(KeyCode::UP)),
        b'B' => Some(KeyCode(KeyCode::DOWN)),
        b'C' => Some(KeyCode(KeyCode::RIGHT)),
        b'D' => Some(KeyCode(KeyCode::LEFT)),
        b'H' => Some(KeyCode(KeyCode::HOME)),
        b'F' => Some(KeyCode(KeyCode::END)),
        b'P' => Some(KeyCode(KeyCode::F1)),
        b'Q' => Some(KeyCode(KeyCode::F2)),
        b'R' => Some(KeyCode(KeyCode::F3)),
        b'S' => Some(KeyCode(KeyCode::F4)),
        _ => None,
    }
}

/// Convert xterm modifier parameter (1-based) to our modifier flags.
fn xterm_modifiers(param: u32) -> u32 {
    if param < 2 {
        return 0;
    }
    let m = param - 1;
    let mut flags = 0;
    if m & 1 != 0 {
        flags |= KeyCode::SHIFT;
    }
    if m & 2 != 0 {
        flags |= KeyCode::META;
    }
    if m & 4 != 0 {
        flags |= KeyCode::CTRL;
    }
    flags
}

/// Parse a key name from config into a KeyCode.
pub(crate) fn parse_key_name(name: &str) -> Option<KeyCode> {
    // Handle modifiers
    let (modifiers, base_name) = if let Some(rest) = name.strip_prefix("C-") {
        (KeyCode::CTRL, rest)
    } else if let Some(rest) = name.strip_prefix("S-") {
        (KeyCode::SHIFT, rest)
    } else if let Some(rest) = name.strip_prefix("M-") {
        (KeyCode::META, rest)
    } else {
        (0, name)
    };

    let base = match base_name {
        "Up" => KeyCode::UP,
        "Down" => KeyCode::DOWN,
        "Left" => KeyCode::LEFT,
        "Right" => KeyCode::RIGHT,
        "Home" => KeyCode::HOME,
        "End" => KeyCode::END,
        "Insert" => KeyCode::INSERT,
        "Delete" => KeyCode::DELETE,
        "PageUp" | "PgUp" => KeyCode::PAGEUP,
        "PageDown" | "PgDn" => KeyCode::PAGEDOWN,
        "Enter" => KeyCode::ENTER,
        "Tab" => KeyCode::TAB,
        "Escape" | "Esc" => KeyCode::ESCAPE,
        "Space" => b' ' as u32,
        "Backspace" | "BSpace" => KeyCode::BACKSPACE,
        "\\" => b'\\' as u32,
        "-" => b'-' as u32,
        "/" => b'/' as u32,
        s if s.len() == 1 => {
            let ch = s.as_bytes()[0];
            if modifiers & KeyCode::CTRL != 0 && ch.is_ascii_alphabetic() {
                ch.to_ascii_lowercase() as u32
            } else {
                ch as u32
            }
        }
        _ => return None,
    };

    Some(KeyCode(base | modifiers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_key_name() {
        assert_eq!(parse_key_name("C-a"), Some(KeyCode(b'a' as u32 | KeyCode::CTRL)));
        assert_eq!(parse_key_name("C-Left"), Some(KeyCode(KeyCode::LEFT | KeyCode::CTRL)));
        assert_eq!(parse_key_name("S-Left"), Some(KeyCode(KeyCode::LEFT | KeyCode::SHIFT)));
        assert_eq!(parse_key_name("Enter"), Some(KeyCode(KeyCode::ENTER)));
        assert_eq!(parse_key_name("d"), Some(KeyCode(b'd' as u32)));
    }

    #[test]
    fn test_parse_csi() {
        let (events, consumed) = parse_input(b"\x1b[A");
        assert_eq!(consumed, 3);
        assert_eq!(events.len(), 1);
        match &events[0] {
            InputEvent::Key(k) => assert_eq!(k.base(), KeyCode::UP),
            _ => panic!("expected key"),
        }
    }

    #[test]
    fn test_parse_ctrl() {
        let (events, consumed) = parse_input(b"\x01");
        assert_eq!(consumed, 1);
        assert_eq!(events.len(), 1);
        match &events[0] {
            InputEvent::Key(k) => {
                assert!(k.has_ctrl());
                assert_eq!(k.base(), b'a' as u32);
            }
            _ => panic!("expected key"),
        }
    }

    #[test]
    fn test_parse_sgr_mouse_wheel() {
        let (events, consumed) = parse_input(b"\x1b[<64;10;20M");
        assert_eq!(consumed, 12);
        assert_eq!(events.len(), 1);
        match &events[0] {
            InputEvent::Mouse(MouseEvent::WheelUp { x, y }) => {
                assert_eq!(*x, 9);
                assert_eq!(*y, 19);
            }
            other => panic!("expected wheel up, got {other:?}"),
        }
    }
}
