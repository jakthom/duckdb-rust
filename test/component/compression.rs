use std::{
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use duckdb_rust::{
    DataType, DatabaseBuilder, Error, Result, Value,
    parallel::{InterruptHandle, QueryContext},
    storage::{
        checkpoint::FileCheckpoint,
        compression::{
            BlockSource, CodecId, DecodeContext, DecodeInput, DecoderRegistry, SegmentDecoder,
            SegmentStatistics, SegmentType,
        },
        duckdb::{
            DuckDbFormat,
            compression::{self, BitPackingDecoder, ScalarBitPackingDecoder},
        },
        filesystem::OpenMode,
    },
};

struct NoBlocks;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl BlockSource for NoBlocks {
    fn block(&self, _id: u64) -> Result<&[u8]> {
        Err(Error::Corrupt("unexpected external block".into()))
    }
}
const STATS: SegmentStatistics = SegmentStatistics {
    has_values: true,
    minimum: Value::Integer(0),
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn read(
    registry: &DecoderRegistry,
    id: u64,
    data: &[u8],
    count: usize,
    kind: SegmentType<'_>,
    query: &QueryContext,
) -> Result<Vec<Value>> {
    registry.decode(
        CodecId(id),
        DecodeInput {
            kind,
            count,
            data,
            statistics: &STATS,
        },
        &DecodeContext {
            blocks: &NoBlocks,
            query,
            vector_size: 2048,
        },
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn readers() -> Vec<Arc<dyn SegmentDecoder>> {
    vec![
        Arc::new(BitPackingDecoder),
        Arc::new(ScalarBitPackingDecoder),
    ]
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn pack(values: &[u128], width: usize) -> Vec<u8> {
    let mut output = vec![0; values.len().div_ceil(32) * 4 * width];
    for (i, value) in values.iter().enumerate() {
        for bit in 0..width {
            output[(i * width + bit) / 8] |=
                (((value >> bit) & 1) as u8) << ((i * width + bit) % 8);
        }
    }
    output
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn wrap(value: i128, size: usize) -> i128 {
    (value << ((16 - size) * 8)) >> ((16 - size) * 8)
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn segment(
    mode: u32,
    size: usize,
    frame: i128,
    width: usize,
    delta: i128,
    packed: &[u128],
) -> Vec<u8> {
    let mut data = vec![0; 8];
    data.extend(&frame.to_le_bytes()[..size]);
    match mode {
        3 => data.extend(&delta.to_le_bytes()[..size]),
        4 | 5 => {
            data.extend(&(width as i128).to_le_bytes()[..size]);
            if mode == 4 {
                data.extend(&delta.to_le_bytes()[..size]);
            }
            data.extend(pack(packed, width));
        }
        _ => (),
    }
    data.extend((mode << 24 | 8).to_le_bytes());
    let end = data.len() as u64;
    data[..8].copy_from_slice(&end.to_le_bytes());
    data
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn both_bitpacking_adapters_obey_integer_and_date_width_wrap_and_tail_contracts() -> Result<()> {
    for decoder in readers() {
        let mut registry = DecoderRegistry::default();
        registry.register(decoder.clone())?;
        for (data_type, size) in [
            (DataType::TinyInt, 1),
            (DataType::SmallInt, 2),
            (DataType::Integer, 4),
            (DataType::Date, 4),
            (DataType::BigInt, 8),
            (DataType::HugeInt, 16),
        ] {
            for count in [1, 31, 32, 33, 2048] {
                for width in 0..=size * 8 {
                    let mask = if width == 128 {
                        u128::MAX
                    } else {
                        (1u128 << width) - 1
                    };
                    let packed: Vec<_> = (0..count)
                        .map(|i| {
                            (i as u128).wrapping_mul(0xef5a271a53981dbbedef784a77629841) & mask
                        })
                        .collect();
                    for mode in [2, 3, 4, 5] {
                        let frame = wrap(-71, size);
                        let delta = wrap(113, size);
                        let data = segment(mode, size, frame, width, delta, &packed);
                        let mut previous = delta;
                        let expected: Vec<_> = packed
                            .iter()
                            .enumerate()
                            .map(|(i, packed)| {
                                let value = match mode {
                                    2 => frame,
                                    3 => wrap(
                                        frame.wrapping_add(delta.wrapping_mul(i as i128)),
                                        size,
                                    ),
                                    4 => {
                                        previous = wrap(
                                            previous.wrapping_add(
                                                (*packed as i128).wrapping_add(frame),
                                            ),
                                            size,
                                        );
                                        previous
                                    }
                                    _ => wrap((*packed as i128).wrapping_add(frame), size),
                                };
                                if data_type == DataType::Date {
                                    if value == i128::from(i32::MIN) {
                                        Value::Null
                                    } else {
                                        Value::Date(
                                            duckdb_rust::Date::from_days(value as i32).unwrap(),
                                        )
                                    }
                                } else {
                                    Value::Integer(value)
                                }
                            })
                            .collect();
                        assert_eq!(
                            read(
                                &registry,
                                6,
                                &data,
                                count,
                                SegmentType::Values(&data_type),
                                &QueryContext::background()
                            )?,
                            expected,
                            "{} {data_type} width {width} count {count} mode {mode}",
                            decoder.name()
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn replacing_a_decoder_changes_file_composition_without_changing_query_callers() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut data = Vec::new();
    flate2::read::GzDecoder::new(&include_bytes!("../data/duckdb/bitpacking.duckdb.gz")[..])
        .read_to_end(&mut data)?;
    for decoder in readers() {
        let path = directory.path().join(decoder.name());
        std::fs::write(&path, &data)?;
        let mut registry = compression::decoders();
        registry.replace(decoder.clone())?;
        let database = DatabaseBuilder::new()
            .durability(Arc::new(FileCheckpoint::open(
                &path,
                OpenMode::ReadWrite,
                Arc::new(DuckDbFormat::new(registry)),
            )?))
            .build()?;
        assert!(
            database
                .adapters()
                .contains(&("segment-decoder", decoder.name()))
        );
        let mut connection = database.connect();
        assert_eq!(
            connection
                .query("SELECT count(*), sum(id), sum(k) FROM t")?
                .rows,
            vec![vec![
                Value::Integer(10000),
                Value::Integer(49995000),
                Value::Integer(29994)
            ]]
        );
        connection.execute("INSERT INTO t VALUES (10000,4)")?;
        drop(connection);
        drop(database);
        assert_eq!(
            duckdb_rust::Database::open_read_only(&path)?
                .connect()
                .query("SELECT count(*) FROM t")?
                .rows,
            vec![vec![Value::Integer(10001)]]
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn alp_group(
    count: usize,
    width: usize,
    exponent: u8,
    factor: u8,
    frame: u64,
    exceptions: &[(u16, u64)],
) -> (Vec<u8>, Vec<Value>) {
    let packed: Vec<_> = (0..count)
        .map(|i| {
            if width == 0 {
                0
            } else if width == 64 {
                u64::MAX as u128 - i as u128
            } else {
                i as u128 & ((1u128 << width) - 1)
            }
        })
        .collect();
    let mut data = vec![0; 4];
    data.extend([exponent, factor]);
    data.extend((exceptions.len() as u16).to_le_bytes());
    data.extend(frame.to_le_bytes());
    data.push(width as u8);
    data.extend(pack(&packed, width));
    for (_, bits) in exceptions {
        data.extend(bits.to_le_bytes());
    }
    for (position, _) in exceptions {
        data.extend(position.to_le_bytes());
    }
    data.extend(4u32.to_le_bytes());
    let len = data.len() as u32;
    data[..4].copy_from_slice(&len.to_le_bytes());
    let f = format!("1e{factor}").parse::<f64>().unwrap();
    let e = format!("1e-{exponent}").parse::<f64>().unwrap();
    let mut expected: Vec<_> = packed
        .iter()
        .map(|v| Value::Double((*v as u64).wrapping_add(frame) as i64 as f64 * f * e))
        .collect();
    for (position, bits) in exceptions {
        expected[*position as usize] = Value::Double(f64::from_bits(*bits));
    }
    (data, expected)
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn equal_bits(actual: &[Value], expected: &[Value]) {
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(
            actual.as_f64().unwrap().to_bits(),
            expected.as_f64().unwrap().to_bits()
        );
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn alp_retains_exception_bits_and_decimal_scaling_in_partial_groups() -> Result<()> {
    let registry = compression::decoders();
    for exponent in 0..=18 {
        for factor in 0..=exponent {
            for width in [0, 1, 3, 7, 8, 9, 31, 32, 33, 63, 64] {
                let exceptions = [
                    (0, (-0.0f64).to_bits()),
                    (1, 0xfff8000000000123),
                    (32, f64::INFINITY.to_bits()),
                ];
                let (data, expected) =
                    alp_group(33, width, exponent, factor, (-73i64) as u64, &exceptions);
                equal_bits(
                    &read(
                        &registry,
                        10,
                        &data,
                        33,
                        SegmentType::Values(&DataType::Double),
                        &QueryContext::background(),
                    )?,
                    &expected,
                );
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn malformed_native_segments_are_rejected_without_panics_or_partial_output() -> Result<()> {
    let registry = compression::decoders();
    let (alp, _) = alp_group(
        33,
        9,
        3,
        2,
        19,
        &[(1, f64::NAN.to_bits()), (32, (-0.0f64).to_bits())],
    );
    let bitpacking = segment(5, 8, 0, 9, 0, &[1; 33]);
    for (id, data, data_type) in [
        (10, alp, DataType::Double),
        (6, bitpacking, DataType::BigInt),
    ] {
        for length in 0..data.len() {
            assert!(
                read(
                    &registry,
                    id,
                    &data[..length],
                    33,
                    SegmentType::Values(&data_type),
                    &QueryContext::background()
                )
                .is_err(),
                "codec {id}, length {length}"
            );
        }
        for index in 0..data.len() {
            for value in [0, 1, 127, 255] {
                let mut bad = data.clone();
                bad[index] = value;
                // Some mutations are valid encodings of different values.
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
        if id == 10 {
            for (offset, value) in [
                (4, 19),
                (5, 4),
                (6, 34),
                (16, 65),
                (data.len() - 8, 33),
                (data.len() - 6, 1),
            ] {
                let mut bad = data.clone();
                bad[offset] = value;
                assert!(
                    matches!(
                        read(
                            &registry,
                            id,
                            &bad,
                            33,
                            SegmentType::Values(&data_type),
                            &QueryContext::background()
                        ),
                        Err(Error::Corrupt(_))
                    ),
                    "ALP field {offset}"
                );
            }
        }
    }
    Ok(())
}

struct InvalidDecoder {
    output: Vec<Value>,
    calls: Arc<AtomicUsize>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SegmentDecoder for InvalidDecoder {
    fn id(&self) -> CodecId {
        CodecId(6)
    }
    fn name(&self) -> &'static str {
        "invalid-test-decoder"
    }
    fn supports(&self, _: SegmentType<'_>) -> bool {
        true
    }
    fn decode(&self, _: DecodeInput<'_>, _: &DecodeContext<'_>) -> Result<Vec<Value>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(self.output.clone())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn registry_validates_selection_resource_limits_cancellation_and_foreign_output() -> Result<()> {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = compression::decoders();
    assert!(registry.register(Arc::new(BitPackingDecoder)).is_err());
    assert!(
        DecoderRegistry::default()
            .replace(Arc::new(BitPackingDecoder))
            .is_err()
    );
    for output in [
        vec![],
        vec![Value::Varchar("1".into())],
        vec![Value::Integer(128)],
    ] {
        registry.replace(Arc::new(InvalidDecoder {
            output,
            calls: calls.clone(),
        }))?;
        assert!(matches!(
            read(
                &registry,
                6,
                &[],
                1,
                SegmentType::Values(&DataType::TinyInt),
                &QueryContext::background()
            ),
            Err(Error::Internal(_))
        ));
    }
    assert!(matches!(
        read(
            &registry,
            600,
            &[],
            1,
            SegmentType::Validity,
            &QueryContext::background()
        ),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        read(
            &registry,
            10,
            &[],
            1,
            SegmentType::Values(&DataType::Integer),
            &QueryContext::background()
        ),
        Err(Error::Unsupported(_))
    ));
    registry.replace(Arc::new(InvalidDecoder {
        output: vec![Value::Null],
        calls: calls.clone(),
    }))?;
    assert!(matches!(
        read(
            &registry,
            6,
            &[],
            1,
            SegmentType::Validity,
            &QueryContext::background()
        ),
        Err(Error::Internal(_))
    ));
    let interrupt = InterruptHandle::default();
    let query = QueryContext::new(interrupt.clone(), None, 8, 2)?;
    calls.store(0, Ordering::Relaxed);
    assert!(matches!(
        read(&registry, 6, &[], 3, SegmentType::Validity, &query),
        Err(Error::Resource(_))
    ));
    interrupt.interrupt();
    assert!(matches!(
        read(&registry, 6, &[], 1, SegmentType::Validity, &query),
        Err(Error::Interrupted)
    ));
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert!(matches!(
        read(
            &registry,
            6,
            &[],
            usize::MAX,
            SegmentType::Validity,
            &QueryContext::background()
        ),
        Err(Error::Resource(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn shared_decoders_keep_concurrent_requests_independent() -> Result<()> {
    for decoder in readers() {
        let mut registry = DecoderRegistry::default();
        registry.register(decoder)?;
        let registry = Arc::new(registry);
        let mut threads = Vec::new();
        for i in 0..8 {
            let registry = registry.clone();
            threads.push(std::thread::spawn(move || -> Result<()> {
                let data = segment(5, 8, i, 9, 0, &[31; 33]);
                let query = QueryContext::background();
                assert_eq!(
                    read(
                        &registry,
                        6,
                        &data,
                        33,
                        SegmentType::Values(&DataType::BigInt),
                        &query
                    )?,
                    vec![Value::Integer(i + 31); 33]
                );
                let interrupt = InterruptHandle::default();
                let query = QueryContext::new(interrupt.clone(), None, 8, 2)?;
                assert!(matches!(
                    read(
                        &registry,
                        6,
                        &data,
                        33,
                        SegmentType::Values(&DataType::BigInt),
                        &query
                    ),
                    Err(Error::Resource(_))
                ));
                interrupt.interrupt();
                assert!(matches!(
                    read(
                        &registry,
                        6,
                        &data,
                        1,
                        SegmentType::Values(&DataType::BigInt),
                        &query
                    ),
                    Err(Error::Interrupted)
                ));
                assert!(matches!(
                    read(
                        &registry,
                        6,
                        &data[..8],
                        33,
                        SegmentType::Values(&DataType::BigInt),
                        &QueryContext::background()
                    ),
                    Err(Error::Corrupt(_))
                ));
                Ok(())
            }));
        }
        for thread in threads {
            thread.join().expect("decoder thread panicked")?;
        }
    }
    Ok(())
}

#[path = "compression/floating.rs"]
mod floating;

#[path = "compression/dict_fsst.rs"]
mod dict_fsst;
