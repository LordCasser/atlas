//! Integration test: verify that `atlas index --analysis full` does not crash
//! with SIGABRT (stack overflow) on deeply nested source files.
//!
//! This test validates the extraction thread pool (8 MiB stacks) and
//! per-file extraction thread isolation fix for GitHub issue stack-overflow.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Build the atlas binary for testing.
fn atlas_binary() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // CARGO_MANIFEST_DIR is crates/atlas-cli
    // Binary is at target/debug/atlas or target/release/atlas
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    path.pop(); // up from atlas-cli
    path.pop(); // up from crates
    path.push("target");
    path.push(profile);
    path.push("atlas");
    assert!(
        path.exists(),
        "atlas binary not found at {}. Build with: cargo build -p atlas-cli",
        path.display()
    );
    path
}

/// Generate a TypeScript file with deeply nested if blocks.
///
/// Produces code like:
/// ```ts
/// if (true) {
///   if (true) {
///     if (true) {
///       ... (200+ levels)
///       const x = 1;
///     }
///   }
/// }
/// ```
fn gen_deep_nested_ifs(tmpdir: &std::path::Path, depth: usize) -> PathBuf {
    let file_path = tmpdir.join("deep_nested_ifs.ts");
    let mut code = String::new();
    // Opening ifs
    for i in 0..depth {
        let indent = "  ".repeat(i);
        code.push_str(&format!("{indent}if (true) {{\n"));
    }
    // Innermost statement
    let indent = "  ".repeat(depth);
    code.push_str(&format!("{indent}const deepVar{depth} = {depth};\n"));
    // Closing braces
    for i in (0..depth).rev() {
        let indent = "  ".repeat(i);
        code.push_str(&format!("{indent}}}\n"));
    }
    fs::write(&file_path, &code).unwrap();
    file_path
}

/// Generate a TypeScript file with deeply nested function calls.
///
/// Produces: `fn1(fn2(fn3(...fn200(0)...)))`
fn gen_deep_nested_calls(tmpdir: &std::path::Path, depth: usize) -> PathBuf {
    let file_path = tmpdir.join("deep_nested_calls.ts");
    let mut code = String::new();
    for i in 1..=depth {
        code.push_str(&format!(
            "function fn{i}(x: number): number {{ return x + {i}; }}\n"
        ));
    }
    // Build: fn1(fn2(fn3(...fn200(0)...)))
    let mut call = "0".to_string();
    for i in (1..depth).rev() {
        call = format!("fn{i}({call})");
    }
    code.push_str(&format!("export const result = {call};\n"));
    fs::write(&file_path, &code).unwrap();
    file_path
}

/// Generate a TypeScript file with deeply nested object literals.
///
/// Produces: `{ a1: { a2: { a3: { ... (200+ levels) } } } }`
fn gen_deep_nested_objects(tmpdir: &std::path::Path, depth: usize) -> PathBuf {
    let file_path = tmpdir.join("deep_nested_objects.ts");
    let mut code = String::new();
    for i in 1..depth {
        let indent = "  ".repeat(i);
        code.push_str(&format!("{indent}a{i}: {{\n"));
    }
    let indent = "  ".repeat(depth);
    code.push_str(&format!("{indent}leaf: {depth}\n"));
    for i in (1..depth).rev() {
        let indent = "  ".repeat(i);
        code.push_str(&format!("{indent}}},\n"));
    }
    // Wrap in export const
    code = format!("export const deepObj = {{\n{code}}};\n");
    fs::write(&file_path, &code).unwrap();
    file_path
}

/// Generate a Java file with deeply nested if blocks.
fn gen_deep_nested_java_ifs(tmpdir: &std::path::Path, depth: usize) -> PathBuf {
    let file_path = tmpdir.join("DeepNested.java");
    let mut code = String::new();
    code.push_str("public class DeepNested {\n");
    code.push_str("    public void deepMethod() {\n");
    for i in 0..depth {
        let indent = format!("        {}", "    ".repeat(i));
        code.push_str(&format!("{indent}if (true) {{\n"));
    }
    let indent = format!("        {}", "    ".repeat(depth));
    code.push_str(&format!("{indent}int x = {depth};\n"));
    for i in (0..depth).rev() {
        let indent = format!("        {}", "    ".repeat(i));
        code.push_str(&format!("{indent}}}\n"));
    }
    code.push_str("    }\n");
    code.push_str("}\n");
    fs::write(&file_path, &code).unwrap();
    file_path
}

// ── Tests ────────────────────────────────────────────────────────────────

#[test]
fn test_index_deep_nested_typescript_no_stack_overflow() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Generate deeply nested source files
    gen_deep_nested_ifs(root, 200);
    gen_deep_nested_calls(root, 200);
    gen_deep_nested_objects(root, 200);

    // Run atlas index --analysis full
    let atlas = atlas_binary();
    let output = Command::new(&atlas)
        .args(["index", "--analysis", "full"])
        .current_dir(root)
        .output()
        .expect("failed to run atlas index");

    // The process should NOT have crashed with SIGABRT
    // stderr may contain warnings, but the process must exit cleanly
    eprintln!("=== STDOUT ===");
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    eprintln!("=== STDERR ===");
    eprintln!("{}", String::from_utf8_lossy(&output.stderr));
    eprintln!("=== STATUS: {:?} ===", output.status);

    // The process should complete successfully (EXIT_SUCCESS = 0)
    // Even if some files have parse errors, atlas should exit 0
    assert!(
        output.status.success(),
        "atlas index failed with status {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify the process wasn't killed by SIGABRT (signal 6 on macOS/Linux)
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_ne!(
            output.status.signal(),
            Some(6),
            "atlas was killed by SIGABRT (stack overflow)"
        );
    }
}

#[test]
fn test_index_deep_nested_java_no_stack_overflow() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Generate deeply nested Java file
    gen_deep_nested_java_ifs(root, 200);

    // Also add a shallow file for context
    fs::write(
        root.join("Shallow.java"),
        "public class Shallow {\n    public int add(int a, int b) { return a + b; }\n}\n",
    )
    .unwrap();

    let atlas = atlas_binary();
    let output = Command::new(&atlas)
        .args(["index", "--analysis", "full"])
        .current_dir(root)
        .output()
        .expect("failed to run atlas index");

    eprintln!("=== STDOUT ===");
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    eprintln!("=== STDERR ===");
    eprintln!("{}", String::from_utf8_lossy(&output.stderr));

    assert!(
        output.status.success(),
        "atlas index failed with status {:?}",
        output.status
    );

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_ne!(
            output.status.signal(),
            Some(6),
            "atlas was killed by SIGABRT"
        );
    }
}

#[test]
fn test_index_deep_nested_with_analysis_modes() {
    // Verify all analysis modes work with deep nesting
    let modes = ["manifest", "structural", "full"];

    for &mode_name in &modes {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        gen_deep_nested_ifs(root, 100);

        let atlas = atlas_binary();
        let output = Command::new(&atlas)
            .args(["index", "--analysis", mode_name])
            .current_dir(root)
            .output()
            .expect("failed to run atlas index");

        assert!(
            output.status.success(),
            "atlas index --analysis {mode_name} failed with status {:?}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );

        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_ne!(
                output.status.signal(),
                Some(6),
                "atlas was killed by SIGABRT in {mode_name} mode"
            );
        }
    }
}
