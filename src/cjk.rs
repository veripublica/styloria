//! The multi-byte legacy encodings of the Encoding Standard (§10–§13):
//! GBK, gb18030, Big5, EUC-JP, ISO-2022-JP, Shift_JIS and EUC-KR. Built only
//! with the `legacy-cjk` feature, since their indexes are large.
//!
//! Each decoder and encoder follows the standard's algorithm step by step,
//! byte for byte, including where an ASCII byte after a bad leading byte is
//! read again rather than swallowed.

use crate::decode::{Cjk, EncodeError};
use crate::encodings_cjk::{
    BIG5, BIG5_ORDER, BIG5_PLANE_2, EUC_KR, EUC_KR_ORDER, GB18030, GB18030_ORDER, GB18030_RANGES,
    ISO_2022_JP_KATAKANA, JIS0208, JIS0208_ORDER, JIS0212, SHIFT_JIS_OVERRIDES,
};

/// What one step of a decoder gives.
enum Step {
    One(u32),
    /// Big5 has four pointers that stand for two code points.
    Two(u32, u32),
    Error,
}

use Step::{Error, One, Two};

/// Decode, and say whether the text holds everything the bytes said: no
/// byte failed, and encoding the text gives these bytes back. The second
/// fails where a code point has more than one spelling (Shift_JIS's NEC and
/// IBM rows, a redundant ISO-2022-JP escape), which a decode cannot keep.
pub(crate) fn decode(bytes: &[u8], cjk: Cjk) -> (String, bool) {
    let (text, clean) = match cjk {
        Cjk::Gbk | Cjk::Gb18030 => run(bytes, gb18030),
        Cjk::Big5 => run(bytes, big5),
        Cjk::EucJp => run(bytes, euc_jp),
        Cjk::Iso2022Jp => iso_2022_jp(bytes),
        Cjk::ShiftJis => run(bytes, shift_jis),
        Cjk::EucKr => run(bytes, euc_kr),
    };
    let mut back = Vec::with_capacity(bytes.len());
    let lossless = clean && encode(&text, cjk, &mut back).is_ok() && back == bytes;
    (text, lossless)
}

/// Run a decoder whose state lasts one sequence: `step` reads the sequence
/// at the start of its input and says how many bytes it took.
fn run(bytes: &[u8], step: fn(&[u8]) -> (Step, usize)) -> (String, bool) {
    let mut text = String::with_capacity(bytes.len());
    let mut clean = true;
    let mut i = 0;
    while i < bytes.len() {
        let (s, n) = step(&bytes[i..]);
        i += n;
        match s {
            One(c) => push(&mut text, c),
            Two(a, b) => {
                push(&mut text, a);
                push(&mut text, b);
            }
            Error => {
                clean = false;
                text.push(char::REPLACEMENT_CHARACTER);
            }
        }
    }
    (text, clean)
}

fn push(text: &mut String, c: u32) {
    // The generator checks that no index holds a surrogate.
    text.push(char::from_u32(c).unwrap_or(char::REPLACEMENT_CHARACTER));
}

/// The code point at `pointer` in `table`, `None` where it is unmapped.
fn at(table: &[u16], pointer: u32) -> Option<u32> {
    let c = *table.get(usize::try_from(pointer).ok()?)?;
    (c != 0).then_some(u32::from(c))
}

/// A leading byte whose second byte gives no code point: an ASCII second
/// byte is put back, to be read on its own.
fn bad_pair(byte: u8) -> (Step, usize) {
    (Error, if byte.is_ascii() { 1 } else { 2 })
}

fn found(c: Option<u32>, n: usize, byte: u8) -> (Step, usize) {
    match c {
        Some(c) => (One(c), n),
        None => bad_pair(byte),
    }
}

/// §10.2.1, also GBK's decoder.
fn gb18030(b: &[u8]) -> (Step, usize) {
    let lead = b[0];
    match lead {
        0x00..=0x7F => return (One(lead.into()), 1),
        0x80 => return (One(0x20AC), 1),
        0x81..=0xFE => {}
        0xFF => return (Error, 1),
    }
    let Some(&second) = b.get(1) else {
        return (Error, 1);
    };
    if (0x30..=0x39).contains(&second) {
        // Four bytes; when the third or fourth does not fit, everything
        // after the first is read again.
        let Some(&third) = b.get(2) else {
            return (Error, 2);
        };
        if !(0x81..=0xFE).contains(&third) {
            return (Error, 1);
        }
        let Some(&fourth) = b.get(3) else {
            return (Error, 3);
        };
        if !(0x30..=0x39).contains(&fourth) {
            return (Error, 1);
        }
        let pointer = (u32::from(lead) - 0x81) * (10 * 126 * 10)
            + (u32::from(second) - 0x30) * (10 * 126)
            + (u32::from(third) - 0x81) * 10
            + u32::from(fourth)
            - 0x30;
        return match ranges_code_point(pointer) {
            Some(c) => (One(c), 4),
            None => (Error, 4),
        };
    }
    let offset = if second < 0x7F { 0x40 } else { 0x41 };
    let pointer = matches!(second, 0x40..=0x7E | 0x80..=0xFE)
        .then(|| (u32::from(lead) - 0x81) * 190 + u32::from(second) - offset);
    found(pointer.and_then(|p| at(&GB18030, p)), 2, second)
}

/// The index gb18030 ranges code point (§5).
fn ranges_code_point(pointer: u32) -> Option<u32> {
    if (pointer > 39419 && pointer < 189000) || pointer > 1237575 {
        return None;
    }
    if pointer == 7457 {
        return Some(0xE7C7);
    }
    let i = GB18030_RANGES.partition_point(|&(p, _)| p <= pointer) - 1;
    let (offset, code_point) = GB18030_RANGES[i];
    Some(code_point + pointer - offset)
}

/// The index gb18030 ranges pointer (§5).
fn ranges_pointer(code_point: u32) -> u32 {
    if code_point == 0xE7C7 {
        return 7457;
    }
    let i = GB18030_RANGES.partition_point(|&(_, c)| c <= code_point) - 1;
    let (pointer, offset) = GB18030_RANGES[i];
    pointer + code_point - offset
}

/// §11.1.1.
fn big5(b: &[u8]) -> (Step, usize) {
    let lead = b[0];
    match lead {
        0x00..=0x7F => return (One(lead.into()), 1),
        0x81..=0xFE => {}
        _ => return (Error, 1),
    }
    let Some(&byte) = b.get(1) else {
        return (Error, 1);
    };
    let offset = if byte < 0x7F { 0x40 } else { 0x62 };
    let pointer = matches!(byte, 0x40..=0x7E | 0xA1..=0xFE)
        .then(|| (u32::from(lead) - 0x81) * 157 + u32::from(byte) - offset);
    match pointer {
        Some(1133) => (Two(0x00CA, 0x0304), 2),
        Some(1135) => (Two(0x00CA, 0x030C), 2),
        Some(1164) => (Two(0x00EA, 0x0304), 2),
        Some(1166) => (Two(0x00EA, 0x030C), 2),
        _ => found(pointer.and_then(big5_code_point), 2, byte),
    }
}

fn big5_code_point(pointer: u32) -> Option<u32> {
    let low = at(&BIG5, pointer)?;
    let word = BIG5_PLANE_2[usize::try_from(pointer / 32).ok()?];
    Some(if word & (1 << (pointer % 32)) != 0 {
        0x20000 | low
    } else {
        low
    })
}

/// §12.1.1.
fn euc_jp(b: &[u8]) -> (Step, usize) {
    let lead = b[0];
    match lead {
        0x00..=0x7F => return (One(lead.into()), 1),
        0x8E | 0x8F | 0xA1..=0xFE => {}
        _ => return (Error, 1),
    }
    let Some(&byte) = b.get(1) else {
        return (Error, 1);
    };
    if lead == 0x8E && (0xA1..=0xDF).contains(&byte) {
        return (One(0xFF61 - 0xA1 + u32::from(byte)), 2);
    }
    if lead == 0x8F && (0xA1..=0xFE).contains(&byte) {
        // JIS X 0212: 0x8F, then a pair read in its index.
        let Some(&trail) = b.get(2) else {
            return (Error, 2);
        };
        let c = (0xA1..=0xFE)
            .contains(&trail)
            .then(|| (u32::from(byte) - 0xA1) * 94 + u32::from(trail) - 0xA1)
            .and_then(|p| at(&JIS0212, p));
        return match c {
            Some(c) => (One(c), 3),
            None => (Error, if trail.is_ascii() { 2 } else { 3 }),
        };
    }
    let c = ((0xA1..=0xFE).contains(&lead) && (0xA1..=0xFE).contains(&byte))
        .then(|| (u32::from(lead) - 0xA1) * 94 + u32::from(byte) - 0xA1)
        .and_then(|p| at(&JIS0208, p));
    found(c, 2, byte)
}

/// §12.2.1. The one decoder whose state outlives a sequence: escapes switch
/// between ASCII, Roman, katakana and JIS X 0208.
fn iso_2022_jp(bytes: &[u8]) -> (String, bool) {
    #[derive(Clone, Copy)]
    enum State {
        Ascii,
        Roman,
        Katakana,
        Lead,
        Trail,
        EscapeStart,
        Escape,
    }
    use State::{Ascii, Escape, EscapeStart, Katakana, Lead, Roman, Trail};
    enum Out {
        Continue,
        Finished,
        Code(u32),
        Error,
    }
    let (mut state, mut output_state) = (Ascii, Ascii);
    let (mut leading, mut output) = (0u8, false);
    let mut text = String::with_capacity(bytes.len());
    let mut clean = true;
    let mut i = 0;
    loop {
        // `None` is the end of the queue. Putting bytes back moves `i`.
        let at_byte = i;
        let byte = bytes.get(at_byte).copied();
        i = at_byte + 1;
        let out = match (state, byte) {
            (Ascii | Roman | Katakana | Lead, Some(0x1B)) => {
                state = EscapeStart;
                Out::Continue
            }
            (Ascii | Roman | Katakana | Lead, None) => Out::Finished,
            (Roman, Some(0x5C)) => {
                output = false;
                Out::Code(0xA5)
            }
            (Roman, Some(0x7E)) => {
                output = false;
                Out::Code(0x203E)
            }
            (Ascii | Roman, Some(b)) if b.is_ascii() && b != 0x0E && b != 0x0F => {
                output = false;
                Out::Code(b.into())
            }
            (Katakana, Some(b @ 0x21..=0x5F)) => {
                output = false;
                Out::Code(0xFF61 - 0x21 + u32::from(b))
            }
            (Lead, Some(b @ 0x21..=0x7E)) => {
                output = false;
                leading = b;
                state = Trail;
                Out::Continue
            }
            (Ascii | Roman | Katakana | Lead, Some(_)) => {
                output = false;
                Out::Error
            }
            (Trail, Some(0x1B)) => {
                state = EscapeStart;
                Out::Error
            }
            (Trail, Some(b @ 0x21..=0x7E)) => {
                state = Lead;
                let pointer = (u32::from(leading) - 0x21) * 94 + u32::from(b) - 0x21;
                at(&JIS0208, pointer).map_or(Out::Error, Out::Code)
            }
            (Trail, _) => {
                state = Lead;
                Out::Error
            }
            (EscapeStart, Some(b @ (0x24 | 0x28))) => {
                leading = b;
                state = Escape;
                Out::Continue
            }
            (EscapeStart, b) => {
                if b.is_some() {
                    i = at_byte;
                }
                output = false;
                state = output_state;
                Out::Error
            }
            (Escape, b) => {
                let lead = std::mem::take(&mut leading);
                let next = match (lead, b) {
                    (0x28, Some(0x42)) => Some(Ascii),
                    (0x28, Some(0x4A)) => Some(Roman),
                    (0x28, Some(0x49)) => Some(Katakana),
                    (0x24, Some(0x40 | 0x42)) => Some(Lead),
                    _ => None,
                };
                if let Some(next) = next {
                    state = next;
                    output_state = next;
                    // Two escapes in a row, with nothing between, are an error.
                    if std::mem::replace(&mut output, true) {
                        Out::Error
                    } else {
                        Out::Continue
                    }
                } else {
                    // The byte after the escape is read again, and this one
                    // too unless the queue ended.
                    i = at_byte - 1;
                    output = false;
                    state = output_state;
                    Out::Error
                }
            }
        };
        match out {
            Out::Continue => {}
            Out::Finished => break,
            Out::Code(c) => push(&mut text, c),
            Out::Error => {
                clean = false;
                text.push(char::REPLACEMENT_CHARACTER);
            }
        }
    }
    (text, clean)
}

/// §12.3.1.
fn shift_jis(b: &[u8]) -> (Step, usize) {
    let lead = b[0];
    match lead {
        0x00..=0x80 => return (One(lead.into()), 1),
        0xA1..=0xDF => return (One(0xFF61 - 0xA1 + u32::from(lead)), 1),
        0x81..=0x9F | 0xE0..=0xFC => {}
        _ => return (Error, 1),
    }
    let Some(&byte) = b.get(1) else {
        return (Error, 1);
    };
    let offset = if byte < 0x7F { 0x40 } else { 0x41 };
    let lead_offset = if lead < 0xA0 { 0x81 } else { 0xC1 };
    let pointer = matches!(byte, 0x40..=0x7E | 0x80..=0xFC)
        .then(|| (u32::from(lead) - lead_offset) * 188 + u32::from(byte) - offset);
    if let Some(p @ 8836..=10715) = pointer {
        // Windows' user-defined characters, in the Private Use Area.
        return (One(0xE000 - 8836 + p), 2);
    }
    found(pointer.and_then(|p| at(&JIS0208, p)), 2, byte)
}

/// §13.1.1.
fn euc_kr(b: &[u8]) -> (Step, usize) {
    let lead = b[0];
    match lead {
        0x00..=0x7F => return (One(lead.into()), 1),
        0x81..=0xFE => {}
        _ => return (Error, 1),
    }
    let Some(&byte) = b.get(1) else {
        return (Error, 1);
    };
    let pointer = (0x41..=0xFE)
        .contains(&byte)
        .then(|| (u32::from(lead) - 0x81) * 190 + u32::from(byte) - 0x41);
    found(pointer.and_then(|p| at(&EUC_KR, p)), 2, byte)
}

/// Encode `text` onto `out`.
pub(crate) fn encode(text: &str, cjk: Cjk, out: &mut Vec<u8>) -> Result<(), EncodeError> {
    let step: fn(u32, &mut Vec<u8>) -> Option<()> = match cjk {
        Cjk::Iso2022Jp => return iso_2022_jp_out(text, out),
        Cjk::Gbk => gbk_out,
        Cjk::Gb18030 => gb18030_out,
        Cjk::Big5 => big5_out,
        Cjk::EucJp => euc_jp_out,
        Cjk::ShiftJis => shift_jis_out,
        Cjk::EucKr => euc_kr_out,
    };
    for (offset, ch) in text.char_indices() {
        let c = u32::from(ch);
        let ok = if ch.is_ascii() {
            bytes(out, &[c])
        } else {
            step(c, out)
        };
        ok.ok_or(EncodeError::Unmappable { offset, ch })?;
    }
    Ok(())
}

/// Push each value as a byte; `None` if one is not a byte.
fn bytes(out: &mut Vec<u8>, values: &[u32]) -> Option<()> {
    for &v in values {
        out.push(u8::try_from(v).ok()?);
    }
    Some(())
}

/// The pointer `code_point` encodes as: `order` lists pointers sorted by
/// the code point `table` gives them.
fn pointer_in(order: &[u16], code_point: u32, table: impl Fn(u32) -> Option<u32>) -> Option<u32> {
    order
        .binary_search_by_key(&Some(code_point), |&p| table(u32::from(p)))
        .ok()
        .map(|i| u32::from(order[i]))
}

/// The two-byte table of gb18030's encoder (§10.2.2), kept from
/// GB18030-2005 for the Private Use code points the 2022 revision moved.
const GB18030_PRIVATE_USE: [(u32, [u32; 2]); 18] = [
    (0xE78D, [0xA6, 0xD9]),
    (0xE78E, [0xA6, 0xDA]),
    (0xE78F, [0xA6, 0xDB]),
    (0xE790, [0xA6, 0xDC]),
    (0xE791, [0xA6, 0xDD]),
    (0xE792, [0xA6, 0xDE]),
    (0xE793, [0xA6, 0xDF]),
    (0xE794, [0xA6, 0xEC]),
    (0xE795, [0xA6, 0xED]),
    (0xE796, [0xA6, 0xF3]),
    (0xE81E, [0xFE, 0x59]),
    (0xE826, [0xFE, 0x61]),
    (0xE82B, [0xFE, 0x66]),
    (0xE82C, [0xFE, 0x67]),
    (0xE832, [0xFE, 0x6D]),
    (0xE843, [0xFE, 0x7E]),
    (0xE854, [0xFE, 0x90]),
    (0xE864, [0xFE, 0xA0]),
];

fn gbk_out(c: u32, out: &mut Vec<u8>) -> Option<()> {
    gb18030_encode(c, out, true)
}

fn gb18030_out(c: u32, out: &mut Vec<u8>) -> Option<()> {
    gb18030_encode(c, out, false)
}

/// §10.2.2, for a code point that is not ASCII.
fn gb18030_encode(c: u32, out: &mut Vec<u8>, is_gbk: bool) -> Option<()> {
    // 0xA3 0xA0 decodes to U+3000, so U+E5E5 has no bytes.
    if c == 0xE5E5 {
        return None;
    }
    if is_gbk && c == 0x20AC {
        return bytes(out, &[0x80]);
    }
    if let Ok(i) = GB18030_PRIVATE_USE.binary_search_by_key(&c, |&(c, _)| c) {
        return bytes(out, &GB18030_PRIVATE_USE[i].1);
    }
    if let Some(p) = pointer_in(&GB18030_ORDER, c, |p| at(&GB18030, p)) {
        let trailing = p % 190;
        let offset = if trailing < 0x3F { 0x40 } else { 0x41 };
        return bytes(out, &[p / 190 + 0x81, trailing + offset]);
    }
    if is_gbk {
        return None;
    }
    let p = ranges_pointer(c);
    bytes(
        out,
        &[
            p / (10 * 126 * 10) + 0x81,
            p % (10 * 126 * 10) / (10 * 126) + 0x30,
            p % (10 * 126) / 10 + 0x81,
            p % 10 + 0x30,
        ],
    )
}

/// §11.1.2.
fn big5_out(c: u32, out: &mut Vec<u8>) -> Option<()> {
    let p = pointer_in(&BIG5_ORDER, c, big5_code_point)?;
    let trailing = p % 157;
    let offset = if trailing < 0x3F { 0x40 } else { 0x62 };
    bytes(out, &[p / 157 + 0x81, trailing + offset])
}

/// §12.1.2.
fn euc_jp_out(c: u32, out: &mut Vec<u8>) -> Option<()> {
    match c {
        0xA5 => return bytes(out, &[0x5C]),
        0x203E => return bytes(out, &[0x7E]),
        0xFF61..=0xFF9F => return bytes(out, &[0x8E, c - 0xFF61 + 0xA1]),
        _ => {}
    }
    let p = jis0208_pointer(if c == 0x2212 { 0xFF0D } else { c })?;
    bytes(out, &[p / 94 + 0xA1, p % 94 + 0xA1])
}

fn jis0208_pointer(c: u32) -> Option<u32> {
    pointer_in(&JIS0208_ORDER, c, |p| at(&JIS0208, p))
}

/// §12.3.2.
fn shift_jis_out(c: u32, out: &mut Vec<u8>) -> Option<()> {
    match c {
        0x80 => return bytes(out, &[0x80]),
        0xA5 => return bytes(out, &[0x5C]),
        0x203E => return bytes(out, &[0x7E]),
        0xFF61..=0xFF9F => return bytes(out, &[c - 0xFF61 + 0xA1]),
        _ => {}
    }
    let c = if c == 0x2212 { 0xFF0D } else { c };
    // The index Shift_JIS pointer: jis0208 without pointers 8272..=8835.
    let p = match SHIFT_JIS_OVERRIDES.binary_search_by_key(&c, |&(c, _)| u32::from(c)) {
        Ok(i) => u32::from(SHIFT_JIS_OVERRIDES[i].1),
        Err(_) => jis0208_pointer(c)?,
    };
    let leading = p / 188;
    let lead_offset = if leading < 0x1F { 0x81 } else { 0xC1 };
    let trailing = p % 188;
    let offset = if trailing < 0x3F { 0x40 } else { 0x41 };
    bytes(out, &[leading + lead_offset, trailing + offset])
}

/// §13.1.2.
fn euc_kr_out(c: u32, out: &mut Vec<u8>) -> Option<()> {
    let p = pointer_in(&EUC_KR_ORDER, c, |p| at(&EUC_KR, p))?;
    bytes(out, &[p / 190 + 0x81, p % 190 + 0x41])
}

/// §12.2.2. Ends in ASCII, as the standard's encoder does.
fn iso_2022_jp_out(text: &str, out: &mut Vec<u8>) -> Result<(), EncodeError> {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Ascii,
        Roman,
        Jis0208,
    }
    const TO_ASCII: [u8; 3] = [0x1B, 0x28, 0x42];
    const TO_ROMAN: [u8; 3] = [0x1B, 0x28, 0x4A];
    const TO_JIS0208: [u8; 3] = [0x1B, 0x24, 0x42];
    let mut state = State::Ascii;
    for (offset, ch) in text.char_indices() {
        let error = || EncodeError::Unmappable { offset, ch };
        let mut c = u32::from(ch);
        // A code point the standard puts back is read again after an escape.
        loop {
            let ascii = ch.is_ascii();
            if state != State::Jis0208 && matches!(c, 0x0E | 0x0F | 0x1B) {
                return Err(error());
            }
            if state == State::Ascii && ascii {
                bytes(out, &[c]);
                break;
            }
            if state == State::Roman
                && ((ascii && c != 0x5C && c != 0x7E) || c == 0xA5 || c == 0x203E)
            {
                let b = match c {
                    0xA5 => 0x5C,
                    0x203E => 0x7E,
                    _ => c,
                };
                bytes(out, &[b]);
                break;
            }
            if ascii {
                out.extend_from_slice(&TO_ASCII);
                state = State::Ascii;
                continue;
            }
            if c == 0xA5 || c == 0x203E {
                out.extend_from_slice(&TO_ROMAN);
                state = State::Roman;
                continue;
            }
            if c == 0x2212 {
                c = 0xFF0D;
            }
            if let Some(k) = c.checked_sub(0xFF61).filter(|&k| k <= 0xFF9F - 0xFF61) {
                c = at(&ISO_2022_JP_KATAKANA, k).ok_or_else(error)?;
            }
            let Some(p) = jis0208_pointer(c) else {
                if state == State::Jis0208 {
                    out.extend_from_slice(&TO_ASCII);
                    state = State::Ascii;
                    continue;
                }
                return Err(error());
            };
            if state != State::Jis0208 {
                out.extend_from_slice(&TO_JIS0208);
                state = State::Jis0208;
                continue;
            }
            bytes(out, &[p / 94 + 0x21, p % 94 + 0x21]);
            break;
        }
    }
    if state != State::Ascii {
        out.extend_from_slice(&TO_ASCII);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::decode::{EncodeError, Encoding, decode_stylesheet, encode_stylesheet};

    fn decode(label: &str, bytes: &[u8]) -> (String, bool) {
        let d = decode_stylesheet(bytes, Some(label), None).unwrap();
        (d.text.into_owned(), d.lossless)
    }

    fn encode(label: &str, text: &str) -> Result<Vec<u8>, EncodeError> {
        encode_stylesheet(text, Encoding::for_label(label).unwrap(), false)
    }

    #[test]
    fn each_encoding_decodes_and_encodes_back() {
        for (label, bytes, text) in [
            ("shift_jis", &b"\x93\xFA\x96\x7B \xB1"[..], "日本 ｱ"),
            ("euc-jp", b"\xC6\xFC\xCB\xDC \x8E\xB1", "日本 ｱ"),
            ("iso-2022-jp", b"\x1B$BF|K\\\x1B(B", "日本"),
            ("gbk", b"\xD6\xD0\xCE\xC4\x80", "中文€"),
            (
                "gb18030",
                b"\xD6\xD0\x81\x30\x84\x36\x94\x39\xDA\x33",
                "中¥💩",
            ),
            ("big5", b"\xA4\xA4\xA4\xE5", "中文"),
            ("euc-kr", b"\xC7\xD1\xB1\xB9", "한국"),
        ] {
            assert_eq!(decode(label, bytes), (text.to_string(), true), "{label}");
            assert_eq!(encode(label, text).unwrap(), bytes, "{label}");
        }
        // Through `@charset`, as a stylesheet names it.
        let d = decode_stylesheet(
            b"@charset \"Shift_JIS\"; p::before { content: \"\x93\xFA\" }",
            None,
            None,
        )
        .unwrap();
        assert_eq!(d.encoding.name(), "Shift_JIS");
        assert!(d.text.ends_with("content: \"日\" }"));
    }

    #[test]
    fn a_bad_sequence_gives_back_its_ascii_byte() {
        // The byte after a bad leading byte is read again when it is ASCII.
        assert_eq!(decode("shift_jis", b"\x81 a").0, "\u{FFFD} a");
        assert_eq!(decode("euc-kr", b"\x81\x20").0, "\u{FFFD} ");
        assert_eq!(decode("big5", b"\xA4\x7F\xA4").0, "\u{FFFD}\u{7F}\u{FFFD}");
        // gb18030 reads everything after the first byte again.
        assert_eq!(decode("gb18030", b"\x81\x30\x20").0, "\u{FFFD}0 ");
        assert_eq!(
            decode("gb18030", b"\x81\x30\x81\x20").0,
            "\u{FFFD}0\u{FFFD} "
        );
        // EUC-JP's three-byte form.
        assert_eq!(decode("euc-jp", b"\x8F\xA1a").0, "\u{FFFD}a");
        // Cut short at the end.
        assert_eq!(decode("euc-kr", b"a\xC7"), ("a\u{FFFD}".into(), false));
        assert_eq!(
            decode("gb18030", b"\x81\x30\x81"),
            ("\u{FFFD}".into(), false)
        );
        // An ISO-2022-JP escape that is not one.
        assert_eq!(decode("iso-2022-jp", b"\x1B(Za").0, "\u{FFFD}(Za");
    }

    #[test]
    fn a_second_spelling_is_not_lossless() {
        // NEC-selected IBM extensions: the same character as IBM's own row,
        // which the encoder writes.
        assert_eq!(decode("shift_jis", b"\xED\x40"), ("纊".into(), false));
        assert_eq!(encode("shift_jis", "纊").unwrap(), b"\xFA\x5C");
        assert!(decode("shift_jis", b"\xFA\x5C").1);
        // gb18030 writes € as A2 E3; GBK as 0x80.
        assert_eq!(decode("gb18030", b"\x80"), ("€".into(), false));
        assert_eq!(decode("gbk", b"\x80"), ("€".into(), true));
        // A redundant escape, or none back to ASCII at the end.
        assert_eq!(decode("iso-2022-jp", b"\x1B(Ba"), ("a".into(), false));
        assert_eq!(decode("iso-2022-jp", b"\x1B$BF|"), ("日".into(), false));
        // Big5's two-code-point pointers decode but have no bytes back.
        assert_eq!(decode("big5", b"\x88\x62"), ("\u{CA}\u{304}".into(), false));
    }

    #[test]
    fn encoding_reports_what_has_no_bytes() {
        let unmappable = |offset, ch| Err(EncodeError::Unmappable { offset, ch });
        assert_eq!(encode("gbk", "a😀"), unmappable(1, '😀'));
        assert_eq!(encode("gb18030", "\u{E5E5}"), unmappable(0, '\u{E5E5}'));
        assert_eq!(encode("euc-kr", "日本"), Ok(b"\xEC\xED\xDC\xE2".to_vec()));
        assert_eq!(encode("euc-kr", "a😀"), unmappable(1, '😀'));
        assert_eq!(encode("shift_jis", "ア😀"), unmappable(3, '😀'));
        // ISO-2022-JP refuses its own escape byte, and ends in ASCII.
        assert_eq!(encode("iso-2022-jp", "a\u{1B}"), unmappable(1, '\u{1B}'));
        assert_eq!(
            encode("iso-2022-jp", "a¥日").unwrap(),
            b"a\x1B(J\x5C\x1B$BF|\x1B(B"
        );
        // Halfwidth katakana are written fullwidth.
        assert_eq!(
            encode("iso-2022-jp", "ｱ").unwrap(),
            encode("iso-2022-jp", "ア").unwrap()
        );
    }
}
