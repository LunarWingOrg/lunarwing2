use get_size2::{GetSize, GetSizeTracker};

use crate::{CharStr, CharString};

#[cfg_attr(docsrs, doc(cfg(feature = "get-size")))]
impl GetSize for CharString {
    fn get_heap_size_with_tracker<T: GetSizeTracker>(&self, mut tracker: T) -> (usize, T) {
        let size = if self.is_heap_allocated() && tracker.track(self.as_str().as_ptr()) {
            self.heap_allocation_size()
        } else {
            0
        };
        (size, tracker)
    }
}

#[cfg_attr(docsrs, doc(cfg(feature = "get-size")))]
impl GetSize for CharStr {
    fn get_heap_size_with_tracker<T: GetSizeTracker>(&self, mut tracker: T) -> (usize, T) {
        let size = if self.is_heap_allocated() && tracker.track(self.as_str().as_ptr()) {
            self.heap_allocation_size()
        } else {
            0
        };
        (size, tracker)
    }
}

#[cfg(test)]
mod tests {
    use core::mem::size_of;

    use super::*;

    #[test]
    fn allocation_sizes_include_their_headers() {
        let exact = CharStr::from("a string longer than the inline limit");
        assert_eq!(exact.heap_allocation_size(), size_of::<usize>() + exact.len());

        let growable = CharString::with_capacity(128);
        assert_eq!(growable.heap_allocation_size(), 2 * size_of::<usize>() + 128);
    }
}
