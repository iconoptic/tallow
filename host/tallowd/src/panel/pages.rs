//! The HTML and CSS the panel serves. The pages are read from `app/` at build time, and
//! the Python's placeholder fills are done here. The login page has no file in `app/`,
//! so its markup is kept here as the Python keeps it.

use std::sync::OnceLock;

use super::light::PHONE_HOME;

/// The access point's network, as the settings page names it.
pub const IOT_NET: &str = "10.87.0.0/24";

const LOGIN: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>tallow</title>
<style>
  body { font-family: system-ui, sans-serif; margin: 0; background: #14120f; color: #f4efe6; }
  main { max-width: 28rem; margin: 0 auto; padding: 1.5rem; }
  input, button { width: 100%; box-sizing: border-box; font-size: 1.05rem; }
  input { margin: 0.6rem 0 1rem; padding: 0.7rem; }
  button { padding: 0.8rem; border: 0; border-radius: 0.6rem; background: #e7d7b1; color: #1c160f; }
  .err { color: #ffb4a8; }
</style>
</head>
<body>
<main>
  <h1>tallow</h1>
  <p class="err">{error}</p>
  <form method="post" action="/login">
    <label for="password">Password</label>
    <input id="password" name="password" type="password" autocomplete="current-password" required>
    <button type="submit">Unlock</button>
  </form>
</main>
</body>
</html>
"#;

const MENU: &str = include_str!("../../../../app/menu_page.html");
const CAMS: &str = include_str!("../../../../app/cams_page.html");
const SETTINGS: &str = include_str!("../../../../app/settings_page.html");
const CSS: &str = include_str!("../../../../app/panel.css");
const CONTROLS: &str = include_str!("../../../../app/h7020_page.html");
const DEVICES: &str = include_str!("../../../../app/devices_page.html");
const TRAFFIC: &str = include_str!("../../../../app/traffic_page.html");
const TIMELINE_JS: &str = include_str!("../../../../app/vendor/vis-timeline-graph2d.min.js");
const TIMELINE_CSS: &str = include_str!("../../../../app/vendor/vis-timeline-graph2d.min.css");

/// The menu tile for the devices page. Python's menu keeps only the comment, so it has no tile.
const DEVICES_TILE: &str = r#"<a class="tile" href="/devices"><strong>Devices</strong><span>What the mesh hears, and where.</span></a>"#;

/// The menu tile for the traffic page.
const TRAFFIC_TILE: &str = r#"<a class="tile" href="/traffic"><strong>Traffic</strong><span>Where addresses move, and which ones pass by.</span></a>"#;

/// The login page. `error` is the words shown above the form, or empty.
pub fn login_page(error: &str) -> String {
    LOGIN.replace("{error}", error)
}

/// The light page, with the phone-home note filled in.
pub fn controls_page() -> &'static str {
    static PAGE: OnceLock<String> = OnceLock::new();
    PAGE.get_or_init(|| CONTROLS.replace("__PHONE_HOME__", PHONE_HOME))
}

pub fn menu_page(title: &str) -> String {
    house_page(MENU, title)
        .replace("<!-- devices tile -->", DEVICES_TILE)
        .replace("<!-- traffic tile -->", TRAFFIC_TILE)
}

pub fn devices_page() -> &'static str {
    DEVICES
}

pub fn traffic_page() -> &'static str {
    TRAFFIC
}

pub fn timeline_js() -> &'static str {
    TIMELINE_JS
}

pub fn timeline_css() -> &'static str {
    TIMELINE_CSS
}

pub fn cams_page(title: &str) -> String {
    house_page(CAMS, title)
}

pub fn settings_page(title: &str) -> String {
    house_page(SETTINGS, title)
}

pub fn panel_css() -> &'static str {
    CSS
}

/// Fills the cameras' name and the access point's network, both HTML-escaped.
fn house_page(source: &str, title: &str) -> String {
    source
        .replace("__CAMS_TITLE__", &escape(title))
        .replace("__IOT_NET__", &escape(IOT_NET))
}

/// What Python's `html.escape(text, quote=True)` does.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_is_escaped_where_the_pages_show_it() {
        let page = cams_page("Cams & <co> \"here\"");
        assert!(page.contains("<h1>Cams &amp; &lt;co&gt; &quot;here&quot;</h1>"));
        assert!(!page.contains("__CAMS_TITLE__"));
        assert!(!page.contains("Cams & <co>"));
        assert!(settings_page("x").contains(IOT_NET));
        assert!(!settings_page("x").contains("__IOT_NET__"));
    }

    #[test]
    fn the_light_page_states_the_wifi_limit() {
        assert!(controls_page().contains("phones home"));
        assert!(controls_page().contains("tallow-iot"));
        assert!(!controls_page().contains("__PHONE_HOME__"));
    }

    #[test]
    fn the_login_page_shows_its_error_and_the_form() {
        let page = login_page("Wrong password.");
        assert!(page.contains("Wrong password."));
        assert!(page.contains(r#"name="password""#));
        assert!(page.contains(r#"action="/login""#));
        assert!(!login_page("").contains("Wrong password."));
    }

    #[test]
    fn the_menu_has_the_devices_tile_in_place_of_its_comment() {
        let page = menu_page("x");
        assert!(page.contains(r#"href="/devices""#));
        assert!(page.contains(r#"href="/traffic""#));
        assert!(!page.contains("<!-- devices tile -->"));
        assert!(!page.contains("<!-- traffic tile -->"));
        assert!(!devices_page().contains("innerHTML"));
        assert!(!traffic_page().contains("innerHTML"));
        assert!(!traffic_page().contains("insertAdjacentHTML"));
        assert!(!traffic_page().contains("outerHTML"));
    }

    #[test]
    fn escaping_matches_the_python() {
        assert_eq!(escape("a&b<c>d\"e'f"), "a&amp;b&lt;c&gt;d&quot;e&#x27;f");
    }
}
