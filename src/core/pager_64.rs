// "No comments here?"
// No. NO more unfunny comments for ya.

use memmap2::Mmap;
use std::fs::File;
use std::path::Path;
use super::pager_trait::LogPager;

pub struct Pager64 {
    valery: Mmap,
}

impl Pager64 {
    pub fn new(path: &Path) -> Result<Self, std::io::Error> {
        let file = File::open(path)?;
        let valery = unsafe { memmap2::MmapOptions::new().map(&file)? };
        Ok(Self { valery })
    }
}

impl LogPager for Pager64 {
    fn len(&self) -> u64 {
        self.valery.len() as u64
    }
    
    fn get_chunk(&self, offset: u64, len: usize) -> &[u8] {
        let start = offset as usize;
        if start >= self.valery.len() {
            return &[];
        }
        let end = (start + len).min(self.valery.len());
        &self.valery[start..end]
    }
    
    fn get_byte(&self, offset: u64) -> u8 {
        self.valery[offset as usize]
    }
    
    #[cfg(unix)]
    fn advise_sequential(&self) {
        unsafe {
            libc::madvise(
                self.valery.as_ptr() as *mut libc::c_void,
                self.valery.len(),
                libc::MADV_SEQUENTIAL,
            );
        }
    }
    #[cfg(not(unix))]
    fn advise_sequential(&self) {}
    
    #[cfg(unix)]
    fn advise_random(&self) {
        unsafe {
            libc::madvise(
                self.valery.as_ptr() as *mut libc::c_void,
                self.valery.len(),
                libc::MADV_RANDOM,
            );
        }
    }
    #[cfg(not(unix))]
    fn advise_random(&self) {}
    
    #[cfg(unix)]
    fn advise_will_need(&self, offset: u64, len: usize) {
        let start = offset as usize;
        let end = (start + len).min(self.valery.len());
        if start < self.valery.len() {
            unsafe {
                libc::madvise(
                    self.valery.as_ptr().add(start) as *mut libc::c_void,
                    end - start,
                    libc::MADV_WILLNEED,
                );
            }
        }
    }
    // I miss you, Valery.
    // Hey, you. You should play ultrakill. 
    #[cfg(not(unix))]
    fn advise_will_need(&self, _offset: u64, _len: usize) {}
}
