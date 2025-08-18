use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Span;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear};

// Help popup drawing (F1)
/// Draw the F1 Help popup with multiline content and a cyan border + shadow.
pub fn draw_help_popup(f: &mut ratatui::Frame<'_>, size: Rect) {
    // Build help text lines (multiline with indentation)
    let lines = vec![
        Line::from(Span::raw(format!("{} v {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")))),
        Line::from(Span::raw(format!("© {} {}", env!("RTOP_COPYRIGHT_YEAR"), env!("CARGO_PKG_AUTHORS")))), 
        Line::from(Span::raw(format!("License: {}", env!("CARGO_PKG_LICENSE")))),
        Line::from(Span::raw(" ")),
        Line::from(Span::raw("Navigation and hotkeys:")),
        Line::from(Span::raw("    - Switch tabs: Left/Right, h/l, Tab/BackTab, or 1/2/3/4/5/6.")),
        Line::from(Span::raw("    - In tables (Processes/Services/Logs/Journal): Home/End jump to first/last; PgUp/PgDn move by 10.")),
        Line::from(Span::raw("    - F2 Dashboard, F3 top/htop, F4 Services (SystemD), F5 Logs, F6 Journal, F12 Shell (embedded PTY).")), 
        Line::from(Span::raw("    - In Shell tab: keys go to your shell; Ctrl-C is sent to the shell (F10 exits app).")), 
        Line::from(Span::raw("    - F10 exit app; q also exits.")), 
    ];

    // Compute popup width: max text width + 1 space padding + 4 (borders)
    let max_text_width: u16 = lines
        .iter()
        .map(|l| l.width() as u16)
        .max()
        .unwrap_or(0)
        .saturating_add(1); // left pad of 1 space for all rows
    let mut popup_w: u16 = max_text_width.saturating_add(4);
    if popup_w > size.width { popup_w = size.width; }

    // Dynamic height based on number of lines (+2 for borders)
    let mut popup_h: u16 = (lines.len() as u16).saturating_add(2);
    if popup_h > size.height { popup_h = size.height; }

    // Center the popup
    let popup_x = size.x + (size.width.saturating_sub(popup_w)) / 2;
    let popup_y = size.y + (size.height.saturating_sub(popup_h)) / 2;
    let area = Rect { x: popup_x, y: popup_y, width: popup_w, height: popup_h };

    // Draw shadow first: offset by (1,1), clamped to screen
    let sx = area.x.saturating_add(1);
    let sy = area.y.saturating_add(1);
    if sx < size.x + size.width && sy < size.y + size.height {
        let sw = area.width.min((size.x + size.width).saturating_sub(sx));
        let sh = area.height.min((size.y + size.height).saturating_sub(sy));
        if sw > 0 && sh > 0 {
            let shadow = Rect { x: sx, y: sy, width: sw, height: sh };
            let shadow_block = Block::default().style(Style::default().bg(Color::Black));
            f.render_widget(shadow_block, shadow);
        }
    }

    // Clear and border for the main popup
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Help ")
        .border_style(Style::default().fg(Color::Cyan));
    f.render_widget(block, area);

    // Inner text area
    let inner = Rect { x: area.x + 1, y: area.y + 1, width: area.width.saturating_sub(2), height: area.height.saturating_sub(2) };
    // Prepend one leading space to every rendered line
    let padded_lines: Vec<Line> = lines
        .into_iter()
        .map(|l| {
            let mut spans = vec![Span::raw(" ")];
            spans.extend(l.spans);
            Line::from(spans)
        })
        .collect();
    let paragraph = ratatui::widgets::Paragraph::new(padded_lines);
    f.render_widget(paragraph, inner);
}

/// Draw the Service Details popup with a dynamic title and multi-line body text.
pub fn draw_service_popup(f: &mut ratatui::Frame<'_>, size: Rect, title: &str, text: &str) {
    // Split text into lines and measure width
    let lines_raw: Vec<&str> = text.split('\n').collect();
    let max_text_width: u16 = lines_raw.iter().map(|l| l.chars().count() as u16).max().unwrap_or(0).saturating_add(1);
    // Add some padding and clamp to screen bounds
    let mut popup_w: u16 = max_text_width.saturating_add(4);
    if popup_w > size.width { popup_w = size.width; }
    // Height is based on number of lines (capped to screen)
    let mut popup_h: u16 = (lines_raw.len() as u16).saturating_add(2);
    if popup_h > size.height { popup_h = size.height; }

    // Center the popup
    let popup_x = size.x + (size.width.saturating_sub(popup_w)) / 2;
    let popup_y = size.y + (size.height.saturating_sub(popup_h)) / 2;
    let area = Rect { x: popup_x, y: popup_y, width: popup_w, height: popup_h };

    // Shadow
    let sx = area.x.saturating_add(1);
    let sy = area.y.saturating_add(1);
    if sx < size.x + size.width && sy < size.y + size.height {
        let sw = area.width.min((size.x + size.width).saturating_sub(sx));
        let sh = area.height.min((size.y + size.height).saturating_sub(sy));
        if sw > 0 && sh > 0 {
            let shadow = Rect { x: sx, y: sy, width: sw, height: sh };
            let shadow_block = Block::default().style(Style::default().bg(Color::Black));
            f.render_widget(shadow_block, shadow);
        }
    }

    // Border and clear
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Service: {} ", title))
        .border_style(Style::default().fg(Color::Green));
    f.render_widget(block, area);

    // Inner text area
    let inner = Rect { x: area.x + 1, y: area.y + 1, width: area.width.saturating_sub(2), height: area.height.saturating_sub(2) };
    let lines: Vec<Line> = lines_raw.into_iter().map(|l| Line::from(Span::raw(format!(" {}", l)))).collect();
    let paragraph = ratatui::widgets::Paragraph::new(lines);
    f.render_widget(paragraph, inner);
}

/// Draw the Process Details popup with a dynamic title and multi-line body text.
pub fn draw_process_popup(f: &mut ratatui::Frame<'_>, size: Rect, title: &str, text: &str) {
    // Split text into lines and measure width
    let lines_raw: Vec<&str> = text.split('\n').collect();
    let max_text_width: u16 = lines_raw.iter().map(|l| l.chars().count() as u16).max().unwrap_or(0).saturating_add(1);
    // Add some padding and clamp to screen bounds
    let mut popup_w: u16 = max_text_width.saturating_add(4);
    if popup_w > size.width { popup_w = size.width; }
    // Height is based on number of lines (capped to screen)
    let mut popup_h: u16 = (lines_raw.len() as u16).saturating_add(2);
    if popup_h > size.height { popup_h = size.height; }

    // Center the popup
    let popup_x = size.x + (size.width.saturating_sub(popup_w)) / 2;
    let popup_y = size.y + (size.height.saturating_sub(popup_h)) / 2;
    let area = Rect { x: popup_x, y: popup_y, width: popup_w, height: popup_h };

    // Shadow
    let sx = area.x.saturating_add(1);
    let sy = area.y.saturating_add(1);
    if sx < size.x + size.width && sy < size.y + size.height {
        let sw = area.width.min((size.x + size.width).saturating_sub(sx));
        let sh = area.height.min((size.y + size.height).saturating_sub(sy));
        if sw > 0 && sh > 0 {
            let shadow = Rect { x: sx, y: sy, width: sw, height: sh };
            let shadow_block = Block::default().style(Style::default().bg(Color::Black));
            f.render_widget(shadow_block, shadow);
        }
    }

    // Border and clear
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Process: {} ", title))
        .border_style(Style::default().fg(Color::Yellow));
    f.render_widget(block, area);

    // Inner text area
    let inner = Rect { x: area.x + 1, y: area.y + 1, width: area.width.saturating_sub(2), height: area.height.saturating_sub(2) };
    let lines: Vec<Line> = lines_raw.into_iter().map(|l| Line::from(Span::raw(format!(" {}", l)))).collect();
    let paragraph = ratatui::widgets::Paragraph::new(lines);
    f.render_widget(paragraph, inner);
}

/// Draw the Log Details popup with a dynamic title and multi-line body text.
pub fn draw_log_popup(f: &mut ratatui::Frame<'_>, size: Rect, title: &str, text: &str, start_offset: usize) {
    // Split text into lines and measure width
    let lines_raw: Vec<&str> = text.split('\n').collect();
    let max_text_width: u16 = lines_raw.iter().map(|l| l.chars().count() as u16).max().unwrap_or(0).saturating_add(1);
    let mut popup_w: u16 = max_text_width.saturating_add(4);
    if popup_w > size.width { popup_w = size.width; }
    let mut popup_h: u16 = (lines_raw.len() as u16).saturating_add(2);
    if popup_h > size.height { popup_h = size.height; }
    let popup_x = size.x + (size.width.saturating_sub(popup_w)) / 2;
    let popup_y = size.y + (size.height.saturating_sub(popup_h)) / 2;
    let area = Rect { x: popup_x, y: popup_y, width: popup_w, height: popup_h };
    let sx = area.x.saturating_add(1);
    let sy = area.y.saturating_add(1);
    if sx < size.x + size.width && sy < size.y + size.height {
        let sw = area.width.min((size.x + size.width).saturating_sub(sx));
        let sh = area.height.min((size.y + size.height).saturating_sub(sy));
        if sw > 0 && sh > 0 { let shadow = Rect { x: sx, y: sy, width: sw, height: sh }; let shadow_block = Block::default().style(Style::default().bg(Color::Black)); f.render_widget(shadow_block, shadow); }
    }
    f.render_widget(Clear, area);
    let block = Block::default().borders(Borders::ALL).title(format!(" Log: {} ", title)).border_style(Style::default().fg(Color::LightBlue));
    f.render_widget(block, area);
    let inner = Rect { x: area.x + 1, y: area.y + 1, width: area.width.saturating_sub(2), height: area.height.saturating_sub(2) };
    // Determine visible window based on start_offset and inner height
    let total = lines_raw.len();
    let vis_rows = inner.height as usize;
    let max_start = total.saturating_sub(vis_rows);
    let start = start_offset.min(max_start);
    let end = start.saturating_add(vis_rows).min(total);
    let lines: Vec<Line> = lines_raw
        .into_iter()
        .skip(start)
        .take(end.saturating_sub(start))
        .map(|l| Line::from(Span::raw(format!(" {}", l))))
        .collect();
    let paragraph = ratatui::widgets::Paragraph::new(lines);
    f.render_widget(paragraph, inner);
}

/// Draw a sudo password prompt popup for Logs, with masked input and error line.
pub fn draw_logs_password_prompt(f: &mut ratatui::Frame<'_>, size: Rect, error_text: &str, chars_len: usize) {
    let lines = vec![
        Line::from(Span::raw("Enter sudo password to read protected logs:")),
        Line::from(Span::raw(" ")), // spacer
        Line::from(Span::raw("Password: ")), // input line label
        Line::from(Span::raw(" ")), // spacer
        Line::from(Span::styled(error_text.to_string(), Style::default().fg(Color::Red))),
    ];
    // Fixed width popup
    let mut popup_w: u16 = 60;
    if popup_w > size.width { popup_w = size.width; }
    let mut popup_h: u16 = 7;
    if popup_h > size.height { popup_h = size.height; }
    let popup_x = size.x + (size.width.saturating_sub(popup_w)) / 2;
    let popup_y = size.y + (size.height.saturating_sub(popup_h)) / 2;
    let area = Rect { x: popup_x, y: popup_y, width: popup_w, height: popup_h };
    let sx = area.x.saturating_add(1);
    let sy = area.y.saturating_add(1);
    if sx < size.x + size.width && sy < size.y + size.height {
        let sw = area.width.min((size.x + size.width).saturating_sub(sx));
        let sh = area.height.min((size.y + size.height).saturating_sub(sy));
        if sw > 0 && sh > 0 { let shadow = Rect { x: sx, y: sy, width: sw, height: sh }; let shadow_block = Block::default().style(Style::default().bg(Color::Black)); f.render_widget(shadow_block, shadow); }
    }
    f.render_widget(Clear, area);
    let block = Block::default().borders(Borders::ALL).title(" Sudo Password ").border_style(Style::default().fg(Color::Magenta));
    f.render_widget(block, area);
    // Inner
    let inner = Rect { x: area.x + 1, y: area.y + 1, width: area.width.saturating_sub(2), height: area.height.saturating_sub(2) };
    let mut display_lines: Vec<Line> = Vec::new();
    display_lines.push(lines[0].clone());
    display_lines.push(lines[1].clone());
    // Render password masked line
    let masked = "*".repeat(chars_len);
    display_lines.push(Line::from(Span::raw(format!("Password: {}", masked))));
    display_lines.push(lines[3].clone());
    display_lines.push(lines[4].clone());
    let paragraph = ratatui::widgets::Paragraph::new(display_lines);
    f.render_widget(paragraph, inner);
}
