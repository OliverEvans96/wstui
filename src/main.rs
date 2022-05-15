use chrono::{DateTime, Local};
use clap::Parser;
/// Based on the tui-rs user_input example
/// https://github.com/fdehau/tui-rs/blob/a6b25a487786534205d818a76acb3989658ae58c/examples/user_input.rs
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::{error::Error, io, sync::Arc, time::Duration};
use tokio::{
    fs::File,
    io::AsyncWriteExt,
    net::TcpStream,
    select,
    sync::{mpsc, oneshot, watch, RwLock},
    time::timeout,
    try_join,
};
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tracing::{error, info, instrument, warn};
use tui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Span, Spans, Text},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame, Terminal,
};
use unicode_width::UnicodeWidthStr;
use url::Url;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

type AppLock = Arc<RwLock<App>>;

// TODO Scrolling messages window
// TODO Multi-line input
// TODO ping/pong messages
// TODO binary/text message modes
// TODO Allow quitting w/ control-c even if a task hangs
// TODO Disconnect / reconnect to server

#[derive(Parser)]
struct CliOpts {
    host: String,
    port: u16,
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
    Input(InputAction),
    NewMessage(Message),
    SetMode(InputMode),
}

#[derive(Debug)]
enum InputAction {
    NewChar(char),
    Backspace,
    // NewLine,
    Send,
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
    /// Current value of the input box
    input: String,
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
            input: String::new(),
            input_mode: InputMode::Editing,
            messages: Vec::new(),
            target: target_url.parse().expect("Invalid target URL"),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Get env vars from .env if present
    dotenv::dotenv().ok();

    // Get command line opts
    let opts = CliOpts::parse();

    // Set up logging
    let log_path = "/tmp/wstui.log";
    let log_file = std::fs::OpenOptions::new()
        .write(true)
        .append(true)
        .open(log_path)?;
    let (non_blocking, _guard) = tracing_appender::non_blocking(log_file);
    tracing_subscriber::fmt().with_writer(non_blocking).init();

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
    info!("Key press: {:?}", key);
    match (key.code, key.modifiers) {
        (KeyCode::Enter, _) => Some(Action::Input(InputAction::Send)),
        // (KeyCode::Enter, KeyModifiers::NONE) => Some(Action::Input(InputAction::NewLine)),
        (KeyCode::Char(c), _) => Some(Action::Input(InputAction::NewChar(c))),
        (KeyCode::Backspace, _) => Some(Action::Input(InputAction::Backspace)),
        (KeyCode::Esc, _) => Some(Action::SetMode(InputMode::Normal)),
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
            Action::Input(input_action) => match input_action {
                InputAction::NewChar(c) => app.input.push(c),
                InputAction::Backspace => {
                    app.input.pop();
                }
                InputAction::Send => {
                    let text = app.input.drain(..).collect();
                    let message = Message::new(text, MessageKind::Outbound);
                    app.messages.push(message);
                }
            },
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
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints(
            [
                Constraint::Length(1),
                Constraint::Length(3),
                Constraint::Min(1),
            ]
            .as_ref(),
        )
        .split(f.size());

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
    let help_message = Paragraph::new(text);
    f.render_widget(help_message, chunks[0]);

    let input = Paragraph::new(app.input.as_ref())
        .style(match app.input_mode {
            InputMode::Normal => Style::default(),
            InputMode::Editing => Style::default().fg(Color::Yellow),
        })
        .block(Block::default().borders(Borders::ALL).title("Input"));
    f.render_widget(input, chunks[1]);
    match app.input_mode {
        InputMode::Normal =>
            // Hide the cursor. `Frame` does this by default, so we don't need to do anything here
            {}

        InputMode::Editing => {
            // Make the cursor visible and ask tui-rs to put it at the specified coordinates after rendering
            f.set_cursor(
                // Put cursor past the end of the input text
                chunks[1].x + app.input.width() as u16 + 1,
                // Move one line down, from the border to the input line
                chunks[1].y + 1,
            )
        }
    }

    let messages: Vec<ListItem> = app.messages.iter().map(format_message).collect();
    let messages =
        List::new(messages).block(Block::default().borders(Borders::ALL).title("Messages"));
    f.render_widget(messages, chunks[2]);
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
