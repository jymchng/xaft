# Benchmarking Strategy

## Performance Targets

| Metric | Target | Measurement method |
|---|---|---|
| Repository indexing (100K LOC) | < 5s | `criterion` benchmark |
| Time-to-first-token | < 500ms | Signal timestamp delta |
| TUI frame time (render) | < 25ms | `criterion` |
| Checkpoint save | < 50ms | Unit test timing |
| Session resume from checkpoint | < 2s | Integration test |
| File write (atomic) | < 5ms | Benchmark |
| Fuzzy search (10K files) | < 100ms | Benchmark |
| Patch apply (1000 hunks) | < 200ms | Benchmark |

## Criterion Benchmarks

```rust
// xaft-index/benches/indexing.rs
use criterion::{criterion_group, criterion_main, Criterion};

fn bench_index_build(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all().build().unwrap();

    c.bench_function("index_build_10k_lines", |b| {
        b.to_async(&rt).iter(|| async {
            let dir = generate_rust_files(100, 100); // 100 files × 100 lines
            let index = RepoIndex::build(&dir.path()).await.unwrap();
            criterion::black_box(index);
        });
    });
}

fn bench_symbol_search(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all().build().unwrap();

    let index = rt.block_on(async {
        let dir = generate_rust_files(1000, 100);
        RepoIndex::build(dir.path()).await.unwrap()
    });

    c.bench_function("symbol_search", |b| {
        b.to_async(&rt).iter(|| async {
            criterion::black_box(
                index.symbols.search("process_request", 10).await
            );
        });
    });
}
```

## SWE-Bench Evaluation

`xaft` targets 75%+ pass rate on SWE-bench Lite (300 GitHub issues):

```bash
# Run SWE-bench evaluation
XAFT_E2E_TESTS=1 XAFT_BENCH=swe_bench \
  cargo test --test swe_bench -- --nocapture 2>&1 | tee swe_bench_results.json
```

Reporting format:
```json
{
  "total": 300,
  "passed": 228,
  "failed": 72,
  "pass_rate": 0.76,
  "avg_cost_usd": 0.43,
  "avg_duration_secs": 187,
  "avg_turns": 14.2
}
```

## Cost Benchmarks

Track cost per task type:

| Task type | Avg cost (Claude 3.5 Sonnet) | Avg cost (Gemini Flash) |
|---|---|---|
| Single file edit | $0.02–0.05 | $0.002–0.008 |
| Multi-file refactor (5 files) | $0.08–0.20 | $0.010–0.030 |
| Test generation (10 tests) | $0.05–0.10 | $0.008–0.015 |
| Bug fix (with test failure) | $0.10–0.30 | $0.015–0.040 |
| Architecture migration | $0.30–1.00 | $0.050–0.150 |

## References

- Criterion: https://bheisler.github.io/criterion.rs/book/
- SWE-bench: https://www.swebench.com/
