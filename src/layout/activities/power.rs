//! power 系 activity の定義。
//!
//! 対象は `A_PWR_CPU` (30) / `A_PWR_FAN` (31) / `A_PWR_TEMP` (32) / `A_PWR_IN` (33) /
//! `A_PWR_FREQ` (35) / `A_PWR_USB` (36) / `A_PWR_BAT` (43)。
//! いずれも sadc の `-S POWER` グループで、すべて `AO_COUNTED` = `has_nr` が真。
//!
//! フィールド順・アラインメント・`size_lp64` / `types_nr` の出所は
//! `docs/format/02-activities.md` §5 (オフセット表) と §9.3 (activity 別差分表)。
//! 時代 A (旧 `FORMAT_MAGIC`) の構造体サイズは `docs/format/01-file-format.md`
//! §5.10 / §5.12 の実測表から写している。
//!
//! ## この group 固有の注意
//!
//! - `stats_pwr_fan` / `stats_pwr_temp` / `stats_pwr_in` の値は **IEEE-754 `double`**。
//!   エンディアン変換も 64bit 整数として行われ、`types_nr[0]` (ULL 群) に計上される (§1.2)。
//!   よってレイアウト記述では [`FieldTy::U64`] とし、値の解釈側で
//!   `f64::from_bits()` 相当を行う。
//! - `unsigned long` (`cpufreq` / `freq`) は `aligned(8)` によりファイル上
//!   **常に 8 バイトのスロット**を占め、32bit ライタでは先頭 4 バイトだけが有効 (§1.1)。
//!   必ず [`FieldTy::CULong`] を使う ([`FieldTy::U32`] では 4 バイトずれる)。
//! - `stats_pwr_bat` は 3 バイト・アラインメント 1 で、item が 3 バイト刻みに並ぶ (§11.2-16)。

use crate::format::wire::{FieldTy, WireField, WireLayout};
use crate::layout::registry::{ActivityDef, ColumnMeta, ItemShape, WireRevision};
use crate::model::{ActivityId, Aggregation, Unit, ValueKind};

/// `MAX_SENSORS_DEV_LEN` (v9.1.5 から不変。§10.1)。
const MAX_SENSORS_DEV_LEN: u16 = 20;
/// `MAX_MANUF_LEN` (§10.1)。
const MAX_MANUF_LEN: u16 = 24;
/// `MAX_PROD_LEN` (§10.1)。
const MAX_PROD_LEN: u16 = 48;

// ===== A_PWR_CPU (30) — sar -m CPU =====
//
// magic は `0x8a` のまま昇格なし。`cpufreq` は終始 `unsigned long aligned(8)` で
// 時代 A / B ともサイズ 8 バイト・オフセット 0 なので revision は 1 つで足りる
// (v11.7.2 でシンボル名が `A_PWR_CPUFREQ` → `A_PWR_CPU` に変わっただけ)。

const PWR_CPU_FIELDS: &[WireField] = &[WireField::aligned("cpufreq", FieldTy::CULong, 8)];

const PWR_CPU_REVISIONS: &[WireRevision] = &[WireRevision {
    magic: 0x8a,
    self_describing: true,
    types_nr: [0, 1, 0],
    size_lp64: 8,
    layout: WireLayout::new("stats_pwr_cpufreq@0x8a", PWR_CPU_FIELDS),
    since: "9.1.6",
}];

/// `sar -m CPU` の列。`hdr_line` = `CPU;MHz` (03 §id=30)。
///
/// 表示側のスケーリング: `MHz` = `cpufreq / 100`
/// (`cpufreq` は **MHz × 100** で保存されている。§11.2-15)。
/// `cpufreq == 0` の CPU はオフライン扱いで行を出さない (§11.2-12)。
const PWR_CPU_COLUMNS: &[ColumnMeta] = &[
    // CPU 番号は wire に無く item のインデックスで決まる (index 0 = 全 CPU の平均、
    // index n = CPU #n-1。§4.2)。
    ColumnMeta {
        public_name: "cpu",
        sar_header: "CPU",
        wire_name: "",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::NotAggregated,
    },
    ColumnMeta {
        public_name: "mhz",
        sar_header: "MHz",
        wire_name: "cpufreq",
        unit: Unit::Megahertz,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
];

// ===== A_PWR_FAN (31) — sar -m FAN =====
//
// magic は `0x8a` のまま昇格なし。サイズも 40 バイトで一度も変わっていない
// (時代 A の `aligned(8)` は現行の自然アラインメントと同じ配置になるため、
// revision は 1 つで足りる)。

const PWR_FAN_FIELDS: &[WireField] = &[
    // double (IEEE-754)。types_nr[0] に計上される。
    // `aligned(8)` は撤去されずに残っている: これが無いと i386 psABI (double の
    // アラインメントが 4) で構造体アラインメントが 4 に落ち、サイズが 40 → 36 に
    // なってしまう。ドキュメントの実測値は 4 ABI すべてで 40 B / アラインメント 8。
    WireField::aligned("rpm", FieldTy::U64, 8),
    WireField::aligned("rpm_min", FieldTy::U64, 8),
    WireField::natural("device", FieldTy::Bytes(MAX_SENSORS_DEV_LEN)),
];

const PWR_FAN_REVISIONS: &[WireRevision] = &[WireRevision {
    magic: 0x8a,
    self_describing: true,
    types_nr: [2, 0, 0],
    size_lp64: 40,
    layout: WireLayout::new("stats_pwr_fan@0x8a", PWR_FAN_FIELDS),
    since: "9.1.6",
}];

/// `sar -m FAN` の列。`hdr_line` = `FAN;DEVICE;rpm;drpm` (03 §id=31)。
/// `DEVICE` は `print_hdr_line(..., -2, ...)` により**行末**へ回る。
///
/// 表示側の計算: `drpm` = `rpm - rpm_min` (追加スケーリング無し)。
/// `FAN` 列はセンサ番号で **1 起点** (item インデックス + 1)。
const PWR_FAN_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "fan_index",
        sar_header: "FAN",
        wire_name: "",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::NotAggregated,
    },
    ColumnMeta {
        public_name: "rpm",
        sar_header: "rpm",
        wire_name: "rpm",
        unit: Unit::Rpm,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    // 派生列: rpm - rpm_min
    ColumnMeta {
        public_name: "rpm_delta",
        sar_header: "drpm",
        wire_name: "",
        unit: Unit::Rpm,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "rpm_min",
        sar_header: "",
        wire_name: "rpm_min",
        unit: Unit::Rpm,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "device",
        sar_header: "DEVICE",
        wire_name: "device",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::NotAggregated,
    },
];

// ===== A_PWR_TEMP (32) — sar -m TEMP =====
//
// magic `0x8a`、サイズ 48 バイトで変更なし。

const PWR_TEMP_FIELDS: &[WireField] = &[
    // double × 3 (IEEE-754)。types_nr[0] に計上される。
    // `aligned(8)` は A_PWR_FAN と同じ理由で必要 (i386 でも 48 B / アラインメント 8)。
    WireField::aligned("temp", FieldTy::U64, 8),
    WireField::aligned("temp_min", FieldTy::U64, 8),
    WireField::aligned("temp_max", FieldTy::U64, 8),
    WireField::natural("device", FieldTy::Bytes(MAX_SENSORS_DEV_LEN)),
];

const PWR_TEMP_REVISIONS: &[WireRevision] = &[WireRevision {
    magic: 0x8a,
    self_describing: true,
    types_nr: [3, 0, 0],
    size_lp64: 48,
    layout: WireLayout::new("stats_pwr_temp@0x8a", PWR_TEMP_FIELDS),
    since: "9.1.6",
}];

/// `sar -m TEMP` の列。`hdr_line` = `TEMP;DEVICE;degC;%temp` (03 §id=32)。
/// `DEVICE` は行末へ回る。
///
/// 表示側の計算:
/// `%temp` = `(temp - temp_min) / (temp_max - temp_min) * 100`
/// (分母が 0 なら 0)。`TEMP` 列はセンサ番号で **1 起点**。
const PWR_TEMP_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "sensor_index",
        sar_header: "TEMP",
        wire_name: "",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::NotAggregated,
    },
    ColumnMeta {
        public_name: "temp_celsius",
        sar_header: "degC",
        wire_name: "temp",
        unit: Unit::Celsius,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    // 派生列: (temp - temp_min) / (temp_max - temp_min) * 100
    ColumnMeta {
        public_name: "temp_pct",
        sar_header: "%temp",
        wire_name: "",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    // min/max はセンサの定格。本家も平均を取らず最後の値を使う (03 §id=32)。
    ColumnMeta {
        public_name: "temp_min",
        sar_header: "",
        wire_name: "temp_min",
        unit: Unit::Celsius,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Last,
    },
    ColumnMeta {
        public_name: "temp_max",
        sar_header: "",
        wire_name: "temp_max",
        unit: Unit::Celsius,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Last,
    },
    ColumnMeta {
        public_name: "device",
        sar_header: "DEVICE",
        wire_name: "device",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::NotAggregated,
    },
];

// ===== A_PWR_IN (33) — sar -m IN =====
//
// magic `0x8a`、サイズ 48 バイトで変更なし。構造は A_PWR_TEMP と同型。

const PWR_IN_FIELDS: &[WireField] = &[
    // double × 3 (IEEE-754)。フィールド名は C の `in` そのまま。
    // `aligned(8)` は A_PWR_FAN と同じ理由で必要 (i386 でも 48 B / アラインメント 8)。
    WireField::aligned("in", FieldTy::U64, 8),
    WireField::aligned("in_min", FieldTy::U64, 8),
    WireField::aligned("in_max", FieldTy::U64, 8),
    WireField::natural("device", FieldTy::Bytes(MAX_SENSORS_DEV_LEN)),
];

const PWR_IN_REVISIONS: &[WireRevision] = &[WireRevision {
    magic: 0x8a,
    self_describing: true,
    types_nr: [3, 0, 0],
    size_lp64: 48,
    layout: WireLayout::new("stats_pwr_in@0x8a", PWR_IN_FIELDS),
    since: "9.1.6",
}];

/// `sar -m IN` の列。`hdr_line` = `IN;DEVICE;inV;%in` (03 §id=33)。
/// `DEVICE` は行末へ回る。
///
/// 表示側の計算: `%in` = `(in - in_min) / (in_max - in_min) * 100` (分母が 0 なら 0)。
/// `IN` 列は **0 起点** (FAN / TEMP は 1 起点なので取り違えに注意)。
const PWR_IN_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "input_index",
        sar_header: "IN",
        wire_name: "",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::NotAggregated,
    },
    ColumnMeta {
        public_name: "volts",
        sar_header: "inV",
        wire_name: "in",
        unit: Unit::Volts,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    // 派生列: (in - in_min) / (in_max - in_min) * 100
    ColumnMeta {
        public_name: "in_pct",
        sar_header: "%in",
        wire_name: "",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    ColumnMeta {
        public_name: "volts_min",
        sar_header: "",
        wire_name: "in_min",
        unit: Unit::Volts,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Last,
    },
    ColumnMeta {
        public_name: "volts_max",
        sar_header: "",
        wire_name: "in_max",
        unit: Unit::Volts,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Last,
    },
    ColumnMeta {
        public_name: "device",
        sar_header: "DEVICE",
        wire_name: "device",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::NotAggregated,
    },
];

// ===== A_PWR_FREQ (35) — sar -m FREQ =====
//
// magic の履歴 (§3.3): v9.1.6 で `0x8a` として新設 → `0x8b` @v11.7.2
// (同時に `A_PWR_WGHFREQ` → `A_PWR_FREQ` に改名)。
// `AO_MATRIX` なので item は nr (CPU 数) × nr2 (周波数状態数) 個並ぶ。

/// 現行 (v11.7.2 〜): `freq` の `aligned(16)` が `aligned(8)` になり 32 → 16 バイト。
const PWR_FREQ_FIELDS_16: &[WireField] = &[
    WireField::natural("time_in_state", FieldTy::U64),
    WireField::aligned("freq", FieldTy::CULong, 8),
];

/// 時代 A (v9.1.6 〜 v11.6.6) の `stats_pwr_wghfreq_8a`: 32 バイト /
/// アラインメント 16 / `unsigned long` 1 本 (01 §5.12)。
/// 両フィールドの `aligned(16)` により 8 バイトの穴が 2 つ空く。
const PWR_FREQ_FIELDS_32: &[WireField] = &[
    WireField::aligned("time_in_state", FieldTy::U64, 16),
    WireField::aligned("freq", FieldTy::CULong, 16),
];

const PWR_FREQ_REVISIONS: &[WireRevision] = &[
    WireRevision {
        magic: 0x8b,
        self_describing: true,
        types_nr: [1, 1, 0],
        size_lp64: 16,
        layout: WireLayout::new("stats_pwr_wghfreq@0x8b/16", PWR_FREQ_FIELDS_16),
        since: "11.7.2",
    },
    // 時代 A。types_nr はファイルに存在しないため、宣言値は整合性検査用。
    WireRevision {
        magic: 0x8a,
        self_describing: false,
        types_nr: [1, 1, 0],
        size_lp64: 32,
        layout: WireLayout::new("stats_pwr_wghfreq_8a@0x8a/32", PWR_FREQ_FIELDS_32),
        since: "9.1.6",
    },
];

/// `sar -m FREQ` の列。`hdr_line` = `CPU;wghMHz` (03 §id=35)。
///
/// 表示側の計算 (CPU 行 `i` について nr2 個の sub-item を畳む):
/// ```text
/// tisfreq = Σ_k (freq[k] / 1000) * Δtime_in_state[k]   // freq/1000 は整数除算 (kHz → MHz)
/// tis     = Σ_k Δtime_in_state[k]
/// wghMHz  = if tis != 0 { tisfreq / tis } else { 0 }
/// ```
/// `freq == 0` の sub-item で内側ループを打ち切る (未使用スロット)。
/// A_PWR_CPU と違い `cpufreq == 0` によるオフライン除外は行わない。
const PWR_FREQ_COLUMNS: &[ColumnMeta] = &[
    // CPU 番号は item の行インデックス (行 0 = 全 CPU の平均。§4.2)。
    ColumnMeta {
        public_name: "cpu",
        sar_header: "CPU",
        wire_name: "",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::NotAggregated,
    },
    // 派生列: time_in_state と freq を nr2 個の sub-item にわたって畳む
    ColumnMeta {
        public_name: "weighted_mhz",
        sar_header: "wghMHz",
        wire_name: "",
        unit: Unit::Megahertz,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    // 当該周波数での滞留時間 (1/100 秒単位の累積)。
    ColumnMeta {
        public_name: "time_in_state",
        sar_header: "",
        wire_name: "time_in_state",
        unit: Unit::Centiseconds,
        kind: ValueKind::Counter,
        aggregation: Aggregation::Sum,
    },
    // 周波数 (kHz)。Unit に kHz が無いため None とし、表示時に /1000 して MHz にする。
    ColumnMeta {
        public_name: "freq_khz",
        sar_header: "",
        wire_name: "freq",
        unit: Unit::None,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Last,
    },
];

// ===== A_PWR_USB (36) — sar -m USB =====
//
// v10.0.1 で `0x8a` として新設、昇格なし。時代 A / B でサイズ 88 バイト・
// オフセットとも不変 (属性撤去のみ) なので revision は 1 つで足りる。

const PWR_USB_FIELDS: &[WireField] = &[
    WireField::natural("bus_nr", FieldTy::U32),
    WireField::natural("vendor_id", FieldTy::U32),
    WireField::natural("product_id", FieldTy::U32),
    WireField::natural("bmaxpower", FieldTy::U32),
    WireField::natural("manufacturer", FieldTy::Bytes(MAX_MANUF_LEN)),
    WireField::natural("product", FieldTy::Bytes(MAX_PROD_LEN)),
];

const PWR_USB_REVISIONS: &[WireRevision] = &[WireRevision {
    magic: 0x8a,
    self_describing: true,
    types_nr: [0, 0, 4],
    size_lp64: 88,
    layout: WireLayout::new("stats_pwr_usb@0x8a", PWR_USB_FIELDS),
    since: "10.0.1",
}];

/// `sar -m USB` の列。テキスト出力はヘッダをハードコードしており
/// `BUS / idvendor / idprod / maxpower / manufact / product` の順に出る
/// (`activity.c` の `hdr_line` = `manufact;product;BUS;idvendor;idprod;maxpower` は
/// sadf 専用。03 §id=36)。ここでは**テキスト出力の並び**で定義する。
///
/// 表示側のスケーリング: `maxpower` = `bmaxpower * 2`
/// (`bMaxPower` は 2 mA 単位なので 2 倍して mA にする)。
/// `idvendor` / `idprod` は小文字 16 進 (`0x` なし) で表示する。
///
/// 集計は [`Aggregation::Last`]。A_PWR_USB は**期間平均を取らない** activity で、
/// `Summary:` 行は観測されたすべての USB デバイスの和集合について
/// 最後に保存された値をそのまま再掲する (03 §id=36)。
const PWR_USB_COLUMNS: &[ColumnMeta] = &[
    ColumnMeta {
        public_name: "bus",
        sar_header: "BUS",
        wire_name: "bus_nr",
        unit: Unit::None,
        kind: ValueKind::Identity,
        aggregation: Aggregation::Last,
    },
    ColumnMeta {
        public_name: "vendor_id",
        sar_header: "idvendor",
        wire_name: "vendor_id",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::Last,
    },
    ColumnMeta {
        public_name: "product_id",
        sar_header: "idprod",
        wire_name: "product_id",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::Last,
    },
    ColumnMeta {
        public_name: "max_power",
        sar_header: "maxpower",
        wire_name: "bmaxpower",
        unit: Unit::None,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Last,
    },
    ColumnMeta {
        public_name: "manufacturer",
        sar_header: "manufact",
        wire_name: "manufacturer",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::Last,
    },
    ColumnMeta {
        public_name: "product",
        sar_header: "product",
        wire_name: "product",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::Last,
    },
];

// ===== A_PWR_BAT (43) — sar -m BAT =====
//
// v12.7.2 で `0x8a` として新設、昇格なし。
// **`types_nr = (0,0,0)` で出荷された唯一の構造体**で、サイズは 3 バイト
// (アラインメント 1)。3 フィールドすべて `char` であり、`char` は
// ULL / UL / U のどの型グループにも計上されないため `MAP_SIZE` は 0 になる。
//
// `char` は x86_64 Linux では符号付きなので、`capacity` の差分は符号付き演算で行う
// (03 §id=43 の指示どおり [`FieldTy::I8`] とする。幅 1 バイトで型グループにも
// 現れないため `types_nr` と `size_lp64` は U8 と同じ結果になる)。

const PWR_BAT_FIELDS: &[WireField] = &[
    WireField::natural("bat_id", FieldTy::I8),
    WireField::natural("capacity", FieldTy::I8),
    WireField::natural("status", FieldTy::I8),
];

const PWR_BAT_REVISIONS: &[WireRevision] = &[WireRevision {
    magic: 0x8a,
    self_describing: true,
    types_nr: [0, 0, 0],
    size_lp64: 3,
    layout: WireLayout::new("stats_pwr_bat@0x8a", PWR_BAT_FIELDS),
    since: "12.7.2",
}];

/// `sar -m BAT` の列。`hdr_line` = `BAT;%cap;cap/min;status` (03 §id=43)。
///
/// 表示側の計算: `cap/min` = `(curr.capacity - prev.capacity) * 6000 / itv`
/// (`itv` は 1/100 秒なので `Δ / (itv/100) * 60` = `Δ * 6000 / itv` → %/分)。
/// `status` は 0 = Unknown / 1 = Charging / 2 = Discharging / 3 = Not charging /
/// 4 = Full で、平均行には出力されない。
const PWR_BAT_COLUMNS: &[ColumnMeta] = &[
    // バッテリ番号は配列添字ではなく bat_id が同定キー (§4.3)。
    ColumnMeta {
        public_name: "bat_id",
        sar_header: "BAT",
        wire_name: "bat_id",
        unit: Unit::Identifier,
        kind: ValueKind::Identity,
        aggregation: Aggregation::NotAggregated,
    },
    ColumnMeta {
        public_name: "capacity_pct",
        sar_header: "%cap",
        wire_name: "capacity",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    // 派生列: Δcapacity * 6000 / itv (%/分)
    ColumnMeta {
        public_name: "capacity_per_min",
        sar_header: "cap/min",
        wire_name: "",
        unit: Unit::Percent,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Mean,
    },
    // 充放電状態。平均を取る意味が無いので最後の値を採る。
    ColumnMeta {
        public_name: "status",
        sar_header: "status",
        wire_name: "status",
        unit: Unit::None,
        kind: ValueKind::Gauge,
        aggregation: Aggregation::Last,
    },
];

/// この group の activity 定義。
pub const DEFS: &[ActivityDef] = &[
    ActivityDef {
        id: ActivityId::PWR_CPU,
        revisions: PWR_CPU_REVISIONS,
        columns: PWR_CPU_COLUMNS,
        shape: ItemShape::List,
        // CPU 番号は item のインデックスで決まる。名前フィールドは持たない。
        item_key: "",
        // AO_COUNTED (§3.1)
        has_nr: true,
    },
    ActivityDef {
        id: ActivityId::PWR_FAN,
        revisions: PWR_FAN_REVISIONS,
        columns: PWR_FAN_COLUMNS,
        shape: ItemShape::List,
        // libsensors が返すセンサデバイス名。順序は安定とは限らないので名前で同定する (§4.3)。
        item_key: "device",
        has_nr: true,
    },
    ActivityDef {
        id: ActivityId::PWR_TEMP,
        revisions: PWR_TEMP_REVISIONS,
        columns: PWR_TEMP_COLUMNS,
        shape: ItemShape::List,
        item_key: "device",
        has_nr: true,
    },
    ActivityDef {
        id: ActivityId::PWR_IN,
        revisions: PWR_IN_REVISIONS,
        columns: PWR_IN_COLUMNS,
        shape: ItemShape::List,
        item_key: "device",
        has_nr: true,
    },
    ActivityDef {
        id: ActivityId::PWR_FREQ,
        revisions: PWR_FREQ_REVISIONS,
        columns: PWR_FREQ_COLUMNS,
        // AO_MATRIX: nr (CPU 数) 行 × nr2 (周波数状態数) 列。
        // nr2 は毎サンプル記録されず file_activity.nr2 を固定値として使う (§11.2-3)。
        shape: ItemShape::Matrix,
        // CPU 番号は行インデックスで決まる。名前フィールドは持たない。
        item_key: "",
        has_nr: true,
    },
    ActivityDef {
        id: ActivityId::PWR_USB,
        revisions: PWR_USB_REVISIONS,
        columns: PWR_USB_COLUMNS,
        shape: ItemShape::List,
        // 本家の同定キーは (bus_nr, vendor_id, product_id) の 3 つ組だが、
        // 文字列で表せる識別子は製品名だけなので表示・突合用にこれを採る。
        item_key: "product",
        has_nr: true,
    },
    ActivityDef {
        id: ActivityId::PWR_BAT,
        revisions: PWR_BAT_REVISIONS,
        columns: PWR_BAT_COLUMNS,
        shape: ItemShape::List,
        // 同定キーは bat_id だが数値フィールドなので item_key (文字列) には載せない (§4.3)。
        item_key: "",
        has_nr: true,
    },
];
