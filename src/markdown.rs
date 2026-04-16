use std::sync::OnceLock;

use crossterm::style::{Attribute, Attributes, Color};
use ratatui::style::Style;
use ratatui::text::Span;
use termimad::{
    CompoundStyle, FmtComposite, FmtLine, FmtTableRow, FmtTableRule, MadSkin, RelativePosition,
};

static SKIN: OnceLock<MadSkin> = OnceLock::new();

fn get_skin() -> &'static MadSkin {
    SKIN.get_or_init(MadSkin::default)
}

fn convert_color(c: Color) -> ratatui::style::Color {
    match c {
        Color::Reset => ratatui::style::Color::Reset,
        Color::Black => ratatui::style::Color::Black,
        Color::DarkGrey => ratatui::style::Color::DarkGray,
        Color::Red => ratatui::style::Color::Red,
        Color::DarkRed => ratatui::style::Color::LightRed,
        Color::Green => ratatui::style::Color::Green,
        Color::DarkGreen => ratatui::style::Color::LightGreen,
        Color::Yellow => ratatui::style::Color::Yellow,
        Color::DarkYellow => ratatui::style::Color::LightYellow,
        Color::Blue => ratatui::style::Color::Blue,
        Color::DarkBlue => ratatui::style::Color::LightBlue,
        Color::Magenta => ratatui::style::Color::Magenta,
        Color::DarkMagenta => ratatui::style::Color::LightMagenta,
        Color::Cyan => ratatui::style::Color::Cyan,
        Color::DarkCyan => ratatui::style::Color::LightCyan,
        Color::White => ratatui::style::Color::White,
        Color::Grey => ratatui::style::Color::Gray,
        Color::Rgb { r, g, b } => ratatui::style::Color::Rgb(r, g, b),
        Color::AnsiValue(v) => ratatui::style::Color::Indexed(v),
    }
}

fn convert_attributes(attrs: Attributes) -> ratatui::style::Modifier {
    let mut mod_val = ratatui::style::Modifier::empty();

    if attrs.has(Attribute::Bold) {
        mod_val |= ratatui::style::Modifier::BOLD;
    }
    if attrs.has(Attribute::Italic) {
        mod_val |= ratatui::style::Modifier::ITALIC;
    }
    if attrs.has(Attribute::Underlined) {
        mod_val |= ratatui::style::Modifier::UNDERLINED;
    }
    if attrs.has(Attribute::CrossedOut) {
        mod_val |= ratatui::style::Modifier::CROSSED_OUT;
    }
    if attrs.has(Attribute::Dim) {
        mod_val |= ratatui::style::Modifier::DIM;
    }
    if attrs.has(Attribute::Reverse) {
        mod_val |= ratatui::style::Modifier::REVERSED;
    }

    mod_val
}

fn compound_style_to_ratatui(cs: &CompoundStyle) -> Style {
    let mut style = Style::default();

    if let Some(fg) = cs.object_style.foreground_color {
        style = style.fg(convert_color(fg));
    }
    if let Some(bg) = cs.object_style.background_color {
        style = style.bg(convert_color(bg));
    }

    let mods = convert_attributes(cs.object_style.attributes);
    if !mods.is_empty() {
        style = style.add_modifier(mods);
    }

    style
}

fn styled_char_to_span(sc: &termimad::StyledChar) -> Span<'static> {
    let style = compound_style_to_ratatui(sc.compound_style());
    Span::styled(sc.nude_char().to_string(), style)
}

fn fmt_composite_to_spans(fc: &FmtComposite, skin: &MadSkin) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let ls = skin.line_style(fc.kind);

    let (lpi, rpi) = fc.completions();
    if lpi > 0 {
        spans.push(Span::raw(" ".repeat(lpi)));
    }

    if let termimad::CompositeKind::ListItem(depth) = fc.kind {
        let indent = " ".repeat(depth as usize);
        if !indent.is_empty() {
            let indent_style = compound_style_to_ratatui(&skin.paragraph.compound_style);
            spans.push(Span::styled(indent, indent_style));
        }
        spans.push(styled_char_to_span(&skin.bullet));
        spans.push(Span::raw(" "));
    }

    if skin.list_items_indentation_mode == termimad::ListItemsIndentationMode::Block
        && let termimad::CompositeKind::ListItemFollowUp(depth) = fc.kind
    {
        let indent = " ".repeat(depth as usize + 2);
        if !indent.is_empty() {
            let indent_style = compound_style_to_ratatui(&skin.paragraph.compound_style);
            spans.push(Span::styled(indent, indent_style));
        }
    }

    if fc.kind == termimad::CompositeKind::Quote {
        spans.push(styled_char_to_span(&skin.quote_mark));
        spans.push(Span::raw(" "));
    }

    for compound in &fc.compounds {
        let cs = skin.compound_style(ls, compound);
        let style = compound_style_to_ratatui(&cs);
        spans.push(Span::styled(compound.as_str().to_string(), style));
    }

    if rpi > 0 {
        spans.push(Span::raw(" ".repeat(rpi)));
    }

    spans
}

fn table_row_to_spans(row: &FmtTableRow, skin: &MadSkin) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let border_style = compound_style_to_ratatui(&skin.table.compound_style);
    let v_char = skin.table_border_chars.vertical;

    spans.push(Span::styled(v_char.to_string(), border_style));

    for cell in &row.cells {
        let cell_spans = fmt_composite_to_spans(cell, skin);
        spans.extend(cell_spans);
        spans.push(Span::styled(v_char.to_string(), border_style));
    }

    spans
}

fn table_rule_to_spans(rule: &FmtTableRule, skin: &MadSkin) -> Vec<Span<'static>> {
    let border_style = compound_style_to_ratatui(&skin.table.compound_style);
    let chars = skin.table_border_chars;

    let left = match rule.position {
        RelativePosition::Top => chars.top_left_corner,
        RelativePosition::Other => chars.left_junction,
        RelativePosition::Bottom => chars.bottom_left_corner,
    };

    let mid = match rule.position {
        RelativePosition::Top => chars.top_junction,
        RelativePosition::Other => chars.cross,
        RelativePosition::Bottom => chars.bottom_junction,
    };

    let right = match rule.position {
        RelativePosition::Top => chars.top_right_corner,
        RelativePosition::Other => chars.right_junction,
        RelativePosition::Bottom => chars.bottom_right_corner,
    };

    let mut line = String::new();
    line.push(left);

    for (i, &w) in rule.widths.iter().enumerate() {
        if i > 0 {
            line.push(mid);
        }
        for _ in 0..w {
            line.push(chars.horizontal);
        }
    }

    line.push(right);

    vec![Span::styled(line, border_style)]
}

pub fn md_body_to_spans(body: &str, width: usize) -> Vec<Vec<Span<'static>>> {
    let skin = get_skin();
    let text = skin.text(body, Some(width));
    let render_width = text.width.unwrap_or(width);

    let mut result = Vec::new();

    for line in &text.lines {
        match line {
            FmtLine::Normal(fc) => {
                let spans = fmt_composite_to_spans(fc, skin);
                if !spans.is_empty() {
                    result.push(spans);
                } else {
                    result.push(vec![Span::raw("")]);
                }
            }
            FmtLine::TableRow(row) => {
                result.push(table_row_to_spans(row, skin));
            }
            FmtLine::TableRule(rule) => {
                result.push(table_rule_to_spans(rule, skin));
            }
            FmtLine::HorizontalRule => {
                let hr_span = styled_char_to_span(&skin.horizontal_rule);
                let hr_str: String =
                    std::iter::repeat_n(skin.horizontal_rule.nude_char(), render_width).collect();
                result.push(vec![Span::styled(hr_str, hr_span.style)]);
            }
        }
    }

    if result.is_empty() {
        result.push(vec![Span::raw("")]);
    }

    result
}
