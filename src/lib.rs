use memchr::{memchr2, memchr2_iter, memmem};
use memmap2::Mmap;
use rayon::prelude::*;
use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::os::raw::c_char;
use std::ptr;
use std::sync::{Arc, RwLock, atomic::{AtomicBool, AtomicUsize, Ordering}};
use std::thread;

// classic piece table implementation.
// Original = points to the readonly memory mapped file.
// Memory = points to heap allocated edits.
#[derive(Clone)]
enum Piece {
    Original { start_line: usize, line_count: usize },
    Memory { start_idx: usize, line_count: usize },
}

impl Piece {
    fn line_count(&self) -> usize {
        match self {
            Piece::Original { line_count, .. } => *line_count,
            Piece::Memory { line_count, .. } => *line_count,
        }
    }
}

#[derive(Clone)]
struct ChunkMeta {
    byte_offset: usize,
    start_line: usize,
}

// shared state for the background indexer. 
// locked behind an RwLock because threads are terrifying.
struct IndexState {
    chunks: Vec<ChunkMeta>,
    original_total_lines: usize,
    is_finished: bool,
}

pub struct LogEngine {
    mmap: Arc<Mmap>, // Arc so the background thread doesn't get rug-pulled
    index: Arc<RwLock<IndexState>>,
    cancel_token: Arc<AtomicBool>, // the cyanide pill for the background thread
    bytes_processed: Arc<AtomicUsize>,
    pieces: Vec<Piece>,
    memory_buffer: Vec<String>,
    last_block: String, // persistent buffer to hand out safe pointers to C
    // atomic flags because users have no patience.
    is_saving: Arc<AtomicBool>,
    save_progress: Arc<AtomicUsize>,
    save_total: Arc<AtomicUsize>,
}

impl LogEngine {
    fn new(path: &str, lazy: bool) -> Result<Self, std::io::Error> {
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
            libc::madvise(
                mmap.as_ptr() as *mut libc::c_void,
                mmap.len(),
                libc::MADV_RANDOM,
            );
        }

        let mmap = Arc::new(mmap);
        let cancel_token = Arc::new(AtomicBool::new(false));
        let bytes_processed = Arc::new(AtomicUsize::new(0));
        let index = Arc::new(RwLock::new(IndexState {
            chunks: Vec::new(),
            original_total_lines: 0,
            is_finished: false,
        }));

        let mut engine = LogEngine {
            mmap: mmap.clone(),
            index: index.clone(),
            cancel_token: cancel_token.clone(),
            bytes_processed: bytes_processed.clone(),
            pieces: vec![Piece::Original {
                start_line: 0,
                line_count: 0, // will be updated dynamically while indexing
            }],
            memory_buffer: Vec::new(),
            last_block: String::new(),
            is_saving: Arc::new(AtomicBool::new(false)),
            save_progress: Arc::new(AtomicUsize::new(0)),
            save_total: Arc::new(AtomicUsize::new(0)),
        };

        if lazy {
            // spawn the background worker and return immediately. godspeed.
            let mmap_bg = mmap.clone();
            let index_bg = index.clone();
            let cancel_bg = cancel_token.clone();
            let bytes_bg = bytes_processed.clone();
            
            thread::spawn(move || {
                Self::build_index_sequential(mmap_bg, index_bg, cancel_bg, bytes_bg);
            });
        } else {
            // block the world. original rayon implementation.
            Self::build_index_rayon(&mmap, &index);
            bytes_processed.store(mmap.len(), Ordering::Relaxed);
            engine.sync_pieces(); // lock in the final line count
        }

        Ok(engine)
    }

    fn build_index_sequential(
        mmap: Arc<Mmap>,
        index: Arc<RwLock<IndexState>>,
        cancel: Arc<AtomicBool>,
        bytes_processed: Arc<AtomicUsize>,
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
            let chunk = &mmap[offset..end];

            let mut count = 0;
            let mut iter = memchr2_iter(b'\n', b'\r', chunk).peekable();
            while let Some(pos) = iter.next() {
                count += 1;
                if chunk[pos] == b'\r' {
                    if let Some(&next_pos) = iter.peek() {
                        if next_pos == pos + 1 && chunk[next_pos] == b'\n' {
                            iter.next();
                        }
                    }
                }
            }

            if offset > 0 && mmap[offset - 1] == b'\r' && mmap.get(offset) == Some(&b'\n') {
                current_line -= 1;
            }

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
        idx.is_finished = true;
        bytes_processed.store(mmap.len(), Ordering::Relaxed);
    }

    fn build_index_rayon(mmap: &Mmap, index: &RwLock<IndexState>) {
        // blast through the file in 1MB chunks to count lines.
        let chunk_size = 1024 * 1024;
        let line_counts: Vec<usize> = mmap
            .par_chunks(chunk_size)
            .map(|chunk| {
                let mut count = 0;
                let mut iter = memchr2_iter(b'\n', b'\r', chunk).peekable();
                while let Some(pos) = iter.next() {
                    count += 1;
                    // the \r\n check here is slightly cursed but prevents overcounting windows line endings.
                    if chunk[pos] == b'\r' {
                        if let Some(&next_pos) = iter.peek() {
                            if next_pos == pos + 1 && chunk[next_pos] == b'\n' {
                                iter.next();
                            }
                        }
                    }
                }
                count
            })
            .collect();

        let mut chunks = Vec::with_capacity(line_counts.len());
        let mut current_line = 0;

        for (i, &count) in line_counts.iter().enumerate() {
            let byte_offset = i * chunk_size;
            // what happens if \r is at the end of chunk N and \n is at the start of chunk N+1?
            // this. this happens. adjust the line count so we don't desync.
            if i > 0 && mmap[byte_offset - 1] == b'\r' && mmap.get(byte_offset) == Some(&b'\n') {
                current_line -= 1;
            }
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
        idx.is_finished = true;
    }

    // keeps the piece table in sync with the background worker
    fn sync_pieces(&mut self) {
        let idx = self.index.read().unwrap();
        if self.pieces.len() == 1 {
            if let Piece::Original { line_count, .. } = &mut self.pieces[0] {
                *line_count = idx.original_total_lines;
            }
        }
    }

    // detached offset calculator. borrow checker appeasement.
    fn calc_offset(index: &RwLock<IndexState>, bytes_processed: &AtomicUsize, mmap: &Mmap, line: usize) -> usize {
        let idx = index.read().unwrap();
        if line >= idx.original_total_lines {
            return if idx.is_finished { mmap.len() } else { bytes_processed.load(Ordering::Relaxed) };
        }
        
        let chunk_idx = match idx.chunks.binary_search_by_key(&line, |c| c.start_line) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        
        let chunk = &idx.chunks[chunk_idx];
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

    fn line_to_byte_offset(&self, line: usize) -> usize {
        Self::calc_offset(&self.index, &self.bytes_processed, &self.mmap, line)
    }

    fn get_original_bytes(&self, start_line: usize, line_count: usize) -> &[u8] {
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

    fn total_lines(&self) -> usize {
        let idx = self.index.read().unwrap();
        if self.pieces.len() == 1 && !idx.is_finished {
            return idx.original_total_lines;
        }
        self.pieces.iter().map(|p| p.line_count()).sum()
    }

    // returns (piece_index, line_offset_inside_piece)
    fn find_piece_idx(&self, logical_line: usize) -> (usize, usize) {
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

    fn split_piece_at(&mut self, piece_idx: usize, offset: usize) {
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

    fn apply_edit(&mut self, start_line: usize, num_deleted: usize, new_text: &str) {
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
                self.pieces.remove(piece_idx);
                remaining_delete -= count;
            } else {
                // partial overlap, split and drop the front
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
    }

    fn get_block(&mut self, start_line: usize, num_lines: usize) -> *const u8 {
        self.sync_pieces();
        self.last_block.clear();
        if num_lines == 0 || start_line >= self.total_lines() {
            return ptr::null();
        }

        let (mut piece_idx, mut offset) = self.find_piece_idx(start_line);
        let mut collected = 0;

        // stitch together pieces until we satisfy the requested line count
        while collected < num_lines && piece_idx < self.pieces.len() {
            let piece = &self.pieces[piece_idx];
            let count = piece.line_count() - offset;
            let take = count.min(num_lines - collected);

            match piece {
                Piece::Original { start_line: p_start, .. } => {
                    let start_byte = self.line_to_byte_offset(p_start + offset);
                    let end_byte = self.line_to_byte_offset(p_start + offset + take);
                    
                    let bytes = &self.mmap[start_byte..end_byte];
                    
                    // logs are dirty. replace garbage bytes with  instead of failing silently.
                    let s = String::from_utf8_lossy(bytes);
                    self.last_block.push_str(&s);
                    if !self.last_block.ends_with('\n') && !self.last_block.is_empty() {
                        self.last_block.push('\n');
                    }
                }
                Piece::Memory { start_idx, .. } => {
                    for i in 0..take {
                        self.last_block.push_str(&self.memory_buffer[start_idx + offset + i]);
                        self.last_block.push('\n');
                    }
                }
            }
            collected += take;
            offset = 0;
            piece_idx += 1;
        }

        // C side expects a pointer. this gets overwritten next call, DO NOT keep it around.
        self.last_block.as_ptr()
    }

    // the magic trick for 'G'. reads backwards from the abyss without needing an index.
    fn get_eof_block(&mut self, num_lines: usize) -> *const u8 {
        self.last_block.clear();
        if self.mmap.is_empty() || num_lines == 0 {
            return ptr::null();
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
        self.last_block.push_str(&s);
        if !self.last_block.ends_with('\n') && !self.last_block.is_empty() {
            self.last_block.push('\n');
        }

        self.last_block.as_ptr()
    }

    fn save(&self, path: &str) -> bool {
        let temp_path = format!("{}.tmp", path);
        let file = match OpenOptions::new().write(true).create(true).truncate(true).open(&temp_path) {
            Ok(f) => f,
            Err(_) => return false,
        };
        let mut writer = BufWriter::new(file);

        for piece in &self.pieces {
            match piece {
                Piece::Original { start_line, line_count } => {
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
                Piece::Memory { start_idx, line_count } => {
                    for i in 0..*line_count {
                        if writer.write_all(self.memory_buffer[start_idx + i].as_bytes()).is_err() {
                            return false;
                        }
                        if writer.write_all(b"\n").is_err() {
                            return false;
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
    fn save_async(&self, path: &str) -> bool {
        if self.is_saving.swap(true, Ordering::SeqCst) {
            return false; 
        }

        let path = path.to_string();
        let pieces = self.pieces.clone();
        let memory_buffer = self.memory_buffer.clone();
        let mmap = self.mmap.clone();
        let index = self.index.clone();
        let bytes_processed = self.bytes_processed.clone();
        
        let is_saving = self.is_saving.clone();
        let save_progress = self.save_progress.clone();
        let save_total = self.save_total.clone();

        let mut total_bytes = 0;
        for p in &pieces {
            match p {
                Piece::Original { start_line, line_count } => {
                    let start = Self::calc_offset(&index, &bytes_processed, &mmap, *start_line);
                    let end = Self::calc_offset(&index, &bytes_processed, &mmap, start_line + line_count);
                    total_bytes += end.saturating_sub(start);
                }
                Piece::Memory { start_idx, line_count } => {
                    for i in 0..*line_count {
                        total_bytes += memory_buffer[*start_idx + i].len() + 1;
                    }
                }
            }
        }
        save_total.store(total_bytes, Ordering::Relaxed);
        save_progress.store(0, Ordering::Relaxed);

        thread::spawn(move || {
            let temp_path = format!("{}.tmp", path);
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
                        let start = Self::calc_offset(&index, &bytes_processed, &mmap, *start_line);
                        let end = Self::calc_offset(&index, &bytes_processed, &mmap, start_line + line_count);
                        let end = end.min(mmap.len());
                        
                        if start < end {
                            let mut current_offset = start;
                            // chunking this so the UI doesn't look dead. 2MB at a time.
                            let chunk_size = 1024 * 1024 * 2; 
                            
                            while current_offset < end {
                                let next_offset = (current_offset + chunk_size).min(end);
                                let chunk = &mmap[current_offset..next_offset];
                                
                                if writer.write_all(chunk).is_err() {
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
                    Piece::Memory { start_idx, line_count } => {
                        for i in 0..*line_count {
                            let line_bytes = memory_buffer[*start_idx + i].as_bytes();
                            if writer.write_all(line_bytes).is_ok() {
                                let _ = writer.write_all(b"\n");
                                current_progress += line_bytes.len() + 1;
                                save_progress.store(current_progress, Ordering::Relaxed);
                            }
                        }
                    }
                }
            }

            if writer.flush().is_ok() {
                let _ = std::fs::rename(&temp_path, path);
            }
            is_saving.store(false, Ordering::SeqCst);
        });

        true
    }
}

// --- C ABI Boundary ---
// Trusting the caller from here on out. standard unsafe boilerplate.

#[no_mangle]
pub extern "C" fn log_engine_new(path: *const c_char, lazy: bool) -> *mut LogEngine {
    if path.is_null() {
        return ptr::null_mut();
    }
    let c_str = unsafe { CStr::from_ptr(path) };
    // paths can be cursed too on some OSes.
    let path_str = c_str.to_string_lossy();
    if let Ok(engine) = LogEngine::new(path_str.as_ref(), lazy) {
        return Box::into_raw(Box::new(engine));
    }
    ptr::null_mut()
}

#[no_mangle]
pub extern "C" fn log_engine_get_progress(engine: *const LogEngine) -> f32 {
    let engine = unsafe {
        if engine.is_null() { return -1.0; }
        &*engine
    };
    let idx = engine.index.read().unwrap();
    if idx.is_finished {
        return 1.0;
    }
    let processed = engine.bytes_processed.load(Ordering::Relaxed) as f32;
    let total = engine.mmap.len() as f32;
    if total == 0.0 { 1.0 } else { processed / total }
}

#[no_mangle]
pub extern "C" fn log_engine_total_lines(engine: *mut LogEngine) -> usize {
    // :LogLines. fast because we already paid the price at startup.
    let engine = unsafe {
        if engine.is_null() {
            return 0;
        }
        &mut *engine
    };
    // force a sync so we don't lie to Lua about having 0 lines
    engine.sync_pieces();
    engine.total_lines()
}

#[no_mangle]
pub extern "C" fn log_engine_get_block(
    engine: *mut LogEngine,
    start_line: usize,
    num_lines: usize,
    out_len: *mut usize,
) -> *const u8 {
    // the thing behind :LogJump and scrolling. fetches chunks without loading the whole file.
    let engine = unsafe {
        if engine.is_null() {
            return ptr::null();
        }
        &mut *engine
    };
    let ptr = engine.get_block(start_line, num_lines);
    if !out_len.is_null() {
        unsafe { *out_len = engine.last_block.len() };
    }
    ptr
}

#[no_mangle]
pub extern "C" fn log_engine_get_eof_block(
    engine: *mut LogEngine,
    num_lines: usize,
    out_len: *mut usize,
) -> *const u8 {
    let engine = unsafe {
        if engine.is_null() { return ptr::null(); }
        &mut *engine
    };
    let ptr = engine.get_eof_block(num_lines);
    if !out_len.is_null() {
        unsafe { *out_len = engine.last_block.len() };
    }
    ptr
}

#[no_mangle]
pub extern "C" fn log_engine_apply_edit(
    engine: *mut LogEngine,
    start_line: usize,
    num_deleted: usize,
    new_text: *const c_char,
) {
    let engine = unsafe {
        if engine.is_null() {
            return;
        }
        &mut *engine
    };
    // nvim might send weird stuff, salvage what we can.
    let text = if new_text.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(new_text) }.to_string_lossy().into_owned()
    };
    engine.apply_edit(start_line, num_deleted, &text);
}

#[no_mangle]
pub extern "C" fn log_engine_save(engine: *const LogEngine, path: *const c_char) -> bool {
    let engine = unsafe {
        if engine.is_null() {
            return false;
        }
        &*engine
    };
    if path.is_null() {
        return false;
    }
    // paths can be cursed too.
    let path_str = unsafe { CStr::from_ptr(path) }.to_string_lossy();
    return engine.save(path_str.as_ref());
}

// FFI for the async save. don't block the UI.
#[no_mangle]
pub extern "C" fn log_engine_save_async(engine: *const LogEngine, path: *const c_char) -> bool {
    let engine = unsafe {
        if engine.is_null() { return false; }
        &*engine
    };
    if path.is_null() { return false; }
    let path_str = unsafe { CStr::from_ptr(path) }.to_string_lossy();
    engine.save_async(path_str.as_ref())
}

// returns -1.0 if not saving, otherwise 0.0 to 1.0
#[no_mangle]
pub extern "C" fn log_engine_get_save_progress(engine: *const LogEngine) -> f32 {
    let engine = unsafe {
        if engine.is_null() { return -1.0; }
        &*engine
    };
    if !engine.is_saving.load(Ordering::Relaxed) {
        return -1.0; 
    }
    let total = engine.save_total.load(Ordering::Relaxed) as f32;
    let current = engine.save_progress.load(Ordering::Relaxed) as f32;
    if total == 0.0 { 0.0 } else { current / total }
}

#[no_mangle]
pub extern "C" fn log_engine_search(
    engine: *mut LogEngine, // changed to mut so we can sync pieces
    query: *const c_char,
    start_line: usize,
) -> isize {
    let engine = unsafe {
        if engine.is_null() {
            return -1;
        }
        &mut *engine
    };
    if query.is_null() {
        return -1;
    }
    let query_bytes = match unsafe { CStr::from_ptr(query) }.to_bytes_with_nul().split_last() {
        Some((&0, bytes)) => bytes,
        _ => return -1,
    };
    if query_bytes.is_empty() {
        return -1;
    }

    engine.sync_pieces();
    let (mut piece_idx, mut offset) = engine.find_piece_idx(start_line);
    let mut current_logical = start_line;

    while piece_idx < engine.pieces.len() {
        let piece = &engine.pieces[piece_idx];
        match piece {
            Piece::Original { start_line: p_start, line_count } => {
                let bytes = engine.get_original_bytes(p_start + offset, line_count - offset);
                if let Some(pos) = memmem::find(bytes, query_bytes) {
                    
                    // found the byte offset, now manually count newlines up to this point
                    // to resolve the actual logical line number. slow but accurate.
                    let slice_to_match = &bytes[..pos];
                    let mut lines = 0;
                    let mut iter = memchr2_iter(b'\n', b'\r', slice_to_match).peekable();
                    while let Some(p) = iter.next() {
                        lines += 1;
                        if slice_to_match[p] == b'\r' {
                            if let Some(&np) = iter.peek() {
                                if np == p + 1 && slice_to_match[np] == b'\n' {
                                    iter.next();
                                }
                            }
                        }
                    }
                    return (current_logical + lines) as isize;
                }
            }
            Piece::Memory { start_idx, line_count } => {
                // query might be cursed too.
                let q_str = String::from_utf8_lossy(query_bytes);
                for i in offset..*line_count {
                    if engine.memory_buffer[start_idx + i].contains(q_str.as_ref()) {
                        return (current_logical + i - offset) as isize;
                    }
                }
            }
        }
        current_logical += piece.line_count() - offset;
        offset = 0;
        piece_idx += 1;
    }
    -1
}

#[no_mangle]
pub extern "C" fn log_engine_search_backward(
    engine: *mut LogEngine, // changed to mut so we can sync pieces
    query: *const c_char,
    start_line: usize,
) -> isize {
    let engine = unsafe {
        if engine.is_null() {
            return -1;
        }
        &mut *engine
    };
    if query.is_null() {
        return -1;
    }
    let query_bytes = match unsafe { CStr::from_ptr(query) }.to_bytes_with_nul().split_last() {
        Some((&0, bytes)) => bytes,
        _ => return -1,
    };
    if query_bytes.is_empty() {
        return -1;
    }

    engine.sync_pieces();
    let (mut piece_idx, mut offset) = engine.find_piece_idx(start_line);
    if piece_idx >= engine.pieces.len() {
        piece_idx = engine.pieces.len().saturating_sub(1);
        offset = engine.pieces[piece_idx].line_count().saturating_sub(1);
    }

    let mut current_logical = start_line;

    // walking backwards through pieces. same logic as forward search but reversed.
    loop {
        let piece = &engine.pieces[piece_idx];
        match piece {
            Piece::Original { start_line: p_start, .. } => {
                let bytes = engine.get_original_bytes(*p_start, offset + 1);
                if let Some(pos) = memmem::rfind(bytes, query_bytes) {
                    let slice_to_match = &bytes[..pos];
                    let mut lines = 0;
                    let mut iter = memchr2_iter(b'\n', b'\r', slice_to_match).peekable();
                    while let Some(p) = iter.next() {
                        lines += 1;
                        if slice_to_match[p] == b'\r' {
                            if let Some(&np) = iter.peek() {
                                if np == p + 1 && slice_to_match[np] == b'\n' {
                                    iter.next();
                                }
                            }
                        }
                    }
                    return (current_logical - offset + lines) as isize;
                }
            }
            Piece::Memory { start_idx, .. } => {
                // query might be cursed too.
                let q_str = String::from_utf8_lossy(query_bytes);
                for i in (0..=offset).rev() {
                    if engine.memory_buffer[start_idx + i].contains(q_str.as_ref()) {
                        return (current_logical - offset + i) as isize;
                    }
                }
            }
        }

        if piece_idx == 0 {
            break;
        }
        current_logical = current_logical.saturating_sub(offset + 1);
        piece_idx -= 1;
        offset = engine.pieces[piece_idx].line_count().saturating_sub(1);
    }
    -1
}

#[no_mangle]
pub extern "C" fn log_engine_free(engine: *mut LogEngine) {
    if !engine.is_null() {
        unsafe {
            let engine_box = Box::from_raw(engine);
            // trigger the cyanide pill. the background thread will see this and die quietly.
            engine_box.cancel_token.store(true, Ordering::Relaxed);
            // Rust's drop takes care of the rest. The Arc<Mmap> will stay alive 
            // just long enough for the background thread to exit cleanly.
        }
    }
}
