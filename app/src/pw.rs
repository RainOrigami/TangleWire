//! PipeWire connection. Runs on its own thread with its own main loop and
//! reports graph changes over a channel.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::thread;

use pipewire as pw;
use pw::client::ClientChangeMask;
use pw::device::DeviceChangeMask;
use pw::node::NodeChangeMask;
use pw::proxy::ProxyT;
use pw::spa::utils::dict::DictRef;
use pw::types::ObjectType;

use crate::graph::{Direction, Event, Id, Link, Node, Port, Props};

/// A change to make to the graph, sent to the PipeWire thread.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Link {
        out_node: Id,
        out_port: Id,
        in_node: Id,
        in_port: Id,
    },
    /// Removes the link with this id.
    Unlink(Id),
}

/// Talks to the PipeWire thread.
#[derive(Clone)]
pub struct Handle(pw::channel::Sender<Request>);

impl Handle {
    pub fn send(&self, command: Command) {
        let _ = self.0.send(Request::Change(command));
    }

    /// Drops the connection and makes a new one. The graph is reported again
    /// from nothing, after an [`Event::Reset`].
    pub fn reconnect(&self) {
        let _ = self.0.send(Request::Reconnect);
    }
}

enum Request {
    Change(Command),
    Reconnect,
}

/// Starts the PipeWire thread. `emit` is called on that thread.
pub fn spawn(emit: impl Fn(Event) + Send + 'static) -> Handle {
    let (sender, receiver) = pw::channel::channel();
    thread::Builder::new()
        .name("pipewire".into())
        .spawn(move || {
            let emit: Emit = Rc::new(emit);
            if let Err(err) = run(&emit, receiver) {
                emit(Event::Error(err.to_string()));
            }
        })
        .expect("failed to spawn PipeWire thread");
    Handle(sender)
}

type Emit = Rc<dyn Fn(Event)>;

/// The loop outlives the connection, so that a lost or stuck one can be
/// replaced.
fn run(events: &Emit, requests: pw::channel::Receiver<Request>) -> Result<(), pw::Error> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let connection = Rc::new(RefCell::new(open(&mainloop, events)));

    let _requests = requests.attach(mainloop.loop_(), {
        let events = events.clone();
        let mainloop = mainloop.clone();
        move |request| {
            let command = match request {
                Request::Change(command) => command,
                Request::Reconnect => {
                    // The old connection goes before the new one comes.
                    let old = connection.borrow_mut().take();
                    drop(old);
                    events(Event::Reset);
                    *connection.borrow_mut() = open(&mainloop, &events);
                    return;
                }
            };
            let connection = connection.borrow();
            let Some(Connection { core, registry, .. }) = connection.as_ref() else {
                events(Event::Notice("Not connected to PipeWire".to_owned()));
                return;
            };
            let result = match command {
                Command::Link {
                    out_node,
                    out_port,
                    in_node,
                    in_port,
                } => core
                    .create_object::<pw::link::Link>(
                        "link-factory",
                        &pw::properties::properties! {
                            "link.output.node" => out_node.to_string(),
                            "link.output.port" => out_port.to_string(),
                            "link.input.node" => in_node.to_string(),
                            "link.input.port" => in_port.to_string(),
                            // Keep the link when our proxy goes away.
                            "object.linger" => "1"
                        },
                    )
                    .map(drop)
                    .map_err(|e| e.to_string()),
                Command::Unlink(id) => registry
                    .destroy_global(id)
                    .into_result()
                    .map(drop)
                    .map_err(|e| e.to_string()),
            };
            if let Err(message) = result {
                events(Event::Notice(message));
            }
        }
    });

    mainloop.run();
    Ok(())
}

/// One connection to PipeWire. The listeners go first: they hold the proxies,
/// which must not outlive the core.
struct Connection {
    _registry_listener: pw::registry::Listener,
    _core_listener: pw::core::Listener,
    registry: pw::registry::RegistryRc,
    core: pw::core::CoreRc,
    _context: pw::context::ContextRc,
}

/// Connects, or reports why not.
fn open(mainloop: &pw::main_loop::MainLoopRc, events: &Emit) -> Option<Connection> {
    connect(mainloop, events)
        .map_err(|err| events(Event::Error(err.to_string())))
        .ok()
}

fn connect(mainloop: &pw::main_loop::MainLoopRc, events: &Emit) -> Result<Connection, pw::Error> {
    let context = pw::context::ContextRc::new(mainloop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry_rc()?;

    // Node properties such as `node.link-group` are absent from the registry
    // global and only arrive with the bound proxy's info event.
    let nodes: Rc<RefCell<HashMap<Id, (pw::node::Node, pw::node::NodeListener)>>> = Rc::default();
    // Bound devices and clients with their listeners.
    let others: Rc<RefCell<HashMap<Id, Box<dyn std::any::Any>>>> = Rc::default();
    // Proxy ids of everything bound above, by global.
    let bound: Rc<RefCell<HashMap<Id, u32>>> = Rc::default();
    // Proxies of removed globals, until the server has answered the sync sent
    // after letting go of them. A bind to a global that was already gone
    // fails with an error addressed to the proxy, which arrives after the
    // removal.
    let leaving: Rc<RefCell<Vec<(pw::spa::utils::result::AsyncSeq, u32)>>> = Rc::default();
    let sync = Rc::new(Sync {
        core: core.downgrade(),
        pending: Cell::new(Some(core.sync(0)?)),
        dirty: Cell::new(false),
    });

    let _core_listener = core
        .add_listener_local()
        .done({
            let events = events.clone();
            let sync = sync.clone();
            let leaving = leaving.clone();
            move |id, seq| {
                leaving.borrow_mut().retain(|(sent, _)| *sent != seq);
                if id == pw::core::PW_ID_CORE && sync.done(seq) {
                    events(Event::Synced);
                }
            }
        })
        .error({
            let events = events.clone();
            let leaving = leaving.clone();
            move |id, _seq, res, message| {
                if id == pw::core::PW_ID_CORE && res == -libc::EPIPE {
                    // The connection is gone, until someone asks for a new one.
                    events(Event::Error(message.to_owned()));
                } else if id == pw::core::PW_ID_CORE && res == -libc::ENOENT {
                    // We let go of something the server had already
                    // removed ("unknown resource"), which is no news.
                } else if leaving.borrow().iter().any(|(_, proxy)| *proxy == id) {
                    // Nor is a short-lived client that went away before we
                    // could bind it ("no global").
                } else {
                    // For instance a link that the server refused to make.
                    events(Event::Notice(message.to_owned()));
                }
            }
        })
        .register();

    let _registry_listener = registry
        .add_listener_local()
        .global({
            let registry = registry.downgrade();
            let events = events.clone();
            let nodes = nodes.clone();
            let others = others.clone();
            let bound = bound.clone();
            let sync = sync.clone();
            move |global| {
                let Some(props) = global.props else { return };
                let id = global.id;
                sync.request();
                match global.type_ {
                    ObjectType::Node => {
                        events(Event::Node(Node {
                            id,
                            props: to_props(props),
                        }));
                        let Some(registry) = registry.upgrade() else {
                            return;
                        };
                        let Ok(node) = registry.bind::<pw::node::Node, _>(global) else {
                            return;
                        };
                        let listener = node
                            .add_listener_local()
                            .info({
                                let events = events.clone();
                                move |info| {
                                    // Other updates carry an empty dict.
                                    if !info.change_mask().contains(NodeChangeMask::PROPS) {
                                        return;
                                    }
                                    if let Some(props) = info.props() {
                                        events(Event::Node(Node {
                                            id,
                                            props: to_props(props),
                                        }));
                                    }
                                }
                            })
                            .register();
                        bound.borrow_mut().insert(id, node.upcast_ref().id());
                        nodes.borrow_mut().insert(id, (node, listener));
                    }
                    ObjectType::Port => {
                        if let Some(port) = to_port(id, props) {
                            events(Event::Port(port));
                        }
                    }
                    // Devices and clients are only kept for what they say
                    // about the nodes that belong to them.
                    ObjectType::Device => {
                        events(Event::Device(id, to_props(props)));
                        let Some(registry) = registry.upgrade() else {
                            return;
                        };
                        let Ok(device) = registry.bind::<pw::device::Device, _>(global) else {
                            return;
                        };
                        let listener = device
                            .add_listener_local()
                            .info({
                                let events = events.clone();
                                move |info| {
                                    if !info.change_mask().contains(DeviceChangeMask::PROPS) {
                                        return;
                                    }
                                    if let Some(props) = info.props() {
                                        events(Event::Device(id, to_props(props)));
                                    }
                                }
                            })
                            .register();
                        bound.borrow_mut().insert(id, device.upcast_ref().id());
                        others.borrow_mut().insert(id, Box::new((device, listener)));
                    }
                    ObjectType::Client => {
                        events(Event::Client(id, to_props(props)));
                        let Some(registry) = registry.upgrade() else {
                            return;
                        };
                        let Ok(client) = registry.bind::<pw::client::Client, _>(global) else {
                            return;
                        };
                        let listener = client
                            .add_listener_local()
                            .info({
                                let events = events.clone();
                                move |info| {
                                    if !info.change_mask().contains(ClientChangeMask::PROPS) {
                                        return;
                                    }
                                    if let Some(props) = info.props() {
                                        events(Event::Client(id, to_props(props)));
                                    }
                                }
                            })
                            .register();
                        bound.borrow_mut().insert(id, client.upcast_ref().id());
                        others.borrow_mut().insert(id, Box::new((client, listener)));
                    }
                    ObjectType::Link => {
                        if let Some(link) = to_link(id, props) {
                            events(Event::Link(link));
                        }
                    }
                    _ => {}
                }
            }
        })
        .global_remove({
            let events = events.clone();
            let nodes = nodes.clone();
            let others = others.clone();
            let bound = bound.clone();
            let leaving = leaving.clone();
            let sync = sync.clone();
            move |id| {
                sync.request();
                nodes.borrow_mut().remove(&id);
                others.borrow_mut().remove(&id);
                if let Some(proxy) = bound.borrow_mut().remove(&id) {
                    let sent = sync.core.upgrade().and_then(|core| core.sync(0).ok());
                    if let Some(sent) = sent {
                        leaving.borrow_mut().push((sent, proxy));
                    }
                }
                events(Event::Removed(id));
            }
        })
        .register();

    Ok(Connection {
        _registry_listener,
        _core_listener,
        registry,
        core,
        _context: context,
    })
}

/// Turns bursts of registry changes into one [`Event::Synced`] each, sent once
/// the server has answered everything the burst caused (node info included).
struct Sync {
    core: pw::core::CoreWeak,
    pending: Cell<Option<pw::spa::utils::result::AsyncSeq>>,
    dirty: Cell<bool>,
}

impl Sync {
    fn request(&self) {
        if self.pending.get().is_some() {
            self.dirty.set(true);
        } else if let Some(core) = self.core.upgrade() {
            self.pending.set(core.sync(0).ok());
        }
    }

    /// Returns true when the graph is settled.
    fn done(&self, seq: pw::spa::utils::result::AsyncSeq) -> bool {
        if self.pending.get() != Some(seq) {
            return false;
        }
        self.pending.set(None);
        if self.dirty.replace(false) {
            self.request();
            return false;
        }
        true
    }
}

fn to_props(dict: &DictRef) -> Props {
    dict.iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

fn to_port(id: Id, props: &DictRef) -> Option<Port> {
    Some(Port {
        id,
        node: props.get("node.id")?.parse().ok()?,
        name: props.get("port.name")?.to_owned(),
        direction: match props.get("port.direction")? {
            "in" => Direction::In,
            "out" => Direction::Out,
            _ => return None,
        },
        index: props
            .get("port.id")
            .and_then(|v| v.parse().ok())
            .unwrap_or(u32::MAX),
        props: to_props(props),
        channel: props.get("audio.channel").map(str::to_owned),
        format: props.get("format.dsp").map(str::to_owned),
        monitor: props.get("port.monitor") == Some("true"),
    })
}

fn to_link(id: Id, props: &DictRef) -> Option<Link> {
    let get = |key| props.get(key)?.parse().ok();
    Some(Link {
        id,
        out_node: get("link.output.node")?,
        out_port: get("link.output.port")?,
        in_node: get("link.input.node")?,
        in_port: get("link.input.port")?,
    })
}
