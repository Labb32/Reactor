use std::{cell::RefCell, io, sync::{Arc, atomic::{AtomicBool, Ordering}}, time::{Duration, Instant}};

struct Budget {
    deadline: Option<Instant>,
    lost: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    ignore_interrupt: bool,
}

thread_local! { static BUDGET: RefCell<Option<Budget>> = const { RefCell::new(None) }; }

pub struct Guard(Option<Budget>);

impl Guard {
    pub fn enter(duration: Duration, lost: Arc<AtomicBool>, cancelled: Arc<AtomicBool>) -> Self {
        Self(BUDGET.with(|slot| slot.replace(Some(Budget {
            deadline: Some(Instant::now() + duration), lost, cancelled, ignore_interrupt: false,
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

pub fn collect_result() {
    BUDGET.with(|slot| {
        if let Some(budget) = slot.borrow_mut().as_mut() {
            budget.deadline = Some(Instant::now() + Duration::from_secs(60));
            budget.cancelled = Arc::new(AtomicBool::new(false));
            budget.ignore_interrupt = true;
        }
    });
}

impl Drop for Guard {
    fn drop(&mut self) {
        BUDGET.with(|slot| { slot.replace(self.0.take()); });
    }
}

pub fn check() -> io::Result<()> {
    let ignore_interrupt = BUDGET.with(|slot| slot.borrow().as_ref().is_some_and(|budget| budget.ignore_interrupt));
    if !ignore_interrupt && crate::interrupt::requested() {
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
