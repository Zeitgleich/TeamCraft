//! Background effects composited with WebGL2 (#439): for browsers that read a frame back from
//! WebGPU too slowly (Firefox: about 80 ms a frame) or have no WebGPU at all.
//!
//! The steps are those of the WebGPU compositor (`effects/compositor.wgsl`): the mask is smoothed
//! over time, feathered and its edge sharpened; the background is blurred at a quarter of the size
//! or replaced by a picture scaled to cover the frame; the person goes on top. Everything stays on
//! the GPU: the camera's picture goes in as a texture straight from its `<video>`, and the result
//! is drawn into a canvas that the encoders take as a `VideoFrame`, so no frame is read back. Only
//! the small picture for the segmentation is (the model's input size, `readPixels`).
//!
//! The textures hold their rows top first, as the camera's picture comes; only the canvas' rows go
//! bottom up, so the last pass turns the picture over.

use std::sync::Arc;

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{
  HtmlCanvasElement, HtmlVideoElement, WebGl2RenderingContext as Gl, WebGlFramebuffer, WebGlProgram,
  WebGlShader, WebGlTexture, WebGlUniformLocation, WebGlVertexArrayObject,
};

use super::{dictionary, js_message, window};
use crate::effects::{
  BackgroundEffect, BackgroundImage, BlurStrength, EffectsConfig, EffectsError, MODEL_INPUT_HEIGHT,
  MODEL_INPUT_WIDTH, SegmentationMask,
};

/// The blurred background is computed at this fraction of the frame size (as with WebGPU).
const BACKGROUND_DOWNSCALE: u32 = 4;

/// A triangle that covers the whole target; `uv` is 0..1 across it.
const VERTEX: &str = r"#version 300 es
out vec2 uv;
void main() {
  vec2 corner = vec2(float((gl_VertexID & 1) << 2), float((gl_VertexID & 2) << 1));
  uv = corner * 0.5;
  gl_Position = vec4(corner - 1.0, 0.0, 1.0);
}
";

const HEADER: &str = r"#version 300 es
precision highp float;
precision highp int;
in vec2 uv;
out vec4 color;
";

/// The new mask blended into the history (the first one taken as it is).
const SMOOTH: &str = r"
uniform sampler2D current;
uniform sampler2D history;
uniform float smoothing;
uniform int has_history;
void main() {
  ivec2 at = ivec2(gl_FragCoord.xy);
  float now = clamp(texelFetch(current, at, 0).r, 0.0, 1.0);
  float value = has_history == 1 ? mix(texelFetch(history, at, 0).r, now, smoothing) : now;
  color = vec4(value, 0.0, 0.0, 1.0);
}
";

/// One direction of a Gaussian blur at the target's size (the mask's feather, the background);
/// `sharpen` maps the result to the mask's edge.
const BLUR: &str = r"
uniform sampler2D source;
uniform ivec2 direction;
uniform int radius;
uniform int sharpen;
uniform float edge_low;
uniform float edge_high;
void main() {
  ivec2 size = textureSize(source, 0);
  ivec2 at = ivec2(gl_FragCoord.xy);
  float sigma = max(float(radius) * 0.5, 0.5);
  vec4 sum = vec4(0.0);
  float weights = 0.0;
  for (int offset = -radius; offset <= radius; offset++) {
    ivec2 point = clamp(at + direction * offset, ivec2(0), size - 1);
    float weight = exp(-float(offset * offset) / (2.0 * sigma * sigma));
    sum += texelFetch(source, point, 0) * weight;
    weights += weight;
  }
  vec4 value = sum / weights;
  if (sharpen == 1) {
    value.r = smoothstep(edge_low, edge_high, value.r);
  }
  color = value;
}
";

/// A smaller copy of the camera's picture: four bilinear samples around each target pixel average
/// a 4x4 block of the source.
const SHRINK: &str = r"
uniform sampler2D source;
void main() {
  vec2 texel = 1.0 / vec2(textureSize(source, 0));
  color = 0.25 * (texture(source, uv + texel * vec2(-1.0, -1.0)) + texture(source, uv + texel * vec2(1.0, -1.0))
    + texture(source, uv + texel * vec2(-1.0, 1.0)) + texture(source, uv + texel * vec2(1.0, 1.0)));
}
";

/// The person over the background, into the canvas (whose rows go bottom up).
const COMPOSITE: &str = r"
uniform sampler2D frame;
uniform sampler2D mask;
uniform sampler2D background;
uniform int is_image;
void main() {
  vec2 at = vec2(uv.x, 1.0 - uv.y);
  vec3 back;
  if (is_image == 1) {
    vec2 frame_size = vec2(textureSize(frame, 0));
    vec2 image_size = vec2(textureSize(background, 0));
    float scale = max(frame_size.x / image_size.x, frame_size.y / image_size.y);
    back = texture(background, ((at - 0.5) * frame_size / scale + 0.5 * image_size) / image_size).rgb;
  } else {
    back = texture(background, at).rgb;
  }
  float person = texture(mask, at).r;
  color = vec4(mix(back, texture(frame, at).rgb, person), 1.0);
}
";

/// The WebGL2 side of the camera's effects.
pub struct GlCompositor {
  canvas: HtmlCanvasElement,
  gl: Gl,
  config: EffectsConfig,
  smooth: Program,
  blur: Program,
  shrink: Program,
  composite: Program,
  _vertices: WebGlVertexArrayObject,
  /// The camera's picture.
  frame: WebGlTexture,
  /// The model's input, read back for the segmentation.
  input: Target,
  maybe_image: Option<(Arc<BackgroundImage>, WebGlTexture)>,
  maybe_targets: Option<Targets>,
}

/// The size-dependent textures.
struct Targets {
  frame_size: (u32, u32),
  mask_size: (u32, u32),
  mask_in: WebGlTexture,
  /// The smoothed mask, new and last (they take turns).
  history: [Target; 2],
  newest: usize,
  has_history: bool,
  feather: Target,
  mask: Target,
  /// The blurred background and its scratch.
  small: [Target; 2],
}

/// A texture to draw into.
struct Target {
  texture: WebGlTexture,
  framebuffer: WebGlFramebuffer,
  width: u32,
  height: u32,
}

struct Program {
  program: WebGlProgram,
  uniforms: Vec<(&'static str, Option<WebGlUniformLocation>)>,
}

impl GlCompositor {
  /// A WebGL2 context on a canvas of its own (not shown: its pictures go to the encoders).
  pub fn new(config: EffectsConfig) -> Result<Self, EffectsError> {
    let canvas: HtmlCanvasElement = window()
      .map_err(|error| EffectsError::Gpu(error.to_string()))?
      .document()
      .ok_or_else(|| EffectsError::Gpu("no document".into()))?
      .create_element("canvas")
      .map_err(gl_error("canvas"))?
      .unchecked_into();
    let options = dictionary(&[
      ("alpha", JsValue::FALSE),
      ("antialias", JsValue::FALSE),
      ("depth", JsValue::FALSE),
      ("stencil", JsValue::FALSE),
      ("premultipliedAlpha", JsValue::FALSE),
    ])
    .map_err(gl_error("WebGL2 options"))?;
    let gl: Gl = canvas
      .get_context_with_context_options("webgl2", &options)
      .map_err(gl_error("WebGL2"))?
      .ok_or(EffectsError::NoAdapter)?
      .unchecked_into();
    let smooth = Program::new(&gl, SMOOTH, &["current", "history", "smoothing", "has_history"])?;
    let blur = Program::new(
      &gl,
      BLUR,
      &[
        "source",
        "direction",
        "radius",
        "sharpen",
        "edge_low",
        "edge_high",
      ],
    )?;
    let shrink = Program::new(&gl, SHRINK, &["source"])?;
    let composite = Program::new(&gl, COMPOSITE, &["frame", "mask", "background", "is_image"])?;
    // The triangle needs no buffers (gl_VertexID), but a vertex array must be bound.
    let vertices = gl
      .create_vertex_array()
      .ok_or_else(|| EffectsError::Gpu("no vertex array".into()))?;
    gl.bind_vertex_array(Some(&vertices));
    gl.pixel_storei(Gl::UNPACK_ALIGNMENT, 1);
    gl.pixel_storei(Gl::PACK_ALIGNMENT, 1);
    let frame = texture(&gl, Gl::LINEAR)?;
    let input = Target::new(&gl, Gl::RGBA8, MODEL_INPUT_WIDTH, MODEL_INPUT_HEIGHT, Gl::LINEAR)?;
    Ok(Self {
      canvas,
      gl,
      config,
      smooth,
      blur,
      shrink,
      composite,
      _vertices: vertices,
      frame,
      input,
      maybe_image: None,
      maybe_targets: None,
    })
  }

  /// The canvas the pictures are drawn into.
  pub fn canvas(&self) -> &HtmlCanvasElement {
    &self.canvas
  }

  /// Takes the current picture of `video` (`width` x `height`) as the frame to composite.
  pub fn take_picture(&mut self, video: &HtmlVideoElement) -> Result<(u32, u32), EffectsError> {
    if self.gl.is_context_lost() {
      return Err(EffectsError::Gpu("the WebGL2 context was lost".into()));
    }
    let gl = &self.gl;
    gl.bind_texture(Gl::TEXTURE_2D, Some(&self.frame));
    gl.tex_image_2d_with_u32_and_u32_and_html_video_element(
      Gl::TEXTURE_2D,
      0,
      Gl::RGBA as i32,
      Gl::RGBA,
      Gl::UNSIGNED_BYTE,
      video,
    )
    .map_err(gl_error("camera picture"))?;
    Ok((video.video_width(), video.video_height()))
  }

  /// The picture taken last at the model's input size, RGBA row by row, top first.
  pub fn model_input(&self) -> Result<Vec<u8>, EffectsError> {
    let gl = &self.gl;
    self.input.bind(gl);
    gl.use_program(Some(&self.shrink.program));
    self.shrink.texture(gl, "source", 0, &self.frame);
    gl.draw_arrays(Gl::TRIANGLES, 0, 3);
    let mut pixels = vec![0; MODEL_INPUT_WIDTH as usize * MODEL_INPUT_HEIGHT as usize * 4];
    gl.read_pixels_with_opt_u8_array(
      0,
      0,
      MODEL_INPUT_WIDTH as i32,
      MODEL_INPUT_HEIGHT as i32,
      Gl::RGBA,
      Gl::UNSIGNED_BYTE,
      Some(&mut pixels),
    )
    .map_err(gl_error("reading the model's input"))?;
    Ok(pixels)
  }

  /// Draws the picture taken last (`width` x `height`) with `effect` behind the person in `mask`
  /// into [`GlCompositor::canvas`].
  pub fn composite(
    &mut self,
    (width, height): (u32, u32),
    mask: &SegmentationMask,
    effect: &BackgroundEffect,
  ) -> Result<(), EffectsError> {
    let radius = match effect {
      BackgroundEffect::None => 0,
      BackgroundEffect::Blur(BlurStrength::Light) => 4,
      BackgroundEffect::Blur(BlurStrength::Strong) => 10,
      BackgroundEffect::Image(image) => {
        self.use_image(image)?;
        0
      },
    };
    if width == 0 || height == 0 {
      return Err(EffectsError::Gpu("the camera has no picture yet".into()));
    }
    self.prepare((width, height), (mask.width(), mask.height()))?;
    if self.canvas.width() != width || self.canvas.height() != height {
      self.canvas.set_width(width);
      self.canvas.set_height(height);
    }
    let Some(targets) = &mut self.maybe_targets else {
      return Err(EffectsError::Gpu("no targets".into()));
    };
    let gl = &self.gl;
    let values: Vec<u8> = mask
      .values()
      .iter()
      .map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u8)
      .collect();
    gl.bind_texture(Gl::TEXTURE_2D, Some(&targets.mask_in));
    gl.tex_sub_image_2d_with_i32_and_i32_and_u32_and_type_and_opt_u8_array(
      Gl::TEXTURE_2D,
      0,
      0,
      0,
      mask.width() as i32,
      mask.height() as i32,
      Gl::RED,
      Gl::UNSIGNED_BYTE,
      Some(&values),
    )
    .map_err(gl_error("mask"))?;

    // The mask: smoothed over time, feathered, its edge sharpened.
    let (last, newest) = (targets.newest, 1 - targets.newest);
    targets.history[newest].bind(gl);
    gl.use_program(Some(&self.smooth.program));
    self.smooth.texture(gl, "current", 0, &targets.mask_in);
    self
      .smooth
      .texture(gl, "history", 1, &targets.history[last].texture);
    self.smooth.float(gl, "smoothing", self.config.mask_smoothing);
    self.smooth.int(gl, "has_history", i32::from(targets.has_history));
    gl.draw_arrays(Gl::TRIANGLES, 0, 3);
    targets.newest = newest;
    targets.has_history = true;
    let (blur, config) = (&self.blur, &self.config);
    let feather = config.feather_radius as i32;
    let history = &targets.history[newest].texture;
    blur_pass(
      gl,
      blur,
      config,
      history,
      &targets.feather,
      (1, 0),
      feather,
      false,
    );
    blur_pass(
      gl,
      blur,
      config,
      &targets.feather.texture,
      &targets.mask,
      (0, 1),
      feather,
      true,
    );

    // The background: blurred at a quarter of the size, or the picture.
    let background = if radius > 0 {
      targets.small[0].bind(gl);
      gl.use_program(Some(&self.shrink.program));
      self.shrink.texture(gl, "source", 0, &self.frame);
      gl.draw_arrays(Gl::TRIANGLES, 0, 3);
      blur_pass(
        gl,
        blur,
        config,
        &targets.small[0].texture,
        &targets.small[1],
        (1, 0),
        radius,
        false,
      );
      blur_pass(
        gl,
        blur,
        config,
        &targets.small[1].texture,
        &targets.small[0],
        (0, 1),
        radius,
        false,
      );
      &targets.small[0].texture
    } else {
      match &self.maybe_image {
        Some((_, image)) => image,
        None => return Err(EffectsError::Gpu("no background picture".into())),
      }
    };

    gl.bind_framebuffer(Gl::FRAMEBUFFER, None);
    gl.viewport(0, 0, width as i32, height as i32);
    gl.use_program(Some(&self.composite.program));
    self.composite.texture(gl, "frame", 0, &self.frame);
    self.composite.texture(gl, "mask", 1, &targets.mask.texture);
    self.composite.texture(gl, "background", 2, background);
    self.composite.int(gl, "is_image", i32::from(radius == 0));
    gl.draw_arrays(Gl::TRIANGLES, 0, 3);
    Ok(())
  }

  /// Uploads a background picture that changed.
  fn use_image(&mut self, image: &Arc<BackgroundImage>) -> Result<(), EffectsError> {
    if self
      .maybe_image
      .as_ref()
      .is_some_and(|(current, _)| Arc::ptr_eq(current, image))
    {
      return Ok(());
    }
    let gl = &self.gl;
    let texture = texture(gl, Gl::LINEAR)?;
    gl.tex_image_2d_with_i32_and_i32_and_i32_and_format_and_type_and_opt_u8_array(
      Gl::TEXTURE_2D,
      0,
      Gl::RGBA8 as i32,
      image.width() as i32,
      image.height() as i32,
      0,
      Gl::RGBA,
      Gl::UNSIGNED_BYTE,
      Some(image.rgba()),
    )
    .map_err(gl_error("background picture"))?;
    if let Some((_, old)) = self.maybe_image.replace((Arc::clone(image), texture)) {
      gl.delete_texture(Some(&old));
    }
    Ok(())
  }

  /// (Re)creates the textures for this frame and mask size; the history survives an unchanged
  /// size.
  fn prepare(&mut self, frame_size: (u32, u32), mask_size: (u32, u32)) -> Result<(), EffectsError> {
    if self
      .maybe_targets
      .as_ref()
      .is_some_and(|targets| targets.frame_size == frame_size && targets.mask_size == mask_size)
    {
      return Ok(());
    }
    let gl = &self.gl;
    if let Some(old) = self.maybe_targets.take() {
      old.delete(gl);
    }
    let (mask_width, mask_height) = mask_size;
    let mask_in = texture(gl, Gl::NEAREST)?;
    gl.tex_image_2d_with_i32_and_i32_and_i32_and_format_and_type_and_opt_u8_array(
      Gl::TEXTURE_2D,
      0,
      Gl::R8 as i32,
      mask_width as i32,
      mask_height as i32,
      0,
      Gl::RED,
      Gl::UNSIGNED_BYTE,
      None,
    )
    .map_err(gl_error("mask"))?;
    let small_size = (
      (frame_size.0 / BACKGROUND_DOWNSCALE).max(1),
      (frame_size.1 / BACKGROUND_DOWNSCALE).max(1),
    );
    let mask_target = |filter| Target::new(gl, Gl::R8, mask_width, mask_height, filter);
    let small_target = || Target::new(gl, Gl::RGBA8, small_size.0, small_size.1, Gl::LINEAR);
    self.maybe_targets = Some(Targets {
      frame_size,
      mask_size,
      mask_in,
      history: [mask_target(Gl::NEAREST)?, mask_target(Gl::NEAREST)?],
      newest: 0,
      has_history: false,
      feather: mask_target(Gl::NEAREST)?,
      mask: mask_target(Gl::LINEAR)?,
      small: [small_target()?, small_target()?],
    });
    Ok(())
  }
}

impl Drop for GlCompositor {
  fn drop(&mut self) {
    // Browsers keep only a few WebGL contexts per page: give this one back at once.
    if let Ok(Some(extension)) = self.gl.get_extension("WEBGL_lose_context") {
      let _ = js_sys::Reflect::get(&extension, &JsValue::from_str("loseContext"))
        .ok()
        .and_then(|lose| lose.dyn_into::<js_sys::Function>().ok())
        .map(|lose| lose.call0(&extension));
    }
  }
}

impl Targets {
  fn delete(self, gl: &Gl) {
    gl.delete_texture(Some(&self.mask_in));
    for target in self
      .history
      .into_iter()
      .chain([self.feather, self.mask])
      .chain(self.small)
    {
      target.delete(gl);
    }
  }
}

impl Target {
  fn new(gl: &Gl, format: u32, width: u32, height: u32, filter: u32) -> Result<Self, EffectsError> {
    let texture = texture(gl, filter)?;
    gl.tex_storage_2d(Gl::TEXTURE_2D, 1, format, width as i32, height as i32);
    let framebuffer = gl
      .create_framebuffer()
      .ok_or_else(|| EffectsError::Gpu("no framebuffer".into()))?;
    gl.bind_framebuffer(Gl::FRAMEBUFFER, Some(&framebuffer));
    gl.framebuffer_texture_2d(
      Gl::FRAMEBUFFER,
      Gl::COLOR_ATTACHMENT0,
      Gl::TEXTURE_2D,
      Some(&texture),
      0,
    );
    if gl.check_framebuffer_status(Gl::FRAMEBUFFER) != Gl::FRAMEBUFFER_COMPLETE {
      return Err(EffectsError::Gpu("a framebuffer is incomplete".into()));
    }
    Ok(Self {
      texture,
      framebuffer,
      width,
      height,
    })
  }

  fn bind(&self, gl: &Gl) {
    gl.bind_framebuffer(Gl::FRAMEBUFFER, Some(&self.framebuffer));
    gl.viewport(0, 0, self.width as i32, self.height as i32);
  }

  fn delete(self, gl: &Gl) {
    gl.delete_framebuffer(Some(&self.framebuffer));
    gl.delete_texture(Some(&self.texture));
  }
}

impl Program {
  fn new(gl: &Gl, fragment: &str, names: &[&'static str]) -> Result<Self, EffectsError> {
    let vertex = shader(gl, Gl::VERTEX_SHADER, VERTEX)?;
    let fragment = shader(gl, Gl::FRAGMENT_SHADER, &format!("{HEADER}{fragment}"))?;
    let program = gl
      .create_program()
      .ok_or_else(|| EffectsError::Gpu("no program".into()))?;
    gl.attach_shader(&program, &vertex);
    gl.attach_shader(&program, &fragment);
    gl.link_program(&program);
    if !gl
      .get_program_parameter(&program, Gl::LINK_STATUS)
      .as_bool()
      .unwrap_or(false)
    {
      let log = gl.get_program_info_log(&program).unwrap_or_default();
      return Err(EffectsError::Gpu(format!("linking a shader: {log}")));
    }
    let uniforms = names
      .iter()
      .map(|name| (*name, gl.get_uniform_location(&program, name)))
      .collect();
    Ok(Self { program, uniforms })
  }

  fn location(&self, name: &str) -> Option<&WebGlUniformLocation> {
    self
      .uniforms
      .iter()
      .find(|(known, _)| *known == name)
      .and_then(|(_, location)| location.as_ref())
  }

  fn texture(&self, gl: &Gl, name: &str, unit: u32, texture: &WebGlTexture) {
    gl.active_texture(Gl::TEXTURE0 + unit);
    gl.bind_texture(Gl::TEXTURE_2D, Some(texture));
    self.int(gl, name, unit as i32);
  }

  fn int(&self, gl: &Gl, name: &str, value: i32) {
    if let Some(location) = self.location(name) {
      gl.uniform1i(Some(location), value);
    }
  }

  fn float(&self, gl: &Gl, name: &str, value: f32) {
    if let Some(location) = self.location(name) {
      gl.uniform1f(Some(location), value);
    }
  }
}

/// One direction of [`BLUR`] from `source` into `target`.
#[allow(clippy::too_many_arguments)] // One pass of one shader; a struct would only move the list.
fn blur_pass(
  gl: &Gl,
  program: &Program,
  config: &EffectsConfig,
  source: &WebGlTexture,
  target: &Target,
  direction: (i32, i32),
  radius: i32,
  sharpen: bool,
) {
  target.bind(gl);
  gl.use_program(Some(&program.program));
  program.texture(gl, "source", 0, source);
  if let Some(location) = program.location("direction") {
    gl.uniform2i(Some(location), direction.0, direction.1);
  }
  program.int(gl, "radius", radius);
  program.int(gl, "sharpen", i32::from(sharpen));
  program.float(gl, "edge_low", config.edge_low);
  program.float(gl, "edge_high", config.edge_high);
  gl.draw_arrays(Gl::TRIANGLES, 0, 3);
}

fn shader(gl: &Gl, kind: u32, source: &str) -> Result<WebGlShader, EffectsError> {
  let shader = gl
    .create_shader(kind)
    .ok_or_else(|| EffectsError::Gpu("no shader".into()))?;
  gl.shader_source(&shader, source);
  gl.compile_shader(&shader);
  if gl
    .get_shader_parameter(&shader, Gl::COMPILE_STATUS)
    .as_bool()
    .unwrap_or(false)
  {
    Ok(shader)
  } else {
    let log = gl.get_shader_info_log(&shader).unwrap_or_default();
    Err(EffectsError::Gpu(format!("compiling a shader: {log}")))
  }
}

/// A 2D texture with `filter`, clamped at the edges, bound.
fn texture(gl: &Gl, filter: u32) -> Result<WebGlTexture, EffectsError> {
  let texture = gl
    .create_texture()
    .ok_or_else(|| EffectsError::Gpu("no texture".into()))?;
  gl.bind_texture(Gl::TEXTURE_2D, Some(&texture));
  for (parameter, value) in [
    (Gl::TEXTURE_MIN_FILTER, filter),
    (Gl::TEXTURE_MAG_FILTER, filter),
    (Gl::TEXTURE_WRAP_S, Gl::CLAMP_TO_EDGE),
    (Gl::TEXTURE_WRAP_T, Gl::CLAMP_TO_EDGE),
  ] {
    gl.tex_parameteri(Gl::TEXTURE_2D, parameter, value as i32);
  }
  Ok(texture)
}

fn gl_error(what: &'static str) -> impl Fn(JsValue) -> EffectsError {
  move |error| EffectsError::Gpu(format!("{what}: {}", js_message(&error)))
}
