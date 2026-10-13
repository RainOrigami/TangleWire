use std::sync::mpsc;

use adw::prelude::*;
use gtk::glib;

mod canvas;
mod connect;
mod demo;
mod graph;
mod groups;
mod mock;
mod pw;
mod search;

use graph::{Direction, Event, Graph};

const APP_ID: &str = "net.codefreak.TangleWire";
const DEMO_ID: &str = "net.codefreak.TangleWire.Demo";
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> gtk::glib::ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("--dump") => return dump(false),
        Some("--monitor") => return dump(true),
        // Test hooks: `--link <output port> <input port>` and `--unlink` with
        // the same arguments make or remove one link and report the outcome.
        // `--find <query>` lists the nodes the search would highlight.
        Some("--find") => return find(),
        Some("--link") => return link(true),
        Some("--unlink") => return link(false),
        Some("--version") => {
            println!("tanglewire {VERSION}");
            return glib::ExitCode::SUCCESS;
        }
        _ => {}
    }
    let options = match Options::parse() {
        Ok(options) => std::rc::Rc::new(options),
        Err(message) => {
            eprintln!("tanglewire: {message}");
            return glib::ExitCode::FAILURE;
        }
    };
    // A demo runs beside the real thing, as an application of its own, and
    // so does a run that only takes a picture.
    let id = if options.mock.is_some() || options.screenshot.is_some() {
        DEMO_ID
    } else {
        APP_ID
    };
    let app = adw::Application::builder().application_id(id).build();
    app.connect_activate(move |app| build_window(app, &options));
    app.run_with_args::<&str>(&[])
}

/// How the window is started.
#[derive(Default)]
struct Options {
    /// `--mock <scenario.toml>`: show a made-up graph instead of PipeWire's.
    mock: Option<std::path::PathBuf>,
    /// `--screenshot <file.png>`: save a picture of the window and quit.
    screenshot: Option<std::path::PathBuf>,
    /// `--demo <script.toml>`: with `--mock`, play a script and quit.
    demo: Option<std::path::PathBuf>,
    /// `--record <file.mp4>`: film the window while it is open. Needs ffmpeg.
    record: Option<std::path::PathBuf>,
    /// `--split`: start in split view.
    split: bool,
    /// `--size <width>x<height>`: the size the window asks for.
    size: Option<(i32, i32)>,
    /// `--scale <n>`: the picture has n times the pixels of the window each
    /// way, so that small text stays readable. 3 if left out.
    scale: Option<f32>,
}

impl Options {
    fn parse() -> Result<Self, String> {
        let mut options = Self::default();
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            let mut value = || args.next().ok_or(format!("{arg} needs a value"));
            match arg.as_str() {
                "--mock" => options.mock = Some(value()?.into()),
                "--screenshot" => options.screenshot = Some(value()?.into()),
                "--demo" => options.demo = Some(value()?.into()),
                "--record" => options.record = Some(value()?.into()),
                "--split" => options.split = true,
                "--scale" => {
                    let text = value()?;
                    options.scale = Some(
                        text.parse()
                            .map_err(|_| format!("{text:?} is not a number"))?,
                    );
                }
                "--size" => {
                    let text = value()?;
                    let size = text
                        .split_once('x')
                        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)));
                    options.size =
                        Some(size.ok_or(format!("{text:?} is not a size like 1600x900"))?);
                }
                _ => return Err(format!("unknown option {arg}")),
            }
        }
        if options.demo.is_some() && options.mock.is_none() {
            return Err("--demo needs --mock".to_owned());
        }
        Ok(options)
    }
}

fn build_window(app: &adw::Application, options: &Options) {
    canvas::load_css();
    let canvas = canvas::Canvas::default();
    // Everything the application shows is inside the canvas area: the
    // window has no title bar of its own. The search field floats over the
    // top of the canvas while it is in use, so nothing moves when it opens.
    let entry = gtk::SearchEntry::new();
    entry.set_placeholder_text(Some("Search: name, port, id, property … or /regex/"));
    entry.set_width_request(380);
    entry.add_css_class("tw-search");
    // The match count sits in a slot of fixed width beside the entry, and
    // an empty slot of the same width on the other side keeps the entry
    // centred. Neither has a background, so only the entry and the count
    // itself cover the canvas.
    let count = gtk::Label::new(None);
    count.add_css_class("tw-count");
    count.set_halign(gtk::Align::Start);
    count.set_valign(gtk::Align::Center);
    count.set_visible(false);
    let slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    slot.set_width_request(120);
    slot.append(&count);
    let balance = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    balance.set_width_request(120);
    let search_box = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    search_box.set_halign(gtk::Align::Center);
    search_box.set_valign(gtk::Align::Start);
    search_box.set_margin_top(14);
    search_box.append(&balance);
    search_box.append(&entry);
    search_box.append(&slot);
    search_box.set_visible(false);
    // Problems reported by PipeWire, bottom left.
    let status = gtk::Label::new(None);
    status.add_css_class("osd");
    status.add_css_class("tw-status");
    status.set_halign(gtk::Align::Start);
    status.set_valign(gtk::Align::End);
    status.set_margin_start(10);
    status.set_margin_bottom(10);
    status.set_can_target(false);
    status.set_visible(false);
    let view = gtk::Overlay::new();
    view.set_child(Some(&canvas));
    view.add_overlay(&search_box);
    view.add_overlay(&status);
    let help = help_card();
    view.add_overlay(&help);
    // What a demo script has to say.
    let caption = gtk::Label::new(None);
    caption.add_css_class("osd");
    caption.add_css_class("tw-caption");
    caption.set_halign(gtk::Align::Center);
    // At the top, below where the search field appears: the bottom edge is
    // where arrows to unlinked nodes show up.
    caption.set_valign(gtk::Align::Start);
    caption.set_margin_top(76);
    caption.set_can_target(false);
    caption.set_visible(false);
    view.add_overlay(&caption);

    entry.connect_search_changed(glib::clone!(
        #[weak]
        canvas,
        #[weak]
        count,
        move |entry| match search::Query::parse(&entry.text()) {
            Ok(query) => {
                entry.remove_css_class("error");
                let searching = query.is_some();
                let found = canvas.set_search(query);
                count.set_label(&match (searching, found) {
                    (false, _) => String::new(),
                    (true, 0) => "no match".to_owned(),
                    (true, 1) => "1 match".to_owned(),
                    (true, n) => format!("{n} matches"),
                });
                count.set_visible(searching);
            }
            Err(_) => {
                entry.add_css_class("error");
                count.set_label("invalid regex");
                count.set_visible(true);
            }
        }
    ));
    for (step, connect) in [
        (
            1,
            gtk::SearchEntry::connect_activate as fn(&gtk::SearchEntry, _) -> _,
        ),
        (1, gtk::SearchEntry::connect_next_match),
        (-1, gtk::SearchEntry::connect_previous_match),
    ] {
        let canvas = canvas.downgrade();
        let go: Box<dyn Fn(&gtk::SearchEntry)> = Box::new(move |_| {
            if let Some(canvas) = canvas.upgrade() {
                canvas.step_match(step);
            }
        });
        connect(&entry, go);
    }

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("TangleWire")
        .default_width(options.size.map_or(1280, |s| s.0))
        .default_height(options.size.map_or(800, |s| s.1))
        .content(&view)
        .build();

    // Actions on plain keys, switched off while the search entry is in use.
    let mut plain = Vec::new();
    let fit = gtk::gio::SimpleAction::new("fit", None);
    fit.connect_activate(glib::clone!(
        #[weak]
        canvas,
        move |_, _| canvas.fit()
    ));
    window.add_action(&fit);
    app.set_accels_for_action("win.fit", &["f"]);
    plain.push(fit);

    let split = gtk::gio::SimpleAction::new("split", None);
    split.connect_activate(glib::clone!(
        #[weak]
        canvas,
        move |_, _| canvas.toggle_split()
    ));
    window.add_action(&split);
    app.set_accels_for_action("win.split", &["s"]);
    plain.push(split);

    // A demo looks the same on every machine: no personal rules.
    if options.mock.is_none() {
        match groups::Config::load() {
            Ok(config) => canvas.set_config(config),
            Err(message) => eprintln!("tanglewire: {message}"),
        }
    }
    if options.split {
        canvas.toggle_split();
    }
    let enlarge = options.scale.unwrap_or(3.0).clamp(1.0, 6.0);
    if options.screenshot.is_some() {
        canvas.set_boost(enlarge);
    }

    const STEP: f64 = 90.0;
    let keys: [(&str, &str, fn(&canvas::Canvas)); 10] = [
        ("groups", "g", canvas::Canvas::toggle_groups),
        ("hidden", "h", canvas::Canvas::toggle_hidden),
        ("list", "l", canvas::Canvas::toggle_list),
        ("delete", "Delete", canvas::Canvas::delete_selected),
        ("relayout", "<Ctrl>r", canvas::Canvas::relayout),
        ("back", "BackSpace", canvas::Canvas::back),
        ("left", "Left", |c| c.pan(STEP, 0.0)),
        ("right", "Right", |c| c.pan(-STEP, 0.0)),
        ("up", "Up", |c| c.pan(0.0, STEP)),
        ("down", "Down", |c| c.pan(0.0, -STEP)),
    ];
    for (name, key, run) in keys {
        let action = gtk::gio::SimpleAction::new(name, None);
        action.connect_activate(glib::clone!(
            #[weak]
            canvas,
            move |_, _| run(&canvas)
        ));
        window.add_action(&action);
        app.set_accels_for_action(&format!("win.{name}"), &[key]);
        plain.push(action);
    }

    // F1 shows the keys and mouse actions; F1 again or a click puts them away.
    let show_help = gtk::gio::SimpleAction::new("help", None);
    show_help.connect_activate(glib::clone!(
        #[weak]
        help,
        move |_, _| help.set_visible(!help.is_visible())
    ));
    window.add_action(&show_help);
    app.set_accels_for_action("win.help", &["F1"]);

    let find = gtk::gio::SimpleAction::new("search", None);
    find.connect_activate(glib::clone!(
        #[weak]
        entry,
        move |_, _| {
            if let Some(search_box) = entry.parent() {
                search_box.set_visible(true);
            }
            entry.grab_focus();
        }
    ));
    window.add_action(&find);
    app.set_accels_for_action("win.search", &["<Ctrl>f", "slash"]);
    plain.push(find);
    // Plain keys are text while the entry has the focus.
    let focus = gtk::EventControllerFocus::new();
    let plain = std::rc::Rc::new(plain);
    canvas.connect_typing(glib::clone!(
        #[strong]
        plain,
        move |typing| plain.iter().for_each(|action| action.set_enabled(!typing))
    ));
    focus.connect_enter(glib::clone!(
        #[strong]
        plain,
        move |_| plain.iter().for_each(|action| action.set_enabled(false))
    ));
    focus.connect_leave(move |_| plain.iter().for_each(|action| action.set_enabled(true)));
    entry.add_controller(focus);
    // Escape ends the search and hands the keyboard back to the graph.
    entry.connect_stop_search(glib::clone!(
        #[weak]
        window,
        move |entry| {
            entry.set_text("");
            if let Some(search_box) = entry.parent() {
                search_box.set_visible(false);
            }
            gtk::prelude::GtkWindowExt::set_focus(&window, gtk::Widget::NONE);
        }
    ));

    let (tx, rx) = async_channel::unbounded();
    let emit = move |event| {
        let _ = tx.send_blocking(event);
    };
    match &options.mock {
        Some(path) => match mock::Mock::load(path, emit) {
            Ok(mock) => {
                let mock = std::rc::Rc::new(std::cell::RefCell::new(mock));
                canvas.connect_command(glib::clone!(
                    #[strong]
                    mock,
                    move |command| mock.borrow_mut().run(command)
                ));
                if let Some(path) = &options.demo {
                    let script = demo::Script::load(path).unwrap_or_else(|message| {
                        eprintln!("tanglewire: {message}");
                        std::process::exit(1);
                    });
                    let stage = demo::Stage {
                        canvas: canvas.clone(),
                        mock,
                        window: window.clone(),
                        entry: entry.clone(),
                        caption: caption.clone(),
                        at: Default::default(),
                        held: Default::default(),
                    };
                    glib::spawn_future_local(demo::play(script, stage));
                }
            }
            Err(message) => {
                eprintln!("tanglewire: {message}");
                std::process::exit(1);
            }
        },
        None => {
            let pipewire = pw::spawn(emit);
            canvas.connect_command({
                let pipewire = pipewire.clone();
                move |command| pipewire.send(command)
            });
            // Also a way out of a connection that was lost or got stuck.
            if let Some(relayout) = window.lookup_action("relayout") {
                let relayout = relayout.downcast::<gtk::gio::SimpleAction>().unwrap();
                relayout.connect_activate(move |_, _| pipewire.reconnect());
            }
        }
    }
    let pictured = canvas.clone();
    gtk::glib::spawn_future_local(async move {
        let graph = std::rc::Rc::new(std::cell::RefCell::new(Graph::default()));
        // Counts updates, so that a burst of them is shown as one.
        let burst = std::rc::Rc::new(std::cell::Cell::new(0u64));
        while let Ok(event) = rx.recv().await {
            match event {
                // A new stream arrives as several updates in a row (node,
                // ports, links). Wait until they stop coming.
                Event::Synced => {
                    burst.set(burst.get() + 1);
                    let this = burst.get();
                    glib::timeout_add_local_once(
                        std::time::Duration::from_millis(120),
                        glib::clone!(
                            #[weak]
                            canvas,
                            #[strong]
                            graph,
                            #[strong]
                            burst,
                            move || {
                                if burst.get() == this {
                                    canvas.set_graph(&graph.borrow());
                                }
                            }
                        ),
                    );
                }
                // A lost connection stays on screen, anything else goes
                // away after a while.
                Event::Notice(message) => {
                    status.set_label(&message);
                    status.set_visible(true);
                    let shown = message.clone();
                    glib::timeout_add_seconds_local_once(
                        8,
                        glib::clone!(
                            #[weak]
                            status,
                            move || {
                                if status.label() == shown {
                                    status.set_visible(false);
                                }
                            }
                        ),
                    );
                }
                Event::Error(message) => {
                    status.set_label(&message);
                    status.set_visible(true);
                }
                Event::Reset => {
                    status.set_visible(false);
                    graph.borrow_mut().apply(Event::Reset);
                }
                event => graph.borrow_mut().apply(event),
            }
        }
    });

    window.present();
    // Not the search entry: plain keys should work from the start.
    gtk::prelude::GtkWindowExt::set_focus(&window, gtk::Widget::NONE);

    if options.demo.is_some() {
        // A demo is not to be disturbed: real keys do nothing, and a sheet
        // over everything keeps the real pointer off the canvas.
        let actions = [
            "fit", "split", "groups", "hidden", "list", "delete", "relayout", "back", "left",
            "right", "up", "down", "search", "help",
        ];
        for name in actions {
            app.set_accels_for_action(&format!("win.{name}"), &[]);
        }
        view.add_overlay(&gtk::Box::new(gtk::Orientation::Vertical, 0));
    }
    if let Some(path) = options.record.clone() {
        record(&window, path);
    }

    // Give the graph time to arrive and settle, then take the picture.
    if let Some(path) = options.screenshot.clone() {
        // The window may have been given another size since it opened.
        glib::timeout_add_local_once(std::time::Duration::from_millis(1700), move || {
            pictured.fit()
        });
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(2500),
            glib::clone!(
                #[weak]
                window,
                #[weak]
                app,
                move || {
                    let (width, height) = (window.width() as f64, window.height() as f64);
                    let picture = gtk::Snapshot::new();
                    picture.scale(enlarge, enlarge);
                    gtk::WidgetPaintable::new(Some(&window)).snapshot(&picture, width, height);
                    let renderer = window.native().and_then(|native| native.renderer());
                    let saved = match (picture.to_node(), renderer) {
                        (Some(node), Some(renderer)) => renderer
                            .render_texture(&node, None)
                            .save_to_png(&path)
                            .map_err(|e| e.to_string()),
                        _ => Err("nothing to draw".to_owned()),
                    };
                    if let Err(message) = saved {
                        eprintln!("tanglewire: {}: {message}", path.display());
                    }
                    app.quit();
                }
            ),
        );
    }
}

fn find() -> glib::ExitCode {
    let text = std::env::args().skip(2).collect::<Vec<_>>().join(" ");
    let query = match search::Query::parse(&text) {
        Ok(Some(query)) => query,
        Ok(None) => return glib::ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("tanglewire: {message}");
            return glib::ExitCode::FAILURE;
        }
    };
    let (tx, rx) = mpsc::channel();
    pw::spawn(move |event| {
        let _ = tx.send(event);
    });
    let mut graph = Graph::default();
    for event in rx {
        match event {
            Event::Synced => break,
            Event::Error(message) => {
                eprintln!("tanglewire: {message}");
                return glib::ExitCode::FAILURE;
            }
            event => graph.apply(event),
        }
    }
    let mut found: Vec<_> = graph
        .nodes
        .values()
        .filter_map(|node| Some((query.matches(&graph, node.id)?, node)))
        .collect();
    found.sort_by_key(|(m, _)| std::cmp::Reverse(m.score));
    for (m, node) in found {
        println!(
            "{:3} node {} {:?} ports {:?}",
            m.score,
            node.id,
            node.name(),
            m.ports
        );
    }
    glib::ExitCode::SUCCESS
}

/// Films the window into `path` for as long as it is open: every frame is
/// drawn a second time into a picture and handed to ffmpeg. Unlike a screen
/// recording this shows the window and nothing else, no pointer included.
fn record(window: &adw::ApplicationWindow, path: std::path::PathBuf) {
    use std::io::Write;
    let encoder: std::rc::Rc<std::cell::RefCell<Option<std::process::Child>>> = Default::default();
    // The window manager may still be giving the window its size and place
    // when it opens. Filming starts a moment later, and the size it has
    // then is the size of the film.
    let opened = std::time::Instant::now();
    let filmed = std::cell::Cell::new((0, 0));
    glib::timeout_add_local(
        std::time::Duration::from_millis(33),
        glib::clone!(
            #[weak]
            window,
            #[strong]
            encoder,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || {
                // Video wants even sizes.
                let (width, height) = (window.width() & !1, window.height() & !1);
                if opened.elapsed() < std::time::Duration::from_millis(1200) {
                    return glib::ControlFlow::Continue;
                }
                let picture = gtk::Snapshot::new();
                gtk::WidgetPaintable::new(Some(&window)).snapshot(
                    &picture,
                    width as f64,
                    height as f64,
                );
                let renderer = window.native().and_then(|native| native.renderer());
                let (Some(node), Some(renderer)) = (picture.to_node(), renderer) else {
                    return glib::ControlFlow::Continue;
                };
                let mut encoder = encoder.borrow_mut();
                if encoder.is_none() {
                    let size = format!("{width}x{height}");
                    // Frames are stamped with the time they arrive, so the
                    // film runs at the speed things happened.
                    let child = std::process::Command::new("ffmpeg")
                        .args([
                            "-loglevel",
                            "error",
                            "-y",
                            "-f",
                            "rawvideo",
                            "-pix_fmt",
                            "bgra",
                            "-s",
                            &size,
                        ])
                        .args([
                            "-use_wallclock_as_timestamps",
                            "1",
                            "-i",
                            "-",
                            "-vf",
                            "fps=30",
                        ])
                        .args([
                            "-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-pix_fmt",
                            "yuv420p",
                        ])
                        .arg(&path)
                        .stdin(std::process::Stdio::piped())
                        .spawn();
                    match child {
                        Ok(child) => {
                            *encoder = Some(child);
                            filmed.set((width, height));
                        }
                        Err(error) => {
                            eprintln!("tanglewire: ffmpeg: {error}");
                            return glib::ControlFlow::Break;
                        }
                    }
                }
                // A frame of another size would garble everything after it.
                if filmed.get() != (width, height) {
                    return glib::ControlFlow::Continue;
                }
                let area = gtk::graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
                let texture = renderer.render_texture(&node, Some(&area));
                let mut pixels = vec![0u8; (width * height * 4) as usize];
                texture.download(&mut pixels, (width * 4) as usize);
                let child = encoder.as_mut().and_then(|child| child.stdin.as_mut());
                if child.is_some_and(|pipe| pipe.write_all(&pixels).is_err()) {
                    return glib::ControlFlow::Break;
                }
                glib::ControlFlow::Continue
            }
        ),
    );
    // Closing the window ends the film.
    window.connect_close_request(move |_| {
        if let Some(mut child) = encoder.take() {
            drop(child.stdin.take());
            let _ = child.wait();
        }
        glib::Propagation::Proceed
    });
}

/// The overlay that lists what the keyboard and mouse do.
fn help_card() -> gtk::Box {
    const KEYS: [(&str, &str); 13] = [
        ("F", "Fit the linked part of the graph into view"),
        ("S", "Split view: one row per link"),
        ("L", "List of all nodes and ports, to drag links from"),
        ("G", "Collapse all groups, or expand them all"),
        ("H", "Show hidden nodes, or hide them again"),
        (
            "Ctrl+F  or  /",
            "Search by name, port, id, property or /regex/",
        ),
        (
            "Enter  /  Ctrl+G",
            "Go to the next match (Ctrl+Shift+G: previous)",
        ),
        ("Esc", "Close search or list"),
        ("Arrow keys", "Move the view"),
        ("Backspace", "Back to the view before a jump"),
        ("Delete", "Remove the selected link"),
        (
            "Ctrl+R",
            "Lay the graph out again and reconnect to PipeWire",
        ),
        ("F1", "This help"),
    ];
    const MOUSE: [(&str, &str); 11] = [
        (
            "Drag background",
            "Move the view (middle or right drag: anywhere)",
        ),
        ("Scroll", "Zoom"),
        (
            "Drag port to port",
            "Link them, or unlink if already linked",
        ),
        ("Drag title to title", "Link all ports, matched by channel"),
        ("Drag title to port", "Link the port that matches"),
        (
            "Ctrl+click ports",
            "Select several, then drag any one of them",
        ),
        ("Click a link", "Select it"),
        ("Right click a port", "Jump to the other end of its link"),
        ("Mouse back button", "Back to the view before a jump"),
        ("Click a group title", "Collapse or expand the group"),
        ("Click a list line", "Show that node"),
    ];
    let grid = gtk::Grid::new();
    grid.set_column_spacing(18);
    grid.set_row_spacing(5);
    let mut row = 0;
    for (heading, lines) in [("Keyboard", &KEYS[..]), ("Mouse", &MOUSE[..])] {
        let title = gtk::Label::new(Some(heading));
        title.add_css_class("heading");
        title.set_xalign(0.0);
        title.set_margin_top(if row == 0 { 0 } else { 10 });
        grid.attach(&title, 0, row, 2, 1);
        row += 1;
        for (what, does) in lines.iter().filter(|line| !line.0.is_empty()) {
            let what = gtk::Label::new(Some(what));
            what.add_css_class("tw-key");
            what.set_xalign(0.0);
            let does = gtk::Label::new(Some(does));
            does.set_xalign(0.0);
            grid.attach(&what, 0, row, 1, 1);
            grid.attach(&does, 1, row, 1, 1);
            row += 1;
        }
    }
    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.add_css_class("tw-panel");
    card.add_css_class("tw-help");
    card.set_halign(gtk::Align::Center);
    card.set_valign(gtk::Align::Center);
    card.append(&grid);
    let version = gtk::Label::new(Some(&format!("TangleWire {VERSION}")));
    version.add_css_class("dim-label");
    version.set_xalign(1.0);
    version.set_margin_top(12);
    card.append(&version);
    card.set_visible(false);
    let click = gtk::GestureClick::new();
    click.connect_released(glib::clone!(
        #[weak]
        card,
        move |_, _, _, _| card.set_visible(false)
    ));
    card.add_controller(click);
    card
}

/// Makes or removes the link between the two ports given on the command line,
/// then waits for PipeWire to confirm.
fn link(make: bool) -> glib::ExitCode {
    let port = |n| {
        std::env::args()
            .nth(n)
            .and_then(|arg| arg.parse::<graph::Id>().ok())
    };
    let (Some(out_port), Some(in_port)) = (port(2), port(3)) else {
        eprintln!("usage: tanglewire --link|--unlink <output port id> <input port id>");
        return glib::ExitCode::FAILURE;
    };
    let (tx, rx) = mpsc::channel();
    let commands = pw::spawn(move |event| {
        let _ = tx.send(event);
    });
    let mut graph = Graph::default();
    let mut sent = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while let Ok(event) =
        rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
    {
        match event {
            Event::Notice(message) | Event::Error(message) => {
                eprintln!("tanglewire: {message}");
                return glib::ExitCode::FAILURE;
            }
            Event::Synced => {
                let ends = connect::End::Ports(vec![out_port]);
                let plan = connect::plan(&graph, &ends, &connect::End::Ports(vec![in_port]));
                let linked = graph
                    .links
                    .values()
                    .any(|l| l.out_port == out_port && l.in_port == in_port);
                if linked == make {
                    println!("{}", if make { "linked" } else { "unlinked" });
                    return glib::ExitCode::SUCCESS;
                }
                if !sent {
                    sent = true;
                    if plan.is_empty() {
                        eprintln!("tanglewire: these two ports cannot be linked");
                        return glib::ExitCode::FAILURE;
                    }
                    for command in plan {
                        commands.send(command);
                    }
                }
            }
            event => graph.apply(event),
        }
    }
    eprintln!("tanglewire: timed out");
    glib::ExitCode::FAILURE
}

/// Prints the graph once it has settled; with `follow`, then prints every change.
fn dump(follow: bool) -> gtk::glib::ExitCode {
    let (tx, rx) = mpsc::channel();
    pw::spawn(move |event| {
        let _ = tx.send(event);
    });
    let mut graph = Graph::default();
    let mut settled = false;
    for event in rx {
        match &event {
            Event::Notice(message) | Event::Error(message) => {
                eprintln!("tanglewire: {message}");
                return gtk::glib::ExitCode::FAILURE;
            }
            Event::Synced if !settled => {
                settled = true;
                print_graph(&graph);
                if !follow {
                    break;
                }
            }
            Event::Synced => println!(
                "= {} nodes, {} ports, {} links",
                graph.nodes.len(),
                graph.ports.len(),
                graph.links.len()
            ),
            Event::Removed(id) if settled => println!("- {id}"),
            _ if settled => println!("+ {event:?}"),
            _ => {}
        }
        graph.apply(event);
    }
    gtk::glib::ExitCode::SUCCESS
}

fn print_graph(graph: &Graph) {
    let grouping = groups::resolve(graph, &groups::Config::load().unwrap_or_default(), false);
    for node in graph.nodes.values() {
        let group = match grouping.paths.get(&node.id) {
            _ if grouping.hidden.contains(&node.id) => "(hidden)".to_owned(),
            Some(path) => path.join("/"),
            None => "-".to_owned(),
        };
        println!(
            "node {} {:?} group={group:?} class={} link-group={} device={}",
            node.id,
            node.name(),
            node.prop("media.class").unwrap_or("-"),
            node.prop("node.link-group").unwrap_or("-"),
            node.prop("device.id").unwrap_or("-"),
        );
        for (direction, arrow) in [(Direction::In, "<-"), (Direction::Out, "->")] {
            for port in graph.ports_of(node.id, direction) {
                println!(
                    "  {arrow} {} {} {} {}{}",
                    port.id,
                    port.name,
                    port.channel.as_deref().unwrap_or("-"),
                    port.format.as_deref().unwrap_or("-"),
                    if port.monitor { " monitor" } else { "" },
                );
            }
        }
    }
    for link in graph.links.values() {
        println!(
            "link {} {}:{} -> {}:{}",
            link.id, link.out_node, link.out_port, link.in_node, link.in_port
        );
    }
    // What is drawn in place of the links through hidden nodes.
    let port = |id: graph::Id| graph.ports.get(&id).map_or("?", |p| p.name.as_str());
    for route in groups::through(graph, &grouping.hidden) {
        let via: Vec<String> = route.via.iter().map(|&n| graph.label(n)).collect();
        println!(
            "through {}:{} -> {}:{} via {}",
            graph.label(route.out_node),
            port(route.out_port),
            graph.label(route.in_node),
            port(route.in_port),
            via.join(", "),
        );
    }
    println!(
        "{} nodes, {} ports, {} links",
        graph.nodes.len(),
        graph.ports.len(),
        graph.links.len()
    );
}
