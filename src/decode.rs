//! Stylesheet bytes to text, and back.
//!
//! [`decode_stylesheet`] follows CSS Syntax 3 §3.2 and the Encoding Standard,
//! in their order: a byte order mark decides; else the protocol's label
//! (`Content-Type: text/css; charset=…`); else a byte-exact `@charset "…";`
//! at the very start ([`charset_rule`]), where `utf-16be` and `utf-16le` mean
//! UTF-8; else the referring document's encoding; else UTF-8. Every consumer
//! that decodes this way parses the same text from the same bytes, so spans
//! agree between them.
//!
//! **Supported:** UTF-8, UTF-16BE, UTF-16LE, every single-byte encoding of
//! the Encoding Standard (windows-1252, ISO-8859-*, KOI8-*, …), from tables
//! generated from its index files, and `replacement` (which only decodes).
//! The multi-byte legacy encodings (Shift_JIS, GBK, gb18030, Big5, EUC-JP,
//! ISO-2022-JP, EUC-KR) need the `legacy-cjk` feature, off by default, since
//! their indexes add about 300 KiB to a WebAssembly build. Without it a stylesheet in one
//! is an [`Unsupported`](DecodeError::Unsupported) error naming the label,
//! never a guess.
//!
//! **Writing back.** [`encode_stylesheet`] turns text into bytes in the same
//! encoding. When the decode was [`lossless`](Decoded::lossless), encoding the
//! unchanged text with [`had_bom`](Decoded::had_bom) gives back the original
//! bytes exactly, so a tool that edits one rule leaves every other byte as it
//! was. When it was not, some bytes became U+FFFD and that cannot be undone:
//! a careful tool declines to write such a file.
//!
//! ```
//! use styloria::{decode_stylesheet, encode_stylesheet};
//!
//! let bytes = b"@charset \"windows-1252\";\np::before { content: \"\x80\" }";
//! let decoded = decode_stylesheet(bytes, None, None).unwrap();
//! assert_eq!(decoded.encoding.name(), "windows-1252");
//! assert!(decoded.text.ends_with("content: \"€\" }"));
//! assert!(decoded.lossless);
//!
//! let back = encode_stylesheet(&decoded.text, decoded.encoding, decoded.had_bom).unwrap();
//! assert_eq!(back, bytes);
//! ```

use std::borrow::Cow;

use crate::encodings::{ENCODINGS, LABELS};

/// An encoding of the Encoding Standard. Every one can be named, looked up by
/// label and compared; only [supported](Self::is_supported) ones decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Encoding(u8);

/// How an encoding decodes; the generated table holds one per encoding.
#[derive(Debug)]
pub(crate) enum Kind {
    Utf8,
    Utf16Be,
    Utf16Le,
    /// Bytes 0x80..=0xFF to code points, 0 where unmapped.
    SingleByte(&'static [u16; 128]),
    /// `x-user-defined`: 0x80..=0xFF to U+F780..=U+F7FF.
    UserDefined,
    /// A multi-byte encoding, supported with the `legacy-cjk` feature
    /// (without it, only its name is used).
    #[cfg_attr(not(feature = "legacy-cjk"), allow(dead_code))]
    Cjk(Cjk),
    /// Any input but none decodes to one U+FFFD; there is no encoder.
    Replacement,
}

/// The multi-byte legacy encodings. GBK decodes as gb18030 does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cjk {
    Gbk,
    Gb18030,
    Big5,
    EucJp,
    Iso2022Jp,
    ShiftJis,
    EucKr,
}

impl Encoding {
    /// UTF-8, the encoding a stylesheet falls back to.
    pub const UTF_8: Encoding = Encoding(0);

    /// The encoding a label names (the Encoding Standard's "get an
    /// encoding"): ASCII whitespace around it is ignored, case is not
    /// significant. `None` for a label the standard does not know.
    pub fn for_label(label: &str) -> Option<Encoding> {
        let label = label.trim_matches(|c: char| c.is_ascii_whitespace());
        if label.len() > 32 {
            return None;
        }
        let lower = label.to_ascii_lowercase();
        LABELS
            .binary_search_by(|(l, _)| (*l).cmp(lower.as_str()))
            .ok()
            .map(|i| Encoding(LABELS[i].1))
    }

    /// The canonical name: `UTF-8`, `windows-1252`, `Shift_JIS`.
    pub fn name(self) -> &'static str {
        ENCODINGS[usize::from(self.0)].0
    }

    /// Whether this crate decodes it, and encodes it unless it is
    /// `replacement`, which the Encoding Standard gives no encoder. The
    /// multi-byte legacy encodings are supported with the `legacy-cjk`
    /// feature.
    pub fn is_supported(self) -> bool {
        cfg!(feature = "legacy-cjk") || !matches!(self.kind(), Kind::Cjk(_))
    }

    fn kind(self) -> &'static Kind {
        &ENCODINGS[usize::from(self.0)].1
    }

    fn get(name: &str) -> Encoding {
        let i = ENCODINGS.iter().position(|(n, _)| *n == name);
        Encoding(i.and_then(|i| u8::try_from(i).ok()).unwrap_or(0))
    }
}

/// A decoded stylesheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded<'b> {
    /// The text, without the byte order mark. Spans from parsing it are
    /// offsets into this text. Borrowed from the input when it was UTF-8
    /// and valid.
    pub text: Cow<'b, str>,
    /// The encoding used.
    pub encoding: Encoding,
    /// Whether the bytes began with a byte order mark (UTF-8, UTF-16BE or
    /// UTF-16LE), which then decided the encoding.
    pub had_bom: bool,
    /// Whether the text keeps everything the bytes said, so that encoding
    /// it gives them back: every byte decoded (no U+FFFD in
    /// [`text`](Self::text) stands for malformed input; one written in the
    /// file itself does not count), and none was one of two spellings of a
    /// character. Only the multi-byte encodings have such spellings:
    /// Shift_JIS's NEC and IBM rows, a redundant ISO-2022-JP escape.
    pub lossless: bool,
}

/// Where an encoding label came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LabelSource {
    /// The `protocol` argument: an HTTP `Content-Type` charset or the like.
    Protocol,
    /// The stylesheet's own `@charset "…";`.
    Charset,
    /// The `environment` argument: the referring document's encoding.
    Environment,
}

/// Why [`decode_stylesheet`] could not decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The label names an encoding this crate does not decode: a multi-byte
    /// legacy encoding, without the `legacy-cjk` feature.
    Unsupported {
        /// The label as written, before trimming or lowercasing.
        label: String,
        encoding: Encoding,
        source: LabelSource,
    },
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Unsupported {
                label, encoding, ..
            } => write!(
                f,
                "unsupported encoding {} (label {label:?})",
                encoding.name()
            ),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Why [`encode_stylesheet`] could not encode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// The encoding is not one this crate encodes: `replacement`, or a
    /// multi-byte legacy encoding without the `legacy-cjk` feature.
    Unsupported(Encoding),
    /// A character the encoding has no byte for, at this byte offset of the
    /// text.
    Unmappable { offset: usize, ch: char },
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodeError::Unsupported(e) => write!(f, "unsupported encoding {}", e.name()),
            EncodeError::Unmappable { offset, ch } => {
                write!(f, "{ch:?} at byte {offset} has no byte in this encoding")
            }
        }
    }
}

impl std::error::Error for EncodeError {}

/// The label of a byte-exact encoding declaration (CSS Syntax 3 §3.2), as
/// written: the first 1024 bytes begin with `@charset "`, then the label in
/// ASCII other than `"`, then `";`.
///
/// Nothing looser counts: single quotes, two spaces, a comment or a byte
/// order mark before it, a non-ASCII byte in the label. Those still parse as
/// an at-rule named `charset`, which declares no encoding. The label is not
/// checked against the known ones; [`Encoding::for_label`] does that. This
/// reads the bytes alone, so it answers even when decoding fails.
pub fn charset_rule(bytes: &[u8]) -> Option<&str> {
    let head = &bytes[..bytes.len().min(1024)];
    let rest = head.strip_prefix(b"@charset \"")?;
    let end = rest.iter().position(|&b| b == b'"' || !b.is_ascii())?;
    if rest[end] != b'"' || rest.get(end + 1) != Some(&b';') {
        return None;
    }
    std::str::from_utf8(&rest[..end]).ok()
}

/// Decode a stylesheet's bytes. `protocol` is the label the transport gave,
/// `environment` the referring document's encoding (as a label); pass `None`
/// for either when there is none. A label the Encoding Standard does not know
/// is skipped, as the spec says.
pub fn decode_stylesheet<'b>(
    bytes: &'b [u8],
    protocol: Option<&str>,
    environment: Option<&str>,
) -> Result<Decoded<'b>, DecodeError> {
    let bom = [
        (&[0xEF, 0xBB, 0xBF][..], "UTF-8"),
        (&[0xFE, 0xFF][..], "UTF-16BE"),
        (&[0xFF, 0xFE][..], "UTF-16LE"),
    ]
    .into_iter()
    .find(|(b, _)| bytes.starts_with(b));
    if let Some((mark, name)) = bom {
        return Ok(decode_with(&bytes[mark.len()..], Encoding::get(name), true));
    }
    let Some((encoding, label, source)) = fallback(bytes, protocol, environment) else {
        return Ok(decode_with(bytes, Encoding::UTF_8, false));
    };
    if !encoding.is_supported() {
        return Err(DecodeError::Unsupported {
            label: label.to_string(),
            encoding,
            source,
        });
    }
    Ok(decode_with(bytes, encoding, false))
}

/// CSS Syntax's fallback encoding, with the label that chose it; `None`
/// means the default, UTF-8.
fn fallback<'l>(
    bytes: &'l [u8],
    protocol: Option<&'l str>,
    environment: Option<&'l str>,
) -> Option<(Encoding, &'l str, LabelSource)> {
    if let Some(l) = protocol
        && let Some(e) = Encoding::for_label(l)
    {
        return Some((e, l, LabelSource::Protocol));
    }
    if let Some(l) = charset_rule(bytes)
        && let Some(e) = Encoding::for_label(l)
    {
        let e = if matches!(e.kind(), Kind::Utf16Be | Kind::Utf16Le) {
            Encoding::UTF_8
        } else {
            e
        };
        return Some((e, l, LabelSource::Charset));
    }
    if let Some(l) = environment
        && let Some(e) = Encoding::for_label(l)
    {
        return Some((e, l, LabelSource::Environment));
    }
    None
}

/// Decode with a supported encoding, the byte order mark already removed.
fn decode_with(bytes: &[u8], encoding: Encoding, had_bom: bool) -> Decoded<'_> {
    let (text, lossless) = match encoding.kind() {
        Kind::Utf8 => match std::str::from_utf8(bytes) {
            Ok(s) => (Cow::Borrowed(s), true),
            Err(_) => (String::from_utf8_lossy(bytes), false),
        },
        Kind::Utf16Be => utf16(bytes, u16::from_be_bytes),
        Kind::Utf16Le => utf16(bytes, u16::from_le_bytes),
        Kind::SingleByte(table) => {
            let mut lossless = true;
            let text = single_byte(bytes, |b| {
                let c =
                    char::from_u32(u32::from(table[usize::from(b - 0x80)])).filter(|&c| c != '\0');
                lossless &= c.is_some();
                c.unwrap_or(char::REPLACEMENT_CHARACTER)
            });
            (text, lossless)
        }
        Kind::UserDefined => (
            single_byte(bytes, |b| {
                char::from_u32(0xF780 + u32::from(b - 0x80)).unwrap_or_default()
            }),
            true,
        ),
        #[cfg(feature = "legacy-cjk")]
        Kind::Cjk(cjk) => {
            let (text, lossless) = crate::cjk::decode(bytes, *cjk);
            (Cow::Owned(text), lossless)
        }
        // `decode_stylesheet` turns these away first.
        #[cfg(not(feature = "legacy-cjk"))]
        Kind::Cjk(_) => (String::from_utf8_lossy(bytes), false),
        Kind::Replacement if bytes.is_empty() => (Cow::Borrowed(""), true),
        Kind::Replacement => (Cow::Borrowed("\u{FFFD}"), false),
    };
    Decoded {
        text,
        encoding,
        had_bom,
        lossless,
    }
}

fn utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> (Cow<'static, str>, bool) {
    let (pairs, odd) = bytes.as_chunks::<2>();
    let mut lossless = odd.is_empty();
    let mut text: String = char::decode_utf16(pairs.iter().map(|p| unit(*p)))
        .map(|r| {
            lossless &= r.is_ok();
            r.unwrap_or(char::REPLACEMENT_CHARACTER)
        })
        .collect();
    if !odd.is_empty() {
        text.push(char::REPLACEMENT_CHARACTER);
    }
    (Cow::Owned(text), lossless)
}

fn single_byte(bytes: &[u8], mut high: impl FnMut(u8) -> char) -> Cow<'static, str> {
    Cow::Owned(
        bytes
            .iter()
            .map(|&b| if b.is_ascii() { char::from(b) } else { high(b) })
            .collect(),
    )
}

/// Encode text in `encoding`, with its byte order mark when `bom` is set
/// (only UTF-8 and UTF-16 have one; the others ignore it). The inverse of
/// [`decode_stylesheet`] for a lossless decode.
pub fn encode_stylesheet(
    text: &str,
    encoding: Encoding,
    bom: bool,
) -> Result<Vec<u8>, EncodeError> {
    let mut out = Vec::with_capacity(text.len() + 3);
    match encoding.kind() {
        Kind::Utf8 => {
            if bom {
                out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
            }
            out.extend_from_slice(text.as_bytes());
        }
        Kind::Utf16Be | Kind::Utf16Le => {
            let be = matches!(encoding.kind(), Kind::Utf16Be);
            let units = bom.then_some(0xFEFF).into_iter().chain(text.encode_utf16());
            for u in units {
                out.extend_from_slice(&if be { u.to_be_bytes() } else { u.to_le_bytes() });
            }
        }
        Kind::SingleByte(table) => {
            single_byte_out(text, &mut out, |c| {
                let c = u16::try_from(u32::from(c)).ok()?;
                let i = table.iter().position(|&t| t == c)?;
                u8::try_from(0x80 + i).ok()
            })?;
        }
        Kind::UserDefined => {
            single_byte_out(text, &mut out, |c| {
                let i = u32::from(c).checked_sub(0xF780).filter(|&i| i < 0x80)?;
                u8::try_from(0x80 + i).ok()
            })?;
        }
        #[cfg(feature = "legacy-cjk")]
        Kind::Cjk(cjk) => crate::cjk::encode(text, *cjk, &mut out)?,
        #[cfg(not(feature = "legacy-cjk"))]
        Kind::Cjk(_) => return Err(EncodeError::Unsupported(encoding)),
        Kind::Replacement => return Err(EncodeError::Unsupported(encoding)),
    }
    Ok(out)
}

fn single_byte_out(
    text: &str,
    out: &mut Vec<u8>,
    high: impl Fn(char) -> Option<u8>,
) -> Result<(), EncodeError> {
    for (offset, ch) in text.char_indices() {
        let b = if ch.is_ascii() {
            u8::try_from(ch).ok()
        } else {
            high(ch)
        };
        out.push(b.ok_or(EncodeError::Unmappable { offset, ch })?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(bytes: &[u8]) -> Decoded<'_> {
        decode_stylesheet(bytes, None, None).unwrap()
    }

    #[test]
    fn labels_follow_get_an_encoding() {
        assert_eq!(Encoding::UTF_8.name(), "UTF-8");
        for (label, name) in [
            ("utf-8", "UTF-8"),
            (" UTF8\t", "UTF-8"),
            ("latin1", "windows-1252"),
            ("iso-8859-1", "windows-1252"),
            ("ascii", "windows-1252"),
            ("utf-16", "UTF-16LE"),
            ("sjis", "Shift_JIS"),
            ("koi8-ru", "KOI8-U"),
        ] {
            assert_eq!(
                Encoding::for_label(label).map(Encoding::name),
                Some(name),
                "{label}"
            );
        }
        assert_eq!(Encoding::for_label("utf-32"), None);
        assert_eq!(Encoding::for_label("utf 8"), None);
        assert_eq!(
            Encoding::for_label("gbk").unwrap().is_supported(),
            cfg!(feature = "legacy-cjk")
        );
        let kr = Encoding::for_label("iso-2022-kr").unwrap();
        assert_eq!((kr.name(), kr.is_supported()), ("replacement", true));
    }

    #[test]
    fn the_charset_rule_is_byte_exact() {
        assert_eq!(charset_rule(b"@charset \"utf-8\"; p {}"), Some("utf-8"));
        assert_eq!(charset_rule(b"@charset \"\";"), Some(""));
        assert_eq!(charset_rule(b"@charset \"a;b\";"), Some("a;b"));
        for bad in [
            &b"@charset 'utf-8';"[..],
            b"@charset  \"utf-8\";",
            b"@CHARSET \"utf-8\";",
            b"@charset \"utf-8\" ;",
            b"@charset \"utf-8\"",
            b" @charset \"utf-8\";",
            b"/**/@charset \"utf-8\";",
            b"\xEF\xBB\xBF@charset \"utf-8\";",
            b"@charset \"\xC3\xA9\";",
        ] {
            assert_eq!(charset_rule(bad), None, "{}", String::from_utf8_lossy(bad));
        }
        // The whole declaration must sit in the first 1024 bytes.
        let label = "x".repeat(1024 - 12);
        let fits = format!("@charset \"{label}\";");
        assert_eq!(fits.len(), 1024);
        assert_eq!(charset_rule(fits.as_bytes()), Some(label.as_str()));
        let over = format!("@charset \"{label}x\";");
        assert_eq!(charset_rule(over.as_bytes()), None);
    }

    #[test]
    fn the_spec_order_decides() {
        let latin = b"@charset \"latin1\"; a { content: \"\xE9\" }";
        // `@charset`.
        let d = decode(latin);
        assert_eq!(d.encoding.name(), "windows-1252");
        assert!(d.text.contains('é'));
        // The protocol beats it.
        let d = decode_stylesheet(latin, Some("koi8-r"), None).unwrap();
        assert_eq!(d.encoding.name(), "KOI8-R");
        // An unknown protocol label is skipped.
        let d = decode_stylesheet(latin, Some("nonsense"), None).unwrap();
        assert_eq!(d.encoding.name(), "windows-1252");
        // A byte order mark beats everything, and is not in the text.
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice(b"@charset \"latin1\"; a {}");
        let d = decode_stylesheet(&bom, Some("koi8-r"), Some("koi8-u")).unwrap();
        assert_eq!(
            (d.encoding, d.had_bom, d.text.as_ref()),
            (Encoding::UTF_8, true, "@charset \"latin1\"; a {}")
        );
        // The environment comes after `@charset`, UTF-8 after everything.
        let d = decode_stylesheet(b"a { content: \"\xE9\" }", None, Some("latin1")).unwrap();
        assert_eq!(d.encoding.name(), "windows-1252");
        let d = decode_stylesheet(b"@charset \"latin2\"; a{}", None, Some("latin1")).unwrap();
        assert_eq!(d.encoding.name(), "ISO-8859-2");
        assert_eq!(decode(b"a {}").encoding, Encoding::UTF_8);
        // `@charset "utf-16le"` without a mark is read as UTF-8.
        let d = decode(b"@charset \"utf-16le\"; a {}");
        assert_eq!((d.encoding, d.lossless), (Encoding::UTF_8, true));
    }

    #[test]
    fn utf16_with_and_without_a_mark() {
        let le: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("a{}".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        let d = decode(&le);
        assert_eq!(
            (d.encoding.name(), d.text.as_ref(), d.had_bom),
            ("UTF-16LE", "a{}", true)
        );
        let be: Vec<u8> = "é".encode_utf16().flat_map(u16::to_be_bytes).collect();
        let d = decode_stylesheet(&be, Some("utf-16be"), None).unwrap();
        assert_eq!((d.text.as_ref(), d.had_bom, d.lossless), ("é", false, true));
        // An odd byte at the end, a lone surrogate.
        assert!(!decode(&[0xFF, 0xFE, b'a', 0, b'b']).lossless);
        assert!(!decode(&[0xFF, 0xFE, 0x00, 0xD8, b'a', 0]).lossless);
    }

    #[test]
    fn lossless_means_no_byte_was_replaced() {
        assert!(decode("a { content: \"\u{FFFD}\" }".as_bytes()).lossless);
        let d = decode(b"a { content: \"\xFF\" }");
        assert!(!d.lossless);
        assert!(d.text.contains('\u{FFFD}'));
        // 0xA5 has no character in ISO-8859-3.
        let d = decode_stylesheet(b"\xA5", Some("iso-8859-3"), None).unwrap();
        assert_eq!((d.text.as_ref(), d.lossless), ("\u{FFFD}", false));
        // Valid UTF-8 is not copied.
        assert!(matches!(decode(b"a{}").text, Cow::Borrowed(_)));
    }

    #[test]
    fn replacement_decodes_to_one_replacement_character() {
        // `iso-2022-kr` and the like name it, so that no byte gets through.
        let d = decode_stylesheet(b"a { color: red }", Some("iso-2022-kr"), None).unwrap();
        assert_eq!(d.encoding.name(), "replacement");
        assert_eq!((d.text.as_ref(), d.lossless), ("\u{FFFD}", false));
        let d = decode_stylesheet(b"", None, Some("hz-gb-2312")).unwrap();
        assert_eq!((d.text.as_ref(), d.lossless), ("", true));
        // A byte order mark still decides.
        let d = decode_stylesheet(b"\xEF\xBB\xBFa{}", Some("iso-2022-kr"), None).unwrap();
        assert_eq!(d.text, "a{}");
    }

    #[test]
    #[cfg(not(feature = "legacy-cjk"))]
    fn unsupported_encodings_are_errors_naming_the_label() {
        let err = decode_stylesheet(b"@charset \"Shift_JIS\"; a{}", None, None).unwrap_err();
        assert_eq!(
            err,
            DecodeError::Unsupported {
                label: "Shift_JIS".into(),
                encoding: Encoding::for_label("shift_jis").unwrap(),
                source: LabelSource::Charset,
            }
        );
        let err = decode_stylesheet(b"a{}", None, Some(" GBK ")).unwrap_err();
        assert!(matches!(
            err,
            DecodeError::Unsupported { ref label, source: LabelSource::Environment, .. } if label == " GBK "
        ));
        // A byte order mark still decides.
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice(b"@charset \"Shift_JIS\"; a{}");
        assert!(decode_stylesheet(&bom, None, None).is_ok());
        // An unknown label is no label.
        assert_eq!(
            decode(b"@charset \"utf-32\"; a{}").encoding,
            Encoding::UTF_8
        );
    }

    #[test]
    fn a_lossless_decode_encodes_back_byte_for_byte() {
        // Every byte of every supported single-byte encoding that maps.
        for (i, (name, _)) in ENCODINGS.iter().enumerate() {
            let enc = Encoding(u8::try_from(i).unwrap());
            if !matches!(
                enc.kind(),
                Kind::Utf8 | Kind::SingleByte(_) | Kind::UserDefined
            ) {
                continue;
            }
            let bytes: Vec<u8> = (0..=255u8)
                .filter(|&b| {
                    let one = [b];
                    decode_stylesheet(&one, Some(name), None).unwrap().lossless
                })
                .collect();
            let d = decode_stylesheet(&bytes, Some(name), None).unwrap();
            assert!(d.lossless, "{name}");
            assert_eq!(
                encode_stylesheet(&d.text, enc, false).unwrap(),
                bytes,
                "{name}"
            );
        }
        for bytes in [
            &b"\xEF\xBB\xBFa { content: \"\xC3\xA9\" }"[..],
            b"\xFE\xFF\x00a\x00{\x00}\xD8\x3D\xDE\x00",
            b"\xFF\xFEa\x00{\x00}\x00",
        ] {
            let d = decode(bytes);
            assert!(d.lossless);
            assert_eq!(
                encode_stylesheet(&d.text, d.encoding, d.had_bom).unwrap(),
                bytes
            );
        }
    }

    #[test]
    fn encoding_reports_what_has_no_byte() {
        let latin = Encoding::for_label("latin1").unwrap();
        assert_eq!(
            encode_stylesheet("a{content:\"€ ✓\"}", latin, false),
            Err(EncodeError::Unmappable {
                offset: 15,
                ch: '✓'
            })
        );
        let sjis = Encoding::for_label("sjis").unwrap();
        assert_eq!(
            encode_stylesheet("a", sjis, false).is_ok(),
            cfg!(feature = "legacy-cjk")
        );
        let replacement = Encoding::for_label("replacement").unwrap();
        assert_eq!(
            encode_stylesheet("", replacement, false),
            Err(EncodeError::Unsupported(replacement))
        );
        let user = Encoding::for_label("x-user-defined").unwrap();
        let d = decode_stylesheet(b"a\x80\xFF", Some("x-user-defined"), None).unwrap();
        assert_eq!(d.text, "a\u{F780}\u{F7FF}");
        assert_eq!(
            encode_stylesheet(&d.text, user, false).unwrap(),
            b"a\x80\xFF"
        );
    }
}
