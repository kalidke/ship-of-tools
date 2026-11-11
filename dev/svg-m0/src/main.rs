//! Throwaway: the fixed cost of one SVG child. `empty` and `fonts` are the children;
//! `bench <label> <runs> <mem-mib> <program> [args]` times fresh spawns from a parent
//! that holds `mem-mib` of touched memory.
use std::process::{Command, Stdio};
use std::time::Instant;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(String::as_str) {
        Some("empty") => {}
        Some("fonts") => {
            let mut db = fontdb::Database::new();
            db.load_system_fonts();
            println!("faces {}", db.len());
        }
        Some("bench") => {
            let label = &a[2];
            let runs: usize = a[3].parse().unwrap();
            let mib: usize = a[4].parse().unwrap();
            let mut ballast = vec![0u8; mib << 20];
            for i in (0..ballast.len()).step_by(4096) {
                ballast[i] = 1;
            }
            let mut ms = Vec::new();
            let mut note = String::new();
            for _ in 0..runs {
                let t = Instant::now();
                let out = Command::new(&a[5])
                    .args(&a[6..])
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .output()
                    .expect("spawn");
                ms.push(t.elapsed().as_secs_f64() * 1e3);
                if note.is_empty() {
                    note = String::from_utf8_lossy(&out.stdout).trim().to_string();
                }
            }
            let first = ms[0];
            let mut s = ms.clone();
            s.sort_by(|x, y| x.partial_cmp(y).unwrap());
            println!(
                "{label}: median {:.1} ms, range {:.1} to {:.1}, first {:.1}, parent {} MiB touched [{}]",
                s[s.len() / 2], s[0], s[s.len() - 1], first, mib + 0 * ballast.len(), note
            );
        }
        _ => eprintln!("usage"),
    }
}
