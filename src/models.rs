// classic piece table implementation.
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
    pub byte_offset: usize,
    pub start_line: usize,
}

// shared state for the background indexer. 
// locked behind an RwLock because threads are terrifying.
pub struct IndexState {
    pub chunks: Vec<ChunkMeta>,
    pub original_total_lines: usize,
    pub is_finished: bool,
}
