//! Locks and one-time initialisation for kernel globals.
//!
//! `IrqSpinLock` disables interrupts while held, so an interrupt handler on
//! the same CPU can never spin forever on a lock its own CPU holds. It
//! restores the previous interrupt state on release, so nested locks work.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use crate::arch::cpu;

pub struct IrqSpinLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: the lock gives exclusive access to `value`, so sharing the lock
// between CPUs is sound whenever T can be sent between them.
unsafe impl<T: Send> Sync for IrqSpinLock<T> {}

impl<T> IrqSpinLock<T> {
    pub const fn new(value: T) -> Self {
        IrqSpinLock {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    pub fn lock(&self) -> IrqSpinGuard<'_, T> {
        let were_enabled = cpu::interrupts_disable();
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while self.locked.load(Ordering::Relaxed) {
                core::hint::spin_loop();
            }
        }
        IrqSpinGuard {
            lock: self,
            were_enabled,
        }
    }

    /// Whether someone holds the lock right now (for diagnostics only).
    pub fn is_locked(&self) -> bool {
        self.locked.load(Ordering::Relaxed)
    }
}

pub struct IrqSpinGuard<'a, T> {
    lock: &'a IrqSpinLock<T>,
    were_enabled: bool,
}

impl<T> Deref for IrqSpinGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: holding the guard means holding the lock.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for IrqSpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: holding the guard means holding the lock, exclusively.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for IrqSpinGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        if self.were_enabled {
            cpu::interrupts_enable();
        }
    }
}

const UNINIT: u8 = 0;
const RUNNING: u8 = 1;
const DONE: u8 = 2;

/// A value set exactly once, then read-only forever.
pub struct Once<T> {
    state: AtomicU8,
    value: UnsafeCell<core::mem::MaybeUninit<T>>,
}

// SAFETY: after `set` the value is never mutated again, so shared reads from
// any CPU are sound when T is Sync; `set` hands T over, so T must be Send.
unsafe impl<T: Send + Sync> Sync for Once<T> {}

impl<T> Once<T> {
    pub const fn new() -> Self {
        Once {
            state: AtomicU8::new(UNINIT),
            value: UnsafeCell::new(core::mem::MaybeUninit::uninit()),
        }
    }

    /// Store the value. Panics if it was already set (or is being set).
    pub fn set(&self, value: T) -> &T {
        assert!(
            self.state
                .compare_exchange(UNINIT, RUNNING, Ordering::Acquire, Ordering::Relaxed)
                .is_ok(),
            "Once::set called twice"
        );
        // SAFETY: the state machine makes us the only writer, and nobody
        // reads before DONE.
        unsafe { (*self.value.get()).write(value) };
        self.state.store(DONE, Ordering::Release);
        self.get().unwrap()
    }

    pub fn get(&self) -> Option<&T> {
        if self.state.load(Ordering::Acquire) == DONE {
            // SAFETY: DONE means the value was written and is never written again.
            Some(unsafe { (*self.value.get()).assume_init_ref() })
        } else {
            None
        }
    }
}
