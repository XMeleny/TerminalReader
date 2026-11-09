use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Clone, Debug)]
pub struct Position {
    pub chapter: usize,
    pub offset: usize,
}

#[derive(Clone, Debug)]
pub struct Bookmark {
    pub id: i64,
    pub chapter: usize,
    pub offset: usize,
    pub name: String,
}

pub struct State {
    conn: Connection,
}

impl State {
    pub fn open() -> Result<Self> {
        let dirs = ProjectDirs::from("com", "xmread", "xmread").context("无法定位用户数据目录")?;
        std::fs::create_dir_all(dirs.data_local_dir())?;
        Self::open_at(&dirs.data_local_dir().join("state.sqlite3"))
    }

    fn open_at(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS books (
                path TEXT PRIMARY KEY,
                chapter INTEGER NOT NULL DEFAULT 0,
                offset INTEGER NOT NULL DEFAULT 0,
                encoding TEXT,
                last_open INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS bookmarks (
                id INTEGER PRIMARY KEY,
                path TEXT NOT NULL,
                chapter INTEGER NOT NULL,
                offset INTEGER NOT NULL,
                name TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS bookmark_path ON bookmarks(path);",
        )?;
        Ok(Self { conn })
    }

    pub fn recent_path(&self) -> Result<Option<PathBuf>> {
        let value: Option<String> = self
            .conn
            .query_row(
                "SELECT path FROM books ORDER BY last_open DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(value.map(PathBuf::from))
    }

    pub fn load(&self, path: &Path) -> Result<(Position, Option<String>)> {
        let key = path.to_string_lossy();
        let row = self
            .conn
            .query_row(
                "SELECT chapter, offset, encoding FROM books WHERE path=?1",
                [key.as_ref()],
                |r| {
                    Ok((
                        Position {
                            chapter: r.get(0)?,
                            offset: r.get(1)?,
                        },
                        r.get(2)?,
                    ))
                },
            )
            .optional()?;
        Ok(row.unwrap_or((
            Position {
                chapter: 0,
                offset: 0,
            },
            None,
        )))
    }

    pub fn save(&self, path: &Path, pos: &Position, encoding: Option<&str>) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO books(path,chapter,offset,encoding,last_open)
             VALUES(?1,?2,?3,?4,MAX(CAST((julianday('now')-2440587.5)*86400000 AS INTEGER),
                 (SELECT COALESCE(MAX(last_open),0)+1 FROM books)))
             ON CONFLICT(path) DO UPDATE SET chapter=excluded.chapter,offset=excluded.offset,
               encoding=excluded.encoding,last_open=excluded.last_open",
                params![path.to_string_lossy(), pos.chapter, pos.offset, encoding],
            )
            .context("保存阅读位置失败")?;
        Ok(())
    }

    pub fn bookmarks(&self, path: &Path) -> Result<Vec<Bookmark>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id,chapter,offset,name FROM bookmarks WHERE path=?1 ORDER BY id")?;
        let rows = stmt.query_map([path.to_string_lossy().as_ref()], |r| {
            Ok(Bookmark {
                id: r.get(0)?,
                chapter: r.get(1)?,
                offset: r.get(2)?,
                name: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn add_bookmark(&self, path: &Path, pos: &Position, name: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO bookmarks(path,chapter,offset,name) VALUES(?1,?2,?3,?4)",
            params![path.to_string_lossy(), pos.chapter, pos.offset, name],
        )?;
        Ok(())
    }

    pub fn delete_bookmark(&self, path: &Path, id: i64) -> Result<bool> {
        Ok(self.conn.execute(
            "DELETE FROM bookmarks WHERE path=?1 AND id=?2",
            params![path.to_string_lossy(), id],
        )? > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_survives_reopening_database() {
        let path = std::env::temp_dir().join(format!(
            "xmread-state-{}-{}.sqlite3",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let book = Path::new("test-book.epub");
        {
            let state = State::open_at(&path).unwrap();
            state
                .save(
                    book,
                    &Position {
                        chapter: 2,
                        offset: 345,
                    },
                    None,
                )
                .unwrap();
            state
                .add_bookmark(
                    book,
                    &Position {
                        chapter: 2,
                        offset: 345,
                    },
                    "marker",
                )
                .unwrap();
        }
        {
            let state = State::open_at(&path).unwrap();
            let (pos, _) = state.load(book).unwrap();
            assert_eq!((pos.chapter, pos.offset), (2, 345));
            assert_eq!(state.bookmarks(book).unwrap()[0].name, "marker");
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn recent_book_tracks_last_open_even_with_fast_switches() {
        let path = std::env::temp_dir().join(format!(
            "xmread-recent-{}-{}.sqlite3",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let state = State::open_at(&path).unwrap();
        let pos = Position {
            chapter: 0,
            offset: 0,
        };
        state.save(Path::new("a.txt"), &pos, None).unwrap();
        state.save(Path::new("b.txt"), &pos, None).unwrap();
        state.save(Path::new("a.txt"), &pos, None).unwrap();
        assert_eq!(
            state.recent_path().unwrap().unwrap(),
            PathBuf::from("a.txt")
        );
        drop(state);
        let _ = std::fs::remove_file(path);
    }
}
