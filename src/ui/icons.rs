//! Значки нового вида: контуры в сетке 24×24, как в макете, — линии с
//! круглыми концами. Рисуются штрихами egui и масштабируются без потерь.


use egui::{pos2, vec2, Color32, Painter, Pos2, Shape, Stroke};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    /// Щит: zapret, обход для YouTube и Discord.
    Shield,
    /// Бумажный самолётик: Telegram.
    Plane,
    /// Искра: ИИ-сервисы.
    Spark,
    /// Глобус: DNS.
    Globe,
    Refresh,
    Power,
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    ChevronRight,
    ChevronDown,
    Play,
    Stop,
    /// Список: журнал.
    List,
    /// Шкала: подбор стратегии.
    Gauge,
    Check,
    Minus,
    Cross,
}

/// Значок `icon` размером `size` с центром в `c`.
pub fn paint(p: &Painter, c: Pos2, icon: Icon, size: f32, color: Color32) {
    paint_turned(p, c, icon, size, color, 0.0);
}

/// То же, повёрнутое на `angle` радиан вокруг центра (крутящееся «обновить»).
pub fn paint_turned(p: &Painter, c: Pos2, icon: Icon, size: f32, color: Color32, angle: f32) {
    let k = size / 24.0;
    let (sin, cos) = angle.sin_cos();
    let at = |x: f32, y: f32| {
        let (dx, dy) = ((x - 12.0) * k, (y - 12.0) * k);
        pos2(c.x + dx * cos - dy * sin, c.y + dx * sin + dy * cos)
    };
    let width = (1.9 * k).max(1.2);
    let stroke = Stroke::new(width, color);
    let line = |pts: &[(f32, f32)]| {
        let pts: Vec<Pos2> = pts.iter().map(|&(x, y)| at(x, y)).collect();
        for &q in &pts {
            p.circle_filled(q, width / 2.0, color);
        }
        p.add(Shape::line(pts, stroke));
    };
    let arc = |cx: f32, cy: f32, r: f32, from: f32, to: f32| {
        let n = 24;
        let pts: Vec<Pos2> = (0..=n)
            .map(|i| {
                let a = (from + (to - from) * i as f32 / n as f32).to_radians();
                at(cx + r * a.cos(), cy + r * a.sin())
            })
            .collect();
        p.circle_filled(pts[0], width / 2.0, color);
        p.circle_filled(pts[n], width / 2.0, color);
        p.add(Shape::line(pts, stroke));
    };
    match icon {
        Icon::Shield => line(&[(12.0, 3.0), (20.0, 6.0), (20.0, 12.0), (18.5, 16.5), (12.0, 21.0), (5.5, 16.5), (4.0, 12.0), (4.0, 6.0), (12.0, 3.0)]),
        Icon::Plane => line(&[(21.0, 4.0), (3.0, 11.0), (9.0, 13.0), (11.0, 19.0), (14.0, 15.0), (19.0, 19.0), (21.0, 4.0)]),
        Icon::Spark => line(&[(12.0, 3.0), (14.0, 8.0), (19.0, 10.0), (14.0, 12.0), (12.0, 17.0), (10.0, 12.0), (5.0, 10.0), (10.0, 8.0), (12.0, 3.0)]),
        Icon::Globe => {
            p.circle_stroke(at(12.0, 12.0), 9.0 * k, stroke);
            line(&[(3.0, 12.0), (21.0, 12.0)]);
            p.add(Shape::ellipse_stroke(at(12.0, 12.0), vec2(4.0, 9.0) * k, stroke));
        }
        Icon::Refresh => {
            arc(12.0, 12.0, 8.0, -40.0, 270.0);
            line(&[(20.0, 4.0), (20.0, 9.0), (15.0, 9.0)]);
        }
        Icon::Power => {
            arc(12.0, 12.5, 8.0, -50.0, 230.0);
            line(&[(12.0, 3.0), (12.0, 12.0)]);
        }
        Icon::ChevronRight => line(&[(9.0, 6.0), (15.0, 12.0), (9.0, 18.0)]),
        Icon::ChevronDown => line(&[(6.0, 9.0), (12.0, 15.0), (18.0, 9.0)]),
        Icon::Play => {
            p.add(Shape::convex_polygon(vec![at(7.0, 4.0), at(20.0, 12.0), at(7.0, 20.0)], color, Stroke::NONE));
        }
        Icon::Stop => {
            p.rect_filled(egui::Rect::from_min_max(at(5.0, 5.0), at(19.0, 19.0)), 2.0 * k, color);
        }
        Icon::List => {
            for y in [6.0, 12.0, 18.0] {
                line(&[(8.0, y), (20.0, y)]);
                p.circle_filled(at(4.0, y), width * 0.7, color);
            }
        }
        Icon::Gauge => {
            arc(12.0, 17.0, 8.0, 180.0, 360.0);
            line(&[(12.0, 17.0), (16.0, 12.0)]);
        }
        Icon::Check => line(&[(4.0, 12.0), (10.0, 18.0), (20.0, 6.0)]),
        Icon::Minus => line(&[(6.0, 12.0), (18.0, 12.0)]),
        Icon::Cross => {
            line(&[(6.0, 6.0), (18.0, 18.0)]);
            line(&[(18.0, 6.0), (6.0, 18.0)]);
        }
    }
}
