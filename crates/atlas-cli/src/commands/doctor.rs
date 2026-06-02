//! `atlas doctor` — check environment readiness for Atlas, including per-language
//! capability profiles.

use std::path::Path;

use atlas_engine::LanguageCapabilityProfile;

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
                check(&format!("SQLite FTS5 check ({e})"), false, &mut all_ok);
            }
        }
    } else {
        println!("  [SKIP] SQLite FTS5 support (no database)");
    }

    // 5. Language grammar availability (compile-time feature check)
    println!();
    println!("  Language grammar support:");
    check_lang("TypeScript", cfg!(feature = "typescript"));
    check_lang("JavaScript", cfg!(feature = "javascript"));
    check_lang("Python", cfg!(feature = "python"));
    check_lang("Java", cfg!(feature = "java"));
    check_lang("C", cfg!(feature = "c"));
    check_lang("C++", cfg!(feature = "cpp"));
    check_lang("ArkTS", cfg!(feature = "arkts"));
    check_experimental_lang("Cangjie", cfg!(feature = "cangjie"));

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

fn check_lang(name: &str, enabled: bool) {
    if enabled {
        println!("    [OK]    {name}");
    } else {
        println!("    [WARN]  {name} (not compiled in)");
    }
}

fn check_experimental_lang(name: &str, enabled: bool) {
    if enabled {
        println!("    [OK]    {name} (experimental)");
    } else {
        println!("    [SKIP]  {name} (experimental opt-in)");
    }
}

/// Check that FTS5 is available in the bundled SQLite.
fn check_fts5(db_path: &Path) -> anyhow::Result<bool> {
    let conn = rusqlite::Connection::open(db_path)?;
    let mut stmt =
        conn.prepare("SELECT 1 FROM pragma_compile_options WHERE compile_options = 'ENABLE_FTS5'")?;
    let has_fts5 = stmt.exists([])?;
    Ok(has_fts5)
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
        "  {:<18} {:<20} {:<7} Key Limitations",
        "Language", "Capability Level", "Conf"
    );
    println!("  {:-<18} {:-<20} {:-<7} {:-<48}", "", "", "", "");

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

    // Show unsupported features from the FeatureMatrix for each language
    println!();
    println!("  Unsupported Features:");
    for p in &profiles {
        if let Some(ref feats) = p.features {
            let unsupported: Vec<&str> = [
                ("symbols", &feats.symbols),
                ("references", &feats.references),
                ("imports", &feats.imports),
                ("scopes", &feats.scopes),
                ("call_graph", &feats.call_graph),
                ("lexical_bindings", &feats.lexical_bindings),
                ("local_dataflow", &feats.local_dataflow),
                ("use_def", &feats.use_def),
                ("field_access", &feats.field_access),
                ("call_arguments", &feats.call_arguments),
                ("returns_flow", &feats.returns_flow),
                ("cfg", &feats.cfg),
                ("interprocedural", &feats.interprocedural_summaries),
            ]
            .iter()
            .filter_map(|(name, fs)| {
                if !fs.is_supported() {
                    Some(*name)
                } else {
                    None
                }
            })
            .collect();
            if !unsupported.is_empty() {
                println!("    {:<16}  {}", p.language, unsupported.join(", "));
            }
        }
    }
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let mut end = 0;
        for (idx, _) in s.char_indices() {
            if idx >= max_len {
                break;
            }
            end = idx;
        }
        format!("{}…", &s[..end])
    }
}

fn compiled_features() -> Vec<&'static str> {
    let mut features = Vec::new();
    if cfg!(feature = "typescript") {
        features.push("typescript");
    }
    if cfg!(feature = "javascript") {
        features.push("javascript");
    }
    if cfg!(feature = "python") {
        features.push("python");
    }
    if cfg!(feature = "java") {
        features.push("java");
    }
    if cfg!(feature = "c") {
        features.push("c");
    }
    if cfg!(feature = "cpp") {
        features.push("cpp");
    }
    if cfg!(feature = "arkts") {
        features.push("arkts");
    }
    if cfg!(feature = "go") {
        features.push("go");
    }
    if cfg!(feature = "csharp") {
        features.push("csharp");
    }
    if cfg!(feature = "rust") {
        features.push("rust");
    }
    if cfg!(feature = "php") {
        features.push("php");
    }
    if cfg!(feature = "ruby") {
        features.push("ruby");
    }
    if cfg!(feature = "kotlin") {
        features.push("kotlin");
    }
    if cfg!(feature = "cangjie") {
        features.push("cangjie");
    }
    features
}
