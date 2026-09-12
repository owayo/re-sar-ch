//! レイアウト適合テスト。
//!
//! 自作 fixture のバイト列 (`tests/fixtures/`) と、`src/format/` のレイアウト解決結果を
//! 突合する。fixture 側のオフセットは `docs/format/01-file-format.md` §3 の表から
//! **独立に書き下ろした**定数であり、本体のレイアウト記述からは導出していない。
//! これが `docs/design.md` §8 の「同じ誤りが往復して通る」を避ける仕組みである。
//!
//! 検証する軸:
//!
//! 1. 6 レイアウトバリアント × 4 ABI で、`file_magic` / `file_header` / `file_activity` /
//!    `record_header` のサイズと主要フィールドのオフセットが一致すること
//! 2. `unsigned long` のスロット幅 8 バイト・有効 4 バイト (32bit ライタ) の扱いが
//!    LE / BE の両方で正しいこと
//! 3. 同じ値を LE / BE で表現した fixture から、同じ正規化結果が得られること
//!    (メタモルフィックテスト)
//! 4. レコード列を走査するとファイル末尾にぴったり到達すること
//! 5. 異常系 fixture が、本家 `data-12.6.0-*` と同じバイト位置の 1 フィールドだけを
//!    壊したものになっていること

mod fixtures;

use fixtures::{ActivitySpec, Corruption, Fixture, FixtureAbi, GenFacts, Generation, RecordKind};

use re_sar_ch::format::abi::{Endian, LayoutAbi, SourceEncoding};
use re_sar_ch::format::reader::Cursor;
use re_sar_ch::format::wire::{FieldTy, ResolvedLayout, WireLayout};
use re_sar_ch::format::{layouts, selfdesc};

// ===========================================================================
// fixture の ABI → 本体の SourceEncoding
// ===========================================================================

/// fixture の ABI 指定を本体の [`SourceEncoding`] へ写す。
///
/// ABI の推定は fixture が書いた `sa_machine` / `sa_sizeof_long` からしか行わない
/// (実行ホストの `size_of::<c_ulong>()` などは使わない)。
fn encoding(f: &Fixture) -> SourceEncoding {
    let endian = if f.abi().is_big_endian() {
        Endian::Big
    } else {
        Endian::Little
    };
    let abi = LayoutAbi::infer(f.abi().machine(), f.abi().long_bytes() as u8)
        .unwrap_or_else(|| panic!("{}: ABI を推定できない", f.label()));
    assert_eq!(
        abi.long_bytes as usize,
        f.abi().long_bytes(),
        "{}: 推定した long 幅が fixture と食い違う",
        f.label()
    );
    SourceEncoding::new(endian, abi)
}

/// fixture のバイト列を、そのファイルのバイト順で読むためのカーソル。
fn cursor(f: &Fixture) -> Cursor<'_> {
    let endian = if f.abi().is_big_endian() {
        Endian::Big
    } else {
        Endian::Little
    };
    Cursor::new(&f.bytes, endian)
}

// ===========================================================================
// 世代 → 本体のレイアウト定義
// ===========================================================================

fn resolve_file_magic(generation: Generation, enc: &SourceEncoding) -> ResolvedLayout {
    let layout: WireLayout = match generation {
        Generation::G2170 | Generation::G2171 => layouts::FILE_MAGIC_G1,
        Generation::G2173 => layouts::FILE_MAGIC_G2,
        Generation::G2175V120 | Generation::G2175V1217 | Generation::G2175Current => {
            layouts::FILE_MAGIC_G3
        }
    };
    layout.resolve(enc).expect("file_magic の解決に失敗")
}

/// `file_header` を解決する。
///
/// 自己記述世代では**固定定義を使わず**、fixture が申告する `hdr_types_nr` と
/// `header_size` から `selfdesc` に組み立てさせる。これが本番と同じ経路である。
fn resolve_file_header(
    generation: Generation,
    facts: &GenFacts,
    enc: &SourceEncoding,
) -> ResolvedLayout {
    match generation {
        Generation::G2170 | Generation::G2171 => layouts::FILE_HEADER_G1
            .resolve(enc)
            .expect("file_header@2171 の解決に失敗"),
        Generation::G2173 => layouts::FILE_HEADER_G2
            .resolve(enc)
            .expect("file_header@2173 の解決に失敗"),
        _ => {
            let t = facts
                .hdr_types_nr
                .expect("自己記述世代は hdr_types_nr を持つ");
            selfdesc::resolve_file_header(selfdesc::TypesNr(t), facts.file_header_size, enc)
                .expect("file_header@2175 の解決に失敗")
        }
    }
}

fn resolve_file_activity(
    generation: Generation,
    facts: &GenFacts,
    enc: &SourceEncoding,
) -> ResolvedLayout {
    match generation {
        Generation::G2170 => layouts::FILE_ACTIVITY_G0
            .resolve(enc)
            .expect("file_activity@2170 の解決に失敗"),
        Generation::G2171 | Generation::G2173 => layouts::FILE_ACTIVITY_G1
            .resolve(enc)
            .expect("file_activity@2171 の解決に失敗"),
        _ => {
            let t = facts
                .act_types_nr
                .expect("自己記述世代は act_types_nr を持つ");
            selfdesc::resolve_file_activity(selfdesc::TypesNr(t), enc)
                .expect("file_activity@2175 の解決に失敗")
        }
    }
}

fn resolve_record_header(
    generation: Generation,
    facts: &GenFacts,
    enc: &SourceEncoding,
) -> ResolvedLayout {
    match generation {
        Generation::G2170 | Generation::G2171 | Generation::G2173 => layouts::RECORD_HEADER_G1
            .resolve(enc)
            .expect("record_header@2171 の解決に失敗"),
        _ => {
            let t = facts
                .rec_types_nr
                .expect("自己記述世代は rec_types_nr を持つ");
            selfdesc::resolve_record_header(selfdesc::TypesNr(t), enc)
                .expect("record_header@2175 の解決に失敗")
        }
    }
}

/// 解決結果を fixture 側の独立した期待値と突合する。
fn assert_layout(
    label: &str,
    what: &str,
    resolved: &ResolvedLayout,
    expected_size: usize,
    expected_offsets: &[(&str, usize)],
) {
    assert_eq!(
        resolved.size, expected_size,
        "{label}: {what} のサイズが fixture の期待値と違う"
    );
    for (name, want) in expected_offsets {
        let field = resolved
            .field(name)
            .unwrap_or_else(|| panic!("{label}: {what} に {name} が無い"));
        assert_eq!(
            field.offset, *want,
            "{label}: {what}.{name} のオフセットが違う"
        );
    }
    // 期待値に挙げたフィールド以外が「間に」割り込んでいないことも見る。
    // 最後のフィールドの終端が申告サイズを超えていたら配置がおかしい。
    let last_end = resolved
        .fields
        .iter()
        .map(|f| f.offset + f.width)
        .max()
        .unwrap_or(0);
    assert!(
        last_end <= expected_size,
        "{label}: {what} の最終フィールド終端 {last_end} が申告サイズ {expected_size} を超えている"
    );
}

// ===========================================================================
// 1. 6 レイアウトバリアント × 4 ABI の配置突合
// ===========================================================================

#[test]
fn file_magic_layout_matches_fixture_for_every_generation_and_abi() {
    for f in fixtures::all_minimal() {
        let enc = encoding(&f);
        let facts = f.facts();
        let r = resolve_file_magic(f.generation(), &enc);
        assert_layout(
            &f.label(),
            "file_magic",
            &r,
            facts.file_magic_size,
            facts.file_magic_offsets,
        );
    }
}

#[test]
fn file_header_layout_matches_fixture_for_every_generation_and_abi() {
    for f in fixtures::all_minimal() {
        let enc = encoding(&f);
        let facts = f.facts();
        let r = resolve_file_header(f.generation(), facts, &enc);
        assert_layout(
            &f.label(),
            "file_header",
            &r,
            facts.file_header_size,
            facts.file_header_offsets,
        );
    }
}

#[test]
fn file_activity_layout_matches_fixture_for_every_generation_and_abi() {
    for f in fixtures::all_minimal() {
        let enc = encoding(&f);
        let facts = f.facts();
        let r = resolve_file_activity(f.generation(), facts, &enc);
        assert_layout(
            &f.label(),
            "file_activity",
            &r,
            facts.file_activity_size,
            facts.file_activity_offsets,
        );
    }
}

#[test]
fn record_header_layout_matches_fixture_for_every_generation_and_abi() {
    for f in fixtures::all_minimal() {
        let enc = encoding(&f);
        let facts = f.facts();
        let r = resolve_record_header(f.generation(), facts, &enc);
        assert_layout(
            &f.label(),
            "record_header",
            &r,
            facts.record_header_size(f.abi()),
            facts.record_header_offsets,
        );
    }
}

/// 申告サイズだけでは G2175V120 と G2175V1217 を区別できない。
/// `hdr_types_nr` / `rec_types_nr` でレイアウトが変わっていることを明示的に押さえる。
#[test]
fn same_declared_size_but_different_layout_is_distinguished() {
    let v120 = Generation::G2175V120.facts();
    let v1217 = Generation::G2175V1217.facts();
    assert_eq!(
        v120.file_header_size, v1217.file_header_size,
        "どちらも 328"
    );
    assert_ne!(v120.hdr_types_nr, v1217.hdr_types_nr);

    let enc = SourceEncoding::new(Endian::Little, LayoutAbi::LP64);
    let a = resolve_file_header(Generation::G2175V120, v120, &enc);
    let b = resolve_file_header(Generation::G2175V1217, v1217, &enc);

    assert!(
        a.field("extra_next").is_none(),
        "(1,1,11) に extra_next は無い"
    );
    assert_eq!(b.field("extra_next").unwrap().offset, 60);
    assert_eq!(a.field("sa_day").unwrap().offset, 60);
    assert_eq!(b.field("sa_day").unwrap().offset, 64, "4 バイト後退する");

    // record_header はサイズが同じ 24 のまま record_type の位置が動く
    let ra = resolve_record_header(Generation::G2175V120, v120, &enc);
    let rb = resolve_record_header(Generation::G2175V1217, v1217, &enc);
    assert_eq!(ra.size, rb.size);
    assert_eq!(ra.field("record_type").unwrap().offset, 16);
    assert_eq!(rb.field("record_type").unwrap().offset, 20);
}

/// `sa_tzname` は `header_size` 336 の世代にしか無い。
#[test]
fn tzname_exists_only_in_the_336_byte_variant() {
    let enc = SourceEncoding::new(Endian::Little, LayoutAbi::LP64);
    for generation in Generation::SELF_DESCRIBED {
        let facts = generation.facts();
        let r = resolve_file_header(generation, facts, &enc);
        let want = facts.file_header_size == 336;
        assert_eq!(
            r.field("sa_tzname").is_some(),
            want,
            "{}: sa_tzname の在否が違う",
            facts.name
        );
    }
}

// ===========================================================================
// 2. 値の読み出し (解決済みレイアウト経由)
// ===========================================================================

/// `file_header` の主要な値が、解決済みレイアウト経由で fixture の論理値どおりに読めること。
#[test]
fn file_header_values_round_trip_through_resolved_layout() {
    for f in fixtures::all_minimal() {
        let enc = encoding(&f);
        let label = f.label();
        let c = cursor(&f);
        let r = resolve_file_header(f.generation(), f.facts(), &enc);
        let base = f.file_header_off;

        let ust = c
            .read_unsigned(base, r.field("sa_ust_time").unwrap())
            .unwrap();
        assert_eq!(ust, f.spec.ust_time, "{label}: sa_ust_time");

        assert_eq!(
            c.read_str(base, r.field("sa_sysname").unwrap()).unwrap(),
            f.spec.sysname,
            "{label}: sa_sysname"
        );
        assert_eq!(
            c.read_str(base, r.field("sa_nodename").unwrap()).unwrap(),
            f.spec.nodename,
            "{label}: sa_nodename"
        );
        assert_eq!(
            c.read_str(base, r.field("sa_release").unwrap()).unwrap(),
            f.spec.release,
            "{label}: sa_release"
        );
        assert_eq!(
            c.read_str(base, r.field("sa_machine").unwrap()).unwrap(),
            f.spec.machine,
            "{label}: sa_machine"
        );
        assert_eq!(
            c.read_signed(base, r.field("sa_sizeof_long").unwrap())
                .unwrap(),
            f.abi().long_bytes() as i64,
            "{label}: sa_sizeof_long"
        );
        assert_eq!(
            c.read_unsigned(base, r.field("sa_day").unwrap()).unwrap(),
            u64::from(f.spec.day),
            "{label}: sa_day"
        );
        assert_eq!(
            c.read_unsigned(base, r.field("sa_month").unwrap()).unwrap(),
            u64::from(f.spec.month),
            "{label}: sa_month (0 起点)"
        );
        assert_eq!(
            c.read_signed(base, r.field("sa_year").unwrap()).unwrap(),
            i64::from(f.spec.year),
            "{label}: sa_year (1900 起点)"
        );

        // sa_hz は自己記述世代のみ。unsigned long なので ABI 差が出る唯一のヘッダ値。
        if let Some(hz) = r.field("sa_hz") {
            assert_eq!(
                c.read_unsigned(base, hz).unwrap(),
                f.spec.hz,
                "{label}: sa_hz"
            );
        }

        // activity 数はどの世代でも読める (名前が世代で違う)
        let act_nr_field = r
            .field("sa_act_nr")
            .or_else(|| r.field("sa_nr_act"))
            .unwrap_or_else(|| panic!("{label}: activity 数のフィールドが無い"));
        assert_eq!(
            c.read_unsigned(base, act_nr_field).unwrap(),
            f.spec.activities.len() as u64,
            "{label}: activity 数"
        );
    }
}

/// `file_activity[]` が並び順どおりに読めること。
#[test]
fn file_activity_values_round_trip_through_resolved_layout() {
    for f in fixtures::all_minimal() {
        let enc = encoding(&f);
        let label = f.label();
        let c = cursor(&f);
        let r = resolve_file_activity(f.generation(), f.facts(), &enc);

        for (i, act) in f.spec.activities.iter().enumerate() {
            let base = f.file_activity_off + i * f.facts().file_activity_size;
            assert_eq!(
                c.read_unsigned(base, r.field("id").unwrap()).unwrap(),
                u64::from(act.id),
                "{label}: file_activity[{i}].id"
            );
            assert_eq!(
                c.read_signed(base, r.field("nr").unwrap()).unwrap(),
                i64::from(act.nr),
                "{label}: file_activity[{i}].nr"
            );
            assert_eq!(
                c.read_signed(base, r.field("size").unwrap()).unwrap(),
                i64::from(act.size),
                "{label}: file_activity[{i}].size"
            );
            // magic / nr2 は 0x2170 には存在しない
            if let Some(magic) = r.field("magic") {
                assert_eq!(
                    c.read_unsigned(base, magic).unwrap(),
                    u64::from(act.magic),
                    "{label}: file_activity[{i}].magic"
                );
            }
            if let Some(nr2) = r.field("nr2") {
                assert_eq!(
                    c.read_signed(base, nr2).unwrap(),
                    i64::from(act.nr2),
                    "{label}: file_activity[{i}].nr2"
                );
            }
            if let Some(has_nr) = r.field("has_nr") {
                assert_eq!(
                    c.read_signed(base, has_nr).unwrap(),
                    i64::from(act.has_nr),
                    "{label}: file_activity[{i}].has_nr"
                );
            }
            if let Some(t0) = r.field("types_nr_0") {
                assert_eq!(
                    c.read_unsigned(base, t0).unwrap(),
                    u64::from(act.types_nr[0]),
                    "{label}: file_activity[{i}].types_nr[0]"
                );
            }
        }
    }
}

/// `record_header` の時刻と種別が読めること。
#[test]
fn record_header_values_round_trip_through_resolved_layout() {
    for f in fixtures::all_minimal() {
        let enc = encoding(&f);
        let label = f.label();
        let c = cursor(&f);
        let r = resolve_record_header(f.generation(), f.facts(), &enc);

        for (i, (off, _len)) in f.record_offsets.iter().enumerate() {
            let rec = &f.spec.records[i];
            let want_type = match &rec.kind {
                RecordKind::Stats { .. } => fixtures::R_STATS,
                RecordKind::Restart { .. } => fixtures::R_RESTART,
                RecordKind::Comment { .. } => fixtures::R_COMMENT,
                RecordKind::Extra { record_type } => *record_type,
            };
            assert_eq!(
                c.read_unsigned(*off, r.field("record_type").unwrap())
                    .unwrap(),
                u64::from(want_type),
                "{label}: record[{i}].record_type"
            );
            assert_eq!(
                c.read_unsigned(*off, r.field("ust_time").unwrap()).unwrap(),
                rec.ust_time,
                "{label}: record[{i}].ust_time"
            );
            assert_eq!(
                c.read_unsigned(*off, r.field("hour").unwrap()).unwrap(),
                u64::from(rec.hour),
                "{label}: record[{i}].hour"
            );
            assert_eq!(
                c.read_unsigned(*off, r.field("second").unwrap()).unwrap(),
                u64::from(rec.second),
                "{label}: record[{i}].second"
            );
        }
    }
}

// ===========================================================================
// 3. unsigned long のスロット幅 8 / 有効 4
// ===========================================================================

/// `unsigned long` は 32bit ライタでも 8 バイトのスロットを占め、
/// 有効バイト数だけが 4 になる。LE / BE の両方で、
/// **スロットの先頭側**を読めば正しい値になること。
#[test]
fn unsigned_long_slot_is_eight_bytes_with_four_effective_on_32bit() {
    for abi in FixtureAbi::ALL {
        // sa_hz を持つ世代で見る (unsigned long のヘッダ値)
        let f = fixtures::minimal(Generation::G2175Current, abi);
        let enc = encoding(&f);
        let label = f.label();
        let r = resolve_file_header(f.generation(), f.facts(), &enc);
        let hz = r.field("sa_hz").expect("sa_hz がある世代");

        assert_eq!(hz.ty, FieldTy::CULong, "{label}: sa_hz は unsigned long");
        assert_eq!(hz.width, 8, "{label}: スロット幅は ABI に依らず 8");
        assert_eq!(
            hz.value_width,
            abi.long_bytes(),
            "{label}: 有効バイト数は sa_sizeof_long に従う"
        );

        // 後続フィールドの位置はスロット幅 8 に支配され、ABI で動かない
        assert_eq!(
            r.field("sa_cpu_nr").unwrap().offset,
            16,
            "{label}: sa_cpu_nr"
        );

        let c = cursor(&f);
        let base = f.file_header_off;
        assert_eq!(
            c.read_unsigned(base, hz).unwrap(),
            f.spec.hz,
            "{label}: sa_hz の値"
        );

        // スロットの後半 4 バイトは 32bit では常にゼロパディング
        if abi.long_bytes() == 4 {
            let tail = &f.bytes[base + hz.offset + 4..base + hz.offset + 8];
            assert_eq!(tail, [0, 0, 0, 0], "{label}: スロット後半はゼロ");
            // 誤って 8 バイト全部を読むと値が壊れることを明示する
            let whole = c.u64_at(base + hz.offset).unwrap();
            assert_ne!(
                whole, f.spec.hz,
                "{label}: 8 バイト読みは一致してはならない"
            );
        }
    }
}

/// 旧世代の `ust_time` も `unsigned long` である。
/// `aligned(16)` の穴と合わせて、32bit でもオフセットが動かないこと。
#[test]
fn old_record_header_ust_time_is_an_unsigned_long_slot() {
    for abi in FixtureAbi::ALL {
        let f = fixtures::minimal(Generation::G2171, abi);
        let enc = encoding(&f);
        let label = f.label();
        let r = resolve_record_header(f.generation(), f.facts(), &enc);
        let ust = r.field("ust_time").unwrap();

        assert_eq!(ust.offset, 32, "{label}: aligned(16) による位置");
        assert_eq!(ust.width, 8, "{label}: スロット幅");
        assert_eq!(ust.value_width, abi.long_bytes(), "{label}: 有効バイト数");
        assert_eq!(r.size, 48, "{label}: 48 バイト");

        let c = cursor(&f);
        let (off, _) = f.record_offsets[0];
        assert_eq!(
            c.read_unsigned(off, ust).unwrap(),
            f.spec.records[0].ust_time,
            "{label}: ust_time の値"
        );
    }
}

// ===========================================================================
// 4. メタモルフィックテスト (LE / BE で同じ正規化結果)
// ===========================================================================

/// 同じ論理値を 4 通りの ABI で表現した fixture から、同じ値が読めること。
///
/// バイト列はまったく違う (エンディアンも `unsigned long` の有効幅も違う) が、
/// 正規化した結果は一致しなければならない。
#[test]
fn all_four_abis_normalize_to_the_same_values() {
    for generation in Generation::ALL {
        let mut seen: Option<Vec<u64>> = None;
        let mut seen_strings: Option<Vec<String>> = None;

        for abi in FixtureAbi::ALL {
            let f = fixtures::minimal(generation, abi);
            let enc = encoding(&f);
            let c = cursor(&f);
            let r = resolve_file_header(generation, f.facts(), &enc);
            let base = f.file_header_off;

            let mut values = vec![
                c.read_unsigned(base, r.field("sa_ust_time").unwrap())
                    .unwrap(),
                c.read_unsigned(base, r.field("sa_day").unwrap()).unwrap(),
                c.read_unsigned(base, r.field("sa_month").unwrap()).unwrap(),
                c.read_signed(base, r.field("sa_year").unwrap()).unwrap() as u64,
            ];
            if let Some(hz) = r.field("sa_hz") {
                values.push(c.read_unsigned(base, hz).unwrap());
            }

            // レコード側も比較する (統計値まで含めて一致すべき)
            let rh = resolve_record_header(generation, f.facts(), &enc);
            for (off, _) in &f.record_offsets {
                values.push(
                    c.read_unsigned(*off, rh.field("ust_time").unwrap())
                        .unwrap(),
                );
                values.push(
                    c.read_unsigned(*off, rh.field("record_type").unwrap())
                        .unwrap(),
                );
            }
            values.extend(read_all_stats(&f, &enc));

            let strings = vec![
                c.read_str(base, r.field("sa_sysname").unwrap())
                    .unwrap()
                    .to_string(),
                c.read_str(base, r.field("sa_nodename").unwrap())
                    .unwrap()
                    .to_string(),
            ];

            match (&seen, &seen_strings) {
                (None, None) => {
                    seen = Some(values);
                    seen_strings = Some(strings);
                }
                (Some(prev), Some(prev_s)) => {
                    assert_eq!(
                        prev,
                        &values,
                        "{}/{}: ABI をまたいで正規化結果が違う",
                        generation.name(),
                        abi.name()
                    );
                    assert_eq!(
                        prev_s,
                        &strings,
                        "{}/{}: 文字列フィールドが違う",
                        generation.name(),
                        abi.name()
                    );
                }
                _ => unreachable!(),
            }
        }
    }
}

/// 全 STATS レコードの統計値を、宣言された `types_nr` の並びどおりに読む。
///
/// 読み取り手順は fixture 生成側とは独立に、
/// 「ull → ul → int の順、`unsigned long` はスロット 8 バイト」という
/// `docs/format/01-file-format.md` §4.1 の規則から組み立てている。
fn read_all_stats(f: &Fixture, enc: &SourceEncoding) -> Vec<u64> {
    let facts = f.facts();
    let c = cursor(f);
    let rh = resolve_record_header(f.generation(), facts, enc);
    let rt_field = rh.field("record_type").unwrap();
    let long_bytes = f.abi().long_bytes();

    let mut out = Vec::new();
    for (i, (off, _)) in f.record_offsets.iter().enumerate() {
        let rt = c.read_unsigned(*off, rt_field).unwrap() as u8;
        if rt != fixtures::R_STATS {
            continue;
        }
        if !matches!(f.spec.records[i].kind, RecordKind::Stats { .. }) {
            continue;
        }

        // extra 連鎖は record_header の直後 (R_STATS の場合)
        let mut cur = off + facts.record_header_size(f.abi());
        if rh.field("extra_next").is_some() {
            for x in &f.spec.records[i].extra {
                cur += x.byte_len();
            }
        }

        for act in &f.spec.activities {
            let self_desc = facts.act_types_nr.is_some();
            let count = if self_desc && act.has_nr {
                let v = c.u32_at(cur).unwrap() as i32;
                cur += 4;
                v
            } else {
                act.nr
            };
            let items = (count as i64 * act.nr2 as i64) as usize;
            for item in 0..items {
                let mut o = cur + item * act.size as usize;
                for _ in 0..act.types_nr[0] {
                    out.push(c.u64_at(o).unwrap());
                    o += 8;
                }
                for _ in 0..act.types_nr[1] {
                    // unsigned long: スロット 8 バイト、値は先頭 long_bytes バイト
                    let v = match long_bytes {
                        8 => c.u64_at(o).unwrap(),
                        _ => u64::from(c.u32_at(o).unwrap()),
                    };
                    out.push(v);
                    o += 8;
                }
                for _ in 0..act.types_nr[2] {
                    out.push(u64::from(c.u32_at(o).unwrap()));
                    o += 4;
                }
            }
            cur += act.size as usize * items;
        }
    }
    out
}

/// 統計値が生成器の決定的な式どおりに入っていること。
///
/// メタモルフィック検証は「4 通りが同じ」しか見ないので、
/// 「4 通りとも同じように間違っている」可能性を潰すために絶対値も 1 点押さえる。
#[test]
fn stats_payload_carries_the_declared_values() {
    for abi in FixtureAbi::ALL {
        let f = fixtures::minimal(Generation::G2175Current, abi);
        let enc = encoding(&f);
        let values = read_all_stats(&f, &enc);
        assert!(!values.is_empty(), "{}: 統計値が読めていない", f.label());

        // 最初の STATS レコード (seq = 1) の A_CPU / item 0 / field 0
        let want = fixtures::stat_value(ActivitySpec::a_cpu(1).id, 1, 0, 0);
        assert_eq!(values[0], want, "{}: 先頭の統計値", f.label());
    }
}

// ===========================================================================
// 5. レコード走査がファイル末尾にぴったり到達する
// ===========================================================================

/// fixture のレコード列を、本体の解決済みレイアウトだけを使って走査し、
/// ファイル末尾にぴったり到達すること。
///
/// `docs/format/04-test-data.md` §1.3 が本家データに対して行った検証と同じ手口を
/// 自作 fixture に適用する。レイアウト定義と fixture のどちらかが 1 バイトでも
/// 食い違えば残余が出る。
#[test]
fn record_walk_lands_exactly_on_eof() {
    for f in fixtures::all_minimal() {
        let enc = encoding(&f);
        let label = f.label();
        let end = walk_records(&f, &enc);
        assert_eq!(
            end,
            f.bytes.len(),
            "{label}: 走査終端がファイル末尾と一致しない"
        );
    }
}

/// extra 連鎖つきの fixture でも末尾にぴったり到達すること。
#[test]
fn record_walk_lands_exactly_on_eof_with_extra_chains() {
    for generation in [Generation::G2175V1217, Generation::G2175Current] {
        for abi in FixtureAbi::ALL {
            let f = fixtures::with_extra_chains(generation, abi);
            let enc = encoding(&f);
            assert!(
                !f.spec.file_extra.is_empty(),
                "{}: extra 連鎖が入っていない",
                f.label()
            );
            assert_eq!(
                walk_records(&f, &enc),
                f.bytes.len(),
                "{}: extra 連鎖ありで走査終端が一致しない",
                f.label()
            );
        }
    }
}

/// レコード列を走査して終端オフセットを返す。
fn walk_records(f: &Fixture, enc: &SourceEncoding) -> usize {
    let facts = f.facts();
    let c = cursor(f);
    let fh = resolve_file_header(f.generation(), facts, enc);
    let fa = resolve_file_activity(f.generation(), facts, enc);
    let rh = resolve_record_header(f.generation(), facts, enc);

    let hdr_base = f.file_header_off;
    let act_nr = c
        .read_unsigned(
            hdr_base,
            fh.field("sa_act_nr")
                .or_else(|| fh.field("sa_nr_act"))
                .unwrap(),
        )
        .unwrap() as usize;

    // activity リストをファイルから読み直す (fixture の spec は見ない)
    struct Act {
        nr: i32,
        nr2: i32,
        has_nr: bool,
        size: i32,
    }
    let acts: Vec<Act> = (0..act_nr)
        .map(|i| {
            let base = f.file_activity_off + i * facts.file_activity_size;
            Act {
                nr: c.read_signed(base, fa.field("nr").unwrap()).unwrap() as i32,
                nr2: fa
                    .field("nr2")
                    .map(|x| c.read_signed(base, x).unwrap() as i32)
                    .unwrap_or(1),
                has_nr: fa
                    .field("has_nr")
                    .map(|x| c.read_signed(base, x).unwrap() != 0)
                    .unwrap_or(false),
                size: c.read_signed(base, fa.field("size").unwrap()).unwrap() as i32,
            }
        })
        .collect();

    let mut cur = f.file_activity_off + act_nr * facts.file_activity_size;

    // file_header.extra_next が立っていれば activity リストの直後に extra 連鎖
    let file_extra_next = fh
        .field("extra_next")
        .map(|x| c.read_unsigned(hdr_base, x).unwrap() != 0)
        .unwrap_or(false);
    if file_extra_next {
        cur = skip_extra_chain(&c, cur);
    }

    let vol_act_nr = fh
        .field("sa_vol_act_nr")
        .map(|x| c.read_unsigned(hdr_base, x).unwrap() as usize)
        .unwrap_or(0);

    while cur < f.bytes.len() {
        if cur + facts.record_header_size(f.abi()) > f.bytes.len() {
            // レコードヘッダが入り切らない = 切り詰め
            return cur;
        }
        let rt = c
            .read_unsigned(cur, rh.field("record_type").unwrap())
            .unwrap() as u8;
        let rec_extra_next = rh
            .field("extra_next")
            .map(|x| c.read_unsigned(cur, x).unwrap() != 0)
            .unwrap_or(false);
        cur += facts.record_header_size(f.abi());

        // R_STATS / R_EXTRA* はヘッダ直後に extra 連鎖 (§1.1)
        let extra_after_header = rt != fixtures::R_COMMENT && rt != fixtures::R_RESTART;
        if rec_extra_next && extra_after_header {
            cur = skip_extra_chain(&c, cur);
        }

        match rt {
            fixtures::R_COMMENT => {
                cur += fixtures::MAX_COMMENT_LEN;
                if rec_extra_next {
                    cur = skip_extra_chain(&c, cur);
                }
            }
            fixtures::R_RESTART => {
                if facts.act_types_nr.is_some() {
                    // 自己記述世代: __nr_t の CPU 数のみ (§6.3)
                    cur += 4;
                } else if vol_act_nr > 0 {
                    // 0x2173: volatile activity リストが並ぶ (§5.7)
                    cur += vol_act_nr * facts.file_activity_size;
                }
                if rec_extra_next {
                    cur = skip_extra_chain(&c, cur);
                }
            }
            t if (fixtures::R_EXTRA_MIN..=15).contains(&t) => {
                // 統計なし (§6.1)
            }
            _ => {
                for act in &acts {
                    let count = if act.has_nr && facts.act_types_nr.is_some() {
                        let v = c.u32_at(cur).unwrap() as i32;
                        cur += 4;
                        v
                    } else {
                        act.nr
                    };
                    cur += act.size as usize * (count as i64 * act.nr2 as i64) as usize;
                }
            }
        }
    }
    cur
}

/// `extra_desc` 連鎖を読み飛ばして次の位置を返す (§3.5)。
fn skip_extra_chain(c: &Cursor<'_>, start: usize) -> usize {
    let mut cur = start;
    loop {
        let nr = c.u32_at(cur).unwrap() as usize;
        let size = c.u32_at(cur + 4).unwrap() as usize;
        let next = c.u32_at(cur + 8).unwrap();
        cur += fixtures::EXTRA_DESC_SIZE + nr * size;
        if next == 0 {
            return cur;
        }
    }
}

// ===========================================================================
// 6. 異常系 fixture の素性
// ===========================================================================

/// 異常系の基準ファイルが本家 `data-12.6.0-*` と同じ構成 (448 バイト) であること。
#[test]
fn error_base_fixture_has_the_same_shape_as_upstream() {
    for abi in FixtureAbi::ALL {
        let f = fixtures::err_base(abi);
        // file_magic 76 + file_header 336 + file_activity 36 × 1 = 448
        assert_eq!(f.bytes.len(), 448, "{}: 448 バイト", f.label());
        assert_eq!(f.file_header_off, 76);
        assert_eq!(f.file_activity_off, 412);
        assert!(f.spec.records.is_empty(), "レコードは 0 件");
    }
}

/// 自作異常系の改変バイト位置が、本家 `-err` 系の改変位置と一致すること。
///
/// 出所: `docs/format/01-file-format.md` §10.5 と `04-test-data.md` §2.2 の表。
/// 自作 fixture が本家と同じ構造になっていることの、独立した裏取りになる。
#[test]
fn corrupted_fixture_offsets_match_upstream_err_files() {
    // (壊し方, 本家が書き換えたフィールドの先頭バイト位置)
    let table = [
        (Corruption::HdrSaActNr, 96usize),
        (Corruption::HdrMapSizeActTypesNr, 104),
        (Corruption::HdrMapSizeRecTypesNr, 116),
        (Corruption::HdrActSize, 128),
        (Corruption::HdrRecSize, 132),
        (Corruption::ActNrZero, 420),
        (Corruption::ActNrHuge, 420),
        (Corruption::ActNrOverNrMax, 420),
        (Corruption::ActNr2Zero, 424),
        (Corruption::ActNr2Huge, 424),
        (Corruption::ActSizeZero, 432),
        (Corruption::ActSizeHuge, 432),
        (Corruption::ActMapSizeTypesNr, 436),
        (Corruption::ActTypesNrNonMonotonic, 436),
    ];

    for (corruption, want_off) in table {
        let c = fixtures::corrupted(FixtureAbi::Le64, corruption);
        assert_eq!(
            c.patched.first().map(|p| p.0),
            Some(want_off),
            "{corruption:?}: 改変位置が本家と違う"
        );
        assert!(
            corruption.upstream_name().is_some(),
            "{corruption:?}: 本家の対応ファイルが未記載"
        );
        // 差分は狙ったフィールドの範囲内だけに収まること
        let allowed: Vec<usize> = c
            .patched
            .iter()
            .flat_map(|(off, len)| *off..off + len)
            .collect();
        for off in c.diff_offsets() {
            assert!(
                allowed.contains(&off),
                "{corruption:?}: 意図しないバイト {off} が変わっている"
            );
        }
        assert_eq!(
            c.bytes.len(),
            c.base.len(),
            "{corruption:?}: サイズは変わらない"
        );
    }
}

/// 全 ABI で、全種類の異常系が生成できること (パニックしない・基準と差が出る)。
#[test]
fn every_corruption_is_generated_for_every_abi() {
    for abi in FixtureAbi::ALL {
        for corruption in Corruption::ALL {
            let c = fixtures::corrupted(abi, corruption);
            assert!(
                !c.bytes.is_empty(),
                "{}/{corruption:?}: 空になっている",
                abi.name()
            );
            assert!(
                !c.diff_offsets().is_empty(),
                "{}/{corruption:?}: 基準ファイルと差が無い",
                abi.name()
            );
            assert!(
                !corruption.detects().is_empty(),
                "{corruption:?}: 検出内容が未記載"
            );
        }
    }
}

/// 切り詰め系はファイルが短くなり、境界の途中で終わっていること。
#[test]
fn truncated_fixtures_end_in_the_middle_of_a_structure() {
    let abi = FixtureAbi::Le64;
    let facts = Generation::G2175Current.facts();

    let c = fixtures::corrupted(abi, Corruption::TruncatedFileHeader);
    assert!(c.bytes.len() > 76, "file_magic は完全");
    assert!(
        c.bytes.len() < 76 + facts.file_header_size,
        "file_header の途中で終わる"
    );

    let c = fixtures::corrupted(abi, Corruption::TruncatedActivityList);
    let list_start = 76 + facts.file_header_size;
    assert!(c.bytes.len() > list_start, "activity リストの途中");
    assert!(
        (c.bytes.len() - list_start) % facts.file_activity_size != 0,
        "activity 境界に揃っていないこと"
    );

    let full = fixtures::minimal(Generation::G2175Current, abi);
    let (last_off, last_len) = *full.record_offsets.last().unwrap();
    for corruption in [
        Corruption::TruncatedRecordHeader,
        Corruption::TruncatedStats,
    ] {
        let c = fixtures::corrupted(abi, corruption);
        assert!(
            c.bytes.len() > last_off && c.bytes.len() < last_off + last_len,
            "{corruption:?}: 最終レコードの途中で終わること"
        );
    }
}

/// `nr × nr2 × size` の u32 オーバーフローは、全サニティチェックを通り抜けてから起きる。
#[test]
fn irq_overflow_fixture_passes_every_limit_check() {
    let c = fixtures::corrupted(FixtureAbi::Le64, Corruption::IrqOverflow);
    let act = ActivitySpec::a_irq_overflow();

    // 個別の上限はすべて「以下」に収まっている
    assert!(act.nr <= fixtures::CPU_NR_MAX);
    assert!(act.nr2 <= fixtures::NR2_MAX);
    assert!(act.size <= fixtures::MAX_ITEM_STRUCT_SIZE);
    assert!(fixtures::map_size(act.types_nr) <= act.size as u32);

    // それでも積は u32 を溢れる
    let product = act.nr as u64 * act.nr2 as u64 * act.size as u64;
    assert!(
        product > u64::from(u32::MAX),
        "積が u32 に収まってしまっている: {product}"
    );
    assert_eq!(product, 34_359_738_368, "本家と同じ積になること");
    assert!(!Corruption::IrqOverflow.fails_header_only_mode());
    assert_eq!(c.bytes.len(), 448);
}

/// 未知 magic / 非 sysstat ファイルは先頭 4 バイトだけが違う。
#[test]
fn magic_corruptions_touch_only_the_first_four_bytes() {
    for (corruption, off) in [
        (Corruption::BadSysstatMagic, 0usize),
        (Corruption::UnknownFormatMagic, 2),
    ] {
        let c = fixtures::corrupted(FixtureAbi::Be64, corruption);
        assert_eq!(c.patched, vec![(off, 2)], "{corruption:?}: 改変位置");
        assert!(
            c.diff_offsets().iter().all(|&o| o == off || o == off + 1),
            "{corruption:?}: 他のバイトが変わっている"
        );
    }
}

/// 切り詰め列が単調に短くなること (性質検証の素材として使えること)。
#[test]
fn truncation_series_is_monotonically_shorter() {
    let series = fixtures::truncation_series(Generation::G2175Current, FixtureAbi::Le64, 64);
    assert!(series.len() > 2);
    for pair in series.windows(2) {
        assert!(pair[0].len() > pair[1].len(), "単調に短くなること");
    }
    assert_eq!(series.last().unwrap().len(), 0);
}
