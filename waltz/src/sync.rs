use std::sync::{Mutex, MutexGuard, PoisonError};

/// Recover from poisoning: a panic contained by supervision must not disable the guarded
/// structure, none of which this crate leaves inconsistent.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(feature = "remote")]
pub(crate) fn read<T>(rw_lock: &std::sync::RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    rw_lock.read().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(feature = "remote")]
pub(crate) fn write<T>(rw_lock: &std::sync::RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    rw_lock.write().unwrap_or_else(PoisonError::into_inner)
}
