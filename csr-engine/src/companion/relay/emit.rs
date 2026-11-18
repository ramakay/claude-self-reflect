//! Writes the plugin the relay runs as: `plugin.json`, `hooks.json` and the generated
//! `register.js`, and the local marketplace `csr-engine companion plugin install` hands to
//! Claude Code.

use super::manifest::{Shape, HOOKS};
use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

const TEMPLATE: &str = include_str!("register.js.j2");
const PLUGIN_TEMPLATE: &str = include_str!("plugin.json.j2");
const MARKETPLACE_TEMPLATE: &str = include_str!("marketplace.json.j2");

/// The marketplace's name, and so the id suffix of the plugin: `csr-companion@claude-self-reflect`.
pub const MARKETPLACE_NAME: &str = "claude-self-reflect";
pub const PLUGIN_NAME: &str = "csr-companion";

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
             return (await run($, '/event', {{ k: '{key}', e }}, SLOW)).answer }}"
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

/// A template whose only hole is `{{ version }}`.
fn fill_version(name: &str, template: &str, version: &str) -> String {
    let mut env = minijinja::Environment::new();
    env.set_keep_trailing_newline(true);
    env.add_template(name, template).expect("a fixed template");
    env.get_template(name)
        .and_then(|t| t.render(minijinja::context! { version => version }))
        .expect("a fixed template")
}

pub fn plugin_json(version: &str) -> String {
    fill_version("plugin.json", PLUGIN_TEMPLATE, version)
}

pub fn marketplace_json(version: &str) -> String {
    fill_version("marketplace.json", MARKETPLACE_TEMPLATE, version)
}

pub const HOOKS_JSON: &str = "{ \"modules\": [\"./register.js\"] }\n";

/// The plugin's files, by path inside the plugin folder.
fn plugin_files(
    engine: &Path,
    sock_dir: &Path,
    version: &str,
) -> Result<Vec<(&'static str, String)>> {
    let text = |p: &Path| {
        p.to_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("{} is not UTF-8", p.display()))
    };
    Ok(vec![
        (".claude-plugin/plugin.json", plugin_json(version)),
        ("hooks/hooks.json", HOOKS_JSON.to_string()),
        (
            "hooks/register.js",
            render(&text(engine)?, &text(sock_dir)?, version)?,
        ),
    ])
}

fn engine_path(engine: Option<&Path>) -> Result<PathBuf> {
    let engine = match engine {
        Some(path) => path.to_path_buf(),
        None => std::env::current_exe().context("finding this binary")?,
    };
    Ok(std::path::absolute(engine)?)
}

pub fn emit(dir: &Path, sock_dir: Option<&Path>, engine: Option<&Path>) -> Result<Emitted> {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let sock_dir = socket_dir(sock_dir, &home);
    let engine = engine_path(engine)?;
    let files = plugin_files(&engine, &sock_dir, env!("CARGO_PKG_VERSION"))?;
    for (rel, body) in &files {
        write_file(&dir.join(rel), body)?;
    }
    Ok(Emitted {
        files: files.into_iter().map(|(rel, _)| dir.join(rel)).collect(),
        sock_dir,
        engine,
    })
}

fn marketplace_root(home: &Path) -> PathBuf {
    home.join(".claude-self-reflect/csr-companion")
}

/// Where the local marketplace lives for one user.
pub fn marketplace_dir(home: &Path) -> PathBuf {
    marketplace_root(home).join("marketplace")
}

pub struct Marketplace {
    pub dir: PathBuf,
    pub plugin_dir: PathBuf,
    /// The `register.js` text the plugin folder now holds.
    pub relay: String,
}

/// Write the local marketplace under `home`: `.claude-plugin/marketplace.json` and the one
/// plugin, `plugins/csr-companion`, at the engine's version. Each file goes in atomically,
/// and the folders are private (0700).
pub fn write_marketplace(home: &Path, engine: Option<&Path>) -> Result<Marketplace> {
    let root = marketplace_root(home);
    let dir = marketplace_dir(home);
    let plugin_dir = dir.join("plugins").join(PLUGIN_NAME);
    let engine = engine_path(engine)?;
    let version = env!("CARGO_PKG_VERSION");
    let mut relay = String::new();
    let mut files = Vec::new();
    for (rel, body) in plugin_files(&engine, &socket_dir(None, home), version)? {
        if rel == "hooks/register.js" {
            relay = body.clone();
        }
        files.push((plugin_dir.join(rel), body));
    }
    files.push((
        dir.join(".claude-plugin/marketplace.json"),
        marketplace_json(version),
    ));
    for (path, body) in &files {
        super::super::private::write_atomic(path, body, &root)
            .with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(Marketplace {
        dir,
        plugin_dir,
        relay,
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

    fn bless_or_compare(got: &str, fixture: &str, name: &str) {
        if std::env::var_os("CSR_BLESS_RELAY").is_some() {
            let path = format!("{}/tests/fixtures/relay/{name}", env!("CARGO_MANIFEST_DIR"));
            std::fs::write(path, got).unwrap();
        } else {
            assert_eq!(
                got, fixture,
                "{name}: rerun with CSR_BLESS_RELAY=1 if the change is meant"
            );
        }
    }

    #[test]
    fn plugin_json_is_byte_equal_to_the_fixture() {
        let fixture = include_str!("../../../tests/fixtures/relay/plugin.json");
        bless_or_compare(&plugin_json("0.0.0-fixture"), fixture, "plugin.json");
    }

    #[test]
    fn marketplace_json_is_byte_equal_to_the_fixture() {
        let fixture = include_str!("../../../tests/fixtures/relay/marketplace.json");
        bless_or_compare(
            &marketplace_json("0.0.0-fixture"),
            fixture,
            "marketplace.json",
        );
    }

    #[test]
    fn plugin_json_carries_the_engine_version_and_no_types() {
        let got: serde_json::Value = serde_json::from_str(&plugin_json("1.2.3")).unwrap();
        assert_eq!(got["version"], "1.2.3");
        assert_eq!(got["name"], PLUGIN_NAME);
        assert!(got.get("types").is_none());
    }

    #[test]
    fn marketplace_json_names_the_one_plugin_at_the_engine_version() {
        let got: serde_json::Value = serde_json::from_str(&marketplace_json("1.2.3")).unwrap();
        assert_eq!(got["name"], MARKETPLACE_NAME);
        assert_eq!(got["plugins"].as_array().unwrap().len(), 1);
        assert_eq!(got["plugins"][0]["name"], PLUGIN_NAME);
        assert_eq!(got["plugins"][0]["source"], "./plugins/csr-companion");
        assert_eq!(got["plugins"][0]["version"], "1.2.3");
    }

    #[test]
    fn write_marketplace_lays_out_the_marketplace_privately() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let engine = Path::new("/opt/csr-engine");
        let out = write_marketplace(home.path(), Some(engine)).unwrap();
        let version = env!("CARGO_PKG_VERSION");
        assert_eq!(out.dir, marketplace_dir(home.path()));
        let read = |rel: &str| std::fs::read_to_string(out.dir.join(rel)).unwrap();
        assert_eq!(
            read(".claude-plugin/marketplace.json"),
            marketplace_json(version)
        );
        let plugin = "plugins/csr-companion";
        assert_eq!(
            read(&format!("{plugin}/.claude-plugin/plugin.json")),
            plugin_json(version)
        );
        assert_eq!(read(&format!("{plugin}/hooks/hooks.json")), HOOKS_JSON);
        assert_eq!(read(&format!("{plugin}/hooks/register.js")), out.relay);
        assert!(out.relay.contains("const ENGINE = '/opt/csr-engine'"));
        for folder in [
            out.dir.clone(),
            out.dir.join(".claude-plugin"),
            out.dir.join(plugin),
            out.dir.join(plugin).join("hooks"),
        ] {
            let mode = std::fs::metadata(&folder).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{}", folder.display());
        }
        // A second write replaces the files and leaves no temporary behind.
        write_marketplace(home.path(), Some(Path::new("/opt/moved"))).unwrap();
        assert!(
            read(&format!("{plugin}/hooks/register.js")).contains("const ENGINE = '/opt/moved'")
        );
        let leftovers = std::fs::read_dir(out.dir.join(plugin).join("hooks"))
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .count();
        assert_eq!(leftovers, 0);
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
            socket_dir(Some(Path::new("/private/tmp/csr/run")), &long),
            PathBuf::from("/private/tmp/csr/run")
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
