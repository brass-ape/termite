// SPDX-License-Identifier: MIT
//! Parsing the OpenSSH `~/.ssh/config` format — the directives M3 cares
//! about (`Host`, `HostName`, `User`, `Port`, `IdentityFile`, `ProxyJump`).
//!
//! Implemented here rather than via a crate, per `ARCHITECTURE.md` §"SSH
//! Config Parsing": the format is not complex and owning the parser gives
//! full control over which directives are supported. Directives Termite
//! doesn't know are skipped (real-world configs are full of them);
//! directives inside `Match` blocks are ignored entirely rather than
//! mis-attributed to the preceding `Host` block.
//!
//! Resolution follows OpenSSH semantics: blocks are scanned top to bottom,
//! the *first* obtained value for a setting wins, and `IdentityFile`
//! accumulates across all matching blocks.

use std::path::{Path, PathBuf};

use crate::error::SshError;

/// The settings [`SshConfig::query`] resolved for one host alias.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostConfig {
    /// `HostName` — the real host to connect to (the queried name is often
    /// just an alias).
    pub host_name: Option<String>,
    /// `User`.
    pub user: Option<String>,
    /// `Port`.
    pub port: Option<u16>,
    /// Every `IdentityFile` from every matching block, in file order, with
    /// a leading `~` expanded to the home directory.
    pub identity_files: Vec<PathBuf>,
    /// `ProxyJump`, verbatim (`[user@]host[:port]`, possibly a
    /// comma-separated chain). Kept as a string until ProxyJump support
    /// lands (M7).
    pub proxy_jump: Option<String>,
}

/// A parsed `ssh_config` file. Build one with [`SshConfig::load`] or
/// [`SshConfig::parse`], then resolve per-host settings with
/// [`SshConfig::query`].
#[derive(Debug, Default)]
pub struct SshConfig {
    blocks: Vec<Block>,
}

#[derive(Debug)]
struct Block {
    /// Patterns from the `Host` line. Empty for `Match` blocks, which this
    /// parser never matches (their criteria language is unsupported).
    patterns: Vec<Pattern>,
    directives: Vec<Directive>,
}

#[derive(Debug)]
struct Pattern {
    negated: bool,
    /// Lowercased; hostname matching is case-insensitive.
    pattern: String,
}

#[derive(Debug)]
enum Directive {
    HostName(String),
    User(String),
    Port(u16),
    IdentityFile(PathBuf),
    ProxyJump(String),
}

/// The conventional location of the user's config, `~/.ssh/config`.
/// `None` when the home directory cannot be determined.
pub fn default_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".ssh").join("config"))
}

impl SshConfig {
    /// Reads and parses the config file at `path`. A missing file is not an
    /// error — most systems have no `~/.ssh/config` — and yields an empty
    /// config that resolves nothing.
    pub fn load(path: &Path) -> Result<Self, SshError> {
        match std::fs::read_to_string(path) {
            Ok(content) => Self::parse(&content),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(SshError::Io(err)),
        }
    }

    /// Parses config text. Unknown directives are skipped; malformed lines
    /// (an unparsable `Port`, a directive missing its argument) are errors —
    /// silently misreading an auth-relevant config would be worse than
    /// refusing it.
    pub fn parse(content: &str) -> Result<Self, SshError> {
        // Directives before any Host line are global defaults; OpenSSH
        // treats them as applying to every host.
        let mut blocks = vec![Block {
            patterns: vec![Pattern {
                negated: false,
                pattern: "*".to_string(),
            }],
            directives: Vec::new(),
        }];

        for (idx, raw_line) in content.lines().enumerate() {
            let line_no = idx + 1;
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let (keyword, args) = split_keyword(line);
            let keyword = keyword.to_ascii_lowercase();
            let args = tokenize(args);
            let parse_err = |message: String| SshError::ConfigParse {
                line: line_no,
                message,
            };
            let single_arg = || -> Result<&String, SshError> {
                args.first()
                    .ok_or_else(|| parse_err(format!("`{keyword}` requires an argument")))
            };

            match keyword.as_str() {
                "host" => {
                    if args.is_empty() {
                        return Err(parse_err("`Host` requires at least one pattern".into()));
                    }
                    blocks.push(Block {
                        patterns: args
                            .iter()
                            .map(|arg| {
                                let (negated, pattern) = match arg.strip_prefix('!') {
                                    Some(rest) => (true, rest),
                                    None => (false, arg.as_str()),
                                };
                                Pattern {
                                    negated,
                                    pattern: pattern.to_ascii_lowercase(),
                                }
                            })
                            .collect(),
                        directives: Vec::new(),
                    });
                }
                // A block whose criteria this parser cannot evaluate; its
                // directives must not leak into the preceding Host block,
                // so open a block that matches nothing.
                "match" => blocks.push(Block {
                    patterns: Vec::new(),
                    directives: Vec::new(),
                }),
                "hostname" => {
                    let value = Directive::HostName(single_arg()?.clone());
                    push_directive(&mut blocks, value);
                }
                "user" => {
                    let value = Directive::User(single_arg()?.clone());
                    push_directive(&mut blocks, value);
                }
                "port" => {
                    let port = single_arg()?
                        .parse::<u16>()
                        .map_err(|_| parse_err(format!("invalid port `{}`", args[0])))?;
                    push_directive(&mut blocks, Directive::Port(port));
                }
                "identityfile" => {
                    let path = expand_tilde(single_arg()?);
                    push_directive(&mut blocks, Directive::IdentityFile(path));
                }
                "proxyjump" => {
                    let value = Directive::ProxyJump(single_arg()?.clone());
                    push_directive(&mut blocks, value);
                }
                _ => {}
            }
        }

        Ok(Self { blocks })
    }

    /// Resolves the settings for `host` (the name the user typed — an alias
    /// or real hostname; matching is case-insensitive).
    pub fn query(&self, host: &str) -> HostConfig {
        let host = host.to_ascii_lowercase();
        let mut resolved = HostConfig::default();

        for block in &self.blocks {
            if !block_matches(block, &host) {
                continue;
            }
            for directive in &block.directives {
                match directive {
                    Directive::HostName(value) => {
                        resolved.host_name.get_or_insert_with(|| value.clone());
                    }
                    Directive::User(value) => {
                        resolved.user.get_or_insert_with(|| value.clone());
                    }
                    Directive::Port(value) => {
                        resolved.port.get_or_insert(*value);
                    }
                    Directive::IdentityFile(path) => resolved.identity_files.push(path.clone()),
                    Directive::ProxyJump(value) => {
                        resolved.proxy_jump.get_or_insert_with(|| value.clone());
                    }
                }
            }
        }

        resolved
    }
}

/// Appends to the most recent block. `parse` seeds `blocks` with the global
/// block, so a last block always exists; the `if let` avoids panicking on
/// that invariant per the no-`expect` rule.
fn push_directive(blocks: &mut [Block], directive: Directive) {
    if let Some(block) = blocks.last_mut() {
        block.directives.push(directive);
    }
}

/// OpenSSH negation semantics: a negated pattern matching vetoes the whole
/// block, and at least one positive pattern must match.
fn block_matches(block: &Block, host: &str) -> bool {
    let mut positive_match = false;
    for pattern in &block.patterns {
        if glob_matches(&pattern.pattern, host) {
            if pattern.negated {
                return false;
            }
            positive_match = true;
        }
    }
    positive_match
}

/// `fnmatch`-style matching with `*` (any run) and `?` (any one char),
/// as used by ssh_config Host patterns. Iterative with single-star
/// backtracking; both sides are already lowercased.
fn glob_matches(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();
    let (mut p, mut t) = (0, 0);
    let mut backtrack: Option<(usize, usize)> = None;

    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            backtrack = Some((p, t));
            p += 1;
        } else if let Some((star_p, star_t)) = backtrack {
            backtrack = Some((star_p, star_t + 1));
            p = star_p + 1;
            t = star_t + 1;
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&byte| byte == b'*')
}

/// Splits a config line into its keyword and the remainder. The separator
/// is whitespace or an optional `=` (`Port 22`, `Port=22`, `Port = 22`).
fn split_keyword(line: &str) -> (&str, &str) {
    let end = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let keyword = &line[..end];
    let rest = line[end..].trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest).trim_start();
    (keyword, rest)
}

/// Splits arguments on whitespace, honoring double quotes (for values with
/// spaces, e.g. `IdentityFile "~/my keys/id_ed25519"`).
fn tokenize(args: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for c in args.chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Expands a leading `~/` (or bare `~`) to the home directory. Left
/// untouched when the home directory cannot be determined.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    } else if path == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_a_plain_host_block() {
        let config = SshConfig::parse(
            "Host work\n\
             \tHostName gitlab.internal.example.com\n\
             \tUser deploy\n\
             \tPort 2222\n\
             \tIdentityFile /keys/id_ed25519\n\
             \tProxyJump bastion.example.com\n",
        )
        .unwrap();

        let resolved = config.query("work");
        assert_eq!(
            resolved.host_name.as_deref(),
            Some("gitlab.internal.example.com")
        );
        assert_eq!(resolved.user.as_deref(), Some("deploy"));
        assert_eq!(resolved.port, Some(2222));
        assert_eq!(
            resolved.identity_files,
            vec![PathBuf::from("/keys/id_ed25519")]
        );
        assert_eq!(resolved.proxy_jump.as_deref(), Some("bastion.example.com"));

        assert_eq!(config.query("other-host"), HostConfig::default());
    }

    #[test]
    fn first_obtained_value_wins_and_identity_files_accumulate() {
        let config = SshConfig::parse(
            "Host web-1\n\
             \tUser alice\n\
             \tIdentityFile /keys/web\n\
             Host web-*\n\
             \tUser bob\n\
             \tPort 2200\n\
             \tIdentityFile /keys/fleet\n",
        )
        .unwrap();

        let resolved = config.query("web-1");
        // `User alice` came first, so `bob` must not override it; Port only
        // appears in the second block, so it still applies.
        assert_eq!(resolved.user.as_deref(), Some("alice"));
        assert_eq!(resolved.port, Some(2200));
        assert_eq!(
            resolved.identity_files,
            vec![PathBuf::from("/keys/web"), PathBuf::from("/keys/fleet")]
        );
    }

    #[test]
    fn global_directives_apply_to_every_host() {
        let config = SshConfig::parse(
            "IdentityFile /keys/default\n\
             Host special\n\
             \tUser root\n",
        )
        .unwrap();

        assert_eq!(
            config.query("anything").identity_files,
            vec![PathBuf::from("/keys/default")]
        );
        assert_eq!(config.query("special").user.as_deref(), Some("root"));
    }

    #[test]
    fn wildcard_and_negation_patterns() {
        let config = SshConfig::parse(
            "Host *.example.com !bad.example.com\n\
             \tUser fleet\n",
        )
        .unwrap();

        assert_eq!(config.query("a.example.com").user.as_deref(), Some("fleet"));
        // Case-insensitive, as OpenSSH matches hostnames.
        assert_eq!(config.query("A.EXAMPLE.COM").user.as_deref(), Some("fleet"));
        assert_eq!(config.query("bad.example.com").user, None);
        assert_eq!(config.query("example.com").user, None);
    }

    #[test]
    fn question_mark_matches_exactly_one_character() {
        let config = SshConfig::parse("Host db?\n\tPort 5432\n").unwrap();

        assert_eq!(config.query("db1").port, Some(5432));
        assert_eq!(config.query("db12").port, None);
        assert_eq!(config.query("db").port, None);
    }

    #[test]
    fn equals_separator_quotes_and_comments() {
        let config = SshConfig::parse(
            "# global comment\n\
             Host = files\n\
             \tPort=2222\n\
             \tIdentityFile \"~/my keys/id_ed25519\"\n\
             \n\
             \t# indented comment\n",
        )
        .unwrap();

        let resolved = config.query("files");
        assert_eq!(resolved.port, Some(2222));
        let expected = dirs::home_dir()
            .expect("test environment has a home directory")
            .join("my keys/id_ed25519");
        assert_eq!(resolved.identity_files, vec![expected]);
    }

    #[test]
    fn unknown_directives_are_skipped_and_match_blocks_are_inert() {
        let config = SshConfig::parse(
            "Host work\n\
             \tForwardAgent yes\n\
             \tUser deploy\n\
             Match user deploy\n\
             \tPort 9999\n",
        )
        .unwrap();

        let resolved = config.query("work");
        assert_eq!(resolved.user.as_deref(), Some("deploy"));
        // The Port belongs to the (unsupported) Match block, and must not
        // leak into `Host work`.
        assert_eq!(resolved.port, None);
    }

    #[test]
    fn malformed_port_is_an_error_with_the_line_number() {
        let err = SshConfig::parse("Host work\n\tPort not-a-number\n").unwrap_err();
        match err {
            SshError::ConfigParse { line, .. } => assert_eq!(line, 2),
            other => panic!("expected ConfigParse, got {other:?}"),
        }
    }

    #[test]
    fn missing_file_loads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let config = SshConfig::load(&dir.path().join("does-not-exist")).unwrap();
        assert_eq!(config.query("any"), HostConfig::default());
    }
}
