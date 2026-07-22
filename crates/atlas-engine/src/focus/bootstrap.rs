//! BootstrapManager — lazy background bootstrap for focus-driven analysis.
//!
//! When MCP detects no full index, BootstrapManager runs in a background
//! thread to populate the focus overlay tables:
//!
//! - Tier 0: `file_inventory` — file discovery with cheap stat() data
//! - Tier 0.5: fingerprints — content hashes for hot files
//! - Tier 1: `symbol_hints` — manifest-level symbol name index
//! - Tier 2: opportunistic manifest extraction — full manifest facts
//!   written to core tables (`files`, `symbols`, `scopes`, `extraction_state`)
//!   for fingerprinted files that haven't been manifest-extracted yet.
//!
//! Each tier checkpoints between batches so cancellation is responsive.
//! Tier 0 is the minimum barrier for focus queries to work.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use std::{fs, time};

use anyhow::Result;
use db::{DiscoveredFile, Store};
use extraction::{ExtractionMode, create_frontend, extract_file_with_mode};
use filesync::discovery::{DiscoveryConfig, discover_files};
use types::FactCoverage;
use types::Language;
use types::ids::FileId;

use db::SymbolHint;

/// Manages lazy bootstrap population in a background thread.
///
/// Tiers 0–1 write to `file_inventory` and `symbol_hints` tables.
/// Tier 2 opportunistically writes manifest facts to core tables
/// (`files`, `symbols`, `scopes`, `extraction_state`) for fingerprinted
/// files, making the project progressively more useful for search/exploration
/// without waiting for a full index.
pub struct BootstrapManager {
    store: Arc<Store>,
    project_root: Option<PathBuf>,
    running: Arc<AtomicBool>,
    tier0_complete: Arc<AtomicBool>,
    tier1_hot_complete: Arc<AtomicBool>,
    /// Number of files successfully extracted during Tier 2
    /// (opportunistic manifest extraction).
    tier2_extracted: Arc<AtomicU64>,
    handle: Option<JoinHandle<()>>,
}

impl BootstrapManager {
    /// Create a new BootstrapManager. Does NOT start any threads.
    pub fn new(store: Arc<Store>, project_root: Option<PathBuf>) -> Self {
        Self {
            store,
            project_root,
            running: Arc::new(AtomicBool::new(false)),
            tier0_complete: Arc::new(AtomicBool::new(false)),
            tier1_hot_complete: Arc::new(AtomicBool::new(false)),
            tier2_extracted: Arc::new(AtomicU64::new(0)),
            handle: None,
        }
    }

    /// Start background bootstrap thread. Idempotent — second call is no-op.
    ///
    /// The thread runs Tier0 → Tier0.5(hot) → Tier1(hot) → Tier2(opportunistic).
    /// Checkpoints between each stage so cancellation is responsive.
    pub fn start(&mut self) {
        if self.running.load(Ordering::SeqCst) {
            return; // already started
        }

        // Persistent project facts are already the discovery source of truth.
        // Re-running Tier0 over the whole tree on the first query performs
        // tens of thousands of tiny inventory writes and starves foreground
        // SQLite readers. Scoped queries enrich these facts on demand; an
        // explicit CLI sync/index owns project-wide refresh.
        let has_persistent_facts = self.store.file_inventory_count().unwrap_or(0) > 0
            || self.store.count_files().unwrap_or(0) > 0;
        if has_persistent_facts {
            self.tier0_complete.store(true, Ordering::SeqCst);
            self.tier1_hot_complete.store(true, Ordering::SeqCst);
            return;
        }

        let store = Arc::clone(&self.store);
        let project_root = self.project_root.clone();
        let running = Arc::clone(&self.running);
        let tier0_complete = Arc::clone(&self.tier0_complete);
        let tier1_hot_complete = Arc::clone(&self.tier1_hot_complete);
        let tier2_extracted = Arc::clone(&self.tier2_extracted);

        running.store(true, Ordering::SeqCst);

        if project_root.is_none() {
            // No project root — mark as complete trivially, no thread needed.
            tier0_complete.store(true, Ordering::SeqCst);
            tier1_hot_complete.store(true, Ordering::SeqCst);
            return;
        }

        let handle = thread::spawn(move || {
            if let Err(e) = run_bootstrap(
                store,
                project_root,
                &running,
                &tier0_complete,
                &tier1_hot_complete,
                &tier2_extracted,
            ) {
                tracing::error!(?e, "bootstrap background thread failed");
            }
        });

        self.handle = Some(handle);
    }

    /// Check if Tier0 (file_inventory) is complete.
    ///
    /// Tier0 is the minimum for focus queries to work. Once true,
    /// FocusRuntime can answer queries even if higher tiers are pending.
    pub fn is_minimum_ready(&self) -> bool {
        self.tier0_complete.load(Ordering::SeqCst)
    }

    /// Block until Tier0 is complete.
    ///
    /// Used by FocusRuntime to ensure bootstrap is ready before returning
    /// results. Polls every 50ms.
    pub fn ensure_minimum_ready(&self) {
        while !self.tier0_complete.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Block until Tier0 is complete, but no longer than `timeout`.
    ///
    /// Large repositories can take long enough to inventory that waiting
    /// unconditionally makes the first MCP tool call look hung. Focus queries
    /// can still make progress from direct scoped/candidate discovery while
    /// Tier0 continues in the background, so callers should prefer this bounded
    /// wait on request paths.
    pub fn wait_minimum_ready(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while !self.tier0_complete.load(Ordering::SeqCst) {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(50));
        }
        true
    }

    /// Check if Tier 0 (file_inventory) is complete.
    pub fn is_tier0_complete(&self) -> bool {
        self.tier0_complete.load(Ordering::SeqCst)
    }

    /// Check if Tier 1 (symbol_hints for hot files) is complete.
    pub fn is_tier1_hot_complete(&self) -> bool {
        self.tier1_hot_complete.load(Ordering::SeqCst)
    }

    /// Number of files successfully extracted during Tier 2
    /// (opportunistic manifest extraction).
    pub fn tier2_extracted(&self) -> u64 {
        self.tier2_extracted.load(Ordering::SeqCst)
    }

    /// Cancel background work. Thread will finish current batch then exit.
    ///
    /// Does NOT join the background thread — it may still be completing
    /// its current batch.
    pub fn cancel(&mut self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

impl Drop for BootstrapManager {
    fn drop(&mut self) {
        self.cancel();
        if let Some(handle) = self.handle.take() {
            // Best-effort join — don't block indefinitely on panic
            let _ = handle.join();
        }
    }
}

// ── background bootstrap loop ──────────────────────────────────────────────

fn run_bootstrap(
    store: Arc<Store>,
    project_root: Option<PathBuf>,
    running: &AtomicBool,
    tier0_complete: &AtomicBool,
    tier1_hot_complete: &AtomicBool,
    tier2_extracted: &AtomicU64,
) -> Result<()> {
    let Some(ref root) = project_root else {
        // No project root — mark as complete trivially
        tier0_complete.store(true, Ordering::SeqCst);
        tier1_hot_complete.store(true, Ordering::SeqCst);
        return Ok(());
    };

    // ── Tier 0: file inventory ──────────────────────────────────────────
    if !running.load(Ordering::SeqCst) {
        return Ok(());
    }
    bootstrap_tier0(&store, root, running)?;
    tier0_complete.store(true, Ordering::SeqCst);

    // ── Tier 0.5: fingerprints for hot files ────────────────────────────
    if !running.load(Ordering::SeqCst) {
        return Ok(());
    }
    bootstrap_tier0_5(&store, root, running)?;

    // ── Tier 1: symbol hints ────────────────────────────────────────────
    if !running.load(Ordering::SeqCst) {
        return Ok(());
    }
    bootstrap_tier1(&store, root, running)?;
    tier1_hot_complete.store(true, Ordering::SeqCst);

    // ── Tier 2: opportunistic manifest extraction ─────────────────────────
    if !running.load(Ordering::SeqCst) {
        return Ok(());
    }
    let extracted = bootstrap_tier2(&store, root, running)?;
    tier2_extracted.store(extracted as u64, Ordering::SeqCst);

    Ok(())
}

// ── Tier 0: file_inventory ─────────────────────────────────────────────────

const TIER0_BATCH_SIZE: usize = 100;

fn bootstrap_tier0(store: &Store, root: &std::path::Path, running: &AtomicBool) -> Result<()> {
    let config = DiscoveryConfig::default();
    let paths = discover_files(root, &config)?;

    for chunk in paths.chunks(TIER0_BATCH_SIZE) {
        if !running.load(Ordering::SeqCst) {
            return Ok(());
        }
        for path in chunk {
            insert_one_inventory(store, root, path)?;
        }
    }

    Ok(())
}

fn insert_one_inventory(
    store: &Store,
    root: &std::path::Path,
    rel_path: &std::path::Path,
) -> Result<()> {
    let abs_path = root.join(rel_path);
    let metadata = fs::metadata(&abs_path)?;

    let file_id = FileId::generate(&rel_path.to_string_lossy());

    // Language detection from file extension — discovery already filters
    // by known extensions, but we handle None defensively.
    let language = Language::from_path(rel_path).unwrap_or(Language::TypeScript);

    // mtime as seconds since UNIX epoch
    let mtime = metadata
        .modified()?
        .duration_since(time::UNIX_EPOCH)?
        .as_secs() as i64;

    // On Unix, extract inode and dev for cheap change detection
    #[cfg(unix)]
    let (inode, dev) = {
        use std::os::unix::fs::MetadataExt;
        (metadata.ino() as i64, metadata.dev() as i64)
    };
    #[cfg(not(unix))]
    let (inode, dev) = (0i64, 0i64);

    store.insert_file_inventory(&DiscoveredFile {
        file_id,
        path: rel_path.to_string_lossy().into_owned(),
        language,
        mtime,
        size: metadata.len() as i64,
        inode,
        dev,
    })?;

    Ok(())
}

// ── Tier 0.5: fingerprints ─────────────────────────────────────────────────

const TIER0_5_BATCH_SIZE: usize = 64;

fn bootstrap_tier0_5(store: &Store, root: &std::path::Path, running: &AtomicBool) -> Result<()> {
    loop {
        if !running.load(Ordering::SeqCst) {
            return Ok(());
        }

        let batch = store.get_unfingerprinted_files(TIER0_5_BATCH_SIZE)?;
        if batch.is_empty() {
            break;
        }

        for (file_id, path) in &batch {
            let abs_path = root.join(path);
            let content = match fs::read(&abs_path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let hash = workspace::file_content_hash(&content);
            let file_id_arr: [u8; 32] = file_id
                .as_slice()
                .try_into()
                .expect("file_id must be 32 bytes");
            store.set_file_fingerprint(&FileId::from_bytes(file_id_arr), &hash)?;
        }
    }

    Ok(())
}

// ── Tier 1: symbol hints ───────────────────────────────────────────────────

const TIER1_BATCH_SIZE: usize = 1000;

fn bootstrap_tier1(store: &Store, root: &std::path::Path, running: &AtomicBool) -> Result<()> {
    // Process fingerprinted files in batches with offset-based pagination
    let db_batch = 64usize;
    let mut offset: usize = 0;
    loop {
        if !running.load(Ordering::SeqCst) {
            return Ok(());
        }

        let batch = store.get_fingerprinted_files(db_batch, offset)?;
        if batch.is_empty() {
            break;
        }

        let mut hint_buffer: Vec<SymbolHint> = Vec::with_capacity(128);

        for (file_id, path) in &batch {
            let hints = extract_hints_for_path(store, root, file_id, path)?;

            hint_buffer.extend(hints);

            // Flush when buffer reaches batch size
            if hint_buffer.len() >= TIER1_BATCH_SIZE {
                store.insert_symbol_hints_batch(&hint_buffer)?;
                hint_buffer.clear();
            }
        }

        // Flush remaining
        if !hint_buffer.is_empty() {
            store.insert_symbol_hints_batch(&hint_buffer)?;
        }

        offset += batch.len();
    }

    Ok(())
}

fn extract_hints_for_path(
    _store: &Store,
    root: &std::path::Path,
    file_id: &[u8],
    rel_path: &str,
) -> Result<Vec<SymbolHint>> {
    let abs_path = root.join(rel_path);

    // Detect language from extension
    let language = match Language::from_path(&abs_path) {
        Some(lang) => lang,
        None => return Ok(Vec::new()),
    };

    // Create frontend for this language
    let frontend = match create_frontend(language) {
        Some(f) => f,
        None => return Ok(Vec::new()),
    };

    // Read source (UTF-8 decode in memory) and file identity hash (raw bytes)
    let (source, content_hash) = match workspace::read_source(&abs_path) {
        Ok(src) => (src.text, src.file_hash),
        Err(_) => return Ok(Vec::new()),
    };

    // Manifest extraction — top-level symbols only, fast
    let file_id_arr: [u8; 32] = file_id.try_into().expect("file_id must be 32 bytes");
    let f_id = FileId::from_bytes(file_id_arr);
    let facts = match extract_file_with_mode(
        &frontend,
        f_id,
        &abs_path,
        &source,
        &content_hash,
        ExtractionMode::Manifest,
        &(),
    ) {
        Ok(f) => f,
        Err(_) => return Ok(Vec::new()),
    };

    let hints: Vec<SymbolHint> = facts
        .symbols
        .iter()
        .map(|sym| SymbolHint {
            name: sym.name.clone(),
            file_id: file_id.to_vec(),
            kind: sym.kind.as_str().to_string(),
            line: sym.range.start_line,
            confidence: 0.9,
            source: "manifest".to_string(),
            freshness: String::new(),
        })
        .collect();

    Ok(hints)
}

// ── Tier 2: opportunistic manifest extraction ───────────────────────────────

const TIER2_BATCH_SIZE: usize = 10;

/// Opportunistic manifest extraction for fingerprinted files.
///
/// Queries file_inventory for files with fingerprints that haven't had
/// manifest extraction yet.  For each file, runs [`extract_file_with_mode`]
/// with [`ExtractionMode::Manifest`] and persists the results to the core
/// tables (`files`, `symbols`, `scopes`, `extraction_state`).
///
/// Failures on individual files are logged and skipped — they do NOT abort
/// the entire tier.  Cancellation is checked between batches.
///
/// Returns the number of files successfully extracted.
pub(crate) fn bootstrap_tier2(
    store: &Store,
    root: &std::path::Path,
    running: &AtomicBool,
) -> Result<usize> {
    let mut extracted: usize = 0;
    let mut offset: usize = 0;

    loop {
        if !running.load(Ordering::SeqCst) {
            break;
        }

        let batch = store.get_fingerprinted_files_without_manifest(TIER2_BATCH_SIZE, offset)?;
        if batch.is_empty() {
            break;
        }

        for (file_id_bytes, rel_path) in &batch {
            if !running.load(Ordering::SeqCst) {
                break;
            }

            let abs_path = root.join(rel_path);

            // Detect language from extension
            let language = match Language::from_path(&abs_path) {
                Some(lang) => lang,
                None => continue,
            };

            // Create frontend for this language
            let frontend = match create_frontend(language) {
                Some(f) => f,
                None => continue,
            };

            // Read source (UTF-8 decode in memory); content_hash = raw file identity
            let (source, content_hash) = match workspace::read_source(&abs_path) {
                Ok(src) => (src.text, src.file_hash),
                Err(e) => {
                    tracing::debug!(?e, path = %rel_path, "Tier2: failed to read file");
                    continue;
                }
            };

            let file_id_arr: [u8; 32] = file_id_bytes
                .as_slice()
                .try_into()
                .expect("file_id must be 32 bytes");
            let file_id = FileId::from_bytes(file_id_arr);

            // Manifest extraction
            let facts = match extract_file_with_mode(
                &frontend,
                file_id,
                std::path::Path::new(rel_path),
                &source,
                &content_hash,
                ExtractionMode::Manifest,
                &(),
            ) {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!(?e, path = %rel_path, "Tier2: manifest extraction failed");
                    continue;
                }
            };

            // Persist facts to core tables
            if let Err(e) = store.insert_file_facts(&facts) {
                tracing::warn!(?e, path = %rel_path, "Tier2: insert_file_facts failed");
                continue;
            }

            // Record extraction state so we don't re-extract on restart
            if let Err(e) = store.upsert_file_extraction_state(
                &file_id,
                "manifest",
                &content_hash,
                "complete",
                FactCoverage::from_layers(&["manifest"]),
            ) {
                tracing::warn!(?e, path = %rel_path, "Tier2: upsert extraction state failed");
                // Facts are already inserted — this is non-fatal for the file
            }

            extracted += 1;
        }

        offset += batch.len();

        // If the batch was smaller than requested, we're at the end
        if batch.len() < TIER2_BATCH_SIZE {
            break;
        }
    }

    if extracted > 0 {
        tracing::info!(
            extracted,
            "Tier2: opportunistic manifest extraction complete"
        );
    }

    Ok(extracted)
}
