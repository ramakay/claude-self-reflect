use crate::search::SearchEngine;
use std::time::{Duration, Instant};

/// Limit-1 diagnostics must stay index lookups at realistic dimensions and
/// population. A full scan over these vectors exceeds the hook's probe budget.
#[test]
fn below_floor_probes_stay_under_five_ms_at_p95() {
    const COUNT: usize = 4096;
    const DIM: usize = 384;
    let mut engine = SearchEngine::new(COUNT);
    let mut state = 0x1234_5678_u32;
    for i in 0..COUNT {
        let mut vector: Vec<f32> = (0..DIM)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as f32 / u32::MAX as f32 - 0.5
            })
            .collect();
        let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        for x in &mut vector {
            *x /= norm;
        }
        engine.insert_chunk(format!("chunk-{i}"), vector.clone());
        assert!(engine.insert_reflection(format!("reflection-{i}"), vector));
    }
    assert_eq!(engine.chunk_count(), COUNT);
    assert_eq!(engine.reflection_count(), COUNT);
    let mut query = vec![0.0; DIM];
    query[0] = 1.0;
    for reflection in [false, true] {
        let search = || {
            if reflection {
                engine.search_reflections(&query, 1, -1.0)
            } else {
                engine.search_chunks(&query, 1, -1.0)
            }
        };
        assert_eq!(search().len(), 1); // warm the index before measuring

        // Scheduler noise only makes samples slower, and a scan is slow in every round: the
        // bound holds if any one of up to three rounds of 50 samples has its p95 under it.
        let mut best = Duration::MAX;
        for _round in 0..3 {
            let mut times = Vec::with_capacity(50);
            for _ in 0..50 {
                let started = Instant::now();
                let hits = std::hint::black_box(search());
                times.push(started.elapsed());
                assert_eq!(hits.len(), 1);
            }
            times.sort_unstable();
            let (median, p95) = (times[25], times[47]);
            eprintln!(
                "limit-1 reflection={reflection}: median={}us p95={}us",
                median.as_micros(),
                p95.as_micros()
            );
            best = best.min(p95);
            if best < Duration::from_millis(5) {
                break;
            }
        }
        assert!(
            best < Duration::from_millis(5),
            "reflection={reflection}: best p95={best:?}"
        );
    }
}
