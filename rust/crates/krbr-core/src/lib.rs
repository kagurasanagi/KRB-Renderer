//! KRB-Renderer の土台：ログ、パス（設定・キャッシュの場所）、ハッシュ。
//! C++ 版の `core/` に相当し、ファイルの置き場所と XXH64 のキーは C++ 版と同じにしてある（キャッシュを共有するため）。

pub mod log;
pub mod platform;

/// XXH64（seed 0）。C++ 版の `krbr::xxh64` と同じ値になる
pub fn xxh64(data: &[u8]) -> u64 {
    xxhash_rust::xxh64::xxh64(data, 0)
}

/// 16 桁の小文字 16 進（C++ 版の `toHex`）
pub fn to_hex(v: u64) -> String {
    format!("{v:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_is_16_lowercase_digits() {
        assert_eq!(to_hex(0), "0000000000000000");
        assert_eq!(to_hex(0xdead_beef), "00000000deadbeef");
    }

    #[test]
    fn xxh64_known_value() {
        // 公式テストベクタ：空列の seed 0
        assert_eq!(xxh64(b""), 0xef46_db37_51d8_e999);
    }
}
