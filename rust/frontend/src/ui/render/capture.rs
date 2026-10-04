//! `--capture` and the selfie readback: the trigger frame, the output path, and the texture
//! copy and PNG write.

use super::*;

/// Trigger frame for `--capture`. Big enough for the transport task to push
/// connect → tree.root → preview.get back to the GPU thread, since each event
/// schedules its own redraw. Tunable if reconnect grows slower.
pub(in crate::ui) const CAPTURE_FRAME: u32 = 30;

/// Destination for a Ctrl+Shift+S selfie: `<dir>/selfie-<YYYYMMDD-HHMMSS>.png`,
/// where `dir` is `$SOT_SELFIE_DIR`, else `<$SOT_REPO_DIR>/selfies`, else the
/// current working directory. Creates the directory if missing.
pub(in crate::ui) fn selfie_path() -> PathBuf {
    let dir = std::env::var_os("SOT_SELFIE_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("SOT_REPO_DIR").map(|r| PathBuf::from(r).join("selfies")))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let _ = std::fs::create_dir_all(&dir);
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    dir.join(format!("selfie-{stamp}.png"))
}

/// Schedule a copy of `texture` into a freshly-allocated MAP_READ buffer.
/// The buffer is returned so the caller can submit the encoder, present the
/// frame, then map and decode the buffer once the GPU has finished.
pub(in crate::ui) fn stage_capture(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> (wgpu::Buffer, u32, u32) {
    let bpp = 4u32;
    let unpadded_bpr = width * bpp;
    let padded_bpr = (unpadded_bpr + wgpu::COPY_BYTES_PER_ROW_ALIGNMENT - 1)
        & !(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT - 1);
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("capture-readback"),
        size: (padded_bpr as u64) * (height as u64),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &buf,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(padded_bpr),
                rows_per_image: None,
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    (buf, padded_bpr, unpadded_bpr)
}

/// Map the readback buffer, compact padded rows, normalize channel order to
/// RGBA8, and write a PNG. Synchronous: we block on `device.poll(Wait)` since
/// the frontend is exiting after this anyway.
pub(in crate::ui) fn finish_capture(
    device: &wgpu::Device,
    buf: wgpu::Buffer,
    padded_bpr: u32,
    unpadded_bpr: u32,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    path: &std::path::Path,
) -> Result<()> {
    let slice = buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::Maintain::Wait);
    rx.recv()
        .context("readback channel closed")?
        .context("map_async failed")?;

    let data = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    let bgra = matches!(
        format,
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
    );
    for y in 0..height {
        let row_start = (y * padded_bpr) as usize;
        let row_end = row_start + unpadded_bpr as usize;
        let row = &data[row_start..row_end];
        for px in row.chunks_exact(4) {
            if bgra {
                pixels.push(px[2]);
                pixels.push(px[1]);
                pixels.push(px[0]);
                pixels.push(px[3]);
            } else {
                pixels.push(px[0]);
                pixels.push(px[1]);
                pixels.push(px[2]);
                pixels.push(px[3]);
            }
        }
    }
    drop(data);
    buf.unmap();

    image::save_buffer(path, &pixels, width, height, image::ColorType::Rgba8)
        .with_context(|| format!("save PNG to {}", path.display()))?;
    Ok(())
}


impl State {
    pub(in crate::ui) fn stage_frame_capture(
        &mut self,
        mut encoder: &mut wgpu::CommandEncoder,
        frame: &wgpu::SurfaceTexture,
    ) -> (Option<(wgpu::Buffer, u32, u32)>, Option<PathBuf>, bool, Option<PathBuf>) {
        // If `--capture` is set and we've waited long enough for transport
        // events to push through, copy the swapchain texture into a CPU
        // buffer in this same encoder, before `frame.present()` consumes it.
        // --capture-preview adds a second async round-trip (preview.get for
        // a specific file) on top of the connect-time root preview. Math
        // also has to wait on the MathJax sidecar per `$$…$$` block. Give
        // it more frames so the readback is taken after the math SVGs have
        // landed and been laid out.
        let capture_target_frame = if self.capture_delay_ms > 0 {
            // Explicit override from --capture-delay-ms. Redraw loop is
            // 60 Hz, so ms * 60 / 1000.
            (self.capture_delay_ms * 60 / 1000).max(1)
        } else if self.capture_preview_armed {
            CAPTURE_FRAME * 4
        } else {
            CAPTURE_FRAME
        };
        let capture_now = self.capture_path.is_some() && self.frame_counter == capture_target_frame;
        // Ctrl+Shift+S selfie: capture the current frame to a timestamped PNG
        // without exiting. Shares the readback machinery with the --capture
        // harness path; the harness `capture_now` (one-shot + exit) wins if
        // both request a shot on the same frame.
        let selfie_target = self.selfie_pending.take();
        let capture_target = if capture_now {
            self.capture_path.clone()
        } else {
            selfie_target.clone()
        };
        let readback = if capture_target.is_some() {
            Some(stage_capture(
                &self.device,
                &mut encoder,
                &frame.texture,
                self.config.width,
                self.config.height,
            ))
        } else {
            None
        };
        (readback, capture_target, capture_now, selfie_target)
    }

    pub(in crate::ui) fn finish_frame_capture(
        &mut self,
        readback: Option<(wgpu::Buffer, u32, u32)>,
        capture_target: Option<PathBuf>,
        capture_now: bool,
        selfie_target: Option<PathBuf>,
    ) {
        if let Some((buf, padded_bpr, unpadded_bpr)) = readback {
            let path = capture_target.unwrap();
            let is_selfie = !capture_now && selfie_target.is_some();
            match finish_capture(
                &self.device,
                buf,
                padded_bpr,
                unpadded_bpr,
                self.config.width,
                self.config.height,
                self.config.format,
                &path,
            ) {
                Ok(()) => {
                    tracing::info!(path = %path.display(), "capture wrote PNG");
                    if is_selfie {
                        self.status = format!("selfie saved: {}", path.display());
                        self.notify_sticky_until = Some(std::time::Instant::now() + NOTIFY_STICKY);
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "capture failed");
                    if is_selfie {
                        self.status = format!("selfie failed: {e}");
                        self.notify_sticky_until = Some(std::time::Instant::now() + NOTIFY_STICKY);
                    }
                }
            }
            // The --capture harness exits after its one shot; a selfie is live,
            // so keep running and repaint once so the toast shows.
            if capture_now {
                self.should_exit = true;
            } else {
                self.window.request_redraw();
            }
        } else if self.capture_path.is_some() && !self.should_exit {
            // Keep redrawing so frame_counter ticks up to CAPTURE_FRAME even
            // when there are no transport events to trigger redraws.
            self.window.request_redraw();
        }
    }
}