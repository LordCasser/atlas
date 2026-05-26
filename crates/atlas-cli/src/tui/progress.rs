//! TUI progress lifecycle — terminal init, draw loop, graceful shutdown.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use atlas_engine::progress::ProgressState;

use super::render;

/// Owns the ratatui terminal and drives the render loop.
pub struct TuiProgress {
    terminal: ratatui::DefaultTerminal,
    state: Arc<Mutex<ProgressState>>,
    tick: u64,
}

impl TuiProgress {
    /// Initialise the TUI. Returns `None` on non-TTY stdout (pipe, CI).
    pub fn try_init(state: Arc<Mutex<ProgressState>>) -> Option<Self> {
        if !atty::is(atty::Stream::Stdout) {
            return None;
        }
        let terminal = match ratatui::try_init() {
            Ok(t) => t,
            Err(_) => return None,
        };
        Some(Self {
            terminal,
            state,
            tick: 0,
        })
    }

    /// Render one frame.
    fn draw(&mut self) -> io::Result<()> {
        self.tick = self.tick.wrapping_add(1);
        let state = self.state.clone();
        self.terminal
            .draw(|frame| render::render(frame, state, self.tick))?;
        Ok(())
    }

    /// Blocking draw loop — renders every 200 ms until done or stopped.
    /// Returns `true` if stopped by Ctrl+C.
    pub fn draw_loop(
        &mut self,
        done_flag: &AtomicBool,
        stop_flag: &AtomicBool,
    ) -> bool {
        loop {
            {
                let mut s = self.state.lock().unwrap();
                s.flush_and_snapshot();
            }

            if let Err(e) = self.draw() {
                eprintln!("TUI draw error: {}", e);
                return false;
            }

            if done_flag.load(Ordering::SeqCst) {
                return false;
            }
            if stop_flag.load(Ordering::SeqCst) {
                return true;
            }

            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Restore terminal state.
    pub fn finish(self) {
        // ratatui::try_restore() is called on drop.
        // We take ownership to enforce terminal cleanup.
        drop(self);
        let _ = ratatui::try_restore();
    }
}

impl Drop for TuiProgress {
    fn drop(&mut self) {
        let _ = ratatui::try_restore();
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// Print a post-mortem summary after the TUI is closed.
pub fn print_summary(_state: &ProgressState) {
    // Simple summary for now — full timing is printed by index.rs afterward.
    println!();
}

/// Print a brief interrupt message after Ctrl+C.
pub fn print_interrupted(state: &ProgressState) {
    use atlas_engine::progress::PhaseState;
    let snap = state.read_snapshot();
    eprintln!("\nInterrupted.");

    if let Some(phase) = snap.current_phase {
        eprintln!("  Current phase: {}", phase.display_name());
        if snap.current > 0 {
            eprintln!("  Progress: {}", snap.current);
        }
    }

    for entry in &snap.phases {
        if let PhaseState::Completed { note, .. } = &entry.state {
            if let Some(n) = note {
                eprintln!("  {} — {}", entry.phase.display_name(), n);
            }
        }
    }

    eprintln!("  Partial results are saved. Run `atlas index` again to resume.");
}
