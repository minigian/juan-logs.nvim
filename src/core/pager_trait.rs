
pub trait LogPager: Send + Sync {
    fn len(&self) -> u64;
    
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    
    /// Ensures the chunk containing the given offset is loaded and returns a slice 
    /// from the given `offset` up to the requested `len`, or up to the end of the current sliding window.
    /// The caller must loop if it needs more data and receives less than `len`.
    fn get_chunk(&self, offset: u64, len: usize) -> &[u8];
    
    fn get_byte(&self, offset: u64) -> u8;
    
    fn last_byte(&self) -> Option<u8> {
        let length = self.len();
        if length > 0 {
            Some(self.get_byte(length - 1))
        } else {
            None
        }
    }
    
    // Abstract cache warming hint to avoid direct mmap pointer access
    fn prefetch(&self, _offset: u64) {}
    
    fn advise_sequential(&self);
    fn advise_random(&self);
    fn advise_will_need(&self, offset: u64, len: usize);
}
