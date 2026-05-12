// I know what are you thinking: "Why are you adding 32-bit support, bro? Nobody uses that shit."
// Because I can, and because someone in Africa or Asia might need it (probably not) xD.
// TODO: Add 16-bit support for the lulz. I need a life...

use std::path::Path;
use std::fs::File;
#[cfg(unix)]
use std::os::unix::fs::FileExt;
#[cfg(windows)]
use std::os::windows::fs::FileExt;
use std::sync::{Arc, RwLock};
use std::time::Instant;
use std::cell::RefCell;

use super::pager_trait::LogPager;

const WINDOW_SIZE: u64 = 256 * 1024 * 1024; // 256 MB chunks
const MAX_WINDOWS: usize = 4; // Max 1GB allocated in 32-bit processes

thread_local! {
    static PINNED: RefCell<Vec<Arc<[u8]>>> = RefCell::new(Vec::new());
}

struct Window {
    id: u32,
    data: Arc<[u8]>,
    last_accessed: Instant,
}

pub struct Pager32 {
    file: File,
    len: u64,
    windows: RwLock<Vec<Window>>,
}

impl Pager32 {
    pub fn new(path: &Path) -> Result<Self, std::io::Error> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self { 
            file, 
            len,
            windows: RwLock::new(Vec::with_capacity(MAX_WINDOWS)),
        })
    }

    fn fetch_window(&self, window_id: u32) -> Option<Arc<[u8]>> {
        let mut cache = self.windows.write().unwrap();
        
        if let Some(w) = cache.iter_mut().find(|w| w.id == window_id) {
            w.last_accessed = Instant::now();
            return Some(Arc::clone(&w.data));
        }

        if cache.len() >= MAX_WINDOWS {
            cache.sort_by_key(|w| w.last_accessed);
            cache.remove(0);
        }

        let offset = window_id as u64 * WINDOW_SIZE;
        let read_len = std::cmp::min(WINDOW_SIZE, self.len.saturating_sub(offset)) as usize;
        
        let mut buf = vec![0u8; read_len];
        
        #[cfg(unix)]
        {
            if self.file.read_exact_at(&mut buf, offset).is_err() {
                return None;
            }
        }
        
        #[cfg(windows)]
        {
            match self.file.seek_read(&mut buf, offset) {
                Ok(n) if n == read_len => {},
                _ => return None,
            }
        }

        let data: Arc<[u8]> = Arc::from(buf.into_boxed_slice());
        
        cache.push(Window {
            id: window_id,
            data: Arc::clone(&data),
            last_accessed: Instant::now(),
        });

        Some(data)
    }
}

impl LogPager for Pager32 {
    fn len(&self) -> u64 { self.len }
    
    fn get_chunk(&self, offset: u64, len: usize) -> &[u8] {
        if offset >= self.len || len == 0 {
            return &[];
        }

        let window_id = (offset / WINDOW_SIZE) as u32;
        let local_offset = (offset % WINDOW_SIZE) as usize;
        
        let mut read_len = len;
        if local_offset + read_len > WINDOW_SIZE as usize {
            read_len = WINDOW_SIZE as usize - local_offset;
        }
        
        let arc = {
            let cache = self.windows.read().unwrap();
            if let Some(w) = cache.iter().find(|w| w.id == window_id) {
                Arc::clone(&w.data)
            } else {
                drop(cache);
                match self.fetch_window(window_id) {
                    Some(arc) => arc,
                    None => return &[],
                }
            }
        };
        
        let ptr = Arc::as_ptr(&arc);
        
        PINNED.with(|p| {
            let mut pinned = p.borrow_mut();
            pinned.clear();
            pinned.push(arc);
        });
        
        unsafe {
            let slice = &*ptr;
            let end = std::cmp::min(local_offset + read_len, slice.len());
            &slice[local_offset..end]
        }
    }

    fn get_byte(&self, offset: u64) -> u8 {
        if offset >= self.len {
            return 0;
        }
        let window_id = (offset / WINDOW_SIZE) as u32;
        let local_offset = (offset % WINDOW_SIZE) as usize;
        
        let cache = self.windows.read().unwrap();
        if let Some(w) = cache.iter().find(|w| w.id == window_id) {
            return w.data[local_offset];
        }
        drop(cache);
        
        match self.fetch_window(window_id) {
            Some(arc) => arc[local_offset],
            None => 0,
        }
    }
    

    // Aight. 
    fn advise_sequential(&self) {}
    fn advise_random(&self) {}
    fn advise_will_need(&self, _offset: u64, _len: usize) {}
}
