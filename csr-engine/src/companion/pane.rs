use super::render::observation::{self, ObsCard, PopupAt};
use super::{
    array, carry, cli::PaneArgs, db::Db, dreams, feed::Feed, infinite, layout, masthead, num,
    pages, paths::Paths, render::text::*, send, session::Session, string, threads, value,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub fn legend(
    view: &str,
    width: usize,
    reload: Option<u64>,
) -> (String, Vec<(usize, usize, String)>) {
    legend_parts(view, width, reload, Layout::default())
}
/// How the pane is laid out for the legend: on a phone, in column mode (a pane narrower than
/// `COLUMN_UNDER`), or zoomed to a phone's whole screen.
#[derive(Clone, Copy, Default)]
pub struct Layout {
    pub phone: bool,
    pub column: bool,
    pub zoomed: bool,
}
/// The bottom row and where each label sits on it: (text, [(first col, past last col, key)]).
/// On a phone the main page's keys start short, one word each, and f goes; zoomed, `z split`
/// gives the screen back to Claude. In column mode, the keys alone.
pub fn legend_parts(
    view: &str,
    width: usize,
    reload: Option<u64>,
    layout: Layout,
) -> (String, Vec<(usize, usize, String)>) {
    let Layout {
        phone,
        column,
        zoomed,
    } = layout;
    if column {
        let mut keys: Vec<&str> = match view {
            "main" => vec!["l", "s", "i", "q"],
            "summary" | "threads" => vec!["d", "j/k", "q"],
            _ => vec!["d", "q"],
        };
        while keys.len() > 1 && 1 + keys.join(" ").chars().count() > width {
            keys.remove(keys.len() - 2);
        }
        let mut at = 1;
        let spans = keys
            .iter()
            .map(|key| {
                let span = (at, at + len(key), (*key).to_owned());
                at += len(key) + 1;
                span
            })
            .collect();
        let shown = keys
            .iter()
            .map(|key| format!("{BOLD}{}{key}{OFF}", ink("you")))
            .collect::<Vec<_>>()
            .join(" ");
        return (format!(" {shown}"), spans);
    }
    let mut words: Vec<(String, String)> = if view == "main" && phone {
        let mut keys = vec![("l", "changes"), ("s", "summary"), ("i", "figure")];
        if zoomed {
            keys.push(("z", "split"));
        }
        keys.push(("q", "close"));
        keys
    } else if view == "main" {
        vec![
            ("l", "changes since your sessions"),
            ("s", "compaction summary"),
            ("f", "every lost thread"),
            ("i", "figure"),
            ("q", "close"),
        ]
    } else if matches!(view, "summary" | "threads") {
        vec![("d", "back"), ("j/k", "scroll"), ("q", "close")]
    } else {
        vec![("d", "back"), ("q", "close")]
    }
    .into_iter()
    .map(|(a, b)| (a.into(), b.into()))
    .collect();
    if let Some(seconds) = reload.filter(|_| view == "main") {
        let s = format!("↻ {seconds}s");
        let s = format!("{s}{}", " ".repeat(5_usize.saturating_sub(len(&s))));
        let n = words.len() - 1;
        words.insert(n, ("r".into(), s));
    }
    let wide = |words: &[(String, String)], gap: usize, marks: &str| {
        len(marks)
            + words
                .iter()
                .map(|(k, w)| len(k) + len(w) + 1 + gap)
                .sum::<usize>()
            - gap
            + 1
    };
    let mut marks = if matches!(view, "main" | "summary" | "threads") {
        ""
    } else {
        "~ changed  - removed  + back   "
    }
    .to_owned();
    if view == "main" && !phone {
        for (i, label) in [
            (0, "changes since your"),
            (0, "changes since"),
            (0, "changes"),
            (1, "summary"),
            (2, "thread"),
            (3, "figure"),
        ] {
            if wide(&words, 2, &marks) <= width {
                break;
            }
            words[i].1 = label.into();
        }
    }
    let sep = if if view == "main" {
        wide(&words, 3, &marks) <= width
    } else {
        len(&marks)
            + words
                .iter()
                .map(|(k, w)| len(k) + len(w) + 4)
                .sum::<usize>()
            <= width
    } {
        "   "
    } else {
        "  "
    };
    let total = |words: &[(String, String)], marks: &str| {
        1 + len(marks)
            + words
                .iter()
                .map(|(k, w)| len(k) + 1 + len(w))
                .sum::<usize>()
            + len(sep) * words.len().saturating_sub(1)
    };
    if total(&words, &marks) > width {
        marks.clear();
    }
    while words.len() > 1 && total(&words, &marks) > width {
        let i = ["f", "s", "i", "r", "j/k"]
            .iter()
            .find_map(|name| {
                words
                    .iter()
                    .enumerate()
                    .find(|(i, (key, _))| key == name && *i > 0 && *i < words.len() - 1)
                    .map(|(i, _)| i)
            })
            .unwrap_or(words.len() - 2);
        words.remove(i);
    }
    if total(&words, &marks) > width {
        let (key, said) = words[0].clone();
        words = vec![(
            key.clone(),
            fit(&said, width.saturating_sub(len(&key) + 2).max(1)),
        )];
    }
    let shown = words
        .iter()
        .map(|(key, said)| format!("{BOLD}{}{key}{OFF}{} {said}{OFF}", ink("you"), ink("faint")))
        .collect::<Vec<_>>()
        .join(sep);
    let mut at = 1 + len(&marks);
    let spans = words
        .iter()
        .map(|(key, said)| {
            let start = at;
            let end = at + len(key) + 1 + len(said);
            at = end + len(sep);
            (start, end, key.clone())
        })
        .collect();
    (format!(" {}{marks}{OFF}{shown}", ink("faint")), spans)
}

/// What the last instrument frame drew, kept under the key of everything it drew from. The same
/// key means the same pixels, placements, hit rows and metrics, so none is rebuilt.
struct Instrument {
    key: u64,
    rows: Vec<String>,
    placed: Vec<super::render::kitty::Placement>,
    acts: BTreeMap<usize, Value>,
    metrics: Option<super::render::instrument::Metrics>,
    more: Vec<Value>,
    hits: BTreeMap<usize, (ObsCard, usize)>,
    story: Option<usize>,
}
/// The observation item the popup shows: its card, the item's line (its identity, since a new look
/// shifts places), the place it had, whether its text was just copied, and what the last `s` said.
#[derive(Clone)]
struct Popup {
    card: ObsCard,
    line: String,
    index: usize,
    copied: bool,
    note: Option<observation::Note>,
}
/// Account reads borrowed while the session is also borrowed.
struct Wide<'a> {
    paths: &'a Paths,
    db: Option<&'a Db>,
    persist: bool,
}

pub struct Pane {
    paths: Paths,
    persist: bool,
    exporting: bool,
    db: Option<Db>,
    root: std::path::PathBuf,
    limit: usize,
    sid: String,
    agent: Option<String>,
    session: Option<Session>,
    pub view: String,
    pub theme: super::render::theme::Theme,
    pub instrument_open: super::render::instrument::Open,
    pub instrument_metrics: Option<super::render::instrument::Metrics>,
    pub open: Value,
    pub sent: Value,
    folds: Vec<Value>,
    feed: Feed,
    dreams: dreams::DreamLog,
    pub acts: BTreeMap<usize, Value>,
    pub top: usize,
    pub threads_top: usize,
    pub summary_top: usize,
    spend: Option<value::Spend>,
    narrative: Option<(Value, f64, Value)>,
    tall: bool,
    anchor: Option<(String, usize)>,
    pub loaded: f64,
    shifts: Vec<Value>,
    total: usize,
    hold: Option<(Value, f64)>,
    pub pixels: bool,
    /// Pixel mode draws cards and the pill instead of the dark ramp's text; set by `enable_cards`.
    pub cards: bool,
    /// Draw the Context card as the older rings and circles, not the dashboard. Set from
    /// `CSR_PANE_RINGS=1`.
    pub rings: bool,
    /// The pill the last frame drew in place of the masthead, for tests and the PNG export.
    pub pill: Option<super::render::cards::Pill>,
    pub placed: Vec<super::render::kitty::Placement>,
    picture_cache: super::render::pictures::Cache,
    pub session_picture: Option<super::render::raster::SessionFacts>,
    pub last_picture: Option<super::render::raster::LastFacts>,
    map: super::shiftmap::MapState,
    pub phone: super::phone::Phone,
    /// This pane in herdr; set by the watch loop, absent in frame and test runs.
    pub herdr: Option<super::phone::Herdr>,
    // the Codex rollouts, the readers behind "How Claude read you", and the dream
    // answer the pane keeps in place of the answer rows for six seconds (dream, answer, when)
    codex: super::codex_runs::CodexRolloutIndex,
    /// The rollouts' fingerprint and when it was taken: walking them is not done every probe.
    codex_look: std::cell::Cell<Option<(f64, u64)>>,
    readers: super::readings::Readers,
    pub answered: Option<(String, String, f64)>,
    /// Column mode's gauge strip: (first column, first row past it); a click there is on no row.
    edge: Option<(usize, usize)>,
    /// The session before this one, read once off the draw path (`carry.rs`).
    carry: carry::Carry,
    carry_gate: Option<std::sync::mpsc::Receiver<()>>,
    /// A compaction's marker was there at the last frame.
    compacting: bool,
    /// Visible notice when the dreams-off offer could not write settings.
    offer_notice: Option<String>,
    popup: Option<Popup>,
    /// The lines of each observation card at the last frame, `[confusing, should_be]`.
    obs_lines: [Vec<String>; 3],
    /// The rows the last frame drew with a trailing "…", each `{title, line}` in full: what a click
    /// on such a row opens.
    more: Vec<Value>,
    /// The item each terminal row opens at the last frame: of the picture rows that share a
    /// terminal row, the one with the most of it. `acts` keeps one act per row and cannot hold
    /// them all.
    hits: BTreeMap<usize, (ObsCard, usize)>,
    /// Where the story is in `more`, when the last frame drew one.
    story: Option<usize>,
    /// The observation items dismissed with `x` and not yet read back from the likes log.
    dismissed: BTreeSet<String>,
    /// What `c` copies for the popup at the last frame.
    popup_copy: Option<String>,
    /// The popup `popup_copy` and `popup_send` were made for. A key that arrives before the frame
    /// of a new popup is drawn must not copy or type the one before it.
    popup_for: Option<(ObsCard, String)>,
    /// What `s` types into Claude's prompt for it, unsent.
    popup_send: Option<String>,
    /// Text for the watch loop to put on the clipboard.
    copy_out: Option<String>,
    instrument_kept: Option<Instrument>,
    gate: super::gate::Gate,
    /// When the picture next changes with no new data, as the last instrument frame saw it.
    pub next_change: Option<f64>,
    /// Pictures rasterised, PNGs encoded and frames served from the kept one, since the start.
    pub rasters: usize,
    pub encodes: usize,
    pub reused: usize,
}
/// The card and place an `observe:<card>:<index>` act names.
fn observe_key(key: &str) -> Option<(ObsCard, usize)> {
    let (card, index) = key.strip_prefix("observe:")?.split_once(':')?;
    let card = match card.parse::<usize>().ok()? {
        0 => ObsCard::Confusing,
        1 => ObsCard::ShouldBe,
        _ => ObsCard::More,
    };
    Some((card, index.parse().ok()?))
}
/// How long the narrative's week query is kept.
const WEEK_EVERY: f64 = 60.;
/// The gate's name for the rollout files' fingerprint.
const CODEX_ROLLOUTS: &str = "codex rollouts";
impl Pane {
    pub fn new(paths: Paths, args: &PaneArgs) -> Self {
        let sid = args.sid.clone().unwrap_or_default();
        let db = Db::open(&paths).ok();
        let root = db
            .as_ref()
            .and_then(|db| db.pane_root(&args.repo))
            .or_else(|| {
                super::paths::git(&args.repo, &["rev-parse", "--show-toplevel"]).map(Into::into)
            })
            .unwrap_or_else(|| args.repo.canonicalize().unwrap_or(args.repo.clone()));
        let feed = Feed::new(&paths, &sid);
        let pixels = super::render::kitty::pixels_enabled();
        let picture_cache = if pixels {
            super::render::pictures::Cache::with_fonts(super::sources::picture_fonts())
        } else {
            super::render::pictures::Cache::default()
        };
        let codex = super::codex_runs::CodexRolloutIndex::new(&paths.home);
        Self {
            paths,
            persist: !args.frame && args.png.is_none(),
            exporting: args.png.is_some(),
            db,
            root,
            limit: args.limit,
            session: (!sid.is_empty()).then(|| Session::new(&sid)),
            sid,
            agent: args.agent.clone(),
            view: args.page.clone(),
            theme: super::render::theme::Theme::from_env(),
            instrument_open: super::render::instrument::Open::default(),
            instrument_metrics: None,
            open: Value::Null,
            sent: Value::Null,
            folds: vec![],
            feed,
            dreams: dreams::DreamLog::default(),
            acts: BTreeMap::new(),
            top: 0,
            threads_top: 0,
            summary_top: 0,
            spend: None,
            narrative: None,
            tall: false,
            anchor: None,
            loaded: 0.,
            shifts: vec![],
            total: 0,
            hold: None,
            pixels,
            cards: false,
            rings: std::env::var("CSR_PANE_RINGS").is_ok_and(|v| v == "1"),
            pill: None,
            placed: vec![],
            picture_cache,
            session_picture: None,
            last_picture: None,
            map: Default::default(),
            phone: Default::default(),
            herdr: None,
            codex,
            codex_look: std::cell::Cell::new(None),
            readers: Default::default(),
            answered: None,
            edge: None,
            carry: Default::default(),
            carry_gate: None,
            compacting: false,
            offer_notice: None,
            popup: None,
            obs_lines: Default::default(),
            more: vec![],
            hits: BTreeMap::new(),
            story: None,
            dismissed: BTreeSet::new(),
            popup_copy: None,
            popup_for: None,
            popup_send: None,
            copy_out: None,
            instrument_kept: None,
            gate: Default::default(),
            next_change: None,
            rasters: 0,
            encodes: 0,
            reused: 0,
        }
    }
    /// Draw cards where pixels and a system font allow; text mode is left byte for byte as it was.
    pub fn enable_cards(&mut self) {
        self.cards = self.pixels && super::render::cards::fonts().is_some();
    }
    /// Draw the page. Everything the frame reads is read now; what it read, and what it left
    /// running, is kept for `due`.
    pub fn frame(&mut self, cols: usize, rows: usize, now: f64) -> Vec<String> {
        // Codex removed since the last scan: its scan state goes too, or the gate would find the
        // files (none) different from the scan's (the old ones) at every check.
        if !self.codex.present() {
            self.codex.forget();
        }
        // Looked at first, so a change made while the frame reads is seen by the next check.
        let (mut snapshot, mut version) = self.look(now);
        let found = self.session.as_ref().and_then(|s| s.path.clone());
        let out = self.build_frame(cols, rows, now);
        // What the frame holds of Codex is what its last scan read, which can be older than the
        // files now (a scan waits two seconds): the gate keeps asking until a scan catches up.
        snapshot = snapshot.including(CODEX_ROLLOUTS, self.codex.scanned());
        // The first frame finds the transcript: it was not in the first look.
        if self.session.as_ref().and_then(|s| s.path.clone()) != found {
            (snapshot, version) = self.look(now);
            snapshot = snapshot.including(CODEX_ROLLOUTS, self.codex.scanned());
        }
        let settled = self.view == "main" && self.db.is_some() && !self.busy();
        self.gate.saw(snapshot, version, now, settled);
        out
    }
    /// The rollouts' fingerprint, taken at most as often as the Codex scan itself runs.
    fn codex_fingerprint(&self, now: f64) -> u64 {
        if let Some((at, print)) = self.codex_look.get() {
            if (0. ..super::codex_runs::EVERY).contains(&(now - at)) {
                return print;
            }
        }
        let print = self.codex.fingerprint(now);
        self.codex_look.set(Some((now, print)));
        print
    }
    fn look(&self, now: f64) -> (super::gate::Snapshot, Option<i64>) {
        let transcript = self.session.as_ref().and_then(|s| s.path.as_deref());
        (
            // Codex's rollouts feed the weekly figure and live outside everything else the
            // snapshot covers; the Claude transcripts the spend reads are listed once a minute
            // by the spend itself, a longer wait than the gate's floor.
            super::gate::Snapshot::take(&self.paths, &self.sid, transcript)
                .including(CODEX_ROLLOUTS, self.codex_fingerprint(now)),
            self.db.as_ref().and_then(Db::version),
        )
    }
    /// Statements the pane has run on its database since it started.
    pub fn queries(&self) -> usize {
        self.db.as_ref().map_or(0, Db::queries)
    }
    /// Work still in flight: its progress shows with no file or row changing.
    fn busy(&self) -> bool {
        self.spend.as_ref().is_some_and(|s| !s.ready)
            || (self.carry.started() && self.carry.done().is_none())
            || self.map.refreshing()
            || self.codex.behind()
            || self.compacting
    }
    /// The first moment the drawing changes by itself: what the instrument frame named, the
    /// six-second windows of a sent or answered row, the Shift Map's fold, and the minute the
    /// week's own sources are read again.
    fn deadline(&self) -> Option<f64> {
        [
            self.next_change,
            self.sent[2].as_f64().map(|at| at + 6.),
            self.answered.as_ref().map(|a| a.2 + 6.),
            self.map.due,
            // What the week shows from sources the gate does not watch is read again this
            // often, as it was when every second drew: the narrative's query, and the listing of
            // the transcripts the spend adds up.
            self.narrative.as_ref().map(|n| n.1 + WEEK_EVERY),
            self.spend.as_ref().and_then(value::Spend::next_listing),
        ]
        .into_iter()
        .flatten()
        .min_by(f64::total_cmp)
    }
    /// Whether a frame at `now` would read or draw anything the last one did not: Gate 1. The
    /// watch loop asks every pass and draws when this is true, a key or click changed the page,
    /// or a wake came.
    pub fn due(&mut self, now: f64) -> bool {
        let deadline = self.deadline();
        let mut gate = std::mem::take(&mut self.gate);
        let due = gate.due(now, deadline, || self.look(now));
        self.gate = gate;
        due
    }
    /// What the page is showing of the pane's own state: a key or click that leaves it as it was
    /// has nothing to draw.
    pub fn ui_mark(&self) -> String {
        format!(
            "{:?}",
            (
                &self.view,
                self.instrument_open,
                self.theme,
                self.popup
                    .as_ref()
                    .map(|p| (p.card as usize, &p.line, p.copied, p.note)),
                &self.open,
                &self.sent,
                &self.answered,
                &self.folds,
                (self.top, self.threads_top, self.summary_top, self.tall),
                (self.phone.zoomed, self.hold.is_some(), self.loaded),
                &self.offer_notice,
            )
        )
    }
    fn build_frame(&mut self, cols: usize, rows: usize, now: f64) -> Vec<String> {
        self.placed.clear();
        self.hits.clear();
        let cols = cols.max(1);
        let rows = rows.max(1);
        if self.db.is_none() {
            self.db = Db::open(&self.paths).ok();
            if self.db.is_none() {
                self.acts.clear();
                let said = if self.paths.db.exists() {
                    "could not read csr-engine"
                } else {
                    "CSR is not set up yet: npx claude-self-reflect setup"
                };
                let mut out = vec![format!("{BOLD}CSR{OFF}")];
                out.extend(
                    wrap(said, cols.saturating_sub(1).max(1), 4)
                        .into_iter()
                        .map(|line| format!("{DIM}{line}{OFF}")),
                );
                out.truncate(rows);
                out.resize(rows, String::new());
                return out;
            }
        }
        let phone = self.phone.read(self.herdr.as_ref(), cols, rows, now);
        let column = cols < super::phone::COLUMN_UNDER;
        self.phone.settle(self.herdr.as_ref(), column, phone);
        // column mode's strip takes the pane's last columns; the pages draw left of it
        let tapes = layout::strip_tapes(cols, rows, column);
        let body_cols = cols - layout::strip_width(tapes);
        if self.sent[2].as_f64().is_some_and(|at| now - at > 6.) {
            if self.open == self.sent[0] {
                self.open = Value::Null;
            }
            self.sent = Value::Null;
        }
        if self.answered.as_ref().is_some_and(|a| now - a.2 > 6.) {
            self.answered = None;
        }

        if self.loaded == 0. || now - self.loaded >= 60. {
            self.shifts = self
                .db
                .as_ref()
                .map(|db| db.shifts(&self.root, usize::MAX))
                .unwrap_or_default();
            self.total = self.shifts.len();
            self.shifts.truncate(self.limit);
            self.loaded = now;
            self.map.cost = self.db.as_ref().and_then(super::shiftmap::story_cost);
        }
        if let Some(db) = self.db.as_ref() {
            self.map.load(db, &self.paths, &self.sid);
        }
        let (refreshing, refresh_note) = self.map.poll(now);
        self.dreams.poll(&self.paths);
        let records = &self.dreams.records;
        let root = self.root.to_string_lossy().into_owned();
        let kept = masthead::kept_status(&self.paths, &self.sid, now);
        let limits = masthead::account_limits(&self.paths, now);
        // The windows the masthead shows: this session's own figures, else the account's.
        let rates = if kept["fields"]["rate_limits"]
            .as_object()
            .is_some_and(|v| !v.is_empty())
        {
            &kept["fields"]["rate_limits"]
        } else {
            &limits
        };
        if self.persist {
            if let Some(at) = kept["at"].as_f64() {
                super::forecast::record(&self.paths.keep, at, &kept["fields"]["rate_limits"]);
            }
        }
        let forecast = super::forecast::outlook(&self.paths.keep, rates, (now / 60.).floor() * 60.);
        let story = self
            .db
            .as_ref()
            .map(|db| pages::session_story(db, &self.paths, &self.sid))
            .unwrap_or(Value::Null);
        let mut data = json!({
            "story_map": self.map.shown,
            "story_map_from": self.map.from,
            "story_map_messages": self.map.messages,
            "refreshing": refreshing,
            "refresh_note": refresh_note,
            "story_cost": self.map.cost,
            "repo": root,
            "shifts": self.shifts,
            "total": self.total,
            "dreams": self.dreams.dreams(&root),
            "chapter": dreams::last_chapter(records, &root, &self.sid),
            "look": dreams::last_look(records, &root),
            "dream_records": records,
            "dreams_on": dreams::dreams_on(&self.paths),
            "story": story,
            "claude_fields": kept["fields"],
            "claude_at": kept["at"],
            "limits": limits,
            "forecast": forecast,
            "rings": self.rings,
            "sts": masthead::csr_status_at(&self.paths, now),
            "compacting": masthead::compacting_info(
                &self.paths,
                &self.sid,
                now,
                self.persist,
            ),
        });
        if let Some(session) = self.session.as_mut() {
            session.read_transcript(&self.paths);
        }
        let summary = match (&self.session, self.view.as_str()) {
            (Some(session), "summary") => threads::newest_summary(session.path.as_deref()),
            _ => Value::Null,
        };
        let can_refresh = self
            .session
            .as_ref()
            .is_some_and(|session| session.path.is_some())
            && super::shiftmap::engine_binary().is_some();
        let mut empty = json!({"id":null});
        if let Some(session) = self.session.as_mut() {
            session.load(&self.paths, self.db.as_ref());
        }
        let project = self.carry_step();
        let dismissed = self.dismissed_keys();
        let s = self
            .session
            .as_mut()
            .map(|s| &mut s.data)
            .unwrap_or(&mut empty);
        if s["last_session"].is_null() {
            if let Some(done) = self.carry.done() {
                s["last_session"] = done.facts.clone();
            }
        }
        s["lost"] = if string(&s["id"]).is_empty() {
            json!([])
        } else {
            carry::lost_of(&self.paths, s, project.as_deref(), self.carry.threads())
        };
        let every = if self.instrument_open.threads || self.view == "threads" {
            array(&s["lost"]).to_vec()
        } else {
            vec![]
        };
        // whether the session before this one still runs: kept apart from its facts, which the
        // picture reads
        s["last_running"] = json!(s["last_session"]["id"]
            .as_str()
            .is_some_and(|id| { super::session::session_record(&self.paths, id).is_some() }));
        if s["last_session"]["todos"].is_array() {
            s["todos"] = s["last_session"]["todos"].clone();
        }

        self.session_picture = (!string(&s["id"]).is_empty())
            .then(|| super::render::raster::SessionFacts::from_value(s, now));
        self.last_picture = s["last_session"]
            .is_object()
            .then(|| super::render::raster::LastFacts::from_value(&s["last_session"]));
        // the mod's plan and what CSR's compactions did: the main page's card and masthead rows,
        // the single instrument's tally
        data["infinite"] = infinite::infinite_info(&self.paths, &self.sid, now, s);
        // the marker went: draw at once, not from what the page last held
        let running = data["compacting"].is_object();
        if self.compacting && !running {
            self.hold = None;
        }
        self.compacting = running;
        data["map_view"] = match self.map.motion(
            now,
            super::shiftmap::map_open(&self.open),
            s["busy"] == true,
        ) {
            Some(open) => json!({ "open": open }),
            None => Value::Null,
        };
        data["can_refresh"] = json!(can_refresh);
        if self.view == "main" {
            // In flight (agents, Codex runs) and How Claude read you
            data["agents"] = json!(super::agents::agents_of(&self.paths, &self.sid));
            data["codex"] = if self.codex.present() {
                self.codex.refresh(now)
            } else {
                Value::Null
            };
            data["readings"] = json!(self.readers.how_claude_read(&self.paths, &self.sid, 2, now));
        }
        if self.view == "main" {
            Self::week_data(
                &Wide {
                    paths: &self.paths,
                    db: self.db.as_ref(),
                    persist: self.persist,
                },
                (&mut self.narrative, &mut self.spend),
                &mut data,
                now,
            );
            self.phone.foot = Some((phone, column));
            self.edge = None;
            data["threads"] = json!(every);
            data["learnt"] = json!(self
                .db
                .as_ref()
                .map(|db| db.learnt_counts(string(&s["project"]), &self.sid, now))
                .unwrap_or_default());
            let mut data = data;
            data["dismissed"] = dismissed;
            if let Some(notice) = &self.offer_notice {
                data["offer_notice"] = json!(notice);
            }
            if array(&s["main"]).is_empty()
                && array(&s["prompts"]).is_empty()
                && masthead::ctx_now(&data["claude_fields"], data["claude_at"].as_f64(), s)
                    .0
                    .is_none_or(|n| n <= 0.)
            {
                let project = s["project"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .or_else(|| super::paths::csr_project(&root))
                    .unwrap_or("");
                data["repo_recap"] = json!(self
                    .db
                    .as_ref()
                    .and_then(|db| db.repo_recap(project, &self.sid, now)));
            }
            let composition = super::render::instrument::Composition::build(&data, s, now);
            return self.instrument_frame(&composition, cols, rows);
        }
        let cards_on = self.pixels && self.cards;
        let cell = super::render::kitty::cell_size();
        let ui = json!({
            "answered": self.answered,
            "pixels": self.pixels,
            "cards": cards_on,
            "cell": [cell.0, cell.1],
            "csr": array(&data["infinite"]["csr_at"]).len(),
            "dreams": if cards_on {
                match super::render::cards::dream_facts(&data, &self.sid) {
                    Some(super::render::cards::Card::WhatIf { count, .. }) => Some(count),
                    _ => None,
                }
            } else {
                None
            },
            "rows": rows,
            "cols": cols,
            "phone": phone,
            "column": column,
            "now": now,
            "open": self.open,
            "sent": self.sent,
            "folds": self.folds,
            "likes": if self.exporting { Value::Null } else { send::read_likes(&self.paths) },
        });
        // the value row above the main page's legend (not in column mode, not in an export)
        let value = if self.view == "main" && !column && !self.exporting {
            Self::week_data(
                &Wide {
                    paths: &self.paths,
                    db: self.db.as_ref(),
                    persist: self.persist,
                },
                (&mut self.narrative, &mut self.spend),
                &mut data,
                now,
            );
            layout::value_rows(&data["value"], cols - 1)
        } else {
            vec![]
        };
        let room = rows.saturating_sub(value.len());
        let extra = matches!(self.view.as_str(), "summary" | "threads");
        let page = if extra {
            Value::Null
        } else if self.view == "log" {
            pages::page_log(&data)
        } else {
            let page = pages::page_main(&data, s, now, &ui, self.db.as_ref());
            if self.exporting || column {
                page
            } else if s["busy"] == true && self.hold.as_ref().is_some_and(|(_, at)| now - at < 120.)
            {
                let mut page = page;
                // In flight is never held; it is drawn fresh among the held ones
                let fresh: Vec<Value> = array(&page["sections"])
                    .iter()
                    .filter(|s| s["name"] == "flight" && !array(&s["items"]).is_empty())
                    .cloned()
                    .collect();
                let mut sections: Vec<Value> =
                    super::feed::living(array(&self.hold.as_ref().unwrap().0).to_vec(), now)
                        .into_iter()
                        .filter(|s| s["name"] != "flight")
                        .collect();
                let at = usize::from(sections.first().is_some_and(|s| s["name"] == "story"));
                sections.splice(at..at, fresh);
                page["sections"] = json!(sections);
                page
            } else {
                let page = self.feed.page(page, now);
                self.hold = Some((page["sections"].clone(), now));
                page
            }
        };
        self.tall =
            self.view == "main" && !column && (self.tall || self.top > 0 || self.open.is_string());
        let mut strip_grounds = vec![];
        let mut lines = match self.view.as_str() {
            "main" if column => {
                let (lines, grounds) = layout::draw_column_main(&page, body_cols, phone);
                strip_grounds = grounds;
                lines
            }
            "summary" => {
                let (lines, top) =
                    threads::draw_summary(&summary, body_cols, rows, self.summary_top);
                self.summary_top = top;
                lines
            }
            "threads" => {
                let (lines, top) = threads::draw_threads(
                    &every,
                    body_cols,
                    rows,
                    &self.open,
                    &self.sent,
                    self.threads_top,
                    false,
                );
                self.threads_top = top;
                lines
            }
            _ => pages::draw_page(&page, body_cols, if self.tall { 10000 } else { room }, &ui),
        };
        if column && self.view != "main" {
            // a page laid out at its narrowest, its words rewrapped
            lines = layout::column_reflow(&lines, body_cols);
        }
        if self.exporting {
            return lines.rows;
        }
        if let Some((key, row)) = self.anchor.take() {
            if let Some(actual) = lines
                .acts
                .iter()
                .filter(|(_, act)| act["key"] == key)
                .map(|(r, _)| *r)
                .min()
            {
                self.top = actual.saturating_sub(row);
            }
        }
        // the rows the page shows: the screen less the value row, the legend's own still counted
        let shown = room.saturating_sub(1);
        let top = if self.view == "main" && !column {
            self.top = self.top.min(lines.rows.len().saturating_sub(shown));
            self.top
        } else {
            0
        };
        if self.view == "main" && !column {
            let names = lines
                .heads
                .iter()
                .filter(|(r, _)| *r >= top && *r < top + shown)
                .map(|(_, n)| n.clone())
                .collect::<Vec<_>>();
            self.feed.shown(&names, now);
            if self.persist {
                self.feed.save(now);
            }
        }
        self.acts = lines
            .acts
            .into_iter()
            .filter(|(r, _)| *r >= top && *r < top + shown)
            .map(|(r, a)| (r - top, a))
            .collect();
        self.pill = lines.pictures.iter().find_map(|w| match &w.picture {
            super::render::pictures::Picture::Pill(p) => Some(p.clone()),
            _ => None,
        });
        if self.pixels {
            let cell = super::render::kitty::cell_size();
            self.placed = lines
                .pictures
                .iter()
                .filter_map(|wanted| {
                    if wanted.row < top || wanted.row + wanted.rows > top + shown {
                        return None;
                    }
                    let mut wanted = wanted.clone();
                    wanted.row -= top;
                    wanted.place(&mut self.picture_cache, cell)
                })
                .collect();
            let logo = lines
                .pictures
                .iter()
                .any(|p| matches!(p.picture, super::render::pictures::Picture::Logo));
            if top == 0 && logo && !self.placed.iter().any(|p| p.col == 1 && p.cols == 4) {
                if let Some(row) = lines.rows.first_mut() {
                    *row = row.replacen(&slot("    ", "normal"), &slot("CTX ", "normal"), 1);
                }
            }
        }
        let body = lines
            .rows
            .drain(top..)
            .take(shown)
            .map(|s| safe_line(&s))
            .collect::<Vec<_>>();
        let mut body = body;
        body.resize(shown, String::new());
        if !value.is_empty() {
            // a click on the value row opens the i page, which says it in words
            self.acts.insert(shown, json!({"key": "instrument:figure"}));
        }
        body.extend(value);
        if tapes > 0 {
            // after each row, by absolute position: a repainted row repaints its strip cells
            let g = masthead::gauges_compacting(
                &data["claude_fields"],
                data["claude_at"].as_f64(),
                s,
                now,
                &data["limits"],
                &data["sts"],
                &data["compacting"],
            );
            let columns = layout::gauge_strip(&g, tapes, rows - 2);
            for (row, strip) in layout::strip_rows(&columns, cols, &strip_grounds)
                .into_iter()
                .enumerate()
            {
                if let Some(line) = body.get_mut(row) {
                    line.push_str(&strip);
                }
            }
            self.edge = Some((body_cols, rows - 1));
        } else {
            self.edge = None;
        }
        let countdown = layout::reload_in(self.loaded, now);
        let (foot, spans) = legend_parts(
            &self.view,
            cols.saturating_sub(1),
            if self.view == "main" && !column {
                Some(countdown)
            } else {
                None
            },
            Layout {
                phone,
                column,
                zoomed: self.phone.zoomed,
            },
        );
        self.phone.foot = Some((phone, column));
        self.acts.insert(
            rows - 1,
            json!({
                "key": "legend",
                "keys": spans,
            }),
        );
        body.push(foot);
        body
    }
    fn instrument_frame(
        &mut self,
        composition: &super::render::instrument::Composition,
        cols: usize,
        rows: usize,
    ) -> Vec<String> {
        use super::render::{instrument, raster, text};
        self.instrument_metrics = None;
        self.obs_lines = [ObsCard::Confusing, ObsCard::ShouldBe, ObsCard::More].map(|card| {
            if card == ObsCard::More {
                return self
                    .more
                    .iter()
                    .map(|i| string(&i["line"]).to_owned())
                    .collect();
            }
            composition
                .items(card)
                .iter()
                .map(|i| string(&i["line"]).to_owned())
                .collect()
        });
        // A popup on a shortened row reads the rows the frame under it drew.
        let with_more;
        let composition = if self.popup.as_ref().is_some_and(|p| p.card == ObsCard::More) {
            with_more = {
                let mut c = composition.clone();
                c.more = self.more.clone();
                c
            };
            &with_more
        } else {
            composition
        };
        let mut open = self.instrument_open;
        open.popup = None;
        self.popup_copy = None;
        self.popup_send = None;
        self.popup_for = None;
        if let Some(popup) = self.popup.clone() {
            match self.popup_place(&popup) {
                Some(index) => {
                    let at = PopupAt {
                        card: popup.card,
                        index,
                        copied: popup.copied,
                        note: popup.note,
                    };
                    open.popup = Some(at);
                    let detail = composition.detail(at);
                    self.popup_copy = detail.as_ref().map(|d| d.copy.clone());
                    self.popup_send = detail.map(|d| d.send).filter(|s| !s.is_empty());
                    self.popup_for = Some((popup.card, popup.line.clone()));
                    if let Some(p) = self.popup.as_mut() {
                        p.index = index;
                    }
                }
                // The item left its card: there is nothing to show.
                None => self.popup = None,
            }
        }
        let cell = super::render::kitty::cell_size();
        self.next_change = composition.next_change_at(open);
        // Everything below is a function of this key. Raw `now` is not in it.
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            composition.draw_key(open).hash(&mut h);
            (cols, rows, cell, self.pixels, self.cards, self.phone.zoomed).hash(&mut h);
            format!("{:?}{:?}", self.theme, open).hash(&mut h);
            h.finish()
        };
        if let Some(kept) = self.instrument_kept.as_ref().filter(|k| k.key == key) {
            self.acts = kept.acts.clone();
            self.hits = kept.hits.clone();
            self.placed = kept.placed.clone();
            self.instrument_metrics = kept.metrics.clone();
            if open.popup.is_none() {
                self.more = kept.more.clone();
                self.story = kept.story;
                self.obs_lines[ObsCard::More as usize] = self
                    .more
                    .iter()
                    .map(|i| string(&i["line"]).to_owned())
                    .collect();
            }
            self.pill = None;
            self.reused += 1;
            return kept.rows.clone();
        }
        let strip = if self.pixels && self.cards && cols < 32 {
            instrument::strip(composition, cols as u32 * cell.0, self.theme, open.figure)
        } else {
            None
        };
        self.rasters += usize::from(strip.is_some());
        let semantic = instrument::rows_with_figure(composition, cols, rows, open, strip.is_none());
        self.acts = instrument::acts(composition, cols);
        // Answer placement follows the rendered rows, including wrapped metadata.
        if let Some((row, act)) = self.acts.iter().next().map(|(r, a)| (*r, a.clone())) {
            if let Some(actual) = semantic.iter().position(|line| {
                line.starts_with("1 helpful") || line.starts_with("Press 1 to turn dreams")
            }) {
                self.acts.remove(&row);
                self.acts.insert(actual, act);
            }
        }
        if composition.dream.state == instrument::Batch::Ready {
            let question_rows =
                text::wrap(&format!("☾ {}", composition.question()), cols.max(1), 1000).len();
            for row in 0..question_rows.min(semantic.len()) {
                self.acts
                    .entry(row)
                    .or_insert_with(|| json!({"key":"instrument:dreams"}));
            }
        }
        self.pill = None;
        // Every wrapped row of an observation opens it, not only its first.
        let mut more: Vec<Value> = vec![];
        let mut hits: BTreeMap<usize, (ObsCard, usize)> = BTreeMap::new();
        for (card, should) in [(ObsCard::Confusing, false), (ObsCard::ShouldBe, true)] {
            let items = composition.items(card);
            let fold = instrument::fold_row(card.title(), items.len());
            if let Some(row) = semantic.iter().position(|l| *l == fold) {
                for r in row..row + text::wrap(&fold, cols.max(1), 1000).len() {
                    self.acts
                        .insert(r, json!({"key": format!("observe:{}:0", card as usize)}));
                    hits.insert(r, (card, 0));
                }
                continue;
            }
            // The card's title opens its first item.
            let title = text::wrap(card.title(), cols.max(1), 1000);
            if let Some(row) = title
                .first()
                .filter(|_| !items.is_empty())
                .and_then(|first| semantic.iter().position(|l| l == first))
            {
                for (offset, line) in title.iter().enumerate() {
                    if semantic.get(row + offset) == Some(line) {
                        hits.insert(row + offset, (card, 0));
                    }
                }
            }
            for (index, item) in items.iter().enumerate() {
                let text = instrument::observation_text(item, should);
                let wrapped = text::wrap(&text, cols.max(1), 1000);
                if let Some(row) = wrapped
                    .first()
                    .and_then(|first| semantic.iter().position(|l| l == first))
                {
                    for (offset, line) in wrapped.iter().enumerate() {
                        if semantic.get(row + offset) == Some(line) {
                            self.acts.insert(
                                row + offset,
                                json!({"key": format!("observe:{}:{index}", card as usize)}),
                            );
                            hits.insert(row + offset, (card, index));
                        }
                    }
                }
            }
        }
        for (row, line) in semantic.iter().enumerate() {
            if line.starts_with("Story ·") {
                self.acts.insert(row, json!({"key":"instrument:threads"}));
            } else if ["! ", "+ ", "~ ", "- "].iter().any(|s| line.starts_with(s)) {
                self.acts.insert(row, json!({"key":"instrument:changes"}));
            } else if line.contains(" in flight · ") {
                self.acts.insert(row, json!({"key":"instrument:agents"}));
            } else if line.contains("CTX ") {
                self.acts.insert(row, json!({"key":"instrument:figure"}));
            }
        }
        // An open figure lists its facts a row each; every row of the list is the figure's.
        if open.figure {
            let rows_of = instrument::figure_text_rows(composition, cols, true, open.gauge);
            if let Some(row) = rows_of
                .first()
                .and_then(|first| semantic.iter().position(|l| l == first))
            {
                for (offset, line) in rows_of.iter().enumerate() {
                    if semantic.get(row + offset) == Some(line) {
                        self.acts
                            .insert(row + offset, json!({"key":"instrument:figure"}));
                    }
                }
            }
        }
        // A story cut at a clause shows its "…": every row of it opens the whole story.
        if let Some((lines, full)) = instrument::text_story_cut(composition, cols) {
            let item = json!({"title": "Where the work stands", "line": full});
            if let Some(row) = lines
                .first()
                .and_then(|first| semantic.iter().position(|l| l == first))
            {
                more.push(item);
                for (offset, line) in lines.iter().enumerate() {
                    if semantic.get(row + offset) == Some(line) {
                        self.acts
                            .insert(row + offset, json!({"key": "observe:2:0"}));
                    }
                }
            }
        }
        // The story's rows open it whole, cut or not.
        let story_rows = instrument::text_story_rows(composition, cols);
        let mut story = None;
        if let Some(row) = story_rows
            .first()
            .and_then(|first| semantic.iter().position(|l| l == first))
        {
            let at = Self::keep_more(&mut more, instrument::story_title(composition), composition);
            story = Some(at);
            for (offset, line) in story_rows.iter().enumerate() {
                if semantic.get(row + offset) == Some(line) {
                    hits.insert(row + offset, (ObsCard::More, at));
                }
            }
        }
        let mut out = semantic;
        if let Some(strip) = strip {
            self.encodes += 1;
            if let Ok(png) = raster::png(&strip) {
                let count = strip.height().div_ceil(cell.1) as usize;
                let row = out.len().min(rows.saturating_sub(1 + count));
                out.truncate(row);
                self.placed = vec![super::render::kitty::Placement {
                    row,
                    col: 0,
                    cols,
                    rows: count,
                    png: png.into(),
                }];
                hits.retain(|r, _| *r < row);
                for r in row..row + count {
                    self.acts.insert(r, json!({"key":"instrument:figure"}));
                }
            }
        } else if self.pixels && self.cards && cols >= 32 {
            let cell = super::render::kitty::cell_size();
            if let Some((pm, metrics)) = instrument::draw(
                composition,
                cols as u32 * cell.0,
                rows.saturating_sub(1) as u32 * cell.1,
                self.theme,
                open,
                false,
            ) {
                self.rasters += 1;
                self.encodes += 1;
                if let Ok(png) = raster::png(&pm) {
                    self.instrument_metrics = Some(metrics.clone());
                    let picture_rows = pm.height().div_ceil(cell.1) as usize;
                    self.placed = vec![super::render::kitty::Placement {
                        row: 0,
                        col: 0,
                        cols,
                        rows: picture_rows,
                        png: png.into(),
                    }];
                    out = vec![String::new(); picture_rows];
                    let to_row = |y: f32| {
                        ((y / metrics.height) * pm.height() as f32 / cell.1 as f32)
                            .floor()
                            .max(0.) as usize
                    };
                    let answer = self.acts.values().find(|a| a["dream"].is_string()).cloned();
                    self.acts.clear();
                    if composition.dream.state == instrument::Batch::Ready {
                        for row in 0..=to_row(metrics.dream + 16.) {
                            self.acts.insert(row, json!({"key":"instrument:dreams"}));
                        }
                    }
                    if let Some((start, height)) = metrics.story_region {
                        for row in to_row(start)..=to_row(start + height) {
                            self.acts.insert(row, json!({"key":"instrument:threads"}));
                        }
                    }
                    if let Some((start, height)) = metrics.agents_region {
                        for row in to_row(start)..=to_row(start + height) {
                            self.acts.insert(row, json!({"key":"instrument:agents"}));
                        }
                    }
                    for y in &metrics.shift_hits {
                        self.acts
                            .insert(to_row(*y), json!({"key":"instrument:changes"}));
                    }
                    for row in to_row(metrics.figure_region.0)
                        ..=to_row(metrics.figure_region.0 + metrics.figure_region.1)
                    {
                        self.acts.insert(row, json!({"key":"instrument:figure"}));
                    }
                    // Each quota circle is its own target, in the cells it covers.
                    let to_col = |x: f32| {
                        (metrics.content_left
                            + x / instrument::LAYOUT_WIDTH * metrics.content_width)
                            * cols as f32
                    };
                    let mut taps: BTreeMap<usize, Vec<Value>> = BTreeMap::new();
                    for gauge in &metrics.quota_gauges {
                        let Some((x0, y0, x1, y1)) = gauge.tap else {
                            continue;
                        };
                        let span = json!([
                            to_col(x0).ceil().max(0.) as usize,
                            (to_col(x1).floor().max(0.) as usize).min(cols),
                            gauge.code
                        ]);
                        for row in to_row(y0)..=to_row(y1) {
                            taps.entry(row).or_default().push(span.clone());
                        }
                    }
                    for (row, spans) in taps {
                        if let Some(act) = self.acts.get_mut(&row) {
                            if act["key"] == "instrument:figure" {
                                act["gauges"] = json!(spans);
                            }
                        }
                    }
                    // Every row drawn with a trailing "…" opens its words in full.
                    more.clear();
                    for cut in &metrics.cuts {
                        let item = json!({"title": cut.title, "line": cut.full});
                        let index = more.iter().position(|m| *m == item).unwrap_or_else(|| {
                            more.push(item);
                            more.len() - 1
                        });
                        for row in to_row(cut.y - cut.size)..=to_row(cut.y + cut.size * 0.3) {
                            self.acts
                                .insert(row, json!({"key": format!("observe:2:{index}")}));
                        }
                    }
                    // A row of text spans more than one terminal row on small cells: the whole
                    // line is the target.
                    let half = super::render::theme::BODY_LINE / 2. - 0.5;
                    for (card, index, y) in &metrics.observation_rows {
                        for row in to_row(*y - half)..=to_row(*y + half) {
                            self.acts.insert(
                                row,
                                json!({"key": format!("observe:{}:{index}", *card as usize)}),
                            );
                        }
                    }
                    // Two picture rows can share a terminal row, and `acts` holds one act per row:
                    // each terminal row opens the picture row that fills most of it. The title
                    // opens the card's first item and the story's title and text open the story.
                    story = metrics.story_open.as_ref().map(|(_, _, title, full)| {
                        let item = json!({"title": title, "line": full});
                        more.iter().position(|m| *m == item).unwrap_or_else(|| {
                            more.push(item);
                            more.len() - 1
                        })
                    });
                    let px = |y: f32| y / metrics.height * pm.height() as f32;
                    let mut spans: Vec<(ObsCard, usize, f32, f32)> = metrics
                        .observation_rows
                        .iter()
                        .map(|(card, index, y)| (*card, *index, px(*y - half), px(*y + half)))
                        .collect();
                    spans.extend(
                        metrics
                            .observation_titles
                            .iter()
                            .map(|(card, top, bottom)| (*card, 0, px(*top), px(*bottom))),
                    );
                    if let (Some(at), Some((top, bottom, _, _))) = (story, &metrics.story_open) {
                        spans.push((ObsCard::More, at, px(*top), px(*bottom)));
                    }
                    hits.clear();
                    for row in 0..picture_rows {
                        let (from, to) = (
                            (row as u32 * cell.1) as f32,
                            ((row + 1) as u32 * cell.1) as f32,
                        );
                        let mut best: Option<(f32, ObsCard, usize)> = None;
                        for (card, index, top, bottom) in &spans {
                            let overlap = bottom.min(to) - top.max(from);
                            if overlap > 0. && best.is_none_or(|b| overlap > b.0) {
                                best = Some((overlap, *card, *index));
                            }
                        }
                        if let Some((_, card, index)) = best {
                            hits.insert(row, (card, index));
                        }
                    }
                    if let (Some(mut answer), Some(y)) = (answer, metrics.answer_y) {
                        let cell = |x: f32| {
                            (metrics.content_left + x / 605. * metrics.content_width) * cols as f32
                        };
                        let mut spans = metrics
                            .answer_spans
                            .iter()
                            .map(|(a, b, word)| {
                                (cell(*a).floor() as usize, cell(*b).ceil() as usize, word)
                            })
                            .collect::<Vec<_>>();
                        // Rounding out can give two answers the same cell; the later answer keeps it.
                        for i in 1..spans.len() {
                            spans[i - 1].1 = spans[i - 1].1.min(spans[i].0);
                        }
                        answer["spans"] = json!(spans
                            .iter()
                            .map(|(a, b, word)| json!([a, b, word]))
                            .collect::<Vec<_>>());
                        self.acts.insert(to_row(y - 4.), answer);
                    }
                    if let Some((y, x0, x1)) = metrics.offer_hit {
                        let left = ((metrics.content_left + x0 / 605. * metrics.content_width)
                            * cols as f32)
                            .floor() as usize;
                        let right = ((metrics.content_left + x1 / 605. * metrics.content_width)
                            * cols as f32)
                            .ceil() as usize;
                        self.acts.insert(
                            to_row(y - 4.),
                            json!({
                                "key": "instrument:dreams-on",
                                "spans": [[left, right, "dreams-on"]],
                            }),
                        );
                    }
                }
            }
        }
        if open.popup.is_none() {
            self.more = more;
            self.story = story;
            self.sync_more_lines();
        }
        if open.popup.is_some() {
            // Only the popup's own footer answers while it is up.
            self.acts.clear();
            hits.clear();
            self.popup_key_acts();
        }
        out.truncate(rows.saturating_sub(1));
        out = out.into_iter().map(|line| text::safe_line(&line)).collect();
        // Text content sits immediately above the keys; placements already own their rows.
        if self.placed.is_empty() && !out.is_empty() {
            let gap = rows.saturating_sub(1 + out.len());
            if gap > 0 {
                self.acts = std::mem::take(&mut self.acts)
                    .into_iter()
                    .map(|(row, act)| (row + gap, act))
                    .collect();
                hits = std::mem::take(&mut hits)
                    .into_iter()
                    .map(|(row, hit)| (row + gap, hit))
                    .collect();
                out.splice(0..0, std::iter::repeat_n(String::new(), gap));
            }
        }
        out.resize(rows.saturating_sub(1), String::new());
        let token = self.theme.tokens();
        let rgb = [token.quiet[0], token.quiet[1], token.quiet[2]];
        let ground = [token.ground[0], token.ground[1], token.ground[2]];
        let ink = [token.ink[0], token.ink[1], token.ink[2]];
        let weekly_csr = format!("CSR {} pts", composition.csr_cost.share_label());
        for line in &mut out {
            let padding = " ".repeat(cols.saturating_sub(text::len(line)));
            if line.contains(&weekly_csr) {
                let csr_ink =
                    text::fg([token.csr_share[0], token.csr_share[1], token.csr_share[2]]);
                *line = line
                    .replace("7D", &format!("{}7D{}", text::fg(rgb), text::fg(ink)))
                    .replace(
                        &weekly_csr,
                        &format!("{csr_ink}{weekly_csr}{}", text::fg(ink)),
                    );
            }
            *line = format!(
                "{}{}{line}{padding}{}",
                text::bg(ground),
                text::fg(ink),
                text::OFF
            );
        }
        let zoom = self.phone.zoomed;
        let mut keys = vec![];
        if zoom {
            keys.push(("z", "split"));
        }
        if !composition.dream.queue.is_empty() {
            keys.push(("d", "queue"));
        }
        keys.extend([("l", "changes"), ("t", "threads"), ("i", "figure")]);
        if !composition.confusing.is_empty() || !composition.should_be.is_empty() {
            keys.push(("o", "details"));
        }
        keys.extend([("b", "theme"), ("q", "close")]);
        if open.popup.is_some() {
            let detail = open.popup.and_then(|at| composition.detail(at));
            keys = observation::footer_keys(detail.as_ref());
            keys.push(("b", "theme"));
        }
        // Same left inset as the cards (1 cell); fit labels into cols-2. Keep q close.
        let inset = if cols > 2 { 1usize } else { 0 };
        let usable = cols.saturating_sub(inset.saturating_mul(2)).max(1);
        let width = |keys: &[(&str, &str)], gap: usize| {
            keys.iter()
                .map(|(key, label)| key.len() + usize::from(!label.is_empty()) + label.len())
                .sum::<usize>()
                + keys.len().saturating_sub(1) * gap
        };
        if open.popup.is_some() {
            // The popup's own keys come first: theme goes, then the labels shorten.
            if width(&keys, 1) > usable {
                keys.retain(|(k, _)| *k != "b");
            }
            for (full, short) in [
                ("copy draft", "copy"),
                ("dismiss", "hide"),
                ("close", ""),
                ("next", ""),
            ] {
                if width(&keys, 1) <= usable {
                    break;
                }
                if let Some((_, label)) = keys.iter_mut().find(|(_, label)| *label == full) {
                    *label = short;
                }
            }
        }
        // "o details" is the optional key: it goes before the gap tightens or any label shortens.
        if width(&keys, 2) > usable {
            keys.retain(|(k, _)| *k != "o");
        }
        let mut gap = 2usize;
        if width(&keys, gap) > usable {
            gap = 1;
        }
        let shorten = if cols < 44 {
            [
                ("changes", "chg"),
                ("threads", "thr"),
                ("figure", "fig"),
                ("theme", "thm"),
                ("queue", "q"),
                ("fig", "fig"),
            ]
        } else {
            [
                ("figure", "fig"),
                ("theme", "thm"),
                ("queue", ""),
                ("fig", ""),
                ("changes", "chg"),
                ("threads", "thr"),
            ]
        };
        for (full, short) in shorten {
            if width(&keys, gap) <= usable {
                break;
            }
            if let Some((_, label)) = keys.iter_mut().find(|(_, label)| *label == full) {
                *label = short;
            }
        }
        for key in ["o", "l", "t", "i", "d", "z"] {
            if width(&keys, gap) <= usable {
                break;
            }
            keys.retain(|(k, _)| *k != key);
        }
        if width(&keys, gap) > usable {
            keys = vec![("b", "theme")];
            if width(&keys, gap) > usable {
                keys = vec![("b", "thm")];
            }
            if width(&keys, gap) > usable {
                keys = vec![("b", "")];
            }
        }
        let sep = if gap == 1 { " " } else { "  " };
        let body = text::fit(
            &keys
                .iter()
                .map(|(key, label)| {
                    if label.is_empty() {
                        (*key).to_owned()
                    } else {
                        format!("{key} {label}")
                    }
                })
                .collect::<Vec<_>>()
                .join(sep),
            usable,
        );
        let footer = format!("{}{body}", " ".repeat(inset));
        let mut start = inset;
        let mut spans = vec![];
        for (key, label) in &keys {
            let end = start + key.len() + usize::from(!label.is_empty()) + label.len();
            if end <= cols {
                spans.push(json!([start, end, key]));
            }
            start = end + gap;
        }
        self.acts
            .insert(rows.saturating_sub(1), json!({"key":"legend","keys":spans}));
        out.push(format!(
            "{}{}{footer}{}{}",
            text::bg(ground),
            text::fg(rgb),
            " ".repeat(cols.saturating_sub(text::len(&footer))),
            text::OFF
        ));
        self.instrument_kept = Some(Instrument {
            key,
            rows: out.clone(),
            placed: self.placed.clone(),
            acts: self.acts.clone(),
            metrics: self.instrument_metrics.clone(),
            more: self.more.clone(),
            hits: hits.clone(),
            story: self.story,
        });
        self.hits = hits;
        out
    }
    /// Account cost reads: the plan's week, what CSR cost
    /// in it, and the counts of what you took.
    fn week_data(
        env: &Wide,
        (narrative_kept, spend_kept): (&mut Option<(Value, f64, Value)>, &mut Option<value::Spend>),
        data: &mut Value,
        now: f64,
    ) {
        let Wide {
            paths, db, persist, ..
        } = *env;
        let plan = value::plan_of(&data["claude_fields"], &data["limits"], now);
        let week = if plan["mode"] == "plan" {
            plan["start"].clone()
        } else {
            plan["mode"].clone()
        };
        let stale = narrative_kept
            .as_ref()
            .is_none_or(|(key, at, _)| *key != week || now - at >= WEEK_EVERY);
        if stale {
            let narrative = db
                .map(|db| value::narrative_week(db, num(&plan["start"])))
                .unwrap_or_else(|| json!({}));
            *narrative_kept = Some((week, now, narrative));
        }
        let narrative = narrative_kept.as_ref().map_or(json!({}), |n| n.2.clone());
        let mut spent = Value::Null;
        if plan["mode"] == "plan" {
            let start = num(&plan["start"]);
            let spend = spend_kept.get_or_insert_with(|| value::Spend::new(paths, persist));
            let budget = if persist { 0.1 } else { 30. };
            spend.step(start, now, std::time::Duration::from_secs_f64(budget));
            if spend.ready && spend.start() == Some(start) {
                spent = spend.total();
            }
        } else {
            // Only a plan's week is added up, and only a frame in plan mode steps the spend: left
            // in place, its listing deadline would stay in the past and its unfinished work
            // would keep the pane busy for good.
            *spend_kept = None;
        }
        data["value"] = value::week_value(paths, &plan, &narrative, &spent);
        data["plan"] = plan;
        data["narrative"] = narrative;
        data["spend"] = spent;
    }
    /// Once, when the session before this one can be found: start the carry (a thread off the draw
    /// path in the watch loop; inline in a one-shot run), take its result when it lands. Returns the
    /// session's project, for its threads.
    fn carry_step(&mut self) -> Option<String> {
        let session = self.session.as_ref()?;
        let mine = session.path.clone().or_else(|| {
            session.data["cwd"].as_str().map(|cwd| {
                self.paths
                    .project_folder(cwd)
                    .join(format!("{}.jsonl", self.sid))
            })
        })?;
        if !self.carry.started() {
            let job = carry::Job {
                paths: self.paths.clone(),
                sid: self.sid.clone(),
                path: mine.clone(),
                started: num(&session.data["started"]),
                persist: self.persist,
            };
            match self.carry_gate.take() {
                Some(gate) if self.persist => self.carry.spawn(move || {
                    let _ = gate.recv();
                    carry::work(&job)
                }),
                _ => self.carry.start(job, !self.persist),
            }
        }
        self.carry.poll();
        mine.parent()
            .and_then(std::path::Path::file_name)
            .and_then(|name| name.to_str())
            .map(str::to_owned)
    }
    /// Hold the carry's thread until the returned sender fires or drops. A test seam: it shows
    /// that a frame draws, and returns, while the carry is still working.
    pub fn hold_carry(&mut self) -> std::sync::mpsc::Sender<()> {
        let (send, receive) = std::sync::mpsc::channel();
        self.carry_gate = Some(receive);
        send
    }
    /// True between frames once the marker of the compaction the pane is showing is gone: the
    /// watch loop then draws at once, not on its next tick.
    pub fn compaction_gone(&self) -> bool {
        self.compacting
            && self.session.is_some()
            && self
                .paths
                .kept_file("compacting", &self.sid)
                .is_none_or(|marker| !marker.exists())
    }
    /// One press enables dreams in this HOME's settings.json. Never calls the live host config API.
    fn press_dreams_offer(&mut self) -> bool {
        match Self::enable_dreams_in_home(&self.paths.home) {
            Ok(()) => {
                self.offer_notice = None;
            }
            Err(notice) => {
                self.offer_notice = Some(notice);
            }
        }
        true
    }

    /// Validate and atomically write `dreams: true` under every csr-companion pluginConfigs entry
    /// that is currently false, preserving unrelated fields and the original file mode.
    fn enable_dreams_in_home(home: &std::path::Path) -> Result<(), String> {
        let path = home.join(".claude/settings.json");
        let raw = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => "{}".into(),
            Err(err) => {
                return Err(format!("Couldn't turn dreams on here: {err}"));
            }
        };
        let prior_mode = std::fs::metadata(&path).ok().map(|m| m.permissions());
        let mut settings: Value = match serde_json::from_str(&raw) {
            Ok(Value::Object(map)) => Value::Object(map),
            Ok(_) => {
                return Err(
                    "Couldn't turn dreams on here. settings.json is not a JSON object.".into(),
                );
            }
            Err(_) => {
                return Err("Couldn't turn dreams on here. settings.json is malformed.".into());
            }
        };
        let root = settings.as_object_mut().unwrap();
        match root.get("pluginConfigs") {
            None => {
                root.insert("pluginConfigs".into(), json!({}));
            }
            Some(Value::Object(_)) => {}
            Some(_) => {
                return Err(
                    "Couldn't turn dreams on here. pluginConfigs must be a JSON object.".into(),
                );
            }
        }
        let plugins = root
            .get_mut("pluginConfigs")
            .unwrap()
            .as_object_mut()
            .unwrap();
        let keys: Vec<String> = plugins
            .keys()
            .filter(|k| k.split('@').next() == Some("csr-companion"))
            .cloned()
            .collect();
        if keys.is_empty() {
            plugins.insert(
                "csr-companion@local".into(),
                json!({"options": {"dreams": true}}),
            );
        } else {
            for plugin_key in keys {
                match plugins.get(&plugin_key) {
                    Some(Value::Object(_)) => {}
                    Some(_) => {
                        return Err(format!(
                            "Couldn't turn dreams on here. {plugin_key} must be a JSON object."
                        ));
                    }
                    None => continue,
                }
                let entry = plugins
                    .get_mut(&plugin_key)
                    .unwrap()
                    .as_object_mut()
                    .unwrap();
                match entry.get("options") {
                    None => {
                        entry.insert("options".into(), json!({}));
                    }
                    Some(Value::Object(_)) => {}
                    Some(_) => {
                        return Err(
                            "Couldn't turn dreams on here. options must be a JSON object.".into(),
                        );
                    }
                }
                let options = entry.get_mut("options").unwrap().as_object_mut().unwrap();
                if options.get("dreams") == Some(&json!(false)) {
                    options.insert("dreams".into(), json!(true));
                }
            }
        }
        let parent = path.parent().ok_or_else(|| {
            "Couldn't turn dreams on here: settings path has no directory".to_string()
        })?;
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("Couldn't turn dreams on here: {err}"))?;
        let body = serde_json::to_string_pretty(&settings)
            .map_err(|err| format!("Couldn't turn dreams on here: {err}"))?;
        let mut n = 0u32;
        let tmp = loop {
            let candidate = parent.join(format!("settings.{}.{n}.tmp", std::process::id()));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(mut file) => {
                    use std::io::Write;
                    if let Err(err) = file
                        .write_all(body.as_bytes())
                        .and_then(|_| file.sync_all())
                    {
                        let _ = std::fs::remove_file(&candidate);
                        return Err(format!("Couldn't turn dreams on here: {err}"));
                    }
                    break candidate;
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    n = n.saturating_add(1);
                    if n > 10_000 {
                        return Err("Couldn't turn dreams on here: temp file collision".into());
                    }
                }
                Err(err) => {
                    return Err(format!("Couldn't turn dreams on here: {err}"));
                }
            }
        };
        if let Some(mode) = prior_mode {
            let _ = std::fs::set_permissions(&tmp, mode);
        }
        std::fs::rename(&tmp, &path).map_err(|err| {
            let _ = std::fs::remove_file(&tmp);
            format!("Couldn't turn dreams on here: {err}")
        })?;
        Ok(())
    }
    /// The first observation card with items on it: the one the number keys and `o` open.
    fn first_card(&self) -> Option<ObsCard> {
        [ObsCard::Confusing, ObsCard::ShouldBe]
            .into_iter()
            .find(|card| !self.obs_lines[*card as usize].is_empty())
    }
    /// Show item `index` of `card` in full. False when the card has no such item.
    fn open_observation(&mut self, card: ObsCard, index: usize) -> bool {
        let Some(line) = self.obs_lines[card as usize].get(index) else {
            return false;
        };
        self.popup = Some(Popup {
            card,
            line: line.clone(),
            index,
            copied: false,
            note: None,
        });
        true
    }
    /// Where the popup's item is on its card now: where it was, or where its line moved to.
    fn popup_place(&self, popup: &Popup) -> Option<usize> {
        let lines = &self.obs_lines[popup.card as usize];
        if lines.get(popup.index) == Some(&popup.line) {
            return Some(popup.index);
        }
        lines.iter().position(|l| *l == popup.line)
    }
    /// The lines of the shortened rows, as the frame just drawn left them.
    fn sync_more_lines(&mut self) {
        self.obs_lines[ObsCard::More as usize] = self
            .more
            .iter()
            .map(|i| string(&i["line"]).to_owned())
            .collect();
    }
    /// The item of `more` that holds `title` and the story's words, added when it is not there.
    fn keep_more(
        more: &mut Vec<Value>,
        title: &str,
        composition: &super::render::instrument::Composition,
    ) -> usize {
        let item =
            json!({"title": title, "line": super::render::instrument::story_full(composition)});
        more.iter().position(|m| *m == item).unwrap_or_else(|| {
            more.push(item);
            more.len() - 1
        })
    }
    /// The keys of the observation items dismissed in this session: those in the likes log with
    /// a no, and those dismissed here and perhaps not yet written.
    fn dismissed_keys(&self) -> Value {
        let mine = format!("observation:{}:", self.sid);
        let mut out = json!({});
        for (key, liked) in send::read_likes(&self.paths)
            .as_object()
            .into_iter()
            .flatten()
        {
            if *liked == json!(false) && key.starts_with(&mine) {
                out[key] = json!(true);
            }
        }
        for key in &self.dismissed {
            out[key] = json!(true);
        }
        out
    }
    /// What each terminal row of the popup's footer answers: the keys the picture drew on it.
    fn popup_key_acts(&mut self) {
        let Some(metrics) = self.instrument_metrics.as_ref() else {
            return;
        };
        let Some(picture) = self.placed.first() else {
            return;
        };
        let cell = super::render::kitty::cell_size();
        let (cols, rows) = (picture.cols, picture.rows as f32 * cell.1 as f32);
        let at = |y: f32| y / metrics.height * rows;
        let col = |x: f32| (metrics.content_left + x / 605. * metrics.content_width) * cols as f32;
        let mut by_row: BTreeMap<usize, Vec<Value>> = BTreeMap::new();
        for (key, left, right, top, bottom) in &metrics.popup_keys {
            let first = (at(*top) / cell.1 as f32).floor().max(0.) as usize;
            let last = (at(*bottom) / cell.1 as f32).floor().max(0.) as usize;
            for row in first..=last {
                by_row.entry(row).or_default().push(json!([
                    col(*left).floor().max(0.) as usize,
                    col(*right).ceil() as usize,
                    key
                ]));
            }
        }
        for (row, keys) in by_row {
            self.acts
                .insert(row, json!({"key": "legend", "keys": keys}));
        }
    }
    /// The text the watch loop should put on the clipboard, once.
    pub fn take_copy(&mut self) -> Option<String> {
        self.copy_out.take()
    }
    /// The items a step in the popup goes through, in the order the pane draws them: Confused
    /// Claude's, the should-be's, then the story. A shortened row steps among its own kind.
    fn walk(&self, at: (ObsCard, usize)) -> Vec<(ObsCard, usize)> {
        let story = self.story.map(|i| (ObsCard::More, i));
        if at.0 == ObsCard::More && story != Some(at) {
            return (0..self.obs_lines[ObsCard::More as usize].len())
                .map(|i| (ObsCard::More, i))
                .collect();
        }
        [ObsCard::Confusing, ObsCard::ShouldBe]
            .into_iter()
            .flat_map(|card| (0..self.obs_lines[card as usize].len()).map(move |i| (card, i)))
            .chain(story)
            .collect()
    }
    /// Whether the frame last drawn is of the popup that is open now.
    fn popup_drawn(&self) -> bool {
        self.popup_for.as_ref().map(|(card, line)| (*card, line))
            == self.popup.as_ref().map(|p| (p.card, &p.line))
    }
    /// `s`: type the popup's message into Claude's prompt, never pressing Enter, and say how it went.
    fn send_popup(&mut self) {
        let Some(text) = self.popup_send.clone().filter(|_| self.popup_drawn()) else {
            return;
        };
        let note = match send::agent_pane(self.agent.as_deref()) {
            None => ("No Claude pane to type into.", false),
            Some(agent) => {
                if send::send_draft(Some(&agent), &text, self.agent.as_deref()) {
                    send::focus_back();
                    (
                        "Typed into Claude's prompt. Press Enter there to send.",
                        true,
                    )
                } else {
                    ("Claude is not at its prompt. Nothing was typed.", false)
                }
            }
        };
        if let Some(popup) = self.popup.as_mut() {
            popup.copied = false;
            popup.note = Some(note);
        }
    }
    /// `x`: hide the popup's item for this session, record it as a no in the likes log, close.
    fn dismiss_popup(&mut self) {
        let Some(popup) = self.popup.clone() else {
            return;
        };
        let card = match popup.card {
            ObsCard::Confusing => "confusing_claude",
            ObsCard::ShouldBe => "should_be",
            ObsCard::More => return,
        };
        let key = observation::dismiss_key(&self.sid, card, &popup.line);
        // A PNG export reads the owner's log and never writes to it.
        if !self.exporting {
            let act = json!({"key": key, "source": "observation", "title": popup.line});
            let _ = send::keep_like(&self.paths, &act, false, super::paths::now());
        }
        self.dismissed.insert(key);
        // A key that comes before the next frame must not find it either.
        self.obs_lines[popup.card as usize].retain(|l| *l != popup.line);
        self.popup = None;
    }
    /// Keys while the popup is up. It is modal: nothing else on the pane answers until it closes.
    fn popup_key(&mut self, key: &str) -> bool {
        let Some(popup) = self.popup.clone() else {
            return false;
        };
        let (card, at) = (popup.card, self.popup_place(&popup).unwrap_or(0));
        let walk = self.walk((card, at));
        let step = |by: isize| {
            let here = walk.iter().position(|w| *w == (card, at)).unwrap_or(0);
            walk.get((here as isize + by).rem_euclid(walk.len().max(1) as isize) as usize)
                .copied()
        };
        match key {
            "escape" | "esc" | "q" => self.popup = None,
            "c" => {
                if let Some(text) = self.popup_copy.clone().filter(|_| self.popup_drawn()) {
                    self.copy_out = Some(text);
                    if let Some(popup) = self.popup.as_mut() {
                        popup.copied = true;
                        popup.note = None;
                    }
                }
            }
            "s" => self.send_popup(),
            "x" => self.dismiss_popup(),
            "j" | "down" | "n" | "right" | "tab" => {
                if let Some((card, to)) = step(1) {
                    self.open_observation(card, to);
                }
            }
            "k" | "up" | "p" | "left" | "shift+tab" => {
                if let Some((card, to)) = step(-1) {
                    self.open_observation(card, to);
                }
            }
            "b" => self.theme.toggle(),
            digit
                if digit.len() == 1
                    && ('1'..='9').contains(&digit.chars().next().unwrap_or(' ')) =>
            {
                let to = digit.parse::<usize>().unwrap_or(1) - 1;
                self.open_observation(card, to);
            }
            _ => (),
        }
        true
    }
    pub fn key(&mut self, key: &str) -> bool {
        if self.popup_key(key) {
            return true;
        }
        match key {
            "o" if self.view == "main" => {
                if let Some(card) = self.first_card() {
                    self.open_observation(card, 0);
                }
                return true;
            }
            "a" if self.view == "main" => {
                self.instrument_open.agents = !self.instrument_open.agents;
                self.instrument_open.changes = false;
                self.instrument_open.threads = false;
                self.instrument_open.dreams = false;
                self.instrument_open.scroll = 0;
                return true;
            }
            "b" => {
                self.theme.toggle();
                return true;
            }
            "i" => {
                self.instrument_open.figure = !self.instrument_open.figure;
                self.instrument_open.gauge = None;
                self.view = "main".into();
                return true;
            }
            // A tap on one quota circle: the `i` page cut to that circle. Tapping the circle
            // that is open closes the page, as `i` does.
            _ if key.starts_with("gauge:") => {
                let Some(gauge) =
                    super::render::instrument::Gauge::from_code(&key["gauge:".len()..])
                else {
                    return true;
                };
                let open = &mut self.instrument_open;
                if open.figure && open.gauge == Some(gauge) {
                    open.figure = false;
                    open.gauge = None;
                } else {
                    open.figure = true;
                    open.gauge = Some(gauge);
                }
                self.view = "main".into();
                return true;
            }
            "l" | "m" => {
                self.instrument_open.changes = !self.instrument_open.changes;
                self.instrument_open.threads = false;
                self.instrument_open.dreams = false;
                self.instrument_open.scroll = 0;
                self.instrument_open.agents = false;
                self.view = "main".into();
                return true;
            }
            "t" | "f" => {
                self.instrument_open.threads = !self.instrument_open.threads;
                self.instrument_open.changes = false;
                self.instrument_open.dreams = false;
                self.instrument_open.scroll = 0;
                self.instrument_open.agents = false;
                self.view = "main".into();
                return true;
            }
            "d" => {
                self.instrument_open.dreams = !self.instrument_open.dreams;
                self.instrument_open.changes = false;
                self.instrument_open.threads = false;
                self.instrument_open.scroll = 0;
                self.instrument_open.agents = false;
                self.view = "main".into();
                return true;
            }
            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" if self.view == "main" => {
                let index = key.parse::<usize>().unwrap_or(1) - 1;
                let answer = self
                    .acts
                    .iter()
                    .find(|(_, a)| a["dream"].is_string())
                    .map(|(r, a)| (*r, a.clone()));
                if let Some((row, act)) = answer {
                    if let Some(span) = array(&act["spans"]).get(index) {
                        // The middle of the span, so a shared edge cell can't answer for a neighbor.
                        let col = (num(&span[0]) + num(&span[1])) / 2.;
                        return self.click(row, col as usize, super::paths::now());
                    }
                }
                if key == "1"
                    && self
                        .acts
                        .values()
                        .any(|a| a["key"] == "instrument:dreams-on")
                {
                    return self.press_dreams_offer();
                }
                // With no dream asking for an answer, the number keys open the first card's items.
                if let Some(card) = self.first_card() {
                    self.open_observation(card, index);
                }
                return true;
            }
            "escape" if self.view == "main" => {
                self.instrument_open = Default::default();
            }
            _ => (),
        }
        match key {
            "q" => return false,
            "d" | "l" => {
                self.view = if key == "d" { "main" } else { "log" }.into();
                self.open = Value::Null;
            }
            "s" => {
                let view = match key {
                    "s" => "summary",
                    _ => "summary",
                };
                self.view = if self.view == view { "main" } else { view }.into();
                self.open = Value::Null;
                if self.view == "threads" {
                    self.threads_top = 0;
                }
            }
            "r" => {
                self.loaded = 0.;
                self.hold = None;
            }
            "escape" if matches!(self.view.as_str(), "summary" | "threads") => {
                self.view = "main".into();
                self.open = Value::Null;
            }
            "escape" => {
                self.open = Value::Null;
                self.top = 0;
                self.tall = false;
            }
            "m" => self.map_key(),
            "z" => {
                self.phone.zoom_key(self.herdr.as_ref());
            }
            "j" | "down" => self.scroll(3),
            "k" | "up" => self.scroll(-3),
            " " => self.scroll(12),
            _ => (),
        }
        true
    }
    /// m: open the Shift Map on the main page at its full size, or fold it again. Nothing
    /// happens with no map to open, or on another page.
    fn map_key(&mut self) {
        if self.view != "main" || !self.map.shown.is_object() {
            return;
        }
        if super::shiftmap::map_open(&self.open) {
            self.open = Value::Null;
            self.sent = Value::Null;
        } else {
            self.open = json!("shifts");
            self.sent = Value::Null;
            self.tall = true;
        }
    }
    /// (seconds for the watch loop's wait, whether to draw a frame now): a frame once the Shift
    /// Map's hold is up and it folds.
    pub fn map_wait(&self, now: f64) -> (f64, bool) {
        self.map.wait(now)
    }
    pub fn scroll(&mut self, step: isize) {
        if self.view == "main"
            && (self.instrument_open.agents
                || self.instrument_open.changes
                || self.instrument_open.threads
                || self.instrument_open.dreams)
        {
            self.instrument_open.scroll = self.instrument_open.scroll.saturating_add_signed(step);
            return;
        }
        let top = match self.view.as_str() {
            "main" => &mut self.top,
            "summary" => &mut self.summary_top,
            "threads" => &mut self.threads_top,
            _ => return,
        };
        *top = top.saturating_add_signed(step);
    }
    /// A click on a thread's answers: drop and later are kept in the log; the two asks are typed
    /// into Claude's prompt, unsent, and kept as sent.
    fn lost_click(&mut self, act: &Value, col: usize, now: f64) {
        let pick = array(&act["spans"])
            .iter()
            .find(|s| num(&s[0]) <= col as f64 && (col as f64) < num(&s[1]))
            .map(|s| string(&s[2]).to_owned());
        let project = threads::project_of(self.session.as_ref());
        match pick.as_deref() {
            Some(pick @ ("drop" | "later")) => threads::keep_lost(
                &self.paths,
                string(&act["lost"]),
                pick,
                now,
                project.as_deref(),
                json!({}),
            ),
            Some(pick @ ("side" | "sub")) => {
                let agent = send::agent_pane(self.agent.as_deref());
                let ask = string(&act["asks"][pick]).to_owned();
                let handoff = if pick == "sub" {
                    threads::new_handoff(&self.paths, &self.sid, string(&act["lost"]))
                } else {
                    None
                };
                let went = match &handoff {
                    Some((_, envelope)) => send::send_draft(
                        agent.as_deref(),
                        &format!("{ask}\n{envelope}"),
                        self.agent.as_deref(),
                    ),
                    None => send::send_text(agent.as_deref(), &ask),
                };
                self.sent = json!([act["item"], went, now]);
                if went {
                    let more = match &handoff {
                        Some((request, _)) => json!({"how": pick, "request": request}),
                        None => json!({"how": pick}),
                    };
                    threads::keep_lost(
                        &self.paths,
                        string(&act["lost"]),
                        "sent",
                        now,
                        project.as_deref(),
                        more,
                    );
                }
                send::focus_back();
            }
            _ => (),
        }
    }
    /// What a click on each terminal row opens at the last frame, as `observe:<card>:<index>`.
    pub fn hit_rows(&self) -> BTreeMap<usize, String> {
        self.hits
            .iter()
            .map(|(row, (card, index))| (*row, format!("observe:{}:{index}", *card as usize)))
            .collect()
    }
    /// The row a click on `act` is sent to: for an observation item the row that opens it, from
    /// the picture row that fills most of it; for any other act the first row that has it.
    pub fn act_row(&self, act: &str) -> Option<usize> {
        if let Some(want) = observe_key(act) {
            if let Some((row, _)) = self.hits.iter().find(|(_, hit)| **hit == want) {
                return Some(*row);
            }
        }
        self.acts
            .iter()
            .find(|(_, a)| a["key"] == act)
            .map(|(row, _)| *row)
    }
    /// What a click at `row`, `col` lands on, for the click log.
    pub fn click_target(&self, row: usize, col: usize) -> serde_json::Value {
        json!({
            "row": row,
            "col": col,
            "view": self.view,
            "popup": self.popup.is_some(),
            "act": self.acts.get(&row).map(|a| a["key"].clone()),
            "hit": self.hits.get(&row).map(|(card, index)| format!("{card:?}:{index}")),
            "foot": self.phone.foot,
            "edge": self.edge,
        })
    }
    pub fn click(&mut self, row: usize, col: usize, now: f64) -> bool {
        let act = self.acts.get(&row).cloned();
        if self.popup.is_some() {
            // The popup's footer keys answer; any other click closes it.
            let pressed = act.as_ref().filter(|a| a["key"] == "legend").and_then(|a| {
                array(&a["keys"])
                    .iter()
                    .find(|s| num(&s[0]) <= col as f64 && (col as f64) < num(&s[1]))
                    .and_then(|s| s[2].as_str().map(str::to_owned))
            });
            return match pressed.as_deref() {
                Some("c" | "s" | "x" | "tab" | "b") => self.key(pressed.as_deref().unwrap_or("")),
                _ => {
                    self.popup = None;
                    true
                }
            };
        }
        // On a phone's narrow column a tap anywhere but the legend and the masthead (the i
        // page) fills the screen.
        if self.phone.foot == Some((true, true))
            && self.view == "main"
            && !act
                .as_ref()
                .is_some_and(|a| a["key"] == "legend" || string(&a["key"]).starts_with("view:"))
        {
            self.phone.zoom_key(self.herdr.as_ref());
            return true;
        }
        // a click on the gauge strip is on no row
        if self
            .edge
            .is_some_and(|(edge, rows)| col >= edge && row < rows)
        {
            return true;
        }
        // The picture row that fills most of this terminal row.
        if self.view == "main" {
            if let Some((card, index)) = self.hits.get(&row).copied() {
                self.open_observation(card, index);
                return true;
            }
        }
        let Some(act) = act else {
            self.open = Value::Null;
            return true;
        };
        if let Some((card, index)) = observe_key(string(&act["key"])) {
            self.open_observation(card, index);
            return true;
        }
        if let Some(key) = string(&act["key"]).strip_prefix("instrument:") {
            if key == "dreams-on" {
                return self.press_dreams_offer();
            }
            // The cell under the tap decides which circle of the figure it is on.
            if key == "figure" {
                if let Some(span) = array(&act["gauges"])
                    .iter()
                    .find(|s| num(&s[0]) <= col as f64 && (col as f64) < num(&s[1]))
                {
                    return self.key(&format!("gauge:{}", string(&span[2])));
                }
            }
            return self.key(match key {
                "dreams" => "d",
                "threads" => "t",
                "figure" => "i",
                "changes" => "l",
                "agents" => "a",
                _ => "escape",
            });
        }
        if act["key"] == "legend" {
            if let Some(span) = array(&act["keys"])
                .iter()
                .find(|s| num(&s[0]) <= col as f64 && (col as f64) < num(&s[1]))
            {
                // Closing requires a deliberate keyboard q, never a stray mouse click.
                if span[2] == "q" {
                    return true;
                }
                if matches!(
                    string(&span[2]),
                    "d" | "l" | "r" | "s" | "f" | "t" | "i" | "b" | "q" | "z"
                ) {
                    return self.key(string(&span[2]));
                }
                return true;
            }
            return true;
        }
        if let Some(h) = act["heart"].as_array() {
            if col as f64 >= num(&h[0]) && (col as f64) < num(&h[1]) {
                let likes = send::read_likes(&self.paths);
                let key = act["like"].as_str().unwrap_or(string(&act["key"]));
                let _ = send::keep_like(&self.paths, &act, likes[key] != true, now);
                return true;
            }
        }
        if act["lost"].is_string() && act["spans"].is_array() {
            self.lost_click(&act, col, now);
            return true;
        }
        if let Some(span) = array(&act["spans"])
            .iter()
            .find(|s| num(&s[0]) <= col as f64 && (col as f64) < num(&s[1]))
        {
            let _ = dreams::keep(
                &self.paths,
                &json!({
                    "kind": "answer",
                    "id": act["dream"],
                    "answer": span[2],
                    "at": now,
                    "by": "pane",
                }),
            );
            if span[2] == "ask_claude" {
                let agent = send::agent_pane(self.agent.as_deref());
                let went = send::send_text(agent.as_deref(), string(&act["send"]));
                self.sent = json!([act["key"], went, now]);
                send::focus_back();
            }
            self.answered = Some((string(&act["dream"]).into(), string(&span[2]).into(), now));
            self.hold = None;
            return true;
        }
        if !array(&act["spans"]).is_empty() {
            // A click that misses a row's answers closes whatever is open.
            self.open = Value::Null;
            return true;
        }
        let key = string(&act["key"]);
        if let Some(view) = key.strip_prefix("view:") {
            if matches!(view, "main" | "log" | "summary" | "threads") {
                self.view = view.into();
                self.open = Value::Null;
            }
            return true;
        }
        if let Some(fold) = key.strip_prefix("fold:") {
            let v = json!(fold);
            if self.folds.contains(&v) {
                self.folds.retain(|s| s != &v);
            } else {
                self.folds.push(v);
            }
            self.hold = None;
            return true;
        }
        if let Some(back) = act["back"].as_str() {
            self.feed.back(back, now);
            self.open = Value::Null;
            self.hold = None;
            self.top = 0;
            return true;
        }
        if act["refresh"] == true {
            // The shift map's refresh: csr-engine tells the story again, detached; the pane
            // waits for a new telling.
            let cwd = self
                .session
                .as_ref()
                .and_then(|s| super::session::session_record(&self.paths, &s.id))
                .and_then(|record| record["cwd"].as_str().map(str::to_owned))
                .filter(|cwd| !cwd.is_empty())
                .unwrap_or_else(|| self.root.to_string_lossy().into_owned());
            let transcript = self
                .session
                .as_ref()
                .and_then(|s| s.path.as_ref())
                .map(|p| p.to_string_lossy().into_owned());
            self.map.start_refresh(
                super::shiftmap::engine_binary(),
                transcript.as_deref(),
                &cwd,
            );
            return true;
        }
        if act["key"] != self.open {
            self.open = act["key"].clone();
            if self.view == "main" {
                self.anchor = Some((key.to_owned(), row));
            }
            self.sent = Value::Null;
            self.hold = None;
            if let Some(sig) = act["expand"].as_str() {
                // Opening an entry is the expand signal: kept once, never sent anywhere.
                let _ = send::keep_shift_signal(&self.paths, sig, string(&act["title"]), now);
            }
        } else if act["nosend"] == true {
            // A report is its words, not yours: a second click folds the row, it sends nothing.
            self.open = if act["expand"].is_string() {
                json!("shifts")
            } else {
                Value::Null
            };
            self.sent = Value::Null;
        } else {
            let agent = send::agent_pane(self.agent.as_deref());
            let went = send::send_text(agent.as_deref(), string(&act["send"]));
            self.sent = json!([act["key"], went, now]);
            if went && act["lost"].is_string() {
                threads::keep_lost(
                    &self.paths,
                    string(&act["lost"]),
                    "sent",
                    now,
                    threads::project_of(self.session.as_ref()).as_deref(),
                    json!({"how": "side"}),
                );
            }
            if went && act["dream"].is_string() {
                let _ = dreams::keep(
                    &self.paths,
                    &json!({
                        "kind": "answer",
                        "id": act["dream"],
                        "answer": "ask_claude",
                        "at": now,
                        "by": "pane",
                    }),
                );
                self.answered = Some((string(&act["dream"]).into(), "ask_claude".into(), now));
                self.hold = None;
            }
            send::focus_back();
        }
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_the_legend_keeps_close_at_phone_width() {
        let (s, spans) = legend("main", 37, Some(42));
        assert!(len(&plain(&s)) <= 37);
        assert!(spans.iter().any(|(_, _, k)| k == "q"));
    }
    #[test]
    fn test_log_marks_go_before_keys_do() {
        let (s, spans) = legend("log", 30, None);
        assert!(!plain(&s).contains("changed"));
        assert_eq!(
            spans.iter().map(|(_, _, s)| s.as_str()).collect::<Vec<_>>(),
            vec!["d", "q"]
        );
    }
}
