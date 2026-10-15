@group(0) @binding(0) var frame: texture_2d<f32>;
@group(0) @binding(1) var<uniform> surface_size: vec4<f32>;

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let positions = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(positions[index], 0.0, 1.0);
}

fn load_clamped(position: vec2<i32>, size: vec2<i32>) -> vec4<f32> {
    return textureLoad(frame, clamp(position, vec2<i32>(0), size - vec2<i32>(1)), 0);
}

fn sample_frame(position: vec2<f32>) -> vec4<f32> {
    let size = textureDimensions(frame);
    if all(size == vec2<u32>(surface_size.xy)) {
        // Preserve exact pixel correspondence and a single read for 1:1 views.
        return textureLoad(frame, vec2<i32>(position), 0);
    }
    let scale = min(surface_size.x / f32(size.x), surface_size.y / f32(size.y));
    let displayed = vec2<f32>(size) * scale;
    let local = position - (surface_size.xy - displayed) * 0.5;
    if any(local < vec2<f32>(0.0)) || any(local >= displayed) {
        return vec4<f32>(1.0);
    }
    // RGBA32Float is unfilterable without optional GPU features. Interpolate
    // premultiplied linear components manually before alpha-over and transfer.
    let source = local / scale - vec2<f32>(0.5);
    let base = vec2<i32>(floor(source));
    let fraction = fract(source);
    let extent = vec2<i32>(size);
    let upper = mix(load_clamped(base, extent), load_clamped(base + vec2<i32>(1, 0), extent), fraction.x);
    let lower = mix(load_clamped(base + vec2<i32>(0, 1), extent), load_clamped(base + vec2<i32>(1, 1), extent), fraction.x);
    return mix(upper, lower, fraction.y);
}

@fragment
fn fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = sample_frame(position.xy);
    // The sRGB swapchain applies its transfer curve. Transparency is displayed
    // over white without changing the original premultiplied compositing image.
    return vec4<f32>(pixel.rgb + vec3<f32>(1.0 - pixel.a), 1.0);
}
