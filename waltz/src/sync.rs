use std::sync::{Mutex, MutexGuard, PoisonError};

/// Recover from poisoning: a panic contained by supervision must not disable the guarded
/// structure, none of which this crate leaves inconsistent.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
