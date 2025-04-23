// Belch Proxy TUI - Passive HTTP Observer with Split-Screen Viewer

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
	net::TcpListener,
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
	fn new() -> App {
		App { logs: VecDeque::new(), selected: 0 }
	}
	fn next(&mut self) {
		if self.selected < self.logs.len().saturating_sub(1) {
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

fn spawn_proxy_listener(app: Arc<Mutex<App>>) {
	thread::spawn(move || {
		let rt = tokio::runtime::Runtime::new().unwrap();
		rt.block_on(async move {
			let listener = TcpListener::bind("127.0.0.1:1337").await.unwrap();
			println!("🔌 Proxy listening on http://127.0.0.1:1337");
			loop {
				if let Ok((mut socket, _)) = listener.accept().await {
					let app = app.clone();
					tokio::spawn(async move {
						let mut buffer = [0; 8192];
						if let Ok(n) = socket.read(&mut buffer).await {
							let request = String::from_utf8_lossy(&buffer[..n]).to_string();
							// Parse request line and headers
							let mut lines = request.lines();
							let start_line = lines.next().unwrap_or("GET / HTTP/1.1");
							let parts: Vec<&str> = start_line.split_whitespace().collect();
							let method = parts.get(0).unwrap_or(&"GET");
							let path = parts.get(1).unwrap_or(&"/");
							let host = lines.find(|l| l.to_lowercase().starts_with("host:")).
								and_then(|l| l.split(':').nth(1)).unwrap_or("").trim();
							let label = format!("{} {} [Host: {}]", method, path, host);
							// Static response placeholder (update as needed)
							let body = "<html><body><h1>It works!</h1></body></html>";
							let response = format!(
								"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
								body.len(), body
							);
							let _ = socket.write_all(response.as_bytes()).await;
							// Log entry
							let mut app = app.lock().unwrap();
							app.logs.push_back(HttpLog {
								url: label,
								request: request.clone(),
								response: response.replace("\r\n", "\n"),
							});
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
	run_app(&mut terminal, app.clone())?;
	disable_raw_mode()?;
	execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
	terminal.show_cursor()?;
	Ok(())
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>, app: Arc<Mutex<App>>) -> std::io::Result<()> {
	loop {
		terminal.draw(|f| {
			let app = app.lock().unwrap();
			let size = f.size();
			let layout = Layout::default()
				.direction(Direction::Vertical)
				.constraints([Constraint::Min(0), Constraint::Length(1)])
				.split(size);

			let chunks = Layout::default()
				.direction(Direction::Horizontal)
				.constraints([Constraint::Length(30), Constraint::Min(50)])
				.split(layout[0]);

			let url_lines = app.logs.iter().enumerate().map(|(i, log)| {
				let style = if i == app.selected {
					Style::default().fg(Color::Black).bg(Color::White)
				} else {
					Style::default()
				};
				Spans::from(Span::styled(log.url.clone(), style))
			}).collect::<Vec<_>>();

			f.render_widget(
				Paragraph::new(url_lines).block(Block::default().borders(Borders::ALL).title("Requests")),
				chunks[0]
			);

			let detail_block = Block::default().borders(Borders::ALL).title("Raw");
			let detail_lines = if let Some(log) = app.selected_log() {
				let mut lines = vec![Spans::from(Span::styled("Request:", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)))];
				lines.extend(log.request.lines().map(|l| Spans::from(Span::raw(l))));
				lines.push(Spans::from(Span::styled("Response:", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))));
				lines.extend(log.response.lines().map(|l| Spans::from(Span::raw(l))));
				lines
			} else { vec![Spans::from(Span::raw("No selection"))] };

			f.render_widget(
				Paragraph::new(detail_lines)
					.block(detail_block)
					.wrap(Wrap { trim: false }),
				chunks[1]
			);

			f.render_widget(
				Paragraph::new("↑↓: Navigate   Q: Quit").style(Style::default().fg(Color::DarkGray)),
				layout[1]
			);
		})?;

		if event::poll(Duration::from_millis(50))? {
			match event::read()? {
				Event::Key(key) => match key.code {
					KeyCode::Char('q') => return Ok(()),
					KeyCode::Up => app.lock().unwrap().previous(),
					KeyCode::Down => app.lock().unwrap().next(),
					_ => {}
				},
				_ => {}
			}
		}
	}
}
