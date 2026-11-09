use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout as TuiLayout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as UiLine, Span};
use ratatui::widgets::{Paragraph, Wrap};
use regex::RegexBuilder;

use crate::document::{self, Book};
use crate::layout::Layout;
use crate::state::{Bookmark, Position, State};

const COMMANDS: &[&str] = &["/toc", "/search", "/bookmark", "/encoding", "/quit"];
const ENCODINGS: &[&str] = &[
    "auto",
    "UTF-8",
    "GBK",
    "BIG5",
    "UTF-16LE",
    "UTF-16BE",
    "SHIFT_JIS",
    "WINDOWS-1252",
];

#[derive(Clone)]
enum Target {
    Chapter(usize),
    Position(Position),
    Encoding(Option<String>),
}

#[derive(Clone)]
struct Entry {
    label: String,
    target: Target,
}

struct Listing {
    title: String,
    entries: Vec<Entry>,
    selected: usize,
    filter: String,
}

struct CommandPrompt {
    input: String,
    cycle: Option<(String, usize)>,
}

impl CommandPrompt {
    fn new() -> Self {
        Self {
            input: "/".into(),
            cycle: None,
        }
    }

    fn complete(&mut self, reverse: bool) {
        let prefix = self
            .cycle
            .as_ref()
            .map_or(self.input.trim_end(), |(prefix, _)| prefix.as_str())
            .to_string();
        let candidates: Vec<&str> = COMMANDS
            .iter()
            .copied()
            .filter(|name| name.starts_with(&prefix))
            .collect();
        if candidates.is_empty() {
            return;
        }
        let index = match &self.cycle {
            Some((_, old)) if reverse => (old + candidates.len() - 1) % candidates.len(),
            Some((_, old)) => (old + 1) % candidates.len(),
            None if reverse => candidates.len() - 1,
            None => 0,
        };
        self.input = format!("{} ", candidates[index]);
        self.cycle = Some((prefix, index));
    }
}

enum View {
    Reading,
    Command(CommandPrompt),
    List(Listing),
}

struct Reader {
    book: Book,
    state: State,
    encoding: Option<String>,
    pos: Position,
    page: usize,
    layout: Layout,
    view: View,
    notice: String,
    boundary: Option<isize>,
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

pub fn run(book: Book, state: State, mut pos: Position, encoding: Option<String>) -> Result<()> {
    if pos.chapter >= book.chapters.len() {
        pos = Position {
            chapter: 0,
            offset: 0,
        };
    }
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let size = terminal.size()?;
    let layout = Layout::new(
        &book.chapters[pos.chapter].text,
        size.width as usize,
        size.height.saturating_sub(2) as usize,
    );
    let page = layout.page_for_offset(pos.offset);
    let mut reader = Reader {
        book,
        state,
        encoding,
        pos,
        page,
        layout,
        view: View::Reading,
        notice: String::new(),
        boundary: None,
    };
    reader.persist_page()?;
    loop {
        terminal.draw(|frame| reader.draw(frame.area(), frame))?;
        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if reader.on_key(key)? {
                        break;
                    }
                }
                Event::Resize(width, height) => reader.resize(width, height)?,
                _ => {}
            }
        }
    }
    Ok(())
}

impl Reader {
    fn draw(&self, area: Rect, frame: &mut ratatui::Frame<'_>) {
        let rows = TuiLayout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);
        match &self.view {
            View::List(list) => self.draw_list(rows[0], frame, list),
            _ => {
                let lines: Vec<UiLine<'_>> = self
                    .layout
                    .page_lines(self.page)
                    .iter()
                    .map(|line| UiLine::raw(line.text.as_str()))
                    .collect();
                frame.render_widget(Paragraph::new(lines), rows[0]);
            }
        }
        let chapter = &self.book.chapters[self.pos.chapter];
        let status = format!(
            "{}  |  {}/{} {}  |  {}/{}{}",
            self.book.title,
            self.pos.chapter + 1,
            self.book.chapters.len(),
            chapter.title,
            self.page + 1,
            self.layout.page_count(),
            if self.notice.is_empty() {
                String::new()
            } else {
                format!("  |  {}", self.notice)
            }
        );
        frame.render_widget(
            Paragraph::new(status).style(Style::default().fg(Color::Black).bg(Color::Gray)),
            rows[1],
        );
        let prompt = match &self.view {
            View::Reading => "".to_string(),
            View::Command(prompt) => {
                let candidates = COMMANDS
                    .iter()
                    .filter(|name| {
                        name.starts_with(
                            prompt
                                .cycle
                                .as_ref()
                                .map_or(prompt.input.as_str(), |(prefix, _)| prefix.as_str()),
                        )
                    })
                    .copied()
                    .collect::<Vec<_>>()
                    .join("  ");
                format!("{}  {candidates}", prompt.input)
            }
            View::List(list) => format!("{}  {}", list.title, list.filter),
        };
        frame.render_widget(Paragraph::new(prompt), rows[2]);
    }

    fn draw_list(&self, area: Rect, frame: &mut ratatui::Frame<'_>, list: &Listing) {
        let visible = list.visible();
        let start = if visible.is_empty() {
            0
        } else {
            list.selected.saturating_sub(area.height as usize / 2)
        };
        let lines: Vec<UiLine<'_>> = visible
            .iter()
            .skip(start)
            .take(area.height as usize)
            .enumerate()
            .map(|(row, (_, entry))| {
                let style = if start + row == list.selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                UiLine::from(Span::styled(entry.label.as_str(), style))
            })
            .collect();
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
    }

    fn on_key(&mut self, key: KeyEvent) -> Result<bool> {
        let view = std::mem::replace(&mut self.view, View::Reading);
        match view {
            View::Reading => self.key_reading(key),
            View::Command(mut prompt) => {
                match key.code {
                    KeyCode::Esc => {}
                    KeyCode::Enter => return self.execute(prompt.input.trim()),
                    KeyCode::Backspace => {
                        prompt.input.pop();
                        prompt.cycle = None;
                        self.view = View::Command(prompt);
                    }
                    KeyCode::Tab => {
                        prompt.complete(key.modifiers.contains(KeyModifiers::SHIFT));
                        self.view = View::Command(prompt);
                    }
                    KeyCode::BackTab => {
                        prompt.complete(true);
                        self.view = View::Command(prompt);
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        prompt.input.push(c);
                        prompt.cycle = None;
                        self.view = View::Command(prompt);
                    }
                    _ => self.view = View::Command(prompt),
                }
                Ok(false)
            }
            View::List(mut list) => {
                let count = list.visible().len();
                match key.code {
                    KeyCode::Esc => {}
                    KeyCode::Enter => {
                        if let Some((_, entry)) = list.visible().get(list.selected) {
                            return self.select(entry.target.clone());
                        }
                        self.view = View::List(list);
                    }
                    KeyCode::Left | KeyCode::Char('k') => {
                        list.selected = list.selected.saturating_sub(1);
                        self.view = View::List(list);
                    }
                    KeyCode::Right | KeyCode::Char('j') => {
                        if count > 0 {
                            list.selected = (list.selected + 1).min(count - 1);
                        }
                        self.view = View::List(list);
                    }
                    KeyCode::Backspace => {
                        list.filter.pop();
                        list.selected = 0;
                        self.view = View::List(list);
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        list.filter.push(c);
                        list.selected = 0;
                        self.view = View::List(list);
                    }
                    _ => self.view = View::List(list),
                }
                Ok(false)
            }
        }
    }

    fn key_reading(&mut self, key: KeyEvent) -> Result<bool> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if key.code == KeyCode::Char('/') && !ctrl {
            self.view = View::Command(CommandPrompt::new());
        } else if let Some(delta) = reading_direction(key.code) {
            if ctrl {
                self.chapter_step(delta)?;
            } else {
                self.page_step(delta)?;
            }
        }
        Ok(false)
    }

    fn page_step(&mut self, delta: isize) -> Result<()> {
        let next = self.page as isize + delta;
        if next >= 0 && (next as usize) < self.layout.page_count() {
            self.go(Position {
                chapter: self.pos.chapter,
                offset: self.layout.page_offset(next as usize),
            })?;
        } else {
            if self.boundary == Some(delta) {
                self.chapter_step(delta)?;
            } else {
                self.boundary = Some(delta);
                self.notice = "已到章节边界，再按一次进入相邻章节".into();
            }
        }
        Ok(())
    }

    fn chapter_step(&mut self, delta: isize) -> Result<()> {
        let next = self.pos.chapter as isize + delta;
        if next >= 0 && (next as usize) < self.book.chapters.len() {
            self.go(Position {
                chapter: next as usize,
                offset: 0,
            })?;
        } else {
            self.notice = "已到书籍边界".into();
        }
        Ok(())
    }

    fn go(&mut self, target: Position) -> Result<()> {
        let chapter = target.chapter.min(self.book.chapters.len() - 1);
        let new_layout = if chapter == self.pos.chapter {
            None
        } else {
            Some(Layout::new(
                &self.book.chapters[chapter].text,
                self.layout.width,
                self.layout.height,
            ))
        };
        let layout = new_layout.as_ref().unwrap_or(&self.layout);
        let page = layout.page_for_offset(target.offset);
        let pos = Position {
            chapter,
            offset: layout.page_offset(page),
        };
        self.state
            .save(&self.book.path, &pos, self.encoding.as_deref())?;
        self.pos = pos;
        self.page = page;
        if let Some(layout) = new_layout {
            self.layout = layout;
        }
        self.boundary = None;
        self.notice.clear();
        Ok(())
    }

    fn persist_page(&mut self) -> Result<()> {
        self.pos.offset = self.layout.page_offset(self.page);
        self.state
            .save(&self.book.path, &self.pos, self.encoding.as_deref())
    }

    fn resize(&mut self, width: u16, height: u16) -> Result<()> {
        self.layout = Layout::new(
            &self.book.chapters[self.pos.chapter].text,
            width as usize,
            height.saturating_sub(2) as usize,
        );
        self.page = self.layout.page_for_offset(self.pos.offset);
        self.persist_page()
    }

    fn execute(&mut self, raw: &str) -> Result<bool> {
        let command = raw.trim_start_matches('/').trim();
        let (name, arg) = command
            .split_once(char::is_whitespace)
            .map_or((command, ""), |(a, b)| (a, b.trim()));
        match name {
            "quit" => {
                self.persist_page()?;
                return Ok(true);
            }
            "toc" if !arg.is_empty() => match arg.parse::<usize>() {
                Ok(n) if n > 0 && n <= self.book.chapters.len() => self.go(Position {
                    chapter: n - 1,
                    offset: 0,
                })?,
                _ => self.notice = format!("章节编号应为 1 到 {}", self.book.chapters.len()),
            },
            "toc" => self.open_toc(),
            "search" if !arg.is_empty() => self.search(arg)?,
            "search" => self.notice = "请输入搜索词：/search 关键词".into(),
            "bookmark" if arg.is_empty() => self.open_bookmarks()?,
            "bookmark" => self.bookmark_command(arg)?,
            "encoding" if arg.is_empty() => self.open_encodings(),
            "encoding" => self.set_encoding(Some(arg))?,
            "" => {}
            _ => self.notice = format!("未知命令：/{name}"),
        }
        Ok(false)
    }

    fn open_toc(&mut self) {
        let entries = self
            .book
            .chapters
            .iter()
            .enumerate()
            .map(|(i, c)| Entry {
                label: format!("{:>4}. {}", i + 1, c.title),
                target: Target::Chapter(i),
            })
            .collect();
        self.view = View::List(Listing {
            title: "目录".into(),
            entries,
            selected: 0,
            filter: String::new(),
        });
    }

    fn search(&mut self, query: &str) -> Result<()> {
        let regex = RegexBuilder::new(&regex::escape(query))
            .case_insensitive(true)
            .build()?;
        let mut entries = Vec::new();
        for (chapter, item) in self.book.chapters.iter().enumerate() {
            for match_ in regex.find_iter(&item.text) {
                let before: String = item.text[..match_.start()]
                    .chars()
                    .rev()
                    .take(18)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                let after: String = item.text[match_.end()..].chars().take(24).collect();
                let snippet = format!("{}{}{}", before, match_.as_str(), after).replace('\n', " ");
                entries.push(Entry {
                    label: format!("{}  {}", item.title, snippet),
                    target: Target::Position(Position {
                        chapter,
                        offset: match_.start(),
                    }),
                });
                if entries.len() >= 500 {
                    break;
                }
            }
            if entries.len() >= 500 {
                break;
            }
        }
        if entries.is_empty() {
            self.notice = "未找到匹配内容".into();
        } else {
            self.view = View::List(Listing {
                title: format!("搜索 {query}（最多 500 条）"),
                entries,
                selected: 0,
                filter: String::new(),
            });
        }
        Ok(())
    }

    fn bookmark_command(&mut self, arg: &str) -> Result<()> {
        if let Some(name) = arg.strip_prefix("add") {
            let name = name.trim();
            let name = if name.is_empty() {
                self.bookmark_name()
            } else {
                name.to_string()
            };
            self.state.add_bookmark(&self.book.path, &self.pos, &name)?;
            self.notice = format!("已添加书签：{name}");
        } else if let Some(id) = arg.strip_prefix("delete ") {
            match id.trim().parse::<i64>() {
                Ok(id) => {
                    self.notice = if self.state.delete_bookmark(&self.book.path, id)? {
                        "已删除书签".into()
                    } else {
                        "书签不存在".into()
                    }
                }
                Err(_) => self.notice = "用法：/bookmark delete 编号".into(),
            }
        } else if arg == "list" {
            self.open_bookmarks()?;
        } else {
            self.notice = "用法：/bookmark [add [名称]|list|delete 编号]".into();
        }
        Ok(())
    }

    fn bookmark_name(&self) -> String {
        let snippet = self
            .layout
            .page_lines(self.page)
            .iter()
            .find(|line| !line.text.trim().is_empty())
            .map(|line| line.text.trim().chars().take(18).collect::<String>())
            .unwrap_or_default();
        format!(
            "{} · {}",
            self.book.chapters[self.pos.chapter].title, snippet
        )
    }

    fn open_bookmarks(&mut self) -> Result<()> {
        let marks: Vec<Bookmark> = self.state.bookmarks(&self.book.path)?;
        if marks.is_empty() {
            self.notice = "没有书签".into();
            return Ok(());
        }
        let entries = marks
            .into_iter()
            .map(|mark| Entry {
                label: format!("{}  {}", mark.id, mark.name),
                target: Target::Position(Position {
                    chapter: mark.chapter,
                    offset: mark.offset,
                }),
            })
            .collect();
        self.view = View::List(Listing {
            title: "书签".into(),
            entries,
            selected: 0,
            filter: String::new(),
        });
        Ok(())
    }

    fn open_encodings(&mut self) {
        let entries = ENCODINGS
            .iter()
            .map(|name| Entry {
                label: name.to_string(),
                target: Target::Encoding(if *name == "auto" {
                    None
                } else {
                    Some(name.to_string())
                }),
            })
            .collect();
        self.view = View::List(Listing {
            title: "编码".into(),
            entries,
            selected: 0,
            filter: String::new(),
        });
    }

    fn set_encoding(&mut self, encoding: Option<&str>) -> Result<()> {
        if self
            .book
            .path
            .extension()
            .is_none_or(|ext| !ext.eq_ignore_ascii_case("txt"))
        {
            self.notice = "仅 TXT 支持手动编码".into();
            return Ok(());
        }
        match document::open(&self.book.path, encoding) {
            Ok(book) => {
                self.book = book;
                self.encoding = encoding.map(str::to_string);
                self.pos.chapter = self.pos.chapter.min(self.book.chapters.len() - 1);
                self.layout = Layout::new(
                    &self.book.chapters[self.pos.chapter].text,
                    self.layout.width,
                    self.layout.height,
                );
                self.page = self.layout.page_for_offset(self.pos.offset);
                self.persist_page()?;
                self.notice = format!(
                    "编码：{}",
                    self.book.detected_encoding.as_deref().unwrap_or("auto")
                );
            }
            Err(err) => self.notice = format!("编码切换失败：{err}"),
        }
        Ok(())
    }

    fn select(&mut self, target: Target) -> Result<bool> {
        match target {
            Target::Chapter(chapter) => self.go(Position { chapter, offset: 0 })?,
            Target::Position(pos) => self.go(pos)?,
            Target::Encoding(label) => self.set_encoding(label.as_deref())?,
        }
        Ok(false)
    }
}

fn reading_direction(key: KeyCode) -> Option<isize> {
    match key {
        KeyCode::Left | KeyCode::Char('k') => Some(-1),
        KeyCode::Right | KeyCode::Char('j') => Some(1),
        _ => None,
    }
}

impl Listing {
    fn visible(&self) -> Vec<(usize, &Entry)> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                entry
                    .label
                    .to_lowercase()
                    .contains(&self.filter.to_lowercase())
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_completion_cycles_both_directions() {
        let mut prompt = CommandPrompt::new();
        prompt.complete(false);
        assert_eq!(prompt.input, "/toc ");
        prompt.complete(false);
        assert_eq!(prompt.input, "/search ");
        prompt.complete(true);
        assert_eq!(prompt.input, "/toc ");
        prompt.input = "/b".into();
        prompt.cycle = None;
        prompt.complete(false);
        assert_eq!(prompt.input, "/bookmark ");
    }

    #[test]
    fn vim_keys_follow_reading_progress() {
        assert_eq!(reading_direction(KeyCode::Char('j')), Some(1));
        assert_eq!(reading_direction(KeyCode::Char('k')), Some(-1));
        assert_eq!(reading_direction(KeyCode::Right), Some(1));
        assert_eq!(reading_direction(KeyCode::Left), Some(-1));
        assert_eq!(reading_direction(KeyCode::Up), None);
    }
}
