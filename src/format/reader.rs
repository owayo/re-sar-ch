//! バイト列からの境界検査付き読み出し。
//!
//! 解決済みの [`PlacedField`] を通して読むため、ホットパスに
//! レイアウト解釈やフィールド名の照合が入らない。

use super::abi::Endian;
use super::wire::{FieldTy, PlacedField};

/// 範囲外アクセス。呼び出し側で `Error::Truncated` へ変換する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutOfBounds {
    pub offset: usize,
    pub need: usize,
    pub len: usize,
}

pub type ReadResult<T> = Result<T, OutOfBounds>;

/// バイト順を伴うバイト列ビュー。
#[derive(Debug, Clone, Copy)]
pub struct Cursor<'a> {
    buf: &'a [u8],
    endian: Endian,
}

impl<'a> Cursor<'a> {
    #[inline]
    pub fn new(buf: &'a [u8], endian: Endian) -> Self {
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

    #[inline]
    pub fn bytes(&self) -> &'a [u8] {
        self.buf
    }

    /// 指定範囲の部分ビューを得る。
    #[inline]
    pub fn slice(&self, offset: usize, len: usize) -> ReadResult<Cursor<'a>> {
        let end = offset.checked_add(len).ok_or(OutOfBounds {
            offset,
            need: len,
            len: self.buf.len(),
        })?;
        if end > self.buf.len() {
            return Err(OutOfBounds {
                offset,
                need: len,
                len: self.buf.len(),
            });
        }
        Ok(Cursor {
            buf: &self.buf[offset..end],
            endian: self.endian,
        })
    }

    #[inline]
    pub fn raw(&self, offset: usize, len: usize) -> ReadResult<&'a [u8]> {
        let end = offset.checked_add(len).ok_or(OutOfBounds {
            offset,
            need: len,
            len: self.buf.len(),
        })?;
        if end > self.buf.len() {
            return Err(OutOfBounds {
                offset,
                need: len,
                len: self.buf.len(),
            });
        }
        Ok(&self.buf[offset..end])
    }

    #[inline]
    fn array<const N: usize>(&self, offset: usize) -> ReadResult<[u8; N]> {
        let s = self.raw(offset, N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(s);
        Ok(out)
    }

    #[inline]
    pub fn u8_at(&self, offset: usize) -> ReadResult<u8> {
        self.raw(offset, 1).map(|s| s[0])
    }

    #[inline]
    pub fn u16_at(&self, offset: usize) -> ReadResult<u16> {
        self.array::<2>(offset).map(|b| self.endian.u16_from(b))
    }

    #[inline]
    pub fn u32_at(&self, offset: usize) -> ReadResult<u32> {
        self.array::<4>(offset).map(|b| self.endian.u32_from(b))
    }

    #[inline]
    pub fn u64_at(&self, offset: usize) -> ReadResult<u64> {
        self.array::<8>(offset).map(|b| self.endian.u64_from(b))
    }

    /// NUL 終端のフィールドを**バイト列のまま**返す。
    ///
    /// Linux のデバイス名やマウントパスが UTF-8 である保証はない。
    /// item の同一性判定は**このバイト列で行うこと**。UTF-8 として解釈した文字列で
    /// 比べると、不正バイトを含む別々の名前が同一視される危険がある。
    pub fn cbytes_at(&self, offset: usize, capacity: usize) -> ReadResult<&'a [u8]> {
        let raw = self.raw(offset, capacity)?;
        let end = raw.iter().position(|&b| b == 0).unwrap_or(capacity);
        // 文字列フィールドはバイト順の影響を受けない
        Ok(&raw[..end])
    }

    /// NUL 終端の文字列フィールドを UTF-8 として読む。
    ///
    /// **不正なバイトを含む場合は `None`** を返す。
    /// 以前は「不正バイトの手前まで」を返していたが、それでは
    /// `dev\xffA` と `dev\xfeB` がどちらも `"dev"` になり、
    /// 別の item を同一視してしまう。
    /// 表示だけが目的なら [`Cursor::cstr_lossy_at`] を使う。
    pub fn cstr_at(&self, offset: usize, capacity: usize) -> ReadResult<Option<&'a str>> {
        let raw = self.cbytes_at(offset, capacity)?;
        Ok(std::str::from_utf8(raw).ok())
    }

    /// NUL 終端のフィールドを、表示用に置換文字つきで読む。
    ///
    /// 不正バイトは U+FFFD になる。**同一性判定には使わないこと**
    /// (異なるバイト列が同じ文字列になり得る)。
    pub fn cstr_lossy_at(
        &self,
        offset: usize,
        capacity: usize,
    ) -> ReadResult<std::borrow::Cow<'a, str>> {
        let raw = self.cbytes_at(offset, capacity)?;
        Ok(String::from_utf8_lossy(raw))
    }

    /// 解決済みフィールドを符号なし 64bit へゼロ拡張して読む。
    ///
    /// wire 上の幅が 4 バイトでも、値は `u64` として扱う。
    /// 元のビット幅は差分計算のラップ判定に必要なので
    /// 呼び出し側が [`PlacedField::width`] から取得する。
    #[inline]
    pub fn read_unsigned(&self, base: usize, f: &PlacedField) -> ReadResult<u64> {
        let off = base.checked_add(f.offset).ok_or(OutOfBounds {
            offset: base,
            need: f.width,
            len: self.buf.len(),
        })?;
        // 読むのは占有幅ではなく「有効バイト数」。unsigned long のスロットは
        // 常に 8 バイトだが、32bit ライタのファイルでは先頭 4 バイトだけが値。
        match f.value_width {
            1 => self.u8_at(off).map(u64::from),
            2 => self.u16_at(off).map(u64::from),
            4 => self.u32_at(off).map(u64::from),
            8 => self.u64_at(off),
            n => {
                // 想定外の幅。範囲検査だけ行って先頭バイトから組み立てる。
                let raw = self.raw(off, n)?;
                let mut v: u64 = 0;
                match self.endian {
                    Endian::Little => {
                        for (i, b) in raw.iter().enumerate().take(8) {
                            v |= (*b as u64) << (8 * i);
                        }
                    }
                    Endian::Big => {
                        for b in raw.iter().take(8) {
                            v = (v << 8) | *b as u64;
                        }
                    }
                }
                Ok(v)
            }
        }
    }

    /// 解決済みフィールドを符号付き 64bit へ符号拡張して読む。
    #[inline]
    pub fn read_signed(&self, base: usize, f: &PlacedField) -> ReadResult<i64> {
        let raw = self.read_unsigned(base, f)?;
        Ok(match f.value_width {
            1 => raw as u8 as i8 as i64,
            2 => raw as u16 as i16 as i64,
            4 => raw as u32 as i32 as i64,
            _ => raw as i64,
        })
    }

    /// 型に応じて符号を判断して読む。
    #[inline]
    pub fn read_int(&self, base: usize, f: &PlacedField) -> ReadResult<i128> {
        if f.ty.is_signed() {
            self.read_signed(base, f).map(i128::from)
        } else {
            self.read_unsigned(base, f).map(i128::from)
        }
    }

    /// 文字列フィールドを UTF-8 として読む。不正バイトを含む場合は `None`。
    ///
    /// item の同一性判定にはこの結果を使ってよい (不正な名前は `None` になり、
    /// 別の名前と同一視されない)。表示だけなら [`Cursor::read_str_lossy`] を使う。
    #[inline]
    pub fn read_str(&self, base: usize, f: &PlacedField) -> ReadResult<Option<&'a str>> {
        let (off, cap) = self.text_span(base, f)?;
        self.cstr_at(off, cap)
    }

    /// 文字列フィールドをバイト列のまま読む。
    #[inline]
    pub fn read_bytes(&self, base: usize, f: &PlacedField) -> ReadResult<&'a [u8]> {
        let (off, cap) = self.text_span(base, f)?;
        self.cbytes_at(off, cap)
    }

    /// 文字列フィールドを表示用に読む (不正バイトは U+FFFD)。
    #[inline]
    pub fn read_str_lossy(
        &self,
        base: usize,
        f: &PlacedField,
    ) -> ReadResult<std::borrow::Cow<'a, str>> {
        let (off, cap) = self.text_span(base, f)?;
        self.cstr_lossy_at(off, cap)
    }

    #[inline]
    fn text_span(&self, base: usize, f: &PlacedField) -> ReadResult<(usize, usize)> {
        let off = base.checked_add(f.offset).ok_or(OutOfBounds {
            offset: base,
            need: f.width,
            len: self.buf.len(),
        })?;
        let cap = match f.ty {
            FieldTy::Bytes(n) => n as usize,
            _ => f.width,
        };
        Ok((off, cap))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::wire::FieldTy;

    fn placed(offset: usize, width: usize, ty: FieldTy) -> PlacedField {
        PlacedField {
            name: "f",
            offset,
            width,
            value_width: width,
            ty,
        }
    }

    /// unsigned long のスロット (占有 8 / 有効 4) を表す。
    fn ul_slot_32bit(offset: usize) -> PlacedField {
        PlacedField {
            name: "ul",
            offset,
            width: 8,
            value_width: 4,
            ty: FieldTy::CULong,
        }
    }

    #[test]
    fn reads_both_endians() {
        let buf = [0x01u8, 0x02, 0x03, 0x04];
        assert_eq!(Cursor::new(&buf, Endian::Little).u32_at(0), Ok(0x04030201));
        assert_eq!(Cursor::new(&buf, Endian::Big).u32_at(0), Ok(0x01020304));
    }

    #[test]
    fn out_of_bounds_is_reported_not_panicking() {
        let buf = [0u8; 4];
        let c = Cursor::new(&buf, Endian::Little);
        assert!(c.u64_at(0).is_err());
        assert!(c.u32_at(1).is_err());
        assert_eq!(
            c.u32_at(4),
            Err(OutOfBounds {
                offset: 4,
                need: 4,
                len: 4
            })
        );
    }

    /// 32bit ライタの unsigned long: スロット 8 バイトのうち先頭 4 バイトだけが値。
    #[test]
    fn reads_leading_half_of_ul_slot_little_endian() {
        let buf = [0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0];
        let c = Cursor::new(&buf, Endian::Little);
        assert_eq!(c.read_unsigned(0, &ul_slot_32bit(0)), Ok(0xffff_ffff));
    }

    /// big endian 32bit でも「スロット先頭 4 バイトを BE で読む」で正しくなる。
    ///
    /// 出所: 本家 `tests/data-ppc-11.7.2` は sa_sizeof_long=4 の PowerPC が書いた
    /// ファイルで、sa_hz の値 100 がスロット先頭に BE u32 (00 00 00 64) として入り、
    /// 後続 4 バイトがゼロになっている。8 バイトを BE u64 として読むと
    /// 100 * 2^32 になってしまう。
    #[test]
    fn reads_leading_half_of_ul_slot_big_endian() {
        let buf = [0x00, 0x00, 0x00, 0x64, 0, 0, 0, 0];
        let c = Cursor::new(&buf, Endian::Big);
        assert_eq!(c.read_unsigned(0, &ul_slot_32bit(0)), Ok(100));
        // 誤って 8 バイト全部を読んだ場合との差を明示する
        assert_eq!(c.u64_at(0), Ok(100u64 << 32));
    }

    #[test]
    fn sign_extends_signed_fields() {
        let buf = [0xff, 0xff, 0xff, 0xff];
        let c = Cursor::new(&buf, Endian::Little);
        let f = placed(0, 4, FieldTy::I32);
        assert_eq!(c.read_signed(0, &f), Ok(-1));
        assert_eq!(c.read_int(0, &f), Ok(-1i128));
    }

    #[test]
    fn nul_terminated_string() {
        let mut buf = [0u8; 8];
        buf[..5].copy_from_slice(b"Linux");
        let c = Cursor::new(&buf, Endian::Little);
        assert_eq!(c.cstr_at(0, 8), Ok(Some("Linux")));
    }

    #[test]
    fn string_without_terminator_uses_full_capacity() {
        let buf = *b"abcd";
        let c = Cursor::new(&buf, Endian::Little);
        assert_eq!(c.cstr_at(0, 4), Ok(Some("abcd")));
    }

    /// 同じ値を LE / BE で表現しても、対応するバイト順で読めば一致する。
    #[test]
    fn endian_roundtrip_yields_same_value() {
        let v: u64 = 0x0123_4567_89ab_cdef;
        let le = v.to_le_bytes();
        let be = v.to_be_bytes();
        assert_eq!(Cursor::new(&le, Endian::Little).u64_at(0), Ok(v));
        assert_eq!(Cursor::new(&be, Endian::Big).u64_at(0), Ok(v));
    }
}
