use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::error::{Result, io_error};

pub fn install(context: &str) -> Result<Arc<AtomicBool>> {
    let interrupted = Arc::new(AtomicBool::new(false));
    let signal = interrupted.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Release))
        .map_err(|source| io_error(context, std::io::Error::other(source)))?;
    Ok(interrupted)
}
