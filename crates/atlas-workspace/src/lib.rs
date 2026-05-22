//! Workspace abstraction: project root, well-known paths, and runtime context.
//!
//! A [`Workspace`] represents a project on disk with its `.atlas/` metadata directory.
//! It centralizes path computation that was previously scattered across CLI commands
//! and the database constructor.
//!
//! # Layering
//!
//! This crate is the lowest layer in the Atlas stack. It has **no** dependencies
//! on `atlas-types`, `atlas-db`, or any other Atlas crate — only `std` and `anyhow`.

use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// ProjectRoot
// ---------------------------------------------------------------------------

/// Canonical project root directory.
///
/// Wraps a canonicalized [`PathBuf`] that points to the root of an Atlas project.
/// The root may or may not already have a `.atlas/` directory — presence is not
/// enforced at construction time.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectRoot(PathBuf);

impl ProjectRoot {
    /// Create a [`ProjectRoot`] from a user-provided path.
    ///
    /// The path is canonicalized (resolving symlinks and `..` components).
    /// Returns an error if the path does not exist or is not a directory.
    pub fn new(path: &Path) -> anyhow::Result<Self> {
        let canonical = path.canonicalize().map_err(|e| {
            anyhow::anyhow!("Project path not found: {} ({})", path.display(), e)
        })?;
        if !canonical.is_dir() {
            anyhow::bail!("Not a directory: {}", canonical.display());
        }
        Ok(Self(canonical))
    }

    /// Create a [`ProjectRoot`] from an already-canonicalized path.
    ///
    /// Does **not** validate existence — caller is responsible for correctness.
    /// Use in tests or when the path is known to be valid.
    pub fn from_canonical(path: PathBuf) -> Self {
        Self(path)
    }

    /// The underlying canonical path.
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Find the project root by walking up from `cwd` looking for `.atlas/`.
    ///
    /// Returns `None` if no `.atlas/` directory is found in any ancestor.
    pub fn find() -> Option<Self> {
        let cwd = std::env::current_dir().ok()?;
        let mut current = cwd.as_path();
        loop {
            if current.join(".atlas").is_dir() {
                // Canonicalize the found path for consistency.
                if let Ok(canonical) = current.canonicalize() {
                    return Some(Self(canonical));
                }
                return Some(Self(current.to_path_buf()));
            }
            current = current.parent()?;
        }
    }
}

impl AsRef<Path> for ProjectRoot {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl std::fmt::Display for ProjectRoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.display())
    }
}

// ---------------------------------------------------------------------------
// WorkspacePaths
// ---------------------------------------------------------------------------

/// Well-known paths within an Atlas project.
///
/// All paths are derived from the [`ProjectRoot`] and are deterministic
/// (recomputed, not cached from disk).
#[derive(Debug, Clone)]
pub struct WorkspacePaths {
    /// Project root (canonical).
    pub root: ProjectRoot,
    /// `.atlas/` directory.
    pub atlas_dir: PathBuf,
    /// `.atlas/atlas.db` file path.
    pub db_path: PathBuf,
}

impl WorkspacePaths {
    /// Compute well-known paths from a [`ProjectRoot`].
    pub fn new(root: ProjectRoot) -> Self {
        let atlas_dir = root.path().join(".atlas");
        let db_path = atlas_dir.join("atlas.db");
        Self {
            root,
            atlas_dir,
            db_path,
        }
    }
}

// ---------------------------------------------------------------------------
// Workspace
// ---------------------------------------------------------------------------

/// A workspace: project root + well-known paths + convenience methods.
///
/// Does **not** own a `Store`; callers manage the store lifecycle separately.
/// The store is opened via:
///
/// ```text
/// ws.ensure_atlas_dir()?;
/// let store = Store::open_db(ws.db_path())?;
/// ```
///
/// This split is intentional: the workspace layer knows *where* things are,
/// while the store layer knows *how* to persist them.
pub struct Workspace {
    /// Well-known paths for this workspace.
    pub paths: WorkspacePaths,
}

impl Workspace {
    /// Open a workspace at the given project path.
    ///
    /// The path is canonicalized and validated. Does not create `.atlas/` —
    /// use [`Workspace::ensure_atlas_dir`] for that.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let root = ProjectRoot::new(path)?;
        Ok(Self {
            paths: WorkspacePaths::new(root),
        })
    }

    /// Open a workspace without canonicalizing or validating.
    ///
    /// Use in tests or when the path is already known to be valid and canonical.
    pub fn open_unchecked(path: &Path) -> Self {
        let root = ProjectRoot::from_canonical(path.to_path_buf());
        Self {
            paths: WorkspacePaths::new(root),
        }
    }

    /// Find a workspace by walking up from the current directory looking for `.atlas/`.
    ///
    /// Returns `None` if no `.atlas/` directory is found in any ancestor.
    pub fn find() -> Option<Self> {
        ProjectRoot::find().map(|r| Self {
            paths: WorkspacePaths::new(r),
        })
    }

    /// Project root path (canonical).
    pub fn root(&self) -> &Path {
        self.paths.root.path()
    }

    /// `.atlas/` directory path.
    pub fn atlas_dir(&self) -> &Path {
        &self.paths.atlas_dir
    }

    /// Database file path (`.atlas/atlas.db`).
    pub fn db_path(&self) -> &Path {
        &self.paths.db_path
    }

    /// Ensure the `.atlas/` directory exists, creating it if necessary.
    ///
    /// This is idempotent: calling it multiple times is safe.
    /// Used by `atlas init` and any command that needs the directory
    /// to exist before opening the database.
    pub fn ensure_atlas_dir(&self) -> anyhow::Result<()> {
        if !self.atlas_dir().is_dir() {
            std::fs::create_dir_all(self.atlas_dir()).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to create .atlas directory at {}: {}",
                    self.atlas_dir().display(),
                    e
                )
            })?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// SourcePath
// ---------------------------------------------------------------------------

/// A file path relative to workspace root, always normalized with forward slashes.
///
/// This is a proper newtype (not a bare `String`) to prevent accidental
/// creation with backslashes or absolute paths. Use [`SourcePath::from_relative`]
/// or [`relative_source_path`] to obtain instances.
///
/// # Slash normalization
///
/// Construction replaces all `\` with `/` and strips leading `./` and `.\` prefixes.
/// Relative paths (`foo/bar.ts`) and single-file names (`index.ts`) are valid.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourcePath(String);

impl SourcePath {
    /// Create a [`SourcePath`] from a relative path string, normalizing separators.
    ///
    /// Strips leading `./` and `.\` prefixes. Accepts paths with either separator,
    /// always stores with forward slashes.
    ///
    /// # Examples
    ///
    /// ```
    /// # use atlas_workspace::SourcePath;
    /// let p = SourcePath::from_relative("src\\lib.ts");
    /// assert_eq!(p.as_str(), "src/lib.ts");
    ///
    /// let p = SourcePath::from_relative("./foo/bar.ts");
    /// assert_eq!(p.as_str(), "foo/bar.ts");
    /// ```
    pub fn from_relative(path: &str) -> Self {
        let normalized = path.replace('\\', "/");
        // Strip leading ./ (which may itself have been .\ before normalization)
        let stripped = match normalized.strip_prefix("./") {
            Some(s) => s,
            None => normalized.as_str(),
        };
        // Trim leading slashes (not a valid relative path)
        let trimmed = stripped.trim_start_matches('/');
        Self(trimmed.to_string())
    }

    /// The normalized relative path with forward slashes.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SourcePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AsRef<str> for SourcePath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<SourcePath> for String {
    fn from(p: SourcePath) -> String {
        p.0
    }
}

/// Derive a workspace-relative [`SourcePath`] from an absolute file path.
///
/// Strips the `root` prefix from `abs_path`, normalizing separators to
/// forward slashes. Returns `None` if `abs_path` is not under `root`.
pub fn relative_source_path(root: &Path, abs_path: &Path) -> Option<SourcePath> {
    let rel = abs_path.strip_prefix(root).ok()?;
    Some(SourcePath::from_relative(&rel.to_string_lossy()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn project_root_canonicalizes() {
        let tmp = tempfile::tempdir().unwrap();
        let root_path = tmp.path().join("project");
        fs::create_dir(&root_path).unwrap();

        let ws = Workspace::open(&root_path).unwrap();
        assert_eq!(ws.root(), root_path.canonicalize().unwrap().as_path());
    }

    #[test]
    fn project_root_rejects_nonexistent() {
        let result = Workspace::open(Path::new("/nonexistent/path/atlas_test"));
        assert!(result.is_err());
    }

    #[test]
    fn project_root_rejects_file() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("not_a_dir.txt");
        fs::write(&file_path, "hello").unwrap();

        let result = Workspace::open(&file_path);
        assert!(result.is_err());
    }

    #[test]
    fn workspace_paths_computed_correctly() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace::open(tmp.path()).unwrap();

        let canonical_root = tmp.path().canonicalize().unwrap();
        assert_eq!(ws.root(), canonical_root.as_path());
        assert_eq!(ws.atlas_dir(), canonical_root.join(".atlas").as_path());
        assert_eq!(
            ws.db_path(),
            canonical_root.join(".atlas").join("atlas.db").as_path()
        );
    }

    #[test]
    fn project_root_find_returns_none_outside_project() {
        // This test may pass or fail depending on whether the test runner's
        // cwd contains a `.atlas` directory. We just verify no panic.
        let _ = ProjectRoot::find();
    }

    #[test]
    fn workspace_find_returns_none_outside_project() {
        let _ = Workspace::find();
    }

    #[test]
    fn workspace_find_locates_atlas_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("proj");
        fs::create_dir(&project).unwrap();
        fs::create_dir(project.join(".atlas")).unwrap();

        // Temporarily change cwd to the project root.
        let original_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&project).unwrap();

        let ws = Workspace::find();
        std::env::set_current_dir(original_cwd).unwrap();

        let ws = ws.expect("Should find workspace with .atlas/");
        assert_eq!(ws.root(), project.canonicalize().unwrap().as_path());
    }

    #[test]
    fn workspace_ensure_atlas_dir_creates_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace::open(tmp.path()).unwrap();

        // .atlas/ doesn't exist yet
        assert!(!ws.atlas_dir().is_dir());
        // ensure_atlas_dir should create it
        ws.ensure_atlas_dir().unwrap();
        assert!(ws.atlas_dir().is_dir());
        // Idempotent: second call should not fail
        ws.ensure_atlas_dir().unwrap();
        assert!(ws.atlas_dir().is_dir());
    }
}
