//! Shared CLI progress sink for index/sync pipelines.

use std::sync::{Arc, Mutex};

use atlas_engine::progress::{ProgressPhase, ProgressState};
use atlas_engine::{PhaseName, ProgressEvent, ProgressSink};

/// Translates pipeline [`ProgressEvent`]s into [`ProgressState`] updates
/// consumed by CLI progress renderers.
pub(crate) struct CliProgressSink {
    pub(crate) progress: Arc<Mutex<ProgressState>>,
}

impl ProgressSink for CliProgressSink {
    fn emit(&self, event: ProgressEvent) {
        let mut state = self.progress.lock().unwrap();
        match event {
            ProgressEvent::PhaseStarted { phase, total } => {
                state.start_phase(phase_name_to_progress_phase(phase), None);
                if total > 0 {
                    state.set_total(total);
                }
            }
            ProgressEvent::ItemProgress { completed, .. } => {
                state.set_current(completed);
            }
            ProgressEvent::PhaseFinished {
                succeeded,
                failed,
                detail,
                ..
            } => {
                state.set_current(succeeded + failed);
                if let Some(msg) = detail {
                    state.set_message(msg);
                }
            }
            ProgressEvent::Warning { phase, message } => {
                tracing::warn!("{phase}: {message}");
            }
            ProgressEvent::Cancelled { last_phase } => {
                tracing::info!("Pipeline cancelled at {last_phase}");
            }
        }
    }

    fn progress_state(&self) -> Option<&Arc<Mutex<ProgressState>>> {
        Some(&self.progress)
    }
}

/// Map the pipeline's [`PhaseName`] to the TUI-facing [`ProgressPhase`].
pub(crate) fn phase_name_to_progress_phase(pn: PhaseName) -> ProgressPhase {
    match pn {
        PhaseName::Discovery => ProgressPhase::Discovery,
        PhaseName::HashCheck => ProgressPhase::HashCheck,
        PhaseName::Cleanup => ProgressPhase::Cleanup,
        PhaseName::LanguageInit => ProgressPhase::LanguageInit,
        PhaseName::Extraction => ProgressPhase::Extraction,
        PhaseName::DbWrite => ProgressPhase::DbWrite,
        PhaseName::Resolution => ProgressPhase::Resolution,
        PhaseName::EdgeBuild => ProgressPhase::EdgeBuilding,
        PhaseName::AnnotationMaterialize | PhaseName::SummaryBuild | PhaseName::Finalize => {
            ProgressPhase::Finalizing
        }
        PhaseName::Custom(_) => ProgressPhase::Finalizing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_progress_sink_sets_total_and_current() {
        let progress = Arc::new(Mutex::new(ProgressState::new()));
        let sink = CliProgressSink {
            progress: progress.clone(),
        };

        sink.emit(ProgressEvent::PhaseStarted {
            phase: PhaseName::Extraction,
            total: 7,
        });
        sink.emit(ProgressEvent::ItemProgress {
            phase: PhaseName::Extraction,
            completed: 3,
        });

        let snap = progress.lock().unwrap().read_snapshot();
        assert_eq!(snap.current_phase, Some(ProgressPhase::Extraction));
        assert_eq!(snap.total, Some(7));
        assert_eq!(snap.current, 3);
    }

    #[test]
    fn cli_progress_sink_sets_current_on_phase_finished() {
        let progress = Arc::new(Mutex::new(ProgressState::new()));
        let sink = CliProgressSink {
            progress: progress.clone(),
        };

        sink.emit(ProgressEvent::PhaseStarted {
            phase: PhaseName::DbWrite,
            total: 3,
        });
        sink.emit(ProgressEvent::PhaseFinished {
            phase: PhaseName::DbWrite,
            succeeded: 2,
            failed: 1,
            detail: Some("2 written, 1 failed".into()),
        });

        let snap = progress.lock().unwrap().read_snapshot();
        assert_eq!(snap.current_phase, Some(ProgressPhase::DbWrite));
        assert_eq!(snap.total, Some(3));
        assert_eq!(snap.current, 3);
        assert_eq!(snap.message.as_deref(), Some("2 written, 1 failed"));
    }
}
