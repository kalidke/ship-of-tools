//! Image previews: PNG decode and SVG rasterization into wgpu quads.

pub(crate) mod figures;
mod replies;
pub(in crate::ui) mod keys;
pub(in crate::ui) mod ink;
pub(in crate::ui) mod ink_input;
mod roi;
pub(crate) mod overlay;
pub(crate) mod png;
pub(in crate::ui) mod raster;
pub(in crate::ui) mod pages;
pub(in crate::ui) mod figure_overlays;
pub(in crate::ui) mod figure_store;
pub(crate) mod svg;
pub(crate) mod svg_child;
pub(crate) mod svg_draws;
pub(crate) mod svg_run;
pub(crate) mod svg_wire;
pub(crate) mod view;

