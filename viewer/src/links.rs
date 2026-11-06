//! Link helpers: hit-testing, tooltips and which web addresses may be
//! opened.

use eframe::egui::Pos2;
use k_pdf::{Link, LinkTarget};

/// The link under `p` (display points on the page). The smallest one wins
/// when links overlap.
pub fn link_at(links: &[Link], p: Pos2) -> Option<&Link> {
    links
        .iter()
        .filter(|l| {
            let [left, top, right, bottom] = l.rect;
            (left..=right).contains(&p.x) && (top..=bottom).contains(&p.y)
        })
        .min_by(|a, b| area(a).total_cmp(&area(b)))
}

fn area(l: &Link) -> f32 {
    (l.rect[2] - l.rect[0]) * (l.rect[3] - l.rect[1])
}

/// The address to hand to the browser, or `None` for schemes that are not
/// safe to open from a document (`file:`, `javascript:`, custom handlers…).
pub fn openable_url(uri: &str) -> Option<String> {
    let uri = uri.trim();
    let lower = uri.to_ascii_lowercase();
    if ["http://", "https://", "ftp://", "mailto:"].iter().any(|s| lower.starts_with(s)) {
        Some(uri.to_string())
    } else if lower.starts_with("www.") {
        Some(format!("https://{uri}"))
    } else {
        None
    }
}

/// Tooltip shown while hovering a link: links within the document are
/// followed with a double-click, web links with Ctrl+click.
pub fn tooltip(target: &LinkTarget) -> String {
    match target {
        LinkTarget::Uri(uri) => match openable_url(uri) {
            Some(url) => format!("{url}\nCtrl+click to open"),
            None => format!("{uri}\n(this kind of link can't be opened)"),
        },
        LinkTarget::Page { page, .. } => format!("Go to page {}\nDouble-click to follow", page + 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_and_mail_links_open() {
        assert_eq!(openable_url(" https://example.com/a "), Some("https://example.com/a".into()));
        assert_eq!(openable_url("HTTP://EXAMPLE.COM"), Some("HTTP://EXAMPLE.COM".into()));
        assert_eq!(openable_url("mailto:someone@example.com"), Some("mailto:someone@example.com".into()));
        assert_eq!(openable_url("www.example.com"), Some("https://www.example.com".into()));
        assert_eq!(openable_url("file:///etc/passwd"), None);
        assert_eq!(openable_url("javascript:alert(1)"), None);
        assert_eq!(openable_url("someone@example.com"), None);
    }

    #[test]
    fn innermost_link_wins() {
        let link = |rect, page| Link { rect, target: LinkTarget::Page { page, top: None } };
        let links = [link([0.0, 0.0, 100.0, 100.0], 1), link([10.0, 10.0, 20.0, 20.0], 2)];
        let hit = |x, y| link_at(&links, Pos2::new(x, y)).map(|l| l.target.clone());
        assert_eq!(hit(15.0, 15.0), Some(LinkTarget::Page { page: 2, top: None }));
        assert_eq!(hit(50.0, 50.0), Some(LinkTarget::Page { page: 1, top: None }));
        assert_eq!(hit(150.0, 50.0), None);
    }
}
