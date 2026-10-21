/// Credential-looking argv for display only; never use this to run or judge a command.
pub(super) fn redact(argv: &[String]) -> Vec<String> {
    const MARK: &str = "[redacted]";
    const FLAGS: &[&str] = &[
        "--password",
        "--passwd",
        "--token",
        "--secret",
        "--api-key",
        "--apikey",
        "--access-key",
        "--client-secret",
    ];
    const HEADERS: &[&str] = &["authorization:", "proxy-authorization:"];
    const SCHEMES: &[&str] = &["bearer", "basic", "token"];
    const QUERY: &[&str] = &[
        "token",
        "key",
        "secret",
        "password",
        "sig",
        "signature",
        "access_token",
    ];
    let boundary = |c: char| c.is_ascii_whitespace() || "\"'`;|()<>".contains(c);
    let url_end = |c: char| c.is_ascii_whitespace() || "\"'`".contains(c);
    // A split header may put its scheme before the credential argument.
    let mut next = None;
    argv.iter()
        .map(|arg| {
            let lower = arg.to_ascii_lowercase();
            if let Some(header) = next.take() {
                if header && SCHEMES.contains(&lower.trim()) {
                    next = Some(false);
                }
                return MARK.to_string();
            }
            let mut spans = Vec::new();
            for prefix in FLAGS.iter().chain(HEADERS).chain(SCHEMES) {
                for (start, _) in lower.match_indices(*prefix) {
                    if start > 0 && !boundary(arg[..start].chars().next_back().unwrap()) {
                        continue;
                    }
                    let mut from = start + prefix.len();
                    let inline = FLAGS.contains(prefix) && lower[from..].starts_with('=');
                    if inline {
                        from += 1;
                    } else if !HEADERS.contains(prefix)
                        && from < arg.len()
                        && !arg.as_bytes()[from].is_ascii_whitespace()
                    {
                        continue;
                    }
                    from += arg[from..].len()
                        - arg[from..].trim_start_matches(char::is_whitespace).len();
                    if from == arg.len() && !inline {
                        next = Some(HEADERS.contains(prefix));
                    } else if HEADERS.contains(prefix) && SCHEMES.contains(&lower[from..].trim()) {
                        next = Some(false);
                    }
                    // Conservatively hide the rest of this argument, including quoted spaces
                    // in a shell script. Other argv entries stay readable.
                    if from < arg.len() {
                        spans.push((from, arg.len()));
                    }
                }
            }
            for (equals, _) in lower.match_indices('=') {
                let name = lower[..equals].rsplit(boundary).next().unwrap();
                if name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                    && ["_token", "_key", "_secret", "password"]
                        .iter()
                        .any(|suffix| name.ends_with(suffix))
                {
                    spans.push((equals + 1, arg.len()));
                }
            }
            for (scheme, _) in lower.match_indices("://") {
                let start = scheme + 3;
                let end = arg[start..]
                    .find(|c: char| "/?#".contains(c) || url_end(c))
                    .map_or(arg.len(), |i| start + i);
                if let Some(at) = arg[start..end].rfind('@')
                    && arg[start..start + at].contains(':')
                {
                    spans.push((start, start + at));
                }
            }
            for (start, _) in lower.match_indices(['?', '&']) {
                let tail = &lower[start + 1..];
                let Some(equals) = tail.find('=') else {
                    continue;
                };
                if QUERY.contains(&&tail[..equals]) {
                    let from = start + 1 + equals + 1;
                    let end = arg[from..]
                        .find(|c: char| "&#".contains(c) || url_end(c))
                        .map_or(arg.len(), |i| from + i);
                    spans.push((from, end));
                }
            }
            let mut run = 0;
            for (i, c) in arg.char_indices().chain(std::iter::once((arg.len(), ' '))) {
                if c.is_ascii_alphanumeric() || "+/=_-".contains(c) {
                    run += 1;
                } else {
                    if run >= 32 && !spans.iter().any(|&(start, end)| start < i && end > i - run) {
                        spans.push((i - run, i));
                    }
                    run = 0;
                }
            }
            spans.sort_unstable();
            let mut shown = String::new();
            let mut cursor = 0;
            for (start, end) in spans {
                if start >= cursor {
                    shown.push_str(&arg[cursor..start]);
                    shown.push_str(MARK);
                }
                cursor = cursor.max(end);
            }
            shown.push_str(&arg[cursor..]);
            shown
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(words: &[&str]) -> Vec<String> {
        redact(&words.iter().map(|word| (*word).into()).collect::<Vec<_>>())
    }

    #[test]
    fn credential_headers_and_schemes_are_redacted() {
        for header in ["Authorization:", "Proxy-Authorization:"] {
            for scheme in ["Bearer", "Basic", "token"] {
                assert_eq!(
                    shown(&[
                        "curl",
                        "-H",
                        &format!("{header} {scheme} header-credential"),
                        "https://example.com"
                    ]),
                    [
                        "curl",
                        "-H",
                        &format!("{header} [redacted]"),
                        "https://example.com"
                    ]
                );
                assert_eq!(
                    shown(&[header, scheme, "header-credential"]),
                    [header, "[redacted]", "[redacted]"]
                );
                assert_eq!(
                    shown(&[scheme, "scheme-credential"]),
                    [scheme, "[redacted]"]
                );
            }
        }
        assert_eq!(
            shown(&["authorization:custom-credential"]),
            ["authorization:[redacted]"]
        );
        assert_eq!(
            shown(&["curl -H 'Authorization: Bearer short-secret' && echo done"]),
            ["curl -H 'Authorization: [redacted]"]
        );
    }

    #[test]
    fn credential_flags_are_redacted_in_both_forms() {
        for flag in [
            "--password",
            "--passwd",
            "--token",
            "--secret",
            "--api-key",
            "--apikey",
            "--access-key",
            "--client-secret",
        ] {
            assert_eq!(
                shown(&["tool", flag, "two word secret", "--verbose"]),
                ["tool", flag, "[redacted]", "--verbose"]
            );
            assert_eq!(
                shown(&["tool", &format!("{flag}=two word secret")]),
                ["tool", &format!("{flag}=[redacted]")]
            );
            assert_eq!(
                shown(&[&format!("tool {flag} 'two word secret' --verbose")]),
                [format!("tool {flag} [redacted]")]
            );
        }
        assert_eq!(
            shown(&["tool", "--password", "Basic", "public"]),
            ["tool", "--password", "[redacted]", "public"]
        );
        assert_eq!(
            shown(&["tool", "--password=Bearer", "public"]),
            ["tool", "--password=[redacted]", "public"]
        );
        assert_eq!(
            shown(&["tool", "--token=", "public"]),
            ["tool", "--token=", "public"]
        );
        assert_eq!(
            shown(&["tool", "--tokenizer=ordinary", "-p8080"]),
            ["tool", "--tokenizer=ordinary", "-p8080"]
        );
    }

    #[test]
    fn url_credentials_and_sensitive_query_values_are_redacted() {
        assert_eq!(
            shown(&["https://user:pass@example.com/path?view=full"]),
            ["https://[redacted]@example.com/path?view=full"]
        );
        assert_eq!(
            shown(&["https://user:p(a);s@example.com/?token=a;b(c)|d&ok=yes"]),
            ["https://[redacted]@example.com/?token=[redacted]&ok=yes"]
        );
        for key in [
            "token",
            "key",
            "secret",
            "password",
            "sig",
            "signature",
            "access_token",
        ] {
            assert_eq!(
                shown(&[&format!(
                    "https://example.com/?view=full&{key}=short%20secret&count=2#part"
                )]),
                [format!(
                    "https://example.com/?view=full&{key}=[redacted]&count=2#part"
                )]
            );
            assert_eq!(
                shown(&[&format!("https://example.com/?{key}=short-secret")]),
                [format!("https://example.com/?{key}=[redacted]")]
            );
        }
    }

    #[test]
    fn credential_environment_assignments_are_redacted() {
        for name in [
            "FOO_TOKEN",
            "API_KEY",
            "APP_SECRET",
            "PASSWORD",
            "DB_PASSWORD",
            "DBPASSWORD",
        ] {
            assert_eq!(
                shown(&["env", &format!("{name}=two word secret"), "tool"]),
                ["env", &format!("{name}=[redacted]"), "tool"]
            );
        }
        assert_eq!(shown(&["env MODE=test tool"]), ["env MODE=test tool"]);
    }

    #[test]
    fn long_credential_looking_runs_are_redacted() {
        let token = "aB3dE5fG7hJ9kL1mN3pQ5rS7tU9vW1xY";
        assert_eq!(
            shown(&["tool", token, &format!("prefix:{token}:suffix")]),
            ["tool", "[redacted]", "prefix:[redacted]:suffix"]
        );
        assert_eq!(
            shown(&["ordinary", "short-token", "the owner", "日本語"]),
            ["ordinary", "short-token", "the owner", "日本語"]
        );
    }
}
