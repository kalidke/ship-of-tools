//! D2: a run keeps no append-only record of the terminal.

use super::*;

/// Every file under `dir`, recursively, as paths relative to it.
fn files_under(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.map(|e| e.unwrap()) {
            let path = e.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                out.push(path.strip_prefix(root).unwrap().to_path_buf());
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out
}

/// A run that prints and exits leaves no segment, no record folder and no blob in its state folder: the only things
/// there are the run's own small files.
#[test]
fn a_run_writes_no_voyage_record() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = shell_command("echo SOT_NO_RECORD_5b1c");
    let cfg = config(dir.path(), "norec1", argv, 80, 25);
    let root = cfg.state_dir.clone();
    let (_tx, rx) = mpsc::channel();
    let summary = capsule::run::<P>(cfg, rx).unwrap();
    assert_eq!(summary.exit_kind, ExitKind::ProducerExited);

    let files = files_under(dir.path());
    let record: Vec<_> = files
        .iter()
        .filter(|f| {
            let s = f.to_string_lossy();
            s.ends_with(".sotseg") || s.contains("seg/") || s.contains("seg\\") || s.contains("blobs")
        })
        .collect();
    assert!(record.is_empty(), "a run left a voyage record under {}: {record:?}", root.display());
}
