use super::*;
use std::{fs::Metadata, path::PathBuf, time::SystemTime};

/// Reuses successful parsing within one caller-owned request over an immutable
/// Store and source root. It never caches selection results or failures. Drop it
/// at request completion; do not retain it across requests or mutate its inputs.
pub struct CallContextSession {
    store: Arc<Store>,
    root: PathBuf,
    pub(super) reusable: ReusableSources,
}

impl CallContextSession {
    pub fn new(store: Arc<Store>, root: PathBuf) -> Self {
        Self {
            store,
            root,
            reusable: ReusableSources::default(),
        }
    }

    /// Each selection retains the ordinary file/analysis/item/cancellation
    /// limits. Result read counters charge only reads actually performed here.
    pub fn inspect(
        &mut self,
        path: &str,
        start_byte: u32,
        end_byte: u32,
        include_control_conditions: bool,
        canceled: &dyn Fn() -> bool,
    ) -> anyhow::Result<CallContextResult> {
        let _diagnostic = tracing::debug_span!(target: "atlas_context_work", "context_selection", path, start_byte, end_byte).entered();
        let mut query = Investigation {
            store: &self.store,
            root: &self.root,
            canceled,
            parsed: BTreeMap::new(),
            result: CallContextResult::default(),
            symbol_static: BTreeMap::new(),
            reusable: Some(&mut self.reusable),
            admitted_bytes: 0,
        };
        let outcome = query.inspect(path, start_byte, end_byte, include_control_conditions);
        let result = query.result;
        if let Err(error) = outcome {
            self.reusable = ReusableSources::default();
            return Err(error);
        }
        tracing::debug!(target: "atlas_context_work", event = "retained", path,
            retained_source_bytes = self.reusable.bytes, retained_files = self.reusable.files.len(),
            peak_retained_source_bytes = self.reusable.peak_bytes, peak_retained_files = self.reusable.peak_files);
        Ok(result)
    }
}

// A metadata check supplements the immutable-input contract and prevents reuse
// after observable replacement/change, without rereading the file on a hit.
#[derive(PartialEq, Eq)]
pub(super) struct SourceStamp {
    len: u64,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl SourceStamp {
    pub(super) fn new(metadata: &Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            created: metadata.created().ok(),
            #[cfg(unix)]
            identity: (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            ),
        }
    }
}

#[derive(Default)]
pub(super) struct ReusableSources {
    pub(super) files: BTreeMap<FileId, Arc<ParsedSource>>,
    pub(super) bytes: usize,
    peak_bytes: usize,
    peak_files: usize,
}

impl ReusableSources {
    pub(super) fn get(&mut self, id: FileId, stamp: &SourceStamp) -> Option<Arc<ParsedSource>> {
        let source = self.files.get(&id)?;
        if &source.stamp == stamp {
            return Some(source.clone());
        }
        self.bytes -= self.files.remove(&id).unwrap().source.len();
        None
    }

    pub(super) fn insert(&mut self, id: FileId, source: Arc<ParsedSource>) {
        if self.files.contains_key(&id) || source.source.len() > MAX_CONTEXT_FILE_BYTES {
            return;
        }
        // Source bytes and entry count bound retained trees and CppFileTypes;
        // this is not an allocator or RSS byte bound. Selection-local live Arcs
        // remain governed by the ordinary 32 MiB analysis limit.
        while self.bytes + source.source.len() > MAX_CONTEXT_FILE_BYTES || self.files.len() >= 16 {
            let (_, evicted) = self.files.pop_first().unwrap();
            self.bytes -= evicted.source.len();
        }
        self.bytes += source.source.len();
        self.files.insert(id, source);
        self.peak_bytes = self.peak_bytes.max(self.bytes);
        self.peak_files = self.peak_files.max(self.files.len());
    }
}
