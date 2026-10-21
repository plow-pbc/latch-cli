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
        "--http-password",
        "--proxy-password",
        "--auth",
    ];
    // Flags whose value may be `user:password`: the user stays readable.
    const USERS: &[&str] = &["--user", "--proxy-user", "--http-user", "-u"];
    // Programs whose `-pPASSWORD` is a password (sshpass's `-p PASSWORD` too).
    const DASH_P: &[&str] = &["mysql", "mysqldump", "mysqladmin", "mariadb", "sshpass"];
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
    let program = argv
        .first()
        .and_then(|first| first.rsplit('/').next())
        .unwrap_or_default();
    let dash_p = DASH_P.contains(&program);
    // A split header may put its scheme before the credential argument.
    let mut next = None;
    // A user flag's value is the next argument.
    let mut next_user = false;
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
            if std::mem::take(&mut next_user) {
                spans.extend(password_of(arg, 0));
            }
            if dash_p && arg.starts_with("-p") {
                if arg.len() > 2 {
                    spans.push((2, arg.len()));
                } else if program == "sshpass" {
                    next = Some(false);
                }
            }
            for prefix in USERS {
                for (start, _) in lower.match_indices(*prefix) {
                    if start > 0 && !boundary(arg[..start].chars().next_back().unwrap()) {
                        continue;
                    }
                    let after = start + prefix.len();
                    let value = match arg[after..].chars().next() {
                        None => {
                            next_user = true;
                            continue;
                        }
                        Some(c) if c == '=' || c.is_ascii_whitespace() => {
                            let rest = &arg[after + 1..];
                            arg.len() - rest.trim_start_matches(char::is_whitespace).len()
                        }
                        // Only `-u` takes its value glued on: `-ualice:secret`.
                        Some(_) if *prefix == "-u" => after,
                        Some(_) => continue,
                    };
                    spans.extend(password_of(arg, value));
                }
            }
            for (at, _) in arg.match_indices('@') {
                let word = arg[..at].rfind(boundary).map_or(0, |i| i + 1);
                let Some(colon) = arg[word..at].rfind(':').map(|i| word + i) else {
                    continue;
                };
                let host = arg[colon + 1..at].starts_with('/');
                if colon > word && colon + 1 < at && !host && !arg[word..colon].contains('@') {
                    spans.push((colon + 1, at));
                }
            }
            for prefix in FLAGS.iter().chain(HEADERS).chain(SCHEMES) {
                for (start, _) in lower.match_indices(*prefix) {
                    // A header may also follow `--header=` or a glued `-H`.
                    let glued_header = HEADERS.contains(prefix)
                        && (lower[..start].ends_with('=') || lower[..start].ends_with("-h"));
                    if start > 0
                        && !glued_header
                        && !boundary(arg[..start].chars().next_back().unwrap())
                    {
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

/// In the word of `arg` starting at `from`, the span after its first colon:
/// the password of a `user:password`, if there is one.
fn password_of(arg: &str, from: usize) -> Option<(usize, usize)> {
    let end = arg[from..]
        .find(|c: char| c.is_ascii_whitespace() || "\"'`;|()<>".contains(c))
        .map_or(arg.len(), |i| from + i);
    let colon = from + arg[from..end].find(':')?;
    (colon + 1 < end).then_some((colon + 1, end))
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

    #[test]
    fn user_passwords_are_redacted_and_user_names_kept() {
        assert_eq!(
            shown(&[
                "curl",
                "--user",
                "alice:short-secret",
                "https://example.com"
            ]),
            ["curl", "--user", "alice:[redacted]", "https://example.com"]
        );
        for flag in ["--user", "--proxy-user", "--http-user", "-u", "-U"] {
            assert_eq!(
                shown(&["tool", flag, "alice:short-secret"]),
                ["tool", flag, "alice:[redacted]"]
            );
            assert_eq!(
                shown(&[&format!(
                    "tool {flag} alice:short-secret https://example.com"
                )]),
                [format!("tool {flag} alice:[redacted] https://example.com")]
            );
        }
        assert_eq!(
            shown(&["curl", "--user=alice:short-secret", "-ubob:other-secret"]),
            ["curl", "--user=alice:[redacted]", "-ubob:[redacted]"]
        );
        // A user with no password, and -u meaning something else, stay.
        assert_eq!(
            shown(&["git", "push", "-u", "origin", "main", "--user", "alice"]),
            ["git", "push", "-u", "origin", "main", "--user", "alice"]
        );
        assert_eq!(shown(&["sort -u notes.txt"]), ["sort -u notes.txt"]);
    }

    #[test]
    fn other_credential_forms_are_redacted() {
        for flag in ["--http-password", "--proxy-password", "--auth"] {
            assert_eq!(
                shown(&["tool", flag, "short-secret", "--verbose"]),
                ["tool", flag, "[redacted]", "--verbose"]
            );
        }
        assert_eq!(
            shown(&["curl", "--header=Authorization: Bearer short-secret"]),
            ["curl", "--header=Authorization: [redacted]"]
        );
        assert_eq!(
            shown(&["curl", "-HAuthorization: Bearer short-secret"]),
            ["curl", "-HAuthorization: [redacted]"]
        );
        assert_eq!(
            shown(&[
                "rsync",
                "-a",
                "src/",
                "alice:short-secret@backup.example.com:/srv"
            ]),
            [
                "rsync",
                "-a",
                "src/",
                "alice:[redacted]@backup.example.com:/srv"
            ]
        );
        assert_eq!(
            shown(&["git clone alice:short-secret@example.com:repo && ls"]),
            ["git clone alice:[redacted]@example.com:repo && ls"]
        );
        // Not a password: an address, a host and port, an email, a scp path.
        assert_eq!(
            shown(&[
                "ssh",
                "git@example.com",
                "-p",
                "2222",
                "a@b.example",
                "git@host:org/repo"
            ]),
            [
                "ssh",
                "git@example.com",
                "-p",
                "2222",
                "a@b.example",
                "git@host:org/repo"
            ]
        );
    }

    #[test]
    fn mysql_and_sshpass_passwords_are_redacted() {
        assert_eq!(
            shown(&["mysql", "-uroot", "-pshort-secret", "app"]),
            ["mysql", "-uroot", "-p[redacted]", "app"]
        );
        // mysql's `-p NAME` prompts for the password and opens database NAME.
        assert_eq!(shown(&["mysql", "-p", "app"]), ["mysql", "-p", "app"]);
        assert_eq!(
            shown(&["/usr/bin/sshpass", "-p", "short-secret", "ssh", "host"]),
            ["/usr/bin/sshpass", "-p", "[redacted]", "ssh", "host"]
        );
        assert_eq!(
            shown(&["sshpass", "-pshort-secret", "ssh", "host"]),
            ["sshpass", "-p[redacted]", "ssh", "host"]
        );
    }
}
