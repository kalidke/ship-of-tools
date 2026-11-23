Part of files: see ../CLAUDE.md.

A plugin's output past `PREVIEW_BYTE_CAP` (2 MiB) is cut at it when its mime is text, and goes to the bytes-level reader
when it is any other mime (token JSON grows with its file). The exception is a picture (an `image/` mime) from a
bounded-output plugin (`is_bounded_output_plugin`: PDF, video, HDF5): its size is the plugin's (a PDF page rasterized for
a zoomed pane, a video's poster), not its input's, and the reader would answer with the file's own bytes, so it ships
whole up to `MAX_BOUNDED_PICTURE_BYTES` (4096 x 4096 x 4 bytes, 64 MiB, a chosen bound on what one kernel line may make
the daemon hold). A bounded plugin's picture past it is refused, never sent to the reader: `try_plugin_preview` answers
`Err("preview_too_large", ...)` and the window keeps its pane and says so on the status line. A bounded plugin's other
non-text output goes to the reader as any other does, and a fallback is named by a WARN with the size and mime.
Behavior tests: `preview_gate_tests::a_bounded_plugins_picture_past_the_text_cap_ships_whole`
(`fit_plugin_output`) and the two through `try_plugin_preview` over a stand-in kernel beside it.

A request with `words` true (`PreviewGetReq::words`, wire's guarantee 17) asks for a page's words, not its picture.
`build_preview_payload` answers it before the folder page, the plugin's picture, the bytes-level reader and the scale
sidecar are reached, through `words_answer`: a folder is `no_words` with no request to the kernel; otherwise the plugin
is asked for the page with `params.words` and nothing else (a request without `words` carries no such key, and a fit is
never forwarded), and only an answer under `PAGE_WORDS_MIME` is shipped, as the plugin gave it. Every other outcome is
the error `no_words`: a folder, a file no plugin claims, a plugin's note, the picture of a plugin that ignored the
parameter (one past `MAX_BOUNDED_PICTURE_BYTES` too, not `preview_too_large`), and an answer with no blob, one that is
not base64 or one past `PREVIEW_BYTE_CAP` (each the reply with which `try_plugin_preview` sends any other request to the
reader). The one other error is a kernel that cannot be asked: `kernel_unavailable` for each type with no bytes-level
answer (PDF, HDF5 and video, `is_bounded_output_plugin`), as a page's is, and `no_words` for any other file. The reader
would answer a words request with the file's own bytes, which a window would read as no words at best, so it is never
reached. When the PDF plugin answered a note (no `pdftotext`, a tool that failed or ran out of time, a page past the
end), its fixed words are logged at info with the node, never the reply's: the window is told `no_words` alone, and the
log tells a missing tool from a bad PDF (`plugin_note`; a PDF is told by the daemon's one rule for it, `is_pdf_path`, the
plugin's own: the path, lowercased, ends in `.pdf`, so a file named exactly `.pdf` is one, and the drawing's page read
asks the same function). The words document is the plugin's and is not decoded here; the
window decodes it with wire's reader. The hello says the daemon does this (`serves_page_words`, server's contract).
Tests, in `preview_gate_tests`: `a_words_request_reaches_the_plugin_with_the_parameter_and_a_page_request_without_it` (a
stand-in kernel records the request it is sent),
`a_words_request_is_answered_with_words_or_refused_never_with_a_picture_or_the_file` (eight plugin answers),
`a_words_request_for_a_folder_is_no_words` (every platform),
`a_words_request_through_the_assembly_with_no_kernel_is_one_refusal_by_its_code` (a kernel of the test's own, whose
program exits at once: a folder and a text file `no_words`, a PDF, an HDF5 file and a video `kernel_unavailable`),
`a_words_request_through_the_assembly_is_answered_with_the_plugins_words` (a kernel of the test's own answers the words
mime: the words ship as given, the kernel is asked the page and `words` alone, and for a folder not at all),
`a_plugins_note_is_logged_with_its_node_and_its_reason_through_words_answer` (an event captured through `words_answer` with platform's `test_log::capture`, level and words,
for `a.pdf`, a file named `.pdf` and `A.PDF`; another file's answer and an answer of words log no reason) and
`the_pdf_plugins_note_is_logged_as_its_fixed_words_and_nothing_else_is` (`plugin_note` alone). The assembly tests give
the workspace a kernel of its own (`Workspace::set_kernel`) and register nothing for the default kernel project, which
another test's workspace could pick up; a kernel stand-in is a shell script, so every test but the folder's runs on Unix.

The bytes-level reader (`read_bytes_preview`) answers what no plugin answered. It looks the path up before it opens
anything, following links as the read resolver does (`node_id_to_path`) and nothing more, and opens only a regular
file. A folder, a FIFO, a socket or a device is answered a short markdown note (`NOT_A_FILE_NOTE`) and is never opened,
so a FIFO's writer is not waited for and keeps its bytes for the reader they were meant for. A file whose mime is not
`text/` and whose size at the lookup is past `PREVIEW_BINARY_CAP` (512 MiB) is refused with the note that names that
size, unopened. Any other regular file is opened through platform's `open_regular` (guarantee 15) and read to its cap
and one byte more, never whole: a text file past `PREVIEW_BYTE_CAP` (2 MiB) is cut at it, and every answer for a
regular file within the caps is the file's bytes under the mime of its name. A missing path is `io_error`. A video is
answered its own note before any lookup, and `preview.get` answers a folder with its folder page before the reader is
reached. Limits: a path replaced between the lookup and the open is judged again on the open handle, so a FIFO planted
in that instant is opened without a wait and refused as `io_error`; a binary that grows past its cap after the lookup
is read to the cap and one byte, 512 MiB held, and refused with that size; a regular file on a hung network mount
still blocks in the OS. On Windows the lookup and the handle check are std's: a folder is answered the note, and a
reserved device name or a named pipe's name is not promised refused, since no test runs one there. Tests, in
`preview_gate_tests`: `the_bytes_reader_answers_a_fifo_with_a_note_and_never_opens_it` (Unix: a writer waits in the
FIFO's open and its bytes are all there afterward), `the_bytes_reader_answers_a_folder_a_socket_and_a_device_with_the_note`
(the socket and `/dev/null` on Unix), `the_bytes_reader_reads_the_cap_and_one_byte_of_a_longer_text_file` (Linux: by
the kernel's count of the bytes the thread read), `the_bytes_reader_refuses_a_binary_past_its_cap_by_its_size_unread`
(the file cannot be opened on Unix; on Windows the test proves the words alone) and
`the_bytes_reader_answers_regular_files_as_it_did` (a table of names and sizes on both sides of the text cap, each
through a link too on Unix).

PNG pHYs supplies a physical scale only for metre-unit densities of at least 100,000 pixels per metre on both axes;
lower values are print density regardless of other chunks. A valid sidecar still takes priority, generated plugin
bytes are excluded, and anisotropic pHYs is suppressed. Behavior test: `scale::phys_scale_tests::
print_density_is_not_a_physical_scale_even_with_writer_metadata` in the existing backend unit suite.

`preview_gate_tests::microscope_header_reaches_preview_with_scale_through_a_file_and_a_symlink` runs
`build_preview_payload` (the `preview.get` assembly) on the captured 5120 × 5120 RGB header in
`fixtures-header.bin`, completed with CRC-correct IDAT and IEND chunks at the observed 11,903,190-byte file size.
It proves both axes reach `extras.physical_scale` as 4.7785 nm per pixel (within 1e-4), through the bytes reader,
sidecar tier and embedded-scale merge, for the file and, on Unix, a second row's symbolic link outside its root.
Windows runs the direct-file case without creating a link. The image is below the backend downsample trigger;
the window retains the served dimensions separately from a texture shrunk to its GPU's limit for the scale bar.
