use gtk::prelude::*;
use std::{cell::Cell, rc::Rc};

/// Keep one finite animation per widget and property. Start retargeting from the visible value.
pub(crate) struct Tween {
    value: Cell<f64>,
    target: Cell<f64>,
    started: Cell<Option<(i64, f64)>>,
    running: Cell<bool>,
}

impl Tween {
    pub fn new(value: f64) -> Rc<Self> {
        Rc::new(Self {
            value: Cell::new(value),
            target: Cell::new(value),
            started: Cell::new(None),
            running: Cell::new(false),
        })
    }

    pub fn value(&self) -> f64 {
        self.value.get()
    }

    pub fn snap(&self, value: f64) {
        self.target.set(value);
        self.value.set(value);
        self.started.set(None);
    }

    pub fn animate<W: IsA<gtk::Widget> + 'static>(
        self: &Rc<Self>,
        widget: &W,
        target: f64,
        millis: u32,
        apply: impl Fn(&W, f64) + 'static,
    ) {
        if !widget.is_mapped() || !super::animations_enabled() {
            let changed = self.value() != target;
            self.snap(target);
            if changed {
                apply(widget, target);
            }
            return;
        }
        if self.target.get() == target {
            return;
        }
        self.target.set(target);
        self.started.set(None);
        if self.running.replace(true) {
            return;
        }

        let tween = self.clone();
        widget.add_tick_callback(move |widget, clock| {
            let now = clock.frame_time();
            let (start, from) = tween.started.get().unwrap_or_else(|| {
                let initial = (now, tween.value());
                tween.started.set(Some(initial));
                initial
            });
            let target = tween.target.get();
            let progress = ((now - start) as f64 / (f64::from(millis) * 1000.0)).clamp(0.0, 1.0);
            if !widget.is_mapped() || from == target {
                tween.running.set(false);
                return glib::ControlFlow::Break;
            }
            if progress == 1.0 || !super::animations_enabled() {
                tween.snap(target);
                tween.running.set(false);
                apply(widget, target);
                return glib::ControlFlow::Break;
            }
            let value = from + (target - from) * ease_out(progress);
            tween.value.set(value);
            apply(widget, value);
            glib::ControlFlow::Continue
        });
    }
}

fn ease_out(progress: f64) -> f64 {
    1.0 - (1.0 - progress).powi(3)
}

pub(crate) fn animate_buttons(widget: &gtk::Widget) {
    if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        animate_button(button);
    }
    if let Some(container) = widget.downcast_ref::<gtk::Container>() {
        for child in container.children() {
            animate_buttons(&child);
        }
    }
}

pub(crate) fn animate_button(button: &impl IsA<gtk::Button>) {
    let button = button.upcast_ref::<gtk::Button>();
    let style = button.style_context();
    if style.has_class("press-animation") {
        return;
    }
    style.add_class("press-animation");

    let scale = Tween::new(1.0);
    let state_scale = scale.clone();
    button.connect_state_flags_changed(move |button, _| {
        let flags = button.state_flags();
        let pressed = flags.contains(gtk::StateFlags::ACTIVE)
            && !flags.contains(gtk::StateFlags::INSENSITIVE);
        state_scale.animate(
            button,
            if pressed { 0.96 } else { 1.0 },
            140,
            |button, _| {
                button.queue_draw();
            },
        );
    });
    let unmap_scale = scale.clone();
    button.connect_unmap(move |_| unmap_scale.snap(1.0));
    button.connect_draw(move |button, cr| {
        let factor = scale.value();
        if factor == 1.0 {
            return glib::Propagation::Proceed;
        }

        // Scale the drawing only. GTK keeps the native hit target and keyboard handling.
        let width = f64::from(button.allocated_width());
        let height = f64::from(button.allocated_height());
        let style = button.style_context();
        let _ = cr.save();
        cr.translate(width * (1.0 - factor) / 2.0, height * (1.0 - factor) / 2.0);
        cr.scale(factor, factor);
        gtk::render_background(&style, cr, 0.0, 0.0, width, height);
        gtk::render_frame(&style, cr, 0.0, 0.0, width, height);
        if let Some(child) = button.child().filter(|child| child.is_visible()) {
            button.propagate_draw(&child, cr);
        }
        let _ = cr.restore();
        if button.has_visible_focus() {
            gtk::render_focus(&style, cr, 0.0, 0.0, width, height);
        }
        glib::Propagation::Stop
    });
}

#[cfg(test)]
mod tests {
    use super::ease_out;

    #[test]
    fn easing_is_monotonic_and_slows_down_at_common_refresh_rates() {
        for hz in [30, 60, 144] {
            let frames = (0.42_f64 * f64::from(hz)).ceil() as u32;
            let mut last = 0.0;
            let mut previous_step = 1.0;
            for frame in 1..=frames {
                let time = f64::from(frame) / f64::from(hz);
                let value = ease_out((time / 0.42).min(1.0));
                let step = value - last;
                assert!(step > 0.0 && step < previous_step);
                previous_step = step;
                last = value;
            }
            assert_eq!(last, 1.0);
        }
    }
}
