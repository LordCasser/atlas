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
            ProgressEvent::PhaseFinished { detail, .. } => {
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
