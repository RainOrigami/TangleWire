//! The zoomable, pannable surface that shows nodes as widgets and links as
//! stroked paths.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};
use tanglewire_layout::{self as layout, Engine};

use crate::connect::{self, End};
use crate::graph::{Direction, Graph, Id};
use crate::groups::{self, Config};
use crate::pw::Command;
use crate::search::Query;

const MARGIN: f64 = 32.0;
const MAX_SCALE: f64 = 3.0;
const PANEL_WIDTH: i32 = 300;
const PANEL_MARGIN: i32 = 12;
/// How long nodes take to glide to a new place, in microseconds.
const GLIDE: f64 = 420_000.0;
/// How long the marks on changed cards pulse, in microseconds.
const PULSE: i64 = 16_000_000;
/// Colour that marks what the last update changed.
const CHANGED: (f32, f32, f32) = (1.0, 0.64, 0.28);
/// Link colours, from the GNOME palette. A link takes the colour of the node
/// it comes from.
const PALETTE: [(u8, u8, u8); 8] = [
    (0x62, 0xa0, 0xea),
    (0x57, 0xe3, 0x89),
    (0xf8, 0xe4, 0x5c),
    (0xff, 0xa3, 0x48),
    (0xed, 0x33, 0x3b),
    (0xc0, 0x61, 0xcb),
    (0x5b, 0xc8, 0xaf),
    (0xb5, 0x83, 0x5a),
];

mod imp {
    use super::*;

    pub struct NodeView {
        pub id: Id,
        /// The PipeWire node; `None` for the card of a collapsed group.
        pub node: Option<Id>,
        pub title: gtk::Label,
        /// Shown on cards that changed in the last update.
        pub badge: gtk::Label,
        /// Tooltips of the rows, handed over to the canvas once the card is in place.
        pub tips: RefCell<Vec<(Id, String)>>,
        /// The PipeWire nodes the card stands for: one, or a collapsed group's.
        pub members: Vec<Id>,
        pub group: Option<Id>,
        pub widget: gtk::Widget,
        pub inputs: Vec<(Id, gtk::Label)>,
        pub outputs: Vec<(Id, gtk::Label)>,
        /// Rows are keyed by row id, see `Canvas::rows`.
        /// Natural size, known once the node has been measured.
        pub size: Cell<(i32, i32)>,
    }

    /// The frame drawn around a group.
    pub struct GroupView {
        pub id: Id,
        pub parent: Option<Id>,
        pub widget: gtk::Widget,
        pub title: gtk::Label,
    }

    /// What a drawn link stands for.
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum Kind {
        Link,
        /// A link that no longer exists.
        Gone,
        /// A route through hidden nodes.
        Through,
    }

    /// Points along a link, and the link's colour.
    pub type Route = (Vec<layout::Point>, gdk::RGBA);

    /// What the pointer is dragging.
    #[derive(Default)]
    pub enum Drag {
        #[default]
        None,
        /// The view. `click` until the pointer has moved noticeably.
        Pan { origin: (f64, f64), button: u32 },
        /// A new link, from a port, the selected ports or a whole node.
        Link {
            from: End,
            /// What the pointer is over, if a link can end there.
            to: Option<End>,
            start: (f64, f64),
            at: (f64, f64),
        },
    }

    pub struct Canvas {
        /// Port behind every port row. In split view a port has several rows.
        pub rows: RefCell<HashMap<Id, Id>>,
        /// Ports picked with Ctrl+click; dragging one of them drags them all.
        pub selection: RefCell<HashSet<Id>>,
        pub selected_link: Cell<Option<Id>>,
        pub drag: RefCell<Drag>,
        pub commands: RefCell<Option<Box<dyn Fn(Command)>>>,
        /// In split view: the link of every row that has one.
        pub row_links: RefCell<HashMap<Id, Id>>,
        /// Views to return to with Backspace: offset and scale.
        pub history: RefCell<Vec<((f64, f64), f64)>>,
        /// Where every card and group frame is drawn right now.
        pub shown: RefCell<HashMap<Id, layout::Point>>,
        pub shown_groups: RefCell<HashMap<Id, layout::Point>>,
        /// Where they were when the current layout was made; they glide from
        /// there to their new places.
        pub from: RefCell<HashMap<Id, layout::Point>>,
        pub from_groups: RefCell<HashMap<Id, layout::Point>>,
        /// Start of the glide as frame time, once its first frame is drawn.
        pub glide_start: Cell<Option<i64>>,
        /// 0 at the start of the glide, 1 when everything is in place.
        pub progress: Cell<f64>,
        pub ticking: Cell<bool>,
        /// Every link's route as a row of evenly spaced points, with its
        /// colour; and the same for the layout before. While cards glide,
        /// links are drawn part-way between the two.
        /// The picture of the links, with what it was painted for (level of
        /// detail, selected link and `generation`) and the area it covers.
        pub layer: RefCell<Option<((u32, Option<Id>, u64), graphene::Rect, gdk::Texture)>>,
        /// Counts layouts.
        pub generation: Cell<u64>,
        pub routes: RefCell<HashMap<Id, Route>>,
        pub routes_from: RefCell<HashMap<Id, Route>>,
        /// Time until which the marks on changed cards pulse, and the time
        /// of the frame being drawn (both monotonic, in microseconds).
        pub pulse_until: Cell<i64>,
        pub frame: Cell<i64>,
        /// What the last change removed: kept on screen, greyed out, until
        /// the next change.
        pub ghost: RefCell<Graph>,
        /// The graph as shown: the real one plus `ghost`.
        pub view: RefCell<Graph>,
        /// Nodes that the last update added or whose ports or links it changed.
        pub changed: RefCell<HashSet<Id>>,
        /// Draw attention to the changed cards at the next rebuild.
        pub flash: Cell<bool>,
        /// The list of all nodes and ports, a fixed place to drag links from.
        pub panel: gtk::Box,
        pub panel_entry: gtk::SearchEntry,
        pub panel_list: gtk::Box,
        /// What each line of the list stands for.
        pub panel_rows: RefCell<Vec<(gtk::Widget, End)>>,
        /// Nodes that are not shown.
        pub hidden: RefCell<HashSet<Id>>,
        /// Show what the rules hide, marked as such.
        pub reveal: Cell<bool>,
        /// Told when a text field of the canvas gains or loses the keyboard.
        pub typing: RefCell<Option<Box<dyn Fn(bool)>>>,
        /// For every port row with links: what it is linked to, as markup.
        pub row_tips: RefCell<HashMap<Id, String>>,
        /// Where the last press happened.
        pub pressed: Cell<(f64, f64)>,
        /// A pointer drawn by the canvas itself, for scripted demos.
        pub fake_pointer: Cell<Option<(f64, f64)>>,
        /// Sharpens the link picture, for pictures taken at more than
        /// screen resolution.
        pub boost: Cell<f32>,
        /// Whether the user has chosen a view. Until then the graph is fitted
        /// again whenever it or the size of the canvas changes.
        pub moved: Cell<bool>,
        pub last_size: Cell<(i32, i32)>,
        pub search: RefCell<Option<Query>>,
        /// Cards that match the search, best first, and the one in focus.
        pub matches: RefCell<Vec<Id>>,
        pub current: Cell<usize>,
        /// Says what the current drag will do; follows the pointer.
        pub tip: gtk::Label,
        pub graph: RefCell<Graph>,
        pub config: RefCell<Config>,
        /// Paths of the groups shown as a single box.
        pub collapsed: RefCell<HashSet<String>>,
        /// Show one port row per link instead of one per port.
        pub split: Cell<bool>,
        pub nodes: RefCell<Vec<NodeView>>,
        /// Outer groups before the groups inside them.
        pub groups: RefCell<Vec<GroupView>>,
        pub links: RefCell<Vec<layout::Link>>,
        /// `None` while the graph has changed and not been laid out again.
        pub layout: RefCell<Option<layout::Layout>>,
        /// Link paths with their colour, in drawing order.
        pub paths: RefCell<Vec<(Id, gsk::Path, gdk::RGBA, Option<Fade>, Kind)>>,
        /// Links that stand for a route through hidden nodes.
        pub through: RefCell<HashSet<Id>>,
        /// Both ends of every link, with the link's colour.
        pub dots: RefCell<Vec<(layout::Point, gdk::RGBA)>>,
        /// Palette entry of every link.
        pub colours: RefCell<HashMap<Id, usize>>,
        /// Position of the layout's origin in widget coordinates.
        pub offset: Cell<(f64, f64)>,
        pub scale: Cell<f64>,
        /// Fit the graph into view at the next allocation.
        pub fit: Cell<bool>,
        pub pointer: Cell<(f64, f64)>,
    }

    impl Default for Canvas {
        fn default() -> Self {
            Self {
                graph: RefCell::default(),
                config: RefCell::default(),
                collapsed: RefCell::default(),
                split: Cell::default(),
                nodes: RefCell::default(),
                groups: RefCell::default(),
                links: RefCell::default(),
                layout: RefCell::default(),
                paths: RefCell::default(),
                through: RefCell::default(),
                dots: RefCell::default(),
                colours: RefCell::default(),
                offset: Cell::default(),
                scale: Cell::new(1.0),
                fit: Cell::new(true),
                pointer: Cell::default(),
                rows: RefCell::default(),
                selection: RefCell::default(),
                selected_link: Cell::default(),
                drag: RefCell::default(),
                commands: RefCell::default(),
                row_links: RefCell::default(),
                history: RefCell::default(),
                shown: RefCell::default(),
                shown_groups: RefCell::default(),
                from: RefCell::default(),
                from_groups: RefCell::default(),
                glide_start: Cell::default(),
                progress: Cell::new(1.0),
                ticking: Cell::default(),
                layer: RefCell::default(),
                generation: Cell::default(),
                routes: RefCell::default(),
                routes_from: RefCell::default(),
                pulse_until: Cell::default(),
                frame: Cell::default(),
                ghost: RefCell::default(),
                view: RefCell::default(),
                changed: RefCell::default(),
                flash: Cell::default(),
                panel: gtk::Box::new(gtk::Orientation::Vertical, 8),
                panel_entry: gtk::SearchEntry::new(),
                panel_list: gtk::Box::new(gtk::Orientation::Vertical, 0),
                panel_rows: RefCell::default(),
                hidden: RefCell::default(),
                reveal: Cell::default(),
                typing: RefCell::default(),
                row_tips: RefCell::default(),
                pressed: Cell::default(),
                fake_pointer: Cell::default(),
                boost: Cell::new(1.0),
                moved: Cell::default(),
                last_size: Cell::default(),
                search: RefCell::default(),
                matches: RefCell::default(),
                current: Cell::default(),
                tip: gtk::Label::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Canvas {
        const NAME: &'static str = "TangleWireCanvas";
        type Type = super::Canvas;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Canvas {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_hexpand(true);
            obj.set_vexpand(true);
            obj.set_overflow(gtk::Overflow::Hidden);
            self.tip.add_css_class("osd");
            self.tip.add_css_class("tw-tip");
            self.tip.set_can_target(false);
            self.tip.set_visible(false);
            self.tip.set_parent(&*obj);

            self.panel.add_css_class("tw-panel");
            self.panel.set_visible(false);
            self.panel_entry.add_css_class("tw-search");
            self.panel_entry
                .set_placeholder_text(Some("Filter nodes and ports"));
            self.panel.append(&self.panel_entry);
            let scroll = gtk::ScrolledWindow::new();
            scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
            scroll.set_vexpand(true);
            scroll.set_child(Some(&self.panel_list));
            self.panel.append(&scroll);
            self.panel.set_parent(&*obj);
            self.panel_entry.connect_search_changed(glib::clone!(
                #[weak(rename_to = canvas)]
                self,
                move |_| canvas.fill_panel()
            ));
            self.panel_entry.connect_stop_search(glib::clone!(
                #[weak(rename_to = canvas)]
                self,
                move |_| canvas.show_panel(false)
            ));
            let focus = gtk::EventControllerFocus::new();
            for (typing, connect) in [
                (
                    true,
                    gtk::EventControllerFocus::connect_enter
                        as fn(&gtk::EventControllerFocus, _) -> _,
                ),
                (false, gtk::EventControllerFocus::connect_leave),
            ] {
                let canvas = self.downgrade();
                let tell: Box<dyn Fn(&gtk::EventControllerFocus)> = Box::new(move |_| {
                    let Some(canvas) = canvas.upgrade() else {
                        return;
                    };
                    if let Some(tell) = canvas.typing.borrow().as_ref() {
                        tell(typing);
                    }
                });
                connect(&focus, tell);
            }
            self.panel_entry.add_controller(focus);

            let drag = gtk::GestureDrag::new();
            drag.set_button(0);
            drag.connect_drag_begin(glib::clone!(
                #[weak(rename_to = canvas)]
                self,
                move |gesture, x, y| {
                    let state = gesture.current_event_state();
                    let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
                    canvas.press(gesture.current_button(), ctrl, x, y);
                }
            ));
            drag.connect_drag_update(glib::clone!(
                #[weak(rename_to = canvas)]
                self,
                move |_, dx, dy| canvas.drag_update(dx, dy)
            ));
            drag.connect_drag_end(glib::clone!(
                #[weak(rename_to = canvas)]
                self,
                move |_, dx, dy| canvas.release(dx, dy)
            ));
            obj.add_controller(drag);

            let motion = gtk::EventControllerMotion::new();
            motion.connect_motion(glib::clone!(
                #[weak(rename_to = canvas)]
                self,
                move |_, x, y| canvas.hover(x, y)
            ));
            motion.connect_leave(glib::clone!(
                #[weak(rename_to = canvas)]
                self,
                move |_| {
                    if matches!(*canvas.drag.borrow(), Drag::None) {
                        canvas.tip.set_visible(false);
                    }
                }
            ));
            obj.add_controller(motion);

            let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
            scroll.connect_scroll(glib::clone!(
                #[weak(rename_to = canvas)]
                self,
                #[upgrade_or]
                glib::Propagation::Proceed,
                move |_, _, dy| {
                    // A list scrolled to its end must not zoom the graph.
                    let (x, y) = canvas.pointer.get();
                    if !canvas.in_panel(x, y) {
                        canvas.zoom(1.15f64.powf(-dy));
                    }
                    glib::Propagation::Stop
                }
            ));
            obj.add_controller(scroll);
        }

        fn dispose(&self) {
            self.tip.unparent();
            self.panel.unparent();
            self.clear();
        }
    }

    impl WidgetImpl for Canvas {
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (0, 0, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let resized = self.last_size.replace((width, height)) != (width, height);
            if !self.moved.get() && (resized || self.layout.borrow().is_none()) {
                self.fit.set(true);
            }
            if self.layout.borrow().is_none() {
                self.run_layout();
            }
            if self.fit.replace(false) {
                self.fit_into(width as f64, height as f64);
            }
            self.keep_in_view(width as f64, height as f64);
            self.place_tip(width, height);
            if self.panel.is_visible() {
                let w = self
                    .panel
                    .measure(gtk::Orientation::Horizontal, -1)
                    .1
                    .max(PANEL_WIDTH);
                let origin = graphene::Point::new(PANEL_MARGIN as f32, PANEL_MARGIN as f32);
                let room = (height - 2 * PANEL_MARGIN).max(0);
                self.panel
                    .allocate(w, room, -1, Some(gsk::Transform::new().translate(&origin)));
            }
            let layout = self.layout.borrow();
            let Some(layout) = layout.as_ref() else {
                return;
            };
            let (ox, oy) = self.offset.get();
            let scale = self.scale.get();
            let transform = |at: layout::Point| {
                let origin =
                    graphene::Point::new((ox + at.x * scale) as f32, (oy + at.y * scale) as f32);
                gsk::Transform::new()
                    .translate(&origin)
                    .scale(scale as f32, scale as f32)
            };
            // Slow start, slow stop.
            let p = self.progress.get();
            let eased = p * p * (3.0 - 2.0 * p);
            let glide = |from: Option<&layout::Point>, to: layout::Point| match from {
                Some(from) => layout::Point {
                    x: from.x + (to.x - from.x) * eased,
                    y: from.y + (to.y - from.y) * eased,
                },
                None => to,
            };
            let mut shown = HashMap::new();
            let from = self.from_groups.borrow();
            for group in self.groups.borrow().iter() {
                let rect = layout.groups[&group.id];
                let size = rect.size();
                let at = glide(from.get(&group.id), rect.min);
                shown.insert(group.id, at);
                group.widget.allocate(
                    size.w.round() as i32,
                    size.h.round() as i32,
                    -1,
                    Some(transform(at)),
                );
            }
            self.shown_groups.replace(shown);
            let mut shown = HashMap::new();
            let from = self.from.borrow();
            for node in self.nodes.borrow().iter() {
                let (w, h) = node.size.get();
                let at = glide(from.get(&node.id), layout.nodes[&node.id]);
                shown.insert(node.id, at);
                node.widget.allocate(w, h, -1, Some(transform(at)));
            }
            self.shown.replace(shown);
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            for group in self.groups.borrow().iter() {
                obj.snapshot_child(&group.widget, snapshot);
            }
            let (ox, oy) = self.offset.get();
            let scale = self.scale.get() as f32;
            let stroke = gsk::Stroke::new(1.6);
            // While cards glide to a new arrangement every link is drawn
            // part-way between its old route and its new one. Links that are
            // new fade in, links that are gone fade out.
            let p = self.progress.get();
            let eased = p * p * (3.0 - 2.0 * p);
            let mut moving: Vec<(Vec<layout::Point>, gdk::RGBA, f64)> = Vec::new();
            if p < 1.0 {
                let (from, to) = (self.routes_from.borrow(), self.routes.borrow());
                for (id, (points, colour)) in to.iter() {
                    match from.get(id) {
                        Some((old, _)) => {
                            let between = old.iter().zip(points).map(|(a, b)| layout::Point {
                                x: a.x + (b.x - a.x) * eased,
                                y: a.y + (b.y - a.y) * eased,
                            });
                            moving.push((between.collect(), *colour, 1.0));
                        }
                        None => moving.push((points.clone(), *colour, p)),
                    }
                }
                for (id, (points, colour)) in from.iter().filter(|(id, _)| !to.contains_key(id)) {
                    let _ = id;
                    moving.push((points.clone(), *colour, 1.0 - p));
                }
            }
            snapshot.save();
            snapshot.translate(&graphene::Point::new(ox as f32, oy as f32));
            snapshot.scale(scale, scale);
            for (points, colour, opacity) in &moving {
                let builder = gsk::PathBuilder::new();
                builder.move_to(points[0].x as f32, points[0].y as f32);
                for point in &points[1..] {
                    builder.line_to(point.x as f32, point.y as f32);
                }
                snapshot.push_opacity(*opacity);
                snapshot.append_stroke(&builder.to_path(), &stroke, colour);
                snapshot.pop();
            }
            let selected = self.selected_link.get();
            let settled = p >= 1.0;
            // Once everything is in place the links are a picture that is
            // painted once and then only moved about. Painting ninety long
            // curves afresh for every frame made panning stutter.
            if settled {
                if let Some((texture, area)) = self.link_layer(selected) {
                    snapshot.append_texture(&texture, &area);
                }
            }
            // What the last update changed: an orange rim with a glow that
            // pulses for a while. Drawn here rather than by the style sheet,
            // so that the whole canvas is repainted with every step.
            let frame = self.frame.get();
            let level = if frame < self.pulse_until.get() {
                0.5 - 0.5 * (frame as f64 / 1_000_000.0 * std::f64::consts::TAU / 1.6).cos()
            } else {
                0.45
            } as f32;
            let orange = |alpha: f32| gdk::RGBA::new(CHANGED.0, CHANGED.1, CHANGED.2, alpha);
            let shown = self.shown.borrow();
            for node in self.nodes.borrow().iter().filter(|n| n.badge.is_visible()) {
                let Some(at) = shown.get(&node.id) else {
                    continue;
                };
                let (w, h) = node.size.get();
                let rect = graphene::Rect::new(at.x as f32, at.y as f32, w as f32, h as f32);
                let outline = gsk::RoundedRect::from_rect(rect, 12.0);
                // The glow is a few flat halos, each wider and fainter than
                // the one inside it. Flat rounded rectangles cost the renderer
                // next to nothing; blurred shadows come out as a cross at some
                // zoom levels, and stroked outlines made panning stutter.
                let reach = 8.0 + 14.0 * level;
                for (part, alpha) in [(1.0, 0.07), (0.72, 0.09), (0.48, 0.12), (0.26, 0.16)] {
                    let halo = orange(alpha * (0.55 + 0.45 * level));
                    snapshot.append_outset_shadow(
                        &outline,
                        &halo,
                        0.0,
                        0.0,
                        2.5 + reach * part,
                        0.0,
                    );
                }
                snapshot.append_outset_shadow(&outline, &orange(1.0), 0.0, 0.0, 2.5, 0.0);
            }
            drop(shown);
            snapshot.restore();
            for node in self.nodes.borrow().iter() {
                obj.snapshot_child(&node.widget, snapshot);
            }
            // A dot where each link meets its port, drawn over the card edge.
            snapshot.save();
            snapshot.translate(&graphene::Point::new(ox as f32, oy as f32));
            snapshot.scale(scale, scale);
            // A clipped square rather than a filled circle path: this one the
            // renderer draws directly.
            let dot = |at: &layout::Point, colour: &gdk::RGBA| {
                let square = graphene::Rect::new(at.x as f32 - 3.5, at.y as f32 - 3.5, 7.0, 7.0);
                snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(square, 3.5));
                snapshot.append_color(colour, &square);
                snapshot.pop();
            };
            for (points, colour, opacity) in &moving {
                snapshot.push_opacity(*opacity);
                dot(&points[0], colour);
                dot(&points[points.len() - 1], colour);
                snapshot.pop();
            }
            for (at, colour) in self.dots.borrow().iter().filter(|_| settled) {
                dot(at, colour);
            }
            snapshot.restore();
            self.snapshot_pointers(snapshot);
            // The link being dragged out.
            if let Drag::Link { start, at, .. } = &*self.drag.borrow() {
                let builder = gsk::PathBuilder::new();
                builder.move_to(start.0 as f32, start.1 as f32);
                builder.line_to(at.0 as f32, at.1 as f32);
                let colour = adw::StyleManager::default().accent_color_rgba();
                snapshot.append_stroke(&builder.to_path(), &gsk::Stroke::new(2.5), &colour);
            }
            obj.snapshot_child(&self.panel, snapshot);
            obj.snapshot_child(&self.tip, snapshot);
            // The pointer of a scripted demo: an ordinary arrow.
            if let Some((x, y)) = self.fake_pointer.get() {
                const ARROW: [(f32, f32); 7] = [
                    (0.0, 0.0),
                    (0.0, 20.0),
                    (5.3, 15.4),
                    (8.9, 23.2),
                    (12.0, 21.8),
                    (8.4, 14.2),
                    (15.0, 14.2),
                ];
                let builder = gsk::PathBuilder::new();
                builder.move_to(x as f32, y as f32);
                for (dx, dy) in &ARROW[1..] {
                    builder.line_to(x as f32 + dx, y as f32 + dy);
                }
                builder.close();
                let arrow = builder.to_path();
                snapshot.append_fill(&arrow, gsk::FillRule::Winding, &gdk::RGBA::WHITE);
                snapshot.append_stroke(&arrow, &gsk::Stroke::new(1.4), &gdk::RGBA::BLACK);
            }
        }
    }

    impl Canvas {
        /// The links around what is in view as one picture, and the part of
        /// the layout it covers. It is painted again only when the links, the
        /// zoom or the selected link change, or when the view has moved past
        /// its edge.
        fn link_layer(&self, selected: Option<Id>) -> Option<(gdk::Texture, graphene::Rect)> {
            /// Room around the layout for the width of the lines.
            const EDGE: f32 = 8.0;
            let obj = self.obj();
            let size = self.layout.borrow().as_ref()?.size;
            let whole = graphene::Rect::new(
                -EDGE,
                -EDGE,
                size.w as f32 + 2.0 * EDGE,
                size.h as f32 + 2.0 * EDGE,
            );
            // What the canvas shows, in layout coordinates.
            let scale = self.scale.get() as f32;
            let (ox, oy) = self.offset.get();
            let (width, height) = (obj.width() as f32 / scale, obj.height() as f32 / scale);
            let view =
                graphene::Rect::new(-(ox as f32) / scale, -(oy as f32) / scale, width, height);
            let seen = view.intersection(&whole)?;
            let detail = scale * obj.scale_factor() as f32 * self.boost.get();
            let key = (detail.to_bits(), selected, self.generation.get());
            if let Some((_, area, texture)) =
                self.layer.borrow().as_ref().filter(|layer| layer.0 == key)
            {
                if area.contains_rect(&seen) {
                    return Some((texture.clone(), *area));
                }
            }
            // Half a canvas more on every side, so that panning has some way
            // to go before the picture has to be painted again.
            let around = graphene::Rect::new(
                view.x() - width / 2.0,
                view.y() - height / 2.0,
                width * 2.0,
                height * 2.0,
            );
            let area = around.intersection(&whole)?;

            let paint = gtk::Snapshot::new();
            paint.scale(detail, detail);
            let stroke = gsk::Stroke::new(1.6);
            let dashed = gsk::Stroke::new(1.6);
            dashed.set_dash(&[5.0, 5.0]);
            // Longer dashes than a link that is gone, and in full colour.
            let bridged = gsk::Stroke::new(1.6);
            bridged.set_dash(&[11.0, 5.0]);
            for (id, path, colour, fade, kind) in self.paths.borrow().iter() {
                if selected == Some(*id) {
                    paint.append_stroke(path, &gsk::Stroke::new(4.5), &obj.color());
                }
                match kind {
                    Kind::Gone => {
                        let faint =
                            gdk::RGBA::new(colour.red(), colour.green(), colour.blue(), 0.6);
                        paint.append_stroke(path, &dashed, &faint);
                        continue;
                    }
                    Kind::Through => {
                        paint.append_stroke(path, &bridged, colour);
                        continue;
                    }
                    Kind::Link => {}
                }
                let fade = fade
                    .as_ref()
                    .and_then(|fade| Some((fade, path.stroke_bounds(&stroke)?)));
                let Some((fade, bounds)) = fade else {
                    paint.append_stroke(path, &stroke, colour);
                    continue;
                };
                paint.push_stroke(path, &stroke);
                paint.append_linear_gradient(
                    &bounds,
                    &graphene::Point::new(fade.from, 0.0),
                    &graphene::Point::new(fade.to, 0.0),
                    &fade.stops,
                );
                paint.pop();
            }
            let node = paint.to_node()?;
            let renderer = obj.native()?.renderer()?;
            let viewport = graphene::Rect::new(
                area.x() * detail,
                area.y() * detail,
                area.width() * detail,
                area.height() * detail,
            );
            let texture = renderer.render_texture(&node, Some(&viewport));
            self.layer.replace(Some((key, area, texture.clone())));
            Some((texture, area))
        }

        /// An arrow at the edge of the canvas for every changed card that is
        /// out of view, pointing towards it.
        fn snapshot_pointers(&self, snapshot: &gtk::Snapshot) {
            const INSET: f64 = 22.0;
            let obj = self.obj();
            let (width, height) = (obj.width() as f64, obj.height() as f64);
            let (ox, oy) = self.offset.get();
            let scale = self.scale.get();
            let shown = self.shown.borrow();
            let colour = gdk::RGBA::new(CHANGED.0, CHANGED.1, CHANGED.2, 1.0);
            let (cx, cy) = (width / 2.0, height / 2.0);
            for node in self.nodes.borrow().iter().filter(|n| n.badge.is_visible()) {
                let Some(at) = shown.get(&node.id) else {
                    continue;
                };
                let (w, h) = node.size.get();
                let (left, top) = (ox + at.x * scale, oy + at.y * scale);
                let (right, bottom) = (left + w as f64 * scale, top + h as f64 * scale);
                if right > 0.0 && left < width && bottom > 0.0 && top < height {
                    continue;
                }
                // From the middle of the canvas towards the card, stopping
                // just short of the edge.
                let (dx, dy) = ((left + right) / 2.0 - cx, (top + bottom) / 2.0 - cy);
                let reach = ((cx - INSET) / dx.abs()).min((cy - INSET) / dy.abs());
                let (x, y) = (cx + dx * reach, cy + dy * reach);
                let length = dx.hypot(dy);
                let (ux, uy) = (dx / length, dy / length);
                let point = |along: f64, across: f64| {
                    graphene::Point::new(
                        (x + ux * along - uy * across) as f32,
                        (y + uy * along + ux * across) as f32,
                    )
                };
                let builder = gsk::PathBuilder::new();
                let (tip, a, b) = (point(14.0, 0.0), point(-8.0, 11.0), point(-8.0, -11.0));
                builder.move_to(tip.x(), tip.y());
                builder.line_to(a.x(), a.y());
                builder.line_to(b.x(), b.y());
                builder.close();
                let arrow = builder.to_path();
                let outline = gdk::RGBA::new(0.0, 0.0, 0.0, 0.55);
                snapshot.append_stroke(&arrow, &gsk::Stroke::new(3.0), &outline);
                snapshot.append_fill(&arrow, gsk::FillRule::Winding, &colour);
            }
        }

        /// Stops the graph from being moved out of sight: a strip of it
        /// always stays inside the canvas.
        fn keep_in_view(&self, width: f64, height: f64) {
            const STRIP: f64 = 120.0;
            let Some(size) = self.layout.borrow().as_ref().map(|l| l.size) else {
                return;
            };
            let scale = self.scale.get();
            let (x, y) = self.offset.get();
            let limit = |at: f64, extent: f64, room: f64| {
                let strip = STRIP.min(room / 2.0);
                at.clamp(
                    strip - extent * scale,
                    (room - strip).max(strip - extent * scale),
                )
            };
            self.offset
                .set((limit(x, size.w, width), limit(y, size.h, height)));
        }

        /// Puts the tip next to the pointer, inside the canvas.
        fn place_tip(&self, width: i32, height: i32) {
            if !self.tip.is_visible() {
                return;
            }
            let at = match &*self.drag.borrow() {
                Drag::Link { at, .. } => *at,
                _ => self.pointer.get(),
            };
            let w = self.tip.measure(gtk::Orientation::Horizontal, -1).1;
            let h = self.tip.measure(gtk::Orientation::Vertical, w).1;
            let x = (at.0 + 16.0).min((width - w) as f64).max(0.0);
            let y = (at.1 + 20.0).min((height - h) as f64).max(0.0);
            let origin = graphene::Point::new(x as f32, y as f32);
            self.tip
                .allocate(w, h, -1, Some(gsk::Transform::new().translate(&origin)));
        }

        /// Adds or removes a style class on the labels that stand for `end`.
        fn set_class(&self, end: &End, class: &str, on: bool) {
            for (widget, row) in self.panel_rows.borrow().iter() {
                let same = match (row, end) {
                    (End::Ports(a), End::Ports(b)) => b.contains(&a[0]),
                    (a, b) => a == b,
                };
                if same && on {
                    widget.add_css_class(class);
                } else if same {
                    widget.remove_css_class(class);
                }
            }
            let rows = self.rows.borrow();
            for node in self.nodes.borrow().iter() {
                let mut labels = Vec::new();
                match end {
                    End::Node(id) if node.node == Some(*id) => labels.push(&node.title),
                    End::Ports(ports) => {
                        let all = node.inputs.iter().chain(&node.outputs);
                        let hit =
                            all.filter(|(row, _)| rows.get(row).is_some_and(|p| ports.contains(p)));
                        labels.extend(hit.map(|(_, label)| label));
                    }
                    End::Node(_) => {}
                }
                for label in labels {
                    if on {
                        label.add_css_class(class);
                    } else {
                        label.remove_css_class(class);
                    }
                }
            }
        }

        fn in_panel(&self, x: f64, y: f64) -> bool {
            let picked = self.obj().pick(x, y, gtk::PickFlags::DEFAULT);
            picked.is_some_and(|w| {
                w == *self.panel.upcast_ref::<gtk::Widget>() || w.is_ancestor(&self.panel)
            })
        }

        /// Shows or hides the list of nodes.
        pub fn show_panel(&self, show: bool) {
            self.panel.set_visible(show);
            if show {
                self.fill_panel();
                self.panel_entry.grab_focus();
            } else if let Some(window) = self.obj().root().and_downcast::<gtk::Window>() {
                // Hand the keyboard back to the graph.
                gtk::prelude::GtkWindowExt::set_focus(&window, gtk::Widget::NONE);
            }
            self.obj().queue_allocate();
        }

        /// Lists every shown node with its ports, narrowed down by the filter.
        pub fn fill_panel(&self) {
            if !self.panel.is_visible() {
                return;
            }
            while let Some(child) = self.panel_list.first_child() {
                self.panel_list.remove(&child);
            }
            let graph = self.graph.borrow();
            let hidden = self.hidden.borrow();
            let query = Query::parse(&self.panel_entry.text()).ok().flatten();
            let mut nodes: Vec<_> = graph
                .nodes
                .values()
                .filter(|n| !hidden.contains(&n.id))
                .filter(|n| {
                    query
                        .as_ref()
                        .is_none_or(|q| q.matches(&graph, n.id).is_some())
                })
                .collect();
            nodes.sort_by_key(|n| (n.name().to_lowercase(), n.id));
            let mut rows = Vec::new();
            for node in nodes {
                let name = graph.label(node.id);
                let title = gtk::Label::new(Some(&name));
                title.set_xalign(0.0);
                title.set_ellipsize(gtk::pango::EllipsizeMode::End);
                title.add_css_class("tw-title");
                title.add_css_class("tw-list-node");
                title.set_cursor_from_name(Some("crosshair"));
                self.panel_list.append(&title);
                rows.push((title.upcast(), End::Node(node.id)));
                for (direction, word) in [(Direction::In, "in"), (Direction::Out, "out")] {
                    for port in graph.ports_of(node.id, direction) {
                        let label = gtk::Label::new(Some(&format!("{word}  {}", port.name)));
                        label.set_xalign(0.0);
                        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                        label.add_css_class("tw-port");
                        label.add_css_class("tw-list-port");
                        label.set_cursor_from_name(Some("crosshair"));
                        self.panel_list.append(&label);
                        rows.push((label.upcast(), End::Ports(vec![port.id])));
                    }
                }
            }
            self.panel_rows.replace(rows);
        }

        /// Moves the view so that a card is in the middle.
        pub fn centre_on(&self, card: Id) {
            self.moved.set(true);
            let obj = self.obj();
            let nodes = self.nodes.borrow();
            let layout = self.layout.borrow();
            let Some(node) = nodes.iter().find(|n| n.id == card) else {
                return;
            };
            let Some(at) = layout.as_ref().and_then(|l| l.nodes.get(&card)) else {
                return;
            };
            let (w, h) = node.size.get();
            let scale = self.scale.get();
            self.offset.set((
                obj.width() as f64 / 2.0 - (at.x + w as f64 / 2.0) * scale,
                obj.height() as f64 / 2.0 - (at.y + h as f64 / 2.0) * scale,
            ));
            obj.queue_allocate();
        }

        /// The port or node title at a point of the canvas.
        fn hit(&self, x: f64, y: f64) -> Option<End> {
            let widget = self.obj().pick(x, y, gtk::PickFlags::DEFAULT)?;
            if let Some((_, end)) = self.panel_rows.borrow().iter().find(|row| row.0 == widget) {
                return Some(end.clone());
            }
            let rows = self.rows.borrow();
            for node in self.nodes.borrow().iter() {
                if widget == *node.title.upcast_ref::<gtk::Widget>() {
                    return node.node.map(End::Node);
                }
                for (row, label) in node.inputs.iter().chain(&node.outputs) {
                    if widget == *label.upcast_ref::<gtk::Widget>() {
                        // Not the rows of a node that is gone.
                        let real = |port: &&Id| self.graph.borrow().ports.contains_key(port);
                        return rows
                            .get(row)
                            .filter(real)
                            .map(|&port| End::Ports(vec![port]));
                    }
                }
            }
            None
        }

        /// The pointer moved to a point of the canvas without a button held:
        /// over a port row, say what the port is linked to.
        pub fn hover(&self, x: f64, y: f64) {
            self.pointer.set((x, y));
            if !matches!(*self.drag.borrow(), Drag::None) {
                return;
            }
            let tip = self
                .hit_row(x, y)
                .and_then(|row| self.row_tips.borrow().get(&row).cloned());
            match tip {
                Some(markup) => {
                    self.tip.set_markup(&markup);
                    self.tip.set_visible(true);
                }
                None => self.tip.set_visible(false),
            }
            self.obj().queue_allocate();
        }

        /// A mouse button went down at a point of the canvas.
        pub fn press(&self, button: u32, ctrl: bool, x: f64, y: f64) {
            self.pressed.set((x, y));
            self.tip.set_visible(false);
            // The mouse's "back" button.
            if button == 8 {
                self.back();
                self.drag.replace(Drag::None);
                return;
            }
            let primary = button == gdk::BUTTON_PRIMARY;
            let hit = if primary { self.hit(x, y) } else { None };
            // The list handles its own scrolling and typing.
            if hit.is_none() && self.in_panel(x, y) {
                self.drag.replace(Drag::None);
                return;
            }
            let drag = match hit {
                Some(End::Ports(ports)) if ctrl => {
                    let mut selection = self.selection.borrow_mut();
                    if !selection.remove(&ports[0]) {
                        selection.insert(ports[0]);
                    }
                    Drag::None
                }
                Some(End::Ports(ports)) => {
                    let graph = self.graph.borrow();
                    let mut from: Vec<Id> = self.selection.borrow().iter().copied().collect();
                    if from.contains(&ports[0]) {
                        from.sort_by_key(|id| graph.ports.get(id).map(|p| (p.node, p.index)));
                    } else {
                        from = ports;
                    }
                    Drag::Link {
                        from: End::Ports(from),
                        to: None,
                        start: (x, y),
                        at: (x, y),
                    }
                }
                Some(from) => Drag::Link {
                    from,
                    to: None,
                    start: (x, y),
                    at: (x, y),
                },
                None => Drag::Pan {
                    origin: self.offset.get(),
                    button,
                },
            };
            if let Drag::Link { from, .. } = &drag {
                self.set_class(from, "tw-source", true);
                self.tip
                    .set_text(&connect::explain(&self.graph.borrow(), from, None));
                self.tip.set_visible(true);
            }
            self.drag.replace(drag);
            self.mark_selection();
        }

        pub fn drag_update(&self, dx: f64, dy: f64) {
            match &mut *self.drag.borrow_mut() {
                Drag::Pan { origin, .. } => {
                    self.moved.set(true);
                    self.offset.set((origin.0 + dx, origin.1 + dy));
                    self.obj().queue_allocate();
                }
                Drag::Link {
                    from,
                    to,
                    start,
                    at,
                } => {
                    *at = (start.0 + dx, start.1 + dy);
                    let over = self.hit(at.0, at.1).filter(|over| over != from);
                    if over != *to {
                        if let Some(old) = to.as_ref() {
                            self.set_class(old, "tw-target", false);
                        }
                        if let Some(new) = over.as_ref() {
                            self.set_class(new, "tw-target", true);
                        }
                        let graph = self.graph.borrow();
                        self.tip
                            .set_text(&connect::explain(&graph, from, over.as_ref()));
                        *to = over;
                    }
                    self.obj().queue_allocate();
                }
                Drag::None => {}
            }
        }

        /// The button came up again, this far from where it went down.
        pub fn release(&self, dx: f64, dy: f64) {
            let (x, y) = self.pressed.get();
            match self.drag.take() {
                Drag::Link { from, to, .. } => {
                    self.tip.set_visible(false);
                    self.set_class(&from, "tw-source", false);
                    if let Some(to) = to {
                        self.set_class(&to, "tw-target", false);
                        let commands = connect::plan(&self.graph.borrow(), &from, &to);
                        if !commands.is_empty() {
                            self.selection.borrow_mut().clear();
                            self.mark_selection();
                        }
                        commands.into_iter().for_each(|c| self.send(c));
                    } else if dx * dx + dy * dy < 9.0 && self.in_panel(x, y) {
                        // A click on a line of the list shows where the node is.
                        let graph = self.graph.borrow();
                        let node = match &from {
                            End::Node(id) => Some(*id),
                            End::Ports(ports) => ports
                                .first()
                                .and_then(|p| graph.ports.get(p))
                                .map(|p| p.node),
                        };
                        let card = self
                            .nodes
                            .borrow()
                            .iter()
                            .find(|n| node.is_some_and(|id| n.members.contains(&id)))
                            .map(|n| n.id);
                        if let Some(card) = card {
                            self.centre_on(card);
                        }
                    }
                }
                // A click rather than a drag: select the link under the pointer.
                // A right click on a port row goes to the other end of its link.
                Drag::Pan {
                    button: gdk::BUTTON_SECONDARY,
                    ..
                } if dx * dx + dy * dy < 9.0 => {
                    if let Some(row) = self.hit_row(x, y) {
                        self.jump(row);
                    }
                }
                Drag::Pan {
                    button: gdk::BUTTON_PRIMARY,
                    ..
                } if dx * dx + dy * dy < 9.0 => {
                    let (ox, oy) = self.offset.get();
                    let scale = self.scale.get();
                    let point =
                        graphene::Point::new(((x - ox) / scale) as f32, ((y - oy) / scale) as f32);
                    let reach = (6.0 / scale) as f32;
                    let paths = self.paths.borrow();
                    let near = paths.iter().filter_map(|(id, path, ..)| {
                        Some((path.closest_point(&point, reach)?.1, *id))
                    });
                    let nearest = near.min_by(|a, b| a.0.total_cmp(&b.0)).map(|n| n.1);
                    self.selected_link.set(nearest);
                }
                _ => {}
            }
            self.obj().queue_draw();
        }

        pub fn send(&self, command: Command) {
            if let Some(send) = self.commands.borrow().as_ref() {
                send(command);
            }
        }

        /// The port row at a point of the canvas.
        fn hit_row(&self, x: f64, y: f64) -> Option<Id> {
            let widget = self.obj().pick(x, y, gtk::PickFlags::DEFAULT)?;
            let nodes = self.nodes.borrow();
            let mut rows = nodes.iter().flat_map(|n| n.inputs.iter().chain(&n.outputs));
            rows.find(|(_, label)| widget == *label.upcast_ref::<gtk::Widget>())
                .map(|r| r.0)
        }

        /// Shows the other end of the link on `row`, remembering the current
        /// view. Outside split view this needs the port to have just one link.
        fn jump(&self, row: Id) {
            let links = self.links.borrow();
            let link = match self.row_links.borrow().get(&row) {
                Some(id) => links.iter().find(|l| l.id == *id),
                None => {
                    let mut on_row = links.iter().filter(|l| l.from == row || l.to == row);
                    on_row.next().filter(|_| on_row.next().is_none())
                }
            };
            let Some(link) = link else { return };
            let layout = self.layout.borrow();
            let Some(path) = layout.as_ref().and_then(|l| l.links.get(&link.id)) else {
                return;
            };
            let end = path.curves.last().map_or(path.start, |c| c.end);
            let (target, other) = if link.from == row {
                (end, link.to)
            } else {
                (path.start, link.from)
            };
            for node in self.nodes.borrow().iter() {
                for (id, label) in node.inputs.iter().chain(&node.outputs) {
                    if *id == other {
                        label.add_css_class("tw-jump");
                    } else {
                        label.remove_css_class("tw-jump");
                    }
                }
            }
            let obj = self.obj();
            let scale = self.scale.get();
            self.moved.set(true);
            self.history.borrow_mut().push((self.offset.get(), scale));
            self.offset.set((
                obj.width() as f64 / 2.0 - target.x * scale,
                obj.height() as f64 / 2.0 - target.y * scale,
            ));
            self.selected_link.set(Some(link.id));
            obj.queue_allocate();
        }

        /// Returns to the view before the last jump.
        pub fn back(&self) {
            if let Some((offset, scale)) = self.history.borrow_mut().pop() {
                self.offset.set(offset);
                self.scale.set(scale);
                self.obj().queue_allocate();
            }
        }

        /// Marks the cards and ports that match the search and dims the rest.
        pub fn apply_search(&self) {
            let graph = self.graph.borrow();
            let search = self.search.borrow();
            let rows = self.rows.borrow();
            let mut matches = Vec::new();
            for node in self.nodes.borrow().iter() {
                let found = search.as_ref().map(|query| {
                    let all = node
                        .members
                        .iter()
                        .filter_map(|&id| query.matches(&graph, id));
                    all.reduce(|mut a, b| {
                        a.score = a.score.max(b.score);
                        a.ports.extend(b.ports);
                        a
                    })
                });
                let set = |widget: &gtk::Widget, class: &str, on: bool| {
                    if on {
                        widget.add_css_class(class);
                    } else {
                        widget.remove_css_class(class);
                    }
                };
                set(&node.widget, "tw-match", matches!(found, Some(Some(_))));
                set(&node.widget, "tw-dim", matches!(found, Some(None)));
                set(&node.widget, "tw-current", false);
                let ports = found
                    .as_ref()
                    .and_then(|f| f.as_ref())
                    .map_or(&[][..], |f| &f.ports);
                for (row, label) in node.inputs.iter().chain(&node.outputs) {
                    let hit = rows.get(row).is_some_and(|port| ports.contains(port));
                    set(label.upcast_ref(), "tw-match-port", hit);
                }
                if let Some(Some(found)) = &found {
                    matches.push((std::cmp::Reverse(found.score), node.id));
                }
            }
            matches.sort();
            self.matches
                .replace(matches.into_iter().map(|m| m.1).collect());
            self.current.set(usize::MAX);
        }

        /// Moves the focus `step` matches on and centres the view on it.
        pub fn step_match(&self, step: isize) {
            let matches = self.matches.borrow();
            if matches.is_empty() {
                return;
            }
            let len = matches.len() as isize;
            let current = match self.current.get() {
                usize::MAX if step < 0 => len - 1,
                usize::MAX => 0,
                at => (at as isize + step).rem_euclid(len),
            };
            self.current.set(current as usize);
            let id = matches[current as usize];
            let obj = self.obj();
            for node in self.nodes.borrow().iter() {
                if node.id != id {
                    node.widget.remove_css_class("tw-current");
                    continue;
                }
                node.widget.add_css_class("tw-current");
                self.moved.set(true);
                let layout = self.layout.borrow();
                let Some(at) = layout.as_ref().and_then(|l| l.nodes.get(&id)) else {
                    continue;
                };
                let (w, h) = node.size.get();
                let scale = self.scale.get();
                self.offset.set((
                    obj.width() as f64 / 2.0 - (at.x + w as f64 / 2.0) * scale,
                    obj.height() as f64 / 2.0 - (at.y + h as f64 / 2.0) * scale,
                ));
                obj.queue_allocate();
            }
        }

        /// Shows which ports are selected.
        pub fn mark_selection(&self) {
            let selection = self.selection.borrow();
            let rows = self.rows.borrow();
            for node in self.nodes.borrow().iter() {
                for (row, label) in node.inputs.iter().chain(&node.outputs) {
                    if rows.get(row).is_some_and(|port| selection.contains(port)) {
                        label.add_css_class("tw-selected");
                    } else {
                        label.remove_css_class("tw-selected");
                    }
                }
            }
        }

        /// Removes every node and group widget.
        pub fn clear(&self) {
            for node in self.nodes.take() {
                node.widget.unparent();
            }
            for group in self.groups.take() {
                group.widget.unparent();
            }
        }

        /// Measures every node, lays the graph out and builds the link paths.
        fn run_layout(&self) {
            let obj = self.obj();
            let before = self.shown.borrow().clone();
            let nodes = self.nodes.borrow();
            let mut graph = layout::Graph {
                links: self.links.borrow().clone(),
                ..layout::Graph::default()
            };
            for group in self.groups.borrow().iter() {
                let w = group.title.measure(gtk::Orientation::Horizontal, -1).1;
                let h = group.title.measure(gtk::Orientation::Vertical, w).1;
                graph.groups.push(layout::Group {
                    id: group.id,
                    parent: group.parent,
                    header: layout::Size {
                        w: w as f64,
                        h: h as f64,
                    },
                });
            }
            for node in nodes.iter() {
                let w = node.widget.measure(gtk::Orientation::Horizontal, -1).1;
                let h = node.widget.measure(gtk::Orientation::Vertical, w).1;
                node.size.set((w, h));
                // Port positions are only known once the node is allocated.
                node.widget.allocate(w, h, -1, None);
                let ports = |list: &[(Id, gtk::Label)]| {
                    list.iter()
                        .map(|(id, label)| {
                            // Both in canvas coordinates: a widget's own
                            // coordinates start inside its padding.
                            let bounds = (
                                label.compute_bounds(&*obj),
                                node.widget.compute_bounds(&*obj),
                            );
                            let y = match bounds {
                                (Some(row), Some(card)) => row.y() + row.height() / 2.0 - card.y(),
                                _ => 0.0,
                            };
                            layout::Port {
                                id: *id,
                                y: y as f64,
                                label: String::new(),
                            }
                        })
                        .collect()
                };
                graph.nodes.push(layout::Node {
                    id: node.id,
                    group: node.group,
                    before: before.get(&node.id).copied(),
                    size: layout::Size {
                        w: w as f64,
                        h: h as f64,
                    },
                    inputs: ports(&node.inputs),
                    outputs: ports(&node.outputs),
                    label: String::new(),
                });
            }
            let engine = layout::Layered::default();
            let mut result = engine.layout(&graph);
            // Starting from an earlier arrangement, the result can still shift
            // once when laid out from itself. Do that now rather than at the
            // next update, so that an update which changes nothing moves nothing.
            if !before.is_empty() {
                for node in &mut graph.nodes {
                    node.before = result.nodes.get(&node.id).copied();
                }
                result = engine.layout(&graph);
            }
            self.start_glide(before);
            let colours = self.colours.borrow();
            let colour_of = |id: &Id| palette(colours.get(id).copied().unwrap_or(0));
            // Links into one port lie on top of each other for a while
            // before they separate. Where they do, they show the average of
            // their colours, so that no single link appears to be the only one.
            let mut fans: HashMap<Id, Vec<Id>> = HashMap::new();
            for link in graph
                .links
                .iter()
                .filter(|l| result.links.contains_key(&l.id))
            {
                fans.entry(link.to).or_default().push(link.id);
            }
            let mut fades: HashMap<Id, Fade> = HashMap::new();
            let mut port_colour: HashMap<Id, gdk::RGBA> = HashMap::new();
            for ids in fans.values_mut().filter(|ids| ids.len() > 1) {
                ids.sort();
                let fan: Vec<_> = ids
                    .iter()
                    .map(|id| (&result.links[id], colour_of(id)))
                    .collect();
                let all: Vec<gdk::RGBA> = fan.iter().map(|f| f.1).collect();
                for (id, fade) in ids.iter().zip(fan_fades(&fan)) {
                    port_colour.insert(*id, mean(&all));
                    if let Some(fade) = fade {
                        fades.insert(*id, fade);
                    }
                }
            }
            let gone: HashSet<Id> = self.ghost.borrow().links.keys().copied().collect();
            let through = self.through.borrow();
            let kind = |id: &Id| match (through.contains(id), gone.contains(id)) {
                (true, _) => Kind::Through,
                (false, true) => Kind::Gone,
                (false, false) => Kind::Link,
            };
            let mut dots = Vec::new();
            // A fixed order, so that redrawing never reshuffles overlaps.
            let mut ids: Vec<&Id> = result.links.keys().collect();
            ids.sort();
            let paths = ids.into_iter().map(|id| {
                let path = &result.links[id];
                let builder = gsk::PathBuilder::new();
                builder.move_to(path.start.x as f32, path.start.y as f32);
                for c in &path.curves {
                    builder.cubic_to(
                        c.c1.x as f32,
                        c.c1.y as f32,
                        c.c2.x as f32,
                        c.c2.y as f32,
                        c.end.x as f32,
                        c.end.y as f32,
                    );
                }
                let colour = colour_of(id);
                let end = path.curves.last().map_or(path.start, |c| c.end);
                let at_port = port_colour.get(id).copied().unwrap_or(colour);
                dots.extend([(path.start, colour), (end, at_port)]);
                (*id, builder.to_path(), colour, fades.remove(id), kind(id))
            });
            let paths = paths.collect();
            self.paths.replace(paths);
            self.generation.set(self.generation.get() + 1);
            let routes = result
                .links
                .iter()
                .map(|(id, path)| (*id, (sample(path), colour_of(id))));
            let old = self.routes.replace(routes.collect());
            self.routes_from.replace(old);
            self.dots.replace(dots);
            self.layout.replace(Some(result));
        }

        /// Lets cards and frames glide from where they are shown now to where
        /// the new layout puts them.
        fn start_glide(&self, from: HashMap<Id, layout::Point>) {
            let obj = self.obj();
            let animate = !from.is_empty() && obj.settings().is_gtk_enable_animations();
            self.from.replace(from);
            self.from_groups.replace(self.shown_groups.borrow().clone());
            self.glide_start.set(None);
            self.progress.set(if animate { 0.0 } else { 1.0 });
            if animate {
                self.ensure_tick();
            }
        }

        /// Keeps frames coming while cards glide or marks pulse.
        pub fn ensure_tick(&self) {
            if self.ticking.replace(true) {
                return;
            }
            self.obj().add_tick_callback(|canvas, clock| {
                let imp = canvas.imp();
                let now = clock.frame_time();
                imp.frame.set(now);
                if imp.progress.get() < 1.0 {
                    let start = imp.glide_start.get().unwrap_or(now);
                    imp.glide_start.set(Some(start));
                    imp.progress.set(((now - start) as f64 / GLIDE).min(1.0));
                    canvas.queue_allocate();
                }
                canvas.queue_draw();
                if imp.progress.get() < 1.0 || now < imp.pulse_until.get() {
                    return glib::ControlFlow::Continue;
                }
                imp.ticking.set(false);
                glib::ControlFlow::Break
            });
        }

        /// Scale at which an area of `size` is fully visible.
        fn fit_scale(&self, size: layout::Size, width: f64, height: f64) -> f64 {
            let fit = (width / (size.w + 2.0 * MARGIN)).min(height / (size.h + 2.0 * MARGIN));
            if fit.is_finite() && fit > 0.0 {
                fit.min(1.0)
            } else {
                1.0
            }
        }

        /// Shows the linked part of the graph. Unlinked nodes are left out, as
        /// there can be so many that the rest would become tiny.
        fn fit_into(&self, width: f64, height: f64) {
            let layout = self.layout.borrow();
            let Some(area) = layout.as_ref().map(|l| l.linked) else {
                return;
            };
            let size = area.size();
            let scale = self.fit_scale(size, width, height);
            self.scale.set(scale);
            self.offset.set((
                (width - size.w * scale) / 2.0 - area.min.x * scale,
                (height - size.h * scale) / 2.0 - area.min.y * scale,
            ));
        }

        /// Zooms by `factor`, keeping the point under the pointer in place.
        fn zoom(&self, factor: f64) {
            self.moved.set(true);
            let obj = self.obj();
            // Zooming out stops a little past the point where everything is visible.
            let size = self
                .layout
                .borrow()
                .as_ref()
                .map(|l| l.size)
                .unwrap_or_default();
            let min = self.fit_scale(size, obj.width() as f64, obj.height() as f64) * 0.8;
            let old = self.scale.get();
            let new = (old * factor).clamp(min, MAX_SCALE);
            let (px, py) = self.pointer.get();
            let (ox, oy) = self.offset.get();
            self.scale.set(new);
            self.offset
                .set((px - (px - ox) * new / old, py - (py - oy) * new / old));
            obj.queue_allocate();
        }
    }
}

glib::wrapper! {
    pub struct Canvas(ObjectSubclass<imp::Canvas>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Canvas {
    fn default() -> Self {
        glib::Object::new()
    }
}

/// Points spread evenly along a link's route. Two routes sampled this way
/// can be blended point by point.
fn sample(path: &layout::Path) -> Vec<layout::Point> {
    const POINTS: usize = 48;
    let mut line = vec![path.start];
    let mut from = path.start;
    for c in &path.curves {
        for step in 1..=12 {
            let t = step as f64 / 12.0;
            let u = 1.0 - t;
            let mix = |a: f64, b: f64, c: f64, d: f64| {
                u * u * u * a + 3.0 * u * u * t * b + 3.0 * u * t * t * c + t * t * t * d
            };
            line.push(layout::Point {
                x: mix(from.x, c.c1.x, c.c2.x, c.end.x),
                y: mix(from.y, c.c1.y, c.c2.y, c.end.y),
            });
        }
        from = c.end;
    }
    // Distance along the line at each of its points.
    let mut along = vec![0.0];
    for pair in line.windows(2) {
        along.push(along[along.len() - 1] + (pair[1].x - pair[0].x).hypot(pair[1].y - pair[0].y));
    }
    let total = along[along.len() - 1];
    let mut at = 0;
    (0..POINTS)
        .map(|i| {
            let want = total * i as f64 / (POINTS - 1) as f64;
            while at + 2 < line.len() && along[at + 1] < want {
                at += 1;
            }
            let Some(next) = line.get(at + 1) else {
                return line[at];
            };
            let span = along[at + 1] - along[at];
            let t = if span > 0.0 {
                ((want - along[at]) / span).clamp(0.0, 1.0)
            } else {
                0.0
            };
            layout::Point {
                x: line[at].x + (next.x - line[at].x) * t,
                y: line[at].y + (next.y - line[at].y) * t,
            }
        })
        .collect()
}

/// Id of the group at `path`. Kept well below the ids used for port rows and
/// collapsed groups.
fn group_id(path: &str) -> Id {
    let hash = path.bytes().fold(0x811c_9dc5u32, |h, b| {
        (h ^ b as u32).wrapping_mul(0x0100_0193)
    });
    hash & 0x0fff_ffff
}

/// Ids of the links that stand for a route through hidden nodes start here,
/// far above anything PipeWire hands out.
const THROUGH: Id = 0x4000_0000;

/// Id of the link that stands for the route between two ports. Derived from
/// the ports, so that it stays the same from one update to the next.
fn through_id(out_port: Id, in_port: Id) -> Id {
    let hash = [out_port, in_port].iter().fold(0x811c_9dc5u32, |h, port| {
        (h ^ port).wrapping_mul(0x0100_0193)
    });
    THROUGH | hash & (THROUGH - 1)
}

/// Colours along the last stretch of a link, where it runs on top of other
/// links into the same port: a horizontal gradient from `from` to `to`.
pub struct Fade {
    from: f32,
    to: f32,
    stops: Vec<gsk::ColorStop>,
}

fn mean(colours: &[gdk::RGBA]) -> gdk::RGBA {
    let of = |channel: fn(&gdk::RGBA) -> f32| {
        colours.iter().map(channel).sum::<f32>() / colours.len() as f32
    };
    gdk::RGBA::new(
        of(gdk::RGBA::red),
        of(gdk::RGBA::green),
        of(gdk::RGBA::blue),
        of(gdk::RGBA::alpha),
    )
}

/// For links that end on the same port, works out where each one is covered
/// by others and what colour the pile has there. `None` for a link that is
/// never covered, or whose end does not simply run left to right.
fn fan_fades(fan: &[(&layout::Path, gdk::RGBA)]) -> Vec<Option<Fade>> {
    /// Links closer than this are drawn on top of each other.
    const WIDTH: f64 = 1.7;
    // The end of each link as points with rising x.
    let tails: Vec<Vec<layout::Point>> = fan
        .iter()
        .map(|(path, _)| {
            let mut tail = Vec::new();
            for i in (0..path.curves.len()).rev() {
                let c = path.curves[i];
                let start = if i == 0 {
                    path.start
                } else {
                    path.curves[i - 1].end
                };
                if c.end == start {
                    continue;
                }
                let rising = start.x <= c.c1.x
                    && c.c1.x <= c.c2.x + 1e-6
                    && c.c2.x <= c.end.x
                    && start.x < c.end.x;
                if !rising {
                    break;
                }
                let piece = (0..=24).map(|step| {
                    let t = step as f64 / 24.0;
                    let u = 1.0 - t;
                    let mix = |a: f64, b: f64, c: f64, d: f64| {
                        u * u * u * a + 3.0 * u * u * t * b + 3.0 * u * t * t * c + t * t * t * d
                    };
                    layout::Point {
                        x: mix(start.x, c.c1.x, c.c2.x, c.end.x),
                        y: mix(start.y, c.c1.y, c.c2.y, c.end.y),
                    }
                });
                tail.splice(0..0, piece);
            }
            tail
        })
        .collect();
    let height = |tail: &[layout::Point], x: f64| -> Option<f64> {
        let after = tail.iter().position(|p| p.x >= x)?;
        let (b, a) = (tail[after], tail[after.checked_sub(1)?]);
        let t = if b.x > a.x {
            (x - a.x) / (b.x - a.x)
        } else {
            1.0
        };
        Some(a.y + (b.y - a.y) * t)
    };
    let same = |a: gdk::RGBA, b: gdk::RGBA| {
        (a.red() - b.red()).abs() + (a.green() - b.green()).abs() + (a.blue() - b.blue()).abs()
            < 0.004
    };
    (0..fan.len())
        .map(|i| {
            let (first, last) = (tails[i].first()?.x, tails[i].last()?.x);
            // One sample about every three pixels.
            let samples = (((last - first) / 3.0) as usize).clamp(16, 600);
            let mut stops: Vec<(f32, gdk::RGBA)> = Vec::new();
            let mut covered = false;
            for step in 1..=samples {
                let x = first + (last - first) * step as f64 / samples as f64;
                let y = height(&tails[i], x)?;
                let pile: Vec<gdk::RGBA> = (0..fan.len())
                    .filter(|&j| {
                        height(&tails[j], x).is_some_and(|other| (other - y).abs() < WIDTH)
                    })
                    .map(|j| fan[j].1)
                    .collect();
                covered |= pile.len() > 1;
                let colour = mean(&pile);
                let offset = step as f32 / samples as f32;
                match stops.last() {
                    Some(&(_, before)) if same(before, colour) => {}
                    // The colour changes within one step.
                    Some(&(_, before)) => {
                        stops.push((offset - 1.0 / samples as f32, before));
                        stops.push((offset, colour));
                    }
                    None => stops.push((0.0, colour)),
                }
            }
            let end = stops.last()?.1;
            stops.push((1.0, end));
            covered.then(|| Fade {
                from: first as f32,
                to: last as f32,
                stops: stops
                    .into_iter()
                    .map(|(offset, colour)| gsk::ColorStop::new(offset, colour))
                    .collect(),
            })
        })
        .collect()
}

/// A port as shown on a card: row id, label and, in split view, the palette
/// entry of the row's link.
type PortRow = (Id, String, Option<usize>);

/// A port row and, as tooltip markup, what it is linked to.
type Row = (PortRow, Option<String>);

fn palette(index: usize) -> gdk::RGBA {
    let (r, g, b) = PALETTE[index % PALETTE.len()];
    gdk::RGBA::new(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 0.9)
}

impl Canvas {
    /// Replaces what is shown with `graph`.
    pub fn set_graph(&self, graph: &Graph) {
        let imp = self.imp();
        let old = imp.graph.replace(graph.clone());
        // What is new: nodes, and nodes whose ports or links are not what
        // they were. Nothing is marked when the graph is first shown.
        let ports =
            |g: &Graph| -> HashSet<(Id, Id)> { g.ports.values().map(|p| (p.node, p.id)).collect() };
        let links = |g: &Graph| -> HashSet<(Id, Id, Id, Id)> {
            g.links
                .values()
                .map(|l| (l.out_node, l.out_port, l.in_node, l.in_port))
                .collect()
        };
        let mut changed: HashSet<Id> = graph
            .nodes
            .keys()
            .filter(|id| !old.nodes.contains_key(id))
            .copied()
            .collect();
        changed.extend(ports(graph).symmetric_difference(&ports(&old)).map(|p| p.0));
        changed.extend(
            links(graph)
                .symmetric_difference(&links(&old))
                .flat_map(|l| [l.0, l.2]),
        );
        let same_nodes = graph.nodes.len() == old.nodes.len();
        let mut ghost = imp.ghost.borrow_mut();
        if !(changed.is_empty() && same_nodes) && !old.nodes.is_empty() {
            // What this change removed stays on screen until the next one:
            // nodes that are gone with their ports, and links that are gone.
            let mut gone = Graph::default();
            for (id, node) in old
                .nodes
                .iter()
                .filter(|(id, _)| !graph.nodes.contains_key(id))
            {
                gone.nodes.insert(*id, node.clone());
            }
            for (id, port) in &old.ports {
                if gone.nodes.contains_key(&port.node) {
                    gone.ports.insert(*id, port.clone());
                }
            }
            let now = links(graph);
            for (id, link) in &old.links {
                if !now.contains(&(link.out_node, link.out_port, link.in_node, link.in_port)) {
                    gone.links.insert(*id, link.clone());
                }
            }
            changed.extend(gone.nodes.keys());
            *ghost = gone;
            imp.changed.replace(changed);
            imp.flash.set(true);
        }
        // An update that only touched properties keeps the marks and the
        // ghosts of the last real change. Ids are reused by PipeWire, so
        // anything that has come back to life is no ghost any more.
        ghost.nodes.retain(|id, _| !graph.nodes.contains_key(id));
        let mut view = graph.clone();
        for (id, node) in &ghost.nodes {
            view.nodes.insert(*id, node.clone());
        }
        ghost.ports.retain(|id, port| {
            !graph.ports.contains_key(id) && view.nodes.contains_key(&port.node)
        });
        for (id, port) in &ghost.ports {
            view.ports.insert(*id, port.clone());
        }
        ghost.links.retain(|id, link| {
            !graph.links.contains_key(id)
                && view
                    .ports
                    .get(&link.out_port)
                    .is_some_and(|p| p.node == link.out_node)
                && view
                    .ports
                    .get(&link.in_port)
                    .is_some_and(|p| p.node == link.in_node)
        });
        for (id, link) in &ghost.links {
            view.links.insert(*id, link.clone());
        }
        drop(ghost);
        imp.view.replace(view);
        self.rebuild();
    }

    pub fn set_config(&self, config: Config) {
        self.imp().config.replace(config);
        self.rebuild();
    }

    /// Shows the linked part of the graph.
    pub fn fit(&self) {
        // Back to following the graph and the window.
        self.imp().moved.set(false);
        self.imp().fit.set(true);
        self.queue_allocate();
    }

    /// Switches between one row per port and one row per link. The second
    /// shows at a glance what exactly is connected to a busy port.
    pub fn toggle_split(&self) {
        let split = &self.imp().split;
        split.set(!split.get());
        self.rebuild();
    }

    /// `handler` receives the changes the user asks for.
    pub fn connect_command(&self, handler: impl Fn(Command) + 'static) {
        self.imp().commands.replace(Some(Box::new(handler)));
    }

    /// Highlights what matches `query` and returns how many cards do.
    pub fn set_search(&self, query: Option<Query>) -> usize {
        let imp = self.imp();
        imp.search.replace(query);
        imp.apply_search();
        imp.matches.borrow().len()
    }

    /// Centres on the next (`1`) or previous (`-1`) match.
    pub fn step_match(&self, step: isize) {
        self.imp().step_match(step);
    }

    /// Input from a scripted demo, handled exactly like the real thing.
    pub fn pointer_moved(&self, x: f64, y: f64) {
        self.imp().hover(x, y);
    }

    pub fn button_pressed(&self, button: u32, x: f64, y: f64) {
        self.imp().press(button, false, x, y);
    }

    pub fn dragged(&self, dx: f64, dy: f64) {
        self.imp().drag_update(dx, dy);
    }

    pub fn button_released(&self, dx: f64, dy: f64) {
        self.imp().release(dx, dy);
    }

    /// Paints links this many times finer than the screen needs.
    pub fn set_boost(&self, boost: f32) {
        self.imp().boost.set(boost);
        self.queue_draw();
    }

    /// Where on the canvas the title of a node or a port row is: in the
    /// node list if `listed`, else on the node's card.
    pub fn spot(&self, end: &End, listed: bool) -> Option<(f64, f64)> {
        let imp = self.imp();
        let centre = |widget: &gtk::Widget| {
            let bounds = widget.compute_bounds(self)?;
            Some((
                bounds.x() as f64 + bounds.width() as f64 / 2.0,
                bounds.y() as f64 + bounds.height() as f64 / 2.0,
            ))
        };
        if listed {
            let rows = imp.panel_rows.borrow();
            return rows
                .iter()
                .find(|row| row.1 == *end)
                .and_then(|row| centre(&row.0));
        }
        let rows = imp.rows.borrow();
        for node in imp.nodes.borrow().iter() {
            match end {
                End::Node(id) if node.node == Some(*id) => return centre(node.title.upcast_ref()),
                End::Ports(ports) => {
                    let all = node.inputs.iter().chain(&node.outputs);
                    let found = all
                        .into_iter()
                        .find(|(row, _)| rows.get(row) == ports.first());
                    if let Some((_, label)) = found {
                        return centre(label.upcast_ref());
                    }
                }
                End::Node(_) => {}
            }
        }
        None
    }

    /// Centres the view on a node, at the given zoom if any.
    pub fn show_node(&self, node: Id, zoom: Option<f64>) {
        let imp = self.imp();
        if let Some(zoom) = zoom {
            imp.scale.set(zoom);
        }
        let card = imp
            .nodes
            .borrow()
            .iter()
            .find(|n| n.members.contains(&node))
            .map(|n| n.id);
        if let Some(card) = card {
            imp.centre_on(card);
        }
    }

    /// Moves the pointer a scripted demo draws; `None` takes it away.
    pub fn set_fake_pointer(&self, at: Option<(f64, f64)>) {
        self.imp().fake_pointer.set(at);
        self.queue_draw();
    }

    /// Types into the filter of the node list.
    pub fn set_list_filter(&self, text: &str) {
        self.imp().panel_entry.set_text(text);
    }

    /// Opens or closes the list of nodes.
    pub fn toggle_list(&self) {
        let imp = self.imp();
        imp.show_panel(!imp.panel.is_visible());
    }

    /// `handler` is told when a text field on the canvas takes or gives up
    /// the keyboard, during which plain keys are text.
    pub fn connect_typing(&self, handler: impl Fn(bool) + 'static) {
        self.imp().typing.replace(Some(Box::new(handler)));
    }

    /// Moves the view by a number of pixels.
    pub fn pan(&self, dx: f64, dy: f64) {
        self.imp().moved.set(true);
        let offset = &self.imp().offset;
        let (x, y) = offset.get();
        offset.set((x + dx, y + dy));
        self.queue_allocate();
    }

    /// Returns to the view before the last jump to the other end of a link.
    pub fn back(&self) {
        self.imp().back();
    }

    /// Removes the selected link.
    pub fn delete_selected(&self) {
        let imp = self.imp();
        let selected = imp.selected_link.take();
        if let Some(link) = selected.filter(|id| imp.graph.borrow().links.contains_key(id)) {
            imp.send(Command::Unlink(link));
            self.queue_draw();
        }
    }

    /// Lays the graph out again from scratch.
    pub fn relayout(&self) {
        self.rebuild();
    }

    /// Collapses every group, or expands them all if any is collapsed.
    pub fn toggle_groups(&self) {
        let imp = self.imp();
        {
            let mut collapsed = imp.collapsed.borrow_mut();
            if collapsed.is_empty() {
                // Outermost groups only: they take everything inside with them.
                let grouping =
                    groups::resolve(&imp.view.borrow(), &imp.config.borrow(), imp.reveal.get());
                collapsed.extend(
                    grouping
                        .paths
                        .values()
                        .filter_map(|path| path.first().cloned()),
                );
            } else {
                collapsed.clear();
            }
        }
        self.rebuild();
    }

    /// Shows the nodes that the rules hide, or hides them again.
    pub fn toggle_hidden(&self) {
        let reveal = &self.imp().reveal;
        reveal.set(!reveal.get());
        self.rebuild();
    }

    fn toggle_collapsed(&self, path: &str) {
        {
            let mut collapsed = self.imp().collapsed.borrow_mut();
            if !collapsed.remove(path) {
                collapsed.insert(path.to_owned());
            }
        }
        self.rebuild();
    }

    /// Makes `title` collapse or expand the group at `path` when clicked.
    fn make_toggle(&self, title: &gtk::Label, path: &str) {
        title.set_cursor_from_name(Some("pointer"));
        let click = gtk::GestureClick::new();
        let path = path.to_owned();
        click.connect_released(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |_, _, _, _| canvas.toggle_collapsed(&path)
        ));
        title.add_controller(click);
    }

    /// Builds the widgets for the current graph, configuration and set of
    /// collapsed groups.
    fn rebuild(&self) {
        let imp = self.imp();
        imp.clear();
        let graph = imp.view.borrow();
        let ghost = imp.ghost.borrow();
        let grouping = groups::resolve(&graph, &imp.config.borrow(), imp.reveal.get());
        let hidden = match imp.reveal.get() {
            true => HashSet::new(),
            false => grouping.hidden.clone(),
        };
        imp.hidden.replace(hidden.clone());
        let collapsed = imp.collapsed.borrow();

        // Every prefix of a group path is a group. A collapsed group swallows
        // everything below it.
        let mut ids: HashMap<String, Id> = HashMap::new();
        let mut names: HashMap<Id, String> = HashMap::new();
        let mut group_views: Vec<imp::GroupView> = Vec::new();
        // Collapsed group id to (path, the names along it, member nodes).
        let mut folded: Vec<(Id, Option<Id>, String, Vec<String>, Vec<Id>)> = Vec::new();
        // Where every visible node ended up: its group, or the collapsed
        // group that stands in for it.
        let mut home: HashMap<Id, (Option<Id>, Option<usize>)> = HashMap::new();
        for node in graph.nodes.values().filter(|n| !hidden.contains(&n.id)) {
            let mut parent = None;
            let mut fold = None;
            let mut path = String::new();
            let groups = grouping.paths.get(&node.id).map_or(&[][..], Vec::as_slice);
            for (depth, name) in groups.iter().enumerate() {
                if !path.is_empty() {
                    path.push('/');
                }
                path.push_str(name);
                let known = ids.get(&path).copied();
                // Derived from the path, so that a group keeps its id (and its
                // place on screen) when other groups come and go.
                let id = known.unwrap_or_else(|| group_id(&path));
                ids.insert(path.clone(), id);
                // A group inside another need not repeat the other's name.
                let name = &groups::short(name, &groups[..depth]);
                names.insert(id, name.clone());
                if collapsed.contains(&path) {
                    let at = match folded.iter().position(|f| f.0 == id) {
                        Some(at) => at,
                        None => {
                            let along = groups[..=depth].to_vec();
                            folded.push((id, parent, path.clone(), along, Vec::new()));
                            folded.len() - 1
                        }
                    };
                    folded[at].4.push(node.id);
                    fold = Some(at);
                    break;
                }
                if known.is_none() {
                    let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
                    frame.add_css_class("tw-group");
                    let title = gtk::Label::new(Some(&format!("▾ {name}")));
                    title.add_css_class("tw-toggle");
                    title.set_xalign(0.0);
                    title.set_halign(gtk::Align::Start);
                    title.set_tooltip_text(Some("Click to collapse"));
                    self.make_toggle(&title, &path);
                    frame.append(&title);
                    group_views.push(imp::GroupView {
                        id,
                        parent,
                        widget: frame.upcast(),
                        title,
                    });
                }
                parent = Some(id);
            }
            home.insert(node.id, (parent, fold));
        }
        // Parents were created before their children.
        for group in &group_views {
            group.widget.set_parent(self);
        }

        // Links inside a collapsed group have no visible end and are left out.
        let visible = |l: &&crate::graph::Link| match (home.get(&l.out_node), home.get(&l.in_node))
        {
            (Some(a), Some(b)) => a.1.is_none() || a.1 != b.1,
            _ => false,
        };
        // Where hidden nodes sit between two shown ones, a link of its own
        // kind stands for the way through them, so that hiding leaves no hole.
        let mut via: HashMap<Id, String> = HashMap::new();
        let mut bridges: Vec<crate::graph::Link> = Vec::new();
        for route in groups::through(&graph, &hidden) {
            let mut id = through_id(route.out_port, route.in_port);
            while via.contains_key(&id) {
                id = THROUGH | (id + 1) & (THROUGH - 1);
            }
            let names: Vec<String> = route.via.iter().map(|&n| graph.label(n)).collect();
            via.insert(id, names.join(", "));
            bridges.push(crate::graph::Link {
                id,
                out_node: route.out_node,
                out_port: route.out_port,
                in_node: route.in_node,
                in_port: route.in_port,
            });
        }
        let shown: Vec<_> = (graph.links.values().chain(&bridges))
            .filter(visible)
            .collect();
        imp.through.replace(
            shown
                .iter()
                .map(|l| l.id)
                .filter(|id| via.contains_key(id))
                .collect(),
        );

        // Hashing the name keeps a node's colour the same from run to run.
        let colour = |node: Id| {
            let name = graph
                .nodes
                .get(&node)
                .map_or("", |n| n.prop("node.name").unwrap_or(""));
            name.bytes()
                .fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize))
                % PALETTE.len()
        };

        // In split view every link gets a row of its own. Rows are ordered by
        // where the card at the other end was in the previous layout, so the
        // links of one neighbour stay together.
        let before: HashMap<Id, f64> = match imp.layout.borrow().as_ref() {
            Some(layout) => layout.nodes.iter().map(|(&id, p)| (id, p.y)).collect(),
            None => HashMap::new(),
        };
        let card_of = |node: Id| match home.get(&node) {
            Some(&(_, Some(at))) => Id::MAX - folded[at].0,
            _ => node,
        };
        let split = imp.split.get();
        // Row that stands for one end of a link: (link, is its output end).
        let mut ends: HashMap<(Id, bool), Id> = HashMap::new();
        let row_ports = RefCell::new(HashMap::new());
        let row_link: RefCell<HashMap<Id, Id>> = RefCell::new(HashMap::new());
        // Rows of a collapsed group that stand for several ports at once,
        // and the row each link to one of those ports ends on.
        let row_members: RefCell<HashMap<Id, Vec<Id>>> = RefCell::new(HashMap::new());
        let merged_ends: RefCell<HashMap<(Id, bool), Id>> = RefCell::new(HashMap::new());
        let merged_next = Cell::new(Id::MAX / 4);
        let mut next_row = Id::MAX / 2;
        let mut split_rows = |rows: Vec<PortRow>, output: bool| -> Vec<PortRow> {
            if !split {
                return rows;
            }
            let mut linked = Vec::new();
            let mut free = Vec::new();
            for (index, (port, label, _)) in rows.into_iter().enumerate() {
                let here =
                    |l: &&&crate::graph::Link| port == if output { l.out_port } else { l.in_port };
                let links: Vec<_> = shown.iter().filter(here).collect();
                if links.is_empty() {
                    free.push((port, label, None));
                    continue;
                }
                for link in links {
                    let peer = card_of(if output { link.in_node } else { link.out_node });
                    let height = before.get(&peer).copied().unwrap_or(f64::MAX);
                    next_row += 1;
                    ends.insert((link.id, output), next_row);
                    row_ports.borrow_mut().insert(next_row, port);
                    row_link.borrow_mut().insert(next_row, link.id);
                    let colour = Some(colour(link.out_node));
                    linked.push(((height, peer, index), (next_row, label.clone(), colour)));
                }
            }
            linked.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            linked.into_iter().map(|row| row.1).chain(free).collect()
        };

        let rows = |node: Id, direction: Direction| -> Vec<PortRow> {
            let ports = graph.ports_of(node, direction);
            ports
                .into_iter()
                .map(|p| (p.id, p.name.clone(), None))
                .collect()
        };
        // Hovering over a port lists what it is linked to, each line in the
        // colour of its link, so that one need not follow the lines.
        let far_end = |link: &crate::graph::Link, output: bool| {
            let (node, port) = if output {
                (link.in_node, link.in_port)
            } else {
                (link.out_node, link.out_port)
            };
            let node = graph.label(node);
            let port = graph.ports.get(&port).map_or("?", |p| p.name.as_str());
            let (r, g, b) = PALETTE[colour(link.out_node)];
            let mut text = if output {
                format!("{node}  {port} ›")
            } else {
                format!("‹ {node}  {port}")
            };
            if let Some(hidden) = via.get(&link.id) {
                text.push_str(&format!("  (through {hidden})"));
            }
            format!(
                "<span foreground=\"#{r:02x}{g:02x}{b:02x}\">{}</span>",
                glib::markup_escape_text(&text)
            )
        };
        let with_peers = |rows: Vec<PortRow>, output: bool| -> Vec<Row> {
            let row_link = row_link.borrow();
            let peers = |row: Id| -> Option<String> {
                // A split row stands for one link, a merged row for several
                // ports, any other row for its port.
                let merged = row_members.borrow();
                let here = |l: &&&crate::graph::Link| {
                    let port = if output { l.out_port } else { l.in_port };
                    match (row_link.get(&row), merged.get(&row)) {
                        (Some(id), _) => l.id == *id,
                        (None, Some(ports)) => ports.contains(&port),
                        (None, None) => row == port,
                    }
                };
                let lines: Vec<String> = shown
                    .iter()
                    .filter(here)
                    .map(|l| far_end(l, output))
                    .collect();
                (!lines.is_empty()).then(|| lines.join("\n"))
            };
            rows.into_iter()
                .map(|row| (peers(row.0), row))
                .map(|(peers, row)| (row, peers))
                .collect()
        };
        let mut nodes = Vec::new();
        for node in graph.nodes.values() {
            let Some(&(group, None)) = home.get(&node.id) else {
                continue;
            };
            let gone = ghost.nodes.contains_key(&node.id);
            // Only there because hidden nodes are being shown.
            let revealed = grouping.hidden.contains(&node.id);
            // The frames around the card say part of its name already.
            let full = graph.label(node.id);
            let within = grouping.paths.get(&node.id).map_or(&[][..], Vec::as_slice);
            let name = groups::short(&full, within);
            let name = match (gone, revealed) {
                (true, _) => format!("{name} (gone)"),
                (false, true) => format!("{name} (hidden)"),
                (false, false) => name,
            };
            let (view, title) = card(
                node.id,
                (!gone).then_some(node.id),
                vec![node.id],
                group,
                &name,
                None,
                with_peers(split_rows(rows(node.id, Direction::In), false), false),
                with_peers(split_rows(rows(node.id, Direction::Out), true), true),
            );
            if name != full {
                title.set_tooltip_text(Some(&full));
            }
            if revealed {
                view.widget.add_css_class("tw-hidden");
            }
            if gone {
                view.widget.add_css_class("tw-ghost");
                // Nothing on it can be used any more.
                for (_, label) in view.inputs.iter().chain(&view.outputs) {
                    label.set_cursor(None::<&gdk::Cursor>);
                }
            }
            nodes.push(view);
        }
        // A collapsed group shows only the ports with links that leave it.
        for (at, (id, parent, path, along, members)) in folded.iter().enumerate() {
            let inside = |node: Id| home.get(&node).is_some_and(|h| h.1 == Some(at));
            let mut inputs: Vec<PortRow> = Vec::new();
            let mut outputs: Vec<PortRow> = Vec::new();
            for &member in members {
                // The plain name, not the one that tells twins apart: rows
                // that then read the same are merged below. Without what
                // the card's title says, which may be all of it.
                let name = graph.nodes[&member].name();
                let name = match along.iter().any(|group| group == name) {
                    true => String::new(),
                    false => groups::short(name, along),
                };
                for (direction, list) in
                    [(Direction::In, &mut inputs), (Direction::Out, &mut outputs)]
                {
                    for port in graph.ports_of(member, direction) {
                        let leaves = graph.links.values().chain(&bridges).any(|l| {
                            (l.out_port == port.id && !inside(l.in_node))
                                || (l.in_port == port.id && !inside(l.out_node))
                        });
                        if leaves {
                            let label = match name.is_empty() {
                                true => port.name.clone(),
                                false => format!("{name}: {}", port.name),
                            };
                            list.push((port.id, label, None));
                        }
                    }
                }
            }
            // Inputs follow the order of what feeds them, so that links
            // between two collapsed groups run side by side, not crossed.
            // The rows of one node stay together where one source feeds
            // several nodes of the group.
            let source = |row: &PortRow| {
                let member = graph.ports.get(&row.0).map_or(Id::MAX, |p| p.node);
                let feeds = shown
                    .iter()
                    .filter(|l| l.in_port == row.0 && !inside(l.out_node));
                let keys = feeds.map(|l| {
                    let height = before
                        .get(&card_of(l.out_node))
                        .copied()
                        .unwrap_or(f64::MAX);
                    let index = graph.ports.get(&l.out_port).map_or(0, |p| p.index);
                    (height, l.out_node, member, index)
                });
                keys.min_by(|a, b| a.partial_cmp(b).unwrap()).unwrap_or((
                    f64::MAX,
                    Id::MAX,
                    member,
                    0,
                ))
            };
            inputs.sort_by(|a, b| source(a).partial_cmp(&source(b)).unwrap());
            // The point of collapsing is to see less: rows that read the same,
            // such as the left input of each of four streams of one
            // application, become a single row that all their links end on.
            // Split view still shows every link by itself.
            let merge = |rows: Vec<PortRow>, output: bool| -> Vec<PortRow> {
                if split {
                    return rows;
                }
                let mut merged: Vec<(PortRow, Vec<Id>)> = Vec::new();
                for row in rows {
                    match merged.iter_mut().find(|m| m.0.1 == row.1) {
                        Some(same) => same.1.push(row.0),
                        None => merged.push((row.clone(), vec![row.0])),
                    }
                }
                let rows = merged.into_iter().map(|(mut row, ports)| {
                    if ports.len() > 1 {
                        merged_next.set(merged_next.get() + 1);
                        row.0 = merged_next.get();
                        for link in &shown {
                            let port = if output { link.out_port } else { link.in_port };
                            if ports.contains(&port) {
                                merged_ends.borrow_mut().insert((link.id, output), row.0);
                            }
                        }
                        row_members.borrow_mut().insert(row.0, ports);
                    }
                    row
                });
                rows.collect()
            };
            let (inputs, outputs) = (merge(inputs, false), merge(outputs, true));
            // Not taken from the path: a name may itself contain a slash.
            let name = &names[id];
            let title = format!("▸ {name}");
            let inputs = with_peers(split_rows(inputs, false), false);
            let outputs = with_peers(split_rows(outputs, true), true);
            let (view, label) = card(
                Id::MAX - id,
                None,
                members.clone(),
                *parent,
                &title,
                Some(members.len()),
                inputs,
                outputs,
            );
            view.widget.add_css_class("tw-collapsed");
            // A row that stands for several ports cannot be linked from.
            for (row, label) in view.inputs.iter().chain(&view.outputs) {
                if row_members.borrow().contains_key(row) {
                    label.set_cursor(None::<&gdk::Cursor>);
                    label.add_css_class("tw-merged");
                }
            }
            label.add_css_class("tw-toggle");
            label.set_tooltip_text(Some("Click to expand"));
            self.make_toggle(&label, path);
            nodes.push(view);
        }
        let mut row_ports = row_ports.into_inner();
        let changed = imp.changed.borrow();
        let flash = imp.flash.replace(false);
        for node in &nodes {
            if node.members.iter().any(|id| changed.contains(id)) {
                node.badge.set_visible(true);
            }
            node.widget.set_parent(self);
            // Outside split view a row is simply its port.
            for (row, _) in node.inputs.iter().chain(&node.outputs) {
                row_ports.entry(*row).or_insert(*row);
            }
        }
        if flash {
            imp.pulse_until.set(glib::monotonic_time() + PULSE);
            imp.ensure_tick();
        }
        imp.rows.replace(row_ports);
        let tips = nodes
            .iter()
            .flat_map(|n| n.tips.borrow_mut().drain(..).collect::<Vec<_>>());
        imp.row_tips.replace(tips.collect());
        imp.row_links
            .replace(ends.iter().map(|(&(link, _), &row)| (row, link)).collect());
        imp.selection
            .borrow_mut()
            .retain(|port| graph.ports.contains_key(port));

        imp.colours
            .replace(shown.iter().map(|l| (l.id, colour(l.out_node))).collect());
        imp.links.replace(
            shown
                .iter()
                .map(|l| layout::Link {
                    id: l.id,
                    from: (ends.get(&(l.id, true)).copied())
                        .or_else(|| merged_ends.borrow().get(&(l.id, true)).copied())
                        .unwrap_or(l.out_port),
                    to: (ends.get(&(l.id, false)).copied())
                        .or_else(|| merged_ends.borrow().get(&(l.id, false)).copied())
                        .unwrap_or(l.in_port),
                })
                .collect(),
        );
        if imp
            .selected_link
            .get()
            .is_some_and(|id| !shown.iter().any(|l| l.id == id))
        {
            imp.selected_link.set(None);
        }
        imp.nodes.replace(nodes);
        imp.groups.replace(group_views);
        imp.layout.replace(None);
        drop(graph);
        imp.mark_selection();
        imp.apply_search();
        imp.fill_panel();
        // What was under the pointer may be something else now.
        if imp.tip.is_visible() {
            let (x, y) = imp.pointer.get();
            imp.hover(x, y);
        }
        self.queue_allocate();
    }
}

/// Builds the card for a node. Also returns its title label.
fn card(
    id: Id,
    node: Option<Id>,
    members: Vec<Id>,
    group: Option<Id>,
    name: &str,
    count: Option<usize>,
    inputs: Vec<Row>,
    outputs: Vec<Row>,
) -> (imp::NodeView, gtk::Label) {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 2);
    card.add_css_class("card");
    card.add_css_class("tw-node");
    let title = gtk::Label::new(Some(name));
    title.set_xalign(0.0);
    title.set_halign(gtk::Align::Start);
    title.add_css_class("heading");
    if node.is_some() {
        // Dragging the title to another node links all ports.
        title.add_css_class("tw-title");
        // A crosshair, like the ports: its hot spot is in the middle,
        // which is where one expects the link to start and end.
        title.set_cursor_from_name(Some("crosshair"));
    }
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    head.append(&title);
    // A collapsed group says how many nodes it holds.
    if let Some(count) = count {
        let pill = gtk::Label::new(Some(&count.to_string()));
        pill.add_css_class("tw-pill");
        pill.set_valign(gtk::Align::Center);
        pill.set_tooltip_text(Some("Nodes in this group"));
        head.append(&pill);
    }
    let badge = gtk::Label::new(Some("●"));
    badge.add_css_class("tw-badge");
    badge.set_hexpand(true);
    badge.set_halign(gtk::Align::End);
    badge.set_tooltip_text(Some("Changed in the last update"));
    badge.set_visible(false);
    head.append(&badge);
    card.append(&head);

    let grid = gtk::Grid::new();
    grid.set_column_spacing(18);
    card.append(&grid);
    let tips = RefCell::new(Vec::new());
    let ports = |rows: Vec<Row>, column, xalign| {
        let mut list = Vec::new();
        for (row, ((id, name, colour), peers)) in rows.into_iter().enumerate() {
            if let Some(peers) = peers {
                tips.borrow_mut().push((id, peers));
            }
            let label = gtk::Label::new(Some(&name));
            if let Some(colour) = colour.map(palette) {
                let channel = |v: f32| (v * 65535.0) as u16;
                let attrs = gtk::pango::AttrList::new();
                attrs.insert(gtk::pango::AttrColor::new_foreground(
                    channel(colour.red()),
                    channel(colour.green()),
                    channel(colour.blue()),
                ));
                label.set_attributes(Some(&attrs));
            }
            label.set_xalign(xalign);
            label.set_hexpand(true);
            label.add_css_class("tw-port");
            label.set_cursor_from_name(Some("crosshair"));
            grid.attach(&label, column, row as i32, 1, 1);
            list.push((id, label));
        }
        list
    };
    let inputs = ports(inputs, 0, 0.0);
    let outputs = ports(outputs, 1, 1.0);
    let view = imp::NodeView {
        id,
        group,
        node,
        title: title.clone(),
        badge,
        tips: RefCell::new(tips.into_inner()),
        members,
        widget: card.upcast(),
        inputs,
        outputs,
        size: Cell::default(),
    };
    (view, title)
}

pub fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(
        ".tw-node { padding: 6px 10px; }
         .tw-port { font-size: 0.8em; padding: 0 3px; border-radius: 4px; }
         .tw-port.tw-selected { background: alpha(@accent_bg_color, 0.55); }
         .tw-title { padding: 0 3px; margin-left: -3px; border-radius: 4px; }
         .tw-port:hover, .tw-title:hover { background: alpha(currentColor, 0.13); }
         .tw-port.tw-source, .tw-title.tw-source { background: alpha(@accent_bg_color, 0.4); }
         .tw-port.tw-target, .tw-title.tw-target { background: @accent_bg_color; color: @accent_fg_color; }
         .tw-dim { opacity: 0.3; }
         .tw-match { outline: 3px solid var(--accent-color); outline-offset: 2px; }
         .tw-match.tw-current { outline-width: 6px; }
         .tw-jump { background: @accent_bg_color; color: @accent_fg_color; }
         .tw-search, .tw-count {
           background-color: mix(@window_bg_color, @window_fg_color, 0.12);
           box-shadow: 0 2px 8px alpha(black, 0.35);
         }
         .tw-search { min-height: 36px; }
         /* Without a line height the text is clipped to its own ink, and the
            tops of tall letters stay behind when the text changes. */
         .tw-search text, .tw-search placeholder { line-height: 1.4; }
         .tw-count { padding: 3px 10px; border-radius: 10px; font-size: 0.9em; }
         .tw-status { padding: 3px 10px; border-radius: 8px; font-size: 0.85em; }
         .tw-match-port { background: alpha(@accent_bg_color, 0.5); }
         .tw-panel {
           padding: 10px;
           border-radius: 14px;
           background-color: mix(@window_bg_color, @window_fg_color, 0.07);
           box-shadow: 0 2px 14px alpha(black, 0.45);
         }
         .tw-panel .tw-search { box-shadow: none; }
         .tw-list-node { font-weight: bold; margin: 7px 0 1px 0; }
         .tw-list-port { margin-left: 14px; }
         .tw-help { padding: 18px 22px; }
         .tw-key { font-weight: bold; opacity: 0.85; }
         .tw-caption { padding: 8px 18px; border-radius: 12px; font-size: 1.25em; font-weight: bold; }
         .tw-tip { padding: 4px 10px; border-radius: 8px; font-size: 0.9em; }
         /* A collapsed group looks like a stack of cards. */
         .tw-collapsed {
           border: 1px solid alpha(currentColor, 0.3);
           box-shadow: 5px 5px 0 0 alpha(currentColor, 0.16), 10px 10px 0 0 alpha(currentColor, 0.08);
         }
         .tw-toggle { padding: 0 4px; margin-left: -4px; border-radius: 4px; }
         .tw-toggle:hover { background: alpha(currentColor, 0.13); }
         .tw-pill {
           padding: 0 7px;
           border-radius: 9px;
           font-size: 0.8em;
           background: alpha(currentColor, 0.15);
         }
         .tw-badge { color: #ffa348; font-size: 0.8em; }
         /* A node that the last change removed. */
         .tw-ghost { opacity: 0.5; border: 1px dashed alpha(currentColor, 0.6); }
         .tw-ghost .tw-port:hover { background: none; }
         /* A node that the rules hide, shown on request. */
         .tw-hidden { border: 1px dashed alpha(currentColor, 0.6); background-color: alpha(currentColor, 0.05); }
         .tw-hidden .heading { font-style: italic; opacity: 0.75; }
         .tw-port.tw-merged:hover { background: none; }
         .tw-group {
           border: 1px solid alpha(currentColor, 0.22);
           border-radius: 14px;
           background: alpha(currentColor, 0.035);
         }
         .tw-group > label { margin: 6px 12px 0 12px; font-weight: bold; opacity: 0.75; }
         .tw-group > label.tw-toggle { margin-left: 8px; }",
    );
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
