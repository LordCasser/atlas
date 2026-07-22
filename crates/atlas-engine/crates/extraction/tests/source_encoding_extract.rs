//! Integration: non-UTF-8 source → `workspace::read_source` → tree-sitter extract.
//!
//! Contract: `docs/testing.md` §2.1.1 and §2.3.
//!
//! Asserts that Chinese identifiers in a **GBK** on-disk file survive decode and
//! appear as symbol names in `FileFacts` (not mojibake), and that extraction is
//! fed `content_hash = file_hash` (raw bytes).

use std::path::Path;

use encoding_rs::GBK;
use extraction::{ExtractionMode, create_frontend, extract_file_with_mode};
use types::Language;
use types::ids::FileId;
use workspace::{file_content_hash, read_source, text_content_hash};

/// Logical UTF-8 Python source with Chinese identifiers (must remain encodable as GBK).
const PY_UTF8: &str = "\
# 中文模块注释：编码解析联调\n\
# 足够汉字以便 chardetng 识别为 GBK 族\n\
\n\
def 计算总和(a, b):\n\
    return a + b\n\
\n\
class 数据服务:\n\
    def 查询(self, key):\n\
        return key\n\
\n\
def helper_local():\n\
    def 内部函数():\n\
        return 1\n\
    return 内部函数\n\
";

fn write_gbk_file(dir: &Path, rel: &str, utf8: &str) -> std::path::PathBuf {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let (encoded, _, unmappable) = GBK.encode(utf8);
    assert!(!unmappable, "fixture not fully GBK-encodable");
    let raw = encoded.as_ref();
    assert!(
        std::str::from_utf8(raw).is_err(),
        "fixture must be non-UTF-8 on disk"
    );
    std::fs::write(&path, raw).unwrap();
    path
}

#[test]
fn gbk_python_extract_preserves_chinese_symbol_names() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_gbk_file(dir.path(), "main.py", PY_UTF8);

    // Product path: unified reader only (never read_to_string on non-UTF-8).
    let src = read_source(&path).expect("read_source GBK python");
    assert!(
        src.encoding.eq_ignore_ascii_case("GBK") || src.encoding.eq_ignore_ascii_case("GB18030"),
        "encoding={}",
        src.encoding
    );
    assert!(src.text.contains("计算总和"));
    assert_eq!(
        src.file_hash,
        file_content_hash(&std::fs::read(&path).unwrap())
    );
    assert_ne!(src.file_hash, src.text_hash());

    let frontend = create_frontend(Language::Python).expect("python frontend");
    let file_id = FileId::generate("main.py");

    // content_hash MUST be raw file identity (same as dirty / files.content_hash).
    let facts = extract_file_with_mode(
        &frontend,
        file_id,
        Path::new("main.py"),
        &src.text,
        &src.file_hash,
        ExtractionMode::Structural,
        &(),
    )
    .expect("extract structural from decoded GBK source");

    assert_eq!(
        facts.file.content_hash, src.file_hash,
        "FileFacts must store raw file_hash, not text_hash"
    );
    assert_ne!(
        facts.file.content_hash,
        text_content_hash(src.text.as_bytes())
    );

    let names: Vec<&str> = facts.symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"计算总和"),
        "expected function 计算总和 in symbols, got {names:?}"
    );
    assert!(
        names.contains(&"数据服务"),
        "expected class 数据服务 in symbols, got {names:?}"
    );
    assert!(
        names.contains(&"查询"),
        "expected method 查询 in symbols, got {names:?}"
    );

    // Disk still original GBK after full read+extract path.
    let on_disk = std::fs::read(&path).unwrap();
    let (expected_raw, _, _) = GBK.encode(PY_UTF8);
    assert_eq!(on_disk, expected_raw.as_ref());
}

#[test]
fn gbk_python_manifest_top_level_chinese_names() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_gbk_file(dir.path(), "mod.py", PY_UTF8);
    let src = read_source(&path).expect("read_source");

    let frontend = create_frontend(Language::Python).expect("python frontend");
    let facts = extract_file_with_mode(
        &frontend,
        FileId::generate("mod.py"),
        Path::new("mod.py"),
        &src.text,
        &src.file_hash,
        ExtractionMode::Manifest,
        &(),
    )
    .expect("manifest extract");

    let top: Vec<&str> = facts.symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(
        top.contains(&"计算总和"),
        "manifest should include top-level 计算总和: {top:?}"
    );
    assert!(
        top.contains(&"数据服务"),
        "manifest should include top-level 数据服务: {top:?}"
    );
    // Encoding-focused: Chinese names must not be mojibake even in Manifest mode.
    assert!(
        !top.iter().any(|n| n.contains('\u{FFFD}')),
        "manifest symbol names must not contain U+FFFD: {top:?}"
    );
}

#[test]
fn utf8_python_still_extracts_chinese_names() {
    // Control: same logical source as UTF-8 on disk (hot path).
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("utf8_main.py");
    std::fs::write(&path, PY_UTF8.as_bytes()).unwrap();

    let src = read_source(&path).expect("read_source utf8");
    assert_eq!(src.encoding, "UTF-8");
    assert_eq!(src.file_hash, src.text_hash());

    let frontend = create_frontend(Language::Python).expect("python frontend");
    let facts = extract_file_with_mode(
        &frontend,
        FileId::generate("utf8_main.py"),
        Path::new("utf8_main.py"),
        &src.text,
        &src.file_hash,
        ExtractionMode::Structural,
        &(),
    )
    .expect("extract");

    assert!(facts.symbols.iter().any(|s| s.name == "计算总和"));
}
