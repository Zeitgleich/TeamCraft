// Background effects on I420 frames (PLAN.md §8): temporal mask smoothing, edge feathering, a
// blurred or replaced background, composited and written back as I420. Compute passes over
// storage buffers only, so the same shader runs on Vulkan, Metal, D3D12 and in browsers (WebGPU).
//
// Colours are BT.601 limited range, like the encoders. The frame width must be a multiple of 8
// (four packed chroma bytes per invocation).

struct Params {
  width: u32,
  height: u32,
  mask_width: u32,
  mask_height: u32,
  small_width: u32,
  small_height: u32,
  image_width: u32,
  image_height: u32,
  // 1: blur, 2: background image.
  mode: u32,
  // Background blur radius in small-buffer pixels.
  radius: u32,
  // Mask feather radius in mask pixels.
  feather_radius: u32,
  // 0 on the first frame: no history to smooth with.
  has_history: u32,
  // Weight of the new mask against the history (1: no temporal smoothing).
  smoothing: f32,
  edge_low: f32,
  edge_high: f32,
  _padding: f32,
}

@group(0) @binding(0) var<uniform> params: Params;
// The input frame, I420 bytes packed four to a word.
@group(0) @binding(1) var<storage, read> frame: array<u32>;
// The segmenter's person probability, mask_width x mask_height.
@group(0) @binding(2) var<storage, read> mask_in: array<f32>;
// Three mask-sized sections: 0 smoothed history, 1 feather scratch, 2 final mask.
@group(0) @binding(3) var<storage, read_write> masks: array<f32>;
// Two small_width x small_height RGB sections for the blurred background.
@group(0) @binding(4) var<storage, read_write> small: array<vec4<f32>>;
// The background image, RGBA8 packed.
@group(0) @binding(5) var<storage, read> image: array<u32>;
// The output frame, I420 bytes packed four to a word.
@group(0) @binding(6) var<storage, read_write> output: array<u32>;

fn mask_size() -> u32 {
  return params.mask_width * params.mask_height;
}

fn small_size() -> u32 {
  return params.small_width * params.small_height;
}

fn gaussian(offset: f32, radius: u32) -> f32 {
  let sigma = max(f32(radius) * 0.5, 0.5);
  return exp(-offset * offset / (2.0 * sigma * sigma));
}

// ---- Mask ---------------------------------------------------------------------------------

@compute @workgroup_size(8, 8)
fn smooth_mask(@builtin(global_invocation_id) id: vec3<u32>) {
  if (id.x >= params.mask_width || id.y >= params.mask_height) {
    return;
  }
  let index = id.y * params.mask_width + id.x;
  let current = clamp(mask_in[index], 0.0, 1.0);
  if (params.has_history == 0u) {
    masks[index] = current;
  } else {
    masks[index] = mix(masks[index], current, params.smoothing);
  }
}

@compute @workgroup_size(8, 8)
fn feather_horizontal(@builtin(global_invocation_id) id: vec3<u32>) {
  if (id.x >= params.mask_width || id.y >= params.mask_height) {
    return;
  }
  let radius = i32(params.feather_radius);
  var sum = 0.0;
  var weights = 0.0;
  for (var offset = -radius; offset <= radius; offset++) {
    let x = clamp(i32(id.x) + offset, 0, i32(params.mask_width) - 1);
    let weight = gaussian(f32(offset), params.feather_radius);
    sum += masks[id.y * params.mask_width + u32(x)] * weight;
    weights += weight;
  }
  masks[mask_size() + id.y * params.mask_width + id.x] = sum / weights;
}

@compute @workgroup_size(8, 8)
fn feather_vertical(@builtin(global_invocation_id) id: vec3<u32>) {
  if (id.x >= params.mask_width || id.y >= params.mask_height) {
    return;
  }
  let radius = i32(params.feather_radius);
  var sum = 0.0;
  var weights = 0.0;
  for (var offset = -radius; offset <= radius; offset++) {
    let y = clamp(i32(id.y) + offset, 0, i32(params.mask_height) - 1);
    let weight = gaussian(f32(offset), params.feather_radius);
    sum += masks[mask_size() + u32(y) * params.mask_width + id.x] * weight;
    weights += weight;
  }
  let feathered = sum / weights;
  masks[2u * mask_size() + id.y * params.mask_width + id.x] =
    smoothstep(params.edge_low, params.edge_high, feathered);
}

// ---- Background blur ------------------------------------------------------------------------

fn frame_byte(index: u32) -> f32 {
  return f32((frame[index >> 2u] >> ((index & 3u) * 8u)) & 0xffu);
}

fn frame_rgb(x: u32, y: u32) -> vec3<f32> {
  let width = params.width;
  let luma_size = width * params.height;
  let chroma_index = (y / 2u) * (width / 2u) + x / 2u;
  let luma = frame_byte(y * width + x) - 16.0;
  let u = frame_byte(luma_size + chroma_index) - 128.0;
  let v = frame_byte(luma_size + luma_size / 4u + chroma_index) - 128.0;
  return clamp(
    vec3<f32>(
      1.164 * luma + 1.596 * v,
      1.164 * luma - 0.392 * u - 0.813 * v,
      1.164 * luma + 2.017 * u,
    ),
    vec3<f32>(0.0),
    vec3<f32>(255.0),
  );
}

// Box-filters the frame into the small buffer (section 0).
@compute @workgroup_size(8, 8)
fn downsample(@builtin(global_invocation_id) id: vec3<u32>) {
  if (id.x >= params.small_width || id.y >= params.small_height) {
    return;
  }
  let step_x = params.width / params.small_width;
  let step_y = params.height / params.small_height;
  var sum = vec3<f32>(0.0);
  for (var dy = 0u; dy < step_y; dy++) {
    for (var dx = 0u; dx < step_x; dx++) {
      sum += frame_rgb(id.x * step_x + dx, id.y * step_y + dy);
    }
  }
  small[id.y * params.small_width + id.x] = vec4<f32>(sum / f32(step_x * step_y), 1.0);
}

@compute @workgroup_size(8, 8)
fn blur_horizontal(@builtin(global_invocation_id) id: vec3<u32>) {
  if (id.x >= params.small_width || id.y >= params.small_height) {
    return;
  }
  let radius = i32(params.radius);
  var sum = vec4<f32>(0.0);
  var weights = 0.0;
  for (var offset = -radius; offset <= radius; offset++) {
    let x = clamp(i32(id.x) + offset, 0, i32(params.small_width) - 1);
    let weight = gaussian(f32(offset), params.radius);
    sum += small[id.y * params.small_width + u32(x)] * weight;
    weights += weight;
  }
  small[small_size() + id.y * params.small_width + id.x] = sum / weights;
}

@compute @workgroup_size(8, 8)
fn blur_vertical(@builtin(global_invocation_id) id: vec3<u32>) {
  if (id.x >= params.small_width || id.y >= params.small_height) {
    return;
  }
  let radius = i32(params.radius);
  var sum = vec4<f32>(0.0);
  var weights = 0.0;
  for (var offset = -radius; offset <= radius; offset++) {
    let y = clamp(i32(id.y) + offset, 0, i32(params.small_height) - 1);
    let weight = gaussian(f32(offset), params.radius);
    sum += small[small_size() + u32(y) * params.small_width + id.x] * weight;
    weights += weight;
  }
  small[id.y * params.small_width + id.x] = sum / weights;
}

// ---- Composite ------------------------------------------------------------------------------

// Bilinear sample of the final mask at a frame pixel.
fn person_at(x: u32, y: u32) -> f32 {
  let position = vec2<f32>(
    (f32(x) + 0.5) * f32(params.mask_width) / f32(params.width) - 0.5,
    (f32(y) + 0.5) * f32(params.mask_height) / f32(params.height) - 0.5,
  );
  let base = vec2<i32>(floor(position));
  let fraction = position - floor(position);
  let limit = vec2<i32>(i32(params.mask_width) - 1, i32(params.mask_height) - 1);
  let offset = 2u * mask_size();
  var values: array<f32, 4>;
  for (var corner = 0u; corner < 4u; corner++) {
    let point = clamp(base + vec2<i32>(i32(corner & 1u), i32(corner >> 1u)), vec2<i32>(0), limit);
    values[corner] = masks[offset + u32(point.y) * params.mask_width + u32(point.x)];
  }
  return mix(mix(values[0], values[1], fraction.x), mix(values[2], values[3], fraction.x), fraction.y);
}

fn blurred_at(x: u32, y: u32) -> vec3<f32> {
  let position = vec2<f32>(
    (f32(x) + 0.5) * f32(params.small_width) / f32(params.width) - 0.5,
    (f32(y) + 0.5) * f32(params.small_height) / f32(params.height) - 0.5,
  );
  let base = vec2<i32>(floor(position));
  let fraction = position - floor(position);
  let limit = vec2<i32>(i32(params.small_width) - 1, i32(params.small_height) - 1);
  var values: array<vec3<f32>, 4>;
  for (var corner = 0u; corner < 4u; corner++) {
    let point = clamp(base + vec2<i32>(i32(corner & 1u), i32(corner >> 1u)), vec2<i32>(0), limit);
    values[corner] = small[u32(point.y) * params.small_width + u32(point.x)].rgb;
  }
  return mix(mix(values[0], values[1], fraction.x), mix(values[2], values[3], fraction.x), fraction.y);
}

// The background image scaled to cover the frame (cropped, aspect kept), nearest sample.
fn image_at(x: u32, y: u32) -> vec3<f32> {
  let scale = max(
    f32(params.width) / f32(params.image_width),
    f32(params.height) / f32(params.image_height),
  );
  let source = vec2<f32>(
    (f32(x) + 0.5 - f32(params.width) * 0.5) / scale + f32(params.image_width) * 0.5,
    (f32(y) + 0.5 - f32(params.height) * 0.5) / scale + f32(params.image_height) * 0.5,
  );
  let point = clamp(
    vec2<u32>(source),
    vec2<u32>(0u),
    vec2<u32>(params.image_width - 1u, params.image_height - 1u),
  );
  return unpack4x8unorm(image[point.y * params.image_width + point.x]).rgb * 255.0;
}

fn composited(x: u32, y: u32) -> vec3<f32> {
  var background: vec3<f32>;
  if (params.mode == 2u) {
    background = image_at(x, y);
  } else {
    background = blurred_at(x, y);
  }
  return mix(background, frame_rgb(x, y), person_at(x, y));
}

fn to_byte(value: f32) -> u32 {
  return u32(clamp(round(value), 0.0, 255.0));
}

fn luma_of(rgb: vec3<f32>) -> f32 {
  return 0.257 * rgb.r + 0.504 * rgb.g + 0.098 * rgb.b + 16.0;
}

// Four horizontal pixels per invocation, packed into one output word.
@compute @workgroup_size(8, 8)
fn composite_luma(@builtin(global_invocation_id) id: vec3<u32>) {
  let x0 = id.x * 4u;
  if (x0 >= params.width || id.y >= params.height) {
    return;
  }
  var word = 0u;
  for (var i = 0u; i < 4u; i++) {
    word |= to_byte(luma_of(composited(x0 + i, id.y))) << (i * 8u);
  }
  output[(id.y * params.width + x0) / 4u] = word;
}

// Four horizontal chroma samples (an 8x2 pixel block) per invocation, into the U and V planes.
@compute @workgroup_size(8, 8)
fn composite_chroma(@builtin(global_invocation_id) id: vec3<u32>) {
  let chroma_width = params.width / 2u;
  let cx0 = id.x * 4u;
  if (cx0 >= chroma_width || id.y >= params.height / 2u) {
    return;
  }
  var u_word = 0u;
  var v_word = 0u;
  for (var i = 0u; i < 4u; i++) {
    let x = (cx0 + i) * 2u;
    let y = id.y * 2u;
    let rgb = (composited(x, y) + composited(x + 1u, y) + composited(x, y + 1u) + composited(x + 1u, y + 1u)) * 0.25;
    let u = -0.148 * rgb.r - 0.291 * rgb.g + 0.439 * rgb.b + 128.0;
    let v = 0.439 * rgb.r - 0.368 * rgb.g - 0.071 * rgb.b + 128.0;
    u_word |= to_byte(u) << (i * 8u);
    v_word |= to_byte(v) << (i * 8u);
  }
  let luma_size = params.width * params.height;
  let offset = id.y * chroma_width + cx0;
  output[(luma_size + offset) / 4u] = u_word;
  output[(luma_size + luma_size / 4u + offset) / 4u] = v_word;
}
