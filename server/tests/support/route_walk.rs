//! The oracle and the checkers of the cross-cutting security suite: the route table of the routes contract, the content security
//! policy and the header set of the cookies-and-origin contract, the scanner that reads the route literals out of the router
//! modules, the header, policy, cookie and asset checkers, and the variations of the cross-site matrix as data.
//!
//! The table and the policy are copied here by hand as the independent oracle. They are compared with the code and never
//! generated from it. A change of either contract is a change of this file in the same commit.
//!
//! Tests pull this file in with `#[path = "support/route_walk.rs"] mod route_walk;`.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

// ---- the route table (routes contract; account and administrator routes) --------------------------------------------------

/// What a route needs before its handler runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// No session.
    Public,
    /// A live session; a change also needs the anti-forgery token.
    Session,
    /// A live session whose sign-in is recent enough for every change (the account routes).
    Fresh,
    /// As `Fresh`, and the role administrator (the administrator routes).
    Admin,
    /// The upgrade of the WebSocket: the handler closes with a code.
    Socket,
    /// As `Session`, with an octet-stream body.
    Upload,
    /// A bearer token of a device; no cookie is read, no Origin and no anti-forgery rule applies, and the body is the
    /// handler's.
    Device,
    /// No credential, JSON only: the start of the sign-in of a native client. No cookie, no Origin rule.
    DeviceOpen,
}

impl Kind {
    pub fn needs_session(self) -> bool {
        !matches!(self, Kind::Public | Kind::Socket | Kind::Device | Kind::DeviceOpen)
    }

    /// Whether the router checks the Origin of a change on this kind.
    pub fn has_origin_rule(self) -> bool {
        !matches!(self, Kind::Socket | Kind::Device | Kind::DeviceOpen)
    }

    /// Whether the router checks the content type of a change on this kind (the type each kind takes is its own).
    pub fn checks_content_type(self) -> bool {
        !matches!(self, Kind::Socket | Kind::Device)
    }

    /// Whether the router reads the JSON body of a change here and refuses one over the cap.
    pub fn reads_json_body(self) -> bool {
        !matches!(self, Kind::Socket | Kind::Upload | Kind::Device)
    }
}

/// One route.
#[derive(Debug, Clone)]
pub struct Row {
    pub method: &'static str,
    /// The pattern, with `{id}`, `{token}`, `{handle}` for the parameters.
    pub path: &'static str,
    pub kind: Kind,
    /// Whether the method changes state (`POST`, `PUT`, `DELETE`).
    pub changes: bool,
    /// The answers a call without a cookie may get: the status and the `code` member (empty for none), or a redirect.
    pub unauth: &'static [(u16, &'static str)],
    /// The status of a correct call.
    pub success: u16,
    /// The audit action a correct call writes, if any.
    pub audit: Option<&'static str>,
}

const BAD: &[(u16, &str)] = &[(400, "bad_request")];
const BAD_OR_INVALID: &[(u16, &str)] = &[(400, "bad_request"), (401, "invalid")];
const INVALID: &[(u16, &str)] = &[(401, "invalid")];

macro_rules! row {
    ($m:literal, $p:literal, $k:ident, $changes:literal, $unauth:expr, $ok:literal, $audit:expr) => {
        Row { method: $m, path: $p, kind: Kind::$k, changes: $changes, unauth: $unauth, success: $ok, audit: $audit }
    };
}

/// The routes of the contract. The files of the page are not rows: they are a group (see [`PAGE_FILES`]).
pub fn table() -> Vec<Row> {
    vec![
        // Public, no session.
        row!("GET", "/", Public, false, &[(200, "")], 200, None),
        row!("GET", "/api/session", Public, false, &[(200, "")], 200, None),
        row!("POST", "/auth/password", Public, true, BAD, 200, None),
        row!("POST", "/auth/totp", Public, true, BAD_OR_INVALID, 200, Some("signin.ok")),
        row!("POST", "/auth/passkey/begin", Public, true, &[(200, ""), (400, "bad_request")], 200, None),
        row!("POST", "/auth/passkey/finish", Public, true, BAD_OR_INVALID, 200, Some("signin.ok")),
        row!("GET", "/auth/oidc/{id}/start", Public, false, &[(302, ""), (404, "not_found")], 302, None),
        row!("GET", "/auth/oidc/callback", Public, false, &[(302, "")], 302, None),
        row!("POST", "/auth/bootstrap", Public, true, BAD, 200, Some("bootstrap.redeem")),
        row!("POST", "/auth/invite/redeem", Public, true, BAD, 200, Some("account.redeem")),
        // A session is required.
        row!("GET", "/ws", Socket, false, &[(400, "bad_request")], 101, None),
        row!("POST", "/auth/logout", Session, true, INVALID, 204, Some("signout")),
        row!("POST", "/auth/unlock", Session, true, INVALID, 200, Some("secrets.unlock")),
        row!("POST", "/auth/step-up", Session, true, INVALID, 200, Some("session.step_up")),
        row!("POST", "/auth/step-up/begin", Session, true, INVALID, 200, None),
        row!("GET", "/files/{token}", Session, false, INVALID, 200, None),
        row!("PUT", "/api/upload", Upload, true, INVALID, 200, None),
        row!("GET", "/api/me", Fresh, false, INVALID, 200, None),
        row!("POST", "/api/me/password", Fresh, true, INVALID, 204, Some("auth.password.change")),
        row!("POST", "/api/me/totp", Fresh, true, INVALID, 200, None),
        row!("POST", "/api/me/passkeys", Fresh, true, INVALID, 200, None),
        row!("DELETE", "/api/me/passkeys/{id}", Fresh, true, INVALID, 204, Some("auth.passkey.remove")),
        row!("PUT", "/api/me/passphrase", Fresh, true, INVALID, 204, Some("auth.passphrase.set")),
        row!("POST", "/api/me/recovery", Fresh, true, INVALID, 200, Some("recovery.replace")),
        row!("DELETE", "/api/me/sessions", Fresh, true, INVALID, 204, Some("signout")),
        row!("DELETE", "/api/me/sessions/{handle}", Fresh, true, INVALID, 204, Some("signout")),
        // The administrator.
        row!("GET", "/api/admin/accounts", Admin, false, INVALID, 200, None),
        row!("POST", "/api/admin/invitations", Admin, true, INVALID, 200, Some("account.invite")),
        row!("DELETE", "/api/admin/accounts/{id}", Admin, true, INVALID, 204, Some("account.delete")),
        row!("POST", "/api/admin/accounts/{id}/role", Admin, true, INVALID, 204, Some("account.role")),
        row!("POST", "/api/admin/accounts/{id}/disable", Admin, true, INVALID, 204, Some("account.disable")),
        row!("POST", "/api/admin/accounts/{id}/enable", Admin, true, INVALID, 204, Some("account.enable")),
        row!("POST", "/api/admin/accounts/{id}/reset-secrets", Admin, true, INVALID, 204, Some("secrets.reset")),
        row!("POST", "/api/admin/accounts/{id}/unlock-signin", Admin, true, INVALID, 204, Some("account.unlock")),
        // The device features. These rows are written by hand like every row above and are not generated from the router. The
        // device route answers a caller with no token 401 while its feature is on; while it is off the path answers 404 (see
        // `GATED_PREFIXES`). The two switch routes are not under a gate: they are how a switch is turned on.
        row!("GET", "/api/device/me", Device, false, INVALID, 200, None),
        row!("GET", "/api/admin/device-access", Admin, false, INVALID, 200, None),
        row!("POST", "/api/admin/device-access", Admin, true, INVALID, 200, Some("device_access.change")),
    ]
}

// ---- the gates of the device features ---------------------------------------------------------------------------------------

/// The prefixes of the paths that belong to a device feature, as whole path segments. They are copied here by hand and never
/// read from the code: a path under one of them answers exactly as a path that is not there (404 `not_found`) for a caller
/// with no credential while its feature is off, and the kind's usual refusal while it is on.
pub const GATED_PREFIXES: &[&str] = &["/api/device/", "/sync/v1/", "/ws/device"];

/// Whether a path of the table lies under one of [`GATED_PREFIXES`].
pub fn is_gated(path: &str) -> bool {
    GATED_PREFIXES.iter().any(|prefix| {
        let bare = prefix.trim_end_matches('/');
        path == bare || path.strip_prefix(bare).is_some_and(|rest| rest.starts_with('/'))
    })
}

/// What a kind refuses a caller with no credential with, when the kind has one answer for every route (a device route: no
/// token, 401 `invalid`).
fn kind_refusal(kind: Kind) -> Option<(u16, &'static str)> {
    match kind {
        Kind::Device => Some((401, "invalid")),
        _ => None,
    }
}

/// The answers a call without a cookie may get from a row. A gated row answers 404 `not_found` while its feature is off and
/// the kind's usual refusal while it is on; every other row answers what the table lists.
pub fn unauth_expectation(row: &Row, feature_on: bool) -> Vec<(u16, &'static str)> {
    if is_gated(row.path) && !feature_on {
        vec![(404, "not_found")]
    } else if is_gated(row.path) {
        kind_refusal(row.kind).map_or_else(|| row.unauth.to_vec(), |refusal| vec![refusal])
    } else {
        row.unauth.to_vec()
    }
}

/// The answer to a method the row does not have, from a caller with no credential. A route that needs a session or a token
/// refuses the caller before the method is looked at; a public route answers 405; a gated path whose feature is off answers 404.
pub fn wrong_method_expectation(row: &Row, feature_on: bool) -> (u16, &'static str) {
    if is_gated(row.path) && !feature_on {
        (404, "not_found")
    } else if row.kind.needs_session() {
        (401, "invalid")
    } else if let Some(refusal) = kind_refusal(row.kind) {
        refusal
    } else {
        (405, "bad_request")
    }
}

/// Reads the parts of the router modules that are mounted as `Access::Device` or `Access::DeviceOpen` and answers one line for
/// every route of them whose path is not under a gated prefix, and for every such part whose routes cannot be read. A route of
/// a device kind that no gate covers would be reachable while its feature is off.
pub fn device_route_problems(files: &[(&str, &str)]) -> Vec<String> {
    let mut problems = Vec::new();
    for (name, text) in files {
        let text = text.split("#[cfg(test)]").next().unwrap_or_default();
        let mut from = 0;
        while let Some(at) = text[from..].find("access: Access::Device") {
            let start = from + at + "access: Access::Device".len();
            from = start;
            let kind: String = std::iter::once("Device").chain(text[start..].starts_with("Open").then_some("Open")).collect();
            let Some(router_at) = text[start..].find("router:") else {
                problems.push(format!("{name}: a part mounted as {kind} has no router member"));
                continue;
            };
            let after = text[start + router_at + "router:".len()..].trim_start();
            let expression = if after.starts_with("Router::new()") {
                after.split('}').next().unwrap_or_default().to_owned()
            } else {
                let ident: String = after.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
                let binding = [format!("let {ident} = Router::new()"), format!("let mut {ident} = Router::new()")]
                    .iter()
                    .find_map(|pattern| text.find(pattern.as_str()));
                match binding {
                    Some(at) => text[at..].split(';').next().unwrap_or_default().to_owned(),
                    None => {
                        problems.push(format!("{name}: the router `{ident}` of a part mounted as {kind} is not built in the same file"));
                        continue;
                    }
                }
            };
            let found = source_routes(&[(name, &expression)]);
            if found.literal.is_empty() || found.dynamic > 0 {
                problems.push(format!("{name}: the routes of a part mounted as {kind} cannot be read (literal paths are required)"));
            }
            for route in &found.literal {
                let path = route.split_once(' ').map_or(route.as_str(), |(_, path)| path);
                if !is_gated(path) {
                    problems.push(format!("{name}: {route} is mounted as {kind} and is not under a gated prefix"));
                }
            }
        }
    }
    problems
}

/// The pages of the application: what a request for a file of the build gets. A file of the sign-in page needs no session, any
/// other file does.
pub const PAGE_FILES: &str = "files of the page (see the asset walk)";

/// The files the sign-in page needs, as the routes contract lists them (the page, its script and style, the shared style,
/// the favicon and the fonts); the modules the script imports are added by following its imports in the build.
pub const PRE_AUTH_FILES: &[&str] = &["/login.html", "/login.js", "/login.css", "/app.css", "/favicon.ico"];

/// The prefix of the fonts, which need no session either.
pub const PRE_AUTH_PREFIX: &str = "/fonts/";

/// The pattern with every parameter replaced by one marker, so a table path and a source path compare equal.
pub fn normalise(path: &str) -> String {
    path.split('/')
        .map(|segment| if segment.starts_with('{') && segment.ends_with('}') { "{}" } else { segment })
        .collect::<Vec<_>>()
        .join("/")
}

/// A concrete path for a pattern.
pub fn concrete(path: &str, id: &str) -> String {
    path.split('/')
        .map(|segment| if segment.starts_with('{') && segment.ends_with('}') { id } else { segment })
        .collect::<Vec<_>>()
        .join("/")
}

/// The table as `METHOD normalised-path` entries.
pub fn table_routes(rows: &[Row]) -> BTreeSet<String> {
    rows.iter().map(|row| format!("{} {}", row.method, normalise(row.path))).collect()
}

// ---- the source scanner -----------------------------------------------------------------------------------------------------

/// What the scanner found in the router modules.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SourceRoutes {
    /// `METHOD normalised-path` for every route registered with a literal path.
    pub literal: BTreeSet<String>,
    /// The number of registrations whose path is not a literal (the files of the page, which come from a table made from the
    /// build).
    pub dynamic: usize,
}

fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Reads the route literals out of the source text of a module: every `.route("path", get(..).post(..))` outside the test
/// module at the end of the file. The scanner knows the five method names of the router; it skips the arguments of a handler
/// without looking inside.
pub fn source_routes(files: &[(&str, &str)]) -> SourceRoutes {
    let mut found = SourceRoutes::default();
    for (_, text) in files {
        let text = text.split("#[cfg(test)]").next().unwrap_or_default();
        let bytes = text.as_bytes();
        let mut from = 0;
        while let Some(at) = text[from..].find(".route(") {
            let mut i = from + at + ".route(".len();
            from = i;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= bytes.len() || bytes[i] != b'"' {
                found.dynamic += 1;
                continue;
            }
            let start = i + 1;
            let Some(len) = text[start..].find('"') else { continue };
            let path = text[start..start + len].to_owned();
            i = start + len + 1;
            // The handler expression: methods at the top level of the argument list.
            let mut depth = 0i32;
            let mut word = String::new();
            let mut methods = Vec::new();
            while i < bytes.len() {
                let c = bytes[i];
                if depth == 0 && is_ident(c) {
                    word.push(char::from(c));
                } else {
                    if depth == 0 && c == b'(' && ["get", "post", "put", "delete", "patch"].contains(&word.as_str()) {
                        methods.push(word.to_ascii_uppercase());
                    }
                    word.clear();
                    match c {
                        b'(' | b'[' | b'{' => depth += 1,
                        b')' | b']' | b'}' => {
                            depth -= 1;
                            if depth < 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                i += 1;
            }
            for method in methods {
                found.literal.insert(format!("{method} {}", normalise(&path)));
            }
        }
    }
    found
}

/// Compares the contract's table with the routes of the source. `Err` holds one line for every route on one side only.
pub fn table_matches_source(table: &BTreeSet<String>, source: &BTreeSet<String>) -> Result<(), Vec<String>> {
    let mut differences = Vec::new();
    for only in source.difference(table) {
        differences.push(format!("in the source and not in the contract's table: {only}"));
    }
    for only in table.difference(source) {
        differences.push(format!("in the contract's table and not in the source: {only}"));
    }
    if differences.is_empty() { Ok(()) } else { Err(differences) }
}

// ---- headers ----------------------------------------------------------------------------------------------------------------

/// The `Permissions-Policy` of every response.
pub const PERMISSIONS_POLICY: &str = "accelerometer=(), bluetooth=(), camera=(), display-capture=(), geolocation=(), gyroscope=(), hid=(), \
magnetometer=(), microphone=(), midi=(), payment=(), serial=(), usb=(), xr-spatial-tracking=(), \
clipboard-read=(self), clipboard-write=(self), publickey-credentials-create=(self), publickey-credentials-get=(self)";

/// The security headers every response carries, with their values.
pub const FIXED_HEADERS: &[(&str, &str)] = &[
    ("strict-transport-security", "max-age=31536000"),
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "no-referrer"),
    ("cross-origin-opener-policy", "same-origin"),
    ("permissions-policy", PERMISSIONS_POLICY),
];

/// What the checker needs to know of a response.
#[derive(Debug, Clone)]
pub struct Seen<'a> {
    pub status: u16,
    pub path: &'a str,
    pub headers: &'a [(String, String)],
}

impl Seen<'_> {
    fn all(&self, name: &str) -> Vec<&str> {
        self.headers.iter().filter(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str()).collect()
    }
}

/// Whether the response is a file the user granted: it carries the file route's own sandbox policy instead of the page's.
fn is_granted_file(seen: &Seen<'_>) -> bool {
    seen.path.starts_with("/files/") && seen.status < 300
}

/// Checks the headers of one response: the fixed headers with their exact values, the policy, `no-store` on every `/api` and
/// `/auth` answer, the resource policy, and no cross-origin sharing header. Answers every problem found.
pub fn check_headers(seen: &Seen<'_>, public_host: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for (name, value) in FIXED_HEADERS {
        match seen.all(name).as_slice() {
            [one] if one == value => {}
            [] => problems.push(format!("{} {}: header {name} is missing (status {})", seen.path, seen.status, seen.status)),
            other => problems.push(format!("{} {}: header {name} has a wrong value: {other:?}", seen.path, seen.status)),
        }
    }
    if seen.all("cross-origin-resource-policy").is_empty() {
        problems.push(format!("{} {}: header cross-origin-resource-policy is missing", seen.path, seen.status));
    }
    if !is_granted_file(seen) {
        match seen.all("content-security-policy").as_slice() {
            [one] => problems.extend(check_csp(one, public_host).into_iter().map(|p| format!("{} {}: {p}", seen.path, seen.status))),
            other => problems.push(format!("{} {}: {} content-security-policy headers", seen.path, seen.status, other.len())),
        }
    }
    let api = seen.path.starts_with("/api") || seen.path.starts_with("/auth");
    if api {
        let cache = seen.all("cache-control");
        if cache != ["no-store"] {
            problems.push(format!("{} {}: cache-control on an api or auth answer is {cache:?}", seen.path, seen.status));
        }
    }
    for (name, _) in seen.headers {
        if name.to_ascii_lowercase().starts_with("access-control-") {
            problems.push(format!("{} {}: a header named {} (cross-origin sharing) is present", seen.path, seen.status, name.to_ascii_lowercase()));
        }
    }
    problems
}

// ---- the content security policy ----------------------------------------------------------------------------------------------

/// The policy of the cookies-and-origin contract with the public host filled in, as directive and sources.
pub fn contract_policy(public_host: &str) -> BTreeMap<String, BTreeSet<String>> {
    let wss = format!("wss://{public_host}");
    let spec: [(&str, Vec<&str>); 11] = [
        ("default-src", vec!["'self'"]),
        ("script-src", vec!["'self'"]),
        ("style-src", vec!["'self'"]),
        ("img-src", vec!["'self'"]),
        ("media-src", vec!["'self'"]),
        ("font-src", vec!["'self'"]),
        ("connect-src", vec!["'self'", &wss]),
        ("object-src", vec!["'none'"]),
        ("base-uri", vec!["'none'"]),
        ("form-action", vec!["'self'"]),
        ("frame-ancestors", vec!["'none'"]),
    ];
    spec.into_iter().map(|(d, sources)| (d.to_owned(), sources.into_iter().map(str::to_owned).collect())).collect()
}

/// A header value as directives with their sources. A directive named twice is a problem the caller sees as an extra source.
pub fn parse_csp(header: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut parsed: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for directive in header.split(';') {
        let mut words = directive.split_whitespace();
        let Some(name) = words.next() else { continue };
        parsed.entry(name.to_ascii_lowercase()).or_default().extend(words.map(str::to_owned));
    }
    parsed
}

/// Compares a policy with the contract's directive by directive (the order of sources does not matter) and rejects the
/// sources that must never appear. Answers every problem found.
pub fn check_csp(header: &str, public_host: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let policy = parse_csp(header);
    let expected = contract_policy(public_host);
    for (directive, sources) in &expected {
        match policy.get(directive) {
            None => problems.push(format!("the policy lacks the directive {directive}")),
            Some(found) if found != sources => problems.push(format!("the directive {directive} is {found:?}, the contract says {sources:?}")),
            Some(_) => {}
        }
    }
    for directive in policy.keys() {
        if !expected.contains_key(directive) {
            problems.push(format!("the policy has the directive {directive}, which the contract does not"));
        }
    }
    for (directive, sources) in &policy {
        for source in sources {
            let lower = source.to_ascii_lowercase();
            if lower == "'unsafe-inline'" || lower == "'unsafe-eval'" || lower == "'unsafe-hashes'" || lower == "'wasm-unsafe-eval'" {
                problems.push(format!("{directive} admits {lower}"));
            } else if lower == "data:" || lower == "blob:" || lower == "filesystem:" {
                problems.push(format!("{directive} admits {lower}"));
            } else if lower == "*" || lower.contains('*') {
                problems.push(format!("{directive} admits a wildcard"));
            } else if lower.ends_with(':') && !lower.contains('/') {
                problems.push(format!("{directive} admits the whole scheme {lower}"));
            } else if lower.starts_with("'nonce-") || lower.starts_with("'sha") || lower == "'strict-dynamic'" {
                problems.push(format!("{directive} admits {lower}, which the contract does not"));
            } else if !lower.starts_with('\'') && lower != format!("wss://{public_host}") {
                problems.push(format!("{directive} admits a host that is not the public one: {lower}"));
            }
        }
    }
    problems
}

// ---- cookies ----------------------------------------------------------------------------------------------------------------

/// The attributes of a `Set-Cookie` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub attributes: BTreeMap<String, String>,
}

pub fn parse_set_cookie(line: &str) -> Cookie {
    let mut parts = line.split(';');
    let first = parts.next().unwrap_or_default();
    let (name, value) = first.split_once('=').unwrap_or((first, ""));
    let mut attributes = BTreeMap::new();
    for part in parts {
        let part = part.trim();
        let (key, value) = part.split_once('=').unwrap_or((part, ""));
        attributes.insert(key.to_ascii_lowercase(), value.to_owned());
    }
    Cookie { name: name.trim().to_owned(), value: value.to_owned(), attributes }
}

/// Checks a `Set-Cookie` line against the contract: the three names, `Secure`, `HttpOnly`, `Path=/`, no `Domain`, the
/// `SameSite` of each, the age of the transaction cookies, and the shape of a session value. Answers every problem found.
pub fn check_set_cookie(line: &str) -> Vec<String> {
    let cookie = parse_set_cookie(line);
    let mut problems = Vec::new();
    let has = |key: &str| cookie.attributes.contains_key(key);
    let get = |key: &str| cookie.attributes.get(key).map(String::as_str);
    let deleting = get("max-age") == Some("0") && cookie.value.is_empty();
    let (same_site, max_age): (&str, Option<&str>) = match cookie.name.as_str() {
        "__Host-hw_session" => ("Strict", None),
        "__Host-hw_pending" => ("Strict", Some("300")),
        "__Host-hw_oidc" => ("Lax", Some("600")),
        other => return vec![format!("a cookie named {other}, which the contract does not know")],
    };
    if !has("secure") {
        problems.push(format!("{} lacks Secure", cookie.name));
    }
    if !has("httponly") {
        problems.push(format!("{} lacks HttpOnly", cookie.name));
    }
    if get("path") != Some("/") {
        problems.push(format!("{} has the path {:?}", cookie.name, get("path")));
    }
    if has("domain") {
        problems.push(format!("{} names a Domain", cookie.name));
    }
    if get("samesite") != Some(same_site) {
        problems.push(format!("{} has SameSite {:?}, the contract says {same_site}", cookie.name, get("samesite")));
    }
    if deleting {
        return problems;
    }
    match (max_age, get("max-age")) {
        (Some(want), Some(have)) if want == have => {}
        (None, None) => {}
        (want, have) => problems.push(format!("{} has Max-Age {have:?}, the contract says {want:?}", cookie.name)),
    }
    if cookie.name == "__Host-hw_session" && !(cookie.value.len() == 43 && cookie.value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')) {
        problems.push("the session cookie value is not 43 characters of the URL-safe alphabet".to_owned());
    }
    problems
}

// ---- the files of the page ----------------------------------------------------------------------------------------------------

/// An absolute address in a served file that is allowed to stay: the file pattern (a suffix of the path), the literal's start and
/// the reason.
pub const URL_ALLOWED: &[(&str, &str, &str)] = &[
    (".svg", "http://www.w3.org/2000/svg", "the SVG namespace of an image; it names no resource and is never fetched"),
    (".js", "http://www.w3.org/2000/svg", "the SVG namespace a script writes into generated markup; never fetched"),
    ("/webtransport.js", "ws://${host}/ws", "the socket address built from the page's own host (location.host)"),
    ("/webtransport.js", "wss://${host}/ws", "the socket address built from the page's own host (location.host)"),
    ("/sync.js", "https://cloud.example.com/", "the grey example text of a settings field; shown, never fetched"),
    ("/dist-", "https://", "a scheme prefix a link check compares with; no host follows"),
];

/// The absolute and protocol-relative URL literals in a text: the scheme, `ws`, `wss`, `http` and `https`, and `//host`.
pub fn url_literals(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for scheme in ["https://", "http://", "wss://", "ws://"] {
        let mut from = 0;
        while let Some(at) = text[from..].find(scheme) {
            let start = from + at;
            let end = text[start..]
                .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '`' | ')' | '<' | '>' | ',' | ';' | '\\'))
                .map_or(text.len(), |e| start + e);
            found.push(text[start..end].to_owned());
            from = start + scheme.len();
        }
    }
    // A protocol-relative address: `"//host.tld`, `'//host`, or `(//host` right after a quote or a parenthesis.
    for quote in ['"', '\'', '`', '(', '='] {
        let needle = format!("{quote}//");
        let mut from = 0;
        while let Some(at) = text[from..].find(&needle) {
            let start = from + at + 1;
            let rest = &text[start + 2..];
            let word: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-')).collect();
            if word.contains('.') && word.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
                found.push(format!("//{word}"));
            }
            from = start + 2;
        }
    }
    found
}

/// The literals of `text` (the body of the served file at `path`) that are not on the allow-list.
pub fn foreign_urls(path: &str, text: &str) -> Vec<String> {
    url_literals(text)
        .into_iter()
        .filter(|literal| !URL_ALLOWED.iter().any(|(file, start, _)| path.contains(file) && literal.starts_with(start)))
        .collect()
}

/// Whether a served HTML text has an inline script body or an event-handler attribute.
pub fn inline_script_problems(html: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find("<script") {
        let start = from + at;
        let Some(open_end) = lower[start..].find('>') else { break };
        let tag = &lower[start..start + open_end];
        let after = start + open_end + 1;
        let close = lower[after..].find("</script").map_or(lower.len(), |c| after + c);
        if !tag.contains(" src=") && !lower[after..close].trim().is_empty() {
            problems.push("an inline script body".to_owned());
        }
        from = after;
    }
    // An attribute named `on...=` inside a tag.
    let bytes = lower.as_bytes();
    let mut in_tag = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'<' => in_tag = true,
            b'>' => in_tag = false,
            b' ' | b'\t' | b'\n' if in_tag => {
                let rest = &lower[i + 1..];
                if rest.starts_with("on") {
                    let name: String = rest.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
                    if name.len() > 2 && rest[name.len()..].trim_start().starts_with('=') {
                        problems.push(format!("an event-handler attribute {name}"));
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    problems
}

// ---- the variations of the cross-site matrix ----------------------------------------------------------------------------------

/// A request as the matrix alters it.
#[derive(Debug, Clone)]
pub struct Parts {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Parts {
    pub fn set(&mut self, name: &str, value: &str) {
        self.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        self.headers.push((name.to_owned(), value.to_owned()));
    }

    pub fn remove(&mut self, name: &str) {
        self.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// What a refusal looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// This status and this `code`.
    Status(u16, &'static str),
    /// Not applicable to the kind of route: the variation is not run.
    NotApplicable,
}

/// One variation: its name, how it alters a correct request, and the answer each kind of route gives. The answers follow the
/// order of the checks of the application: the origin and the content type and the declared size of a change first, then the
/// session, then the anti-forgery token.
pub struct Variation {
    pub name: &'static str,
    pub alter: fn(&mut Parts),
    pub expect: fn(Kind) -> Expect,
}

const FOREIGN: &str = "https://evil.example.test";

/// A refusal of a change that every kind with an Origin rule gives.
fn origin_only(kind: Kind, status: u16, code: &'static str) -> Expect {
    if kind.has_origin_rule() { Expect::Status(status, code) } else { Expect::NotApplicable }
}

/// A refusal of a change that every kind that reads a JSON body gives.
fn json_only(kind: Kind, status: u16, code: &'static str) -> Expect {
    if kind.reads_json_body() { Expect::Status(status, code) } else { Expect::NotApplicable }
}

/// A refusal of a change that every kind that checks the content type gives.
fn type_only(kind: Kind, status: u16, code: &'static str) -> Expect {
    if kind.checks_content_type() { Expect::Status(status, code) } else { Expect::NotApplicable }
}

fn session_only(kind: Kind, status: u16, code: &'static str) -> Expect {
    if kind.needs_session() { Expect::Status(status, code) } else { Expect::NotApplicable }
}

/// The oversize body: one byte over the 64 KiB cap of a JSON route, valid JSON padded with spaces.
pub fn oversize_body(original: &[u8]) -> Vec<u8> {
    let cap = 64 * 1024;
    let mut body = original.to_vec();
    if body.is_empty() {
        body = b"{}".to_vec();
    }
    body.resize(cap + 1, b' ');
    body
}

pub fn variations() -> Vec<Variation> {
    vec![
        Variation { name: "no cookie", alter: |p| p.remove("cookie"), expect: |k| session_only(k, 401, "invalid") },
        Variation { name: "a session and no anti-forgery token", alter: |p| p.remove("x-csrf-token"), expect: |k| session_only(k, 403, "forbidden") },
        Variation {
            name: "a wrong anti-forgery token",
            alter: |p| p.set("x-csrf-token", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
            expect: |k| session_only(k, 403, "forbidden"),
        },
        Variation { name: "a foreign Origin", alter: |p| p.set("origin", FOREIGN), expect: |k| origin_only(k, 403, "forbidden") },
        Variation {
            name: "no Origin and Sec-Fetch-Site cross-site",
            alter: |p| {
                p.remove("origin");
                p.set("sec-fetch-site", "cross-site");
            },
            expect: |k| origin_only(k, 403, "forbidden"),
        },
        Variation { name: "Origin null", alter: |p| p.set("origin", "null"), expect: |k| origin_only(k, 403, "forbidden") },
        Variation {
            name: "content type text/plain",
            alter: |p| p.set("content-type", "text/plain"),
            expect: |k| type_only(k, 400, "bad_request"),
        },
        Variation {
            name: "content type application/x-www-form-urlencoded",
            alter: |p| p.set("content-type", "application/x-www-form-urlencoded"),
            expect: |k| type_only(k, 400, "bad_request"),
        },
        Variation {
            name: "content type multipart/form-data",
            alter: |p| p.set("content-type", "multipart/form-data; boundary=x"),
            expect: |k| type_only(k, 400, "bad_request"),
        },
        Variation { name: "no content type", alter: |p| p.remove("content-type"), expect: |k| type_only(k, 400, "bad_request") },
        Variation {
            name: "a body one byte over 64 KiB",
            alter: |p| p.body = oversize_body(&p.body),
            expect: |k| json_only(k, 400, "bad_request"),
        },
    ]
}

/// The methods the matrix tries on a path besides the ones the route has.
pub const OTHER_METHODS: &[&str] = &["GET", "POST", "PUT", "DELETE", "PATCH"];

/// The methods of the table on one normalised path.
pub fn methods_of(rows: &[Row], path: &str) -> BTreeSet<&'static str> {
    rows.iter().filter(|row| normalise(row.path) == normalise(path)).map(|row| row.method).collect()
}
