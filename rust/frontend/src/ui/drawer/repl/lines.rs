//! Thin State adapters: upload outside ratatui, then return an owned line build.

use crate::ui::*;
use super::presentation::ReplImageUpload;

struct QuadUpload<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    pipeline: &'a QuadPipeline,
}

impl ReplImageUpload<Quad> for QuadUpload<'_> {
    fn upload(&mut self, rgba: &[u8], width: u32, height: u32) -> Result<Quad> {
        Quad::from_rgba8(self.device, self.queue, self.pipeline, rgba, width, height)
    }
}

impl State {
    pub(in crate::ui) fn decode_repl_images(&mut self) {
        let row = self.active_ws_key();
        let mut upload = QuadUpload {
            device: self.gpu.device(), queue: self.gpu.queue(), pipeline: &self.quad_pipeline,
        };
        self.repl_presentation.refresh_images(&row, self.repl_model.entries(), &mut upload);
    }

    pub(in crate::ui) fn build_repl_view(&mut self, scroll: u16) -> (Vec<RtLine<'static>>, u16) {
        let row = self.active_ws_key();
        let starting = self.active_repl_starting();
        self.repl_presentation.build(
            &row, self.repl_model.entries(), starting, self.canvas.cell_w(), self.canvas.cell_h(), scroll,
        )
    }
}

#[cfg(test)]
#[path = "lines_tests.rs"]
mod tests;
