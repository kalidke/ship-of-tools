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
