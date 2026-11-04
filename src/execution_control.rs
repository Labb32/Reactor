use std::{cell::RefCell, io, sync::{Arc, atomic::{AtomicBool, Ordering}}, time::{Duration, Instant}};

struct Budget {
    deadline: Option<Instant>,
    lost: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
}

thread_local! { static BUDGET: RefCell<Option<Budget>> = const { RefCell::new(None) }; }

pub struct Guard(Option<Budget>);

impl Guard {
    pub fn enter(duration: Duration, lost: Arc<AtomicBool>, cancelled: Arc<AtomicBool>) -> Self {
        Self(BUDGET.with(|slot| slot.replace(Some(Budget {
            deadline: Some(Instant::now() + duration), lost, cancelled,
        }))))
    }
}

pub fn set_timeout(timeout: Option<Duration>) {
    BUDGET.with(|slot| {
        if let Some(budget) = slot.borrow_mut().as_mut() {
            budget.deadline = timeout.map(|value| Instant::now() + value);
        }
    });
}

impl Drop for Guard {
    fn drop(&mut self) {
        BUDGET.with(|slot| { slot.replace(self.0.take()); });
    }
}

pub fn check() -> io::Result<()> {
    if crate::interrupt::requested() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "runner interrupted"));
    }
    BUDGET.with(|slot| {
        if let Some(budget) = slot.borrow().as_ref() {
            if budget.lost.load(Ordering::Acquire) || budget.cancelled.load(Ordering::Acquire) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "execution stopped"));
            }
            if budget.deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "execution stage timed out"));
            }
        }
        Ok(())
    })
}
