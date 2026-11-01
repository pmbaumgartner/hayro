use crate::reader::Reader;
use alloc::vec;
use alloc::vec::Vec;

/// Decodes at most `limit` bytes.
pub(crate) fn decode(data: &[u8], limit: usize) -> Option<Vec<u8>> {
    let mut reader = Reader::new(data);
    let mut decoded = vec![];

    while decoded.len() < limit {
        let length = reader.read_byte()?;

        match length {
            128 => break,
            0..=127 => {
                // PDFBOX-3990, just abort early if stream is invalid.
                let Some(bytes) = reader.read_bytes(length as usize + 1) else {
                    break;
                };

                decoded.extend(bytes);
            }
            _ => {
                let length = 257 - length as usize;
                decoded.extend([reader.read_byte()?].repeat(length));
            }
        }
    }

    decoded.truncate(limit);
    Some(decoded)
}

#[cfg(test)]
mod tests {
    use crate::filter::run_length::decode;

    #[test]
    fn run_length() {
        let input = vec![4, 10, 11, 12, 13, 14, 253, 3, 128];
        assert_eq!(
            decode(&input, usize::MAX).unwrap(),
            vec![10, 11, 12, 13, 14, 3, 3, 3, 3]
        );
    }
}
