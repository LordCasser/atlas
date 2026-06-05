//! Terminal progress lifecycle backed by indicatif.
//!
//! The index command is a command-line workflow, not a full-screen UI.  The
//! progress display therefore behaves like wget/curl: it updates one terminal
//! line while work is running, preserves that line on Ctrl+C, and prints normal
//! command output below it on completion or interruption.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use atlas_engine::progress::{PhaseState, ProgressPhase, ProgressSnapshot, ProgressState};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

const TICK_MS: u64 = 200;

/// Owns the terminal progress bar and drives periodic updates.
pub struct TuiProgress {
    bar: ProgressBar,
    state: Arc<Mutex<ProgressState>>,
    last_phase: Option<ProgressPhase>,
    last_had_total: Option<bool>,
}

impl TuiProgress {
    /// Initialise terminal progress. Returns `None` on non-TTY stdout.
    pub fn try_init(state: Arc<Mutex<ProgressState>>) -> Option<Self> {
        if !atty::is(atty::Stream::Stdout) {
            return None;
        }

        let bar = ProgressBar::new_spinner();
        bar.set_draw_target(ProgressDrawTarget::stdout_with_hz(10));
        bar.set_style(spinner_style());
        bar.enable_steady_tick(Duration::from_millis(120));

        Some(Self {
            bar,
            state,
            last_phase: None,
            last_had_total: None,
        })
    }

    /// Blocking draw loop — renders every 200 ms until done or stopped.
    /// Returns `true` if stopped by Ctrl+C.
    pub fn draw_loop(&mut self, done_flag: &AtomicBool, stop_flag: &AtomicBool) -> bool {
        loop {
            if stop_flag.load(Ordering::SeqCst) {
                return true;
            }
            if done_flag.load(Ordering::SeqCst) {
                return false;
            }

            let snap = self
                .state
                .try_lock()
                .ok()
                .map(|mut state| state.flush_and_snapshot());
            if let Some(snap) = snap {
                self.render_snapshot(&snap);
            }

            if stop_flag.load(Ordering::SeqCst) {
                return true;
            }
            if done_flag.load(Ordering::SeqCst) {
                return false;
            }

            std::thread::sleep(Duration::from_millis(TICK_MS));
        }
    }

    /// Finish on normal completion.
    pub fn finish(self, files: u64, symbols: u64, edges: u64) {
        self.bar.finish_and_clear();
        print_summary(files, symbols, edges);
    }

    /// Clear the progress line without printing an index-specific summary.
    pub fn clear(self) {
        self.bar.finish_and_clear();
    }

    /// Finish after Ctrl+C and print the interrupt summary below.
    pub fn interrupt(self, state: &ProgressState) {
        self.bar.finish_and_clear();
        print_interrupted_stdout(state);
    }

    fn render_snapshot(&mut self, snap: &ProgressSnapshot) {
        let phase = snap.current_phase;
        let has_total = snap.total.is_some();
        if self.last_phase != phase || self.last_had_total != Some(has_total) {
            self.last_phase = phase;
            self.last_had_total = Some(has_total);
            self.bar.reset_elapsed();
            if has_total {
                self.bar.set_style(bar_style());
            } else {
                self.bar.set_style(spinner_style());
            }
        }

        let prefix = phase
            .map(|p| p.display_name().to_string())
            .unwrap_or_else(|| "Starting".to_string());
        self.bar.set_prefix(prefix);

        if let Some(total) = snap.total {
            self.bar.set_length(total);
            self.bar.set_position(snap.current.min(total));
        } else {
            self.bar.unset_length();
            self.bar.set_position(snap.current);
            self.bar.tick();
        }

        self.bar.set_message(progress_message(snap));
    }
}

/// Print a brief interrupt message after Ctrl+C.
pub fn print_interrupted(state: &ProgressState) {
    let mut stderr = std::io::stderr();
    let _ = write_interrupted(&mut stderr, state);
}

fn spinner_style() -> ProgressStyle {
    ProgressStyle::with_template("{spinner} {prefix} {pos} {msg}")
        .unwrap_or_else(|_| ProgressStyle::default_spinner())
}

fn bar_style() -> ProgressStyle {
    ProgressStyle::with_template("{prefix} {bar:32} {pos}/{len} ({percent}%) {msg}")
        .unwrap_or_else(|_| ProgressStyle::default_bar())
        .progress_chars("=>-")
}

fn progress_message(snap: &ProgressSnapshot) -> String {
    let mut parts = Vec::new();
    if let Some(rate) = snap.rate {
        parts.push(format!("{rate:.0}/s"));
    }
    parts.push(format!("elapsed {:.1}s", snap.elapsed.as_secs_f64()));
    if let Some(msg) = &snap.message {
        parts.push(msg.clone());
    }
    parts.join(" | ")
}

fn print_summary(files: u64, symbols: u64, edges: u64) {
    println!(" ◆ Index complete");
    println!("   Files:   {files}");
    println!("   Symbols: {symbols}");
    println!("   Edges:   {edges}");
}

fn print_interrupted_stdout(state: &ProgressState) {
    let mut stdout = std::io::stdout();
    let _ = write_interrupted(&mut stdout, state);
}

fn write_interrupted<W: Write>(writer: &mut W, state: &ProgressState) -> std::io::Result<()> {
    let snap = state.read_snapshot();
    writeln!(writer, "Interrupted.")?;

    if let Some(phase) = snap.current_phase {
        writeln!(writer, "  Current phase: {}", phase.display_name())?;
        if snap.current > 0 {
            writeln!(writer, "  Progress: {}", snap.current)?;
        }
    }

    for entry in &snap.phases {
        if let PhaseState::Completed { note: Some(n), .. } = &entry.state {
            writeln!(writer, "  {} — {}", entry.phase.display_name(), n)?;
        }
    }

    writeln!(
        writer,
        "  Partial results are saved. Run `atlas index` again to resume."
    )?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::{bar_style, spinner_style};

    #[test]
    fn progress_styles_compile() {
        let _ = spinner_style();
        let _ = bar_style();
    }
}
