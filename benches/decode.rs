//! デコード性能のベンチマーク。
//!
//! 計測対象は `docs/design.md` の「6.5 計測」に対応する:
//! ヘッダのみ / 全 activity / 単一 activity 選択 / 時刻フィルタ / 複数ファイル横断。
//! 実装の進行に合わせて各ベンチを追加する。

use criterion::{Criterion, criterion_group, criterion_main};

fn bench_header_only(c: &mut Criterion) {
    c.bench_function("header_only/placeholder", |b| {
        b.iter(|| std::hint::black_box(0u64))
    });
}

criterion_group!(benches, bench_header_only);
criterion_main!(benches);
