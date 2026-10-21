use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

pub(crate) static CLOSEOUT_GATE: Mutex<Option<Arc<tokio::sync::Semaphore>>> = Mutex::new(None);
pub(crate) static FAIL_RETIREMENT: AtomicBool = AtomicBool::new(false);
