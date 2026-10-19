//! Writes the plugin the relay runs as: `plugin.json`, `hooks.json` and the generated
//! `register.js`.

use super::manifest::{Shape, HOOKS};
use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

const TEMPLATE: &str = include_str!("register.js.j2");

/// A Unix socket path may be 104 bytes (macOS) or 108 (Linux); the Mods docs say near 100.
const SOCKET_PATH_LIMIT: usize = 90;
/// `<8 characters of the session id>.sock`, and the slash before it.
const SOCKET_NAME_LEN: usize = 1 + 8 + 5;

pub struct Emitted {
    pub files: Vec<PathBuf>,
    pub sock_dir: PathBuf,
    pub engine: PathBuf,
}

/// A JavaScript single-quoted string's body.
fn js_str(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out
}

fn fn_decl(hook: &super::manifest::Hook) -> String {
    let (name, key) = (hook.name, hook.key);
    match hook.shape {
        Shape::Start => String::new(),
        Shape::Gate => {
            format!("function {name}($, e, next) {{ return gated($, '{key}', e, next) }}")
        }
        Shape::Notify => {
            format!("function {name}($, e, next) {{ return told($, '{key}', e, next) }}")
        }
        Shape::Render => {
            format!("function {name}($, e, next) {{ return drawn($, '{key}', e, next) }}")
        }
        Shape::Stream => format!(
            "async function* {name}($, e, next) {{\n  const result = yield* next(e)\n  \
             await run($, '/after', {{ k: '{key}', e, result }}).catch(() => undefined)\n  \
             return result\n}}"
        ),
        Shape::Answer => format!(
            "async function {name}($, e) {{ \
             return (await run($, '/event', {{ k: '{key}', e }})).answer }}"
        ),
    }
}

/// The relay's source for one engine path, socket folder and version.
pub fn render(engine: &str, sock_dir: &str, version: &str) -> Result<String> {
    let mut env = minijinja::Environment::new();
    env.set_keep_trailing_newline(true);
    env.add_template("register.js", TEMPLATE)
        .context("compiling the relay template")?;
    let hooks: Vec<_> = HOOKS
        .iter()
        .map(|h| {
            minijinja::context! {
                name => h.name,
                event => h.event,
                matcher => h.matcher,
                fail_open => h.fail_open,
                fn_decl => fn_decl(h),
            }
        })
        .collect();
    env.get_template("register.js")?
        .render(minijinja::context! {
            engine => js_str(engine),
            sock_dir => js_str(sock_dir),
            version => js_str(version),
            hooks => hooks,
        })
        .context("rendering the relay")
}

/// The socket folder to bake in: the one asked for, else the user's, and `/tmp/csr-<uid>` when
/// the socket path in it would be too long for `sun_path`.
pub fn socket_dir(asked: Option<&Path>, home: &Path) -> PathBuf {
    let dir = asked
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.join(".claude-self-reflect/csr-companion/run"));
    if dir.as_os_str().len() + SOCKET_NAME_LEN > SOCKET_PATH_LIMIT {
        PathBuf::from(format!("/tmp/csr-{}", unsafe { libc::getuid() }))
    } else {
        dir
    }
}

fn write_file(path: &Path, text: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("{} has no folder", path.display()))?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{}.tmp", std::process::id()));
    let temp = PathBuf::from(name);
    std::fs::write(&temp, text).with_context(|| format!("writing {}", temp.display()))?;
    std::fs::rename(&temp, path).with_context(|| {
        let _ = std::fs::remove_file(&temp);
        format!("replacing {}", path.display())
    })
}

pub fn plugin_json(version: &str) -> String {
    let value = serde_json::json!({
        "name": "csr-companion",
        "version": version,
        "description": "Claude Self-Reflect beside Claude, run by the csr-engine binary",
        "author": { "name": "Rama Annaswamy" },
        "homepage": "https://github.com/ramakay/claude-self-reflect",
        "repository": "https://github.com/ramakay/claude-self-reflect",
        "license": "MIT",
        "keywords": ["memory", "side-pane", "dreams", "claude-self-reflect"],
    });
    serde_json::to_string_pretty(&value).expect("plain data") + "\n"
}

pub const HOOKS_JSON: &str = "{ \"modules\": [\"./register.js\"] }\n";

pub fn emit(dir: &Path, sock_dir: Option<&Path>, engine: Option<&Path>) -> Result<Emitted> {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let sock_dir = socket_dir(sock_dir, &home);
    let engine = match engine {
        Some(path) => path.to_path_buf(),
        None => std::env::current_exe().context("finding this binary")?,
    };
    let engine = std::path::absolute(engine)?;
    let text = |p: &Path| {
        p.to_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("{} is not UTF-8", p.display()))
    };
    let version = env!("CARGO_PKG_VERSION");
    let relay = render(&text(&engine)?, &text(&sock_dir)?, version)?;
    let files = vec![
        (dir.join(".claude-plugin/plugin.json"), plugin_json(version)),
        (dir.join("hooks/hooks.json"), HOOKS_JSON.to_string()),
        (dir.join("hooks/register.js"), relay),
    ];
    for (path, body) in &files {
        write_file(path, body)?;
    }
    Ok(Emitted {
        files: files.into_iter().map(|(path, _)| path).collect(),
        sock_dir,
        engine,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../../tests/fixtures/relay/register.js");

    pub(crate) fn fixture_render() -> String {
        render("/opt/csr/bin/csr-engine", "/run/csr", "0.0.0-fixture").unwrap()
    }

    #[test]
    fn the_relay_is_byte_equal_to_the_fixture() {
        let got = fixture_render();
        if std::env::var_os("CSR_BLESS_RELAY").is_some() {
            let path = concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/relay/register.js"
            );
            std::fs::write(path, &got).unwrap();
        } else {
            assert_eq!(
                got, FIXTURE,
                "rerun with CSR_BLESS_RELAY=1 if the change is meant"
            );
        }
    }

    #[test]
    fn every_manifest_row_is_declared_once_in_order() {
        let got = fixture_render();
        let lines: Vec<&str> = got
            .lines()
            .skip_while(|l| !l.starts_with("export function register"))
            .skip(1)
            .take_while(|l| *l != "}")
            .collect();
        assert_eq!(lines.len(), HOOKS.len());
        for (line, hook) in lines.iter().zip(HOOKS) {
            assert!(
                line.starts_with(&format!("  on('{}'", hook.event)),
                "{line}"
            );
            assert!(line.contains(&format!(", {})", hook.name)), "{line}");
            assert_eq!(line.ends_with(".catch(pass)"), hook.fail_open, "{line}");
            if let Some(matcher) = hook.matcher {
                assert!(line.contains(matcher), "{line}");
            }
        }
    }

    #[test]
    fn every_named_handler_is_defined() {
        let got = fixture_render();
        for hook in HOOKS {
            let sync = format!("function {}(", hook.name);
            let gen = format!("function* {}(", hook.name);
            assert!(got.contains(&sync) || got.contains(&gen), "{}", hook.name);
        }
    }

    #[test]
    fn paths_are_escaped_into_single_quoted_strings() {
        let got = render("/a b/it's\\x", "/run", "1").unwrap();
        assert!(got.contains("const ENGINE = '/a b/it\\'s\\\\x'\n"), "{got}");
    }

    #[test]
    fn a_long_home_falls_back_to_tmp() {
        let long = PathBuf::from(format!("/Users/{}", "x".repeat(80)));
        let uid = unsafe { libc::getuid() };
        assert_eq!(
            socket_dir(None, &long),
            PathBuf::from(format!("/tmp/csr-{uid}"))
        );
        let short = PathBuf::from("/Users/a");
        assert_eq!(
            socket_dir(None, &short),
            PathBuf::from("/Users/a/.claude-self-reflect/csr-companion/run")
        );
        assert_eq!(
            socket_dir(Some(Path::new("/private/tmp/csr-sbx/wp0/run")), &long),
            PathBuf::from("/private/tmp/csr-sbx/wp0/run")
        );
    }

    #[test]
    fn emit_writes_the_three_files() {
        let dir = tempfile::tempdir().unwrap();
        let out = emit(
            dir.path(),
            Some(Path::new("/run/csr")),
            Some(Path::new("/opt/csr-engine")),
        )
        .unwrap();
        assert_eq!(out.files.len(), 3);
        let plugin: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".claude-plugin/plugin.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(plugin["name"], "csr-companion");
        assert_eq!(plugin["version"], env!("CARGO_PKG_VERSION"));
        let hooks: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("hooks/hooks.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(hooks["modules"][0], "./register.js");
        let relay = std::fs::read_to_string(dir.path().join("hooks/register.js")).unwrap();
        assert!(relay.contains("const ENGINE = '/opt/csr-engine'"));
        assert!(relay.contains(&format!("const V = '{}'", env!("CARGO_PKG_VERSION"))));
    }
}
