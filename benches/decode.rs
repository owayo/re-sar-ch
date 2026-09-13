//! デコード性能のベンチマーク。
//!
//! `docs/design.md` の「6.5 計測」に対応する。
//!
//! ```sh
//! RESARCH_BENCH_FILE=/path/to/sa01 cargo bench
//! ```
//!
//! 環境変数が未設定のときは、本家 fixture (`make fixtures` で取得) のうち
//! 最も大きいものを使う。それも無ければ計測をスキップする。
//!
//! 最適化を入れるときは**必ずこのベンチの差分を添える**こと。
//! 「速くなったつもり」を排除するための仕組みである。
//!
//! # 対象が変わったら別の系列になる
//!
//! ベンチ名に**対象ファイル名とバイト数**を含めている。
//! criterion の保存済みベースラインはベンチ名だけで引かれるため、
//! 対象が変わったまま同じ名前で測ると `change:` が「別のファイルとの比較」に
//! なってしまう (fixture を 1 本足して `bench_target()` の選択が変わり、
//! 38 KB と 979 KB の測定が同じ名前で比較された実例がある)。
//!
//! `docs/design.md` 6.5 の実測値は **979 KB の実データ**に対するもので、
//! `make fixtures` の既定対象 (`data-11.6.5`、38 KB) とは別の系列である。
//! 表を更新するときはどちらの対象かを必ず書くこと。

use std::hint::black_box;
use std::path::PathBuf;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use re_sar_ch::format::file::ScanControl;
use re_sar_ch::format::{MmapPolicy, OpenOptions, SaFile};
use re_sar_ch::model::ActivityId;
use re_sar_ch::series::{RecordRange, Selection, WalkItem, walk_items, walk_items_in};

/// 計測対象のファイルを決める。
fn bench_target() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RESARCH_BENCH_FILE") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
        eprintln!("RESARCH_BENCH_FILE が指すファイルが無い: {}", p.display());
        return None;
    }

    // 本家 fixture のうち最大のものを使う (make fixtures で取得済みなら)
    let dir = PathBuf::from("target/fixtures/upstream");
    let mut best: Option<(u64, PathBuf)> = None;
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let path = entry.path();
        let name = path.file_name()?.to_string_lossy().to_string();
        // 異常系データは計測に使わない
        if !name.starts_with("data-") || name.contains("err") || name.contains("trunc") {
            continue;
        }
        // `data-*.xml` のような付随データが混ざるので、読めるものだけを候補にする
        if SaFile::open(&path).is_err() {
            continue;
        }
        let len = entry.metadata().ok()?.len();
        if best.as_ref().is_none_or(|(b, _)| len > *b) {
            best = Some((len, path));
        }
    }
    best.map(|(_, p)| p)
}

fn bench_decode(c: &mut Criterion) {
    let Some(path) = bench_target() else {
        eprintln!(
            "計測対象が無いためスキップします。\n\
             `make fixtures` を実行するか RESARCH_BENCH_FILE を指定してください。"
        );
        return;
    };
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    eprintln!("計測対象: {} ({} バイト)", path.display(), size);

    // **グループ名に計測対象を入れる。**
    //
    // criterion の保存済みベースラインはベンチ名だけで引かれるので、
    // 対象ファイルが変わったまま同じ名前で測ると `change:` が
    // 「別のファイルとの比較」になり、意味のない改善率が出る
    // (fixture を 1 本足しただけで `bench_target()` の選択が変わり、
    // 38 KB と 979 KB の測定が同じ名前で比較されていた)。
    //
    // ファイル名とバイト数を名前に含めれば、対象が変われば別の系列になる。
    let label = format!(
        "decode/{}-{size}B",
        path.file_name().unwrap_or_default().to_string_lossy()
    );
    let mut group = c.benchmark_group(label);
    group.throughput(Throughput::Bytes(size));

    // ファイルあたりの固定コスト (ヘッダ解析のみ)
    // 既定 (Auto: サイズ閾値で選択)
    group.bench_function("open_header_only", |b| {
        b.iter(|| {
            let f = SaFile::open(&path).expect("open");
            black_box(f.activities().len())
        })
    });

    // mmap を強制した場合との比較
    group.bench_function("open_header_only_mmap_always", |b| {
        b.iter(|| {
            let f = SaFile::open_with(
                &path,
                OpenOptions {
                    mmap: MmapPolicy::Always,
                    ..Default::default()
                },
            )
            .expect("open");
            black_box(f.activities().len())
        })
    });

    // read を強制した場合との比較
    group.bench_function("open_header_only_no_mmap", |b| {
        b.iter(|| {
            let f = SaFile::open_with(
                &path,
                OpenOptions {
                    mmap: MmapPolicy::Never,
                    ..Default::default()
                },
            )
            .expect("open");
            black_box(f.activities().len())
        })
    });

    let file = SaFile::open(&path).expect("open");

    // レコード境界の走査のみ (統計をデコードしない)
    group.bench_function("scan_boundaries_only", |b| {
        b.iter(|| {
            let s = file
                .scan(|rec| {
                    black_box(rec.slices.len());
                    Ok(ScanControl::Continue)
                })
                .expect("scan");
            black_box(s.total_records())
        })
    });

    // 全 activity をデコード
    group.bench_function("walk_all_activities", |b| {
        b.iter(|| {
            let s = walk_items(&file, &Selection::All, |item| {
                if let WalkItem::Sample(view) = item {
                    black_box(view.curr.activities.len());
                }
                Ok(ScanControl::Continue)
            })
            .expect("walk");
            black_box(s.total_records())
        })
    });

    // CPU だけを選択 (選択外 activity をスキップする最適化の効果)
    group.bench_function("walk_cpu_only", |b| {
        b.iter(|| {
            let s = walk_items(&file, &Selection::Only(vec![ActivityId::CPU]), |item| {
                if let WalkItem::Sample(view) = item {
                    black_box(view.curr.activities.len());
                }
                Ok(ScanControl::Continue)
            })
            .expect("walk");
            black_box(s.total_records())
        })
    });

    // 走査範囲を後半だけに限る (区間外をデコードしない効果)
    //
    // `sar` 互換出力は「区間 × activity」で何度も走査する。範囲外の
    // レコードまでデコードすると RESTART の個数に比例して無駄が増えるので、
    // `walk_items_in` が本当にデコードを飛ばしているかをここで見る。
    // 後半だけなら全件のおよそ半分の時間になるのが期待値。
    let total = file
        .scan(|_| Ok(ScanControl::Continue))
        .map(|s| s.total_records())
        .unwrap_or(0);
    let half = RecordRange::new(total as usize / 2, usize::MAX);
    group.bench_function("walk_all_activities_second_half", |b| {
        b.iter(|| {
            let s = walk_items_in(&file, &Selection::All, half, |item| {
                if let WalkItem::Sample(view) = item {
                    black_box(view.curr.activities.len());
                }
                Ok(ScanControl::Continue)
            })
            .expect("walk");
            black_box(s.total_records())
        })
    });

    group.finish();
}

criterion_group!(benches, bench_decode);
criterion_main!(benches);
