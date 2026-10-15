//! Bounded LZ4 block decoding with bulk copies for overlapping matches.
//!
//! The encoder and test oracle remain lz4_flex. This implementation follows
//! https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md and uses only safe
//! slice operations. Repeated RGBA pixels produce long matches with offsets of
//! four or eight bytes; copying one byte at a time makes those tiles needlessly
//! expensive. Seed one complete period, then double the initialized prefix.

pub(crate) fn decompress_into(input: &[u8], output: &mut [u8]) -> Result<usize, &'static str> {
    let mut source = 0_usize;
    let mut target = 0_usize;
    loop {
        let token = *input.get(source).ok_or("missing sequence token")?;
        source += 1;
        let literals = length(usize::from(token >> 4), input, &mut source)?;
        let source_end = source
            .checked_add(literals)
            .ok_or("literal length overflow")?;
        let target_end = target
            .checked_add(literals)
            .ok_or("literal output overflow")?;
        let literal_bytes = input.get(source..source_end).ok_or("truncated literals")?;
        output
            .get_mut(target..target_end)
            .ok_or("literal output exceeds tile")?
            .copy_from_slice(literal_bytes);
        source = source_end;
        target = target_end;
        if source == input.len() {
            return Ok(target);
        }
        let offset_end = source.checked_add(2).ok_or("offset overflow")?;
        let offset_bytes = input.get(source..offset_end).ok_or("truncated offset")?;
        let offset = usize::from(u16::from_le_bytes([offset_bytes[0], offset_bytes[1]]));
        if offset == 0 || offset > target {
            return Err("match offset precedes initialized output");
        }
        source = offset_end;
        let matched = length(usize::from(token & 15), input, &mut source)?
            .checked_add(4)
            .ok_or("match length overflow")?;
        let end = target
            .checked_add(matched)
            .filter(|end| *end <= output.len())
            .ok_or("match output exceeds tile")?;
        let initial = offset.min(matched);
        output.copy_within(target - offset..target - offset + initial, target);
        let mut copied = initial;
        while copied < matched {
            let amount = copied.min(matched - copied);
            output.copy_within(target..target + amount, target + copied);
            copied += amount;
        }
        target = end;
        // The next iteration requires a final literal sequence. A match cannot
        // silently consume the whole input without that final token.
    }
}

fn length(base: usize, input: &[u8], source: &mut usize) -> Result<usize, &'static str> {
    if base != 15 {
        return Ok(base);
    }
    let mut length = base;
    loop {
        let byte = *input.get(*source).ok_or("truncated length extension")?;
        *source += 1;
        length = length
            .checked_add(usize::from(byte))
            .ok_or("length extension overflow")?;
        if byte != 255 {
            return Ok(length);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::decompress_into;

    fn compare(bytes: &[u8]) {
        let compressed = lz4_flex::block::compress(bytes);
        let mut actual = vec![0x33; bytes.len()];
        let mut reference = vec![0x55; bytes.len()];
        assert_eq!(
            decompress_into(&compressed, &mut actual).unwrap(),
            bytes.len()
        );
        assert_eq!(
            lz4_flex::block::decompress_into(&compressed, &mut reference).unwrap(),
            bytes.len()
        );
        assert_eq!(actual, reference);
        assert_eq!(actual, bytes);
    }

    #[test]
    fn bulk_overlap_decoder_matches_reference_for_rgba_runs_and_every_short_period() {
        compare(&[]);
        for length in 0..128 {
            compare(&vec![0x71; length]);
        }
        for period in 1..=257 {
            let bytes: Vec<_> = (0..131_077).map(|i| ((i % period) * 37) as u8).collect();
            compare(&bytes);
        }
        for length in [65_535, 65_536, 262_144, 524_288] {
            let bytes: Vec<_> = (0..length)
                .map(|i| [0, 0x38, 0, 0, 0, 0, 0, 0x3c][i % 8])
                .collect();
            compare(&bytes);
        }
    }

    #[test]
    fn seeded_random_and_mixed_literal_match_tiles_decode_identically_to_reference() {
        let mut state = 79_u64;
        for trial in 0..1000 {
            let length = (trial * 7919) % 8192;
            let mut bytes = Vec::with_capacity(length);
            for i in 0..length {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let value = if (i / 31 + trial).is_multiple_of(3) {
                    (i % 8) as u8
                } else {
                    state as u8
                };
                bytes.push(value);
            }
            compare(&bytes);
        }
    }

    #[test]
    fn malformed_streams_reject_truncated_lengths_offsets_and_output_overruns() {
        for bytes in [
            vec![],
            vec![0xf0],
            vec![0xf0, 255],
            vec![0x20, 1],
            vec![0x10, 1, 1],
            vec![0x10, 1, 0, 0, 0],
            vec![0x10, 1, 2, 0, 0],
            vec![0x1f, 1, 1, 0, 255],
            vec![0x10, 1, 1, 0],
            vec![0xff, 255, 255, 255, 255],
        ] {
            assert!(decompress_into(&bytes, &mut [0; 128]).is_err(), "{bytes:?}");
        }
        let compressed = lz4_flex::block::compress(&[0x73; 1024]);
        for length in 0..1024 {
            assert!(decompress_into(&compressed, &mut vec![0; length]).is_err());
        }
    }

    #[test]
    fn mutated_blocks_never_panic_or_write_outside_the_output_and_match_reference_when_accepted() {
        let original: Vec<_> = (0..1024).map(|i| (i % 37) as u8).collect();
        let compressed = lz4_flex::block::compress(&original);
        for index in 0..compressed.len() {
            for byte in [0, 1, 15, 16, 127, 255] {
                let mut mutation = compressed.clone();
                mutation[index] = byte;
                let mut guarded = [0xa7; 1032];
                let decoded = decompress_into(&mutation, &mut guarded[4..1028]);
                assert_eq!(&guarded[..4], &[0xa7; 4]);
                assert_eq!(&guarded[1028..], &[0xa7; 4]);
                if let Ok(size) = decoded {
                    let mut reference = [0; 1024];
                    let reference_size =
                        lz4_flex::block::decompress_into(&mutation, &mut reference).unwrap();
                    assert_eq!(size, reference_size);
                    assert_eq!(&guarded[4..4 + size], &reference[..size]);
                }
            }
        }
    }
}
