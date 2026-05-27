//! TUI progress lifecycle — terminal init (inline), draw loop, graceful shutdown.
//!
//! Inline mode: renders below the shell prompt without a full-screen takeover.
//!
//! ## Ctrl+C detection
//!
//! Raw terminal mode (crossterm `enable_raw_mode()`) disables the `ISIG`
//! terminal flag, so Ctrl+C is delivered as a keyboard event (0x03) rather
//! than the OS-level SIGINT signal.  The `ctrlc` crate's signal handler
//! therefore never fires.  We detect Ctrl+C by polling crossterm keyboard
//! events with `event::poll()` in the draw loop.  The SIGINT handler is
//! retained as a fallback for non-raw-mode environments (pipes, CI).

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use atlas_engine::progress::ProgressState;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::{TerminalOptions, Viewport};

use super::render;

/// Number of terminal rows reserved for the inline progress display.
/// 4 plain rows: completed, gauge, pending, footer.
const INLINE_ROWS: u16 = 4;

/// Owns the ratatui terminal and drives the render loop.
pub struct TuiProgress {
    terminal: ratatui::DefaultTerminal,
    state: Arc<Mutex<ProgressState>>,
    tick: u64,
}

impl TuiProgress {
    /// Initialise inline TUI just below the cursor.
    /// Returns `None` on non-TTY stdout (pipe, CI).
    pub fn try_init(state: Arc<Mutex<ProgressState>>) -> Option<Self> {
        if !atty::is(atty::Stream::Stdout) {
            return None;
        }
        let options = TerminalOptions {
            viewport: Viewport::Inline(INLINE_ROWS),
        };
        let terminal = match ratatui::try_init_with_options(options) {
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
            // ── Check exit flags FIRST — before any blocking call ──
            // This is load-bearing for Ctrl+C: if the signal arrives
            // during sleep or the previous draw, we must exit
            // *before* the next Mutex-lock or terminal-write, not after.
            if stop_flag.load(Ordering::SeqCst) {
                return true;
            }
            if done_flag.load(Ordering::SeqCst) {
                return false;
            }

            // ── Flush atomic counters into ProgressState ──
            // Use try_lock to avoid blocking if the worker holds the
            // mutex (rare, but possible during phase transitions).
            match self.state.try_lock() {
                Ok(mut s) => {
                    s.flush_and_snapshot();
                }
                Err(_) => {
                    // Mutex is held by the worker — skip this frame
                    // and try again after sleep.  The atomic counters
                    // will be picked up on the next iteration.
                }
            }

            // Double-check flags after flush (may have changed during lock wait)
            if stop_flag.load(Ordering::SeqCst) {
                return true;
            }
            if done_flag.load(Ordering::SeqCst) {
                return false;
            }

            // ── Render one frame ──
            if let Err(_) = self.draw() {
                // Terminal error — skip this frame.
            }

            // ── Poll for Ctrl+C key event ──
            // In raw mode, ISIG is off — Ctrl+C arrives as a keyboard
            // event, not as SIGINT.  We poll with a zero timeout: if
            // there are events, read them; otherwise continue.
            //
            // Drain ALL pending events (not just Ctrl+C) to avoid
            // event queue back-pressure.
            while event::poll(Duration::from_millis(0)).unwrap_or(false) {
                if let Ok(Event::Key(key)) = event::read() {
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        return true;
                    }
                }
            }

            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Finish on normal completion — renders a compact summary frame
    /// via ratatui (replacing the progress bars), then drops the
    /// terminal and positions the cursor below the summary.  No manual
    /// escape-sequence clearing is needed — ratatui handles all screen
    /// management within the viewport.
    pub fn finish(mut self, files: u64, symbols: u64, edges: u64) {
        // Render the completion summary — this overwrites the progress
        // bars with the final statistics inside the same 4-row viewport.
        let _ = self
            .terminal
            .draw(|frame| render::render_summary(frame, files, symbols, edges));

        // Drop the terminal.  The Drop impl calls ratatui::try_restore(),
        // which disables raw mode and restores the cursor to its
        // pre-init position (the start of the first summary row).
        drop(self);

        // Move the cursor down by INLINE_ROWS to land just below the
        // rendered summary.  This is the same pattern as `leave()`
        // above; it uses a standard cursor-move escape (not content
        // clearing), and the summary stays intact on screen.
        let down = format!("\x1b[{}B", INLINE_ROWS);
        let _ = std::io::Write::write_all(&mut std::io::stdout(), down.as_bytes());
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }

    /// Leave on Ctrl+C — exits raw mode while preserving the progress
    /// display on screen (like `wget` on interrupt).  The cursor lands
    /// below the rendered content so the interrupt message prints
    /// cleanly without overwriting the progress lines.
    pub fn leave(self) {
        // Drop the terminal.  The Drop impl calls ratatui::try_restore(),
        // which disables raw mode and restores the cursor to its
        // pre-init position (above the rendered inline content).
        drop(self);

        // Move cursor forward by INLINE_ROWS to land below the rendered
        // content.  This allows the interrupt message to print on the
        // next line without overwriting the progress display.
        let down = format!("\x1b[{}B", INLINE_ROWS);
        let _ = std::io::Write::write_all(&mut std::io::stdout(), down.as_bytes());
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }
}

impl Drop for TuiProgress {
    fn drop(&mut self) {
        // Best-effort restore.  If finish() was called, this is a no-op.
        let _ = ratatui::try_restore();
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

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
