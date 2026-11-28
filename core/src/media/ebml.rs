/// Inspect only the bounded EBML header. The Matroska demuxer serves both
/// representations; filenames and its combined name cannot identify WebM.
pub(super) fn document_type(header: &[u8]) -> Option<&'static str> {
    fn vint(bytes: &[u8], keep_marker: bool) -> Option<(u64, usize)> {
        let first = *bytes.first()?;
        let length = first.leading_zeros() as usize + 1;
        if length > 8 || bytes.len() < length {
            return None;
        }
        let marker = 1u8 << (8 - length);
        let mut value = u64::from(if keep_marker {
            first
        } else {
            first & (marker - 1)
        });
        for byte in &bytes[1..length] {
            value = (value << 8) | u64::from(*byte);
        }
        if !keep_marker && value == (1u64 << (7 * length)) - 1 {
            return None;
        }
        Some((value, length))
    }
    let (id, id_length) = vint(header, true)?;
    if id != 0x1a45dfa3 {
        return None;
    }
    let (length, size_length) = vint(header.get(id_length..)?, false)?;
    let start = id_length.checked_add(size_length)?;
    let end = start.checked_add(usize::try_from(length).ok()?)?;
    let body = header.get(start..end)?;
    let mut position = 0usize;
    let mut result = None;
    while position < body.len() {
        let (id, id_length) = vint(body.get(position..)?, true)?;
        position = position.checked_add(id_length)?;
        let (length, size_length) = vint(body.get(position..)?, false)?;
        position = position.checked_add(size_length)?;
        let end = position.checked_add(usize::try_from(length).ok()?)?;
        let value = body.get(position..end)?;
        if id == 0x4282 {
            if result.is_some() {
                return None;
            }
            result = Some(match value {
                b"webm" => "webm",
                b"matroska" => "matroska",
                _ => return None,
            });
        }
        position = end;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_only_structural_document_types() {
        let webm = b"\x1a\x45\xdf\xa3\x87\x42\x82\x84webm";
        assert_eq!(document_type(webm), Some("webm"));
        assert_eq!(
            document_type(b"\x1a\x45\xdf\xa3\x8b\x42\x82\x88matroska"),
            Some("matroska")
        );
        for length in 0..webm.len() {
            assert_eq!(document_type(&webm[..length]), None);
        }
        assert_eq!(document_type(b"\x1a\x45\xdf\xa3\x87\x42\x86\x84webm"), None);
        assert_eq!(document_type(b"\x1a\x45\xdf\xa3\xff\x42\x82\x84webm"), None);
        assert_eq!(
            document_type(b"\x1a\x45\xdf\xa3\x8e\x42\x82\x84webm\x42\x82\x84webm"),
            None
        );
    }
}
