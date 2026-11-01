// Всё, что строится поверх готовой сцены: копия на экран, пирамида уменьшенных
// копий, размытие по Гауссу в два прохода и стеклянные панели.
// Значения везде в sRGB, как в макете; в линейные — только на выходе в окно.

struct P {
    a: vec4f,
    b: vec4f,
    c: vec4f,
    d: vec4f,
}

@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var smp: sampler;

struct V {
    @builtin(position) pos: vec4f,
    @location(0) uv: vec2f,
}

@vertex
fn vs(@builtin(vertex_index) i: u32) -> V {
    let q = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    var o: V;
    o.pos = vec4f(q * 2.0 - 1.0, 0.0, 1.0);
    o.uv = vec2f(q.x, 1.0 - q.y);
    return o;
}

fn to_linear(c: vec3f) -> vec3f {
    return select(pow((c + 0.055) / 1.055, vec3f(2.4)), c / 12.92, c <= vec3f(0.04045));
}

// Копия как есть: уменьшение в два раза (линейная выборка усредняет четыре текселя).
@fragment
fn fs_copy(v: V) -> @location(0) vec4f {
    return vec4f(textureSample(tex, smp, v.uv).rgb, 1.0);
}

// Сцена в окно. a.x = 1 — окно в sRGB-формате.
@fragment
fn fs_blit(v: V) -> @location(0) vec4f {
    var c = textureSample(tex, smp, v.uv).rgb;
    if (p.a.x > 0.5) {
        c = to_linear(c);
    }
    return vec4f(c, 1.0);
}

// Половина размытия по Гауссу: a.xy — шаг в uv вдоль направления, a.z — сигма в текселях.
@fragment
fn fs_blur(v: V) -> @location(0) vec4f {
    let sigma = max(p.a.z, 0.5);
    let radius = min(i32(ceil(sigma * 3.0)), 24);
    var sum = vec3f(0.0);
    var weight = 0.0;
    for (var i = -radius; i <= radius; i = i + 1) {
        let x = f32(i);
        let w = exp(-x * x / (2.0 * sigma * sigma));
        sum = sum + textureSampleLevel(tex, smp, v.uv + p.a.xy * x, 0.0).rgb * w;
        weight = weight + w;
    }
    return vec4f(sum / weight, 1.0);
}

// Расстояние до прямоугольника со скруглёнными углами (отрицательное внутри).
fn rounded_box(q: vec2f, half: vec2f, r: f32) -> f32 {
    let d = abs(q) - half + vec2f(r);
    return length(max(d, vec2f(0.0))) + min(max(d.x, d.y), 0.0) - r;
}

// Стекло: размытая сцена под панелью, насыщенность 125% и тон поверх.
// a — панель в пикселях окна (x, y, w, h); b.x — радиус, b.yz — размер окна,
// b.w = 1 — окно в sRGB; c и d — тон в начале и в конце градиента (rgb, доля);
// градиент идёт под 145°, как в макете, и заканчивается на 70%.
@fragment
fn fs_glass(v: V) -> @location(0) vec4f {
    let px = v.pos.xy;
    let center = p.a.xy + p.a.zw * 0.5;
    let alpha = clamp(0.5 - rounded_box(px - center, p.a.zw * 0.5, p.b.x), 0.0, 1.0);
    if (alpha <= 0.0) {
        discard;
    }
    var c = textureSampleLevel(tex, smp, px / p.b.yz, 0.0).rgb;
    let l = dot(c, vec3f(0.2126, 0.7152, 0.0722));
    c = clamp(mix(vec3f(l), c, 1.25), vec3f(0.0), vec3f(1.0));

    let angle = radians(145.0);
    let dir = vec2f(sin(angle), -cos(angle));
    let span = abs(p.a.z * dir.x) + abs(p.a.w * dir.y);
    let t = clamp((dot(px - center, dir) / span + 0.5) / 0.7, 0.0, 1.0);
    let tint = mix(p.c, p.d, t);
    c = mix(c, tint.rgb, tint.a);

    if (p.b.w > 0.5) {
        c = to_linear(c);
    }
    return vec4f(c, alpha);
}
