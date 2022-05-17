mod component;
mod text_area;

use chrono::{DateTime, Local};
use clap::Parser;
/// Based on the tui-rs user_input example
/// https://github.com/fdehau/tui-rs/blob/a6b25a487786534205d818a76acb3989658ae58c/examples/user_input.rs
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::{
    fs::create_dir_all,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use text_area::TextArea;
use tokio::{
    net::TcpStream,
    select,
    sync::{mpsc, oneshot, watch, RwLock},
    time::timeout,
    try_join,
};
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tracing::{error, info, warn};
use tracing_appender::non_blocking::WorkerGuard;
use tui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Span, Spans, Text},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame, Terminal,
};
use url::Url;

use crate::text_area::Action as TextAreaAction;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

type AppLock = Arc<RwLock<App>>;

// TODO Scrolling messages window
// TODO Multi-line input
// TODO ping/pong messages
// TODO binary/text message modes
// TODO Allow quitting w/ control-c even if a task hangs
// TODO Disconnect / reconnect to server
// TODO Component trait for composable UI elements

#[derive(Parser)]
struct CliOpts {
    /// Host part of target WS server address
    host: String,
    /// Port of target WS server
    port: u16,
    /// Log file to append to (since stdout/stderr is occupied with the TUI)
    /// I suggest running `tail -f` in another terminal window alongside wstui.
    #[clap(short, default_value = "/tmp/wstui/wstui.log")]
    log_path: PathBuf,
}

#[derive(Debug)]
enum InputMode {
    Normal,
    Editing,
}

/// An action the app can take
#[derive(Debug)]
enum Action {
    Quit,
    Input(TextAreaAction),
    NewMessage(Message),
    SetMode(InputMode),
}

#[derive(Debug)]
struct Message {
    pub ts: DateTime<Local>,
    pub text: String,
    pub kind: MessageKind,
}

impl Message {
    pub fn new(text: String, kind: MessageKind) -> Self {
        let ts = Local::now();
        Self { ts, text, kind }
    }
}

#[derive(Debug)]
enum MessageKind {
    /// An inbound message from the server
    Inbound,
    /// An outbound message to the server
    Outbound,
    /// An update about the state of the connection.
    Status,
}

/// App holds the state of the application
struct App {
    // /// Current value of the input box
    // input: String,
    /// Text input area
    text_area: TextArea,
    /// Current input mode
    input_mode: InputMode,
    /// History of recorded messages
    messages: Vec<Message>,
    /// Target server to connect to
    target: Url,
}

impl App {
    pub fn new(opts: CliOpts) -> Self {
        let target_url = format!("ws://{}:{}", opts.host, opts.port);
        Self {
            text_area: TextArea::new(),
            input_mode: InputMode::Editing,
            messages: Vec::new(),
            target: target_url.parse().expect("Invalid target URL"),
        }
    }
}

/// Configure tracing (logging)
fn setup_tracing(log_path: &Path) -> io::Result<WorkerGuard> {
    // Set up logging
    if let Some(log_dir) = log_path.parent() {
        // Create directory if it doesn't exist already
        create_dir_all(log_dir)?;
    }
    let log_file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .append(true)
        .open(log_path)?;
    let (non_blocking, guard) = tracing_appender::non_blocking(log_file);
    tracing_subscriber::fmt().with_writer(non_blocking).init();

    Ok(guard)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Get env vars from .env if present
    dotenv::dotenv().ok();

    // Get command line opts
    let opts = CliOpts::parse();

    let _log_guard = setup_tracing(&opts.log_path)?;

    info!("");
    info!("TEST 123");
    warn!("456 :)");

    // setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    info!("Terminal set up :+1:");

    // create app and run it
    let app = App::new(opts);
    let app_lock = Arc::new(RwLock::new(app));
    let res = run_app(&mut terminal, app_lock).await;

    info!("app finished");

    // restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        error!("{:?}", err)
    }

    info!("main done");

    Ok(())
}

/// Handle key inputs
fn get_action_from_key_event(key: KeyEvent, app: &App) -> Option<Action> {
    match app.input_mode {
        InputMode::Normal => handle_normal_input(key, app),
        InputMode::Editing => handle_edit_input(key, app),
    }
}

/// Handle key inputs in edit mode
fn handle_edit_input(key: KeyEvent, _app: &App) -> Option<Action> {
    use KeyCode::*;
    info!("Key press: {:?}", key);
    match (key.code, key.modifiers) {
        // TODO: Actually send message?
        // (Enter, _) => Some(Action::Input(TextAreaAction::Clear)),
        (Enter, _) => Some(Action::Input(TextAreaAction::NewLine)),
        (Backspace, _) => Some(Action::Input(TextAreaAction::Backspace)),
        (Esc, _) => Some(Action::SetMode(InputMode::Normal)),

        // Cursor movement
        (Left, _) => Some(Action::Input(TextAreaAction::MoveLeft)),
        (Right, _) => Some(Action::Input(TextAreaAction::MoveRight)),
        (Up, _) => Some(Action::Input(TextAreaAction::MoveUp)),
        (Down, _) => Some(Action::Input(TextAreaAction::MoveDown)),
        (Home, _) => Some(Action::Input(TextAreaAction::MoveHome)),
        (End, _) => Some(Action::Input(TextAreaAction::MoveEnd)),

        (Char(c), _) => Some(Action::Input(TextAreaAction::NewChar(c))),

        _ => None,
    }
}

/// Handle key inputs in normal mode
fn handle_normal_input(key: KeyEvent, _app: &App) -> Option<Action> {
    match key.code {
        KeyCode::Char('e') => Some(Action::SetMode(InputMode::Editing)),
        KeyCode::Char('q') => Some(Action::Quit),
        _ => None,
    }
}

async fn run_app<B: Backend>(terminal: &mut Terminal<B>, app_lock: AppLock) -> anyhow::Result<()> {
    let action_buf_size = 1000;
    let (action_tx, action_rx) = mpsc::channel(action_buf_size);
    let (quit_tx, quit_rx) = watch::channel(false);

    {
        // Initial draw
        let app = app_lock.read().await;
        terminal.draw(|f| ui(f, &app))?;

        // Connect to server.
        // TODO: Move this somewhere else?
        let conn_fut = connect_to_server(app.target.clone(), action_tx.clone());
        tokio::spawn(conn_fut);
    };

    // Spawn tasks
    info!("app 123");
    let keys_fut = listen_for_keys(action_tx.clone(), app_lock.clone(), quit_rx.clone());
    let msgs_fut = listen_for_messages(action_tx, app_lock.clone(), quit_rx);
    let update_fut = update_state(terminal, action_rx, app_lock, quit_tx);
    info!("app 456");

    try_join!(keys_fut, msgs_fut, update_fut)?;
    info!("app joined!");

    Ok(())

    // loop {
    //     terminal.draw(|f| ui(f, &app))?;

    //     if let Event::Key(key) = event::read()? {
    //         if let Some(action) = handle_input(key, &mut app) {
    //             match action {
    //                 // Quit the app
    //                 Action::Quit => return Ok(()),
    //             }
    //         }
    //     }
    // }
}

/// Get crossterm event asynchronously from
/// another thread using a oneshot channel.
fn get_crossterm_event() -> oneshot::Receiver<crossterm::Result<Event>> {
    let (tx, rx) = oneshot::channel();
    std::thread::spawn(|| tx.send(event::read()));
    rx
}

/// Listen for and react to keypress events.
async fn listen_for_keys(
    mut action_tx: mpsc::Sender<Action>,
    mut app_lock: AppLock,
    mut quit_rx: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    loop {
        select! {
            Ok(()) = quit_rx.changed() => {
                warn!("Received quit signal");
                return Ok(())
            },
            Ok(Ok(event)) = get_crossterm_event() => {
                match event {
                    Event::Key(key) => {
                        handle_input_action(key, &mut action_tx, &mut app_lock).await?;
                    }
                    Event::Mouse(_) => {}
                    Event::Resize(_, _) => {}
                }
            }
        }
    }
}

async fn handle_input_action(
    key: KeyEvent,
    action_tx: &mut mpsc::Sender<Action>,
    app_lock: &mut AppLock,
) -> anyhow::Result<()> {
    info!("start handle_input_action");
    let app = app_lock.read().await;
    if let Some(action) = get_action_from_key_event(key, &app) {
        if let Err(err) = action_tx.send(action).await {
            error!("Oops: {}", err);
        }
    }

    info!("end handle_input_action");
    Ok(())
}

/// Listen for and react to external messages.
async fn listen_for_messages(
    action_tx: mpsc::Sender<Action>,
    app_lock: AppLock,
    quit_rx: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    // TODO
    info!("listen for messages");
    Ok(())
}

/// Task to update state from actions (like a reducer)
async fn update_state<B: Backend>(
    terminal: &mut Terminal<B>,
    mut action_rx: mpsc::Receiver<Action>,
    app_lock: AppLock,
    quit_tx: watch::Sender<bool>,
) -> anyhow::Result<()> {
    while let Some(action) = action_rx.recv().await {
        info!("Got action: {:?}", action);
        let mut app = app_lock.write().await;
        match action {
            Action::Quit => {
                warn!("Quitting.");
                // Tell other tasks to end
                quit_tx.send(true)?;
                // Quit this task
                return Ok(());
            }
            Action::Input(input_action) => app.text_area.update(input_action),

            // Action::Input(input_action) => match input_action {
            //     TextAreaAction::NewChar(c) => app.input.push(c),
            //     TextAreaAction::Backspace => {
            //         app.input.pop();
            //     }
            //     TextAreaAction::Send => {
            //         let text = app.input.drain(..).collect();
            //         let message = Message::new(text, MessageKind::Outbound);
            //         app.messages.push(message);
            //     }
            //     TextAreaAction::NewLine => todo!(), // TODO
            // },
            Action::NewMessage(message) => app.messages.push(message),
            Action::SetMode(mode) => {
                app.input_mode = mode;
            }
        }

        info!("drawing.");
        terminal.draw(|f| ui(f, &app))?;
    }

    info!("update_state exiting.");

    Ok(())
}

fn ui<B: Backend>(f: &mut Frame<B>, app: &App) {
    let num_input_lines = app.text_area.nlines() as u16;
    info!("num_input_lines = {}", num_input_lines);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints(
            [
                Constraint::Length(1),
                Constraint::Length(num_input_lines + 2), // 2 for borders
                Constraint::Min(1),
            ]
            .as_ref(),
        )
        .split(f.size());

    let help_message = create_help_message(&app);
    f.render_widget(help_message, chunks[0]);

    let input_area = chunks[1];
    // let input = create_input(&app);
    let text_input = app.text_area.render(&input_area);
    handle_input_cursor(f, &app, &input_area);
    f.render_widget(text_input, input_area);

    let messages: Vec<ListItem> = app.messages.iter().map(format_message).collect();
    let messages =
        List::new(messages).block(Block::default().borders(Borders::ALL).title("Messages"));
    f.render_widget(messages, chunks[2]);
}

/// Manage cursor control for the input field
fn handle_input_cursor<B: Backend>(frame: &mut Frame<B>, app: &App, area: &Rect) {
    match app.input_mode {
        InputMode::Normal =>
            // Hide the cursor. `Frame` does this by default, so we don't need to do anything here
            {}

        InputMode::Editing => {
            // Make the cursor visible and ask tui-rs to put it at the specified coordinates after rendering
            // frame.set_cursor(
            //     // Put cursor past the end of the input text
            //     area.x + app.input.width() as u16 + 1,
            //     // Move one line down, from the border to the input line
            //     area.y + 1,
            // )

            frame.set_cursor(
                area.x + app.text_area.cursor.col as u16 + 1,
                area.y + app.text_area.cursor.row as u16 + 1,
            );
        }
    }
}

// fn create_input<'a>(app: &'a App) -> Paragraph<'a> {
//     Paragraph::new(app.input.as_ref())
//         .style(match app.input_mode {
//             InputMode::Normal => Style::default(),
//             InputMode::Editing => Style::default().fg(Color::Yellow),
//         })
//         .block(Block::default().borders(Borders::ALL).title("Input"))
// }

fn create_help_message(app: &App) -> Paragraph {
    let (msg, style) = match app.input_mode {
        InputMode::Normal => (
            vec![
                Span::raw("Press "),
                Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(" to exit, "),
                Span::styled("e", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(" to start editing."),
            ],
            Style::default(),
        ),
        InputMode::Editing => (
            vec![
                Span::raw("Press "),
                Span::styled("Esc", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(" to stop editing, "),
                Span::styled("Enter", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(" to record the message"),
            ],
            Style::default(),
        ),
    };
    let mut text = Text::from(Spans::from(msg));
    text.patch_style(style);

    Paragraph::new(text)
}

fn format_message(message: &Message) -> ListItem<'static> {
    let style = match message.kind {
        MessageKind::Inbound => Style::default().fg(Color::Green),
        MessageKind::Outbound => Style::default().fg(Color::Green),
        MessageKind::Status => Style::default(),
    };

    let ts_str = message.ts.format("%Y-%m-%d %H:%m:%S");
    let line = format!("{:?} [{}] {}", message.kind, ts_str, message.text);

    let mut text = Text::from(Spans::from(line));
    text.patch_style(style);

    ListItem::new(text)
}

fn new_status_msg_action(text: String) -> Action {
    Action::NewMessage(Message::new(text, MessageKind::Status))
}

async fn connect_to_server(url: Url, action_tx: mpsc::Sender<Action>) -> anyhow::Result<WsStream> {
    let msg = format!("Connecting to {}", url);
    let action = new_status_msg_action(msg);
    action_tx.send(action).await?;

    // TODO: Add timeout / retry?
    let timeout_secs = 3;
    let timeout_duration = Duration::from_secs(timeout_secs);
    let conn_fut = connect_async(url);
    let conn_timeout = timeout(timeout_duration, conn_fut);
    match conn_timeout.await {
        Ok(Ok((stream, _))) => {
            let action = new_status_msg_action("Connection established!".to_string());
            action_tx.send(action).await?;
            Ok(stream)
        }
        Ok(Err(err)) => {
            error!("misc conn err: {}", err);
            // TODO: Error style (distinct from status/info)
            let msg = format!("Error connecting to server: {}", err);
            let action = new_status_msg_action(msg);
            action_tx.send(action).await?;
            Err(err.into())
        }
        Err(err) => {
            error!("timeout err: {}", err);
            let msg = format!("Timed out after {} seconds: {}", timeout_secs, err);
            let action = new_status_msg_action(msg);
            action_tx.send(action).await?;
            Err(err.into())
        }
    }
}
