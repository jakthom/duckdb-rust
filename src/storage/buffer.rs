//! Resident native-block ownership.  Admission happens before loading; a pin
//! owns the payload reservation and eviction never discards dirty/pinned data.
use std::{collections::BTreeMap, sync::{Arc, Mutex}};
use crate::{common::{Error, Result}, parallel::{MemoryPool, QueryContext, Reservation}};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadOrigin { Memory, PhysicalRead }
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IoAttribution { pub physical_reads: u64, pub physical_bytes: u64, pub cache_loads: u64 }
struct Payload { bytes: Arc<[u8]>, _reservation: Reservation }
struct Entry { payload: Arc<Payload>, pins: usize, dirty: bool, tick: u64 }
#[derive(Default)] struct State { entries: BTreeMap<u64, Entry>, tick: u64, io: IoAttribution }
pub struct BlockBuffer { pool: Arc<MemoryPool>, limit: usize, state: Mutex<State> }
pub struct Pin { id: u64, payload: Arc<Payload>, owner: Arc<BlockBuffer> }

impl BlockBuffer {
    pub fn new(pool: Arc<MemoryPool>, limit: usize) -> Arc<Self> { Arc::new(Self { pool, limit, state: Mutex::new(State::default()) }) }
    /// `expected_bytes` is a checked allocation contract supplied by the native
    /// block reader.  The loader runs while the entry lock is held, making one
    /// miss install atomic; callers must keep loaders bounded/non-reentrant.
    pub fn pin_or_load(self: &Arc<Self>, id: u64, expected_bytes: usize, origin: LoadOrigin, load: impl FnOnce() -> Result<Vec<u8>>, query: &QueryContext) -> Result<Pin> {
        query.check()?;
        let mut state = self.state.lock().map_err(|_| Error::Internal("buffer lock poisoned".into()))?;
        state.tick = state.tick.checked_add(1).ok_or_else(|| Error::Resource("buffer LRU clock exhausted".into()))?;
        let tick = state.tick;
        if let Some(entry) = state.entries.get_mut(&id) { entry.pins += 1; entry.tick = tick; return Ok(Pin { id, payload: entry.payload.clone(), owner: self.clone() }); }
        self.evict_locked(&mut state, expected_bytes)?;
        // Reservation precedes allocation/loader execution. Dropping it rolls
        // back admission on cancellation or read failure.
        let reservation = self.pool.reserve(expected_bytes, query)?;
        query.check()?;
        let loaded = load()?;
        query.check()?;
        if loaded.len() != expected_bytes { return Err(Error::Corrupt("native block loader size differs from admitted size".into())); }
        let payload = Arc::new(Payload { bytes: loaded.into(), _reservation: reservation });
        match origin { LoadOrigin::PhysicalRead => { state.io.physical_reads += 1; state.io.physical_bytes = state.io.physical_bytes.saturating_add(expected_bytes as u64); }, LoadOrigin::Memory => state.io.cache_loads += 1 }
        state.entries.insert(id, Entry { payload: payload.clone(), pins: 1, dirty: false, tick });
        Ok(Pin { id, payload, owner: self.clone() })
    }
    /// A successful writeback receipt is the only transition that cleans dirty
    /// data. Callback failure leaves the cache entry dirty and non-evictable.
    pub fn publish_dirty(&self, id: u64, publish: impl FnOnce(&[u8]) -> Result<()>, query: &QueryContext) -> Result<()> {
        query.check()?;
        let mut state = self.state.lock().map_err(|_| Error::Internal("buffer lock poisoned".into()))?;
        let entry = state.entries.get_mut(&id).ok_or_else(|| Error::Catalog("buffer block missing".into()))?;
        if !entry.dirty { return Ok(()); }
        publish(&entry.payload.bytes)?;
        query.check()?;
        entry.dirty = false;
        Ok(())
    }
    pub fn mark_dirty(&self, id: u64) -> Result<()> { let mut s=self.state.lock().map_err(|_| Error::Internal("buffer lock poisoned".into()))?; s.entries.get_mut(&id).ok_or_else(|| Error::Catalog("buffer block missing".into()))?.dirty=true; Ok(()) }
    pub fn io(&self) -> Result<IoAttribution> { Ok(self.state.lock().map_err(|_| Error::Internal("buffer lock poisoned".into()))?.io) }
    fn evict_locked(&self, state: &mut State, incoming: usize) -> Result<()> { while resident(state).saturating_add(incoming) > self.limit { let id=state.entries.iter().filter(|(_,e)|e.pins==0&&!e.dirty).min_by_key(|(_,e)|e.tick).map(|(id,_)|*id).ok_or_else(|| Error::Resource("buffer limit held by pinned or dirty blocks".into()))?; state.entries.remove(&id); } Ok(()) }
    fn unpin(&self,id:u64) { if let Ok(mut s)=self.state.lock() { if let Some(e)=s.entries.get_mut(&id) { e.pins=e.pins.saturating_sub(1); } } }
}
fn resident(state:&State)->usize { state.entries.values().map(|entry|entry.payload.bytes.len()).sum() }
impl Pin { pub fn bytes(&self)->&[u8] { &self.payload.bytes } }
impl Drop for Pin { fn drop(&mut self){ self.owner.unpin(self.id) } }

#[cfg(test)]
mod tests {
 use super::*;
 fn pool() -> Arc<MemoryPool> { Arc::new(MemoryPool::default()) }
 fn query() -> QueryContext { QueryContext::background() }
 #[test] fn pin_retains_payload_and_hit_has_no_physical_read() { let b=BlockBuffer::new(pool(),8); let p=b.pin_or_load(1,4,LoadOrigin::PhysicalRead,||Ok(vec![1;4]),&query()).unwrap(); let q=b.pin_or_load(1,4,LoadOrigin::PhysicalRead,||panic!(),&query()).unwrap(); assert_eq!(p.bytes(),q.bytes()); assert_eq!(b.io().unwrap().physical_reads,1); }
 #[test] fn loader_failure_rolls_back_admission() { let p=pool(); let b=BlockBuffer::new(p.clone(),8); assert!(b.pin_or_load(1,4,LoadOrigin::PhysicalRead,||Err(Error::Interrupted),&query()).is_err()); assert_eq!(p.used().unwrap(),0); }
 #[test] fn dirty_needs_successful_publish() { let b=BlockBuffer::new(pool(),4); let p=b.pin_or_load(1,4,LoadOrigin::Memory,||Ok(vec![1;4]),&query()).unwrap(); b.mark_dirty(1).unwrap(); assert!(b.publish_dirty(1, |_| Err(Error::Interrupted), &query()).is_err()); drop(p); assert!(b.pin_or_load(2,4,LoadOrigin::Memory,||Ok(vec![2;4]),&query()).is_err()); b.publish_dirty(1, |_| Ok(()), &query()).unwrap(); assert!(b.pin_or_load(2,4,LoadOrigin::Memory,||Ok(vec![2;4]),&query()).is_ok()); }
}
