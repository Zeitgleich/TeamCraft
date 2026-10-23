//! Runs `compositor.wgsl`: uploads the frame and mask, dispatches the passes and reads the result
//! back. Buffers are kept between frames (the mask history lives on the GPU) and rebuilt when the
//! frame or mask size changes.

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use super::{BackgroundEffect, BackgroundImage, BlurStrength, EffectsConfig, EffectsError, SegmentationMask};
use crate::video::VideoFrame;

const SHADER: &str = include_str!("compositor.wgsl");
/// The blurred background is computed at this fraction of the frame size.
const BACKGROUND_DOWNSCALE: u32 = 4;
const WORKGROUP: u32 = 8;

/// The GPU side of the effects.
pub struct Compositor {
  device: wgpu::Device,
  queue: wgpu::Queue,
  layout: wgpu::BindGroupLayout,
  pipelines: Pipelines,
  config: EffectsConfig,
  maybe_buffers: Option<Buffers>,
}

struct Pipelines {
  smooth_mask: wgpu::ComputePipeline,
  feather_horizontal: wgpu::ComputePipeline,
  feather_vertical: wgpu::ComputePipeline,
  downsample: wgpu::ComputePipeline,
  blur_horizontal: wgpu::ComputePipeline,
  blur_vertical: wgpu::ComputePipeline,
  composite_luma: wgpu::ComputePipeline,
  composite_chroma: wgpu::ComputePipeline,
}

/// Size-dependent GPU state.
struct Buffers {
  key: SizeKey,
  params: wgpu::Buffer,
  frame: wgpu::Buffer,
  mask_in: wgpu::Buffer,
  // Only used through the bind group; kept to own them alongside it.
  _masks: wgpu::Buffer,
  _small: wgpu::Buffer,
  _image: wgpu::Buffer,
  maybe_image: Option<Arc<BackgroundImage>>,
  output: wgpu::Buffer,
  staging: wgpu::Buffer,
  bind_group: wgpu::BindGroup,
  has_history: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct SizeKey {
  width: u32,
  height: u32,
  mask_width: u32,
  mask_height: u32,
}

/// Mirrors `Params` in the shader.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
  width: u32,
  height: u32,
  mask_width: u32,
  mask_height: u32,
  small_width: u32,
  small_height: u32,
  image_width: u32,
  image_height: u32,
  mode: u32,
  radius: u32,
  feather_radius: u32,
  has_history: u32,
  smoothing: f32,
  edge_low: f32,
  edge_high: f32,
  padding: f32,
}

impl Compositor {
  /// Opens the default GPU (a software rasterizer counts too, e.g. lavapipe in CI).
  pub async fn new(config: EffectsConfig) -> Result<Self, EffectsError> {
    let instance = wgpu::Instance::default();
    let adapter = instance
      .request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
      })
      .await
      .map_err(|_| EffectsError::NoAdapter)?;
    let (device, queue) = adapter
      .request_device(&wgpu::DeviceDescriptor {
        label: Some("effects"),
        ..Default::default()
      })
      .await
      .map_err(gpu_error)?;
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
      label: Some("compositor"),
      source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
      label: Some("compositor"),
      entries: &[
        entry(0, wgpu::BufferBindingType::Uniform),
        entry(1, storage(true)),
        entry(2, storage(true)),
        entry(3, storage(false)),
        entry(4, storage(false)),
        entry(5, storage(true)),
        entry(6, storage(false)),
      ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
      label: Some("compositor"),
      bind_group_layouts: &[Some(&layout)],
      immediate_size: 0,
    });
    let pipeline = |entry_point: &str| {
      device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(entry_point),
        layout: Some(&pipeline_layout),
        module: &module,
        entry_point: Some(entry_point),
        compilation_options: Default::default(),
        cache: None,
      })
    };
    let pipelines = Pipelines {
      smooth_mask: pipeline("smooth_mask"),
      feather_horizontal: pipeline("feather_horizontal"),
      feather_vertical: pipeline("feather_vertical"),
      downsample: pipeline("downsample"),
      blur_horizontal: pipeline("blur_horizontal"),
      blur_vertical: pipeline("blur_vertical"),
      composite_luma: pipeline("composite_luma"),
      composite_chroma: pipeline("composite_chroma"),
    };
    Ok(Self {
      device,
      queue,
      layout,
      pipelines,
      config,
      maybe_buffers: None,
    })
  }

  /// Composites `frame` with `effect` behind the person in `mask`. `BackgroundEffect::None`
  /// returns the frame unchanged without touching the GPU.
  pub async fn apply(
    &mut self,
    frame: &VideoFrame,
    mask: &SegmentationMask,
    effect: &BackgroundEffect,
  ) -> Result<VideoFrame, EffectsError> {
    let (mode, radius) = match effect {
      BackgroundEffect::None => return Ok(frame.clone()),
      BackgroundEffect::Blur(BlurStrength::Light) => (1, 4),
      BackgroundEffect::Blur(BlurStrength::Strong) => (1, 10),
      BackgroundEffect::Image(_) => (2, 0),
    };
    let resolution = frame.resolution();
    if !resolution.width.is_multiple_of(8) {
      return Err(EffectsError::UnsupportedWidth(resolution.width));
    }
    let key = SizeKey {
      width: u32::from(resolution.width),
      height: u32::from(resolution.height),
      mask_width: mask.width(),
      mask_height: mask.height(),
    };
    self.prepare(key, effect);
    let Some(buffers) = &mut self.maybe_buffers else {
      return Err(EffectsError::Gpu("buffers missing".into()));
    };
    let (image_width, image_height) = match &buffers.maybe_image {
      Some(image) => (image.width(), image.height()),
      None => (1, 1),
    };
    let (small_width, small_height) = small_size(key);
    let params = Params {
      width: key.width,
      height: key.height,
      mask_width: key.mask_width,
      mask_height: key.mask_height,
      small_width,
      small_height,
      image_width,
      image_height,
      mode,
      radius,
      feather_radius: self.config.feather_radius,
      has_history: u32::from(buffers.has_history),
      smoothing: self.config.mask_smoothing,
      edge_low: self.config.edge_low,
      edge_high: self.config.edge_high,
      padding: 0.0,
    };
    buffers.has_history = true;
    self
      .queue
      .write_buffer(&buffers.params, 0, bytemuck::bytes_of(&params));
    self.queue.write_buffer(&buffers.frame, 0, frame.data());
    self
      .queue
      .write_buffer(&buffers.mask_in, 0, bytemuck::cast_slice(mask.values()));

    let mut encoder = self
      .device
      .create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("effects"),
      });
    {
      let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some("effects"),
        timestamp_writes: None,
      });
      pass.set_bind_group(0, &buffers.bind_group, &[]);
      let mask_groups = (groups(key.mask_width), groups(key.mask_height));
      for pipeline in [
        &self.pipelines.smooth_mask,
        &self.pipelines.feather_horizontal,
        &self.pipelines.feather_vertical,
      ] {
        pass.set_pipeline(pipeline);
        pass.dispatch_workgroups(mask_groups.0, mask_groups.1, 1);
      }
      if mode == 1 {
        let small_groups = (groups(small_width), groups(small_height));
        for pipeline in [
          &self.pipelines.downsample,
          &self.pipelines.blur_horizontal,
          &self.pipelines.blur_vertical,
        ] {
          pass.set_pipeline(pipeline);
          pass.dispatch_workgroups(small_groups.0, small_groups.1, 1);
        }
      }
      pass.set_pipeline(&self.pipelines.composite_luma);
      pass.dispatch_workgroups(groups(key.width / 4), groups(key.height), 1);
      pass.set_pipeline(&self.pipelines.composite_chroma);
      pass.dispatch_workgroups(groups(key.width / 8), groups(key.height / 2), 1);
    }
    encoder.copy_buffer_to_buffer(&buffers.output, 0, &buffers.staging, 0, buffers.output.size());
    self.queue.submit(Some(encoder.finish()));

    let data = read_back(&self.device, &buffers.staging).await?;
    Ok(VideoFrame::new(resolution, frame.timestamp_us(), data)?)
  }

  /// (Re)creates the buffers for this size and uploads a changed background image.
  fn prepare(&mut self, key: SizeKey, effect: &BackgroundEffect) {
    let image = match effect {
      BackgroundEffect::Image(image) => Some(Arc::clone(image)),
      _ => None,
    };
    let is_current = self
      .maybe_buffers
      .as_ref()
      .is_some_and(|buffers| buffers.key == key && buffers.maybe_image == image);
    if is_current {
      return;
    }
    let has_history = self
      .maybe_buffers
      .as_ref()
      .is_some_and(|buffers| buffers.key == key && buffers.has_history);
    self.maybe_buffers = Some(self.create_buffers(key, image, has_history));
  }

  fn create_buffers(
    &self,
    key: SizeKey,
    maybe_image: Option<Arc<BackgroundImage>>,
    has_history: bool,
  ) -> Buffers {
    let device = &self.device;
    let frame_bytes = u64::from(key.width) * u64::from(key.height) * 3 / 2;
    let mask_values = u64::from(key.mask_width) * u64::from(key.mask_height);
    let (small_width, small_height) = small_size(key);
    let buffer = |label: &str, size: u64, usage: wgpu::BufferUsages| {
      device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: size.max(16),
        usage,
        mapped_at_creation: false,
      })
    };
    let storage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
    let params = buffer(
      "params",
      std::mem::size_of::<Params>() as u64,
      wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    );
    let frame = buffer("frame", frame_bytes, storage);
    let mask_in = buffer("mask_in", mask_values * 4, storage);
    let masks = buffer("masks", mask_values * 4 * 3, wgpu::BufferUsages::STORAGE);
    let small = buffer(
      "small",
      u64::from(small_width) * u64::from(small_height) * 16 * 2,
      wgpu::BufferUsages::STORAGE,
    );
    let image_bytes: &[u8] = match &maybe_image {
      Some(image) => image.rgba(),
      None => &[0; 4],
    };
    let image = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
      label: Some("image"),
      contents: image_bytes,
      usage: wgpu::BufferUsages::STORAGE,
    });
    let output = buffer(
      "output",
      frame_bytes,
      wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
    );
    let staging = buffer(
      "staging",
      frame_bytes,
      wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
    );
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
      label: Some("compositor"),
      layout: &self.layout,
      entries: &[
        binding(0, &params),
        binding(1, &frame),
        binding(2, &mask_in),
        binding(3, &masks),
        binding(4, &small),
        binding(5, &image),
        binding(6, &output),
      ],
    });
    Buffers {
      key,
      params,
      frame,
      mask_in,
      _masks: masks,
      _small: small,
      _image: image,
      maybe_image,
      output,
      staging,
      bind_group,
      has_history,
    }
  }
}

/// Maps the staging buffer and copies it out. Native waits on the device; browsers resolve the
/// mapping on their own event loop.
async fn read_back(device: &wgpu::Device, staging: &wgpu::Buffer) -> Result<Vec<u8>, EffectsError> {
  let slice = staging.slice(..);
  let (mapped, mapped_rx) = futures_channel::oneshot::channel();
  slice.map_async(wgpu::MapMode::Read, move |result| {
    // The receiver only goes away when the caller stopped waiting.
    let _ = mapped.send(result);
  });
  #[cfg(not(target_arch = "wasm32"))]
  device
    .poll(wgpu::PollType::wait_indefinitely())
    .map_err(gpu_error)?;
  #[cfg(target_arch = "wasm32")]
  let _ = device;
  mapped_rx
    .await
    .map_err(|_| EffectsError::Gpu("read-back cancelled".into()))?
    .map_err(gpu_error)?;
  let data = slice.get_mapped_range().map_err(gpu_error)?.to_vec();
  staging.unmap();
  Ok(data)
}

fn small_size(key: SizeKey) -> (u32, u32) {
  (
    (key.width / BACKGROUND_DOWNSCALE).max(1),
    (key.height / BACKGROUND_DOWNSCALE).max(1),
  )
}

fn groups(size: u32) -> u32 {
  size.div_ceil(WORKGROUP)
}

fn storage(read_only: bool) -> wgpu::BufferBindingType {
  wgpu::BufferBindingType::Storage { read_only }
}

fn entry(index: u32, ty: wgpu::BufferBindingType) -> wgpu::BindGroupLayoutEntry {
  wgpu::BindGroupLayoutEntry {
    binding: index,
    visibility: wgpu::ShaderStages::COMPUTE,
    ty: wgpu::BindingType::Buffer {
      ty,
      has_dynamic_offset: false,
      min_binding_size: None,
    },
    count: None,
  }
}

fn binding(index: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
  wgpu::BindGroupEntry {
    binding: index,
    resource: buffer.as_entire_binding(),
  }
}

fn gpu_error(error: impl std::fmt::Display) -> EffectsError {
  EffectsError::Gpu(error.to_string())
}
