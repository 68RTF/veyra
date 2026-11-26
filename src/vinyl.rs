use super::*;
use gtk::cairo::{Context, Format, ImageSurface, LinearGradient};
use std::f64::consts::TAU;

pub(super) fn build(model: &Rc<RefCell<Model>>) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.style_context().add_class("vinyl");
    area.set_no_show_all(true);
    area.set_visible(model.borrow().current.is_some());
    let angle = Rc::new(Cell::new(0.0_f64));
    let texture = pressing();
    area.connect_draw({
        let angle = angle.clone();
        move |area, cr| {
            let height = f64::from(area.allocated_height());
            let radius = height * 0.48;
            cr.translate(radius, height / 2.0);
            cr.scale(radius, radius);
            cr.arc(0.0, 0.0, 1.0, 0.0, TAU);
            cr.clip();
            let _ = cr.save();
            cr.rotate(angle.get());
            cr.scale(1.0 / 256.0, 1.0 / 256.0);
            if cr.set_source_surface(&texture, -256.0, -256.0).is_ok() {
                let _ = cr.paint();
            }
            let _ = cr.restore();
            let light = LinearGradient::new(-0.5, -1.0, 0.17, 0.8);
            for (offset, alpha) in [
                (0.0, 0.0),
                (0.26, 0.16),
                (0.41, 0.02),
                (0.72, 0.0),
                (1.0, 0.08),
            ] {
                light.add_color_stop_rgba(offset, 0.84, 0.88, 0.92, alpha);
            }
            let _ = cr.set_source(&light);
            let _ = cr.paint();
            glib::Propagation::Proceed
        }
    });

    let timer = Rc::new(RefCell::new(None::<glib::SourceId>));
    area.connect_unmap({
        let timer = timer.clone();
        move |_| {
            if let Some(source) = timer.borrow_mut().take() {
                source.remove();
            }
        }
    });
    let updates = model.borrow().updates.clone();
    let model = model.clone();
    let target_speed = Rc::new(Cell::new(0.0_f64));
    let widget = area.downgrade();
    updates.listen(&area, move || {
        let Some(area) = widget.upgrade() else { return };
        let state = model.borrow();
        area.set_visible(state.current.is_some());
        let playing = state
            .mpv
            .as_ref()
            .is_some_and(|p| p.has_track() && !p.paused);
        target_speed.set(if playing { TAU / 14.0 } else { 0.0 });
        if !area.is_mapped() || !animations_enabled() || timer.borrow().is_some() || !playing {
            return;
        }
        let widget = area.downgrade();
        let angle = angle.clone();
        let target = target_speed.clone();
        let running = timer.clone();
        let mut speed = 0.0_f64;
        let mut last = Instant::now();
        let source = glib::timeout_add_local(Duration::from_millis(40), move || {
            let Some(area) = widget
                .upgrade()
                .filter(|w| w.is_mapped() && animations_enabled())
            else {
                running.borrow_mut().take();
                return glib::ControlFlow::Break;
            };
            let now = Instant::now();
            let dt = (now - last).as_secs_f64().min(0.1);
            last = now;
            let step = dt * (TAU / 14.0) / 0.35;
            speed += (target.get() - speed).clamp(-step, step);
            if speed <= 0.0 {
                running.borrow_mut().take();
                return glib::ControlFlow::Break;
            }
            angle.set((angle.get() + speed * dt) % TAU);
            area.queue_draw();
            glib::ControlFlow::Continue
        });
        timer.replace(Some(source));
    });
    area
}

// Rasterize the grooves once. Only the exposed strip is repainted as the disc turns.
fn pressing() -> ImageSurface {
    let surface = ImageSurface::create(Format::Rgb24, 512, 512).expect("vinyl surface");
    let cr = Context::new(&surface).expect("vinyl context");
    cr.translate(256.0, 256.0);
    cr.set_source_rgb(0.087, 0.092, 0.10);
    let _ = cr.paint();
    for i in 0..67 {
        let radius = 252.0 - f64::from(i) * 2.6;
        let tone = if i % 13 == 0 { 0.24 } else { 0.16 };
        cr.set_line_width(1.0);
        cr.set_source_rgb(tone, tone, tone + 0.008);
        cr.arc(0.0, 0.0, radius, 0.0, TAU);
        let _ = cr.stroke();
        let start = f64::from(i) * 0.006 + (f64::from(i) * 0.7).sin() * 0.045;
        for offset in [0.0, 2.3, 4.5] {
            cr.set_source_rgba(0.66, 0.68, 0.70, 0.22);
            cr.arc(0.0, 0.0, radius, start + offset, start + offset + 0.65);
            let _ = cr.stroke();
        }
    }
    surface
}
