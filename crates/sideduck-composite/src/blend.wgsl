// Linear-light, premultiplied source-over; mode equations follow W3C Compositing.
struct Parameters {
    mode: u32,
    opacity: f32,
    width: u32,
    height: u32,
};

@group(0) @binding(0) var bottom: texture_2d<f32>;
@group(0) @binding(1) var top: texture_2d<f32>;
@group(0) @binding(2) var output_image: texture_storage_2d<rgba32float, write>;
@group(0) @binding(3) var<uniform> parameters: Parameters;

fn unit(value: f32) -> f32 {
    if !(abs(value) <= 3.402823466e38) { return 0.0; }
    return clamp(value, 0.0, 1.0);
}

fn premultiplied(value: vec4<f32>) -> vec4<f32> {
    let alpha = unit(value.a);
    return vec4<f32>(min(vec3<f32>(unit(value.r), unit(value.g), unit(value.b)), vec3<f32>(alpha)), alpha);
}

fn luminosity(c: vec3<f32>) -> f32 {
    return 0.3 * c.r + 0.59 * c.g + 0.11 * c.b;
}

fn saturation(c: vec3<f32>) -> f32 {
    return max(max(c.r, c.g), c.b) - min(min(c.r, c.g), c.b);
}

fn clip_color(value: vec3<f32>) -> vec3<f32> {
    var c = value;
    let l = luminosity(c);
    let n = min(min(c.r, c.g), c.b);
    let x = max(max(c.r, c.g), c.b);
    if n < 0.0 {
        if l - n > 0.0 {
            c = vec3<f32>(l) + ((c - vec3<f32>(l)) * l) / (l - n);
        } else {
            c = vec3<f32>(0.0);
        }
    }
    if x > 1.0 {
        if x - l > 0.0 {
            c = vec3<f32>(l) + ((c - vec3<f32>(l)) * (1.0 - l)) / (x - l);
        } else {
            c = vec3<f32>(1.0);
        }
    }
    return clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
}

fn set_luminosity(c: vec3<f32>, l: f32) -> vec3<f32> {
    return clip_color(c + vec3<f32>(l - luminosity(c)));
}

fn set_saturation(value: vec3<f32>, s: f32) -> vec3<f32> {
    let low = min(min(value.r, value.g), value.b);
    let high = max(max(value.r, value.g), value.b);
    if high > low {
        return (value - vec3<f32>(low)) * s / (high - low);
    }
    return vec3<f32>(0.0);
}

fn separable(b: f32, s: f32, mode: u32) -> f32 {
    switch mode {
        case 0u: { return s; }
        case 1u: { return b * s; }
        case 2u: { return b + s - b * s; }
        case 3u: {
            if b <= 0.5 { return 2.0 * b * s; }
            return 1.0 - 2.0 * (1.0 - b) * (1.0 - s);
        }
        case 4u: { return min(b, s); }
        case 5u: { return max(b, s); }
        case 6u: {
            if b == 0.0 { return 0.0; }
            if s == 1.0 { return 1.0; }
            return min(1.0, b / (1.0 - s));
        }
        case 7u: {
            if b == 1.0 { return 1.0; }
            if s == 0.0 { return 0.0; }
            return 1.0 - min(1.0, (1.0 - b) / s);
        }
        case 8u: {
            if s <= 0.5 { return 2.0 * b * s; }
            return 1.0 - 2.0 * (1.0 - b) * (1.0 - s);
        }
        case 9u: {
            if s <= 0.5 { return b - (1.0 - 2.0 * s) * b * (1.0 - b); }
            var d = sqrt(b);
            if b <= 0.25 { d = ((16.0 * b - 12.0) * b + 4.0) * b; }
            return b + (2.0 * s - 1.0) * (d - b);
        }
        case 10u: { return abs(b - s); }
        case 11u: { return b + s - 2.0 * b * s; }
        case 16u: { return min(1.0, b + s); }
        case 17u: { return max(0.0, b - s); }
        default: { return s; }
    }
}

fn blend_color(b: vec3<f32>, s: vec3<f32>, mode: u32) -> vec3<f32> {
    switch mode {
        case 12u: { return set_luminosity(set_saturation(s, saturation(b)), luminosity(b)); }
        case 13u: { return set_luminosity(set_saturation(b, saturation(s)), luminosity(b)); }
        case 14u: { return set_luminosity(s, luminosity(b)); }
        case 15u: { return set_luminosity(b, luminosity(s)); }
        default: { return vec3<f32>(separable(b.r, s.r, mode), separable(b.g, s.g, mode), separable(b.b, s.b, mode)); }
    }
}

@compute @workgroup_size(16, 16)
fn composite(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= parameters.width || id.y >= parameters.height { return; }
    let position = vec2<i32>(id.xy);
    let b = premultiplied(textureLoad(bottom, position, 0));
    let source = premultiplied(textureLoad(top, position, 0));
    let s = source * unit(parameters.opacity);
    let clipping = (parameters.mode & 0x80000000u) != 0u;
    let mode = parameters.mode & 0x7fffffffu;
    if s.a == 0.0 {
        textureStore(output_image, position, b);
        return;
    }
    if b.a == 0.0 {
        textureStore(output_image, position, select(s, vec4<f32>(0.0), clipping));
        return;
    }
    var result: vec4<f32>;
    if mode == 0u {
        if clipping {
            result = vec4<f32>(s.rgb * b.a + b.rgb * (1.0 - s.a), b.a);
        } else {
            result = s + b * (1.0 - s.a);
        }
    } else {
        var cb = vec3<f32>(0.0);
        var cs = vec3<f32>(0.0);
        if b.a > 0.0 { cb = b.rgb / b.a; }
        if source.a > 0.0 { cs = source.rgb / source.a; }
        let mixed = blend_color(cb, cs, mode);
        if clipping {
            result = vec4<f32>((1.0 - s.a) * b.rgb + b.a * s.a * mixed, b.a);
        } else {
            let color = (1.0 - s.a) * b.rgb + (1.0 - b.a) * s.rgb + b.a * s.a * mixed;
            result = vec4<f32>(color, s.a + b.a * (1.0 - s.a));
        }
    }
    textureStore(output_image, position, premultiplied(result));
}

@compute @workgroup_size(16, 16)
fn clear(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(output_image);
    if id.x < size.x && id.y < size.y {
        textureStore(output_image, vec2<i32>(id.xy), vec4<f32>(0.0));
    }
}
