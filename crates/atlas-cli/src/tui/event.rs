//! TUI event types and crossterm event reader.
//!
//! Produces high-level [`Event`] items from low-level crossterm events,
//! plus a periodic [`Event::Tick`] for frame-rate control.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::Context;
use ratatui::crossterm::event::{self, Event as CrosstermEvent, KeyEvent};

/// TUI application event.
#[derive(Debug, Clone)]
pub enum Event {
    /// A key press event.
    Key(KeyEvent),
    /// Terminal resize event (columns, rows).
    Resize(u16, u16),
    /// Periodic tick for redraw scheduling.
    Tick,
}

/// Background event reader that converts crossterm events into [`Event`] items.
///
/// Spawns a thread that polls crossterm at the given tick rate.  When no
/// crossterm event arrives within the interval, an [`Event::Tick`] is sent.
pub struct EventHandler {
    rx: mpsc::Receiver<Event>,
}

impl EventHandler {
    /// Create a new event handler polling at `tick_rate`.
    pub fn new(tick_rate: Duration) -> Self {
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            loop {
                match event::poll(tick_rate) {
                    Ok(true) => {
                        let ev = match event::read() {
                            Ok(ev) => ev,
                            Err(_) => break,
                        };
                        let event = match ev {
                            CrosstermEvent::Key(key) => Event::Key(key),
                            CrosstermEvent::Resize(w, h) => Event::Resize(w, h),
                            // Ignore focus, mouse, and paste events.
                            _ => continue,
                        };
                        if tx.send(event).is_err() {
                            break;
                        }
                    }
                    Ok(false) => {
                        // poll timed out — send tick.
                        if tx.send(Event::Tick).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Self { rx }
    }

    /// Block until the next [`Event`] is available.
    pub fn next(&self) -> anyhow::Result<Event> {
        self.rx.recv().context("event channel closed")
    }
}
