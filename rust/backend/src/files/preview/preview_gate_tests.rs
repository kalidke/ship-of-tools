use super::{
    build_preview_payload, fit_plugin_output, is_bounded_output_plugin, read_bytes_preview, try_plugin_preview, words_answer, Unfit,
    MAX_BOUNDED_PICTURE_BYTES, NOT_A_FILE_NOTE, PREVIEW_BYTE_CAP,
};
use std::path::Path;

#[test]
fn every_video_extension_in_any_case_is_bounded() {
    for p in ["a.mp4", "A.MP4", "b.WebM", "c.mov", "d.MKV", "e.m4v"] {
        assert!(is_bounded_output_plugin(Path::new(p)), "{p} should be exempt");
    }
    for p in ["clip.avi", "song.mp3", "x.gif", "noext", "mp4"] {
        assert!(!is_bounded_output_plugin(Path::new(p)), "{p} should be gated");
    }
}

#[test]
fn the_bytes_reader_answers_a_video_with_the_unavailable_note() {
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("clip.MOV");
    std::fs::write(&video, b"\x00\x01\x02").unwrap();
    let (mime, body) = read_bytes_preview(&video, "n1").unwrap();
    assert_eq!(mime, "text/markdown");
    assert!(body.starts_with(b"# video preview unavailable"));
    let other = dir.path().join("clip.avi");
    std::fs::write(&other, b"raw-bytes").unwrap();
    let (_, body) = read_bytes_preview(&other, "n2").unwrap();
    assert_eq!(body, b"raw-bytes");
}

/// Bytes that show where a cut fell.
fn patterned(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// `f`'s answer, or a failure in place of a test that never ends.
#[cfg(unix)]
fn answered<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap_or_else(|_| panic!("{what} did not return"))
}

/// A FIFO with a writer waiting in its open: any reader's open lets the writer through, and its bytes go to that
/// reader. The bytes-level reader answers its note, and the writer's bytes are all there for the reader they were
/// meant for, here this test.
#[cfg(unix)]
#[test]
fn the_bytes_reader_answers_a_fifo_with_a_note_and_never_opens_it() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    let fifo = dir.path().join("pipe.txt");
    let c_path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: a plain mkfifo(3) of a path in this test's own folder.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    let writer = {
        let fifo = fifo.clone();
        std::thread::spawn(move || std::fs::write(fifo, b"meant for another reader"))
    };
    let asked = fifo.clone();
    let (mime, body) = answered("the read of a FIFO", move || read_bytes_preview(&asked, "n")).unwrap();
    assert_eq!((mime.as_str(), body.as_slice()), ("text/markdown", NOT_A_FILE_NOTE.as_bytes()));
    let rest = answered("the other reader", move || std::fs::read(fifo)).unwrap();
    assert_eq!(rest, b"meant for another reader");
    writer.join().unwrap().unwrap();
}

/// Whatever else is no regular file gets the same note: a folder, and on Unix a socket and a device.
#[test]
fn the_bytes_reader_answers_a_folder_a_socket_and_a_device_with_the_note() {
    let dir = tempfile::tempdir().unwrap();
    #[allow(unused_mut)]
    let mut cases = vec![dir.path().join("folder.png")];
    std::fs::create_dir(&cases[0]).unwrap();
    #[cfg(unix)]
    let _listener = {
        let socket = dir.path().join("s.json");
        cases.push(socket.clone());
        cases.push("/dev/null".into());
        std::os::unix::net::UnixListener::bind(socket).unwrap()
    };
    for path in cases {
        let (mime, body) = read_bytes_preview(&path, "n").unwrap_or_else(|e| panic!("{path:?}: {e}"));
        assert_eq!((mime.as_str(), body.as_slice()), ("text/markdown", NOT_A_FILE_NOTE.as_bytes()), "{path:?}");
    }
}

/// The bytes this thread has read from files so far, as the kernel counts them.
#[cfg(target_os = "linux")]
fn thread_bytes_read() -> u64 {
    let io = std::fs::read_to_string("/proc/thread-self/io").expect("the kernel's per-thread I/O account");
    let line = io.lines().find_map(|l| l.strip_prefix("rchar: ")).expect("an rchar line");
    line.parse().unwrap()
}

/// A text file past the cap is answered cut at the cap, and the reader reads the cap and one byte of it, by the
/// kernel's count of this thread's reads, which the two readings of the count themselves raise by under 4096.
#[cfg(target_os = "linux")]
#[test]
fn the_bytes_reader_reads_the_cap_and_one_byte_of_a_longer_text_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("long.txt");
    let whole = patterned(PREVIEW_BYTE_CAP + 1024 * 1024);
    std::fs::write(&path, &whole).unwrap();
    let before = thread_bytes_read();
    let (mime, body) = read_bytes_preview(&path, "n").unwrap();
    let read = thread_bytes_read() - before;
    assert_eq!(mime, "text/plain; charset=utf-8");
    assert!(body == whole[..PREVIEW_BYTE_CAP], "cut at the cap: {} bytes", body.len());
    let most = PREVIEW_BYTE_CAP as u64 + 1;
    assert!((most..most + 4096).contains(&read), "read {read} bytes of a file of {}", whole.len());
}

/// A binary past its cap is refused by its size alone: the file here cannot be opened (Unix), and holds no data.
#[test]
fn the_bytes_reader_refuses_a_binary_past_its_cap_by_its_size_unread() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("huge.png");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(sot_protocol::ir::PREVIEW_BINARY_CAP as u64 + 1024 * 1024 + 512 * 1024).unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();
    let (mime, body) = read_bytes_preview(&path, "n").unwrap();
    assert_eq!(mime, "text/markdown");
    assert_eq!(
        String::from_utf8(body).unwrap(),
        "# preview too large\n\nfile is 513.5 MiB; binary preview cap is 512 MiB.\n\nbinary mimes can't be safely truncated — a partial PNG/JPEG won't decode.\n"
    );
}

/// Every answer for a regular file within the caps, and the cut of a text file past its cap, as before the reader
/// was bounded.
#[test]
fn the_bytes_reader_answers_regular_files_as_it_did() {
    let dir = tempfile::tempdir().unwrap();
    let text = "text/plain; charset=utf-8";
    let cases: [(&str, &str, usize, usize); 9] = [
        ("empty.txt", text, 0, 0),
        ("notes.md", "text/markdown", 300, 300),
        ("noext", text, 300, 300),
        ("at.txt", text, PREVIEW_BYTE_CAP, PREVIEW_BYTE_CAP),
        ("over.rs", "text/x-rust", PREVIEW_BYTE_CAP + 1, PREVIEW_BYTE_CAP),
        ("far-over.toml", "text/x-toml", PREVIEW_BYTE_CAP + 70_000, PREVIEW_BYTE_CAP),
        ("empty.png", "image/png", 0, 0),
        ("pic.png", "image/png", 300, 300),
        ("past-the-text-cap.json", "application/json", PREVIEW_BYTE_CAP + 70_000, PREVIEW_BYTE_CAP + 70_000),
    ];
    for (name, mime, len, kept) in cases {
        let path = dir.path().join(name);
        let whole = patterned(len);
        std::fs::write(&path, &whole).unwrap();
        let (got_mime, body) = read_bytes_preview(&path, "n").unwrap();
        assert_eq!(got_mime, mime, "{name}");
        assert!(body == whole[..kept], "{name}: {} bytes answered", body.len());
        // A link to the file answers as the file does: the reader follows links, as the read resolver does.
        #[cfg(unix)]
        {
            let link = dir.path().join(format!("link-{name}"));
            std::os::unix::fs::symlink(&path, &link).unwrap();
            let (link_mime, link_body) = read_bytes_preview(&link, "n").unwrap();
            assert!(link_mime == mime && link_body == body, "{name} through a link");
        }
    }
    let missing = read_bytes_preview(&dir.path().join("absent.txt"), "n").unwrap_err();
    assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn bounded_output_plugins_exempt_from_size_gate() {
    // HDF5 + video are metadata/poster-only → bounded output → must NOT be
    // skipped on big input (the multi-GB .h5 freeze bug).
    for p in [
        "data.h5",
        "scan.hdf5",
        "old.hdf",
        "DATA.H5",
        "clip.mp4",
        "v.mkv",
    ] {
        assert!(
            is_bounded_output_plugin(Path::new(p)),
            "{p} should be exempt"
        );
    }
    // Plugins whose output scales with input (or plain files) stay gated.
    for p in ["mod.jl", "notes.txt", "data.json", "big.csv", "noext"] {
        assert!(
            !is_bounded_output_plugin(Path::new(p)),
            "{p} should be gated"
        );
    }
}

/// A PDF page rasterized for a zoomed pane, or a video's poster, past the text cap ships whole: the bytes-level reader
/// would answer with the file's own bytes (a PDF's as `text/plain`, which the window only summarizes). Text is still cut
/// at the cap, token JSON past it still goes to the bytes-level reader, a bounded plugin's output that is no picture
/// goes there too, and a picture past the bound is refused, not sent there.
#[test]
fn a_bounded_plugins_picture_past_the_text_cap_ships_whole() {
    let picture = vec![0x89; PREVIEW_BYTE_CAP + 1];
    for path in ["paper.pdf", "Paper.PDF", "clip.mp4"] {
        let shipped = fit_plugin_output(Path::new(path), "n", "image/png", picture.clone());
        assert!(shipped.as_deref() == Ok(&picture[..]), "{path}: shipped {:?} bytes", shipped.map(|b| b.len()));
    }
    assert_eq!(fit_plugin_output(Path::new("mod.jl"), "n", "application/vnd.sot.tokens+json", picture.clone()), Err(Unfit::Reader));
    let tokens = fit_plugin_output(Path::new("paper.pdf"), "n", "application/vnd.sot.tokens+json", picture.clone());
    assert_eq!(tokens, Err(Unfit::Reader), "a bounded plugin's output that is no picture goes to the reader");
    let text = fit_plugin_output(Path::new("data.h5"), "n", "text/markdown", picture.clone()).expect("text is cut, not dropped");
    assert_eq!(text.len(), PREVIEW_BYTE_CAP);
    let bound = vec![0x89; MAX_BOUNDED_PICTURE_BYTES];
    assert!(fit_plugin_output(Path::new("paper.pdf"), "n", "image/png", bound).is_ok(), "exactly the bound ships");
    let huge = vec![0x89; MAX_BOUNDED_PICTURE_BYTES + 1];
    assert_eq!(
        fit_plugin_output(Path::new("paper.pdf"), "n", "image/png", huge),
        Err(Unfit::TooLarge(MAX_BOUNDED_PICTURE_BYTES + 1)),
        "past the bound is refused, not sent to the reader"
    );
}

/// A stand-in kernel that answers its first line as a kernel does and its second, the `file.preview` request, with `reply`
/// as the payload, after writing that request's line to `<dir>/request`.
#[cfg(unix)]
fn stub_kernel(dir: &Path, reply: serde_json::Value) -> crate::sidecars::kernel::Kernel {
    let replied = dir.join("reply");
    let line = serde_json::json!({"id": 2, "payload": reply});
    std::fs::write(&replied, format!("{line}\n")).unwrap();
    let stub = dir.join("stub-julia");
    let script = format!(
        "#!/bin/sh\nread l\necho '{{\"id\":1,\"payload\":{{\"protocol\":1}}}}'\nread l\nprintf '%s\\n' \"$l\" > {}\ncat {}\nexec sleep 30\n",
        dir.join("request").display(),
        replied.display()
    );
    sot_platform::test_exec::write_executable(&stub, script);
    let project = dir.join("kp");
    std::fs::create_dir(&project).unwrap();
    crate::sidecars::kernel::tests::STUB_BIN.lock().unwrap().push((project.clone(), stub.to_string_lossy().into_owned()));
    crate::sidecars::kernel::Kernel::new(project, dir.to_path_buf())
}

/// A stand-in kernel's reply that matched a plugin and answered `mime` with `blob`.
#[cfg(unix)]
fn matched(mime: &str, blob: &[u8]) -> serde_json::Value {
    use base64::Engine as _;
    serde_json::json!({"matched": true, "mime": mime, "blob_base64": base64::engine::general_purpose::STANDARD.encode(blob)})
}

/// `try_plugin_preview`'s answer for a stand-in kernel that replies to `file.preview` with a picture of `len` bytes.
#[cfg(unix)]
async fn plugin_answer(path: &str, len: usize) -> Result<Option<(String, Vec<u8>, Option<serde_json::Value>)>, (String, String)> {
    let dir = tempfile::tempdir().unwrap();
    let kernel = stub_kernel(dir.path(), matched("image/png", &vec![0x89; len]));
    try_plugin_preview(&kernel, Path::new(path), "n", None, None, None, false).await
}

/// The call site hands `fit_plugin_output` the path it was asked about: a PDF page past the text cap, through a
/// stand-in kernel, ships whole.
#[cfg(unix)]
#[tokio::test]
async fn a_pdf_page_past_the_text_cap_ships_whole_through_the_kernel_call() {
    let (mime, bytes, _) = plugin_answer("paper.pdf", PREVIEW_BYTE_CAP + 1).await.expect("shipped").expect("matched");
    assert_eq!((mime.as_str(), bytes.len()), ("image/png", PREVIEW_BYTE_CAP + 1));
}

/// A bounded plugin's picture past 64 MiB is an error the window shows on its status line, not the file's own bytes.
#[cfg(unix)]
#[tokio::test]
async fn a_bounded_picture_past_the_bound_is_refused_through_the_kernel_call() {
    let (code, message) = plugin_answer("paper.pdf", MAX_BOUNDED_PICTURE_BYTES + 1).await.expect_err("refused");
    assert_eq!(code, "preview_too_large");
    assert_eq!(message, "this picture is 64.0 MiB; the preview shows at most 64 MiB");
}

#[tokio::test]
async fn microscope_header_reaches_preview_with_scale_through_a_file_and_a_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let row = dir.path().join("row");
    std::fs::create_dir(&row).unwrap();
    let path = row.join("microscope.png");
    let mut png = include_bytes!("fixtures-header.bin").to_vec();
    let append_chunk = |png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]| {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        png.extend_from_slice(kind);
        png.extend_from_slice(data);
        let mut crc = crc32fast::Hasher::new();
        crc.update(kind);
        crc.update(data);
        png.extend_from_slice(&crc.finalize().to_be_bytes());
    };
    // Match the observed file size: above the plugin input gate and below the backend downsample trigger.
    // Pixel data is immaterial to this bytes/metadata path; each appended chunk has its real CRC.
    let data = vec![0; 11_903_190 - png.len() - 24];
    append_chunk(&mut png, b"IDAT", &data);
    append_chunk(&mut png, b"IEND", &[]);
    std::fs::write(&path, &png).unwrap();
    let workspace = |root| crate::rows::Workspace::meta_only(
        "ws-preview".into(), "preview".into(), "Preview".into(), root,
        "preview".into(), 0, false, "none".into(), String::new(), String::new(),
    );
    let direct = workspace(row);
    let mut cases = vec![(direct, "files:microscope.png")];
    #[cfg(unix)]
    {
        // A second row reads the very same file through a link outside its root, as with external storage.
        let linked_row = dir.path().join("linked-row");
        std::fs::create_dir(&linked_row).unwrap();
        std::os::unix::fs::symlink(&path, linked_row.join("microscope.png")).unwrap();
        cases.push((workspace(linked_row), "files:microscope.png"));
    }
    for (ws, node_id) in cases.drain(..) {
        let req = sot_protocol::PreviewGetReq {
            node_id: node_id.into(), workspace_id: None, page: None, fit_w: None, fit_h: None, words: false,
        };
        let (mime, bytes, extras) = build_preview_payload(&ws, &req).await.unwrap().unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(bytes, png);
        let extras = extras.expect("the preview reply carries the embedded physical scale");
        let scale = sot_protocol::physical_scale::parse_physical_scale(&extras["physical_scale"]).unwrap();
        assert_eq!(scale.unit, "nm");
        assert_eq!(scale.axes.len(), 2);
        for (axis, name) in scale.axes.iter().zip(["x", "y"]) {
            assert_eq!(axis.name, name);
            assert!((axis.nm_per_px - 4.7785).abs() < 1e-4, "{axis:?}");
        }
    }
}

/// The one line the stand-in kernel read as `file.preview`: its payload.
#[cfg(unix)]
fn requested(dir: &Path) -> serde_json::Value {
    let line = std::fs::read_to_string(dir.join("request")).expect("the kernel was asked");
    let frame: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(frame["op"], "file.preview", "{frame}");
    frame["payload"].clone()
}

/// A words request carries `params.words` to the kernel's plugin beside the page; a page request carries no such key.
#[cfg(unix)]
#[tokio::test]
async fn a_words_request_reaches_the_plugin_with_the_parameter_and_a_page_request_without_it() {
    for (words, want) in [(true, serde_json::json!({"page": 3, "words": true})), (false, serde_json::json!({"page": 3}))] {
        let dir = tempfile::tempdir().unwrap();
        let kernel = stub_kernel(dir.path(), matched("image/png", b"png"));
        try_plugin_preview(&kernel, Path::new("paper.pdf"), "n", Some(3), None, None, words).await.expect("answered");
        assert_eq!(requested(dir.path())["params"], want, "words {words}");
    }
    // The same request through the words path: asked for words, with no page, the parameter alone.
    let dir = tempfile::tempdir().unwrap();
    let kernel = stub_kernel(dir.path(), matched(sot_protocol::ops::PAGE_WORDS_MIME, br#"{"page":1,"cut":false,"lines":[]}"#));
    let req = sot_protocol::PreviewGetReq { node_id: "n".into(), workspace_id: None, page: None, fit_w: None, fit_h: None, words: true };
    words_answer(&kernel, Path::new("paper.pdf"), &req).await.expect("words");
    assert_eq!(requested(dir.path())["params"], serde_json::json!({"words": true}));
}

/// A words request is answered with the plugin's words or refused `no_words`: not with a picture (a plugin that ignored
/// the parameter), not with a note, not with the file (the bytes-level reader's answer to no plugin and to output past
/// the cap), and the words as the plugin gave them when it did.
#[cfg(unix)]
#[tokio::test]
async fn a_words_request_is_answered_with_words_or_refused_never_with_a_picture_or_the_file() {
    let words = br#"{"page":1,"cut":false,"lines":[[[0,0,9,9,"a"]]]}"#;
    let past_the_cap = vec![b' '; PREVIEW_BYTE_CAP + 1];
    let cases: [(&str, serde_json::Value, bool); 7] = [
        ("words", matched(sot_protocol::ops::PAGE_WORDS_MIME, words), true),
        ("a picture", matched("image/png", b"\x89PNG"), false),
        ("a note", matched("text/markdown", b"# pdf\n\nno pdftotext\n"), false),
        ("words past the cap", matched(sot_protocol::ops::PAGE_WORDS_MIME, &past_the_cap), false),
        ("no plugin", serde_json::json!({"matched": false}), false),
        ("no blob", serde_json::json!({"matched": true, "mime": sot_protocol::ops::PAGE_WORDS_MIME}), false),
        ("a bad blob", serde_json::json!({"matched": true, "mime": sot_protocol::ops::PAGE_WORDS_MIME, "blob_base64": "!"}), false),
    ];
    for (name, reply, served) in cases {
        let dir = tempfile::tempdir().unwrap();
        let kernel = stub_kernel(dir.path(), reply);
        let req = sot_protocol::PreviewGetReq { node_id: "n".into(), workspace_id: None, page: Some(1), fit_w: None, fit_h: None, words: true };
        let answer = words_answer(&kernel, Path::new("paper.pdf"), &req).await;
        match (served, answer) {
            (true, Ok((mime, bytes, _))) => assert_eq!((mime.as_str(), bytes.as_slice()), (sot_protocol::ops::PAGE_WORDS_MIME, &words[..]), "{name}"),
            (false, Err((code, _))) => assert_eq!(code, "no_words", "{name}"),
            (_, other) => panic!("{name}: {:?}", other.map(|(m, b, _)| (m, b.len()))),
        }
    }
}

/// Through the whole assembly: a words request for a folder and for a file no plugin claims is `no_words`
/// (never the folder's page or the file's bytes), and the same file asked as a page is its bytes.
#[tokio::test]
async fn a_words_request_through_the_assembly_never_reaches_the_pages_or_the_bytes_reader() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("row");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(root.join("folder")).unwrap();
    std::fs::write(root.join("notes.txt"), b"plain bytes").unwrap();
    let ws = crate::rows::Workspace::meta_only(
        "ws-words".into(), "words".into(), "Words".into(), root,
        "words".into(), 0, false, "none".into(), String::new(), String::new(),
    );
    let ask = |node: &str, words| sot_protocol::PreviewGetReq { node_id: node.into(), workspace_id: None, page: None, fit_w: None, fit_h: None, words };
    let page = build_preview_payload(&ws, &ask("files:notes.txt", false)).await.unwrap().expect("a page request");
    assert_eq!(page.1, b"plain bytes");
    for node in ["files:folder", "files:notes.txt"] {
        let refused = build_preview_payload(&ws, &ask(node, true)).await.unwrap().expect_err(node);
        assert_eq!(refused.0, "no_words", "{node}: {refused:?}");
    }
    // A PDF the dead kernel cannot read is that error, as a page's is: an answer that is neither words nor the file.
    assert!(build_preview_payload(&ws, &ask("files:absent.pdf", true)).await.unwrap().is_err());
}
