pub mod audio;
pub mod gpu_compute;
pub mod gpu_model1;
pub mod video;

/// Something drawn over the emulated image, into the same surface texture.
///
/// The renderer owns the wgpu device and the swapchain, so anything that wants
/// to draw on top has to be handed the encoder mid-frame rather than opening a
/// surface of its own.
pub trait UiPass {
    fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
    );
}
