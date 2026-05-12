// Abandon all hope, ye who enter here.
use std::ffi::CStr;
use std::os::raw::c_char;
use std::path::Path;
use std::ptr;
use std::sync::atomic::Ordering;
use memchr::memmem;

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

use crate::core::{LogEngine, LogPager};
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
pub extern "C" fn log_engine_total_lines(engine: *mut LogEngine) -> u64 {
    // :LogLines. fast because we already paid the price at startup.
    let engine = unsafe {
        if engine.is_null() {
            return 0;
        }
        &mut *engine
    };
    // force a sync so we don't lie to Lua about having 0 lines
    engine.sync_pieces();
    engine.total_lines() as u64
}

#[no_mangle]
pub extern "C" fn log_engine_get_block(
    engine: *mut LogEngine,
    start_line: u64,
    num_lines: u64,
    out_buffer: *mut c_char,
    max_len: u64,
    out_len: *mut u64,
) -> bool {
    // the thing behind :LogJump and scrolling. fetches chunks without loading the whole file.
    let engine = unsafe {
        if engine.is_null() || out_buffer.is_null() || max_len == 0 {
            return false;
        }
        &mut *engine
    };
    let block = engine.get_block(start_line as usize, num_lines as usize);
    
    let copy_len = std::cmp::min(block.len() as u64, max_len - 1);
    unsafe {
        std::ptr::copy_nonoverlapping(block.as_ptr(), out_buffer as *mut u8, copy_len as usize);
        *out_buffer.add(copy_len as usize) = 0;
        if !out_len.is_null() {
            *out_len = copy_len;
        }
    }
    true
}

#[no_mangle]
pub extern "C" fn log_engine_get_eof_block(
    engine: *mut LogEngine,
    num_lines: u64,
    out_buffer: *mut c_char,
    max_len: u64,
    out_len: *mut u64,
) -> bool {
    let engine = unsafe {
        if engine.is_null() || out_buffer.is_null() || max_len == 0 { return false; }
        &mut *engine
    };
    let block = engine.get_eof_block(num_lines as usize);
    
    let copy_len = std::cmp::min(block.len() as u64, max_len - 1);
    unsafe {
        std::ptr::copy_nonoverlapping(block.as_ptr(), out_buffer as *mut u8, copy_len as usize);
        *out_buffer.add(copy_len as usize) = 0;
        if !out_len.is_null() {
            *out_len = copy_len;
        }
    }
    true
}

#[no_mangle]
pub extern "C" fn log_engine_apply_edit(
    engine: *mut LogEngine,
    start_line: u64,
    num_deleted: u64,
    new_text: *const c_char,
    new_text_len: u64,
) {
    let engine = unsafe {
        if engine.is_null() {
            return;
        }
        &mut *engine
    };
    // nvim might send weird stuff, salvage what we can.
    let text = if new_text.is_null() || new_text_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(new_text as *const u8, new_text_len as usize) }
    };
    engine.apply_edit(start_line as usize, num_deleted as usize, text);
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
    query_len: u64,
    start_line: u64,
    end_line: u64, // Bounded search to prevent the PC from melting
) -> isize {
    let engine = unsafe {
        if engine.is_null() {
            return -1;
        }
        &mut *engine
    };
    if query.is_null() || query_len == 0 {
        return -1;
    }
    let query_bytes = unsafe { std::slice::from_raw_parts(query as *const u8, query_len as usize) };

    engine.sync_pieces();
    let (mut piece_idx, mut offset) = engine.find_piece_idx(start_line as usize);
    let mut current_logical = start_line as usize;
    let end_logical = end_line as usize;

    while piece_idx < engine.pieces.len() {
        if current_logical > end_logical {
            break; // User's patience limit reached
        }

        let piece = &engine.pieces[piece_idx];
        let available_lines = piece.line_count() - offset;
        let lines_to_search = std::cmp::min(available_lines, end_logical.saturating_sub(current_logical) + 1);

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
                // native bytes
                for i in 0..lines_to_search {
                    if memmem::find(&engine.memory_buffer[start_idx + offset + i], query_bytes).is_some() {
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
    query_len: u64,
    start_line: u64,
    end_line: u64, // The floor of our search
) -> isize {
    let engine = unsafe {
        if engine.is_null() {
            return -1;
        }
        &mut *engine
    };
    if query.is_null() || query_len == 0 {
        return -1;
    }
    let query_bytes = unsafe { std::slice::from_raw_parts(query as *const u8, query_len as usize) };

    engine.sync_pieces();
    let (mut piece_idx, mut offset) = engine.find_piece_idx(start_line as usize);
    if piece_idx >= engine.pieces.len() {
        piece_idx = engine.pieces.len().saturating_sub(1);
        offset = engine.pieces[piece_idx].line_count().saturating_sub(1);
    }

    let mut current_logical = start_line as usize;
    let end_logical = end_line as usize;

    // walking backwards through pieces.
    loop {
        let piece_start_logical = current_logical.saturating_sub(offset);
        if current_logical < end_logical { 
            break; // Hit the floor
        }

        let piece = &engine.pieces[piece_idx];
        let skip = if end_logical > piece_start_logical { end_logical - piece_start_logical } else { 0 };
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
                for i in (skip..=offset).rev() {
                    if memmem::rfind(&engine.memory_buffer[start_idx + i], query_bytes).is_some() {
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
    pub total_lines: u64,
    pub file_size_bytes: u64,
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
        (*out_stats).total_lines = engine.pieces.iter().map(|p| p.line_count() as u64).sum();
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
    query_len: u64,
    start_line: u64,
) {
    let engine = unsafe {
        if engine.is_null() { return; }
        &mut *engine
    };
    if query.is_null() || query_len == 0 { return; }
    let query_bytes = unsafe { std::slice::from_raw_parts(query as *const u8, query_len as usize) };
    engine.search_async(query_bytes, start_line as usize);
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