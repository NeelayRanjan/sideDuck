@group(0) @binding(0) var input_image: texture_2d<f32>;
@group(0) @binding(1) var output_image: texture_storage_2d<rgba32float, write>;

fn input_pixel(position: vec2<u32>) -> vec4<f32> {
    if (all(position < textureDimensions(input_image))) {
        return textureLoad(input_image, vec2<i32>(position), 0);
    }
    return vec4<f32>(0.0);
}

@compute @workgroup_size(16, 16)
fn downsample(@builtin(global_invocation_id) id: vec3<u32>) {
    let output_size = textureDimensions(output_image);
    if (any(id.xy >= output_size)) {
        return;
    }
    let origin = id.xy * 2u;
    // Match the scalar pyramid's traversal and premultiplied linear arithmetic.
    var color = input_pixel(origin) * 0.25;
    color += input_pixel(origin + vec2<u32>(1u, 0u)) * 0.25;
    color += input_pixel(origin + vec2<u32>(0u, 1u)) * 0.25;
    color += input_pixel(origin + vec2<u32>(1u, 1u)) * 0.25;
    textureStore(output_image, vec2<i32>(id.xy), color);
}
