//! New release, new tools. The server remembers which release a symlink (for example `/opt/my-app/current`)
//! pointed at when it started. It checks after each answer and in the background while idle; once the link points
//! somewhere else the server exits cleanly, and the MCP client starts it again and reads the new tool list.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

pub const RELEASE_POLL_INTERVAL: Duration = Duration::from_secs(30);

pub struct ReleaseWatch {
    link: PathBuf,
    started_on: PathBuf,
}

fn target(link: &Path) -> Result<PathBuf, String> {
    std::fs::read_link(link).map_err(|error| format!("can't read the release link {}: {error}", link.display()))
}

impl ReleaseWatch {
    pub fn start(link: &Path) -> Result<ReleaseWatch, String> {
        Ok(ReleaseWatch { link: link.to_path_buf(), started_on: target(link)? })
    }

    /// Whether the link now points at another release. A link that can't be read is an error: say it and stop.
    pub fn moved(&self) -> Result<bool, String> {
        Ok(target(&self.link)? != self.started_on)
    }

    /// Watch while the server is idle as well as while it is answering requests. The receiver gets one
    /// event and the watcher ends; the caller exits only after its current request has been answered.
    pub fn watch(&self) -> Receiver<Result<(), String>> {
        self.watch_every(RELEASE_POLL_INTERVAL)
    }

    fn watch_every(&self, interval: Duration) -> Receiver<Result<(), String>> {
        let (sender, receiver) = mpsc::channel();
        let watch = ReleaseWatch { link: self.link.clone(), started_on: self.started_on.clone() };
        thread::spawn(move || {
            loop {
                thread::sleep(interval);
                match watch.moved() {
                    Ok(false) => {}
                    Ok(true) => {
                        let _ = sender.send(Ok(()));
                        break;
                    }
                    Err(why) => {
                        let _ = sender.send(Err(why));
                        break;
                    }
                }
            }
        });
        receiver
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn notices_when_the_link_moves() -> Result<(), String> {
        let dir = std::env::temp_dir().join(format!("mcp-core-release-{}", std::process::id()));
        let link = dir.join("current");
        let make = |to: &str| -> Result<(), String> {
            let _ = std::fs::remove_file(&link);
            std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
            symlink(to, &link).map_err(|error| error.to_string())
        };
        make("releases/one")?;
        let watch = ReleaseWatch::start(&link)?;
        assert!(!watch.moved()?);
        make("releases/two")?;
        assert!(watch.moved()?);
        std::fs::remove_file(&link).map_err(|error| error.to_string())?;
        assert!(watch.moved().is_err());
        std::fs::remove_dir_all(&dir).map_err(|error| error.to_string())?;
        assert!(ReleaseWatch::start(&link).is_err());
        Ok(())
    }

    #[test]
    fn background_watch_notices_a_link_move_while_idle() -> Result<(), String> {
        let dir = std::env::temp_dir().join(format!("mcp-core-release-watch-{}", std::process::id()));
        let link = dir.join("current");
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        symlink("releases/one", &link).map_err(|error| error.to_string())?;
        let watch = ReleaseWatch::start(&link)?;
        let receiver = watch.watch_every(Duration::from_millis(10));
        std::thread::sleep(Duration::from_millis(20));
        // Swap it the way a deploy does, in one rename: a remove then a new link leaves a moment with no link.
        symlink("releases/two", dir.join("current.next")).map_err(|error| error.to_string())?;
        std::fs::rename(dir.join("current.next"), &link).map_err(|error| error.to_string())?;
        assert_eq!(receiver.recv_timeout(Duration::from_secs(10)).map_err(|error| error.to_string())?, Ok(()));
        std::fs::remove_dir_all(&dir).map_err(|error| error.to_string())?;
        Ok(())
    }

    #[test]
    fn background_watch_reports_a_link_read_failure() -> Result<(), String> {
        let dir = std::env::temp_dir().join(format!("mcp-core-release-watch-error-{}", std::process::id()));
        let link = dir.join("current");
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        symlink("releases/one", &link).map_err(|error| error.to_string())?;
        let watch = ReleaseWatch::start(&link)?;
        let receiver = watch.watch_every(Duration::from_millis(10));
        std::thread::sleep(Duration::from_millis(20));
        std::fs::remove_file(&link).map_err(|error| error.to_string())?;
        let event = receiver.recv_timeout(Duration::from_secs(10)).map_err(|error| error.to_string())?;
        assert!(event.is_err());
        std::fs::remove_dir_all(&dir).map_err(|error| error.to_string())?;
        Ok(())
    }
}
