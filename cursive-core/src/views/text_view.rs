use parking_lot::Mutex;
use std::ops::Deref;
use std::sync::Arc;

use unicode_width::UnicodeWidthStr;

use crate::align::*;
use crate::style::{Effect, StyleType};
use crate::utils::lines::spans::{LinesIterator, Row};
use crate::utils::markup::StyledString;
use crate::view::{View, combine_layout_key, fresh_layout_key};
use crate::{Printer, Vec2, With};

// Content type used internally for caching and storage
type InnerContentType = Arc<StyledString>;

/// Provides access to the content of a [`TextView`].
///
/// [`TextView`]: struct.TextView.html
///
/// Cloning this object will still point to the same content.
///
/// # Examples
///
/// ```rust
/// # use cursive_core::views::{TextView, TextContent};
/// let mut content = TextContent::new("content");
/// let view = TextView::new_with_content(content.clone());
///
/// // Later, possibly in a different thread
/// content.set_content("new content");
/// assert!(view.get_content().source().contains("new"));
/// ```
#[derive(Clone)]
pub struct TextContent {
    content: Arc<Mutex<TextContentInner>>,
}

impl TextContent {
    /// Creates a new text content around the given value.
    ///
    /// Parses the given value.
    pub fn new<S>(content: S) -> Self
    where
        S: Into<StyledString>,
    {
        let content = Arc::new(content.into());
        let version = fresh_layout_key();

        TextContent {
            content: Arc::new(Mutex::new(TextContentInner {
                content_value: content,
                version,
                replaced_version: version,
            })),
        }
    }
}

/// A reference to the text content.
///
/// This can be deref'ed into a [`StyledString`].
///
/// [`StyledString`]: ../utils/markup/type.StyledString.html
pub struct TextContentRef {
    // We also need to keep a copy of Arc so `deref` can return
    // a reference to the `StyledString`
    data: Arc<StyledString>,
}

impl Deref for TextContentRef {
    type Target = StyledString;

    fn deref(&self) -> &StyledString {
        self.data.as_ref()
    }
}

impl TextContent {
    /// Replaces the content with the given value.
    pub fn set_content<S>(&self, content: S)
    where
        S: Into<StyledString>,
    {
        self.with_content(|c| {
            *c = content.into();
        });
    }

    /// Append `content` to the end of a `TextView`.
    pub fn append<S>(&self, content: S)
    where
        S: Into<StyledString>,
    {
        // Views sharing this content can then keep the rows they computed.
        self.with_content_inner(true, |c| {
            // This will only clone content if content_cached and content_value
            // are sharing the same underlying Rc.
            Arc::make_mut(&mut c.content_value).append(content);
        })
    }

    /// Returns a reference to the content.
    ///
    /// This locks the data while the returned value is alive,
    /// so don't keep it too long.
    pub fn get_content(&self) -> TextContentRef {
        TextContentInner::get_content(&self.content)
    }

    /// Apply the given closure to the inner content, and bust the cache afterward.
    pub fn with_content<F, O>(&self, f: F) -> O
    where
        F: FnOnce(&mut StyledString) -> O,
    {
        self.with_content_inner(false, |c| f(Arc::make_mut(&mut c.content_value)))
    }

    /// Apply the given closure to the inner content, and bump its version.
    fn with_content_inner<F, O>(&self, appended: bool, f: F) -> O
    where
        F: FnOnce(&mut TextContentInner) -> O,
    {
        let mut content = self.content.lock();

        let out = f(&mut content);

        content.version = fresh_layout_key();
        if !appended {
            content.replaced_version = content.version;
        }

        out
    }

    // The current content, its version, and the version of the last change
    // that wasn't just appending to it.
    fn snapshot(&self) -> (InnerContentType, u64, u64) {
        let content = self.content.lock();
        (
            Arc::clone(&content.content_value),
            content.version,
            content.replaced_version,
        )
    }

    fn version(&self) -> u64 {
        self.content.lock().version
    }

    // The length of the current content, in bytes.
    fn len(&self) -> usize {
        self.content.lock().content_value.source().len()
    }
}

/// Internal representation of the content for a `TextView`.
///
/// This is mostly just a `StyledString`.
///
/// Can be shared (through a `Arc<Mutex>`).
struct TextContentInner {
    // This is what `set_content` changes.
    content_value: InnerContentType,

    // Changes whenever `content_value` does.
    //
    // Several views can share this content: each keeps track of the version
    // it computed its rows from, rather than relying on a shared cache.
    version: u64,

    // The version of the last change that wasn't just appending: content at
    // any version since then is a prefix of the current one.
    replaced_version: u64,
}

impl TextContentInner {
    /// From a shareable content (Arc + Mutex), return a
    fn get_content(content: &Arc<Mutex<TextContentInner>>) -> TextContentRef {
        let data = Arc::clone(&content.lock().content_value);
        TextContentRef { data }
    }
}

/// A simple view showing a fixed text.
///
/// # Examples
///
/// ```rust
/// # use cursive_core::Cursive;
/// # use cursive_core::views::TextView;
/// let mut siv = Cursive::new();
///
/// siv.add_layer(TextView::new("Hello world!"));
/// ```
pub struct TextView {
    // Possibly shared content
    content: TextContent,

    // The content `rows` were computed from (the shared content may have
    // changed since), and its version.
    snapshot: InnerContentType,
    snapshot_version: u64,

    // Pre-computed rows for `snapshot`, based on the last view size.
    rows: Vec<Row>,

    // The width `rows` were computed for (`usize::MAX` without wrapping).
    rows_width: Option<usize>,

    // Whether some text was left out of `rows` (wider than a whole row).
    truncated: bool,

    // `snapshot_version` at the last `layout()`.
    laid_out_version: Option<u64>,

    // Sizes computed for `snapshot` at other widths (`(width, size)`), so
    // `required_size` can answer without wrapping the text again: parents
    // often ask for several widths during a single layout.
    sizes: Vec<(usize, Vec2)>,

    // Text alignment
    align: Align,

    // Default style for the text.
    //
    // Note that the text itself can be styled, which will override this.
    style: StyleType,

    // True if we can wrap long lines.
    wrap: bool,

    // Last requested width.
    //
    // Usually the longest row, but if a row had to be wrapped, it may be a bit larger.
    width: Option<usize>,
    // Selection?
    // selection: Option<Selection>,
}

// struct Selection {
//     segments: Vec<crate::utils::lines::spans::Segment>,
//     // dragging?
// }

impl TextView {
    /// Creates a new TextView with the given content.
    pub fn new<S>(content: S) -> Self
    where
        S: Into<StyledString>,
    {
        Self::new_with_content(TextContent::new(content))
    }

    /// Convenient function to create a TextView by parsing the given content as cursup.
    ///
    /// Shortcut for `TextView::new(cursup::parse(content))`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use cursive_core::views::TextView;
    /// let view = TextView::cursup("/red+bold{warning}");
    /// ```
    pub fn cursup<S>(content: S) -> Self
    where
        S: Into<String>,
    {
        Self::new(crate::utils::markup::cursup::parse(content))
    }

    /// Creates a new TextView using the given `TextContent`.
    ///
    /// If you kept a clone of the given content, you'll be able to update it
    /// remotely.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use cursive_core::views::{TextView, TextContent};
    /// let mut content = TextContent::new("content");
    /// let view = TextView::new_with_content(content.clone());
    ///
    /// // Later, possibly in a different thread
    /// content.set_content("new content");
    /// assert!(view.get_content().source().contains("new"));
    /// ```
    pub fn new_with_content(content: TextContent) -> Self {
        TextView {
            content,
            style: StyleType::default(),
            snapshot: Arc::new(StyledString::default()),
            snapshot_version: 0,
            rows: Vec::new(),
            rows_width: None,
            truncated: false,
            laid_out_version: None,
            sizes: Vec::new(),
            wrap: true,
            align: Align::top_left(),
            width: None,
        }
    }

    /// Creates a new empty `TextView`.
    pub fn empty() -> Self {
        TextView::new("")
    }

    /// Sets the effect for the entire content.
    #[deprecated(since = "0.16.0", note = "Use `set_style()` instead.")]
    pub fn set_effect(&mut self, effect: Effect) {
        self.set_style(effect);
    }

    /// Sets the style for the content.
    pub fn set_style<S: Into<StyleType>>(&mut self, style: S) {
        self.style = style.into();
    }

    /// Sets the effect for the entire content.
    ///
    /// Chainable variant.
    #[deprecated(since = "0.16.0", note = "Use `style()` instead.")]
    #[must_use]
    pub fn effect(self, effect: Effect) -> Self {
        self.style(effect)
    }

    /// Sets the style for the entire content.
    ///
    /// Chainable variant.
    #[must_use]
    pub fn style<S: Into<StyleType>>(self, style: S) -> Self {
        self.with(|s| s.set_style(style))
    }

    /// Disables content wrap for this view.
    ///
    /// This may be useful if you want horizontal scrolling.
    #[must_use]
    pub fn no_wrap(self) -> Self {
        self.with(|s| s.set_content_wrap(false))
    }

    /// Controls content wrap for this view.
    ///
    /// If `true` (the default), text will wrap long lines when needed.
    pub fn set_content_wrap(&mut self, wrap: bool) {
        if wrap != self.wrap {
            self.wrap = wrap;
            self.rows_width = None;
            self.sizes.clear();
            self.laid_out_version = None;
        }
    }

    /// Sets the horizontal alignment for this view.
    #[must_use]
    pub fn h_align(mut self, h: HAlign) -> Self {
        self.align.h = h;

        self
    }

    /// Sets the vertical alignment for this view.
    #[must_use]
    pub fn v_align(mut self, v: VAlign) -> Self {
        self.align.v = v;

        self
    }

    /// Sets the alignment for this view.
    #[must_use]
    pub fn align(mut self, a: Align) -> Self {
        self.align = a;

        self
    }

    /// Center the text horizontally and vertically inside the view.
    #[must_use]
    pub fn center(mut self) -> Self {
        self.align = Align::center();
        self
    }

    /// Replace the text in this view.
    ///
    /// Chainable variant.
    #[must_use]
    pub fn content<S>(self, content: S) -> Self
    where
        S: Into<StyledString>,
    {
        self.with(|s| s.set_content(content))
    }

    /// Replace the text in this view.
    pub fn set_content<S>(&mut self, content: S)
    where
        S: Into<StyledString>,
    {
        self.content.set_content(content);
    }

    /// Append `content` to the end of a `TextView`.
    pub fn append<S>(&mut self, content: S)
    where
        S: Into<StyledString>,
    {
        self.content.append(content);
    }

    /// Returns the current text in this view.
    pub fn get_content(&self) -> TextContentRef {
        TextContentInner::get_content(&self.content.content)
    }

    /// Returns a shared reference to the content, allowing content mutation.
    pub fn get_shared_content(&mut self) -> TextContent {
        // We take &mut here without really needing it,
        // because it sort of "makes sense".
        TextContent {
            content: Arc::clone(&self.content.content),
        }
    }

    // This must be non-destructive, as it may be called
    // multiple times during layout.
    // The width text is wrapped at, for the given view size.
    fn wrap_width(&self, size: Vec2) -> usize {
        if self.wrap { size.x } else { usize::MAX }
    }

    // Catches up with the shared content, dropping anything computed for
    // an older version.
    fn update_snapshot(&mut self) {
        let (content, version, replaced_version) = self.content.snapshot();
        if version != self.snapshot_version {
            // If text was only appended since, most rows are still valid.
            let appended = self.snapshot_version != 0 && self.snapshot_version >= replaced_version;
            match self.rows_width {
                Some(width) if appended && width > 0 && !self.truncated => {
                    self.append_rows(&content, width);
                }
                _ => self.rows_width = None,
            }
            self.snapshot = content;
            self.snapshot_version = version;
            self.sizes.clear();
        }
    }

    // Updates `rows` (computed for `self.snapshot` at `width`) for `content`,
    // which is `self.snapshot` with some text appended.
    //
    // Rows of a line only depend on that line: only the last line of the
    // snapshot (which may continue in `content`) and what follows need to be
    // wrapped again.
    fn append_rows(&mut self, content: &InnerContentType, width: usize) {
        let (span, offset) = last_line_start(&self.snapshot);
        let old_rows = LinesIterator::new_at(self.snapshot.as_ref(), width, span, offset).count();
        self.rows.truncate(self.rows.len().saturating_sub(old_rows));

        let mut lines = LinesIterator::new_at(content.as_ref(), width, span, offset);
        self.rows.extend(lines.by_ref());
        self.truncated = lines.is_truncated();
        self.update_width(width);
    }

    // Sets `self.width` for `rows` computed at `width`.
    fn update_width(&mut self, width: usize) {
        self.width = if self.truncated || self.rows.iter().any(|row| row.is_wrapped) {
            // If any rows are wrapped (or some text didn't even fit), then
            // require the full width.
            Some(width)
        } else {
            self.rows.iter().map(|row| row.width).max()
        };
    }

    fn compute_rows(&mut self, size: Vec2) {
        let width = self.wrap_width(size);
        self.update_snapshot();

        if self.rows_fit(width) {
            return;
        }

        self.rows_width = Some(width);
        if width == 0 {
            // Nothing fits.
            self.rows.clear();
            self.width = None;
            self.truncated = false;
            return;
        }

        let mut lines = LinesIterator::new(self.snapshot.as_ref(), width);
        self.rows = lines.by_ref().collect();
        self.truncated = lines.is_truncated();
        self.update_width(width);

        const SIZES_CAP: usize = 8;
        if self.sizes.len() == SIZES_CAP {
            self.sizes.remove(0);
        }
        self.sizes.push((width, self.current_size()));
    }

    fn current_size(&self) -> Vec2 {
        Vec2::new(self.width.unwrap_or(0), self.rows.len())
    }

    // The size for `width`, if we can tell without wrapping the text again.
    fn known_size(&self, width: usize) -> Option<Vec2> {
        if self.rows_fit(width) {
            return Some(self.current_size());
        }
        self.sizes
            .iter()
            .find(|&&(w, size)| fits(w, size.x, width))
            .map(|&(_, size)| size)
    }

    // Are `rows` what `width` would give?
    fn rows_fit(&self, width: usize) -> bool {
        self.rows_width
            .is_some_and(|w| fits(w, self.width.unwrap_or(0), width))
    }
}

// Where the last line of `text` starts: (span, byte offset in that span).
//
// That's right after its last line break, skipping empty spans (as iterating
// over the whole text would).
fn last_line_start(text: &StyledString) -> (usize, usize) {
    let spans = text.spans_raw();
    let span_text = |i: usize| spans[i].content.resolve(text.source());

    let Some((mut span, mut offset)) = (0..spans.len())
        .rev()
        .find_map(|i| span_text(i).rfind('\n').map(|pos| (i, pos + 1)))
    else {
        return (0, 0);
    };
    while span < spans.len() && offset >= span_text(span).len() {
        span += 1;
        offset = 0;
    }
    (span, offset)
}

// Is what was computed at width `computed` (using `used` of it) also what
// `width` would give?
//
// For the same width, or when it didn't use all the width it had (so nothing
// was wrapped) and `width` still holds it. Not when it used exactly all of
// it: a trailing space may have been dropped without the row counting as
// wrapped. Never for width 0 either, where nothing is shown at all.
fn fits(computed: usize, used: usize, width: usize) -> bool {
    computed == width || (width > 0 && used < computed && used <= width)
}

impl View for TextView {
    fn draw(&self, printer: &Printer) {
        let h = self.rows.len();
        // If the content is smaller than the view, align it somewhere.
        let offset = self.align.v.get_offset(h, printer.size.y);
        let printer = &printer.offset((0, offset));

        printer.with_style(self.style, |printer| {
            for (y, row) in self
                .rows
                .iter()
                .enumerate()
                .skip(printer.content_offset.y)
                .take(printer.output_size.y)
            {
                let l = row.width;
                let mut x = self.align.h.get_offset(l, printer.size.x);

                for span in row.resolve_stream(self.snapshot.as_ref()) {
                    printer.with_style(*span.attr, |printer| {
                        printer.print((x, y), span.content);
                        x += span.content.width();
                    });
                }
            }
        });
    }

    fn needs_relayout(&self) -> bool {
        self.laid_out_version != Some(self.content.version())
    }

    fn layout_key(&self) -> u64 {
        combine_layout_key(self.content.version(), &self.wrap)
    }

    fn required_size(&mut self, size: Vec2) -> Vec2 {
        let width = self.wrap_width(size);
        if width == 0 {
            // No room at all: we'd need at least one column, and at most one
            // row per byte (each row at width 1 holds at least one).
            // (Without updating the snapshot: `rows` must stay in sync with
            // it, and there are none to compute here.)
            let len = self.content.len();
            return Vec2::new(usize::from(len > 0), len);
        }
        self.update_snapshot();
        if let Some(size) = self.known_size(width) {
            return size;
        }

        self.compute_rows(size);
        self.current_size()
    }

    fn layout(&mut self, size: Vec2) {
        // Compute the text rows.
        self.compute_rows(size);
        self.laid_out_version = Some(self.snapshot_version);
    }
}

// Need: a name, a base (potential dependencies), setters
#[crate::blueprint(TextView::empty())]
enum Blueprint {
    // We accept `TextView` without even a body
    Empty,

    // Inline content
    Content(StyledString),

    // Full object with optional content field
    // This is also used to add a `with` block
    Object { content: Option<StyledString> },
}

#[cfg(test)]
mod tests {
    use super::TextView;
    use crate::Vec2;
    use crate::view::View;

    #[test]
    fn appended_rows_match_fresh_rows() {
        // Appending only wraps the last line again: after any sequence of
        // appends (and replacements), rows must be what wrapping everything
        // gives.
        use crate::style::Effect;
        use crate::utils::markup::StyledString;
        use crate::views::TextContent;

        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut rnd = move |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        let pieces = [
            "a",
            "word ",
            " leading",
            "\n",
            "\n\n",
            "e\u{301}",
            "中文",
            "trailing  ",
            "",
            "longer text that wraps\n",
            "\u{200b}",
            "tab\tthere",
            "x",
        ];
        let random_text = |rnd: &mut dyn FnMut(usize) -> usize| {
            let mut text = StyledString::new();
            for _ in 0..rnd(4) {
                let piece = pieces[rnd(pieces.len())];
                if rnd(3) == 0 {
                    text.append_styled(piece, Effect::Bold);
                } else {
                    text.append_plain(piece);
                }
            }
            text
        };

        for _ in 0..300 {
            let content = TextContent::new(random_text(&mut rnd));
            let mut view = TextView::new_with_content(content.clone());
            for _ in 0..30 {
                match rnd(8) {
                    0 => content.set_content(random_text(&mut rnd)),
                    1 | 2 => {}
                    _ => content.append(random_text(&mut rnd)),
                }
                let size = Vec2::new(rnd(30), 5);
                if rnd(2) == 0 {
                    view.layout(size);
                }
                let mut fresh = TextView::new(content.get_content().clone());
                assert_eq!(view.required_size(size), fresh.required_size(size));
                view.layout(size);
                fresh.layout(size);
                assert_eq!(
                    view.rows,
                    fresh.rows,
                    "{:?}",
                    content.get_content().source()
                );
            }
        }
    }

    #[test]
    fn size_cache_stays_bounded() {
        let mut view = TextView::new("some text that wraps at many widths");
        for i in 0..5000 {
            view.required_size(Vec2::new(i % 300, 5));
            if i % 7 == 0 {
                view.layout(Vec2::new(i % 300, 5));
            }
            assert!(view.sizes.len() <= 8);
        }
    }

    #[test]
    fn zero_width_keeps_rows_in_sync() {
        // Rows must match the snapshot they were computed from (`draw` uses
        // both): asking for width 0 after a change must not replace one
        // without the other.
        let mut view = TextView::new("first");
        view.layout(Vec2::new(10, 5));
        let rows_version = view.snapshot_version;

        view.set_content("second, longer text");
        assert_eq!(view.required_size(Vec2::new(0, 5)).x, 1);
        assert_eq!(view.snapshot_version, rows_version);
    }

    #[test]
    fn zero_width_needs_a_column() {
        // Not "nothing": parents measuring their minimum size through
        // decorations reach width 0, and must not think the text is free.
        let size = TextView::new("abc def").required_size(Vec2::new(0, 5));
        assert_eq!(size.x, 1);
        assert!(size.y >= TextView::new("abc def").required_size(Vec2::new(1, 5)).y);
        assert_eq!(
            TextView::new("").required_size(Vec2::new(0, 5)),
            Vec2::zero()
        );
    }

    #[test]
    fn reused_rows_match_fresh_rows() {
        // A view measured at one width, then another, must answer like a
        // view measured at the second width directly: reusing rows across
        // widths must not change anything. Covers trailing spaces (dropped
        // when they don't fit, without counting as wrapped) and characters
        // wider than the whole line (left out).
        let words = [
            "a",
            "ab ",
            " ",
            "few words",
            "中",
            "中文字",
            "a中",
            "🦀",
            "e\u{301}",
            "\u{200b}",
            "日本 語",
            "x\n中",
            "",
        ];
        for a in words {
            for b in words {
                for c in words {
                    let text = format!("{a} {b}{c}");
                    for w0 in 0..14 {
                        let mut view = TextView::new(text.clone());
                        view.required_size(Vec2::new(w0, 10));
                        for w in 0..14 {
                            let fresh = TextView::new(text.clone()).required_size(Vec2::new(w, 10));
                            let reused = view.required_size(Vec2::new(w, 10));
                            assert_eq!(reused, fresh, "{text:?}: width {w0} then {w}");
                            // Go back, so the next width is also reached from `w0`.
                            view.required_size(Vec2::new(w0, 10));
                        }
                    }
                }
            }
        }
    }
}
