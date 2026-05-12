
#[cfg(target_has_atomic = "64")]
pub type AtomicOffset = std::sync::atomic::AtomicU64;

#[cfg(not(target_has_atomic = "64"))]
pub struct AtomicOffset {
    // A channel could work, but since updates are highly infrequent (per MB chunks), an RwLock does exactly the same safely.
    val: std::sync::RwLock<u64>,
}

#[cfg(not(target_has_atomic = "64"))]
impl AtomicOffset {
    pub fn new(v: u64) -> Self { Self { val: std::sync::RwLock::new(v) } }
    pub fn load(&self, _order: Ordering) -> u64 { *self.val.read().unwrap() }
    pub fn store(&self, v: u64, _order: Ordering) { *self.val.write().unwrap() = v; }
}
// Original = points to the readonly memory mapped file.
// Memory = points to heap allocated edits.
#[derive(Clone)]
pub enum Piece {
    Original { start_line: usize, line_count: usize },
    Memory { start_idx: usize, line_count: usize },
}

impl Piece {
    pub fn line_count(&self) -> usize {
        match self {
            Piece::Original { line_count, .. } => *line_count,
            Piece::Memory { line_count, .. } => *line_count,
        }
    }
}

#[derive(Clone)]
pub struct ChunkMeta {
    pub byte_offset: u64,
    pub start_line: usize,
}

// shared state for the background indexer. 
// locked behind an RwLock because threads are terrifying.
pub struct IndexState {
    pub chunks: Vec<ChunkMeta>,
    pub original_total_lines: usize,
    pub is_finished: bool,
    pub indexing_time_ms: u128, // how long did the background thread take to eat the file?
}