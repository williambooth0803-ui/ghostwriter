//! `wordcraft-companion` (fork addition, see `docs/companion.md`): watches the WordCraft window you
//! write in and, when you pause, asks Codex or Claude (through your CLI login) for a few edits to the
//! paragraph you're on. They appear in the page in orange; click ✓ or ✗.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod claude;
mod codex;
mod prompt;
mod watch;

use std::collections::VecDeque;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wordcraft_mcp::{Backend, Remote};

use crate::claude::Claude;
use crate::codex::Codex;
use crate::watch::{Context, Settings, Watch};

const USAGE: &str = "\
wordcraft-companion — AI inline suggestions in the WordCraft window you write in

Start WordCraft with a control port first: wordcraft --control 7981 (or set WORDCRAFT_CONTROL_PORT).

USAGE:
  wordcraft-companion [options]

OPTIONS:
  --provider NAME        claude (default) or codex; uses that CLI's login
  --mode NAME            study (default, preserves notes) or edit (inline rewrites)
  --research             Codex: enable live web search for evidence and source URLs
  --connect HOST:PORT    the WordCraft window (default 127.0.0.1:7981)
  --brief FILE           what this piece is for: thesis, audience, tone (read again on every call)
  --style FILE           your style guide (read again on every call)
  --model NAME           model for the selected provider (Claude: sonnet; Codex: CLI default)
  --idle SECONDS         pause before asking (default 4)
  --max N                suggestions per paragraph at most (default 3)
  --reach N              look only this many paragraphs around the caret (default 2)
  --review-existing      also review text that was there when the companion started
  --max-per-hour N       AI calls per hour at most (default 60)
  --claude PATH          the claude program (default: claude on the PATH)
  --claude-arg ARG       one more argument for claude -p (repeatable), e.g. --claude-arg=--effort=low
  --codex PATH           the codex executable (default: codex on the PATH)
  --codex-arg ARG        one more argument for codex exec (repeatable)
  --once                 review the caret's paragraph now, post, and exit
  --dry-run              print suggestions instead of posting them
  --verbose              print the provider's whole reply
";

enum Assistant {
    Claude(Claude),
    Codex(Codex),
}

impl Assistant {
    fn provider(&self) -> &'static str {
        match self {
            Self::Claude(_) => "claude",
            Self::Codex(_) => "codex",
        }
    }

    fn description(&self) -> String {
        match self {
            Self::Claude(c) => format!("Claude ({})", c.model),
            Self::Codex(c) => format!("Codex ({})", c.model.as_deref().unwrap_or("CLI default model")),
        }
    }

    fn ask(&self, prompt: &str) -> Result<String, String> {
        match self {
            Self::Claude(c) => c.ask(prompt),
            Self::Codex(c) => c.ask(prompt),
        }
    }
}

struct Options {
    connect: String,
    brief: Option<String>,
    style: Option<String>,
    idle: Duration,
    max: usize,
    reach: Option<usize>,
    review_existing: bool,
    max_per_hour: usize,
    once: bool,
    dry_run: bool,
    verbose: bool,
    assistant: Assistant,
    study: bool,
}

fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut provider = "claude".to_string();
    let mut model = None;
    let mut claude = Claude { program: "claude".into(), model: "sonnet".into(), extra: Vec::new(), timeout: Duration::from_secs(180) };
    let mut codex = Codex { program: "codex".into(), model: None, extra: Vec::new(), timeout: Duration::from_secs(180), research: false };
    let mut o = Options {
        connect: "127.0.0.1:7981".into(),
        study: true,
        brief: None,
        style: None,
        idle: Duration::from_secs(4),
        max: 3,
        reach: Some(2),
        review_existing: false,
        max_per_hour: 60,
        once: false,
        dry_run: false,
        verbose: false,
        assistant: Assistant::Claude(Claude {
            program: "claude".into(),
            model: "sonnet".into(),
            extra: Vec::new(),
            timeout: Duration::from_secs(180),
        }),
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let mut value = || inline.clone().or_else(|| it.next().cloned()).ok_or_else(|| format!("{flag} needs a value"));
        let number = |v: String| v.parse::<u64>().map_err(|_| format!("{flag}: not a number: {v}"));
        match flag {
            "--provider" => provider = value()?,
            "--mode" => {
                o.study = match value()?.as_str() {
                    "study" => true,
                    "edit" => false,
                    _ => return Err("--mode must be study or edit".into()),
                }
            }
            "--research" => codex.research = true,
            "--connect" => o.connect = value()?,
            "--brief" => o.brief = Some(value()?),
            "--style" => o.style = Some(value()?),
            "--model" => model = Some(value()?),
            "--idle" => o.idle = Duration::from_secs(number(value()?)?.max(1)),
            "--max" => o.max = usize::try_from(number(value()?)?.clamp(1, 10)).unwrap_or(3),
            "--reach" => o.reach = usize::try_from(number(value()?)?).ok(),
            "--review-existing" => {
                o.review_existing = true;
                o.reach = None;
            }
            "--max-per-hour" => o.max_per_hour = usize::try_from(number(value()?)?).unwrap_or(60),
            "--claude" => claude.program = value()?,
            "--claude-arg" => claude.extra.push(value()?),
            "--codex" => codex.program = value()?,
            "--codex-arg" => codex.extra.push(value()?),
            "--once" => o.once = true,
            "--dry-run" => o.dry_run = true,
            "--verbose" => o.verbose = true,
            "-h" | "--help" => return Err(String::new()),
            _ => return Err(format!("unknown option {a}")),
        }
    }
    o.assistant = match provider.as_str() {
        "claude" => {
            if codex.program != "codex" || !codex.extra.is_empty() || codex.research {
                return Err("--codex and --codex-arg require --provider codex".into());
            }
            if let Some(model) = model {
                claude.model = model;
            }
            Assistant::Claude(claude)
        }
        "codex" => {
            if claude.program != "claude" || !claude.extra.is_empty() {
                return Err("--claude and --claude-arg require --provider claude".into());
            }
            codex.model = model;
            Assistant::Codex(codex)
        }
        _ => return Err(format!("unknown provider {provider:?}: choose claude or codex")),
    };
    Ok(o)
}

fn read_optional(path: &Option<String>) -> Option<String> {
    let p = path.as_ref()?;
    match std::fs::read_to_string(p) {
        Ok(t) => Some(t),
        Err(e) => {
            log(&format!("can't read {p}: {e}"));
            None
        }
    }
}

fn log(msg: &str) {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    println!("[{:02}:{:02}:{:02}] {msg}", (secs / 3600) % 24, (secs / 60) % 60, secs % 60);
}

fn context(app: &mut impl Backend) -> Result<Context, String> {
    app.call("engine.execute", json!({"command": "suggest.context", "params": {}})).map(|v| Context::from_json(&v))
}

/// Ask about paragraph `target` and post what comes back. Returns how many were posted.
fn review(
    app: &mut impl Backend,
    o: &Options,
    watch: &mut Watch,
    ctx: &Context,
    target: usize,
    calls: &mut VecDeque<Instant>,
) -> Result<usize, String> {
    let Some(block) = ctx.block(target) else { return Ok(0) };
    let (brief, style) = (read_optional(&o.brief), read_optional(&o.style));
    let material = prompt::Material { brief: brief.as_deref(), style: style.as_deref(), max: o.max, study: o.study };
    let Some(base) = prompt::build(ctx, target, &material) else { return Ok(0) };
    let questions = if o.study { prompt::questions(&block.text) } else { vec![] };
    let tasks: Vec<Option<&str>> = if questions.is_empty() {
        vec![None]
    } else {
        questions.iter().copied().filter(|q| !block.answered.iter().any(|a| a == q)).map(Some).collect()
    };
    let mut posted = 0;
    for question in tasks {
        let now = Instant::now();
        while calls.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(3600)) {
            calls.pop_front();
        }
        if calls.len() >= o.max_per_hour {
            watch.forget(&block.text);
            log("hourly request budget reached; unanswered questions will resume later");
            return Ok(posted);
        }
        let mut p = base.clone();
        if let Some(q) = question {
            p.push_str("\n\n## This request: answer ONLY this question, as one separate information proposal. Copy this EXACT anchor:\n");
            p.push_str(q);
        }
        if matches!(&o.assistant, Assistant::Codex(c) if c.research) {
            p.push_str("\nLive web search is available. For factual checks use reliable sources and include the actual URLs consulted. Distinguish checked evidence from inference.");
        } else {
            p.push_str("\nLive research is unavailable. Do not imply independent verification or invent citations.");
        }
        log(&format!("asking {} about ¶{target}{}", o.assistant.description(), question.map(|q| format!(": {q}")).unwrap_or_default()));
        calls.push_back(now);
        let reply = o.assistant.ask(&p)?;
        if o.verbose {
            log(&format!("reply: {}", reply.trim()));
        }
        let proposals = prompt::parse(&reply)?;
        let latest = context(app)?;
        if latest.document_id != ctx.document_id || latest.block(target).map(|b| &b.text) != Some(&block.text) {
            watch.forget(&block.text);
            log("paragraph changed during generation; discarding this reply");
            return Ok(posted);
        }
        let mut answered = false;
        for pr in proposals.iter().take(if question.is_some() { 1 } else { o.max }) {
            let mut params = pr.params.clone();
            let command = if o.study {
                let information =
                    params.get("information").or_else(|| params.get("insert")).or_else(|| params.get("replace")).and_then(Value::as_str);
                let quote = question.or_else(|| ["anchor", "find", "after"].into_iter().find_map(|key| params.get(key).and_then(Value::as_str)));
                let (Some(information), Some(quote)) = (information, quote) else { continue };
                if !block.text.contains(quote) {
                    continue;
                }
                params = json!({"anchor":quote,"information":information,"kind":if question.is_some(){"answer"}else{params.get("kind").and_then(Value::as_str).unwrap_or("context")}});
                "study.add"
            } else {
                let quote = ["find", "after", "before"].into_iter().find_map(|key| params.get(key).and_then(Value::as_str));
                if !quote.is_some_and(|q| block.text.contains(q)) {
                    continue;
                }
                "suggest.add"
            };
            if let Some(obj) = params.as_object_mut() {
                obj.insert("provider".into(), json!(o.assistant.provider()));
                obj.insert("block".into(), json!(target));
            }
            if o.dry_run {
                log(&format!("  {command} {params} — {}", pr.why));
                answered = true;
                continue;
            }
            match app.call("engine.execute", json!({"command":command,"params":params})) {
                Ok(_) => {
                    posted += 1;
                    answered = true;
                    log(&format!("  posted {command} {}", short(&params)));
                }
                Err(e) => log(&format!("  skipped: {e}")),
            }
        }
        if question.is_some() && !answered {
            watch.forget(&block.text);
            return Err("no usable answer was returned; the question remains queued".into());
        }
    }
    if let Ok(now) = context(app)
        && let Some(b) = now.block(target)
    {
        watch.mark(&b.text);
    }
    Ok(posted)
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 90 { format!("{}…", s.chars().take(90).collect::<String>()) } else { s }
}

fn run(o: Options) -> Result<(), String> {
    let mut app = Remote::connect(&o.connect).map_err(|e| format!("can't reach WordCraft at {}: {e} (start it with --control)", o.connect))?;
    if let Some(w) = app.key_warning() {
        return Err(w);
    }
    let first = context(&mut app)?;
    if o.study && !first.study_supported {
        return Err(
            "study mode requires the updated WordCraft app: rebuild and restart both WordCraft and wordcraft-companion (or use --mode edit)".into()
        );
    }
    let mut watch = Watch::new(Settings { study: o.study && !o.dry_run, idle: o.idle, min_words: 8, reach: o.reach });
    if o.once {
        let target = first.caret.ok_or("the caret isn't in the body")?;
        review(&mut app, &o, &mut watch, &first, target, &mut VecDeque::new())?;
        return Ok(());
    }
    if !o.review_existing {
        watch.mark_all(&first);
    }
    log(&format!("watching {} — write; suggestions appear after a {} s pause. Ctrl+C stops.", o.connect, o.idle.as_secs()));
    let mut calls: VecDeque<Instant> = VecDeque::new();
    let mut warned_budget = false;
    loop {
        std::thread::sleep(Duration::from_millis(800));
        let ctx = match context(&mut app) {
            Ok(c) => c,
            Err(e) => {
                log(&format!("WordCraft not answering ({e}); retrying"));
                std::thread::sleep(Duration::from_secs(3));
                continue;
            }
        };
        let now = Instant::now();
        while calls.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(3600)) {
            calls.pop_front();
        }
        if calls.len() >= o.max_per_hour {
            if !warned_budget {
                log(&format!("reached {} calls this hour (--max-per-hour); pausing", o.max_per_hour));
                warned_budget = true;
            }
            continue;
        }
        warned_budget = false;
        let Some(target) = watch.observe(&ctx, now) else { continue };
        if let Err(e) = review(&mut app, &o, &mut watch, &ctx, target, &mut calls) {
            if let Some(b) = ctx.block(target) {
                watch.forget(&b.text);
            }
            log(&format!("no suggestions this time: {e}"));
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let o = match parse_args(&args) {
        Ok(o) => o,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("{e}\n");
            }
            eprint!("{USAGE}");
            return if e.is_empty() { ExitCode::SUCCESS } else { ExitCode::from(2) };
        }
    };
    match run(o) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("wordcraft-companion: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn options() {
        let o = parse_args(&args(&["--model=opus", "--idle", "6", "--claude-arg=--effort=low", "--review-existing", "--brief", "b.md"])).unwrap();
        let Assistant::Claude(c) = &o.assistant else { panic!("default provider changed") };
        assert_eq!(c.model, "opus");
        assert_eq!(o.idle, Duration::from_secs(6));
        assert_eq!(c.extra, vec!["--effort=low"]);
        assert_eq!(o.reach, None);
        assert_eq!(o.brief.as_deref(), Some("b.md"));
        assert!(parse_args(&args(&["--idle", "x"])).is_err());
        assert!(parse_args(&args(&["--nope"])).is_err());
        assert!(parse_args(&args(&["--model"])).is_err());
    }

    #[test]
    fn codex_options_are_order_independent_and_do_not_inherit_claude_defaults() {
        let o = parse_args(&args(&["--provider=codex"])).unwrap();
        let Assistant::Codex(c) = o.assistant else { panic!("expected Codex") };
        assert_eq!(c.program, "codex");
        assert_eq!(c.model, None);
        let o = parse_args(&args(&[
            "--model",
            "test-model",
            "--codex",
            "/path with spaces/codex",
            "--codex-arg=--config=model_reasoning_effort=\"low\"",
            "--provider",
            "codex",
            "--once",
            "--dry-run",
        ]))
        .unwrap();
        let Assistant::Codex(c) = o.assistant else { panic!("expected Codex") };
        assert_eq!(c.model.as_deref(), Some("test-model"));
        assert_eq!(c.program, "/path with spaces/codex");
        assert_eq!(c.extra, ["--config=model_reasoning_effort=\"low\""]);
        assert!(o.once && o.dry_run);
        for a in [
            &["--provider", "other"][..],
            &["--provider"],
            &["--codex"],
            &["--codex-arg"],
            &["--codex", "custom"],
            &["--provider", "codex", "--claude-arg=x"],
        ] {
            assert!(parse_args(&args(a)).is_err(), "{a:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn codex_review_posts_suggestions_with_attribution_but_skips_stale_text_and_dry_runs() {
        struct App {
            text: String,
            posted: Vec<Value>,
        }
        impl Backend for App {
            fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
                assert_eq!(method, "engine.execute");
                match params["command"].as_str().unwrap() {
                    "suggest.context" => Ok(json!({"blocks": [{"index": 0, "text": self.text}], "caret": {"block": 0}})),
                    "suggest.add" => {
                        self.posted.push(params["params"].clone());
                        Ok(json!({"id": 0}))
                    }
                    other => panic!("unexpected command {other}"),
                }
            }
            fn has_ui(&self) -> bool {
                true
            }
            fn describe(&self) -> String {
                "test app".into()
            }
        }
        let (_dir, codex) = codex::tests::fake(
            r#"
while [ "$1" != --output-last-message ]; do shift; done
shift
cat >/dev/null
printf '%s' '{"suggestions":[{"find":"builds","replace":"constructs","why":"precise"},{"find":"another paragraph","replace":"outside target","why":"wrong"}]}' > "$1"
"#,
        );
        let mut o = parse_args(&args(&["--provider", "codex", "--mode", "edit"])).unwrap();
        o.assistant = Assistant::Codex(codex);
        let original = "The state builds them.";
        let ctx = Context::from_json(&json!({"blocks": [{"index": 0, "text": original}], "caret": {"block": 0}}));
        for (text, dry_run, count) in [(original, false, 1), (original, true, 0), ("The writer has changed this paragraph.", false, 0)] {
            o.dry_run = dry_run;
            let mut app = App { text: text.into(), posted: vec![] };
            let mut watch = Watch::new(Settings { study: false, idle: Duration::from_secs(4), min_words: 8, reach: Some(2) });
            assert_eq!(review(&mut app, &o, &mut watch, &ctx, 0, &mut VecDeque::new()).unwrap(), count);
            assert_eq!(app.posted.len(), count);
            if count > 0 {
                assert_eq!(app.posted[0], json!({"find": "builds", "replace": "constructs", "provider": "codex", "block": 0}));
            }
        }
    }
}

#[cfg(test)]
#[cfg(unix)]
mod study_tests {
    use super::*;
    struct App {
        text: String,
        posted: Vec<Value>,
        answered: Vec<String>,
    }
    impl Backend for App {
        fn call(&mut self, _: &str, p: Value) -> Result<Value, String> {
            match p["command"].as_str().unwrap() {
                "suggest.context" => Ok(json!({"blocks":[{"index":0,"text":self.text,"answered":self.answered}],"caret":{"block":0}})),
                "study.add" => {
                    self.answered.push(p["params"]["anchor"].as_str().unwrap().into());
                    self.posted.push(p["params"].clone());
                    Ok(json!({"id":self.posted.len()}))
                }
                other => panic!("unexpected {other}"),
            }
        }
        fn has_ui(&self) -> bool {
            true
        }
        fn describe(&self) -> String {
            "study mock".into()
        }
    }
    #[test]
    fn two_questions_make_two_independent_calls_even_with_max_one_and_resume_after_budget() {
        let (_dir, codex) = codex::tests::fake(
            r#"
while [ "$1" != --output-last-message ]; do shift; done
shift
cat >/dev/null
printf '%s' '{"suggestions":[{"anchor":"ignored","information":"An independent answer.","kind":"answer","why":"answer"}]}' > "$1"
"#,
        );
        let args: Vec<_> = ["--provider", "codex", "--max", "1", "--max-per-hour", "1"].iter().map(|s| s.to_string()).collect();
        let mut o = parse_args(&args).unwrap();
        assert!(o.study);
        o.assistant = Assistant::Codex(codex);
        let mut app = App { text: "Why? What?".into(), posted: vec![], answered: vec![] };
        let mut w = Watch::new(Settings { study: false, idle: Duration::from_secs(1), min_words: 8, reach: None });
        let mut calls = VecDeque::new();
        let c = context(&mut app).unwrap();
        assert_eq!(review(&mut app, &o, &mut w, &c, 0, &mut calls).unwrap(), 1);
        assert_eq!(app.posted[0]["anchor"], "Why?");
        calls.clear();
        let c = context(&mut app).unwrap();
        assert_eq!(review(&mut app, &o, &mut w, &c, 0, &mut calls).unwrap(), 1);
        assert_eq!(app.posted[1]["anchor"], "What?");
        let c = context(&mut app).unwrap();
        assert_eq!(review(&mut app, &o, &mut w, &c, 0, &mut calls).unwrap(), 0);
        assert_eq!(app.posted.len(), 2);
    }
    #[test]
    fn research_is_explicit_and_requires_codex() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(parse_args(&args(&["--research"])).is_err());
        let o = parse_args(&args(&["--provider", "codex", "--research"])).unwrap();
        assert!(matches!(o.assistant,Assistant::Codex(c) if c.research));
        assert!(parse_args(&args(&["--mode", "unknown"])).is_err());
    }
}
