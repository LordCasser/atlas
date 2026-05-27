//! Plain-text progress fallback for non-TTY environments (pipes, CI).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use atlas_engine::progress::{PhaseState, ProgressPhase, ProgressState};

/// Plain-text progress renderer for non-TTY stdout.
pub struct TextFallback {
    state: Arc<Mutex<ProgressState>>,
    last_phase: Option<ProgressPhase>,
    last_print: Instant,
    last_percent: Option<u32>,
}

impl TextFallback {
    pub fn new(state: Arc<Mutex<ProgressState>>) -> Self {
        Self {
            state,
            last_phase: None,
            last_print: Instant::now(),
            last_percent: None,
        }
    }

    /// Called periodically (every 200ms) to update the progress display.
    pub fn tick(&mut self) {
        let guard = self.state.lock().unwrap();
        let snap = guard.read_snapshot();
        let now = Instant::now();

        if self.last_phase != snap.current_phase {
            if self.last_phase.is_some() {
                eprint!("\n");
            }
            if let Some(phase) = snap.current_phase {
                eprintln!(
                    "[{}] {}",
                    phase.display_name(),
                    snap.message.as_deref().unwrap_or("")
                );
            }
            self.last_phase = snap.current_phase;
            self.last_percent = None;
            self.last_print = now;
        }

        if now.duration_since(self.last_print) < Duration::from_millis(500) {
            return;
        }

        if let Some(total) = snap.total {
            if total > 0 {
                let pct = ((snap.current as f64 / total as f64) * 100.0) as u32;
                let should_print = self.last_percent.map_or(true, |last| pct > last);
                if should_print {
                    let rate_str = snap.rate.map_or(String::new(), |r| format!("  {:.0}/s", r));
                    eprint!("\r  {}/{} ({}%){}", snap.current, total, pct, rate_str);
                    self.last_percent = Some(pct);
                    self.last_print = now;
                }
            }
        } else if snap.current > 0 {
            let rate_str = snap.rate.map_or(String::new(), |r| format!("  {:.0}/s", r));
            eprint!("\r  {} matched{}", snap.current, rate_str);
            self.last_print = now;
        }
    }

    /// Flush and print completion.
    pub fn finish(&mut self) {
        eprint!("\n");
        let guard = self.state.lock().unwrap();
        let snap = guard.read_snapshot();

        println!();
        for entry in &snap.phases {
            if let PhaseState::Completed { elapsed, note, .. } = &entry.state {
                println!(
                    "  {} {}  ({:.1}s)",
                    entry.phase.display_name(),
                    note.as_deref().unwrap_or("— done"),
                    elapsed.as_secs_f64()
                );
            }
        }
    }
}
