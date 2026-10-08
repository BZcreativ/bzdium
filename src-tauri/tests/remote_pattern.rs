//! The service-webview capability grants `allow-report-title` to remote URLs
//! via the URLPattern `"https://*"` (capabilities/service-webviews.json).
//! Tauri parses that string with its `RemoteUrlPattern::from_str`
//! (tauri-utils src/acl/mod.rs), which post-processes the pattern before
//! handing it to the `urlpattern` crate. This test replicates that exact
//! pipeline and pins the behavior the badge feature depends on: every https
//! service host must match, non-https must not.

use urlpattern::{UrlPattern, UrlPatternInit, UrlPatternMatchInput};

/// Mirror of tauri_utils::acl::RemoteUrlPattern::from_str.
fn tauri_remote_pattern(s: &str) -> UrlPattern {
    let mut init =
        UrlPatternInit::parse_constructor_string::<regex::Regex>(s, None).expect("parses");
    if init.search.as_ref().map(|p| p.is_empty()).unwrap_or(true) {
        init.search.replace("*".to_string());
    }
    if init.hash.as_ref().map(|p| p.is_empty()).unwrap_or(true) {
        init.hash.replace("*".to_string());
    }
    if init
        .pathname
        .as_ref()
        .map(|p| p.is_empty() || p == "/")
        .unwrap_or(true)
    {
        init.pathname.replace("*".to_string());
    }
    UrlPattern::parse(init, Default::default()).expect("pattern must be valid")
}

fn matches(pattern: &str, url: &str) -> bool {
    let p = tauri_remote_pattern(pattern);
    let u = url::Url::parse(url).expect("url must parse");
    p.test(UrlPatternMatchInput::Url(u))
        .expect("test must not error")
}

#[test]
fn https_wildcard_matches_all_service_hosts() {
    let hosts = [
        "https://web.whatsapp.com/",
        "https://web.telegram.org/",
        "https://discord.com/app",
        "https://app.slack.com/",
        "https://mail.google.com/",
        "https://outlook.live.com/",
        "https://teams.microsoft.com/",
        "https://app.element.io/",
        "https://x.com/",
        "https://www.linkedin.com/",
        "https://sub.deep.very-long-domain.example.com/path?q=1",
    ];
    for url in hosts {
        assert!(
            matches("https://*", url),
            "pattern https://* must match {url}"
        );
    }
}

#[test]
fn https_wildcard_rejects_non_https() {
    assert!(!matches("https://*", "http://web.whatsapp.com/"));
}
