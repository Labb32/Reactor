use std::sync::{
    Arc,
    OnceLock,
    atomic::{AtomicBool, Ordering},
};

use crate::error::{Result, io_error};

static INTERRUPTED: OnceLock<Arc<AtomicBool>> = OnceLock::new();

pub fn requested() -> bool {
    INTERRUPTED.get().is_some_and(|signal| signal.load(Ordering::Acquire))
}

pub fn install(context: &str) -> Result<Arc<AtomicBool>> {
    let interrupted = Arc::new(AtomicBool::new(false));
    let signal = interrupted.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Release))
        .map_err(|source| io_error(context, std::io::Error::other(source)))?;
    let _ = INTERRUPTED.set(interrupted.clone());
    Ok(interrupted)
}
