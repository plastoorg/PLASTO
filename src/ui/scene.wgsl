// Сцена окна: фото с зелёным экраном монитора, собранное одним проходом.
// Хромакей по зелени, плоскость экрана в перспективе, бочка кинескопа,
// развёртка, объём из яркости зелени, перекраска подсвеченного корпуса.
// Та же математика на CPU — в scene.rs (запасной вывод без видеокарты).
// Живой фон: вместо снимка — кадр видео в NV12 (яркость и цвет отдельно).

struct U {
    a: vec4f,     // xy — uv окна → uv снимка; z — строк развёртки; w — сила блика
    roi: vec4f,   // где искать зелень, в uv снимка
    m0: vec4f,    // однородная матрица: uv снимка → uv плоскости экрана, по строкам
    m1: vec4f,
    m2: vec4f,
    c1: vec4f,    // цвет экрана: центр, середина, край
    c2: vec4f,
    c3: vec4f,
    glow: vec4f,  // свет экрана на корпусе: rgb и сила
    out: vec4f,   // x = 1 — кадр в sRGB-формате, перевести в линейный; y = 1 — фон из видео
}

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var photo: texture_2d<f32>;
@group(0) @binding(2) var smp: sampler;
@group(0) @binding(3) var luma: texture_2d<f32>;
@group(0) @binding(4) var chroma: texture_2d<f32>;

struct V {
    @builtin(position) pos: vec4f,
    @location(0) uv: vec2f,
}

// Один треугольник на весь прямоугольник сцены.
@vertex
fn vs(@builtin(vertex_index) i: u32) -> V {
    let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    var o: V;
    o.pos = vec4f(p * 2.0 - 1.0, 0.0, 1.0);
    o.uv = vec2f(p.x, 1.0 - p.y);
    return o;
}

// Свечение экрана: радиальный переход из центра чуть выше середины.
fn screen_bg(uv: vec2f) -> vec3f {
    let d = length((uv - vec2f(0.5, 0.45)) * vec2f(440.0, 352.0)) / (0.62 * 440.0);
    if (d < 0.6) {
        return mix(u.c1.rgb, u.c2.rgb, d / 0.6);
    }
    return mix(u.c2.rgb, u.c3.rgb, clamp((d - 0.6) / 0.4, 0.0, 1.0));
}

// Кадр видео: NV12 → RGB по BT.601, ограниченный диапазон (так закодирован ролик).
fn video_rgb(uv: vec2f) -> vec3f {
    let y = (textureSample(luma, smp, uv).r - 16.0 / 255.0) * (255.0 / 219.0);
    let c = (textureSample(chroma, smp, uv).rg - 128.0 / 255.0) * (255.0 / 224.0);
    return clamp(vec3f(y + 1.402 * c.y, y - 0.344136 * c.x - 0.714136 * c.y, y + 1.772 * c.x), vec3f(0.0), vec3f(1.0));
}

fn to_linear(c: vec3f) -> vec3f {
    return select(pow((c + 0.055) / 1.055, vec3f(2.4)), c / 12.92, c <= vec3f(0.04045));
}

@fragment
fn fs(v: V) -> @location(0) vec4f {
    let suv = v.uv * u.a.xy;
    // Видео — та же сцена в той же пропорции: координаты снимка подходят как есть.
    // Маска экрана, его объём и отсвет на корпусе — всегда по снимку: в сжатом
    // видео зелень по краю кинескопа шумит от кадра к кадру, и граница дрожала бы.
    // Из видео берутся только цвета всего остального.
    let m = textureSample(photo, smp, suv).rgb;
    let c = select(m, video_rgb(suv), u.out.y > 0.5);
    let roi = step(u.roi.x, suv.x) * step(u.roi.y, suv.y) * step(suv.x, u.roi.z) * step(suv.y, u.roi.w);

    // хромакей: насколько зелёный перевешивает
    let dom = m.g - max(m.r, m.b);
    let key = smoothstep(0.06, 0.20, dom) * roi;

    // плоскость экрана в перспективе и бочка кинескопа
    let s = vec3f(suv, 1.0);
    let p = vec3f(dot(u.m0.xyz, s), dot(u.m1.xyz, s), dot(u.m2.xyz, s));
    var uv = p.xy / p.z;
    let q = uv - 0.5;
    uv = 0.5 + q * (1.0 + 0.06 * dot(q, q));
    let scr = screen_bg(clamp(uv, vec2f(0.0), vec2f(1.0)));

    // развёртка, объём из яркости зелени, блик слева сверху
    let scan = 0.87 + 0.13 * step(0.5, fract(uv.y * u.a.z));
    let shade = pow(clamp(m.g / 0.86, 0.0, 1.0), 1.3);
    let gq = (uv - vec2f(0.24, 0.2)) * vec2f(3.2, 4.4);
    let glare = exp(-dot(gq, gq)) * 0.26 * u.a.w;
    let screen_col = scr * scan * shade + glare;

    // корпус: зелень с пластика убираем, яркость оставляем в бежевом, добавляем свет экрана
    let lum = dot(m, vec3f(0.3, 0.59, 0.11));
    let rest = (1.0 - key) * roi;
    let greenish = clamp((m.g - 0.9 * m.r) / 0.12, 0.0, 1.0) * smoothstep(0.43, 0.67, lum);
    let spill = clamp(max(greenish, (dom - 0.015) / 0.16), 0.0, 1.0) * rest;
    let beige = vec3f(1.0, 0.94, 0.8);
    let plastic = beige * lum / dot(beige, vec3f(0.3, 0.59, 0.11));
    // Где на корпус падает зелёный отсвет — цвет тоже со снимка: в видео кромка
    // рамки у кинескопа зеленеет по-разному в каждом кадре.
    // Так же — полоса рамки вокруг кинескопа (по плоскости экрана, его край —
    // 0.5 от середины): тёмная щель у края в видео тоже шумит.
    let rim = (1.0 - smoothstep(0.58, 0.66, max(abs(q.x), abs(q.y)))) * roi;
    let body = mix(c, m, clamp(max(spill * 4.0, rim), 0.0, 1.0));
    var base = mix(body, plastic, clamp(spill * 1.4, 0.0, 1.0));
    base = base + u.glow.rgb * u.glow.a * spill * (1.0 - base);

    var color = mix(base, screen_col, key);
    if (u.out.x > 0.5) {
        color = to_linear(color);
    }
    return vec4f(color, 1.0);
}
