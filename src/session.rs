use controller;
use crossbeam_channel::{after, bounded, Receiver, Sender};
use editor_transport;
use fnv::FnvHashMap;
use languageserver_types::notification::Notification;
use languageserver_types::*;
use project_root::find_project_root;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use toml;
use types::*;
use util::*;

struct ControllerHandle {
    sender: Option<Sender<EditorRequest>>,
    is_alive: Receiver<Void>,
    thread: JoinHandle<()>,
}

type Controllers = FnvHashMap<Route, ControllerHandle>;

/// Start the main event loop.
///
/// This function starts editor transport and routes incoming editor requests to controllers.
/// One controller is spawned per unique route, which is essentially a product of editor session,
/// file type (represented as language id) and project (represented as project root path).
///
/// `initial_request` could be passed to avoid extra synchronization churn if event loop is started
/// as a result of request from editor.
pub fn start(config: &Config, initial_request: Option<&str>) -> i32 {
    info!("Starting main event loop");

    let editor = editor_transport::start(config, initial_request);
    if let Err(code) = editor {
        return code;
    }
    let editor = editor.unwrap();

    let languages = config.language.clone();
    let filetypes = filetype_to_language_id_map(config);

    let mut controllers: Controllers = FnvHashMap::default();

    let timeout = config.server.timeout;

    'event_loop: loop {
        // have to clone & collect as we mutate controllers inside `select!`
        let is_alive = controllers
            .values()
            .map(|c| c.is_alive.clone())
            .collect::<Vec<_>>();

        let timeout_channel = if timeout > 0 {
            Some(after(Duration::from_secs(timeout)))
        } else {
            None
        };

        select! {
            recv(timeout_channel) => {
                break 'event_loop
            }

            recv(is_alive, msg, from) => {
                assert!(msg.is_none()); // msg type is Void, so we only can get a closed event
                let mut route: Option<Route> = None;
                for (k, c) in controllers.iter() {
                    if c.is_alive == *from {
                        route = Some(k.clone());
                        break;
                    }
                }
                let c = controllers.remove(&route.unwrap()).unwrap();
                if c.thread.join().is_err() {
                    error!("Failed to join controller thread");
                };
            }

            recv(editor.receiver, request) => {
                // editor_tx was closed, either because of the unrecoverable error or timeout
                // nothing we can do except to gracefully exit by stopping session
                // luckily, next `kak-lsp --request` invocation would spin up fresh session
                if request.is_none() {
                    break 'event_loop;
                }
                // should be safe to unwrap as we just checked request for being None
                // done this way instead of `match` to reduce nesting
                let request = request.unwrap();
                // editor explicitely asked us to stop kak-lsp session
                // (and we stop, even if other editor sessions are using this kak-lsp session)
                if request.method == "stop" {
                    break 'event_loop;
                }
                // editor exited, we need to cleanup associated controllers
                if request.method == notification::Exit::METHOD {
                    exit_editor_session(&mut controllers, &request);
                    continue 'event_loop;
                }

                let language_id = filetypes.get(&request.meta.filetype);
                if language_id.is_none() {
                    debug!(
                        "Language server is not configured for filetype `{}`",
                        &request.meta.filetype
                    );
                    continue 'event_loop;
                }
                let language_id = language_id.unwrap();

                let root_path = find_project_root(&languages[language_id].roots, &request.meta.buffile);
                let route = Route {
                    session: request.meta.session.clone(),
                    language: language_id.clone(),
                    root: root_path.clone(),
                };

                debug!("Routing editor request to {:?}", route);

                if controllers.contains_key(&route) {
                    let controller = &controllers[&route];
                    if let Some(sender) = controller.sender.as_ref() {
                        sender.send(request);
                    }
                } else {
                    let controller = spawn_controller(
                        config.clone(),
                        route.clone(),
                        request,
                        editor.sender.clone(),
                    );
                    controllers.insert(route, controller);
                }
            }
        }
    }
    stop_session(&mut controllers);
    // signal to editor transport to stop writer thread
    drop(editor.sender);
    if editor.thread.join().is_err() {
        error!("Editor transport panicked");
    }
    0
}

/// Reap controllers associated with editor session.
fn exit_editor_session(controllers: &mut Controllers, request: &EditorRequest) {
    info!(
        "Editor session `{}` closed, shutting down associated language servers",
        request.meta.session
    );
    for (route, controller) in controllers.iter_mut() {
        if route.session == request.meta.session {
            info!("Exit {} in project {}", route.language, route.root);
            // to notify kak-lsp about editor session end we use the same `exit` notification as
            // used in LSP spec to notify language server to exit, thus we can just clone request
            // and pass it along
            if let Some(sender) = controller.sender.as_ref() {
                sender.send(request.clone());
            }
            controller.sender = None;
        }
    }
}

/// Shut down all language servers and exit.
fn stop_session(controllers: &mut Controllers) {
    let request = EditorRequest {
        meta: EditorMeta {
            session: "".to_string(),
            buffile: "".to_string(),
            filetype: "".to_string(),
            client: None,
            version: 0,
            fifo: None,
        },
        method: notification::Exit::METHOD.to_string(),
        params: toml::Value::Table(toml::value::Table::default()),
    };
    info!("Shutting down language servers and exiting");
    for (route, mut controller) in controllers.drain() {
        if let Some(sender) = controller.sender.as_ref() {
            sender.send(request.clone());
        }
        controller.sender = None;
        if controller.thread.join().is_err() {
            error!("{:?} controller panicked", route);
        };
        info!("Exit {} in project {}", route.language, route.root);
    }
}

fn spawn_controller(
    config: Config,
    route: Route,
    request: EditorRequest,
    editor_tx: Sender<EditorResponse>,
) -> ControllerHandle {
    let (is_alive_tx, is_alive_rx) = bounded(0);
    // NOTE 1024 is arbitrary
    let (controller_tx, controller_rx) = bounded(1024);

    let thread = thread::spawn(move || {
        controller::start(
            editor_tx,
            &controller_rx,
            is_alive_tx,
            &route,
            request,
            config,
        );
    });

    ControllerHandle {
        is_alive: is_alive_rx,
        sender: Some(controller_tx),
        thread,
    }
}
