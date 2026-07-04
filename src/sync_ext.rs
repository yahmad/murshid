//! Poison-tolerant mutex locking.
//!
//! Every piece of shared mutable state in murshid is recompute-on-read — the
//! `events` table is the append-only spine, and the in-memory `Mutex`-guarded
//! state (queue, budget bucket, pending card/offer, tracking) is derived and
//! rebuildable. A panic in another thread poisons a `Mutex`, but the guarded
//! data is never left in a shape a reader can't tolerate, so recovering the
//! guard is always the right move here. This centralizes the ~100
//! `.lock().unwrap_or_else(|e| e.into_inner())` poison-recovery sites behind one
//! named call so the intent is stated once, not copy-pasted.

use std::sync::{Mutex, MutexGuard};

pub trait LockExt<T> {
    /// Locks the mutex, recovering the guard if it was poisoned (see the
    /// module doc for why that is safe throughout this crate).
    fn lock_poison_safe(&self) -> MutexGuard<'_, T>;
}

impl<T> LockExt<T> for Mutex<T> {
    fn lock_poison_safe(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_lock_poison_safe_recovers_a_poisoned_mutex() {
        let m = Arc::new(Mutex::new(7));
        let m2 = Arc::clone(&m);
        // Poison the mutex: panic while holding the guard.
        let _ = std::thread::spawn(move || {
            let _guard = m2.lock().unwrap();
            panic!("poison it");
        })
        .join();
        assert!(m.lock().is_err(), "mutex should be poisoned for this test");
        // The poison-safe lock still yields the last-written value.
        assert_eq!(*m.lock_poison_safe(), 7);
    }

    #[test]
    fn test_lock_poison_safe_matches_plain_lock_when_healthy() {
        let m = Mutex::new(String::from("ok"));
        *m.lock_poison_safe() = "changed".to_string();
        assert_eq!(&*m.lock_poison_safe(), "changed");
    }
}
