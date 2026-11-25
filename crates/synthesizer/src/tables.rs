//! Live wavetable handoff: the coordinator builds tables off-thread and sends
//! them into the running synth through a bounded queue; the synth crossfades
//! and returns the old tables for deallocation off the audio thread.

use rawdawg_dsp::wavetable::MipTable;
use rawdawg_rt::queue::{ControlQueue, QueueConsumer, QueueProducer};

/// One prepared table for a layer.
pub struct TableUpdate {
    pub layer: usize,
    pub table: Box<MipTable>,
    /// Generation stamp so stale builds are ignored by the receiver.
    pub generation: u64,
}

/// Coordinator side.
pub struct TableSender {
    updates: QueueProducer<TableUpdate>,
    retired: QueueConsumer<Box<MipTable>>,
    generation: u64,
}

/// Synth (RT) side.
pub struct TableInbox {
    updates: QueueConsumer<TableUpdate>,
    retired: QueueProducer<Box<MipTable>>,
    last_generation: [u64; crate::patch::MAX_LAYERS],
}

pub const TABLE_QUEUE_DEPTH: usize = 8;

pub fn table_channel() -> (TableSender, TableInbox) {
    let (up, uc) = ControlQueue::create::<TableUpdate>(TABLE_QUEUE_DEPTH);
    let (rp, rc) = ControlQueue::create::<Box<MipTable>>(TABLE_QUEUE_DEPTH * 2);
    (TableSender { updates: up, retired: rc, generation: 0 }, TableInbox { updates: uc, retired: rp, last_generation: [0; crate::patch::MAX_LAYERS] })
}

impl TableSender {
    /// Offer a table; returns it back if the queue is full (coalesce and retry later).
    pub fn send(&mut self, layer: usize, table: Box<MipTable>) -> Result<u64, Box<MipTable>> {
        self.generation += 1;
        let g = self.generation;
        self.updates.push(TableUpdate { layer, table, generation: g }).map(|_| g).map_err(|u| u.table)
    }
    /// Drop retired tables here (off RT). Returns how many were freed.
    pub fn collect_retired(&mut self) -> usize {
        let mut n = 0;
        while self.retired.pop().is_some() {
            n += 1;
        }
        n
    }
    pub fn pending(&self) -> usize {
        TABLE_QUEUE_DEPTH - self.updates.free_slots()
    }
    pub fn is_receiver_alive(&self) -> bool {
        self.updates.is_consumer_alive()
    }
}

impl TableInbox {
    /// RT: next update whose generation is newer than the last applied for its layer.
    #[inline]
    pub fn pop(&mut self) -> Option<TableUpdate> {
        while let Some(u) = self.updates.pop() {
            if u.layer < crate::patch::MAX_LAYERS && u.generation > self.last_generation[u.layer] {
                self.last_generation[u.layer] = u.generation;
                return Some(u);
            }
            // stale or invalid: must not drop on RT; park it in the retire queue
            if self.retired.push(u.table).is_err() {
                // retire queue full: nothing we can do without allocating; leak is
                // impossible to recover here, so stop draining this block
                return None;
            }
        }
        None
    }
    #[inline]
    pub fn can_retire(&self) -> bool {
        self.retired.free_slots() > 0
    }
    #[inline]
    pub fn retire(&mut self, table: Box<MipTable>) -> Result<(), Box<MipTable>> {
        self.retired.push(table)
    }
}
