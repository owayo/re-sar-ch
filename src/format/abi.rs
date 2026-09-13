//! 生成元ホストの ABI とバイト順。
//!
//! ここで扱う ABI は **`sa` ファイルを書き出したホスト**のものであり、
//! reSARch を実行しているホストのものではない。
//! `size_of::<c_ulong>()` などから決めてはいけない。

use std::fmt;

/// 整数のバイト順。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endian {
    Little,
    Big,
}

impl Endian {
    #[inline]
    pub fn u16_from(self, b: [u8; 2]) -> u16 {
        match self {
            Endian::Little => u16::from_le_bytes(b),
            Endian::Big => u16::from_be_bytes(b),
        }
    }

    #[inline]
    pub fn u32_from(self, b: [u8; 4]) -> u32 {
        match self {
            Endian::Little => u32::from_le_bytes(b),
            Endian::Big => u32::from_be_bytes(b),
        }
    }

    #[inline]
    pub fn u64_from(self, b: [u8; 8]) -> u64 {
        match self {
            Endian::Little => u64::from_le_bytes(b),
            Endian::Big => u64::from_be_bytes(b),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Endian::Little => "little",
            Endian::Big => "big",
        }
    }
}

impl fmt::Display for Endian {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 構造体レイアウトを決める ABI パラメータ。
///
/// `long_bytes` だけでは配置は決まらない。32bit ABI 同士でも 8 バイト整数の
/// 自然アラインメントが異なる (i386 は 4、ARM EABI は 8) ため、
/// アラインメントも独立したパラメータとして持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutAbi {
    /// 識別名 (診断・fixture 記録用)。
    pub name: &'static str,
    /// `unsigned long` / `long` のバイト幅。
    pub long_bytes: u8,
    /// `long` の自然アラインメント。
    pub long_align: u8,
    /// `long long` (8 バイト整数) の自然アラインメント。
    pub u64_align: u8,
    /// 構造体全体の最大自然アラインメント。
    pub max_align: u8,
}

impl LayoutAbi {
    /// x86_64 / aarch64 / ppc64 / s390x など、一般的な LP64。
    pub const LP64: Self = Self {
        name: "lp64",
        long_bytes: 8,
        long_align: 8,
        u64_align: 8,
        max_align: 8,
    };

    /// i386 / i686 System V。8 バイト整数のアラインメントが 4 である点が特徴。
    pub const I386: Self = Self {
        name: "i386",
        long_bytes: 4,
        long_align: 4,
        u64_align: 4,
        max_align: 4,
    };

    /// ARM EABI / PowerPC 32bit など、8 バイト整数を 8 境界に置く ILP32。
    pub const ILP32_ALIGN8: Self = Self {
        name: "ilp32-align8",
        long_bytes: 4,
        long_align: 4,
        u64_align: 8,
        max_align: 8,
    };

    /// `sa_machine` (uname の machine) と `sa_sizeof_long` から ABI を推定する。
    ///
    /// 判別できない場合は `None` を返す。呼び出し側が診断を出すか
    /// `Error::AmbiguousAbi` にするかを決める。
    pub fn infer(machine: &str, sizeof_long: u8) -> Option<Self> {
        let m = machine.trim().to_ascii_lowercase();

        if sizeof_long == 8 {
            // LP64 系。既知のアーキテクチャ名なら LP64 で確定する。
            return match m.as_str() {
                "x86_64" | "amd64" | "aarch64" | "arm64" | "ppc64" | "ppc64le" | "s390x"
                | "riscv64" | "ia64" | "sparc64" | "mips64" | "loongarch64" | "alpha" => {
                    Some(Self::LP64)
                }
                _ => Some(Self::LP64), // 64bit long を持つ ABI で max_align 8 以外は稀
            };
        }

        if sizeof_long == 4 {
            return match m.as_str() {
                // x86 32bit: long long は 4 境界
                "i386" | "i486" | "i586" | "i686" | "x86" | "i86pc" | "x86_64" | "amd64" => {
                    Some(Self::I386)
                }
                // ARM EABI / PowerPC / MIPS などは 8 境界
                s if s.starts_with("armv") || s == "arm" || s.starts_with("aarch32") => {
                    Some(Self::ILP32_ALIGN8)
                }
                "ppc" | "powerpc" | "ppcle" | "mips" | "mipsel" | "sparc" | "s390" | "riscv32"
                | "sh4" | "m68k" | "aarch64" | "arm64" | "ppc64" | "ppc64le" | "mips64"
                | "s390x" | "sparc64" | "riscv64" => Some(Self::ILP32_ALIGN8),
                _ => None,
            };
        }

        None
    }

    /// `FieldTy` のアラインメント解決に使う、この ABI 上での自然アラインメント。
    #[inline]
    pub fn natural_align(&self, width: usize) -> usize {
        match width {
            0 | 1 => 1,
            2 => 2,
            4 => 4,
            8 => self.u64_align as usize,
            // 文字列など幅の大きいバイト列は 1 バイト境界
            _ => 1,
        }
    }
}

/// バイト順と ABI の組。デコード計画の入力となる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceEncoding {
    pub endian: Endian,
    pub abi: LayoutAbi,
}

impl SourceEncoding {
    pub fn new(endian: Endian, abi: LayoutAbi) -> Self {
        Self { endian, abi }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endian_reads_both_orders() {
        assert_eq!(Endian::Little.u16_from([0x96, 0xd5]), 0xd596);
        assert_eq!(Endian::Big.u16_from([0xd5, 0x96]), 0xd596);
        assert_eq!(Endian::Little.u32_from([1, 0, 0, 0]), 1);
        assert_eq!(Endian::Big.u32_from([0, 0, 0, 1]), 1);
    }

    #[test]
    fn infers_lp64_from_x86_64() {
        let abi = LayoutAbi::infer("x86_64", 8).expect("x86_64 は判別できる");
        assert_eq!(abi.long_bytes, 8);
        assert_eq!(abi.u64_align, 8);
    }

    #[test]
    fn distinguishes_i386_from_arm_on_32bit() {
        let i386 = LayoutAbi::infer("i686", 4).expect("i686 は判別できる");
        let arm = LayoutAbi::infer("armv7l", 4).expect("armv7l は判別できる");
        // 8 バイト整数のアラインメントが ABI で異なる点が重要
        assert_eq!(i386.u64_align, 4);
        assert_eq!(arm.u64_align, 8);
    }

    #[test]
    fn unknown_32bit_machine_is_ambiguous() {
        assert!(LayoutAbi::infer("some-unknown-arch", 4).is_none());
    }
}
