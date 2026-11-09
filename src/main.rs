mod document;
mod layout;
mod state;
mod ui;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

#[derive(Parser)]
#[command(name = "xmread", about = "终端纯文字阅读器")]
struct Args {
    path: Option<PathBuf>,
    #[arg(long, value_name = "NUMBER")]
    chap: Option<usize>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let state = state::State::open()?;
    let path = match args.path {
        Some(path) => path,
        None => state
            .recent_path()?
            .context("没有阅读历史；请提供书籍路径")?,
    };
    let path = path
        .canonicalize()
        .with_context(|| format!("无法打开 {}", path.display()))?;
    let (saved, encoding) = state.load(&path)?;
    let book = document::open(&path, encoding.as_deref())?;
    let position = if let Some(chap) = args.chap {
        if chap == 0 || chap > book.chapters.len() {
            anyhow::bail!("章节编号应为 1 到 {}", book.chapters.len());
        }
        state::Position {
            chapter: chap - 1,
            offset: 0,
        }
    } else {
        saved
    };
    ui::run(book, state, position, encoding)
}
