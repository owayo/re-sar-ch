//! 解決済みレイアウトへの値の書き込み。
//!
//! [`super::reader::Cursor`] の対称形である。読み取りと同じ
//! [`PlacedField`] を通して書くので、「読めた位置に書く」ことが保証される。
//!
//! ## `unsigned long` を**バイト列のままコピーしてはいけない**理由
//!
//! `unsigned long` はディスク上で常に 8 バイトのスロットを占め、
//! 有効値はスロット**先頭** `sa_sizeof_long` バイトだけである
//! ([`super::wire::FieldTy::value_width`])。したがって
//!
//! - 32bit ビッグエンディアンのファイルで `unsigned long` = 100 は
//!   `00 00 00 64 | 00 00 00 00`
//! - 8 バイトまとめて `u64` (BE) として読むと `100 * 2^32`
//!
//! になる。本家がこの罠を `moveto_long_long()`
//! (`docs/format/01-file-format.md` §5.13) で「32 ビット回転」により回避しているのは、
//! **構造体をバイト列のまま持ち回る**実装だからである。
//!
//! reSARch は値を `u64` へ正規化してから出力側の型・幅で**再直列化**する。
//! 回転は不要になり、`arch_64` × `endian_mismatch` の 4 通りの場合分けも消える。
//!
//! ## 作業バッファは必ずゼロ初期化する
//!
//! 「旧構造体に無いフィールドが 0 になる」根拠はゼロ初期化だけである
//! (`docs/format/01-file-format.md` §5.5)。[`WriteCursor::zero`] か、
//! `vec![0u8; n]` で確保したバッファを渡すこと。

use super::abi::Endian;
use super::reader::OutOfBounds;
use super::wire::PlacedField;

/// バイト順を伴う書き込み用バイト列ビュー。
///
/// 範囲外への書き込みは [`OutOfBounds`] を返す。呼び出し側は
/// `Error::Truncated` など適切な分類へ写す。
#[derive(Debug)]
pub struct WriteCursor<'a> {
    buf: &'a mut [u8],
    endian: Endian,
}

impl<'a> WriteCursor<'a> {
    #[inline]
    pub fn new(buf: &'a mut [u8], endian: Endian) -> Self {
        Self { buf, endian }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    #[inline]
    pub fn endian(&self) -> Endian {
        self.endian
    }

    /// 書き込み済みのバイト列。
    #[inline]
    pub fn bytes(&self) -> &[u8] {
        self.buf
    }

    /// 指定範囲の可変スライスを得る。
    #[inline]
    fn span(&mut self, offset: usize, len: usize) -> Result<&mut [u8], OutOfBounds> {
        let total = self.buf.len();
        let end = offset.checked_add(len).ok_or(OutOfBounds {
            offset,
            need: len,
            len: total,
        })?;
        if end > total {
            return Err(OutOfBounds {
                offset,
                need: len,
                len: total,
            });
        }
        Ok(&mut self.buf[offset..end])
    }

    /// 指定範囲を 0 で埋める。
    #[inline]
    pub fn zero(&mut self, offset: usize, len: usize) -> Result<(), OutOfBounds> {
        self.span(offset, len)?.fill(0);
        Ok(())
    }

    /// 生バイト列をそのまま置く (バイト順の影響を受けない領域用)。
    #[inline]
    pub fn put_raw(&mut self, offset: usize, src: &[u8]) -> Result<(), OutOfBounds> {
        self.span(offset, src.len())?.copy_from_slice(src);
        Ok(())
    }

    /// 符号なし整数を `width` バイトでファイルのバイト順に置く。
    ///
    /// `width` が 8 未満の場合は**下位** `width` バイトだけを書く。
    /// 値が収まらない場合も切り詰めて書き、判定は [`value_fits`] に任せる
    /// (呼び出し側が「損失あり」を診断として集約できるようにするため)。
    pub fn put_uint(&mut self, offset: usize, width: usize, value: u64) -> Result<(), OutOfBounds> {
        let endian = self.endian;
        let dst = self.span(offset, width)?;
        match endian {
            Endian::Little => {
                for (i, b) in dst.iter_mut().enumerate() {
                    *b = if i < 8 { (value >> (8 * i)) as u8 } else { 0 };
                }
            }
            Endian::Big => {
                // 末尾が最下位バイト。`width` > 8 の余剰は先頭側を 0 で埋める。
                let n = dst.len();
                for (i, b) in dst.iter_mut().enumerate() {
                    let from_end = n - 1 - i;
                    *b = if from_end < 8 {
                        (value >> (8 * from_end)) as u8
                    } else {
                        0
                    };
                }
            }
        }
        Ok(())
    }

    // --- 幅ごとの糖衣 ---
    //
    // **4 つの幅を揃えて置く。参照が無いものも消さない。**
    // 読み手側の [`Cursor`](super::reader::Cursor) が `u8` / `u16` / `u32` / `u64` を
    // 揃えて持っているので、書き手側が幅を欠くと「読めるが書けない幅」ができる。
    // 現在の呼び出しは `put_u32` / `put_u64` だけだが、
    // `sa_day` (u8) や将来の u16 フィールドを書くときにここが要る。
    // dead-code 検査はこれらを未参照として挙げるが、対応は不要である。

    #[inline]
    pub fn put_u8(&mut self, offset: usize, value: u8) -> Result<(), OutOfBounds> {
        self.put_uint(offset, 1, value as u64)
    }

    #[inline]
    pub fn put_u16(&mut self, offset: usize, value: u16) -> Result<(), OutOfBounds> {
        self.put_uint(offset, 2, value as u64)
    }

    #[inline]
    pub fn put_u32(&mut self, offset: usize, value: u32) -> Result<(), OutOfBounds> {
        self.put_uint(offset, 4, value as u64)
    }

    #[inline]
    pub fn put_u64(&mut self, offset: usize, value: u64) -> Result<(), OutOfBounds> {
        self.put_uint(offset, 8, value)
    }

    /// 解決済みフィールドへ符号なし値を書く。
    ///
    /// 書くのは占有幅 (`width`) ではなく**有効バイト数** (`value_width`)。
    /// `unsigned long` のスロット後半は 0 のまま残り、32bit ライタが書いた
    /// ファイルと同じバイト像になる。**バッファがゼロ初期化されていることが前提**。
    #[inline]
    pub fn write_unsigned(
        &mut self,
        base: usize,
        f: &PlacedField,
        value: u64,
    ) -> Result<(), OutOfBounds> {
        let off = base.checked_add(f.offset).ok_or(OutOfBounds {
            offset: base,
            need: f.width,
            len: self.buf.len(),
        })?;
        self.put_uint(off, f.value_width, value)
    }

    /// 解決済みフィールドへ符号付き値を書く。
    ///
    /// 2 の補数表現をそのまま置くので、読み戻し
    /// ([`super::reader::Cursor::read_signed`]) で符号拡張されて元の値になる。
    #[inline]
    pub fn write_signed(
        &mut self,
        base: usize,
        f: &PlacedField,
        value: i64,
    ) -> Result<(), OutOfBounds> {
        self.write_unsigned(base, f, value as u64)
    }

    /// 解決済みの文字列 / バイト列フィールドへ書く。
    ///
    /// 入力が短ければ残りを NUL で埋め、長ければ切り詰める。
    /// **UTF-8 として解釈しない** (`data-non-printable` のように不正バイトを含む
    /// ファイルが実在する。デバイス名の同一性はバイト列で決まる)。
    ///
    /// 切り詰めた場合に末尾を NUL にするかは呼び出し側の判断に委ねる
    /// (本家はコメントだけ末尾 NUL を強制する)。
    #[inline]
    pub fn write_bytes(
        &mut self,
        base: usize,
        f: &PlacedField,
        src: &[u8],
    ) -> Result<(), OutOfBounds> {
        let off = base.checked_add(f.offset).ok_or(OutOfBounds {
            offset: base,
            need: f.width,
            len: self.buf.len(),
        })?;
        let dst = self.span(off, f.width)?;
        let n = src.len().min(dst.len());
        dst[..n].copy_from_slice(&src[..n]);
        dst[n..].fill(0);
        Ok(())
    }
}

/// 値がそのフィールドの有効幅に収まるか。
///
/// 収まらない場合は書けても値が壊れる。変換で `unsigned long long` から
/// `unsigned long` (32bit ライタのファイルでは 4 バイト) へ落ちる経路があるため、
/// 呼び出し側は損失を診断として残せる必要がある。
#[inline]
pub fn value_fits(f: &PlacedField, value: u64) -> bool {
    match f.value_width {
        w if w >= 8 => true,
        0 => value == 0,
        w => value >> (8 * w as u32) == 0,
    }
}

/// 符号付き値がそのフィールドの有効幅に収まるか。
#[inline]
pub fn signed_value_fits(f: &PlacedField, value: i64) -> bool {
    match f.value_width {
        w if w >= 8 => true,
        0 => value == 0,
        w => {
            let bits = 8 * w as u32;
            let min = -(1i64 << (bits - 1));
            let max = (1i64 << (bits - 1)) - 1;
            (min..=max).contains(&value)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::abi::{Endian, LayoutAbi, SourceEncoding};
    use crate::format::reader::Cursor;
    use crate::format::wire::{FieldTy, WireField, WireLayout};

    /// `unsigned long` 1 本だけの構造体。スロットは 8 バイト、有効値は ABI 依存。
    const UL_ONLY: WireLayout = WireLayout::new(
        "ul_only",
        &[
            WireField::aligned("value", FieldTy::CULong, 8),
            WireField::natural("tail", FieldTy::U32),
        ],
    );

    fn enc(endian: Endian, abi: LayoutAbi) -> SourceEncoding {
        SourceEncoding::new(endian, abi)
    }

    /// 書いた値が同じレイアウト・同じ符号化で読み戻せること (4 通り全部)。
    #[test]
    fn round_trips_unsigned_long_on_every_encoding() {
        for endian in [Endian::Little, Endian::Big] {
            for abi in [LayoutAbi::LP64, LayoutAbi::I386, LayoutAbi::ILP32_ALIGN8] {
                let e = enc(endian, abi);
                let layout = UL_ONLY.resolve(&e).unwrap();
                let f = layout.field("value").unwrap();

                let mut buf = vec![0u8; layout.size];
                let mut w = WriteCursor::new(&mut buf, endian);
                w.write_unsigned(0, f, 100).unwrap();

                let cur = Cursor::new(&buf, endian);
                assert_eq!(
                    cur.read_unsigned(0, f).unwrap(),
                    100,
                    "endian={endian} abi={}",
                    abi.name
                );
            }
        }
    }

    /// 32bit のスロットは**先頭**側が値。後半 4 バイトには触らない。
    ///
    /// ここを 8 バイトまとめて書くと、BE 32bit のファイルで
    /// `100` が `100 * 2^32` に化ける (本家 `moveto_long_long()` が扱う罠)。
    #[test]
    fn narrow_long_writes_only_the_leading_bytes() {
        let e = enc(Endian::Big, LayoutAbi::ILP32_ALIGN8);
        let layout = UL_ONLY.resolve(&e).unwrap();
        let f = layout.field("value").unwrap();
        assert_eq!(f.width, 8, "スロットは常に 8 バイト");
        assert_eq!(f.value_width, 4, "有効値は先頭 4 バイト");

        let mut buf = vec![0xffu8; layout.size];
        let mut w = WriteCursor::new(&mut buf, Endian::Big);
        w.zero(0, layout.size).unwrap();
        w.write_unsigned(0, f, 100).unwrap();

        assert_eq!(&buf[0..8], &[0, 0, 0, 100, 0, 0, 0, 0]);
    }

    /// 8 バイトを 1 回で書く経路 (LP64) も同じ値になる。
    #[test]
    fn wide_long_writes_the_whole_slot() {
        let e = enc(Endian::Big, LayoutAbi::LP64);
        let layout = UL_ONLY.resolve(&e).unwrap();
        let f = layout.field("value").unwrap();
        let mut buf = vec![0u8; layout.size];
        let mut w = WriteCursor::new(&mut buf, Endian::Big);
        w.write_unsigned(0, f, 0x0102_0304_0506_0708).unwrap();
        assert_eq!(&buf[0..8], &[1, 2, 3, 4, 5, 6, 7, 8]);
    }

    /// 符号付きフィールドは読み戻しで符号拡張されて元の値になる。
    #[test]
    fn signed_round_trip() {
        const LAYOUT: WireLayout = WireLayout::new(
            "signed",
            &[
                WireField::natural("a", FieldTy::I32),
                WireField::natural("b", FieldTy::I8),
            ],
        );
        for endian in [Endian::Little, Endian::Big] {
            let e = enc(endian, LayoutAbi::LP64);
            let layout = LAYOUT.resolve(&e).unwrap();
            let mut buf = vec![0u8; layout.size];
            let mut w = WriteCursor::new(&mut buf, endian);
            w.write_signed(0, layout.field("a").unwrap(), -5).unwrap();
            w.write_signed(0, layout.field("b").unwrap(), -1).unwrap();
            let cur = Cursor::new(&buf, endian);
            assert_eq!(cur.read_signed(0, layout.field("a").unwrap()).unwrap(), -5);
            assert_eq!(cur.read_signed(0, layout.field("b").unwrap()).unwrap(), -1);
        }
    }

    /// 文字列は短ければ NUL 埋め、長ければ切り詰め。UTF-8 検査はしない。
    #[test]
    fn bytes_are_padded_and_truncated_without_utf8_checks() {
        const LAYOUT: WireLayout =
            WireLayout::new("text", &[WireField::natural("name", FieldTy::Bytes(4))]);
        let e = enc(Endian::Little, LayoutAbi::LP64);
        let layout = LAYOUT.resolve(&e).unwrap();
        let f = layout.field("name").unwrap();

        let mut buf = vec![0xaau8; layout.size];
        let mut w = WriteCursor::new(&mut buf, Endian::Little);
        w.write_bytes(0, f, b"ab").unwrap();
        assert_eq!(&buf[0..4], b"ab\0\0");

        // 不正バイトを含んでもそのまま通す
        let mut w = WriteCursor::new(&mut buf, Endian::Little);
        w.write_bytes(0, f, &[0xff, 0xfe, 0x41, 0x42, 0x43])
            .unwrap();
        assert_eq!(&buf[0..4], &[0xff, 0xfe, 0x41, 0x42]);
    }

    /// 範囲外は書かずにエラーにする。
    #[test]
    fn out_of_bounds_is_reported() {
        let mut buf = vec![0u8; 4];
        let mut w = WriteCursor::new(&mut buf, Endian::Little);
        assert!(w.put_u64(0, 1).is_err());
        assert!(w.put_u32(1, 1).is_err());
        assert!(w.put_u32(0, 1).is_ok());
    }

    #[test]
    fn fit_checks_match_the_effective_width() {
        let e = enc(Endian::Little, LayoutAbi::I386);
        let layout = UL_ONLY.resolve(&e).unwrap();
        let f = layout.field("value").unwrap();
        assert!(value_fits(f, u32::MAX as u64));
        assert!(!value_fits(f, u32::MAX as u64 + 1));
        assert!(signed_value_fits(f, i32::MIN as i64));
        assert!(!signed_value_fits(f, i32::MIN as i64 - 1));

        let e64 = enc(Endian::Little, LayoutAbi::LP64);
        let layout64 = UL_ONLY.resolve(&e64).unwrap();
        let f64 = layout64.field("value").unwrap();
        assert!(value_fits(f64, u64::MAX));
    }
}
