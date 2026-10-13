//! Plays a script on a made-up graph: moves a drawn pointer, drags links,
//! presses keys and lets nodes come and go. Used to record demo videos that
//! can be made again whenever the application changes.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use serde::Deserialize;

use crate::canvas::Canvas;
use crate::mock::Mock;

#[derive(Deserialize)]
pub struct Script {
    #[serde(rename = "step")]
    steps: Vec<Step>,
}

/// One thing to do; exactly one field is set. Nodes and ports are named as in
/// the scenario (`node` or `node:port`); `list:` in front means the line in
/// the node list rather than the card.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Step {
    /// Seconds to do nothing.
    wait: Option<f64>,
    /// Text shown at the bottom until the next caption; empty to remove it.
    caption: Option<String>,
    /// Moves the pointer there.
    hover: Option<String>,
    /// Drags from the first to the second.
    drag: Option<[String; 2]>,
    /// Right click: jump to the other end of a port's link.
    jump: Option<String>,
    /// `fit`, `split`, `list`, `groups`, `hidden`, `back`, `relayout`, `delete`
    /// or `help`.
    key: Option<String>,
    /// Opens the search and types this.
    search: Option<String>,
    /// Enter in the search: go to the next match.
    enter: Option<bool>,
    /// Escape in the search: close it.
    escape: Option<bool>,
    /// Types this into the filter of the node list.
    filter: Option<String>,
    /// Makes a node of the scenario marked `later` appear.
    appear: Option<String>,
    /// Makes a node go away.
    vanish: Option<String>,
    /// Centres the view on a node.
    show: Option<String>,
    /// With `show`: the zoom to use, 1 being full size.
    zoom: Option<f64>,
}

impl Script {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// What a script acts on.
pub struct Stage {
    pub canvas: Canvas,
    pub mock: Rc<RefCell<Mock>>,
    pub window: adw::ApplicationWindow,
    pub entry: gtk::SearchEntry,
    pub caption: gtk::Label,
    /// Where the drawn pointer is, and where its button went down.
    pub at: Cell<(f64, f64)>,
    pub held: Cell<Option<(f64, f64)>>,
}

async fn pause(seconds: f64) {
    glib::timeout_future(Duration::from_secs_f64(seconds)).await;
}

impl Stage {
    /// Where on the canvas `name` is.
    fn spot(&self, name: &str) -> Option<(f64, f64)> {
        let (listed, name) = match name.strip_prefix("list:") {
            Some(name) => (true, name),
            None => (false, name),
        };
        let end = self.mock.borrow().resolve(name);
        let spot = end.and_then(|end| self.canvas.spot(&end, listed));
        if spot.is_none() {
            eprintln!("tanglewire: demo: {name:?} is not on the canvas");
        }
        spot
    }

    fn point(&self, to: (f64, f64)) {
        self.at.set(to);
        self.canvas.set_fake_pointer(Some(to));
        match self.held.get() {
            Some(from) => self.canvas.dragged(to.0 - from.0, to.1 - from.1),
            None => self.canvas.pointer_moved(to.0, to.1),
        }
    }

    /// Moves the pointer the way a hand would: off gently, and slowing down
    /// before it arrives.
    async fn glide(&self, to: (f64, f64)) {
        let from = self.at.get();
        let distance = (to.0 - from.0).hypot(to.1 - from.1);
        let frames = ((distance / 900.0).clamp(0.35, 1.1) * 60.0) as usize;
        for frame in 1..=frames {
            let t = frame as f64 / frames as f64;
            let eased = t * t * (3.0 - 2.0 * t);
            self.point((
                from.0 + (to.0 - from.0) * eased,
                from.1 + (to.1 - from.1) * eased,
            ));
            pause(1.0 / 60.0).await;
        }
    }

    fn press(&self, button: u32) {
        let (x, y) = self.at.get();
        self.held.set(Some((x, y)));
        self.canvas.button_pressed(button, x, y);
    }

    fn release(&self) {
        let (x, y) = self.at.get();
        if let Some(from) = self.held.take() {
            self.canvas.button_released(x - from.0, y - from.1);
        }
    }

    async fn type_into(&self, text: &str, put: impl Fn(&str)) {
        let mut typed = String::new();
        for letter in text.chars() {
            typed.push(letter);
            put(&typed);
            pause(0.09).await;
        }
    }

    fn close_search(&self) {
        self.entry.set_text("");
        if let Some(search_box) = self.entry.parent() {
            search_box.set_visible(false);
        }
        gtk::prelude::GtkWindowExt::set_focus(&self.window, gtk::Widget::NONE);
    }

    async fn perform(&self, step: &Step) {
        if let Some(seconds) = step.wait {
            pause(seconds).await;
        } else if let Some(text) = &step.caption {
            self.caption.set_label(text);
            self.caption.set_visible(!text.is_empty());
        } else if let Some(name) = &step.hover {
            if let Some(spot) = self.spot(name) {
                self.glide(spot).await;
            }
        } else if let Some([from, to]) = &step.drag {
            let Some(start) = self.spot(from) else { return };
            self.glide(start).await;
            pause(0.3).await;
            self.press(gtk::gdk::BUTTON_PRIMARY);
            let Some(end) = self.spot(to) else {
                self.release();
                return;
            };
            self.glide(end).await;
            // Time to read what the drop will do.
            pause(0.9).await;
            self.release();
        } else if let Some(name) = &step.jump {
            let Some(spot) = self.spot(name) else { return };
            self.glide(spot).await;
            pause(0.4).await;
            self.press(gtk::gdk::BUTTON_SECONDARY);
            self.release();
        } else if let Some(key) = &step.key {
            match key.as_str() {
                "fit" => self.canvas.fit(),
                "split" => self.canvas.toggle_split(),
                "list" => self.canvas.toggle_list(),
                "groups" => self.canvas.toggle_groups(),
                "hidden" => self.canvas.toggle_hidden(),
                "back" => self.canvas.back(),
                "relayout" => self.canvas.relayout(),
                "delete" => self.canvas.delete_selected(),
                "help" => {
                    let _ = WidgetExt::activate_action(&self.window, "win.help", None);
                }
                other => eprintln!("tanglewire: demo: no key called {other:?}"),
            }
        } else if let Some(text) = &step.search {
            let _ = WidgetExt::activate_action(&self.window, "win.search", None);
            self.type_into(text, |typed| self.entry.set_text(typed))
                .await;
        } else if step.enter == Some(true) {
            self.canvas.step_match(1);
        } else if step.escape == Some(true) {
            self.close_search();
        } else if let Some(text) = &step.filter {
            self.type_into(text, |typed| self.canvas.set_list_filter(typed))
                .await;
        } else if let Some(key) = &step.appear {
            self.mock.borrow_mut().appear(key);
        } else if let Some(key) = &step.vanish {
            self.mock.borrow_mut().vanish(key);
        } else if let Some(name) = &step.show {
            if let Some(crate::connect::End::Node(id)) = self.mock.borrow().resolve(name) {
                self.canvas.show_node(id, step.zoom);
            }
        }
    }
}

/// Plays the script, then closes the window.
pub async fn play(script: Script, stage: Stage) {
    // The pointer starts in the middle.
    stage.at.set((
        stage.canvas.width() as f64 / 2.0,
        stage.canvas.height() as f64 / 2.0,
    ));
    for step in &script.steps {
        stage.perform(step).await;
    }
    pause(0.5).await;
    stage.window.close();
}
