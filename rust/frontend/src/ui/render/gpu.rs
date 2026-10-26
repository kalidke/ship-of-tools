//! The GPU's lifetime owner: the surface, the device and queue, the surface's configuration and its clear colour.

/// The window's GPU. Field order is load-bearing: fields drop in declaration order, as `State`'s did. The surface's
/// size changes only by `resize`, and a lost or outdated surface is reconfigured once by `acquire_frame`.
pub(in crate::ui) struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    /// The frame's clear colour, chosen for the surface's format (`clear_color_for_surface`).
    clear_color: wgpu::Color,
}

impl Gpu {
    pub(in crate::ui) fn new(
        surface: wgpu::Surface<'static>, device: wgpu::Device, queue: wgpu::Queue, config: wgpu::SurfaceConfiguration,
        clear_color: wgpu::Color,
    ) -> Self {
        Self { surface, device, queue, config, clear_color }
    }

    pub(in crate::ui) fn clear_color(&self) -> wgpu::Color { self.clear_color }

    pub(in crate::ui) fn device(&self) -> &wgpu::Device { &self.device }
    pub(in crate::ui) fn queue(&self) -> &wgpu::Queue { &self.queue }
    pub(in crate::ui) fn config(&self) -> &wgpu::SurfaceConfiguration { &self.config }

    /// A new window size: the surface is reconfigured to it.
    pub(in crate::ui) fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    /// The frame's texture; a lost or outdated surface is reconfigured and asked once more.
    pub(in crate::ui) fn acquire_frame(&self) -> Result<wgpu::SurfaceTexture, wgpu::SurfaceError> {
        match self.surface.get_current_texture() {
            Ok(f) => Ok(f),
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.surface.configure(&self.device, &self.config);
                self.surface.get_current_texture()
            }
            Err(e) => Err(e),
        }
    }
}
