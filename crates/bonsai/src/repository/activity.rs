//! The activity log (CONTEXT.md): every Git command an operation ran.

use std::{collections::VecDeque, sync::Mutex};

use bonsai_git::{CommandLog, CommandRecord};

/// How many commands the log keeps per repository.
const LIMIT: usize = 1_000;

/// The commands run in this repository, oldest first, capped at 1,000.
#[derive(Default)]
pub struct Activity {
    records: VecDeque<CommandRecord>,
}

impl Activity {
    pub fn push(&mut self, record: CommandRecord) {
        if self.records.len() == LIMIT {
            self.records.pop_front();
        }
        self.records.push_back(record);
    }

    pub fn extend(&mut self, records: impl IntoIterator<Item = CommandRecord>) {
        for record in records {
            self.push(record);
        }
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &CommandRecord> {
        self.records.iter()
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
