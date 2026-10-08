//! Windows only: the Application event log's Application Hang (1002) events for one fixture child.
//! An unreadable log is an error, never an empty pass.

use windows_sys::Win32::System::EventLog::{EvtClose, EvtNext, EvtQuery, EvtRender};

const QUERY_CHANNEL_PATH: u32 = 0x1;
const QUERY_FORWARD: u32 = 0x100;
const RENDER_EVENT_XML: u32 = 1;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn render(event: isize) -> anyhow::Result<String> {
    let mut used = 0u32;
    let mut props = 0u32;
    // SAFETY: a null buffer of size 0 asks for the required size; `event` is a live event handle.
    unsafe { EvtRender(0, event, RENDER_EVENT_XML, 0, std::ptr::null_mut(), &mut used, &mut props) };
    anyhow::ensure!(used > 0, "event log: render sized nothing: {}", std::io::Error::last_os_error());
    let mut buffer = vec![0u16; (used as usize).div_ceil(2) + 1];
    // SAFETY: the buffer holds `used` bytes and outlives the call.
    let ok = unsafe { EvtRender(0, event, RENDER_EVENT_XML, used, buffer.as_mut_ptr().cast(), &mut used, &mut props) };
    anyhow::ensure!(ok != 0, "event log: render failed: {}", std::io::Error::last_os_error());
    let chars = (used as usize / 2).saturating_sub(1);
    Ok(String::from_utf16_lossy(&buffer[..chars]))
}

/// Application Hang events raised within the last `within_ms` that name `image` (the executable file name) and the
/// process id `pid` (hex, as the event writes it). Err when the log cannot be queried.
pub(super) fn hang_events_for(image: &str, pid: u32, within_ms: u64) -> anyhow::Result<Vec<String>> {
    let channel = wide("Application");
    let query = wide(&format!(
        "*[System[Provider[@Name='Application Hang'] and (EventID=1002) and TimeCreated[timediff(@SystemTime) <= {within_ms}]]]"
    ));
    // SAFETY: both strings are NUL-terminated and outlive the call.
    let results = unsafe { EvtQuery(0, channel.as_ptr(), query.as_ptr(), QUERY_CHANNEL_PATH | QUERY_FORWARD) };
    anyhow::ensure!(results != 0, "event log: the Application log cannot be queried: {}", std::io::Error::last_os_error());
    let mut found = Vec::new();
    let mut failure = None;
    loop {
        let mut events = [0isize; 16];
        let mut returned = 0u32;
        // SAFETY: `events` has room for 16 handles; `results` is live.
        let ok = unsafe { EvtNext(results, 16, events.as_mut_ptr(), 1000, 0, &mut returned) };
        if ok == 0 {
            // ERROR_NO_MORE_ITEMS (259) ends the walk; anything else is a failure.
            if std::io::Error::last_os_error().raw_os_error() != Some(259) {
                failure = Some(anyhow::anyhow!("event log: reading failed: {}", std::io::Error::last_os_error()));
            }
            break;
        }
        for &event in &events[..returned as usize] {
            match render(event) {
                Ok(xml) => {
                    let lower = xml.to_lowercase();
                    if lower.contains(&image.to_lowercase()) && lower.contains(&format!(">{pid:x}<")) {
                        found.push(xml);
                    }
                }
                Err(e) => failure = Some(e),
            }
            // SAFETY: the handle came from EvtNext and is closed once.
            unsafe { EvtClose(event) };
        }
    }
    // SAFETY: the query handle is closed once.
    unsafe { EvtClose(results) };
    failure.map_or(Ok(found), Err)
}
