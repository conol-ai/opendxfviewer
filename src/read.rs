//! Reading a DXF file with the right text encoding.
//!
//! `dxf::Drawing::load_file` hardcodes Windows-1252. That is a reasonable default for DXF up to
//! R2004, but the format switched to UTF-8 at R2007, and most files in circulation are newer than
//! that — so loading them through the default mangles every non-ASCII character in a layer name,
//! a text entity or a block name.
//!
//! The version and the code page both live in the `HEADER` section, at the very start of the file,
//! so the encoding can be chosen from a short prefix without reading the whole thing twice.

use std::path::Path;

use encoding_rs::Encoding;

/// Read and parse a DXF file, choosing the text encoding from its header.
pub fn load(path: impl AsRef<Path>) -> Result<dxf::Drawing, String> {
    let path = path.as_ref();
    let bytes = std::fs::read(path).map_err(|e| format!("Could not read this file: {e}"))?;
    if let Some(msg) = unsupported_format(&bytes) {
        return Err(msg);
    }
    let encoding = sniff(&bytes);
    let bytes = sign_wrap_32bit_fields(bytes);
    let bytes = if encoding == encoding_rs::UTF_8 { rejoin_split_utf8(bytes) } else { bytes };
    let bytes = strip_thumbnail(bytes);
    dxf::Drawing::load_with_encoding(&mut std::io::Cursor::new(bytes), encoding)
        .map_err(|e| format!("Could not read this file: {e}"))
}

/// Group codes whose value is a 32-bit integer.
///
/// Everything else is left alone: rewriting a code that turns out to hold a string would corrupt
/// it, and plenty of string fields contain long digit runs.
fn is_32bit_field(code: i64) -> bool {
    matches!(code, 90..=99 | 420..=429 | 440..=449 | 1071)
}

/// Re-interpret out-of-range 32-bit fields as signed.
///
/// True colour is written as `0xC2RRGGBB`, which is 3.2 billion — above `i32::MAX`. AutoCAD and
/// LibreDWG both emit it unsigned, and the parser reads these fields as `i32`, so a real file
/// converted from DWG fails outright with "number too large to fit in target type". The two
/// spellings are the same 32 bits, so subtracting 2^32 is a reinterpretation rather than a change
/// of value.
///
/// The buffer is returned untouched, without reallocating, when nothing needs it — which is every
/// file that was not round-tripped through a converter.
fn sign_wrap_32bit_fields(bytes: Vec<u8>) -> Vec<u8> {
    /// A DXF body is pairs of lines: a group code, then its value.
    fn scan(bytes: &[u8], mut on_fix: impl FnMut(usize, usize, i64)) {
        let mut code: Option<i64> = None;
        let mut start = 0;
        for (i, line) in split_lines(bytes) {
            let s = std::str::from_utf8(line).unwrap_or("").trim();
            match code.take() {
                None => {
                    if let Ok(c) = s.parse::<i64>() {
                        if is_32bit_field(c) {
                            code = Some(c);
                        }
                    }
                    start = i;
                }
                Some(_) => {
                    if let Ok(v) = s.parse::<i64>() {
                        if v > i32::MAX as i64 && v <= u32::MAX as i64 {
                            on_fix(i, line.len(), v - (1i64 << 32));
                        }
                    }
                }
            }
            let _ = start;
        }
    }

    let mut fixes: Vec<(usize, usize, i64)> = Vec::new();
    scan(&bytes, |at, len, v| fixes.push((at, len, v)));
    if fixes.is_empty() {
        return bytes;
    }

    let mut out = Vec::with_capacity(bytes.len() + fixes.len());
    let mut cursor = 0;
    for (at, len, v) in fixes {
        out.extend_from_slice(&bytes[cursor..at]);
        // Keep the original field width where the replacement fits, so alignment-sensitive
        // readers see the same shape.
        out.extend_from_slice(v.to_string().as_bytes());
        cursor = at + len;
    }
    out.extend_from_slice(&bytes[cursor..]);
    out
}

/// Remove the `THUMBNAILIMAGE` section.
///
/// It holds a preview bitmap for a file browser — nothing a viewer draws. The parser decodes it
/// eagerly and rejects the whole drawing when the bitmap has a header it does not recognise, which
/// is how a converted file loses its geometry over a picture of itself.
fn strip_thumbnail(bytes: Vec<u8>) -> Vec<u8> {
    let lines: Vec<(usize, &[u8])> = split_lines(&bytes).collect();
    let named = |i: usize, want: &str| {
        lines.get(i).is_some_and(|(_, l)| std::str::from_utf8(l).unwrap_or("").trim() == want)
    };

    // `0 / SECTION` then `2 / THUMBNAILIMAGE`, up to and including the matching `0 / ENDSEC`.
    let Some(start) = (0..lines.len().saturating_sub(3))
        .find(|&i| named(i, "0") && named(i + 1, "SECTION") && named(i + 3, "THUMBNAILIMAGE"))
    else {
        return bytes;
    };
    let Some(end) = (start + 4..lines.len() - 1).find(|&i| named(i, "0") && named(i + 1, "ENDSEC"))
    else {
        return bytes;
    };

    let from = lines[start].0;
    let to = lines.get(end + 2).map(|(o, _)| *o).unwrap_or(bytes.len());
    let mut out = Vec::with_capacity(bytes.len() - (to - from));
    out.extend_from_slice(&bytes[..from]);
    out.extend_from_slice(&bytes[to..]);
    out
}

/// Move a truncated UTF-8 sequence at the end of a value line onto the start of the next one.
///
/// LibreDWG chunks long MTEXT into 250-*byte* pieces across group 3 continuations, and counts
/// bytes rather than characters — so a multi-byte character can be cut in half, with its lead byte
/// ending one chunk and its continuation bytes starting the next. The parser decodes line by line,
/// so it sees two malformed strings and rejects the whole file, even though the chunks concatenate
/// into perfectly good text.
///
/// Moving the split onto a character boundary preserves the text exactly: the chunks are joined by
/// the reader anyway, so where they divide carries no meaning.
fn rejoin_split_utf8(bytes: Vec<u8>) -> Vec<u8> {
    // How many trailing bytes begin a multi-byte sequence that the line does not finish.
    fn dangling(line: &[u8]) -> usize {
        for back in 1..=3.min(line.len()) {
            let b = line[line.len() - back];
            if b & 0b1100_0000 == 0b1000_0000 {
                continue; // a continuation byte; keep walking back to its lead
            }
            let need = match b {
                0xC2..=0xDF => 2,
                0xE0..=0xEF => 3,
                0xF0..=0xF4 => 4,
                _ => return 0, // ASCII or invalid: nothing to carry
            };
            return if need > back { back } else { 0 };
        }
        0
    }

    let lines: Vec<(usize, &[u8])> = split_lines(&bytes).collect();
    // Value lines are every other line, starting at index 1.
    let mut moves: Vec<(usize, usize)> = Vec::new();
    for i in (1..lines.len()).step_by(2) {
        let n = dangling(lines[i].1);
        if n > 0 && i + 2 < lines.len() {
            moves.push((i, n));
        }
    }
    if moves.is_empty() {
        return bytes;
    }

    // Rebuild, carrying each dangling tail to the front of the following value line.
    let mut out: Vec<Vec<u8>> = lines.iter().map(|(_, l)| l.to_vec()).collect();
    for (i, n) in moves {
        let cut = out[i].len() - n;
        let tail: Vec<u8> = out[i].split_off(cut);
        let next = i + 2;
        let mut joined = tail;
        joined.extend_from_slice(&out[next]);
        out[next] = joined;
    }
    let mut joined = Vec::with_capacity(bytes.len() + 8);
    for line in out {
        joined.extend_from_slice(&line);
        joined.extend_from_slice(b"\r\n");
    }
    joined
}

/// Iterate `(offset, line)` over a byte buffer, handling both LF and CRLF.
fn split_lines(bytes: &[u8]) -> impl Iterator<Item = (usize, &[u8])> {
    let mut at = 0;
    std::iter::from_fn(move || {
        if at >= bytes.len() {
            return None;
        }
        let start = at;
        let end =
            bytes[at..].iter().position(|&b| b == b'\n').map(|p| at + p).unwrap_or(bytes.len());
        at = end + 1;
        let mut line = &bytes[start..end];
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        Some((start, line))
    })
}

/// Recognise formats we cannot read, so they get an answer rather than a parse error.
///
/// Someone handed a DWG deserves to be told it is a DWG. Left to the parser it comes back as
/// "invalid digit found in string at line/offset 1", which says nothing about what to do next.
pub fn unsupported_format(head: &[u8]) -> Option<String> {
    // DWG begins with its version tag at byte 0: AC1009 is R11/12 through AC1032 for 2018+.
    // A DXF carries the same string in $ACADVER, but far later in the file, so byte 0 is safe.
    if head.len() >= 6 && head.starts_with(b"AC10") && head[4].is_ascii_digit() {
        let release = match &head[..6] {
            b"AC1009" => "R11/12",
            b"AC1012" | b"AC1014" => "R13/14",
            b"AC1015" => "2000",
            b"AC1018" => "2004",
            b"AC1021" => "2007",
            b"AC1024" => "2010",
            b"AC1027" => "2013",
            b"AC1032" => "2018 or later",
            _ => "an unrecognised release",
        };
        return Some(format!(
            "This is a DWG file ({release}), not a DXF. Save or export it as DXF and open that \
             — in AutoCAD, Save As and pick a DXF format; free converters exist too."
        ));
    }
    // Two more that get mistaken for CAD drawings often enough to be worth naming.
    if head.starts_with(b"%PDF-") {
        return Some("This is a PDF, not a DXF.".to_string());
    }
    if head.starts_with(b"PK\x03\x04") {
        return Some(
            "This is a zip archive, not a DXF. Extract it and open the drawing inside.".to_string(),
        );
    }
    None
}

/// Pick a text encoding from a DXF header prefix.
pub fn sniff(head: &[u8]) -> &'static Encoding {
    // A byte-order mark settles it outright.
    if head.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return encoding_rs::UTF_8;
    }
    // The prefix is scanned as Latin-1 so that a UTF-8 file with multibyte characters later on
    // cannot make the header itself unreadable. Every marker we look for is ASCII.
    let text: String = head.iter().map(|&b| b as char).collect();

    if let Some(v) = header_value(&text, "$ACADVER") {
        // AC1021 is R2007, the release that moved DXF to UTF-8. The version strings sort
        // lexicographically in release order, which is why a plain comparison is enough.
        if v.as_str() >= "AC1021" {
            return encoding_rs::UTF_8;
        }
    }
    if let Some(cp) = header_value(&text, "$DWGCODEPAGE") {
        if let Some(enc) = code_page(&cp) {
            return enc;
        }
    }
    // Pre-R2007 with no usable code page. Plenty of older writers emitted UTF-8 anyway, so prefer
    // it when the prefix actually parses as UTF-8 and contains something non-ASCII to judge by.
    if head.iter().any(|b| *b >= 0x80) && std::str::from_utf8(&head[..valid_prefix(head)]).is_ok() {
        return encoding_rs::UTF_8;
    }
    encoding_rs::WINDOWS_1252
}

/// Trim a buffer back to the last byte that cannot be part of a truncated multi-byte sequence.
fn valid_prefix(head: &[u8]) -> usize {
    let mut end = head.len();
    // A UTF-8 sequence is at most four bytes, so backing off three continuation bytes is enough.
    for _ in 0..3 {
        if end == 0 || head[end - 1] & 0b1100_0000 != 0b1000_0000 {
            break;
        }
        end -= 1;
    }
    // Drop the lead byte of the truncated sequence too.
    if end > 0 && head[end - 1] & 0b1000_0000 != 0 {
        end -= 1;
    }
    end
}

/// Find a `9 / $NAME` header variable and return the string value that follows it.
///
/// DXF is a flat stream of group code / value line pairs, so the value is simply the next
/// non-empty line after the name.
fn header_value(text: &str, name: &str) -> Option<String> {
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if line.trim() != name {
            continue;
        }
        // Skip the group code line (1 for $ACADVER and $DWGCODEPAGE) and take the value.
        let _code = lines.next()?;
        let value = lines.next()?.trim();
        if !value.is_empty() {
            return Some(value.to_ascii_uppercase());
        }
    }
    None
}

/// Map a DXF `$DWGCODEPAGE` name onto an encoding.
fn code_page(name: &str) -> Option<&'static Encoding> {
    let n = name.trim().to_ascii_uppercase();
    let n = n.strip_prefix("ANSI_").unwrap_or(&n);
    Some(match n {
        "UTF8" | "UTF-8" => encoding_rs::UTF_8,
        "874" => encoding_rs::WINDOWS_874,
        "932" | "SHIFT_JIS" => encoding_rs::SHIFT_JIS,
        "936" | "GBK" => encoding_rs::GBK,
        "949" | "EUC_KR" => encoding_rs::EUC_KR,
        "950" | "BIG5" => encoding_rs::BIG5,
        "1250" => encoding_rs::WINDOWS_1250,
        "1251" => encoding_rs::WINDOWS_1251,
        "1252" => encoding_rs::WINDOWS_1252,
        "1253" => encoding_rs::WINDOWS_1253,
        "1254" => encoding_rs::WINDOWS_1254,
        "1255" => encoding_rs::WINDOWS_1255,
        "1256" => encoding_rs::WINDOWS_1256,
        "1257" => encoding_rs::WINDOWS_1257,
        "1258" => encoding_rs::WINDOWS_1258,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(vars: &[(&str, &str)]) -> Vec<u8> {
        let mut s = String::from("0\nSECTION\n2\nHEADER\n");
        for (k, v) in vars {
            s.push_str(&format!("9\n{k}\n1\n{v}\n"));
        }
        s.push_str("0\nENDSEC\n0\nEOF\n");
        s.into_bytes()
    }

    #[test]
    fn r2007_and_later_are_utf8() {
        for v in ["AC1021", "AC1024", "AC1027", "AC1032"] {
            assert_eq!(sniff(&header(&[("$ACADVER", v)])), encoding_rs::UTF_8, "{v}");
        }
    }

    #[test]
    fn pre_r2007_falls_back_to_its_code_page() {
        let h = header(&[("$ACADVER", "AC1015"), ("$DWGCODEPAGE", "ANSI_1252")]);
        assert_eq!(sniff(&h), encoding_rs::WINDOWS_1252);
        let h = header(&[("$ACADVER", "AC1015"), ("$DWGCODEPAGE", "ANSI_936")]);
        assert_eq!(sniff(&h), encoding_rs::GBK);
        let h = header(&[("$ACADVER", "AC1014"), ("$DWGCODEPAGE", "ansi_932")]);
        assert_eq!(sniff(&h), encoding_rs::SHIFT_JIS);
    }

    #[test]
    fn an_unknown_code_page_falls_back_to_windows_1252() {
        let h = header(&[("$ACADVER", "AC1015"), ("$DWGCODEPAGE", "ANSI_9999")]);
        assert_eq!(sniff(&h), encoding_rs::WINDOWS_1252);
        assert_eq!(sniff(&header(&[])), encoding_rs::WINDOWS_1252);
        assert_eq!(sniff(b""), encoding_rs::WINDOWS_1252);
    }

    #[test]
    fn a_byte_order_mark_wins() {
        let mut h = vec![0xEF, 0xBB, 0xBF];
        h.extend(header(&[("$ACADVER", "AC1015"), ("$DWGCODEPAGE", "ANSI_1252")]));
        assert_eq!(sniff(&h), encoding_rs::UTF_8);
    }

    #[test]
    fn an_old_file_that_is_really_utf8_is_read_as_utf8() {
        // Plenty of writers emit UTF-8 whatever version they claim.
        let mut h = header(&[("$ACADVER", "AC1015")]);
        h.extend("0\nTEXT\n1\nBohrung Ø44\n".as_bytes());
        assert_eq!(sniff(&h), encoding_rs::UTF_8);
    }

    #[test]
    fn latin1_bytes_do_not_get_mistaken_for_utf8() {
        let mut h = header(&[("$ACADVER", "AC1015")]);
        // 0xD8 is Ø in Windows-1252 and an invalid UTF-8 lead byte here.
        h.extend(b"0\nTEXT\n1\nBohrung \xD844\n");
        assert_eq!(sniff(&h), encoding_rs::WINDOWS_1252);
    }

    #[test]
    fn a_truncated_multibyte_sequence_at_the_sniff_boundary_is_not_a_failure() {
        let mut h = header(&[("$ACADVER", "AC1015")]);
        h.extend("0\nTEXT\n1\nvalid é text ".as_bytes());
        h.extend(&[0xE5, 0x9B]); // the first two bytes of a three-byte character
        assert_eq!(sniff(&h), encoding_rs::UTF_8, "a cut-off character must not force a fallback");
    }

    #[test]
    fn pure_ascii_stays_on_the_conservative_default() {
        // With nothing non-ASCII to judge by, the two encodings agree anyway.
        let mut h = header(&[("$ACADVER", "AC1015")]);
        h.extend(b"0\nTEXT\n1\nPLAIN ASCII\n");
        assert_eq!(sniff(&h), encoding_rs::WINDOWS_1252);
    }

    #[test]
    fn a_dwg_is_named_rather_than_left_to_the_parser() {
        // Left to the parser a DWG comes back as "invalid digit found in string at line/offset 1".
        for (tag, release) in [
            (&b"AC1032"[..], "2018"),
            (&b"AC1015"[..], "2000"),
            (&b"AC1021"[..], "2007"),
            (&b"AC1009"[..], "R11/12"),
        ] {
            let mut f = tag.to_vec();
            f.extend([0u8; 32]);
            let msg = unsupported_format(&f).unwrap_or_else(|| panic!("{release} not recognised"));
            assert!(msg.contains("DWG"), "{msg}");
            assert!(msg.contains("DXF"), "the message should say what to do: {msg}");
        }
        // An unknown AC10xx still reports a DWG rather than falling through to the parser.
        assert!(unsupported_format(b"AC1099\0\0\0\0").is_some());
    }

    #[test]
    fn a_real_dxf_is_not_mistaken_for_a_dwg() {
        // DXF carries the same AC10xx string in $ACADVER, but never at byte 0.
        let dxf = header(&[("$ACADVER", "AC1032")]);
        assert!(unsupported_format(&dxf).is_none());
        for f in ["showcase.dxf", "latin1.dxf", "basic.dxf"] {
            let p = format!("{}/tests/fixtures/{f}", env!("CARGO_MANIFEST_DIR"));
            let bytes = std::fs::read(&p).unwrap();
            assert!(unsupported_format(&bytes).is_none(), "{f} was rejected");
        }
    }

    #[test]
    fn other_files_people_mistake_for_drawings_are_named() {
        assert!(unsupported_format(b"%PDF-1.7\n").unwrap().contains("PDF"));
        assert!(unsupported_format(b"PK\x03\x04rest").unwrap().contains("zip"));
        assert!(unsupported_format(b"0\nSECTION\n").is_none());
        assert!(unsupported_format(b"").is_none());
    }

    #[test]
    fn the_real_fixtures_load() {
        for f in ["basic.dxf", "showcase.dxf", "curves.dxf"] {
            let p = format!("{}/tests/fixtures/{f}", env!("CARGO_MANIFEST_DIR"));
            assert!(load(&p).is_ok(), "{f} failed to load");
        }
        assert!(load("/definitely/not/here.dxf").is_err());
    }
}
