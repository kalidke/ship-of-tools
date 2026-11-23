//! The window_minimized target's children and parent: the start its two children share, the State fixtures' own
//! child, and the parent that runs both.

use super::tests::{fixture_cli, FixtureHome, NativeProgressLedger, HOST};
use super::*;
use std::io::Write;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use winit::event_loop::EventLoop;

/// A harness ticker counts pending work before posting its unit-valued event.
#[derive(Clone, Default)]
pub(super) struct HarnessTick(Arc<std::sync::atomic::AtomicUsize>);

impl HarnessTick {
    pub(super) fn post(&self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Release);
    }

    pub(super) fn dispatch<D>(
        &self,
        driver: &mut D,
        forward: impl FnOnce(&mut D),
        work: impl FnOnce(&mut D, usize),
    ) {
        forward(driver);
        let count = self.0.swap(0, std::sync::atomic::Ordering::AcqRel);
        if count > 0 {
            work(driver, count);
        }
    }
}

const FIXTURES_BOUND: Duration = Duration::from_secs(120);
/// The three phases (2 s, 20 s, 600 s), their native transitions, the final drain and the first frames, with room.
const WORKLOAD_BOUND: Duration = Duration::from_secs(780);

/// The start the window_minimized target's two children share: the fixture home, the "not runnable here" panic hook,
/// an event loop and an App on one synthetic host with test-owned startup inputs. The close and pane children build
/// their own.
pub(super) struct NativeChild {
    pub(super) _home: FixtureHome,
    pub(super) event_loop: EventLoop<()>,
    pub(super) app: App,
    pub(super) outgoing: tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>,
    pub(super) inputs: crate::ui::init::NativeStartupInputs,
}

pub(super) fn native_child(ledger: Arc<Mutex<NativeProgressLedger>>) -> Result<NativeChild> {
    let home = FixtureHome::enter()?;
    // A session with no usable display or GPU fails inside wgpu with a panic; say it is not runnable here.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info.to_string();
        if ["Surface::configure", "device is lost", "wgpu"]
            .iter()
            .any(|w| message.contains(w))
        {
            println!(
                "window-progress not runnable here: {}",
                message.lines().next().unwrap_or_default()
            );
            let _ = std::io::stdout().flush();
        }
        previous(info);
    }));
    let event_loop = EventLoop::new().map_err(|e| anyhow::anyhow!("not runnable here: {e}"))?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    let (req_tx, outgoing) = tokio::sync::mpsc::unbounded_channel();
    let app = App::new(
        evt_rx,
        None,
        fixture_cli(),
        evt_tx,
        vec![(HOST.to_string(), req_tx)],
        None,
        crate::lease::Leases::new(true, Vec::new()),
        MainWake::new(event_loop.create_proxy()),
    );
    let inputs = crate::ui::init::NativeStartupInputs {
        resume: crate::ui::persist::resume::GlobalState {
            window_w: Some(640.0),
            window_h: Some(480.0),
            ..Default::default()
        },
        topology: None,
        settings: Settings::default(),
        keybindings: KeyBindings::defaults(),
        ledger,
    };
    Ok(NativeChild { _home: home, event_loop, app, outgoing, inputs })
}

/// The State fixtures of the result-routing, badge, account, canonical-row, Monitor and REPL drawer commits. They
/// rewire State (its event receiver, its connections, its active host), so they run in a child and window of their
/// own, never on the workload's State.
struct FixturesDriver {
    app: App,
    outcome: Option<Result<()>>,
}

impl ApplicationHandler for FixturesDriver {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.app.resumed(event_loop);
        if self.outcome.is_some() {
            return;
        }
        self.outcome = Some(match self.app.state.as_mut() {
            Some(state) => run_state_fixtures(state),
            None => Err(anyhow::anyhow!("not runnable here: native App/State startup did not complete")),
        });
        // The fixture that needs the App's own event handler, after the State's, on the State they left.
        if self.outcome.as_ref().is_some_and(|r| r.is_ok()) {
            let result = a_loss_of_focus_ends_a_drag_over_a_pdf_page(&mut self.app, event_loop);
            println!("state-fixture name=a_loss_of_focus_ends_a_drag_over_a_pdf_page ok={}", result.is_ok());
            self.outcome = Some(result);
        }
        event_loop.exit();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        self.app.window_event(event_loop, id, event);
    }
}

/// The window's loss of focus ends a drag over a PDF page, through the real event: the pointer then moving over the page
/// selects no more than it had.
fn a_loss_of_focus_ends_a_drag_over_a_pdf_page(app: &mut App, event_loop: &ActiveEventLoop) -> Result<()> {
    let state = app.state.as_mut().ok_or_else(|| anyhow::anyhow!("no State"))?;
    let (id, canvas) = (state.window.id(), state.a_pdf_drag_is_begun()?);
    anyhow::ensure!(state.a_pdf_drag_moves_to(canvas, 0.25).as_deref() == Some("alpha"), "the drag selected {:?}", state.pdf_select.text());
    app.window_event(event_loop, id, WindowEvent::Focused(false));
    let state = app.state.as_mut().ok_or_else(|| anyhow::anyhow!("no State"))?;
    let after = state.a_pdf_drag_moves_to(canvas, 0.55);
    anyhow::ensure!(after.as_deref() == Some("alpha"), "the pointer extended a drag the loss of focus ended: {after:?}");
    Ok(())
}

fn run_state_fixtures(state: &mut State) -> Result<()> {
    let fixtures: [(&str, fn(&mut State) -> Result<()>); 58] = [
        // First: the window's start-up picture is still the preview's.
        (
            "primary_u_over_the_start_picture_says_the_preview_shows_no_file",
            State::primary_u_over_the_start_picture_says_the_preview_shows_no_file,
        ),
        ("result_target_uses_host_and_listed_identity", State::result_target_uses_host_and_listed_identity),
        ("passing_a_result_row_keeps_its_badge", State::passing_a_result_row_keeps_its_badge),
        (
            "replaced_result_rejects_old_tree_attempt_before_effects",
            State::replaced_result_rejects_old_tree_attempt_before_effects,
        ),
        ("first_named_account_is_sent", State::first_named_account_is_sent),
        ("pending_result_dies_with_its_canonical_row", State::pending_result_dies_with_its_canonical_row),
        (
            "a_held_monitor_subscription_is_sent_on_each_new_connection",
            State::a_held_monitor_subscription_is_sent_on_each_new_connection,
        ),
        (
            "a_monitor_chart_is_redrawn_on_resize_and_on_return",
            State::a_monitor_chart_is_redrawn_on_resize_and_on_return,
        ),
        (
            "a_run_its_connections_end_closes_keeps_its_output",
            State::a_run_its_connections_end_closes_keeps_its_output,
        ),
        (
            "a_returning_link_reopens_the_runs_still_running",
            State::a_returning_link_reopens_the_runs_still_running,
        ),
        ("a_wheel_step_reports_presence", State::a_wheel_step_reports_presence),
        (
            "the_bars_enter_sends_page_reply_to_the_pages_host",
            State::the_bars_enter_sends_page_reply_to_the_pages_host,
        ),
        (
            "a_destroyed_session_moves_the_view_to_a_neighbor",
            State::a_destroyed_session_moves_the_view_to_a_neighbor,
        ),
        (
            "a_session_started_from_this_window_is_first_after_its_reply",
            State::a_session_started_from_this_window_is_first_after_its_reply,
        ),
        (
            "a_destroy_in_flight_and_a_dropped_row_are_handled_by_the_window",
            State::a_destroy_in_flight_and_a_dropped_row_are_handled_by_the_window,
        ),
        ("a_rows_drawing_stays_with_its_row", State::a_rows_drawing_stays_with_its_row),
        ("a_notice_stays_with_its_file", State::a_notice_stays_with_its_file),
        ("a_text_selections_reply_stays_off_the_box", State::a_text_selections_reply_stays_off_the_box),
        ("a_drawing_refusal_is_boxed_and_leaves_the_status_line", State::a_drawing_refusal_is_boxed_and_leaves_the_status_line),
        ("a_shown_pdf_page_is_the_drawing_base_by_its_page", State::a_shown_pdf_page_is_the_drawing_base_by_its_page),
        ("a_drag_over_a_pdf_page_selects_marks_and_copies_its_words", State::a_drag_over_a_pdf_page_selects_marks_and_copies_its_words),
        (
            "a_preview_message_is_on_the_status_line_and_boxed_and_a_row_switch_drops_it",
            State::a_preview_message_is_on_the_status_line_and_boxed_and_a_row_switch_drops_it,
        ),
        ("a_rows_drawer_stays_with_its_row", State::a_rows_drawer_stays_with_its_row),
        ("a_rows_drawer_height_stays_with_its_row", State::a_rows_drawer_height_stays_with_its_row),
        (
            "a_row_left_before_its_preview_came_back_shows_its_own",
            State::a_row_left_before_its_preview_came_back_shows_its_own,
        ),
        (
            "an_svg_preview_is_a_raster_and_a_broken_one_is_its_text",
            State::an_svg_preview_is_a_raster_and_a_broken_one_is_its_text,
        ),
        (
            "a_page_of_math_is_drawn_by_the_worker_in_one_batch",
            State::a_page_of_math_is_drawn_by_the_worker_in_one_batch,
        ),
        (
            "a_chat_of_200_messages_scrolls_posts_and_recovers",
            State::a_chat_of_200_messages_scrolls_posts_and_recovers,
        ),
        (
            "a_chats_pictures_show_and_one_opens_in_the_preview",
            State::a_chats_pictures_show_and_one_opens_in_the_preview,
        ),
        (
            "a_full_height_drawer_shrinks_only_for_an_answer_that_is_the_preview",
            State::a_full_height_drawer_shrinks_only_for_an_answer_that_is_the_preview,
        ),
        (
            "a_paste_in_the_connect_step_is_a_token_and_never_the_draft",
            State::a_paste_in_the_connect_step_is_a_token_and_never_the_draft,
        ),
        (
            "a_preview_scroll_moves_over_frames_and_comes_to_rest",
            State::a_preview_scroll_moves_over_frames_and_comes_to_rest,
        ),
        ("a_repl_wheel_tick_rolls_a_row_at_a_time", State::a_repl_wheel_tick_rolls_a_row_at_a_time),
        (
            "primary_u_over_a_chat_picture_keeps_its_words_through_a_relist_until_the_hold_ends",
            State::primary_u_over_a_chat_picture_keeps_its_words_through_a_relist_until_the_hold_ends,
        ),
        (
            "the_connection_line_never_says_connected_for_a_computer_whose_link_is_down",
            State::the_connection_line_never_says_connected_for_a_computer_whose_link_is_down,
        ),
        (
            "a_reconnect_ends_a_disconnect_line_that_carried_a_word",
            State::a_reconnect_ends_a_disconnect_line_that_carried_a_word,
        ),
        (
            "a_refetch_failure_is_said_by_whose_request_it_answers",
            State::a_refetch_failure_is_said_by_whose_request_it_answers,
        ),
        ("an_edit_view_follows_its_cursor", State::an_edit_view_follows_its_cursor),
        ("an_edit_footer_says_what_the_edit_is", State::an_edit_footer_says_what_the_edit_is),
        (
            "a_row_of_wide_tables_arrives_without_shaping_them_in_its_first_frame",
            State::a_row_of_wide_tables_arrives_without_shaping_them_in_its_first_frame,
        ),
        ("a_page_of_tables_shapes_only_the_rows_in_view", State::a_page_of_tables_shapes_only_the_rows_in_view),
        (
            "the_update_mark_is_drawn_for_this_computers_own_daemon",
            State::the_update_mark_is_drawn_for_this_computers_own_daemon,
        ),
        ("a_return_checks_its_parked_file_once", State::a_return_checks_its_parked_file_once),
        ("a_return_keeps_equal_bytes_and_replaces_changed_bytes", State::a_return_keeps_equal_bytes_and_replaces_changed_bytes),
        ("a_return_checks_only_the_file_on_screen", State::a_return_checks_only_the_file_on_screen),
        ("a_return_yields_to_existing_reads", State::a_return_yields_to_existing_reads),
        ("a_return_checks_only_a_drawn_preview", State::a_return_checks_only_a_drawn_preview),
        ("a_queued_read_survives_a_failed_dial", State::a_queued_read_survives_a_failed_dial),
        ("a_return_preserves_image_calibration", State::a_return_preserves_image_calibration),
        ("a_paged_raster_keeps_its_zoom_on_return", State::a_paged_raster_keeps_its_zoom_on_return),
        ("restoring_the_active_session_skips_the_check", State::restoring_the_active_session_skips_the_check),
        ("a_return_waits_for_the_departed_rows_check", State::a_return_waits_for_the_departed_rows_check),
        ("a_preamble_on_another_host_suppresses_the_return", State::a_preamble_on_another_host_suppresses_the_return),
        ("a_return_uses_only_the_current_connection", State::a_return_uses_only_the_current_connection),
        ("a_replaced_folder_keeps_its_installed_page", State::a_replaced_folder_keeps_its_installed_page),
        ("a_close_dialog_covers_the_returned_preview", State::a_close_dialog_covers_the_returned_preview),
        ("a_pdf_pages_reply_and_notice_are_its_own", State::a_pdf_pages_reply_and_notice_are_its_own),
        ("a_departing_pdf_request_says_nothing_in_the_session_entered", State::a_departing_pdf_request_says_nothing_in_the_session_entered),
    ];
    // Every fixture runs and reports; the child fails after the last if any failed.
    let mut failed = Vec::new();
    for (name, fixture) in fixtures {
        let result = fixture(state);
        println!("state-fixture name={name} ok={}", result.is_ok());
        if let Err(error) = result {
            eprintln!("state-fixture name={name}: {error:#}");
            failed.push(name);
        }
    }
    anyhow::ensure!(failed.is_empty(), "State fixtures failed: {}", failed.join(", "));
    Ok(())
}

pub(in crate::ui) fn state_fixtures_child() -> Result<()> {
    let NativeChild { _home, event_loop, app, outgoing: _, inputs } =
        native_child(Arc::new(Mutex::new(NativeProgressLedger::default())))?;
    let mut driver = FixturesDriver { app, outcome: None };
    crate::ui::init::with_native_startup(inputs, || event_loop.run_app(&mut driver))?;
    driver.outcome.unwrap_or_else(|| Err(anyhow::anyhow!("the State fixtures exited without a verdict")))
}

/// The window_minimized target's parent: the State fixtures and then the workload, each a child of this binary with a
/// window of its own. Their output streams straight through, so an overrun loses none of it. Both run whatever the
/// first did; then each must have exited 0 in its bound.
pub(in crate::ui) fn window_progress_parent() -> Result<()> {
    let mut failed = Vec::new();
    for (role, bound) in [("fixtures", FIXTURES_BOUND), ("workload", WORKLOAD_BOUND)] {
        let mut child = std::process::Command::new(std::env::current_exe()?)
            .args(["child", role])
            .stdin(std::process::Stdio::null())
            .spawn()?;
        match sot_platform::test_isolated::wait_until(&mut child, Instant::now() + bound) {
            Ok(status) if status.success() => println!("window-progress child={role} ok=true"),
            Ok(status) => failed.push(format!("window-progress child {role} failed: {status}")),
            Err(e) => failed.push(format!("window-progress child {role} did not finish within {bound:?}: {e}")),
        }
    }
    anyhow::ensure!(failed.is_empty(), "{}", failed.join("; "));
    Ok(())
}
