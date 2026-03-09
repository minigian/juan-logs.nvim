// Abandon all hope, ye who enter here.
use std::ffi::CStr;
use std::os::raw::c_char;
use std::path::Path;
use std::ptr;
use std::sync::atomic::Ordering;
use memchr::memmem;

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

use crate::core::LogEngine;
use crate::models::Piece;

fn cstr_to_path<'a>(c_str: &'a CStr) -> &'a Path {
    #[cfg(unix)]
    {
        Path::new(std::ffi::OsStr::from_bytes(c_str.to_bytes()))
    }
    #[cfg(not(unix))]
    {
        Path::new(std::str::from_utf8(c_str.to_bytes()).unwrap_or_default())
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
    let file_path = cstr_to_path(c_str);
    if let Ok(engine) = LogEngine::new(file_path, lazy) {
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
    out_buffer: *mut c_char,
    max_len: usize,
    out_len: *mut usize,
) -> bool {
    // the thing behind :LogJump and scrolling. fetches chunks without loading the whole file.
    let engine = unsafe {
        if engine.is_null() || out_buffer.is_null() || max_len == 0 {
            return false;
        }
        &mut *engine
    };
    let block = engine.get_block(start_line, num_lines);
    let mut bytes = block.into_bytes();
    
    for b in &mut bytes {
        if *b == 0 { *b = b' '; }
    }
    
    let copy_len = std::cmp::min(bytes.len(), max_len - 1);
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buffer as *mut u8, copy_len);
        *out_buffer.add(copy_len) = 0;
        if !out_len.is_null() {
            *out_len = copy_len;
        }
    }
    true
}

#[no_mangle]
pub extern "C" fn log_engine_get_eof_block(
    engine: *mut LogEngine,
    num_lines: usize,
    out_buffer: *mut c_char,
    max_len: usize,
    out_len: *mut usize,
) -> bool {
    let engine = unsafe {
        if engine.is_null() || out_buffer.is_null() || max_len == 0 { return false; }
        &mut *engine
    };
    let block = engine.get_eof_block(num_lines);
    let mut bytes = block.into_bytes();
    
    for b in &mut bytes {
        if *b == 0 { *b = b' '; }
    }
    
    let copy_len = std::cmp::min(bytes.len(), max_len - 1);
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buffer as *mut u8, copy_len);
        *out_buffer.add(copy_len) = 0;
        if !out_len.is_null() {
            *out_len = copy_len;
        }
    }
    true
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
    let c_str = unsafe { CStr::from_ptr(path) };
    let file_path = cstr_to_path(c_str);
    return engine.save(file_path);
}

// FFI for the async save. don't block the UI.
#[no_mangle]
pub extern "C" fn log_engine_save_async(engine: *const LogEngine, path: *const c_char) -> bool {
    let engine = unsafe {
        if engine.is_null() { return false; }
        &*engine
    };
    if path.is_null() { return false; }
    let c_str = unsafe { CStr::from_ptr(path) };
    let file_path = cstr_to_path(c_str);
    engine.save_async(file_path)
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
    engine: *mut LogEngine,
    query: *const c_char,
    start_line: usize,
    end_line: usize, // Bounded search to prevent the PC from melting
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
        if current_logical > end_line {
            break; // User's patience limit reached
        }

        let piece = &engine.pieces[piece_idx];
        let available_lines = piece.line_count() - offset;
        let lines_to_search = std::cmp::min(available_lines, end_line.saturating_sub(current_logical) + 1);

        match piece {
            Piece::Original { start_line: p_start, .. } => {
                let bytes = engine.get_original_bytes(p_start + offset, lines_to_search);
                if let Some(pos) = memmem::find(bytes, query_bytes) {
                    // AVX2 goes brrrrr on search now. No more slow iterators.
                    let lines = if engine.is_fixed_width {
                        pos / engine.fixed_width_size
                    } else {
                        crate::core::count_newlines(&bytes[..pos])
                    };
                    return (current_logical + lines) as isize;
                }
            }
            Piece::Memory { start_idx, .. } => {
                // query might be cursed too.
                let q_str = String::from_utf8_lossy(query_bytes);
                for i in 0..lines_to_search {
                    if engine.memory_buffer[start_idx + offset + i].contains(q_str.as_ref()) {
                        return (current_logical + i) as isize;
                    }
                }
            }
        }
        current_logical += available_lines;
        offset = 0;
        piece_idx += 1;
    }
    -1
}

#[no_mangle]
pub extern "C" fn log_engine_search_backward(
    engine: *mut LogEngine,
    query: *const c_char,
    start_line: usize,
    end_line: usize, // The floor of our search
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

    // walking backwards through pieces.
    loop {
        let piece_start_logical = current_logical.saturating_sub(offset);
        if current_logical < end_line { 
            break; // Hit the floor
        }

        let piece = &engine.pieces[piece_idx];
        let skip = if end_line > piece_start_logical { end_line - piece_start_logical } else { 0 };
        let lines_to_fetch = offset + 1 - skip;

        match piece {
            Piece::Original { start_line: p_start, .. } => {
                let bytes = engine.get_original_bytes(p_start + skip, lines_to_fetch);
                if let Some(pos) = memmem::rfind(bytes, query_bytes) {
                    // AVX2 backwards. Still fast.
                    let lines = if engine.is_fixed_width {
                        pos / engine.fixed_width_size
                    } else {
                        crate::core::count_newlines(&bytes[..pos])
                    };
                    return (piece_start_logical + skip + lines) as isize;
                }
            }
            Piece::Memory { start_idx, .. } => {
                let q_str = String::from_utf8_lossy(query_bytes);
                for i in (skip..=offset).rev() {
                    if engine.memory_buffer[start_idx + i].contains(q_str.as_ref()) {
                        return (piece_start_logical + i) as isize;
                    }
                }
            }
        }

        if piece_idx == 0 {
            break;
        }
        piece_idx -= 1;
        offset = engine.pieces[piece_idx].line_count().saturating_sub(1);
        current_logical = piece_start_logical.saturating_sub(1);
    }
    -1
}

#[repr(C)]
pub struct LogStats {
    pub progress: f32,
    pub total_lines: usize,
    pub file_size_bytes: usize,
    pub indexing_time_ms: u64,
}

#[no_mangle]
pub extern "C" fn log_engine_get_stats(engine: *const LogEngine, out_stats: *mut LogStats) -> bool {
    let engine = unsafe {
        if engine.is_null() || out_stats.is_null() { return false; }
        &*engine
    };
    
    let idx = engine.index.read().unwrap();
    let processed = engine.bytes_processed.load(Ordering::Relaxed) as f32;
    let total = engine.mmap.len() as f32;
    
    let progress = if idx.is_finished { 1.0 } else if total == 0.0 { 1.0 } else { processed / total };
    
    unsafe {
        (*out_stats).progress = progress;
        // We don't call sync_pieces here because we only have a const pointer.
        // Summing the pieces gives us the currently synced total, which is accurate enough for stats.
        (*out_stats).total_lines = engine.pieces.iter().map(|p| p.line_count()).sum();
        (*out_stats).file_size_bytes = engine.mmap.len();
        (*out_stats).indexing_time_ms = idx.indexing_time_ms as u64;
    }
    true
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

#[no_mangle]
pub extern "C" fn log_engine_search_async(
    engine: *mut LogEngine,
    query: *const c_char,
    start_line: usize,
) {
    let engine = unsafe {
        if engine.is_null() { return; }
        &mut *engine
    };
    if query.is_null() { return; }
    let query_str = unsafe { CStr::from_ptr(query) }.to_string_lossy();
    engine.search_async(query_str.as_ref(), start_line);
}

#[no_mangle]
pub extern "C" fn log_engine_get_search_status(engine: *const LogEngine) -> isize {
    let engine = unsafe {
        if engine.is_null() { return -2; }
        &*engine
    };
    if engine.is_searching.load(Ordering::SeqCst) {
        return -1;
    }
    engine.search_result.load(Ordering::SeqCst)
}

#[no_mangle]
pub extern "C" fn log_engine_cancel_search(engine: *mut LogEngine) {
    let engine = unsafe {
        if engine.is_null() { return; }
        &mut *engine
    };
    engine.search_cancel.store(true, Ordering::SeqCst);
}

#[no_mangle]
pub extern "C" fn log_engine_is_fixed_width(engine: *const LogEngine) -> bool {
    let engine = unsafe {
        if engine.is_null() { return false; }
        &*engine
    };
    engine.is_fixed_width
}