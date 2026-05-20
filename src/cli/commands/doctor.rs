//! `atlas doctor` — check environment readiness for Atlas, including per-language
//! capability profiles.

use std::path::Path;

use crate::types::LanguageCapabilityProfile;

pub fn run(project: &str) -> anyhow::Result<()> {
    let root = Path::new(project);
    let atlas_dir = root.join(".atlas");
    let mut all_ok = true;

    println!("Atlas Doctor");
    println!("============");
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

    // 4. SQLite FTS5 support
    if db_exists {
        match check_fts5(&db_path) {
            Ok(true) => check("SQLite FTS5 support", true, &mut all_ok),
            Ok(false) => {
                check("SQLite FTS5 support", false, &mut all_ok);
                println!("     Hint: Rebuild rusqlite with 'bundled' feature");
            }
            Err(e) => {
                check(
                    &format!("SQLite FTS5 check ({e})"),
                    false,
                    &mut all_ok,
                );
            }
        }
    } else {
        println!("  [SKIP] SQLite FTS5 support (no database)");
    }

    // 5. Schema version
    if db_exists {
        match check_schema(&db_path) {
            Ok(Some(ver)) => {
                let current = crate::db::CURRENT_SCHEMA_VERSION;
                check(
                    &format!("Schema version ({ver} == {current})"),
                    ver == current,
                    &mut all_ok,
                );
            }
            Ok(None) => {
                check("Schema version", false, &mut all_ok);
                println!("     Hint: Run `atlas init` to initialize the database");
            }
            Err(e) => {
                check(
                    &format!("Schema version check ({e})"),
                    false,
                    &mut all_ok,
                );
            }
        }
    }

    // 6. Language grammar availability (compile-time feature check)
    println!();
    println!("  Language grammar support:");
    check_lang("TypeScript/JavaScript", cfg!(feature = "typescript"), &mut all_ok);
    check_lang("Python", cfg!(feature = "python"), &mut all_ok);
    check_lang("Java", cfg!(feature = "java"), &mut all_ok);
    check_lang("C", cfg!(feature = "c"), &mut all_ok);
    check_lang("C++", cfg!(feature = "cpp"), &mut all_ok);
    check_lang("ArkTS", cfg!(feature = "arkts"), &mut all_ok);
    check_lang("Cangjie", cfg!(feature = "cangjie"), &mut all_ok);

    // 7. Per-language capability summary
    print_capabilities();

    // Summary
    println!();
    if all_ok {
        println!("All checks passed. Atlas is ready!");
    } else {
        println!("Some checks failed. Run `atlas init` to fix database issues.");
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

fn check_lang(name: &str, enabled: bool, all_ok: &mut bool) {
    if enabled {
        println!("    [OK]    {name}");
    } else {
        println!("    [WARN]  {name} (not compiled in)");
        *all_ok = false;
    }
}

/// Check that FTS5 is available in the bundled SQLite.
fn check_fts5(db_path: &Path) -> anyhow::Result<bool> {
    let conn = rusqlite::Connection::open(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT 1 FROM pragma_compile_options WHERE compile_options = 'ENABLE_FTS5'",
    )?;
    let has_fts5 = stmt.exists([])?;
    Ok(has_fts5)
}

/// Read the current schema version from the database.
fn check_schema(db_path: &Path) -> anyhow::Result<Option<i64>> {
    let conn = rusqlite::Connection::open(db_path)?;
    // Check if schema_versions table exists
    let table_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='schema_versions'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|c| c > 0)
        .unwrap_or(false);

    if !table_exists {
        return Ok(None);
    }

    let ver: i64 = conn.query_row(
        "SELECT version FROM schema_versions ORDER BY version DESC LIMIT 1",
        [],
        |r| r.get(0),
    )?;
    Ok(Some(ver))
}

/// Print per-language capability levels for all compiled-in languages.
fn print_capabilities() {
    let profiles = LanguageCapabilityProfile::all_compiled();
    if profiles.is_empty() {
        return;
    }

    println!();
    println!("  Capability Profile by Language:");
    println!(
        "  {:<18} {:<20} {:<7} {}",
        "Language", "Capability Level", "Conf", "Key Limitations"
    );
    println!(
        "  {:-<18} {:-<20} {:-<7} {:-<48}",
        "", "", "", ""
    );

    for p in &profiles {
        let limiter = p
            .limitations
            .first()
            .map(|s| truncate_str(s, 48))
            .unwrap_or_default();
        println!(
            "  {:<18} {:<20} {:<4.0}%  {}",
            p.language,
            p.capability_level.as_str(),
            p.confidence_floor * 100.0,
            limiter,
        );
    }
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}…", &s[..max_len - 1])
    }
}
