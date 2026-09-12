//! エラー型。
//!
//! 既定は strict であり、疑わしいデータを黙って通さない。
//! `--lenient` で回復させる範囲は `docs/design.md` の「7. 壊れたファイルへの態度」に従う。

use std::path::PathBuf;

/// 解析全体のエラー。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// sysstat のファイルではない (先頭のマジックナンバーが一致しない)。
    #[error(
        "{path}: sysstat のデータファイルではありません (magic=0x{found:04x}, expected 0x{expected:04x})"
    )]
    NotSysstatFile {
        path: PathBuf,
        found: u16,
        expected: u16,
    },

    /// sysstat のファイルだが、未知のフォーマット世代。
    #[error(
        "{path}: 未対応のフォーマットです (format_magic=0x{format_magic:04x}, sysstat {version})"
    )]
    UnsupportedFormat {
        path: PathBuf,
        format_magic: u16,
        version: String,
    },

    /// ファイルが途中で終わっている。
    #[error("{path}: ファイルが途中で終わっています ({context}: {need} バイト必要, 残り {have})")]
    Truncated {
        path: PathBuf,
        context: String,
        need: usize,
        have: usize,
    },

    /// 生成元 ABI を一意に決められない。
    #[error("{path}: 生成元の ABI を判別できません ({detail})")]
    AmbiguousAbi { path: PathBuf, detail: String },

    /// ヘッダの記述が内部矛盾している。
    #[error("{path}: ヘッダの内容が矛盾しています ({detail})")]
    InconsistentHeader { path: PathBuf, detail: String },

    /// レコード境界を失った。
    #[error("{path}: レコード境界を失いました (offset={offset}, {detail})")]
    RecordBoundaryLost {
        path: PathBuf,
        offset: u64,
        detail: String,
    },

    /// サニティチェック上限の超過。
    #[error("{path}: {what} が上限を超えています ({value} > {limit})")]
    LimitExceeded {
        path: PathBuf,
        what: String,
        value: u64,
        limit: u64,
    },

    /// レイアウト記述の解決に失敗した (実装側の不整合)。
    #[error("レイアウト定義の不整合: {0}")]
    Layout(#[from] LayoutError),

    /// 出力先への書き込みに失敗した。
    ///
    /// 入力ファイルの読み取りエラーは [`Error::Io`] (パス付き) を使う。
    /// こちらは `stdout` やパイプへの書き込みなど、パスを持たない経路のためのもの。
    #[error("出力に失敗しました: {0}")]
    Write(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

/// レイアウト記述を解決する際のエラー。
///
/// 入力データではなく実装側の宣言に問題がある場合に返る。
#[derive(Debug, thiserror::Error)]
pub enum LayoutError {
    #[error("{layout}: フィールド {field} のアラインメント {align} が 2 の冪ではありません")]
    AlignNotPowerOfTwo {
        layout: &'static str,
        field: &'static str,
        align: u16,
    },

    #[error("{layout}: 構造体アラインメント {align} が 2 の冪ではありません")]
    StructAlignNotPowerOfTwo { layout: &'static str, align: u16 },

    #[error("{layout}: サイズ計算が桁溢れしました (field={field})")]
    SizeOverflow {
        layout: &'static str,
        field: &'static str,
    },

    #[error(
        "{layout}: 実測サイズ {computed} が期待値 {expected} と一致しません (abi={abi}, endian={endian})"
    )]
    SizeMismatch {
        layout: &'static str,
        computed: usize,
        expected: usize,
        abi: &'static str,
        endian: &'static str,
    },

    #[error(
        "{layout}: フィールド {field} の実測オフセット {computed} が期待値 {expected} と一致しません"
    )]
    OffsetMismatch {
        layout: &'static str,
        field: &'static str,
        computed: usize,
        expected: usize,
    },
}

pub type Result<T> = std::result::Result<T, Error>;
