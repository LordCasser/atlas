//! `atlas doctor` — check environment readiness for Atlas, including per-language
//! capability profiles.

use std::path::Path;

use atlas_engine::{CURRENT_SCHEMA_VERSION, LanguageCapabilityProfile, Store};

pub fn run(project: &str) -> anyhow::Result<()> {
    let root = Path::new(project);
    let atlas_dir = root.join(".atlas");
    let mut all_ok = true;
    let mut needs_rebuild_hint = false;

    println!("Atlas Doctor");
    println!("============");
    println!("  Atlas version: {}", env!("CARGO_PKG_VERSION"));
    println!();

    // 1. Project root exists and is a directory
    check(
        "Project root exists",
        root.try_exists().is_ok_and(|e| e),
        &mut all_ok,
    );
    check("Project root is a directory", root.is_dir(), &mut all_ok);

    // 2. .atlas/ directory
    let atlas_exists = atlas_dir.is_dir();
    check("Atlas directory (.atlas/)", atlas_exists, &mut all_ok);

    // 3. Database file
    let db_path = atlas_dir.join("atlas.db");
    let db_exists = db_path.is_file();
    check("Atlas database (atlas.db)", db_exists, &mut all_ok);
    needs_rebuild_hint |= !db_exists;

    // 4. SQLite FTS5 support
    if db_exists {
        match check_fts5(&db_path) {
            Ok(true) => check("SQLite FTS5 support", true, &mut all_ok),
            Ok(false) => {
                check("SQLite FTS5 support", false, &mut all_ok);
                println!("     Hint: Rebuild rusqlite with 'bundled' feature");
            }
            Err(e) => {
                check(&format!("SQLite FTS5 check ({e})"), false, &mut all_ok);
            }
        }
    } else {
        println!("  [SKIP] SQLite FTS5 support (no database)");
    }

    // 5. Schema and index mode
    if db_exists {
        match read_schema_version(&db_path) {
            Ok(version) if version == CURRENT_SCHEMA_VERSION => check(
                &format!("Schema version (v{CURRENT_SCHEMA_VERSION})"),
                true,
                &mut all_ok,
            ),
            Ok(version) => {
                check(
                    &format!(
                        "Schema version (found v{version}, expected v{CURRENT_SCHEMA_VERSION})"
                    ),
                    false,
                    &mut all_ok,
                );
                needs_rebuild_hint = true;
            }
            Err(e) => {
                check(&format!("Schema version check ({e})"), false, &mut all_ok);
                needs_rebuild_hint = true;
            }
        }

        match read_catalog_tier(&db_path) {
            Ok(mode) => check(&format!("Index mode ({mode})"), true, &mut all_ok),
            Err(e) => {
                check(&format!("Index mode check ({e})"), false, &mut all_ok);
                needs_rebuild_hint = true;
            }
        }
    } else {
        println!("  [SKIP] Schema version (no database)");
        println!("  [SKIP] Index mode (no database)");
    }

    // 6. Language grammar availability (compile-time feature check)
    println!();
    println!("  Language grammar support:");
    check_lang("TypeScript", cfg!(feature = "typescript"));
    check_lang("JavaScript", cfg!(feature = "javascript"));
    check_lang("Python", cfg!(feature = "python"));
    check_lang("Java", cfg!(feature = "java"));
    check_lang("C", cfg!(feature = "c"));
    check_lang("C++", cfg!(feature = "cpp"));
    check_lang("ArkTS", cfg!(feature = "arkts"));
    check_lang("Cangjie", cfg!(feature = "cangjie"));

    // Post-MVP languages
    check_lang("Go", cfg!(feature = "go"));
    check_lang("C#", cfg!(feature = "csharp"));
    check_lang("Rust", cfg!(feature = "rust"));
    check_lang("PHP", cfg!(feature = "php"));
    check_lang("Ruby", cfg!(feature = "ruby"));
    check_lang("Kotlin", cfg!(feature = "kotlin"));

    println!();
    println!("  Compiled features: {}", compiled_features().join(", "));

    // 7. Per-language capability summary
    print_capabilities();

    // Summary
    println!();
    if all_ok {
        println!("All checks passed. Atlas is ready!");
    } else {
        println!("Some checks failed.");
        if needs_rebuild_hint {
            print_rebuild_hint(project, &db_path);
        }
    }

    Ok(())
}

// --- helpers ---

fn check(name: &str, ok: bool, all_ok: &mut bool) {
    if ok {
        println!("  [OK]    {name}");
    } else {
        println!("  [FAIL]  {name}");
        *all_ok = false;
    }
}

fn check_lang(name: &str, enabled: bool) {
    if enabled {
        println!("    [OK]    {name}");
    } else {
        println!("    [WARN]  {name} (not compiled in)");
    }
}

/// Check that FTS5 is available in the bundled SQLite.
fn check_fts5(db_path: &Path) -> anyhow::Result<bool> {
    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt =
        conn.prepare("SELECT 1 FROM pragma_compile_options WHERE compile_options = 'ENABLE_FTS5'")?;
    let has_fts5 = stmt.exists([])?;
    Ok(has_fts5)
}

fn read_schema_version(db_path: &Path) -> anyhow::Result<i64> {
    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

fn read_catalog_tier(db_path: &Path) -> anyhow::Result<String> {
    let store = Store::open_db_read_only(db_path)?;
    store.read_catalog_tier()
}

fn print_rebuild_hint(project: &str, db_path: &Path) {
    println!(
        "     Hint: Run `atlas index --project {project}` to rebuild the database for the current schema."
    );
    println!(
        "     Hint: For incompatible development databases, move or remove `{}` or the project `.atlas/` directory, then rerun `atlas index --project {project}`.",
        db_path.display()
    );
}

/// Print per-language capability levels for all compiled-in languages.
///
/// Surfaces honest L0 theory capability (`CapabilityLevel` + `confidence_floor`)
/// without dumping per-feature matrices. Feature-level detail stays in
/// `FeatureMatrix` for gate checks; operators use confidence as the summary.
fn print_capabilities() {
    let profiles = LanguageCapabilityProfile::all_compiled();
    if profiles.is_empty() {
        return;
    }

    println!();
    println!("  Capability Profile by Language:");
    println!(
        "  {:<18} {:<20} Confidence Floor",
        "Language", "Capability Level"
    );
    println!("  {:-<18} {:-<20} {:-<16}", "", "", "");

    for p in &profiles {
        println!(
            "  {:<18} {:<20} {:.0}%",
            p.language,
            p.capability_level.as_str(),
            p.confidence_floor * 100.0,
        );
    }
}

fn compiled_features() -> Vec<String> {
    LanguageCapabilityProfile::all_compiled()
        .into_iter()
        .map(|profile| profile.language)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn initialized_db_path(temp_dir: &tempfile::TempDir) -> std::path::PathBuf {
        let db_path = temp_dir.path().join("atlas.db");
        let store = Store::open_db(&db_path).unwrap();
        store.init_schema().unwrap();
        db_path
    }

    #[test]
    fn read_schema_version_reports_current_initialized_schema() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = initialized_db_path(&temp_dir);

        assert_eq!(
            read_schema_version(&db_path).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
    }

    #[test]
    fn read_schema_version_reports_raw_incompatible_schema() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("atlas.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        drop(conn);

        assert_eq!(read_schema_version(&db_path).unwrap(), 1);
    }

    #[test]
    fn read_catalog_tier_uses_store_status_boundary() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = initialized_db_path(&temp_dir);

        assert_eq!(read_catalog_tier(&db_path).unwrap(), "none");
    }
}
