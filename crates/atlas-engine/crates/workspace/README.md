# workspace

Project root and source-path abstractions. The lowest layer in the Atlas stack — no dependencies on other Atlas crates.

## Components

### ProjectRoot

Canonical project root directory. Created from a user-provided path (canonicalized, validated as existing directory).

```rust
let root = ProjectRoot::new("/path/to/project")?;  // canonicalize + validate
let found = ProjectRoot::find();                    // walk up from CWD looking for .atlas/
```

### Workspace

Well-known paths for a project:
- `root` — canonical project root
- `atlas_dir` — `<root>/.atlas/`
- `db_path` — `<root>/.atlas/atlas.db`

```rust
let ws = Workspace::open("/path/to/project")?;
ws.ensure_atlas_dir()?;  // create .atlas/ if missing
let store = Store::open_db(ws.db_path())?;
```

### SourcePath

Normalized relative file path (forward slashes, no `./` prefix, no `..` traversal).

```rust
let p = SourcePath::from_relative("src\\lib.ts");  // → "src/lib.ts"
let p = SourcePath::try_from_relative("./foo/bar.ts")?;  // → "foo/bar.ts"
```
