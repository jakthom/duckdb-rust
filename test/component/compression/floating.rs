use super::{DataType, Error, QueryContext, Result, SegmentType, Value, compression, pack, read};

fn value(bits: u64, width: usize) -> Value {
    if width == 32 {
        Value::Float(f32::from_bits(bits as u32))
    } else {
        Value::Double(f64::from_bits(bits))
    }
}
fn same_bits(actual: &[Value], expected: &[Value]) {
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        match (actual, expected) {
            (Value::Float(a), Value::Float(b)) => assert_eq!(a.to_bits(), b.to_bits()),
            (Value::Double(a), Value::Double(b)) => assert_eq!(a.to_bits(), b.to_bits()),
            _ => panic!("wrong floating type"),
        }
    }
}
fn type_for(width: usize) -> DataType {
    if width == 32 {
        DataType::Float
    } else {
        DataType::Double
    }
}

fn alprd(width: usize, right_width: usize, dictionary_size: usize) -> (Vec<u8>, Vec<Value>) {
    let count = 33;
    let left_width = ((usize::BITS - (dictionary_size - 1).leading_zeros()) as usize).max(1);
    let high_mask = (1 << (width - right_width)) - 1;
    let dictionary: Vec<u16> = (0..dictionary_size)
        .map(|i| ((i * 31) & high_mask) as u16)
        .collect();
    let mut left: Vec<_> = (0..count).map(|i| (i % dictionary_size) as u128).collect();
    left[0] = dictionary_size as u128 & ((1 << left_width) - 1);
    left[32] = left[0];
    let right: Vec<_> = (0..count)
        .map(|i| (i as u128 * 0xef5788da92765319) & ((1u128 << right_width) - 1))
        .collect();
    let mut expected: Vec<_> = (0..count)
        .map(|i| {
            value(
                (u64::from(dictionary[i % dictionary_size]) << right_width) | right[i] as u64,
                width,
            )
        })
        .collect();
    let replacement = high_mask as u16;
    expected[0] = value(
        (u64::from(replacement) << right_width) | right[0] as u64,
        width,
    );
    expected[32] = value(
        (u64::from(replacement) << right_width) | right[32] as u64,
        width,
    );
    let mut data = vec![0; 4];
    data.extend([right_width as u8, left_width as u8, dictionary_size as u8]);
    for v in dictionary {
        data.extend(v.to_le_bytes());
    }
    let start = data.len() as u32;
    data.extend(2u16.to_le_bytes());
    data.extend(pack(&left, left_width));
    data.extend(pack(&right, right_width));
    for _ in 0..2 {
        data.extend(replacement.to_le_bytes());
    }
    data.extend(0u16.to_le_bytes());
    data.extend(32u16.to_le_bytes());
    data.extend(start.to_le_bytes());
    let end = data.len() as u32;
    data[..4].copy_from_slice(&end.to_le_bytes());
    (data, expected)
}

#[test]
fn alprd_covers_every_cut_width_dictionary_size_and_exception_index() -> Result<()> {
    let registry = compression::decoders();
    for width in [32, 64] {
        for right in width - 16..width {
            for dictionary_size in 1..=8 {
                let (data, expected) = alprd(width, right, dictionary_size);
                same_bits(
                    &read(
                        &registry,
                        11,
                        &data,
                        33,
                        SegmentType::Values(&type_for(width)),
                        &QueryContext::background(),
                    )?,
                    &expected,
                );
            }
        }
    }
    Ok(())
}

fn sample_bits(width: usize, count: usize) -> Vec<u64> {
    let special: Vec<u64> = if width == 32 {
        vec![
            0, 0x80000000, 1, 0x80000001, 0x007fffff, 0x00800000, 0x7f7fffff, 0xff7fffff,
            0x7f800000, 0xff800000, 0xffc00123,
        ]
    } else {
        vec![
            0,
            0x8000000000000000,
            1,
            0x8000000000000001,
            0x000fffffffffffff,
            0x0010000000000000,
            0x7fefffffffffffff,
            0xffefffffffffffff,
            0x7ff0000000000000,
            0xfff0000000000000,
            0xfff8000000000123,
        ]
    };
    (0..count)
        .map(|i| special[(i / 2) % special.len()])
        .collect()
}

fn patas(width: usize, count: usize) -> (Vec<u8>, Vec<Value>) {
    let bits = sample_bits(width, count);
    let mut data = vec![0; 4];
    let mut groups = Vec::new();
    for group in bits.chunks(1024) {
        let start = data.len() as u32;
        let mut stats = Vec::new();
        for (i, &v) in group.iter().enumerate() {
            let distance = if i == 0 {
                0
            } else if i >= 127 {
                127
            } else {
                1
            };
            let xor = if i == 0 { v } else { v ^ group[i - distance] };
            let trailing = if i == 0 {
                0
            } else if xor == 0 {
                width - 1
            } else {
                xor.trailing_zeros() as usize
            };
            let residual = xor >> trailing;
            let bytes = if i == 0 {
                width / 8
            } else {
                (64 - residual.leading_zeros() as usize).div_ceil(8)
            };
            data.extend(&residual.to_le_bytes()[..bytes]);
            stats.extend(
                ((distance as u16) << 9 | ((bytes % 8) as u16) << 6 | trailing as u16)
                    .to_le_bytes(),
            );
        }
        stats.extend(start.to_le_bytes());
        groups.push(stats);
    }
    while !data.len().is_multiple_of(8) {
        data.push(0);
    }
    for group in groups.into_iter().rev() {
        data.extend(group);
    }
    let end = data.len() as u32;
    data[..4].copy_from_slice(&end.to_le_bytes());
    (data, bits.into_iter().map(|v| value(v, width)).collect())
}

struct Bits {
    data: Vec<u8>,
    position: usize,
}
impl Bits {
    fn write(&mut self, value: u64, count: usize) {
        for bit in (0..count).rev() {
            if self.position.is_multiple_of(8) {
                self.data.push(0);
            }
            let last = self.data.last_mut().unwrap();
            *last |= (((value >> bit) & 1) as u8) << (7 - self.position % 8);
            self.position += 1;
        }
    }
}
fn chimp(width: usize, count: usize) -> (Vec<u8>, Vec<Value>) {
    const ZEROS: [usize; 8] = [0, 8, 12, 16, 18, 20, 22, 24];
    let mut bits = Bits {
        data: vec![],
        position: 0,
    };
    let mut groups = Vec::new();
    let mut expected = Vec::new();
    for (group_index, first) in (0..count).step_by(1024).enumerate() {
        let offset = if group_index == 0 {
            4
        } else {
            bits.position.div_ceil(8) as u32
        };
        let mut ring = [0u64; 128];
        let mut previous = if width == 32 {
            0xffc00123
        } else {
            0xfff8000000000123
        };
        ring[0] = previous;
        bits.write(previous, width);
        expected.push(value(previous, width));
        let mut flags = Vec::new();
        let mut leading_codes = Vec::new();
        let mut leading = 0;
        let mut packed = Vec::new();
        for i in 1..1024.min(count - first) {
            let random = (i as u64).wrapping_mul(0xcf24619357892173);
            let flag = match i % 4 {
                0 => 0,
                1 => 3,
                2 => 2,
                _ => 1,
            };
            flags.push(flag);
            let v = match flag {
                0 => {
                    bits.write(((i - 1) % 128) as u64, 7);
                    previous
                }
                1 => {
                    let index = (i / 2) % 128;
                    let code = i % 8;
                    leading = ZEROS[code];
                    let n = 7.min(width - leading);
                    let residual = random & ((1 << n) - 1);
                    bits.write(residual, n);
                    packed.extend(
                        ((index as u16) << 9 | (code as u16) << 6 | n as u16).to_le_bytes(),
                    );
                    (residual << (width - leading - n)) ^ ring[index]
                }
                _ => {
                    if flag == 3 {
                        let code = i % 8;
                        leading_codes.push(code);
                        leading = ZEROS[code];
                    }
                    let n = width - leading;
                    let residual = if n == 64 {
                        random
                    } else {
                        random & ((1 << n) - 1)
                    };
                    bits.write(residual, n);
                    residual ^ previous
                }
            };
            previous = v;
            ring[i % 128] = v;
            expected.push(value(v, width));
        }
        let mut flag_bytes = vec![0; flags.len().div_ceil(4)];
        for (i, flag) in flags.into_iter().enumerate() {
            flag_bytes[i / 4] |= flag << (6 - 2 * (i % 4));
        }
        let mut leading_bytes = Vec::new();
        for codes in leading_codes.chunks(8) {
            let word = codes
                .iter()
                .enumerate()
                .fold(0u32, |word, (i, code)| word | (*code as u32) << (3 * i));
            leading_bytes.extend(&word.to_le_bytes()[..3]);
        }
        let tail = flag_bytes.len() + leading_bytes.len() + 5;
        if tail % 2 != 0 {
            packed.push(0);
        }
        packed.extend(flag_bytes);
        packed.extend(&leading_bytes);
        packed.push((leading_bytes.len() / 3) as u8);
        packed.extend(offset.to_le_bytes());
        groups.push(packed);
    }
    let mut data = vec![0; 4];
    data.extend(bits.data);
    while !data.len().is_multiple_of(8) {
        data.push(0);
    }
    for group in groups.into_iter().rev() {
        data.extend(group);
    }
    let end = data.len() as u32;
    data[..4].copy_from_slice(&end.to_le_bytes());
    (data, expected)
}

#[test]
fn legacy_xor_codecs_preserve_bits_references_and_unaligned_group_boundaries() -> Result<()> {
    let registry = compression::decoders();
    for width in [32, 64] {
        for count in [1, 2, 3, 31, 32, 33, 1023, 1024, 1025, 2057] {
            for (id, (data, expected)) in [(8, chimp(width, count)), (9, patas(width, count))] {
                same_bits(
                    &read(
                        &registry,
                        id,
                        &data,
                        count,
                        SegmentType::Values(&type_for(width)),
                        &QueryContext::background(),
                    )?,
                    &expected,
                );
            }
        }
    }
    Ok(())
}

#[test]
fn floating_decoders_reject_truncated_or_invalid_references_and_metadata() -> Result<()> {
    let registry = compression::decoders();
    for width in [32, 64] {
        let data_type = type_for(width);
        for (id, (data, _)) in [
            (8, chimp(width, 33)),
            (9, patas(width, 33)),
            (11, alprd(width, width - 16, 3)),
        ] {
            for end in 0..data.len() {
                assert!(
                    read(
                        &registry,
                        id,
                        &data[..end],
                        33,
                        SegmentType::Values(&data_type),
                        &QueryContext::background()
                    )
                    .is_err(),
                    "codec {id} prefix {end}"
                );
            }
            for index in 0..data.len() {
                for byte in [0, 1, 127, 255] {
                    let mut bad = data.clone();
                    bad[index] = byte;
                    if let Ok(values) = read(
                        &registry,
                        id,
                        &bad,
                        33,
                        SegmentType::Values(&data_type),
                        &QueryContext::background(),
                    ) {
                        assert_eq!(values.len(), 33);
                    }
                }
            }
            assert!(matches!(
                read(
                    &registry,
                    id,
                    &data,
                    33,
                    SegmentType::Validity,
                    &QueryContext::background()
                ),
                Err(Error::Unsupported(_))
            ));
        }
        let (mut data, _) = patas(width, 33);
        let metadata_start = data.len() - 4 - 33 * 2;
        data[metadata_start + 1] |= 2; // First row illegally refers to a prior row.
        assert!(matches!(
            read(
                &registry,
                9,
                &data,
                33,
                SegmentType::Values(&data_type),
                &QueryContext::background()
            ),
            Err(Error::Corrupt(_))
        ));
        let (mut data, _) = alprd(width, width - 16, 3);
        data[4] = width as u8; // Shift equal to the physical width is invalid.
        assert!(matches!(
            read(
                &registry,
                11,
                &data,
                33,
                SegmentType::Values(&data_type),
                &QueryContext::background()
            ),
            Err(Error::Corrupt(_))
        ));
    }
    Ok(())
}

#[test]
fn alp_single_precision_uses_its_own_rounding_and_exception_width() -> Result<()> {
    let registry = compression::decoders();
    for exponent in 0..=10u8 {
        for factor in 0..=exponent {
            for width in [0, 7, 31, 63] {
                let mask = (1u128 << width) - 1;
                let integers: Vec<_> = (0..33).map(|i| i as u128 & mask).collect();
                let frame = (-73i64) as u64;
                let mut data = vec![0; 4];
                data.extend([exponent, factor]);
                data.extend(2u16.to_le_bytes());
                data.extend(frame.to_le_bytes());
                data.push(width as u8);
                data.extend(pack(&integers, width));
                data.extend(0xff800123u32.to_le_bytes());
                data.extend(0x80000000u32.to_le_bytes());
                data.extend(0u16.to_le_bytes());
                data.extend(32u16.to_le_bytes());
                data.extend(4u32.to_le_bytes());
                let end = data.len() as u32;
                data[..4].copy_from_slice(&end.to_le_bytes());
                let factor = format!("1e{factor}").parse::<f32>().unwrap();
                let fraction = format!("1e-{exponent}").parse::<f32>().unwrap();
                let mut expected: Vec<_> = integers
                    .iter()
                    .map(|i| {
                        Value::Float(
                            (*i as u64).wrapping_add(frame) as i64 as f32 * factor * fraction,
                        )
                    })
                    .collect();
                expected[0] = Value::Float(f32::from_bits(0xff800123));
                expected[32] = Value::Float(-0.0);
                same_bits(
                    &read(
                        &registry,
                        10,
                        &data,
                        33,
                        SegmentType::Values(&DataType::Float),
                        &QueryContext::background(),
                    )?,
                    &expected,
                );
                data[4] = 11;
                assert!(matches!(
                    read(
                        &registry,
                        10,
                        &data,
                        33,
                        SegmentType::Values(&DataType::Float),
                        &QueryContext::background()
                    ),
                    Err(Error::Corrupt(_))
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn chimp_rejects_float_width_bits_that_belong_only_to_double() -> Result<()> {
    let registry = compression::decoders();
    for width in [0u16, 32] {
        // Two values: a raw FLOAT followed by a trailing-XOR record whose
        // width field is invalid for FLOAT, with enough bytes to hide overread.
        let mut data = vec![0; 16];
        data.extend(width.to_le_bytes());
        data.extend([0x40, 0]); // trailing-XOR flag; zero leading-zero blocks
        data.extend(4u32.to_le_bytes());
        let end = data.len() as u32;
        data[..4].copy_from_slice(&end.to_le_bytes());
        assert!(matches!(
            read(
                &registry,
                8,
                &data,
                2,
                SegmentType::Values(&DataType::Float),
                &QueryContext::background()
            ),
            Err(Error::Corrupt(_))
        ));
    }
    Ok(())
}
