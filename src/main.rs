// Belch Proxy TUI – Passive HTTP Observer with Split-Screen Viewer

use std::collections::VecDeque;
use std::error::Error;
use std::io;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Span, Spans},
    widgets::{Block, Borders, Paragraph, Wrap},
    Terminal,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

#[derive(Clone)]
struct HttpLog {
    url: String,
    request: String,
    response: String,
}

struct App {
    logs: VecDeque<HttpLog>,
    selected: usize,
}

impl App {
    fn new() -> Self {
        App { logs: VecDeque::new(), selected: 0 }
    }
    fn next(&mut self) {
        if self.selected + 1 < self.logs.len() {
            self.selected += 1;
        }
    }
    fn previous(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }
    fn selected_log(&self) -> Option<&HttpLog> {
        self.logs.get(self.selected)
    }
}

/// Spawn a thread running Tokio listener on 127.0.0.1:1337
fn spawn_proxy_listener(app: Arc<Mutex<App>>) {
    thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let listener = TcpListener::bind("127.0.0.1:1337").await.unwrap();
            println!("🔌 Proxy listening on http://127.0.0.1:1337");

            loop {
                if let Ok((mut client, _)) = listener.accept().await {
                    let app = Arc::clone(&app);
                    tokio::spawn(async move {
                        let mut buf = [0u8; 8192];
                        if let Ok(n) = client.read(&mut buf).await {
                            let request = String::from_utf8_lossy(&buf[..n]).to_string();
                            let mut lines = request.lines();
                            let start_line = lines.next().unwrap_or("GET / HTTP/1.1");
                            let parts: Vec<&str> = start_line.split_whitespace().collect();
                            let method = parts.get(0).unwrap_or(&"GET");
                            let target = parts.get(1).unwrap_or(&"/");

                            // Manual absolute-URL parsing or Host header
                            let (host, port, path) = if target.starts_with("http://") {
                                let rest = &target[7..]; // strip "http://"
                                let mut split = rest.splitn(2, '/');
                                let hp = split.next().unwrap_or("");
                                let path = format!("/{}", split.next().unwrap_or(""));
                                let mut h = hp;
                                let mut p = 80;
                                if let Some(idx) = hp.rfind(':') {
                                    if let Ok(pp) = hp[idx+1..].parse::<u16>() {
                                        p = pp;
                                        h = &hp[..idx];
                                    }
                                }
                                (h.to_string(), p, path)
                            } else {
                                // Use Host: header for host and port, keep target as path
                                let host_hdr = request
                                    .lines()
                                    .find(|l| l.to_lowercase().starts_with("host:"))
                                    .and_then(|l| l.splitn(2, ' ').nth(1))
                                    .unwrap_or("127.0.0.1");
                                let mut hp = host_hdr.split(':');
                                let h = hp.next().unwrap_or("127.0.0.1").to_string();
                                let p = hp.next().and_then(|x| x.parse().ok()).unwrap_or(80);
                                (h, p, target.to_string())
                            };

                            // Construct and forward HTTP request
                            let forward = format!(
                                "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
                                method, path, host
                            );
                            if let Ok(mut upstream) = TcpStream::connect((host.as_str(), port)).await {
                                let _ = upstream.write_all(forward.as_bytes()).await;
                                let mut resp_buf = Vec::new();
                                let _ = upstream.read_to_end(&mut resp_buf).await;
                                let _ = client.write_all(&resp_buf).await;

                                // Log for TUI
                                let resp_txt = String::from_utf8_lossy(&resp_buf).to_string();
                                let mut app = app.lock().unwrap();
                                app.logs.push_back(HttpLog {
                                    url: format!("{} {} [Host: {}]", method, path, host),
                                    request: forward.clone(),  // log the cleaned request
                                    response: resp_txt.replace("
", "
"),
                                });
                            }
                        }
                    });
                }
            }
        });
    });
}

fn main() -> Result<(), Box<dyn Error>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let app = Arc::new(Mutex::new(App::new()));
    spawn_proxy_listener(app.clone());
    run_app(&mut terminal, app)?;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: Arc<Mutex<App>>,
) -> std::io::Result<()> {
    loop {
        terminal.draw(|f| {
            let app = app.lock().unwrap();
            let size = f.size();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(1)])
                .split(size);

            let panels = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(30), Constraint::Min(50)])
                .split(chunks[0]);

            let request_list = app.logs.iter().enumerate().map(|(i, log)| {
                let style = if i == app.selected {
                    Style::default().fg(Color::Black).bg(Color::White)
                } else {
                    Style::default()
                };
                Spans::from(Span::styled(log.url.clone(), style))
            }).collect::<Vec<_>>();

            f.render_widget(
                Paragraph::new(request_list)
                    .block(Block::default().borders(Borders::ALL).title("Requests")),
                panels[0],
            );

            let mut detail = vec![Spans::from(Span::styled(
                "Request:",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ))];
            if let Some(log) = app.selected_log() {
                detail.extend(log.request.lines().map(|l| Spans::from(Span::raw(l))));
                detail.push(Spans::from(Span::styled(
                    "Response:",
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                )));
                detail.extend(log.response.lines().map(|l| Spans::from(Span::raw(l))));
            } else {
                detail.push(Spans::from("No requests yet"));
            }

            f.render_widget(
                Paragraph::new(detail)
                    .block(Block::default().borders(Borders::ALL).title("Raw"))
                    .wrap(Wrap { trim: false }),
                panels[1],
            );

            f.render_widget(
                Paragraph::new("↑↓: Navigate   Q: Quit")
                    .style(Style::default().fg(Color::DarkGray)),
                chunks[1],
            );
        })?;

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Up => app.lock().unwrap().previous(),
                    KeyCode::Down => app.lock().unwrap().next(),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}
