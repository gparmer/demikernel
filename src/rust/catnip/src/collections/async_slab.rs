#![allow(unused)]
// Leaving this TODO for now, since I'm just going to fork `unicycle` for now.
// Here's two main ideas for how we could do this efficiently.
//
// Option 1 (alignment tricks):
// 1) Create a simple PinSlab<T> to store our futures.
// 2) Bound the number of futures in the system by N (say 4096). Or, accept lower
//    fidelity wakeups based on N.
// 3) Heap allocate a reference counted WakerState with alignment N.
// 4) When creating a Waker for Future number i, compute the pointer WakerState + i.
// 5) When waking a Waker, compute the original WakerState pointer and set the
//    bit for the original index.
// 6) When cloning a Waker, compute the original pointer, clone that, and then
//    return the *same* pointer.
//
// Option 2 (intrusive waker):
// 1) Build a specialized PinSlab, where each Page has a header and is aligned
//    to some N.
// 2) Wakers are just pointers to the future directly.
// 3) Each header contains a reference count, and creating a waker bumps this
//    reference count. The Waker has no guarantee that the original future is
//    still there, but it knows that the page is still live.
// 4) Each header has a reference counted pointer to a central control block,
//    which contains a root bitset indicating which pages have ready futures and
//    a Option<Waker> for the parent waker.
// 5) The AsyncSet then has a pointer to the control block it uses for finding
//    which futures are currently ready.
//
// I'm partial to Option 2 but we'll have to benchmark and see how it also plays
// into our multicore scalable design.
//

// use super::pin_slab::{PinSlab, SlabKey};
use uniset::BitSet;
// use parking_lot::Mutex;
// use std::sync::Arc;
// use std::task::{Context, Wake, Waker, Poll, RawWaker, RawWakerVTable};
use std::pin::Pin;
// use std::future::Future;
// use futures::Stream;

// use bit_vec::BitVec;
use std::slice;
use std::ops::Deref;
use std::ptr::{self, NonNull};
use std::cell::{RefCell, Cell};
use std::alloc::{AllocRef, Global, Layout};
use std::mem::{self, MaybeUninit};

struct AllocOwned<T> {
    has_free: BitSet,
    pages: Vec<PagePtr<T>>
}

struct Root<T> {
    refcount: Cell<usize>,
    shared_ready: RefCell<BitSet>,
    owned: RefCell<AllocOwned<T>>,
}

impl<T> Root<T> {
    fn new() -> RootPtr<T> {
        let owned = AllocOwned {
            has_free: BitSet::new(),
            pages: Vec::new(),
        };
        let root = Self {
            refcount: Cell::new(1),
            shared_ready: RefCell::new(BitSet::new()),
            owned: RefCell::new(owned),
        };
        let ptr = Global.alloc(Layout::for_value(&root)).expect("Allocation failed").cast();
        unsafe { ptr::write(ptr.as_ptr(), root) };
        RootPtr { ptr }
    }
}


struct RootPtr<T> {
    ptr: NonNull<Root<T>>,
}

impl<T> RootPtr<T> {
    fn add_page(&self) -> usize {
        let page_ix = {
            let mut owned = self.owned.borrow_mut();
            let page_ptr = Page::new(self.clone());
            let page_ix = owned.pages.len();
            owned.pages.push(page_ptr);
            owned.has_free.set(page_ix);
            page_ix
        };
        self.shared_ready.borrow_mut().reserve(page_ix + 1);
        page_ix
    }

    fn alloc(&self, value: T) -> (usize, usize) {
        {
            let mut owned = self.owned.borrow_mut();
            for slab_ix in owned.has_free.iter() {
                let (page_ix, newly_full) = owned.pages[slab_ix].alloc(value);
                if newly_full {
                    owned.has_free.clear(slab_ix);
                }
                return (slab_ix, page_ix);
            }
        }
        let slab_ix = self.add_page();
        {
            let mut owned = self.owned.borrow_mut();
            let (page_ix, newly_full) = owned.pages[slab_ix].alloc(value);
            assert!(!newly_full);
            (slab_ix, page_ix)
        }
    }

    fn free(&self, slab_ix: usize, page_ix: usize) -> bool {
        let mut owned = self.owned.borrow_mut();
        if slab_ix >= owned.pages.len() {
            return false;
        }
        if !owned.pages[slab_ix].free(page_ix) {
            return false;
        }
        if !owned.has_free.test(slab_ix) {
            owned.has_free.set(slab_ix);
        }
        true
    }

    fn get(&self, slab_ix: usize, page_ix: usize) -> Option<&T> {
        let mut owned = self.owned.borrow();
        Some(unsafe { &*owned.pages.get(slab_ix)?.get(page_ix)? })
    }
}

impl<T> Clone for RootPtr<T> {
    fn clone(&self) -> Self {
        let root = unsafe { self.ptr.as_ref() };
        let refcount = root.refcount.get();
        if refcount == 0 || refcount == usize::MAX {
            panic!("Invalid refcount");
        }
        root.refcount.set(refcount + 1);
        Self { ptr: self.ptr }
    }
}

impl<T> Deref for RootPtr<T> {
    type Target = Root<T>;

    fn deref(&self) -> &Root<T> {
        unsafe { self.ptr.as_ref() }
    }
}

impl<T> Drop for RootPtr<T> {
    fn drop(&mut self) {
        let root = unsafe { self.ptr.as_ref() };
        let refcount = root.refcount.get();
        if refcount == 0 {
            panic!("Invalid refcount");
        }
        // TODO: Subtract out page back pointers here.
        if refcount == 1 {
            unsafe {
                Global.dealloc(self.ptr.cast(), Layout::for_value(self.ptr.as_ref()));
            }
        } else {
            root.refcount.set(refcount - 1);
        }
    }
}


struct PagePtr<T> {
    ptr: NonNull<Page<T>>,
}

impl<T> Clone for PagePtr<T> {
    fn clone(&self) -> Self {
        let root = unsafe { self.ptr.as_ref() };
        let refcount = root.header.refcount.get();
        if refcount == 0 || refcount == usize::MAX {
            panic!("Invalid refcount");
        }
        root.header.refcount.set(refcount + 1);
        Self { ptr: self.ptr }
    }
}

impl<T> Deref for PagePtr<T> {
    type Target = Page<T>;

    fn deref(&self) -> &Page<T> {
        unsafe { self.ptr.as_ref() }
    }
}

impl<T> Drop for PagePtr<T> {
    fn drop(&mut self) {
        let root = unsafe { self.ptr.as_ref() };
        let refcount = root.header.refcount.get();
        if refcount == 0 {
            panic!("Invalid refcount");
        }
        if refcount == 1 {
            unsafe {
                ptr::drop_in_place(self.ptr.as_ptr());
                let (layout, _) = Page::<T>::layout();
                Global.dealloc(self.ptr.cast(), layout);
            }
        } else {
            root.header.refcount.set(refcount - 1);
        }
    }
}

struct PageHeader<T> {
    refcount: Cell<usize>,
    root: RootPtr<T>,
    allocated: Cell<u64>,
    ready: Cell<u64>,
}

struct Page<T> {
    header: PageHeader<T>,
    // Actually an array of values (see Self::layout()).
    values: MaybeUninit<T>,
}

impl<T> Page<T> {
    const fn layout() -> (Layout, usize) {
        let page_size = 4096;
        let header_layout = Layout::new::<PageHeader<T>>();

        assert!(header_layout.align() <= page_size);

        // No ZSTs here!
        let value_layout = Layout::new::<T>();
        assert!(value_layout.size() > 0);

        let padding = header_layout.padding_needed_for(value_layout.align());

        // Check that we have at least space for one value.
        assert!(header_layout.size() + padding + value_layout.size() <= page_size);

        let num_values = (page_size - (header_layout.size() + padding)) / value_layout.size();

        // TODO: This alignment is too strict. For `n` values, we only need the pointer to be of
        // alignment `n`: For element `i` we can hand out pointer `ptr as *mut u8 + i`.
        (unsafe { Layout::from_size_align_unchecked(page_size, page_size) }, num_values)
    }

    pub fn new(root: RootPtr<T>) -> PagePtr<T> {
        let page = Self {
            header: PageHeader {
                refcount: Cell::new(1),
                root,
                allocated: Cell::new(0),
                ready: Cell::new(0),
            },
            values: MaybeUninit::uninit(),
        };
        let (layout, num_values) = Self::layout();
        let ptr = Global.alloc(layout).expect("Allocation failed").cast();
        unsafe { ptr::write(ptr.as_ptr(), page) };

        let values: &mut [MaybeUninit<T>] = unsafe {
            slice::from_raw_parts_mut(&mut (*ptr.as_ptr()).values as *mut _, num_values)
        };
        for v in values.iter_mut() {
            *v = MaybeUninit::zeroed();
        }
        PagePtr { ptr }
    }

    unsafe fn values(&self) -> &mut [MaybeUninit<T>] {
        let (_, num_values) = Self::layout();
        slice::from_raw_parts_mut(&self.values as *const _ as *mut _, num_values)
    }

    pub fn alloc(&self, value: T) -> (usize, bool) {
        let allocated = self.header.allocated.get();
        let first_free = allocated.trailing_ones();
        if first_free == 64 {
            panic!("Allocated in full page");
        }
        let ix = first_free as usize;

        debug_assert_eq!(allocated & (1 << first_free), 0);
        let new_allocated = allocated | (1 << first_free);
        self.header.allocated.set(new_allocated);
        unsafe {
            self.values()[ix].write(value);
        }
        let newly_full = new_allocated == u64::MAX;
        (ix, newly_full)
    }

    pub fn free(&self, ix: usize) -> bool {
        let (_, num_values) = Self::layout();
        assert!(ix < num_values);

        let allocated = self.header.allocated.get();
        if allocated & (1 << ix) == 0 {
            return false;
        }
        unsafe {
            ptr::drop_in_place(&mut self.values()[ix] as *mut _);
        };
        self.header.allocated.set(allocated & !(1 << ix));
        true
    }

    pub fn get(&self, ix: usize) -> Option<*mut T> {
        let (_, num_values) = Self::layout();
        if ix >= num_values {
            return None;
        }
        let allocated = self.header.allocated.get();
        if allocated & (1 << ix) == 0 {
            return None;
        }
        unsafe {
            Some(self.values()[ix].as_mut_ptr())
        }
    }
}

impl<T> Drop for Page<T> {
    fn drop(&mut self) {
        // Adapted from https://lemire.me/blog/2018/02/21/iterating-over-set-bits-quickly/
        let mut allocated = self.header.allocated.get();
        let values = unsafe { self.values() };

        while allocated != 0 {
            let t = allocated & allocated.wrapping_neg();
            let ix = allocated.trailing_zeros();
            unsafe {
                ptr::drop_in_place(&mut values[ix as usize] as *mut _);
            }
            allocated ^= t;
        }
    }
}



// const PAGE_SIZE: usize = 32;

// struct WakerState {
//     ready: BitSet,
//     parent_waker: Option<Waker>,
// }

// pub struct AsyncSet<T> {
//     slab: PinSlab<T, PAGE_SIZE>,

//     // TODO: Investigate using `PinSlab`'s optimizations here.
//     waker_state: Arc<Mutex<WakerState>>,
//     current_ready: BitSet,
// }

// impl<T> AsyncSet<T> {
//     pub fn new() -> Self {
//         let waker_state = WakerState {
//             ready: BitSet::new(),
//             parent_waker: None,
//         };
//         Self {
//             slab: PinSlab::new(),
//             waker_state: Arc::new(Mutex::new(waker_state)),
//             current_ready: BitSet::new(),
//         }
//     }

//     pub fn insert(&mut self, value: T) -> SlabKey {
//         let key = self.slab.alloc(value);

//         self.current_ready.set(key.into());

//         let mut waker_state = self.waker_state.lock();
//         if let Some(p) = waker_state.parent_waker.take() {
//             p.wake();
//         }

//         key
//     }

//     pub fn remove(&mut self, key: SlabKey) -> bool {
//         self.slab.free(key)
//     }

//     pub fn get(&self, key: SlabKey) -> Option<&T> {
//         self.slab.get(key)
//     }

//     pub fn get_pin_mut(&mut self, key: SlabKey) -> Option<Pin<&mut T>> {
//         self.slab.get_pin_mut(key)
//     }
// }

// impl<T: Future> Stream for AsyncSet<T> {
//     type Item = (SlabKey, T::Output);

//     fn poll_next(self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Option<Self::Item>> {
//         let self_ = self.get_mut();

//         for i in self_.current_ready.drain() {
//             let key = SlabKey::from(i);
//             let future = self_.slab.get_pin_mut(key).expect("Invalid ready bit");
//         }
//         todo!()
//     }
// }

// struct IndexWaker {
//     index: usize,
//     state: Arc<Mutex<WakerState>>,
// }

// impl IndexWaker {
//     unsafe fn clone(this: *const ()) -> RawWaker {
//         let this = &*(this as *const Self);

//     }
// }

// static INDEX_WAKER_VTABLE: &RawWakerVTable = &RawWakerVTable::new(
//     Index
// )