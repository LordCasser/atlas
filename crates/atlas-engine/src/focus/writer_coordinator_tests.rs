//! Tests for ProjectWriteCoordinator — serialized DB write access.

use super::scheduler::FocusPriority;
use super::writer_coordinator::ProjectWriteCoordinator;

// ── Tests ───────────────────────────────────────────────────────────────────

#[test]
fn test_coordinator_new() {
    let coordinator = ProjectWriteCoordinator::new();
    assert!(
        !coordinator.is_background_cancelled(),
        "new coordinator should not be cancelled"
    );
}

#[test]
fn test_acquire_sync_sets_cancellation() {
    let coordinator = ProjectWriteCoordinator::new();
    assert!(!coordinator.is_background_cancelled());

    let _guard = coordinator.acquire(FocusPriority::Sync);
    assert!(
        coordinator.is_background_cancelled(),
        "acquiring Sync should set background_cancelled"
    );
}

#[test]
fn test_reset_cancellation() {
    let coordinator = ProjectWriteCoordinator::new();

    // Acquire sets cancellation
    let guard = coordinator.acquire(FocusPriority::Sync);
    assert!(coordinator.is_background_cancelled());

    // Drop guard + reset
    drop(guard);
    coordinator.reset_cancellation();
    assert!(
        !coordinator.is_background_cancelled(),
        "reset_cancellation should clear the flag"
    );
}

#[test]
fn test_acquire_returns_guard_with_correct_priority() {
    let coordinator = ProjectWriteCoordinator::new();

    let guard = coordinator.acquire(FocusPriority::Sync);
    assert_eq!(guard.priority, FocusPriority::Sync);
    drop(guard);

    let guard = coordinator.acquire(FocusPriority::UserFocus);
    assert_eq!(guard.priority, FocusPriority::UserFocus);
    drop(guard);

    let guard = coordinator.acquire(FocusPriority::Recent);
    assert_eq!(guard.priority, FocusPriority::Recent);
    drop(guard);

    let guard = coordinator.acquire(FocusPriority::Speculative);
    assert_eq!(guard.priority, FocusPriority::Speculative);
    drop(guard);
}

#[test]
fn test_exclusive_guard() {
    let coordinator = ProjectWriteCoordinator::new();
    assert!(!coordinator.is_background_cancelled());

    let _exclusive = coordinator.enter_exclusive();
    assert!(
        coordinator.is_background_cancelled(),
        "enter_exclusive should set background_cancelled"
    );
}

#[test]
fn test_user_focus_also_sets_cancellation() {
    let coordinator = ProjectWriteCoordinator::new();
    assert!(!coordinator.is_background_cancelled());

    let _guard = coordinator.acquire(FocusPriority::UserFocus);
    assert!(
        coordinator.is_background_cancelled(),
        "acquiring UserFocus should also set background_cancelled"
    );
}
