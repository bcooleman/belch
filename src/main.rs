// Belch Proxy TUI – Passive HTTP/HTTPS Observer

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
    io::{AsyncReadExt, AsyncWriteExt, split},
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
        Self { logs: VecDeque::new(), selected: 0 }
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

/// Start the proxy listener on localhost:1337
fn spawn_proxy_listener(app: Arc<Mutex<App>>) {
    thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let listener = TcpListener::bind("127.0.0.1:1337").await.unwrap();
            println!("🔌 Proxy listening on http://127.0.0.1:1337");
            loop {
                let (mut client, _) = listener.accept().await.unwrap();
                let app = Arc::clone(&app);
                tokio::spawn(async move {
                    // Read first frame
                    let mut buf = [0u8; 8192];
                    let n = match client.read(&mut buf).await {
                        Ok(n) if n > 0 => n,
                        _ => return,
                    };
                    let header = String::from_utf8_lossy(&buf[..n]).to_string();
                    let mut lines = header.lines();
                    let start = lines.next().unwrap_or_default();
                    let mut parts = start.split_whitespace();
                    let method = parts.next().unwrap_or("");
                    let target = parts.next().unwrap_or("");

                    // Split client into read/write halves
                    let (mut client_r, mut client_w) = split(client);

                    if method.eq_ignore_ascii_case("CONNECT") {
                        // Acknowledge tunnel
                        let _ = client_w.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await;
                        {
                            let mut guard = app.lock().unwrap();
                            guard.logs.push_back(HttpLog {
                                url: format!("CONNECT {}", target),
                                request: start.to_string(),
                                response: "[Tunnel established]".to_string(),
                            });
                        }
                        // Connect upstream
                        if let Ok(upstream) = TcpStream::connect(target).await {
                            let (mut up_r, mut up_w) = split(upstream);
                            let mut cbuf = [0u8; 8192];
                            let mut ubuf = [0u8; 8192];
                            loop {
                                // Read client -> upstream
                                let cm = match client_r.read(&mut cbuf).await {
                                    Ok(0) | Err(_) => break,
                                    Ok(m) => m,
                                };
                                let creq = String::from_utf8_lossy(&cbuf[..cm]).to_string();
                                if up_w.write_all(&cbuf[..cm]).await.is_err() { break; }
                                // Read upstream -> client
                                let um = match up_r.read(&mut ubuf).await {
                                    Ok(0) | Err(_) => break,
                                    Ok(m) => m,
                                };
                                let uresp = String::from_utf8_lossy(&ubuf[..um]).to_string();
                                let _ = client_w.write_all(&ubuf[..um]).await;
                                // Log as single entry
                                let mut guard = app.lock().unwrap();
                                guard.logs.push_back(HttpLog {
                                    url: format!("Tunnel {}", target),
                                    request: creq,
                                    response: uresp,
                                });
                            }
                        }
                    } else {
                        // Plain HTTP
                        // header already contains initial request
                        let request = header.clone();
                        let mut lines = request.lines();
                        let first = lines.next().unwrap_or_default();
                        let parts: Vec<&str> = first.split_whitespace().collect();
                        let meth = parts.get(0).copied().unwrap_or("");
                        let path = parts.get(1).copied().unwrap_or("/");
                        let host_hdr = request.lines()
                            .find(|l| l.to_lowercase().starts_with("host:"))
                            .and_then(|l| l.splitn(2, ' ').nth(1))
                            .unwrap_or("127.0.0.1");
                        let mut hp = host_hdr.split(':');
                        let host = hp.next().unwrap_or("127.0.0.1");
                        let port = hp.next().and_then(|x| x.parse().ok()).unwrap_or(80);
                        let forward = format!("{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n", meth, path, host);
                        if let Ok(mut upstream) = TcpStream::connect((host, port)).await {
                            let _ = upstream.write_all(forward.as_bytes()).await;
                            let mut resp_buf = Vec::new();
                            let _ = upstream.read_to_end(&mut resp_buf).await;
                            let resp_str = String::from_utf8_lossy(&resp_buf).to_string().replace("\r\n", "\n");
                            {
                                let mut guard = app.lock().unwrap();
                                guard.logs.push_back(HttpLog {
                                    url: format!("{} {} [Host: {}]", meth, path, host),
                                    request: forward.clone(),
                                    response: resp_str.clone(),
                                });
                            }
                            let _ = client_w.write_all(&resp_buf).await;
                        }
                    }
                });
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
            let guard = app.lock().unwrap();
            let size = f.size();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(1)])
                .split(size);

            let panels = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(30), Constraint::Min(50)])
                .split(chunks[0]);

            // Requests list
            let list = guard.logs.iter().enumerate().map(|(i, log)| {
                let style = if i == guard.selected { Style::default().fg(Color::Black).bg(Color::White) } else { Style::default() };
                Spans::from(Span::styled(log.url.clone(), style))
            }).collect::<Vec<_>>();
            f.render_widget(
                Paragraph::new(list)
                    .block(Block::default().borders(Borders::ALL).title("Requests")),
                panels[0],
            );

            // Detail pane
            let mut detail = vec![Spans::from(Span::styled(
                "Request:", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ))];
            if let Some(log) = guard.selected_log() {
                detail.extend(log.request.lines().map(|l| Spans::from(Span::raw(l))));
                detail.push(Spans::from(Span::styled(
                    "Response:", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
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

            // Footer
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
