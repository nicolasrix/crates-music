//! Settings (section 8): a grouped form — Playback / Storage / Autoplay /
//! Account / Admin. Interactive rows show `label … value`; the selected one is
//! highlighted. `enter` cycles/flips/triggers, `h`/`l` adjust numbers. The
//! account card + group headers are rendered inline but aren't selectable
//! (the cursor walks `App::settings_rows` only — same list the reducer uses).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::super::state::{App, SettingRow};
use super::super::theme::Theme;
use super::super::widgets::human_bytes;

pub(crate) fn draw(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let rows = app.settings_rows();
    let selected = app.settings.table.selected();
    let mut lines: Vec<Line> = Vec::new();

    for (i, row) in rows.iter().enumerate() {
        // Group headers (and the account card) are injected before the first
        // row of each group.
        if let Some(header) = group_before(*row) {
            if !lines.is_empty() {
                lines.push(Line::from(""));
            }
            lines.push(Line::from(Span::styled(header, theme.accent)));
            if *row == SettingRow::SignOut {
                account_card(app, theme, &mut lines);
            }
        }
        lines.push(row_line(app, *row, Some(i) == selected, theme));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " enter: change · h/l: adjust · esc: back",
        theme.dim,
    )));

    f.render_widget(Paragraph::new(lines), area);
}

/// The group header that precedes `row`, or `None` if it's mid-group.
fn group_before(row: SettingRow) -> Option<&'static str> {
    match row {
        SettingRow::StreamQuality => Some(" Playback"),
        SettingRow::RegularBudget => Some(" Storage"),
        SettingRow::AutoplayEnabled => Some(" Autoplay"),
        SettingRow::SignOut => Some(" Account"),
        SettingRow::InvalidateCache => Some(" Admin"),
        _ => None,
    }
}

/// The non-interactive identity lines under the Account header.
fn account_card(app: &App, theme: &Theme, lines: &mut Vec<Line>) {
    match &app.whoami {
        Some(w) => {
            lines.push(Line::from(Span::styled(
                format!("   {}  ({})", w.label(), w.role),
                theme.text,
            )));
        }
        None => lines.push(Line::from(Span::styled(
            "   direct mode (no gateway account)",
            theme.dim,
        ))),
    }
    if let Some(url) = &app.server_url {
        lines.push(Line::from(Span::styled(format!("   server: {url}"), theme.dim)));
    }
}

/// One interactive row: `▸ label … value`, highlighted when selected.
fn row_line<'a>(app: &App, row: SettingRow, is_sel: bool, theme: &Theme) -> Line<'a> {
    let marker = if is_sel { "▸ " } else { "  " };
    let label = label_for(row);
    let value = value_for(app, row);
    let style = if is_sel { theme.selected } else { theme.text };
    // Pad the label column so values line up.
    let text = if value.is_empty() {
        format!("{marker}{label}")
    } else {
        format!("{marker}{label:<26}{value}")
    };
    Line::from(Span::styled(text, style))
}

fn label_for(row: SettingRow) -> &'static str {
    match row {
        SettingRow::StreamQuality => "stream quality",
        SettingRow::DownloadQuality => "download quality",
        SettingRow::OutputDevice => "play audio on this device",
        SettingRow::RegularBudget => "regular cache budget",
        SettingRow::PinnedBudget => "pinned cache budget",
        SettingRow::AutoplayEnabled => "autoplay",
        SettingRow::MinUpcoming => "min upcoming",
        SettingRow::LeashTau => "leash radius \u{3c4}",
        SettingRow::LeashLambda => "leash strength \u{3bb}",
        SettingRow::FrontierWeight => "frontier weight \u{3b2}",
        SettingRow::FrontierDecay => "frontier decay",
        SettingRow::FrontierWindow => "frontier window",
        SettingRow::MmrLambda => "mmr \u{3bb}",
        SettingRow::ResetAutoplay => "reset autoplay to defaults",
        SettingRow::SignOut => "sign out",
        SettingRow::InvalidateCache => "invalidate gateway cache",
    }
}

fn value_for(app: &App, row: SettingRow) -> String {
    let s = &app.settings;
    let on_off = |b: bool| if b { "on" } else { "off" }.to_owned();
    match row {
        SettingRow::StreamQuality => s.stream_quality.label().to_owned(),
        SettingRow::DownloadQuality => s.download_quality.label().to_owned(),
        SettingRow::OutputDevice => on_off(app.sync.output_on),
        SettingRow::RegularBudget => human_bytes(s.regular_budget_bytes),
        SettingRow::PinnedBudget => human_bytes(s.pinned_budget_bytes),
        SettingRow::AutoplayEnabled => on_off(app.autoplay.enabled),
        SettingRow::MinUpcoming => app.autoplay.min_upcoming.to_string(),
        SettingRow::LeashTau => format!("{:.2}", s.leash_tau),
        SettingRow::LeashLambda => format!("{:.1}", s.leash_lambda),
        SettingRow::FrontierWeight => format!("{:.2}", s.frontier_weight),
        SettingRow::FrontierDecay => format!("{:.2}", s.frontier_decay),
        SettingRow::FrontierWindow => s.frontier_window.to_string(),
        SettingRow::MmrLambda => format!("{:.2}", s.mmr_lambda),
        SettingRow::ResetAutoplay | SettingRow::InvalidateCache => String::new(),
        SettingRow::SignOut => {
            if app.settings.confirm_signout {
                "press enter to confirm".to_owned()
            } else {
                String::new()
            }
        }
    }
}
