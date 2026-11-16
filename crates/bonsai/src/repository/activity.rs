//! The activity log (CONTEXT.md): every Git command an operation ran.

use std::{collections::VecDeque, sync::Mutex};

use bonsai_git::{CommandLog, CommandRecord};

/// How many commands the log keeps per repository.
const LIMIT: usize = 1_000;

/// The commands run in this repository, oldest first, capped at 1,000.
#[derive(Default)]
pub struct Activity {
    /// Each record with a number that stays the same as older ones drop off.
    records: VecDeque<(u64, CommandRecord)>,
    next: u64,
}

impl Activity {
    pub fn push(&mut self, record: CommandRecord) {
        if self.records.len() == LIMIT {
            self.records.pop_front();
        }
        self.records.push_back((self.next, record));
        self.next += 1;
    }

    pub fn extend(&mut self, records: impl IntoIterator<Item = CommandRecord>) {
        for record in records {
            self.push(record);
        }
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &CommandRecord> {
        self.records.iter().map(|(_, record)| record)
    }

    /// Records with their numbers, oldest first.
    pub fn entries(&self) -> impl DoubleEndedIterator<Item = (u64, &CommandRecord)> {
        self.records.iter().map(|(id, record)| (*id, record))
    }

    #[must_use]
    pub fn get(&self, id: u64) -> Option<&CommandRecord> {
        let first = self.records.front()?.0;
        let index = usize::try_from(id.checked_sub(first)?).ok()?;
        self.records.get(index).map(|(_, record)| record)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

/// Collects records from background threads until the model drains them.
#[derive(Default)]
pub(super) struct ActivitySink(Mutex<Vec<CommandRecord>>);

impl ActivitySink {
    pub(super) fn drain(&self) -> Vec<CommandRecord> {
        std::mem::take(
            &mut *self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}

impl CommandLog for ActivitySink {
    fn record(&self, record: &CommandRecord) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(record.clone());
    }
}
