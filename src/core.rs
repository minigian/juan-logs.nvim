use memchr::{memchr2};
use memmap2::Mmap;
use rayon::prelude::*;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::{Arc, RwLock, atomic::{AtomicBool, AtomicUsize, AtomicIsize, Ordering}};
use std::thread;
use std::time::Instant;

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

use crate::models::{ChunkMeta, IndexState, Piece};

// If you read this, I'm so so sorry xD.

// Processes 32 bytes per cycle. If you run this on a toaster, it will crash.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn count_newlines_avx2(start: *const u8, len: usize) -> usize {
    let mut count = 0;
    let mut i = 0;
    let newline_mask = _mm256_set1_epi8(b'\n' as i8);

    while i + 32 <= len {
        let chunk = _mm256_loadu_si256(start.add(i) as *const __m256i);
        let cmp = _mm256_cmpeq_epi8(chunk, newline_mask);
        let mask = _mm256_movemask_epi8(cmp);
        // Hardware bit counting. 1 cycle. Beautiful.
        count += mask.count_ones() as usize;
        i += 32;
    }

    // Fallback for the tail bytes
    if i < len {
        count += count_newlines_swar(std::slice::from_raw_parts(start.add(i), len - i));
    }
    count
}

#[inline(always)]
fn count_newlines_swar(chunk: &[u8]) -> usize {
    // evil bit-level hacking. Carmack would be proud.
    let (prefix, aligned_u64, suffix) = unsafe { chunk.align_to::<u64>() };
    let mut count = prefix.iter().filter(|&&b| b == b'\n').count();

    for &block in aligned_u64 {
        let xor_block = block ^ 0x0A0A0A0A0A0A0A0A;
        // what the fuck?
        let zero_bytes = (xor_block.wrapping_sub(0x0101010101010101)) 
                         & !xor_block 
                         & 0x8080808080808080;
        count += zero_bytes.count_ones() as usize;
    }

    count += suffix.iter().filter(|&&b| b == b'\n').count();
    count
}

// The router. Checks if we can use the illegal instructions or if we have to play nice.
// Made public so the C API can use it for blazing fast synchronous searches.
#[inline(always)]
pub fn count_newlines(bytes: &[u8]) -> usize {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { count_newlines_avx2(bytes.as_ptr(), bytes.len()) };
        }
    }
    count_newlines_swar(bytes)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn detect_monster_line_avx2(chunk: &[u8]) -> bool {
    let mut i = 0;
    let newline_mask = _mm256_set1_epi8(b'\n' as i8);
    let mut consecutive_no_newline = 0;

    while i + 32 <= chunk.len() {
        let data = _mm256_loadu_si256(chunk.as_ptr().add(i) as *const __m256i);
        let cmp = _mm256_cmpeq_epi8(data, newline_mask);
        let mask = _mm256_movemask_epi8(cmp);
        
        if mask == 0 {
            consecutive_no_newline += 32;
            if consecutive_no_newline >= 10000 {
                return true;
            }
        } else {
            consecutive_no_newline = 0;
        }
        i += 32;
    }
    false
}

#[inline(always)]
fn detect_monster_line_swar(chunk: &[u8]) -> bool {
    let (prefix, aligned_u64, suffix) = unsafe { chunk.align_to::<u64>() };
    let mut consecutive_no_newline = 0;

    for &b in prefix {
        if b == b'\n' { consecutive_no_newline = 0; } else { consecutive_no_newline += 1; }
    }

    for &block in aligned_u64 {
        let xor_block = block ^ 0x0A0A0A0A0A0A0A0A;
        let zero_bytes = (xor_block.wrapping_sub(0x0101010101010101)) 
                         & !xor_block 
                         & 0x8080808080808080;
        if zero_bytes == 0 {
            consecutive_no_newline += 8;
            if consecutive_no_newline >= 10000 {
                return true;
            }
        } else {
            consecutive_no_newline = 0;
        }
    }

    for &b in suffix {
        if b == b'\n' { consecutive_no_newline = 0; } else { consecutive_no_newline += 1; }
        if consecutive_no_newline >= 10000 {
            return true;
        }
    }
    false
}

#[inline(always)]
pub fn is_monster_line(chunk: &[u8]) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { detect_monster_line_avx2(chunk) };
        }
    }
    detect_monster_line_swar(chunk)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn find_safe_cut_avx2(mmap: &[u8], base: usize) -> usize {
    let mut offset = base;
    let limit = base.saturating_sub(128);
    
    let comma = _mm256_set1_epi8(b',' as i8);
    let space = _mm256_set1_epi8(b' ' as i8);
    let brace = _mm256_set1_epi8(b'}' as i8);
    let bracket = _mm256_set1_epi8(b']' as i8);

    while offset >= 32 && offset > limit {
        offset -= 32;
        let chunk = _mm256_loadu_si256(mmap.as_ptr().add(offset) as *const __m256i);
        
        let eq_comma = _mm256_cmpeq_epi8(chunk, comma);
        let eq_space = _mm256_cmpeq_epi8(chunk, space);
        let eq_brace = _mm256_cmpeq_epi8(chunk, brace);
        let eq_bracket = _mm256_cmpeq_epi8(chunk, bracket);
        
        let m1 = _mm256_or_si256(eq_comma, eq_space);
        let m2 = _mm256_or_si256(eq_brace, eq_bracket);
        let m_all = _mm256_or_si256(m1, m2);
        
        let mask = _mm256_movemask_epi8(m_all) as u32;
        if mask != 0 {
            let idx = 31 - mask.leading_zeros();
            return offset + idx as usize + 1;
        }
    }
    
    find_safe_cut_swar(mmap, base)
}

#[inline(always)]
fn find_safe_cut_swar(mmap: &[u8], base: usize) -> usize {
    let limit = base.saturating_sub(128);
    let mut i = base;
    // wanna see a magic trick??
    while i >= 8 && i > limit {
        i -= 8;
        let chunk = unsafe { std::ptr::read_unaligned(mmap.as_ptr().add(i) as *const u64) };
        
        let xor_comma = chunk ^ 0x2C2C2C2C2C2C2C2C;
        let xor_space = chunk ^ 0x2020202020202020;
        let xor_brace = chunk ^ 0x7D7D7D7D7D7D7D7D;
        let xor_bracket = chunk ^ 0x5D5D5D5D5D5D5D5D;
        
        let z_comma = (xor_comma.wrapping_sub(0x0101010101010101)) & !xor_comma & 0x8080808080808080;
        let z_space = (xor_space.wrapping_sub(0x0101010101010101)) & !xor_space & 0x8080808080808080;
        let z_brace = (xor_brace.wrapping_sub(0x0101010101010101)) & !xor_brace & 0x8080808080808080;
        let z_bracket = (xor_bracket.wrapping_sub(0x0101010101010101)) & !xor_bracket & 0x8080808080808080;
        
        let mask = z_comma | z_space | z_brace | z_bracket;
        if mask != 0 {
            let idx = 63 - mask.leading_zeros();
            return i + (idx as usize / 8) + 1;
        }
    }
    
    while i > limit && i > 0 {
        i -= 1;
        let b = mmap[i];
        if b == b',' || b == b' ' || b == b'}' || b == b']' {
            return i + 1;
        }
    }
    base
}

#[inline(always)]
pub fn find_safe_cut(mmap: &[u8], base: usize) -> usize {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { find_safe_cut_avx2(mmap, base) };
        }
    }
    find_safe_cut_swar(mmap, base)
}

pub struct LogEngine {
    pub mmap: Arc<Mmap>, // Arc so the background thread doesn't get rug-pulled
    pub index: Arc<RwLock<IndexState>>,
    pub cancel_token: Arc<AtomicBool>, // the cyanide pill for the background thread
    pub bytes_processed: Arc<AtomicUsize>,
    pub pieces: Vec<Piece>,
    pub indexed_lines_added: usize, // Tracks how many original lines we've synced
    pub memory_buffer: Vec<String>,
    // atomic flags because users have no patience.
    pub is_saving: Arc<AtomicBool>,
    pub save_progress: Arc<AtomicUsize>,
    pub save_total: Arc<AtomicUsize>,
    pub wasted_memory_lines: usize,
    pub is_searching: Arc<AtomicBool>,
    pub search_cancel: Arc<AtomicBool>,
    pub search_result: Arc<AtomicIsize>,
    pub is_fixed_width: bool,
    pub fixed_width_size: usize,
}

impl LogEngine {
    pub fn new(path: &Path, lazy: bool) -> Result<Self, std::io::Error> {
        let file = File::open(path)?;
        let mmap = unsafe { memmap2::MmapOptions::new().map(&file)? };

        #[cfg(unix)]
        unsafe {
            // give the OS a heads up. sequential for parsing now, random for actual usage later.
            libc::madvise(
                mmap.as_ptr() as *mut libc::c_void,
                mmap.len(),
                libc::MADV_SEQUENTIAL,
            );
        }

        let mmap = Arc::new(mmap);
        let cancel_token = Arc::new(AtomicBool::new(false));
        let bytes_processed = Arc::new(AtomicUsize::new(0));
        let index = Arc::new(RwLock::new(IndexState {
            chunks: Vec::new(),
            original_total_lines: 0,
            is_finished: false,
            indexing_time_ms: 0,
        }));

        let sample_size = mmap.len().min(64 * 1024);
        let is_fixed_width = is_monster_line(&mmap[..sample_size]);
        let fixed_width_size = 4096;

        let mut engine = LogEngine {
            mmap: mmap.clone(),
            index: index.clone(),
            cancel_token: cancel_token.clone(),
            bytes_processed: bytes_processed.clone(),
            pieces: vec![Piece::Original {
                start_line: 0,
                line_count: 0, // will be updated dynamically while indexing
            }],
            indexed_lines_added: 0,
            memory_buffer: Vec::new(),
            is_saving: Arc::new(AtomicBool::new(false)),
            save_progress: Arc::new(AtomicUsize::new(0)),
            save_total: Arc::new(AtomicUsize::new(0)),
            wasted_memory_lines: 0,
            is_searching: Arc::new(AtomicBool::new(false)),
            search_cancel: Arc::new(AtomicBool::new(false)),
            search_result: Arc::new(AtomicIsize::new(-1)),
            is_fixed_width,
            fixed_width_size,
        };

        let start_time = Instant::now();

        if is_fixed_width {
            let total_lines = (mmap.len() + fixed_width_size - 1) / fixed_width_size;
            engine.pieces[0] = Piece::Original {
                start_line: 0,
                line_count: total_lines,
            };
            engine.indexed_lines_added = total_lines;
            
            let mut idx = index.write().unwrap();
            idx.original_total_lines = total_lines;
            idx.indexing_time_ms = start_time.elapsed().as_millis();
            idx.is_finished = true;
            bytes_processed.store(mmap.len(), Ordering::Relaxed);
        } else if lazy {
            // spawn the background worker and return immediately. godspeed.
            let mmap_bg = mmap.clone();
            let index_bg = index.clone();
            let cancel_bg = cancel_token.clone();
            let bytes_bg = bytes_processed.clone();
            
            thread::spawn(move || {
                Self::build_index_sequential(mmap_bg, index_bg, cancel_bg, bytes_bg, start_time);
            });
        } else {
            // block the world. original rayon implementation.
            Self::build_index_rayon(&mmap, &index, start_time);
            bytes_processed.store(mmap.len(), Ordering::Relaxed);
            engine.sync_pieces(); // lock in the final line count
        }

        Ok(engine)
    }

    pub fn build_index_sequential(
        mmap: Arc<Mmap>,
        index: Arc<RwLock<IndexState>>,
        cancel: Arc<AtomicBool>,
        bytes_processed: Arc<AtomicUsize>,
        start_time: Instant,
    ) {
        let chunk_size = 1024 * 1024 * 5; // 5MB chunks for the background worker
        let mut current_line = 0;
        let mut local_chunks = Vec::new();
        let mut offset = 0;

        while offset < mmap.len() {
            // check if the user got bored and closed the file
            if cancel.load(Ordering::Relaxed) {
                return; 
            }

            let end = (offset + chunk_size).min(mmap.len());

            #[cfg(unix)]
            if end < mmap.len() {
                let next_end = (end + chunk_size).min(mmap.len());
                unsafe {
                    libc::madvise(
                        mmap.as_ptr().add(end) as *mut libc::c_void,
                        next_end - end,
                        libc::MADV_WILLNEED,
                    );
                }
            }

            let chunk = &mmap[offset..end];

            // SIMD goes brrrrr
            let count = count_newlines(chunk);

            local_chunks.push(ChunkMeta {
                byte_offset: offset,
                start_line: current_line,
            });

            current_line += count;
            offset = end;

            bytes_processed.store(offset, Ordering::Relaxed);

            // drain local chunks into the shared state. no more OOM clones.
            let mut idx = index.write().unwrap();
            idx.chunks.extend(local_chunks.drain(..));
            idx.original_total_lines = current_line;
        }

        let mut final_lines = current_line;
        if !mmap.is_empty() {
            let last_byte = mmap.last().copied();
            if last_byte != Some(b'\n') && last_byte != Some(b'\r') {
                final_lines += 1;
            }
            if final_lines == 0 {
                final_lines = 1;
            }
        }

        let mut idx = index.write().unwrap();
        idx.chunks.extend(local_chunks); // flush whatever is left
        idx.original_total_lines = final_lines;
        idx.indexing_time_ms = start_time.elapsed().as_millis();
        idx.is_finished = true;
        bytes_processed.store(mmap.len(), Ordering::Relaxed);

        #[cfg(unix)]
        unsafe {
            libc::madvise(
                mmap.as_ptr() as *mut libc::c_void,
                mmap.len(),
                libc::MADV_RANDOM,
            );
        }
    }

    pub fn build_index_rayon(mmap: &Mmap, index: &RwLock<IndexState>, start_time: Instant) {
        // blast through the file in 1MB chunks to count lines.
        let chunk_size = 1024 * 1024;
        let line_counts: Vec<usize> = mmap
            .par_chunks(chunk_size)
            .map(|chunk| count_newlines(chunk))
            .collect();

        let mut chunks = Vec::with_capacity(line_counts.len());
        let mut current_line = 0;

        for (i, &count) in line_counts.iter().enumerate() {
            let byte_offset = i * chunk_size;
            // \r\n boundary checks removed. SWAR/AVX2 doesn't care about your carriage returns.
            chunks.push(ChunkMeta {
                byte_offset,
                start_line: current_line,
            });
            current_line += count;
        }

        let mut original_total_lines = current_line;
        if !mmap.is_empty() {
            // handle files without a trailing newline
            let last_byte = mmap.last().copied();
            if last_byte != Some(b'\n') && last_byte != Some(b'\r') {
                original_total_lines += 1;
            }
            if original_total_lines == 0 {
                original_total_lines = 1;
            }
        }

        let mut idx = index.write().unwrap();
        idx.chunks = chunks;
        idx.original_total_lines = original_total_lines;
        idx.indexing_time_ms = start_time.elapsed().as_millis();
        idx.is_finished = true;

        #[cfg(unix)]
        unsafe {
            libc::madvise(
                mmap.as_ptr() as *mut libc::c_void,
                mmap.len(),
                libc::MADV_RANDOM,
            );
        }
    }

    // keeps the piece table in sync with the background worker
    pub fn sync_pieces(&mut self) {
        let idx = self.index.read().unwrap();
        let current_original = idx.original_total_lines;
        
        if current_original > self.indexed_lines_added {
            let diff = current_original - self.indexed_lines_added;
            
            let mut extended = false;
            if let Some(Piece::Original { start_line, line_count }) = self.pieces.last_mut() {
                if *start_line + *line_count == self.indexed_lines_added {
                    *line_count += diff;
                    extended = true;
                }
            }
            
            if !extended {
                self.pieces.push(Piece::Original {
                    start_line: self.indexed_lines_added,
                    line_count: diff,
                });
            }
            
            self.indexed_lines_added = current_original;
        }
    }

    // detached offset calculator. borrow checker appeasement.
    pub fn calc_offset(index: &RwLock<IndexState>, bytes_processed: &AtomicUsize, mmap: &Mmap, line: usize, is_fixed_width: bool, fixed_width_size: usize) -> usize {
        if is_fixed_width {
            let base = (line * fixed_width_size).min(mmap.len());
            if base == 0 || base == mmap.len() { return base; }
            return find_safe_cut(mmap, base);
        }

        let idx = index.read().unwrap();
        if line >= idx.original_total_lines {
            return if idx.is_finished { mmap.len() } else { bytes_processed.load(Ordering::Relaxed) };
        }
        
        let chunks = &idx.chunks;
        
        // Branchless binary search. 
        // The branch predictor is a lie. Math is absolute. Do not touch.
        let mut base = 0;
        let mut len = chunks.len();

        while len > 1 {
            let half = len / 2;
            let mid = base + half;
            
            // Evaluates to 0 or 1. CPU executes this straight through.
            let cmp = (chunks[mid].start_line <= line) as usize;
            base += cmp * half;
            len -= half;
        }
        
        let chunk_idx = base + (base + 1 < chunks.len() && chunks[base + 1].start_line <= line) as usize;
        
        let chunk = &chunks[chunk_idx];
        let mut offset = chunk.byte_offset;
        let mut skip = line - chunk.start_line;
        
        while skip > 0 && offset < mmap.len() {
            let slice = &mmap[offset..];
            if let Some(pos) = memchr2(b'\n', b'\r', slice) {
                offset += pos + 1;
                if slice[pos] == b'\r' && offset < mmap.len() && mmap[offset] == b'\n' {
                    offset += 1; 
                }
                skip -= 1;
            } else {
                offset = mmap.len();
                break;
            }
        }
        offset
    }

    pub fn line_to_byte_offset(&self, line: usize) -> usize {
        Self::calc_offset(&self.index, &self.bytes_processed, &self.mmap, line, self.is_fixed_width, self.fixed_width_size)
    }

    pub fn get_original_bytes(&self, start_line: usize, line_count: usize) -> &[u8] {
        if line_count == 0 {
            return &[];
        }
        let start = self.line_to_byte_offset(start_line);
        let end = self.line_to_byte_offset(start_line + line_count);
        
        // safety net in case the background thread hasn't reached `end` yet
        if start >= self.mmap.len() { return &[]; }
        let end = end.min(self.mmap.len());
        
        &self.mmap[start..end]
    }

    pub fn total_lines(&self) -> usize {
        self.pieces.iter().map(|p| p.line_count()).sum()
    }

    // returns (piece_index, line_offset_inside_piece)
    pub fn find_piece_idx(&self, logical_line: usize) -> (usize, usize) {
        let mut current = 0;
        for (i, piece) in self.pieces.iter().enumerate() {
            let count = piece.line_count();
            if logical_line < current + count {
                return (i, logical_line - current);
            }
            current += count;
        }
        (self.pieces.len(), 0)
    }

    pub fn split_piece_at(&mut self, piece_idx: usize, offset: usize) {
        self.sync_pieces();
        if offset == 0 || piece_idx >= self.pieces.len() {
            return;
        }
        let piece = self.pieces[piece_idx].clone();
        if offset >= piece.line_count() {
            return;
        }

        match piece {
            Piece::Original { start_line, line_count } => {
                self.pieces[piece_idx] = Piece::Original { start_line, line_count: offset };
                self.pieces.insert(piece_idx + 1, Piece::Original {
                    start_line: start_line + offset,
                    line_count: line_count - offset,
                });
            }
            Piece::Memory { start_idx, line_count } => {
                self.pieces[piece_idx] = Piece::Memory { start_idx, line_count: offset };
                self.pieces.insert(piece_idx + 1, Piece::Memory {
                    start_idx: start_idx + offset,
                    line_count: line_count - offset,
                });
            }
        }
    }

    pub fn apply_edit(&mut self, start_line: usize, num_deleted: usize, new_text: &str) {
        self.sync_pieces();
        let (mut piece_idx, offset) = self.find_piece_idx(start_line);

        if piece_idx < self.pieces.len() {
            self.split_piece_at(piece_idx, offset);
            if offset > 0 {
                piece_idx += 1;
            }
        }

        let mut remaining_delete = num_deleted;
        
        // nuke pieces fully contained in the deletion range
        while remaining_delete > 0 && piece_idx < self.pieces.len() {
            let count = self.pieces[piece_idx].line_count();
            if count <= remaining_delete {
                if let Piece::Memory { .. } = self.pieces[piece_idx] {
                    self.wasted_memory_lines += count;
                }
                self.pieces.remove(piece_idx);
                remaining_delete -= count;
            } else {
                // partial overlap, split and drop the front
                if let Piece::Memory { .. } = self.pieces[piece_idx] {
                    self.wasted_memory_lines += remaining_delete;
                }
                self.split_piece_at(piece_idx, remaining_delete);
                self.pieces.remove(piece_idx);
                remaining_delete = 0;
            }
        }

        if !new_text.is_empty() {
            let mut lines: Vec<String> = new_text.split('\n').map(|s| s.to_string()).collect();
            // drop the trailing empty string from split if it exists
            if lines.last().map(|s| s.is_empty()).unwrap_or(false) {
                lines.pop();
            }
            if !lines.is_empty() {
                let start_idx = self.memory_buffer.len();
                let line_count = lines.len();
                self.memory_buffer.extend(lines);
                self.pieces.insert(piece_idx, Piece::Memory { start_idx, line_count });
            }
        }

        if self.wasted_memory_lines > 10000 {
            self.compact_memory();
        }
    }

    pub fn compact_memory(&mut self) {
        let mut new_memory_buffer = Vec::new();
        for piece in &mut self.pieces {
            if let Piece::Memory { start_idx, line_count } = piece {
                let new_start = new_memory_buffer.len();
                for i in 0..*line_count {
                    new_memory_buffer.push(self.memory_buffer[*start_idx + i].clone());
                }
                *start_idx = new_start;
            }
        }
        self.memory_buffer = new_memory_buffer;
        self.wasted_memory_lines = 0;
    }

    pub fn get_block(&mut self, start_line: usize, num_lines: usize) -> String {
        self.sync_pieces();
        let mut block = String::new();
        if num_lines == 0 || start_line >= self.total_lines() {
            return block;
        }

        let (mut piece_idx, mut offset) = self.find_piece_idx(start_line);
        let mut collected = 0;

        // stitch together pieces until we satisfy the requested line count
        while collected < num_lines && piece_idx < self.pieces.len() {
            let piece = &self.pieces[piece_idx];
            
            // Speculative prefetching. 
            // Forcing the L1 cache to eat the next 100 lines before the user even knows they want them.
            // If they scroll up instead, we just polluted the cache. Oh well.
            #[cfg(target_arch = "x86_64")]
            if let Piece::Original { start_line: p_start, .. } = piece {
                let speculative_offset = self.line_to_byte_offset(p_start + offset + num_lines + 100);
                if speculative_offset < self.mmap.len() {
                    unsafe {
                        _mm_prefetch(
                            self.mmap.as_ptr().add(speculative_offset) as *const i8, 
                            _MM_HINT_T0
                        );
                    }
                }
            }

            let count = piece.line_count() - offset;
            let take = count.min(num_lines - collected);

            match piece {
                Piece::Original { start_line: p_start, .. } => {
                    let start_byte = self.line_to_byte_offset(p_start + offset);
                    let end_byte = self.line_to_byte_offset(p_start + offset + take);
                    
                    if self.is_fixed_width {
                        let mut current = start_byte;
                        for i in 0..take {
                            let logical_line = p_start + offset + i + 1;
                            let next = self.line_to_byte_offset(logical_line);
                            if current < next {
                                let bytes = &self.mmap[current..next];
                                let s = String::from_utf8_lossy(bytes);
                                block.push_str(&s);
                            }
                            block.push('\n');
                            current = next;
                        }
                    } else {
                        let bytes = &self.mmap[start_byte..end_byte];
                        
                        // logs are dirty. replace garbage bytes with  instead of failing silently.
                        let s = String::from_utf8_lossy(bytes);
                        block.push_str(&s);
                        if !block.ends_with('\n') && !block.is_empty() {
                            block.push('\n');
                        }
                    }
                }
                Piece::Memory { start_idx, .. } => {
                    for i in 0..take {
                        block.push_str(&self.memory_buffer[start_idx + offset + i]);
                        block.push('\n');
                    }
                }
            }
            collected += take;
            offset = 0;
            piece_idx += 1;
        }

        // C side expects a pointer. this gets overwritten next call, DO NOT keep it around.
        block
    }

    // the magic trick for 'G'. reads backwards from the abyss without needing an index.
    pub fn get_eof_block(&mut self, num_lines: usize) -> String {
        let mut block = String::new();
        if self.mmap.is_empty() || num_lines == 0 {
            return block;
        }

        if self.is_fixed_width {
            let start_line = self.total_lines().saturating_sub(num_lines);
            let start_byte = self.line_to_byte_offset(start_line);
            
            let mut current = start_byte;
            for i in 0..num_lines {
                let logical_line = start_line + i + 1;
                let next = self.line_to_byte_offset(logical_line);
                if current < next {
                    let bytes = &self.mmap[current..next];
                    let s = String::from_utf8_lossy(bytes);
                    block.push_str(&s);
                }
                block.push('\n');
                current = next;
            }
            return block;
        }

        let mut newlines_found = 0;
        let mut start_byte = 0;
        
        for i in (0..self.mmap.len()).rev() {
            if self.mmap[i] == b'\n' {
                newlines_found += 1;
                // +1 because the very last byte might be a newline, which doesn't count as a new line block
                if newlines_found == num_lines + 1 {
                    start_byte = i + 1;
                    break;
                }
            }
        }

        let bytes = &self.mmap[start_byte..];
        let s = String::from_utf8_lossy(bytes);
        block.push_str(&s);
        if !block.ends_with('\n') && !block.is_empty() {
            block.push('\n');
        }

        block
    }

    pub fn save(&self, path: &Path) -> bool {
        let temp_path = format!("{}.tmp", path.to_string_lossy());
        let file = match OpenOptions::new().write(true).create(true).truncate(true).open(&temp_path) {
            Ok(f) => f,
            Err(_) => return false,
        };
        let mut writer = BufWriter::new(file);

        for piece in &self.pieces {
            match piece {
                Piece::Original { start_line, line_count } => {
                    if self.is_fixed_width {
                        for i in 0..*line_count {
                            let l_start = self.line_to_byte_offset(start_line + i);
                            let l_end = self.line_to_byte_offset(start_line + i + 1);
                            if l_start < l_end {
                                if writer.write_all(&self.mmap[l_start..l_end]).is_err() {
                                    return false;
                                }
                            }
                        }
                    } else {
                        let bytes = self.get_original_bytes(*start_line, *line_count);
                        if writer.write_all(bytes).is_err() {
                            return false;
                        }
                        if !bytes.ends_with(b"\n") && !bytes.is_empty() {
                            if writer.write_all(b"\n").is_err() {
                                return false;
                            }
                        }
                    }
                }
                Piece::Memory { start_idx, line_count } => {
                    for i in 0..*line_count {
                        if writer.write_all(self.memory_buffer[start_idx + i].as_bytes()).is_err() {
                            return false;
                        }
                        if !self.is_fixed_width {
                            if writer.write_all(b"\n").is_err() {
                                return false;
                            }
                        }
                    }
                }
            }
        }

        if writer.flush().is_err() {
            return false;
        }
        // atomic swap
        std::fs::rename(&temp_path, path).is_ok()
    }

    // clone the world and run away to a background thread.
    pub fn save_async(&self, path: &Path) -> bool {
        if self.is_saving.swap(true, Ordering::SeqCst) {
            return false; 
        }

        let path_buf = path.to_path_buf();
        let pieces = self.pieces.clone();
        let memory_buffer = self.memory_buffer.clone();
        let mmap = self.mmap.clone();
        let index = self.index.clone();
        let bytes_processed = self.bytes_processed.clone();
        
        let is_saving = self.is_saving.clone();
        let save_progress = self.save_progress.clone();
        let save_total = self.save_total.clone();
        let is_fixed_width = self.is_fixed_width;
        let fixed_width_size = self.fixed_width_size;

        let mut total_bytes = 0;
        for p in &pieces {
            match p {
                Piece::Original { start_line, line_count } => {
                    let start = Self::calc_offset(&index, &bytes_processed, &mmap, *start_line, is_fixed_width, fixed_width_size);
                    let end = Self::calc_offset(&index, &bytes_processed, &mmap, start_line + line_count, is_fixed_width, fixed_width_size);
                    total_bytes += end.saturating_sub(start);
                }
                Piece::Memory { start_idx, line_count } => {
                    for i in 0..*line_count {
                        total_bytes += memory_buffer[*start_idx + i].len();
                        if !is_fixed_width {
                            total_bytes += 1;
                        }
                    }
                }
            }
        }
        save_total.store(total_bytes, Ordering::Relaxed);
        save_progress.store(0, Ordering::Relaxed);

        thread::spawn(move || {
            let temp_path = format!("{}.tmp", path_buf.to_string_lossy());
            let file = match OpenOptions::new().write(true).create(true).truncate(true).open(&temp_path) {
                Ok(f) => f,
                Err(_) => {
                    is_saving.store(false, Ordering::SeqCst);
                    return;
                }
            };
            let mut writer = BufWriter::new(file);
            let mut current_progress = 0;

            for piece in &pieces {
                match piece {
                    Piece::Original { start_line, line_count } => {
                        if is_fixed_width {
                            for i in 0..*line_count {
                                let logical_line = start_line + i;
                                let l_start = Self::calc_offset(&index, &bytes_processed, &mmap, logical_line, is_fixed_width, fixed_width_size);
                                let l_end = Self::calc_offset(&index, &bytes_processed, &mmap, logical_line + 1, is_fixed_width, fixed_width_size);
                                if l_start < l_end {
                                    let chunk = &mmap[l_start..l_end];
                                    if writer.write_all(chunk).is_err() {
                                        let _ = std::fs::remove_file(&temp_path);
                                        break;
                                    }
                                    current_progress += chunk.len();
                                }
                                save_progress.store(current_progress, Ordering::Relaxed);
                            }
                        } else {
                            let start = Self::calc_offset(&index, &bytes_processed, &mmap, *start_line, is_fixed_width, fixed_width_size);
                            let end = Self::calc_offset(&index, &bytes_processed, &mmap, start_line + line_count, is_fixed_width, fixed_width_size);
                            let end = end.min(mmap.len());
                            
                            if start < end {
                                let mut current_offset = start;
                                // chunking this so the UI doesn't look dead. 2MB at a time.
                                let chunk_size = 1024 * 1024 * 2; 
                                
                                while current_offset < end {
                                    let next_offset = (current_offset + chunk_size).min(end);
                                    let chunk = &mmap[current_offset..next_offset];
                                    
                                    if writer.write_all(chunk).is_err() {
                                        let _ = std::fs::remove_file(&temp_path);
                                        break; // disk is probably full. rip.
                                    }
                                    
                                    current_progress += chunk.len();
                                    save_progress.store(current_progress, Ordering::Relaxed);
                                    current_offset = next_offset;
                                }
                                
                                if mmap[end - 1] != b'\n' {
                                    let _ = writer.write_all(b"\n");
                                }
                            }
                        }
                    }
                    Piece::Memory { start_idx, line_count } => {
                        for i in 0..*line_count {
                            let line_bytes = memory_buffer[*start_idx + i].as_bytes();
                            if writer.write_all(line_bytes).is_err() {
                                let _ = std::fs::remove_file(&temp_path);
                                break;
                            }
                            current_progress += line_bytes.len();
                            
                            if !is_fixed_width {
                                if writer.write_all(b"\n").is_err() {
                                    let _ = std::fs::remove_file(&temp_path);
                                    break;
                                }
                                current_progress += 1;
                            }
                            save_progress.store(current_progress, Ordering::Relaxed);
                        }
                    }
                }
            }

            if writer.flush().is_ok() {
                let _ = std::fs::rename(&temp_path, path_buf);
            }
            is_saving.store(false, Ordering::SeqCst);
        });

        true
    }

    // Lock-free asynchronous black magic.
    pub fn search_async(&mut self, query: &str, start_line: usize) {
        if self.is_searching.swap(true, Ordering::SeqCst) {
            return;
        }
        self.search_cancel.store(false, Ordering::SeqCst);
        self.search_result.store(-1, Ordering::SeqCst);

        self.sync_pieces();

        let query_bytes = query.as_bytes().to_vec();
        if query_bytes.is_empty() {
            self.is_searching.store(false, Ordering::SeqCst);
            return;
        }

        let pieces = self.pieces.clone();
        let memory_buffer = self.memory_buffer.clone();
        let mmap = self.mmap.clone();
        let index = self.index.clone();
        let bytes_processed = self.bytes_processed.clone();
        let is_searching = self.is_searching.clone();
        let search_cancel = self.search_cancel.clone();
        let search_result = self.search_result.clone();
        let is_fixed_width = self.is_fixed_width;
        let fixed_width_size = self.fixed_width_size;

        let mut current_logical = 0;
        let mut start_piece_idx = pieces.len();
        let mut start_offset = 0;
        for (idx, piece) in pieces.iter().enumerate() {
            let count = piece.line_count();
            if start_line < current_logical + count {
                start_piece_idx = idx;
                start_offset = start_line - current_logical;
                break;
            }
            current_logical += count;
        }
        
        let mut current_line = start_line;

        thread::spawn(move || {
            for piece_idx in start_piece_idx..pieces.len() {
                if search_cancel.load(Ordering::Relaxed) {
                    break;
                }

                let piece = &pieces[piece_idx];
                let offset = if piece_idx == start_piece_idx { start_offset } else { 0 };

                let mut piece_bytes: Vec<u8> = Vec::new();
                match piece {
                    Piece::Original { start_line: p_start, line_count } => {
                        let start_byte = LogEngine::calc_offset(&index, &bytes_processed, &mmap, p_start + offset, is_fixed_width, fixed_width_size);
                        let end_byte = LogEngine::calc_offset(&index, &bytes_processed, &mmap, p_start + line_count, is_fixed_width, fixed_width_size);
                        let end_byte = end_byte.min(mmap.len());
                        if start_byte < end_byte {
                            piece_bytes.extend_from_slice(&mmap[start_byte..end_byte]);
                        }
                    }
                    Piece::Memory { start_idx, line_count } => {
                        for i in offset..*line_count {
                            piece_bytes.extend_from_slice(memory_buffer[*start_idx + i].as_bytes());
                            piece_bytes.push(b'\n');
                        }
                    }
                }

                if let Some(pos) = memchr::memmem::find(&piece_bytes, &query_bytes) {
                    let slice_to_match = &piece_bytes[..pos];
                    
                    // Nuke the iterator. We count lines in bulk now. CPU goes brrrrr.
                    let lines = if is_fixed_width {
                        pos / fixed_width_size
                    } else {
                        count_newlines(slice_to_match)
                    };
                    
                    let match_start = current_line + lines;
                    search_result.store(match_start as isize, Ordering::SeqCst);
                    is_searching.store(false, Ordering::SeqCst);
                    return;
                }

                current_line += piece.line_count() - offset;
            }

            search_result.store(-2, Ordering::SeqCst);
            is_searching.store(false, Ordering::SeqCst);
        });
    }
}
