//! Unified source-file reader: decode to UTF-8 in memory, never rewrite disk.
//!
//! # Hash policy
//!
//! - **File identity** ([`SourceText::file_hash`]): `blake3` of **raw on-disk bytes**.
//!   Used for dirty detection, fingerprints, `files.content_hash`, stale checks.
//! - **Content digests** ([`text_content_hash`]): `blake3` of **decoded UTF-8**
//!   text (or a slice thereof). Used when hashing symbol bodies / snippets.
//!
//! All Atlas source-reading paths must go through [`read_source`] /
//! [`decode_source`]. Do not call `std::fs::read_to_string` on source files.
//!
//! Required tests: `docs/source-encoding.md` §6, `docs/testing.md` §2.1.1.

use std::path::Path;

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::Encoding;

/// Decoded source text plus identity metadata.
///
/// The original file on disk is never modified.
#[derive(Debug, Clone)]
pub struct SourceText {
    /// UTF-8 source for parsing, ranges, and display (in-memory only).
    pub text: String,
    /// `blake3` hex of raw on-disk bytes (file identity).
    pub file_hash: String,
    /// Detected/used encoding name (e.g. `"UTF-8"`, `"GBK"`, `"windows-1252"`).
    pub encoding: &'static str,
    /// Whether `encoding_rs` substituted replacement characters while decoding.
    pub had_errors: bool,
}

impl SourceText {
    /// `blake3` hex of the full decoded UTF-8 text.
    pub fn text_hash(&self) -> String {
        text_content_hash(self.text.as_bytes())
    }
}

/// `blake3` hex of decoded UTF-8 content (full file or a partial slice).
pub fn text_content_hash(utf8_bytes: &[u8]) -> String {
    blake3::hash(utf8_bytes).to_hex().to_string()
}

/// `blake3` hex of raw file bytes (same as [`SourceText::file_hash`]).
pub fn file_content_hash(raw_bytes: &[u8]) -> String {
    blake3::hash(raw_bytes).to_hex().to_string()
}

/// Read a source file from disk: raw bytes → optional charset decode → UTF-8.
///
/// Never writes back to `path`.
pub fn read_source(path: &Path) -> anyhow::Result<SourceText> {
    let bytes = std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("failed to read source {}: {e}", path.display()))?;
    Ok(decode_source(&bytes))
}

/// Decode already-loaded raw file bytes into UTF-8 [`SourceText`].
pub fn decode_source(bytes: &[u8]) -> SourceText {
    let file_hash = file_content_hash(bytes);
    let (text, encoding, had_errors) = decode_bytes_to_utf8(bytes);
    SourceText {
        text,
        file_hash,
        encoding,
        had_errors,
    }
}

/// Core decode: UTF-8 fast path, then chardetng + encoding_rs.
///
/// Prioritizes correct handling of legacy Chinese (GBK/GB18030) and
/// Western European (ISO-8859-1 / windows-1252) sources.
fn decode_bytes_to_utf8(bytes: &[u8]) -> (String, &'static str, bool) {
    // Hot path: valid UTF-8 (includes pure ASCII).
    if let Ok(s) = std::str::from_utf8(bytes) {
        return (s.to_owned(), "UTF-8", false);
    }

    let mut detector = EncodingDetector::new(Iso2022JpDetection::Deny);
    detector.feed(bytes, true);
    let encoding: &'static Encoding = detector.guess(None, Utf8Detection::Allow);

    let (cow, used, had_errors) = encoding.decode(bytes);
    (cow.into_owned(), used.name(), had_errors)
}

#[cfg(test)]
mod tests {
    //! Normative unit matrix for `docs/source-encoding.md` §6.1 and
    //! `docs/testing.md` §2.1.1. Do not weaken assertions without updating both docs.

    use super::*;
    use encoding_rs::{GBK, WINDOWS_1252};

    // ── Fixture builders (expected UTF-8 is the golden; raw is encoded) ──

    /// Logical UTF-8 body used for GBK Chinese source-file scenarios.
    /// Enough Han characters so chardetng prefers GBK over Latin single-byte.
    const GBK_SOURCE_UTF8: &str = "\
# 这是中文注释：源文件编码与解析回归测试用例\n\
# 覆盖：读取、解码、hash 语义、符号标识符\n\
def 计算总和(a, b):\n\
    \"\"\"返回两数之和\"\"\"\n\
    return a + b\n\
\n\
class 数据服务:\n\
    def 查询(self, key):\n\
        return key\n\
";

    const LATIN1_SOURCE_UTF8: &str = "\
// café résumé naïve — Western European 8-bit source\n\
// commentaires: année, hôtel, façade\n\
fn parse_resume() {}\n\
";

    fn encode_gbk(utf8: &str) -> Vec<u8> {
        let (cow, _, had_unmappable) = GBK.encode(utf8);
        assert!(
            !had_unmappable,
            "fixture must be fully encodable as GBK: {utf8:?}"
        );
        let raw = cow.into_owned();
        assert!(
            std::str::from_utf8(&raw).is_err(),
            "GBK fixture must not be valid UTF-8"
        );
        raw
    }

    fn encode_windows_1252(utf8: &str) -> Vec<u8> {
        let (cow, _, had_unmappable) = WINDOWS_1252.encode(utf8);
        assert!(
            !had_unmappable,
            "fixture must be fully encodable as windows-1252"
        );
        let raw = cow.into_owned();
        assert!(
            std::str::from_utf8(&raw).is_err(),
            "latin1-class fixture must not be valid UTF-8"
        );
        raw
    }

    fn assert_gbk_family(encoding: &str) {
        let ok = encoding.eq_ignore_ascii_case("GBK")
            || encoding.eq_ignore_ascii_case("GB18030")
            || encoding.eq_ignore_ascii_case("gbk")
            || encoding.eq_ignore_ascii_case("gb18030");
        assert!(ok, "expected GBK/GB18030 family, got {encoding}");
    }

    fn assert_western_8bit(encoding: &str) {
        let ok = encoding == "windows-1252"
            || encoding == "ISO-8859-1"
            || encoding == "ISO-8859-15";
        assert!(ok, "expected western 8-bit encoding, got {encoding}");
    }

    // ── §6.1 UTF-8 hot path ──────────────────────────────────────────────

    #[test]
    fn utf8_chinese_preserves_text_and_dual_hash_equality() {
        let raw = "fn main() { /* 中文注释 */ }\n".as_bytes();
        let src = decode_source(raw);

        assert_eq!(src.encoding, "UTF-8");
        assert!(!src.had_errors);
        assert!(src.text.contains("中文注释"));
        assert_eq!(src.file_hash, file_content_hash(raw));
        assert_eq!(src.text_hash(), text_content_hash(src.text.as_bytes()));
        // Pure UTF-8: file identity hash equals full-text content hash.
        assert_eq!(src.file_hash, src.text_hash());
    }

    #[test]
    fn utf8_pure_ascii_hot_path() {
        let raw = b"int main(void) { return 0; }\n";
        let src = decode_source(raw);
        assert_eq!(src.encoding, "UTF-8");
        assert!(!src.had_errors);
        assert_eq!(src.text.as_bytes(), raw);
        assert_eq!(src.file_hash, src.text_hash());
    }

    #[test]
    fn utf8_empty_file() {
        let src = decode_source(b"");
        assert_eq!(src.encoding, "UTF-8");
        assert_eq!(src.text, "");
        assert_eq!(src.file_hash, file_content_hash(b""));
        assert_eq!(src.file_hash, src.text_hash());
    }

    // ── §6.1 GBK Chinese ─────────────────────────────────────────────────

    #[test]
    fn gbk_chinese_decodes_to_expected_utf8() {
        let raw = encode_gbk(GBK_SOURCE_UTF8);
        let src = decode_source(&raw);

        assert_gbk_family(src.encoding);
        assert!(
            src.text.contains("计算总和")
                && src.text.contains("数据服务")
                && src.text.contains("源文件编码"),
            "decoded text missing expected Chinese: {:?}",
            src.text
        );
        // Round-trip body: decode should recover the logical UTF-8 source.
        assert_eq!(src.text, GBK_SOURCE_UTF8);
    }

    #[test]
    fn gbk_file_hash_is_raw_not_decoded() {
        let raw = encode_gbk(GBK_SOURCE_UTF8);
        let src = decode_source(&raw);

        assert_eq!(
            src.file_hash,
            file_content_hash(&raw),
            "file_hash must be blake3(raw)"
        );
        assert_eq!(src.file_hash, blake3::hash(&raw).to_hex().to_string());
        assert_ne!(
            src.file_hash,
            src.text_hash(),
            "GBK raw must not equal UTF-8 text hash (prevents permanent dirty)"
        );
        assert_ne!(
            src.file_hash,
            text_content_hash(GBK_SOURCE_UTF8.as_bytes()),
            "must not hash decoded UTF-8 as file identity"
        );
    }

    // ── §6.1 Western 8-bit (ISO-8859-1 class / windows-1252) ─────────────

    #[test]
    fn western_8bit_decodes_latin_characters() {
        let raw = encode_windows_1252(LATIN1_SOURCE_UTF8);
        let src = decode_source(&raw);

        assert_western_8bit(src.encoding);
        assert!(
            src.text.contains("café") && src.text.contains("résumé") && src.text.contains("naïve"),
            "decoded text: {:?}",
            src.text
        );
        assert_eq!(src.file_hash, file_content_hash(&raw));
        assert_ne!(src.file_hash, src.text_hash());
    }

    // ── §6.1 partial content hash ────────────────────────────────────────

    #[test]
    fn text_content_hash_uses_decoded_utf8_slice() {
        let raw = encode_gbk(GBK_SOURCE_UTF8);
        let src = decode_source(&raw);

        // Symbol-name slice as it would appear after decode (partial content).
        let name = "计算总和";
        assert!(src.text.contains(name));
        let partial = name.as_bytes();

        assert_eq!(
            text_content_hash(partial),
            blake3::hash(partial).to_hex().to_string()
        );
        assert_ne!(
            text_content_hash(partial),
            file_content_hash(&raw),
            "partial UTF-8 digest must not equal raw file hash"
        );
        // Partial digests of different slices differ.
        assert_ne!(
            text_content_hash("计算总和".as_bytes()),
            text_content_hash("数据服务".as_bytes())
        );
    }

    // ── §6.1 read_source: no rewrite ─────────────────────────────────────

    #[test]
    fn read_source_does_not_rewrite_disk_gbk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.py");
        let raw = encode_gbk(GBK_SOURCE_UTF8);
        std::fs::write(&path, &raw).unwrap();

        let src = read_source(&path).expect("read_source");
        assert_eq!(src.text, GBK_SOURCE_UTF8);
        assert_gbk_family(src.encoding);
        assert_eq!(src.file_hash, file_content_hash(&raw));

        let on_disk = std::fs::read(&path).unwrap();
        assert_eq!(
            on_disk, raw,
            "read_source must never rewrite the original file"
        );
    }

    #[test]
    fn read_source_does_not_rewrite_disk_western() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latin.c");
        let raw = encode_windows_1252(LATIN1_SOURCE_UTF8);
        std::fs::write(&path, &raw).unwrap();

        let src = read_source(&path).expect("read_source");
        assert!(src.text.contains("café"));
        assert_eq!(std::fs::read(&path).unwrap(), raw);
    }

    #[test]
    fn read_source_missing_file_errors() {
        let err = read_source(Path::new("/nonexistent/atlas_source_encoding_test.py")).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("failed to read") || msg.contains("No such file"),
            "unexpected error: {msg}"
        );
    }

    // ── Hash API consistency ─────────────────────────────────────────────

    #[test]
    fn file_content_hash_matches_source_text_file_hash() {
        for raw in [
            b"ascii\n".as_slice(),
            "中文 UTF-8\n".as_bytes(),
            encode_gbk(GBK_SOURCE_UTF8).as_slice(),
        ] {
            let src = decode_source(raw);
            assert_eq!(src.file_hash, file_content_hash(raw));
        }
    }
}
