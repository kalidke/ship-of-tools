// The harness=false root of the opt-in `window_close` target: the product's own modules, a main thread that runs
// winit, and nothing else. It copies no App, lease, render or startup logic; ordinary `main.rs` is not involved.

mod browser_open;
mod cli;
mod lease;
mod net;
mod pages;
mod relaunch;
mod selfupdate;
mod ui;

fn main() -> anyhow::Result<()> {
    ui::run_native_window_close()
}
