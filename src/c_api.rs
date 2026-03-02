use std::ffi::CStr;
use std::os::raw::c_char;
use std::ptr;
use std::sync::atomic::Ordering;
use memchr::{memchr2_iter, memmem};

use crate::core::LogEngine;
use crate::models::Piece;

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
