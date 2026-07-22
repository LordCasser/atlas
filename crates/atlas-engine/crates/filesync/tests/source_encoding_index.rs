//! Index + dirty hash regression for non-UTF-8 sources.
//!
//! Contract: `docs/testing.md` §2.1.1 and §2.3.
//!
//! - Index a GBK Python file through the shared pipeline.
//! - DB `files.content_hash` equals raw on-disk blake3 (not decoded UTF-8 hash).
//! - Re-scan without edits → disk hash still matches DB (no permanent dirty).
//! - Chinese symbol names are present in the store after structural index.

use std::path::Path;
use std::sync::Arc;

use db::Store;
use encoding_rs::GBK;
use extraction::ExtractionMode;
use filesync::{IndexPipelineOptions, run_index_pipeline};
use workspace::{file_content_hash, read_source, text_content_hash};

const PY_UTF8: &str = "\
# 中文注释：索引与 dirty 回归（源编码）\n\
# 汉字上下文用于 GBK 检测\n\
\n\
def 计算总和(a, b):\n\
    return a + b\n\
\n\
class 数据服务:\n\
    pass\n\
";

fn write_gbk_project(root: &Path) -> Vec<u8> {
    let (encoded, _, unmappable) = GBK.encode(PY_UTF8);
    assert!(!unmappable);
    let raw = encoded.into_owned();
    assert!(std::str::from_utf8(&raw).is_err());
    std::fs::write(root.join("main.py"), &raw).unwrap();
    raw
}

#[test]
fn gbk_index_stores_raw_file_hash_and_chinese_symbols() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let raw = write_gbk_project(root);
    let raw_hash = file_content_hash(&raw);
    let text_hash = text_content_hash(PY_UTF8.as_bytes());
    assert_ne!(raw_hash, text_hash, "precondition: dual hash diverge");

    let src = read_source(&root.join("main.py")).unwrap();
    assert_eq!(src.file_hash, raw_hash);
    assert_ne!(src.file_hash, src.text_hash());

    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();

    let stats = run_index_pipeline(
        &store,
        root,
        IndexPipelineOptions::new(ExtractionMode::Structural),
    )
    .expect("index structural GBK project");
    assert_eq!(stats.failed, 0, "stats={stats:?}");
    assert!(stats.indexed >= 1, "stats={stats:?}");
    assert!(stats.symbols > 0, "stats={stats:?}");

    let files = store.list_files().expect("list_files");
    let main = files
        .iter()
        .find(|f| f.path == "main.py" || f.path.ends_with("main.py"))
        .unwrap_or_else(|| panic!("main.py missing from store: {files:?}"));

    assert_eq!(
        main.content_hash, raw_hash,
        "DB content_hash must be blake3(raw), not UTF-8 text hash"
    );
    assert_ne!(main.content_hash, text_hash);

    let symbols = store.get_all_symbols().expect("get_all_symbols");
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"计算总和"),
        "indexed symbols should include 计算总和, got {names:?}"
    );
    assert!(
        names.contains(&"数据服务"),
        "indexed symbols should include 数据服务, got {names:?}"
    );

    assert_eq!(std::fs::read(root.join("main.py")).unwrap(), raw);
}

#[test]
fn gbk_unchanged_file_is_not_permanently_dirty() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let raw = write_gbk_project(root);
    let raw_hash = file_content_hash(&raw);

    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();

    run_index_pipeline(
        &store,
        root,
        IndexPipelineOptions::new(ExtractionMode::Structural),
    )
    .expect("first index");

    let files = store.list_files().unwrap();
    let main = files
        .iter()
        .find(|f| f.path.ends_with("main.py"))
        .expect("main.py");
    assert_eq!(main.content_hash, raw_hash);

    let on_disk = std::fs::read(root.join("main.py")).unwrap();
    let curr = file_content_hash(&on_disk);
    assert_eq!(
        curr, main.content_hash,
        "raw re-hash must match DB — otherwise file is permanently dirty"
    );

    run_index_pipeline(
        &store,
        root,
        IndexPipelineOptions::new(ExtractionMode::Structural),
    )
    .expect("second index");

    let files2 = store.list_files().unwrap();
    let main2 = files2
        .iter()
        .find(|f| f.path.ends_with("main.py"))
        .expect("main.py after reindex");
    assert_eq!(
        main2.content_hash, raw_hash,
        "reindex must not rewrite content_hash to text_hash"
    );
}
