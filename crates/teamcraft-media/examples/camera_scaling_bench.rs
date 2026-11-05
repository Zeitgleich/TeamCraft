//! CPU time and latency of the desktop camera send path when the camera delivers more than the
//! call sends (issue #537): 4K and 1080p cameras shrunk to the call's send size (1080p in Auto
//! quality, 720p when the user caps it there), against cameras that deliver the send size, with
//! and without a background effect, with OpenH264 and (feature `gstreamer`) the hardware encoder.
//! Each shrink is timed twice: on the CPU (`VideoFrame::scaled`, what the encoder thread does) and
//! on the GPU through wgpu (upload, one compute pass, read-back: what scaling in the effects
//! pipeline would cost), to decide whether moving it to the GPU pays.
//!
//! The frames are synthetic: the moving test pattern with a little sensor noise, as packed YUV
//! (`yuvs`, what AVFoundation hands the macOS app), so no camera is needed. Each stage is timed on
//! its own, then the whole chain per camera and quality at 30 fps: the copy out of the camera's
//! buffer, YUYV → I420, the shrink to the send size, the effect (segmentation + compositor) and the
//! simulcast layers of `camera_layers`, whose encoders scale the lower layers themselves.
//!
//! ```sh
//! crates/teamcraft-media/models/fetch.sh
//! TEAMCRAFT_SEGMENTATION_MODEL=crates/teamcraft-media/models/selfie_segmentation_landscape.onnx \
//!   cargo run --release -p teamcraft-media --example camera_scaling_bench \
//!   --features openh264-source,segmentation,gstreamer
//! ```
//!
//! Without a model the effect's mask comes from a fixed ellipse (the compositor still runs).
//! `TEAMCRAFT_BENCH_STAGES_ONLY=1` skips the whole chains (a few seconds instead of minutes),
//! `TEAMCRAFT_BENCH_CHAINS=hardware` runs only the chains whose name contains that text.
//! "CPU" is the process's CPU time (all threads: encoder threads, GPU driver) per frame, and
//! "% core" that time against the 33 ms a frame may take at 30 fps.

use std::time::{Duration, Instant};

use anyhow::Context;
use teamcraft_media::capture_convert::{PixelFormat, RawPicture};
use teamcraft_media::effects::{
  BackgroundEffect, BlurStrength, Compositor, EffectsConfig, EffectsPipeline, EllipseSegmenter,
  PersonSegmenter, SegmentationModel, TractSegmenter,
};
use teamcraft_media::video::{VideoFrame, VideoResolution, camera_layers, camera_limit};
use teamcraft_media::video_capture::test_pattern;
use teamcraft_media::video_codec::{SimulcastEncoder, VideoContent};
use teamcraft_media::video_pipeline::{EncoderFactory, FrameFilter, software_encoders};
use teamcraft_proto::VideoLayerInfo;
use tracing::info;

const UHD: VideoResolution = VideoResolution {
  width: 3840,
  height: 2160,
};
const FULL_HD: VideoResolution = VideoResolution {
  width: 1920,
  height: 1080,
};
const HD: VideoResolution = VideoResolution {
  width: 1280,
  height: 720,
};
/// Camera size, and the quality the user picked (`None`: Auto, else the lines it is capped at).
const CASES: [(&str, VideoResolution, Option<u16>); 5] = [
  ("4K", UHD, None),
  ("4K", UHD, Some(720)),
  ("1080p", FULL_HD, None),
  ("1080p", FULL_HD, Some(720)),
  ("720p", HD, None),
];
const FRAMERATE: u8 = 30;
/// Distinct source pictures, reused in turn (16 4K pictures in YUYV are 265 MB).
const RING: u64 = 16;
/// Frames per measurement: 10 s at 30 fps.
const FRAMES: u64 = 300;
const FRAME_INTERVAL: Duration = Duration::from_micros(33_333);

/// Where the shrink to the send size runs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scaler {
  Cpu,
  Gpu,
}

fn main() -> anyhow::Result<()> {
  tracing_subscriber::fmt().with_target(false).init();
  let maybe_model = match std::env::var("TEAMCRAFT_SEGMENTATION_MODEL") {
    Ok(path) => Some(std::fs::read(&path).with_context(|| format!("reading {path}"))?),
    Err(_) => {
      info!("TEAMCRAFT_SEGMENTATION_MODEL not set: the effect's mask is a fixed ellipse");
      None
    },
  };
  let stages_only = std::env::var_os("TEAMCRAFT_BENCH_STAGES_ONLY").is_some();
  let maybe_chain_filter = std::env::var("TEAMCRAFT_BENCH_CHAINS").ok();
  let encoders = encoder_choices();
  let mut gpu = GpuScaler::new()?;
  let mut stages = Vec::new();
  let mut chains = Vec::new();

  let mut maybe_pictures: Option<(VideoResolution, Vec<Vec<u8>>)> = None;
  for (name, source, maybe_lines) in CASES {
    let quality = maybe_lines.map_or("Auto".to_owned(), |lines| format!("{lines}p"));
    let is_new_source = maybe_pictures.as_ref().is_none_or(|(size, _)| *size != source);
    if is_new_source {
      let pictures = (0..RING)
        .map(|index| yuyv_picture(source, index))
        .collect::<anyhow::Result<Vec<_>>>()?;
      maybe_pictures = Some((source, pictures));
    }
    let pictures = maybe_pictures
      .as_ref()
      .map(|(_, pictures)| pictures)
      .context("pictures")?;
    let raw = |index: u64| RawPicture {
      format: PixelFormat::Yuyv,
      width: u32::from(source.width),
      height: u32::from(source.height),
      stride: 0,
      data: &pictures[(index % RING) as usize],
    };
    let layers = camera_layers(source, FRAMERATE, camera_limit(maybe_lines));
    let target = top_size(&layers);

    if is_new_source {
      stages.push(measure(
        &format!("{name}: copy out of the camera buffer"),
        false,
        |index| {
          std::hint::black_box(raw(index).data.to_vec());
          Ok(())
        },
      )?);
      stages.push(measure(&format!("{name}: YUYV -> I420"), false, |index| {
        std::hint::black_box(raw(index).to_i420(index * 33_333)?);
        Ok(())
      })?);
    }
    let scalers: &[Scaler] = if target == source {
      &[Scaler::Cpu]
    } else {
      let frames = (0..RING)
        .map(|index| raw(index).to_i420(0))
        .collect::<Result<Vec<_>, _>>()?;
      let size = format!("{}x{}", target.width, target.height);
      stages.push(measure(
        &format!("{name} -> {size}: VideoFrame::scaled (CPU)"),
        false,
        |index| {
          std::hint::black_box(frames[(index % RING) as usize].scaled(target)?);
          Ok(())
        },
      )?);
      stages.push(measure(
        &format!("{name} -> {size}: GPU (upload, scale, read back)"),
        false,
        |index| {
          std::hint::black_box(gpu.scale(&frames[(index % RING) as usize], target)?);
          Ok(())
        },
      )?);
      let gpu_picture = gpu.scale(&frames[0], target)?;
      let cpu_picture = frames[0].scaled(target)?;
      info!(
        psnr_db = format!("{:.1}", luma_psnr(&cpu_picture, &gpu_picture)),
        "{name} -> {size}: GPU (area filter) against CPU (bilinear)"
      );
      &[Scaler::Cpu, Scaler::Gpu]
    };
    // The same stages back to back, paced like a camera: idle cores clock down between frames
    // and freed buffers go back to the system, so this costs more than the stages' sum.
    stages.push(measure(
      &format!("{name} {quality}: copy, YUYV -> I420, CPU scaling, paced"),
      true,
      |index| {
        let copied = raw(index).data.to_vec();
        let picture = RawPicture {
          data: &copied,
          ..raw(index)
        };
        let frame = picture.to_i420(index * 33_333)?;
        std::hint::black_box(frame.at_size(target)?);
        Ok(())
      },
    )?);
    if stages_only {
      continue;
    }

    for (encoder_name, factory) in &encoders {
      for effect in [false, true] {
        for &scaler in scalers {
          let mut maybe_filter = if effect {
            Some(effect_filter(maybe_model.as_deref())?)
          } else {
            None
          };
          let mut simulcast = SimulcastEncoder::new(&layers, |layer| factory(layer, VideoContent::Camera))?;
          let label = format!(
            "{name} {quality} chain{}, {}, {encoder_name}",
            match (target == source, scaler) {
              (true, _) => "",
              (false, Scaler::Cpu) => " (CPU scaling)",
              (false, Scaler::Gpu) => " (GPU scaling)",
            },
            if effect { "blur" } else { "no effect" }
          );
          if maybe_chain_filter
            .as_deref()
            .is_some_and(|filter| !label.contains(filter))
          {
            continue;
          }
          chains.push(measure(&label, true, |index| {
            let timestamp_us = index * 33_333;
            let copied = raw(index).data.to_vec();
            let picture = RawPicture {
              data: &copied,
              ..raw(index)
            };
            let frame = picture.to_i420(timestamp_us)?;
            let frame = match (target == frame.resolution(), scaler) {
              (true, _) => frame,
              (false, Scaler::Cpu) => frame.scaled(target)?,
              (false, Scaler::Gpu) => gpu.scale(&frame, target)?,
            };
            let frame = match &mut maybe_filter {
              Some(filter) => filter(frame),
              None => frame,
            };
            std::hint::black_box(simulcast.encode(&frame)?);
            Ok(())
          })?);
        }
      }
    }
  }

  // What the encoders do per frame besides encoding: the lower layers scaled from the top one
  // (from a picture twice their size where there is one, as `SimulcastEncoder` does), and for the
  // hardware encoder the I420 → NV12 repacking (inside the encode rows above, not added to them).
  for (name, top) in [("1080p", FULL_HD), ("720p", HD)] {
    let frames = (0..RING)
      .map(|index| test_pattern(top, index, 0))
      .collect::<Result<Vec<_>, _>>()?;
    let layers = camera_layers(top, FRAMERATE, camera_limit(None));
    stages.push(measure(
      &format!("{name}: encoder-side layer scaling ({} layers)", layers.len()),
      false,
      |index| {
        let frame = &frames[(index % RING) as usize];
        let mut shrunk: Vec<VideoFrame> = Vec::new();
        for layer in layers.iter().rev() {
          let size = VideoResolution {
            width: layer.width,
            height: layer.height,
          };
          let source = shrunk
            .iter()
            .find(|picture| picture.resolution().width == size.width * 2)
            .unwrap_or(frame);
          if source.resolution() != size {
            let picture = source.scaled(size)?;
            shrunk.push(picture);
          }
        }
        std::hint::black_box(shrunk);
        Ok(())
      },
    )?);
  }
  for (name, size) in [("1080p", FULL_HD), ("720p", HD)] {
    let frame = test_pattern(size, 0, 0)?;
    stages.push(measure(
      &format!("{name}: compositor only (light blur, GPU)"),
      false,
      {
        let mut compositor = pollster::block_on(Compositor::new(EffectsConfig::default()))?;
        let mask = EllipseSegmenter {
          width: 256,
          height: 144,
        }
        .segment(&frame)?;
        let frame = frame.clone();
        move |_| {
          pollster::block_on(compositor.apply(&frame, &mask, &BackgroundEffect::Blur(BlurStrength::Light)))?;
          Ok(())
        }
      },
    )?);
  }
  if let Some(model) = &maybe_model {
    let frame = test_pattern(HD, 0, 0)?;
    let mut segmenter = TractSegmenter::from_onnx(model, SegmentationModel::MEDIAPIPE_LANDSCAPE)?;
    stages.push(measure("720p: segmentation only (tract, CPU)", false, |_| {
      std::hint::black_box(segmenter.segment(&frame)?);
      Ok(())
    })?);
  }

  info!("| Stage | Average | Worst | CPU per frame | % of a core at 30 fps |");
  for row in stages.iter().chain(&chains) {
    info!(
      "| {} | {:.2} ms | {:.2} ms | {:.2} ms | {:.1} % |",
      row.name,
      millis(row.average),
      millis(row.worst),
      millis(row.cpu_per_frame),
      row.cpu_per_frame.as_secs_f64() / FRAME_INTERVAL.as_secs_f64() * 100.0
    );
  }
  Ok(())
}

fn top_size(layers: &[VideoLayerInfo]) -> VideoResolution {
  layers
    .iter()
    .map(|layer| VideoResolution {
      width: layer.width,
      height: layer.height,
    })
    .max_by_key(|size| u32::from(size.width) * u32::from(size.height))
    .unwrap_or(HD)
}

/// OpenH264, and the hardware encoder when GStreamer has one.
fn encoder_choices() -> Vec<(&'static str, EncoderFactory)> {
  #[allow(unused_mut)]
  let mut encoders = vec![("OpenH264", software_encoders())];
  #[cfg(feature = "gstreamer")]
  {
    use teamcraft_media::video_gstreamer::GstreamerEncoder;
    let probe = camera_layers(HD, FRAMERATE, camera_limit(None));
    match probe
      .last()
      .and_then(|layer| GstreamerEncoder::new(layer, VideoContent::Camera))
    {
      Some(encoder) => {
        use teamcraft_media::video_codec::VideoEncoder;
        info!(element = encoder.name(), "hardware encoder");
        encoders.push(("hardware", teamcraft_media::video_pipeline::preferred_encoders()));
      },
      None => info!("no hardware encoder through GStreamer"),
    }
  }
  encoders
}

/// The effect as the encoder thread runs it: segmentation, then the compositor's light blur.
fn effect_filter(maybe_model: Option<&[u8]>) -> anyhow::Result<FrameFilter> {
  let segmenter: Box<dyn PersonSegmenter> = match maybe_model {
    Some(model) => Box::new(TractSegmenter::from_onnx(
      model,
      SegmentationModel::MEDIAPIPE_LANDSCAPE,
    )?),
    None => Box::new(EllipseSegmenter {
      width: 256,
      height: 144,
    }),
  };
  let compositor = pollster::block_on(Compositor::new(EffectsConfig::default()))?;
  let pipeline = EffectsPipeline::new(segmenter, compositor, BackgroundEffect::Blur(BlurStrength::Light));
  Ok(pipeline.into_frame_filter().0)
}

/// The test pattern at `resolution` as packed YUYV, with a little noise on the luma (a camera's
/// sensor noise; a noiseless picture is unrealistically cheap to encode).
fn yuyv_picture(resolution: VideoResolution, index: u64) -> anyhow::Result<Vec<u8>> {
  let frame = test_pattern(resolution, index, 0)?;
  let (y, u, v) = frame.planes();
  let (width, height) = (usize::from(resolution.width), usize::from(resolution.height));
  let mut noise = 0x9e37_79b9_u32.wrapping_mul(index as u32 + 1);
  let mut noisy = |luma: u8| {
    noise ^= noise << 13;
    noise ^= noise >> 17;
    noise ^= noise << 5;
    luma.saturating_add((noise & 7) as u8).saturating_sub(4)
  };
  let mut packed = Vec::with_capacity(width * height * 2);
  for row in 0..height {
    let chroma_row = (row / 2) * (width / 2);
    for pair in 0..width / 2 {
      packed.extend([
        noisy(y[row * width + pair * 2]),
        u[chroma_row + pair],
        noisy(y[row * width + pair * 2 + 1]),
        v[chroma_row + pair],
      ]);
    }
  }
  Ok(packed)
}

/// Luma PSNR of `b` against `a` (same size).
fn luma_psnr(a: &VideoFrame, b: &VideoFrame) -> f64 {
  let (a, b) = (a.planes().0, b.planes().0);
  let squared: f64 = a
    .iter()
    .zip(b)
    .map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2))
    .sum();
  let mse = squared / a.len().max(1) as f64;
  if mse == 0.0 {
    f64::INFINITY
  } else {
    10.0 * (255.0 * 255.0 / mse).log10()
  }
}

/// The shrink on the GPU, as scaling in the effects pipeline would do it: the camera's I420
/// picture uploaded, one compute pass (an area filter: each target pixel averages the source
/// pixels it covers, so 4K → 720p doesn't alias like bilinear's one tap in nine), the send-size
/// picture read back for the encoders. Bench only.
struct GpuScaler {
  device: wgpu::Device,
  queue: wgpu::Queue,
  layout: wgpu::BindGroupLayout,
  pipeline: wgpu::ComputePipeline,
  maybe_buffers: Option<GpuBuffers>,
}

struct GpuBuffers {
  source: VideoResolution,
  target: VideoResolution,
  input: wgpu::Buffer,
  output: wgpu::Buffer,
  staging: wgpu::Buffer,
  bind_group: wgpu::BindGroup,
}

const SCALE_SHADER: &str = r"
struct Params { src_width: u32, src_height: u32, dst_width: u32, dst_height: u32 }
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> src: array<u32>;
@group(0) @binding(2) var<storage, read_write> dst: array<u32>;

fn src_byte(index: u32) -> f32 {
  return f32((src[index >> 2u] >> ((index & 3u) * 8u)) & 0xffu);
}

fn bilinear(offset: u32, width: u32, height: u32, x: f32, y: f32) -> f32 {
  let cx = clamp(x, 0.0, f32(width - 1u));
  let cy = clamp(y, 0.0, f32(height - 1u));
  let x0 = u32(floor(cx));
  let y0 = u32(floor(cy));
  let x1 = min(x0 + 1u, width - 1u);
  let y1 = min(y0 + 1u, height - 1u);
  let fx = cx - f32(x0);
  let fy = cy - f32(y0);
  let top = mix(src_byte(offset + y0 * width + x0), src_byte(offset + y0 * width + x1), fx);
  let bottom = mix(src_byte(offset + y1 * width + x0), src_byte(offset + y1 * width + x1), fx);
  return mix(top, bottom, fy);
}

// z: 0 luma, 1 U, 2 V. Each invocation writes four target pixels (one u32).
@compute @workgroup_size(8, 8, 1)
fn scale(@builtin(global_invocation_id) id: vec3<u32>) {
  var sw = params.src_width;
  var sh = params.src_height;
  var dw = params.dst_width;
  var dh = params.dst_height;
  var src_offset = 0u;
  var dst_offset = 0u;
  if id.z > 0u {
    src_offset = sw * sh + (id.z - 1u) * (sw * sh / 4u);
    dst_offset = dw * dh + (id.z - 1u) * (dw * dh / 4u);
    sw = sw / 2u;
    sh = sh / 2u;
    dw = dw / 2u;
    dh = dh / 2u;
  }
  let x0 = id.x * 4u;
  let y = id.y;
  if x0 >= dw || y >= dh {
    return;
  }
  let ratio_x = f32(sw) / f32(dw);
  let ratio_y = f32(sh) / f32(dh);
  let taps_x = max(1u, u32(ceil(ratio_x)));
  let taps_y = max(1u, u32(ceil(ratio_y)));
  var packed = 0u;
  for (var i = 0u; i < 4u; i++) {
    let x = x0 + i;
    var sum = 0.0;
    for (var ty = 0u; ty < taps_y; ty++) {
      let sy = (f32(y) + (f32(ty) + 0.5) / f32(taps_y)) * ratio_y - 0.5;
      for (var tx = 0u; tx < taps_x; tx++) {
        let sx = (f32(x) + (f32(tx) + 0.5) / f32(taps_x)) * ratio_x - 0.5;
        sum += bilinear(src_offset, sw, sh, sx, sy);
      }
    }
    let value = u32(clamp(round(sum / f32(taps_x * taps_y)), 0.0, 255.0));
    packed |= value << (i * 8u);
  }
  dst[(dst_offset + y * dw + x0) >> 2u] = packed;
}
";

impl GpuScaler {
  fn new() -> anyhow::Result<Self> {
    pollster::block_on(async {
      let instance = wgpu::Instance::default();
      let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
          power_preference: wgpu::PowerPreference::HighPerformance,
          ..Default::default()
        })
        .await?;
      info!(adapter = ?adapter.get_info().name, "GPU for the scaling rows");
      let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
          label: Some("scale"),
          ..Default::default()
        })
        .await?;
      let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("scale"),
        source: wgpu::ShaderSource::Wgsl(SCALE_SHADER.into()),
      });
      let entry = |binding: u32, ty: wgpu::BufferBindingType| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
          ty,
          has_dynamic_offset: false,
          min_binding_size: None,
        },
        count: None,
      };
      let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("scale"),
        entries: &[
          entry(0, wgpu::BufferBindingType::Uniform),
          entry(1, wgpu::BufferBindingType::Storage { read_only: true }),
          entry(2, wgpu::BufferBindingType::Storage { read_only: false }),
        ],
      });
      let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("scale"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
      });
      let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("scale"),
        layout: Some(&pipeline_layout),
        module: &module,
        entry_point: Some("scale"),
        compilation_options: Default::default(),
        cache: None,
      });
      Ok(Self {
        device,
        queue,
        layout,
        pipeline,
        maybe_buffers: None,
      })
    })
  }

  fn scale(&mut self, frame: &VideoFrame, target: VideoResolution) -> anyhow::Result<VideoFrame> {
    let source = frame.resolution();
    anyhow::ensure!(
      target.width.is_multiple_of(8) && frame.data().len().is_multiple_of(4),
      "the GPU scaler wants a target width that is a multiple of 8"
    );
    let is_current = self
      .maybe_buffers
      .as_ref()
      .is_some_and(|buffers| buffers.source == source && buffers.target == target);
    if !is_current {
      self.maybe_buffers = Some(self.create_buffers(source, target));
    }
    let buffers = self.maybe_buffers.as_ref().context("buffers")?;
    self.queue.write_buffer(&buffers.input, 0, frame.data());
    let mut encoder = self
      .device
      .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("scale") });
    {
      let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some("scale"),
        timestamp_writes: None,
      });
      pass.set_pipeline(&self.pipeline);
      pass.set_bind_group(0, &buffers.bind_group, &[]);
      pass.dispatch_workgroups(
        (u32::from(target.width) / 4).div_ceil(8),
        u32::from(target.height).div_ceil(8),
        3,
      );
    }
    encoder.copy_buffer_to_buffer(&buffers.output, 0, &buffers.staging, 0, buffers.output.size());
    self.queue.submit(Some(encoder.finish()));
    let slice = buffers.staging.slice(..);
    let (mapped, mapped_rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
      let _ = mapped.send(result);
    });
    self.device.poll(wgpu::PollType::wait_indefinitely())?;
    mapped_rx.recv()??;
    let data = slice.get_mapped_range()?.to_vec();
    buffers.staging.unmap();
    Ok(VideoFrame::new(target, frame.timestamp_us(), data)?)
  }

  fn create_buffers(&self, source: VideoResolution, target: VideoResolution) -> GpuBuffers {
    let i420_bytes = |size: VideoResolution| u64::from(size.width) * u64::from(size.height) * 3 / 2;
    let buffer = |label: &str, size: u64, usage: wgpu::BufferUsages| {
      self.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage,
        mapped_at_creation: false,
      })
    };
    let params = [
      u32::from(source.width),
      u32::from(source.height),
      u32::from(target.width),
      u32::from(target.height),
    ];
    let params_buffer = buffer(
      "params",
      16,
      wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    );
    let params_bytes: Vec<u8> = params.iter().flat_map(|value| value.to_le_bytes()).collect();
    self.queue.write_buffer(&params_buffer, 0, &params_bytes);
    let input = buffer(
      "input",
      i420_bytes(source),
      wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    );
    let output = buffer(
      "output",
      i420_bytes(target),
      wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
    );
    let staging = buffer(
      "staging",
      i420_bytes(target),
      wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
    );
    let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
      label: Some("scale"),
      layout: &self.layout,
      entries: &[
        wgpu::BindGroupEntry {
          binding: 0,
          resource: params_buffer.as_entire_binding(),
        },
        wgpu::BindGroupEntry {
          binding: 1,
          resource: input.as_entire_binding(),
        },
        wgpu::BindGroupEntry {
          binding: 2,
          resource: output.as_entire_binding(),
        },
      ],
    });
    GpuBuffers {
      source,
      target,
      input,
      output,
      staging,
      bind_group,
    }
  }
}

struct Row {
  name: String,
  average: Duration,
  worst: Duration,
  cpu_per_frame: Duration,
}

/// Runs `work` for [`FRAMES`] frames after a few warm-up frames: wall time per frame (latency)
/// and the process's CPU time per frame. `paced` spaces the frames 33 ms apart like a camera, so
/// threads that work between frames (hardware encoders, the GPU) count as they would in a call.
fn measure(name: &str, paced: bool, mut work: impl FnMut(u64) -> anyhow::Result<()>) -> anyhow::Result<Row> {
  for index in 0..5 {
    work(index)?;
  }
  let mut worst = Duration::ZERO;
  let mut total = Duration::ZERO;
  let cpu_start = cpu_time();
  let start = Instant::now();
  for index in 0..FRAMES {
    if paced {
      let due = start + FRAME_INTERVAL * index as u32;
      if let Some(wait) = due.checked_duration_since(Instant::now()) {
        std::thread::sleep(wait);
      }
    }
    let frame_start = Instant::now();
    work(index + 5)?;
    let took = frame_start.elapsed();
    worst = worst.max(took);
    total += took;
  }
  let row = Row {
    name: name.to_owned(),
    average: total / FRAMES as u32,
    worst,
    cpu_per_frame: cpu_time().saturating_sub(cpu_start) / FRAMES as u32,
  };
  info!(
    average_ms = format!("{:.2}", millis(row.average)),
    worst_ms = format!("{:.2}", millis(row.worst)),
    cpu_ms = format!("{:.2}", millis(row.cpu_per_frame)),
    "{name}"
  );
  Ok(row)
}

fn millis(duration: Duration) -> f64 {
  duration.as_secs_f64() * 1000.0
}

/// User plus system CPU time of the whole process so far.
#[cfg(unix)]
fn cpu_time() -> Duration {
  use nix::sys::resource::{UsageWho, getrusage};
  getrusage(UsageWho::RUSAGE_SELF).map_or(Duration::ZERO, |usage| {
    let to_duration = |time: nix::sys::time::TimeVal| {
      Duration::new(time.tv_sec().max(0) as u64, (time.tv_usec().max(0) * 1000) as u32)
    };
    to_duration(usage.user_time()) + to_duration(usage.system_time())
  })
}

/// Not measured outside Unix: the CPU column stays 0.
#[cfg(not(unix))]
fn cpu_time() -> Duration {
  Duration::ZERO
}
