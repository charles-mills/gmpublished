//! Draw custom primitives.
use crate::core::{self, Rectangle};
use crate::graphics::Viewport;
use crate::graphics::futures::{MaybeSend, MaybeSync};

use rustc_hash::FxHashMap;
use std::any::{Any, TypeId};
use std::fmt::Debug;

/// A batch of primitives.
pub type Batch = Vec<Instance>;

/// A set of methods which allows a [`Primitive`] to be rendered.
pub trait Primitive: Debug + MaybeSend + MaybeSync + 'static {
    /// The shared renderer of this [`Primitive`].
    ///
    /// Normally, this will contain a bunch of [`wgpu`] state; like
    /// a rendering pipeline, buffers, and textures.
    ///
    /// All instances of this [`Primitive`] type will share the same
    /// [`Renderer`].
    type Pipeline: Pipeline + MaybeSend + MaybeSync;

    /// Processes the [`Primitive`], allowing for GPU buffer allocation.
    fn prepare(
        &self,
        pipeline: &mut Self::Pipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        viewport: &Viewport,
    );

    /// Draws the [`Primitive`] in the given [`wgpu::RenderPass`].
    ///
    /// When possible, this should be implemented over [`render`](Self::render)
    /// since reusing the existing render pass should be considerably more
    /// efficient than issuing a new one.
    ///
    /// The viewport and scissor rect of the render pass provided is set
    /// to the bounds and clip bounds of the [`Primitive`], respectively.
    ///
    /// If you have complex composition needs, then you can leverage
    /// [`render`](Self::render) by returning `false` here.
    ///
    /// By default, it does nothing and returns `false`.
    fn draw(&self, _pipeline: &Self::Pipeline, _render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        false
    }

    /// Renders the [`Primitive`], using the given [`wgpu::CommandEncoder`].
    ///
    /// This will only be called if [`draw`](Self::draw) returns `false`.
    ///
    /// By default, it does nothing.
    fn render(
        &self,
        _pipeline: &Self::Pipeline,
        _encoder: &mut wgpu::CommandEncoder,
        _target: &wgpu::TextureView,
        _clip_bounds: &Rectangle<u32>,
    ) {
    }
}

/// The pipeline of a graphics [`Primitive`].
pub trait Pipeline: Any + MaybeSend + MaybeSync {
    /// Creates the [`Pipeline`] of a [`Primitive`].
    ///
    /// This will only be called once, when the first [`Primitive`] with this kind
    /// of [`Pipeline`] is encountered.
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self
    where
        Self: Sized;

    /// Creates a pipeline using the rendering adapter's capabilities.
    fn new_with_capabilities(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        _capabilities: &wgpu::DownlevelCapabilities,
    ) -> Self
    where
        Self: Sized,
    {
        Self::new(device, queue, format)
    }

    /// Starts demand collection for one compositor frame.
    fn begin_prepare(&mut self) {}

    /// Resolves collected demand before any primitive draws.
    fn finish_prepare(&mut self, _device: &wgpu::Device, _queue: &wgpu::Queue) {}

    /// Transfers frame-resource ownership after command submission.
    fn submitted(&mut self, _queue: &wgpu::Queue) {}

    /// Releases cached data after drawing.
    fn trim(&mut self) {}
}

pub(crate) trait Stored: Debug + MaybeSend + MaybeSync + 'static {
    fn prepare(
        &self,
        storage: &mut Storage,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        bounds: &Rectangle,
        viewport: &Viewport,
    );

    fn draw(&self, storage: &Storage, render_pass: &mut wgpu::RenderPass<'_>) -> bool;

    fn render(
        &self,
        storage: &Storage,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        clip_bounds: &Rectangle<u32>,
    );
}

#[derive(Debug)]
struct BlackBox<P: Primitive> {
    primitive: P,
}

impl<P: Primitive> Stored for BlackBox<P> {
    fn prepare(
        &self,
        storage: &mut Storage,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        bounds: &Rectangle,
        viewport: &Viewport,
    ) {
        if !storage.has::<P>() {
            let pipeline = match storage.capabilities.as_ref() {
                Some(capabilities) => {
                    P::Pipeline::new_with_capabilities(device, queue, format, capabilities)
                }
                None => P::Pipeline::new(device, queue, format),
            };
            storage.store::<P, _>(pipeline);
        }

        let renderer = storage
            .get_mut::<P>()
            .expect("renderer should be initialized")
            .downcast_mut::<P::Pipeline>()
            .expect("renderer should have the proper type");

        self.primitive
            .prepare(renderer, device, queue, bounds, viewport);
    }

    fn draw(&self, storage: &Storage, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        let renderer = storage
            .get::<P>()
            .expect("renderer should be initialized")
            .downcast_ref::<P::Pipeline>()
            .expect("renderer should have the proper type");

        self.primitive.draw(renderer, render_pass)
    }

    fn render(
        &self,
        storage: &Storage,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        clip_bounds: &Rectangle<u32>,
    ) {
        let renderer = storage
            .get::<P>()
            .expect("renderer should be initialized")
            .downcast_ref::<P::Pipeline>()
            .expect("renderer should have the proper type");

        self.primitive
            .render(renderer, encoder, target, clip_bounds);
    }
}

#[derive(Debug)]
/// An instance of a specific [`Primitive`].
pub struct Instance {
    /// The bounds of the [`Instance`].
    pub(crate) bounds: Rectangle,

    /// The [`Primitive`] to render.
    pub(crate) primitive: Box<dyn Stored>,
}

impl Instance {
    /// Creates a new [`Instance`] with the given [`Primitive`].
    pub fn new(bounds: Rectangle, primitive: impl Primitive) -> Self {
        Instance {
            bounds,
            primitive: Box::new(BlackBox { primitive }),
        }
    }
}

/// A renderer than can draw custom primitives.
pub trait Renderer: core::Renderer {
    /// Draws a custom primitive.
    fn draw_primitive(&mut self, bounds: Rectangle, primitive: impl Primitive);
}

/// Stores custom, user-provided types.
#[derive(Default)]
pub struct Storage {
    capabilities: Option<wgpu::DownlevelCapabilities>,
    pipelines: FxHashMap<TypeId, Box<dyn Pipeline>>,
    preparing: bool,
}

impl Storage {
    pub(crate) fn with_capabilities(capabilities: wgpu::DownlevelCapabilities) -> Self {
        Self {
            capabilities: Some(capabilities),
            ..Self::default()
        }
    }

    /// Returns `true` if `Storage` contains a type `T`.
    pub fn has<T: 'static>(&self) -> bool {
        self.pipelines.contains_key(&TypeId::of::<T>())
    }

    /// Inserts the data `T` in to [`Storage`].
    pub fn store<T: 'static, P: Pipeline>(&mut self, mut pipeline: P) {
        if self.preparing {
            pipeline.begin_prepare();
        }
        let _ = self.pipelines.insert(TypeId::of::<T>(), Box::new(pipeline));
    }

    pub(crate) fn begin_prepare(&mut self) {
        assert!(!self.preparing, "nested primitive preparation");
        self.preparing = true;
        for pipeline in self.pipelines.values_mut() {
            pipeline.begin_prepare();
        }
    }

    pub(crate) fn finish_prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        assert!(self.preparing, "primitive preparation was not started");
        for pipeline in self.pipelines.values_mut() {
            pipeline.finish_prepare(device, queue);
        }
        self.preparing = false;
    }

    pub(crate) fn submitted(&mut self, queue: &wgpu::Queue) {
        for pipeline in self.pipelines.values_mut() {
            pipeline.submitted(queue);
        }
    }

    /// Returns a reference to the data with type `T` if it exists in [`Storage`].
    pub fn get<T: 'static>(&self) -> Option<&dyn Any> {
        self.pipelines
            .get(&TypeId::of::<T>())
            .map(|pipeline| pipeline.as_ref() as &dyn Any)
    }

    /// Returns a mutable reference to the data with type `T` if it exists in [`Storage`].
    pub fn get_mut<T: 'static>(&mut self) -> Option<&mut dyn Any> {
        self.pipelines
            .get_mut(&TypeId::of::<T>())
            .map(|pipeline| pipeline.as_mut() as &mut dyn Any)
    }

    /// Trims the cache of all the pipelines in the [`Storage`].
    pub fn trim(&mut self) {
        for pipeline in self.pipelines.values_mut() {
            pipeline.trim();
        }
    }
}
