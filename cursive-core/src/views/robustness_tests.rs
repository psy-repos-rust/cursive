//! Containers must not break because of misbehaving children.
//!
//! An "erratic" view gives random sizes and keys on every call (as a buggy
//! third-party view, or one reading state changed by another thread might).
//! Whatever containers are built around it must not panic, and once it
//! behaves again, must quickly give the same sizes as a fresh tree.

use crate::{
    Printer, Rect, Vec2,
    buffer::PrintBuffer,
    direction::{Direction, Orientation},
    event::{Event, Key, MouseButton, MouseEvent},
    theme::Theme,
    view::{Margins, Nameable, SizeConstraint, View},
    views::{
        BoxedView, CircularFocus, Dialog, EnableableView, FixedLayout, HideableView, Layer,
        LinearLayout, ListView, OnEventView, PaddedView, Panel, ResizedView, ScreensView,
        ScrollView, ShadowView, StackView, TextArea, TextView,
    },
};
use parking_lot::RwLock;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Shared by all erratic views of a tree: whether they behave, and a seed.
struct Mood {
    stable: AtomicBool,
    seed: AtomicU64,
}

impl Mood {
    fn rnd(&self, n: usize) -> usize {
        let mut seed = self.seed.load(Ordering::Relaxed);
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        self.seed.store(seed, Ordering::Relaxed);
        (seed % n.max(1) as u64) as usize
    }
}

struct Erratic {
    id: usize,
    mood: Arc<Mood>,
}

impl View for Erratic {
    fn draw(&self, printer: &Printer) {
        for y in 0..printer.size.y.min(100) {
            for x in 0..printer.size.x.min(100) {
                printer.print((x, y), "x");
            }
        }
    }

    fn required_size(&mut self, req: Vec2) -> Vec2 {
        if self.mood.stable.load(Ordering::Relaxed) {
            Vec2::new(req.x.min(3 + self.id % 5), 1 + self.id % 3)
        } else {
            let mood = &self.mood;
            Vec2::new(mood.rnd(req.x * 2 + 6), mood.rnd(req.y * 2 + 6))
        }
    }

    fn needs_relayout(&self) -> bool {
        !self.mood.stable.load(Ordering::Relaxed) && self.mood.rnd(2) == 0
    }

    fn layout_key(&self) -> u64 {
        if self.mood.stable.load(Ordering::Relaxed) {
            self.id as u64
        } else {
            // Changing all the time, but honestly: a new key every time.
            crate::view::fresh_layout_key()
        }
    }

    fn important_area(&self, size: Vec2) -> Rect {
        if self.mood.stable.load(Ordering::Relaxed) {
            Rect::from_size(Vec2::zero(), size)
        } else {
            let m = &self.mood;
            Rect::from_size(
                (m.rnd(size.x + 5), m.rnd(size.y + 5)),
                (m.rnd(size.x + 5), m.rnd(size.y + 5)),
            )
        }
    }

    fn take_focus(
        &mut self,
        _: Direction,
    ) -> Result<crate::event::EventResult, crate::view::CannotFocus> {
        Ok(crate::event::EventResult::consumed())
    }
}

/// The shape of a tree, so the same one can be built again.
#[derive(Clone, Debug)]
enum T {
    Erratic(usize),
    Text(&'static str),
    Area,
    Linear(Orientation, Vec<T>),
    List(Vec<Option<T>>),
    Stack(Vec<(u8, T)>),
    Dialog(Box<T>, usize),
    Scroll(bool, bool, Box<T>),
    Panel(Box<T>),
    Padded(Box<T>),
    Resized(u8, u8, Box<T>),
    Hideable(bool, Box<T>),
    Shadow(Box<T>),
    Fixed(Vec<((usize, usize, usize, usize), T)>),
    Screens(Vec<T>, usize),
    Wrappers(u8, Box<T>),
}

fn constraint(c: u8) -> SizeConstraint {
    match c % 5 {
        0 => SizeConstraint::Free,
        1 => SizeConstraint::Full,
        2 => SizeConstraint::Fixed(4),
        3 => SizeConstraint::AtMost(6),
        _ => SizeConstraint::AtLeast(3),
    }
}

fn build(t: &T, mood: &Arc<Mood>) -> BoxedView {
    let b = |t: &T| build(t, mood);
    match t {
        T::Erratic(id) => BoxedView::boxed(Erratic {
            id: *id,
            mood: mood.clone(),
        }),
        T::Text(s) => BoxedView::boxed(TextView::new(*s)),
        T::Area => BoxedView::boxed(TextArea::new().content("some text\nand more")),
        T::Linear(o, cs) => {
            let mut l = LinearLayout::new(*o);
            for c in cs {
                l.add_child(b(c));
            }
            BoxedView::boxed(l)
        }
        T::List(cs) => {
            let mut l = ListView::new();
            for c in cs {
                match c {
                    Some(c) => l.add_child("label", b(c)),
                    None => l.add_delimiter(),
                }
            }
            BoxedView::boxed(l)
        }
        T::Stack(cs) => {
            let mut s = StackView::new();
            for (kind, c) in cs {
                match kind % 3 {
                    0 => s.add_layer(b(c)),
                    1 => s.add_fullscreen_layer(b(c)),
                    _ => s.add_transparent_layer(b(c)),
                }
            }
            BoxedView::boxed(s)
        }
        T::Dialog(c, buttons) => {
            let mut d = Dialog::around(b(c)).title("title");
            for _ in 0..*buttons {
                d.add_button("Ok", |_| ());
            }
            BoxedView::boxed(d)
        }
        T::Scroll(x, y, c) => BoxedView::boxed(ScrollView::new(b(c)).scroll_x(*x).scroll_y(*y)),
        T::Panel(c) => BoxedView::boxed(Panel::new(b(c))),
        T::Padded(c) => BoxedView::boxed(PaddedView::new(Margins::lrtb(1, 2, 1, 0), b(c))),
        T::Resized(w, h, c) => {
            BoxedView::boxed(ResizedView::new(constraint(*w), constraint(*h), b(c)))
        }
        T::Hideable(visible, c) => {
            let mut h = HideableView::new(b(c));
            h.set_visible(*visible);
            BoxedView::boxed(h)
        }
        T::Shadow(c) => BoxedView::boxed(ShadowView::new(b(c))),
        T::Fixed(cs) => {
            let mut f = FixedLayout::new();
            for ((x, y, w, h), c) in cs {
                f = f.child(Rect::from_size((*x, *y), (*w, *h)), b(c));
            }
            BoxedView::boxed(f)
        }
        T::Screens(cs, active) => {
            let mut s = ScreensView::new();
            for c in cs {
                s.add_screen(b(c));
            }
            if !cs.is_empty() {
                s.set_active_screen(active % cs.len());
            }
            BoxedView::boxed(s)
        }
        T::Wrappers(kind, c) => match kind % 5 {
            0 => BoxedView::boxed(b(c).with_name("named")),
            1 => BoxedView::boxed(OnEventView::new(b(c))),
            2 => BoxedView::boxed(CircularFocus::new(b(c)).wrap_tab()),
            3 => BoxedView::boxed(EnableableView::new(b(c))),
            _ => BoxedView::boxed(Layer::new(b(c))),
        },
    }
}

fn make(rnd: &mut dyn FnMut(usize) -> usize, depth: usize, next_id: &mut usize) -> T {
    let child = |rnd: &mut dyn FnMut(usize) -> usize, next_id: &mut usize| {
        Box::new(make(rnd, depth - 1, next_id))
    };
    match if depth == 0 { rnd(3) } else { 3 + rnd(14) } {
        0 => {
            *next_id += 1;
            T::Erratic(*next_id)
        }
        1 => T::Text(["hello world", "a\nb\nc", "中文 wide", ""][rnd(4)]),
        2 => T::Area,
        3 | 4 => T::Linear(
            if rnd(2) == 0 {
                Orientation::Horizontal
            } else {
                Orientation::Vertical
            },
            (0..1 + rnd(3))
                .map(|_| make(rnd, depth - 1, next_id))
                .collect(),
        ),
        5 => T::List(
            (0..1 + rnd(3))
                .map(|_| (rnd(4) != 0).then(|| make(rnd, depth - 1, next_id)))
                .collect(),
        ),
        6 => T::Stack(
            (0..1 + rnd(3))
                .map(|_| (rnd(3) as u8, make(rnd, depth - 1, next_id)))
                .collect(),
        ),
        7 => T::Dialog(child(rnd, next_id), rnd(3)),
        8 => T::Scroll(rnd(2) == 0, rnd(2) == 0, child(rnd, next_id)),
        9 => T::Panel(child(rnd, next_id)),
        10 => T::Padded(child(rnd, next_id)),
        11 => T::Resized(rnd(5) as u8, rnd(5) as u8, child(rnd, next_id)),
        12 => T::Hideable(rnd(3) != 0, child(rnd, next_id)),
        13 => T::Shadow(child(rnd, next_id)),
        14 => T::Fixed(
            (0..1 + rnd(3))
                .map(|_| {
                    (
                        (rnd(10), rnd(6), rnd(10), rnd(6)),
                        make(rnd, depth - 1, next_id),
                    )
                })
                .collect(),
        ),
        15 => T::Screens(
            (0..1 + rnd(2))
                .map(|_| make(rnd, depth - 1, next_id))
                .collect(),
            rnd(2),
        ),
        _ => T::Wrappers(rnd(5) as u8, child(rnd, next_id)),
    }
}

fn random_event(rnd: &mut dyn FnMut(usize) -> usize, size: Vec2) -> Event {
    let position = Vec2::new(rnd(size.x + 3), rnd(size.y + 3));
    let mouse = |event| Event::Mouse {
        offset: Vec2::zero(),
        position,
        event,
    };
    match rnd(7) {
        0 => mouse(MouseEvent::Press(MouseButton::Left)),
        1 => mouse(MouseEvent::Release(MouseButton::Left)),
        2 => mouse(MouseEvent::Hold(MouseButton::Left)),
        3 => mouse(MouseEvent::WheelDown),
        4 => mouse(MouseEvent::WheelUp),
        // Only events that don't edit anything: the tree must end up like a
        // fresh one.
        _ => Event::Key(
            [
                Key::Tab,
                Key::Left,
                Key::Right,
                Key::Up,
                Key::Down,
                Key::PageDown,
                Key::End,
                Key::Home,
            ][rnd(8)],
        ),
    }
}

/// Runs one random scenario, returns an error if the tree didn't settle.
fn run_trial(seed: u64) -> Result<(), String> {
    let mut state = seed;
    let mut rnd = move |n: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % n.max(1) as u64) as usize
    };
    let mut next_id = 0;
    let shape = make(&mut rnd, 3, &mut next_id);
    let mood = Arc::new(Mood {
        stable: AtomicBool::new(false),
        seed: AtomicU64::new(seed ^ 0x9e37_79b9_7f4a_7c15),
    });
    let mut tree = build(&shape, &mood);
    let theme = Theme::default();
    let buffer = RwLock::new(PrintBuffer::new());

    let random_size = |rnd: &mut dyn FnMut(usize) -> usize| {
        if rnd(10) == 0 {
            Vec2::new(rnd(500), rnd(300))
        } else {
            Vec2::new(rnd(60), rnd(30))
        }
    };

    for _ in 0..40 {
        let size = random_size(&mut rnd);
        match rnd(6) {
            0 => {
                tree.required_size(size);
            }
            1 | 2 => {
                tree.layout(size);
                buffer.write().resize(size);
                tree.draw(&Printer::new(size, &theme, &buffer));
            }
            3 => {
                tree.important_area(size);
            }
            4 => {
                let _ = tree.take_focus(Direction::none());
            }
            _ => {
                let event = random_event(&mut rnd, size);
                tree.on_event(event);
            }
        }
    }

    // Now children behave: after a couple of frames, sizes must be right.
    mood.stable.store(true, Ordering::Relaxed);
    let size = random_size(&mut rnd);
    for _ in 0..3 {
        tree.layout(size);
        buffer.write().resize(size);
        tree.draw(&Printer::new(size, &theme, &buffer));
    }
    let request = random_size(&mut rnd);
    let got = tree.required_size(request);
    let fresh = build(&shape, &mood).required_size(request);
    if got != fresh {
        return Err(format!(
            "{shape:?}\n  at {request:?}: {got:?}, fresh {fresh:?}"
        ));
    }
    Ok(())
}

#[test]
fn containers_survive_erratic_children() {
    for seed in 1..300u64 {
        if let Err(e) = run_trial(seed.wrapping_mul(0x2545_f491_4f6c_dd1d)) {
            panic!("seed {seed}: {e}");
        }
    }
}

/// What `view` (laid out at `full` size) draws in the `window`-sized area
/// at `offset`, as seen through a scrolled printer (like `ScrollView` uses):
/// the text of each cell, row by row.
fn draw_window(view: &dyn View, full: Vec2, offset: Vec2, window: Vec2) -> Vec<Vec<String>> {
    let theme = Theme::default();
    let buffer = RwLock::new(PrintBuffer::new());
    buffer.write().resize(window);
    view.draw(
        &Printer::new(window, &theme, &buffer)
            .content_offset(offset)
            .inner_size(full),
    );
    let buffer = buffer.read();
    (0..window.y)
        .map(|y| {
            (0..window.x)
                .map(|x| {
                    let cell = buffer.cell_at(Vec2::new(x, y));
                    cell.map_or("", |c| c.text()).to_string()
                })
                .collect()
        })
        .collect()
}

#[test]
fn scrolled_drawing_matches_full_drawing() {
    // Containers skip drawing children that aren't visible: what is visible
    // must be exactly what drawing everything shows there.
    use crate::views::SelectView;

    let mut vertical = LinearLayout::vertical();
    let mut horizontal = LinearLayout::horizontal();
    let mut list = ListView::new();
    let mut select = SelectView::new();
    for i in 0..200 {
        vertical.add_child(
            LinearLayout::horizontal()
                .child(TextView::new(format!("row {i}")))
                .child(TextView::new("a\nb")),
        );
        horizontal.add_child(TextView::new(format!("c{i}\nx")));
        list.add_child(
            format!("label {i}"),
            TextView::new(format!("value {i}\nmore")),
        );
        select.add_item(format!("item {i}"), i);
    }
    select.set_selection(120);
    let views: [(&str, BoxedView); 4] = [
        ("vertical", BoxedView::boxed(vertical)),
        ("horizontal", BoxedView::boxed(horizontal)),
        ("list", BoxedView::boxed(list)),
        ("select", BoxedView::boxed(select)),
    ];

    let window = Vec2::new(20, 10);
    for (name, mut view) in views {
        let full = view.required_size(Vec2::new(2000, 1000));
        view.layout(full);
        let reference = draw_window(&view, full, Vec2::zero(), full);
        let offsets = [
            (0, 0),
            (0, 1),
            (3, 57),
            (0, 150),
            (400, 2),
            (full.x.saturating_sub(5), full.y.saturating_sub(3)),
        ];
        for offset in offsets {
            let offset = Vec2::from(offset).or_min(full);
            let expected: Vec<Vec<String>> = (0..window.y)
                .map(|y| {
                    (0..window.x)
                        .map(|x| {
                            reference
                                .get(offset.y + y)
                                .and_then(|row| row.get(offset.x + x))
                                .cloned()
                                .unwrap_or_default()
                        })
                        .collect()
                })
                .collect();
            let got = draw_window(&view, full, offset, window);
            assert_eq!(got, expected, "{name} at {offset:?}");
        }
    }
}
