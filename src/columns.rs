/// Layout direction for [`format`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// Fill down each column before moving right.
    Column,
    /// Fill across each row before moving down.
    Row,
    /// Print one item per line.
    Plain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DenseLayout {
    rows: usize,
    cols: usize,
    widths: Vec<usize>,
}

/// Options controlling [`format`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options<'a> {
    /// Layout direction.
    pub layout: Layout,
    /// Use per-column widths and try to reduce row count, like `git column --mode=...,dense`.
    pub dense: bool,
    /// Maximum output width, including indentation.
    pub width: usize,
    /// Minimum spaces between columns.
    pub padding: usize,
    /// Prefix printed before each output line.
    pub indent: &'a str,
    /// Suffix printed after each output line.
    pub line_ending: &'a str,
}

fn compute_column_widths(
    item_widths: &[usize],
    layout: Layout,
    rows: usize,
    cols: usize,
) -> Vec<usize> {
    let mut widths = vec![0; cols];

    for (x, column_width) in widths.iter_mut().enumerate() {
        for y in 0..rows {
            let item_index = linear_index(layout, rows, cols, x, y);
            if let Some(width) = item_widths.get(item_index) {
                *column_width = (*column_width).max(*width);
            }
        }
    }

    widths
}

fn display_width(value: &str) -> usize {
    console::measure_text_width(value)
}

/// Format a list of cells using the same row/column rules as `git column`.
///
/// This intentionally mirrors Git's small column formatter instead of shelling
/// out to `git column`: non-dense mode uses a single global cell width, while
/// dense mode shrinks the row count using per-column widths.
#[must_use]
pub fn format<S: AsRef<str>>(items: &[S], options: Options<'_>) -> String {
    if items.is_empty() {
        return String::new();
    }

    if options.layout == Layout::Plain {
        return format_plain(items, options);
    }

    let item_widths = items
        .iter()
        .map(|item| display_width(item.as_ref()))
        .collect::<Vec<_>>();
    let max_item_width = item_widths.iter().copied().max().unwrap_or(0);
    let initial_width = max_item_width + options.padding;
    let indent_width = display_width(options.indent);
    let available_width = options.width.saturating_sub(indent_width);

    let mut cols = available_width
        .checked_div(initial_width)
        .unwrap_or(items.len())
        .max(1)
        .min(items.len());
    let mut rows = items.len().div_ceil(cols);
    let mut column_widths = None;

    if options.dense {
        let dense = shrink_columns(&item_widths, options, rows, cols);
        rows = dense.rows;
        cols = dense.cols;
        column_widths = Some(dense.widths);
    }

    format_table(
        items,
        &item_widths,
        options,
        rows,
        cols,
        initial_width,
        column_widths.as_deref(),
    )
}

/// Format with options matching the current `git column` call site.
#[must_use]
pub fn format_git<S: AsRef<str>>(items: &[S], indent: &str, width: usize) -> String {
    format(items, Options::git(indent, width))
}

fn format_plain<S: AsRef<str>>(items: &[S], options: Options<'_>) -> String {
    let mut output = String::new();

    for item in items {
        output.push_str(options.indent);
        output.push_str(item.as_ref());
        output.push_str(options.line_ending);
    }

    output
}

fn format_table<S: AsRef<str>>(
    items: &[S],
    item_widths: &[usize],
    options: Options<'_>,
    rows: usize,
    cols: usize,
    initial_width: usize,
    column_widths: Option<&[usize]>,
) -> String {
    let mut output = String::new();

    for y in 0..rows {
        for x in 0..cols {
            let item_index = linear_index(options.layout, rows, cols, x, y);
            if item_index >= items.len() {
                break;
            }

            if x == 0 {
                output.push_str(options.indent);
            }

            output.push_str(items[item_index].as_ref());

            if is_last_cell_in_row(options.layout, item_index, items.len(), rows, cols, x) {
                output.push_str(options.line_ending);
            } else {
                let target_width = column_widths.map_or(initial_width, |widths| {
                    widths.get(x).copied().unwrap_or(0) + options.padding
                });
                push_spaces(
                    &mut output,
                    target_width.saturating_sub(item_widths[item_index]),
                );
            }
        }
    }

    output
}

const fn is_last_cell_in_row(
    layout: Layout,
    item_index: usize,
    item_count: usize,
    rows: usize,
    cols: usize,
    x: usize,
) -> bool {
    match layout {
        Layout::Column => item_index + rows >= item_count,
        Layout::Row => x == cols - 1 || item_index == item_count - 1,
        Layout::Plain => unreachable!(),
    }
}

const fn linear_index(layout: Layout, rows: usize, cols: usize, x: usize, y: usize) -> usize {
    match layout {
        Layout::Column => x * rows + y,
        Layout::Row => y * cols + x,
        Layout::Plain => unreachable!(),
    }
}

fn push_spaces(output: &mut String, count: usize) {
    output.extend(std::iter::repeat_n(' ', count));
}

fn shrink_columns(
    item_widths: &[usize],
    options: Options<'_>,
    mut rows: usize,
    mut cols: usize,
) -> DenseLayout {
    let indent_width = display_width(options.indent);

    while rows > 1 {
        let previous_rows = rows;
        let previous_cols = cols;

        rows -= 1;
        cols = item_widths
            .len()
            .div_ceil(rows)
            .max(1)
            .min(item_widths.len());

        let candidate_widths = compute_column_widths(item_widths, options.layout, rows, cols);
        let total_width = indent_width
            + candidate_widths.iter().sum::<usize>()
            + options.padding.saturating_mul(cols);

        if total_width > options.width {
            rows = previous_rows;
            cols = previous_cols;
            break;
        }
    }

    DenseLayout {
        rows,
        cols,
        widths: compute_column_widths(item_widths, options.layout, rows, cols),
    }
}

impl<'a> Options<'a> {
    /// Options matching `git column --mode=always --indent=<indent> --width=<width>`.
    #[must_use]
    pub const fn git(indent: &'a str, width: usize) -> Self {
        Self {
            layout: Layout::Column,
            dense: false,
            width,
            padding: 1,
            indent,
            line_ending: "\n",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ITEMS: &[&str] = &[
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven",
    ];

    const fn options(layout: Layout, width: usize) -> Options<'static> {
        Options {
            layout,
            dense: false,
            width,
            padding: 1,
            indent: "",
            line_ending: "\n",
        }
    }

    #[test]
    fn empty_list_is_empty() {
        assert_eq!(format::<&str>(&[], Options::git("  ", 80)), "");
    }

    #[test]
    fn plain_layout_prints_one_indented_item_per_line() {
        let opts = Options {
            layout: Layout::Plain,
            dense: false,
            width: 80,
            padding: 1,
            indent: "Z",
            line_ending: "\n",
        };

        assert_eq!(
            format(ITEMS, opts),
            "Zone\nZtwo\nZthree\nZfour\nZfive\nZsix\nZseven\nZeight\nZnine\nZten\nZeleven\n"
        );
    }

    #[test]
    fn column_layout_fits_eighty_columns_like_git() {
        assert_eq!(
            format(ITEMS, options(Layout::Column, 80)),
            "one    two    three  four   five   six    seven  eight  nine   ten    eleven\n"
        );
    }

    #[test]
    fn column_layout_width_one_falls_back_to_one_per_line() {
        assert_eq!(
            format(ITEMS, options(Layout::Column, 1)),
            "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\n"
        );
    }

    #[test]
    fn column_layout_width_twenty_matches_git() {
        assert_eq!(
            format(ITEMS, options(Layout::Column, 20)),
            "one    seven\ntwo    eight\nthree  nine\nfour   ten\nfive   eleven\nsix\n"
        );
    }

    #[test]
    fn column_layout_width_twenty_padding_two_matches_git() {
        let opts = Options {
            padding: 2,
            ..options(Layout::Column, 20)
        };

        assert_eq!(
            format(ITEMS, opts),
            "one     seven\ntwo     eight\nthree   nine\nfour    ten\nfive    eleven\nsix\n"
        );
    }

    #[test]
    fn column_layout_width_twenty_indented_matches_git() {
        let opts = Options {
            indent: "  ",
            ..options(Layout::Column, 20)
        };

        assert_eq!(
            format(ITEMS, opts),
            "  one    seven\n  two    eight\n  three  nine\n  four   ten\n  five   eleven\n  six\n"
        );
    }

    #[test]
    fn dense_column_layout_width_twenty_matches_git() {
        let opts = Options {
            dense: true,
            ..options(Layout::Column, 20)
        };

        assert_eq!(
            format(ITEMS, opts),
            "one   five  nine\ntwo   six   ten\nthree seven eleven\nfour  eight\n"
        );
    }

    #[test]
    fn row_layout_width_twenty_matches_git() {
        assert_eq!(
            format(ITEMS, options(Layout::Row, 20)),
            "one    two\nthree  four\nfive   six\nseven  eight\nnine   ten\neleven\n"
        );
    }

    #[test]
    fn dense_row_layout_width_twenty_matches_git() {
        let opts = Options {
            dense: true,
            ..options(Layout::Row, 20)
        };

        assert_eq!(
            format(ITEMS, opts),
            "one   two    three\nfour  five   six\nseven eight  nine\nten   eleven\n"
        );
    }

    #[test]
    fn zero_padding_keeps_adjacent_columns_adjacent() {
        let opts = Options {
            padding: 0,
            ..options(Layout::Column, 10)
        };

        assert_eq!(format(&["a", "b"], opts), "ab\n");
    }

    #[test]
    fn long_items_are_not_truncated() {
        assert_eq!(
            format(&["abcdef", "g"], options(Layout::Column, 3)),
            "abcdef\ng\n"
        );
    }

    #[test]
    fn format_git_matches_current_call_with_actual_cells() {
        let languages = ["rust", "ruby", "json", "typescript", "python"];

        assert_eq!(
            format_git(&languages, "  ", 80),
            "  rust       ruby       json       typescript python\n"
        );
    }
}
