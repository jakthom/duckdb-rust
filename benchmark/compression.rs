//! Selectable native decoders, including the same checked boundary used by files.
use duckdb_rust::{
    DataType, Error, Result, Value,
    parallel::QueryContext,
    storage::{
        compression::{
            BlockSource, CodecId, DecodeContext, DecodeInput, DecoderRegistry, SegmentDecoder,
            SegmentStatistics, SegmentType,
        },
        duckdb::compression::{BitPackingDecoder, ScalarBitPackingDecoder},
    },
};
use serde_json::json;
use std::{sync::Arc, time::Instant};

struct NoBlocks;
impl BlockSource for NoBlocks {
    fn block(&self, _: u64) -> Result<&[u8]> {
        Err(Error::Internal("bitpacking has no external blocks".into()))
    }
}

fn segment(mode: u32, width: usize, size: usize) -> (Vec<u8>, Vec<Value>) {
    let mut data = vec![0; 8];
    let frame = -17i128;
    let delta = 3i128;
    data.extend(&frame.to_le_bytes()[..size]);
    if mode == 3 {
        data.extend(&delta.to_le_bytes()[..size]);
    }
    let mut expected = Vec::new();
    if mode >= 4 {
        data.extend(&(width as i128).to_le_bytes()[..size]);
        if mode == 4 {
            data.extend(&delta.to_le_bytes()[..size]);
        }
        let mut packed = vec![0; 256 * width];
        let mask = (1u128 << width) - 1;
        let mut previous = delta;
        for i in 0..2048 {
            let value = (i as u128).wrapping_mul(0xe53106b491267acf323348936275da1f) & mask;
            for bit in 0..width {
                packed[(i * width + bit) / 8] |=
                    (((value >> bit) & 1) as u8) << ((i * width + bit) % 8);
            }
            let value = (value as i128).wrapping_add(frame);
            let value = if mode == 4 {
                previous = previous.wrapping_add(value);
                previous
            } else {
                value
            };
            let shift = (16 - size) * 8;
            expected.push(Value::Integer((value << shift) >> shift));
        }
        data.extend(packed);
    } else {
        expected.extend(
            (0..2048).map(|i| Value::Integer(if mode == 2 { frame } else { frame + delta * i })),
        );
    }
    data.extend((mode << 24 | 8).to_le_bytes());
    let end = data.len() as u64;
    data[..8].copy_from_slice(&end.to_le_bytes());
    (data, expected)
}

pub fn run(rows: usize, iterations: usize) -> Result<serde_json::Value> {
    let mut results = Vec::new();
    let mut comparisons = Vec::new();
    // Fixed before measurement: the word implementation may regress at most
    // 25% against the scalar median in any covered case. This narrow selection
    // budget does not establish engine or end-to-end file performance parity.
    let budget = 1.25;
    for (mode, width, size) in [
        (2, 0, 8),
        (3, 0, 8),
        (5, 0, 8),
        (5, 1, 8),
        (5, 7, 8),
        (4, 9, 8),
        (5, 17, 8),
        (4, 31, 8),
        (5, 63, 8),
        (5, 127, 16),
    ] {
        let data_type = if size == 8 {
            DataType::BigInt
        } else {
            DataType::HugeInt
        };
        let (data, expected) = segment(mode, width, size);
        let statistics = SegmentStatistics {
            has_values: true,
            minimum: Value::Integer(-17),
        };
        let query = QueryContext::new(Default::default(), None, 2048, 2048)?;
        let context = DecodeContext {
            blocks: &NoBlocks,
            query: &query,
            vector_size: 2048,
        };
        let mut medians = Vec::new();
        let decoders: Vec<Arc<dyn SegmentDecoder>> = vec![
            Arc::new(ScalarBitPackingDecoder),
            Arc::new(BitPackingDecoder),
        ];
        let registries = decoders
            .into_iter()
            .map(|decoder| {
                let mut registry = DecoderRegistry::default();
                registry.register(decoder)?;
                Ok(registry)
            })
            .collect::<Result<Vec<_>>>()?;
        let mut samples = [Vec::new(), Vec::new()];
        for iteration in 0..iterations + 3 {
            for adapter in if iteration % 2 == 0 { [0, 1] } else { [1, 0] } {
                let start = Instant::now();
                let mut remaining = rows;
                while remaining != 0 {
                    let count = remaining.min(2048);
                    let values = registries[adapter].decode(
                        CodecId(6),
                        DecodeInput {
                            kind: SegmentType::Values(&data_type),
                            count,
                            data: std::hint::black_box(&data),
                            statistics: &statistics,
                        },
                        &context,
                    )?;
                    if values != expected[..count] {
                        return Err(Error::Execution(
                            "compression benchmark correctness failure".into(),
                        ));
                    }
                    remaining -= count;
                }
                if iteration >= 3 {
                    samples[adapter].push(start.elapsed().as_nanos());
                }
            }
        }
        for (registry, samples) in registries.iter().zip(samples) {
            let mut ordered = samples.clone();
            ordered.sort_unstable();
            let median = ordered[ordered.len() / 2];
            medians.push(median);
            results.push(json!({"mode": mode, "width": width, "type": data_type.to_string(), "adapters": registry.adapters(), "samples_ns": samples, "median_ns": median}));
        }
        let ratio = medians[1] as f64 / medians[0] as f64;
        comparisons.push(json!({"mode": mode, "width": width, "type": data_type.to_string(), "word_to_scalar_median": ratio, "within_budget": ratio <= budget}));
    }
    let budget_passed = comparisons.iter().all(|c| c["within_budget"] == true);
    Ok(
        json!({"suite": "compression", "rows": rows, "iterations": iterations, "warmups": 3, "order": "alternating adapters per iteration", "segment_rows": 2048, "max_intermediate_rows": 2048, "wire_codec": 6, "results": results, "correctness": "passed", "selection_budget": {"max_word_to_scalar_median": budget, "passed": budget_passed, "comparisons": comparisons}}),
    )
}
