//! Polite fetching of a company's own pages: robots.txt respected, one request per 5 s per
//! domain, an honest User-Agent, plain text out. Never LinkedIn.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub use reqwest::Url;

pub const USER_AGENT: &str = concat!(
    "career-compass/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/hugovangarderen-papersteel/career-compass)"
);

/// The robots.txt token we answer to.
const AGENT: &str = "career-compass";
/// Bodies beyond this are cut; page text beyond `PAGE_CHARS` too (what the model sees).
const MAX_BODY: usize = 2_000_000;
pub const PAGE_CHARS: usize = 12_000;

#[derive(Debug, PartialEq)]
pub enum FetchError {
    /// robots.txt says no (or could not be read: then we stay out).
    Robots,
    /// LinkedIn and the like: never fetched.
    Banned,
    Http(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Robots => write!(f, "robots.txt disallows this page"),
            FetchError::Banned => write!(f, "site is never fetched"),
            FetchError::Http(e) => write!(f, "{e}"),
        }
    }
}

pub struct Fetcher {
    http: reqwest::Client,
    gap: Duration,
    next: Mutex<HashMap<String, Instant>>,
    robots: Mutex<HashMap<String, Option<String>>>,
}

impl Fetcher {
    /// `gap` between two requests to one host: 5 s for company sites.
    pub fn new(gap: Duration) -> Fetcher {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .expect("http client");
        Fetcher {
            http,
            gap,
            next: Mutex::default(),
            robots: Mutex::default(),
        }
    }

    /// Waits for this host's turn. `gap` overrides the default (Nominatim: 1 s).
    async fn turn(&self, host: &str, gap: Duration) {
        let wait = {
            let mut m = self.next.lock().unwrap();
            let now = Instant::now();
            let at = m.get(host).copied().unwrap_or(now).max(now);
            m.insert(host.to_string(), at + gap);
            at - now
        };
        tokio::time::sleep(wait).await;
    }

    async fn get(&self, url: &Url, gap: Duration) -> Result<(u16, String, String), FetchError> {
        self.turn(url.host_str().unwrap_or_default(), gap).await;
        let r = self
            .http
            .get(url.clone())
            .send()
            .await
            .map_err(|e| FetchError::Http(e.to_string()))?;
        let code = r.status().as_u16();
        let ctype = r
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let mut body = r.bytes().await.map_err(|e| FetchError::Http(e.to_string()))?.to_vec();
        body.truncate(MAX_BODY);
        Ok((code, ctype, String::from_utf8_lossy(&body).into_owned()))
    }

    /// A company page as raw HTML/XML, if robots.txt lets us.
    pub async fn page(&self, url: &Url) -> Result<String, FetchError> {
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        if banned(&host) {
            return Err(FetchError::Banned);
        }
        let robots = self.robots_txt(url).await;
        let path = match url.query() {
            Some(q) => format!("{}?{q}", url.path()),
            None => url.path().to_string(),
        };
        match robots {
            Some(txt) if !robots_allows(&txt, AGENT, &path) => return Err(FetchError::Robots),
            None => return Err(FetchError::Robots),
            _ => {}
        }
        let (code, ctype, body) = self.get(url, self.gap).await?;
        if !(200..300).contains(&code) {
            return Err(FetchError::Http(format!("HTTP {code}")));
        }
        if !(ctype.is_empty() || ctype.contains("html") || ctype.contains("xml") || ctype.starts_with("text/")) {
            return Err(FetchError::Http(format!("not a page: {ctype}")));
        }
        Ok(body)
    }

    /// An API made for machines (Nominatim): its own rate, no robots.txt.
    pub async fn api(&self, url: &Url, gap: Duration) -> Result<String, FetchError> {
        let (code, _, body) = self.get(url, gap).await?;
        if !(200..300).contains(&code) {
            return Err(FetchError::Http(format!("HTTP {code}")));
        }
        Ok(body)
    }

    /// The host's robots.txt, cached. A missing file (4xx) allows all; an unreachable one
    /// (5xx, network) is `None`: we stay out (RFC 9309 §2.3.1).
    async fn robots_txt(&self, url: &Url) -> Option<String> {
        let origin = url.origin().ascii_serialization();
        if let Some(r) = self.robots.lock().unwrap().get(&origin) {
            return r.clone();
        }
        let r = match Url::parse(&format!("{origin}/robots.txt")) {
            Ok(u) => match self.get(&u, self.gap).await {
                Ok((code, _, body)) if (200..300).contains(&code) => Some(body),
                Ok((code, _, _)) if (400..500).contains(&code) => Some(String::new()),
                _ => None,
            },
            Err(_) => None,
        };
        self.robots.lock().unwrap().insert(origin, r.clone());
        r
    }
}

/// Sites we never fetch, whatever their robots.txt says.
pub fn banned(host: &str) -> bool {
    let h = host.trim_start_matches("www.");
    h == "linkedin.com" || h.ends_with(".linkedin.com") || h == "lnkd.in"
}

/// A robots.txt group: its user agents and its (allow, path prefix) rules.
type Group = (Vec<String>, Vec<(bool, String)>);

/// RFC 9309: the group naming our agent (else `*`), longest matching rule wins, Allow on a tie.
pub fn robots_allows(txt: &str, agent: &str, path: &str) -> bool {
    let mut groups: Vec<Group> = Vec::new();
    let mut in_agents = false;
    for line in txt.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((k, v)) = line.split_once(':') else { continue };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        match k.as_str() {
            "user-agent" => {
                if !in_agents {
                    groups.push((Vec::new(), Vec::new()));
                    in_agents = true;
                }
                if let Some(g) = groups.last_mut() {
                    g.0.push(v.to_ascii_lowercase());
                }
            }
            "allow" | "disallow" => {
                in_agents = false;
                if let (Some(g), false) = (groups.last_mut(), v.is_empty()) {
                    g.1.push((k == "allow", v.to_string()));
                }
            }
            _ => {}
        }
    }
    let agent = agent.to_ascii_lowercase();
    let ours = |g: &&(Vec<String>, Vec<(bool, String)>)| g.0.iter().any(|a| a != "*" && agent.contains(a.as_str()));
    let any = |g: &&(Vec<String>, Vec<(bool, String)>)| g.0.iter().any(|a| a == "*");
    let Some(g) = groups.iter().find(ours).or_else(|| groups.iter().find(any)) else {
        return true;
    };
    let mut best: Option<(usize, bool)> = None;
    for (allow, pat) in &g.1 {
        if rule_matches(pat, path) && best.is_none_or(|(l, a)| pat.len() > l || (pat.len() == l && *allow && !a)) {
            best = Some((pat.len(), *allow));
        }
    }
    best.is_none_or(|(_, a)| a)
}

/// A robots path pattern: a prefix, with `*` for any run and a trailing `$` for "ends here".
fn rule_matches(pat: &str, path: &str) -> bool {
    let (pat, anchored) = match pat.strip_suffix('$') {
        Some(p) => (p, true),
        None => (pat, false),
    };
    let parts: Vec<&str> = pat.split('*').collect();
    if !path.starts_with(parts[0]) {
        return false;
    }
    let mut pos = parts[0].len();
    for (i, part) in parts.iter().enumerate().skip(1) {
        if anchored && i == parts.len() - 1 {
            return path.len() >= pos + part.len() && path.ends_with(part);
        }
        match path[pos..].find(part) {
            Some(j) => pos += j + part.len(),
            None => return false,
        }
    }
    !anchored || pos == path.len()
}

/// Readable text of an HTML or RSS page: scripts, styles and tags gone, entities decoded,
/// blocks on their own line, cut at `PAGE_CHARS`.
pub fn page_text(html: &str) -> String {
    let src = html.replace("<![CDATA[", "").replace("]]>", "");
    let lower = src.to_ascii_lowercase();
    let mut out = String::new();
    let mut i = 0;
    while let Some(lt) = src[i..].find('<').map(|j| i + j) {
        out.push_str(&src[i..lt]);
        let Some(gt) = src[lt..].find('>').map(|j| lt + j) else {
            break;
        };
        let name: String = lower[lt + 1..gt]
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        i = gt + 1;
        if lower[lt..].starts_with("<!--") {
            i = lower[lt..].find("-->").map_or(src.len(), |j| lt + j + 3);
        } else if ["script", "style", "noscript", "svg", "template"].contains(&name.as_str())
            && !lower[lt + 1..].starts_with('/')
        {
            i = lower[gt..].find(&format!("</{name}")).map_or(src.len(), |j| gt + j);
        } else if [
            "p", "div", "br", "li", "h1", "h2", "h3", "h4", "tr", "section", "article", "item", "title", "footer",
        ]
        .contains(&name.as_str())
        {
            out.push('\n');
        } else {
            out.push(' ');
        }
    }
    if i < src.len() {
        out.push_str(&src[i..]);
    }
    let text = unescape(&out);
    let lines: Vec<String> = text
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect();
    let joined = lines.join("\n");
    match joined.char_indices().nth(PAGE_CHARS) {
        Some((cut, _)) => joined[..cut].to_string(),
        None => joined,
    }
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(a) = rest.find('&') {
        out.push_str(&rest[..a]);
        rest = &rest[a..];
        // Look for ';' within the next 10 characters (char-safe: pages carry U+202F and friends).
        let end = rest.char_indices().take(10).find(|&(_, c)| c == ';').map(|(i, _)| i);
        let ent = end.map(|e| &rest[1..e]);
        let ch = match ent {
            Some("amp") => Some('&'),
            Some("lt") => Some('<'),
            Some("gt") => Some('>'),
            Some("quot") => Some('"'),
            Some("apos") => Some('\''),
            Some("nbsp") => Some(' '),
            Some(e) if e.starts_with("#x") || e.starts_with("#X") => {
                u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32)
            }
            Some(e) if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match (ch, end) {
            (Some(c), Some(e)) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The pages worth reading, found from the site's own links: one per kind, same site only.
pub const PAGE_KINDS: [(&str, &[&str]); 4] = [
    (
        "about",
        &[
            "about",
            "over-ons",
            "over ons",
            "overons",
            "mission",
            "missie",
            "who-we-are",
            "wie-zijn-wij",
            "wie we zijn",
        ],
    ),
    (
        "team",
        &[
            "team",
            "leadership",
            "management",
            "directie",
            "bestuur",
            "board",
            "our-people",
        ],
    ),
    (
        "careers",
        &[
            "career",
            "jobs",
            "vacature",
            "werken-bij",
            "werkenbij",
            "werken bij",
            "join-us",
            "join us",
        ],
    ),
    ("news", &["news", "nieuws", "press", "pers", "blog", "media", "updates"]),
];

pub fn discover(base: &Url, html: &str) -> Vec<(&'static str, Url)> {
    let lower = html.to_ascii_lowercase();
    let site = |u: &Url| u.host_str().map(|h| h.trim_start_matches("www.").to_string());
    let mut found: Vec<(&'static str, Url)> = Vec::new();
    let mut i = 0;
    while let Some(a) = lower[i..].find("<a ").map(|j| i + j) {
        let Some(gt) = lower[a..].find('>').map(|j| a + j) else {
            break;
        };
        let close = lower[gt..].find("</a").map_or(lower.len(), |j| gt + j);
        i = gt + 1;
        let Some(href) = attr(&html[a..gt], "href") else {
            continue;
        };
        let Ok(url) = base.join(&href) else { continue };
        if site(&url) != site(base) || !url.scheme().starts_with("http") || url.path() == base.path() {
            continue;
        }
        let label = format!(
            "{} {}",
            url.path().to_ascii_lowercase(),
            page_text(&html[gt + 1..close]).to_lowercase()
        );
        for (kind, words) in PAGE_KINDS {
            if !found.iter().any(|f| f.0 == kind) && words.iter().any(|w| label.contains(w)) {
                let mut u = url.clone();
                u.set_fragment(None);
                found.push((kind, u));
                break;
            }
        }
    }
    // An RSS/Atom feed the site announces.
    let mut j = 0;
    while let Some(l) = lower[j..].find("<link ").map(|k| j + k) {
        let gt = lower[l..].find('>').map_or(lower.len(), |k| l + k);
        j = gt.min(lower.len() - 1) + 1;
        let tag = &html[l..gt];
        let is_feed = attr(tag, "type").is_some_and(|t| t.contains("rss") || t.contains("atom"));
        if let (true, Some(href)) = (is_feed, attr(tag, "href")) {
            if let Ok(u) = base.join(&href) {
                if site(&u) == site(base) {
                    found.push(("feed", u));
                    break;
                }
            }
        }
        if j >= lower.len() {
            break;
        }
    }
    found
}

/// The value of attribute `name` in one tag (`<a href="…" …`).
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(p) = lower[from..].find(name).map(|k| from + k) {
        from = p + name.len();
        let before = lower[..p].chars().last();
        let rest = lower[from..].trim_start();
        if !before.is_some_and(|c| c.is_whitespace()) || !rest.starts_with('=') {
            continue;
        }
        let vstart = tag.len() - rest.len() + 1;
        let v = tag[vstart..].trim_start();
        let (q, body) = match v.chars().next() {
            Some(q @ ('"' | '\'')) => (Some(q), &v[1..]),
            _ => (None, v),
        };
        let end = match q {
            Some(q) => body.find(q),
            None => body.find(|c: char| c.is_whitespace() || c == '>'),
        }
        .unwrap_or(body.len());
        return Some(unescape(&body[..end]));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn robots_rules() {
        let txt =
            "User-agent: *\nDisallow: /private\nAllow: /private/open\n\nUser-agent: career-compass\nDisallow: /no-cc\n";
        assert!(!robots_allows(txt, AGENT, "/no-cc/x"));
        assert!(
            robots_allows(txt, AGENT, "/private"),
            "our own group replaces the * group"
        );
        assert!(!robots_allows(txt, "otherbot", "/private/x"));
        assert!(robots_allows(txt, "otherbot", "/private/open/x"));
        assert!(!robots_allows("User-agent: *\nDisallow: /*.pdf$\n", AGENT, "/a/b.pdf"));
        assert!(robots_allows(
            "User-agent: *\nDisallow: /*.pdf$\n",
            AGENT,
            "/a/b.pdf.html"
        ));
        assert!(robots_allows("User-agent: *\nDisallow:\n", AGENT, "/x"));
    }

    #[test]
    fn unescape_multibyte_near_ampersand() {
        // Zivver's page, 2026-11-25: a narrow no-break space inside the 10-byte window panicked.
        // Same layout as the crash: '&', 8 bytes, then U+202F at bytes 9..12 of the window.
        assert_eq!(unescape("a &abcdefgh\u{202f} b"), "a &abcdefgh\u{202f} b");
        assert_eq!(unescape("€&amp;€ &#x20AC; &é;"), "€&€ € &é;");
        // U+202F is whitespace, so page_text folds it like any space.
        assert_eq!(
            page_text("<p>Prijs&\u{202f}9\u{202f}000&nbsp;euro</p>"),
            "Prijs& 9 000 euro"
        );
    }

    #[test]
    fn text_and_links() {
        let html = r#"<html><head><style>p{}</style><link rel="alternate" type="application/rss+xml" href="/feed"></head>
            <body><!-- x --><p>We make&nbsp;<b>scanners</b> &amp; more.</p><script>var a="<p>no</p>";</script>
            <a href="/over-ons">Over ons</a> <a class=x href='/jobs#top'>Werken bij</a> <a href="https://www.linkedin.com/company/x">in</a>
            <a href="https://other.example/news">news</a></body></html>"#;
        let t = page_text(html);
        assert!(t.contains("We make scanners & more."), "{t}");
        assert!(!t.contains("no") && !t.contains("p{}"), "{t}");
        let base = Url::parse("https://www.example.nl/").unwrap();
        let found: Vec<(&str, String)> = discover(&base, html)
            .into_iter()
            .map(|(k, u)| (k, u.to_string()))
            .collect();
        assert_eq!(
            found,
            vec![
                ("about", "https://www.example.nl/over-ons".to_string()),
                ("careers", "https://www.example.nl/jobs".to_string()),
                ("feed", "https://www.example.nl/feed".to_string()),
            ]
        );
        assert!(banned("www.linkedin.com") && banned("nl.linkedin.com") && !banned("example.nl"));
    }

    /// A local site whose robots.txt shuts /private: the fetcher never asks for that page.
    #[tokio::test]
    async fn robots_disallow_is_refused() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let asked = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
        let log = asked.clone();
        std::thread::spawn(move || {
            for s in listener.incoming() {
                let mut s = s.unwrap();
                let mut buf = [0u8; 2048];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("").to_string();
                log.lock().unwrap().push(path.clone());
                let body = if path == "/robots.txt" {
                    "User-agent: *\nDisallow: /private\n"
                } else {
                    "<p>hello</p>"
                };
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        let f = Fetcher::new(Duration::ZERO);
        let base = format!("http://127.0.0.1:{port}");
        let private = Url::parse(&format!("{base}/private/team")).unwrap();
        assert_eq!(f.page(&private).await, Err(FetchError::Robots));
        let open = Url::parse(&format!("{base}/about")).unwrap();
        assert_eq!(page_text(&f.page(&open).await.unwrap()), "hello");
        assert_eq!(*asked.lock().unwrap(), vec!["/robots.txt", "/about"]);
    }
}
