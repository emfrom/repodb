use crate::Result;

pub(crate) fn slice_utf8(
    bytes: &[u8],
    blob_id: &str,
    start: usize,
    end: usize,
    kind: &str,
) -> Result<String> {
    if start > end {
        return Err(format!("invalid {kind} Git byte range {start}..{end}").into());
    }
    let blob = std::str::from_utf8(bytes)
        .map_err(|_| format!("{kind} Git blob {blob_id} is no longer valid UTF-8"))?;
    blob.get(start..end).map(str::to_owned).ok_or_else(|| {
        format!("invalid {kind} Git byte range {start}..{end} for blob {blob_id}").into()
    })
}

#[cfg(test)]
mod tests {
    use super::slice_utf8;

    #[test]
    fn slices_valid_utf8_byte_ranges() {
        assert_eq!(
            slice_utf8("aéz".as_bytes(), "blob", 1, 3, "indexed").unwrap(),
            "é"
        );
    }

    #[test]
    fn validates_order_before_utf8_and_preserves_contextual_errors() {
        assert_eq!(
            slice_utf8(b"\xff", "blob", 3, 2, "imported")
                .unwrap_err()
                .to_string(),
            "invalid imported Git byte range 3..2"
        );
        assert_eq!(
            slice_utf8(b"\xff", "blob", 0, 1, "indexed")
                .unwrap_err()
                .to_string(),
            "indexed Git blob blob is no longer valid UTF-8"
        );
    }

    #[test]
    fn all_utf8_byte_ranges_match_independent_slice_validation() {
        let source = "Aé🦀\nZ";
        let bytes = source.as_bytes();
        let boundaries = source
            .char_indices()
            .map(|(offset, _)| offset)
            .chain([bytes.len()])
            .collect::<Vec<_>>();
        for start in 0..=bytes.len() {
            for end in 0..=bytes.len() {
                if start > end || !boundaries.contains(&start) || !boundaries.contains(&end) {
                    assert!(slice_utf8(bytes, "blob", start, end, "indexed").is_err());
                    continue;
                }
                let expected = std::str::from_utf8(&bytes[start..end]).unwrap();
                assert_eq!(
                    slice_utf8(bytes, "blob", start, end, "indexed").unwrap(),
                    expected
                );
            }
        }
    }

    #[test]
    fn rejects_out_of_bounds_and_non_character_boundaries() {
        for (start, end) in [(0, 9), (2, 3)] {
            assert!(
                slice_utf8("aéz".as_bytes(), "blob", start, end, "indexed")
                    .unwrap_err()
                    .to_string()
                    .contains(&format!(
                        "invalid indexed Git byte range {start}..{end} for blob blob"
                    ))
            );
        }
    }
}
