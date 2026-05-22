//! Optional file watcher using the `notify` crate.
//!
//! Provides a streaming channel of file change events for real-time incremental sync.

use anyhow::Result;
use std::path::PathBuf;

/// A file system event from the watcher.
#[derive(Debug, Clone)]
pub enum WatchEvent {
    Created(PathBuf),
    Modified(PathBuf),
    Removed(PathBuf),
}

/// File system watcher that emits `WatchEvent`s on a channel.
pub struct FileWatcher {
    _watcher: Option<notify::RecommendedWatcher>,
}

impl FileWatcher {
    /// Start watching `root` for file changes.
    /// Events are delivered via the returned `std::sync::mpsc::Receiver`.
    pub fn start(root: &std::path::Path) -> Result<(Self, std::sync::mpsc::Receiver<WatchEvent>)> {
        use notify::{Event, EventKind, RecursiveMode, Watcher};

        let (tx, rx) = std::sync::mpsc::channel();

        let root = root.to_path_buf();
        let mut watcher = notify::recommended_watcher(
            move |event: std::result::Result<Event, notify::Error>| {
                if let Ok(event) = event {
                    let kind = match event.kind {
                        EventKind::Create(_) => WatchEvent::Created,
                        EventKind::Modify(_) => WatchEvent::Modified,
                        EventKind::Remove(_) => WatchEvent::Removed,
                        _ => return,
                    };
                    for path in event.paths {
                        let _ = tx.send(kind(path));
                    }
                }
            },
        )?;

        watcher.watch(&root, RecursiveMode::Recursive)?;

        Ok((
            Self {
                _watcher: Some(watcher),
            },
            rx,
        ))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn watcher_module_exists() {
        // Basic smoke test — actual watcher requires a filesystem and notify dep
    }
}
