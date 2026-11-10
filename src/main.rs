//! `bike-shop-mcp`: the sample server. An MCP client starts it with the settings in its environment (see config.rs)
//! and talks MCP on stdin/stdout. If a setting is wrong it still answers `initialize`, with the reason as the error,
//! then exits 1.

use std::io::{BufRead, Write};
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::thread;
use std::time::Duration;

use mcp_agent_tools_core::access::{AccessRequest, AccessRequests, Filed, MemoryAccessRequests};
use mcp_agent_tools_core::bike_shop::{self, BikeShop};
use mcp_agent_tools_core::config::Config;
use mcp_agent_tools_core::mcp::{Ready, Server};
use mcp_agent_tools_core::redact::Redactor;
use mcp_agent_tools_core::release::ReleaseWatch;
use mcp_agent_tools_core::tool::Tool;

const NAME: &str = "bike-shop-mcp";

fn log(message: &str) {
    eprintln!("{NAME}: {message}");
}

/// Keeps requests in memory and says each new one on stderr, where the person running the demo sees it.
struct LoggedRequests(MemoryAccessRequests);

impl AccessRequests for LoggedRequests {
    fn file(&self, request: AccessRequest) -> Result<Filed, String> {
        let (asked_by, kind, need) = (request.asked_by.clone(), request.kind, request.need.clone());
        let filed = self.0.file(request)?;
        if filed.created {
            log(&format!("access request {} from {asked_by} ({}): {need}", filed.id, kind.as_str()));
        }
        Ok(filed)
    }
}

fn start(config: &Config, tools: &[Tool<BikeShop>]) -> Result<(Ready<BikeShop>, Option<ReleaseWatch>), String> {
    let watch = config.release_link.as_deref().map(ReleaseWatch::start).transpose()?;
    for scope in &config.key.scopes {
        if !tools.iter().any(|tool| tool.scope_names().any(|name| name == scope)) {
            log(&format!("the key's scope {scope} isn't used by any tool here; it grants nothing"));
        }
    }
    let ready =
        Ready { state: BikeShop::seeded(), key: config.key.clone(), requests: Box::new(LoggedRequests(MemoryAccessRequests::new())) };
    Ok((ready, watch))
}

/// What the release watcher said, if anything: `Some(code)` means exit with it.
fn release_moved(watch: &Option<Receiver<Result<(), String>>>) -> Option<ExitCode> {
    let watch = watch.as_ref()?;
    match watch.try_recv() {
        Ok(Ok(())) => {
            log("a new release is live; exiting so the client starts it and reads its tools");
            Some(ExitCode::SUCCESS)
        }
        Ok(Err(why)) => {
            log(&why);
            Some(ExitCode::FAILURE)
        }
        Err(TryRecvError::Disconnected) => {
            log("the release watcher stopped unexpectedly");
            Some(ExitCode::FAILURE)
        }
        Err(TryRecvError::Empty) => None,
    }
}

fn main() -> ExitCode {
    let tools = bike_shop::tools();
    let started = Config::from_env().and_then(|config| start(&config, &tools));
    let (state, watch) = match started {
        Ok((ready, watch)) => (Ok(ready), watch.map(|watch| watch.watch())),
        Err(why) => {
            log(&format!("can't start: {why}"));
            (Err(why), None)
        }
    };
    let server = Server::new(NAME, env!("CARGO_PKG_VERSION"), tools, state, Redactor::new());
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    let (lines_tx, lines_rx) = mpsc::channel();
    thread::spawn(move || {
        for line in stdin.lock().lines() {
            if lines_tx.send(line).is_err() {
                return;
            }
        }
    });
    loop {
        if let Some(code) = release_moved(&watch) {
            return code;
        }
        let received = if watch.is_some() {
            match lines_rx.recv_timeout(Duration::from_millis(250)) {
                Ok(line) => Some(line),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => None,
            }
        } else {
            lines_rx.recv().ok()
        };
        let Some(line) = received else { return ExitCode::SUCCESS };
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                log(&format!("can't read from the client: {error}"));
                return ExitCode::FAILURE;
            }
        };
        let handled = server.handle(&line);
        if let Some(reply) = handled.reply {
            let written = writeln!(stdout, "{reply}").and_then(|()| stdout.flush());
            if let Err(error) = written {
                log(&format!("can't answer the client: {error}"));
                return ExitCode::FAILURE;
            }
        }
        if handled.stop {
            return ExitCode::FAILURE;
        }
    }
}
