// One frame of a clip: the background gradient and the frame's shadow (with a background),
// then the recording, placed by the view and zoom, through the rounded frame. Pixel
// coordinates have a top-left origin, as the canvas and the recording do.

struct Scene {
    // Canvas width and height, the recording's width and height.
    size: vec4f,
    // The frame inside the canvas: x, y, w, h.
    inner: vec4f,
    // The recording's pixels the frame shows: x, y, w, h.
    shown: vec4f,
    // Corner radius, 1 with a background, sample taps per side, unused.
    flags: vec4f,
    from_color: vec4f,
    to_color: vec4f,
    // Shadow opacity, blur (Gaussian sigma), downward offset, unused.
    shadow: vec4f,
};

@group(0) @binding(0) var<uniform> scene: Scene;
@group(0) @binding(1) var recording: texture_2d<f32>;
@group(0) @binding(2) var linear: sampler;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    // One triangle over the whole canvas.
    let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4f(p * 2.0 - 1.0, 0.0, 1.0);
}

// Signed distance from `p` to a rounded rectangle: negative inside.
fn rounded_rect(p: vec2f, rect: vec4f, radius: f32) -> f32 {
    let half = rect.zw * 0.5;
    let r = min(radius, min(half.x, half.y));
    let q = abs(p - (rect.xy + half)) - half + vec2f(r);
    return length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

// The recording at canvas pixel `p`: black outside it. Downscaled, it averages taps² samples
// spread over the canvas pixel's footprint, so detail doesn't alias.
fn recording_at(p: vec2f) -> vec3f {
    let k = min(scene.inner.z / scene.shown.z, scene.inner.w / scene.shown.w);
    let origin = scene.inner.xy + (scene.inner.zw - scene.shown.zw * k) * 0.5;
    let at = scene.shown.xy + (p - origin) / k;
    let size = scene.size.zw;
    if (at.x < 0.0 || at.y < 0.0 || at.x > size.x || at.y > size.y) {
        return vec3f(0.0);
    }
    let taps = u32(scene.flags.z);
    var sum = vec3f(0.0);
    for (var i = 0u; i < taps; i++) {
        for (var j = 0u; j < taps; j++) {
            let offset = (vec2f(f32(i), f32(j)) + 0.5) / f32(taps) - 0.5;
            sum += textureSampleLevel(recording, linear, (at + offset / k) / size, 0.0).rgb;
        }
    }
    return sum / f32(taps * taps);
}

@fragment
fn fs(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let p = position.xy;
    let radius = scene.flags.x;
    var color = vec3f(0.0);
    if (scene.flags.y > 0.5) {
        // Top-left to bottom-right, as the gradient's start and end points are.
        let t = clamp((p.x / scene.size.x + p.y / scene.size.y) * 0.5, 0.0, 1.0);
        color = mix(scene.from_color.rgb, scene.to_color.rgb, t);
        let shadow_rect = scene.inner + vec4f(0.0, scene.shadow.z, 0.0, 0.0);
        let d = rounded_rect(p, shadow_rect, radius);
        let shade = scene.shadow.x * (1.0 - smoothstep(-2.0 * scene.shadow.y, 2.0 * scene.shadow.y, d));
        color *= 1.0 - shade;
    }
    let cover = clamp(0.5 - rounded_rect(p, scene.inner, radius), 0.0, 1.0);
    if (cover > 0.0) {
        color = mix(color, recording_at(p), cover);
    }
    return vec4f(color, 1.0);
}
