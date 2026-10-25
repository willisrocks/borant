//! The web page: one embedded file that reads only the read-only routes of
//! the spec, so that `/` cannot show a control that changes state.

/// The page that the hub serves at `/`.
const PAGE: &str = include_str!("../src/web/index.html");

/// Every API route that the page reads. The graph route is built from the
/// channel name at run time, so the page names it in two parts.
const ROUTES: [&str; 4] = ["/api/snapshot", "/api/channels", "/graph", "/api/events"];

#[test]
fn web_page_names_each_route_it_reads() {
    for route in ROUTES {
        assert!(PAGE.contains(route), "the web page does not name {route}");
    }
}

#[test]
fn web_page_is_one_self_contained_file() {
    assert!(PAGE.contains("<style>"), "the CSS is not inline");
    assert!(PAGE.contains("<script>"), "the JS is not inline");
    assert!(
        !PAGE.contains("<script src"),
        "the page must not load a script"
    );
    assert!(
        !PAGE.contains("<link rel=\"stylesheet\""),
        "the page must not load a stylesheet"
    );
    assert!(
        !PAGE.contains("<form") && !PAGE.contains("<button"),
        "the page must not hold a control that changes state"
    );
}
