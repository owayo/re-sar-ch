//! activity の識別子。
//!
//! sysstat は 43 種の activity を持ち、それぞれが固有の統計構造体を伴う。
//! 未知の ID が現れても解析を続けられるよう、列挙型ではなく数値の newtype にしている
//! (未知 activity はスキップ対象であり、エラーではない)。

use serde::Serialize;

/// activity の識別子 (`file_activity.id`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ActivityId(pub u32);

/// activity 数の健全性上限。
///
/// 既知の activity は 43 種だが、`sa_act_nr` の上限は本家の `MAX_NR_ACT` に合わせる。
/// 未知 activity を持つ新しいファイルを読めるようにするため、43 で打ち切らない。
pub const MAX_NR_ACT: u32 = 256;

/// 既知 activity の件数。
pub const NR_ACT: u32 = 43;

macro_rules! define_activities {
    ($( $konst:ident = $id:expr, $symbol:literal, $label:literal, $opt:literal ; )*) => {
        impl ActivityId {
            $( pub const $konst: ActivityId = ActivityId($id); )*

            /// 本家のシンボル名 (`A_CPU` など)。未知なら `None`。
            pub const fn symbol(self) -> Option<&'static str> {
                match self.0 {
                    $( $id => Some($symbol), )*
                    _ => None,
                }
            }

            /// 人間向けの名称。未知なら `None`。
            pub const fn label(self) -> Option<&'static str> {
                match self.0 {
                    $( $id => Some($label), )*
                    _ => None,
                }
            }

            /// この activity を選ぶ `sar` のオプション。未知なら `None`。
            pub const fn sar_option(self) -> Option<&'static str> {
                match self.0 {
                    $( $id => Some($opt), )*
                    _ => None,
                }
            }
        }

        /// 既知 activity の一覧 (ID 昇順)。
        pub const KNOWN_ACTIVITIES: &[ActivityId] = &[ $( ActivityId($id), )* ];
    };
}

define_activities! {
    CPU          =  1, "A_CPU",         "CPU 使用率",                          "-u";
    PCSW         =  2, "A_PCSW",        "プロセス生成・コンテキストスイッチ",   "-w";
    IRQ          =  3, "A_IRQ",         "割り込み",                            "-I";
    SWAP         =  4, "A_SWAP",        "スワップイン・アウト",                 "-W";
    PAGE         =  5, "A_PAGE",        "ページング",                          "-B";
    IO           =  6, "A_IO",          "I/O 転送レート",                      "-b";
    MEMORY       =  7, "A_MEMORY",      "メモリ・スワップ利用状況",             "-r";
    KTABLES      =  8, "A_KTABLES",     "カーネルテーブル",                     "-v";
    QUEUE        =  9, "A_QUEUE",       "ロードアベレージ・実行キュー",         "-q";
    SERIAL       = 10, "A_SERIAL",      "シリアルポート",                       "-y";
    DISK         = 11, "A_DISK",        "ブロックデバイス",                     "-d";
    NET_DEV      = 12, "A_NET_DEV",     "ネットワークインターフェース",         "-n DEV";
    NET_EDEV     = 13, "A_NET_EDEV",    "ネットワークインターフェースのエラー", "-n EDEV";
    NET_NFS      = 14, "A_NET_NFS",     "NFS クライアント",                     "-n NFS";
    NET_NFSD     = 15, "A_NET_NFSD",    "NFS サーバ",                           "-n NFSD";
    NET_SOCK     = 16, "A_NET_SOCK",    "ソケット (IPv4)",                      "-n SOCK";
    NET_IP       = 17, "A_NET_IP",      "IP トラフィック (IPv4)",               "-n IP";
    NET_EIP      = 18, "A_NET_EIP",     "IP エラー (IPv4)",                     "-n EIP";
    NET_ICMP     = 19, "A_NET_ICMP",    "ICMP トラフィック (IPv4)",             "-n ICMP";
    NET_EICMP    = 20, "A_NET_EICMP",   "ICMP エラー (IPv4)",                   "-n EICMP";
    NET_TCP      = 21, "A_NET_TCP",     "TCP トラフィック (IPv4)",              "-n TCP";
    NET_ETCP     = 22, "A_NET_ETCP",    "TCP エラー (IPv4)",                    "-n ETCP";
    NET_UDP      = 23, "A_NET_UDP",     "UDP トラフィック (IPv4)",              "-n UDP";
    NET_SOCK6    = 24, "A_NET_SOCK6",   "ソケット (IPv6)",                      "-n SOCK6";
    NET_IP6      = 25, "A_NET_IP6",     "IP トラフィック (IPv6)",               "-n IP6";
    NET_EIP6     = 26, "A_NET_EIP6",    "IP エラー (IPv6)",                     "-n EIP6";
    NET_ICMP6    = 27, "A_NET_ICMP6",   "ICMP トラフィック (IPv6)",             "-n ICMP6";
    NET_EICMP6   = 28, "A_NET_EICMP6",  "ICMP エラー (IPv6)",                   "-n EICMP6";
    NET_UDP6     = 29, "A_NET_UDP6",    "UDP トラフィック (IPv6)",              "-n UDP6";
    PWR_CPU      = 30, "A_PWR_CPU",     "CPU 動作周波数",                       "-m CPU";
    PWR_FAN      = 31, "A_PWR_FAN",     "ファン回転数",                         "-m FAN";
    PWR_TEMP     = 32, "A_PWR_TEMP",    "デバイス温度",                         "-m TEMP";
    PWR_IN       = 33, "A_PWR_IN",      "電圧入力",                             "-m IN";
    HUGE         = 34, "A_HUGE",        "HugePages 利用状況",                   "-H";
    PWR_FREQ     = 35, "A_PWR_FREQ",    "CPU 周波数の重み付き平均",             "-m FREQ";
    PWR_USB      = 36, "A_PWR_USB",     "USB デバイス",                         "-m USB";
    FS           = 37, "A_FS",          "ファイルシステム利用状況",             "-F";
    NET_FC       = 38, "A_NET_FC",      "ファイバチャネル HBA",                 "-n FC";
    NET_SOFT     = 39, "A_NET_SOFT",    "ソフトウェア割り込み (ネットワーク)",  "-n SOFT";
    PSI_CPU      = 40, "A_PSI_CPU",     "CPU の待ち圧力 (PSI)",                 "-q CPU";
    PSI_IO       = 41, "A_PSI_IO",      "I/O の待ち圧力 (PSI)",                 "-q IO";
    PSI_MEM      = 42, "A_PSI_MEM",     "メモリの待ち圧力 (PSI)",               "-q MEM";
    PWR_BAT      = 43, "A_PWR_BAT",     "バッテリ状態",                         "-m BAT";
}

impl ActivityId {
    #[inline]
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// 既知の activity か。
    #[inline]
    pub const fn is_known(self) -> bool {
        self.symbol().is_some()
    }

    /// 診断表示用。未知の場合は ID を含む文字列になる。
    pub fn display_name(self) -> String {
        match self.symbol() {
            Some(s) => s.to_string(),
            None => format!("A_UNKNOWN({})", self.0),
        }
    }
}

impl std::fmt::Display for ActivityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.symbol() {
            Some(s) => f.write_str(s),
            None => write!(f, "A_UNKNOWN({})", self.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_43_activities_are_defined() {
        assert_eq!(KNOWN_ACTIVITIES.len(), NR_ACT as usize);
        for (i, a) in KNOWN_ACTIVITIES.iter().enumerate() {
            assert_eq!(a.0, i as u32 + 1, "ID は 1 から連番であること");
            assert!(a.symbol().is_some());
            assert!(a.label().is_some());
            assert!(a.sar_option().is_some());
        }
    }

    #[test]
    fn known_ids_match_upstream_enum() {
        assert_eq!(ActivityId::CPU.0, 1);
        assert_eq!(ActivityId::DISK.0, 11);
        assert_eq!(ActivityId::HUGE.0, 34);
        assert_eq!(ActivityId::PWR_BAT.0, 43);
        assert_eq!(ActivityId::CPU.symbol(), Some("A_CPU"));
        assert_eq!(ActivityId::NET_DEV.sar_option(), Some("-n DEV"));
    }

    #[test]
    fn unknown_activity_is_not_an_error() {
        let unknown = ActivityId(200);
        assert!(!unknown.is_known());
        assert_eq!(unknown.symbol(), None);
        assert_eq!(unknown.display_name(), "A_UNKNOWN(200)");
    }

    /// activity 数の上限は既知件数ではなく本家の MAX_NR_ACT に合わせる。
    /// 43 で打ち切ると、未知 activity を持つ新しいファイルが読めなくなる。
    #[test]
    fn act_nr_limit_allows_future_activities() {
        assert!(MAX_NR_ACT > NR_ACT);
        assert_eq!(MAX_NR_ACT, 256);
    }
}
