//! Validates the contract suite itself against a minimal in-memory store: with no shipped
//! reference implementation, this proves the checks are runnable and pass against a store which
//! follows the contract by construction.

#![cfg(feature = "persistence-tests")]

use std::{
    collections::HashMap,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};
use thiserror::Error;
use waltz::{
    AppendError, EncodedEvent, EncodedSnapshot, EventStore, PersistenceId, SeqNo, SnapshotStore,
    StoredEvent, StoredSnapshot,
};

#[tokio::test]
async fn event_store_contract() {
    waltz::persistence_tests::event_store_contract(MemoryStore::default()).await;
}

#[tokio::test]
async fn snapshot_store_contract() {
    waltz::persistence_tests::snapshot_store_contract(MemoryStore::default()).await;
}

#[tokio::test]
async fn snapshot_with_event_tail() {
    let store = MemoryStore::default();
    waltz::persistence_tests::snapshot_with_event_tail(store.clone(), store).await;
}

#[derive(Debug, Clone, Default)]
struct MemoryStore {
    streams: Arc<Mutex<HashMap<PersistenceId, Vec<StoredEvent>>>>,
    snapshots: Arc<Mutex<HashMap<PersistenceId, StoredSnapshot>>>,
}

impl EventStore for MemoryStore {
    type Error = MemoryStoreError;

    async fn append(
        &self,
        id: &PersistenceId,
        next_seq_no: SeqNo,
        events: Vec<EncodedEvent>,
    ) -> Result<(), AppendError<Self::Error>> {
        let mut streams = self.streams.lock().expect("streams lock poisoned");
        let stream = streams.entry(id.clone()).or_default();
        if SeqNo::new(stream.len() as u64) != next_seq_no {
            return Err(AppendError::Conflict);
        }

        for (n, event) in events.into_iter().enumerate() {
            stream.push(StoredEvent {
                seq_no: next_seq_no.advanced_by(n),
                event,
            });
        }

        Ok(())
    }

    async fn read(
        &self,
        id: &PersistenceId,
        from_seq_no: SeqNo,
        limit: NonZeroUsize,
    ) -> Result<Vec<StoredEvent>, Self::Error> {
        let streams = self.streams.lock().expect("streams lock poisoned");
        let events = streams
            .get(id)
            .map(|stream| {
                stream
                    .iter()
                    .filter(|stored| stored.seq_no >= from_seq_no)
                    .take(limit.get())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        Ok(events)
    }
}

impl SnapshotStore for MemoryStore {
    type Error = MemoryStoreError;

    async fn save(
        &self,
        id: &PersistenceId,
        next_seq_no: SeqNo,
        snapshot: EncodedSnapshot,
    ) -> Result<(), Self::Error> {
        self.snapshots
            .lock()
            .expect("snapshots lock poisoned")
            .insert(
                id.clone(),
                StoredSnapshot {
                    next_seq_no,
                    snapshot,
                },
            );

        Ok(())
    }

    async fn load(&self, id: &PersistenceId) -> Result<Option<StoredSnapshot>, Self::Error> {
        let snapshot = self
            .snapshots
            .lock()
            .expect("snapshots lock poisoned")
            .get(id)
            .cloned();

        Ok(snapshot)
    }
}

#[derive(Debug, Error)]
#[error("memory store failure")]
struct MemoryStoreError;
