use std::{
    ptr::null_mut,
    sync::atomic::{AtomicPtr, Ordering},
    task::Waker,
};

#[cfg(feature = "cache-padded")]
use crossbeam_utils::CachePadded;

#[cfg(feature = "cache-padded")]
pub struct WakerQueue {
    head: CachePadded<AtomicPtr<WakerNode>>,
    tail: CachePadded<AtomicPtr<WakerNode>>,
}

#[cfg(not(feature = "cache-padded"))]
pub struct WakerQueue {
    head: AtomicPtr<WakerNode>,
    tail: AtomicPtr<WakerNode>,
}

impl Drop for WakerQueue {
    fn drop(&mut self) {
        let mut head = self.head.swap(null_mut(), Ordering::SeqCst);
        let tail = self.tail.load(Ordering::SeqCst);

        while head != tail {
            if head.is_null() {
                unreachable!("failed to deallocate WakerQueue");
            }

            let tmp = unsafe { Box::from_raw(head) };
            head = tmp.next.swap(null_mut(), Ordering::SeqCst);
            drop(tmp);
        }

        if !head.is_null() {
            unsafe { drop(Box::from_raw(head)) };
        }
    }
}

struct WakerNode {
    next: AtomicPtr<WakerNode>,
    waker: Option<Waker>,
}

impl WakerQueue {
    #[cfg(feature = "cache-padded")]
    /// creates a new cache padded WakerQueue.
    pub const fn new() -> Self {
        WakerQueue {
            head: CachePadded::new(AtomicPtr::new(null_mut::<WakerNode>())),
            tail: CachePadded::new(AtomicPtr::new(null_mut::<WakerNode>())),
        }
    }

    #[cfg(not(feature = "cache-padded"))]
    /// creates a new WakerQueue.
    pub const fn new() -> Self {
        WakerQueue {
            head: AtomicPtr::new(null_mut::<WakerNode>()),
            tail: AtomicPtr::new(null_mut::<WakerNode>()),
        }
    }

    /// appends a waker to the WakerQueue.
    ///
    /// this is thread safe.
    pub fn register(&self, waker: Waker) {
        let node = Box::into_raw(Box::new(WakerNode {
            next: AtomicPtr::new(null_mut()),
            waker: Some(waker),
        }));

        let prev_tail = self.tail.swap(node, Ordering::Relaxed);

        unsafe {
            match prev_tail.as_mut() {
                Some(prev) => prev.next.store(node, Ordering::Relaxed),
                // generally, if tail is null it's implied that head is also null.
                // however, this might not be true if wake_all is happening simultaneously
                // so we need a loop here to fix the race condition.
                //
                // in theory, this is a rare occurence and should not impact performance
                None => loop {
                    if self
                        .head
                        .compare_exchange_weak(
                            null_mut(),
                            node,
                            Ordering::Relaxed,
                            Ordering::Relaxed,
                        )
                        .is_ok()
                    {
                        break;
                    }
                },
            }
        }
    }

    /// wakes all wakers in the WakerQueue and clears it.
    ///
    /// this is thread safe.
    pub fn wake_all(&self) {
        let tail = self
            .tail
            .swap(null_mut::<WakerNode>().into(), Ordering::Relaxed);

        // tail being null implies nothing has been pushed into the queue
        if tail.is_null() {
            return;
        }

        let mut head = self
            .head
            .swap(null_mut::<WakerNode>().into(), Ordering::Relaxed);

        // if tail isn't null we are just waiting for a register
        // to finish setting the head
        while head.is_null() {
            head = self
                .head
                .swap(null_mut::<WakerNode>().into(), Ordering::Relaxed);
        }

        // safety: we know head isn't null from above
        let mut head = unsafe { Box::from_raw(head) };
        head.waker.take().map(|w| w.wake());

        while head.as_ref() as *const _ != tail {
            head = loop {
                let next = head.next.load(Ordering::Relaxed);

                if next.is_null() {
                    std::hint::spin_loop();
                    continue;
                }

                break unsafe { Box::from_raw(next) };
            };

            head.waker.take().map(|w| w.wake());
        }
    }
}
