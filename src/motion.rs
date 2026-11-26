use gtk::prelude::*;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

/// One finite animation per widget/property. Retarget from the visible value.
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
        self.animate_with(widget, target, millis, apply, ease_out);
    }

    pub fn animate_smooth<W: IsA<gtk::Widget> + 'static>(
        self: &Rc<Self>,
        widget: &W,
        target: f64,
        millis: u32,
        apply: impl Fn(&W, f64) + 'static,
    ) {
        self.animate_with(widget, target, millis, apply, smooth_step);
    }

    fn animate_with<W: IsA<gtk::Widget> + 'static>(
        self: &Rc<Self>,
        widget: &W,
        target: f64,
        millis: u32,
        apply: impl Fn(&W, f64) + 'static,
        easing: fn(f64) -> f64,
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
            let value = from + (target - from) * easing(progress);
            tween.value.set(value);
            apply(widget, value);
            glib::ControlFlow::Continue
        });
    }
}

/// Only the active lyric owns a frame callback. The audio clock is sampled on
/// each frame; easing an old UI snapshot would introduce another moving delay.
pub(crate) struct LyricHighlight {
    label: RefCell<Option<gtk::Label>>,
    interval: Cell<(f64, f64)>,
    rendered: RefCell<Vec<(u32, u32, u16)>>,
    scratch: RefCell<Vec<(u32, u32, u16)>>,
    timeline: RefCell<Option<crate::lyric_timing::Timeline>>,
    tick: RefCell<Option<gtk::TickCallbackId>>,
    wait: RefCell<Option<glib::SourceId>>,
    position: Box<dyn Fn() -> Option<(f64, bool, f64)>>,
    on_boundary: Box<dyn Fn()>,
}

impl LyricHighlight {
    pub fn new(
        position: impl Fn() -> Option<(f64, bool, f64)> + 'static,
        on_boundary: impl Fn() + 'static,
    ) -> Rc<Self> {
        Rc::new(Self {
            label: RefCell::new(None),
            interval: Cell::new((0.0, 0.0)),
            rendered: RefCell::new(Vec::new()),
            scratch: RefCell::new(Vec::new()),
            timeline: RefCell::new(None),
            tick: RefCell::new(None),
            wait: RefCell::new(None),
            position: Box::new(position),
            on_boundary: Box::new(on_boundary),
        })
    }

    pub fn clear(&self) {
        self.stop();
        if let Some(label) = self.label.borrow_mut().take() {
            label.set_attributes(None);
        }
        self.rendered.borrow_mut().clear();
        self.timeline.borrow_mut().take();
    }

    pub fn is_exact(&self) -> bool {
        self.timeline
            .borrow()
            .as_ref()
            .is_some_and(|timeline| timeline.exact)
    }

    fn stop(&self) {
        if let Some(wait) = self.wait.borrow_mut().take() {
            wait.remove();
        }
        if let Some(tick) = self.tick.borrow_mut().take() {
            tick.remove();
        }
    }

    #[cfg(test)]
    pub fn update(self: &Rc<Self>, label: &gtk::Label, start: f64, end: f64) {
        self.update_timed(label, &label.text(), start, end, 6.5);
    }

    pub fn update_timed(
        self: &Rc<Self>,
        label: &gtk::Label,
        raw: &str,
        start: f64,
        end: f64,
        rate: f64,
    ) {
        if self.label.borrow().as_ref() != Some(label) {
            self.clear();
            self.label.replace(Some(label.clone()));
        }
        if self.interval.get() != (start, end)
            || self
                .timeline
                .borrow()
                .as_ref()
                .is_none_or(|timeline| timeline.raw != raw)
        {
            self.timeline
                .replace(Some(crate::lyric_timing::Timeline::new(
                    raw, start, end, rate,
                )));
        }
        self.interval.set((start, end));
        self.refresh(false);
    }

    fn refresh(self: &Rc<Self>, notify_end: bool) {
        if !self.paint() {
            self.stop();
            self.settle(notify_end);
        } else {
            if let Some(wait) = self.wait.borrow_mut().take() {
                wait.remove();
            }
            if self.tick.borrow().is_some() {
                return;
            }
            let weak = Rc::downgrade(self);
            let Some(label) = self.label.borrow().clone() else {
                return;
            };
            let tick = label.add_tick_callback(move |_, _| {
                let Some(highlight) = weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                if highlight.paint() {
                    glib::ControlFlow::Continue
                } else {
                    highlight.tick.borrow_mut().take();
                    highlight.settle(true);
                    glib::ControlFlow::Break
                }
            });
            self.tick.replace(Some(tick));
        }
    }

    fn settle(self: &Rc<Self>, notify_end: bool) {
        if !super::animations_enabled()
            || !self
                .label
                .borrow()
                .as_ref()
                .is_some_and(|label| label.is_mapped())
        {
            return;
        }
        let Some((position, playing, speed)) = (self.position)() else {
            return;
        };
        if !playing {
            return;
        }
        let end = self.interval.get().1;
        if position >= end {
            if notify_end {
                (self.on_boundary)();
            }
            return;
        }
        let next = self
            .timeline
            .borrow()
            .as_ref()
            .and_then(|timeline| timeline.next_change(position, speed))
            .unwrap_or(end)
            .min(end);
        if !next.is_finite() || !speed.is_finite() || speed <= 0.0 {
            return;
        }
        let delay =
            std::time::Duration::from_secs_f64(((next - position) / speed).clamp(0.001, 86400.0));
        let weak = Rc::downgrade(self);
        let wait = glib::timeout_add_local_once(delay, move || {
            if let Some(highlight) = weak.upgrade() {
                highlight.wait.borrow_mut().take();
                highlight.refresh(true);
            }
        });
        self.wait.replace(Some(wait));
    }

    fn paint(&self) -> bool {
        let Some((position, playing, speed)) = (self.position)() else {
            return false;
        };
        let label = self.label.borrow();
        let Some(label) = label.as_ref().filter(|label| label.is_mapped()) else {
            return false;
        };
        if !super::animations_enabled() {
            label.set_attributes(None);
            self.rendered.borrow_mut().clear();
            return false;
        }
        let (start, end) = self.interval.get();
        let timeline = self.timeline.borrow();
        let Some(timeline) = timeline.as_ref() else {
            return false;
        };
        let mut alphas = self.scratch.borrow_mut();
        timeline.fill_alphas_at_speed(position, speed, &mut alphas);
        let mut rendered = self.rendered.borrow_mut();
        if *rendered != *alphas {
            let attrs = gtk::pango::AttrList::new();
            for &(start, end, alpha) in alphas.iter() {
                let mut attr = gtk::pango::AttrInt::new_foreground_alpha(alpha);
                attr.set_start_index(start);
                attr.set_end_index(end);
                attrs.insert(attr);
            }
            label.set_attributes(Some(&attrs));
            std::mem::swap(&mut *rendered, &mut *alphas);
        }
        playing
            && end.is_finite()
            && end > start
            && position < end
            && timeline.next_change(position, speed) == Some(position)
    }
}

#[cfg(test)]
fn lyric_alphas(text: &str, start: f64, end: f64, position: f64) -> Vec<(u32, u32, u16)> {
    let timeline = crate::lyric_timing::Timeline::new(text, start, end, 6.5);
    let mut ranges = Vec::new();
    timeline.fill_alphas(position, &mut ranges);
    ranges
}

// Zero velocity and acceleration at both ends keeps lyric scrolling gentle.
fn smooth_step(progress: f64) -> f64 {
    progress.powi(3) * (10.0 + progress * (-15.0 + 6.0 * progress))
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
        if button.has_visible_focus() {
            gtk::render_focus(&style, cr, 0.0, 0.0, width, height);
        }
        let _ = cr.restore();
        glib::Propagation::Stop
    });
}

#[cfg(test)]
mod tests {
    use super::{ease_out, lyric_alphas};

    fn alpha_at(text: &str, position: f64, byte: usize) -> u16 {
        lyric_alphas(text, 4.0, 8.0, position)
            .into_iter()
            .find(|&(start, end, _)| start as usize <= byte && byte < end as usize)
            .map_or(u16::MAX, |(_, _, alpha)| alpha)
    }

    #[test]
    fn lyric_fades_are_continuous_and_complete_at_letter_onsets() {
        let text = "Тихо";
        let before = alpha_at(text, 4.85, 2);
        let beginning = alpha_at(text, 4.90, 2);
        let middle = alpha_at(text, 4.94, 2);
        let almost = alpha_at(text, 4.98, 2);
        let onset = alpha_at(text, 5.0, 2);
        assert!(before < beginning && beginning < middle && middle < almost && almost < onset);
        assert_eq!(onset, u16::MAX);
        assert_eq!(alpha_at(text, 4.0, 0), u16::MAX);
        assert!(
            lyric_alphas(text, 4.0, 8.0, 7.0).is_empty(),
            "last letter finishes before the line ends"
        );
        assert_eq!(
            alpha_at(text, 4.90, 2),
            beginning,
            "seek back is independent of previous frames"
        );
    }

    #[test]
    fn lyric_attributes_preserve_utf8_and_handle_missing_intervals() {
        let text = "Тихо 🌙";
        for (start, end, _) in lyric_alphas(text, 0.0, 6.0, 3.95) {
            assert!(text.is_char_boundary(start as usize));
            assert!(text.is_char_boundary(end as usize));
        }
        assert!(lyric_alphas("", 0.0, 4.0, 2.0).is_empty());
        assert!(lyric_alphas(text, 4.0, 4.0, 4.0).is_empty());
        assert!(lyric_alphas(text, 4.0, f64::NAN, 4.0).is_empty());
    }

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
