use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Debug)]
pub struct Line {
    pub text: String,
    pub offset: usize,
}

#[derive(Clone, Debug)]
pub struct Layout {
    pub lines: Vec<Line>,
    pub width: usize,
    pub height: usize,
}

impl Layout {
    pub fn new(text: &str, width: usize, height: usize) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let mut lines = Vec::new();
        let mut current = String::new();
        let mut cells = 0;
        let mut start = 0;
        for (byte, grapheme) in text.grapheme_indices(true) {
            if grapheme == "\n" {
                lines.push(Line {
                    text: std::mem::take(&mut current),
                    offset: start,
                });
                cells = 0;
                start = byte + 1;
                continue;
            }
            let w = UnicodeWidthStr::width(grapheme);
            if cells > 0 && cells + w > width {
                lines.push(Line {
                    text: std::mem::take(&mut current),
                    offset: start,
                });
                start = byte;
                cells = 0;
            }
            current.push_str(grapheme);
            cells += w;
        }
        lines.push(Line {
            text: current,
            offset: start,
        });
        Self {
            lines,
            width,
            height,
        }
    }

    pub fn page_count(&self) -> usize {
        self.lines.len().div_ceil(self.height).max(1)
    }

    pub fn page_for_offset(&self, offset: usize) -> usize {
        let line = self
            .lines
            .partition_point(|line| line.offset <= offset)
            .saturating_sub(1);
        line / self.height
    }

    pub fn page_offset(&self, page: usize) -> usize {
        self.lines
            .get(page.saturating_mul(self.height))
            .map_or(0, |line| line.offset)
    }

    pub fn page_lines(&self, page: usize) -> &[Line] {
        let start = page.saturating_mul(self.height).min(self.lines.len());
        let end = (start + self.height).min(self.lines.len());
        &self.lines[start..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_chinese_by_terminal_cells() {
        let layout = Layout::new("甲乙丙丁", 4, 1);
        assert_eq!(layout.page_count(), 2);
        assert_eq!(layout.page_lines(0)[0].text, "甲乙");
        assert_eq!(layout.page_lines(1)[0].text, "丙丁");
        assert_eq!(layout.page_for_offset("甲乙".len()), 1);
    }

    #[test]
    fn repagination_can_restore_from_text_offset() {
        let text = "abcdefghij";
        let old = Layout::new(text, 3, 1);
        let offset = old.page_offset(2);
        let new = Layout::new(text, 4, 1);
        assert_eq!(new.page_for_offset(offset), 1);
    }
}
