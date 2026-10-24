use super::*;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A terminal that takes nothing until released, as an agent that reads no input yet.
struct Gated {
    open: Arc<(Mutex<bool>, std::sync::Condvar)>,
    got: Arc<Mutex<Vec<u8>>>,
}

impl Write for Gated {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let (lock, cv) = &*self.open;
        let mut open = lock.lock().unwrap();
        while !*open {
            open = cv.wait(open).unwrap();
        }
        self.got.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_write_the_terminal_does_not_take_never_blocks_the_caller() {
    let open = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let got = Arc::new(Mutex::new(Vec::new()));
    let pump = InputPump::new(Gated { open: Arc::clone(&open), got: Arc::clone(&got) }).unwrap();
    let (reply, answered) = mpsc::sync_channel(1);
    let started = std::time::Instant::now();
    pump.write(b"paste".to_vec(), reply);
    assert!(started.elapsed() < Duration::from_millis(500), "handing the bytes over does not wait for the terminal");
    assert!(answered.recv_timeout(Duration::from_millis(100)).is_err(), "no answer while the terminal takes nothing");
    *open.0.lock().unwrap() = true;
    open.1.notify_all();
    assert_eq!(answered.recv_timeout(Duration::from_secs(5)).unwrap(), Ok(5));
    assert_eq!(*got.lock().unwrap(), b"paste");
}

#[test]
fn writes_arrive_in_the_order_they_were_handed_over_and_a_full_pump_says_busy() {
    let open = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let got = Arc::new(Mutex::new(Vec::new()));
    let pump = InputPump::new(Gated { open: Arc::clone(&open), got: Arc::clone(&got) }).unwrap();
    let mut answers = Vec::new();
    // The first write is taken by the pump's thread, which blocks in it; the next ones fill the queue behind it.
    for i in 0..(PENDING_WRITES as u8 + 3) {
        let (reply, answered) = mpsc::sync_channel(1);
        pump.write(vec![b'a' + i], reply);
        answers.push(answered);
        std::thread::sleep(Duration::from_millis(5));
    }
    let busy = answers.iter().filter(|a| matches!(a.try_recv(), Ok(Err(m)) if m.starts_with("busy"))).count();
    assert!(busy >= 1, "past the bound a write is refused as busy at once");
    *open.0.lock().unwrap() = true;
    open.1.notify_all();
    for a in &answers {
        let _ = a.recv_timeout(Duration::from_secs(5));
    }
    let got = got.lock().unwrap().clone();
    assert!(got.windows(2).all(|w| w[0] < w[1]), "in the order handed over: {got:?}");
}

#[test]
fn retiring_answers_the_pending_writes_run_ended_and_ends_the_thread() {
    let open = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let got = Arc::new(Mutex::new(Vec::new()));
    let pump = InputPump::new(Gated { open: Arc::clone(&open), got: Arc::clone(&got) }).unwrap();
    let (first, first_answer) = mpsc::sync_channel(1);
    let (second, second_answer) = mpsc::sync_channel(1);
    pump.write(b"one".to_vec(), first);
    std::thread::sleep(Duration::from_millis(50));
    pump.write(b"two".to_vec(), second);
    // The terminal takes nothing until the run's end hangs it up: the blocked write fails, as a closed terminal's does.
    let release = {
        let open = Arc::clone(&open);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            *open.0.lock().unwrap() = true;
            open.1.notify_all();
        })
    };
    assert!(pump.retire(Duration::from_secs(5)), "the input thread ends within its bound");
    release.join().unwrap();
    let answer = second_answer.recv_timeout(Duration::from_secs(1)).expect("a pending write is answered at the end");
    assert!(matches!(&answer, Err(m) if m.starts_with("run ended")), "{answer:?}");
    let _ = first_answer.recv_timeout(Duration::from_secs(1));
    assert!(!got.lock().unwrap().ends_with(b"two"), "a write that was pending at the end is never written");
}

#[test]
fn a_detached_write_goes_through_the_same_queue_in_order() {
    let open = Arc::new((Mutex::new(true), std::sync::Condvar::new()));
    let got = Arc::new(Mutex::new(Vec::new()));
    let pump = InputPump::new(Gated { open, got: Arc::clone(&got) }).unwrap();
    let (reply, answered) = mpsc::sync_channel(1);
    pump.write(b"typed ".to_vec(), reply);
    pump.write_detached(b"da1".to_vec());
    assert_eq!(answered.recv_timeout(Duration::from_secs(5)).unwrap(), Ok(6));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while got.lock().unwrap().len() < 9 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(*got.lock().unwrap(), b"typed da1");
}
